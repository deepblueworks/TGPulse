use i960::bus::Bus;
use i960::cpu::defs::{FAULT_ARITHMETIC, FSUB_ZERO_DIVIDE};
use i960::cpu::I960Cpu;

// --- Mock System Implementation (Bus) ---
struct System {
    ram: [u8; 0x10000],
}

impl System {
    fn new() -> Self {
        Self { ram: [0; 0x10000] }
    }

    fn write_u32(&mut self, addr: u32, val: u32) {
        let addr = addr as usize;
        if addr + 4 <= self.ram.len() {
            self.ram[addr] = (val & 0xFF) as u8;
            self.ram[addr + 1] = ((val >> 8) & 0xFF) as u8;
            self.ram[addr + 2] = ((val >> 16) & 0xFF) as u8;
            self.ram[addr + 3] = ((val >> 24) & 0xFF) as u8;
        }
    }

    fn read_u32(&mut self, addr: u32) -> u32 {
        let addr = addr as usize;
        if addr + 4 <= self.ram.len() {
            let b0 = self.ram[addr] as u32;
            let b1 = self.ram[addr + 1] as u32;
            let b2 = self.ram[addr + 2] as u32;
            let b3 = self.ram[addr + 3] as u32;
            b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
        } else {
            0
        }
    }

    fn load_program(&mut self, addr: u32, opcodes: &[u32]) {
        for (i, &word) in opcodes.iter().enumerate() {
            self.write_u32(addr + (i as u32 * 4), word);
        }
    }
}

impl Bus for System {
    fn read_byte(&mut self, addr: u32) -> u8 {
        if (addr as usize) < self.ram.len() {
            self.ram[addr as usize]
        } else {
            0
        }
    }

    fn write_byte(&mut self, addr: u32, val: u8) {
        if (addr as usize) < self.ram.len() {
            self.ram[addr as usize] = val;
        }
    }
}

// --- Constants ---
const PRCB_PTR: u32 = 0x1000;
const FAULT_TAB_PTR: u32 = 0x2000;
const HANDLER_PTR: u32 = 0x3000;
const MAIN_PTR: u32 = 0x0100;
const STACK_TOP: u32 = 0x5000;

#[test]
fn test_faults() {
    println!("--- Starting i960 Fault Handling Test ---");
    test_divide_by_zero();
    println!("--- Test Completed ---");
}

fn test_divide_by_zero() {
    println!("\n[Test] Arithmetic Fault (Divide by Zero)...");
    let mut sys = System::new();
    let mut cpu = I960Cpu::new();

    // 1. Setup PRCB
    // PRCB Pointer is in cpu.prcb.
    // Offset 0 of PRCB points to the Fault Table.
    cpu.prcb = PRCB_PTR;
    sys.write_u32(PRCB_PTR, FAULT_TAB_PTR);

    // 2. Setup Fault Table
    // Entry = TableBase + (FaultType * 4).
    // FAULT_ARITHMETIC is type 3. Offset = 12.
    let entry_addr = FAULT_TAB_PTR + (FAULT_ARITHMETIC * 4);
    sys.write_u32(entry_addr, HANDLER_PTR);

    println!(
        "  -> Wrote Fault Handler 0x{:04X} to Table at 0x{:04X}",
        HANDLER_PTR, entry_addr
    );

    // 3. Setup CPU State
    cpu.r[1] = STACK_TOP; // SP
    cpu.r[31] = STACK_TOP - 64; // FP (Fake previous frame)
    cpu.ip = MAIN_PTR;

    // 4. Load Fault Handler
    // Just a halt loop to catch the CPU
    let handler_prog = [0x08000000]; // b .
    sys.load_program(HANDLER_PTR, &handler_prog);

    // 5. Load Main Program
    // divi r3, r4, r5
    // r3 (src1) = 0 (Divisor)
    // r4 (src2) = 100
    // Op: 0x74, Sub: 0xB
    let op_divi: u32 = 0x74000000 | (5 << 19) | (4 << 14) | (0xB << 7) | 3;
    let main_prog = [op_divi, 0x08000000];
    sys.load_program(MAIN_PTR, &main_prog);

    // Setup registers
    cpu.r[3] = 0; // Divisor
    cpu.r[4] = 100; // Dividend

    println!("[Run] Executing Divide Instruction...");
    cpu.execute_run(&mut sys, 20);

    // 6. Verify Result
    println!(
        "  -> Final IP: 0x{:04X} (Expected Handler 0x{:04X})",
        cpu.ip, HANDLER_PTR
    );
    assert_eq!(
        cpu.ip, HANDLER_PTR,
        "Fault failed: CPU did not vector to the Fault Handler."
    );

    // 7. Verify Fault Record on Stack
    // The new frame starts at the old SP (aligned).
    // do_call aligns SP: (STACK_TOP + 63) & !63 -> 0x5000 + 64 = 0x5040 (New SP).
    // New FP = Old SP aligned = 0x5000.
    // Fault Record is at New FP - 16.
    // Address: 0x5000 - 16 = 0x4FF0.

    // We can also read it dynamically from current FP.
    let current_fp = cpu.r[31] & !0x3f;
    let record_addr = current_fp.wrapping_sub(16);
    let fault_word = sys.read_u32(record_addr);
    let fault_ip = sys.read_u32(record_addr + 4);

    println!(
        "  -> Fault Record at 0x{:04X}: 0x{:08X}",
        record_addr, fault_word
    );
    println!("  -> Faulting IP: 0x{:04X}", fault_ip);

    // Decode Fault Word
    // Bits 0-7: Flags (0x02 pending)
    // Bits 8-15: Type
    // Bits 16-31: Subtype
    let flags = fault_word & 0xFF;
    let ftype = (fault_word >> 8) & 0xFF;
    let fsubtype = (fault_word >> 16) & 0xFFFF;

    assert_eq!(
        flags & 0x02,
        0x02,
        "Fault Record Invalid: Pending bit not set"
    );
    assert_eq!(
        ftype, FAULT_ARITHMETIC,
        "Incorrect Fault Type. Expected 3, got {}",
        ftype
    );
    assert_eq!(
        fsubtype, FSUB_ZERO_DIVIDE,
        "Incorrect Fault Subtype. Expected 3 (Zero Divide), got {}",
        fsubtype
    );
    assert_eq!(
        fault_ip, MAIN_PTR,
        "Incorrect Faulting Instruction Pointer."
    );

    println!("  -> Passed");
}
