//! Execute generated A32 basic blocks against the same checked memory Unicorn maps.

use crate::machine::LIB_BASE;
use crate::mem::Mem;
use unicorn_engine::{uc_error, RegisterARM, Unicorn};

const N: u32 = 1 << 31;
const Z: u32 = 1 << 30;
const C: u32 = 1 << 29;
const V: u32 = 1 << 28;
const NZCV: u32 = N | Z | C | V;
const THUMB: u32 = 1 << 5;
const UNSUPPORTED_STATE: u32 = THUMB | (1 << 24) | (1 << 9);

pub struct Translation {
    pub name: &'static str,
    start: u32,
    end: u32,
    block: fn(&mut State, &mut Mem, u32) -> u32,
    pub hooks: &'static [u32],
}

pub fn translations() -> &'static [Translation] {
    TRANSLATIONS
}

pub struct Execution {
    pub next: u32,
    pub progressed: bool,
    #[cfg(test)]
    pub steps: usize,
}

pub fn run(
    uc: &mut Unicorn<'_, ()>,
    mem: &mut Mem,
    index: usize,
    entry: u32,
) -> Result<Execution, uc_error> {
    use RegisterARM::*;
    let cpsr = uc.reg_read(CPSR)? as u32;
    if cpsr & UNSUPPORTED_STATE != 0 {
        return Ok(Execution {
            next: entry | u32::from(cpsr & THUMB != 0),
            progressed: false,
            #[cfg(test)]
            steps: 0,
        });
    }
    let mut registers = [
        R0, R1, R2, R3, R4, R5, R6, R7, R8, R9, R10, R11, R12, SP, LR,
    ];
    let snapshot = uc.reg_read_batch(&mut registers, 15)?;
    let mut state = State {
        r: std::array::from_fn(|i| snapshot[i] as u32),
        flags: cpsr & NZCV,
        #[cfg(test)]
        steps: 0,
    };
    let t = &TRANSLATIONS[index];
    let range = (LIB_BASE + t.start)..(LIB_BASE + t.end);
    let mut pc = entry;
    let mut progressed = false;
    while range.contains(&pc) && pc & 3 == 0 {
        let next = (t.block)(&mut state, mem, pc);
        if next == pc {
            break;
        }
        pc = next;
        progressed = true;
    }
    // A block may execute instructions before branching back to its own start.
    // Write the snapshot back even when dispatch will resume at the same PC.
    let values = state.r.map(u64::from);
    uc.reg_write_batch(&registers, &values, 15)?;
    uc.reg_write(APSR_NZCV, state.flags.into())?;
    Ok(Execution {
        next: pc,
        progressed,
        #[cfg(test)]
        steps: state.steps,
    })
}

struct State {
    r: [u32; 15],
    flags: u32,
    #[cfg(test)]
    steps: usize,
}

impl State {
    fn retry(&mut self, pc: u32) -> u32 {
        #[cfg(test)]
        {
            self.steps -= 1;
        }
        pc
    }

    fn reg(&self, reg: usize, pc: u32) -> u32 {
        if reg == 15 {
            pc + 8
        } else {
            self.r[reg]
        }
    }

    fn carry(&self) -> bool {
        self.flags & C != 0
    }

    fn passes(&self, condition: u32) -> bool {
        let n = self.flags & N != 0;
        let z = self.flags & Z != 0;
        let c = self.flags & C != 0;
        let v = self.flags & V != 0;
        let positive = match condition >> 1 {
            0 => z,
            1 => c,
            2 => n,
            3 => v,
            4 => c && !z,
            5 => n == v,
            6 => n == v && !z,
            _ => true,
        };
        positive ^ (condition & 1 != 0)
    }

    fn logic(&mut self, value: u32, carry: bool) {
        self.flags = (self.flags & V) | nz(value) | flag(carry, C);
    }

    fn add(&mut self, lhs: u32, rhs: u32, set: bool) -> u32 {
        let (value, carry) = lhs.overflowing_add(rhs);
        if set {
            let overflow = ((lhs ^ value) & (rhs ^ value) & N) != 0;
            self.flags = nz(value) | flag(carry, C) | flag(overflow, V);
        }
        value
    }

    fn sub(&mut self, lhs: u32, rhs: u32, set: bool) -> u32 {
        let (value, borrow) = lhs.overflowing_sub(rhs);
        if set {
            let overflow = ((lhs ^ rhs) & (lhs ^ value) & N) != 0;
            self.flags = nz(value) | flag(!borrow, C) | flag(overflow, V);
        }
        value
    }

    fn shift(&self, value: u32, kind: u32, mut amount: u32, by_register: bool) -> (u32, bool) {
        if amount == 0 {
            match (kind, by_register) {
                (0, _) | (_, true) => return (value, self.carry()),
                (3, false) => {
                    return (
                        (u32::from(self.carry()) << 31) | (value >> 1),
                        value & 1 != 0,
                    )
                }
                _ => amount = 32,
            }
        }
        match kind {
            0 => (
                value.checked_shl(amount).unwrap_or(0),
                amount <= 32 && (value >> (32 - amount)) & 1 != 0,
            ),
            1 => (
                value.checked_shr(amount).unwrap_or(0),
                amount <= 32 && (value >> (amount - 1)) & 1 != 0,
            ),
            2 => {
                let shifted = (value as i32) >> amount.min(31);
                (shifted as u32, (value >> (amount - 1).min(31)) & 1 != 0)
            }
            _ => {
                let shifted = value.rotate_right(amount);
                (shifted, shifted & N != 0)
            }
        }
    }

    fn transfer_many(
        &mut self,
        mem: &mut Mem,
        base: usize,
        mask: u32,
        load: bool,
        pre: bool,
        up: bool,
        writeback: bool,
    ) -> Option<u32> {
        let len = mask.count_ones() * 4;
        let base_addr = self.r[base];
        if base_addr & 3 != 0 {
            return None;
        }
        let updated = if up {
            base_addr.wrapping_add(len)
        } else {
            base_addr.wrapping_sub(len)
        };
        let start = if up { base_addr } else { updated };
        let addr = start.wrapping_add(4 * u32::from(pre == up));
        let regs = (0..16).filter(|r| mask & (1 << r) != 0);
        let mut target = 0;
        if load {
            let bytes = mem.slice(addr, len).ok()?;
            for (reg, word) in regs.zip(bytes.chunks_exact(4)) {
                let value = u32::from_le_bytes(word.try_into().unwrap());
                if reg == 15 {
                    target = value;
                } else {
                    self.r[reg] = value;
                }
            }
        } else {
            let bytes = mem.slice_mut(addr, len).ok()?;
            for (reg, word) in regs.zip(bytes.chunks_exact_mut(4)) {
                word.copy_from_slice(&self.r[reg].to_le_bytes());
            }
        }
        if writeback {
            self.r[base] = updated;
        }
        Some(target)
    }
}

fn flag(set: bool, mask: u32) -> u32 {
    if set {
        mask
    } else {
        0
    }
}

fn nz(value: u32) -> u32 {
    (value & N) | flag(value == 0, Z)
}

fn read(mem: &Mem, addr: u32, size: u32, signed: bool) -> Option<u32> {
    if addr & (size - 1) != 0 {
        return None;
    }
    let bytes = mem.slice(addr, size).ok()?;
    Some(match (size, signed) {
        (1, true) => (bytes[0] as i8) as u32,
        (1, false) => bytes[0].into(),
        (2, true) => i16::from_le_bytes(bytes.try_into().unwrap()) as u32,
        (2, false) => u16::from_le_bytes(bytes.try_into().unwrap()).into(),
        (4, false) => u32::from_le_bytes(bytes.try_into().unwrap()),
        _ => unreachable!("unsupported AOT load width"),
    })
}

fn write(mem: &mut Mem, addr: u32, size: u32, value: u32) -> bool {
    addr & (size - 1) == 0
        && mem
            .write(addr, &value.to_le_bytes()[..size as usize])
            .is_ok()
}

include!(concat!(env!("OUT_DIR"), "/aot_blocks.rs"));
