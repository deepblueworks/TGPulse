//! V60 operand access, the F12 two-operand format, and the opcode
//! implementations built on them.
//!
//! Operand size is `moddim`: 0 = byte, 1 = halfword, 2 = word. ReadAM yields a
//! zero-extended value; the register-direct and immediate modes are handled the
//! way the reference am1 does, on top of the am2 address decoder already ported.

use crate::bus::Bus;
use crate::cpu::{AP, FP, PC, PSW, SBR, SP, SYCW, TKCW, TR, V60};

/// How an F12 operand is taken: as a value to read, or as an address to
/// read-modify-write.
#[derive(Copy, Clone, PartialEq)]
enum Op {
    Value,
    Addr,
}

impl V60 {
    #[inline]
    fn sized(&self, v: u32) -> u32 {
        match self.moddim {
            0 => v & 0xff,
            1 => v & 0xffff,
            _ => v,
        }
    }
    fn mem_read<B: Bus>(&self, bus: &mut B, addr: u32) -> u32 {
        match self.moddim {
            0 => bus.read_u8(addr) as u32,
            1 => bus.read_u16(addr) as u32,
            _ => bus.read_u32(addr),
        }
    }
    fn mem_write<B: Bus>(&self, bus: &mut B, addr: u32, val: u32) {
        match self.moddim {
            0 => bus.write_u8(addr, val as u8),
            1 => bus.write_u16(addr, val as u16),
            _ => bus.write_u32(addr, val),
        }
    }

    /// ReadAM: decode the operand and return its value in `amout`. `amflag`
    /// stays meaningful for read-modify-write callers (register vs memory).
    fn read_am<B: Bus>(&mut self, bus: &mut B) -> u32 {
        let len = self.read_am_address(bus);
        if self.am_value {
            // immediate: amout already holds the value
        } else if self.amflag {
            self.amout = self.sized(self.reg[self.amout as usize]);
        } else {
            self.amout = self.mem_read(bus, self.amout);
        }
        len
    }

    /// WriteAM: store `val` to the operand decoded at `modadd`.
    fn write_am<B: Bus>(&mut self, bus: &mut B, val: u32) -> u32 {
        let len = self.read_am_address(bus);
        if self.amflag {
            let r = self.amout as usize;
            self.reg[r] = match self.moddim {
                0 => (self.reg[r] & 0xffff_ff00) | (val & 0xff),
                1 => (self.reg[r] & 0xffff_0000) | (val & 0xffff),
                _ => val,
            };
        } else {
            self.mem_write(bus, self.amout, val);
        }
        len
    }

    /// Loads the value at the operand just decoded by `read_am_address`
    /// (register-direct or memory), sized by the current `moddim`.
    fn load_decoded<B: Bus>(&mut self, bus: &mut B) -> u32 {
        if self.amflag {
            self.sized(self.reg[self.amout as usize])
        } else {
            self.mem_read(bus, self.amout)
        }
    }
    /// Stores `val` to the operand just decoded by `read_am_address`.
    fn store_decoded<B: Bus>(&mut self, bus: &mut B, val: u32) {
        if self.amflag {
            let r = self.amout as usize;
            self.reg[r] = match self.moddim {
                0 => (self.reg[r] & 0xffff_ff00) | (val & 0xff),
                1 => (self.reg[r] & 0xffff_0000) | (val & 0xffff),
                _ => val,
            };
        } else {
            self.mem_write(bus, self.amout, val);
        }
    }

    /// INC/DEC: a single operand at PC+1 is incremented or decremented by one,
    /// with full add/sub flag semantics. `modm` comes from the opcode's _0/_1.
    fn op_incdec<B: Bus>(&mut self, bus: &mut B, dim: u8, modm: bool, inc: bool) -> u32 {
        self.modm = modm;
        self.moddim = dim;
        self.modadd = self.reg[PC].wrapping_add(1);
        let len = self.read_am_address(bus);
        let cur = self.load_decoded(bus);
        let r = if inc {
            self.alu_add(cur, 1, 0, dim)
        } else {
            self.alu_sub(cur, 1, 0, dim)
        };
        self.store_decoded(bus, r);
        len + 1
    }

    /// Folds the CY/OV/S/Z flags into PSW and returns it.
    fn read_psw(&mut self) -> u32 {
        self.reg[PSW] = (self.reg[PSW] & 0xffff_fff0)
            | (self.z as u32)
            | ((self.s as u32) << 1)
            | ((self.ov as u32) << 2)
            | ((self.cy as u32) << 3);
        self.reg[PSW]
    }
    /// Saves the live SP into whichever stack pointer the current PSW selects:
    /// the interrupt stack (PSW.IS) or one of the four level stacks.
    fn save_stack(&mut self) {
        if self.reg[PSW] & 0x1000_0000 != 0 {
            self.reg[36] = self.reg[SP]; // ISP
        } else {
            self.reg[37 + ((self.reg[PSW] >> 24) & 3) as usize] = self.reg[SP];
        }
    }
    /// Loads SP from the stack pointer the current PSW selects.
    fn reload_stack(&mut self) {
        if self.reg[PSW] & 0x1000_0000 != 0 {
            self.reg[SP] = self.reg[36];
        } else {
            self.reg[SP] = self.reg[37 + ((self.reg[PSW] >> 24) & 3) as usize];
        }
    }

    /// Writes PSW, switching the active stack pointer when the interrupt-stack
    /// bit or (in user mode) the execution level changes. The reference v60WritePSW.
    fn write_psw(&mut self, v: u32) {
        let old = self.reg[PSW];
        let update = ((v ^ old) & 0x1000_0000 != 0)
            || (old & 0x1000_0000 == 0 && (v ^ old) & 0x0300_0000 != 0);
        if update {
            self.save_stack();
        }
        self.reg[PSW] = v;
        self.z = v & 1 != 0;
        self.s = v & 2 != 0;
        self.ov = v & 4 != 0;
        self.cy = v & 8 != 0;
        if update {
            self.reload_stack();
        }
    }

    /// Rewrites PSW for an exception/interrupt: drop to level 0, clear IE/TE/TP/
    /// AE/EM, set IS for interrupts and ASA, returning the old PSW to be pushed.
    /// The reference v60_update_psw_for_exception.
    fn update_psw_exception(&mut self, is_interrupt: bool, level: u32) -> u32 {
        let old = self.read_psw();
        let mut new = old;
        new &= !(3 << 24);
        new |= level << 24;
        new &= !(1 << 18); // IE
        new &= !(1 << 16); // TE
        new &= !(1 << 27); // TP
        new &= !(1 << 17); // AE
        new &= !(1 << 29); // EM
        if is_interrupt {
            new |= 1 << 28; // IS
        }
        new |= 1 << 31; // ASA
        self.write_psw(new);
        old
    }

    /// Reads an interrupt/exception vector from the SBR table.
    fn get_intvect<B: Bus>(&mut self, bus: &mut B, n: u32) -> u32 {
        bus.read_u32((self.reg[SBR] & !0xfff).wrapping_add(n * 4))
    }

    /// Enters an interrupt/exception: push old PSW and PC (onto the interrupt
    /// stack, since update_psw_exception sets IS), then jump to the vector.
    fn do_irq<B: Bus>(&mut self, bus: &mut B, vector: u32) {
        self.irq_taken += 1;
        let old = self.update_psw_exception(true, 0);
        self.reg[SP] = self.reg[SP].wrapping_sub(4);
        let sp = self.reg[SP];
        bus.write_u32(sp, old);
        self.reg[SP] = self.reg[SP].wrapping_sub(4);
        let sp = self.reg[SP];
        let pc = self.reg[PC];
        bus.write_u32(sp, pc);
        self.reg[PC] = self.get_intvect(bus, vector);
    }

    fn op_halt(&mut self) -> u32 {
        self.halted = true;
        1
    }

    /// Checks the IRQ line: if asserted and PSW.IE is set, acknowledge (the
    /// device returns the level) and take the interrupt at vector level+0x40.
    pub(crate) fn try_irq<B: Bus>(&mut self, bus: &mut B) -> bool {
        // An in-ISR acknowledge clears the controller's status while our latched
        // line is still asserted; reflect that immediately so we don't re-take
        // the same level after `reti` (else the vblank ISR runs twice a frame).
        if self.irq_line && bus.irq_active() == Some(false) {
            self.irq_line = false;
        }
        if !self.irq_line || self.reg[PSW] & (1 << 18) == 0 {
            return false;
        }
        let vector = self.irq_vector as u32 + 0x40;
        self.halted = false;
        self.do_irq(bus, vector);
        true
    }

    /// RETIU/RETIS: pop PC and PSW from the stack, releasing `operand` bytes of
    /// frame. Restoring PSW switches back to the interrupted stack.
    fn op_reti<B: Bus>(&mut self, bus: &mut B, modm: bool) -> u32 {
        self.modm = modm;
        self.moddim = 1;
        self.modadd = self.reg[PC].wrapping_add(1);
        self.read_am(bus);
        let adjust = self.amout;
        self.reg[PC] = self.pop32(bus);
        let new_psw = self.pop32(bus);
        self.reg[SP] = self.reg[SP].wrapping_add(adjust);
        self.write_psw(new_psw);
        0
    }

    /// One operand of an F12 decode. Returns (op, flag, length).
    fn f12_operand<B: Bus>(
        &mut self,
        bus: &mut B,
        kind: Op,
        dim: u8,
        modm: bool,
        at: u32,
    ) -> (u32, bool, u32) {
        self.moddim = dim;
        self.modm = modm;
        self.modadd = at;
        let len = match kind {
            Op::Value => self.read_am(bus),
            Op::Addr => self.read_am_address(bus),
        };
        (self.amout, self.amflag, len)
    }

    /// F12DecodeOperands: the general two-operand decoder, handling F1 and both
    /// F2 short forms. Sets op1/flag1/op2/flag2/amlength1/amlength2.
    fn f12_decode_operands<B: Bus>(&mut self, bus: &mut B, k1: Op, dim1: u8, k2: Op, dim2: u8) {
        let f = bus.read_u8(self.reg[PC].wrapping_add(1));
        self.instflags = f;
        let pc2 = self.reg[PC].wrapping_add(2);
        if f & 0x80 != 0 {
            let (o1, fl1, l1) = self.f12_operand(bus, k1, dim1, f & 0x40 != 0, pc2);
            self.op1 = o1;
            self.flag1 = fl1;
            self.amlength1 = l1;
            let (o2, fl2, l2) =
                self.f12_operand(bus, k2, dim2, f & 0x20 != 0, pc2.wrapping_add(l1));
            self.op2 = o2;
            self.flag2 = fl2;
            self.amlength2 = l2;
        } else if f & 0x20 != 0 {
            // op2 is the short-form register; op1 decoded at PC+2.
            if k2 == Op::Addr {
                self.op2 = (f & 0x1f) as u32;
                self.flag2 = true;
            } else {
                self.moddim = dim2;
                self.op2 = self.sized(self.reg[(f & 0x1f) as usize]);
                self.flag2 = false;
            }
            self.amlength2 = 0;
            let (o1, fl1, l1) = self.f12_operand(bus, k1, dim1, f & 0x40 != 0, pc2);
            self.op1 = o1;
            self.flag1 = fl1;
            self.amlength1 = l1;
        } else {
            // op1 is the short-form register; op2 decoded after it.
            if k1 == Op::Addr {
                self.op1 = (f & 0x1f) as u32;
                self.flag1 = true;
            } else {
                self.moddim = dim1;
                self.op1 = self.sized(self.reg[(f & 0x1f) as usize]);
                self.flag1 = false;
            }
            self.amlength1 = 0;
            let (o2, fl2, l2) = self.f12_operand(bus, k2, dim2, f & 0x40 != 0, pc2);
            self.op2 = o2;
            self.flag2 = fl2;
            self.amlength2 = l2;
        }
    }

    /// F12DecodeFirstOperand: decode operand 1 as a value (`read_value`) or as
    /// an address, honouring the F1/F2 short forms. Sets op1/flag1/amlength1.
    fn f12_first<B: Bus>(&mut self, bus: &mut B, read_value: bool, dim1: u8) {
        self.instflags = bus.read_u8(self.reg[PC].wrapping_add(1));
        let f = self.instflags;
        if f & 0x80 != 0 || f & 0x20 != 0 {
            // F1, or F2 with the D flag: decode a full operand at PC+2.
            self.moddim = dim1;
            self.modm = f & 0x40 != 0;
            self.modadd = self.reg[PC].wrapping_add(2);
            self.amlength1 = if read_value {
                self.read_am(bus)
            } else {
                self.read_am_address(bus)
            };
            self.op1 = self.amout;
            self.flag1 = self.amflag;
        } else {
            // F2 short form: operand 1 is the register in the low 5 bits.
            if read_value {
                self.moddim = dim1;
                self.op1 = self.sized(self.reg[(f & 0x1f) as usize]);
                self.flag1 = false;
            } else {
                self.flag1 = true;
                self.op1 = (f & 0x1f) as u32;
            }
            self.amlength1 = 0;
        }
    }

    /// F12WriteSecondOperand: write `val` to operand 2.
    fn f12_write_second<B: Bus>(&mut self, bus: &mut B, dim2: u8, val: u32) {
        self.moddim = dim2;
        let f = self.instflags;
        if f & 0x80 != 0 {
            self.modm = f & 0x20 != 0;
            self.modadd = self.reg[PC].wrapping_add(2 + self.amlength1);
            self.amlength2 = self.write_am(bus, val);
        } else if f & 0x20 != 0 {
            let r = (f & 0x1f) as usize;
            self.reg[r] = match dim2 {
                0 => (self.reg[r] & 0xffff_ff00) | (val & 0xff),
                1 => (self.reg[r] & 0xffff_0000) | (val & 0xffff),
                _ => val,
            };
            self.amlength2 = 0;
        } else {
            self.modm = f & 0x40 != 0;
            self.modadd = self.reg[PC].wrapping_add(2);
            self.amlength2 = self.write_am(bus, val);
        }
    }

    /// Load a decoded operand: register-direct (`flag`) reads the register
    /// sized by `dim`, otherwise reads memory at `op`.
    fn load_operand<B: Bus>(&mut self, bus: &mut B, op: u32, flag: bool, dim: u8) -> u32 {
        self.moddim = dim;
        if flag {
            self.sized(self.reg[op as usize])
        } else {
            self.mem_read(bus, op)
        }
    }
    fn store_operand<B: Bus>(&mut self, bus: &mut B, op: u32, flag: bool, dim: u8, val: u32) {
        self.moddim = dim;
        if flag {
            let r = op as usize;
            self.reg[r] = match dim {
                0 => (self.reg[r] & 0xffff_ff00) | (val & 0xff),
                1 => (self.reg[r] & 0xffff_0000) | (val & 0xffff),
                _ => val,
            };
        } else {
            self.mem_write(bus, op, val);
        }
    }
    fn load_op2<B: Bus>(&mut self, bus: &mut B, dim: u8) -> u32 {
        let (op, flag) = (self.op2, self.flag2);
        self.load_operand(bus, op, flag, dim)
    }
    fn store_op2<B: Bus>(&mut self, bus: &mut B, dim: u8, val: u32) {
        let (op, flag) = (self.op2, self.flag2);
        self.store_operand(bus, op, flag, dim, val)
    }

    #[inline]
    fn f12_len(&self) -> u32 {
        self.amlength1 + self.amlength2 + 2
    }

    /// (mask, sign-bit, carry-bit) for a size: byte/halfword/word.
    #[inline]
    fn dim_params(dim: u8) -> (u32, u32, u64) {
        match dim {
            0 => (0xff, 0x80, 0x100),
            1 => (0xffff, 0x8000, 0x1_0000),
            _ => (0xffff_ffff, 0x8000_0000, 0x1_0000_0000),
        }
    }

    /// Sets Z (on the truncated value) and S (on the size's sign bit), leaving
    /// CY/OV alone. The reference SetSZPF_*.
    #[inline]
    fn set_szl(&mut self, x: u32, dim: u8) {
        let (mask, msb, _) = Self::dim_params(dim);
        self.z = (x & mask) == 0;
        self.s = (x & msb) != 0;
    }

    /// dst + src + carry, with V60 flag semantics (SetCF/SetOF_Add/SetSZPF).
    fn alu_add(&mut self, dst: u32, src: u32, c: u32, dim: u8) -> u32 {
        let (mask, msb, cbit) = Self::dim_params(dim);
        let res = (dst & mask) as u64 + (src & mask) as u64 + c as u64;
        self.cy = res & cbit != 0;
        let r = res as u32;
        self.ov = ((r ^ src) & (r ^ dst) & msb) != 0;
        self.set_szl(r, dim);
        r & mask
    }

    /// dst - src - carry, with V60 flag semantics (borrow into CY, OF_Sub).
    fn alu_sub(&mut self, dst: u32, src: u32, c: u32, dim: u8) -> u32 {
        let (mask, msb, cbit) = Self::dim_params(dim);
        let res = ((dst & mask) as u64)
            .wrapping_sub((src & mask) as u64)
            .wrapping_sub(c as u64);
        self.cy = res & cbit != 0;
        let r = res as u32;
        self.ov = ((dst ^ src) & (dst ^ r) & msb) != 0;
        self.set_szl(r, dim);
        r & mask
    }

    /// Bitwise op: clears OV, sets S/Z, leaves CY.
    fn alu_logic(&mut self, r: u32, dim: u8) -> u32 {
        let (mask, _, _) = Self::dim_params(dim);
        self.ov = false;
        self.set_szl(r, dim);
        r & mask
    }

    /// Multiply (signed or unsigned). Low half is the result; OV flags a
    /// non-zero high half. CY is untouched.
    fn alu_mul(&mut self, dst: u32, src: u32, dim: u8, signed: bool) -> u32 {
        match dim {
            0 => {
                let t = if signed {
                    ((dst as i8 as i32) * (src as i8 as i32)) as u32
                } else {
                    (dst & 0xff) * (src & 0xff)
                };
                self.z = t & 0xff == 0;
                self.s = t & 0x80 != 0;
                self.ov = (t >> 8) != 0;
                t & 0xff
            }
            1 => {
                let t = if signed {
                    ((dst as i16 as i32) * (src as i16 as i32)) as u32
                } else {
                    (dst & 0xffff) * (src & 0xffff)
                };
                self.z = t & 0xffff == 0;
                self.s = t & 0x8000 != 0;
                self.ov = (t >> 16) != 0;
                t & 0xffff
            }
            _ => {
                let t: u64 = if signed {
                    ((dst as i32 as i64) * (src as i32 as i64)) as u64
                } else {
                    (dst as u64) * (src as u64)
                };
                let r = t as u32;
                self.z = r == 0;
                self.s = r & 0x8000_0000 != 0;
                self.ov = (t >> 32) != 0;
                r
            }
        }
    }

    /// Divide (signed or unsigned). Sets OV on the sole overflowing signed case
    /// (MIN / -1) or never for unsigned; a zero divisor leaves dst unchanged.
    fn alu_div(&mut self, dst: u32, src: u32, dim: u8, signed: bool) -> u32 {
        let (mask, msb, _) = Self::dim_params(dim);
        let d = dst & mask;
        let s = src & mask;
        let mut r = d;
        if !signed {
            self.ov = false;
            if let Some(q) = d.checked_div(s) {
                r = q;
            }
        } else {
            self.ov = d == msb && s == mask; // MIN / -1
            if s != 0 && !self.ov {
                r = match dim {
                    0 => ((d as i8) / (s as i8)) as u32 & 0xff,
                    1 => ((d as i16) / (s as i16)) as u32 & 0xffff,
                    _ => ((d as i32) / (s as i32)) as u32,
                };
            }
        }
        self.z = (r & mask) == 0;
        self.s = (r & msb) != 0;
        r & mask
    }

    /// Load the low/high words of an X-format register or memory pair.
    fn load_pair<B: Bus>(&mut self, bus: &mut B) -> (u32, u32) {
        if self.flag2 {
            let register = (self.op2 & 0x1f) as usize;
            (self.reg[register], self.reg[register + 1])
        } else {
            (
                bus.read_u32(self.op2),
                bus.read_u32(self.op2.wrapping_add(4)),
            )
        }
    }

    /// Store the low/high words of an X-format register or memory pair.
    fn store_pair<B: Bus>(&mut self, bus: &mut B, low: u32, high: u32) {
        if self.flag2 {
            let register = (self.op2 & 0x1f) as usize;
            self.reg[register] = low;
            self.reg[register + 1] = high;
        } else {
            bus.write_u32(self.op2, low);
            bus.write_u32(self.op2.wrapping_add(4), high);
        }
    }

    /// MULX/MULUX: multiply the destination low word by op1 and write the
    /// complete 64-bit product to the destination pair. CY and OV are unchanged.
    fn op_mulx<B: Bus>(&mut self, bus: &mut B, signed: bool) -> u32 {
        self.f12_decode_operands(bus, Op::Value, 2, Op::Addr, 3);
        let (destination, _) = self.load_pair(bus);

        let product = if signed {
            (destination as i32 as i64 * self.op1 as i32 as i64) as u64
        } else {
            destination as u64 * self.op1 as u64
        };

        let low = product as u32;
        let high = (product >> 32) as u32;

        self.s = high & 0x8000_0000 != 0;
        self.z = low == 0 && high == 0;
        self.store_pair(bus, low, high);
        self.f12_len()
    }

    /// DIVX/DIVUX: divide the 64-bit destination pair by op1, storing the
    /// quotient in the low word and remainder in the high word. As in the reference,
    /// CY and OV are unchanged. Invalid host divisions remain traceable as an
    /// unimplemented instruction rather than panicking the emulator.
    fn op_divx<B: Bus>(&mut self, bus: &mut B, signed: bool) -> Option<u32> {
        self.f12_decode_operands(bus, Op::Value, 2, Op::Addr, 3);
        let (low, high) = self.load_pair(bus);

        let (quotient, remainder) = if signed {
            let dividend = (((high as u64) << 32) | low as u64) as i64;
            let divisor = self.op1 as i32 as i64;

            if divisor == 0 || (dividend == i64::MIN && divisor == -1) {
                return None;
            }

            // The reference helper first converts the full quotient to int32_t, then
            // calculates the remainder using that truncated quotient.
            let quotient = (dividend / divisor) as i32;
            let remainder = (dividend - divisor * quotient as i64) as i32;
            (quotient as u32, remainder as u32)
        } else {
            let dividend = ((high as u64) << 32) | low as u64;
            let divisor = self.op1 as u64;

            if divisor == 0 {
                return None;
            }

            let quotient = (dividend / divisor) as u32;
            let remainder = (dividend - divisor * quotient as u64) as u32;
            (quotient, remainder)
        };

        self.s = quotient & 0x8000_0000 != 0;
        self.z = quotient == 0;
        self.store_pair(bus, quotient, remainder);
        Some(self.f12_len())
    }

    /// The F12 read-modify-write skeleton for the arithmetic/logic ops: op1 is a
    /// value of `dim1`, op2 an address of `dim2` that is loaded, transformed by
    /// `f`, and stored back.
    fn alu_rmw<B: Bus, F>(&mut self, bus: &mut B, dim1: u8, dim2: u8, f: F) -> u32
    where
        F: FnOnce(&mut Self, u32, u32) -> u32,
    {
        self.f12_decode_operands(bus, Op::Value, dim1, Op::Addr, dim2);
        let dst = self.load_op2(bus, dim2);
        let src = self.op1;
        let r = f(self, dst, src);
        self.store_op2(bus, dim2, r);
        self.f12_len()
    }

    /// CMP: op2 - op1 for flags only, both read as values, nothing stored.
    fn op_cmp<B: Bus>(&mut self, bus: &mut B, dim: u8) -> u32 {
        self.f12_decode_operands(bus, Op::Value, dim, Op::Value, dim);
        let (a, b) = (self.op2, self.op1);
        self.alu_sub(a, b, 0, dim);
        self.f12_len()
    }

    /// SHL (logical shift): op1 is a signed byte count, op2 the word/half/byte
    /// shifted. Left sets CY to the last bit out and clears OV; right shifts
    /// zero in and set CY to the last bit out.
    fn op_shl<B: Bus>(&mut self, bus: &mut B, dim: u8) -> u32 {
        self.alu_rmw(bus, 0, dim, |cpu, dst, src| {
            let (mask, _, _) = Self::dim_params(dim);
            let bits = match dim {
                0 => 8u32,
                1 => 16,
                _ => 32,
            };
            let count = (src & 0xff) as i8 as i32;
            let d = dst & mask;
            let r;
            if count > 0 {
                cpu.ov = false;
                let c = count as u32;
                cpu.cy = c < 64 && ((d as u64) << c) & (1u64 << bits) != 0;
                r = if c >= bits { 0 } else { d << c };
            } else if count == 0 {
                cpu.cy = false;
                cpu.ov = false;
                r = d;
            } else {
                let c = (-count) as u32;
                cpu.cy = c >= 1 && ((d >> (c - 1)) & 1) != 0;
                cpu.ov = false;
                r = if c >= bits { 0 } else { d >> c };
            }
            cpu.set_szl(r, dim);
            r & mask
        })
    }

    /// SHA (arithmetic shift): like SHL but the right shift is sign-preserving
    /// and the left shift computes overflow from the bits shifted past the sign.
    fn op_sha<B: Bus>(&mut self, bus: &mut B, dim: u8) -> u32 {
        self.alu_rmw(bus, 0, dim, |cpu, dst, src| {
            let (mask, msb, _) = Self::dim_params(dim);
            let bits: u32 = match dim {
                0 => 8,
                1 => 16,
                _ => 32,
            };
            let count = (src & 0xff) as i8 as i32;
            let d = dst & mask;
            let r;
            if count == 0 {
                cpu.cy = false;
                cpu.ov = false;
                r = d;
            } else if count > 0 {
                let c = count as u32;
                // OV: set if any bit shifted out of the top differs from the
                // resulting sign.
                let tmp = if c >= bits {
                    mask
                } else {
                    ((1u64 << c) - 1) as u32
                } << (bits - c.min(bits));
                cpu.ov = if (d >> (bits - 1)) & 1 != 0 {
                    (d & tmp) != tmp
                } else {
                    (d & tmp) != 0
                };
                cpu.cy = c <= bits && (d >> (bits - c)) & 1 != 0;
                r = if c >= bits { 0 } else { (d << c) & mask };
            } else {
                let c = (-count) as u32;
                cpu.ov = false;
                cpu.cy = (d >> (c - 1).min(bits - 1)) & 1 != 0;
                let sign = d & msb != 0;
                r = if c >= bits {
                    if sign {
                        mask
                    } else {
                        0
                    }
                } else {
                    let sh = (d >> c) & (mask >> c);
                    if sign {
                        sh | (mask & !(mask >> c))
                    } else {
                        sh
                    }
                };
            }
            cpu.set_szl(r, dim);
            r & mask
        })
    }

    /// ROT (rotate): op1 signed count, positive left / negative right; CY takes
    /// the bit rotated into the far end.
    fn op_rot<B: Bus>(&mut self, bus: &mut B, dim: u8) -> u32 {
        self.alu_rmw(bus, 0, dim, |cpu, dst, src| {
            let (mask, msb, _) = Self::dim_params(dim);
            let bits: u32 = match dim {
                0 => 8,
                1 => 16,
                _ => 32,
            };
            let count = (src & 0xff) as i8 as i32;
            let mut v = dst & mask;
            if count > 0 {
                for _ in 0..count {
                    v = ((v << 1) | ((v & msb) >> (bits - 1))) & mask;
                }
                cpu.cy = v & 1 != 0;
            } else if count < 0 {
                for _ in 0..(-count) {
                    v = ((v >> 1) | ((v & 1) << (bits - 1))) & mask;
                }
                cpu.cy = v & msb != 0;
            } else {
                cpu.cy = false;
            }
            cpu.ov = false;
            cpu.s = v & msb != 0;
            cpu.z = v == 0;
            v
        })
    }

    /// ROTC (rotate through carry): the carry participates as an extra bit.
    fn op_rotc<B: Bus>(&mut self, bus: &mut B, dim: u8) -> u32 {
        self.alu_rmw(bus, 0, dim, |cpu, dst, src| {
            let (mask, msb, _) = Self::dim_params(dim);
            let bits: u32 = match dim {
                0 => 8,
                1 => 16,
                _ => 32,
            };
            let count = (src & 0xff) as i8 as i32;
            let mut v = dst & mask;
            if count > 0 {
                for _ in 0..count {
                    let cy = cpu.cy as u32;
                    cpu.cy = (v & msb) != 0;
                    v = ((v << 1) | cy) & mask;
                }
            } else if count < 0 {
                for _ in 0..(-count) {
                    let cy = cpu.cy as u32;
                    cpu.cy = (v & 1) != 0;
                    v = ((v >> 1) | (cy << (bits - 1))) & mask;
                }
            } else {
                cpu.cy = false;
            }
            cpu.ov = false;
            cpu.s = v & msb != 0;
            cpu.z = v == 0;
            v
        })
    }

    /// Bit ops SET1/CLR1/NOT1: op1 selects the bit of the word at op2's address;
    /// CY takes its old value, Z its complement, then the bit is updated.
    fn op_bit<B: Bus>(&mut self, bus: &mut B, mode: u8) -> u32 {
        self.alu_rmw(bus, 2, 2, |cpu, dst, src| {
            let bit = 1u32 << (src & 0x1f);
            cpu.cy = dst & bit != 0;
            cpu.z = !cpu.cy;
            match mode {
                0 => dst | bit,  // SET1
                1 => dst & !bit, // CLR1
                _ => {
                    if cpu.cy {
                        dst & !bit
                    } else {
                        dst | bit
                    }
                } // NOT1
            }
        })
    }

    /// TEST1: CY = bit `op1` of value `op2`; Z = !CY. Nothing is stored.
    fn op_test1<B: Bus>(&mut self, bus: &mut B) -> u32 {
        self.f12_decode_operands(bus, Op::Value, 2, Op::Value, 2);
        let bit = 1u32 << (self.op1 & 0x1f);
        self.cy = self.op2 & bit != 0;
        self.z = !self.cy;
        self.f12_len()
    }

    /// Move effective address: op1's address (computed with `dim`) is written
    /// to op2 as a word.
    fn op_movea<B: Bus>(&mut self, bus: &mut B, dim: u8) -> u32 {
        self.f12_first(bus, false, dim);
        let v = self.op1;
        self.f12_write_second(bus, 2, v);
        self.f12_len()
    }

    /// Reads the sub-opcode byte (PC+1) into `subop` and returns its low 5
    /// bits, the dispatch key for the 0x58-0x5D group opcodes.
    fn subop_group<B: Bus>(&mut self, bus: &mut B) -> u8 {
        self.subop = bus.read_u8(self.reg[PC].wrapping_add(1));
        self.subop & 0x1f
    }

    /// A move: read op1 as a value of `dim`, write it to op2.
    fn op_mov<B: Bus>(&mut self, bus: &mut B, dim: u8) -> u32 {
        self.f12_first(bus, true, dim);
        let v = self.op1;
        self.f12_write_second(bus, dim, v);
        self.f12_len()
    }

    /// MOVD (0x3F): move double -- a 64-bit transfer between a register pair
    /// and memory (or another register pair). Operands decode with dim 3 so
    /// indexed modes scale x8; the value itself is moved as two dwords.
    /// opMOVD.
    fn op_movd<B: Bus>(&mut self, bus: &mut B) -> u32 {
        self.f12_decode_operands(bus, Op::Addr, 3, Op::Addr, 3);
        let d = if self.flag1 {
            let r = (self.op1 & 0x1f) as usize;
            u64::from(self.reg[r]) | (u64::from(self.reg[r + 1]) << 32)
        } else {
            self.moddim = 2;
            u64::from(self.mem_read(bus, self.op1))
                | (u64::from(self.mem_read(bus, self.op1.wrapping_add(4))) << 32)
        };
        if self.flag2 {
            let r = (self.op2 & 0x1f) as usize;
            self.reg[r] = d as u32;
            self.reg[r + 1] = (d >> 32) as u32;
        } else {
            self.moddim = 2;
            self.mem_write(bus, self.op2, d as u32);
            self.mem_write(bus, self.op2.wrapping_add(4), (d >> 32) as u32);
        }
        self.f12_len()
    }

    /// RVBIT (0x08): reverse the bits of op1 (byte) into op2.
    fn op_rvbit<B: Bus>(&mut self, bus: &mut B) -> u32 {
        self.f12_first(bus, true, 0);
        let v = u32::from((self.op1 as u8).reverse_bits());
        self.f12_write_second(bus, 0, v);
        self.f12_len()
    }

    /// CLRTLBA (0x10): no TLB is modeled; one byte long.
    fn op_clrtlba(&mut self) -> u32 {
        1
    }

    /// CHKAR/CHKAW/CHKAE (0x4D/0x4E/0x4F): array bounds check. Without the MMU
    /// permission model the reference just sets Z and clears CY and S.
    fn op_chka<B: Bus>(&mut self, bus: &mut B) -> u32 {
        self.f12_decode_operands(bus, Op::Value, 0, Op::Value, 0);
        self.z = true;
        self.cy = false;
        self.s = false;
        self.f12_len()
    }

    /// CHLVL (0x4B): change execution level to op1 (0..3), pushing op2 on the
    /// new level's stack. An op1 above 3 is unreachable in real game code.
    fn op_chlvl<B: Bus>(&mut self, bus: &mut B) -> u32 {
        self.f12_decode_operands(bus, Op::Value, 0, Op::Value, 0);
        let _old_psw = self.update_psw_exception(false, self.op1);
        self.reg[SP] = self.reg[SP].wrapping_sub(4);
        let sp = self.reg[SP];
        let v = self.op2;
        bus.write_u32(sp, v);
        self.f12_len()
    }

    /// BRK (0xC8): It is skipped, its trap body is
    /// commented out.
    fn op_brk(&mut self) -> u32 {
        1
    }

    /// BRKV (0xC9): break with vector -- enter exception vector 21, pushing
    /// the exception frame the exception frame.
    fn op_brkv<B: Bus>(&mut self, bus: &mut B) -> u32 {
        let old = self.update_psw_exception(false, 0);
        self.reg[SP] = self.reg[SP].wrapping_sub(4);
        let sp = self.reg[SP];
        let pc = self.reg[PC];
        bus.write_u32(sp, pc);
        self.reg[SP] = self.reg[SP].wrapping_sub(4);
        let sp = self.reg[SP];
        bus.write_u32(sp, (0x1501 << 16) | 4);
        self.reg[SP] = self.reg[SP].wrapping_sub(4);
        let sp = self.reg[SP];
        bus.write_u32(sp, old);
        self.reg[SP] = self.reg[SP].wrapping_sub(4);
        let sp = self.reg[SP];
        bus.write_u32(sp, pc.wrapping_add(1));
        self.reg[PC] = self.get_intvect(bus, 21);
        0
    }

    /// TRAPFL (0xCB): FPU trap-enable check. The taken case is a decimal/FPU
    /// exception, so no Model 1 game relies on it.
    fn op_trapfl(&mut self) -> u32 {
        if (self.reg[TKCW] & 0x1f0) & ((self.read_psw() & 0x1f00) >> 4) != 0 {
            log::debug!(target: "v60", "TRAPFL taken at {:06X}", self.reg[PC]);
        }
        1
    }

    /// CLRTLB (0xFE/0xFF): decode the operand; no TLB is modeled. opCLRTLB.
    fn op_clrtlb<B: Bus>(&mut self, bus: &mut B) -> u32 {
        self.modadd = self.reg[PC].wrapping_add(1);
        self.moddim = 2;
        let len = self.read_am(bus);
        len + 1
    }

    /// TRAP (0xF8/0xF9): conditional software trap into vectors 48-63. The
    /// high nibble of the operand selects the flag condition, mirroring the
    /// branch conditions; the low nibble is the vector offset.
    fn op_trap<B: Bus>(&mut self, bus: &mut B) -> u32 {
        self.modadd = self.reg[PC].wrapping_add(1);
        self.moddim = 0;
        let len = self.read_am(bus);
        let vector = self.amout;
        let taken = match (vector >> 4) & 0xf {
            0 => self.ov,
            1 => !self.ov,
            2 => self.cy,
            3 => !self.cy,
            4 => self.z,
            5 => !self.z,
            6 => self.cy || self.z,
            7 => !(self.cy || self.z),
            8 => self.s,
            9 => !self.s,
            10 => true,
            11 => false,
            12 => self.s != self.ov,
            13 => self.s == self.ov,
            14 => (self.s != self.ov) || self.z,
            _ => !((self.s != self.ov) || self.z),
        };
        if !taken {
            return len + 1;
        }
        let old = self.update_psw_exception(false, 0);
        self.reg[SP] = self.reg[SP].wrapping_sub(4);
        let sp = self.reg[SP];
        bus.write_u32(sp, ((0x3000 + 0x100 * (vector & 0xf)) << 16) | 4);
        self.reg[SP] = self.reg[SP].wrapping_sub(4);
        let sp = self.reg[SP];
        bus.write_u32(sp, old);
        self.reg[SP] = self.reg[SP].wrapping_sub(4);
        let sp = self.reg[SP];
        bus.write_u32(sp, self.reg[PC].wrapping_add(len + 1));
        self.reg[PC] = self.get_intvect(bus, 48 + (vector & 0xf));
        0
    }

    /// STTASK (0xFC/0xFD): store task state -- TKCW, the enabled level stack
    /// pointers, and the registers selected by the operand mask, at TR.
    /// opSTTASK.
    fn op_sttask<B: Bus>(&mut self, bus: &mut B) -> u32 {
        self.modadd = self.reg[PC].wrapping_add(1);
        self.moddim = 2;
        let len = self.read_am(bus);
        let mask = self.amout;
        let mut adr = self.reg[TR];

        let psw = self.read_psw() | 0x1000_0000;
        self.write_psw(psw);
        self.save_stack();

        let tkcw = self.reg[TKCW];
        bus.write_u32(adr, tkcw);
        adr = adr.wrapping_add(4);
        let sycw = self.reg[SYCW];
        for bit in 0..4 {
            if sycw & (0x100 << bit) != 0 {
                let v = self.reg[37 + bit];
                bus.write_u32(adr, v);
                adr = adr.wrapping_add(4);
            }
        }
        // 31 registers supported, _not_ 32
        for i in 0..31 {
            if mask & (1 << i) != 0 {
                let v = self.reg[i];
                bus.write_u32(adr, v);
                adr = adr.wrapping_add(4);
            }
        }
        len + 1
    }

    /// LDTASK (0x01): load task state -- TKCW and the enabled level stack
    /// pointers from TR, then the registers selected by the op1 mask.
    /// opLDTASK.
    fn op_ldtask<B: Bus>(&mut self, bus: &mut B) -> u32 {
        self.f12_decode_operands(bus, Op::Addr, 2, Op::Value, 2);

        let psw = self.read_psw() & 0xefff_ffff;
        self.write_psw(psw);

        self.reg[TR] = self.op2;
        let mut adr = self.op2;
        let tkcw = bus.read_u32(adr);
        self.reg[TKCW] = tkcw;
        adr = adr.wrapping_add(4);
        let sycw = self.reg[SYCW];
        for bit in 0..4 {
            if sycw & (0x100 << bit) != 0 {
                self.reg[37 + bit] = bus.read_u32(adr);
                adr = adr.wrapping_add(4);
            }
        }
        self.reload_stack();

        // 31 registers supported, _not_ 32
        for i in 0..31 {
            if self.op1 & (1 << i) != 0 {
                self.reg[i] = bus.read_u32(adr);
                adr = adr.wrapping_add(4);
            }
        }
        self.f12_len()
    }

    /// Bitwise complement. The reference clears OV, updates S/Z from the truncated
    /// result, and leaves CY unchanged.
    fn op_not<B: Bus>(&mut self, bus: &mut B, dim: u8) -> u32 {
        self.f12_first(bus, true, dim);
        let (mask, _, _) = Self::dim_params(dim);
        let result = !self.op1 & mask;
        self.ov = false;
        self.set_szl(result, dim);
        self.f12_write_second(bus, dim, result);
        self.f12_len()
    }

    /// Two's-complement negation. The reference calculates zero minus the source with
    /// subtraction flags, then defines CY as result != 0.
    fn op_neg<B: Bus>(&mut self, bus: &mut B, dim: u8) -> u32 {
        self.f12_first(bus, true, dim);
        let result = self.alu_sub(0, self.op1, 0, dim);
        self.cy = result != 0;
        self.f12_write_second(bus, dim, result);
        self.f12_len()
    }

    /// A transforming move: read op1 (`dim1`), map it through `f`, write the
    /// result to op2 (`dim2`). Covers the sign/zero-extend and truncate moves.
    fn op_mov_xf<B: Bus, F>(&mut self, bus: &mut B, dim1: u8, dim2: u8, f: F) -> u32
    where
        F: FnOnce(&mut Self, u32) -> u32,
    {
        self.f12_first(bus, true, dim1);
        let v = f(self, self.op1);
        self.f12_write_second(bus, dim2, v);
        self.f12_len()
    }

    /// MOVT: truncate op1 to `dim2`, setting OV when the discarded high bits were
    /// not just sign extension of the kept value.
    fn movt<B: Bus>(&mut self, bus: &mut B, dim1: u8, dim2: u8) -> u32 {
        self.op_mov_xf(bus, dim1, dim2, |cpu, v| {
            let (mask, msb, _) = Self::dim_params(dim2);
            let low = v & mask;
            let sign = low & msb != 0;
            let high = v & !mask;
            let ok = (sign && high == !mask) || (!sign && high == 0);
            cpu.ov = !ok;
            low
        })
    }

    /// LDPR: load a privileged register (index op2, 0..28 -> reg[op2+36]) from
    /// op1. The 0xf4 immediate form is taken by value, register forms by content.
    fn op_ldpr<B: Bus>(&mut self, bus: &mut B) -> u32 {
        self.f12_decode_operands(bus, Op::Addr, 2, Op::Value, 2);
        let pr = self.op2;
        if pr <= 28 {
            let b1 = bus.read_u8(self.reg[PC].wrapping_add(1));
            let b2 = bus.read_u8(self.reg[PC].wrapping_add(2));
            let v = if self.flag1 && !((b1 & 0x80 != 0) && b2 == 0xf4) {
                self.reg[self.op1 as usize]
            } else {
                self.op1
            };
            self.reg[(pr + 36) as usize] = v;
        }
        self.f12_len()
    }

    /// STPR: store a privileged register (index op1) into op2.
    fn op_stpr<B: Bus>(&mut self, bus: &mut B) -> u32 {
        self.f12_first(bus, true, 2);
        let pr = self.op1;
        let v = if pr <= 28 {
            self.reg[(pr + 36) as usize]
        } else {
            0
        };
        self.f12_write_second(bus, 2, v);
        self.f12_len()
    }

    /// UPDPSW: PSW = (PSW & ~mask) | (bits & mask). Both operands are values;
    /// op1 = bits, op2 = mask.
    fn op_updpsw<B: Bus>(&mut self, bus: &mut B, mask: u32) -> u32 {
        self.f12_decode_operands(bus, Op::Value, 2, Op::Value, 2);
        let m = self.op2 & mask;
        let bits = self.op1 & mask;
        let v = (self.read_psw() & !m) | (bits & m);
        self.write_psw(v);
        self.f12_len()
    }

    // --- String / block instructions (the 0x58 byte and 0x5A halfword groups).

    #[inline]
    fn read_elem<B: Bus>(&self, bus: &mut B, addr: u32, esz: u32) -> u32 {
        if esz == 1 {
            bus.read_u8(addr) as u32
        } else {
            bus.read_u16(addr) as u32
        }
    }
    #[inline]
    fn write_elem<B: Bus>(&self, bus: &mut B, addr: u32, esz: u32, v: u32) {
        if esz == 1 {
            bus.write_u8(addr, v as u8)
        } else {
            bus.write_u16(addr, v as u16)
        }
    }
    #[inline]
    fn elem_mask(v: u32, esz: u32) -> u32 {
        if esz == 1 {
            v & 0xff
        } else {
            v & 0xffff
        }
    }

    /// F7aDecodeOperands: both operands are addresses (ReadAMAddress), each
    /// followed by a length byte. A length byte with bit 7 set names a register
    /// holding the count; otherwise the byte itself is the count. `m` for op1
    /// comes from subop bit 6, for op2 from bit 5.
    fn f7a_decode<B: Bus>(&mut self, bus: &mut B, dim1: u8, dim2: u8) {
        let pc = self.reg[PC];
        self.moddim = dim1;
        self.modm = self.subop & 0x40 != 0;
        self.modadd = pc.wrapping_add(2);
        self.amlength1 = self.read_am_address(bus);
        self.flag1 = self.amflag;
        self.op1 = self.amout;
        let appb = bus.read_u8(pc.wrapping_add(2 + self.amlength1));
        self.lenop1 = if appb & 0x80 != 0 {
            self.reg[(appb & 0x1f) as usize]
        } else {
            appb as u32
        };

        self.moddim = dim2;
        self.modm = self.subop & 0x20 != 0;
        self.modadd = pc.wrapping_add(3 + self.amlength1);
        self.amlength2 = self.read_am_address(bus);
        self.flag2 = self.amflag;
        self.op2 = self.amout;
        let appb = bus.read_u8(pc.wrapping_add(3 + self.amlength1 + self.amlength2));
        self.lenop2 = if appb & 0x80 != 0 {
            self.reg[(appb & 0x1f) as usize]
        } else {
            appb as u32
        };
    }

    /// F7bDecodeOperands: op1 is an address with a trailing length byte, op2 is
    /// a value (the character to search for). Only lenop1 is read.
    fn f7b_decode<B: Bus>(&mut self, bus: &mut B, dim1: u8, dim2: u8) {
        let pc = self.reg[PC];
        self.moddim = dim1;
        self.modm = self.subop & 0x40 != 0;
        self.modadd = pc.wrapping_add(2);
        self.amlength1 = self.read_am_address(bus);
        self.flag1 = self.amflag;
        self.op1 = self.amout;
        let appb = bus.read_u8(pc.wrapping_add(2 + self.amlength1));
        self.lenop1 = if appb & 0x80 != 0 {
            self.reg[(appb & 0x1f) as usize]
        } else {
            appb as u32
        };

        self.moddim = dim2;
        self.modm = self.subop & 0x20 != 0;
        self.modadd = pc.wrapping_add(3 + self.amlength1);
        self.amlength2 = self.read_am(bus);
        self.flag2 = self.amflag;
        self.op2 = self.amout;
    }

    #[inline]
    fn f7a_len(&self) -> u32 {
        self.amlength1 + self.amlength2 + 4
    }
    #[inline]
    fn f7b_len(&self) -> u32 {
        self.amlength1 + self.amlength2 + 3
    }

    /// F7cDecodeOperands: op1 is a value (dim1), op2 an address (dim2), then a
    /// trailing pattern/length byte into lenop1. The reference F7c frame, used by the
    /// BCD arithmetic and the bit-field inserts.
    fn f7c_decode<B: Bus>(&mut self, bus: &mut B, dim1: u8, dim2: u8) {
        let pc = self.reg[PC];
        self.moddim = dim1;
        self.modm = self.subop & 0x40 != 0;
        self.modadd = pc.wrapping_add(2);
        self.amlength1 = self.read_am(bus);
        self.flag1 = self.amflag;
        self.op1 = self.amout;

        self.moddim = dim2;
        self.modm = self.subop & 0x20 != 0;
        self.modadd = pc.wrapping_add(2 + self.amlength1);
        self.amlength2 = self.read_am_address(bus);
        self.flag2 = self.amflag;
        self.op2 = self.amout;

        let appb = bus.read_u8(pc.wrapping_add(2 + self.amlength1 + self.amlength2));
        self.lenop1 = if appb & 0x80 != 0 {
            self.reg[(appb & 0x1f) as usize]
        } else {
            appb as u32
        };
    }

    /// ADDDC (0x59 sub 0x00): packed-BCD add with carry. The Z flag only ever
    /// clears (result nonzero or carry out). opADDDC.
    fn op_adddc<B: Bus>(&mut self, bus: &mut B) -> u32 {
        self.f7c_decode(bus, 0, 0);
        let appb = self.load_op2(bus, 0);
        let src = ((self.op1 >> 4) & 0xf) * 10 + (self.op1 & 0xf);
        let dst = ((appb >> 4) & 0xf) * 10 + (appb & 0xf);
        let mut v = src + dst + u32::from(self.cy);
        if v >= 100 {
            v -= 100;
            self.cy = true;
        } else {
            self.cy = false;
        }
        if v != 0 || self.cy {
            self.z = false;
        }
        let r = ((v / 10) << 4) | (v % 10);
        self.store_op2(bus, 0, r);
        self.f7b_len()
    }

    /// SUBDC (0x59 sub 0x01): packed-BCD subtract (dst - src) with borrow.
    fn op_subdc<B: Bus>(&mut self, bus: &mut B) -> u32 {
        self.f7c_decode(bus, 0, 0);
        let appb = self.load_op2(bus, 0);
        let src = ((self.op1 >> 4) & 0xf) * 10 + (self.op1 & 0xf);
        let dst = ((appb >> 4) & 0xf) * 10 + (appb & 0xf);
        let mut v = dst as i64 - src as i64 - i64::from(self.cy);
        if v < 0 {
            v += 100;
            self.cy = true;
        } else {
            self.cy = false;
        }
        if v != 0 || self.cy {
            self.z = false;
        }
        let v = v as u32;
        let r = ((v / 10) << 4) | (v % 10);
        self.store_op2(bus, 0, r);
        self.f7b_len()
    }

    /// SUBRDC (0x59 sub 0x02): packed-BCD reverse subtract (src - dst).
    fn op_subrdc<B: Bus>(&mut self, bus: &mut B) -> u32 {
        self.f7c_decode(bus, 0, 0);
        let appb = self.load_op2(bus, 0);
        let src = ((self.op1 >> 4) & 0xf) * 10 + (self.op1 & 0xf);
        let dst = ((appb >> 4) & 0xf) * 10 + (appb & 0xf);
        let mut v = src as i64 - dst as i64 - i64::from(self.cy);
        if v < 0 {
            v += 100;
            self.cy = true;
        } else {
            self.cy = false;
        }
        if v != 0 || self.cy {
            self.z = false;
        }
        let v = v as u32;
        let r = ((v / 10) << 4) | (v % 10);
        self.store_op2(bus, 0, r);
        self.f7b_len()
    }

    /// CVTDPZ (0x59 sub 0x10): packed-BCD byte to zoned halfword; the zone
    /// nibbles come from the pattern operand. opCVTDPZ.
    fn op_cvtdpz<B: Bus>(&mut self, bus: &mut B) -> u32 {
        self.f7c_decode(bus, 0, 1);
        let mut apph = ((self.op1 >> 4) & 0xf) | ((self.op1 & 0xf) << 8);
        apph |= self.lenop1 & 0xffff;
        apph |= (self.lenop1 << 8) & 0xffff;
        if self.op1 != 0 {
            self.z = false;
        }
        self.store_op2(bus, 1, apph);
        self.f7b_len()
    }

    /// CVTDZP (0x59 sub 0x18): zoned halfword to packed-BCD byte. The decimal
    /// exceptions are logerror-only in the reference, so they are not modeled.
    fn op_cvtdzp<B: Bus>(&mut self, bus: &mut B) -> u32 {
        self.f7c_decode(bus, 1, 0);
        let appb = ((self.op1 >> 8) & 0xf) | ((self.op1 & 0xf) << 4);
        if appb != 0 {
            self.z = false;
        }
        self.store_op2(bus, 0, appb);
        self.f7b_len()
    }

    /// Decode a BAM1 bit-field source and return `(word, bit_offset, length)`.
    ///
    /// These are the non-indexed forms used by the Model 1 boot ROM: bit
    /// displacement from a register, register indirect, PC displacement,
    /// direct address, and bit-field auto-increment/decrement.
    /// EXTBF.S, EXTBF.Z, and EXTBF.L.
    ///
    /// `kind` is 0 for sign extension, 1 for zero extension, and 2 for
    /// left-justification.
    fn op_extbf<B: Bus>(&mut self, bus: &mut B, kind: u8) -> Option<u32> {
        let pc = self.reg[PC];
        // F7bDecodeFirstOperand(BitReadAM, 11): the source decodes through the
        // full bam1 table (bit_read_am), dimension 11 (bit-field dword).
        self.moddim = 11;
        self.modm = self.subop & 0x40 != 0;
        self.modadd = pc.wrapping_add(2);
        let source_length = self.bit_read_am(bus)?;
        let (source, bit_offset) = (self.amout, self.bamoffset);
        self.amlength1 = source_length;

        let extension = bus.read_u8(pc.wrapping_add(2 + source_length));
        self.lenop1 = if extension & 0x80 != 0 {
            self.reg[(extension & 0x1f) as usize]
        } else {
            extension as u32
        };

        let width = self.lenop1.min(32);
        let mask = match width {
            0 => 0,
            32 => u32::MAX,
            _ => (1u32 << width) - 1,
        };
        let extracted = (source >> bit_offset) & mask;

        let value = match kind {
            0 if width == 0 => 0,
            0 if width == 32 => extracted,
            0 => {
                let sign = 1u32 << (width - 1);
                if extracted & sign != 0 {
                    extracted | !mask
                } else {
                    extracted
                }
            }
            1 => extracted,
            2 if width == 0 => 0,
            2 if width == 32 => extracted,
            2 => extracted << (32 - width),
            _ => unreachable!(),
        };

        self.moddim = 2;
        self.modm = self.subop & 0x20 != 0;
        self.modadd = pc.wrapping_add(3 + source_length);
        self.amlength2 = self.write_am(bus, value);

        Some(self.f7b_len())
    }

    // --- Bit-string instructions (the 0x5B group and the 0x5D INSBF pair).
    // These decode operands as bit addresses via `bit_read_am_address`
    //: a byte base in `op*` plus a bit offset in `bamoffset*`.

    /// F7bDecodeFirstOperand with BitReadAMAddress (dim 10): op1 is the byte
    /// base, `bamoffset` its bit offset, followed by the length byte (a
    /// register when bit 7 is set, as with f7a/f7b).
    fn f7b_first_bit<B: Bus>(&mut self, bus: &mut B) {
        let pc = self.reg[PC];
        self.moddim = 10;
        self.modm = self.subop & 0x40 != 0;
        self.modadd = pc.wrapping_add(2);
        self.amlength1 = self.bit_read_am_address(bus);
        self.flag1 = self.amflag;
        self.op1 = self.amout;
        let appb = bus.read_u8(pc.wrapping_add(2 + self.amlength1));
        self.lenop1 = if appb & 0x80 != 0 {
            self.reg[(appb & 0x1f) as usize]
        } else {
            appb as u32
        };
    }

    /// F7bDecodeOperands for the bit-string moves: both operands are bit
    /// addresses (dim 10); only op1 is followed by a length byte. The two bit
    /// offsets land in `bamoffset1`/`bamoffset2`.
    fn f7b_decode_bitpair<B: Bus>(&mut self, bus: &mut B) {
        self.f7b_first_bit(bus);
        self.bamoffset1 = self.bamoffset;

        let pc = self.reg[PC];
        self.moddim = 10;
        self.modm = self.subop & 0x20 != 0;
        self.modadd = pc.wrapping_add(3 + self.amlength1);
        self.amlength2 = self.bit_read_am_address(bus);
        self.flag2 = self.amflag;
        self.op2 = self.amout;
        self.bamoffset2 = self.bamoffset;
    }

    /// F7bWriteSecondOperand: write `val` to the word operand sitting after
    /// op1 and its length byte.
    fn f7b_write_second<B: Bus>(&mut self, bus: &mut B, dim2: u8, val: u32) {
        self.moddim = dim2;
        self.modm = self.subop & 0x20 != 0;
        self.modadd = self.reg[PC].wrapping_add(3 + self.amlength1);
        self.amlength2 = self.write_am(bus, val);
    }

    /// F7cDecodeOperands with a bit-address op2: op1 is a word value
    /// (ReadAM, dim 2), op2 a bit address (BitReadAMAddress, dim 11), then
    /// the trailing length/pattern byte.
    fn f7c_decode_bit<B: Bus>(&mut self, bus: &mut B) {
        let pc = self.reg[PC];
        self.moddim = 2;
        self.modm = self.subop & 0x40 != 0;
        self.modadd = pc.wrapping_add(2);
        self.amlength1 = self.read_am(bus);
        self.flag1 = self.amflag;
        self.op1 = self.amout;

        self.moddim = 11;
        self.modm = self.subop & 0x20 != 0;
        self.modadd = pc.wrapping_add(2 + self.amlength1);
        self.amlength2 = self.bit_read_am_address(bus);
        self.flag2 = self.amflag;
        self.op2 = self.amout;

        let appb = bus.read_u8(pc.wrapping_add(2 + self.amlength1 + self.amlength2));
        self.lenop1 = if appb & 0x80 != 0 {
            self.reg[(appb & 0x1f) as usize]
        } else {
            appb as u32
        };
    }

    /// F7CCREATEBITMASK: (1 << len) - 1, with len clamped to the 32-bit range.
    fn f7_bitmask(len: u32) -> u32 {
        match len.min(32) {
            0 => 0,
            32 => u32::MAX,
            w => (1u32 << w) - 1,
        }
    }

    /// SCH0BSU/SCH1BSU (0x5B sub 0x00/0x02): scan the bit string at op1
    /// upward for the first 0 (search1=false) or 1 bit. R28 tracks the byte
    /// being tested, the destination receives the bit index, and Z is set
    /// when the bit was not found. opSCHBS.
    fn op_schbs<B: Bus>(&mut self, bus: &mut B, search1: bool) -> u32 {
        self.f7b_first_bit(bus);

        // Read the first byte at the bit offset.
        let mut op1 = self.op1.wrapping_add(self.bamoffset / 8);
        let mut data = bus.read_u8(op1);
        let mut offset = self.bamoffset & 7;

        let mut i = 0u32;
        while i < self.lenop1 {
            // Update the work register.
            self.reg[28] = op1;

            // Is there a 0 / 1 at the current offset?
            if (data & (1 << offset) != 0) == search1 {
                break;
            }

            // Next bit, crossing into the next byte at the boundary.
            offset += 1;
            if offset == 8 {
                offset = 0;
                op1 = op1.wrapping_add(1);
                data = bus.read_u8(op1);
            }
            i += 1;
        }

        // Set zero if the bit was not found.
        self.z = i == self.lenop1;

        // Write the final offset to the destination.
        self.f7b_write_second(bus, 2, i);
        self.f7b_len()
    }

    /// INSBFR/INSBFL (0x5D sub 0x18/0x19): insert the `lenop1`-bit field of
    /// op1 into the bit string at op2. The field is right-justified for
    /// INSBFR; INSBFL takes it left-justified (op1 >>= 32 - len first).
    /// opINSBFR/opINSBFL.
    fn op_insbf<B: Bus>(&mut self, bus: &mut B, left: bool) -> u32 {
        self.f7c_decode_bit(bus);

        let width = self.lenop1.min(32);
        let mut op1 = self.op1;
        if left && width > 0 {
            op1 >>= 32 - width;
        }
        let mask = Self::f7_bitmask(self.lenop1);

        let op2 = self.op2.wrapping_add(self.bamoffset / 8);
        let offset = self.bamoffset & 7;
        let mut appw = bus.read_u32(op2);
        appw &= !(mask << offset);
        appw |= (mask & op1) << offset;
        bus.write_u32(op2, appw);

        self.f7b_len() // == F7CEND: amlength1 + amlength2 + 3
    }

    /// MOVBSU/MOVBSD (0x5B sub 0x08/0x09): copy the `lenop1`-bit string at
    /// op1 to op2, upward (low to high addresses) or downward. R28/R27 track
    /// the current source/destination byte. opMOVBSU/opMOVBSD.
    fn op_movbs<B: Bus>(&mut self, bus: &mut B, down: bool) -> u32 {
        self.f7b_decode_bitpair(bus);

        let mut off1 = self.bamoffset1;
        let mut off2 = self.bamoffset2;
        if down {
            // The downward form starts from the string's last bit.
            off1 = off1.wrapping_add(self.lenop1.wrapping_sub(1));
            off2 = off2.wrapping_add(self.lenop1.wrapping_sub(1));
        }
        let mut op1 = self.op1.wrapping_add(off1 / 8);
        let mut op2 = self.op2.wrapping_add(off2 / 8);
        off1 &= 7;
        off2 &= 7;

        let mut src = bus.read_u8(op1);
        let mut dst = bus.read_u8(op2);

        for _ in 0..self.lenop1 {
            // Update the work registers.
            self.reg[28] = op1;
            self.reg[27] = op2;

            dst &= !(1 << off2);
            dst |= ((src >> off1) & 1) << off2;

            if down {
                if off1 == 0 {
                    off1 = 8;
                    op1 = op1.wrapping_sub(1);
                    src = bus.read_u8(op1);
                }
                if off2 == 0 {
                    bus.write_u8(op2, dst);
                    off2 = 8;
                    op2 = op2.wrapping_sub(1);
                    dst = bus.read_u8(op2);
                }
                off1 -= 1;
                off2 -= 1;
            } else {
                off1 += 1;
                off2 += 1;
                if off1 == 8 {
                    off1 = 0;
                    op1 = op1.wrapping_add(1);
                    src = bus.read_u8(op1);
                }
                if off2 == 8 {
                    bus.write_u8(op2, dst);
                    off2 = 0;
                    op2 = op2.wrapping_add(1);
                    dst = bus.read_u8(op2);
                }
            }
        }

        // Flush of the final data.
        if down {
            if off2 != 7 {
                bus.write_u8(op2, dst);
            }
        } else if off2 != 0 {
            bus.write_u8(op2, dst);
        }

        self.f7b_len()
    }

    /// Opcode 5D bit-field group.
    fn op_5d<B: Bus>(&mut self, bus: &mut B) -> Option<u32> {
        self.subop = bus.read_u8(self.reg[PC].wrapping_add(1));

        match self.subop & 0x1f {
            0x08 => self.op_extbf(bus, 0),
            0x09 => self.op_extbf(bus, 1),
            0x0a => self.op_extbf(bus, 2),
            0x18 => Some(self.op_insbf(bus, false)), // INSBFR
            0x19 => Some(self.op_insbf(bus, true)),  // INSBFL
            _ => None,
        }
    }

    /// MOVC upward (MOVCU/MOVCFU/MOVCSU): copy op1->op2 low-to-high, optionally
    /// stopping at the terminator in R26 and/or filling the tail with it.
    fn op_movstr_u<B: Bus>(&mut self, bus: &mut B, esz: u32, fill: bool, stop: bool) -> u32 {
        let dim = if esz == 1 { 0 } else { 1 };
        self.f7a_decode(bus, dim, dim);
        let (op1, op2, l1, l2) = (self.op1, self.op2, self.lenop1, self.lenop2);
        let r26 = Self::elem_mask(self.reg[26], esz);
        let dest = l1.min(l2);
        let mut i = 0u32;
        while i < dest {
            let c1 = self.read_elem(bus, op1.wrapping_add(i * esz), esz);
            self.write_elem(bus, op2.wrapping_add(i * esz), esz, c1);
            if stop && c1 == r26 {
                break;
            }
            i += 1;
        }
        self.reg[28] = op1.wrapping_add(i * esz);
        self.reg[27] = op2.wrapping_add(i * esz);
        if fill && l1 < l2 {
            while i < l2 {
                self.write_elem(bus, op2.wrapping_add(i * esz), esz, r26);
                i += 1;
            }
            self.reg[27] = op2.wrapping_add(i * esz);
        }
        self.f7a_len()
    }

    /// MOVC downward (MOVCD/MOVCFD): copy op1->op2 high-to-low. The byte and
    /// halfword forms differ in the tail-fill addressing, so both are spelled
    /// out.
    fn op_movstr_d<B: Bus>(&mut self, bus: &mut B, esz: u32, fill: bool, stop: bool) -> u32 {
        let dim = if esz == 1 { 0 } else { 1 };
        self.f7a_decode(bus, dim, dim);
        let (op1, op2, l1, l2) = (self.op1, self.op2, self.lenop1, self.lenop2);
        let r26 = Self::elem_mask(self.reg[26], esz);
        let dest = l1.min(l2);
        let mut i = 0u32;
        while i < dest {
            let off = (dest - i - 1) * esz;
            let c1 = self.read_elem(bus, op1.wrapping_add(off), esz);
            self.write_elem(bus, op2.wrapping_add(off), esz, c1);
            if stop && c1 == r26 {
                break;
            }
            i += 1;
        }
        self.reg[28] = op1.wrapping_add((l1.wrapping_sub(i).wrapping_sub(1)) * esz);
        self.reg[27] = op2.wrapping_add((l2.wrapping_sub(i).wrapping_sub(1)) * esz);
        if fill && l1 < l2 {
            while i < l2 {
                let addr = if esz == 1 {
                    op2.wrapping_add(dest)
                        .wrapping_add(l2.wrapping_sub(i).wrapping_sub(1))
                } else {
                    op2.wrapping_add((l2.wrapping_sub(i).wrapping_sub(1)) * esz)
                };
                self.write_elem(bus, addr, esz, r26);
                i += 1;
            }
            self.reg[27] = op2.wrapping_add((l2.wrapping_sub(i).wrapping_sub(1)) * esz);
        }
        self.f7a_len()
    }

    /// CMPC (CMPC/CMPCF/CMPCS): compare two strings, setting Z/S (and CY for the
    /// stop form). Optionally pads the shorter operand with R26 first.
    fn op_cmpstr<B: Bus>(&mut self, bus: &mut B, esz: u32, fill: bool, stop: bool) -> u32 {
        let dim = if esz == 1 { 0 } else { 1 };
        self.f7a_decode(bus, dim, dim);
        let (op1, op2, l1, l2) = (self.op1, self.op2, self.lenop1, self.lenop2);
        let r26 = Self::elem_mask(self.reg[26], esz);
        if fill {
            if l1 < l2 {
                for k in l1..l2 {
                    self.write_elem(bus, op1.wrapping_add(k * esz), esz, r26);
                }
            } else if l2 < l1 {
                for k in l2..l1 {
                    self.write_elem(bus, op2.wrapping_add(k * esz), esz, r26);
                }
            }
        }
        let dest = l1.min(l2);
        self.z = false;
        self.s = false;
        if stop {
            self.cy = true;
        }
        let mut i = 0u32;
        while i < dest {
            let c1 = self.read_elem(bus, op1.wrapping_add(i * esz), esz);
            let c2 = self.read_elem(bus, op2.wrapping_add(i * esz), esz);
            if c1 > c2 {
                self.s = true;
                break;
            } else if c2 > c1 {
                self.s = false;
                break;
            }
            if stop && (c1 == r26 || c2 == r26) {
                self.cy = false;
                break;
            }
            i += 1;
        }
        self.reg[28] = l1.wrapping_add(i * esz);
        self.reg[27] = l2.wrapping_add(i * esz);
        if i == dest {
            if l1 > l2 {
                self.s = true;
            } else if l2 > l1 {
                self.s = false;
            } else {
                self.z = true;
            }
        }
        self.f7a_len()
    }

    /// SCHC/SKPC: scan op1 for (search=true) or past (search=false) the value in
    /// op2. `down` walks high-to-low. Z is set the opposite of the manual, as
    /// The reference notes.
    fn op_search<B: Bus>(&mut self, bus: &mut B, esz: u32, search: bool, down: bool) -> u32 {
        let dim = if esz == 1 { 0 } else { 1 };
        self.f7b_decode(bus, dim, dim);
        let (op1, l1, target) = (self.op1, self.lenop1, Self::elem_mask(self.op2, esz));
        let hit; // index at which the scan stopped
        if !down {
            let mut i = 0u32;
            while i < l1 {
                let found = self.read_elem(bus, op1.wrapping_add(i * esz), esz) == target;
                if (search && found) || (!search && !found) {
                    break;
                }
                i += 1;
            }
            hit = i;
            self.reg[28] = op1.wrapping_add(i * esz);
            self.reg[27] = i;
        } else {
            let mut i = l1 as i64 - 1;
            while i >= 0 {
                let found = self.read_elem(bus, op1.wrapping_add(i as u32 * esz), esz) == target;
                if (search && found) || (!search && !found) {
                    break;
                }
                i -= 1;
            }
            self.reg[28] = op1.wrapping_add((i as u32).wrapping_mul(esz));
            self.reg[27] = i as u32;
            hit = i as u32;
        }
        self.z = hit == l1;
        self.f7b_len()
    }

    /// 0x58 (byte) and 0x5A (halfword) sub-dispatch, keyed by the next byte's
    /// low 5 bits.
    fn op_string_group<B: Bus>(&mut self, bus: &mut B, esz: u32) -> Option<u32> {
        self.subop = bus.read_u8(self.reg[PC].wrapping_add(1));
        Some(match self.subop & 0x1f {
            0x00 => self.op_cmpstr(bus, esz, false, false), // CMPC
            0x01 => self.op_cmpstr(bus, esz, true, false),  // CMPCF
            0x02 => self.op_cmpstr(bus, esz, false, true),  // CMPCS
            0x08 => self.op_movstr_u(bus, esz, false, false), // MOVCU
            0x09 => self.op_movstr_d(bus, esz, false, false), // MOVCD
            0x0a => self.op_movstr_u(bus, esz, true, false), // MOVCFU
            0x0b => self.op_movstr_d(bus, esz, true, false), // MOVCFD
            0x0c => self.op_movstr_u(bus, esz, false, true), // MOVCSU
            0x18 => self.op_search(bus, esz, true, false),  // SCHCU
            0x19 => self.op_search(bus, esz, true, true),   // SCHCD
            0x1a => self.op_search(bus, esz, false, false), // SKPCU
            0x1b => self.op_search(bus, esz, false, true),  // SKPCD
            _ => return None,
        })
    }

    /// IN: op1 is an I/O address, its contents (of `dim`) are written to op2.
    /// The hardware I/O-stall path is not modelled; a real stall never happens
    /// with a memory-backed bus.
    fn op_in<B: Bus>(&mut self, bus: &mut B, dim: u8) -> u32 {
        self.f12_first(bus, false, dim);
        let addr = self.op1;
        let v = match dim {
            0 => bus.read_io8(addr) as u32,
            1 => bus.read_io16(addr) as u32,
            _ => bus.read_io32(addr),
        };
        self.f12_write_second(bus, dim, v);
        self.f12_len()
    }

    /// OUT: op1 is a value of `dim`, op2 is an I/O address it is written to.
    fn op_out<B: Bus>(&mut self, bus: &mut B, dim: u8) -> u32 {
        self.f12_decode_operands(bus, Op::Value, dim, Op::Addr, 2);
        let (addr, v) = (self.op2, self.op1);
        match dim {
            0 => bus.write_io8(addr, v as u8),
            1 => bus.write_io16(addr, v as u16),
            _ => bus.write_io32(addr, v),
        }
        self.f12_len()
    }

    // --- Format-3 single-operand stack / subroutine instructions.

    /// Sets up the format-3 operand decode: the sole operand sits at PC+1, with
    /// `m` taken from the opcode's _0/_1 suffix.
    #[inline]
    fn f3_setup(&mut self, dim: u8, modm: bool) {
        self.modm = modm;
        self.moddim = dim;
        self.modadd = self.reg[PC].wrapping_add(1);
    }
    #[inline]
    fn push32<B: Bus>(&mut self, bus: &mut B, v: u32) {
        self.reg[SP] = self.reg[SP].wrapping_sub(4);
        let sp = self.reg[SP];
        bus.write_u32(sp, v);
    }
    #[inline]
    fn pop32<B: Bus>(&mut self, bus: &mut B) -> u32 {
        let sp = self.reg[SP];
        let v = bus.read_u32(sp);
        self.reg[SP] = sp.wrapping_add(4);
        v
    }

    /// PUSH: push the word operand onto the stack.
    fn op_push<B: Bus>(&mut self, bus: &mut B, modm: bool) -> u32 {
        self.f3_setup(2, modm);
        let len = self.read_am(bus);
        let v = self.amout;
        self.push32(bus, v);
        len + 1
    }
    /// POP: pop a word off the stack into the operand.
    fn op_pop<B: Bus>(&mut self, bus: &mut B, modm: bool) -> u32 {
        self.f3_setup(2, modm);
        let v = self.pop32(bus);
        let len = self.write_am(bus, v);
        len + 1
    }
    /// PUSHM: push the registers named by a bitmask, PSW first (bit 31), then
    /// R30 down to R0.
    fn op_pushm<B: Bus>(&mut self, bus: &mut B, modm: bool) -> u32 {
        self.f3_setup(2, modm);
        let len = self.read_am(bus);
        let mask = self.amout;
        if mask & (1 << 31) != 0 {
            let psw = self.read_psw();
            self.push32(bus, psw);
        }
        for i in 0..31 {
            if mask & (1 << (30 - i)) != 0 {
                let r = self.reg[(30 - i) as usize];
                self.push32(bus, r);
            }
        }
        len + 1
    }
    /// POPM: pop registers named by a bitmask, R0 up to R30, then PSW's low half.
    fn op_popm<B: Bus>(&mut self, bus: &mut B, modm: bool) -> u32 {
        self.f3_setup(2, modm);
        let len = self.read_am(bus);
        let mask = self.amout;
        for i in 0..31 {
            if mask & (1 << i) != 0 {
                self.reg[i] = self.pop32(bus);
            }
        }
        if mask & (1 << 31) != 0 {
            let sp = self.reg[SP];
            let w = bus.read_u16(sp) as u32;
            let v = (self.read_psw() & 0xffff_0000) | w;
            self.write_psw(v);
            self.reg[SP] = sp.wrapping_add(4);
        }
        len + 1
    }
    /// JSR: push the return address and jump to the operand address.
    fn op_jsr<B: Bus>(&mut self, bus: &mut B, modm: bool) -> u32 {
        self.f3_setup(0, modm);
        let len = self.read_am_address(bus);
        let ret = self.reg[PC].wrapping_add(len + 1);
        self.push32(bus, ret);
        self.reg[PC] = self.amout;
        0
    }
    /// RET: pop PC and AP, then release `operand` more bytes of stack frame.
    fn op_ret<B: Bus>(&mut self, bus: &mut B, modm: bool) -> u32 {
        self.f3_setup(2, modm);
        self.read_am(bus);
        let adjust = self.amout;
        self.reg[PC] = self.pop32(bus);
        self.reg[AP] = self.pop32(bus);
        self.reg[SP] = self.reg[SP].wrapping_add(adjust);
        0
    }
    /// TEST: set Z/S from the operand's value, clearing CY/OV.
    fn op_test<B: Bus>(&mut self, bus: &mut B, dim: u8, modm: bool) -> u32 {
        self.f3_setup(dim, modm);
        let len = self.read_am(bus);
        let (_, msb, _) = Self::dim_params(dim);
        self.z = self.amout == 0;
        self.s = self.amout & msb != 0;
        self.cy = false;
        self.ov = false;
        len + 1
    }
    /// GETPSW: store the PSW into the operand.
    fn op_getpsw<B: Bus>(&mut self, bus: &mut B, modm: bool) -> u32 {
        self.f3_setup(2, modm);
        let psw = self.read_psw();
        let len = self.write_am(bus, psw);
        len + 1
    }
    /// TASI: test-and-set a byte (flags from byte-0xFF, then store 0xFF).
    fn op_tasi<B: Bus>(&mut self, bus: &mut B, modm: bool) -> u32 {
        self.f3_setup(0, modm);
        let len = self.read_am_address(bus);
        let cur = self.load_decoded(bus);
        self.alu_sub(cur, 0xff, 0, 0);
        self.store_decoded(bus, 0xff);
        len + 1
    }
    /// PREPARE: build a stack frame -- push FP, set FP=SP, reserve `operand`
    /// bytes of locals.
    fn op_prepare<B: Bus>(&mut self, bus: &mut B, modm: bool) -> u32 {
        self.f3_setup(2, modm);
        let len = self.read_am(bus);
        let fp = self.reg[FP];
        self.push32(bus, fp);
        self.reg[FP] = self.reg[SP];
        self.reg[SP] = self.reg[SP].wrapping_sub(self.amout);
        len + 1
    }
    /// DISPOSE: tear down a PREPARE frame (SP=FP, pop FP).
    fn op_dispose<B: Bus>(&mut self, bus: &mut B) -> u32 {
        self.reg[SP] = self.reg[FP];
        self.reg[FP] = self.pop32(bus);
        1
    }

    /// Conditional branch: if `cond`, add the sign-extended 8- or 16-bit
    /// displacement (relative to the opcode) to PC and consume nothing more;
    /// otherwise fall through past the 2- or 3-byte instruction.
    fn branch<B: Bus>(&mut self, bus: &mut B, cond: bool, wide: bool) -> u32 {
        if cond {
            let disp = if wide {
                bus.read_u16(self.reg[PC].wrapping_add(1)) as i16 as i32 as u32
            } else {
                bus.read_u8(self.reg[PC].wrapping_add(1)) as i8 as i32 as u32
            };
            self.reg[PC] = self.reg[PC].wrapping_add(disp);
            0
        } else if wide {
            3
        } else {
            2
        }
    }

    /// The 16 V60 condition codes (shared by the Bcc block and SETF). Index
    /// 0x_b (11) is "always false".
    fn cond_code(&self, c: u8) -> bool {
        match c & 0x0f {
            0x0 => self.ov,                      // V
            0x1 => !self.ov,                     // NV
            0x2 => self.cy,                      // L / C
            0x3 => !self.cy,                     // NL
            0x4 => self.z,                       // E / Z
            0x5 => !self.z,                      // NE
            0x6 => self.cy | self.z,             // NH
            0x7 => !(self.cy | self.z),          // H
            0x8 => self.s,                       // N
            0x9 => !self.s,                      // P
            0xa => true,                         // always
            0xb => false,                        // never
            0xc => self.s ^ self.ov,             // LT
            0xd => !(self.s ^ self.ov),          // GE
            0xe => (self.s ^ self.ov) | self.z,  // LE
            _ => !((self.s ^ self.ov) | self.z), // GT
        }
    }

    /// The 0x60-0x7F conditional/unconditional relative branch block. `op & 0x10`
    /// selects the 16-bit-displacement half; `op & 0x0F` is the condition.
    fn op_branch<B: Bus>(&mut self, bus: &mut B, op: u8) -> Option<u32> {
        if op & 0x0f == 0x0b {
            return None; // 0x6b / 0x7b are unassigned
        }
        let wide = op & 0x10 != 0;
        let cond = self.cond_code(op & 0x0f);
        Some(self.branch(bus, cond, wide))
    }

    /// DBcc / TB (the 0xC6/0xC7 groups): the sub-byte's top 3 bits pick the
    /// condition, its low 5 the loop register. DBcc decrements the register and
    /// branches (16-bit displacement at PC+2) while it is non-zero and the
    /// condition holds; TB branches when the register is already zero.
    fn op_dbcc<B: Bus>(&mut self, bus: &mut B, c7: bool) -> u32 {
        let subop = bus.read_u8(self.reg[PC].wrapping_add(1));
        let idx = subop >> 5;
        let reg = (subop & 0x1f) as usize;
        let take;
        if c7 && idx == 5 {
            // TB: branch if the register is zero (no decrement).
            take = self.reg[reg] == 0;
        } else {
            let cond = idx * 2 + c7 as u8; // C6 -> even codes, C7 -> odd
            self.reg[reg] = self.reg[reg].wrapping_sub(1);
            take = self.reg[reg] != 0 && self.cond_code(cond);
        }
        if take {
            let disp = bus.read_u16(self.reg[PC].wrapping_add(2)) as i16 as i32 as u32;
            self.reg[PC] = self.reg[PC].wrapping_add(disp);
            0
        } else {
            4
        }
    }

    /// BSR: push the 3-byte-return address and branch by a 16-bit displacement.
    fn op_bsr<B: Bus>(&mut self, bus: &mut B) -> u32 {
        let ret = self.reg[PC].wrapping_add(3);
        self.push32(bus, ret);
        let disp = bus.read_u16(self.reg[PC].wrapping_add(1)) as i16 as i32 as u32;
        self.reg[PC] = self.reg[PC].wrapping_add(disp);
        0
    }

    /// CALL: push AP then the return address, set AP to op2's address and jump to
    /// op1's address.
    fn op_call<B: Bus>(&mut self, bus: &mut B) -> u32 {
        self.f12_decode_operands(bus, Op::Addr, 0, Op::Addr, 2);
        let ret = self.reg[PC].wrapping_add(self.amlength1 + self.amlength2 + 2);
        let ap = self.reg[AP];
        self.push32(bus, ap);
        self.reg[AP] = self.op2;
        self.push32(bus, ret);
        self.reg[PC] = self.op1;
        0
    }

    /// XCH: exchange two operands of `dim`.
    fn op_xch<B: Bus>(&mut self, bus: &mut B, dim: u8) -> u32 {
        self.f12_decode_operands(bus, Op::Addr, dim, Op::Addr, dim);
        let (o1, f1, o2, f2) = (self.op1, self.flag1, self.op2, self.flag2);
        let a = self.load_operand(bus, o1, f1, dim);
        let b = self.load_operand(bus, o2, f2, dim);
        self.store_operand(bus, o1, f1, dim, b);
        self.store_operand(bus, o2, f2, dim, a);
        self.f12_len()
    }

    /// SETF: write 1/0 to op2 depending on whether condition code op1 holds.
    fn op_setf<B: Bus>(&mut self, bus: &mut B) -> u32 {
        self.f12_first(bus, true, 0);
        let v = self.cond_code(self.op1 as u8) as u32;
        self.f12_write_second(bus, 0, v);
        self.f12_len()
    }

    /// Remainder (signed/unsigned). Divisor 0 leaves dst unchanged; OV is always
    /// cleared.
    fn alu_rem(&mut self, dst: u32, src: u32, dim: u8, signed: bool) -> u32 {
        let (mask, msb, _) = Self::dim_params(dim);
        let d = dst & mask;
        let s = src & mask;
        self.ov = false;
        let mut r = d;
        if s != 0 {
            r = if signed {
                match dim {
                    0 => (d as i8).wrapping_rem(s as i8) as u32 & 0xff,
                    1 => (d as i16).wrapping_rem(s as i16) as u32 & 0xffff,
                    _ => (d as i32).wrapping_rem(s as i32) as u32,
                }
            } else {
                d % s
            };
        }
        self.z = (r & mask) == 0;
        self.s = (r & msb) != 0;
        r & mask
    }

    // --- F2 two-operand format (the single-precision FP groups 0x5C/0x5F).
    // The sub-opcode byte at PC+1 doubles as the addressing flags: bit 6 is
    // op1's `m`, bit 5 is op2's.

    fn f2_first<B: Bus>(&mut self, bus: &mut B, read_value: bool, dim1: u8) {
        self.moddim = dim1;
        self.modm = self.instflags & 0x40 != 0;
        self.modadd = self.reg[PC].wrapping_add(2);
        self.amlength1 = if read_value {
            self.read_am(bus)
        } else {
            self.read_am_address(bus)
        };
        self.op1 = self.amout;
        self.flag1 = self.amflag;
    }
    fn f2_second<B: Bus>(&mut self, bus: &mut B, read_value: bool, dim2: u8) {
        self.moddim = dim2;
        self.modm = self.instflags & 0x20 != 0;
        self.modadd = self.reg[PC].wrapping_add(2 + self.amlength1);
        self.amlength2 = if read_value {
            self.read_am(bus)
        } else {
            self.read_am_address(bus)
        };
        self.op2 = self.amout;
        self.flag2 = self.amflag;
    }
    fn f2_write_second<B: Bus>(&mut self, bus: &mut B, dim2: u8, val: u32) {
        self.moddim = dim2;
        self.modm = self.instflags & 0x20 != 0;
        self.modadd = self.reg[PC].wrapping_add(2 + self.amlength1);
        self.amlength2 = self.write_am(bus, val);
    }
    #[inline]
    fn f2_len(&self) -> u32 {
        2 + self.amlength1 + self.amlength2
    }
    /// Load the float at operand 2 (register-direct or memory), for the
    /// read-modify-write float ops.
    fn f2_load_float2<B: Bus>(&mut self, bus: &mut B) -> f32 {
        let bits = if self.flag2 {
            self.reg[self.op2 as usize]
        } else {
            bus.read_u32(self.op2)
        };
        f32::from_bits(bits)
    }
    fn f2_store_float2<B: Bus>(&mut self, bus: &mut B, v: f32) {
        let bits = v.to_bits();
        if self.flag2 {
            self.reg[self.op2 as usize] = bits;
        } else {
            bus.write_u32(self.op2, bits);
        }
    }
    #[inline]
    fn set_fp_flags(&mut self, v: f32) {
        self.ov = false;
        self.cy = false;
        self.s = v.to_bits() & 0x8000_0000 != 0;
        self.z = v == 0.0;
    }

    /// CVT.W.S / CVT.S.W: integer<->single conversions.
    fn op_cvtws<B: Bus>(&mut self, bus: &mut B) -> u32 {
        self.f2_first(bus, true, 2);
        let val = self.op1 as i32 as f32;
        let bits = val.to_bits();
        self.ov = false;
        self.cy = val < 0.0;
        self.s = bits & 0x8000_0000 != 0;
        self.z = val == 0.0;
        self.f2_write_second(bus, 2, bits);
        self.f2_len()
    }
    fn op_cvtsw<B: Bus>(&mut self, bus: &mut B) -> u32 {
        self.f2_first(bus, true, 2);
        let raw = f32::from_bits(self.op1);
        let val = match self.reg[TKCW] & 7 {
            0 => raw.round(),
            1 => raw.floor(),
            2 => raw.ceil(),
            _ => raw.trunc(),
        };
        let w = val as i64 as u32;
        self.s = w & 0x8000_0000 != 0;
        self.ov = (self.s && val >= 0.0) || (!self.s && val <= -1.0);
        self.z = w == 0;
        self.f2_write_second(bus, 2, w);
        self.f2_len()
    }
    fn op_cmpf<B: Bus>(&mut self, bus: &mut B) -> u32 {
        self.f2_first(bus, true, 2);
        self.f2_second(bus, true, 2);
        let appf = f32::from_bits(self.op2) - f32::from_bits(self.op1);
        self.z = appf == 0.0;
        self.s = appf < 0.0;
        self.ov = false;
        self.cy = false;
        self.f2_len()
    }
    fn op_movfs<B: Bus>(&mut self, bus: &mut B) -> u32 {
        self.f2_first(bus, true, 2);
        let v = self.op1;
        self.f2_write_second(bus, 2, v);
        self.f2_len()
    }
    /// A single-precision op writing op2: `f` maps (op2 value, op1 value) to the
    /// result; `load` says whether op2 is read first (arithmetic) or write-only
    /// (neg/abs). op1's size is `dim1` (halfword for SCLFS, word otherwise).
    fn op_fp_store<B: Bus, F: Fn(f32, f32) -> f32>(
        &mut self,
        bus: &mut B,
        dim1: u8,
        load: bool,
        neg_cy: bool,
        f: F,
    ) -> u32 {
        self.f2_first(bus, true, dim1);
        self.f2_second(bus, false, 2);
        let cur = if load { self.f2_load_float2(bus) } else { 0.0 };
        let src = f32::from_bits(self.op1);
        let r = f(cur, src);
        self.set_fp_flags(r);
        if neg_cy {
            self.cy = r < 0.0;
        }
        self.f2_store_float2(bus, r);
        self.f2_len()
    }
    fn op_sclfs<B: Bus>(&mut self, bus: &mut B) -> u32 {
        self.f2_first(bus, true, 1);
        self.f2_second(bus, false, 2);
        let mut appf = self.f2_load_float2(bus);
        let sh = self.op1 as u16 as i16;
        if sh < 0 {
            appf /= (1i32 << (-sh)) as f32;
        } else {
            appf *= (1i32 << sh) as f32;
        }
        self.set_fp_flags(appf);
        self.f2_store_float2(bus, appf);
        self.f2_len()
    }

    /// 0x5F: FP conversion group.
    fn op_fp_conv<B: Bus>(&mut self, bus: &mut B) -> Option<u32> {
        self.instflags = bus.read_u8(self.reg[PC].wrapping_add(1));
        match self.instflags & 0x1f {
            0x00 => Some(self.op_cvtws(bus)),
            0x01 => Some(self.op_cvtsw(bus)),
            _ => None,
        }
    }
    /// 0x5C: single-precision FP arithmetic group.
    fn op_fp_arith<B: Bus>(&mut self, bus: &mut B) -> Option<u32> {
        self.instflags = bus.read_u8(self.reg[PC].wrapping_add(1));
        Some(match self.instflags & 0x1f {
            0x00 => self.op_cmpf(bus),
            0x08 => self.op_movfs(bus),
            0x09 => self.op_fp_store(bus, 2, false, true, |_, s| -s), // NEGFS
            0x0a => self.op_fp_store(bus, 2, false, false, |_, s| s.abs()), // ABSFS
            0x10 => self.op_sclfs(bus),                               // SCLFS
            0x18 => self.op_fp_store(bus, 2, true, false, |d, s| d + s), // ADDFS
            0x19 => self.op_fp_store(bus, 2, true, false, |d, s| d - s), // SUBFS
            0x1a => self.op_fp_store(bus, 2, true, false, |d, s| d * s), // MULFS
            0x1b => self.op_fp_store(bus, 2, true, false, |d, s| d / s), // DIVFS
            _ => return None,
        })
    }

    /// Opcode dispatch. Filled in against the reference optable; opcodes not present
    /// here are counted and stop the slice.
    pub(crate) fn exec_op<B: Bus>(&mut self, bus: &mut B, op: u8) -> Option<u32> {
        match op {
            0xD6 | 0xD7 => {
                self.modm = op & 1 != 0;
                self.modadd = self.reg[PC].wrapping_add(1);
                self.moddim = 0;
                self.read_am_address(bus);
                self.reg[PC] = self.amout;
                Some(0)
            }
            0x09 => Some(self.op_mov(bus, 0)),        // MOVB
            0x1b => Some(self.op_mov(bus, 1)),        // MOVH
            0x2d => Some(self.op_mov(bus, 2)),        // MOVW
            0x3f => Some(self.op_movd(bus)),          // MOVD
            0x01 => Some(self.op_ldtask(bus)),        // LDTASK
            0x08 => Some(self.op_rvbit(bus)),         // RVBIT
            0x10 => Some(self.op_clrtlba()),          // CLRTLBA
            0x4b => Some(self.op_chlvl(bus)),         // CHLVL
            0x4d => Some(self.op_chka(bus)),          // CHKAR
            0x4e => Some(self.op_chka(bus)),          // CHKAW
            0x4f => Some(self.op_chka(bus)),          // CHKAE
            0xc8 => Some(self.op_brk()),              // BRK
            0xc9 => Some(self.op_brkv(bus)),          // BRKV
            0xcb => Some(self.op_trapfl()),           // TRAPFL
            0xf8 | 0xf9 => Some(self.op_trap(bus)),   // TRAP
            0xfc | 0xfd => Some(self.op_sttask(bus)), // STTASK
            0xfe | 0xff => Some(self.op_clrtlb(bus)), // CLRTLB
            0x0a => Some(self.op_mov_xf(bus, 0, 1, |_, v| v as u8 as i8 as i16 as u16 as u32)), // MOVSBH
            0x0b => Some(self.op_mov_xf(bus, 0, 1, |_, v| v & 0xff)), // MOVZBH
            0x0c => Some(self.op_mov_xf(bus, 0, 2, |_, v| v as u8 as i8 as i32 as u32)), // MOVSBW
            0x0d => Some(self.op_mov_xf(bus, 0, 2, |_, v| v & 0xff)), // MOVZBW
            0x1c => Some(self.op_mov_xf(bus, 1, 2, |_, v| v as u16 as i16 as i32 as u32)), // MOVSHW
            0x1d => Some(self.op_mov_xf(bus, 1, 2, |_, v| v & 0xffff)), // MOVZHW
            0x19 => Some(self.movt(bus, 1, 0)),                       // MOVTHB
            0x29 => Some(self.movt(bus, 2, 0)),                       // MOVTWB
            0x2b => Some(self.movt(bus, 2, 1)),                       // MOVTWH
            0x2c => Some(self.op_mov_xf(bus, 2, 2, |_, v| v.swap_bytes())), // RVBYT
            0x02 => Some(self.op_stpr(bus)),                          // STPR
            0x12 => Some(self.op_ldpr(bus)),                          // LDPR
            0x41 => Some(self.op_xch(bus, 0)),                        // XCHB
            0x43 => Some(self.op_xch(bus, 1)),                        // XCHH
            0x45 => Some(self.op_xch(bus, 2)),                        // XCHW
            0x47 => Some(self.op_setf(bus)),                          // SETF
            0x48 => Some(self.op_bsr(bus)),                           // BSR
            0x49 => Some(self.op_call(bus)),                          // CALL
            0x4a => Some(self.op_updpsw(bus, 0xffff)),                // UPDPSWH
            0x50 => Some(self.alu_rmw(bus, 0, 0, |c, d, s| c.alu_rem(d, s, 0, true))), // REMB
            0x51 => Some(self.alu_rmw(bus, 0, 0, |c, d, s| c.alu_rem(d, s, 0, false))), // REMUB
            0x52 => Some(self.alu_rmw(bus, 1, 1, |c, d, s| c.alu_rem(d, s, 1, true))), // REMH
            0x53 => Some(self.alu_rmw(bus, 1, 1, |c, d, s| c.alu_rem(d, s, 1, false))), // REMUH
            0x54 => Some(self.alu_rmw(bus, 2, 2, |c, d, s| c.alu_rem(d, s, 2, true))), // REMW
            0x5d => self.op_5d(bus),                                  // EXTBF.L bit-field group
            0x55 => Some(self.alu_rmw(bus, 2, 2, |c, d, s| c.alu_rem(d, s, 2, false))), // REMUW
            0x40 => Some(self.op_movea(bus, 0)),                      // MOVEAB
            0x42 => Some(self.op_movea(bus, 1)),                      // MOVEAH
            0x38 => Some(self.op_not(bus, 0)),                        // NOTB
            0x39 => Some(self.op_neg(bus, 0)),                        // NEGB
            0x3a => Some(self.op_not(bus, 1)),                        // NOTH
            0x3b => Some(self.op_neg(bus, 1)),                        // NEGH
            0x3c => Some(self.op_not(bus, 2)),                        // NOTW
            0x3d => Some(self.op_neg(bus, 2)),                        // NEGW
            0x44 => Some(self.op_movea(bus, 2)),                      // MOVEAW
            0x13 => Some(self.op_updpsw(bus, 0xff_ffff)),             // UPDPSWW

            // Two-operand ALU block 0x80-0xBD. Column = operation, low bit of
            // the row picks byte/halfword/word (0/1/2) for the arithmetic ops.
            0x80 => Some(self.alu_rmw(bus, 0, 0, |c, d, s| c.alu_add(d, s, 0, 0))), // ADDB
            0x82 => Some(self.alu_rmw(bus, 1, 1, |c, d, s| c.alu_add(d, s, 0, 1))), // ADDH
            0x84 => Some(self.alu_rmw(bus, 2, 2, |c, d, s| c.alu_add(d, s, 0, 2))), // ADDW
            0x90 => Some(self.alu_rmw(bus, 0, 0, |c, d, s| {
                let y = c.cy as u32;
                c.alu_add(d, s, y, 0)
            })), // ADDCB
            0x92 => Some(self.alu_rmw(bus, 1, 1, |c, d, s| {
                let y = c.cy as u32;
                c.alu_add(d, s, y, 1)
            })), // ADDCH
            0x94 => Some(self.alu_rmw(bus, 2, 2, |c, d, s| {
                let y = c.cy as u32;
                c.alu_add(d, s, y, 2)
            })), // ADDCW
            0xa8 => Some(self.alu_rmw(bus, 0, 0, |c, d, s| c.alu_sub(d, s, 0, 0))), // SUBB
            0xaa => Some(self.alu_rmw(bus, 1, 1, |c, d, s| c.alu_sub(d, s, 0, 1))), // SUBH
            0xac => Some(self.alu_rmw(bus, 2, 2, |c, d, s| c.alu_sub(d, s, 0, 2))), // SUBW
            0x98 => Some(self.alu_rmw(bus, 0, 0, |c, d, s| {
                let y = c.cy as u32;
                c.alu_sub(d, s, y, 0)
            })), // SUBCB
            0x9a => Some(self.alu_rmw(bus, 1, 1, |c, d, s| {
                let y = c.cy as u32;
                c.alu_sub(d, s, y, 1)
            })), // SUBCH
            0x9c => Some(self.alu_rmw(bus, 2, 2, |c, d, s| {
                let y = c.cy as u32;
                c.alu_sub(d, s, y, 2)
            })), // SUBCW
            0x88 => Some(self.alu_rmw(bus, 0, 0, |c, d, s| c.alu_logic(d | s, 0))), // ORB
            0x8a => Some(self.alu_rmw(bus, 1, 1, |c, d, s| c.alu_logic(d | s, 1))), // ORH
            0x8c => Some(self.alu_rmw(bus, 2, 2, |c, d, s| c.alu_logic(d | s, 2))), // ORW
            0xa0 => Some(self.alu_rmw(bus, 0, 0, |c, d, s| c.alu_logic(d & s, 0))), // ANDB
            0xa2 => Some(self.alu_rmw(bus, 1, 1, |c, d, s| c.alu_logic(d & s, 1))), // ANDH
            0xa4 => Some(self.alu_rmw(bus, 2, 2, |c, d, s| c.alu_logic(d & s, 2))), // ANDW
            0xb0 => Some(self.alu_rmw(bus, 0, 0, |c, d, s| c.alu_logic(d ^ s, 0))), // XORB
            0xb2 => Some(self.alu_rmw(bus, 1, 1, |c, d, s| c.alu_logic(d ^ s, 1))), // XORH
            0xb4 => Some(self.alu_rmw(bus, 2, 2, |c, d, s| c.alu_logic(d ^ s, 2))), // XORW
            0x81 => Some(self.alu_rmw(bus, 0, 0, |c, d, s| c.alu_mul(d, s, 0, true))), // MULB
            0x83 => Some(self.alu_rmw(bus, 1, 1, |c, d, s| c.alu_mul(d, s, 1, true))), // MULH
            0x85 => Some(self.alu_rmw(bus, 2, 2, |c, d, s| c.alu_mul(d, s, 2, true))), // MULW
            0x86 => Some(self.op_mulx(bus, true)),                                  // MULX
            0x91 => Some(self.alu_rmw(bus, 0, 0, |c, d, s| c.alu_mul(d, s, 0, false))), // MULUB
            0x93 => Some(self.alu_rmw(bus, 1, 1, |c, d, s| c.alu_mul(d, s, 1, false))), // MULUH
            0x95 => Some(self.alu_rmw(bus, 2, 2, |c, d, s| c.alu_mul(d, s, 2, false))), // MULUW
            0x96 => Some(self.op_mulx(bus, false)),                                 // MULUX
            0xa1 => Some(self.alu_rmw(bus, 0, 0, |c, d, s| c.alu_div(d, s, 0, true))), // DIVB
            0xa3 => Some(self.alu_rmw(bus, 1, 1, |c, d, s| c.alu_div(d, s, 1, true))), // DIVH
            0xa5 => Some(self.alu_rmw(bus, 2, 2, |c, d, s| c.alu_div(d, s, 2, true))), // DIVW
            0xa6 => self.op_divx(bus, true),                                        // DIVX
            0xb1 => Some(self.alu_rmw(bus, 0, 0, |c, d, s| c.alu_div(d, s, 0, false))), // DIVUB
            0xb3 => Some(self.alu_rmw(bus, 1, 1, |c, d, s| c.alu_div(d, s, 1, false))), // DIVUH
            0xb5 => Some(self.alu_rmw(bus, 2, 2, |c, d, s| c.alu_div(d, s, 2, false))), // DIVUW
            0xb6 => self.op_divx(bus, false),                                       // DIVUX
            0xb8 => Some(self.op_cmp(bus, 0)),                                      // CMPB
            0xba => Some(self.op_cmp(bus, 1)),                                      // CMPH
            0xbc => Some(self.op_cmp(bus, 2)),                                      // CMPW
            0xa9 => Some(self.op_shl(bus, 0)),                                      // SHLB
            0xab => Some(self.op_shl(bus, 1)),                                      // SHLH
            0xad => Some(self.op_shl(bus, 2)),                                      // SHLW
            0xb9 => Some(self.op_sha(bus, 0)),                                      // SHAB
            0xbb => Some(self.op_sha(bus, 1)),                                      // SHAH
            0xbd => Some(self.op_sha(bus, 2)),                                      // SHAW
            0x89 => Some(self.op_rot(bus, 0)),                                      // ROTB
            0x8b => Some(self.op_rot(bus, 1)),                                      // ROTH
            0x8d => Some(self.op_rot(bus, 2)),                                      // ROTW
            0x99 => Some(self.op_rotc(bus, 0)),                                     // ROTCB
            0x9b => Some(self.op_rotc(bus, 1)),                                     // ROTCH
            0x9d => Some(self.op_rotc(bus, 2)),                                     // ROTCW
            0x87 => Some(self.op_test1(bus)),                                       // TEST1
            0x97 => Some(self.op_bit(bus, 0)),                                      // SET1
            0xa7 => Some(self.op_bit(bus, 1)),                                      // CLR1
            0xb7 => Some(self.op_bit(bus, 2)),                                      // NOT1

            0x58 => self.op_string_group(bus, 1), // byte string group
            0x59 => Some(match self.subop_group(bus) {
                0x00 => self.op_adddc(bus),  // ADDDC
                0x01 => self.op_subdc(bus),  // SUBDC
                0x02 => self.op_subrdc(bus), // SUBRDC
                0x10 => self.op_cvtdpz(bus), // CVTDPZ
                0x18 => self.op_cvtdzp(bus), // CVTDZP
                _ => return None,
            }),
            0x5a => self.op_string_group(bus, 2), // halfword string group
            0x5b => Some(match self.subop_group(bus) {
                0x00 => self.op_schbs(bus, false), // SCH0BSU
                0x02 => self.op_schbs(bus, true),  // SCH1BSU
                0x08 => self.op_movbs(bus, false), // MOVBSU
                0x09 => self.op_movbs(bus, true),  // MOVBSD
                _ => return None,
            }), // bit-string group
            0x5c => self.op_fp_arith(bus),        // single-precision FP arithmetic
            0x5f => self.op_fp_conv(bus),         // FP conversions
            // Single-operand INC/DEC (0xD0-0xD5 dec, 0xD8-0xDD inc); the low bit
            // is `modm`, the size follows byte/halfword/word.
            0xd0 => Some(self.op_incdec(bus, 0, false, false)), // DECB
            0xd1 => Some(self.op_incdec(bus, 0, true, false)),
            0xd2 => Some(self.op_incdec(bus, 1, false, false)), // DECH
            0xd3 => Some(self.op_incdec(bus, 1, true, false)),
            0xd4 => Some(self.op_incdec(bus, 2, false, false)), // DECW
            0xd5 => Some(self.op_incdec(bus, 2, true, false)),
            0xd8 => Some(self.op_incdec(bus, 0, false, true)), // INCB
            0xd9 => Some(self.op_incdec(bus, 0, true, true)),
            0xda => Some(self.op_incdec(bus, 1, false, true)), // INCH
            0xdb => Some(self.op_incdec(bus, 1, true, true)),
            0xdc => Some(self.op_incdec(bus, 2, false, true)), // INCW
            0xdd => Some(self.op_incdec(bus, 2, true, true)),
            0xcd => Some(1),                    // NOP
            0x00 => Some(self.op_halt()),       // HALT: resume only through an enabled IRQ
            0xcc => Some(self.op_dispose(bus)), // DISPOSE
            0xca => {
                self.reg[PC] = self.pop32(bus);
                Some(0)
            } // RSR
            0xc6 => Some(self.op_dbcc(bus, false)), // DBcc group
            0xc7 => Some(self.op_dbcc(bus, true)), // DBcc/TB group

            // Format-3 stack / subroutine block (0xE0-0xEF, F0-F5). Low bit = modm.
            0xe0 => Some(self.op_tasi(bus, false)),
            0xe1 => Some(self.op_tasi(bus, true)),
            0xe2 => Some(self.op_ret(bus, false)),
            0xe3 => Some(self.op_ret(bus, true)),
            0xe4 => Some(self.op_popm(bus, false)),
            0xe5 => Some(self.op_popm(bus, true)),
            0xe6 => Some(self.op_pop(bus, false)),
            0xe7 => Some(self.op_pop(bus, true)),
            0xe8 => Some(self.op_jsr(bus, false)),
            0xe9 => Some(self.op_jsr(bus, true)),
            0xec => Some(self.op_pushm(bus, false)),
            0xed => Some(self.op_pushm(bus, true)),
            0xee => Some(self.op_push(bus, false)),
            0xef => Some(self.op_push(bus, true)),
            0xf0 => Some(self.op_test(bus, 0, false)), // TESTB
            0xf1 => Some(self.op_test(bus, 0, true)),
            0xf2 => Some(self.op_test(bus, 1, false)), // TESTH
            0xf3 => Some(self.op_test(bus, 1, true)),
            0xf4 => Some(self.op_test(bus, 2, false)), // TESTW
            0xf5 => Some(self.op_test(bus, 2, true)),
            0xf6 => Some(self.op_getpsw(bus, false)),
            0xf7 => Some(self.op_getpsw(bus, true)),
            0xea => Some(self.op_reti(bus, false)), // RETIU
            0xeb => Some(self.op_reti(bus, true)),
            0xfa => Some(self.op_reti(bus, false)), // RETIS
            0xfb => Some(self.op_reti(bus, true)),
            0xde => Some(self.op_prepare(bus, false)),
            0xdf => Some(self.op_prepare(bus, true)),
            0x60..=0x7f => self.op_branch(bus, op), // relative branch block
            0x20 => Some(self.op_in(bus, 0)),       // INB
            0x21 => Some(self.op_out(bus, 0)),      // OUTB
            0x22 => Some(self.op_in(bus, 1)),       // INH
            0x23 => Some(self.op_out(bus, 1)),      // OUTH
            0x24 => Some(self.op_in(bus, 2)),       // INW
            0x25 => Some(self.op_out(bus, 2)),      // OUTW
            _ => None,
        }
    }
}

#[cfg(test)]
mod unary_tests {
    use crate::V60;

    #[test]
    fn unary_value_helpers_set_the_documented_flags() {
        let mut cpu = V60::new();

        cpu.cy = true;
        cpu.ov = true;
        let not_byte = !0x55u32 & 0xff;
        cpu.ov = false;
        cpu.set_szl(not_byte, 0);
        assert_eq!(not_byte, 0xaa);
        assert!(cpu.cy, "NOT must preserve CY");
        assert!(!cpu.ov);
        assert!(cpu.s);
        assert!(!cpu.z);

        let neg_byte = cpu.alu_sub(0, 0x80, 0, 0);
        cpu.cy = neg_byte != 0;
        assert_eq!(neg_byte, 0x80);
        assert!(cpu.cy);
        assert!(cpu.ov, "NEG of byte MIN must overflow");
        assert!(cpu.s);
        assert!(!cpu.z);

        let neg_zero = cpu.alu_sub(0, 0, 0, 2);
        cpu.cy = neg_zero != 0;
        assert_eq!(neg_zero, 0);
        assert!(!cpu.cy);
        assert!(!cpu.ov);
        assert!(!cpu.s);
        assert!(cpu.z);
    }
}
