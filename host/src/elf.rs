//! Minimal ELF32/ARM shared-object loader.
//!
//! The library file is treated as untrusted input: every offset is bounds-checked and a
//! malformed file produces an error, never a panic or an out-of-range read.

use std::collections::HashMap;

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_ARM_EXIDX: u32 = 0x7000_0001;
const DT_PLTRELSZ: u32 = 2;
const DT_HASH: u32 = 4;
const DT_STRTAB: u32 = 5;
const DT_SYMTAB: u32 = 6;
const DT_INIT: u32 = 12;
const DT_REL: u32 = 17;
const DT_RELSZ: u32 = 18;
const DT_JMPREL: u32 = 23;
const DT_INIT_ARRAY: u32 = 25;
const SHT_SYMTAB: u32 = 2;

pub const R_ARM_ABS32: u8 = 2;
pub const R_ARM_GLOB_DAT: u8 = 21;
pub const R_ARM_JUMP_SLOT: u8 = 22;
pub const R_ARM_RELATIVE: u8 = 23;

pub struct Segment {
    pub vaddr: u32,
    pub memsz: u32,
    pub write: bool,
    pub exec: bool,
}

pub struct Reloc {
    pub offset: u32,
    pub kind: u8,
    /// Symbol name and its value if the library defines it (`None` for imports).
    pub sym: Option<(String, Option<u32>, bool)>,
}

pub struct Image {
    /// Memory image at link addresses (index = vaddr), zero-filled where the file has none.
    pub data: Vec<u8>,
    pub segments: Vec<Segment>,
    pub relocs: Vec<Reloc>,
    /// Exported functions and objects: name -> vaddr.
    pub exports: HashMap<String, u32>,
    /// All named functions sorted by address, for diagnostics.
    pub symbols: Vec<(u32, String)>,
    /// `.ARM.exidx` (vaddr, entry count), for `__gnu_Unwind_Find_exidx`.
    pub exidx: (u32, u32),
}

struct Rd<'a>(&'a [u8]);
impl Rd<'_> {
    fn u16(&self, o: usize) -> Result<u16, String> {
        self.0.get(o..o + 2).map(|b| u16::from_le_bytes(b.try_into().unwrap())).ok_or_else(|| format!("truncated at {o:#x}"))
    }
    fn u32(&self, o: usize) -> Result<u32, String> {
        self.0.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).ok_or_else(|| format!("truncated at {o:#x}"))
    }
    fn cstr(&self, o: usize) -> Result<String, String> {
        let t = self.0.get(o..).ok_or("string offset out of range")?;
        let n = t.iter().position(|&b| b == 0).ok_or("unterminated string")?;
        Ok(String::from_utf8_lossy(&t[..n]).into_owned())
    }
}

pub fn load(file: &[u8]) -> Result<Image, String> {
    let f = Rd(file);
    if file.get(..7) != Some(&[0x7f, b'E', b'L', b'F', 1, 1, 1]) {
        return Err("not a 32-bit little-endian ELF file".into());
    }
    if f.u16(16)? != 3 || f.u16(18)? != 40 {
        return Err("not an ARM shared object".into());
    }
    let (phoff, phentsize, phnum) = (f.u32(28)? as usize, f.u16(42)? as usize, f.u16(44)? as usize);
    if phentsize < 32 {
        return Err("bad program header size".into());
    }

    let mut segments = Vec::new();
    let mut loads = Vec::new();
    let mut dynamic = None;
    let mut exidx = (0, 0);
    let mut top = 0u32;
    for i in 0..phnum {
        let p = phoff + i * phentsize;
        let (ty, off, vaddr, filesz, memsz, flags) =
            (f.u32(p)?, f.u32(p + 4)?, f.u32(p + 8)?, f.u32(p + 16)?, f.u32(p + 20)?, f.u32(p + 24)?);
        match ty {
            PT_LOAD => {
                if filesz > memsz {
                    return Err("segment file size exceeds memory size".into());
                }
                let end = vaddr.checked_add(memsz).filter(|&e| e <= 0x0400_0000).ok_or("segment too large")?;
                top = top.max(end);
                segments.push(Segment { vaddr, memsz, write: flags & 2 != 0, exec: flags & 1 != 0 });
                loads.push((off as usize, vaddr as usize, filesz as usize));
            }
            PT_DYNAMIC => dynamic = Some((vaddr as usize, memsz as usize)),
            PT_ARM_EXIDX => exidx = (vaddr, memsz / 8),
            _ => {}
        }
    }
    if segments.is_empty() {
        return Err("no loadable segments".into());
    }
    for s in &segments {
        if s.write && s.exec {
            return Err("refusing to load a segment that is both writable and executable".into());
        }
    }
    let mut data = vec![0u8; ((top + 0xfff) & !0xfff) as usize];
    for (off, vaddr, filesz) in loads {
        let src = file.get(off..off + filesz).ok_or("segment data out of range")?;
        data[vaddr..vaddr + filesz].copy_from_slice(src);
    }

    // Dynamic section (read from the memory image, addresses are vaddrs).
    let m = Rd(&data);
    let (dyn_at, dyn_len) = dynamic.ok_or("no dynamic section")?;
    let mut dt: HashMap<u32, u32> = HashMap::new();
    for i in 0..dyn_len / 8 {
        let tag = m.u32(dyn_at + i * 8)?;
        if tag == 0 {
            break;
        }
        dt.entry(tag).or_insert(m.u32(dyn_at + i * 8 + 4)?);
    }
    if dt.contains_key(&DT_INIT) || dt.contains_key(&DT_INIT_ARRAY) {
        return Err("library has initialisers, which this host does not run".into());
    }
    let need = |t: u32, n: &str| dt.get(&t).copied().ok_or(format!("missing {n}"));
    let symtab = need(DT_SYMTAB, "DT_SYMTAB")? as usize;
    let strtab = need(DT_STRTAB, "DT_STRTAB")? as usize;
    let nsyms = m.u32(need(DT_HASH, "DT_HASH")? as usize + 4)? as usize;

    let dynsym = |idx: usize| -> Result<(String, Option<u32>, bool), String> {
        if idx >= nsyms {
            return Err("symbol index out of range".into());
        }
        let s = symtab + idx * 16;
        let name = m.cstr(strtab + m.u32(s)? as usize)?;
        let value = m.u32(s + 4)?;
        let info = *data.get(s + 12).ok_or("truncated symbol")?;
        let shndx = m.u16(s + 14)?;
        Ok((name, (shndx != 0).then_some(value), info & 0xf == 2))
    };

    let mut exports = HashMap::new();
    for i in 1..nsyms {
        if let (name, Some(v), _) = dynsym(i)? {
            exports.insert(name, v);
        }
    }

    let mut relocs = Vec::new();
    for (tab, sz) in [(DT_REL, DT_RELSZ), (DT_JMPREL, DT_PLTRELSZ)] {
        let (Some(&at), Some(&len)) = (dt.get(&tab), dt.get(&sz)) else { continue };
        for i in 0..(len / 8) as usize {
            let offset = m.u32(at as usize + i * 8)?;
            let info = m.u32(at as usize + i * 8 + 4)?;
            let (kind, symidx) = (info as u8, (info >> 8) as usize);
            if offset.checked_add(4).map_or(true, |e| e as usize > data.len()) {
                return Err("relocation target out of range".into());
            }
            let sym = match kind {
                R_ARM_RELATIVE => None,
                R_ARM_ABS32 | R_ARM_GLOB_DAT | R_ARM_JUMP_SLOT => Some(dynsym(symidx)?),
                k => return Err(format!("unsupported relocation type {k}")),
            };
            relocs.push(Reloc { offset, kind, sym });
        }
    }

    // Full symbol table from the section headers, if present (diagnostics only).
    let mut symbols = Vec::new();
    let (shoff, shentsize, shnum) = (f.u32(32)? as usize, f.u16(46)? as usize, f.u16(48)? as usize);
    if shentsize >= 40 {
        for i in 0..shnum {
            let s = shoff + i * shentsize;
            if f.u32(s + 4).ok() != Some(SHT_SYMTAB) {
                continue;
            }
            let (off, size, link) = (f.u32(s + 16)? as usize, f.u32(s + 20)? as usize, f.u32(s + 24)? as usize);
            let stroff = f.u32(shoff + link * shentsize + 16)? as usize;
            for j in 0..size / 16 {
                let e = off + j * 16;
                let (Ok(name), Ok(value), Some(&info)) = (f.u32(e), f.u32(e + 4), file.get(e + 12)) else { break };
                if info & 0xf == 2 && value != 0 {
                    if let Ok(n) = f.cstr(stroff + name as usize) {
                        symbols.push((value, n));
                    }
                }
            }
        }
    }
    if symbols.is_empty() {
        symbols = exports.iter().map(|(n, &v)| (v, n.clone())).collect();
    }
    symbols.sort();
    symbols.dedup_by_key(|s| s.0);

    Ok(Image { data, segments, relocs, exports, symbols, exidx })
}
