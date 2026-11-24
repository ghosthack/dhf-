//! Opt-in native translations for the exact bundled ARM library.

use crate::aot_blocks;
use crate::machine::LIB_BASE;
use crate::mem::Mem;
use crate::KNOWN_LIB_SHA256;
use sha2::{Digest, Sha256};
use std::{cell::Cell, rc::Rc};
use unicorn_engine::{uc_error, RegisterARM, Unicorn};

const DIVIDE: u64 = (LIB_BASE + 0x000f_3394) as u64;
const N: u32 = 1 << 31;
const Z: u32 = 1 << 30;
const C: u32 = 1 << 29;
const V: u32 = 1 << 28;
const THUMB_OR_JAZELLE: u32 = (1 << 5) | (1 << 24);

#[derive(Default)]
pub struct Status {
    pub calls: Cell<u64>,
    pub error: Cell<Option<uc_error>>,
    pub pending: Cell<Option<(usize, u32)>>,
    pub skip: Cell<Option<u32>>,
    blocks: Vec<Cell<u64>>,
    #[cfg(test)]
    pub verification: crate::aot_verify::Verification,
}

impl Status {
    pub fn run_segment(
        &self,
        uc: &mut Unicorn<'_, ()>,
        mem: &mut Mem,
        index: usize,
        entry: u32,
    ) -> Result<(u32, bool), uc_error> {
        #[cfg(test)]
        if self.verification.should_check(entry) {
            return self.verification.check(uc, mem, index, entry);
        }
        aot_blocks::run(uc, mem, index, entry).map(|result| (result.next, result.progressed))
    }

    pub fn count_segment(&self, index: usize) {
        self.blocks[index].set(self.blocks[index].get() + 1);
    }

    pub fn report(&self) {
        eprintln!("[host] AOT __aeabi_idiv calls: {}", self.calls.get());
        for (translation, count) in aot_blocks::translations().iter().zip(&self.blocks) {
            eprintln!("[host] AOT {} segments: {}", translation.name, count.get());
        }
    }
}

pub fn install(uc: &mut Unicorn<'_, ()>, library: &[u8]) -> Result<Option<Rc<Status>>, uc_error> {
    let digest = format!("{:x}", Sha256::digest(library));
    if digest != KNOWN_LIB_SHA256 {
        return Ok(None);
    }
    let translations = aot_blocks::translations();
    let status = Rc::new(Status {
        blocks: vec![Cell::new(0); translations.len()],
        ..Status::default()
    });
    let hook_status = status.clone();
    uc.add_code_hook(DIVIDE, DIVIDE, move |uc, _, _| {
        #[cfg(test)]
        if hook_status.verification.checking.get() {
            return;
        }
        match try_divide(uc) {
            Ok(true) => hook_status.calls.set(hook_status.calls.get() + 1),
            Ok(false) => (),
            Err(err) => {
                hook_status.error.set(Some(err));
                let _ = uc.emu_stop();
            }
        }
    })?;
    for (index, translation) in translations.iter().enumerate() {
        for &offset in translation.hooks {
            let address = LIB_BASE + offset;
            let status = status.clone();
            uc.add_code_hook(address.into(), address.into(), move |uc, _, _| {
                #[cfg(test)]
                if status.verification.checking.get() {
                    return;
                }
                if status.skip.get() == Some(address) {
                    status.skip.set(None);
                    return;
                }
                status.pending.set(Some((index, address)));
                let _ = uc.emu_stop();
            })?;
        }
    }
    Ok(Some(status))
}

fn try_divide(uc: &mut Unicorn<'_, ()>) -> Result<bool, uc_error> {
    use RegisterARM::*;
    let cpsr = uc.reg_read(CPSR)? as u32;
    let denominator = uc.reg_read(R1)? as u32;
    let lr = uc.reg_read(LR)?;
    // The zero-divisor path calls __aeabi_idiv0 and touches the guest stack.
    // Let Unicorn also handle entries or returns that aren't word-aligned ARM.
    if cpsr & THUMB_OR_JAZELLE != 0 || denominator == 0 || lr & 3 != 0 {
        return Ok(false);
    }

    let numerator = uc.reg_read(R0)? as u32;
    let result = divide(numerator, denominator);
    uc.reg_write(R0, result.quotient.into())?;
    uc.reg_write(R1, result.divisor.into())?;
    uc.reg_write(R2, result.bit.into())?;
    if let Some(remainder) = result.remainder {
        uc.reg_write(R3, remainder.into())?;
    }
    uc.reg_write(R12, (numerator ^ denominator).into())?;
    uc.reg_write(APSR_NZCV, result.flags.into())?;
    // A Unicorn PC write exits the current translation block and continues at
    // the new address. Unicorn still checks the fetch and handles host traps.
    uc.reg_write(PC, lr)?;
    Ok(true)
}

struct Division {
    quotient: u32,
    divisor: u32,
    bit: u32,
    remainder: Option<u32>,
    flags: u32,
}

fn nz(value: u32) -> u32 {
    (value & N) | if value == 0 { Z } else { 0 }
}

fn cmp(left: u32, right: u32) -> u32 {
    let difference = left.wrapping_sub(right);
    let overflow = ((left ^ right) & (left ^ difference) & N) != 0;
    nz(difference) | if left >= right { C } else { 0 } | if overflow { V } else { 0 }
}

fn signed(quotient: u32, signs: u32) -> u32 {
    if signs & N != 0 {
        quotient.wrapping_neg()
    } else {
        quotient
    }
}

/// Ahead-of-time Rust translation of the nonzero-divisor path in __aeabi_idiv.
/// Preserve the returned scratch registers and condition flags as well as r0.
fn divide(numerator: u32, denominator: u32) -> Division {
    let signs = numerator ^ denominator;
    let mut divisor = (denominator as i32).unsigned_abs();
    let mut bit = divisor - 1;
    if bit == 0 {
        return Division {
            quotient: if denominator & N != 0 {
                numerator.wrapping_neg()
            } else {
                numerator
            },
            divisor,
            bit,
            remainder: None,
            // TEQ r12, r0 reduces to testing the original denominator and
            // preserves the carry from SUBS r2, r1, #1.
            flags: nz(denominator) | C,
        };
    }

    let mut remainder = (numerator as i32).unsigned_abs();
    if remainder <= divisor {
        return Division {
            quotient: if remainder < divisor {
                0
            } else {
                signed(1, signs)
            },
            divisor,
            bit,
            remainder: Some(remainder),
            flags: cmp(remainder, divisor),
        };
    }

    let mut quotient = 0;
    if divisor.is_power_of_two() {
        bit = divisor.trailing_zeros();
        quotient = remainder >> bit;
    } else {
        let shift = divisor.leading_zeros() - remainder.leading_zeros();
        divisor <<= shift;
        bit = 1 << shift;
        // The ARM routine processes four quotient bits per iteration.
        loop {
            for shift in 0..4 {
                let subtrahend = divisor >> shift;
                if remainder >= subtrahend {
                    remainder -= subtrahend;
                    quotient |= bit >> shift;
                }
            }
            if remainder == 0 {
                break;
            }
            bit >>= 4;
            // LSRS sets Z before the conditional shift of r1.
            if bit == 0 {
                break;
            }
            divisor >>= 4;
        }
    }
    Division {
        quotient: signed(quotient, signs),
        divisor,
        bit,
        remainder: Some(remainder),
        flags: nz(signs) | C,
    }
}
