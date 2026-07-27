//! Dev-only differential check: run the interpreter and the dynarec over the
//! same bus traffic and diff architectural state.
//!
//! The interpreter runs first against the real bus behind `RecordBus`, which
//! logs every byte read and written. The dynarec then runs the same cycle
//! budget against `ReplayBus`, which serves recorded reads by address and
//! asserts that every write matches the interpreter's stream exactly, in
//! order. After each chunk the two CPUs' architectural state is compared.
//! Any divergence -- a mislowered opcode, a wrong cycle count, a missing
//! side effect -- panics with the differing field.
//!
//! Enabled in the test suite with `I960_DUALRUN=1`. Limitations: external
//! stalls are not replayed (the mock buses never raise them) and read values
//! must be stable per address, so buses with destructive-read MMIO are out of
//! scope. That covers every bus in the i960 test suite, which is what this
//! exists to check.

use std::collections::{HashMap, VecDeque};

use crate::bus::Bus;
use crate::cpu::defs::I960Cpu;

/// Cycles per interpreter/JIT pair before the state diff runs.
const CHUNK: i32 = 64;

struct RecordBus<'a, 'b, B: Bus> {
    inner: &'a mut B,
    reads: &'b mut HashMap<u32, u8>,
    writes: &'b mut Vec<(u32, u8)>,
}

impl<B: Bus> RecordBus<'_, '_, B> {
    fn rec_read(&mut self, addr: u32, v: u8) {
        if let Some(old) = self.reads.insert(addr, v) {
            assert_eq!(
                old, v,
                "dualrun: read of {addr:08X} changed value between accesses; \
                 this bus is outside what dualrun can check"
            );
        }
    }
}

impl<B: Bus> Bus for RecordBus<'_, '_, B> {
    fn read_byte(&mut self, addr: u32) -> u8 {
        let v = self.inner.read_byte(addr);
        self.rec_read(addr, v);
        v
    }

    fn write_byte(&mut self, addr: u32, val: u8) {
        self.inner.write_byte(addr, val);
        self.writes.push((addr, val));
    }

    fn take_irq_lines(&mut self) -> Option<[bool; 4]> {
        self.inner.take_irq_lines()
    }

    fn take_stall(&mut self) -> bool {
        self.inner.take_stall()
    }

    fn burst_capable(&self, addr: u32) -> bool {
        self.inner.burst_capable(addr)
    }

    fn code_epoch(&self, page: u32) -> u64 {
        self.inner.code_epoch(page)
    }
}

struct ReplayBus {
    reads: HashMap<u32, u8>,
    writes: VecDeque<(u32, u8)>,
}

impl Bus for ReplayBus {
    fn read_byte(&mut self, addr: u32) -> u8 {
        *self
            .reads
            .get(&addr)
            .unwrap_or_else(|| panic!("dualrun: JIT read {addr:08X}, which the interpreter never read"))
    }

    fn write_byte(&mut self, addr: u32, val: u8) {
        match self.writes.pop_front() {
            Some((a, v)) if a == addr && v == val => {}
            Some((a, v)) => panic!(
                "dualrun: write divergence: JIT wrote {val:02X} to {addr:08X}, \
                 interpreter wrote {v:02X} to {a:08X}"
            ),
            None => panic!(
                "dualrun: JIT wrote {val:02X} to {addr:08X}, but the interpreter \
                 made no further writes"
            ),
        }
    }
}

/// Runs `cycles` on both engines as described above; leaves `cpu` with the
/// interpreter's resulting state (the authoritative one).
pub fn run<B: Bus + 'static>(cpu: &mut I960Cpu, bus: &mut B, cycles: i32) {
    // Single-instruction blocks: the JIT then stops after exactly the cycles
    // the interpreter spent (it can only exit between blocks), so the two
    // consume identical budgets per chunk and their read/write streams line
    // up one for one.
    super::BLOCK_CAP.store(1, std::sync::atomic::Ordering::Relaxed);
    let mut interp = cpu.clone();
    let mut jit = cpu.clone();
    let mut reads: HashMap<u32, u8> = HashMap::new();

    let mut remaining = cycles;
    while remaining > 0 {
        let step = remaining.min(CHUNK);
        let mut writes = Vec::new();
        {
            let mut rec = RecordBus {
                inner: &mut *bus,
                reads: &mut reads,
                writes: &mut writes,
            };
            interp.execute_run(&mut rec, step);
        }
        let n_writes = writes.len();
        let mut rep = ReplayBus {
            reads: reads.clone(),
            writes: writes.into_iter().collect(),
        };
        jit.execute_run_jit(&mut rep, step);
        assert!(
            rep.writes.is_empty(),
            "dualrun: interpreter made {n_writes} writes this chunk, JIT left {} unmatched",
            rep.writes.len()
        );
        diff(&interp, &jit);
        remaining -= step;
    }

    *cpu = interp;
}

fn diff(a: &I960Cpu, b: &I960Cpu) {
    macro_rules! chk {
        ($field:ident) => {
            assert!(
                a.$field == b.$field,
                "dualrun: {} diverged: interp={:?} jit={:?}",
                stringify!($field),
                a.$field,
                b.$field
            )
        };
    }
    chk!(r);
    chk!(rcache);
    chk!(rcache_frame_addr);
    chk!(rcache_pos);
    chk!(fp);
    chk!(sat);
    chk!(prcb);
    chk!(pc);
    chk!(ac);
    chk!(ip);
    chk!(pip);
    chk!(icr);
    chk!(tmr);
    chk!(tcr);
    chk!(trr);
    chk!(icount);
    chk!(stalled);
    chk!(stall_state);
    chk!(immediate_irq);
    chk!(immediate_vector);
    chk!(immediate_pri);
    chk!(pending_irq_check);
    chk!(deferred_vector);
    chk!(irq_line_state);
}
