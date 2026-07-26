use crate::bus::Bus;
use crate::cpu::defs::{I960Cpu, FP, SP};

// IRQ Line Constants
pub const I960_IRQ0: usize = 0;
pub const I960_IRQ1: usize = 1;
pub const I960_IRQ2: usize = 2;
pub const I960_IRQ3: usize = 3;

impl I960Cpu {
    /// External hardware calls this to raise an IRQ line (0-3).
    ///
    /// NOTE: This function does NOT require the Bus. If an interrupt needs to be
    /// queued to memory, it sets a deferred flag which is processed next CPU cycle.
    /// Same as `set_irq_line`, but with the bus to hand so a request the CPU
    /// cannot take right now is recorded in the PRCB's pending-interrupt table
    /// rather than a private latch. `ret` from an interrupt rescans that table,
    /// which is how the hardware re-enters a handler that re-armed its own
    /// source -- the game's sound task depends on exactly that.
    pub fn set_irq_line_bus<B: Bus>(&mut self, bus: &mut B, irqline: usize, state: bool) {
        if irqline >= self.irq_line_state.len() || self.irq_line_state[irqline] == state {
            return;
        }
        self.irq_line_state[irqline] = state;
        if !state {
            return;
        }
        let vector = match irqline {
            I960_IRQ0 => self.icr & 0xff,
            I960_IRQ1 => (self.icr >> 8) & 0xff,
            I960_IRQ2 => (self.icr >> 16) & 0xff,
            I960_IRQ3 => (self.icr >> 24) & 0xff,
            _ => 0,
        };
        if vector != 0 {
            self.request_irq_vector(bus, vector as i32);
        }
    }

    pub fn set_irq_line(&mut self, irqline: usize, state: bool) {
        if irqline >= self.irq_line_state.len() || self.irq_line_state[irqline] == state {
            return;
        }
        self.irq_line_state[irqline] = state;
        if !state {
            return;
        }

        let vector = match irqline {
            I960_IRQ0 => self.icr & 0xff,
            I960_IRQ1 => (self.icr >> 8) & 0xff,
            I960_IRQ2 => (self.icr >> 16) & 0xff,
            I960_IRQ3 => (self.icr >> 24) & 0xff,
            _ => 0,
        };

        if vector != 0 {
            let priority = (vector / 8) as i32;
            let cpu_pri = (self.pc >> 16) & 0x1f;

            // Check if we can take this "right now" (Immediate Dispatch) without Bus access
            if (cpu_pri < priority as u32 || priority == 31) && !self.immediate_irq {
                self.immediate_irq = true;
                self.immediate_vector = vector as i32;
                self.immediate_pri = priority;
                self.immediate_line = Some(irqline);
            } else {
                // Otherwise, we need to write to the Pending Interrupt Table in RAM.
                // Since we don't have Bus access here, we latch the vector.
                // The core loop will pick this up immediately.
                self.deferred_vector = vector as i32;
                self.pending_irq_check = true;
            }
        }
    }

    /// Request an interrupt by Vector Number directly (Used by Timers/Software/Internal)
    /// This version requires Bus access.
    pub fn request_irq_vector<B: Bus>(&mut self, bus: &mut B, vector: i32) {
        let priority = vector / 8;
        let cpu_pri = (self.pc >> 16) & 0x1f;

        // Check if we can take this "right now" (Immediate Dispatch)
        if (cpu_pri < priority as u32 || priority == 31) && !self.immediate_irq {
            self.immediate_irq = true;
            self.immediate_vector = vector;
            self.immediate_pri = priority;
        } else {
            // Otherwise, Queue it in the memory-based Pending Interrupt Table
            let int_tab = bus.read_u32(self.prcb + 20);

            // 1. Set bit in "Pending Priorities" word (Offset 0)
            let mut pend = bus.read_u32(int_tab);
            pend |= 1 << priority;
            bus.write_u32(int_tab, pend);

            // 2. Set bit in the specific priority vector word
            // Word offset calculation: (priority / 4) * 4 bytes + 4 (skip pending word)
            let word_offset = ((vector / 32) * 4) + 4;
            let bit_offset = vector % 32;

            let addr = int_tab.wrapping_add(word_offset as u32);
            let mut vword = bus.read_u32(addr);
            vword |= 1 << bit_offset;
            bus.write_u32(addr, vword);
        }
    }

    /// Check if an immediate interrupt is waiting and service it
    pub fn check_immediate_irqs<B: Bus>(&mut self, bus: &mut B) {
        let cpu_pri = (self.pc >> 16) & 0x1f;

        // A request whose line has since gone away is not taken: the board
        // deasserted the pin (typically by masking the source) before the CPU
        // could get to it, and dispatching anyway lands in the game's
        // spurious-interrupt handler.
        if let Some(line) = self.immediate_line {
            if !self.irq_line_state[line] {
                self.immediate_irq = false;
                self.immediate_line = None;
                return;
            }
        }
        if self.immediate_irq
            && ((cpu_pri < self.immediate_pri as u32) || (self.immediate_pri == 31))
        {
            let vec = self.immediate_vector;
            let pri = self.immediate_pri;
            self.take_interrupt(bus, vec, pri);
            self.immediate_irq = false;
            self.immediate_line = None;
        }
    }

    /// Scan memory for pending interrupts (Full Logic)
    pub fn check_pending_irqs<B: Bus>(&mut self, bus: &mut B) {
        let int_tab = bus.read_u32(self.prcb + 20);
        let cpu_pri = (self.pc >> 16) & 0x1f;
        let mut pending_pri = bus.read_u32(int_tab);

        // Scan priority levels 31 down to 0
        for lvl in (0..=31).rev() {
            // If bit set AND (level > cpu_pri OR level is NMI 31)
            if ((pending_pri & (1 << lvl)) != 0) && ((cpu_pri < lvl) || (lvl == 31)) {
                let word_offset = ((lvl / 4) * 4) + 4;
                // Calculate which bits in the 32-bit word belong to this level (8 bits per level)
                let bit_start = (lvl % 4) * 8;
                let bit_end = bit_start + 8; // exclusive

                let addr = int_tab.wrapping_add(word_offset);
                let mut vword = bus.read_u32(addr);

                // Find the first set bit in this level's range
                for irq in (bit_start..bit_end).rev() {
                    if (vword & (1 << irq)) != 0 {
                        // Found a vector!

                        // Clear the bit in memory
                        vword &= !(1 << irq);
                        bus.write_u32(addr, vword);

                        // Calculate actual vector number
                        let vector = ((lvl / 4) * 32) + irq;

                        // An entry posted by one of the four external lines is
                        // only honoured while that line is still asserted; the
                        // board deasserts it when the source is masked, and
                        // taking it anyway reaches the game's spurious-
                        // interrupt handler.
                        let from_line = [
                            self.icr & 0xff,
                            (self.icr >> 8) & 0xff,
                            (self.icr >> 16) & 0xff,
                            (self.icr >> 24) & 0xff,
                        ]
                        .iter()
                        .position(|v| *v != 0 && *v == vector);
                        if let Some(line) = from_line {
                            if !self.irq_line_state[line] {
                                continue;
                            }
                        }

                        // Check if we cleared the last bit for this level
                        let lvl_mask = 0xFF << ((lvl % 4) * 8);
                        if (vword & lvl_mask) == 0 {
                            pending_pri &= !(1 << lvl);
                            bus.write_u32(int_tab, pending_pri);
                        }

                        self.take_interrupt(bus, vector as i32, lvl as i32);
                        return; // Only take one interrupt per check
                    }
                }

                // If we got here, the pending bit was set but no vector was found in the word.
                // This is an error state in the table. Clear the phantom pending bit.
                pending_pri &= !(1 << lvl);
                bus.write_u32(int_tab, pending_pri);
                return;
            }
        }
    }

    /// Perform the context switch for an interrupt
    fn take_interrupt<B: Bus>(&mut self, bus: &mut B, vector: i32, lvl: i32) {
        let int_tab = bus.read_u32(self.prcb + 20);
        let int_sp = bus.read_u32(self.prcb + 24);

        // Read vector entry from table
        // Table starts at offset 36. Each entry is 32-bits.
        let vector_entry_addr = int_tab + 36 + ((vector as u32 - 8) * 4);
        let irq_handler = bus.read_u32(vector_entry_addr);
        self.interrupt_count += 1;
        self.last_interrupt_vector = vector;
        self.last_interrupt_handler = irq_handler;

        // Determine stack to use
        // If PC has bit 13 (0x2000) set, we are already in Interrupted State (nested).
        let mut sp = if (self.pc & 0x2000) == 0 {
            int_sp // Switch to Interrupt Stack
        } else {
            self.r[SP] // Use current stack
        };

        // Align stack (Round up to next 64-byte boundary)
        sp = (sp + 63) & !63;
        sp += 64; // Padding

        // Perform the call (Type 7 = Interrupt)
        self.do_call(bus, irq_handler, 7, sp);

        // Save Processor State to the new stack frame (Pre-decrement from FP)
        bus.write_u32(self.r[FP].wrapping_sub(16), self.pc);
        bus.write_u32(self.r[FP].wrapping_sub(12), self.ac);
        bus.write_u32(self.r[FP].wrapping_sub(8), (vector - 8) as u32);

        // Update PC Register (Status)
        self.pc &= !0x001f_0000; // Clear old priority (PC bits 16-20)
        self.pc |= (lvl as u32) << 16; // Set new priority
        self.pc |= 0x2002; // Set Supervisor Mode & Interrupt Flag
    }

    /// Inter-Agent Communication (IAC) - CPU Control Messages
    pub fn send_iac<B: Bus>(&mut self, bus: &mut B, addr: u32) {
        let w0 = bus.read_u32(addr);
        let w1 = bus.read_u32(addr + 4);
        let w2 = bus.read_u32(addr + 8);
        let w3 = bus.read_u32(addr + 12);

        let msg_type = w0 >> 24;

        match msg_type {
            0x40 => { /* Generate IRQ */ }
            0x41 => {
                self.check_pending_irqs(bus);
            }
            0x80 => {
                bus.write_u32(w1, self.sat);
                bus.write_u32(w1 + 4, self.prcb);
            }
            0x89 => { /* Invalidate I-Cache */ }
            0x8F => { /* Breakpoints */ }
            0x91 => { /* Stop Processor */ }
            0x92 => { /* Continue */ }
            0x93 => {
                // Re-Initialize (Warm Boot)
                self.sat = w1;
                self.prcb = w2;
                self.ip = w3;
            }
            _ => {}
        }
    }
}
