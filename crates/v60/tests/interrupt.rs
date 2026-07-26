//! Validates the V60 maskable-interrupt path in isolation: acknowledge, the
//! PSW-exception rewrite, the user->interrupt stack switch, the pushed
//! PC/PSW frame, the SBR vector fetch, and the RETIU round-trip back.
//!
//!

use v60::cpu::{PC, PSW, SP};
use v60::{Bus, V60};

struct Ram(Vec<u8>);
impl Bus for Ram {
    fn read_u8(&mut self, a: u32) -> u8 {
        self.0.get(a as usize).copied().unwrap_or(0)
    }
    fn write_u8(&mut self, a: u32, v: u8) {
        if let Some(b) = self.0.get_mut(a as usize) {
            *b = v;
        }
    }
}

const ISP: usize = 36; // interrupt stack pointer
const L0SP: usize = 37; // level-0 (user) stack pointer register
const SBR: usize = 41;

#[test]
fn vblank_irq_enters_handler_and_returns() {
    let mut mem = Ram(vec![0u8; 0x10000]);
    // Handler at 0x1000: NOP, then RETIU #0 (0xE0 = ImmediateQuick 0).
    mem.0[0x1000] = 0xcd; // NOP
    mem.0[0x1001] = 0xea; // RETIU (modm 0)
    mem.0[0x1002] = 0xe0; // operand: immediate 0 -> no stack adjust
                          // Interrupted code at 0x0100: a NOP we should return to.
    mem.0[0x0100] = 0xcd;
    // SBR interrupt table at 0x2000; vblank vector index = 1 + 0x40 = 0x41.
    let vec_slot = 0x2000 + 0x41 * 4;
    mem.0[vec_slot..vec_slot + 4].copy_from_slice(&0x0000_1000u32.to_le_bytes());

    let mut cpu = V60::new();
    cpu.reg[PSW] = 0x0004_0000; // IE set, IS clear, level 0
    cpu.reg[PC] = 0x0100;
    cpu.reg[SP] = 0x8000; // user stack
    cpu.reg[ISP] = 0x9000; // interrupt stack
    cpu.reg[SBR] = 0x2000;

    // Fire vblank (level 1). It is taken before the next instruction.
    cpu.assert_irq(1);
    cpu.run(&mut mem, 8); // acknowledge + one handler instruction (the NOP)
                          // Single pulse: release the level-held line now that it is acknowledged, so
                          // it does not re-fire once RETIU restores IE.
    cpu.clear_irq();

    // Entered the handler one instruction in (NOP advanced 0x1000 -> 0x1001).
    assert_eq!(cpu.reg[PC], 0x1001, "did not vector to the handler");
    // Frame pushed onto the interrupt stack: SP moved down 8 from ISP.
    assert_eq!(cpu.reg[SP], 0x9000 - 8, "frame not on the interrupt stack");
    assert_eq!(mem.read_u32(0x9000 - 8), 0x0100, "saved PC wrong");
    assert_eq!(mem.read_u32(0x9000 - 4), 0x0004_0000, "saved PSW wrong");
    // PSW rewritten for the interrupt: IS set, ASA set, IE cleared.
    assert_eq!(cpu.reg[PSW], 0x9000_0000, "exception PSW wrong");
    // The user SP was banked out to the level-0 stack register.
    assert_eq!(cpu.reg[L0SP], 0x8000, "user SP not banked out");

    // Run the RETIU: pop PC and PSW, switch back to the user stack.
    cpu.run(&mut mem, 8);
    assert_eq!(cpu.reg[PC], 0x0100, "RETIU did not restore PC");
    assert_eq!(cpu.reg[PSW], 0x0004_0000, "RETIU did not restore PSW");
    assert_eq!(cpu.reg[SP], 0x8000, "RETIU did not restore the user stack");
    assert_eq!(cpu.reg[ISP], 0x9000, "interrupt SP not banked back");
}

#[test]
fn irq_ignored_while_disabled() {
    let mut mem = Ram(vec![0u8; 0x10000]);
    mem.0[0x0100] = 0xcd; // NOP
    let mut cpu = V60::new();
    cpu.reg[PSW] = 0x0000_0000; // IE clear
    cpu.reg[PC] = 0x0100;
    cpu.reg[SP] = 0x8000;

    cpu.assert_irq(1);
    cpu.run(&mut mem, 8);
    // With interrupts masked, the NOP just runs; no vectoring, no push.
    assert_eq!(cpu.reg[PC], 0x0101, "took an interrupt while IE was clear");
    assert_eq!(cpu.reg[SP], 0x8000, "stack disturbed while IE was clear");
}
