//! Guest address space.
//!
//! Every region the guest can touch is a host allocation owned by [`Mem`] and mapped into
//! the emulator by pointer. The host only ever reaches guest memory through the checked
//! accessors here, so a bad guest pointer or length becomes a [`Fault`], never a host
//! out-of-bounds access.

use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::fmt;

pub const PAGE: u32 = 0x1000;

#[derive(Debug, Clone)]
pub enum Fault {
    /// The guest handed the host an address range that is not mapped (or not writable).
    BadAccess { addr: u32, len: u32, write: bool },
    /// The emulator stopped on a CPU-level error (unmapped fetch, invalid instruction, ...).
    Cpu(String),
    /// The guest called something the host does not provide.
    Unimplemented(String),
    /// The guest asked to exit (`exit`, `abort`, `OnExitApplication`).
    Exit(i32),
    /// Anything else the host refuses to continue from.
    Other(String),
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fault::BadAccess { addr, len, write } => write!(
                f,
                "guest passed a bad pointer: {} of {len} bytes at {addr:#010x}",
                if *write { "write" } else { "read" }
            ),
            Fault::Cpu(s) => write!(f, "cpu fault: {s}"),
            Fault::Unimplemented(s) => write!(f, "not implemented: {s}"),
            Fault::Exit(c) => write!(f, "guest exited with code {c}"),
            Fault::Other(s) => write!(f, "{s}"),
        }
    }
}

pub type R<T> = Result<T, Fault>;

struct Region {
    base: u32,
    len: u32,
    writable: bool,
    ptr: *mut u8,
    name: &'static str,
}

impl Drop for Region {
    fn drop(&mut self) {
        unsafe { dealloc(self.ptr, Layout::from_size_align(self.len as usize, PAGE as usize).unwrap()) }
    }
}

#[derive(Default)]
pub struct Mem {
    regions: Vec<Region>,
    #[cfg(test)]
    pub aot_write_log: Option<std::collections::BTreeMap<u32, u8>>,
}

impl Mem {
    /// Allocate a zeroed, page-aligned region. Returns the host pointer so the caller can
    /// map it into the emulator. `writable` controls whether the *host* will write there on
    /// the guest's behalf (the emulator enforces the guest's own permissions separately).
    pub fn add(&mut self, name: &'static str, base: u32, len: u32, writable: bool) -> *mut u8 {
        assert!(base % PAGE == 0 && len % PAGE == 0 && len > 0);
        assert!(base.checked_add(len).is_some(), "region wraps the address space");
        for r in &self.regions {
            assert!(base + len <= r.base || r.base + r.len <= base, "regions overlap");
        }
        let ptr = unsafe { alloc_zeroed(Layout::from_size_align(len as usize, PAGE as usize).unwrap()) };
        assert!(!ptr.is_null(), "out of host memory");
        self.regions.push(Region { base, len, writable, ptr, name });
        ptr
    }

    pub fn describe(&self, addr: u32) -> &'static str {
        self.regions.iter().find(|r| addr >= r.base && addr - r.base < r.len).map_or("unmapped", |r| r.name)
    }

    fn find(&self, addr: u32, len: u32, write: bool) -> R<(&Region, usize)> {
        for r in &self.regions {
            if addr >= r.base && addr - r.base < r.len {
                let off = addr - r.base;
                if len <= r.len - off && (!write || r.writable) {
                    return Ok((r, off as usize));
                }
                break;
            }
        }
        Err(Fault::BadAccess { addr, len, write })
    }

    pub fn slice(&self, addr: u32, len: u32) -> R<&[u8]> {
        if len == 0 {
            return Ok(&[]);
        }
        let (r, off) = self.find(addr, len, false)?;
        Ok(unsafe { std::slice::from_raw_parts(r.ptr.add(off), len as usize) })
    }

    pub fn slice_mut(&mut self, addr: u32, len: u32) -> R<&mut [u8]> {
        if len == 0 {
            return Ok(&mut []);
        }
        let (r, off) = self.find(addr, len, true)?;
        let bytes = unsafe { std::slice::from_raw_parts_mut(r.ptr.add(off), len as usize) };
        #[cfg(test)]
        if let Some(log) = &mut self.aot_write_log {
            for (offset, &byte) in bytes.iter().enumerate() {
                log.entry(addr + offset as u32).or_insert(byte);
            }
        }
        Ok(bytes)
    }

    /// Bytes available from `addr` to the end of its region (for bounded string scans).
    pub fn tail(&self, addr: u32) -> R<&[u8]> {
        let (r, off) = self.find(addr, 1, false)?;
        Ok(unsafe { std::slice::from_raw_parts(r.ptr.add(off), r.len as usize - off) })
    }

    pub fn r16(&self, a: u32) -> R<u16> {
        Ok(u16::from_le_bytes(self.slice(a, 2)?.try_into().unwrap()))
    }
    pub fn r32(&self, a: u32) -> R<u32> {
        Ok(u32::from_le_bytes(self.slice(a, 4)?.try_into().unwrap()))
    }
    pub fn r64(&self, a: u32) -> R<u64> {
        Ok(u64::from_le_bytes(self.slice(a, 8)?.try_into().unwrap()))
    }
    pub fn w8(&mut self, a: u32, v: u8) -> R<()> {
        self.slice_mut(a, 1)?[0] = v;
        Ok(())
    }
    pub fn w16(&mut self, a: u32, v: u16) -> R<()> {
        self.slice_mut(a, 2)?.copy_from_slice(&v.to_le_bytes());
        Ok(())
    }
    pub fn w32(&mut self, a: u32, v: u32) -> R<()> {
        self.slice_mut(a, 4)?.copy_from_slice(&v.to_le_bytes());
        Ok(())
    }
    pub fn w64(&mut self, a: u32, v: u64) -> R<()> {
        self.slice_mut(a, 8)?.copy_from_slice(&v.to_le_bytes());
        Ok(())
    }
    pub fn write(&mut self, a: u32, data: &[u8]) -> R<()> {
        self.slice_mut(a, data.len() as u32)?.copy_from_slice(data);
        Ok(())
    }

    /// Length of the NUL-terminated string at `a`. Faults if no NUL before the region ends.
    pub fn strlen(&self, a: u32) -> R<u32> {
        let t = self.tail(a)?;
        match t.iter().position(|&b| b == 0) {
            Some(n) => Ok(n as u32),
            None => Err(Fault::BadAccess { addr: a, len: t.len() as u32 + 1, write: false }),
        }
    }

    /// The NUL-terminated string at `a`, without the NUL.
    pub fn cstr(&self, a: u32) -> R<&[u8]> {
        let n = self.strlen(a)?;
        self.slice(a, n)
    }

    pub fn cstr_lossy(&self, a: u32) -> R<String> {
        Ok(String::from_utf8_lossy(self.cstr(a)?).into_owned())
    }

    /// `memmove` semantics between two guest ranges.
    pub fn copy(&mut self, dst: u32, src: u32, len: u32) -> R<()> {
        if len == 0 {
            return Ok(());
        }
        let (rs, os) = self.find(src, len, false)?;
        let sp = unsafe { rs.ptr.add(os) };
        let (rd, od) = self.find(dst, len, true)?;
        let dp = unsafe { rd.ptr.add(od) };
        unsafe { std::ptr::copy(sp, dp, len as usize) };
        Ok(())
    }
}
