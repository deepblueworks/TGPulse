//! ROM loading for Sega Model 2: which chip goes where in each ROM region,
//! and how the interleaved ones are woven together.

use std::fs::File;
use std::io::Read;
use zip::ZipArchive;

/// The ROM regions the i960 and the TGP see. Each is a byte image of a the reference
/// ROM_REGION, already interleaved, indexed by region-relative byte offset.
pub struct Roms {
    /// "maincpu": i960 program, 2MB region (only the first 256KB is populated).
    pub maincpu: Vec<u8>,
    /// "main_data": 32MB data region, visible at 0x02000000 and 0x06000000.
    pub main_data: Vec<u8>,
    /// "copro_data": 8MB coprocessor data (collision / height maps).
    pub copro_data: Vec<u8>,
    /// CPU-board lookup ROMs used by the TGP math ports.
    pub copro_tables: Vec<u8>,
    /// Geometry engine model ROM, 16MB.
    pub polygons: Vec<u8>,
    /// Rasterizer texture ROM, 16MB (half populated on Daytona).
    pub textures: Vec<u8>,
    /// Factory contents of the serial EEPROM, when the romset ships one.
    /// The reference provides this for the handful of sets whose cabinet configuration
    /// cannot be reached from the game's own menus -- Manx TT's DX and twin
    /// modes, for instance.
    pub eeprom: Vec<u8>,

    // --- Model 1 sound board (segam1audio) ---
    /// "m1audio:sndcpu": the board's 68000 program, 768KB region.
    ///
    /// The reference declares this ROMREGION_BE|ROMREGION_16BIT and loads it with
    /// ROM_LOAD16_WORD_SWAP, i.e. the bytes of every 16-bit word are exchanged
    /// on the way in. Doing that is what makes the reset vectors read back as a
    /// stack pointer at the top of the board's RAM and an entry point inside
    /// the ROM; without it both are garbage.
    pub sndcpu: Vec<u8>,
    /// "m1audio:pcm1"/"pcm2": 4MB of samples for each MultiPCM.
    pub mpcm1: Vec<u8>,
    pub mpcm2: Vec<u8>,
    /// True on Model 2A (SCSP sound board). `sndcpu` is then the SCSP board's
    /// 68000 program and `mpcm1`+`mpcm2` concatenated form the 8MB "samples"
    /// region (already word-swapped), banked into the top of the board's map.
    pub sound_scsp: bool,
    /// The geometry coprocessor the board carries. The original and 2A boards
    /// use the MB86234 TGP (fully emulated); 2B uses the ADSP-21062 SHARC.
    pub coprocessor: crate::roms_db::Board,
}

/// Copies a chip in with the byte order of each 16-bit word exchanged.
pub(crate) fn load16_word_swap(dest: &mut [u8], offset: usize, src: &[u8]) -> Result<(), String> {
    if offset + src.len() > dest.len() {
        return Err(format!("chip at {:#x} overruns its region", offset));
    }
    for (i, pair) in src.chunks_exact(2).enumerate() {
        dest[offset + i * 2] = pair[1];
        dest[offset + i * 2 + 1] = pair[0];
    }
    Ok(())
}

/// Loads a Model 2 game, dispatching on the ROM set in the archive: Daytona
/// USA, Sega Rally Championship (Model 2A), or Virtua Cop.
pub fn load_model2_zip(path: &str) -> Result<Roms, String> {
    let names = archive_names(path)?;
    let def = crate::roms_db::identify(&names)
        .ok_or_else(|| format!("{path}: no matching Model 2 game in the ROM database"))?;
    if def.board.is_model1() {
        return Err(format!("{} is a Model 1 game, not Model 2", def.name));
    }
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|e| e.to_string())?;
    log::info!(target: "loader", "{} ({:?})", def.name, def.board);
    let regions = crate::roms_db::build_regions(def, &mut archive)?;
    Ok(build_model2(regions, def.board))
}

/// Maps the built ROM regions onto the Model 2 `Roms` the system consumes.
fn build_model2(
    mut regions: std::collections::HashMap<String, Vec<u8>>,
    board: crate::roms_db::Board,
) -> Roms {
    let take = |r: &mut std::collections::HashMap<String, Vec<u8>>, name: &str, size: usize| {
        r.remove(name).unwrap_or_else(|| vec![0u8; size])
    };
    // Sound is either the Model 1 audio board (MultiPCM) or the 2A SCSP board,
    // told apart by which regions the set carries.
    let (sndcpu, mpcm1, mpcm2, sound_scsp) = if regions.contains_key("m1audio:sndcpu") {
        (
            take(&mut regions, "m1audio:sndcpu", 0xc0000),
            take(&mut regions, "m1audio:pcm1", 0x400000),
            take(&mut regions, "m1audio:pcm2", 0x400000),
            false,
        )
    } else {
        let sndcpu = take(&mut regions, "audiocpu", 0x80000);
        let mut samples = take(&mut regions, "samples", 0x800000);
        if samples.len() < 0x800000 {
            samples.resize(0x800000, 0);
        }
        let mpcm2 = samples.split_off(samples.len() / 2);
        (sndcpu, samples, mpcm2, true)
    };
    Roms {
        maincpu: take(&mut regions, "maincpu", 0x200000),
        main_data: take(&mut regions, "main_data", 0x2000000),
        eeprom: take(&mut regions, "eeprom", 0),
        copro_data: take(&mut regions, "copro_data", 0x800000),
        copro_tables: take(&mut regions, "copro_tgp_tables", 0x40000),
        polygons: take(&mut regions, "polygons", 0x1000000),
        textures: take(&mut regions, "textures", 0x1000000),
        sndcpu,
        mpcm1,
        mpcm2,
        sound_scsp,
        coprocessor: board,
    }
}

/// Maps the built ROM regions onto the Model 1 `Model1Roms`. The u32 regions
/// are little-endian views of their byte images.
fn build_model1(
    mut regions: std::collections::HashMap<String, Vec<u8>>,
    ioboard_config: Vec<u8>,
) -> Model1Roms {
    let take = |r: &mut std::collections::HashMap<String, Vec<u8>>, name: &str, size: usize| {
        r.remove(name).unwrap_or_else(|| vec![0u8; size])
    };
    let words = |b: Vec<u8>| -> Vec<u32> {
        b.chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    };
    Model1Roms {
        maincpu: {
            let mut m = take(&mut regions, "maincpu", 0x2000000);
            // The V60 region is ROMREGION_ERASEFF; build_regions already filled
            // it, but guard the fallback path too.
            if m.iter().all(|&b| b == 0) {
                m.fill(0xff);
            }
            m
        },
        tgp: take(&mut regions, "tgp_copro", 0x2000),
        copro_tables: words(take(&mut regions, "copro_tables", 0x40000)),
        polygons: words(take(&mut regions, "polygons", 0x1000000)),
        copro_data: words(take(&mut regions, "copro_data", 0x200000)),
        sndcpu: take(&mut regions, "m1audio:sndcpu", 0xc0000),
        mpcm1: take(&mut regions, "m1audio:pcm1", 0x400000),
        mpcm2: take(&mut regions, "m1audio:pcm2", 0x400000),
        iocpu: take(&mut regions, "ioboard:iocpu", 0x10000),
        ioboard_config: {
            // The romset's 93C45 dump is preferred; `vr_defaults.nv` is the
            // same thing under an older name.
            let dumped = take(&mut regions, "ioboard:eeprom", 0);
            if dumped.is_empty() {
                ioboard_config
            } else {
                dumped
            }
        },
    }
}

/// The reference: interleaves a 16-bit chip onto the even or odd
/// 16-bit lane of a little-endian 32-bit region (offset's low bits pick which).
pub(crate) fn load32_word(dest: &mut [u8], offset: usize, src: &[u8]) -> Result<(), String> {
    if !src.len().is_multiple_of(2) {
        return Err(format!(
            "chip size {} is not a whole number of 16-bit words",
            src.len()
        ));
    }
    let end = offset + (src.len() - 2) * 2 + 2;
    if end > dest.len() {
        return Err(format!(
            "chip at offset {:#x} overruns its {:#x}-byte region",
            offset,
            dest.len()
        ));
    }
    for (i, word) in src.chunks_exact(2).enumerate() {
        dest[offset + i * 4] = word[0];
        dest[offset + i * 4 + 1] = word[1];
    }
    Ok(())
}

/// The reference: scatters a byte-wide chip onto one of four byte
/// lanes in a little-endian 32-bit region.
pub(crate) fn load32_byte(
    dest: &mut [u8],
    offset: usize,
    lane: usize,
    src: &[u8],
) -> Result<(), String> {
    if lane >= 4 {
        return Err(format!("invalid 32-bit byte lane {}", lane));
    }
    if offset + src.len() * 4 > dest.len() {
        return Err(format!("chip at {:#x} overruns region", offset));
    }
    for (i, &byte) in src.iter().enumerate() {
        dest[offset + i * 4 + lane] = byte;
    }
    Ok(())
}

/// Reads a chip out of the archive by exact filename.
/// Lists the file names in a ROM archive, for board autodetection.
pub fn archive_names(path: &str) -> Result<Vec<String>, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|e| e.to_string())?;
    Ok((0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|f| f.name().to_string()))
        .collect())
}

/// Loads a Model 1 game, dispatching on the ROM set in the archive: Virtua
/// Racing (315-5573 TGP program) or Virtua Fighter (315-5724).
pub fn load_model1_zip(path: &str) -> Result<Model1Roms, String> {
    let names = archive_names(path)?;
    let def = crate::roms_db::identify(&names)
        .ok_or_else(|| format!("{path}: no matching Model 1 game in the ROM database"))?;
    if !def.board.is_model1() {
        return Err(format!("{} is a Model 2 game, not Model 1", def.name));
    }
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|e| e.to_string())?;
    log::info!(target: "loader", "{} ({:?})", def.name, def.board);
    // The I/O board's operator-config default, if the set ships one; the real
    // Z80 firmware mirrors it into dpram so the game boots configured.
    let ioboard_config = read_chip(&mut archive, "vr_defaults.nv").unwrap_or_default();
    let regions = crate::roms_db::build_regions(def, &mut archive)?;
    Ok(build_model1(regions, ioboard_config))
}

/// Loads Star Wars Arcade, building the V60 memory image
/// `ROM_START(swa)` lays it out.
pub struct Model1Roms {
    /// "maincpu": the V60's whole 32MB address image. The reset vector lives at
    /// region offset 0xfffff0, inside the boot ROMs at 0xfe0000.
    pub maincpu: Vec<u8>,
    /// "tgp_copro": the 8KB program uploaded to the MB86233 (315-5573.bin).
    pub tgp: Vec<u8>,
    /// "copro_tables": the CPU board's math lookup ROM (sin/cos, atan, 1/x,
    /// 1/sqrt(x)), 0x10000 32-bit entries built from opr14742/opr14743. The
    /// TGP's I/O-mapped geometry accelerators index straight into this.
    pub copro_tables: Vec<u32>,
    /// Geometry-board model ROM, exposed as little-endian 32-bit words.
    pub polygons: Vec<u32>,
    /// TGP external data ROM addressed through the I/O 0x8000-0xffff window.
    pub copro_data: Vec<u32>,
    /// The I/O board's Z80 firmware (`epr-14869`), which answers the V60's
    /// commands and owns the 93C45 the operator settings live in.
    pub iocpu: Vec<u8>,
    /// Model 1 sound board (segam1audio): 68000 program + two sample banks.
    pub sndcpu: Vec<u8>,
    pub mpcm1: Vec<u8>,
    pub mpcm2: Vec<u8>,
    /// I/O-board EEPROM/config defaults (`vr_defaults.nv`), if present: the
    /// operator config the real Z80 firmware mirrors into dpram 0x100.. so the
    /// game boots with valid settings instead of the setup menu. One config
    /// byte per 16-bit word (low byte), starting with the "SEGA" magic.
    pub ioboard_config: Vec<u8>,
}

/// Loads Virtua Racing, building the V60 memory image
/// `ROM_START(vr)` lays it out. Only the layout is established here; nothing is
/// executed yet.
pub(crate) fn load16_byte(
    dest: &mut [u8],
    offset: usize,
    lane: usize,
    src: &[u8],
) -> Result<(), String> {
    if offset + src.len() * 2 > dest.len() {
        return Err(format!("chip at {:#x} overruns region", offset));
    }
    for (i, &b) in src.iter().enumerate() {
        dest[offset + i * 2 + lane] = b;
    }
    Ok(())
}

pub(crate) fn copy_at(dest: &mut [u8], offset: usize, src: &[u8]) -> Result<(), String> {
    dest.get_mut(offset..offset + src.len())
        .ok_or_else(|| format!("chip at {:#x} overruns region", offset))?
        .copy_from_slice(src);
    Ok(())
}

pub(crate) fn read_chip(archive: &mut ZipArchive<File>, name: &str) -> Result<Vec<u8>, String> {
    let mut file = archive
        .by_name(name)
        .map_err(|_| format!("ROM '{}' not found in archive", name))?;
    let mut buf = Vec::with_capacity(file.size() as usize);
    file.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}
