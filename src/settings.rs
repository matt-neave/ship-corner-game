//! Persistent user settings (NIGHT / CRT / VSYNC).
//!
//! Stored as a tiny `key=value` text file at the OS-standard config dir:
//!   - Windows: `%APPDATA%\ship-game\settings.txt`
//!   - Linux:   `~/.config/ship-game/settings.txt`
//!   - macOS:   `~/Library/Application Support/ship-game/settings.txt`
//!
//! Cross-platform path discovery uses the `dirs` crate so we don't have
//! to branch on OS ourselves. If the dir lookup fails (sandbox, missing
//! HOME, etc.), settings still work in-memory but won't persist.
//!
//! Flow
//! ----
//! - **Startup**: `apply_loaded_settings` runs once on the first frame,
//!   reads the file (if any), and writes the values into the live
//!   `NightMode` / `CrtMode` / `VsyncMode` resources. Existing
//!   `apply_*_mode` systems then propagate the change to the world.
//! - **Per-frame**: `persist_settings_on_change` watches those three
//!   resources for `is_changed()`. On any flip, snapshots the current
//!   values into a single `Settings` and writes the file.

use bevy::prelude::*;
use std::fs;
use std::path::PathBuf;

use crate::modes::{CrtMode, NightMode, VsyncMode};

/// Snapshot of every persisted setting. Lives as a resource only so the
/// load + save systems can share the same struct shape; runtime reads /
/// writes go through the per-feature mode resources.
#[derive(Resource, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub night: bool,
    pub crt: bool,
    pub vsync: bool,
}

impl Settings {
    fn from_modes(night: &NightMode, crt: &CrtMode, vsync: &VsyncMode) -> Self {
        Self {
            night: night.active,
            crt: crt.active,
            vsync: vsync.enabled,
        }
    }
}

const SETTINGS_FILE: &str = "settings.txt";
const APP_FOLDER: &str = "ship-game";

/// Full on-disk path. Returns `None` if the OS config dir lookup fails.
/// On WASM there's no filesystem and no `dirs` crate available, so this
/// always returns None — `load`/`save` callers already treat that as
/// "skip persistence", which is the right behaviour inside a browser
/// embed (settings reset each page load).
#[cfg(not(target_arch = "wasm32"))]
fn settings_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join(APP_FOLDER).join(SETTINGS_FILE))
}

#[cfg(target_arch = "wasm32")]
fn settings_path() -> Option<PathBuf> {
    None
}

/// Parse a `key=value` text blob. Anything malformed is silently
/// dropped — defaults will fill the gap.
fn parse(blob: &str) -> Settings {
    let mut s = Settings::default();
    for line in blob.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim() == "true";
        match k.trim() {
            "night" => s.night = v,
            "crt"   => s.crt   = v,
            "vsync" => s.vsync = v,
            _ => {}
        }
    }
    s
}

fn serialize(s: &Settings) -> String {
    format!("night={}\ncrt={}\nvsync={}\n", s.night, s.crt, s.vsync)
}

/// Read the settings file. Missing file / unreadable disk returns
/// `Settings::default()` — first-run experience is "everything off".
fn load_from_disk() -> Settings {
    let Some(path) = settings_path() else { return Settings::default() };
    match fs::read_to_string(&path) {
        Ok(blob) => parse(&blob),
        Err(_) => Settings::default(),
    }
}

/// Write the settings file. Best-effort: missing dir is created
/// recursively; any I/O error is swallowed (logging only) so a
/// permission issue doesn't crash the game.
fn save_to_disk(s: &Settings) {
    let Some(path) = settings_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Err(e) = fs::write(&path, serialize(s)) {
        warn!("Failed to write settings file at {:?}: {}", path, e);
    }
}

// ---------- Bevy systems ----------

/// Read the file once at startup and write values into the live mode
/// resources. Runs first frame; the per-mode `apply_*_mode` systems
/// will then propagate the change to the world.
pub fn apply_loaded_settings(
    mut night: ResMut<NightMode>,
    mut crt: ResMut<CrtMode>,
    mut vsync: ResMut<VsyncMode>,
    mut commands: Commands,
    mut done: Local<bool>,
) {
    if *done {
        return;
    }
    *done = true;

    let s = load_from_disk();
    night.active = s.night;
    crt.active = s.crt;
    vsync.enabled = s.vsync;
    commands.insert_resource(s);
}

/// Watch the three mode resources for runtime flips and persist them.
/// Cheap (3 `is_changed` checks); only writes the file when something
/// actually changed.
pub fn persist_settings_on_change(
    night: Res<NightMode>,
    crt: Res<CrtMode>,
    vsync: Res<VsyncMode>,
    mut last: Local<Option<Settings>>,
) {
    let now = Settings::from_modes(&night, &crt, &vsync);
    if last.as_ref() == Some(&now) {
        return;
    }
    *last = Some(now);
    save_to_disk(&now);
}
