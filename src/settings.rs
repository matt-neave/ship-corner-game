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

use crate::modes::{
    BackgroundKind, BackgroundSetting, BloomMode, CrtMode, NightMode, ResolutionKind,
    ResolutionSetting, VsyncMode, WindowModeKind, WindowModeSetting,
};
use crate::sfx::{MusicVolume, SfxVolume};

/// Snapshot of every persisted setting. Lives as a resource only so the
/// load + save systems can share the same struct shape; runtime reads /
/// writes go through the per-feature mode resources.
///
/// `sfx_volume` is stored as a raw `f32` (linear, 0.0..=1.0) rather
/// than a serialised tier so a hand-edited `settings.txt` can dial in
/// any volume the player wants — the settings UI just cycles through
/// the canonical `SfxVolume::STEPS` rungs.
#[derive(Resource, Default, Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub night: bool,
    pub crt: bool,
    pub vsync: bool,
    pub bloom: bool,
    pub window_mode: WindowModeKind,
    pub resolution: ResolutionKind,
    pub sfx_volume: f32,
    pub music_volume: f32,
    pub background: BackgroundKind,
}

impl Settings {
    fn from_modes(
        night: &NightMode,
        crt: &CrtMode,
        vsync: &VsyncMode,
        bloom: &BloomMode,
        win_mode: &WindowModeSetting,
        res: &ResolutionSetting,
        sfx_volume: &SfxVolume,
        music_volume: &MusicVolume,
        background: &BackgroundSetting,
    ) -> Self {
        Self {
            night: night.active,
            crt: crt.active,
            vsync: vsync.enabled,
            bloom: bloom.active,
            window_mode: win_mode.mode,
            resolution: res.res,
            sfx_volume: sfx_volume.0,
            music_volume: music_volume.0,
            background: background.kind,
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
    // Start from the runtime default so a missing key keeps the live
    // default rather than zeroing fields like sfx_volume that have a
    // non-default sensible value.
    let mut s = Settings {
        sfx_volume: SfxVolume::default().0,
        music_volume: MusicVolume::default().0,
        ..Settings::default()
    };
    for line in blob.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let key = k.trim();
        let val = v.trim();
        let bool_val = val == "true";
        match key {
            "night" => s.night = bool_val,
            "crt"   => s.crt   = bool_val,
            "vsync" => s.vsync = bool_val,
            "bloom" => s.bloom = bool_val,
            "window_mode" => if let Some(m) = WindowModeKind::parse(val) {
                s.window_mode = m;
            },
            "resolution"  => if let Some(r) = ResolutionKind::parse(val) {
                s.resolution = r;
            },
            "sfx_volume" => if let Ok(f) = val.parse::<f32>() {
                s.sfx_volume = f.clamp(0.0, 1.0);
            },
            "music_volume" => if let Ok(f) = val.parse::<f32>() {
                s.music_volume = f.clamp(0.0, 1.0);
            },
            "background"  => if let Some(b) = BackgroundKind::parse(val) {
                s.background = b;
            },
            _ => {}
        }
    }
    s
}

fn serialize(s: &Settings) -> String {
    format!(
        "night={}\ncrt={}\nvsync={}\nbloom={}\nwindow_mode={}\nresolution={}\nsfx_volume={}\nmusic_volume={}\nbackground={}\n",
        s.night,
        s.crt,
        s.vsync,
        s.bloom,
        s.window_mode.serialize(),
        s.resolution.serialize(),
        s.sfx_volume,
        s.music_volume,
        s.background.serialize(),
    )
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
    mut bloom: ResMut<BloomMode>,
    mut win_mode: ResMut<WindowModeSetting>,
    mut res: ResMut<ResolutionSetting>,
    mut sfx_vol: ResMut<SfxVolume>,
    mut music_vol: ResMut<MusicVolume>,
    mut bg: ResMut<BackgroundSetting>,
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
    bloom.active = s.bloom;
    win_mode.mode = s.window_mode;
    res.res = s.resolution;
    sfx_vol.0 = s.sfx_volume.clamp(0.0, 1.0);
    music_vol.0 = s.music_volume.clamp(0.0, 1.0);
    bg.kind = s.background;
    commands.insert_resource(s);
}

/// Watch every persisted mode resource for runtime flips and save on
/// change. Cheap; only writes the file when something actually
/// changed.
pub fn persist_settings_on_change(
    night: Res<NightMode>,
    crt: Res<CrtMode>,
    vsync: Res<VsyncMode>,
    bloom: Res<BloomMode>,
    win_mode: Res<WindowModeSetting>,
    res: Res<ResolutionSetting>,
    sfx_vol: Res<SfxVolume>,
    music_vol: Res<MusicVolume>,
    bg: Res<BackgroundSetting>,
    mut last: Local<Option<Settings>>,
) {
    let now = Settings::from_modes(&night, &crt, &vsync, &bloom, &win_mode, &res, &sfx_vol, &music_vol, &bg);
    if last.as_ref() == Some(&now) {
        return;
    }
    *last = Some(now);
    save_to_disk(&now);
}
