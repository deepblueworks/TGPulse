//! ASTAT / STKY / MODE1 bit definitions, from the reference.
#![allow(dead_code)]

// --- ASTAT flags ---
pub const AZ: u32 = 0x0000_0001; // ALU result zero
pub const AV: u32 = 0x0000_0002; // ALU overflow
pub const AN: u32 = 0x0000_0004; // ALU result negative
pub const AC: u32 = 0x0000_0008; // ALU fixed-point carry
pub const AS: u32 = 0x0000_0010; // ALU X input sign
pub const AI: u32 = 0x0000_0020; // ALU floating-point invalid
pub const MN: u32 = 0x0000_0040; // Multiplier result negative
pub const MV: u32 = 0x0000_0080; // Multiplier overflow
pub const MU: u32 = 0x0000_0100; // Multiplier underflow
pub const MI: u32 = 0x0000_0200; // Multiplier floating-point invalid
pub const AF: u32 = 0x0000_0400; // Last ALU op was floating-point
pub const SV: u32 = 0x0000_0800; // Shifter overflow
pub const SZ: u32 = 0x0000_1000; // Shifter result zero
pub const SS: u32 = 0x0000_2000; // Shifter input sign
pub const BTF: u32 = 0x0004_0000; // Bit Test Flag
pub const FLG0: u32 = 0x0008_0000;
pub const FLG1: u32 = 0x0010_0000;
pub const FLG2: u32 = 0x0020_0000;
pub const FLG3: u32 = 0x0040_0000;

pub const AZ_SHIFT: u32 = 0;
pub const AV_SHIFT: u32 = 1;
pub const AN_SHIFT: u32 = 2;
pub const AC_SHIFT: u32 = 3;
pub const AS_SHIFT: u32 = 4;
pub const AI_SHIFT: u32 = 5;
pub const MN_SHIFT: u32 = 6;
pub const MV_SHIFT: u32 = 7;
pub const MU_SHIFT: u32 = 8;
pub const MI_SHIFT: u32 = 9;
pub const AF_SHIFT: u32 = 10;
pub const SV_SHIFT: u32 = 11;
pub const SZ_SHIFT: u32 = 12;
pub const SS_SHIFT: u32 = 13;
pub const BTF_SHIFT: u32 = 18;
pub const FLG0_SHIFT: u32 = 19;

// --- STKY flags ---
pub const AOS: u32 = 0x0000_0004; // ALU fixed-point overflow
pub const AUS: u32 = 0x0000_0001; // ALU floating-point underflow
pub const AVS: u32 = 0x0000_0002; // ALU floating-point overflow
pub const AIS: u32 = 0x0000_0020; // ALU floating-point invalid
pub const MVS: u32 = 0x0000_0080; // Multiplier floating-point overflow
pub const MUS: u32 = 0x0000_0100; // Multiplier underflow
pub const MIS: u32 = 0x0000_0200; // Multiplier floating-point invalid
pub const PCFL: u32 = 0x0020_0000; // PC stack full
pub const PCEM: u32 = 0x0040_0000; // PC stack empty
pub const SSOV: u32 = 0x0080_0000; // Status stack overflow
pub const SSEM: u32 = 0x0100_0000; // Status stack empty
pub const LSOV: u32 = 0x0200_0000; // Loop stacks overflow
pub const LSEM: u32 = 0x0400_0000; // Loop stacks empty

// --- MODE1 flags ---
pub const MODE1_BR8: u32 = 0x0000_0001;
pub const MODE1_BR0: u32 = 0x0000_0002;
pub const MODE1_SRCU: u32 = 0x0000_0004; // alt register select, compute units
pub const MODE1_SRD1H: u32 = 0x0000_0008; // DAG alt select 7-4
pub const MODE1_SRD1L: u32 = 0x0000_0010; // DAG alt select 3-0
pub const MODE1_SRD2H: u32 = 0x0000_0020; // DAG alt select 15-12
pub const MODE1_SRD2L: u32 = 0x0000_0040; // DAG alt select 11-8
pub const MODE1_SRRFH: u32 = 0x0000_0080; // register file alt select R15-8
pub const MODE1_SRRFL: u32 = 0x0000_0400; // register file alt select R7-0
pub const MODE1_NESTM: u32 = 0x0000_0800;
pub const MODE1_IRPTEN: u32 = 0x0000_1000;
pub const MODE1_ALUSAT: u32 = 0x0000_2000;
pub const MODE1_SSE: u32 = 0x0000_4000; // short word sign extension
pub const MODE1_TRUNCATE: u32 = 0x0000_8000;
pub const MODE1_RND32: u32 = 0x0001_0000;
pub const MODE1_CSEL: u32 = 0x0006_0000;
