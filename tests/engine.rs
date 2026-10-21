use std::fs::OpenOptions;
use std::io::Write;
use wisclite::sstable::{self, SsTable};
use wisclite::wal::Wal;
use wisclite::{Db, Entry, Mode, Options, ValuePtr};

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("wisclite-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}
fn opts(mode: Mode, mem: usize) -> Options { Options { mode, memtable_bytes: mem, sync_writes: false } }

#[test]
fn put_get_both_modes() {
    for m in [Mode::WiscKey, Mode::Baseline] {
        let d = tmp(&format!("pg{m:?}"));
        let mut db = Db::open(&d, opts(m, 1 << 20)).unwrap();
        db.put(b"k1", b"v1").unwrap();
        db.put(b"k2", b"v2").unwrap();
        assert_eq!(db.get(b"k1").unwrap(), Some(b"v1".to_vec()));
        assert_eq!(db.get(b"nope").unwrap(), None);
    }
}

#[test]
fn overwrite_newest_wins_across_flushes() {
    for m in [Mode::WiscKey, Mode::Baseline] {
        let d = tmp(&format!("ow{m:?}"));
        let mut db = Db::open(&d, opts(m, 1 << 20)).unwrap();
        db.put(b"k", b"old").unwrap();
        db.flush().unwrap();
        db.put(b"k", b"mid").unwrap();
        db.flush().unwrap();
        assert_eq!(db.get(b"k").unwrap(), Some(b"mid".to_vec()));
        db.put(b"k", b"new").unwrap(); // memtable beats sstables
        assert_eq!(db.get(b"k").unwrap(), Some(b"new".to_vec()));
    }
}

#[test]
fn tombstones_shadow_older_tables() {
    for m in [Mode::WiscKey, Mode::Baseline] {
        let d = tmp(&format!("ts{m:?}"));
        let mut db = Db::open(&d, opts(m, 1 << 20)).unwrap();
        db.put(b"gone", b"x").unwrap();
        db.flush().unwrap();
        db.delete(b"gone").unwrap();
        assert_eq!(db.get(b"gone").unwrap(), None);
        db.flush().unwrap(); // tombstone now lives in an sstable
        drop(db);
        let mut db = Db::open(&d, opts(m, 1 << 20)).unwrap();
        assert_eq!(db.get(b"gone").unwrap(), None);
        db.put(b"gone", b"back").unwrap();
        assert_eq!(db.get(b"gone").unwrap(), Some(b"back".to_vec()));
    }
}

#[test]
fn crash_recovery_replays_wal() {
    for m in [Mode::WiscKey, Mode::Baseline] {
        let d = tmp(&format!("cr{m:?}"));
        {
            let mut db = Db::open(&d, opts(m, 1 << 20)).unwrap();
            db.put(b"a", b"1").unwrap();
            db.flush().unwrap();
            db.put(b"b", b"2").unwrap();
            db.delete(b"a").unwrap();
            // dropped without flush: simulates a crash; only the WAL has b and the delete
        }
        let mut db = Db::open(&d, opts(m, 1 << 20)).unwrap();
        assert_eq!(db.get(b"b").unwrap(), Some(b"2".to_vec()));
        assert_eq!(db.get(b"a").unwrap(), None);
    }
}

#[test]
fn torn_wal_tail_is_ignored() {
    let d = tmp("torn");
    {
        let mut db = Db::open(&d, opts(Mode::WiscKey, 1 << 20)).unwrap();
        db.put(b"ok", b"fine").unwrap();
    }
    let mut f = OpenOptions::new().append(true).open(d.join("wal")).unwrap();
    f.write_all(&[0xde, 0xad, 0xbe, 0xef, 50, 0, 0, 0, 1, 2]).unwrap(); // half a record
    drop(f);
    let mut db = Db::open(&d, opts(Mode::WiscKey, 1 << 20)).unwrap();
    assert_eq!(db.get(b"ok").unwrap(), Some(b"fine".to_vec()));
}

#[test]
fn corrupt_wal_record_stops_replay() {
    let d = tmp("crc");
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join("wal");
    {
        let mut w = Wal::open(&p).unwrap();
        w.append(b"x", &Entry::Inline(b"1".to_vec())).unwrap();
        w.append(b"y", &Entry::Inline(b"2".to_vec())).unwrap();
        w.flush().unwrap();
    }
    let mut bytes = std::fs::read(&p).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff; // flip a bit in the second record
    std::fs::write(&p, &bytes).unwrap();
    let r = Wal::replay(&p).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].0, b"x");
}

#[test]
fn sstable_sorted_and_binary_search() {
    let d = tmp("sst");
    std::fs::create_dir_all(&d).unwrap();
    let mut entries: Vec<(Vec<u8>, Entry)> = (0..1000u32)
        .map(|i| (format!("k{i:05}").into_bytes(), Entry::Ptr(ValuePtr { offset: i as u64 * 10, len: i })))
        .collect();
    entries.push((b"zz".to_vec(), Entry::Tombstone));
    let p = d.join("1.sst");
    sstable::write(&p, entries.iter().map(|(k, e)| (k, e))).unwrap();
    let mut t = SsTable::open(&p).unwrap();
    assert_eq!(t.len(), 1001);
    let keys: Vec<_> = t.keys().cloned().collect();
    assert!(keys.windows(2).all(|w| w[0] < w[1]), "keys must be sorted");
    assert_eq!(t.get(b"k00500").unwrap(), Some(Entry::Ptr(ValuePtr { offset: 5000, len: 500 })));
    assert_eq!(t.get(b"zz").unwrap(), Some(Entry::Tombstone));
    assert_eq!(t.get(b"k99999").unwrap(), None);
    assert_eq!(t.get(b"a").unwrap(), None);
}

#[test]
fn auto_flush_and_reopen_many_keys() {
    for m in [Mode::WiscKey, Mode::Baseline] {
        let d = tmp(&format!("many{m:?}"));
        let val = vec![7u8; 512];
        {
            let mut db = Db::open(&d, opts(m, 16 << 10)).unwrap();
            for i in 0..2000u32 { db.put(format!("key{i}").as_bytes(), &val).unwrap(); }
            assert!(db.stats().flushes > 0);
        }
        let mut db = Db::open(&d, opts(m, 16 << 10)).unwrap();
        for i in (0..2000u32).step_by(37) {
            assert_eq!(db.get(format!("key{i}").as_bytes()).unwrap(), Some(val.clone()));
        }
    }
}

#[test]
fn mode_is_persisted() {
    let d = tmp("mode");
    { Db::open(&d, opts(Mode::Baseline, 1 << 20)).unwrap(); }
    let db = Db::open(&d, opts(Mode::WiscKey, 1 << 20)).unwrap();
    assert_eq!(db.mode(), Mode::Baseline);
}

#[test]
fn wisckey_writes_less_to_tree() {
    let mut wa = vec![];
    for m in [Mode::Baseline, Mode::WiscKey] {
        let d = tmp(&format!("wa{m:?}"));
        let mut db = Db::open(&d, opts(m, 256 << 10)).unwrap();
        for i in 0..3000u32 { db.put(format!("k{i}").as_bytes(), &[1u8; 1024]).unwrap(); }
        db.flush().unwrap();
        wa.push(db.stats());
    }
    assert!(wa[1].sst_bytes * 10 < wa[0].sst_bytes, "tree should be >10x smaller with separation");
    assert!(wa[1].write_amplification < wa[0].write_amplification);
}
