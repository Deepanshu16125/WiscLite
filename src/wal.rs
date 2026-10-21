//! Write-ahead log. Record: crc32(payload) | len(payload) | payload,
//! payload = klen u32 | key | encoded Entry. Torn/corrupt tails are dropped on replay.
use crate::Entry;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

pub struct Wal {
    path: PathBuf,
    w: BufWriter<File>,
    pub bytes_written: u64,
}

impl Wal {
    pub fn open(path: &Path) -> io::Result<Self> {
        let f = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self { path: path.to_path_buf(), w: BufWriter::with_capacity(1 << 20, f), bytes_written: 0 })
    }
    pub fn append(&mut self, key: &[u8], e: &Entry) -> io::Result<()> {
        let mut p = Vec::with_capacity(4 + key.len() + e.encoded_len());
        p.extend_from_slice(&(key.len() as u32).to_le_bytes());
        p.extend_from_slice(key);
        e.encode(&mut p);
        self.w.write_all(&crc32fast::hash(&p).to_le_bytes())?;
        self.w.write_all(&(p.len() as u32).to_le_bytes())?;
        self.w.write_all(&p)?;
        self.bytes_written += 8 + p.len() as u64;
        Ok(())
    }
    pub fn flush(&mut self) -> io::Result<()> { self.w.flush() }
    pub fn sync(&mut self) -> io::Result<()> { self.w.flush()?; self.w.get_ref().sync_data() }
    /// Replay all valid records, stopping at the first torn or corrupt one.
    pub fn replay(path: &Path) -> io::Result<Vec<(Vec<u8>, Entry)>> {
        let mut buf = Vec::new();
        match File::open(path) {
            Ok(mut f) => { f.read_to_end(&mut buf)?; }
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e),
        }
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
        self.w.flush()?;
        let f = OpenOptions::new().create(true).write(true).truncate(true).open(&self.path)?;
        f.sync_all()?;
        let f = OpenOptions::new().append(true).open(&self.path)?;
        self.w = BufWriter::with_capacity(1 << 20, f);
        Ok(())
    }
}
