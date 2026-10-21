//! The storage engine: WAL -> memtable -> sstables (size-tiered compaction),
//! with values in a vlog (WiscKey) or inline (Baseline).
//!
//! A MANIFEST file (written atomically via tmp+rename) is the commit point for
//! flushes, compactions and GC: it lists the live sstables, newest last, and the
//! current vlog generation. Files not listed are garbage and deleted on open.
use crate::memtable::MemTable;
use crate::sstable::{self, SsTable};
use crate::storage::{Fs, StdFs};
use crate::vlog::ValueLog;
use crate::wal::Wal;
use crate::Entry;
use serde::Serialize;
use std::collections::BTreeMap;
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
    pub fn as_str(&self) -> &'static str { match self { Mode::WiscKey => "wisckey", Mode::Baseline => "baseline" } }
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
    /// Size-tiered compaction: merge once this many same-tier tables pile up (0 = off).
    pub compaction_fanin: usize,
}

impl Default for Options {
    fn default() -> Self { Self { mode: Mode::WiscKey, memtable_bytes: 4 << 20, sync_writes: false, compaction_fanin: 4 } }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Stats {
    pub logical_bytes: u64,
    pub wal_bytes: u64,
    pub vlog_bytes: u64,
    #[serde(skip)]
    vlog_bytes_retired: u64,
    /// Bytes written by memtable flushes.
    pub sst_bytes: u64,
    /// Bytes written by compactions (rewriting sstables).
    pub compaction_bytes: u64,
    /// Bytes written by value-log GC (new vlog + rewritten tree).
    pub gc_bytes: u64,
    pub physical_bytes: u64,
    pub write_amplification: f64,
    /// (flush + compaction) bytes per logical byte: the LSM tree's own amplification.
    pub tree_amplification: f64,
    pub flushes: u64,
    pub compactions: u64,
    pub tombstones_dropped: u64,
    pub gc_runs: u64,
    pub gc_reclaimed_bytes: u64,
    /// Table probes skipped because the Bloom filter said "definitely absent".
    pub bloom_negatives: u64,
    /// Bloom said "maybe" but the key was not in the table.
    pub bloom_false_positives: u64,
    /// false_positives / (false_positives + negatives): measured FPR over absent-key probes.
    pub bloom_fpr: f64,
    pub sstables: usize,
    pub sstable_disk_bytes: u64,
    pub vlog_disk_bytes: u64,
    pub memtable_entries: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct GcReport {
    pub live_keys: usize,
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub reclaimed_bytes: u64,
}

pub struct Db {
    fs: Fs,
    dir: PathBuf,
    opts: Options,
    wal: Wal,
    vlog: Option<ValueLog>,
    vlog_gen: u64,
    mem: MemTable,
    /// Oldest first; reads scan newest -> oldest.
    tables: Vec<SsTable>,
    ids: Vec<u64>,
    next_id: u64,
    stats: Stats,
}

fn sst_name(id: u64) -> String { format!("{id:06}.sst") }
fn vlog_name(g: u64) -> String { if g == 0 { "vlog".into() } else { format!("vlog.{g}") } }

impl Db {
    /// Open on the native file system.
    pub fn open(dir: impl AsRef<Path>, opts: Options) -> io::Result<Self> { Self::open_with(StdFs::new(), dir, opts) }

    /// Open on any storage backend (e.g. `MemFs` in the browser).
    pub fn open_with(fs: Fs, dir: impl AsRef<Path>, mut opts: Options) -> io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        fs.create_dir_all(&dir)?;
        let mode_path = dir.join("MODE");
        match fs.read(&mode_path).map(|b| String::from_utf8_lossy(&b).into_owned()) {
            Ok(s) => {
                opts.mode = Mode::parse(&s)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad MODE file"))?;
            }
            Err(_) => fs.write(&mode_path, opts.mode.as_str().as_bytes())?,
        }
        let names = fs.list(&dir)?;
        let on_disk: Vec<u64> = names.iter().filter_map(|n| n.strip_suffix(".sst")?.parse().ok()).collect();
        let (vlog_gen, ids) = match fs.read(&dir.join("MANIFEST")).map(|b| String::from_utf8_lossy(&b).into_owned()) {
            Ok(m) => {
                let mut lines = m.lines();
                let bad = || io::Error::new(io::ErrorKind::InvalidData, "bad MANIFEST");
                let g: u64 = lines.next().ok_or_else(bad)?.trim().parse().map_err(|_| bad())?;
                let ids: Vec<u64> = lines.next().unwrap_or("").split_whitespace()
                    .map(|x| x.parse().map_err(|_| bad())).collect::<io::Result<_>>()?;
                (g, ids)
            }
            Err(_) => { let mut v = on_disk.clone(); v.sort(); (0, v) } // pre-manifest layout
        };
        // Garbage left by an interrupted flush/compaction/GC.
        for id in on_disk.iter().filter(|i| !ids.contains(i)) { let _ = fs.remove(&dir.join(sst_name(*id))); }
        for n in &names {
            if (n.starts_with("vlog") && *n != vlog_name(vlog_gen)) || n.ends_with(".tmp") { let _ = fs.remove(&dir.join(n)); }
        }
        let mut tables = Vec::new();
        for id in &ids { tables.push(SsTable::open(fs.clone(), &dir.join(sst_name(*id)))?); }
        let next_id = on_disk.iter().chain(ids.iter()).max().map_or(1, |x| x + 1);
        let vlog = if opts.mode == Mode::WiscKey { Some(ValueLog::open(fs.clone(), &dir.join(vlog_name(vlog_gen)))?) } else { None };
        // Crash recovery: rebuild the memtable from the WAL.
        let wal_path = dir.join("wal");
        let mut mem = MemTable::new();
        for (k, e) in Wal::replay(&fs, &wal_path)? { mem.insert(k, e); }
        let wal = Wal::open(fs.clone(), &wal_path)?;
        let db = Self { fs, dir, opts, wal, vlog, vlog_gen, mem, tables, ids, next_id, stats: Stats::default() };
        db.write_manifest()?;
        Ok(db)
    }

    pub fn mode(&self) -> Mode { self.opts.mode }

    fn write_manifest(&self) -> io::Result<()> {
        let ids: Vec<String> = self.ids.iter().map(|i| i.to_string()).collect();
        let tmp = self.dir.join("MANIFEST.tmp");
        self.fs.write(&tmp, format!("{}\n{}\n", self.vlog_gen, ids.join(" ")).as_bytes())?;
        self.fs.sync(&tmp)?;
        self.fs.rename(&tmp, &self.dir.join("MANIFEST"))
    }

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

    fn resolve(&mut self, e: Entry) -> io::Result<Option<Vec<u8>>> {
        match e {
            Entry::Tombstone => Ok(None),
            Entry::Inline(v) => Ok(Some(v)),
            Entry::Ptr(p) => self.vlog.as_mut().expect("ptr without vlog").read(p).map(Some),
        }
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
        match found { None => Ok(None), Some(e) => self.resolve(e) }
    }

    /// Live (key, value) pairs with start <= key < end, in key order.
    pub fn scan(&mut self, start: &[u8], end: Option<&[u8]>) -> io::Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let mut merged: BTreeMap<Vec<u8>, Entry> = BTreeMap::new();
        for t in self.tables.iter_mut() { // oldest -> newest, newer overwrite
            for (k, e) in t.range(start, end)? { merged.insert(k, e); }
        }
        for (k, e) in self.mem.range(start, end) { merged.insert(k.clone(), e.clone()); }
        let mut out = Vec::new();
        for (k, e) in merged {
            if let Some(v) = self.resolve(e)? { out.push((k, v)); }
        }
        Ok(out)
    }

    /// Write the memtable to a new sstable, reset the WAL, then maybe compact.
    pub fn flush(&mut self) -> io::Result<()> {
        if self.mem.is_empty() { return Ok(()); }
        if let Some(v) = &mut self.vlog { v.sync()?; }
        let id = self.next_id;
        self.next_id += 1;
        let path = self.dir.join(sst_name(id));
        let n = sstable::write(&self.fs, &path, self.mem.iter())?;
        self.stats.sst_bytes += n;
        self.stats.flushes += 1;
        self.tables.push(SsTable::open(self.fs.clone(), &path)?);
        self.ids.push(id);
        self.write_manifest()?;
        self.mem.clear();
        self.wal.reset()?;
        self.maybe_compact()
    }

    fn tier(&self, size: u64) -> u32 {
        let r = size as f64 / self.opts.memtable_bytes as f64 * 1.5;
        if r < 4.0 { 0 } else { r.log(4.0).floor() as u32 }
    }

    /// Size-tiered compaction: while the `fanin` newest tables share a size tier,
    /// merge them into one (the merged table lands in the next tier up).
    fn maybe_compact(&mut self) -> io::Result<()> {
        let f = self.opts.compaction_fanin;
        if f < 2 { return Ok(()); }
        loop {
            let n = self.tables.len();
            if n < f { return Ok(()); }
            let t = self.tier(self.tables[n - f].size);
            if !self.tables[n - f..].iter().all(|x| self.tier(x.size) == t) { return Ok(()); }
            self.compact_range(n - f, n)?;
        }
    }

    /// Merge every sstable into one, dropping all tombstones.
    pub fn compact_all(&mut self) -> io::Result<()> {
        self.flush()?;
        let n = self.tables.len();
        if n > 0 { self.compact_range(0, n)?; }
        Ok(())
    }

    fn merge(&mut self, lo: usize, hi: usize) -> io::Result<BTreeMap<Vec<u8>, Entry>> {
        let mut merged = BTreeMap::new();
        for t in &mut self.tables[lo..hi] { for (k, e) in t.all()? { merged.insert(k, e); } }
        if lo == 0 {
            // No older table can hold a shadowed version, so tombstones can go.
            let before = merged.len();
            merged.retain(|_, e| !matches!(e, Entry::Tombstone));
            self.stats.tombstones_dropped += (before - merged.len()) as u64;
        }
        Ok(merged)
    }

    fn compact_range(&mut self, lo: usize, hi: usize) -> io::Result<()> {
        let merged = self.merge(lo, hi)?;
        let mut new_tables = Vec::new();
        let mut new_ids = Vec::new();
        if !merged.is_empty() {
            let id = self.next_id;
            self.next_id += 1;
            let path = self.dir.join(sst_name(id));
            self.stats.compaction_bytes += sstable::write(&self.fs, &path, merged.iter())?;
            new_tables.push(SsTable::open(self.fs.clone(), &path)?);
            new_ids.push(id);
        }
        let old: Vec<u64> = self.ids.splice(lo..hi, new_ids).collect();
        self.tables.splice(lo..hi, new_tables);
        self.write_manifest()?;
        for id in old { let _ = self.fs.remove(&self.dir.join(sst_name(id))); }
        self.stats.compactions += 1;
        Ok(())
    }

    /// Value-log garbage collection (stop-the-world copy): copy only live values into
    /// a new vlog generation, rewrite the tree as one sstable pointing at it, commit
    /// via MANIFEST, then delete the old vlog. Baseline mode just compacts everything.
    pub fn gc(&mut self) -> io::Result<GcReport> {
        self.flush()?;
        let before = self.disk_bytes_of_data()?;
        let n = self.tables.len();
        let merged = self.merge(0, n)?;
        let live_keys = merged.len();
        let new_gen = self.vlog_gen + 1;
        let mut new_vlog = match self.opts.mode {
            Mode::WiscKey => Some(ValueLog::open(self.fs.clone(), &self.dir.join(vlog_name(new_gen)))?),
            Mode::Baseline => None,
        };
        let mut rewritten = BTreeMap::new();
        for (k, e) in merged {
            let e = match (e, &mut new_vlog) {
                (Entry::Ptr(p), Some(nv)) => {
                    let v = self.vlog.as_mut().unwrap().read(p)?;
                    Entry::Ptr(nv.append(&v)?)
                }
                (e, _) => e,
            };
            rewritten.insert(k, e);
        }
        let mut written = 0;
        if let Some(nv) = &mut new_vlog { nv.sync()?; written += nv.bytes_written; nv.bytes_written = 0; }
        let mut new_ids = vec![];
        let mut new_tables = vec![];
        if !rewritten.is_empty() {
            let id = self.next_id;
            self.next_id += 1;
            let path = self.dir.join(sst_name(id));
            written += sstable::write(&self.fs, &path, rewritten.iter())?;
            new_tables.push(SsTable::open(self.fs.clone(), &path)?);
            new_ids.push(id);
        }
        let old_ids = std::mem::replace(&mut self.ids, new_ids);
        self.tables = new_tables;
        let old_vlog = self.vlog_gen;
        if let Some(nv) = new_vlog {
            self.stats.vlog_bytes_retired += self.vlog.as_ref().map_or(0, |v| v.bytes_written);
            self.vlog = Some(nv);
            self.vlog_gen = new_gen;
        }
        self.write_manifest()?; // commit point
        for id in old_ids { let _ = self.fs.remove(&self.dir.join(sst_name(id))); }
        if self.vlog_gen != old_vlog { let _ = self.fs.remove(&self.dir.join(vlog_name(old_vlog))); }
        let after = self.disk_bytes_of_data()?;
        let reclaimed = before.saturating_sub(after);
        self.stats.gc_runs += 1;
        self.stats.gc_bytes += written;
        self.stats.gc_reclaimed_bytes += reclaimed;
        Ok(GcReport { live_keys, bytes_before: before, bytes_after: after, reclaimed_bytes: reclaimed })
    }

    fn disk_bytes_of_data(&mut self) -> io::Result<u64> {
        let mut t: u64 = self.tables.iter().map(|x| x.size).sum();
        if let Some(v) = &mut self.vlog { v.flush()?; t += self.fs.len(&self.dir.join(vlog_name(self.vlog_gen)))?; }
        Ok(t)
    }

    /// Carry I/O counters over from a previous process/instance (used after a
    /// simulated crash so cumulative write amplification stays meaningful).
    pub fn adopt_counters(&mut self, prev: &Stats) {
        let mut s = prev.clone();
        s.vlog_bytes_retired = prev.vlog_bytes;
        self.wal.bytes_written += prev.wal_bytes;
        s.bloom_fpr = 0.0;
        self.stats = s;
    }

    pub fn stats(&self) -> Stats {
        let mut s = self.stats.clone();
        s.wal_bytes = self.wal.bytes_written;
        s.vlog_bytes = s.vlog_bytes_retired + self.vlog.as_ref().map_or(0, |v| v.bytes_written);
        s.physical_bytes = s.wal_bytes + s.vlog_bytes + s.sst_bytes + s.compaction_bytes + s.gc_bytes;
        if s.logical_bytes > 0 {
            s.write_amplification = s.physical_bytes as f64 / s.logical_bytes as f64;
            s.tree_amplification = (s.sst_bytes + s.compaction_bytes) as f64 / s.logical_bytes as f64;
        }
        let absent = s.bloom_false_positives + s.bloom_negatives;
        if absent > 0 { s.bloom_fpr = s.bloom_false_positives as f64 / absent as f64; }
        s.sstables = self.tables.len();
        s.sstable_disk_bytes = self.tables.iter().map(|x| x.size).sum();
        s.vlog_disk_bytes = self.vlog.as_ref().map_or(0, |v| v.size());
        s.memtable_entries = self.mem.len();
        s
    }

    /// On-disk footprint of the database directory.
    pub fn disk_usage(&self) -> io::Result<u64> {
        Ok(self.list_files()?.iter().map(|f| f.1).sum())
    }

    /// (file name, size in bytes) for every file in the database directory, sorted by name.
    pub fn list_files(&self) -> io::Result<Vec<(String, u64)>> {
        let mut v: Vec<(String, u64)> = self.fs.list(&self.dir)?.into_iter()
            .map(|n| { let l = self.fs.len(&self.dir.join(&n)).unwrap_or(0); (n, l) }).collect();
        v.sort();
        Ok(v)
    }
}
