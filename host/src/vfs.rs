//! The guest's view of the file system.
//!
//! The guest sees exactly two things:
//!
//! * `/apk/base.apk` — a read-only, in-memory zip archive synthesised from the assets
//!   directory (the game reads its assets out of "the APK" with its own zip code);
//! * `/data/...` — the save directory, mapped onto one host directory. Paths are
//!   normalised component by component, so the guest cannot name anything outside it.
//!
//! Every other path does not exist. The guest never receives a host file descriptor.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub const APK_PATH: &str = "/apk/base.apk";
pub const DOC_PATH: &str = "/data";

pub const ENOENT: u32 = 2;
pub const EBADF: u32 = 9;
pub const EACCES: u32 = 13;
pub const EINVAL: u32 = 22;
pub const EMFILE: u32 = 24;
/// Open streams the guest may hold at once.
const MAX_OPEN: usize = 64;

pub enum Node {
    Mem { data: Rc<Vec<u8>>, pos: u64 },
    Host { file: fs::File },
}

pub struct Open {
    pub node: Node,
    pub fd: u32,
}

pub enum Target {
    Apk,
    Doc(PathBuf),
    Nothing,
}

pub struct Stat {
    pub size: u64,
    pub is_dir: bool,
    pub mtime: u64,
}

pub struct Vfs {
    apk: Rc<Vec<u8>>,
    doc_host: PathBuf,
    /// Open streams keyed by the guest `FILE*`.
    pub open: HashMap<u32, Open>,
    next_fd: u32,
    pub verbose: bool,
}

pub fn crc32(mut crc: u32, data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let t = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *e = c;
        }
        t
    });
    crc = !crc;
    for &b in data {
        crc = t[((crc ^ b as u32) & 0xff) as usize] ^ (crc >> 8);
    }
    !crc
}

/// Build a stored (uncompressed) zip of `dir` with every entry under `assets/`.
fn synth_apk(dir: &Path) -> Result<Vec<u8>, String> {
    fn walk(dir: &Path, rel: &str, out: &mut Vec<(String, PathBuf)>) -> Result<(), String> {
        let rd = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for e in rd {
            let e = e.map_err(|e| e.to_string())?;
            let name = e.file_name().to_string_lossy().into_owned();
            let ty = e.file_type().map_err(|e| e.to_string())?;
            let rel = format!("{rel}/{name}");
            if ty.is_dir() {
                walk(&e.path(), &rel, out)?;
            } else if ty.is_file() {
                out.push((rel, e.path()));
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(dir, "assets", &mut files)?;
    if files.is_empty() {
        return Err(format!("no files under {}", dir.display()));
    }
    files.sort();
    let mut zip = Vec::new();
    let mut central = Vec::new();
    let w16 = |v: &mut Vec<u8>, x: u16| v.extend_from_slice(&x.to_le_bytes());
    let w32 = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
    const DOS_DATE: u16 = (30 << 9) | (1 << 5) | 1; // 2010-01-01
    for (name, path) in &files {
        let data = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let (crc, len, off) = (crc32(0, &data), data.len() as u32, zip.len() as u32);
        for (v, is_central) in [(&mut zip, false), (&mut central, true)] {
            w32(v, if is_central { 0x0201_4b50 } else { 0x0403_4b50 });
            if is_central {
                w16(v, 20);
            }
            w16(v, 20); // version needed
            w16(v, 0); // flags
            w16(v, 0); // method: stored
            w16(v, 0); // time
            w16(v, DOS_DATE);
            w32(v, crc);
            w32(v, len);
            w32(v, len);
            w16(v, name.len() as u16);
            w16(v, 0); // extra
            if is_central {
                w16(v, 0); // comment
                w16(v, 0); // disk
                w16(v, 0); // internal attributes
                w32(v, 0); // external attributes
                w32(v, off);
            }
            v.extend_from_slice(name.as_bytes());
        }
        zip.extend_from_slice(&data);
    }
    let cd_off = zip.len() as u32;
    zip.extend_from_slice(&central);
    w32(&mut zip, 0x0605_4b50);
    w16(&mut zip, 0);
    w16(&mut zip, 0);
    w16(&mut zip, files.len() as u16);
    w16(&mut zip, files.len() as u16);
    w32(&mut zip, central.len() as u32);
    w32(&mut zip, cd_off);
    w16(&mut zip, 0);
    if files.len() > 0xffff {
        return Err("too many asset files for a zip archive".into());
    }
    Ok(zip)
}

impl Vfs {
    pub fn new(assets: &Path, save_dir: &Path) -> Result<Vfs, String> {
        let apk = synth_apk(assets)?;
        fs::create_dir_all(save_dir).map_err(|e| format!("{}: {e}", save_dir.display()))?;
        // The Java layer copies the bundled saves and config out of the APK on first start.
        for name in ["Config.dat", "Save0.dat", "Save1.dat", "Save2.dat"] {
            let (src, dst) = (assets.join(name), save_dir.join(name));
            if src.is_file() && !dst.exists() {
                fs::copy(&src, &dst).map_err(|e| format!("{}: {e}", dst.display()))?;
            }
        }
        let doc_host = save_dir.canonicalize().map_err(|e| format!("{}: {e}", save_dir.display()))?;
        Ok(Vfs { apk: Rc::new(apk), doc_host, open: HashMap::new(), next_fd: 100, verbose: false })
    }

    pub fn apk_len(&self) -> usize {
        self.apk.len()
    }

    pub fn resolve(&self, path: &[u8]) -> Target {
        let Ok(p) = std::str::from_utf8(path) else { return Target::Nothing };
        if p == APK_PATH {
            return Target::Apk;
        }
        let Some(rest) = p.strip_prefix(DOC_PATH) else { return Target::Nothing };
        if !(rest.is_empty() || rest.starts_with('/')) {
            return Target::Nothing;
        }
        let mut host = self.doc_host.clone();
        for comp in rest.split('/') {
            match comp {
                "" | "." => {}
                ".." => return Target::Nothing,
                c if c.contains(['\\', '\0', ':']) => return Target::Nothing,
                c => host.push(c),
            }
        }
        // Refuse anything that resolves outside the save directory through a symlink.
        let mut probe = host.as_path();
        loop {
            if let Ok(real) = probe.canonicalize() {
                if !real.starts_with(&self.doc_host) {
                    return Target::Nothing;
                }
                break;
            }
            match probe.parent() {
                Some(p) => probe = p,
                None => return Target::Nothing,
            }
        }
        Target::Doc(host)
    }

    pub fn fopen(&mut self, key: u32, path: &[u8], mode: &[u8]) -> Result<u32, u32> {
        if self.open.len() >= MAX_OPEN {
            return Err(EMFILE);
        }
        let (write, plus) = (mode.first().is_some_and(|c| *c != b'r'), mode.contains(&b'+'));
        let node = match self.resolve(path) {
            Target::Apk if !write && !plus => Node::Mem { data: self.apk.clone(), pos: 0 },
            Target::Apk => return Err(EACCES),
            Target::Doc(host) => {
                let mut o = fs::OpenOptions::new();
                match mode.first() {
                    Some(b'r') => o.read(true).write(plus),
                    Some(b'w') => o.write(true).create(true).truncate(true).read(plus),
                    Some(b'a') => o.append(true).create(true).read(plus),
                    _ => return Err(EINVAL),
                };
                match o.open(&host) {
                    Ok(file) if file.metadata().map_or(false, |m| m.is_file()) => Node::Host { file },
                    _ => return Err(ENOENT),
                }
            }
            Target::Nothing => return Err(ENOENT),
        };
        let fd = self.next_fd;
        self.next_fd += 1;
        self.open.insert(key, Open { node, fd });
        Ok(fd)
    }

    pub fn stat(&self, path: &[u8]) -> Result<Stat, u32> {
        match self.resolve(path) {
            Target::Apk => Ok(Stat { size: self.apk.len() as u64, is_dir: false, mtime: 1_262_304_000 }),
            Target::Doc(host) => {
                let md = fs::metadata(host).map_err(|_| ENOENT)?;
                let mtime = md.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
                Ok(Stat { size: md.len(), is_dir: md.is_dir(), mtime })
            }
            Target::Nothing => Err(ENOENT),
        }
    }

    pub fn fstat(&self, fd: u32) -> Result<Stat, u32> {
        let o = self.open.values().find(|o| o.fd == fd).ok_or(EBADF)?;
        match &o.node {
            Node::Mem { data, .. } => Ok(Stat { size: data.len() as u64, is_dir: false, mtime: 1_262_304_000 }),
            Node::Host { file } => {
                let md = file.metadata().map_err(|_| EBADF)?;
                Ok(Stat { size: md.len(), is_dir: false, mtime: 0 })
            }
        }
    }

    /// Path operations that only make sense inside the save directory.
    pub fn doc_path(&self, path: &[u8]) -> Result<PathBuf, u32> {
        match self.resolve(path) {
            Target::Doc(p) if p != self.doc_host => Ok(p),
            Target::Doc(_) | Target::Apk => Err(EACCES),
            Target::Nothing => Err(ENOENT),
        }
    }
}

impl Open {
    pub fn read(&mut self, buf: &mut [u8]) -> usize {
        match &mut self.node {
            Node::Mem { data, pos } => {
                let start = (*pos).min(data.len() as u64) as usize;
                let n = buf.len().min(data.len() - start);
                buf[..n].copy_from_slice(&data[start..start + n]);
                *pos += n as u64;
                n
            }
            Node::Host { file } => {
                let mut done = 0;
                while done < buf.len() {
                    match file.read(&mut buf[done..]) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => done += n,
                    }
                }
                done
            }
        }
    }

    pub fn write(&mut self, buf: &[u8]) -> usize {
        match &mut self.node {
            Node::Mem { .. } => 0,
            Node::Host { file } => if file.write_all(buf).is_ok() { buf.len() } else { 0 },
        }
    }

    pub fn seek(&mut self, off: i64, whence: u32) -> Result<u64, u32> {
        match &mut self.node {
            Node::Mem { data, pos } => {
                let base = match whence {
                    0 => 0,
                    1 => *pos as i64,
                    2 => data.len() as i64,
                    _ => return Err(EINVAL),
                };
                let np = base.checked_add(off).filter(|&p| p >= 0).ok_or(EINVAL)?;
                *pos = np as u64;
                Ok(*pos)
            }
            Node::Host { file } => {
                let from = match whence {
                    0 if off >= 0 => SeekFrom::Start(off as u64),
                    1 => SeekFrom::Current(off),
                    2 => SeekFrom::End(off),
                    _ => return Err(EINVAL),
                };
                file.seek(from).map_err(|_| EINVAL)
            }
        }
    }

    pub fn tell(&mut self) -> u64 {
        match &mut self.node {
            Node::Mem { pos, .. } => *pos,
            Node::Host { file } => file.stream_position().unwrap_or(0),
        }
    }
}
