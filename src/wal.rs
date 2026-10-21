//! Write-ahead log. Record: crc32(payload) | len(payload) | payload,
//! payload = klen u32 | key | encoded Entry. Torn/corrupt tails are dropped on replay.
use crate::storage::Fs;
use crate::Entry;
use std::io;
use std::path::{Path, PathBuf};

pub struct Wal {
    fs: Fs,
    path: PathBuf,
    buf: Vec<u8>,
    pub bytes_written: u64,
}

impl Wal {
    pub fn open(fs: Fs, path: &Path) -> io::Result<Self> {
        if !fs.exists(path) { fs.write(path, &[])?; }
        Ok(Self { fs, path: path.to_path_buf(), buf: Vec::with_capacity(1 << 16), bytes_written: 0 })
    }
    pub fn append(&mut self, key: &[u8], e: &Entry) -> io::Result<()> {
        let mut p = Vec::with_capacity(4 + key.len() + e.encoded_len());
        p.extend_from_slice(&(key.len() as u32).to_le_bytes());
        p.extend_from_slice(key);
        e.encode(&mut p);
        self.buf.extend_from_slice(&crc32fast::hash(&p).to_le_bytes());
        self.buf.extend_from_slice(&(p.len() as u32).to_le_bytes());
        self.buf.extend_from_slice(&p);
        self.bytes_written += 8 + p.len() as u64;
        Ok(())
    }
    pub fn flush(&mut self) -> io::Result<()> {
        if !self.buf.is_empty() { self.fs.append(&self.path, &self.buf)?; self.buf.clear(); }
        Ok(())
    }
    pub fn sync(&mut self) -> io::Result<()> { self.flush()?; self.fs.sync(&self.path) }
    /// Replay all valid records, stopping at the first torn or corrupt one.
    pub fn replay(fs: &Fs, path: &Path) -> io::Result<Vec<(Vec<u8>, Entry)>> {
        let buf = match fs.read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e),
        };
        let mut out = Vec::new();
        let mut i = 0;
        while i + 8 <= buf.len() {
            let crc = u32::from_le_bytes(buf[i..i + 4].try_into().unwrap());
            let n = u32::from_le_bytes(buf[i + 4..i + 8].try_into().unwrap()) as usize;
            if i + 8 + n > buf.len() { break; }
            let p = &buf[i + 8..i + 8 + n];
            if crc32fast::hash(p) != crc || p.len() < 4 { break; }
            let kl = u32::from_le_bytes(p[0..4].try_into().unwrap()) as usize;
            if 4 + kl > p.len() { break; }
            let Ok((e, _)) = Entry::decode(&p[4 + kl..]) else { break };
            out.push((p[4..4 + kl].to_vec(), e));
            i += 8 + n;
        }
        Ok(out)
    }
    /// Discard the log after its contents are durable in an sstable.
    pub fn reset(&mut self) -> io::Result<()> {
        self.buf.clear();
        self.fs.write(&self.path, &[])?;
        self.fs.sync(&self.path)
    }
}
