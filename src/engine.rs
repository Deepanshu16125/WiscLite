//! The storage engine: WAL -> memtable -> sstables, with values in a vlog (WiscKey)
//! or inline (Baseline).
use crate::memtable::MemTable;
use crate::sstable::{self, SsTable};
use crate::vlog::ValueLog;
use crate::wal::Wal;
use crate::Entry;
use serde::Serialize;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Mode {
    /// Key-value separation: tree stores (key, ValuePtr), values go to the vlog.
    WiscKey,
    /// Classic LSM: values stored inline in WAL and sstables.
    Baseline,
}

impl Mode {
    fn as_str(&self) -> &'static str { match self { Mode::WiscKey => "wisckey", Mode::Baseline => "baseline" } }
    fn parse(s: &str) -> Option<Mode> {
        match s.trim() { "wisckey" => Some(Mode::WiscKey), "baseline" => Some(Mode::Baseline), _ => None }
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    pub mode: Mode,
    /// Flush the memtable once its approximate size exceeds this many bytes.
    pub memtable_bytes: usize,
    /// fsync WAL/vlog on every write (slow but strictly durable).
    pub sync_writes: bool,
}

impl Default for Options {
    fn default() -> Self { Self { mode: Mode::WiscKey, memtable_bytes: 4 << 20, sync_writes: false } }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Stats {
    pub logical_bytes: u64,
    pub wal_bytes: u64,
    pub vlog_bytes: u64,
    pub sst_bytes: u64,
    pub physical_bytes: u64,
    pub write_amplification: f64,
    /// Bytes written into the LSM tree itself (sstables) per logical byte —
    /// this is what compaction would multiply.
    pub tree_amplification: f64,
    pub flushes: u64,
    /// Table probes skipped because the Bloom filter said "definitely absent".
    pub bloom_negatives: u64,
    /// Bloom said "maybe" but the key was not in the table.
    pub bloom_false_positives: u64,
    /// false_positives / (false_positives + negatives): measured FPR over absent-key probes.
    pub bloom_fpr: f64,
    pub sstables: usize,
    pub memtable_entries: usize,
}

pub struct Db {
    dir: PathBuf,
    opts: Options,
    wal: Wal,
    vlog: Option<ValueLog>,
    mem: MemTable,
    /// Oldest first; reads scan newest -> oldest.
    tables: Vec<SsTable>,
    next_id: u64,
    stats: Stats,
}

impl Db {
    pub fn open(dir: impl AsRef<Path>, mut opts: Options) -> io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;
        let mode_path = dir.join("MODE");
        match fs::read_to_string(&mode_path) {
            Ok(s) => {
                opts.mode = Mode::parse(&s)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad MODE file"))?;
            }
            Err(_) => fs::write(&mode_path, opts.mode.as_str())?,
        }
        let mut ids: Vec<u64> = fs::read_dir(&dir)?
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let n = e.file_name().into_string().ok()?;
                n.strip_suffix(".sst")?.parse().ok()
            })
            .collect();
        ids.sort();
        let mut tables = Vec::new();
        for id in &ids { tables.push(SsTable::open(&dir.join(format!("{id:06}.sst")))?); }
        let next_id = ids.last().map_or(1, |x| x + 1);
        let vlog = if opts.mode == Mode::WiscKey { Some(ValueLog::open(&dir.join("vlog"))?) } else { None };
        // Crash recovery: rebuild the memtable from the WAL.
        let wal_path = dir.join("wal");
        let mut mem = MemTable::new();
        for (k, e) in Wal::replay(&wal_path)? { mem.insert(k, e); }
        let wal = Wal::open(&wal_path)?;
        let stats = Stats { sstables: tables.len(), ..Default::default() };
        Ok(Self { dir, opts, wal, vlog, mem, tables, next_id, stats })
    }

    pub fn mode(&self) -> Mode { self.opts.mode }

    pub fn put(&mut self, key: &[u8], value: &[u8]) -> io::Result<()> {
        let e = match &mut self.vlog {
            Some(v) => Entry::Ptr(v.append(value)?),
            None => Entry::Inline(value.to_vec()),
        };
        self.stats.logical_bytes += (key.len() + value.len()) as u64;
        self.write_entry(key, e)
    }

    pub fn delete(&mut self, key: &[u8]) -> io::Result<()> {
        self.stats.logical_bytes += key.len() as u64;
        self.write_entry(key, Entry::Tombstone)
    }

    fn write_entry(&mut self, key: &[u8], e: Entry) -> io::Result<()> {
        if let Some(v) = &mut self.vlog {
            // value must be durable before the pointer that references it
            if self.opts.sync_writes { v.sync()?; } else { v.flush()?; }
        }
        self.wal.append(key, &e)?;
        if self.opts.sync_writes { self.wal.sync()?; } else { self.wal.flush()?; }
        self.mem.insert(key.to_vec(), e);
        if self.mem.approx_bytes() >= self.opts.memtable_bytes { self.flush()?; }
        Ok(())
    }

    pub fn get(&mut self, key: &[u8]) -> io::Result<Option<Vec<u8>>> {
        let found = match self.mem.get(key) {
            Some(e) => Some(e.clone()),
            None => {
                let mut f = None;
                for t in self.tables.iter_mut().rev() {
                    if !t.may_contain(key) { self.stats.bloom_negatives += 1; continue; }
                    match t.get(key)? {
                        Some(e) => { f = Some(e); break; }
                        None => self.stats.bloom_false_positives += 1,
                    }
                }
                f
            }
        };
        match found {
            None | Some(Entry::Tombstone) => Ok(None),
            Some(Entry::Inline(v)) => Ok(Some(v)),
            Some(Entry::Ptr(p)) => self.vlog.as_mut().expect("ptr without vlog").read(p).map(Some),
        }
    }

    /// Write the memtable to a new sstable and reset the WAL.
    pub fn flush(&mut self) -> io::Result<()> {
        if self.mem.is_empty() { return Ok(()); }
        if let Some(v) = &mut self.vlog { v.sync()?; }
        let path = self.dir.join(format!("{:06}.sst", self.next_id));
        self.next_id += 1;
        let n = sstable::write(&path, self.mem.iter())?;
        self.stats.sst_bytes += n;
        self.stats.flushes += 1;
        self.tables.push(SsTable::open(&path)?);
        self.mem.clear();
        self.wal.reset()?;
        Ok(())
    }

    pub fn stats(&self) -> Stats {
        let mut s = self.stats.clone();
        s.wal_bytes = self.wal.bytes_written;
        s.vlog_bytes = self.vlog.as_ref().map_or(0, |v| v.bytes_written);
        s.physical_bytes = s.wal_bytes + s.vlog_bytes + s.sst_bytes;
        if s.logical_bytes > 0 {
            s.write_amplification = s.physical_bytes as f64 / s.logical_bytes as f64;
            s.tree_amplification = s.sst_bytes as f64 / s.logical_bytes as f64;
        }
        let absent = s.bloom_false_positives + s.bloom_negatives;
        if absent > 0 { s.bloom_fpr = s.bloom_false_positives as f64 / absent as f64; }
        s.sstables = self.tables.len();
        s.memtable_entries = self.mem.len();
        s
    }

    /// On-disk footprint of the database directory.
    pub fn disk_usage(&self) -> io::Result<u64> {
        let mut t = 0;
        for e in fs::read_dir(&self.dir)? { t += e?.metadata()?.len(); }
        Ok(t)
    }
}
