use v60::cpu::{PC, PSW, SP};
use v60::{Bus, V60};

struct Ram(Vec<u8>);

impl Bus for Ram {
    fn read_u8(&mut self, addr: u32) -> u8 {
        self.0.get(addr as usize).copied().unwrap_or(0)
    }

    fn write_u8(&mut self, addr: u32, value: u8) {
        if let Some(dst) = self.0.get_mut(addr as usize) {
            *dst = value;
        }
    }
}

#[test]
fn halt_waits_without_refetching_and_wakes_on_irq() {
    const ISP: usize = 36;
    const SBR: usize = 41;

    let mut ram = Ram(vec![0; 0x4000]);
    ram.0[0x0100] = 0x00;
    ram.0[0x0101] = 0xcd;
    ram.0[0x0200] = 0xcd;

    let vector = 0x1000 + 0x41 * 4;
    ram.0[vector..vector + 4].copy_from_slice(&0x0200u32.to_le_bytes());

    let mut cpu = V60::new();
    cpu.reg[PC] = 0x0100;
    cpu.reg[PSW] = 1 << 18;
    cpu.reg[SP] = 0x3000;
    cpu.reg[ISP] = 0x3800;
    cpu.reg[SBR] = 0x1000;

    cpu.run(&mut ram, 32);
    assert!(cpu.halted);
    assert_eq!(cpu.pc(), 0x0101);
    assert_eq!(cpu.op_count[0x00], 1);

    cpu.run(&mut ram, 32);
    assert_eq!(cpu.pc(), 0x0101);
    assert_eq!(cpu.op_count[0x00], 1);

    cpu.assert_irq(1);
    cpu.run(&mut ram, 8);
    cpu.clear_irq();

    assert!(!cpu.halted);
    assert_eq!(cpu.pc(), 0x0201);
}

struct ExternalHaltRam {
    bytes: Vec<u8>,
    halt: bool,
}

impl Bus for ExternalHaltRam {
    fn read_u8(&mut self, addr: u32) -> u8 {
        let value = self.bytes.get(addr as usize).copied().unwrap_or(0);
        if addr == 0x0100 {
            self.halt = true;
        }
        value
    }

    fn write_u8(&mut self, addr: u32, value: u8) {
        if let Some(dst) = self.bytes.get_mut(addr as usize) {
            *dst = value;
        }
    }

    fn halt_requested(&self) -> bool {
        self.halt
    }
}

#[test]
fn external_fifo_halt_stops_after_current_instruction() {
    let mut ram = ExternalHaltRam {
        bytes: vec![0; 0x200],
        halt: false,
    };
    ram.bytes[0x0100] = 0xcd;
    ram.bytes[0x0101] = 0xcd;

    let mut cpu = V60::new();
    cpu.reg[PC] = 0x0100;

    cpu.run(&mut ram, 64);

    assert_eq!(cpu.pc(), 0x0101);
    assert_eq!(cpu.icount, 0);
    assert_eq!(cpu.op_count[0xcd], 1);

    ram.halt = false;
    cpu.run(&mut ram, 8);

    assert_eq!(cpu.pc(), 0x0102);
    assert_eq!(cpu.op_count[0xcd], 2);
}
