//! Differential checks for selected native segments during a test replay.

use crate::{aot_blocks, mem::Mem};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, HashSet},
    rc::Rc,
};
use unicorn_engine::{uc_error, unicorn_const::HookType, RegisterARM, Unicorn};

#[derive(Default)]
pub struct Verification {
    pub enabled: Cell<bool>,
    pub checking: Cell<bool>,
    attempted: RefCell<HashSet<u32>>,
    pub verified: RefCell<HashSet<usize>>,
}

impl Verification {
    pub fn should_check(&self, entry: u32) -> bool {
        self.enabled.get() && self.attempted.borrow_mut().insert(entry)
    }

    pub fn check(
        &self,
        uc: &mut Unicorn<'_, ()>,
        mem: &mut Mem,
        index: usize,
        entry: u32,
    ) -> Result<(u32, bool), uc_error> {
        use RegisterARM::*;
        let name = aot_blocks::translations()[index].name;
        let context = uc.context_init()?;
        mem.aot_write_log = Some(BTreeMap::new());
        let native = aot_blocks::run(uc, mem, index, entry);
        let before = mem.aot_write_log.take().unwrap();
        let aot_blocks::Execution {
            next,
            progressed,
            steps,
        } = native?;
        if steps == 0 {
            return Ok((next, progressed));
        }

        const THUMB: u32 = 1 << 5;
        let mut registers = [
            R0, R1, R2, R3, R4, R5, R6, R7, R8, R9, R10, R11, R12, SP, LR, CPSR,
        ];
        let mut expected: Vec<_> = uc
            .reg_read_batch(&mut registers, 16)?
            .into_iter()
            .map(|value| value as u32)
            .collect();
        // The native handoff writes PC (and sets T) when Unicorn resumes.
        expected[15] = (expected[15] & !THUMB) | ((next & 1) << 5);
        let mut expected_bytes = BTreeMap::new();
        for (address, byte) in before {
            expected_bytes.insert(address, mem.slice(address, 1).unwrap()[0]);
            mem.w8(address, byte).unwrap();
        }
        uc.context_restore(&context)?;

        // Observe unanticipated Unicorn writes as well as those the native segment performed.
        let unicorn_writes = Rc::new(RefCell::new(BTreeMap::new()));
        let writes = unicorn_writes.clone();
        let hook = uc.add_mem_hook(HookType::MEM_WRITE, 1, 0, move |uc, _, address, len, _| {
            if let Ok(bytes) = uc.mem_read_as_vec(address, len) {
                let mut writes = writes.borrow_mut();
                for (offset, byte) in bytes.into_iter().enumerate() {
                    writes.entry(address as u32 + offset as u32).or_insert(byte);
                }
            }
            true
        })?;
        uc.ctl_flush_tb()?;
        self.checking.set(true);
        let baseline = uc.emu_start(entry.into(), 0xffff_fff0, 0, steps);
        self.checking.set(false);
        uc.remove_hook(hook)?;
        assert!(
            baseline.is_ok(),
            "{name} at {entry:#x}, next {next:#x}: {baseline:?}"
        );
        assert_eq!(
            uc.reg_read(PC)?,
            u64::from(next & !1),
            "{name} at {entry:#x}"
        );
        let actual = uc.reg_read_batch(&mut registers, 16)?;
        for ((register, expected), actual) in registers.into_iter().zip(expected).zip(actual) {
            assert_eq!(
                expected, actual as u32,
                "{name} at {entry:#x} → {next:#x}, {register:?}"
            );
        }
        for (&address, &byte) in unicorn_writes.borrow().iter() {
            expected_bytes.entry(address).or_insert(byte);
        }
        for (address, byte) in expected_bytes {
            assert_eq!(
                mem.slice(address, 1).unwrap()[0],
                byte,
                "{name} at {entry:#x} → {next:#x}, byte at {address:#x}"
            );
        }
        self.verified.borrow_mut().insert(index);
        Ok((next, progressed))
    }
}
