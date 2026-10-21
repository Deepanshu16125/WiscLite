//! Append-only value log. Each record: crc32 | len | value. Pointers address the value bytes.
use crate::ValuePtr;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write, BufWriter};
use std::path::Path;

pub struct ValueLog {
    w: BufWriter<File>,
    r: File,
    end: u64,
    pub bytes_written: u64,
}

impl ValueLog {
    pub fn open(path: &Path) -> io::Result<Self> {
        let f = OpenOptions::new().create(true).append(true).read(true).open(path)?;
        let end = f.metadata()?.len();
        let r = File::open(path)?;
        Ok(Self { w: BufWriter::with_capacity(1 << 20, f), r, end, bytes_written: 0 })
    }
    /// Append a value, returning where its bytes live.
    pub fn append(&mut self, value: &[u8]) -> io::Result<ValuePtr> {
        let crc = crc32fast::hash(value);
        self.w.write_all(&crc.to_le_bytes())?;
        self.w.write_all(&(value.len() as u32).to_le_bytes())?;
        self.w.write_all(value)?;
        let ptr = ValuePtr { offset: self.end + 8, len: value.len() as u32 };
        let n = 8 + value.len() as u64;
        self.end += n;
        self.bytes_written += n;
        Ok(ptr)
    }
    pub fn flush(&mut self) -> io::Result<()> { self.w.flush() }
    pub fn sync(&mut self) -> io::Result<()> { self.w.flush()?; self.w.get_ref().sync_data() }
    pub fn read(&mut self, p: ValuePtr) -> io::Result<Vec<u8>> {
        self.w.flush()?;
        self.r.seek(SeekFrom::Start(p.offset - 8))?;
        let mut hdr = [0u8; 8];
        self.r.read_exact(&mut hdr)?;
        let crc = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
        let mut v = vec![0u8; p.len as usize];
        self.r.read_exact(&mut v)?;
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
        let mut v = ValueLog::open(&d).unwrap();
        let a = v.append(b"hello").unwrap();
        let b = v.append(b"world!").unwrap();
        assert_eq!(a, ValuePtr { offset: 8, len: 5 });
        assert_eq!(b.offset, 8 + 5 + 8);
        assert_eq!(v.read(b).unwrap(), b"world!");
        v.sync().unwrap();
        let mut bytes = std::fs::read(&d).unwrap();
        bytes[9] ^= 1;
        std::fs::write(&d, bytes).unwrap();
        let mut v = ValueLog::open(&d).unwrap();
        assert!(v.read(a).is_err());
    }
}
