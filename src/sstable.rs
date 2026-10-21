//! Sorted string table.
//! Layout: [data: klen u32 | key | Entry]* [index: count u32, (klen u32 | key | off u64)*]
//!         [bloom: k u8 | bits] [footer: index_off u64 | bloom_off u64 | crc32(index+bloom) u32 | magic u32]
//! The index and Bloom filter are loaded on open; gets check the filter, then binary-search.
use crate::bloom::Bloom;
use crate::storage::Fs;
use crate::Entry;
use std::io;
use std::path::{Path, PathBuf};

const MAGIC: u32 = 0x5749_5332; // "WIS2"
const FOOTER: u64 = 24;
pub const BLOOM_BITS_PER_KEY: usize = 10;

pub struct SsTable {
    pub path: PathBuf,
    index: Vec<(Vec<u8>, u64)>,
    fs: Fs,
    data_end: u64,
    bloom: Bloom,
    pub size: u64,
}

/// Write entries (must be sorted by key) to tmp, sync, then rename. Returns bytes written.
pub fn write<'a, I>(fs: &Fs, path: &Path, entries: I) -> io::Result<u64>
where I: IntoIterator<Item = (&'a Vec<u8>, &'a Entry)> {
    let tmp = path.with_extension("tmp");
    fs.write(&tmp, &[])?;
    let mut w: Vec<u8> = Vec::with_capacity(1 << 20);
    let mut off = 0u64;
    let mut index = Vec::new();
    let mut buf = Vec::new();
    let mut prev: Option<&Vec<u8>> = None;
    for (k, e) in entries {
        assert!(prev.is_none_or(|p| p < k), "sstable keys must be strictly sorted");
        prev = Some(k);
        buf.clear();
        buf.extend_from_slice(&(k.len() as u32).to_le_bytes());
        buf.extend_from_slice(k);
        e.encode(&mut buf);
        w.extend_from_slice(&buf);
        if w.len() >= 1 << 20 { fs.append(&tmp, &w)?; w.clear(); }
        index.push((k, off));
        off += buf.len() as u64;
    }
    let index_off = off;
    let mut ib = Vec::new();
    ib.extend_from_slice(&(index.len() as u32).to_le_bytes());
    for (k, o) in &index {
        ib.extend_from_slice(&(k.len() as u32).to_le_bytes());
        ib.extend_from_slice(k);
        ib.extend_from_slice(&o.to_le_bytes());
    }
    let bloom_off = index_off + ib.len() as u64;
    Bloom::build(index.iter().map(|(k, _)| k.as_slice()), BLOOM_BITS_PER_KEY).encode(&mut ib);
    w.extend_from_slice(&ib);
    w.extend_from_slice(&index_off.to_le_bytes());
    w.extend_from_slice(&bloom_off.to_le_bytes());
    w.extend_from_slice(&crc32fast::hash(&ib).to_le_bytes());
    w.extend_from_slice(&MAGIC.to_le_bytes());
    fs.append(&tmp, &w)?;
    fs.sync(&tmp)?;
    fs.rename(&tmp, path)?;
    Ok(index_off + ib.len() as u64 + FOOTER)
}

impl SsTable {
    pub fn open(fs: Fs, path: &Path) -> io::Result<Self> {
        let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_string());
        let size = fs.len(path)?;
        if size < FOOTER { return Err(bad("sstable too small")); }
        let ft = fs.read_at(path, size - FOOTER, FOOTER as usize)?;
        let index_off = u64::from_le_bytes(ft[0..8].try_into().unwrap());
        let bloom_off = u64::from_le_bytes(ft[8..16].try_into().unwrap());
        let crc = u32::from_le_bytes(ft[16..20].try_into().unwrap());
        if u32::from_le_bytes(ft[20..24].try_into().unwrap()) != MAGIC { return Err(bad("bad magic")); }
        if !(index_off <= bloom_off && bloom_off <= size - FOOTER) { return Err(bad("bad footer")); }
        let ib = fs.read_at(path, index_off, (size - FOOTER - index_off) as usize)?;
        if crc32fast::hash(&ib) != crc { return Err(bad("index checksum mismatch")); }
        let bloom = Bloom::decode(&ib[(bloom_off - index_off) as usize..]).ok_or_else(|| bad("bad bloom"))?;
        let ib = &ib[..(bloom_off - index_off) as usize];
        let n = u32::from_le_bytes(ib[0..4].try_into().unwrap()) as usize;
        let mut index = Vec::with_capacity(n);
        let mut i = 4;
        for _ in 0..n {
            let kl = u32::from_le_bytes(ib[i..i + 4].try_into().unwrap()) as usize;
            let k = ib[i + 4..i + 4 + kl].to_vec();
            let o = u64::from_le_bytes(ib[i + 4 + kl..i + 12 + kl].try_into().unwrap());
            index.push((k, o));
            i += 12 + kl;
        }
        Ok(Self { path: path.to_path_buf(), index, fs, data_end: index_off, bloom, size })
    }
    pub fn len(&self) -> usize { self.index.len() }
    pub fn is_empty(&self) -> bool { self.index.is_empty() }
    /// Bloom check only: false means the key is definitely absent.
    pub fn may_contain(&self, key: &[u8]) -> bool { self.bloom.may_contain(key) }
    /// Binary-search the index, then read one record from disk (no Bloom check).
    pub fn get(&mut self, key: &[u8]) -> io::Result<Option<Entry>> {
        match self.index.binary_search_by(|(k, _)| k.as_slice().cmp(key)) {
            Ok(i) => self.read_at(i).map(|(_, e)| Some(e)),
            Err(_) => Ok(None),
        }
    }
    fn read_at(&mut self, i: usize) -> io::Result<(Vec<u8>, Entry)> {
        let start = self.index[i].1;
        let end = self.index.get(i + 1).map_or(self.data_end, |x| x.1);
        let buf = self.fs.read_at(&self.path, start, (end - start) as usize)?;
        let kl = u32::from_le_bytes(buf[0..4].try_into().unwrap()) as usize;
        let (e, _) = Entry::decode(&buf[4 + kl..])?;
        Ok((buf[4..4 + kl].to_vec(), e))
    }
    /// All entries with start <= key < end (end None = unbounded), in key order.
    pub fn range(&mut self, start: &[u8], end: Option<&[u8]>) -> io::Result<Vec<(Vec<u8>, Entry)>> {
        let lo = self.index.partition_point(|(k, _)| k.as_slice() < start);
        let mut out = Vec::new();
        for i in lo..self.index.len() {
            if end.is_some_and(|e| self.index[i].0.as_slice() >= e) { break; }
            out.push(self.read_at(i)?);
        }
        Ok(out)
    }
    /// Every entry, sequentially (used by compaction and GC).
    pub fn all(&mut self) -> io::Result<Vec<(Vec<u8>, Entry)>> {
        let buf = self.fs.read_at(&self.path, 0, self.data_end as usize)?;
        let mut out = Vec::with_capacity(self.index.len());
        let mut i = 0;
        while i < buf.len() {
            let kl = u32::from_le_bytes(buf[i..i + 4].try_into().unwrap()) as usize;
            let k = buf[i + 4..i + 4 + kl].to_vec();
            let (e, n) = Entry::decode(&buf[i + 4 + kl..])?;
            out.push((k, e));
            i += 4 + kl + n;
        }
        Ok(out)
    }
    pub fn keys(&self) -> impl Iterator<Item = &Vec<u8>> { self.index.iter().map(|(k, _)| k) }
}
