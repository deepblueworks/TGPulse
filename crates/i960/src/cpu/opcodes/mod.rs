use crate::bus::Bus;
use crate::cpu::defs::I960Cpu;

pub mod fpu;
pub mod int;
pub mod mem;
pub mod sys;
pub mod sys_complex;

impl I960Cpu {
    pub fn dispatch_op<B: Bus>(&mut self, bus: &mut B, opcode: u32) {
        let op_idx = opcode >> 24;

        match op_idx {
            // --- Control Flow ---
            0x08..=0x0B | 0x10..=0x17 | 0x30..=0x3E => self.op_sys(bus, opcode),

            // --- Complex System Instructions ---
            0x61 | 0x64..=0x66 => self.op_sys_complex(bus, opcode),

            // --- Integer Math, Logic & Moves ---
            // 0x58-0x5B = logic/arith, 0x5C-0x5F = mov/movl/movt/movq,
            // 0x60 = synmov, 0x70 = unsigned mul/div, 0x74 = signed mul/div
            0x20..=0x27 | 0x58..=0x5F | 0x60 | 0x70 | 0x74 => self.op_int(bus, opcode),

            // --- Memory ---
            0x80..=0x92 | 0x98 | 0x9A | 0xA0..=0xB2 | 0xC0..=0xCA => self.op_mem(bus, opcode),

            // --- FPU ---
            0x67..=0x6E | 0x78..=0x7F => self.op_fpu(bus, opcode),

            _ => {
                self.icount -= 1;
            }
        }
    }
}
