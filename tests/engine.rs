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
fn opts(mode: Mode, mem: usize) -> Options { Options { mode, memtable_bytes: mem, sync_writes: false, ..Default::default() } }

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
        let mut w = Wal::open(wisclite::storage::StdFs::new(), &p).unwrap();
        w.append(b"x", &Entry::Inline(b"1".to_vec())).unwrap();
        w.append(b"y", &Entry::Inline(b"2".to_vec())).unwrap();
        w.flush().unwrap();
    }
    let mut bytes = std::fs::read(&p).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff; // flip a bit in the second record
    std::fs::write(&p, &bytes).unwrap();
    let r = Wal::replay(&wisclite::storage::StdFs::new(), &p).unwrap();
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
    sstable::write(&wisclite::storage::StdFs::new(), &p, entries.iter().map(|(k, e)| (k, e))).unwrap();
    let mut t = SsTable::open(wisclite::storage::StdFs::new(), &p).unwrap();
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

#[test]
fn bloom_filters_skip_tables_and_report_fpr() {
    let d = tmp("bloom");
    let mut db = Db::open(&d, opts(Mode::WiscKey, 8 << 10)).unwrap();
    for i in 0..3000u32 { db.put(format!("key{i}").as_bytes(), b"v").unwrap(); }
    db.flush().unwrap();
    for i in 0..2000u32 { assert_eq!(db.get(format!("missing{i}").as_bytes()).unwrap(), None); }
    let s = db.stats();
    assert!(s.bloom_negatives > 0);
    assert!(s.bloom_fpr < 0.05, "fpr {}", s.bloom_fpr);
    assert_eq!(db.get(b"key1234").unwrap(), Some(b"v".to_vec()));
}

fn small(m: Mode) -> Options { Options { mode: m, memtable_bytes: 4 << 10, sync_writes: false, compaction_fanin: 4 } }

#[test]
fn compaction_merges_tables_and_keeps_data() {
    for m in [Mode::WiscKey, Mode::Baseline] {
        let d = tmp(&format!("cmp{m:?}"));
        let mut db = Db::open(&d, small(m)).unwrap();
        for i in 0..3000u32 { db.put(format!("k{i:05}").as_bytes(), format!("v{i}").as_bytes()).unwrap(); }
        for i in 0..1500u32 { db.put(format!("k{i:05}").as_bytes(), format!("new{i}").as_bytes()).unwrap(); }
        db.flush().unwrap();
        let s = db.stats();
        assert!(s.compactions > 0, "{m:?}: expected compactions");
        assert!(s.compaction_bytes > 0);
        assert!(s.sstables < s.flushes as usize, "tables should be merged");
        assert!(s.physical_bytes >= s.wal_bytes + s.sst_bytes + s.compaction_bytes);
        drop(db);
        let mut db = Db::open(&d, small(m)).unwrap();
        for i in (0..3000u32).step_by(7) {
            let want = if i < 1500 { format!("new{i}") } else { format!("v{i}") };
            assert_eq!(db.get(format!("k{i:05}").as_bytes()).unwrap(), Some(want.into_bytes()));
        }
    }
}

#[test]
fn full_compaction_drops_tombstones() {
    let d = tmp("tomb-drop");
    let mut db = Db::open(&d, opts(Mode::WiscKey, 1 << 20)).unwrap();
    for i in 0..100u32 { db.put(format!("k{i}").as_bytes(), b"x").unwrap(); }
    db.flush().unwrap();
    for i in 0..50u32 { db.delete(format!("k{i}").as_bytes()).unwrap(); }
    db.flush().unwrap();
    db.compact_all().unwrap();
    let s = db.stats();
    assert_eq!(s.sstables, 1);
    assert_eq!(s.tombstones_dropped, 50);
    assert_eq!(db.get(b"k10").unwrap(), None);
    assert_eq!(db.get(b"k60").unwrap(), Some(b"x".to_vec()));
    assert_eq!(db.scan(b"", None).unwrap().len(), 50);
}

#[test]
fn partial_compaction_keeps_tombstones_that_shadow_older_tables() {
    let d = tmp("tomb-keep");
    // fanin 2 so the two newest tables merge while an older one remains
    let o = Options { mode: Mode::Baseline, memtable_bytes: 1 << 20, sync_writes: false, compaction_fanin: 0 };
    let mut db = Db::open(&d, o.clone()).unwrap();
    db.put(b"a", b"old").unwrap();
    db.flush().unwrap();
    for i in 0..200u32 { db.put(format!("pad{i}").as_bytes(), &[0u8; 100]).unwrap(); }
    db.flush().unwrap();
    db.delete(b"a").unwrap();
    db.flush().unwrap();
    drop(db);
    let mut db = Db::open(&d, Options { compaction_fanin: 2, ..o }).unwrap();
    db.put(b"z", b"1").unwrap();
    db.flush().unwrap(); // triggers merges of same-tier newest tables
    assert_eq!(db.get(b"a").unwrap(), None, "tombstone must still hide the old value");
}

#[test]
fn scan_is_ordered_merges_layers_and_skips_tombstones() {
    for m in [Mode::WiscKey, Mode::Baseline] {
        let d = tmp(&format!("scan{m:?}"));
        let mut db = Db::open(&d, opts(m, 1 << 20)).unwrap();
        for k in ["b", "d", "f", "h"] { db.put(k.as_bytes(), b"t1").unwrap(); }
        db.flush().unwrap();
        db.put(b"d", b"t2").unwrap();
        db.put(b"e", b"t2").unwrap();
        db.flush().unwrap();
        db.delete(b"f").unwrap(); // memtable tombstone
        db.put(b"c", b"mem").unwrap();
        let got = db.scan(b"c", Some(b"h")).unwrap();
        let want: Vec<(Vec<u8>, Vec<u8>)> = [("c", "mem"), ("d", "t2"), ("e", "t2")]
            .iter().map(|(k, v)| (k.as_bytes().to_vec(), v.as_bytes().to_vec())).collect();
        assert_eq!(got, want);
        assert_eq!(db.scan(b"", None).unwrap().len(), 5); // b c d e h
        assert!(db.scan(b"x", None).unwrap().is_empty());
    }
}

#[test]
fn gc_reclaims_dead_values_and_survives_reopen() {
    let d = tmp("gc");
    let mut db = Db::open(&d, opts(Mode::WiscKey, 64 << 10)).unwrap();
    let v = vec![9u8; 1024];
    for round in 0..4u8 {
        for i in 0..500u32 { let mut x = v.clone(); x[0] = round; db.put(format!("k{i}").as_bytes(), &x).unwrap(); }
    }
    for i in 0..100u32 { db.delete(format!("k{i}").as_bytes()).unwrap(); }
    let r = db.gc().unwrap();
    assert_eq!(r.live_keys, 400);
    // 2000 values written, 400 live -> at least ~75% of the vlog reclaimed
    assert!(r.reclaimed_bytes > 1_500_000, "reclaimed {}", r.reclaimed_bytes);
    assert_eq!(db.get(b"k250").unwrap().unwrap()[0], 3);
    db.put(b"after", b"gc").unwrap();
    drop(db);
    let mut db = Db::open(&d, opts(Mode::WiscKey, 64 << 10)).unwrap();
    assert_eq!(db.get(b"k499").unwrap().unwrap()[0], 3);
    assert_eq!(db.get(b"k5").unwrap(), None);
    assert_eq!(db.get(b"after").unwrap(), Some(b"gc".to_vec()));
    assert!(!d.join("vlog").exists(), "old vlog generation should be deleted");
    assert_eq!(db.scan(b"", None).unwrap().len(), 401);
}

#[test]
fn unlisted_files_are_cleaned_on_open() {
    let d = tmp("orphan");
    { let mut db = Db::open(&d, opts(Mode::WiscKey, 1 << 20)).unwrap(); db.put(b"a", b"1").unwrap(); db.flush().unwrap(); }
    std::fs::write(d.join("999999.sst"), b"half-written garbage").unwrap();
    std::fs::write(d.join("vlog.7"), b"orphan").unwrap();
    let mut db = Db::open(&d, opts(Mode::WiscKey, 1 << 20)).unwrap();
    assert!(!d.join("999999.sst").exists() && !d.join("vlog.7").exists());
    assert_eq!(db.get(b"a").unwrap(), Some(b"1".to_vec()));
}

// Same engine, in-memory storage (what the WebAssembly build uses).
#[test]
fn memfs_engine_full_cycle_and_crash_recovery() {
    use wisclite::storage::{MemFs, Storage};
    use std::sync::Arc;
    for m in [Mode::WiscKey, Mode::Baseline] {
        let mem = MemFs::shared();
        let fs: wisclite::storage::Fs = Arc::new(mem.clone());
        let o = Options { mode: m, memtable_bytes: 8 << 10, sync_writes: false, compaction_fanin: 4 };
        {
            let mut db = Db::open_with(fs.clone(), "db", o.clone()).unwrap();
            for i in 0..2000u32 { db.put(format!("k{i:05}").as_bytes(), &[7u8; 256]).unwrap(); }
            db.delete(b"k00010").unwrap();
            assert!(db.stats().compactions > 0);
            assert_eq!(db.scan(b"k00000", Some(b"k00020")).unwrap().len(), 19);
            db.gc().unwrap();
            db.put(b"tail", b"in-wal-only").unwrap(); // not flushed: lives only in the WAL
        } // "crash": drop without flush
        assert!(mem.list(std::path::Path::new("db")).unwrap().iter().any(|n| n == "wal"));
        let mut db = Db::open_with(fs.clone(), "db", o).unwrap();
        assert_eq!(db.get(b"tail").unwrap(), Some(b"in-wal-only".to_vec()));
        assert_eq!(db.get(b"k00010").unwrap(), None);
        assert_eq!(db.get(b"k01999").unwrap(), Some(vec![7u8; 256]));
        let files = db.list_files().unwrap();
        assert!(files.iter().any(|(n, _)| n.ends_with(".sst")));
    }
}
