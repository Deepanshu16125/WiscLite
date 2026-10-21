use rand::{rngs::StdRng, Rng, SeedableRng};
use serde::Serialize;
use std::time::Instant;
use wisclite::{Db, Mode, Options, Stats};

fn usage() -> ! {
    eprintln!("usage:\n  wisclite [--dir DIR] [--baseline] put KEY VALUE\n  wisclite [--dir DIR] get KEY\n  wisclite [--dir DIR] del KEY\n  wisclite [--dir DIR] stats\n  wisclite bench [--n N] [--value-size B] [--gets G] [--out bench.json]");
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
    disk_bytes: u64,
    stats: Stats,
}

#[derive(Serialize)]
struct Bench { generated_at_unix: u64, machine: String, memtable_bytes: usize, results: Vec<BenchMode> }

fn pct(v: &[f64], p: f64) -> f64 { v[((v.len() as f64 - 1.0) * p).round() as usize] }

fn bench_one(mode: Mode, n: usize, vs: usize, gets: usize, mem: usize) -> std::io::Result<BenchMode> {
    let dir = std::env::temp_dir().join(format!("wisclite-bench-{}-{}", mode_name(mode), std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut db = Db::open(&dir, Options { mode, memtable_bytes: mem, sync_writes: false })?;
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
    let mut lat = Vec::with_capacity(gets);
    for _ in 0..gets {
        let k = &keys[rng.random_range(0..n)];
        let t = Instant::now();
        let v = db.get(k)?;
        lat.push(t.elapsed().as_secs_f64() * 1e6);
        assert!(v.is_some());
    }
    lat.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let stats = db.stats();
    let disk_bytes = db.disk_usage()?;
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(BenchMode {
        mode, ops: n, value_size: vs, put_secs,
        puts_per_sec: n as f64 / put_secs,
        mb_per_sec: stats.logical_bytes as f64 / put_secs / 1e6,
        get_p50_us: pct(&lat, 0.5), get_p99_us: pct(&lat, 0.99),
        get_mean_us: lat.iter().sum::<f64>() / lat.len() as f64,
        disk_bytes, stats,
    })
}

fn mode_name(m: Mode) -> &'static str { match m { Mode::WiscKey => "wisckey", Mode::Baseline => "baseline" } }

fn main() -> std::io::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut take = |flag: &str| -> Option<String> {
        let i = args.iter().position(|a| a == flag)?;
        args.remove(i);
        if i < args.len() { Some(args.remove(i)) } else { usage() }
    };
    let dir = take("--dir").unwrap_or_else(|| "wisclite-data".into());
    let n: usize = take("--n").map(|s| s.parse().unwrap()).unwrap_or(50_000);
    let vs: usize = take("--value-size").map(|s| s.parse().unwrap()).unwrap_or(1024);
    let gets: usize = take("--gets").map(|s| s.parse().unwrap()).unwrap_or(10_000);
    let mem: usize = take("--memtable").map(|s| s.parse().unwrap()).unwrap_or(4 << 20);
    let out = take("--out").unwrap_or_else(|| "bench.json".into());
    let baseline = if let Some(i) = args.iter().position(|a| a == "--baseline") { args.remove(i); true } else { false };
    let opts = Options { mode: if baseline { Mode::Baseline } else { Mode::WiscKey }, ..Default::default() };
    match args.iter().map(|s| s.as_str()).collect::<Vec<_>>().as_slice() {
        ["put", k, v] => { let mut db = Db::open(&dir, opts)?; db.put(k.as_bytes(), v.as_bytes())?; println!("OK"); }
        ["get", k] => {
            let mut db = Db::open(&dir, opts)?;
            match db.get(k.as_bytes())? { Some(v) => println!("{}", String::from_utf8_lossy(&v)), None => { println!("(not found)"); std::process::exit(1) } }
        }
        ["del", k] => { let mut db = Db::open(&dir, opts)?; db.delete(k.as_bytes())?; println!("OK"); }
        ["stats"] => {
            let db = Db::open(&dir, opts)?;
            println!("mode: {:?}\n{}\ndisk bytes: {}", db.mode(), serde_json::to_string_pretty(&db.stats()).unwrap(), db.disk_usage()?);
        }
        ["bench"] => {
            let mut results = Vec::new();
            for m in [Mode::Baseline, Mode::WiscKey] {
                let r = bench_one(m, n, vs, gets, mem)?;
                println!("{:<9} puts/s {:>9.0}  {:>6.1} MB/s  WA {:.3}  tree-WA {:.3}  get p50 {:.1}us p99 {:.1}us  flushes {}",
                    mode_name(m), r.puts_per_sec, r.mb_per_sec, r.stats.write_amplification, r.stats.tree_amplification,
                    r.get_p50_us, r.get_p99_us, r.stats.flushes);
                results.push(r);
            }
            let machine = std::fs::read_to_string("/proc/cpuinfo").ok()
                .and_then(|c| c.lines().find(|l| l.starts_with("model name")).map(|l| l.split(':').nth(1).unwrap().trim().to_string()))
                .unwrap_or_else(|| std::env::consts::OS.into());
            let b = Bench { generated_at_unix: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(), machine, memtable_bytes: mem, results };
            std::fs::write(&out, serde_json::to_string_pretty(&b).unwrap())?;
            println!("wrote {out}");
        }
        _ => usage(),
    }
    Ok(())
}
