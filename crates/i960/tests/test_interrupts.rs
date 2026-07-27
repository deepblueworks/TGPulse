mod common;

use i960::bus::Bus;
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

    #[allow(dead_code)]
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

// --- Test Configuration ---

const PRCB_PTR: u32 = 0x1000; // Processor Control Block
const INT_TAB_PTR: u32 = 0x2000; // Interrupt Table
const INT_STACK_PTR: u32 = 0x3000; // Interrupt Stack Base
const MAIN_PTR: u32 = 0x0100; // Main loop address
const ISR_PTR: u32 = 0x0400; // Interrupt Service Routine address

#[test]
fn test_interrupts() {
    println!("--- Starting i960 Interrupt Handling Test ---");
    test_interrupt_vectoring();
    println!("--- Test Completed ---");
}

fn test_interrupt_vectoring() {
    let mut sys = System::new();
    let mut cpu = I960Cpu::new();

    println!("[Setup] Configuring Memory Structures...");

    // 1. Configure PRCB in Memory
    sys.write_u32(PRCB_PTR + 20, INT_TAB_PTR);
    sys.write_u32(PRCB_PTR + 24, INT_STACK_PTR);

    // 2. Configure Interrupt Table Entry (Vector 244)
    let vector = 0xF4;
    let vector_offset = 36 + ((vector - 8) * 4);
    let vector_addr = INT_TAB_PTR + vector_offset;

    sys.write_u32(vector_addr, ISR_PTR);
    println!(
        "  -> Wrote ISR Address 0x{:04X} to Interrupt Table at 0x{:04X} (Vector {})",
        ISR_PTR, vector_addr, vector
    );

    // 3. Configure CPU Registers
    cpu.prcb = PRCB_PTR;
    cpu.icr = 0x000000F4; // Map IRQ0 to Vector 0xF4
    cpu.r[31] = 0x00005000; // FP
    cpu.r[1] = 0x00005040; // SP
    cpu.pc = 0x00002002; // Priority 0, Supervisor

    // 4. Load Programs

    // Main Program: Infinite Loop
    sys.load_program(MAIN_PTR, &[0x08000000]);
    cpu.ip = MAIN_PTR;

    // ISR Program:
    // 1. mov 16, r16  (Global Register)
    //    Op: 5C, Dst: r16 (16), Val: 16
    //    Encoding: 0x5C800E10
    // 2. ret
    //    Encoding: 0x0A000000
    let isr_prog = [0x5C800E10, 0x0A000000];
    sys.load_program(ISR_PTR, &isr_prog);

    println!("[Run] Executing Main Loop (10 cycles)...");
    common::run(&mut cpu, &mut sys, 10);

    assert_eq!(cpu.ip, MAIN_PTR, "CPU should be looping at Main");
    assert_eq!(cpu.r[16], 0, "r16 should be 0 before ISR");

    println!("[Trigger] Firing IRQ0 (Vector 0xF4)...");
    cpu.set_irq_line(0, true);

    println!("[Run] Executing Post-Trigger (50 cycles)...");
    common::run(&mut cpu, &mut sys, 50);

    println!("[Verify] Checking State...");
    println!(
        "  -> Final IP: 0x{:04X} (Expected ~0x{:04X})",
        cpu.ip, MAIN_PTR
    );
    println!("  -> r16 Value: {} (Expected 16 from ISR)", cpu.r[16]);

    // Verify side effects
    assert_eq!(
        cpu.r[16], 16,
        "ISR failed: Global register r16 was not modified."
    );
    assert_eq!(
        cpu.ip, MAIN_PTR,
        "Return failed: CPU did not return to Main Loop."
    );

    println!("  -> Passed");
}
