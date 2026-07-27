// examples/test_timer_unit.rs

mod common;

use i960::bus::Bus;
use i960::cpu::I960Cpu;

// --- Mock System ---
struct System {
    ram: [u8; 0x10000],
}

impl System {
    fn new() -> Self {
        Self { ram: [0; 0x10000] }
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

#[test]
fn test_timer_unit() {
    println!("--- Starting i960 Timer Unit Integration Test ---");
    test_timer_oneshot();
    test_timer_periodic();
    println!("--- All Timer Tests Passed ---");
}

fn test_timer_oneshot() {
    println!("[Test] Timer 0 One-Shot Mode...");
    let mut sys = System::new();
    let mut cpu = I960Cpu::new();

    // 1. Configure Timer 0
    // TMR (Mode): Enable (Bit 1) | No Suppress Int
    // TRR (Reload): 1000 (Irrelevant for one-shot but good practice)
    // TCR (Count): 50 cycles

    // Note: In a real scenario, we would use 'modtc' opcode.
    // Here we inject state directly to test the core loop logic.

    // Inject State:
    cpu.trr[0] = 1000;
    cpu.tcr[0] = 50;
    cpu.tmr[0] = 0x02; // Bit 1 = Enable. Bit 2 (Periodic) = 0.

    // 2. Run CPU for 30 cycles (Timer should still be running)
    // We execute a simple loop or NOPs. Here we just rely on execute_run consuming cycles.
    // Opcode 0x00 is invalid but consumes 1 cycle in our safety fallback.
    common::run(&mut cpu, &mut sys, 30);

    // Verify: TCR should be approx 20 (50 - 30)
    let current_tcr = cpu.tcr[0];
    assert!(
        current_tcr > 0 && current_tcr <= 20,
        "Timer did not decrement correctly. Expected ~20, got {}",
        current_tcr
    );
    println!("  -> Decrement OK (Value: {})", current_tcr);

    // 3. Run CPU for 30 more cycles (Timer should expire)
    common::run(&mut cpu, &mut sys, 30);

    // Verify:
    // - TCR should be 0 (or reloaded if periodic, but this is one-shot)
    // - TMR Bit 1 (Enable) should be CLEARED (Auto-stop)
    // - TMR Bit 0 (TC) should be SET

    assert_eq!(cpu.tcr[0], 0, "One-shot timer should stop at 0");
    assert_eq!(
        cpu.tmr[0] & 0x02,
        0,
        "One-shot timer should clear Enable bit"
    );
    assert_eq!(
        cpu.tmr[0] & 0x01,
        1,
        "Timer should set Terminal Count (TC) bit"
    );

    println!("  -> Expiration/Stop OK");
}

fn test_timer_periodic() {
    println!("[Test] Timer 1 Periodic Mode...");
    let mut sys = System::new();
    let mut cpu = I960Cpu::new();

    // 1. Configure Timer 1
    // Reload Value: 20
    // Mode: Enable (Bit 1) | Periodic (Bit 2) = 0x06
    cpu.trr[1] = 20;
    cpu.tcr[1] = 20;
    cpu.tmr[1] = 0x06;

    // 2. Run for 25 cycles (Should wrap around)
    // 20 - 25 = -5 => Reloads 20, subtracts remaining 5 => 15.
    common::run(&mut cpu, &mut sys, 25);

    // Verify:
    // - Enable bit should STILL be set
    // - TCR should be around 15
    assert_eq!(cpu.tmr[1] & 0x02, 2, "Periodic timer must remain enabled");
    assert!(
        cpu.tcr[1] < 20,
        "Timer should have reloaded and decremented"
    );

    println!("  -> Periodic Reload OK (Value: {})", cpu.tcr[1]);
}
