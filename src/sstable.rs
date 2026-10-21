//! Sorted string table.
//! Layout: [data: klen u32 | key | Entry]* [index: count u32, (klen u32 | key | off u64)*]
//!         [footer: index_off u64 | crc32(index) u32 | magic u32]
//! The index is loaded on open and searched with binary search.
use crate::Entry;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const MAGIC: u32 = 0x5749_5343; // "WISC"

pub struct SsTable {
    pub path: PathBuf,
    index: Vec<(Vec<u8>, u64)>,
    file: File,
    pub size: u64,
}

/// Write entries (must be sorted by key). Returns bytes written.
pub fn write<'a, I>(path: &Path, entries: I) -> io::Result<u64>
where I: IntoIterator<Item = (&'a Vec<u8>, &'a Entry)> {
    let tmp = path.with_extension("tmp");
    let f = OpenOptions::new().create(true).write(true).truncate(true).open(&tmp)?;
    let mut w = BufWriter::with_capacity(1 << 20, f);
    let mut off = 0u64;
    let mut index = Vec::new();
    let mut buf = Vec::new();
    let mut prev: Option<&Vec<u8>> = None;
    for (k, e) in entries {
        assert!(prev.map_or(true, |p| p < k), "sstable keys must be strictly sorted");
        prev = Some(k);
        buf.clear();
        buf.extend_from_slice(&(k.len() as u32).to_le_bytes());
        buf.extend_from_slice(k);
        e.encode(&mut buf);
        w.write_all(&buf)?;
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
    w.write_all(&ib)?;
    w.write_all(&index_off.to_le_bytes())?;
    w.write_all(&crc32fast::hash(&ib).to_le_bytes())?;
    w.write_all(&MAGIC.to_le_bytes())?;
    w.flush()?;
    w.get_ref().sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(index_off + ib.len() as u64 + 16)
}

impl SsTable {
    pub fn open(path: &Path) -> io::Result<Self> {
        let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_string());
        let mut file = File::open(path)?;
        let size = file.metadata()?.len();
        if size < 16 { return Err(bad("sstable too small")); }
        file.seek(SeekFrom::Start(size - 16))?;
        let mut ft = [0u8; 16];
        file.read_exact(&mut ft)?;
        let index_off = u64::from_le_bytes(ft[0..8].try_into().unwrap());
        let crc = u32::from_le_bytes(ft[8..12].try_into().unwrap());
        if u32::from_le_bytes(ft[12..16].try_into().unwrap()) != MAGIC { return Err(bad("bad magic")); }
        file.seek(SeekFrom::Start(index_off))?;
        let mut ib = vec![0u8; (size - 16 - index_off) as usize];
        file.read_exact(&mut ib)?;
        if crc32fast::hash(&ib) != crc { return Err(bad("index checksum mismatch")); }
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
        Ok(Self { path: path.to_path_buf(), index, file, size })
    }
    pub fn len(&self) -> usize { self.index.len() }
    pub fn is_empty(&self) -> bool { self.index.is_empty() }
    /// Binary-search the index, then read one record from disk.
    pub fn get(&mut self, key: &[u8]) -> io::Result<Option<Entry>> {
        let Ok(i) = self.index.binary_search_by(|(k, _)| k.as_slice().cmp(key)) else { return Ok(None) };
        let start = self.index[i].1;
        let end = self.index.get(i + 1).map(|x| x.1).unwrap_or_else(|| self.index_end());
        let mut buf = vec![0u8; (end - start) as usize];
        self.file.seek(SeekFrom::Start(start))?;
        self.file.read_exact(&mut buf)?;
        let kl = u32::from_le_bytes(buf[0..4].try_into().unwrap()) as usize;
        let (e, _) = Entry::decode(&buf[4 + kl..])?;
        Ok(Some(e))
    }
    fn index_end(&mut self) -> u64 {
        // data section ends where the index begins
        let mut ft = [0u8; 8];
        self.file.seek(SeekFrom::Start(self.size - 16)).ok();
        self.file.read_exact(&mut ft).ok();
        u64::from_le_bytes(ft)
    }
    pub fn keys(&self) -> impl Iterator<Item = &Vec<u8>> { self.index.iter().map(|(k, _)| k) }
}
