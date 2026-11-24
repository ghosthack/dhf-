//! The emulated machine: CPU, guest memory, and the trap mechanism through which the
//! guest reaches the host.
//!
//! Guest calls to host services go through the trap page. Each imported function and
//! each JNI table slot is bound to an address in that page. A
//! code hook on that page records the address and stops the CPU; the handler then runs as
//! ordinary Rust in [`Machine::run`], outside the emulator, with bounds-checked access to
//! guest memory. Nothing in the guest can reach a host function that is not in the table.

use crate::aot;
use crate::elf::{self, Image};
use crate::heap::Heap;
use crate::java::Java;
use crate::jni::Jni;
use crate::mem::{Fault, Mem, PAGE, R};
use crate::vfs::Vfs;
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use unicorn_engine::unicorn_const::{Arch, Mode, Prot};
use unicorn_engine::{RegisterARM, RegisterARMCP, Unicorn};

pub const TRAP_BASE: u32 = 0x0fff_0000;
pub const TRAP_LEN: u32 = 0x2000;
/// Return address for host-initiated calls: the first trap slot.
pub const RET_ADDR: u32 = TRAP_BASE;
/// `emu_start` wants an end address; this one is unmapped and never reached.
const NEVER: u64 = 0xffff_fff0;
/// Guest page tables (read-only to the guest). See [`Machine::enable_mmu`].
pub const PT_BASE: u32 = 0x0800_0000;
pub const PT_LEN: u32 = 0x0010_0000;
pub const LIB_BASE: u32 = 0x1000_0000;
pub const HEAP_BASE: u32 = 0x4000_0000;
pub const HEAP_LEN: u32 = 0x1000_0000;
pub const STACK_BASE: u32 = 0x7000_0000;
pub const STACK_LEN: u32 = 0x0010_0000;

pub type Handler = fn(&mut Machine) -> R<()>;

#[derive(Clone, Copy)]
pub enum Trap {
    Import(usize),
    JniEnv(u32),
    JavaVm(u32),
}

pub struct Import {
    pub name: String,
    pub handler: Option<Handler>,
    pub calls: u64,
}

#[derive(Default, Clone)]
pub struct Options {
    /// Log every import and JNI call.
    pub trace: bool,
    /// Log file-system and Java-layer activity.
    pub verbose: bool,
    /// Enable native translations for the known library.
    pub aot: bool,
    /// Count executed guest instructions per function (slow; for performance work).
    pub profile: bool,
}

pub struct Machine {
    pub uc: Unicorn<'static, ()>,
    pub mem: Mem,
    pub heap: Heap,
    pub img: Image,
    pub opts: Options,
    pub aot: Option<Rc<aot::Status>>,
    pub vfs: Vfs,
    pub jni: Jni,
    pub java: Java,
    pub clock: Clock,
    pub imports: Vec<Import>,
    pub zstreams: HashMap<u32, Box<miniz_oxide::inflate::stream::InflateState>>,
    /// Lazily allocated guest scratch buffers, keyed by purpose.
    pub statics: HashMap<&'static str, u32>,
    pub errno_addr: u32,
    traps: Vec<Trap>,
    hit: Rc<Cell<u32>>,
    /// Set by the interrupt hook when the guest CPU raises an exception.
    excp: Rc<Cell<Option<u32>>>,
    /// Guest instruction counts per basic-block address, when profiling.
    pub profile: Rc<std::cell::RefCell<HashMap<u32, u64>>>,
    warned: std::collections::HashSet<String>,
    depth: u32,
}

/// Wall clock seen by the guest. Virtual time makes headless runs reproducible.
pub enum Clock {
    Real,
    Virtual { micros: u64 },
}

impl Clock {
    pub fn now_micros(&self) -> u64 {
        match self {
            Clock::Real => std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_micros() as u64),
            Clock::Virtual { micros } => *micros,
        }
    }
    pub fn advance_ms(&mut self, ms: u64) {
        if let Clock::Virtual { micros } = self {
            *micros += ms * 1000;
        }
    }
}

/// Fill `tables` (mapped at [`PT_BASE`]) with ARMv7 short-descriptor page tables that
/// identity-map `layout`: 1 MiB sections where a megabyte is uniform, 4 KiB pages
/// elsewhere. Non-executable ranges get the execute-never bit.
fn build_page_tables(tables: &mut [u8], layout: &[(u32, u32, bool)]) -> Result<(), String> {
    const MB: u32 = 1 << 20;
    const AP_FULL: u32 = 3;
    let put = |tables: &mut [u8], off: u32, v: u32| tables[off as usize..off as usize + 4].copy_from_slice(&v.to_le_bytes());
    let mut next_l2 = 0x4000u32; // the 16 KiB first-level table comes first
    let mut l2_of: HashMap<u32, u32> = HashMap::new();
    for &(base, len, exec) in layout {
        let mut addr = base;
        let end = base + len;
        while addr < end {
            let mb = addr / MB;
            if addr % MB == 0 && end - addr >= MB && !l2_of.contains_key(&mb) {
                put(tables, mb * 4, addr | AP_FULL << 10 | (!exec as u32) << 4 | 2);
                addr += MB;
                continue;
            }
            let l2 = match l2_of.get(&mb) {
                Some(&t) => t,
                None => {
                    if next_l2 + 0x400 > PT_LEN {
                        return Err("page table region too small".into());
                    }
                    let t = next_l2;
                    next_l2 += 0x400;
                    l2_of.insert(mb, t);
                    put(tables, mb * 4, (PT_BASE + t) | 1);
                    t
                }
            };
            put(tables, l2 + (addr % MB) / PAGE * 4, addr | AP_FULL << 4 | 2 | !exec as u32);
            addr += PAGE;
        }
    }
    Ok(())
}

fn cpu<T>(r: Result<T, unicorn_engine::uc_error>, what: &str) -> R<T> {
    r.map_err(|e| Fault::Cpu(format!("{what}: {e:?}")))
}

impl Machine {
    pub fn new(lib: &[u8], vfs: Vfs, java: Java, clock: Clock, opts: Options) -> Result<Machine, String> {
        let img = elf::load(lib)?;
        let mut uc = Unicorn::new(Arch::ARM, Mode::LITTLE_ENDIAN).map_err(|e| format!("emulator init: {e:?}"))?;
        let mut mem = Mem::default();
        let map = |uc: &mut Unicorn<'static, ()>, mem: &mut Mem, name, base: u32, len: u32, prot: Prot| -> Result<*mut u8, String> {
            let ptr = mem.add(name, base, len, prot.0 & Prot::WRITE.0 != 0);
            unsafe { uc.mem_map_ptr(base as u64, len as u64, prot, ptr.cast()) }.map_err(|e| format!("map {name}: {e:?}"))?;
            Ok(ptr)
        };

        // Trap page: every slot is `bx lr`, so a handled call simply returns.
        let p = map(&mut uc, &mut mem, "traps", TRAP_BASE, TRAP_LEN, Prot::READ | Prot::EXEC)?;
        let page = unsafe { std::slice::from_raw_parts_mut(p, TRAP_LEN as usize) };
        for w in page.chunks_exact_mut(4) {
            w.copy_from_slice(&0xe12f_ff1eu32.to_le_bytes());
        }

        let mut m = Machine {
            uc,
            mem,
            heap: Heap::new(HEAP_BASE, HEAP_LEN),
            img,
            opts,
            aot: None,
            vfs,
            jni: Jni::default(),
            java,
            clock,
            imports: Vec::new(),
            zstreams: HashMap::new(),
            statics: HashMap::new(),
            errno_addr: 0,
            traps: Vec::new(),
            hit: Rc::new(Cell::new(0)),
            excp: Rc::new(Cell::new(None)),
            profile: Default::default(),
            warned: Default::default(),
            depth: 0,
        };

        map(&mut m.uc, &mut m.mem, "heap", HEAP_BASE, HEAP_LEN, Prot::READ | Prot::WRITE)?;
        map(&mut m.uc, &mut m.mem, "stack", STACK_BASE, STACK_LEN, Prot::READ | Prot::WRITE)?;
        // (base, length, executable) of everything the guest may address.
        let mut layout = vec![(TRAP_BASE, TRAP_LEN, true), (HEAP_BASE, HEAP_LEN, false), (STACK_BASE, STACK_LEN, false)];
        m.errno_addr = m.heap.alloc(16);

        // Relocate the image in a private copy, then place each segment.
        let mut data = std::mem::take(&mut m.img.data);
        let relocs = std::mem::take(&mut m.img.relocs);
        let mut import_slots: HashMap<String, u32> = HashMap::new();
        for r in &relocs {
            let at = r.offset as usize;
            let cur = u32::from_le_bytes(data[at..at + 4].try_into().unwrap());
            let val = match (&r.sym, r.kind) {
                (None, _) => cur.wrapping_add(LIB_BASE),
                (Some((name, def, is_func)), kind) => {
                    let s = match def {
                        Some(v) => LIB_BASE + v,
                        None if name == "__stack_chk_guard" => {
                            let g = m.heap.alloc(16);
                            // A fixed canary is fine: the guest cannot leak it anywhere useful.
                            m.mem.w32(g, 0x5a3c_96e1).map_err(|e| e.to_string())?;
                            g
                        }
                        None => {
                            let _ = is_func;
                            match import_slots.get(name) {
                                Some(&a) => a,
                                None => {
                                    let idx = m.imports.len();
                                    m.imports.push(Import { name: name.clone(), handler: crate::libc::lookup(name), calls: 0 });
                                    let a = m.new_trap(Trap::Import(idx));
                                    import_slots.insert(name.clone(), a);
                                    a
                                }
                            }
                        }
                    };
                    if kind == elf::R_ARM_ABS32 { cur.wrapping_add(s) } else { s }
                }
            };
            data[at..at + 4].copy_from_slice(&val.to_le_bytes());
        }
        for i in 0..m.img.segments.len() {
            let (vaddr, memsz, write, exec) = {
                let s = &m.img.segments[i];
                (s.vaddr, s.memsz, s.write, s.exec)
            };
            let start = vaddr & !(PAGE - 1);
            let end = (vaddr + memsz + PAGE - 1) & !(PAGE - 1);
            let prot = if write { Prot::READ | Prot::WRITE } else if exec { Prot::READ | Prot::EXEC } else { Prot::READ };
            let name = if write { "lib data" } else { "lib code" };
            let p = map(&mut m.uc, &mut m.mem, name, LIB_BASE + start, end - start, prot)?;
            unsafe { std::ptr::copy_nonoverlapping(data[start as usize..].as_ptr(), p, (end - start) as usize) };
            layout.push((LIB_BASE + start, end - start, exec));
        }
        let pt = map(&mut m.uc, &mut m.mem, "page tables", PT_BASE, PT_LEN, Prot::READ)?;
        let tables = unsafe { std::slice::from_raw_parts_mut(pt, PT_LEN as usize) };
        build_page_tables(tables, &layout)?;

        crate::jni::install(&mut m).map_err(|e| e.to_string())?;

        let hit = m.hit.clone();
        m.uc
            .add_code_hook(TRAP_BASE as u64, (TRAP_BASE + TRAP_LEN - 1) as u64, move |uc, addr, _| {
                hit.set(addr as u32);
                let _ = uc.emu_stop();
            })
            .map_err(|e| format!("hook: {e:?}"))?;
        // Any CPU exception (bad pointer, undefined instruction, svc) ends the run: the
        // guest has no exception handlers and no kernel to call.
        let excp = m.excp.clone();
        m.uc
            .add_intr_hook(move |uc, n| {
                excp.set(Some(n));
                let _ = uc.emu_stop();
            })
            .map_err(|e| format!("hook: {e:?}"))?;

        if m.opts.profile {
            let prof = m.profile.clone();
            m.uc
                .add_block_hook(LIB_BASE as u64, (LIB_BASE + 0x0100_0000) as u64, move |_, addr, size| {
                    *prof.borrow_mut().entry(addr as u32).or_insert(0) += (size / 4) as u64;
                })
                .map_err(|e| format!("hook: {e:?}"))?;
        }

        if m.opts.aot {
            m.aot = aot::install(&mut m.uc, lib).map_err(|e| format!("AOT hook: {e:?}"))?;
        }

        // Enable VFP: full access to cp10/cp11, then FPEXC.EN.
        let e = |r: Result<(), unicorn_engine::uc_error>| r.map_err(|e| format!("cpu setup: {e:?}"));
        let cpacr = m.uc.reg_read(RegisterARM::C1_C0_2).map_err(|e| format!("{e:?}"))?;
        e(m.uc.reg_write(RegisterARM::C1_C0_2, cpacr | (0xf << 20)))?;
        e(m.uc.reg_write(RegisterARM::FPEXC, 0x4000_0000))?;
        e(m.uc.reg_write(RegisterARM::SP, (STACK_BASE + STACK_LEN - 64) as u64))?;
        m.enable_mmu()?;
        Ok(m)
    }

    fn cp15(&mut self, crn: u32, crm: u32, opc2: u32, set: impl FnOnce(u64) -> u64) -> Result<u64, String> {
        let mut r = RegisterARMCP { cp: 15, is64: 0, sec: 0, crn, crm, opc1: 0, opc2, val: 0 };
        self.uc.reg_read_arm_coproc(&mut r).map_err(|e| format!("cp15 read: {e:?}"))?;
        let old = r.val;
        r.val = set(old);
        if r.val != old {
            self.uc.reg_write_arm_coproc(&r).map_err(|e| format!("cp15 write: {e:?}"))?;
        }
        Ok(old)
    }

    /// Turn on the guest MMU with an identity mapping.
    ///
    /// This is a performance measure, not a security boundary (the emulator's own memory
    /// map is that). With the MMU off the emulator considers every page executable and
    /// then sends every guest store down a slow path that checks for self-modifying code.
    /// Marking data pages execute-never in real page tables lets stores run at full speed.
    fn enable_mmu(&mut self) -> Result<(), String> {
        self.cp15(2, 0, 2, |_| 0)?; // TTBCR: TTBR0 only
        self.cp15(2, 0, 0, |_| PT_BASE as u64)?; // TTBR0
        self.cp15(3, 0, 0, |_| 1)?; // DACR: domain 0 is a client, so permissions apply
        self.cp15(1, 0, 0, |v| v | 1)?; // SCTLR.M
        Ok(())
    }

    /// Fault address registers, for diagnostics after a guest exception.
    fn fault_info(&mut self) -> String {
        let mut rd = |crn, crm, opc2| self.cp15(crn, crm, opc2, |v| v).unwrap_or(0);
        format!("data fault address {:#010x} (status {:#x}), fetch fault address {:#010x} (status {:#x})", rd(6, 0, 0), rd(5, 0, 0), rd(6, 0, 2), rd(5, 0, 1))
    }

    pub fn new_trap(&mut self, t: Trap) -> u32 {
        self.traps.push(t);
        let addr = TRAP_BASE + 4 * self.traps.len() as u32;
        assert!(addr < TRAP_BASE + TRAP_LEN, "trap page full");
        addr
    }

    // ---- registers and arguments -------------------------------------------------

    pub fn reg(&self, r: RegisterARM) -> R<u32> {
        cpu(self.uc.reg_read(r), "reg read").map(|v| v as u32)
    }
    pub fn set_reg(&mut self, r: RegisterARM, v: u32) -> R<()> {
        cpu(self.uc.reg_write(r, v as u64), "reg write")
    }
    /// Integer argument `i` of the call that trapped (AAPCS: r0-r3, then the stack).
    pub fn arg(&self, i: u32) -> R<u32> {
        match i {
            0 => self.reg(RegisterARM::R0),
            1 => self.reg(RegisterARM::R1),
            2 => self.reg(RegisterARM::R2),
            3 => self.reg(RegisterARM::R3),
            _ => self.mem.r32(self.reg(RegisterARM::SP)?.wrapping_add(4 * (i - 4))),
        }
    }
    pub fn ret(&mut self, v: u32) -> R<()> {
        self.set_reg(RegisterARM::R0, v)
    }
    pub fn ret64(&mut self, v: u64) -> R<()> {
        self.set_reg(RegisterARM::R0, v as u32)?;
        self.set_reg(RegisterARM::R1, (v >> 32) as u32)
    }
    pub fn set_errno(&mut self, e: u32) -> R<()> {
        self.mem.w32(self.errno_addr, e)
    }

    /// A zeroed guest buffer that lives for the whole run (for C APIs returning statics).
    pub fn static_buf(&mut self, key: &'static str, len: u32) -> R<u32> {
        if let Some(&a) = self.statics.get(key) {
            return Ok(a);
        }
        let a = self.galloc(len)?;
        self.statics.insert(key, a);
        Ok(a)
    }

    /// Allocate zeroed guest memory for the host's own use.
    pub fn galloc(&mut self, len: u32) -> R<u32> {
        let a = self.heap.alloc(len);
        if a == 0 {
            return Err(Fault::Other("guest heap exhausted".into()));
        }
        self.mem.slice_mut(a, len)?.fill(0);
        Ok(a)
    }

    // ---- diagnostics -------------------------------------------------------------

    pub fn symbolize(&self, addr: u32) -> String {
        if (TRAP_BASE..TRAP_BASE + TRAP_LEN).contains(&addr) {
            return match self.traps.get(((addr - TRAP_BASE) / 4).wrapping_sub(1) as usize) {
                Some(Trap::Import(i)) => format!("{}@host", self.imports[*i].name),
                Some(Trap::JniEnv(i)) => format!("JNIEnv[{i}]@host"),
                Some(Trap::JavaVm(i)) => format!("JavaVM[{i}]@host"),
                None => "host return".to_string(),
            };
        }
        let Some(off) = addr.checked_sub(LIB_BASE) else { return format!("{addr:#x} ({})", self.mem.describe(addr)) };
        let i = self.img.symbols.partition_point(|s| s.0 <= off);
        match i.checked_sub(1).map(|i| &self.img.symbols[i]) {
            Some((a, n)) if off - a < 0x10000 => format!("{n}+{:#x}", off - a),
            _ => format!("{addr:#x} ({})", self.mem.describe(addr)),
        }
    }

    pub fn state(&self) -> String {
        let g = |r| self.reg(r).unwrap_or(0);
        use RegisterARM::*;
        let (pc, lr, sp) = (g(PC), g(LR), g(SP));
        let mut s = format!(
            "  pc={pc:#010x} {}\n  lr={lr:#010x} {}\n  sp={sp:#010x} r0={:#x} r1={:#x} r2={:#x} r3={:#x}\n  r4={:#x} r5={:#x} r6={:#x} r7={:#x} r8={:#x} r9={:#x} r10={:#x} r11={:#x} r12={:#x}",
            self.symbolize(pc), self.symbolize(lr),
            g(R0), g(R1), g(R2), g(R3), g(R4), g(R5), g(R6), g(R7), g(R8), g(R9), g(R10), g(R11), g(R12)
        );
        // Best-effort backtrace: stack words that point into library code.
        let mut found = 0;
        for i in 0..512 {
            let Ok(w) = self.mem.r32(sp.wrapping_add(i * 4)) else { break };
            if w >= LIB_BASE && self.mem.describe(w) == "lib code" && w % 4 == 0 {
                s += &format!("\n    stack+{:#x}: {}", i * 4, self.symbolize(w));
                found += 1;
                if found == 12 {
                    break;
                }
            }
        }
        s
    }

    /// Guest instructions executed per function, most expensive first.
    pub fn profile_report(&self) -> Vec<(String, u64)> {
        let mut per_fn: HashMap<String, u64> = HashMap::new();
        for (&addr, &n) in self.profile.borrow().iter() {
            let name = self.symbolize(addr);
            *per_fn.entry(name.split('+').next().unwrap_or("?").to_string()).or_insert(0) += n;
        }
        let mut v: Vec<_> = per_fn.into_iter().collect();
        v.sort_by_key(|e| std::cmp::Reverse(e.1));
        v
    }

    pub fn warn_once(&mut self, msg: String) {
        if self.warned.len() < 1000 && self.warned.insert(msg.clone()) {
            eprintln!("[host] warning: {msg}");
        }
    }

    // ---- running guest code ------------------------------------------------------

    fn run(&mut self, entry: u32) -> R<()> {
        let mut pc = entry;
        loop {
            self.hit.set(0);
            let res = self.uc.emu_start(pc as u64, NEVER, 0, 0);
            let t = self.hit.get();
            if let Some(err) = self.aot.as_ref().and_then(|status| status.error.take()) {
                return Err(Fault::Cpu(format!("AOT register handoff: {err:?}\n{}", self.state())));
            }
            if let Some(n) = self.excp.take() {
                let what = match n {
                    1 => "undefined instruction",
                    2 => "supervisor call (the guest has no kernel)",
                    3 => "jump to an address that is not mapped or not executable",
                    4 => "access to an address that is not mapped",
                    _ => "CPU exception",
                };
                return Err(Fault::Cpu(format!("{what} (exception {n}); {}\n{}", self.fault_info(), self.state())));
            }
            if let Err(e) = res {
                return Err(Fault::Cpu(format!("{e:?}\n{}", self.state())));
            }
            if let Some(status) = &self.aot {
                status.skip.set(None);
                if let Some((index, address)) = status.pending.take() {
                    let (next, progressed) = cpu(status.run_segment(&mut self.uc, &mut self.mem, index, address), "AOT register handoff")?;
                    if progressed {
                        status.count_segment(index);
                    }
                    if next & !1 == address {
                        status.skip.set(Some(address));
                    }
                    pc = next;
                    continue;
                }
            }
            if t == RET_ADDR {
                return Ok(());
            }
            if t == 0 {
                return Err(Fault::Cpu(format!("emulator stopped unexpectedly\n{}", self.state())));
            }
            let lr = self.reg(RegisterARM::LR)? & !1;
            // A jump into the trap page that is not a bound slot is a wild jump.
            let slot = ((t - TRAP_BASE) / 4).checked_sub(1).filter(|_| t % 4 == 0);
            let Some(&trap) = slot.and_then(|i| self.traps.get(i as usize)) else {
                return Err(Fault::Cpu(format!("jump to an unused host-call address {t:#x}\n{}", self.state())));
            };
            match trap {
                Trap::Import(i) => {
                    self.imports[i].calls += 1;
                    if self.opts.trace {
                        eprintln!(
                            "[trace] {}{}({:#x}, {:#x}, {:#x}, {:#x}) from {}",
                            "  ".repeat(self.depth as usize),
                            self.imports[i].name,
                            self.arg(0)?, self.arg(1)?, self.arg(2)?, self.arg(3)?,
                            self.symbolize(lr)
                        );
                    }
                    match self.imports[i].handler {
                        Some(h) => h(self)?,
                        None => return Err(Fault::Unimplemented(format!("import `{}`\n{}", self.imports[i].name, self.state()))),
                    }
                }
                Trap::JniEnv(i) => crate::jni::env_call(self, i)?,
                Trap::JavaVm(i) => crate::jni::vm_call(self, i)?,
            }
            pc = lr;
        }
    }

    /// Call a guest function with integer/pointer arguments and return r0.
    /// Safe to call from inside a trap handler (the guest's stack pointer is restored).
    pub fn call(&mut self, addr: u32, args: &[u32]) -> R<u32> {
        if self.depth > 64 {
            return Err(Fault::Other("guest/host call nesting too deep".into()));
        }
        let sp0 = self.reg(RegisterARM::SP)?;
        let lr0 = self.reg(RegisterARM::LR)?;
        let extra = args.len().saturating_sub(4) as u32;
        let sp = sp0.wrapping_sub(4 * extra + 32) & !7;
        if !(STACK_BASE + 0x1000..STACK_BASE + STACK_LEN).contains(&sp) {
            return Err(Fault::Other(format!("guest stack pointer out of range ({sp0:#x})")));
        }
        for (i, &a) in args.iter().enumerate() {
            match i {
                0 => self.set_reg(RegisterARM::R0, a)?,
                1 => self.set_reg(RegisterARM::R1, a)?,
                2 => self.set_reg(RegisterARM::R2, a)?,
                3 => self.set_reg(RegisterARM::R3, a)?,
                _ => self.mem.w32(sp + 4 * (i as u32 - 4), a)?,
            }
        }
        self.set_reg(RegisterARM::SP, sp)?;
        self.set_reg(RegisterARM::LR, RET_ADDR)?;
        self.depth += 1;
        let r = self.run(addr);
        self.depth -= 1;
        r?;
        let r0 = self.reg(RegisterARM::R0)?;
        self.set_reg(RegisterARM::SP, sp0)?;
        self.set_reg(RegisterARM::LR, lr0)?;
        Ok(r0)
    }

    /// Guest address of the trap bound to an imported function.
    #[cfg(test)]
    pub fn import_addr(&self, name: &str) -> Option<u32> {
        let idx = self.imports.iter().position(|i| i.name == name)?;
        let slot = self.traps.iter().position(|t| matches!(t, Trap::Import(i) if *i == idx))?;
        Some(TRAP_BASE + 4 * (slot as u32 + 1))
    }

    /// Count how often the guest executes the instruction at `addr` (a function entry).
    pub fn watch(&mut self, addr: u32) -> R<Rc<Cell<u32>>> {
        let count = Rc::new(Cell::new(0u32));
        let c = count.clone();
        cpu(self.uc.add_code_hook(addr as u64, addr as u64, move |_, _, _| c.set(c.get().wrapping_add(1))), "hook")?;
        Ok(count)
    }

    pub fn export(&self, name: &str) -> R<u32> {
        self.img.exports.get(name).map(|v| LIB_BASE + v).ok_or_else(|| Fault::Other(format!("library does not export `{name}`")))
    }

    /// Call a `Java_...` native method as the Java layer would: (env, this/class, args...).
    /// Local references created during the call are released when it returns.
    pub fn call_native(&mut self, symbol: &str, this: u32, args: &[u32]) -> R<u32> {
        let addr = self.export(symbol)?;
        let mut full = vec![self.jni.env, this];
        full.extend_from_slice(args);
        self.jni.push_frame();
        let r = self.call(addr, &full);
        self.jni.pop_frame();
        r
    }
}
