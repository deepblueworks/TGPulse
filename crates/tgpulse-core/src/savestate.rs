//! Save states.
//!
//! A snapshot holds everything the machine can *change* and nothing it cannot:
//! the CPU cores, every RAM, the coprocessor queues and the board registers.
//! The ROM regions are deliberately excluded -- they are tens of megabytes,
//! they never change, and the loaded game already has them -- so a state file
//! is small and only ever valid against the same romset.
//!
//! `Model2System::snapshot()` and `restore()` are the whole interface; the
//! debugger uses them for its state slots and the front end for save/load keys.

use crate::system::Model2System;

/// Bumped whenever the snapshot layout changes, so an old file is rejected
/// rather than silently misread into the machine. Version 2: the geometry
/// buffer's live type became `SharedBuffer` for the coprocessor worker
/// thread (its wire format is unchanged, but the version makes that
/// coupling explicit).
pub const FORMAT_VERSION: u32 = 2;

/// Declares the snapshot struct and the copy in/out, from one field list, so
/// the two directions can never drift apart.
macro_rules! snapshot_fields {
    ($($field:ident : $ty:ty),* $(,)?) => {
        #[derive(Clone, serde::Serialize, serde::Deserialize)]
        pub struct Snapshot {
            pub version: u32,
            pub game: String,
            $(pub $field: $ty,)*
        }

        impl Model2System {
            /// Captures the mutable machine state. With the coprocessor on
            /// its worker thread the worker is first parked between batches
            /// and its state synced back into the system fields (Supermodel's
            /// stop-the-world method: the in-flight FIFO contents are part of
            /// the serialized state).
            pub fn snapshot(&mut self) -> Snapshot {
                self.copro_pause_sync_from_worker();
                let s = Snapshot {
                    version: FORMAT_VERSION,
                    game: self.snapshot_game.clone(),
                    $($field: self.$field.clone(),)*
                };
                self.copro_resume();
                s
            }

            /// Puts a snapshot back. The caller has already checked that it
            /// belongs to this game.
            pub fn restore(&mut self, s: &Snapshot) {
                $(self.$field = s.$field.clone();)*
                // Runtime-only engine switches are not serialized; re-apply
                // the live configuration.
                self.sharc.jit_enabled = self.config.sharc_jit;
                self.tgpx4.jit_enabled = self.config.mb86235_jit;
                self.copro_sync_to_worker();
            }
        }
    };
}

snapshot_fields! {
    // --- processors ---
    main_cpu: Box<i960::cpu::defs::I960Cpu>,
    tgp_cpu: Box<mb86233::Mb86233>,
    sharc: Box<sharc::Sharc>,
    tgpx4: Box<mb86235::Mb86235>,

    // --- memories the machine writes ---
    ram_low: Vec<u32>,
    work_ram: Vec<u32>,
    buffer_ram: crate::copro::SharedBuffer,
    tile_ram: Vec<u32>,
    char_ram: Vec<u32>,
    palette_ram: Vec<u32>,
    colorxlat_ram: Vec<u32>,
    luma_ram: Vec<u8>,
    texture_ram0: Vec<u32>,
    texture_ram1: Vec<u32>,
    backup_ram: Vec<u32>,
    dpram: Vec<u8>,
    cpu_ctl: Vec<u32>,
    tgp_program_ram: Vec<u32>,
    tgp_data_ram: Vec<u32>,

    // --- coprocessor plumbing ---
    copro_fifo_in: std::collections::VecDeque<u32>,
    copro_fifo_out: std::collections::VecDeque<u32>,
    copro_ctl: u32,
    copro_cnt: u32,
    copro_halted: bool,
    copro_bank_reg: u32,
    copro_sincos_base: u32,
    copro_atan_base: [u32; 4],
    copro_inv_base: u32,
    copro_isqrt_base: u32,
    copro_stall: bool,
    main_stall: bool,

    // --- board registers ---
    irq_request: u32,
    irq_enable: u32,
    geo_ctl: u32,
    geo_cnt: u32,
    geo_write_start_address: u32,
    geo_read_start_address: u32,
    colorxlat_written: bool,
    colorxlat_dirty: bool,
    crtc_xoffset: i16,
    crtc_yoffset: i16,
    crtc_xraw: u16,
    crtc_yraw: u16,
    video_ctl: u32,
    render_mode_ctl: u32,
    frame_num: u32,
    timer_vals: [u32; 4],
    timer_running: [bool; 4],
    drive_cmd: u8,

    // --- I/O ---
    eeprom: crate::eeprom93c46::Eeprom93c46,
    io5649_ports: [u8; 8],
    io5649_analog: u32,
    io5649_gun_mux: u32,
    io5649_ctrlmode: bool,
    inputs: crate::config::Inputs,
}

/// Encodes a snapshot for storage on disk.
pub fn encode(s: &Snapshot) -> Result<Vec<u8>, String> {
    bincode::serialize(s).map_err(|e| e.to_string())
}

/// Decodes a snapshot, refusing one from a different layout or a different
/// game rather than corrupting the running machine.
pub fn decode(blob: &[u8], game: &str) -> Result<Snapshot, String> {
    let s: Snapshot = bincode::deserialize(blob).map_err(|e| e.to_string())?;
    if s.version != FORMAT_VERSION {
        return Err(format!(
            "save state is format {} but this build reads {FORMAT_VERSION}",
            s.version
        ));
    }
    if !s.game.is_empty() && !game.is_empty() && s.game != game {
        return Err(format!("save state is for {}, not {game}", s.game));
    }
    Ok(s)
}

pub fn path_for(game: &str, slot: u32) -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from("states");
    p.push(format!("{game}.{slot}.state"));
    p
}

pub fn save_to_file(
    sys: &mut Model2System,
    game: &str,
    slot: u32,
) -> Result<std::path::PathBuf, String> {
    let path = path_for(game, slot);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let blob = encode(&sys.snapshot())?;
    std::fs::write(&path, blob).map_err(|e| e.to_string())?;
    Ok(path)
}

pub fn load_from_file(
    sys: &mut Model2System,
    game: &str,
    slot: u32,
) -> Result<std::path::PathBuf, String> {
    let path = path_for(game, slot);
    let blob = std::fs::read(&path).map_err(|e| e.to_string())?;
    let snap = decode(&blob, game)?;
    sys.restore(&snap);
    Ok(path)
}
