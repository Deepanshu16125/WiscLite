use rand::{rngs::StdRng, Rng, SeedableRng};
use serde::Serialize;
use std::time::Instant;
use wisclite::{Db, GcReport, Mode, Options, Stats};

fn usage() -> ! {
    eprintln!("usage: wisclite [--dir DIR] [--baseline] <command>
  put KEY VALUE | get KEY | del KEY
  scan START [END] [--limit N]      keys in [START, END), END omitted = unbounded
  gc                                value-log garbage collection
  compact                           merge all sstables (drops tombstones)
  stats
  serve [--port 7878]               dashboard + JSON API on http://127.0.0.1:PORT
  bench [--n 50000] [--value-size 1024] [--gets 10000] [--memtable BYTES] [--fanin 4] [--out bench.json]");
    std::process::exit(2)
}

#[derive(Serialize)]
struct BenchMode {
    mode: Mode,
    ops: usize,
    value_size: usize,
    put_secs: f64,
    puts_per_sec: f64,
    mb_per_sec: f64,
    get_p50_us: f64,
    get_p99_us: f64,
    get_mean_us: f64,
    miss_get_p50_us: f64,
    /// Range scan of ["key8", "key9"): ~1/16 of keys, values resolved.
    scan_us: f64,
    scan_rows: usize,
    disk_bytes: u64,
    stats: Stats,
    /// After overwriting 25% of keys, run GC.
    gc: GcReport,
    gc_secs: f64,
}

#[derive(Serialize)]
struct Bench { generated_at_unix: u64, machine: String, memtable_bytes: usize, compaction_fanin: usize, results: Vec<BenchMode> }

fn pct(v: &[f64], p: f64) -> f64 { v[((v.len() as f64 - 1.0) * p).round() as usize] }
fn sorted(mut v: Vec<f64>) -> Vec<f64> { v.sort_by(|a, b| a.partial_cmp(b).unwrap()); v }

fn bench_one(mode: Mode, n: usize, vs: usize, gets: usize, mem: usize, fanin: usize) -> std::io::Result<BenchMode> {
    let dir = std::env::temp_dir().join(format!("wisclite-bench-{}-{}", mode.as_str(), std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut db = Db::open(&dir, Options { mode, memtable_bytes: mem, sync_writes: false, compaction_fanin: fanin })?;
    let mut rng = StdRng::seed_from_u64(42);
    let keys: Vec<Vec<u8>> = (0..n).map(|_| format!("key{:016x}", rng.random::<u64>()).into_bytes()).collect();
    let mut val = vec![0u8; vs];
    let t = Instant::now();
    for k in &keys {
        rng.fill(&mut val[..]);
        db.put(k, &val)?;
    }
    db.flush()?;
    let put_secs = t.elapsed().as_secs_f64();
    let stats_after_load = db.stats();
    let mut lat = Vec::with_capacity(gets);
    for _ in 0..gets {
        let k = &keys[rng.random_range(0..n)];
        let t = Instant::now();
        let v = db.get(k)?;
        lat.push(t.elapsed().as_secs_f64() * 1e6);
        assert!(v.is_some());
    }
    let mut miss = Vec::with_capacity(gets);
    for i in 0..gets {
        let k = format!("nokey{i:012}");
        let t = Instant::now();
        assert!(db.get(k.as_bytes())?.is_none());
        miss.push(t.elapsed().as_secs_f64() * 1e6);
    }
    let t = Instant::now();
    let scan_rows = db.scan(b"key8", Some(b"key9"))?.len();
    let scan_us = t.elapsed().as_secs_f64() * 1e6;
    let lat = sorted(lat);
    let miss = sorted(miss);
    // Stats for the load phase (WA) + Bloom counters from the read phase.
    let read_stats = db.stats();
    let mut stats = stats_after_load;
    stats.bloom_negatives = read_stats.bloom_negatives;
    stats.bloom_false_positives = read_stats.bloom_false_positives;
    stats.bloom_fpr = read_stats.bloom_fpr;
    let disk_bytes = db.disk_usage()?;
    for k in keys.iter().take(n / 4) { rng.fill(&mut val[..]); db.put(k, &val)?; }
    let t = Instant::now();
    let gc = db.gc()?;
    let gc_secs = t.elapsed().as_secs_f64();
    assert!(db.get(&keys[n - 1])?.is_some());
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(BenchMode {
        mode, ops: n, value_size: vs, put_secs,
        puts_per_sec: n as f64 / put_secs,
        mb_per_sec: stats.logical_bytes as f64 / put_secs / 1e6,
        get_p50_us: pct(&lat, 0.5), get_p99_us: pct(&lat, 0.99),
        get_mean_us: lat.iter().sum::<f64>() / lat.len() as f64,
        miss_get_p50_us: pct(&miss, 0.5),
        scan_us, scan_rows,
        disk_bytes, stats, gc, gc_secs,
    })
}

fn main() -> std::io::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut take = |flag: &str| -> Option<String> {
        let i = args.iter().position(|a| a == flag)?;
        args.remove(i);
        if i < args.len() { Some(args.remove(i)) } else { usage() }
    };
    let num = |s: Option<String>, d: usize| s.map(|s| s.parse().unwrap_or_else(|_| usage())).unwrap_or(d);
    let dir = take("--dir").unwrap_or_else(|| "wisclite-data".into());
    let n = num(take("--n"), 50_000);
    let vs = num(take("--value-size"), 1024);
    let gets = num(take("--gets"), 10_000);
    let mem = num(take("--memtable"), 4 << 20);
    let fanin = num(take("--fanin"), 4);
    let limit = num(take("--limit"), 100);
    let port = num(take("--port"), 7878);
    let out = take("--out").unwrap_or_else(|| "bench.json".into());
    let baseline = if let Some(i) = args.iter().position(|a| a == "--baseline") { args.remove(i); true } else { false };
    let opts = Options { mode: if baseline { Mode::Baseline } else { Mode::WiscKey }, compaction_fanin: fanin, ..Default::default() };
    match args.iter().map(|s| s.as_str()).collect::<Vec<_>>().as_slice() {
        ["put", k, v] => { let mut db = Db::open(&dir, opts)?; db.put(k.as_bytes(), v.as_bytes())?; println!("OK"); }
        ["get", k] => {
            let mut db = Db::open(&dir, opts)?;
            match db.get(k.as_bytes())? { Some(v) => println!("{}", String::from_utf8_lossy(&v)), None => { println!("(not found)"); std::process::exit(1) } }
        }
        ["del", k] => { let mut db = Db::open(&dir, opts)?; db.delete(k.as_bytes())?; println!("OK"); }
        ["scan", rest @ ..] if rest.len() <= 2 => {
            let mut db = Db::open(&dir, opts)?;
            let start = rest.first().copied().unwrap_or("");
            let rows = db.scan(start.as_bytes(), rest.get(1).map(|e| e.as_bytes()))?;
            for (k, v) in rows.iter().take(limit) { println!("{}\t{}", String::from_utf8_lossy(k), String::from_utf8_lossy(v)); }
            eprintln!("({} keys{})", rows.len(), if rows.len() > limit { format!(", showing {limit}") } else { String::new() });
        }
        ["gc"] => {
            let mut db = Db::open(&dir, opts)?;
            let r = db.gc()?;
            println!("live keys: {}\nbefore: {} bytes\nafter:  {} bytes\nreclaimed: {} bytes ({:.1}%)", r.live_keys, r.bytes_before, r.bytes_after,
                r.reclaimed_bytes, if r.bytes_before > 0 { 100.0 * r.reclaimed_bytes as f64 / r.bytes_before as f64 } else { 0.0 });
        }
        ["compact"] => { let mut db = Db::open(&dir, opts)?; db.compact_all()?; println!("OK ({} sstables)", db.stats().sstables); }
        ["stats"] => {
            let db = Db::open(&dir, opts)?;
            println!("mode: {:?}\n{}\ndisk bytes: {}", db.mode(), serde_json::to_string_pretty(&db.stats()).unwrap(), db.disk_usage()?);
        }
        ["serve"] => {
            let db = Db::open(&dir, opts)?;
            let l = std::net::TcpListener::bind(("127.0.0.1", port as u16))?;
            println!("WiscLite ({}) serving http://127.0.0.1:{port}/  — data dir {dir}", db.mode().as_str());
            wisclite::server::serve(l, db, std::env::current_dir()?)?;
        }
        ["bench"] => {
            let mut results = Vec::new();
            for m in [Mode::Baseline, Mode::WiscKey] {
                let r = bench_one(m, n, vs, gets, mem, fanin)?;
                let s = &r.stats;
                println!("{:<9} puts/s {:>7.0} {:>6.1} MB/s  WA {:.3} (wal {:.2} vlog {:.2} flush {:.2} compact {:.2})  compactions {}  get p50 {:.1}us p99 {:.1}us  miss p50 {:.1}us  bloom FPR {:.4}  scan {} rows {:.1}ms  gc reclaimed {:.1} MB",
                    m.as_str(), r.puts_per_sec, r.mb_per_sec, s.write_amplification,
                    s.wal_bytes as f64 / s.logical_bytes as f64, s.vlog_bytes as f64 / s.logical_bytes as f64,
                    s.sst_bytes as f64 / s.logical_bytes as f64, s.compaction_bytes as f64 / s.logical_bytes as f64,
                    s.compactions, r.get_p50_us, r.get_p99_us, r.miss_get_p50_us, s.bloom_fpr, r.scan_rows, r.scan_us / 1e3, r.gc.reclaimed_bytes as f64 / 1e6);
                results.push(r);
            }
            let machine = std::fs::read_to_string("/proc/cpuinfo").ok()
                .and_then(|c| c.lines().find(|l| l.starts_with("model name")).map(|l| l.split(':').nth(1).unwrap().trim().to_string()))
                .unwrap_or_else(|| std::env::consts::OS.into());
            let b = Bench { generated_at_unix: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(), machine, memtable_bytes: mem, compaction_fanin: fanin, results };
            std::fs::write(&out, serde_json::to_string_pretty(&b).unwrap())?;
            println!("wrote {out}");
        }
        _ => usage(),
    }
    Ok(())
}
