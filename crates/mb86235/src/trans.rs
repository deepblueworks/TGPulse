//! The transfer slots: the data movement that issues alongside the ALU.
//!

use crate::state::Mb86235;
use crate::Mb86235Bus;

impl Mb86235 {
    /// Effective address for a transfer slot. Modes that
    /// post-modify an address register are frozen while the instruction is
    /// stalled on the input FIFO, so a retry recomputes the same address.
    fn decode_ea(&mut self, mode: u8, rx: usize, ry: usize, disp: u32, isbbus: bool) -> u32 {
        // Bit-reversed/modulo addressing width, from MOD.
        let bits = if isbbus {
            (self.mod_ >> 8) & 7
        } else {
            (self.mod_ >> 12) & 7
        };
        let masked = |v: u32| v & (0x1ff >> (7 - bits.min(7)));
        let stalled = self.stalled;

        match mode {
            0x00 => self.ar[rx],
            0x01 => {
                let res = self.ar[rx];
                if !stalled {
                    self.ar[rx] = res.wrapping_add(1) & 0x3fff;
                }
                res
            }
            0x02 => {
                let res = self.ar[rx];
                if !stalled {
                    self.ar[rx] = res.wrapping_sub(1) & 0x3fff;
                }
                res
            }
            0x03 => {
                let res = self.ar[rx];
                if !stalled {
                    self.ar[rx] = res.wrapping_add(disp) & 0x3fff;
                }
                res
            }
            0x04 => self.ar[rx].wrapping_add(self.ar[ry]),
            0x05 => {
                let res = self.ar[ry];
                if !stalled {
                    self.ar[ry] = res.wrapping_add(1) & 0x3fff;
                }
                self.ar[rx].wrapping_add(res)
            }
            0x06 => {
                let res = self.ar[ry];
                if !stalled {
                    self.ar[ry] = res.wrapping_sub(1) & 0x3fff;
                }
                self.ar[rx].wrapping_add(res)
            }
            0x07 => {
                let res = self.ar[ry];
                if !stalled {
                    self.ar[ry] = res.wrapping_add(disp) & 0x3fff;
                }
                self.ar[rx].wrapping_add(res)
            }
            0x08 => {
                let y = if isbbus {
                    self.ar[ry] & 0x7f
                } else {
                    self.ar[ry] >> 7
                };
                self.ar[rx].wrapping_add(y)
            }
            0x09 => {
                let y = if isbbus {
                    self.ar[ry] >> 7
                } else {
                    self.ar[ry] & 0x7f
                };
                self.ar[rx].wrapping_add(y)
            }
            0x0a => self.ar[rx].wrapping_add(disp),
            0x0b => self.ar[rx].wrapping_add(self.ar[ry]).wrapping_add(disp),
            0x0d => {
                let res = masked(self.ar[ry]);
                if !stalled {
                    self.ar[ry] = self.ar[ry].wrapping_add(1) & 0x3fff;
                }
                self.ar[rx].wrapping_add(res)
            }
            0x0e => {
                let res = masked(self.ar[ry]);
                if !stalled {
                    self.ar[ry] = self.ar[ry].wrapping_sub(1) & 0x3fff;
                }
                self.ar[rx].wrapping_add(res)
            }
            0x0f => {
                let res = masked(self.ar[ry]);
                if !stalled {
                    self.ar[ry] = self.ar[ry].wrapping_add(disp) & 0x3fff;
                }
                self.ar[rx].wrapping_add(res)
            }
            _ => 0,
        }
    }

    /// Reads a transfer source that may be a register or a memory word; the
    /// 0x40 bit picks memory and 0x20 picks the B bus. `imm58` supplies the
    /// literal that source code 0x58 means for this class.
    #[allow(clippy::too_many_arguments)]
    fn read_src<B: Mb86235Bus>(
        &mut self,
        bus: &mut B,
        sr: u8,
        mode: u8,
        rx: usize,
        ry: usize,
        disp: u32,
        imm58: Option<u32>,
    ) -> u32 {
        if sr & 0x40 != 0 {
            if let (0x58, Some(v)) = (sr, imm58) {
                return v;
            }
            let isbbus = sr & 0x20 != 0;
            let addr = self.decode_ea(mode, (sr & 7) as usize, ry, disp, isbbus);
            let _ = rx;
            if isbbus {
                self.read_bbus(addr)
            } else {
                self.read_abus(bus, addr)
            }
        } else {
            self.get_transfer_reg(bus, sr)
        }
    }

    fn write_dst<B: Mb86235Bus>(
        &mut self,
        bus: &mut B,
        dr: u8,
        mode: u8,
        ry: usize,
        disp: u32,
        value: u32,
    ) {
        if dr & 0x40 != 0 {
            let isbbus = dr & 0x20 != 0;
            let addr = self.decode_ea(mode, (dr & 7) as usize, ry, disp, isbbus);
            if isbbus {
                self.write_bbus(addr, value);
            } else {
                self.write_abus(bus, addr, value);
            }
        } else {
            self.set_transfer_reg(bus, dr, value);
        }
    }

    /// Sign-extends the 6-bit EO displacement the external transfers carry.
    #[inline]
    fn eo_disp(raw: u32) -> i32 {
        let d = (raw & 0x3f) as i32;
        if d & 0x20 != 0 {
            d - 0x40
        } else {
            d
        }
    }

    /// Class 0: ALU (2 slots) + two transfers.
    pub(crate) fn do_alu2_trans2<B: Mb86235Bus>(&mut self, bus: &mut B, op: u64) {
        let sd = ((op >> 25) & 3) as u8;
        let (ares, bres) = match sd {
            0 | 1 => {
                let a = self.get_transfer_reg(bus, ((op >> 20) & 0x1f) as u8);
                let b = self.get_transfer_reg(bus, (((op >> 10) & 0xf) as u8) | 0x20);
                (a, b)
            }
            _ => {
                let aa = self.decode_ea(
                    (op & 0xf) as u8,
                    ((op >> 17) & 7) as usize,
                    ((op >> 14) & 7) as usize,
                    0,
                    false,
                );
                let a = self.read_abus(bus, aa);
                let ba = self.decode_ea(
                    (op & 0xf) as u8,
                    ((op >> 7) & 7) as usize,
                    ((op >> 4) & 7) as usize,
                    0,
                    true,
                );
                let b = self.read_bbus(ba);
                (a, b)
            }
        };

        self.do_alu2_op(op);

        match sd {
            0 | 2 => {
                self.set_transfer_reg(bus, ((op >> 20) & 0x1f) as u8, ares);
                self.set_transfer_reg(bus, (((op >> 10) & 0xf) as u8) | 0x20, bres);
            }
            _ => {
                let aa = self.decode_ea(
                    (op & 0xf) as u8,
                    ((op >> 17) & 7) as usize,
                    ((op >> 14) & 7) as usize,
                    0,
                    false,
                );
                self.write_abus(bus, aa, ares);
                let ba = self.decode_ea(
                    (op & 0xf) as u8,
                    ((op >> 7) & 7) as usize,
                    ((op >> 4) & 7) as usize,
                    0,
                    true,
                );
                self.write_bbus(ba, bres);
            }
        }
    }

    /// Class 1: ALU (2 slots) + one transfer, including the external-bus forms.
    pub(crate) fn do_alu2_trans1<B: Mb86235Bus>(&mut self, bus: &mut B, op: u64) {
        let mode = (op & 0xf) as u8;
        let ry = ((op >> 4) & 7) as usize;
        let disp = ((op >> 7) & 0x1f) as u32;

        if op & (1 << 26) != 0 {
            // external transfer
            if op & (1 << 25) != 0 {
                // ext -> int
                let addr = self.eb.wrapping_add(self.eo);
                let res = bus.data_read(addr);
                self.do_alu2_op(op);
                let dr = ((op >> 12) & 0x7f) as u8;
                self.write_dst(bus, dr, mode, ry, disp, res);
                self.eo = self
                    .eo
                    .wrapping_add(Self::eo_disp((op >> 19) as u32) as u32);
            } else {
                // int -> ext
                let sr = ((op >> 12) & 0x7f) as u8;
                let res = self.read_src(bus, sr, mode, 0, ry, disp, None);
                self.do_alu2_op(op);
                let addr = self.eb.wrapping_add(self.eo);
                bus.data_write(addr, res);
                self.eo = self
                    .eo
                    .wrapping_add(Self::eo_disp((op >> 19) as u32) as u32);
            }
        } else {
            let sr = ((op >> 19) & 0x7f) as u8;
            let res = self.read_src(bus, sr, mode, 0, ry, disp, Some((op & 0xfff) as u32));
            self.do_alu2_op(op);
            let dr = ((op >> 12) & 0x7f) as u8;
            self.write_dst(bus, dr, mode, ry, disp, res);
        }
    }

    /// Class 4: ALU (1 slot) + two transfers.
    pub(crate) fn do_alu1_trans2<B: Mb86235Bus>(&mut self, bus: &mut B, op: u64) {
        let sda = ((op >> 38) & 3) as u8;
        let sdb = ((op >> 18) & 3) as u8;

        let ares = match sda {
            0 | 1 => self.get_transfer_reg(bus, ((op >> 33) & 0x1f) as u8),
            _ => {
                let a = self.decode_ea(
                    ((op >> 20) & 0xf) as u8,
                    ((op >> 30) & 7) as usize,
                    ((op >> 27) & 7) as usize,
                    ((op >> 24) & 7) as u32,
                    false,
                );
                self.read_abus(bus, a)
            }
        };
        let bres = match sdb {
            0 | 1 => self.get_transfer_reg(bus, (((op >> 13) & 0x1f) as u8) | 0x20),
            _ => {
                let b = self.decode_ea(
                    (op & 0xf) as u8,
                    ((op >> 10) & 7) as usize,
                    ((op >> 7) & 7) as usize,
                    ((op >> 4) & 7) as u32,
                    true,
                );
                self.read_bbus(b)
            }
        };

        self.do_alu1_op(op);

        match sda {
            0 | 2 => self.set_transfer_reg(bus, ((op >> 28) & 0x1f) as u8, ares),
            _ => {
                let a = self.decode_ea(
                    ((op >> 20) & 0xf) as u8,
                    ((op >> 30) & 7) as usize,
                    ((op >> 27) & 7) as usize,
                    ((op >> 24) & 7) as u32,
                    false,
                );
                self.write_abus(bus, a, ares);
            }
        }
        match sdb {
            0 | 2 => self.set_transfer_reg(bus, (((op >> 8) & 0x1f) as u8) | 0x20, bres),
            _ => {
                let b = self.decode_ea(
                    (op & 0xf) as u8,
                    ((op >> 10) & 7) as usize,
                    ((op >> 7) & 7) as usize,
                    ((op >> 4) & 7) as u32,
                    true,
                );
                self.write_bbus(b, bres);
            }
        }
    }

    /// Class 5: ALU (1 slot) + one transfer.
    pub(crate) fn do_alu1_trans1<B: Mb86235Bus>(&mut self, bus: &mut B, op: u64) {
        let mode = (op & 0xf) as u8;
        let ry = ((op >> 4) & 7) as usize;
        let disp = ((op >> 7) & 0x3fff) as u32;

        if op & (1 << 38) != 0 {
            if op & (1 << 37) != 0 {
                // ext -> int
                let addr = self.eb.wrapping_add(self.eo);
                let res = bus.data_read(addr);
                self.do_alu1_op(op);
                let dr = ((op >> 24) & 0x7f) as u8;
                self.write_dst(bus, dr, mode, ry, disp, res);
                self.eo = self
                    .eo
                    .wrapping_add(Self::eo_disp((op >> 31) as u32) as u32);
            } else {
                // int -> ext
                let sr = ((op >> 24) & 0x7f) as u8;
                let res = self.read_src(bus, sr, mode, 0, ry, disp, Some((op & 0xff_ffff) as u32));
                self.do_alu1_op(op);
                let addr = self.eb.wrapping_add(self.eo);
                bus.data_write(addr, res);
                self.eo = self
                    .eo
                    .wrapping_add(Self::eo_disp((op >> 31) as u32) as u32);
            }
        } else {
            let sr = ((op >> 31) & 0x7f) as u8;
            let res = self.read_src(bus, sr, mode, 0, ry, disp, Some((op & 0xff_ffff) as u32));
            self.do_alu1_op(op);
            let dr = ((op >> 24) & 0x7f) as u8;
            self.write_dst(bus, dr, mode, ry, disp, res);
        }
    }

    /// Class 7: a single transfer of a 32-bit immediate.
    pub(crate) fn do_trans1_imm<B: Mb86235Bus>(&mut self, bus: &mut B, op: u64) {
        let dr = ((op >> 19) & 0x7f) as u8;
        let imm = ((op >> 27) & 0xffff_ffff) as u32;
        let mode = (op & 0xf) as u8;
        let ry = ((op >> 4) & 7) as usize;
        let disp = ((op >> 7) & 0xfff) as u32;
        self.write_dst(bus, dr, mode, ry, disp, imm);
    }
}
