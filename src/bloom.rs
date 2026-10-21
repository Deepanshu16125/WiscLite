//! Bloom filter: ~10 bits/key, 7 probes via double hashing (≈1% false positives).
pub struct Bloom {
    bits: Vec<u8>,
    k: u8,
}

fn fnv1a(key: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key { h ^= *b as u64; h = h.wrapping_mul(0x100_0000_01b3); }
    // final avalanche (splitmix64) so the two halves are independent
    h ^= h >> 30; h = h.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h ^= h >> 27; h = h.wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^ (h >> 31)
}

impl Bloom {
    pub fn build<'a>(keys: impl ExactSizeIterator<Item = &'a [u8]>, bits_per_key: usize) -> Self {
        let n = keys.len().max(1);
        let m = (n * bits_per_key).max(64).div_ceil(8) * 8;
        let k = ((bits_per_key as f64) * 0.69).round().clamp(1.0, 30.0) as u8;
        let mut b = Bloom { bits: vec![0; m / 8], k };
        for key in keys { b.add(key); }
        b
    }
    fn probes(&self, key: &[u8]) -> impl Iterator<Item = usize> + '_ {
        let h = fnv1a(key);
        let (h1, h2) = (h as u32 as u64, (h >> 32) | 1);
        let m = self.bits.len() as u64 * 8;
        (0..self.k as u64).map(move |i| (h1.wrapping_add(i.wrapping_mul(h2)) % m) as usize)
    }
    fn add(&mut self, key: &[u8]) {
        let ps: Vec<usize> = self.probes(key).collect();
        for p in ps { self.bits[p / 8] |= 1 << (p % 8); }
    }
    pub fn may_contain(&self, key: &[u8]) -> bool {
        self.probes(key).all(|p| self.bits[p / 8] & (1 << (p % 8)) != 0)
    }
    pub fn encode(&self, out: &mut Vec<u8>) { out.push(self.k); out.extend_from_slice(&self.bits); }
    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() < 2 { return None; }
        Some(Bloom { k: b[0], bits: b[1..].to_vec() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn no_false_negatives_and_low_fpr() {
        let keys: Vec<Vec<u8>> = (0..10_000).map(|i| format!("key{i}").into_bytes()).collect();
        let b = Bloom::build(keys.iter().map(|k| k.as_slice()), 10);
        assert!(keys.iter().all(|k| b.may_contain(k)));
        let fp = (0..10_000).filter(|i| b.may_contain(format!("other{i}").as_bytes())).count();
        assert!(fp < 300, "fpr too high: {fp}/10000");
        let mut enc = vec![]; b.encode(&mut enc);
        let d = Bloom::decode(&enc).unwrap();
        assert!(keys.iter().all(|k| d.may_contain(k)));
    }
}
