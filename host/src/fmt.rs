//! `printf`-family formatting over guest varargs.
//!
//! `%n` is deliberately not supported (it is a write primitive); it consumes its argument
//! and does nothing. Widths and precisions are clamped so a hostile format string cannot
//! make the host allocate without bound.

use crate::machine::Machine;
use crate::mem::R;

const CLAMP: usize = 4096;

/// Where the variable arguments come from.
pub enum Va {
    /// `f(fmt, ...)`: arguments continue from call-argument index `idx` (r0-r3, then stack).
    Call { idx: u32 },
    /// `vf(fmt, va_list)`: arguments are read from guest memory at `addr`.
    List { addr: u32 },
}

impl Va {
    pub fn u32(&mut self, m: &Machine) -> R<u32> {
        match self {
            Va::Call { idx } => {
                let v = m.arg(*idx)?;
                *idx += 1;
                Ok(v)
            }
            Va::List { addr } => {
                let v = m.mem.r32(*addr)?;
                *addr = addr.wrapping_add(4);
                Ok(v)
            }
        }
    }
    /// 64-bit arguments are 8-byte aligned under the ARM EABI.
    pub fn u64(&mut self, m: &Machine) -> R<u64> {
        match self {
            Va::Call { idx } => *idx = (*idx + 1) & !1,
            Va::List { addr } => *addr = addr.wrapping_add(7) & !7,
        }
        let lo = self.u32(m)? as u64;
        let hi = self.u32(m)? as u64;
        Ok(hi << 32 | lo)
    }
}

fn pad(out: &mut Vec<u8>, body: &[u8], width: usize, left: bool, zero: bool) {
    let fill = width.saturating_sub(body.len());
    if left {
        out.extend_from_slice(body);
        out.extend(std::iter::repeat(b' ').take(fill));
    } else if zero {
        // Zero padding goes after the sign or 0x prefix.
        let split = if body.starts_with(b"0x") || body.starts_with(b"0X") {
            2
        } else if matches!(body.first(), Some(b'-' | b'+' | b' ')) {
            1
        } else {
            0
        };
        out.extend_from_slice(&body[..split]);
        out.extend(std::iter::repeat(b'0').take(fill));
        out.extend_from_slice(&body[split..]);
    } else {
        out.extend(std::iter::repeat(b' ').take(fill));
        out.extend_from_slice(body);
    }
}

pub fn format(m: &Machine, fmt_addr: u32, va: &mut Va) -> R<Vec<u8>> {
    let f = m.mem.cstr(fmt_addr)?.to_vec();
    let mut out = Vec::new();
    let mut i = 0;
    while i < f.len() {
        if f[i] != b'%' {
            out.push(f[i]);
            i += 1;
            continue;
        }
        i += 1;
        let (mut left, mut zero, mut plus, mut space, mut alt) = (false, false, false, false, false);
        while let Some(&c) = f.get(i) {
            match c {
                b'-' => left = true,
                b'0' => zero = true,
                b'+' => plus = true,
                b' ' => space = true,
                b'#' => alt = true,
                _ => break,
            }
            i += 1;
        }
        let num = |i: &mut usize, va: &mut Va| -> R<Option<i64>> {
            if f.get(*i) == Some(&b'*') {
                *i += 1;
                return Ok(Some(va.u32(m)? as i32 as i64));
            }
            let mut v: Option<i64> = None;
            while let Some(d) = f.get(*i).filter(|c| c.is_ascii_digit()) {
                v = Some((v.unwrap_or(0) * 10 + (d - b'0') as i64).min(1 << 30));
                *i += 1;
            }
            Ok(v)
        };
        let mut width = num(&mut i, va)?.unwrap_or(0);
        if width < 0 {
            left = true;
            width = -width;
        }
        let width = (width as usize).min(CLAMP);
        let mut prec = None;
        if f.get(i) == Some(&b'.') {
            i += 1;
            prec = Some(num(&mut i, va)?.unwrap_or(0).clamp(0, CLAMP as i64) as usize);
        }
        let mut long_long = false;
        let mut short = 0;
        while let Some(&c) = f.get(i) {
            match c {
                b'l' => long_long = f.get(i + 1) == Some(&b'l') || long_long,
                b'h' => short += 1,
                b'L' | b'q' | b'j' => long_long = true,
                b'z' | b't' => {}
                _ => break,
            }
            i += 1;
        }
        let Some(&conv) = f.get(i) else { break };
        i += 1;
        let mut body: Vec<u8> = Vec::new();
        match conv {
            b'%' => body.push(b'%'),
            b'd' | b'i' => {
                let v: i64 = if long_long { va.u64(m)? as i64 } else { va.u32(m)? as i32 as i64 };
                let v = match short {
                    1 => v as i16 as i64,
                    2.. => v as i8 as i64,
                    _ => v,
                };
                let digits = v.unsigned_abs().to_string();
                if v < 0 {
                    body.push(b'-');
                } else if plus {
                    body.push(b'+');
                } else if space {
                    body.push(b' ');
                }
                if let Some(p) = prec {
                    body.extend(std::iter::repeat(b'0').take(p.saturating_sub(digits.len())));
                    zero = false;
                }
                body.extend_from_slice(digits.as_bytes());
            }
            b'u' | b'x' | b'X' | b'o' | b'p' => {
                let v: u64 = if long_long { va.u64(m)? } else { va.u32(m)? as u64 };
                let v = match short {
                    1 => v as u16 as u64,
                    2.. => v as u8 as u64,
                    _ => v,
                };
                let digits = match conv {
                    b'u' => v.to_string(),
                    b'o' => format!("{v:o}"),
                    b'X' => format!("{v:X}"),
                    _ => format!("{v:x}"),
                };
                if conv == b'p' || (alt && v != 0 && conv != b'u' && conv != b'o') {
                    body.extend_from_slice(if conv == b'X' { b"0X" } else { b"0x" });
                }
                if let Some(p) = prec {
                    body.extend(std::iter::repeat(b'0').take(p.saturating_sub(digits.len())));
                    zero = false;
                }
                body.extend_from_slice(digits.as_bytes());
            }
            b'c' => body.push(va.u32(m)? as u8),
            b's' => {
                let p = va.u32(m)?;
                if p == 0 {
                    body.extend_from_slice(b"(null)");
                } else {
                    let s = m.mem.cstr(p)?;
                    body.extend_from_slice(&s[..prec.map_or(s.len(), |p| p.min(s.len()))]);
                }
                zero = false;
            }
            b'f' | b'F' | b'e' | b'E' | b'g' | b'G' => {
                let v = f64::from_bits(va.u64(m)?);
                let p = prec.unwrap_or(6);
                let mut s = match conv {
                    b'f' | b'F' => format!("{v:.p$}"),
                    b'e' | b'E' => {
                        let t = format!("{v:.p$e}");
                        // Rust prints `1.5e2`; C prints `1.5e+02`.
                        match t.split_once('e') {
                            Some((mant, exp)) => {
                                let (sign, digits) = exp.strip_prefix('-').map_or(('+', exp), |d| ('-', d));
                                format!("{mant}e{sign}{digits:0>2}")
                            }
                            None => t,
                        }
                    }
                    _ => {
                        let t = format!("{v:.*}", p.max(1));
                        if t.contains('.') { t.trim_end_matches('0').trim_end_matches('.').to_string() } else { t }
                    }
                };
                if conv.is_ascii_uppercase() {
                    s = s.to_uppercase();
                }
                if v.is_sign_positive() && plus {
                    s.insert(0, '+');
                } else if v.is_sign_positive() && space {
                    s.insert(0, ' ');
                }
                body = s.into_bytes();
            }
            b'n' => {
                va.u32(m)?;
            }
            other => {
                body.push(b'%');
                body.push(other);
            }
        }
        pad(&mut out, &body, width, left, zero);
    }
    Ok(out)
}
