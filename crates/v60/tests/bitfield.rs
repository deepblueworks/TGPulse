use v60::cpu::{AP, PC};
use v60::{Bus, V60};

struct Ram(Vec<u8>);

impl Bus for Ram {
    fn read_u8(&mut self, address: u32) -> u8 {
        self.0.get(address as usize).copied().unwrap_or(0)
    }

    fn write_u8(&mut self, address: u32, value: u8) {
        if let Some(destination) = self.0.get_mut(address as usize) {
            *destination = value;
        }
    }
}

#[test]
fn extbfl_matches_virtua_racing_boot_encoding() {
    const START: usize = 0x100;
    const SOURCE: usize = 0x0050_1480;

    let mut ram = Ram(vec![0; SOURCE + 4]);

    // EXTBF.L direct-address source, width 11, destination R1:
    // 5D AA F3 80 14 50 00 0B 61
    ram.0[START..START + 9]
        .copy_from_slice(&[0x5d, 0xaa, 0xf3, 0x80, 0x14, 0x50, 0x00, 0x0b, 0x61]);
    ram.0[SOURCE..SOURCE + 4].copy_from_slice(&0x0000_05a5u32.to_le_bytes());

    let mut cpu = V60::new();
    cpu.reg[PC] = START as u32;
    cpu.run(&mut ram, 1);

    assert_eq!(cpu.reg[1], 0xb4a0_0000);
    assert_eq!(cpu.reg[PC], (START + 9) as u32);
    assert_eq!(cpu.op_count[0x5d], 1);
    assert_eq!(cpu.op_unimpl[0x5d], 0);
}

#[test]
fn extbfz_reads_register_relative_bit_displacement() {
    const START: usize = 0x100;
    const SOURCE: usize = 0x200;

    let mut ram = Ram(vec![0; 0x400]);

    // Virtua Racing encoding at FFE4A5:
    // EXTBF.Z 5[AP], width 3, destination R11.
    ram.0[START..START + 6].copy_from_slice(&[0x5d, 0xa9, 0x1d, 0x05, 0x03, 0x6b]);
    ram.0[SOURCE..SOURCE + 4].copy_from_slice(&0x0000_00a0u32.to_le_bytes());

    let mut cpu = V60::new();
    cpu.reg[AP] = SOURCE as u32;
    cpu.reg[PC] = START as u32;
    cpu.run(&mut ram, 1);

    assert_eq!(cpu.reg[11], 5);
    assert_eq!(cpu.reg[PC], (START + 6) as u32);
    assert_eq!(cpu.op_unimpl[0x5d], 0);
}

#[test]
fn extbfs_sign_extends_the_selected_field() {
    const START: usize = 0x100;
    const SOURCE: usize = 0x200;

    let mut ram = Ram(vec![0; 0x400]);

    // Select binary 100 at bit offset 2; signed three-bit 100 is -4.
    ram.0[START..START + 6].copy_from_slice(&[0x5d, 0xa8, 0x1d, 0x02, 0x03, 0x6a]);
    ram.0[SOURCE..SOURCE + 4].copy_from_slice(&0x0000_0010u32.to_le_bytes());

    let mut cpu = V60::new();
    cpu.reg[AP] = SOURCE as u32;
    cpu.reg[PC] = START as u32;
    cpu.run(&mut ram, 1);

    assert_eq!(cpu.reg[10], 0xffff_fffc);
    assert_eq!(cpu.reg[PC], (START + 6) as u32);
    assert_eq!(cpu.op_unimpl[0x5d], 0);
}
