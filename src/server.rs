//! `wisclite serve`: a tiny single-threaded HTTP/1.1 server (std only) bound to localhost.
//!
//! GET  /, /metrics.html   dashboard (from disk if present, else the embedded copy)
//! GET  /bench.json        last bench results
//! GET  /api/stats         live engine stats
//! POST /api/put           {"key": "...", "value": "..."}
//! GET  /api/get?key=k     or POST /api/get {"key": "..."}
//! POST /api/del           {"key": "..."}
//! GET  /api/scan?start=a&end=b&limit=100
//! POST /api/gc            run value-log GC
use crate::Db;
use serde_json::{json, Value};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};

const EMBEDDED_DASHBOARD: &str = include_str!("../metrics.html");
const MAX_BODY: usize = 16 << 20;

pub struct Request { pub method: String, pub path: String, pub query: Vec<(String, String)>, pub body: Vec<u8> }

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(v) => { out.push(v); i += 2; }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn read_request(s: &mut TcpStream) -> io::Result<Request> {
    let mut r = BufReader::new(s);
    let mut line = String::new();
    r.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let mut len = 0usize;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 || h == "\r\n" || h == "\n" { break; }
        if let Some((k, v)) = h.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") { len = v.trim().parse().unwrap_or(0); }
        }
    }
    if len > MAX_BODY { return Err(io::Error::new(io::ErrorKind::InvalidData, "body too large")); }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    let (path, q) = target.split_once('?').unwrap_or((&target, ""));
    let query = q.split('&').filter(|x| !x.is_empty())
        .map(|kv| { let (k, v) = kv.split_once('=').unwrap_or((kv, "")); (pct_decode(k), pct_decode(v)) })
        .collect();
    Ok(Request { method, path: path.to_string(), query, body })
}

fn respond(s: &mut TcpStream, code: u16, ctype: &str, body: &[u8]) -> io::Result<()> {
    let reason = match code { 200 => "OK", 400 => "Bad Request", 404 => "Not Found", 405 => "Method Not Allowed", _ => "Internal Server Error" };
    write!(s, "HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", body.len())?;
    s.write_all(body)
}

fn json_resp(s: &mut TcpStream, code: u16, v: Value) -> io::Result<()> {
    respond(s, code, "application/json", v.to_string().as_bytes())
}

/// Handle one request against the db. Returns (status, content-type, body).
pub fn handle(db: &mut Db, static_dir: &Path, req: &Request) -> (u16, &'static str, Vec<u8>) {
    let j = |code: u16, v: Value| (code, "application/json", v.to_string().into_bytes());
    let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
    let param = |name: &str| -> Option<String> {
        req.query.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
            .or_else(|| body.get(name).and_then(|v| v.as_str()).map(String::from))
    };
    let err = |e: io::Error| j(500, json!({"error": e.to_string()}));
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") | ("GET", "/metrics.html") => {
            let html = std::fs::read(static_dir.join("metrics.html")).unwrap_or_else(|_| EMBEDDED_DASHBOARD.as_bytes().to_vec());
            (200, "text/html; charset=utf-8", html)
        }
        ("GET", "/bench.json") => match std::fs::read(static_dir.join("bench.json")) {
            Ok(b) => (200, "application/json", b),
            Err(_) => j(404, json!({"error": "no bench.json; run `wisclite bench`"})),
        },
        ("GET", "/api/stats") => {
            let disk = db.disk_usage().unwrap_or(0);
            j(200, json!({"mode": db.mode().as_str(), "disk_bytes": disk, "stats": db.stats()}))
        }
        ("POST", "/api/put") => match (param("key"), param("value")) {
            (Some(k), Some(v)) if !k.is_empty() => match db.put(k.as_bytes(), v.as_bytes()) {
                Ok(()) => j(200, json!({"ok": true, "key": k})),
                Err(e) => err(e),
            },
            _ => j(400, json!({"error": "need non-empty key and value"})),
        },
        ("GET", "/api/get") | ("POST", "/api/get") => match param("key") {
            Some(k) => match db.get(k.as_bytes()) {
                Ok(v) => j(200, json!({"key": k, "found": v.is_some(), "value": v.map(|v| String::from_utf8_lossy(&v).into_owned())})),
                Err(e) => err(e),
            },
            None => j(400, json!({"error": "need key"})),
        },
        ("POST", "/api/del") => match param("key") {
            Some(k) => match db.delete(k.as_bytes()) { Ok(()) => j(200, json!({"ok": true})), Err(e) => err(e) },
            None => j(400, json!({"error": "need key"})),
        },
        ("GET", "/api/scan") => {
            let start = param("start").unwrap_or_default();
            let end = param("end").filter(|e| !e.is_empty());
            let limit: usize = param("limit").and_then(|l| l.parse().ok()).unwrap_or(100).min(10_000);
            match db.scan(start.as_bytes(), end.as_deref().map(str::as_bytes)) {
                Ok(rows) => {
                    let total = rows.len();
                    let items: Vec<Value> = rows.into_iter().take(limit)
                        .map(|(k, v)| json!({"key": String::from_utf8_lossy(&k), "value": String::from_utf8_lossy(&v)})).collect();
                    j(200, json!({"total": total, "items": items}))
                }
                Err(e) => err(e),
            }
        }
        ("POST", "/api/gc") => match db.gc() { Ok(r) => j(200, json!(r)), Err(e) => err(e) },
        (_, p) if p.starts_with("/api/") => j(405, json!({"error": "unsupported method or route"})),
        _ => j(404, json!({"error": "not found"})),
    }
}

/// Serve forever on an already-bound listener (one connection at a time).
pub fn serve(listener: TcpListener, mut db: Db, static_dir: PathBuf) -> io::Result<()> {
    for stream in listener.incoming() {
        let Ok(mut s) = stream else { continue };
        let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(5)));
        match read_request(&mut s) {
            Ok(req) => {
                let (code, ct, body) = handle(&mut db, &static_dir, &req);
                let _ = respond(&mut s, code, ct, &body);
            }
            Err(e) => { let _ = json_resp(&mut s, 400, json!({"error": e.to_string()})); }
        }
    }
    Ok(())
}
