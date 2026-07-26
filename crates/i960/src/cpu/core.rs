//!/ Cycles still owed in the quantum the CPU is executing. The board's hardware
//!/ timers are read back mid-quantum and their low bits are used as an entropy
//!/ source, so the bus needs to know how far through the quantum a read lands --
//!/ returning the value as of the last scheduler boundary makes those bits
//!/ constant.
pub static LIVE_ICOUNT: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

use crate::bus::Bus;
use crate::cpu::defs::{I960Cpu, FP, I960_RCACHE_SIZE, PFP, RIP, SP};

impl I960Cpu {
    pub fn reset<B: Bus>(&mut self, bus: &mut B) {
        self.sat = bus.read_u32(0);
        self.prcb = bus.read_u32(4);
        self.ip = bus.read_u32(12);
        self.pc = 0x001f2002;
        self.ac = 0;
        self.icr = 0xff000000;

        let interrupt_stack = bus.read_u32(self.prcb + 24);
        self.r[FP] = interrupt_stack;
        self.r[SP] = interrupt_stack + 64;
        self.rcache_pos = 0;
        self.immediate_irq = false;
        self.immediate_vector = 0;
        self.immediate_pri = 0;
        self.irq_line_state = [false; 4];

        log::debug!(
            target: "i960",
            "reset: sat={:08X} prcb={:08X} ip={:08X}",
            self.sat,
            self.prcb,
            self.ip
        );
    }

    pub fn execute_run<B: Bus>(&mut self, bus: &mut B, cycles: i32) {
        self.icount = cycles;

        if self.deferred_vector != 0 {
            let vec = self.deferred_vector;
            self.deferred_vector = 0;
            self.request_irq_vector(bus, vec);
        }

        if !self.stall_state.burst_mode {
            // Interrupts asserted while the current priority masks them are
            // recorded in the PRCB pending table. Revisit that table whenever
            // execution resumes: firmware can lower priority with modpc long
            // after the external input edge, and there will be no second edge
            // to remind the controller.
            self.check_pending_irqs(bus);
            self.pending_irq_check = false;
            self.check_immediate_irqs(bus);
        }

        while self.icount > 0 {
            if let Some(lines) = bus.take_irq_lines() {
                for (i, state) in lines.iter().enumerate() {
                    self.set_irq_line(i, *state);
                }
            }
            let start_icount = self.icount;
            if !self.breakpoints.is_empty() && self.breakpoints.contains(&self.ip) {
                self.bp_hit = Some(self.ip);
                break;
            }
            self.pip = self.ip;
            LIVE_ICOUNT.store(self.icount, std::sync::atomic::Ordering::Relaxed);
            if self.trace.is_some() && self.ip == self.trace_stop {
                self.trace_frozen = true;
            }
            if let Some(t) = self.trace.as_mut().filter(|_| !self.trace_frozen) {
                let n = t.0.len();
                if t.1 >= n {
                    // A full buffer stops recording rather than wrapping, so a
                    // trace taken from reset covers the first N instructions
                    // and can be diffed against another emulator's.
                    self.trace_frozen = true;
                } else {
                    t.0[t.1] = self.ip;
                    t.1 += 1;
                }
            }
            let opcode = bus.read_u32(self.ip);

            // --- TRAPS DISABLED ---
            // (Removed loop spam and bounds check spam for speed)

            self.ip = self.ip.wrapping_add(4);
            self.stalled = false;

            if self.stall_state.burst_mode {
                self.execute_burst_stall_op(bus, opcode);
            } else {
                self.dispatch_op(bus, opcode);
            }

            if self.stalled | bus.take_stall() {
                self.ip = self.pip;
                break;
            }

            let elapsed = (start_icount - self.icount) as u32;

            // Timer Logic
            for t in 0..2 {
                if (self.tmr[t] & 2) != 0 {
                    if self.tcr[t] > elapsed {
                        self.tcr[t] -= elapsed;
                    } else {
                        let remainder = elapsed - self.tcr[t];
                        self.tmr[t] |= 1;
                        if (self.tmr[t] & 8) == 0 {
                            self.request_irq_vector(bus, 248 + t as i32);
                            self.tmr[t] |= 0x10;
                        }
                        if (self.tmr[t] & 4) != 0 {
                            if self.trr[t] > remainder {
                                self.tcr[t] = self.trr[t] - remainder;
                            } else {
                                self.tcr[t] = 0;
                            }
                        } else {
                            self.tmr[t] &= !2;
                            self.tcr[t] = 0;
                        }
                    }
                }
            }
        }
    }

    pub fn burst_stall_save(
        &mut self,
        t1: u32,
        t2: usize,
        index: usize,
        size: usize,
        is_write: bool,
    ) {
        self.stall_state.t1 = t1;
        self.stall_state.t2 = t2;
        self.stall_state.index = index;
        self.stall_state.size = size;
        self.stall_state.is_write_op = is_write;
        self.stall_state.burst_mode = true;
    }

    pub fn execute_burst_stall_op<B: Bus>(&mut self, bus: &mut B, opcode: u32) {
        // The instruction is re-fetched from scratch, so walk its effective
        // address again -- for the MEMB forms that is what steps the IP past
        // the displacement word.
        let _ = self.get_ea(bus, opcode);
        for i in self.stall_state.index..self.stall_state.size {
            self.icount -= 1;
            if self.stall_state.is_write_op {
                let val = self.r[self.stall_state.t2 + i];
                bus.write_u32(self.stall_state.t1, val);
            } else {
                let val = bus.read_u32(self.stall_state.t1);
                self.r[self.stall_state.t2 + i] = val;
            }
            self.stalled |= bus.take_stall();
            if self.stalled {
                self.stall_state.index = i;
                self.ip = self.pip;
                return;
            }
            if bus.burst_capable(self.stall_state.t1) {
                self.stall_state.t1 = self.stall_state.t1.wrapping_add(4);
            }
        }
        self.stall_state.burst_mode = false;
        self.check_immediate_irqs(bus);
    }

    pub fn do_call<B: Bus>(&mut self, bus: &mut B, adr: u32, type_: u32, stack: u32) {
        self.icount -= 9;
        self.r[RIP] = self.ip;
        if self.rcache_pos >= I960_RCACHE_SIZE as i32 {
            let fp = self.r[FP] & !0x3f;
            for i in 0..16 {
                bus.write_u32(fp + (i as u32 * 4), self.r[i]);
            }
        } else {
            let pos = self.rcache_pos as usize;
            self.rcache[pos].copy_from_slice(&self.r[0..16]);
            self.rcache_frame_addr[pos] = self.r[FP] & !0x3f;
        }
        self.rcache_pos += 1;
        self.ip = adr;
        self.r[PFP] = (self.r[FP] & !7) | type_;
        if type_ == 7 {
            self.r[SP] = stack;
        }
        self.r[FP] = (self.r[SP] + 63) & !63;
        self.r[SP] = self.r[FP] + 64;
    }

    /// Restores the previous register frame and returns to the caller. This is
    /// the common part of every return type; `do_ret` adds the type-specific
    /// handling on top.
    pub fn do_ret_0<B: Bus>(&mut self, bus: &mut B) {
        self.r[FP] = self.r[PFP] & !0x3f;
        self.rcache_pos -= 1;
        if self.rcache_pos >= I960_RCACHE_SIZE as i32 || self.rcache_pos < 0 {
            for i in 0..16 {
                self.r[i] = bus.read_u32(self.r[FP] + (i as u32 * 4));
            }
            if self.rcache_pos < 0 {
                self.rcache_pos = 0;
            }
        } else {
            self.r[0..16].copy_from_slice(&self.rcache[self.rcache_pos as usize]);
        }
        self.ip = self.r[RIP];
    }

    pub fn do_ret<B: Bus>(&mut self, bus: &mut B) {
        self.icount -= 7;
        match self.r[PFP] & 7 {
            0 => self.do_ret_0(bus),
            7 => {
                // Interrupt return: restore the saved PC and AC, which is what
                // drops the CPU's priority back down and re-enables further
                // interrupts.
                let saved_pc = bus.read_u32(self.r[FP].wrapping_sub(16));
                let saved_ac = bus.read_u32(self.r[FP].wrapping_sub(12));
                self.do_ret_0(bus);
                self.ac = saved_ac;
                self.pc = saved_pc;
                self.check_pending_irqs(bus);
            }
            _ => self.do_ret_0(bus),
        }
    }

    pub fn generate_fault<B: Bus>(&mut self, bus: &mut B, ftype: u32, fsubtype: u32) {
        log::debug!(
            target: "i960",
            "fault type={ftype} sub={fsubtype} at ip={:08X}",
            self.ip
        );
        let fault_tab = bus.read_u32(self.prcb);
        let entry_addr = fault_tab.wrapping_add(ftype * 4);
        let handler_addr = bus.read_u32(entry_addr);
        let sp = self.r[SP];
        self.do_call(bus, handler_addr, 0, sp);
        let fault_word = 0x02 | (ftype << 8) | (fsubtype << 16);
        let fp = self.r[FP] & !0x3f;
        bus.write_u32(fp.wrapping_sub(16), fault_word);
        bus.write_u32(fp.wrapping_sub(12), self.pip);
        bus.write_u32(fp.wrapping_sub(8), 0);
    }
}
