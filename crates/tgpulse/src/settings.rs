//! The player's adjustments, remembered between runs.
//!
//! Everything the settings window touches is written to `config/settings.conf`
//! the moment it changes and read back at startup, so the emulator comes back
//! the way it was left. A command-line flag overrides the file for that run
//! without rewriting it, and the debugger never reads it, so scripts and
//! captures stay reproducible.
//!
//! The file is plain `key = value` lines, written out in full on first run so
//! it is discoverable and hand-editable. A line the parser does not understand
//! is warned about and skipped; a setting the file does not mention keeps its
//! shipped value, so a file written by an older build still works.

use std::path::{Path, PathBuf};

use tgpulse_core::config::{Cabinet, Config};

/// The adjustable subset of `Config` that is worth remembering between runs.
///
/// Fullscreen is deliberately not here. It is how the window is being looked
/// at right now, not a preference: toggling it with the hotkey mid-game would
/// otherwise decide how the emulator starts next time, which is a surprise
/// nobody asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub ssaa: u32,
    pub widescreen: bool,
    pub widescreen_stretch_2d: bool,
    pub smooth_shadows: bool,
    pub volume: u32,
    pub rumble: bool,
    pub cabinet: Cabinet,
}

impl Default for Settings {
    fn default() -> Self {
        Self::from_config(&Config::default())
    }
}

impl Settings {
    pub fn from_config(config: &Config) -> Self {
        Self {
            ssaa: config.ssaa,
            widescreen: config.widescreen,
            widescreen_stretch_2d: config.widescreen_stretch_2d,
            smooth_shadows: config.smooth_shadows,
            volume: config.volume,
            rumble: config.rumble,
            cabinet: config.cabinet,
        }
    }

    pub fn apply_to(&self, config: &mut Config) {
        config.ssaa = self.ssaa;
        config.widescreen = self.widescreen;
        config.widescreen_stretch_2d = self.widescreen_stretch_2d;
        config.smooth_shadows = self.smooth_shadows;
        config.volume = self.volume;
        config.rumble = self.rumble;
        config.cabinet = self.cabinet;
    }

    pub fn path() -> PathBuf {
        PathBuf::from("config").join("settings.conf")
    }

    /// Reads the file, writing the defaults out first if there is none yet, so
    /// the format is discoverable without having to change something in the
    /// interface to make one appear.
    pub fn load_or_create(path: &Path) -> Self {
        if !path.exists() {
            let defaults = Self::default();
            if let Err(e) = defaults.save(path) {
                log::warn!(target: "settings", "cannot write {}: {e}", path.display());
            }
            return defaults;
        }
        Self::load(path)
    }

    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let mut settings = Self::default();
        for (number, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let Some((name, value)) = line.split_once('=') else {
                log::warn!(target: "settings", "{}:{}: not a setting", path.display(), number + 1);
                continue;
            };
            let (name, value) = (name.trim(), value.trim());
            let boolean = |v: &str| match v {
                "on" | "true" | "1" => Some(true),
                "off" | "false" | "0" => Some(false),
                _ => None,
            };
            match name {
                "ssaa" => match value.parse::<u32>().ok().filter(|n| (1..=4).contains(n)) {
                    Some(n) => settings.ssaa = n,
                    None => {
                        log::warn!(target: "settings", "{}:{}: bad ssaa '{value}' (want 1..4)", path.display(), number + 1)
                    }
                },
                "widescreen" => settings.widescreen = boolean(value).unwrap_or(settings.widescreen),
                "widescreen_stretch_2d" => {
                    settings.widescreen_stretch_2d =
                        boolean(value).unwrap_or(settings.widescreen_stretch_2d)
                }
                "smooth_shadows" => {
                    settings.smooth_shadows = boolean(value).unwrap_or(settings.smooth_shadows)
                }
                // Fullscreen used to be written here. It is a view mode, not a
                // preference: it belongs to the run, set by --fullscreen or
                // toggled with the hotkey. Accepted and ignored so an older
                // file does not warn.
                "fullscreen" => {}
                "volume" => match value.parse::<u32>() {
                    Ok(n) => settings.volume = n,
                    Err(_) => {
                        log::warn!(target: "settings", "{}:{}: bad volume '{value}'", path.display(), number + 1)
                    }
                },
                "rumble" => settings.rumble = boolean(value).unwrap_or(settings.rumble),
                "cabinet" => match value.parse::<Cabinet>() {
                    Ok(c) => settings.cabinet = c,
                    Err(_) => {
                        log::warn!(target: "settings", "{}:{}: unknown cabinet '{value}'", path.display(), number + 1)
                    }
                },
                other => {
                    log::warn!(target: "settings", "{}:{}: unknown setting '{other}'", path.display(), number + 1)
                }
            }
        }
        log::info!(target: "settings", "settings from {}", path.display());
        settings
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let on_off = |b: bool| if b { "on" } else { "off" };
        let cabinet = match self.cabinet {
            Cabinet::Twin => "twin",
            Cabinet::Single => "single",
        };
        let out = format!(
            "# TGPulse settings.\n\
             #\n\
             # Written when a setting changes in the interface; the command line\n\
             # overrides any of these for one run without rewriting the file.\n\
             # Delete a line to go back to the shipped value.\n\
             \n\
             ssaa = {}\n\
             widescreen = {}\n\
             widescreen_stretch_2d = {}\n\
             smooth_shadows = {}\n\
             volume = {}\n\
             rumble = {}\n\
             cabinet = {}\n",
            self.ssaa,
            on_off(self.widescreen),
            on_off(self.widescreen_stretch_2d),
            on_off(self.smooth_shadows),
            self.volume,
            on_off(self.rumble),
            cabinet,
        );
        std::fs::write(path, out).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_file_loads_back_identically() {
        let dir = std::env::temp_dir().join(format!("tgpulse-settings-{}", std::process::id()));
        let path = dir.join("settings.conf");
        let settings = Settings {
            ssaa: 4,
            widescreen: true,
            smooth_shadows: false,
            volume: 400,
            cabinet: Cabinet::Twin,
            ..Settings::default()
        };
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path), settings);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_partial_file_keeps_shipped_values_for_the_rest() {
        let dir =
            std::env::temp_dir().join(format!("tgpulse-settings-partial-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.conf");
        std::fs::write(&path, "volume = 250\nunknown_thing = yes\n").unwrap();
        let settings = Settings::load(&path);
        assert_eq!(settings.volume, 250);
        assert_eq!(settings.ssaa, Settings::default().ssaa);
        assert_eq!(settings.cabinet, Settings::default().cabinet);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_and_from_config_round_trip() {
        let mut config = Config::default();
        Settings {
            volume: 300,
            ..Settings::default()
        }
        .apply_to(&mut config);
        assert_eq!(config.volume, 300);
        assert_eq!(
            Settings::from_config(&config),
            Settings {
                volume: 300,
                ..Settings::default()
            }
        );
    }
}
