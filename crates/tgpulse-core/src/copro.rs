//! The Model 2 geometry coprocessor on its own thread.
//!
//! With `Config::multithreaded` set (the default) the board's DSP -- the
//! MB86233 TGP on original/2A, the ADSP-21062 SHARC on 2B, the MB86235 TGPx4
//! on 2C -- runs on a long-lived worker thread instead of sharing the main
//! thread's 64/43-cycle lockstep. With it clear, nothing in this module runs
//! and the machine takes the original single-threaded path byte-for-byte.
//!
//! ## Audit: what the DSPs touch, and who else touches it
//!
//! The three bus implementations (`impl Mb86233Bus/SharcBus/Mb86235Bus for
//! Model2System` in memory.rs) reach this state:
//!
//! * `copro_fifo_in` / `copro_fifo_out` -- the hardware command/result
//!   FIFOs. The i960 also drives both: it pushes command words (0x880000
//!   function port, 0x884000 FIFO port) and pops results (0x884000 read,
//!   with `main_stall` on empty, and the 0x980004 empty flag). SHARED.
//! * `buffer_ram` (128KB display-list buffer) -- written by every DSP (TGP
//!   through its banked window, SHARC/TGPx4 through their external data
//!   space) AND by the i960 (0x900000 window and the geometrizer's
//!   `push_geo_data`), and read by the rasterizer at V-blank. A genuine
//!   dual-port RAM on the board. SHARED, lock-free (see `SharedBuffer`).
//! * `tgp_program_ram` -- read by the TGP every fetch; written by the i960
//!   during a microcode upload (while the TGP is halted). SHARED.
//! * `tgp_data_ram` -- TGP-only scratch. Worker-local, but savestates are
//!   taken on the main thread, so it lives in the shared block for them.
//! * `copro_bank_reg`, `copro_sincos_base`, `copro_atan_base`,
//!   `copro_gpio0`, `copro_inv_base`, `copro_isqrt_base`, `copro_stall` --
//!   TGP-only registers (the math-function units and the atan comparator
//!   pin). Worker-local, shared only for savestates.
//! * `copro_data` / `copro_tables` -- ROM, immutable after load. Shared
//!   read-only through `Arc`.
//! * `copro_halted` -- written by the i960 (`copro_ctl` bit 31), gates the
//!   worker. SHARED.
//! * The DSP core itself: the i960 pokes it directly for microcode upload
//!   and boot (`copro_ctl_w`, `copro_fifo_w`) and, on 2B, through the IOP
//!   window at 0x8C0000 -- which carries the doorbell interrupt that wakes
//!   the SHARC's idle park. SHARED, ordered through a control-op queue.
//! * Bring-up counters (`sharc_reads`, `tgpx4_pops`, ...). Diagnostics
//!   only; synced back for the debugger.
//!
//! State the DSPs do NOT touch (verified against the three bus traits and
//! the cores): i960 interrupt lines (no DSP raises one on Model 2), the
//! geometrizer's input (fed by the i960 alone), `main_stall`, sound,
//! timers, comm, I/O. Those stay on the main thread with no synchronization.
//!
//! ## Design (PCSX2 MTVU-style, per docs/PERFORMANCE.md)
//!
//! The FIFO semantics are the whole contract between the threads; the
//! 64/43-cycle lockstep is not preserved across them. The i960 side keeps
//! its existing stall behaviour exactly: an input FIFO pushed past
//! `COPRO_FIFO_DEPTH` deschedules the i960 in `run_slice` (the main thread
//! never blocks on the worker), and an empty output FIFO read ends the
//! i960's quantum early via `main_stall`. The worker free-runs in batches
//! of `BATCH_CYCLES`, bounded by the same backpressure the hardware
//! applies: it parks when the output FIFO overflows, when the DSP is
//! halted, and -- when the DSP is observably stuck waiting for input (TGP
//! stall retry, TGPx4 `stalled`, SHARC `idle`) -- until the i960 supplies a
//! word or pokes a control op. One mutex plus one condvar covers
//! everything; the i960's own reads go through lock-free FIFO depth mirrors
//! so its per-quantum checks never queue behind a batch.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use crate::roms_db::Board;
use crate::system::{COPRO_FIFO_DEPTH, Model2System};

/// DSP cycles per worker wake-up. Batching is the tuning knob the
/// PERFORMANCE.md plan calls for -- but on this codebase a sweep
/// (4096/1024/256/32/8) measured *faster* the smaller the batch, levelling
/// off below ~16: the FIFOs pace both sides anyway, so a big batch mostly
/// adds hand-off latency (the i960's per-word FIFO pop queues behind the
/// worker's lock) rather than saving synchronization. 8 sits in the flat
/// part of the curve; idle DSP time is handled by the nap, not by batch
/// size.
const BATCH_CYCLES: i32 = 8;

/// How long the worker naps when the DSP spent a whole batch polling FIFO
/// flags without touching anything. A push or doorbell wakes it instantly;
/// the timeout only bounds the cost of a misjudged batch (a DSP doing
/// purely internal math), it can never deadlock the machine.
const SPIN_NAP: Duration = Duration::from_micros(100);

/// The 128KB display-list buffer, dual-ported between the i960 and the
/// geometry coprocessor on the real board. Per-word atomics let both
/// threads treat it exactly like the hardware's dual-port RAM; the FIFO
/// mutex/condvar pairs provide the happens-before edge that orders a
/// result's buffer writes before the i960 pops its FIFO word.
/// Single-threaded mode uses the same storage, so both paths share one
/// implementation.
///
/// `Clone` is a DEEP copy (savestate semantics: a snapshot must not alias
/// the live machine); `share()` is the shallow, aliasing handle the worker
/// runs against.
#[derive(Debug)]
pub struct SharedBuffer(Arc<Vec<AtomicU32>>);

impl SharedBuffer {
    pub fn new(fill: u32, words: usize) -> Self {
        Self(Arc::new(
            (0..words).map(|_| AtomicU32::new(fill)).collect(),
        ))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// The aliasing handle handed to the worker thread.
    pub fn share(&self) -> Self {
        Self(Arc::clone(&self.0))
    }

    pub fn read(&self, idx: u32) -> u32 {
        self.0
            .get(idx as usize)
            .map(|w| w.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    pub fn write(&self, idx: u32, val: u32) {
        if let Some(w) = self.0.get(idx as usize) {
            w.store(val, Ordering::Relaxed);
        }
    }

    /// Byte-offset forms, for the i960's 0x900000 window.
    pub fn read_word(&self, byte_off: u32) -> u32 {
        self.read(byte_off >> 2)
    }

    pub fn write_word(&self, byte_off: u32, val: u32) {
        self.write(byte_off >> 2, val);
    }

    pub fn to_vec(&self) -> Vec<u32> {
        self.0.iter().map(|w| w.load(Ordering::Relaxed)).collect()
    }

    fn from_vec(v: Vec<u32>) -> Self {
        Self(Arc::new(v.into_iter().map(AtomicU32::new).collect()))
    }
}

impl Clone for SharedBuffer {
    /// Deep copy: a snapshot must stay put while the machine runs on.
    fn clone(&self) -> Self {
        Self::from_vec(self.to_vec())
    }
}

// Serialized as a plain word sequence, the same shape `Vec<u32>` had.
impl serde::Serialize for SharedBuffer {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        serde::Serialize::serialize(&self.to_vec(), s)
    }
}

impl<'de> serde::Deserialize<'de> for SharedBuffer {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self::from_vec(<Vec<u32> as serde::Deserialize>::deserialize(
            d,
        )?))
    }
}

/// The coprocessor core the worker owns. `None` is the parking placeholder,
/// the same trick `parked_*` plays on the main thread: the bus needs
/// `&mut CoproInner` while the core needs `&mut self`, so the running core
/// steps out of the struct for the duration of a batch.
#[derive(Clone)]
enum CoproCore {
    None,
    Tgp(Box<mb86233::Mb86233>),
    Sharc(Box<sharc::Sharc>),
    Tgpx4(Box<mb86235::Mb86235>),
}

impl CoproCore {
    fn execute(&mut self, bus: &mut BatchBus, cycles: i32) {
        match self {
            CoproCore::Tgp(c) => c.execute(bus, cycles),
            CoproCore::Sharc(c) => c.execute(bus, cycles),
            CoproCore::Tgpx4(c) => c.execute(bus, cycles),
            CoproCore::None => unreachable!("coprocessor core parked during batch"),
        }
    }
}

/// A board-level poke the i960 makes into the DSP core while the core lives
/// on the worker thread. Queued under the lock and applied by the worker
/// before its next batch, which keeps them ordered against the halt flag
/// exactly the way `copro_ctl_w` orders them against execution on the
/// single-threaded path.
pub(crate) enum ControlOp {
    TgpReset,
    SharcReset,
    /// Boot the SHARC after upload: execution starts at internal RAM.
    SharcBoot,
    SharcDma { index: u32, value: u32 },
    SharcIop { offset: u32, value: u32 },
    Tgpx4Reset,
    Tgpx4Upload { index: u32, value: u32 },
}

impl ControlOp {
    fn apply(self, core: &mut CoproCore, bus: &mut BatchBus) {
        match (self, core) {
            (ControlOp::TgpReset, CoproCore::Tgp(c)) => c.reset(),
            (ControlOp::SharcReset, CoproCore::Sharc(c)) => c.reset(),
            (ControlOp::SharcBoot, CoproCore::Sharc(c)) => {
                // The SHARC begins executing the just-uploaded program at the
                // start of internal RAM.
                c.pc = 0x20004;
                c.daddr = 0x20004;
                c.faddr = 0x20005;
                c.nfaddr = 0x20006;
                c.idle = false;
            }
            (ControlOp::SharcDma { index, value }, CoproCore::Sharc(c)) => {
                c.external_dma_write(bus, index, value & 0xffff)
            }
            (ControlOp::SharcIop { offset, value }, CoproCore::Sharc(c)) => {
                c.external_iop_write(bus, offset, value)
            }
            (ControlOp::Tgpx4Reset, CoproCore::Tgpx4(c)) => c.reset(),
            (ControlOp::Tgpx4Upload { index, value }, CoproCore::Tgpx4(c)) => {
                c.upload_program_half(index, value)
            }
            // A poke meant for a different board's core: harmless, drop it.
            _ => {}
        }
    }
}

/// Everything the DSP bus can reach that the i960 can also reach, plus the
/// worker's own bookkeeping. Guarded by one mutex; the condvar is signalled
/// by the main thread when it changes something the worker waits on.
pub struct CoproShared {
    inner: Mutex<CoproInner>,
    work: Condvar,
    /// Lock-free mirrors of the two FIFO depths, updated under the lock at
    /// every push and pop. The i960 reads them every 64-cycle quantum (the
    /// `run_slice` overflow check and the fifo_control register), and taking
    /// the mutex that often starved it outright: the worker holds the lock
    /// for a whole batch, so a quantum's lock request queued behind
    /// back-to-back batches and the machine livelocked (vstriker). A racing
    /// read can be a word off, which only shifts a stall by a word -- slack
    /// the hardware's own FIFO synchronization has anyway.
    lens: FifoLens,
}

#[derive(Default)]
struct FifoLens {
    in_len: AtomicUsize,
    out_len: AtomicUsize,
}

struct CoproInner {
    core: CoproCore,
    fifo_in: VecDeque<u32>,
    fifo_out: VecDeque<u32>,
    buffer: SharedBuffer,
    tgp_program_ram: Vec<u32>,
    tgp_data_ram: Vec<u32>,
    /// ROM views, `Arc` clones of the system's; read-only after load.
    copro_data: Arc<Vec<u32>>,
    copro_tables: Arc<Vec<u32>>,
    bank_reg: u32,
    sincos_base: u32,
    atan_base: [u32; 4],
    gpio0: bool,
    inv_base: u32,
    isqrt_base: u32,
    stall: bool,
    /// The copro_ctl halt line, mirrored from the main thread.
    halted: bool,
    /// Set when the last batch ended with the DSP stuck on an empty input
    /// pop; the worker then sleeps until the i960 supplies a word. A DSP in
    /// mid-computation shows no stall, so this never parks a busy one.
    sleep_on_input: bool,
    control: VecDeque<ControlOp>,
    /// Savestate stop-the-world: the worker finishes its batch and parks
    /// until cleared.
    paused: bool,
    quit: bool,
    /// True while the worker is inside `Condvar::wait`, so the main thread
    /// only pays for a wake-up when someone is listening.
    worker_waiting: bool,

    // Bring-up counters, mirrored to the system for the debugger.
    sharc_reads: u64,
    sharc_writes: u64,
    sharc_read_addrs: [u64; 4],
    sharc_write_addrs: [u64; 4],
    sharc_write_samples: [u32; 8],
    tgpx4_pops: u64,
    tgpx4_pushes: u64,
    tgpx4_ext_r: u64,
    tgpx4_ext_w: u64,
    tgpx4_rbucket: [u64; 3],
    tgpx4_rsample: [u32; 8],
}

impl CoproInner {
    fn new(
        core: CoproCore,
        buffer: SharedBuffer,
        copro_data: Arc<Vec<u32>>,
        copro_tables: Arc<Vec<u32>>,
    ) -> Self {
        Self {
            core,
            fifo_in: VecDeque::new(),
            fifo_out: VecDeque::new(),
            buffer,
            tgp_program_ram: vec![0; 0x1000],
            tgp_data_ram: vec![0; 0x400],
            copro_data,
            copro_tables,
            bank_reg: 0,
            sincos_base: 0,
            atan_base: [0; 4],
            gpio0: false,
            inv_base: 0,
            isqrt_base: 0,
            stall: false,
            // The TGP idles until the main CPU uploads microcode and boots it.
            halted: true,
            sleep_on_input: false,
            control: VecDeque::new(),
            paused: false,
            quit: false,
            worker_waiting: false,
            sharc_reads: 0,
            sharc_writes: 0,
            sharc_read_addrs: [0; 4],
            sharc_write_addrs: [0; 4],
            sharc_write_samples: [0; 8],
            tgpx4_pops: 0,
            tgpx4_pushes: 0,
            tgpx4_ext_r: 0,
            tgpx4_ext_w: 0,
            tgpx4_rbucket: [0; 3],
            tgpx4_rsample: [0; 8],
        }
    }
}

/// The main thread's handle to the running worker.
pub struct CoproWorker {
    shared: Arc<CoproShared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl CoproWorker {
    fn spawn(shared: Arc<CoproShared>) -> Self {
        let worker_shared = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("copro".into())
            .spawn(move || worker_loop(worker_shared))
            .expect("spawn copro worker");
        Self {
            shared,
            thread: Some(thread),
        }
    }

    fn lock(&self) -> MutexGuard<'_, CoproInner> {
        self.shared.inner.lock().unwrap()
    }

    /// Wakes the worker if it parked. Called after the main thread changed
    /// something the worker's park condition reads.
    fn notify(&self, guard: &MutexGuard<'_, CoproInner>) {
        if guard.worker_waiting {
            self.shared.work.notify_one();
        }
    }

    /// i960 push to the command FIFO (0x880000 / 0x884000 data writes). The
    /// FIFO accepts the word even past its depth -- the producer is
    /// descheduled by `run_slice` after the overflow word, the same rule the
    /// single-threaded path applies.
    pub(crate) fn push_input(&self, val: u32) {
        let mut g = self.lock();
        g.fifo_in.push_back(val);
        self.shared.lens.in_len.fetch_add(1, Ordering::Relaxed);
        g.sleep_on_input = false;
        self.notify(&g);
    }

    /// i960 pop from the result FIFO (0x884000 read). `None` on empty; the
    /// caller raises `main_stall` exactly as on the single-threaded path.
    pub(crate) fn pop_output(&self) -> Option<u32> {
        let mut g = self.lock();
        let v = g.fifo_out.pop_front();
        if v.is_some() {
            self.shared.lens.out_len.fetch_sub(1, Ordering::Relaxed);
        }
        self.notify(&g);
        v
    }

    /// fifo_control_r (0x980004): 1 when the output FIFO is empty.
    pub(crate) fn output_empty(&self) -> bool {
        self.shared.lens.out_len.load(Ordering::Relaxed) == 0
    }

    /// Input FIFO depth, for `run_slice`'s producer-overflow check.
    pub(crate) fn input_len(&self) -> usize {
        self.shared.lens.in_len.load(Ordering::Relaxed)
    }

    /// TGP microcode upload word. The program RAM is board memory, not core
    /// state, so no control op is needed -- just the lock.
    pub(crate) fn write_tgp_program(&self, index: u32, val: u32) {
        let mut g = self.lock();
        if let Some(slot) = g.tgp_program_ram.get_mut(index as usize) {
            *slot = val;
        }
    }

    pub(crate) fn control(&self, op: ControlOp) {
        let mut g = self.lock();
        g.control.push_back(op);
        self.notify(&g);
    }

    pub(crate) fn set_halted(&self, halted: bool) {
        let mut g = self.lock();
        g.halted = halted;
        self.notify(&g);
    }

    fn shutdown(&mut self) {
        {
            let mut g = self.lock();
            g.quit = true;
            g.paused = false;
        }
        self.shared.work.notify_one();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for CoproWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The DSP-side view of the shared block during one batch. Implements all
/// three coprocessor bus traits; the bodies mirror the single-threaded
/// `Model2System` implementations in memory.rs, which stay the reference.
struct BatchBus<'a> {
    io: &'a mut CoproInner,
    lens: &'a FifoLens,
    /// Set when the TGP retried an empty input pop during this batch.
    saw_input_stall: bool,
    /// Shared-state accesses this batch (FIFO words, buffer and ROM reads,
    /// function-unit pokes). Zero means the DSP spent the batch polling
    /// flags: it is waiting for input and the worker can nap until some
    /// arrives, instead of holding the lock against the i960.
    traffic: u64,
}

impl BatchBus<'_> {
    fn pop_in(&mut self) -> Option<u32> {
        let v = self.io.fifo_in.pop_front();
        if v.is_some() {
            self.lens.in_len.fetch_sub(1, Ordering::Relaxed);
        }
        v
    }

    fn push_out(&mut self, data: u32) {
        self.io.fifo_out.push_back(data);
        self.lens.out_len.fetch_add(1, Ordering::Relaxed);
    }
}

// --- Math-function units ---
//
// Table lookups driven by the base registers, shared with the
// single-threaded path in memory.rs so the two cannot drift apart.

pub(crate) fn copro_table(tables: &[u32], index: usize) -> u32 {
    tables.get(index).copied().unwrap_or(0)
}

pub(crate) fn copro_sincos_r(tables: &[u32], base: u32, offset: u32) -> u32 {
    let ang = base.wrapping_add(offset * 0x4000);
    let mut index = (ang & 0x3fff) as usize;
    if ang & 0x4000 != 0 {
        index = (0x4000usize - index).min(0x3fff);
    }
    let mut result = copro_table(tables, index);
    if ang & 0x8000 != 0 {
        result ^= 0x8000_0000;
    }
    result
}

pub(crate) fn copro_inv_r(tables: &[u32], base: u32, offset: u32) -> u32 {
    let index = (((base >> 9) & 0x3ffe) | (offset & 1)) as usize;
    let mut result = copro_table(tables, index | 0x8000);
    let base_exp = ((base >> 23) & 0xff) as u8;
    let exp = ((result >> 23) as u8).wrapping_add(0x7f_u8.wrapping_sub(base_exp));
    result = (result & 0x007f_ffff) | ((exp as u32) << 23);
    if base & 0x8000_0000 != 0 && offset != 0 {
        result |= 0x8000_0000;
    }
    result
}

pub(crate) fn copro_isqrt_r(tables: &[u32], base: u32, offset: u32) -> u32 {
    let index = (0x2000 ^ (((base >> 10) & 0x3ffe) | (offset & 1))) as usize;
    let mut result = copro_table(tables, index | 0xc000);
    let base_exp = ((base >> 24) & 0x7f) as u8;
    let exp = ((result >> 23) as u8).wrapping_add(0x3f_u8.wrapping_sub(base_exp));
    result = (result & 0x807f_ffff) | ((exp as u32) << 23);
    if offset & 1 == 0 {
        result &= 0x7fff_ffff;
    }
    result
}

pub(crate) fn copro_atan_r(tables: &[u32], atan_base: [u32; 4]) -> u32 {
    let ie = 0x88_u8.wrapping_sub((atan_base[3] >> 23) as u8);
    let s0 = atan_base[0] & 0x8000_0000 != 0;
    let s1 = atan_base[1] & 0x8000_0000 != 0;
    let s2 = (atan_base[0] & 0x7fff_ffff) <= (atan_base[1] & 0x7fff_ffff);
    let im = atan_base[3] & 0x7f_ffff;
    let mut index = if ie <= 0x17 {
        ((im | 0x80_0000) >> ie) as usize
    } else {
        0
    };
    if index == 0x4000 {
        index = 0x3fff;
    }
    let mut result = copro_table(tables, index | 0x4000);
    if s0 ^ s1 ^ s2 {
        result >>= 16;
    }
    if s2 {
        result = result.wrapping_add(0x4000);
    }
    if (s0 && !s2) || (s1 && s2) {
        result = result.wrapping_add(0x8000);
    }
    result & 0xffff
}

// The TGP's external data window: the bank register supplies the high byte,
// so the same 16-bit offset reaches either the coprocessor data ROM or the
// buffer RAM it shares with the i960.
fn banked_read(io: &CoproInner, offset: u32) -> u32 {
    let adr = (io.bank_reg & 0xFF_0000) | offset;
    if adr & 0x80_0000 != 0 {
        let masked = adr & (io.copro_data.len() as u32 - 1);
        return io.copro_data[masked as usize];
    }
    if adr & 0x40_0000 != 0 {
        return io.buffer.read(adr & 0x7FFF);
    }
    0
}

/// The writable half of that window. Only the buffer RAM answers; the data
/// ROM is read-only and a write to it goes nowhere.
fn banked_write(io: &CoproInner, offset: u32, data: u32) {
    let adr = (io.bank_reg & 0xFF_0000) | offset;
    if adr & 0x40_0000 != 0 {
        io.buffer.write(adr & 0x7FFF, data);
    }
}

impl mb86233::Mb86233Bus for BatchBus<'_> {
    fn read_program(&mut self, addr: u32) -> u32 {
        self.io
            .tgp_program_ram
            .get(addr as usize)
            .copied()
            .unwrap_or(0)
    }

    fn read_data(&mut self, addr: u32) -> u32 {
        self.traffic += 1;
        self.io.tgp_data_ram.get(addr as usize).copied().unwrap_or(0)
    }

    fn write_data(&mut self, addr: u32, data: u32) {
        self.traffic += 1;
        if let Some(slot) = self.io.tgp_data_ram.get_mut(addr as usize) {
            *slot = data;
        }
    }

    /// copro_tgp_io_map. Modal, exactly as on the single-threaded path:
    /// with the bank register's view bits set the whole range shadows
    /// external memory; clear, the math registers answer instead.
    fn read_io(&mut self, addr: u32) -> u32 {
        self.traffic += 1;
        if self.io.bank_reg & 0xc0_0000 != 0 {
            return banked_read(self.io, addr);
        }
        let tables = &self.io.copro_tables;
        match addr {
            0x20..=0x23 => copro_sincos_r(tables, self.io.sincos_base, addr - 0x20),
            0x24..=0x27 => copro_atan_r(tables, self.io.atan_base),
            0x28..=0x29 => copro_inv_r(tables, self.io.inv_base, addr - 0x28),
            0x2A..=0x2B => copro_isqrt_r(tables, self.io.isqrt_base, addr - 0x2A),
            // View disabled: nothing else is mapped.
            _ => 0,
        }
    }

    fn write_io(&mut self, addr: u32, data: u32) {
        self.traffic += 1;
        if self.io.bank_reg & 0xc0_0000 != 0 {
            return banked_write(self.io, addr, data);
        }
        match addr {
            0x20..=0x23 => self.io.sincos_base = data,
            0x24..=0x27 => {
                self.io.atan_base[(addr - 0x24) as usize] = data;
                // The atan comparator drives the TGP's GPIO0 pin; the
                // microcode branches on it to pick the octant, so it must be
                // visible to the very next instruction.
                self.io.gpio0 = (self.io.atan_base[0] & 0x7fff_ffff)
                    <= (self.io.atan_base[1] & 0x7fff_ffff);
            }
            0x28..=0x29 => self.io.inv_base = data,
            0x2A..=0x2B => self.io.isqrt_base = data,
            _ => {}
        }
    }

    fn read_rf(&mut self, addr: u32) -> u32 {
        match addr {
            1 => match self.pop_in() {
                Some(v) => {
                    self.traffic += 1;
                    v
                }
                None => {
                    // Nothing to do yet: ask the TGP to retry this
                    // instruction rather than run on with a bogus value.
                    self.io.stall = true;
                    self.saw_input_stall = true;
                    0
                }
            },
            _ => 0,
        }
    }

    fn take_stall(&mut self) -> bool {
        std::mem::take(&mut self.io.stall)
    }

    fn gpio(&mut self, index: u32) -> bool {
        // Only pin 0 is wired on Model 2 (the atan comparator).
        index == 0 && self.io.gpio0
    }

    fn halt_requested(&self) -> bool {
        // The ninth word is accepted by the FIFO, then its synchronized full
        // callback asserts HALT; `Mb86233::execute` checks this after
        // completing the current instruction.
        self.io.fifo_out.len() > COPRO_FIFO_DEPTH
    }

    fn write_rf(&mut self, addr: u32, data: u32) {
        match addr {
            0 => {} // leds / busy flag
            2 => {
                self.traffic += 1;
                self.push_out(data);
            }
            3 => self.io.bank_reg = data,
            _ => {}
        }
    }
}

impl sharc::SharcBus for BatchBus<'_> {
    fn dm_ext_read(&mut self, addr: u32) -> u32 {
        self.traffic += 1;
        self.io.sharc_reads += 1;
        let bucket = match addr {
            0x0400000..=0x0bfffff => 0,
            0x1400000..=0x1bfffff => 1,
            0x1c00000..=0x1dfffff => 2,
            _ => 3,
        };
        self.io.sharc_read_addrs[bucket] += 1;
        match addr {
            0x0400000..=0x0bfffff => self.pop_in().unwrap_or(0),
            0x1400000..=0x1bfffff => self.io.buffer.read(addr & 0x7fff),
            0x1c00000..=0x1dfffff => self
                .io
                .copro_data
                .get((addr & 0x1f_ffff) as usize)
                .copied()
                .unwrap_or(0),
            _ => 0,
        }
    }

    fn dm_ext_write(&mut self, addr: u32, data: u32) {
        self.traffic += 1;
        self.io.sharc_writes += 1;
        let b = match addr {
            0x0c00000..=0x13fffff => 0,
            0x1400000..=0x1bfffff => 1,
            _ => 2,
        };
        self.io.sharc_write_addrs[b] += 1;
        if b == 2 && self.io.sharc_writes < 400 {
            let n = (self.io.sharc_writes as usize) % 8;
            self.io.sharc_write_samples[n] = addr;
        }
        match addr {
            0x0c00000..=0x13fffff => self.push_out(data),
            0x1400000..=0x1bfffff => self.io.buffer.write(addr & 0x7fff, data),
            _ => {}
        }
    }

    fn fifo_in_empty(&self) -> bool {
        self.io.fifo_in.is_empty()
    }

    fn fifo_out_full(&self) -> bool {
        // The reference wires FLAG1 to the *input* FIFO's full state, and
        // both 2B FIFOs are 16 deep.
        self.io.fifo_in.len() >= 16
    }
}

impl mb86235::Mb86235Bus for BatchBus<'_> {
    fn data_read(&mut self, addr: u32) -> u32 {
        self.traffic += 1;
        self.io.tgpx4_ext_r += 1;
        let b = match addr {
            0x0040_0000..=0x007F_FFFF => 0,
            0x0080_0000..=0x009F_FFFF => 1,
            _ => 2,
        };
        self.io.tgpx4_rbucket[b] += 1;
        if b == 2 && self.io.tgpx4_rbucket[2] < 9 {
            self.io.tgpx4_rsample[(self.io.tgpx4_rbucket[2] as usize - 1) & 7] = addr;
        }
        match addr {
            0x0040_0000..=0x007F_FFFF => self.io.buffer.read(addr & 0x7fff),
            0x0080_0000..=0x009F_FFFF => self
                .io
                .copro_data
                .get(((addr - 0x0080_0000) & 0x1f_ffff) as usize)
                .copied()
                .unwrap_or(0),
            _ => 0,
        }
    }

    fn data_write(&mut self, addr: u32, data: u32) {
        self.traffic += 1;
        self.io.tgpx4_ext_w += 1;
        if let 0x0040_0000..=0x007F_FFFF = addr {
            self.io.buffer.write(addr & 0x7fff, data);
        }
    }

    fn fifo_in_pop(&mut self) -> Option<u32> {
        let v = self.pop_in();
        if v.is_some() {
            self.io.tgpx4_pops += 1;
        }
        v
    }

    fn fifo_out_push(&mut self, data: u32) {
        self.io.tgpx4_pushes += 1;
        self.push_out(data);
    }

    fn fifo_in_empty(&self) -> bool {
        self.io.fifo_in.is_empty()
    }

    fn fifo_in_full(&self) -> bool {
        self.io.fifo_in.len() >= COPRO_FIFO_DEPTH
    }

    fn fifo_out_empty(&self) -> bool {
        self.io.fifo_out.is_empty()
    }

    fn fifo_out_full(&self) -> bool {
        self.io.fifo_out.len() >= COPRO_FIFO_DEPTH
    }
}

/// The worker thread's body: apply board pokes, run a batch, park when the
/// hardware says there is nothing to do.
fn worker_loop(shared: Arc<CoproShared>) {
    loop {
        let mut g = shared.inner.lock().unwrap();

        // Board-level pokes from the i960, in order, before any execution.
        while let Some(op) = g.control.pop_front() {
            let mut core = std::mem::replace(&mut g.core, CoproCore::None);
            {
                let mut bus = BatchBus {
                    io: &mut g,
                    lens: &shared.lens,
                    saw_input_stall: false,
                    traffic: 0,
                };
                op.apply(&mut core, &mut bus);
            }
            g.core = core;
        }

        if g.quit {
            return;
        }

        // Park conditions, each a hardware backpressure line: halted by
        // copro_ctl, output FIFO full, paused for a savestate, or the DSP
        // observably parked -- stuck on an empty input pop (TGP stall retry
        // / TGPx4 `stalled`) or sitting in the SHARC's idle park, whose only
        // wake source is the i960's IOP doorbell (a control op).
        let dsp_idle = matches!(&g.core, CoproCore::Sharc(s) if s.idle);
        let starved = g.sleep_on_input && g.fifo_in.is_empty();
        if g.halted || g.paused || g.fifo_out.len() > COPRO_FIFO_DEPTH || dsp_idle || starved {
            g.worker_waiting = true;
            let mut g = shared.work.wait(g).unwrap();
            g.worker_waiting = false;
            drop(g);
            continue;
        }

        // One batch. The core steps out of the shared block so the bus can
        // borrow it, the same parking trick the main thread uses.
        let mut core = std::mem::replace(&mut g.core, CoproCore::None);
        let (saw_input_stall, traffic);
        {
            let mut bus = BatchBus {
                io: &mut g,
                lens: &shared.lens,
                saw_input_stall: false,
                traffic: 0,
            };
            core.execute(&mut bus, BATCH_CYCLES);
            saw_input_stall = bus.saw_input_stall;
            traffic = bus.traffic;
        }
        let tgpx4_stalled = matches!(&core, CoproCore::Tgpx4(t) if t.stalled);
        g.core = core;
        g.sleep_on_input = saw_input_stall || tgpx4_stalled;

        // A batch that touched nothing external and left the input FIFO
        // empty was the DSP spinning on its flag pins waiting for a command.
        // Nap until the i960 supplies one (the push and the doorbell both
        // signal the condvar); without this the worker re-locks faster than
        // the woken i960 can grab the mutex and starves it outright. The
        // timeout covers the misjudged case -- a DSP doing purely internal
        // math -- which then only pays latency, never deadlock.
        if !g.halted
            && !g.paused
            && g.fifo_in.is_empty()
            && g.fifo_out.len() <= COPRO_FIFO_DEPTH
            && !matches!(&g.core, CoproCore::Sharc(s) if s.idle)
            && !g.sleep_on_input
            && traffic == 0
        {
            g.worker_waiting = true;
            let (mut g, _) = shared.work.wait_timeout(g, SPIN_NAP).unwrap();
            g.worker_waiting = false;
            drop(g);
            continue;
        }

        drop(g);
        // Between batches, let the woken i960 win the lock once.
        std::thread::yield_now();
    }
}

impl Model2System {
    /// Starts the coprocessor worker thread and hands it the board's DSP
    /// core. Called at construction when `Config::multithreaded` is set;
    /// with it clear the system keeps its single-threaded lockstep.
    pub fn start_copro_worker(&mut self) {
        let core = match self.coprocessor {
            Board::Model2b => CoproCore::Sharc(std::mem::replace(
                &mut self.sharc,
                self.parked_sharc.take().expect("sharc placeholder"),
            )),
            Board::Model2c => CoproCore::Tgpx4(std::mem::replace(
                &mut self.tgpx4,
                self.parked_tgpx4.take().expect("tgpx4 placeholder"),
            )),
            _ => CoproCore::Tgp(std::mem::replace(
                &mut self.tgp_cpu,
                self.parked_tgp.take().expect("tgp placeholder"),
            )),
        };
        let inner = CoproInner::new(
            core,
            self.buffer_ram.share(),
            Arc::clone(&self.copro_data),
            Arc::clone(&self.copro_tables),
        );
        let shared = Arc::new(CoproShared {
            inner: Mutex::new(inner),
            work: Condvar::new(),
            lens: FifoLens::default(),
        });
        self.copro_mt = Some(CoproWorker::spawn(shared));
    }

    fn copro_shared(&self) -> Option<Arc<CoproShared>> {
        self.copro_mt.as_ref().map(|w| Arc::clone(&w.shared))
    }

    /// Savestate stop-the-world: park the worker between batches and copy
    /// every worker-owned field back into the system struct, where the
    /// snapshot machinery (and the debugger) reads them. `buffer_ram`
    /// needs no copy -- the system's `SharedBuffer` aliases the worker's.
    pub fn copro_pause_sync_from_worker(&mut self) {
        let Some(shared) = self.copro_shared() else {
            return;
        };
        let mut g = shared.inner.lock().unwrap();
        g.paused = true;
        self.copro_fifo_in = g.fifo_in.clone();
        self.copro_fifo_out = g.fifo_out.clone();
        self.tgp_program_ram = g.tgp_program_ram.clone();
        self.tgp_data_ram = g.tgp_data_ram.clone();
        match &g.core {
            CoproCore::Tgp(c) => self.tgp_cpu = c.clone(),
            CoproCore::Sharc(c) => self.sharc = c.clone(),
            CoproCore::Tgpx4(c) => self.tgpx4 = c.clone(),
            CoproCore::None => {}
        }
        self.copro_bank_reg = g.bank_reg;
        self.copro_sincos_base = g.sincos_base;
        self.copro_atan_base = g.atan_base;
        self.copro_gpio0 = g.gpio0;
        self.copro_inv_base = g.inv_base;
        self.copro_isqrt_base = g.isqrt_base;
        self.copro_stall = g.stall;
        self.copro_halted = g.halted;
        self.sharc_reads = g.sharc_reads;
        self.sharc_writes = g.sharc_writes;
        self.sharc_read_addrs = g.sharc_read_addrs;
        self.sharc_write_addrs = g.sharc_write_addrs;
        self.sharc_write_samples = g.sharc_write_samples;
        self.tgpx4_pops = g.tgpx4_pops;
        self.tgpx4_pushes = g.tgpx4_pushes;
        self.tgpx4_ext_r = g.tgpx4_ext_r;
        self.tgpx4_ext_w = g.tgpx4_ext_w;
        self.tgpx4_rbucket = g.tgpx4_rbucket;
        self.tgpx4_rsample = g.tgpx4_rsample;
    }

    /// Releases the worker after `copro_pause_sync_from_worker`.
    pub fn copro_resume(&mut self) {
        let Some(shared) = self.copro_shared() else {
            return;
        };
        {
            let mut g = shared.inner.lock().unwrap();
            g.paused = false;
        }
        shared.work.notify_one();
    }

    /// Pushes restored snapshot state into the worker: the core, the FIFOs,
    /// the TGP RAMs and registers, and the (replaced) buffer. The worker
    /// sees it all under one lock, so there is no torn restore.
    pub fn copro_sync_to_worker(&mut self) {
        let Some(shared) = self.copro_shared() else {
            return;
        };
        {
            let mut g = shared.inner.lock().unwrap();
            g.core = match self.coprocessor {
                Board::Model2b => CoproCore::Sharc(self.sharc.clone()),
                Board::Model2c => CoproCore::Tgpx4(self.tgpx4.clone()),
                _ => CoproCore::Tgp(self.tgp_cpu.clone()),
            };
            g.fifo_in = self.copro_fifo_in.clone();
            g.fifo_out = self.copro_fifo_out.clone();
            g.tgp_program_ram = self.tgp_program_ram.clone();
            g.tgp_data_ram = self.tgp_data_ram.clone();
            g.buffer = self.buffer_ram.share();
            g.bank_reg = self.copro_bank_reg;
            g.sincos_base = self.copro_sincos_base;
            g.atan_base = self.copro_atan_base;
            g.gpio0 = self.copro_gpio0;
            g.inv_base = self.copro_inv_base;
            g.isqrt_base = self.copro_isqrt_base;
            g.stall = self.copro_stall;
            g.halted = self.copro_halted;
            g.sleep_on_input = false;
            g.paused = false;
            g.control.clear();
            shared.lens.in_len.store(g.fifo_in.len(), Ordering::Relaxed);
            shared.lens.out_len.store(g.fifo_out.len(), Ordering::Relaxed);
        }
        shared.work.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spawn_with(inner: CoproInner) -> (Arc<CoproShared>, CoproWorker) {
        let shared = Arc::new(CoproShared {
            inner: Mutex::new(inner),
            work: Condvar::new(),
            lens: FifoLens::default(),
        });
        let worker = CoproWorker::spawn(Arc::clone(&shared));
        (shared, worker)
    }

    fn tgp_inner() -> CoproInner {
        CoproInner::new(
            CoproCore::Tgp(Box::new(mb86233::Mb86233::new())),
            SharedBuffer::new(0, 0x8000),
            Arc::new(vec![0; 0x100]),
            Arc::new(vec![0; 0x100]),
        )
    }

    #[test]
    fn shared_buffer_read_write() {
        let b = SharedBuffer::new(0xdead_beef, 4);
        assert_eq!(b.len(), 4);
        assert_eq!(b.read(0), 0xdead_beef);
        assert_eq!(b.read(4), 0); // out of range reads back 0
        b.write(1, 42);
        assert_eq!(b.read(1), 42);
        b.write_word(8, 7);
        assert_eq!(b.read(2), 7);
        b.write(9, 1); // out of range writes go nowhere
        assert_eq!(b.read(9), 0);
    }

    #[test]
    fn shared_buffer_share_aliases_clone_does_not() {
        let b = SharedBuffer::new(0, 4);
        let alias = b.share();
        alias.write(0, 1);
        assert_eq!(b.read(0), 1, "share() must alias the same storage");
        let snap = b.clone();
        snap.write(0, 2);
        assert_eq!(b.read(0), 1, "clone() must be an independent copy");
        assert_eq!(snap.read(0), 2);
    }

    #[test]
    fn shared_buffer_serde_round_trip() {
        let b = SharedBuffer::new(0, 8);
        b.write(3, 0x1234_5678);
        let blob = bincode::serialize(&b).unwrap();
        // The layout is the one a Vec<u32> produces, so states written by
        // the pre-threading format read back unchanged.
        let as_vec: Vec<u32> = bincode::deserialize(&blob).unwrap();
        assert_eq!(as_vec[3], 0x1234_5678);
        let back: SharedBuffer = bincode::deserialize(&blob).unwrap();
        assert_eq!(back.read(3), 0x1234_5678);
        assert_eq!(back.len(), 8);
    }

    #[test]
    fn worker_starts_halted_and_applies_control_ops() {
        let (shared, worker) = spawn_with(tgp_inner());
        worker.control(ControlOp::TgpReset);
        worker.push_input(0x1234);
        // Give the worker a moment to drain the queue; it must not execute
        // anything while halted, so the input word stays put.
        std::thread::sleep(std::time::Duration::from_millis(50));
        {
            let g = shared.inner.lock().unwrap();
            assert!(g.halted);
            assert!(g.control.is_empty(), "control op should be consumed");
            assert_eq!(g.fifo_in.len(), 1);
        }
        drop(worker);
    }

    #[test]
    fn worker_parks_on_sharc_idle_and_wakes_on_control_op() {
        let mut sharc = Box::new(sharc::Sharc::new());
        sharc.idle = true;
        let mut inner = CoproInner::new(
            CoproCore::Sharc(sharc),
            SharedBuffer::new(0, 0x8000),
            Arc::new(vec![0; 0x100]),
            Arc::new(vec![0; 0x100]),
        );
        inner.halted = false;
        let (shared, worker) = spawn_with(inner);
        std::thread::sleep(std::time::Duration::from_millis(50));
        {
            let g = shared.inner.lock().unwrap();
            assert!(g.worker_waiting, "an idle SHARC should park the worker");
        }
        // The i960's doorbell arrives as a control op; reset clears idle.
        worker.control(ControlOp::SharcReset);
        std::thread::sleep(std::time::Duration::from_millis(50));
        {
            let g = shared.inner.lock().unwrap();
            assert!(g.control.is_empty());
            let idle = matches!(&g.core, CoproCore::Sharc(s) if s.idle);
            assert!(!idle, "the reset op should have woken the SHARC");
        }
        drop(worker);
    }

    #[test]
    fn worker_starved_on_input_wakes_on_push() {
        let mut inner = tgp_inner();
        inner.halted = false;
        inner.sleep_on_input = true; // as the last batch left it
        let (shared, worker) = spawn_with(inner);
        std::thread::sleep(std::time::Duration::from_millis(50));
        {
            let g = shared.inner.lock().unwrap();
            assert!(g.worker_waiting, "a starved DSP should park the worker");
        }
        worker.push_input(0xaaaa_bbbb);
        std::thread::sleep(std::time::Duration::from_millis(50));
        {
            let g = shared.inner.lock().unwrap();
            assert!(!g.sleep_on_input);
            // The zeroed program RAM never pops, so the word stays queued.
            assert_eq!(g.fifo_in.len(), 1);
        }
        drop(worker);
    }

    #[test]
    fn worker_shutdown_from_parked_state() {
        let (_shared, worker) = spawn_with(tgp_inner());
        // Halted and never booted: the worker is inside Condvar::wait.
        drop(worker); // must not hang
    }

    #[test]
    fn worker_shutdown_mid_batch() {
        let mut inner = tgp_inner();
        inner.halted = false; // free-running on zeroed program RAM
        let (_shared, worker) = spawn_with(inner);
        std::thread::sleep(std::time::Duration::from_millis(20));
        drop(worker); // must not hang
    }
}
