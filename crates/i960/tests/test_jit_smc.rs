mod common;

// The dynarec caches compiled blocks by address; code in RAM that is later
// rewritten (uploaded or self-modifying code) must not run stale. The mock
// bus here bumps the page epoch on every write, the way the Model 2 memory
// map does for its executable RAM, so the block cache is forced to revalidate.

use i960::bus::Bus;
use i960::cpu::I960Cpu;

struct System {
    ram: [u8; 0x10000],
    epochs: std::collections::HashMap<u32, u64>,
}

impl System {
    fn new() -> Self {
        Self {
            ram: [0; 0x10000],
            epochs: std::collections::HashMap::new(),
        }
    }

    fn write_u32(&mut self, addr: u32, val: u32) {
        for (i, b) in val.to_le_bytes().into_iter().enumerate() {
            self.write_byte(addr + i as u32, b);
        }
    }

    fn read_u32(&mut self, addr: u32) -> u32 {
        u32::from_le_bytes([
            self.read_byte(addr),
            self.read_byte(addr + 1),
            self.read_byte(addr + 2),
            self.read_byte(addr + 3),
        ])
    }

    fn load_program(&mut self, addr: u32, opcodes: &[u32]) {
        for (i, &word) in opcodes.iter().enumerate() {
            self.write_u32(addr + (i as u32 * 4), word);
        }
    }
}

impl Bus for System {
    fn read_byte(&mut self, addr: u32) -> u8 {
        self.ram.get(addr as usize).copied().unwrap_or(0)
    }

    fn write_byte(&mut self, addr: u32, val: u8) {
        if let Some(b) = self.ram.get_mut(addr as usize) {
            *b = val;
            *self.epochs.entry(addr >> 12).or_insert(0) += 1;
        }
    }

    fn code_epoch(&self, page: u32) -> u64 {
        self.epochs.get(&page).copied().unwrap_or(0)
    }
}

/// Same minimal PRCB the other suites install, so the pending-interrupt scan
/// reads a zeroed table instead of the program.
fn setup_interrupts(sys: &mut System, cpu: &mut I960Cpu) {
    const PRCB: u32 = 0x0F00;
    const INT_TABLE: u32 = 0x0F80;
    cpu.prcb = PRCB;
    sys.write_u32(PRCB + 20, INT_TABLE);
}

#[test]
fn test_jit_code_rewrite() {
    let mut sys = System::new();
    let mut cpu = I960Cpu::new();
    setup_interrupts(&mut sys, &mut cpu);

    let program = [
        0x5C180E01, // 0x00: mov 1, r3
        0x92180100, // 0x04: st r3, 0x100
        0x08000000, // 0x08: b .
    ];
    sys.load_program(0x00, &program);
    cpu.ip = 0x00;
    common::run(&mut cpu, &mut sys, 10);
    assert_eq!(sys.read_u32(0x100), 1, "initial run should store 1");

    // Patch the first instruction in place and run it again. The write bumps
    // the page epoch, so the cached block for 0x00 must be recompiled --
    // with a stale cache the store would still write 1.
    sys.write_u32(0x00, 0x5C180E02); // mov 2, r3
    cpu.ip = 0x00;
    common::run(&mut cpu, &mut sys, 10);
    assert_eq!(
        sys.read_u32(0x100),
        2,
        "after patching the instruction the block must be recompiled"
    );
}

#[test]
fn test_jit_data_write_same_page() {
    // A data write to the page a block was compiled from also invalidates it
    // (the epoch is per page, not per word). Correctness must hold; this is
    // the coarse-invalidation cost documented in cpu::jit.
    let mut sys = System::new();
    let mut cpu = I960Cpu::new();
    setup_interrupts(&mut sys, &mut cpu);

    let program = [
        0x5C180E07, // 0x00: mov 7, r3
        0x92180100, // 0x04: st r3, 0x100
        0x08000000, // 0x08: b .
    ];
    sys.load_program(0x00, &program);
    cpu.ip = 0x00;
    common::run(&mut cpu, &mut sys, 10);
    assert_eq!(sys.read_u32(0x100), 7);

    // Data write into the same page, leaving the code untouched.
    sys.write_u32(0x200, 0xDEADBEEF);
    cpu.ip = 0x00;
    common::run(&mut cpu, &mut sys, 10);
    assert_eq!(sys.read_u32(0x100), 7);
    assert_eq!(sys.read_u32(0x200), 0xDEADBEEF);
}
