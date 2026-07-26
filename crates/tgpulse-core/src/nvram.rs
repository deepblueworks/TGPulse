//! Battery-backed storage that survives a run.
//!
//! A Model 1/2 cabinet keeps its bookkeeping in two places: a battery-backed
//! SRAM (the "backup RAM") and, on the 2A/2B/2C video boards, a small serial
//! EEPROM. Games checksum both at boot; starting from blank memory every time
//! makes them announce "BACKUP RAM IS BROKEN. INITIALIZED." on every launch and
//! forget settings and high scores in between.
//!
//! Files live in `nvram/<game>.nv` next to the working directory, mirroring
//! The reference layout closely enough to be recognisable.

use std::path::PathBuf;

/// A tagged container so the file survives a change to either block's size.
const MAGIC: &[u8; 8] = b"TGPULSE1";

pub fn path_for(game: &str) -> PathBuf {
    let mut p = PathBuf::from("nvram");
    p.push(format!("{game}.nv"));
    p
}

/// Serialises the two blocks with their lengths, so a later build that resizes
/// one of them rejects the stale half instead of misreading it.
pub fn encode(backup: &[u8], eeprom: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(MAGIC.len() + 8 + backup.len() + eeprom.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(backup.len() as u32).to_le_bytes());
    out.extend_from_slice(&(eeprom.len() as u32).to_le_bytes());
    out.extend_from_slice(backup);
    out.extend_from_slice(eeprom);
    out
}

/// Returns `(backup, eeprom)` if the file is one of ours and both blocks are
/// the size this build expects.
pub fn decode(blob: &[u8], backup_len: usize, eeprom_len: usize) -> Option<(Vec<u8>, Vec<u8>)> {
    if blob.len() < MAGIC.len() + 8 || &blob[..MAGIC.len()] != MAGIC {
        return None;
    }
    let n = |off: usize| {
        u32::from_le_bytes([blob[off], blob[off + 1], blob[off + 2], blob[off + 3]]) as usize
    };
    let (bl, el) = (n(8), n(12));
    if bl != backup_len || el != eeprom_len || blob.len() < 16 + bl + el {
        return None;
    }
    Some((
        blob[16..16 + bl].to_vec(),
        blob[16 + bl..16 + bl + el].to_vec(),
    ))
}

/// A shipped starting configuration for a game, used when the player has no
/// saved NVRAM yet.
///
/// These machines keep region, cabinet type, difficulty and coinage in
/// battery-backed RAM, not in DIP switches -- on real hardware you set them
/// from the game's own test menu, and the reference models them the same way (its Model
/// 1/2 DIP ports are almost entirely "unused" placeholders). So a default is
/// simply a captured NVRAM image: configure the game once, write it here, and
/// every later first boot starts from that configuration instead of whatever
/// the game's own cold-boot defaults happen to be.
pub fn defaults_path_for(game: &str) -> PathBuf {
    let mut p = PathBuf::from("nvram");
    p.push("defaults");
    p.push(format!("{game}.nv"));
    p
}

pub fn load(game: &str, backup_len: usize, eeprom_len: usize) -> Option<(Vec<u8>, Vec<u8>)> {
    // The player's own NVRAM wins; a shipped default only seeds a first boot.
    if let Ok(blob) = std::fs::read(path_for(game)) {
        if let Some(v) = decode(&blob, backup_len, eeprom_len) {
            return Some(v);
        }
    }
    let blob = std::fs::read(defaults_path_for(game)).ok()?;
    let v = decode(&blob, backup_len, eeprom_len)?;
    log::info!(target: "nvram", "starting from the shipped defaults for {game}");
    Some(v)
}

/// Records the current NVRAM as this game's shipped default.
pub fn save_defaults(game: &str, backup: &[u8], eeprom: &[u8]) -> Result<PathBuf, String> {
    let path = defaults_path_for(game);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, encode(backup, eeprom)).map_err(|e| e.to_string())?;
    Ok(path)
}

pub fn save(game: &str, backup: &[u8], eeprom: &[u8]) {
    let path = path_for(game);
    if let Some(dir) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            log::warn!(target: "nvram", "cannot create {}: {e}", dir.display());
            return;
        }
    }
    if let Err(e) = std::fs::write(&path, encode(backup, eeprom)) {
        log::warn!(target: "nvram", "cannot write {}: {e}", path.display());
    }
}
