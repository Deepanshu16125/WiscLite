//! In-memory sorted buffer: key -> Entry (ValuePtr, inline value, or tombstone).
use crate::Entry;
use std::collections::BTreeMap;

#[derive(Default)]
pub struct MemTable {
    map: BTreeMap<Vec<u8>, Entry>,
    approx_bytes: usize,
}

impl MemTable {
    pub fn new() -> Self { Self::default() }
    pub fn insert(&mut self, key: Vec<u8>, e: Entry) {
        let kl = key.len();
        self.approx_bytes += kl + e.encoded_len();
        if let Some(old) = self.map.insert(key, e) {
            self.approx_bytes = self.approx_bytes.saturating_sub(kl + old.encoded_len());
        }
    }
    pub fn get(&self, key: &[u8]) -> Option<&Entry> { self.map.get(key) }
    pub fn approx_bytes(&self) -> usize { self.approx_bytes }
    pub fn len(&self) -> usize { self.map.len() }
    pub fn is_empty(&self) -> bool { self.map.is_empty() }
    pub fn iter(&self) -> impl Iterator<Item = (&Vec<u8>, &Entry)> { self.map.iter() }
    pub fn range(&self, start: &[u8], end: Option<&[u8]>) -> impl Iterator<Item = (&Vec<u8>, &Entry)> {
        use std::ops::Bound;
        let hi = end.map_or(Bound::Unbounded, |e| Bound::Excluded(e.to_vec()));
        self.map.range((Bound::Included(start.to_vec()), hi))
    }
    pub fn clear(&mut self) { self.map.clear(); self.approx_bytes = 0; }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ordered_iteration_and_size() {
        let mut m = MemTable::new();
        m.insert(b"b".to_vec(), Entry::Inline(b"2".to_vec()));
        m.insert(b"a".to_vec(), Entry::Inline(b"1".to_vec()));
        m.insert(b"a".to_vec(), Entry::Tombstone);
        let ks: Vec<_> = m.iter().map(|(k, _)| k.clone()).collect();
        assert_eq!(ks, vec![b"a".to_vec(), b"b".to_vec()]);
        assert_eq!(m.get(b"a"), Some(&Entry::Tombstone));
        assert_eq!(m.approx_bytes(), (1 + 1) + (1 + 6));
    }
}
