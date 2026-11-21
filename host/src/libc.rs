//! Host implementations of the library's imported C functions.
//!
//! These are the whole of the guest's "operating system". Each one reads its arguments
//! from guest registers, touches guest memory only through the checked accessors, and has
//! no access to anything the host has not explicitly modelled. Networking, threads,
//! dynamic loading and OpenGL are refused or ignored here.

use crate::fmt::{self, Va};
use crate::machine::{Handler, Machine, LIB_BASE};
use crate::mem::{Fault, R};
use crate::vfs::{self, EACCES, EBADF, EINVAL};
use miniz_oxide::inflate::stream::{inflate as mz_inflate, InflateState};
use miniz_oxide::{DataFormat, MZError, MZFlush, MZStatus};
use std::cmp::Ordering;
use std::fs;

const ENOMEM: u32 = 12;
const EPERM: u32 = 1;
const ENETUNREACH: u32 = 101;
const EOF_FLAG: u16 = 0x20;
const ERR_FLAG: u16 = 0x40;
/// Offset of `_flags` in bionic's `FILE`; `feof`/`ferror` are macros that read it directly.
const FILE_FLAGS: u32 = 12;
const FILE_SIZE: u32 = 0x60;

pub fn lookup(name: &str) -> Option<Handler> {
    let h: Handler = match name {
        // memory
        "malloc" => |m| { let a = m.heap.alloc(m.arg(0)?); if a == 0 { m.set_errno(ENOMEM)?; } m.ret(a) },
        "calloc" => calloc,
        "realloc" => realloc,
        "free" => free,
        "memcpy" | "memmove" => |m| { let d = m.arg(0)?; m.mem.copy(d, m.arg(1)?, m.arg(2)?)?; m.ret(d) },
        "memset" => |m| { let d = m.arg(0)?; let c = m.arg(1)? as u8; m.mem.slice_mut(d, m.arg(2)?)?.fill(c); m.ret(d) },
        "memcmp" => |m| { let n = m.arg(2)?; let o = m.mem.slice(m.arg(0)?, n)?.cmp(m.mem.slice(m.arg(1)?, n)?); m.ret(ord(o)) },
        "memchr" => |m| {
            let (s, c) = (m.arg(0)?, m.arg(1)? as u8);
            let p = m.mem.slice(s, m.arg(2)?)?.iter().position(|&b| b == c);
            m.ret(p.map_or(0, |i| s + i as u32))
        },
        // strings
        "strlen" => |m| { let n = m.mem.strlen(m.arg(0)?)?; m.ret(n) },
        "strcpy" => |m| { let (d, s) = (m.arg(0)?, m.arg(1)?); let n = m.mem.strlen(s)?; m.mem.copy(d, s, n + 1)?; m.ret(d) },
        "strcat" => |m| {
            let (d, s) = (m.arg(0)?, m.arg(1)?);
            let (dn, sn) = (m.mem.strlen(d)?, m.mem.strlen(s)?);
            let at = d.checked_add(dn).ok_or(Fault::BadAccess { addr: d, len: dn, write: true })?;
            m.mem.copy(at, s, sn + 1)?;
            m.ret(d)
        },
        "strncpy" => |m| {
            let (d, s, n) = (m.arg(0)?, m.arg(1)?, m.arg(2)?);
            let src: Vec<u8> = m.mem.tail(s)?.iter().take(n as usize).take_while(|&&b| b != 0).copied().collect();
            let dst = m.mem.slice_mut(d, n)?;
            dst[..src.len()].copy_from_slice(&src);
            dst[src.len()..].fill(0);
            m.ret(d)
        },
        "strcmp" => |m| { let o = m.mem.cstr(m.arg(0)?)?.cmp(m.mem.cstr(m.arg(1)?)?); m.ret(ord(o)) },
        "strncmp" => |m| {
            let n = m.arg(2)? as usize;
            let cut = |s: &[u8]| -> Vec<u8> { s.iter().take(n).take_while(|&&b| b != 0).copied().collect() };
            let (a, b) = (cut(m.mem.tail(m.arg(0)?)?), cut(m.mem.tail(m.arg(1)?)?));
            m.ret(ord(a.cmp(&b)))
        },
        "strcasecmp" => |m| {
            let a = m.mem.cstr(m.arg(0)?)?.to_ascii_lowercase();
            let b = m.mem.cstr(m.arg(1)?)?.to_ascii_lowercase();
            m.ret(ord(a.cmp(&b)))
        },
        "strrchr" => |m| {
            let (s, c) = (m.arg(0)?, m.arg(1)? as u8);
            let n = m.mem.strlen(s)?;
            let p = m.mem.slice(s, n + 1)?.iter().rposition(|&b| b == c);
            m.ret(p.map_or(0, |i| s + i as u32))
        },
        "strdup" => |m| {
            let s = m.mem.cstr(m.arg(0)?)?.to_vec();
            let a = m.heap.alloc(s.len() as u32 + 1);
            if a != 0 {
                m.mem.write(a, &s)?;
                m.mem.w8(a + s.len() as u32, 0)?;
            }
            m.ret(a)
        },
        "atoi" => |m| { let (v, _) = parse_int(m.mem.cstr(m.arg(0)?)?, 10); m.ret(v as u32) },
        "strtoul" => |m| {
            let (s, endp, base) = (m.arg(0)?, m.arg(1)?, m.arg(2)?);
            let (v, used) = parse_int(m.mem.cstr(s)?, base);
            if endp != 0 {
                m.mem.w32(endp, s + used as u32)?;
            }
            m.ret(v as u32)
        },
        "strerror" => |m| {
            let a = m.static_buf("strerror", 32)?;
            m.mem.write(a, b"error\0")?;
            m.ret(a)
        },
        "sprintf" => |m| {
            let out = fmt::format(m, m.arg(1)?, &mut Va::Call { idx: 2 })?;
            emit(m, m.arg(0)?, None, out)
        },
        "snprintf" => |m| {
            let out = fmt::format(m, m.arg(2)?, &mut Va::Call { idx: 3 })?;
            emit(m, m.arg(0)?, Some(m.arg(1)?), out)
        },
        "vsprintf" => |m| {
            let out = fmt::format(m, m.arg(1)?, &mut Va::List { addr: m.arg(2)? })?;
            emit(m, m.arg(0)?, None, out)
        },
        "__android_log_print" => |m| {
            let tag = m.mem.cstr_lossy(m.arg(1)?)?;
            let out = fmt::format(m, m.arg(2)?, &mut Va::Call { idx: 3 })?;
            eprintln!("[guest log] {tag}: {}", String::from_utf8_lossy(&out).trim_end());
            m.ret(out.len() as u32)
        },
        // maths (soft-float ABI: doubles travel in r0:r1)
        "ceil" => |m| { let v = f64::from_bits(m.arg(0)? as u64 | (m.arg(1)? as u64) << 32); m.ret64(v.ceil().to_bits()) },
        "floor" => |m| { let v = f64::from_bits(m.arg(0)? as u64 | (m.arg(1)? as u64) << 32); m.ret64(v.floor().to_bits()) },
        "qsort" => qsort,
        // time
        "gettimeofday" => |m| {
            let (tv, us) = (m.arg(0)?, m.clock.now_micros());
            if tv != 0 {
                m.mem.w32(tv, (us / 1_000_000) as u32)?;
                m.mem.w32(tv + 4, (us % 1_000_000) as u32)?;
            }
            m.ret(0)
        },
        "time" => |m| {
            let (p, s) = (m.arg(0)?, (m.clock.now_micros() / 1_000_000) as u32);
            if p != 0 {
                m.mem.w32(p, s)?;
            }
            m.ret(s)
        },
        "localtime" => localtime,
        "mktime" => mktime,
        // files
        "fopen" => fopen,
        "fclose" => fclose,
        "fread" => fread,
        "fwrite" => fwrite,
        "fseek" | "fseeko" => fseek,
        "ftell" | "ftello" => |m| with_file(m, 0, |o| o.tell() as u32),
        "clearerr" => |m| { let f = m.arg(0)?; if m.vfs.open.contains_key(&f) { m.mem.w16(f + FILE_FLAGS, 0)?; } Ok(()) },
        "fileno" => |m| with_file(m, 0, |o| o.fd),
        "__swbuf" => |m| {
            let (c, f) = (m.arg(0)? as u8, m.arg(1)?);
            let ok = m.vfs.open.get_mut(&f).map_or(0, |o| o.write(&[c]));
            m.ret(if ok == 1 { c as u32 } else { u32::MAX })
        },
        "stat" => |m| { let r = m.vfs.stat(m.mem.cstr(m.arg(0)?)?); put_stat(m, r) },
        "fstat" => |m| { let r = m.vfs.fstat(m.arg(0)?); put_stat(m, r) },
        "mkdir" => |m| path_op(m, |p, _| fs::create_dir(p).is_ok()),
        "rmdir" => |m| path_op(m, |p, _| fs::remove_dir(p).is_ok()),
        "remove" => |m| path_op(m, |p, _| fs::remove_file(p).is_ok()),
        "rename" => |m| {
            let to = m.vfs.doc_path(m.mem.cstr(m.arg(1)?)?);
            path_op(m, |p, _| to.as_ref().is_ok_and(|t| fs::rename(p, t).is_ok()))
        },
        "chmod" => |m| path_op(m, |_, _| true),
        "umask" => |m| m.ret(0o022),
        // Writing a new archive (libzip's zip_close) needs these; the game never does that.
        "mkstemp" | "close" => |m| { m.set_errno(EACCES)?; m.ret(u32::MAX) },
        "fdopen" => |m| { m.set_errno(EBADF)?; m.ret(0) },
        // zlib
        "crc32" => |m| {
            let (crc, buf, len) = (m.arg(0)?, m.arg(1)?, m.arg(2)?);
            let v = if buf == 0 { 0 } else { vfs::crc32(crc, m.mem.slice(buf, len)?) };
            m.ret(v)
        },
        "inflateInit2_" => |m| {
            let (strm, bits) = (m.arg(0)?, m.arg(1)? as i32);
            let fmt = if bits < 0 { DataFormat::Raw } else { DataFormat::Zlib };
            if m.zstreams.len() >= 64 && !m.zstreams.contains_key(&strm) {
                return m.ret(-4i32 as u32);
            }
            m.zstreams.insert(strm, InflateState::new_boxed(fmt));
            m.mem.w32(strm + 8, 0)?;
            m.mem.w32(strm + 20, 0)?;
            m.mem.w32(strm + 24, 0)?;
            m.ret(0)
        },
        "inflate" => inflate,
        "inflateEnd" => |m| { let s = m.arg(0)?; let had = m.zstreams.remove(&s).is_some(); m.ret(if had { 0 } else { -2i32 as u32 }) },
        "deflateInit2_" | "deflate" | "deflateEnd" => |m| m.ret(-2i32 as u32),
        "zError" => |m| {
            let a = m.static_buf("zError", 16)?;
            m.mem.write(a, b"zlib error\0")?;
            m.ret(a)
        },
        // process
        "__errno" => |m| m.ret(m.errno_addr),
        "exit" => |m| Err(Fault::Exit(m.arg(0)? as i32)),
        "abort" => |m| Err(Fault::Other(format!("guest called abort()\n{}", m.state()))),
        "__stack_chk_fail" => |m| Err(Fault::Other(format!("guest stack canary check failed\n{}", m.state()))),
        "__gnu_Unwind_Find_exidx" => |m| {
            let (addr, count) = m.img.exidx;
            let p = m.arg(1)?;
            m.mem.w32(p, count)?;
            m.ret(LIB_BASE + addr)
        },
        // One host thread only: the sole caller is the network connect path.
        "pthread_create" => |m| { m.warn_once("guest tried to start a thread (network code); refused".into()); m.ret(EPERM) },
        // No network, by construction.
        "socket" | "connect" | "bind" | "send" | "sendto" | "recv" | "select" | "setsockopt" | "shutdown" => |m| {
            m.warn_once("guest attempted network access; refused".into());
            m.set_errno(ENETUNREACH)?;
            m.ret(u32::MAX)
        },
        "gethostbyname" => |m| {
            let host = m.mem.cstr_lossy(m.arg(0)?)?;
            m.warn_once(format!("guest tried to resolve host `{host}`; refused"));
            m.ret(0)
        },
        "inet_addr" => |m| m.ret(u32::MAX),
        "inet_ntoa" => |m| {
            let a = m.static_buf("inet_ntoa", 16)?;
            m.mem.write(a, b"0.0.0.0\0")?;
            m.ret(a)
        },
        // No dynamic loading: the only user binds the system GL library, which is not offered.
        "dlopen" => |m| {
            let name = m.mem.cstr_lossy(m.arg(0)?)?;
            m.warn_once(format!("guest tried to dlopen `{name}`; refused"));
            m.ret(0)
        },
        "dlsym" | "dlclose" => |m| m.ret(0),
        n if n.starts_with("gl") => |m| {
            m.warn_once("guest called OpenGL; ignored (the host uses the software renderer)".into());
            m.ret(0)
        },
        // C++ exception machinery: only reached if the guest throws, which it should not.
        _ => return None,
    };
    Some(h)
}

fn ord(o: Ordering) -> u32 {
    match o {
        Ordering::Less => u32::MAX,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }
}

/// C-style integer prefix parse. Returns (value, bytes consumed).
fn parse_int(s: &[u8], base: u32) -> (i64, usize) {
    let mut i = s.iter().take_while(|b| b.is_ascii_whitespace()).count();
    let neg = s.get(i) == Some(&b'-');
    if matches!(s.get(i), Some(b'-' | b'+')) {
        i += 1;
    }
    let mut base = base;
    let hex = s.get(i) == Some(&b'0') && matches!(s.get(i + 1), Some(b'x' | b'X'));
    if (base == 0 || base == 16) && hex {
        base = 16;
        i += 2;
    } else if base == 0 {
        base = if s.get(i) == Some(&b'0') { 8 } else { 10 };
    }
    if !(2..=36).contains(&base) {
        return (0, 0);
    }
    let start = i;
    let mut v: u64 = 0;
    while let Some(d) = s.get(i).and_then(|&c| (c as char).to_digit(base)) {
        v = v.wrapping_mul(base as u64).wrapping_add(d as u64).min(u32::MAX as u64);
        i += 1;
    }
    if i == start {
        return (0, 0);
    }
    (if neg { -(v as i64) } else { v as i64 }, i)
}

/// Write formatted output plus a NUL to the guest, optionally bounded like `snprintf`.
fn emit(m: &mut Machine, buf: u32, limit: Option<u32>, out: Vec<u8>) -> R<()> {
    match limit {
        None => {
            m.mem.write(buf, &out)?;
            m.mem.w8(buf.wrapping_add(out.len() as u32), 0)?;
        }
        Some(0) => {}
        Some(n) => {
            let k = out.len().min(n as usize - 1);
            m.mem.write(buf, &out[..k])?;
            m.mem.w8(buf + k as u32, 0)?;
        }
    }
    m.ret(out.len() as u32)
}

fn calloc(m: &mut Machine) -> R<()> {
    let Some(n) = m.arg(0)?.checked_mul(m.arg(1)?) else { return m.ret(0) };
    let a = m.heap.alloc(n);
    if a != 0 {
        m.mem.slice_mut(a, n.max(1))?.fill(0);
    }
    m.ret(a)
}

fn realloc(m: &mut Machine) -> R<()> {
    let (p, n) = (m.arg(0)?, m.arg(1)?);
    if p == 0 {
        let a = m.heap.alloc(n);
        return m.ret(a);
    }
    let Some(cap) = m.heap.capacity(p) else {
        m.warn_once(format!("realloc of a pointer that is not a live block ({p:#x})"));
        return m.ret(0);
    };
    if n == 0 {
        m.heap.free(p);
        return m.ret(0);
    }
    if n <= cap {
        return m.ret(p);
    }
    let a = m.heap.alloc(n);
    if a != 0 {
        m.mem.copy(a, p, cap)?;
        m.heap.free(p);
    }
    m.ret(a)
}

fn free(m: &mut Machine) -> R<()> {
    let p = m.arg(0)?;
    if p != 0 && !m.heap.free(p) {
        let lr = m.reg(unicorn_engine::RegisterARM::LR)?;
        let from = m.symbolize(lr);
        m.warn_once(format!("free of a pointer that is not a live block ({p:#x}) from {from}; ignored"));
    }
    Ok(())
}

fn qsort(m: &mut Machine) -> R<()> {
    let (base, n, size, cmp) = (m.arg(0)?, m.arg(1)?, m.arg(2)?, m.arg(3)?);
    let total = n.checked_mul(size).ok_or(Fault::BadAccess { addr: base, len: u32::MAX, write: true })?;
    if n < 2 || size == 0 {
        return Ok(());
    }
    let data = m.mem.slice(base, total)?.to_vec();
    let elems: Vec<&[u8]> = data.chunks_exact(size as usize).collect();
    let (a, b) = (m.galloc(size)?, m.galloc(size)?);
    // Hand-written merge sort: it must tolerate an inconsistent guest comparator.
    let mut order: Vec<usize> = (0..n as usize).collect();
    let mut tmp = order.clone();
    let mut width = 1;
    while width < order.len() {
        for lo in (0..order.len()).step_by(2 * width) {
            let (mid, hi) = ((lo + width).min(order.len()), (lo + 2 * width).min(order.len()));
            let (mut i, mut j, mut k) = (lo, mid, lo);
            while i < mid && j < hi {
                m.mem.write(a, elems[order[i]])?;
                m.mem.write(b, elems[order[j]])?;
                if (m.call(cmp, &[a, b])? as i32) <= 0 {
                    tmp[k] = order[i];
                    i += 1;
                } else {
                    tmp[k] = order[j];
                    j += 1;
                }
                k += 1;
            }
            while i < mid {
                tmp[k] = order[i];
                i += 1;
                k += 1;
            }
            while j < hi {
                tmp[k] = order[j];
                j += 1;
                k += 1;
            }
        }
        std::mem::swap(&mut order, &mut tmp);
        width *= 2;
    }
    let sorted: Vec<u8> = order.iter().flat_map(|&i| elems[i].iter().copied()).collect();
    m.mem.write(base, &sorted)?;
    m.heap.free(a);
    m.heap.free(b);
    Ok(())
}

// ---- time -------------------------------------------------------------------------

/// Days since 1970-01-01 to (year, month 1-12, day 1-31).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + (mth <= 2) as i64, mth, d)
}

fn days_from_civil(y: i64, mth: i64, d: i64) -> i64 {
    let y = if mth <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if mth > 2 { mth - 3 } else { mth + 9 }) + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

/// The guest's clock is UTC; the host's time zone is not exposed.
fn localtime(m: &mut Machine) -> R<()> {
    let t = m.mem.r32(m.arg(0)?)? as i32 as i64;
    let (days, secs) = (t.div_euclid(86_400), t.rem_euclid(86_400));
    let (y, mth, d) = civil(days);
    let yday = days - days_from_civil(y, 1, 1);
    let tm = m.static_buf("tm", 64)?;
    let fields = [secs % 60, secs / 60 % 60, secs / 3600, d as i64, mth as i64 - 1, y - 1900, (days + 4).rem_euclid(7), yday, 0];
    for (i, v) in fields.iter().enumerate() {
        m.mem.w32(tm + 4 * i as u32, *v as u32)?;
    }
    m.ret(tm)
}

fn mktime(m: &mut Machine) -> R<()> {
    let tm = m.arg(0)?;
    let mut f = [0i64; 6];
    for (i, v) in f.iter_mut().enumerate() {
        *v = m.mem.r32(tm + 4 * i as u32)? as i32 as i64;
    }
    let months = f[5] * 12 + f[4];
    let days = days_from_civil(1900 + months.div_euclid(12), months.rem_euclid(12) + 1, 1) + f[3] - 1;
    m.ret((days * 86_400 + f[2] * 3600 + f[1] * 60 + f[0]) as u32)
}

// ---- files ------------------------------------------------------------------------

fn fopen(m: &mut Machine) -> R<()> {
    let path = m.mem.cstr(m.arg(0)?)?.to_vec();
    let mode = m.mem.cstr(m.arg(1)?)?.to_vec();
    let file = m.galloc(FILE_SIZE)?;
    let res = m.vfs.fopen(file, &path, &mode);
    if m.vfs.verbose {
        eprintln!("[vfs] fopen({:?}, {:?}) -> {}", String::from_utf8_lossy(&path), String::from_utf8_lossy(&mode), if res.is_ok() { "ok" } else { "refused" });
    }
    match res {
        Ok(fd) => {
            m.mem.w16(file + 14, fd as u16)?;
            m.ret(file)
        }
        Err(e) => {
            m.heap.free(file);
            m.set_errno(e)?;
            m.ret(0)
        }
    }
}

fn fclose(m: &mut Machine) -> R<()> {
    let f = m.arg(0)?;
    if m.vfs.open.remove(&f).is_some() {
        m.heap.free(f);
        m.ret(0)
    } else {
        m.set_errno(EBADF)?;
        m.ret(u32::MAX)
    }
}

fn with_file(m: &mut Machine, arg: u32, f: impl FnOnce(&mut vfs::Open) -> u32) -> R<()> {
    let key = m.arg(arg)?;
    match m.vfs.open.get_mut(&key) {
        Some(o) => {
            let v = f(o);
            m.ret(v)
        }
        None => {
            m.set_errno(EBADF)?;
            m.ret(u32::MAX)
        }
    }
}

fn fread(m: &mut Machine) -> R<()> {
    let (ptr, size, n, f) = (m.arg(0)?, m.arg(1)?, m.arg(2)?, m.arg(3)?);
    let total = size.checked_mul(n).ok_or(Fault::BadAccess { addr: ptr, len: u32::MAX, write: true })?;
    if total == 0 {
        return m.ret(0);
    }
    let buf = m.mem.slice_mut(ptr, total)?;
    let Some(o) = m.vfs.open.get_mut(&f) else { return m.ret(0) };
    let got = o.read(buf) as u32;
    if got < total {
        let fl = m.mem.r16(f + FILE_FLAGS)?;
        m.mem.w16(f + FILE_FLAGS, fl | EOF_FLAG)?;
    }
    m.ret(got / size)
}

fn fwrite(m: &mut Machine) -> R<()> {
    let (ptr, size, n, f) = (m.arg(0)?, m.arg(1)?, m.arg(2)?, m.arg(3)?);
    let total = size.checked_mul(n).ok_or(Fault::BadAccess { addr: ptr, len: u32::MAX, write: false })?;
    if total == 0 {
        return m.ret(0);
    }
    let buf = m.mem.slice(ptr, total)?;
    let Some(o) = m.vfs.open.get_mut(&f) else { return m.ret(0) };
    let done = o.write(buf) as u32;
    if done < total {
        let fl = m.mem.r16(f + FILE_FLAGS)?;
        m.mem.w16(f + FILE_FLAGS, fl | ERR_FLAG)?;
    }
    m.ret(done / size)
}

fn fseek(m: &mut Machine) -> R<()> {
    let (f, off, whence) = (m.arg(0)?, m.arg(1)? as i32 as i64, m.arg(2)?);
    let res = match m.vfs.open.get_mut(&f) {
        Some(o) => o.seek(off, whence),
        None => Err(EBADF),
    };
    match res {
        Ok(_) => {
            let fl = m.mem.r16(f + FILE_FLAGS)?;
            m.mem.w16(f + FILE_FLAGS, fl & !EOF_FLAG)?;
            m.ret(0)
        }
        Err(e) => {
            m.set_errno(e)?;
            m.ret(u32::MAX)
        }
    }
}

/// Fill a bionic (32-bit ARM) `struct stat` at argument 1.
fn put_stat(m: &mut Machine, r: Result<vfs::Stat, u32>) -> R<()> {
    let st = m.arg(1)?;
    match r {
        Ok(s) => {
            m.mem.slice_mut(st, 104)?.fill(0);
            m.mem.w32(st + 16, if s.is_dir { 0o040755 } else { 0o100644 })?;
            m.mem.w32(st + 20, 1)?;
            m.mem.w64(st + 48, s.size)?;
            m.mem.w32(st + 56, 4096)?;
            m.mem.w64(st + 64, s.size.div_ceil(512))?;
            for off in [72, 80, 88] {
                m.mem.w32(st + off, s.mtime as u32)?;
            }
            m.ret(0)
        }
        Err(e) => {
            m.set_errno(e)?;
            m.ret(u32::MAX)
        }
    }
}

/// Run a path-based operation, which is only ever allowed inside the save directory.
fn path_op(m: &mut Machine, f: impl FnOnce(&std::path::Path, &mut Machine) -> bool) -> R<()> {
    let path = m.mem.cstr(m.arg(0)?)?.to_vec();
    let r = m.vfs.doc_path(&path);
    if m.vfs.verbose {
        eprintln!("[vfs] path op on {:?} -> {}", String::from_utf8_lossy(&path), if r.is_ok() { "allowed" } else { "refused" });
    }
    match r {
        Ok(p) => {
            if f(&p, m) {
                m.ret(0)
            } else {
                m.set_errno(EINVAL)?;
                m.ret(u32::MAX)
            }
        }
        Err(e) => {
            m.set_errno(e)?;
            m.ret(u32::MAX)
        }
    }
}

// ---- zlib -------------------------------------------------------------------------

fn inflate(m: &mut Machine) -> R<()> {
    let strm = m.arg(0)?;
    let (next_in, avail_in) = (m.mem.r32(strm)?, m.mem.r32(strm + 4)?);
    let (next_out, avail_out) = (m.mem.r32(strm + 12)?, m.mem.r32(strm + 16)?);
    let input = m.mem.slice(next_in, avail_in)?.to_vec();
    // Validate the output range before sizing a host buffer from it.
    m.mem.slice_mut(next_out, avail_out)?;
    let Some(state) = m.zstreams.get_mut(&strm) else { return m.ret(-2i32 as u32) };
    let mut out = vec![0u8; avail_out as usize];
    let res = mz_inflate(state, &input, &mut out, MZFlush::None);
    let (used, made) = (res.bytes_consumed as u32, res.bytes_written as u32);
    m.mem.write(next_out, &out[..made as usize])?;
    m.mem.w32(strm, next_in + used)?;
    m.mem.w32(strm + 4, avail_in - used)?;
    m.mem.w32(strm + 8, m.mem.r32(strm + 8)?.wrapping_add(used))?;
    m.mem.w32(strm + 12, next_out + made)?;
    m.mem.w32(strm + 16, avail_out - made)?;
    m.mem.w32(strm + 20, m.mem.r32(strm + 20)?.wrapping_add(made))?;
    let code: i32 = match res.status {
        Ok(MZStatus::Ok) => 0,
        Ok(MZStatus::StreamEnd) => 1,
        Ok(MZStatus::NeedDict) => 2,
        Err(MZError::Buf) => -5,
        Err(MZError::Data) => -3,
        Err(MZError::Mem) => -4,
        Err(_) => -2,
    };
    m.ret(code as u32)
}
