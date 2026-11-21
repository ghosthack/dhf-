//! Guest heap allocator.
//!
//! All bookkeeping lives on the host side, so a guest buffer overflow can corrupt the
//! guest's own data but never the allocator: there are no in-band headers to smash, and
//! double frees and frees of unknown pointers are detected instead of acted on.

use std::collections::{BTreeMap, HashMap};

const ALIGN: u32 = 16;
const SMALL_MAX: u32 = 2048;
const BINS: usize = (SMALL_MAX / ALIGN) as usize;

pub struct Heap {
    end: u32,
    brk: u32,
    /// Live blocks: address -> capacity.
    live: HashMap<u32, u32>,
    bins: Vec<Vec<u32>>,
    /// Free large blocks: address -> size, coalesced with neighbours.
    large_free: BTreeMap<u32, u32>,
    pub in_use: u64,
    pub peak: u64,
}

impl Heap {
    pub fn new(base: u32, len: u32) -> Self {
        Heap {
            end: base + len,
            brk: base,
            live: HashMap::new(),
            bins: vec![Vec::new(); BINS + 1],
            large_free: BTreeMap::new(),
            in_use: 0,
            peak: 0,
        }
    }

    fn bump(&mut self, cap: u32) -> Option<u32> {
        if self.end - self.brk < cap {
            return None;
        }
        let a = self.brk;
        self.brk += cap;
        Some(a)
    }

    /// Returns 0 when the guest heap is exhausted (the guest sees a failed `malloc`).
    pub fn alloc(&mut self, size: u32) -> u32 {
        let size = size.max(1);
        if size > self.end {
            return 0;
        }
        let addr = if size <= SMALL_MAX {
            let cap = (size + ALIGN - 1) & !(ALIGN - 1);
            let bin = (cap / ALIGN) as usize;
            match self.bins[bin].pop() {
                Some(a) => Some((a, cap)),
                None => self.bump(cap).map(|a| (a, cap)),
            }
        } else {
            let cap = match size.checked_add(0xfff) {
                Some(v) => v & !0xfff,
                None => return 0,
            };
            let hit = self.large_free.iter().find(|(_, &s)| s >= cap).map(|(&a, &s)| (a, s));
            match hit {
                Some((a, s)) => {
                    self.large_free.remove(&a);
                    if s > cap {
                        self.large_free.insert(a + cap, s - cap);
                    }
                    Some((a, cap))
                }
                None => {
                    // Keep large blocks page aligned.
                    let pad = (0x1000 - (self.brk & 0xfff)) & 0xfff;
                    if self.end - self.brk < pad {
                        None
                    } else {
                        self.brk += pad;
                        self.bump(cap).map(|a| (a, cap))
                    }
                }
            }
        };
        match addr {
            Some((a, cap)) => {
                self.live.insert(a, cap);
                self.in_use += cap as u64;
                self.peak = self.peak.max(self.in_use);
                a
            }
            None => 0,
        }
    }

    /// Capacity of a live block, if `addr` is one.
    pub fn capacity(&self, addr: u32) -> Option<u32> {
        self.live.get(&addr).copied()
    }

    /// Returns false if `addr` is not a live block (double free or wild pointer).
    pub fn free(&mut self, addr: u32) -> bool {
        let Some(cap) = self.live.remove(&addr) else { return false };
        self.in_use -= cap as u64;
        if cap <= SMALL_MAX {
            self.bins[(cap / ALIGN) as usize].push(addr);
        } else {
            let (mut a, mut s) = (addr, cap);
            if let Some((&pa, &ps)) = self.large_free.range(..a).next_back() {
                if pa + ps == a {
                    self.large_free.remove(&pa);
                    a = pa;
                    s += ps;
                }
            }
            if let Some(&ns) = self.large_free.get(&(a + s)) {
                self.large_free.remove(&(a + s));
                s += ns;
            }
            self.large_free.insert(a, s);
        }
        true
    }
}
