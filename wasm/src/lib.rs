//! WebAssembly bindings: the real WiscLite engine on an in-memory file system.
use std::path::Path;
use std::sync::Arc;
use serde_json::json;
use wasm_bindgen::prelude::*;
use wisclite::storage::{Fs, MemFs, Storage};
use wisclite::{Db, Mode, Options};

fn err(e: std::io::Error) -> JsValue { JsValue::from_str(&e.to_string()) }

#[wasm_bindgen]
pub struct WiscDb {
    mem: Arc<MemFs>,
    opts: Options,
    db: Option<Db>,
    rng: u64,
    keys: Vec<String>,
}

impl WiscDb {
    fn db(&mut self) -> &mut Db { self.db.as_mut().expect("db open") }
    fn next(&mut self) -> u64 { self.rng ^= self.rng << 13; self.rng ^= self.rng >> 7; self.rng ^= self.rng << 17; self.rng }
}

#[wasm_bindgen]
impl WiscDb {
    /// mode: "wisckey" or "baseline".
    #[wasm_bindgen(constructor)]
    pub fn new(mode: &str, memtable_bytes: u32) -> Result<WiscDb, JsValue> {
        let mode = if mode == "baseline" { Mode::Baseline } else { Mode::WiscKey };
        let opts = Options { mode, memtable_bytes: memtable_bytes as usize, sync_writes: false, compaction_fanin: 4 };
        let mem = MemFs::shared();
        let fs: Fs = Arc::new(mem.clone());
        let db = Db::open_with(fs, "db", opts.clone()).map_err(err)?;
        Ok(WiscDb { mem, opts, db: Some(db), rng: 0x9E37_79B9_7F4A_7C15, keys: Vec::new() })
    }
    pub fn put(&mut self, key: &str, value: &str) -> Result<(), JsValue> { self.db().put(key.as_bytes(), value.as_bytes()).map_err(err) }
    pub fn get(&mut self, key: &str) -> Result<Option<String>, JsValue> {
        Ok(self.db().get(key.as_bytes()).map_err(err)?.map(|v| String::from_utf8_lossy(&v).into_owned()))
    }
    pub fn del(&mut self, key: &str) -> Result<(), JsValue> { self.db().delete(key.as_bytes()).map_err(err) }
    /// JSON array of [key, value] (values longer than 40 bytes are summarised).
    pub fn scan(&mut self, start: &str, end: &str) -> Result<String, JsValue> {
        let end = if end.is_empty() { None } else { Some(end.as_bytes()) };
        let rows = self.db().scan(start.as_bytes(), end).map_err(err)?;
        let out: Vec<_> = rows.iter().map(|(k, v)| {
            let s = String::from_utf8_lossy(v);
            json!([String::from_utf8_lossy(k), if v.len() > 40 { format!("{}… ({} B)", &s[..s.char_indices().nth(12).map_or(s.len(), |c| c.0)], v.len()) } else { s.into_owned() }])
        }).collect();
        Ok(json!(out).to_string())
    }
    pub fn flush(&mut self) -> Result<(), JsValue> { self.db().flush().map_err(err) }
    pub fn compact(&mut self) -> Result<(), JsValue> { self.db().compact_all().map_err(err) }
    pub fn gc(&mut self) -> Result<String, JsValue> { Ok(json!(self.db().gc().map_err(err)?).to_string()) }
    /// Drop the engine without flushing (the memtable is lost), then reopen on the
    /// same files: the memtable is rebuilt by replaying the real WAL.
    pub fn crash_and_recover(&mut self) -> Result<String, JsValue> {
        let prev = self.db().stats();
        let before = prev.memtable_entries;
        let wal_bytes = self.mem.len(Path::new("db/wal")).unwrap_or(0);
        self.db = None; // crash
        let mut db = Db::open_with(Arc::new(self.mem.clone()), "db", self.opts.clone()).map_err(err)?;
        db.adopt_counters(&prev);
        let after = db.stats().memtable_entries;
        self.db = Some(db);
        Ok(json!({"memtable_before": before, "wal_bytes": wal_bytes, "replayed_entries": after}).to_string())
    }
    pub fn stats(&mut self) -> String {
        let disk = self.db().disk_usage().unwrap_or(0);
        let db = self.db();
        json!({"mode": db.mode().as_str(), "disk_bytes": disk, "stats": db.stats()}).to_string()
    }
    /// JSON array of [name, size] for the engine's real files.
    pub fn list_files(&mut self) -> String { json!(self.db().list_files().unwrap_or_default()).to_string() }
    /// Bulk benchmark helper: n puts of random 16-byte keys with value_size random bytes.
    pub fn put_random(&mut self, n: u32, value_size: u32) -> Result<(), JsValue> {
        let mut val = vec![0u8; value_size as usize];
        for _ in 0..n {
            let k = format!("key{:016x}", self.next());
            for c in val.chunks_mut(8) { let r = self.next().to_le_bytes(); c.copy_from_slice(&r[..c.len()]); }
            self.db().put(k.as_bytes(), &val).map_err(err)?;
            self.keys.push(k);
        }
        Ok(())
    }
    /// n gets of random keys previously written by put_random; returns hits.
    pub fn get_random(&mut self, n: u32) -> Result<u32, JsValue> {
        let mut hits = 0;
        for _ in 0..n {
            if self.keys.is_empty() { break; }
            let i = (self.next() % self.keys.len() as u64) as usize;
            let k = self.keys[i].clone();
            if self.db().get(k.as_bytes()).map_err(err)?.is_some() { hits += 1; }
        }
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wasm_api_roundtrip() {
        let mut d = WiscDb::new("wisckey", 4096).unwrap();
        d.put("a", "1").unwrap();
        d.put_random(500, 256).unwrap();
        assert_eq!(d.get("a").unwrap().as_deref(), Some("1"));
        d.del("a").unwrap();
        let r: serde_json::Value = serde_json::from_str(&d.crash_and_recover().unwrap()).unwrap();
        assert!(r["replayed_entries"].as_u64().unwrap() > 0);
        assert_eq!(d.get("a").unwrap(), None);
        assert!(d.list_files().contains("vlog"));
        let s: serde_json::Value = serde_json::from_str(&d.stats()).unwrap();
        assert!(s["stats"]["flushes"].as_u64().unwrap() > 0, "{s}");
        assert_eq!(d.get_random(500).unwrap(), 500);
        assert!(s["stats"]["logical_bytes"].as_u64().unwrap() > 128_000, "counters survive crash");
    }
}
