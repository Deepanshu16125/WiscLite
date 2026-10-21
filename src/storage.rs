//! File-system abstraction so the same engine runs natively (std::fs) and in the
//! browser (an in-memory file system, compiled to wasm32).
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub trait Storage: Send + Sync {
    fn create_dir_all(&self, dir: &Path) -> io::Result<()>;
    /// Whole-file read. NotFound if missing.
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    /// Create or truncate, then write.
    fn write(&self, path: &Path, data: &[u8]) -> io::Result<()>;
    /// Append (creating the file if needed).
    fn append(&self, path: &Path, data: &[u8]) -> io::Result<()>;
    fn read_at(&self, path: &Path, offset: u64, len: usize) -> io::Result<Vec<u8>>;
    fn len(&self, path: &Path) -> io::Result<u64>;
    fn exists(&self, path: &Path) -> bool { self.len(path).is_ok() }
    /// File names (not paths) directly inside `dir`.
    fn list(&self, dir: &Path) -> io::Result<Vec<String>>;
    fn remove(&self, path: &Path) -> io::Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Make the file durable (fsync natively; no-op in memory).
    fn sync(&self, path: &Path) -> io::Result<()>;
}

pub type Fs = Arc<dyn Storage>;

fn not_found(p: &Path) -> io::Error { io::Error::new(io::ErrorKind::NotFound, format!("{} not found", p.display())) }

// ---------------------------------------------------------------- native
/// std::fs backend with cached append/read handles (keeps the hot path to one write syscall).
#[derive(Default)]
pub struct StdFs {
    handles: Mutex<HashMap<PathBuf, std::fs::File>>,
}

impl StdFs {
    pub fn new() -> Fs { Arc::new(StdFs::default()) }
    fn with<R>(&self, path: &Path, f: impl FnOnce(&mut std::fs::File) -> io::Result<R>) -> io::Result<R> {
        let mut h = self.handles.lock().unwrap();
        if !h.contains_key(path) {
            let file = std::fs::OpenOptions::new().read(true).append(true).create(true).open(path)?;
            h.insert(path.to_path_buf(), file);
        }
        f(h.get_mut(path).unwrap())
    }
    fn forget(&self, path: &Path) { self.handles.lock().unwrap().remove(path); }
}

impl Storage for StdFs {
    fn create_dir_all(&self, dir: &Path) -> io::Result<()> { std::fs::create_dir_all(dir) }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> { std::fs::read(path) }
    fn write(&self, path: &Path, data: &[u8]) -> io::Result<()> { self.forget(path); std::fs::write(path, data) }
    fn append(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        use std::io::Write;
        self.with(path, |f| f.write_all(data))
    }
    fn read_at(&self, path: &Path, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom};
        if !path.exists() { return Err(not_found(path)); }
        self.with(path, |f| {
            let mut buf = vec![0u8; len];
            f.seek(SeekFrom::Start(offset))?;
            f.read_exact(&mut buf)?;
            Ok(buf)
        })
    }
    fn len(&self, path: &Path) -> io::Result<u64> { Ok(std::fs::metadata(path)?.len()) }
    fn list(&self, dir: &Path) -> io::Result<Vec<String>> {
        Ok(std::fs::read_dir(dir)?.filter_map(|e| e.ok()?.file_name().into_string().ok()).collect())
    }
    fn remove(&self, path: &Path) -> io::Result<()> { self.forget(path); std::fs::remove_file(path) }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> { self.forget(from); self.forget(to); std::fs::rename(from, to) }
    fn sync(&self, path: &Path) -> io::Result<()> {
        if self.handles.lock().unwrap().contains_key(path) { self.with(path, |f| f.sync_data()) } else { std::fs::File::open(path)?.sync_all() }
    }
}

// ---------------------------------------------------------------- in-memory
/// In-memory file system: used by the WebAssembly build and by tests.
#[derive(Default)]
pub struct MemFs {
    files: Mutex<HashMap<PathBuf, Vec<u8>>>,
}

impl MemFs {
    pub fn new() -> Fs { Arc::new(MemFs::default()) }
    pub fn shared() -> Arc<MemFs> { Arc::new(MemFs::default()) }
}

impl Storage for MemFs {
    fn create_dir_all(&self, _dir: &Path) -> io::Result<()> { Ok(()) }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.files.lock().unwrap().get(path).cloned().ok_or_else(|| not_found(path))
    }
    fn write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        self.files.lock().unwrap().insert(path.to_path_buf(), data.to_vec());
        Ok(())
    }
    fn append(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        self.files.lock().unwrap().entry(path.to_path_buf()).or_default().extend_from_slice(data);
        Ok(())
    }
    fn read_at(&self, path: &Path, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let f = self.files.lock().unwrap();
        let d = f.get(path).ok_or_else(|| not_found(path))?;
        let (s, e) = (offset as usize, offset as usize + len);
        if e > d.len() { return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "read past end")); }
        Ok(d[s..e].to_vec())
    }
    fn len(&self, path: &Path) -> io::Result<u64> {
        self.files.lock().unwrap().get(path).map(|d| d.len() as u64).ok_or_else(|| not_found(path))
    }
    fn list(&self, dir: &Path) -> io::Result<Vec<String>> {
        Ok(self.files.lock().unwrap().keys()
            .filter(|p| p.parent() == Some(dir))
            .filter_map(|p| p.file_name()?.to_str().map(String::from)).collect())
    }
    fn remove(&self, path: &Path) -> io::Result<()> {
        self.files.lock().unwrap().remove(path).map(|_| ()).ok_or_else(|| not_found(path))
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let mut f = self.files.lock().unwrap();
        let d = f.remove(from).ok_or_else(|| not_found(from))?;
        f.insert(to.to_path_buf(), d);
        Ok(())
    }
    fn sync(&self, _path: &Path) -> io::Result<()> { Ok(()) }
}

impl Storage for Arc<MemFs> {
    fn create_dir_all(&self, d: &Path) -> io::Result<()> { (**self).create_dir_all(d) }
    fn read(&self, p: &Path) -> io::Result<Vec<u8>> { (**self).read(p) }
    fn write(&self, p: &Path, d: &[u8]) -> io::Result<()> { (**self).write(p, d) }
    fn append(&self, p: &Path, d: &[u8]) -> io::Result<()> { (**self).append(p, d) }
    fn read_at(&self, p: &Path, o: u64, l: usize) -> io::Result<Vec<u8>> { (**self).read_at(p, o, l) }
    fn len(&self, p: &Path) -> io::Result<u64> { (**self).len(p) }
    fn list(&self, d: &Path) -> io::Result<Vec<String>> { (**self).list(d) }
    fn remove(&self, p: &Path) -> io::Result<()> { (**self).remove(p) }
    fn rename(&self, a: &Path, b: &Path) -> io::Result<()> { (**self).rename(a, b) }
    fn sync(&self, p: &Path) -> io::Result<()> { (**self).sync(p) }
}
