use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use wisclite::{Db, Mode, Options};

fn req(addr: &str, method: &str, path: &str, body: &str) -> (u16, String) {
    let mut s = TcpStream::connect(addr).unwrap();
    write!(s, "{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let code = out[9..12].parse().unwrap();
    let body = out.split_once("\r\n\r\n").unwrap().1.to_string();
    (code, body)
}

#[test]
fn http_api_put_get_scan_stats_dashboard() {
    let d = std::env::temp_dir().join(format!("wisclite-srv-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let db = Db::open(&d, Options { mode: Mode::WiscKey, ..Default::default() }).unwrap();
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    let static_dir = d.join("static-missing"); // forces the embedded dashboard
    std::thread::spawn(move || wisclite::server::serve(l, db, static_dir));

    let (c, b) = req(&addr, "POST", "/api/put", r#"{"key":"user:1","value":"Dee"}"#);
    assert_eq!(c, 200, "{b}");
    req(&addr, "POST", "/api/put", r#"{"key":"user:2","value":"two words"}"#);
    let (c, b) = req(&addr, "GET", "/api/get?key=user%3A2", "");
    assert_eq!(c, 200);
    let v: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert_eq!(v["value"], "two words");
    let (_, b) = req(&addr, "POST", "/api/get", r#"{"key":"nope"}"#);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&b).unwrap()["found"], false);
    req(&addr, "POST", "/api/del", r#"{"key":"user:1"}"#);
    let (_, b) = req(&addr, "GET", "/api/scan?start=user&end=v", "");
    let v: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert_eq!(v["total"], 1);
    let (_, b) = req(&addr, "GET", "/api/stats", "");
    let v: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert_eq!(v["mode"], "wisckey");
    assert!(v["stats"]["logical_bytes"].as_u64().unwrap() > 0);
    let (c, b) = req(&addr, "GET", "/", "");
    assert_eq!(c, 200);
    assert!(b.contains("WiscLite"));
    assert_eq!(req(&addr, "POST", "/api/put", r#"{"value":"x"}"#).0, 400);
    assert_eq!(req(&addr, "GET", "/nope", "").0, 404);
    assert_eq!(req(&addr, "POST", "/api/gc", "").0, 200);
}
