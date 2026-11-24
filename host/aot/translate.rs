//! Emit Rust basic blocks from the little-endian A32 instructions in the selected ranges.
//! An unrecognised instruction exits to Unicorn at its original guest PC.

use std::{collections::BTreeSet, fmt::Write};

struct Instruction {
    body: String,
    ends_block: bool,
    target: Option<u32>,
    call: bool,
    indirect: bool,
}

impl Instruction {
    fn next(body: String) -> Self {
        Self {
            body,
            ends_block: false,
            target: None,
            call: false,
            indirect: false,
        }
    }

    fn exit(body: String) -> Self {
        Self {
            ends_block: true,
            ..Self::next(body)
        }
    }
}

fn bit(word: u32, n: u32) -> bool {
    word & (1 << n) != 0
}

fn field(word: u32, n: u32) -> u32 {
    (word >> n) & 15
}

fn reg(n: u32) -> String {
    format!("s.reg({n}, pc)")
}

fn decode(word: u32, pc: u32) -> Option<Instruction> {
    let rn = field(word, 16);
    let rd = field(word, 12);
    let rm = field(word, 0);
    let flags = bit(word, 20);

    if word & 0x0fff_fff0 == 0x012f_ff10 {
        return Some(Instruction::exit(format!("return {};", reg(rm))));
    }
    if word & 0x0e00_0000 == 0x0a00_0000 {
        let displacement = (((word & 0x00ff_ffff) << 8) as i32 >> 6) as u32;
        let target = pc.wrapping_add(8).wrapping_add(displacement);
        let link = if bit(word, 24) {
            "s.r[14] = pc + 4;"
        } else {
            ""
        };
        return Some(Instruction {
            target: Some(target),
            call: bit(word, 24),
            ..Instruction::exit(format!("{link} return LIB_BASE.wrapping_add({target:#x});"))
        });
    }

    if word & 0x0fb0_0000 == 0x0300_0000 && rd != 15 {
        let imm = ((word >> 4) & 0xf000) | (word & 0xfff);
        let value = if bit(word, 22) {
            format!("(s.r[{rd}] & 0xffff) | {:#x}", imm << 16)
        } else {
            format!("{imm:#x}")
        };
        return Some(Instruction::next(format!("s.r[{rd}] = {value};")));
    }
    if word & 0x0fe0_0070 == 0x07e0_0050 && rd != 15 {
        let shift = (word >> 7) & 31;
        let width = ((word >> 16) & 31) + 1;
        if shift + width > 32 {
            return None;
        }
        let mask = u32::MAX >> (32 - width);
        return Some(Instruction::next(format!(
            "s.r[{rd}] = ({} >> {shift}) & {mask:#x};",
            reg(rm)
        )));
    }
    if word & 0x0f80_03f0 == 0x0680_0070 && rd != 15 {
        let ty = match (word >> 20) & 7 {
            2 => "i8",
            3 => "i16",
            6 => "u8",
            7 => "u16",
            _ => return None,
        };
        let rotate = ((word >> 10) & 3) * 8;
        let value = format!("({}.rotate_right({rotate}) as {ty} as u32)", reg(rm));
        let value = if rn == 15 {
            value
        } else {
            format!("{}.wrapping_add({value})", reg(rn))
        };
        return Some(Instruction::next(format!("s.r[{rd}] = {value};")));
    }
    if word & 0x0fc0_00f0 == 0x0000_0090 && rn != 15 && !flags {
        let rs = field(word, 8);
        let value = format!("{}.wrapping_mul({})", reg(rm), reg(rs));
        let value = if bit(word, 21) {
            format!("{value}.wrapping_add({})", reg(rd))
        } else {
            value
        };
        return Some(Instruction::next(format!("s.r[{rn}] = {value};")));
    }

    if word & 0x0e00_0000 == 0x0800_0000 {
        let mask = word & 0xffff;
        if mask == 0
            || rn == 15
            || bit(word, 22)
            || (!flags && mask & 0x8000 != 0)
            || (bit(word, 21) && mask & (1 << rn) != 0)
        {
            return None;
        }
        let body = format!(
            "let Some(target) = s.transfer_many(mem, {rn}, {mask}, {flags}, {}, {}, {}) else {{ return s.retry(pc); }};",
            bit(word, 24), bit(word, 23), bit(word, 21),
        );
        return Some(if mask & 0x8000 != 0 {
            Instruction::exit(format!("{body} return target;"))
        } else {
            Instruction::next(body.replace("Some(target)", "Some(_)"))
        });
    }

    let extra_transfer = word & 0x0e00_0090 == 0x0000_0090 && word & 0x60 != 0;
    let regular_transfer = word & 0x0c00_0000 == 0x0400_0000;
    if extra_transfer || regular_transfer {
        let offset = if extra_transfer {
            if bit(word, 22) {
                format!("{}", ((word >> 4) & 0xf0) | (word & 15))
            } else {
                reg(rm)
            }
        } else if bit(word, 25) {
            if bit(word, 4) {
                return None;
            }
            format!("{}.0", shift(word, false))
        } else {
            format!("{}", word & 0xfff)
        };
        let size = if extra_transfer {
            match (word >> 5) & 3 {
                1 => 2,
                2 if flags => 1,
                3 if flags => 2,
                _ => return None,
            }
        } else if bit(word, 22) {
            1
        } else {
            4
        };
        let signed = extra_transfer && bit(word, 6);
        let pre = bit(word, 24);
        let writeback = !pre || bit(word, 21);
        if rd == 15 || (writeback && (rn == rd || rn == 15)) || (!pre && bit(word, 21)) {
            return None;
        }
        let direction = if bit(word, 23) { "add" } else { "sub" };
        let base = reg(rn);
        let address = if pre { "updated" } else { "base" };
        let effect = if flags {
            format!("let Some(value) = read(mem, {address}, {size}, {signed}) else {{ return s.retry(pc); }}; s.r[{rd}] = value;")
        } else {
            format!("if !write(mem, {address}, {size}, s.r[{rd}]) {{ return s.retry(pc); }}")
        };
        let writeback = if writeback {
            format!("s.r[{rn}] = updated;")
        } else {
            String::new()
        };
        return Some(Instruction::next(format!(
            "let base = {base}; let updated = base.wrapping_{direction}({offset}); {effect} {writeback}"
        )));
    }

    if word & 0x0c00_0000 != 0 || (!bit(word, 25) && word & 0x90 == 0x90) {
        return None;
    }
    let op = (word >> 21) & 15;
    let test = (8..=11).contains(&op);
    if test && (!flags || rd != 0) || flags && rd == 15 {
        return None;
    }
    let operand = if bit(word, 25) {
        let rotate = ((word >> 8) & 15) * 2;
        let imm = (word & 255).rotate_right(rotate);
        let carry = if rotate == 0 {
            "s.carry()".to_string()
        } else {
            format!("{}", bit(imm, 31))
        };
        format!("({imm:#x}u32, {carry})")
    } else {
        shift(word, bit(word, 4))
    };
    let action = match op {
        0 | 8 => "lhs & rhs",
        1 | 9 => "lhs ^ rhs",
        2 | 10 => "s.sub(lhs, rhs, SET)",
        3 => "s.sub(rhs, lhs, SET)",
        4 | 11 => "s.add(lhs, rhs, SET)",
        12 => "lhs | rhs",
        13 => "rhs",
        14 => "lhs & !rhs",
        15 => "!rhs",
        _ => return None,
    }
    .replace("SET", if flags { "true" } else { "false" });
    let flag_write = if flags && !matches!(op, 2..=4 | 10 | 11) {
        "s.logic(value, carry);"
    } else {
        ""
    };
    let mut body = format!(
        "let lhs = {}; let (rhs, carry) = {operand}; let value = {action}; {flag_write}",
        reg(rn)
    );
    if test {
        return Some(Instruction::next(body));
    }
    if rd == 15 {
        body.push_str("return value & !3;");
        return Some(Instruction {
            indirect: true,
            ..Instruction::exit(body)
        });
    }
    write!(body, "s.r[{rd}] = value;").unwrap();
    Some(Instruction::next(body))
}

fn shift(word: u32, by_register: bool) -> String {
    let kind = (word >> 5) & 3;
    let amount = if by_register {
        format!("{} & 255", reg(field(word, 8)))
    } else {
        format!("{}", (word >> 7) & 31)
    };
    let rm = field(word, 0);
    format!("s.shift({}, {kind}, {amount}, {by_register})", reg(rm))
}

pub fn generate(library: &[u8]) -> String {
    let mut source = String::new();
    source.push_str("const TRANSLATIONS: &[Translation] = &[\n");
    for (index, &(name, start, end)) in crate::spec::FUNCTIONS.iter().enumerate() {
        writeln!(source, "Translation {{ name: {name:?}, start: {start:#x}, end: {:#x}, block: run_{index}, hooks: HOOKS_{index} }},", start + end).unwrap();
    }
    source.push_str("];\n");
    for (index, &(_, start, size)) in crate::spec::FUNCTIONS.iter().enumerate() {
        let end = start + size;
        let word_at =
            |pc: u32| u32::from_le_bytes(library[pc as usize..pc as usize + 4].try_into().unwrap());
        let decode_at = |pc| {
            let word = word_at(pc);
            if (word >> 28) == 15 {
                None
            } else {
                decode(word, pc)
            }
        };
        let mut blocks = BTreeSet::from([start]);
        let mut hooks = BTreeSet::from([start]);
        for pc in (start..end).step_by(4) {
            let Some(insn) = decode_at(pc) else {
                if pc + 4 < end {
                    blocks.insert(pc + 4);
                }
                continue;
            };
            if let Some(target) = insn.target.filter(|target| (start..end).contains(target)) {
                blocks.insert(target);
            }
            if insn.ends_block && pc + 4 < end {
                blocks.insert(pc + 4);
            }
            if insn.call && pc + 4 < end {
                hooks.insert(pc + 4);
            }
            if insn.indirect && pc + 8 < end {
                // A32 jump-table entries in these routines are unconditional branches.
                blocks.extend(
                    (pc + 8..end)
                        .step_by(4)
                        .take_while(|&entry| word_at(entry) >> 24 == 0xea),
                );
            }
        }
        writeln!(
            source,
            "const HOOKS_{index}: &[u32] = &{:?};",
            hooks.into_iter().collect::<Vec<_>>()
        )
        .unwrap();
        writeln!(source, "#[allow(unused_variables, unused_mut, unused_assignments, unused_parens, unreachable_code)]\nfn run_{index}(s: &mut State, mem: &mut Mem, pc: u32) -> u32 {{ match pc - LIB_BASE {{").unwrap();
        let starts: Vec<u32> = blocks.into_iter().chain([end]).collect();
        for pair in starts.windows(2) {
            let block = pair[0];
            writeln!(source, "{block:#x} => {{ let mut pc = pc;").unwrap();
            for address in (block..pair[1]).step_by(4) {
                if address != block {
                    writeln!(source, "pc = LIB_BASE + {address:#x};").unwrap();
                }
                let Some(insn) = decode_at(address) else {
                    source.push_str("return pc;\n");
                    break;
                };
                source.push_str("#[cfg(test)] { s.steps += 1; }\n");
                let cond = word_at(address) >> 28;
                if cond == 14 {
                    writeln!(source, "{{ {} }}", insn.body).unwrap();
                } else {
                    writeln!(source, "if s.passes({cond}) {{ {} }}", insn.body).unwrap();
                }
            }
            writeln!(source, "LIB_BASE + {:#x} }},", pair[1]).unwrap();
        }
        source.push_str("_ => pc, } }\n");
    }
    source
}
