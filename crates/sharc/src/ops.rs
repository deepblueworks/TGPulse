//! Instruction decode and execution.
//!
//! Dispatch mirrors the reference exactly: the top nine bits of the 48-bit opcode
//! (`opcode[47:39]`) index a 512-entry table, built by matching each index
//! (shifted into a 16-bit `op = idx << 7`) against the (mask, value) pairs
//! below.

use std::sync::LazyLock;

use crate::state::Sharc;
use crate::SharcBus;

/// One decoded instruction class -- the Rust stand-in for the reference handler
/// function pointers.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum Op {
    ComputeDregDmDregPm,
    Compute,
    ComputeUregDmpmPremod,
    ComputeUregDmpmPostmod,
    ComputeDmToDregImmmod,
    ComputeDregToDmImmmod,
    ComputePmToDregImmmod,
    ComputeDregToPmImmmod,
    ComputeUregToUreg,
    ImmShiftDregDmpm,
    ImmShift,
    ComputeModify,
    DirectJump,
    DirectCall,
    RelativeJump,
    RelativeCall,
    IndirectJump,
    IndirectCall,
    RelativeJumpCompute,
    RelativeCallCompute,
    IndirectJumpComputeDregDm,
    RelativeJumpComputeDregDm,
    Rts,
    Rti,
    DoUntilCounterImm,
    DoUntilCounterUreg,
    DoUntil,
    DmToUregDirect,
    UregToDmDirect,
    PmToUregDirect,
    UregToPmDirect,
    DmToUregIndirect,
    UregToDmIndirect,
    PmToUregIndirect,
    UregToPmIndirect,
    ImmToDmpm,
    ImmToUreg,
    SysregBitop,
    Modify,
    BitReverse,
    PushPopStacks,
    Nop,
    Idle,
    Unimplemented,
}

const TABLE: &[(u16, u16, Op)] = &[
    (0xe000, 0x2000, Op::ComputeDregDmDregPm),
    (0xff00, 0x0100, Op::Compute),
    (0xf000, 0x4000, Op::ComputeUregDmpmPremod),
    (0xf000, 0x5000, Op::ComputeUregDmpmPostmod),
    (0xf180, 0x6000, Op::ComputeDmToDregImmmod),
    (0xf180, 0x6080, Op::ComputeDregToDmImmmod),
    (0xf180, 0x6100, Op::ComputePmToDregImmmod),
    (0xf180, 0x6180, Op::ComputeDregToPmImmmod),
    (0xf000, 0x7000, Op::ComputeUregToUreg),
    (0xf000, 0x8000, Op::ImmShiftDregDmpm),
    (0xff00, 0x0200, Op::ImmShift),
    (0xff00, 0x0400, Op::ComputeModify),
    (0xff80, 0x0600, Op::DirectJump),
    (0xff80, 0x0680, Op::DirectCall),
    (0xff80, 0x0700, Op::RelativeJump),
    (0xff80, 0x0780, Op::RelativeCall),
    (0xff80, 0x0800, Op::IndirectJump),
    (0xff80, 0x0880, Op::IndirectCall),
    (0xff80, 0x0900, Op::RelativeJumpCompute),
    (0xff80, 0x0980, Op::RelativeCallCompute),
    (0xe000, 0xc000, Op::IndirectJumpComputeDregDm),
    (0xe000, 0xe000, Op::RelativeJumpComputeDregDm),
    (0xff00, 0x0a00, Op::Rts),
    (0xff00, 0x0b00, Op::Rti),
    (0xff00, 0x0c00, Op::DoUntilCounterImm),
    (0xff00, 0x0d00, Op::DoUntilCounterUreg),
    (0xff00, 0x0e00, Op::DoUntil),
    (0xff00, 0x1000, Op::DmToUregDirect),
    (0xff00, 0x1100, Op::UregToDmDirect),
    (0xff00, 0x1200, Op::PmToUregDirect),
    (0xff00, 0x1300, Op::UregToPmDirect),
    (0xf100, 0xa000, Op::DmToUregIndirect),
    (0xf100, 0xa100, Op::UregToDmIndirect),
    (0xf100, 0xb000, Op::PmToUregIndirect),
    (0xf100, 0xb100, Op::UregToPmIndirect),
    (0xf000, 0x9000, Op::ImmToDmpm),
    (0xff00, 0x0f00, Op::ImmToUreg),
    (0xff00, 0x1400, Op::SysregBitop),
    (0xff80, 0x1600, Op::Modify),
    (0xff80, 0x1680, Op::BitReverse),
    (0xff00, 0x1700, Op::PushPopStacks),
    (0xff80, 0x0000, Op::Nop),
    (0xff80, 0x0080, Op::Idle),
];


/// The instruction class of a 48-bit opcode word (the JIT decodes at compile
/// time through the same table the interpreter consults per instruction).
pub(crate) fn decode(opcode: u64) -> Op {
    DISPATCH[((opcode >> 39) & 0x1ff) as usize]
}

static DISPATCH: LazyLock<[Op; 512]> = LazyLock::new(|| {
    let mut t = [Op::Unimplemented; 512];
    for (i, slot) in t.iter_mut().enumerate() {
        let op = (i as u16) << 7;
        for &(mask, value, handler) in TABLE {
            if (mask & op) == value {
                *slot = handler;
                break;
            }
        }
    }
    t
});

// --- opcode field extractors (from sharcinternal.ipp) ---
#[inline]
fn sext(v: u32, bits: u32) -> u32 {
    let shift = 32 - bits;
    (((v << shift) as i32) >> shift) as u32
}
#[inline]
fn op_cond(op: u64) -> u32 {
    ((op >> 33) & 0x1f) as u32
}
#[inline]
fn op_compute(op: u64) -> u32 {
    (op & 0x7fffff) as u32
}
#[inline]
fn op_ureg_src(op: u64) -> usize {
    ((op >> 36) & 0xff) as usize
}
#[inline]
fn op_ureg_dst(op: u64) -> usize {
    ((op >> 23) & 0xff) as usize
}
/// The universal-register field of the direct / immediate / indirect move
/// forms. Unlike the compute+move classes (which use bit 23), these carry it
/// at bit 32 -- getting this wrong silently drops every DAG register load.
#[inline]
fn op_ureg_move(op: u64) -> usize {
    ((op >> 32) & 0xff) as usize
}
#[inline]
fn op_cond_ureg(op: u64) -> u32 {
    ((op >> 31) & 0x1f) as u32
}
#[inline]
fn op_dmi(op: u64) -> usize {
    ((op >> 41) & 0x7) as usize
}
#[inline]
fn op_dmm(op: u64) -> usize {
    ((op >> 38) & 0x7) as usize
}
#[inline]
fn op_pmi(op: u64) -> usize {
    ((op >> 30) & 0x7) as usize
}
#[inline]
fn op_pmm(op: u64) -> usize {
    ((op >> 27) & 0x7) as usize
}
#[inline]
fn op_jump_j(op: u64) -> bool {
    (op >> 26) & 1 != 0
}
#[inline]
fn op_jump_la(op: u64) -> bool {
    (op >> 38) & 1 != 0
}
#[inline]
fn op_jump_ci(op: u64) -> bool {
    (op >> 24) & 1 != 0
}

impl Sharc {
    /// Runs the core for approximately `cycles` instructions.
    pub fn execute<B: SharcBus>(&mut self, bus: &mut B, cycles: i32) {
        #[cfg(feature = "jit")]
        if self.jit_enabled {
            crate::jit::execute(self, bus, cycles);
            return;
        }
        self.icount = cycles;
        while self.icount > 0 && !self.idle {
            self.step(bus);
        }
    }

    /// One interpreter loop iteration: interrupt check, FIFO-flag refresh,
    /// pipeline advance, fetch, hardware-loop close, dispatch. Also the JIT's
    /// per-instruction path for pipeline states a compiled block cannot cover
    /// (the two instructions behind a delayed branch).
    pub fn step<B: SharcBus>(&mut self, bus: &mut B) {
        // An interrupt vectors out of whatever the microcode is doing --
        // including its own `DO... UNTIL FOREVER` idle park, which is the
        // only way the Model 2B geometry loop ever resumes.
        if self.irq_pending != 0 {
            self.check_interrupts();
        }
        // Refresh the FIFO status flags the microcode polls (FLAG0 = input
        // FIFO empty, FLAG1 = output FIFO full).
        self.flag[0] = bus.fifo_in_empty() as u32;
        self.flag[1] = bus.fifo_out_full() as u32;

        self.pc = self.daddr;
        self.daddr = self.faddr;
        self.faddr = self.nfaddr;
        self.nfaddr = self.nfaddr.wrapping_add(1);

        // Fetch *before* closing a hardware loop: the instruction sitting
        // at the loop's bottom address still executes on the iteration that
        // branches back, so the fetch has to happen at the pre-branch PC.
        //
        let opcode = self.pm_read48(bus, self.pc);
        self.opcode = opcode;

        if self.stky & crate::consts::LSEM == 0 && self.pc == self.laddr_addr {
            self.handle_loop();
        }
        let op = decode(opcode);
        self.dispatch(bus, op);

        self.insns = self.insns.wrapping_add(1);
        self.icount -= 1;
    }

    /// Closes a hardware loop when the PC reaches its bottom address. Either
    /// the loop terminates (pop both stacks and fall through) or it repeats
    /// (jump back to the top, which is the PC stack's top entry).
    pub(crate) fn handle_loop(&mut self) {
        match self.laddr_loop_type {
            0 => {
                // arithmetic condition based
                if self.do_cond(self.laddr_code) {
                    self.pop_loop();
                    self.pop_pc();
                } else {
                    let top = self.pcstk;
                    self.change_pc(top);
                }
            }
            // Counter-based; types 1 and 2 differ only in pipeline unwind,
            // which this interpreter does not model separately.
            1..=3 => {
                self.curlcntr = self.curlcntr.wrapping_sub(1);
                if self.curlcntr == 0 {
                    self.pop_loop();
                    self.pop_pc();
                } else {
                    let top = self.pcstk;
                    self.change_pc(top);
                }
            }
            _ => {}
        }
    }

    #[inline]
    fn change_pc(&mut self, newpc: u32) {
        self.pc = newpc;
        self.daddr = newpc;
        self.faddr = newpc.wrapping_add(1);
        self.nfaddr = newpc.wrapping_add(2);
    }

    #[inline]
    fn change_pc_delayed(&mut self, newpc: u32) {
        // The two instructions already in the pipeline (daddr, faddr) run first.
        self.nfaddr = newpc;
    }

    #[inline]
    fn update_circular_pm(&mut self, i: usize) {
        if self.dag2.l[i] != 0 {
            if self.dag2.i[i] > self.dag2.b[i].wrapping_add(self.dag2.l[i]) {
                self.dag2.i[i] = self.dag2.i[i].wrapping_sub(self.dag2.l[i]);
            } else if self.dag2.i[i] < self.dag2.b[i] {
                self.dag2.i[i] = self.dag2.i[i].wrapping_add(self.dag2.l[i]);
            }
        }
    }
    #[inline]
    fn update_circular_dm(&mut self, i: usize) {
        if self.dag1.l[i] != 0 {
            if self.dag1.i[i] > self.dag1.b[i].wrapping_add(self.dag1.l[i]) {
                self.dag1.i[i] = self.dag1.i[i].wrapping_sub(self.dag1.l[i]);
            } else if self.dag1.i[i] < self.dag1.b[i] {
                self.dag1.i[i] = self.dag1.i[i].wrapping_add(self.dag1.l[i]);
            }
        }
    }

    pub(crate) fn dispatch<B: SharcBus>(&mut self, bus: &mut B, op: Op) {
        let opcode = self.opcode;
        match op {
            Op::Nop => {}

            // IDLE parks the core until an interrupt arrives. The Model 2
            // microcode reaches it between command batches, and running on
            // through it burns the whole quantum re-executing the same
            // instruction instead of leaving the fetch pipeline where the
            // interrupt handler expects it.
            Op::Idle => {
                self.daddr = self.pc;
                self.faddr = self.pc.wrapping_add(1);
                self.nfaddr = self.pc.wrapping_add(2);
                self.idle = true;
            }

            Op::Compute => {
                let cond = op_cond(opcode);
                let compute = op_compute(opcode);
                if self.if_cond(cond) && compute != 0 && !self.compute(compute) {
                    self.note_unimpl(op);
                }
            }

            Op::ComputeDregDmDregPm => {
                let pm_dreg = ((opcode >> 23) & 0xf) as usize;
                let pmm = op_pmm(opcode);
                let pmi = op_pmi(opcode);
                let dm_dreg = ((opcode >> 33) & 0xf) as usize;
                let dmm = op_dmm(opcode);
                let dmi = op_dmi(opcode);
                let pmd = (opcode >> 37) & 1 != 0;
                let dmd = (opcode >> 44) & 1 != 0;
                let compute = op_compute(opcode);

                let parallel_pm = self.r[pm_dreg];
                let parallel_dm = self.r[dm_dreg];
                if compute != 0 && !self.compute(compute) {
                    self.note_unimpl(op);
                }
                if pmd {
                    let addr = self.dag2.i[pmi];
                    self.pm_write32(bus, addr, parallel_pm);
                } else {
                    let addr = self.dag2.i[pmi];
                    self.r[pm_dreg] = self.pm_read32(bus, addr);
                }
                self.dag2.i[pmi] = self.dag2.i[pmi].wrapping_add(self.dag2.m[pmm]);
                self.update_circular_pm(pmi);

                if dmd {
                    let addr = self.dag1.i[dmi];
                    self.dm_write32(bus, addr, parallel_dm);
                } else {
                    let addr = self.dag1.i[dmi];
                    self.r[dm_dreg] = self.dm_read32(bus, addr);
                }
                self.dag1.i[dmi] = self.dag1.i[dmi].wrapping_add(self.dag1.m[dmm]);
                self.update_circular_dm(dmi);
            }

            // Direct and PC-relative jumps/calls share the same 24-bit address
            // field; the relative forms add it (sign-extended) to the PC.
            Op::DirectJump | Op::DirectCall | Op::RelativeJump | Op::RelativeCall => {
                let la = op_jump_la(opcode);
                let ci = op_jump_ci(opcode);
                let j = op_jump_j(opcode);
                let cond = op_cond(opcode);
                let field = (opcode & 0xffffff) as u32;
                let call = op == Op::DirectCall || op == Op::RelativeCall;
                let relative = op == Op::RelativeJump || op == Op::RelativeCall;
                let target = if relative {
                    self.pc.wrapping_add(sext(field, 24))
                } else {
                    field
                };
                if self.if_cond(cond) {
                    if call {
                        // The return address is the next instruction already in
                        // the pipeline: daddr, or nfaddr past the delay slots.
                        let ret = if j { self.nfaddr } else { self.daddr };
                        self.push_pc(ret);
                    } else if ci {
                        self.clear_current_interrupt();
                    }
                    if !call && la {
                        self.pop_pc();
                        self.pop_loop();
                    }
                    if j {
                        self.change_pc_delayed(target);
                    } else {
                        self.change_pc(target);
                    }
                }
            }

            // Indirect forms target PM_REG_I(pmi) + PM_REG_M(pmm), and carry an
            // IF/ELSE bit that runs the compute on the not-taken path.
            Op::IndirectJump | Op::IndirectCall => {
                let la = op_jump_la(opcode);
                let ci = op_jump_ci(opcode);
                let j = op_jump_j(opcode);
                let e = (opcode >> 25) & 1 != 0;
                let pmi = op_pmi(opcode);
                let pmm = op_pmm(opcode);
                let cond = op_cond(opcode);
                let compute = op_compute(opcode);
                let call = op == Op::IndirectCall;
                if ci {
                    self.clear_current_interrupt();
                }
                let target = self.dag2.i[pmi].wrapping_add(self.dag2.m[pmm]);
                if self.if_cond(cond) {
                    if !e && compute != 0 && !self.compute(compute) {
                        self.note_unimpl(op);
                    }
                    if call {
                        let ret = if j { self.nfaddr } else { self.daddr };
                        self.push_pc(ret);
                    } else if la {
                        self.pop_pc();
                        self.pop_loop();
                    }
                    if j {
                        self.change_pc_delayed(target);
                    } else {
                        self.change_pc(target);
                    }
                } else if e && compute != 0 && !self.compute(compute) {
                    self.note_unimpl(op);
                }
            }

            // "Jump, ELSE compute and move": the branch and the compute/move
            // are mutually exclusive here -- taking the jump skips both.
            Op::IndirectJumpComputeDregDm | Op::RelativeJumpComputeDregDm => {
                let d = (opcode >> 44) & 1 != 0;
                let dmi = op_dmi(opcode);
                let dmm = op_dmm(opcode);
                let cond = op_cond(opcode);
                let dreg = ((opcode >> 23) & 0xf) as usize;
                if self.if_cond(cond) {
                    let target = if op == Op::IndirectJumpComputeDregDm {
                        self.dag2.i[op_pmi(opcode)].wrapping_add(self.dag2.m[op_pmm(opcode)])
                    } else {
                        // 6-bit signed PC-relative displacement.
                        self.pc
                            .wrapping_add(sext(((opcode >> 27) & 0x3f) as u32, 6))
                    };
                    self.change_pc(target);
                } else {
                    let compute = op_compute(opcode);
                    let parallel = self.r[dreg];
                    if compute != 0 && !self.compute(compute) {
                        self.note_unimpl(op);
                    }
                    let addr = self.dag1.i[dmi];
                    if d {
                        self.dm_write32(bus, addr, parallel);
                    } else {
                        let v = self.dm_read32(bus, addr);
                        self.r[dreg] = v;
                    }
                    self.dag1.i[dmi] = addr.wrapping_add(self.dag1.m[dmm]);
                    self.update_circular_dm(dmi);
                }
            }

            Op::Rts | Op::Rti => {
                let j = op_jump_j(opcode);
                let e = (opcode >> 25) & 1 != 0;
                let cond = op_cond(opcode);
                let compute = op_compute(opcode);
                if self.if_cond(cond) {
                    if op == Op::Rti {
                        self.clear_current_interrupt();
                    }
                    if !e && compute != 0 && !self.compute(compute) {
                        self.note_unimpl(op);
                    }
                    let target = self.pop_pc();
                    if j {
                        self.change_pc_delayed(target);
                    } else {
                        self.change_pc(target);
                    }
                } else if e && compute != 0 && !self.compute(compute) {
                    self.note_unimpl(op);
                }
            }

            Op::ImmToUreg => {
                let ureg = op_ureg_move(opcode);
                let data = opcode as u32;
                self.set_ureg(ureg, data);
            }

            Op::DmToUregDirect | Op::PmToUregDirect => {
                let ureg = op_ureg_move(opcode);
                let addr = opcode as u32;
                let v = if op == Op::PmToUregDirect {
                    self.pm_read32(bus, addr)
                } else {
                    self.dm_read32(bus, addr)
                };
                self.set_ureg(ureg, v);
            }

            Op::UregToDmDirect | Op::UregToPmDirect => {
                let ureg = op_ureg_move(opcode);
                let addr = opcode as u32;
                let v = self.get_ureg(ureg);
                if op == Op::UregToPmDirect {
                    self.pm_write32(bus, addr, v);
                } else {
                    self.dm_write32(bus, addr, v);
                }
            }

            Op::DmToUregIndirect | Op::PmToUregIndirect => {
                // Index + 32-bit literal offset; the index register is not
                // modified by these forms.
                let ureg = op_ureg_move(opcode);
                let i = ((opcode >> 41) & 0x7) as usize;
                let offset = opcode as u32;
                let pm = op == Op::PmToUregIndirect;
                let addr = if pm { self.dag2.i[i] } else { self.dag1.i[i] }.wrapping_add(offset);
                let v = if pm {
                    self.pm_read32(bus, addr)
                } else {
                    self.dm_read32(bus, addr)
                };
                self.set_ureg(ureg, v);
            }

            Op::UregToDmIndirect | Op::UregToPmIndirect => {
                let ureg = op_ureg_move(opcode);
                let i = ((opcode >> 41) & 0x7) as usize;
                let offset = opcode as u32;
                let pm = op == Op::UregToPmIndirect;
                let v = self.get_ureg(ureg);
                let addr = if pm { self.dag2.i[i] } else { self.dag1.i[i] }.wrapping_add(offset);
                if pm {
                    self.pm_write32(bus, addr, v);
                } else {
                    self.dm_write32(bus, addr, v);
                }
            }

            Op::ComputeUregToUreg => {
                // ureg <- ureg, with an optional compute in parallel.
                let src = op_ureg_src(opcode);
                let dst = op_ureg_dst(opcode);
                let compute = op_compute(opcode);
                if !self.if_cond(op_cond_ureg(opcode)) {
                    return;
                }
                let v = self.get_ureg(src);
                if compute != 0 && !self.compute(compute) {
                    self.note_unimpl(op);
                }
                self.set_ureg(dst, v);
            }

            Op::ImmShift => {
                let cond = op_cond(opcode);
                let data = (((opcode >> 8) & 0xff) | ((opcode >> 19) & 0xf00)) as i32;
                let shiftop = ((opcode >> 16) & 0x3f) as u32;
                let rn = ((opcode >> 4) & 0xf) as usize;
                let rx = (opcode & 0xf) as usize;
                if self.if_cond(cond) && !self.shift_imm(shiftop, data, rn, rx) {
                    self.note_unimpl(op);
                }
            }

            Op::ComputeUregDmpmPostmod | Op::ComputeUregDmpmPremod => {
                let i = ((opcode >> 41) & 0x7) as usize;
                let m = ((opcode >> 38) & 0x7) as usize;
                let cond = op_cond(opcode);
                let g = (opcode >> 32) & 1 != 0; // PM (true) or DM
                let d = (opcode >> 31) & 1 != 0; // ureg -> memory
                let ureg = ((opcode >> 23) & 0xff) as usize;
                let compute = op_compute(opcode);
                if !self.if_cond(cond) {
                    return;
                }
                // The source ureg must be latched: the compute may change it.
                let parallel = self.get_ureg(ureg);
                if compute != 0 && !self.compute(compute) {
                    self.note_unimpl(op);
                }
                // Pre-modify addresses with I+M without updating I; post-modify
                // uses I then advances it.
                let pre = op == Op::ComputeUregDmpmPremod;
                if g {
                    let base = self.dag2.i[i];
                    let addr = if pre {
                        base.wrapping_add(self.dag2.m[m])
                    } else {
                        base
                    };
                    if d {
                        if ureg == 0xdb {
                            let px = self.px;
                            self.pm_write48(bus, addr, px);
                        } else {
                            self.pm_write32(bus, addr, parallel);
                        }
                    } else if ureg == 0xdb {
                        self.px = self.pm_read48(bus, addr);
                    } else {
                        let v = self.pm_read32(bus, addr);
                        self.set_ureg(ureg, v);
                    }
                    if !pre {
                        self.dag2.i[i] = base.wrapping_add(self.dag2.m[m]);
                        self.update_circular_pm(i);
                    }
                } else {
                    let base = self.dag1.i[i];
                    let addr = if pre {
                        base.wrapping_add(self.dag1.m[m])
                    } else {
                        base
                    };
                    if d {
                        self.dm_write32(bus, addr, parallel);
                    } else {
                        let v = self.dm_read32(bus, addr);
                        self.set_ureg(ureg, v);
                    }
                    if !pre {
                        self.dag1.i[i] = base.wrapping_add(self.dag1.m[m]);
                        self.update_circular_dm(i);
                    }
                }
            }

            Op::ImmToDmpm => {
                let i = ((opcode >> 41) & 0x7) as usize;
                let m = ((opcode >> 38) & 0x7) as usize;
                let g = (opcode >> 37) & 1 != 0;
                let data = opcode as u32;
                if g {
                    let addr = self.dag2.i[i];
                    self.pm_write32(bus, addr, data);
                    self.dag2.i[i] = addr.wrapping_add(self.dag2.m[m]);
                    self.update_circular_pm(i);
                } else {
                    let addr = self.dag1.i[i];
                    self.dm_write32(bus, addr, data);
                    self.dag1.i[i] = addr.wrapping_add(self.dag1.m[m]);
                    self.update_circular_dm(i);
                }
            }

            Op::SysregBitop => {
                let bop = (opcode >> 37) & 0x7;
                let sreg = ((opcode >> 32) & 0xf) as usize;
                let data = opcode as u32;
                let mut src = self.get_ureg(0x70 | sreg);
                match bop {
                    0 => src |= data,
                    1 => src &= !data,
                    2 => src ^= data,
                    4 => {
                        if src & data == data {
                            self.astat |= crate::consts::BTF;
                        } else {
                            self.astat &= !crate::consts::BTF;
                        }
                    }
                    5 => {
                        if src == data {
                            self.astat |= crate::consts::BTF;
                        } else {
                            self.astat &= !crate::consts::BTF;
                        }
                    }
                    _ => self.note_unimpl(op),
                }
                self.set_ureg(0x70 | sreg, src);
            }

            // DO <addr> UNTIL <cond>: pushes the PC and loop stacks and arms
            // the hardware loop that `handle_loop` closes at `laddr`.
            Op::DoUntil | Op::DoUntilCounterImm | Op::DoUntilCounterUreg => {
                let offset = sext((opcode & 0xffffff) as u32, 24);
                let address = self.pc.wrapping_add(offset);
                let distance = (offset as i32).unsigned_abs();
                let (cond, loop_type) = match op {
                    Op::DoUntil => (op_cond(opcode), 0),
                    // Counter-based: run until the loop counter expires. The
                    // type encodes the body length, which decides how the
                    // pipeline unwinds at the bottom.
                    _ => (
                        0xf,
                        match distance {
                            1 => 1,
                            2 => 2,
                            _ => 3,
                        },
                    ),
                };
                match op {
                    Op::DoUntilCounterImm => self.lcntr = ((opcode >> 24) & 0xffff) as u32,
                    Op::DoUntilCounterUreg => {
                        self.lcntr = self.get_ureg(op_ureg_move(opcode));
                    }
                    _ => {}
                }
                self.push_pc_raw();
                self.push_loop();
                self.pcstk = self.pc.wrapping_add(1);
                self.laddr_addr = address;
                self.laddr_code = cond;
                self.laddr_loop_type = loop_type;
            }

            // Compute in parallel with a data-register move whose address
            // modifier is a 6-bit signed immediate. `u` picks post-modify
            // (use I, then advance it) over pre-modify (use I+mod, leave I).
            Op::ComputeDmToDregImmmod
            | Op::ComputeDregToDmImmmod
            | Op::ComputePmToDregImmmod
            | Op::ComputeDregToPmImmmod => {
                let cond = op_cond(opcode);
                let u = (opcode >> 38) & 1 != 0;
                let dreg = ((opcode >> 23) & 0xf) as usize;
                let i = ((opcode >> 41) & 0x7) as usize;
                let mod_ = sext(((opcode >> 27) & 0x3f) as u32, 6);
                let compute = op_compute(opcode);
                let pm = matches!(op, Op::ComputePmToDregImmmod | Op::ComputeDregToPmImmmod);
                let store = matches!(op, Op::ComputeDregToDmImmmod | Op::ComputeDregToPmImmmod);

                // The source register must be latched: the compute may change
                // it before the store reads it.
                let parallel = self.r[dreg];
                if !self.if_cond(cond) {
                    return;
                }
                if compute != 0 && !self.compute(compute) {
                    self.note_unimpl(op);
                }
                let base = if pm { self.dag2.i[i] } else { self.dag1.i[i] };
                let addr = if u { base } else { base.wrapping_add(mod_) };
                if store {
                    if pm {
                        self.pm_write32(bus, addr, parallel);
                    } else {
                        self.dm_write32(bus, addr, parallel);
                    }
                } else {
                    let v = if pm {
                        self.pm_read32(bus, addr)
                    } else {
                        self.dm_read32(bus, addr)
                    };
                    self.r[dreg] = v;
                }
                if u {
                    if pm {
                        self.dag2.i[i] = base.wrapping_add(mod_);
                        self.update_circular_pm(i);
                    } else {
                        self.dag1.i[i] = base.wrapping_add(mod_);
                        self.update_circular_dm(i);
                    }
                }
            }

            _ => self.note_unimpl(op),
        }
    }

    /// Vectors to the highest-priority pending, unmasked interrupt.
    /// Ported from the reference: the vector table is four words
    /// per source at the base of internal program memory.
    pub(crate) fn check_interrupts(&mut self) {
        if self.imask & self.irq_pending == 0
            || self.mode1 & crate::consts::MODE1_IRPTEN == 0
            || self.interrupt_active
        {
            return;
        }
        let which = (self.imask & self.irq_pending).trailing_zeros();

        self.push_pc_raw();
        self.pcstk = if self.idle {
            self.pc.wrapping_add(1)
        } else {
            self.daddr
        };
        self.irptl |= 1 << which;
        // The timer and VIRPT sources also save MODE1/ASTAT.
        if (6..=8).contains(&which) {
            self.push_status();
        }
        self.change_pc(0x20000 + which * 4);
        self.active_irq_num = which as i32;
        self.irq_pending &= !(1 << which);
        self.interrupt_active = true;
        self.idle = false;
    }

    fn clear_current_interrupt(&mut self) {
        if self.active_irq_num >= 0 {
            self.irptl &= !(1 << self.active_irq_num);
        }
        self.active_irq_num = -1;
        self.interrupt_active = false;
    }

    #[inline]
    fn note_unimpl(&mut self, op: Op) {
        self.unimpl_count = self.unimpl_count.wrapping_add(1);
        self.last_unimpl = op;
        self.unimpl_hist[op as usize] += 1;
    }
}
