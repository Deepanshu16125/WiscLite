//! WiscLite: a small LSM key-value store with WiscKey-style key-value separation.
pub mod bloom;
pub mod engine;
pub mod memtable;
pub mod sstable;
pub mod vlog;
pub mod wal;

pub use engine::{Db, Mode, Options, Stats};

/// Pointer into the value log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValuePtr {
    pub offset: u64,
    pub len: u32,
}

/// What a key maps to inside the LSM tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// WiscKey mode: value lives in the vlog.
    Ptr(ValuePtr),
    /// Baseline mode: value stored inline in the tree.
    Inline(Vec<u8>),
    /// Deletion marker.
    Tombstone,
}

impl Entry {
    pub(crate) fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Entry::Ptr(p) => {
                out.push(0);
                out.extend_from_slice(&p.offset.to_le_bytes());
                out.extend_from_slice(&p.len.to_le_bytes());
            }
            Entry::Inline(v) => {
                out.push(1);
                out.extend_from_slice(&(v.len() as u32).to_le_bytes());
                out.extend_from_slice(v);
            }
            Entry::Tombstone => out.push(2),
        }
    }
    /// Decode from buf; returns (entry, bytes consumed).
    pub(crate) fn decode(buf: &[u8]) -> std::io::Result<(Entry, usize)> {
        let bad = || std::io::Error::new(std::io::ErrorKind::InvalidData, "bad entry");
        let tag = *buf.first().ok_or_else(bad)?;
        match tag {
            0 => {
                if buf.len() < 13 { return Err(bad()); }
                let offset = u64::from_le_bytes(buf[1..9].try_into().unwrap());
                let len = u32::from_le_bytes(buf[9..13].try_into().unwrap());
                Ok((Entry::Ptr(ValuePtr { offset, len }), 13))
            }
            1 => {
                if buf.len() < 5 { return Err(bad()); }
                let n = u32::from_le_bytes(buf[1..5].try_into().unwrap()) as usize;
                if buf.len() < 5 + n { return Err(bad()); }
                Ok((Entry::Inline(buf[5..5 + n].to_vec()), 5 + n))
            }
            2 => Ok((Entry::Tombstone, 1)),
            _ => Err(bad()),
        }
    }
    pub(crate) fn encoded_len(&self) -> usize {
        match self { Entry::Ptr(_) => 13, Entry::Inline(v) => 5 + v.len(), Entry::Tombstone => 1 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn entry_roundtrip() {
        for e in [Entry::Ptr(ValuePtr { offset: 99, len: 7 }), Entry::Inline(b"abc".to_vec()), Entry::Tombstone] {
            let mut b = vec![];
            e.encode(&mut b);
            assert_eq!(b.len(), e.encoded_len());
            assert_eq!(Entry::decode(&b).unwrap(), (e.clone(), b.len()));
        }
    }
}
