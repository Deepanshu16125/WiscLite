//! Append-only value log. Each record: crc32 | len | value. Pointers address the value bytes.
use crate::storage::Fs;
use crate::ValuePtr;
use std::io;
use std::path::{Path, PathBuf};

pub struct ValueLog {
    fs: Fs,
    path: PathBuf,
    buf: Vec<u8>,
    end: u64,
    pub bytes_written: u64,
}

impl ValueLog {
    pub fn open(fs: Fs, path: &Path) -> io::Result<Self> {
        if !fs.exists(path) { fs.write(path, &[])?; }
        let end = fs.len(path)?;
        Ok(Self { fs, path: path.to_path_buf(), buf: Vec::with_capacity(1 << 16), end, bytes_written: 0 })
    }
    /// Append a value, returning where its bytes live.
    pub fn append(&mut self, value: &[u8]) -> io::Result<ValuePtr> {
        let crc = crc32fast::hash(value);
        self.buf.extend_from_slice(&crc.to_le_bytes());
        self.buf.extend_from_slice(&(value.len() as u32).to_le_bytes());
        self.buf.extend_from_slice(value);
        let ptr = ValuePtr { offset: self.end + 8, len: value.len() as u32 };
        let n = 8 + value.len() as u64;
        self.end += n;
        self.bytes_written += n;
        Ok(ptr)
    }
    pub fn flush(&mut self) -> io::Result<()> {
        if !self.buf.is_empty() { self.fs.append(&self.path, &self.buf)?; self.buf.clear(); }
        Ok(())
    }
    pub fn sync(&mut self) -> io::Result<()> { self.flush()?; self.fs.sync(&self.path) }
    pub fn read(&mut self, p: ValuePtr) -> io::Result<Vec<u8>> {
        self.flush()?;
        let rec = self.fs.read_at(&self.path, p.offset - 8, 8 + p.len as usize)?;
        let crc = u32::from_le_bytes(rec[0..4].try_into().unwrap());
        let v = rec[8..].to_vec();
        if crc32fast::hash(&v) != crc {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "vlog checksum mismatch"));
        }
        Ok(v)
    }
    pub fn size(&self) -> u64 { self.end }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn append_read_and_checksum() {
        let d = std::env::temp_dir().join(format!("vlog-unit-{}", std::process::id()));
        let _ = std::fs::remove_file(&d);
        let mut v = ValueLog::open(crate::storage::StdFs::new(), &d).unwrap();
        let a = v.append(b"hello").unwrap();
        let b = v.append(b"world!").unwrap();
        assert_eq!(a, ValuePtr { offset: 8, len: 5 });
        assert_eq!(b.offset, 8 + 5 + 8);
        assert_eq!(v.read(b).unwrap(), b"world!");
        v.sync().unwrap();
        let mut bytes = std::fs::read(&d).unwrap();
        bytes[9] ^= 1;
        std::fs::write(&d, bytes).unwrap();
        let mut v = ValueLog::open(crate::storage::StdFs::new(), &d).unwrap();
        assert!(v.read(a).is_err());
    }
}
