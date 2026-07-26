pub trait Bus {
    fn read_byte(&mut self, addr: u32) -> u8;
    fn write_byte(&mut self, addr: u32, val: u8);

    /// Interrupt line states the board has changed since the last call, if
    /// any. The board cannot poke the CPU directly while it is executing --
    /// the scheduler has moved the core out of the machine struct -- so line
    /// changes made from a memory write are handed over here instead, and take
    /// effect on the very next instruction rather than at the next quantum.
    fn take_irq_lines(&mut self) -> Option<[bool; 4]> {
        None
    }

    /// Returns and clears an external wait request. The current instruction
    /// is retried after the machine scheduler lets the peer device run.
    fn take_stall(&mut self) -> bool {
        false
    }

    /// Whether a burst transfer (`ldl`/`ldt`/`ldq` and the store forms) may
    /// advance the address between the words of the burst.
    ///
    /// The i960's burst bus cycle presents one address and clocks out up to
    /// four words; only memory able to keep up with that answers with
    /// successive words. Registers and other single-cycle devices see the same
    /// address for every word of the burst, so a two-word load from a register
    /// pair reads the *first* register twice rather than two neighbours. Boards
    /// that do not care can leave this as it is.
    fn burst_capable(&self, _addr: u32) -> bool {
        true
    }

    fn read_u16(&mut self, addr: u32) -> u16 {
        let low = self.read_byte(addr) as u16;
        let high = self.read_byte(addr.wrapping_add(1)) as u16;
        (high << 8) | low
    }

    fn write_u16(&mut self, addr: u32, val: u16) {
        self.write_byte(addr, (val & 0xFF) as u8);
        self.write_byte(addr.wrapping_add(1), ((val >> 8) & 0xFF) as u8);
    }

    fn read_u32(&mut self, addr: u32) -> u32 {
        u32::from_le_bytes([
            self.read_byte(addr),
            self.read_byte(addr.wrapping_add(1)),
            self.read_byte(addr.wrapping_add(2)),
            self.read_byte(addr.wrapping_add(3)),
        ])
    }

    fn write_u32(&mut self, addr: u32, val: u32) {
        let bytes = val.to_le_bytes();
        self.write_byte(addr, bytes[0]);
        self.write_byte(addr.wrapping_add(1), bytes[1]);
        self.write_byte(addr.wrapping_add(2), bytes[2]);
        self.write_byte(addr.wrapping_add(3), bytes[3]);
    }
}
