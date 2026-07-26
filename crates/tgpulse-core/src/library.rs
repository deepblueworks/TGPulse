//! The ROM library: what is in the `roms` directory and what it is.
//!
//! A romset is a zip archive of the individual chip dumps. The archive's own
//! name carries no authority -- people rename them -- so each one is identified
//! by matching the files inside it against the ROM database, which is the same
//! thing the loader does when it builds the memory image.
//!
//! Scanning is cheap: it reads each archive's central directory, never its
//! contents.

use std::path::{Path, PathBuf};

use crate::loader;
use crate::roms_db::{self, Board, GameDef, Scheme};

/// Where romsets live by default, relative to the working directory.
pub const DEFAULT_DIR: &str = "roms";

/// Archive extensions worth opening.
const EXTENSIONS: [&str; 1] = ["zip"];

/// One archive in the library, with whatever the database knows about it.
#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    /// Short set name (`daytona`, `vf2`), or the file stem when unrecognised.
    pub set: String,
    pub title: String,
    pub year: String,
    pub manufacturer: String,
    pub board: Option<Board>,
    pub scheme: Option<Scheme>,
    /// Files the matched set expects that the archive does not contain. A set
    /// can still be playable with a few missing -- the loader fills the gaps
    /// with the region's fill byte -- so this is shown, not enforced.
    pub missing: Vec<String>,
}

impl Entry {
    /// Whether the database recognised the archive at all.
    pub fn is_known(&self) -> bool {
        self.board.is_some()
    }

    /// Whether every file the set expects is present.
    pub fn is_complete(&self) -> bool {
        self.is_known() && self.missing.is_empty()
    }

    /// One-line status for a list view.
    pub fn status(&self) -> String {
        if !self.is_known() {
            "unrecognised".to_string()
        } else if self.missing.is_empty() {
            "ok".to_string()
        } else {
            format!("{} file(s) missing", self.missing.len())
        }
    }
}

/// Reads a directory of archives. Returns entries sorted by title, with the
/// unrecognised ones last so a misnamed download does not hide the library.
///
/// A directory that does not exist is not an error: it just has nothing in it.
pub fn scan(dir: impl AsRef<Path>) -> Vec<Entry> {
    let dir = dir.as_ref();
    let Ok(read) = std::fs::read_dir(dir) else {
        log::info!(target: "library", "no ROM directory at {}", dir.display());
        return Vec::new();
    };

    let mut out: Vec<Entry> = read
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
                .unwrap_or(false)
        })
        .map(|p| describe(&p))
        .collect();

    out.sort_by(|a, b| {
        b.is_known()
            .cmp(&a.is_known())
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });
    log::info!(target: "library", "{} romset(s) in {}", out.len(), dir.display());
    out
}

/// Identifies a single archive.
pub fn describe(path: &Path) -> Entry {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

    let names = loader::archive_names(&path.to_string_lossy()).unwrap_or_default();
    match roms_db::identify(&names) {
        Some(def) => Entry {
            path: path.to_path_buf(),
            set: def.name.clone(),
            title: def.title.clone(),
            year: def.year.clone(),
            manufacturer: def.manufacturer.clone(),
            board: Some(def.board),
            scheme: Some(def.scheme),
            missing: missing_files(def, &names),
        },
        None => Entry {
            path: path.to_path_buf(),
            title: stem.clone(),
            set: stem,
            year: String::new(),
            manufacturer: String::new(),
            board: None,
            scheme: None,
            missing: Vec::new(),
        },
    }
}

fn missing_files(def: &GameDef, present: &[String]) -> Vec<String> {
    let have: std::collections::HashSet<&str> = present.iter().map(String::as_str).collect();
    let mut missing: Vec<String> = def
        .files()
        .filter(|f| !have.contains(f))
        .map(str::to_string)
        .collect();
    missing.sort();
    missing.dedup();
    missing
}

/// Finds one romset by short name, so the command line can take `vf2` rather
/// than a path.
pub fn find(dir: impl AsRef<Path>, set: &str) -> Option<Entry> {
    scan(dir).into_iter().find(|e| e.set == set)
}
