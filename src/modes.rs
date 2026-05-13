//! Top-level mode toggles and their visual side-effects:
//!
//! - **`GameMode`** — Sandbox vs Wave (read by lots of systems).
//! - **`NightMode`** — overrides `Palette.ocean` with a near-black tone.
//! - **`CrtMode`** — toggles the scanline overlay sprite.
//! - **`VsyncMode`** — toggles the window's present mode.
//! - **`WindowModeSetting`** — Windowed vs Fullscreen.
//! - **`ResolutionSetting`** — windowed-mode resolution preset.
//!
//! Each toggle holds a `last_applied` field so its `apply_*_mode` system can
//! flip-detect and only do work when the resource actually changes.

use bevy::prelude::*;
use bevy::window::{PresentMode, PrimaryWindow};

use crate::balance::{PLAY_INTERNAL_H, PLAY_INTERNAL_W};
use crate::palette::{hex, Palette};

// ---------- Resources ----------

/// Top-level game-mode resource. Single-variant for now (Sandbox); the
/// enum lets future modes plug in without re-wiring resource insertion.
#[derive(Resource, Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum GameMode {
    #[default]
    Sandbox,
}

/// Toggled by the NIGHT button. When active, swaps `Palette.ocean` to a
/// near-black navy. The previous ocean color is stashed and restored on
/// toggle-off so it composes with future palette changes.
#[derive(Resource, Default)]
pub struct NightMode {
    pub active: bool,
    pub last_applied: Option<bool>,
    pub saved_ocean: Option<Color>,
}

/// Toggled by the CRT button. Shows/hides a scanline overlay on top of the
/// play sprite (the overlay sprite itself is spawned in `setup_render`).
#[derive(Resource, Default)]
pub struct CrtMode {
    pub active: bool,
    pub last_applied: Option<bool>,
}

/// Toggled by the VSYNC button (top-right corner). When active, the primary
/// window uses `AutoVsync`; when off, `AutoNoVsync` so the FPS counter can
/// show the engine's true headroom rather than the monitor's refresh cap.
#[derive(Resource)]
pub struct VsyncMode {
    pub enabled: bool,
    pub last_applied: Option<bool>,
}

impl Default for VsyncMode {
    fn default() -> Self {
        // VSync off by default — input lag and missed-vsync stutters in this
        // game's variable-frame-time loop are noticeable enough that the
        // trade for tear-free presentation isn't worth it. `apply_vsync_mode`
        // flips the window's present_mode on the first frame.
        Self { enabled: false, last_applied: None }
    }
}

/// Toggled by the FOLLOW button (under the MAP button). When active,
/// `apply_camera_follow` writes the friendly ship's world position
/// into the play camera's `Transform.translation` each frame, giving
/// a follow-cam view of combat. When off, the camera snaps back to
/// the world origin (the default fixed view).
#[derive(Resource, Default)]
pub struct CameraFollow {
    pub active: bool,
}

// ---------- Window mode / resolution (user-facing settings) ----------

/// Two-state window mode driven by the settings panel. Borderless
/// fullscreen (rather than exclusive) so alt-tab is fast and the
/// browser-iframe (wasm) build remains the no-op default.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum WindowModeKind {
    #[default]
    Windowed,
    Fullscreen,
}

impl WindowModeKind {
    pub fn cycle(self) -> Self {
        match self {
            Self::Windowed   => Self::Fullscreen,
            Self::Fullscreen => Self::Windowed,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Windowed   => "WINDOWED",
            Self::Fullscreen => "FULLSCREEN",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "windowed"   => Some(Self::Windowed),
            "fullscreen" => Some(Self::Fullscreen),
            _ => None,
        }
    }
    pub fn serialize(self) -> &'static str {
        match self {
            Self::Windowed   => "windowed",
            Self::Fullscreen => "fullscreen",
        }
    }
}

/// User's chosen window mode. `last_applied` lets the apply system
/// flip-detect and only push to the bevy_window resource on change.
#[derive(Resource, Default, Clone, Copy, Debug)]
pub struct WindowModeSetting {
    pub mode: WindowModeKind,
    pub last_applied: Option<WindowModeKind>,
}

/// Fixed-size resolution presets for the windowed-mode dropdown.
/// `Native` means "use the monitor's reported size minus a small
/// margin"; it's the closest thing to "auto-fit" without going
/// borderless-fullscreen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResolutionKind {
    R1280x800,
    R1600x1000,
    R1920x1200,
    Native,
}

impl Default for ResolutionKind {
    fn default() -> Self { Self::R1280x800 }
}

impl ResolutionKind {
    pub fn cycle(self) -> Self {
        match self {
            Self::R1280x800  => Self::R1600x1000,
            Self::R1600x1000 => Self::R1920x1200,
            Self::R1920x1200 => Self::Native,
            Self::Native     => Self::R1280x800,
        }
    }
    /// Concrete `(w, h)` for the preset. `Native` returns `None`
    /// because the actual size has to come from the monitor at
    /// apply time.
    pub fn dimensions(self) -> Option<(f32, f32)> {
        match self {
            Self::R1280x800  => Some((1280.0, 800.0)),
            Self::R1600x1000 => Some((1600.0, 1000.0)),
            Self::R1920x1200 => Some((1920.0, 1200.0)),
            Self::Native     => None,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::R1280x800  => "1280X800",
            Self::R1600x1000 => "1600X1000",
            Self::R1920x1200 => "1920X1200",
            Self::Native     => "NATIVE",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "1280x800"  => Some(Self::R1280x800),
            "1600x1000" => Some(Self::R1600x1000),
            "1920x1200" => Some(Self::R1920x1200),
            "native"    => Some(Self::Native),
            _ => None,
        }
    }
    pub fn serialize(self) -> &'static str {
        match self {
            Self::R1280x800  => "1280x800",
            Self::R1600x1000 => "1600x1000",
            Self::R1920x1200 => "1920x1200",
            Self::Native     => "native",
        }
    }
}

#[derive(Resource, Default, Clone, Copy, Debug)]
pub struct ResolutionSetting {
    pub res: ResolutionKind,
    pub last_applied: Option<ResolutionKind>,
}

// ---------- Marker components ----------

/// CRT scanline overlay sprite — sized to match the play sprite, hidden
/// unless `CrtMode.active`. Spawned by the rendering setup.
#[derive(Component)]
pub struct ScanlineSprite;

// ---------- Layout helpers (used by mode + UI + ship systems) ----------

/// Authoritative play-area screen rect for the current window size. Both
/// the upscale sprite placement and cursor→world mapping read this so
/// they can't drift out of sync as the window resizes. Returns
/// `(left, top, width, height)`.
///
/// Fractional fit-scale: the play area grows / shrinks smoothly with
/// the window instead of snapping to integer multiples of the internal
/// resolution. The previous `.floor()` math was correct in principle
/// (one internal pixel = N screen pixels, crisp) but produced a hard
/// "snap" at every threshold — drag the window 1 px past a boundary
/// and the play area would jump 200 px. With nearest-neighbour sampling
/// on the underlying play-render-target image (set in `setup_render`),
/// fractional scales still preserve the chunky-pixel look; some screen
/// pixels just double-up while others single, which is virtually
/// invisible during gameplay and a clear win over leaving large
/// letterbox gutters.
pub fn play_area_screen_rect(logical_w: f32, logical_h: f32) -> (f32, f32, f32, f32) {
    let scale_x = logical_w / PLAY_INTERNAL_W as f32;
    let scale_y = logical_h / PLAY_INTERNAL_H as f32;
    // Floor at 0.5 so a very small window still shows a usable play
    // area rather than collapsing to nothing. Otherwise pure fit.
    let scale = scale_x.min(scale_y).max(0.5);
    let w = PLAY_INTERNAL_W as f32 * scale;
    let h = PLAY_INTERNAL_H as f32 * scale;
    let left = (logical_w - w) / 2.0;
    let top = (logical_h - h) / 2.0;
    (left, top, w, h)
}

// ---------- Systems ----------

/// Show / hide the scanline overlay when `CrtMode` flips. Uses `last_applied`
/// so we don't write the Visibility component every frame.
pub fn apply_crt_mode(
    mut crt: ResMut<CrtMode>,
    mut q: Query<&mut Visibility, With<ScanlineSprite>>,
) {
    if crt.last_applied == Some(crt.active) { return; }
    crt.last_applied = Some(crt.active);
    for mut v in &mut q {
        *v = if crt.active { Visibility::Inherited } else { Visibility::Hidden };
    }
}

/// Push `VsyncMode.enabled` into the primary window's `present_mode`. Only
/// runs work on flips. `AutoVsync` caps to the monitor's refresh; `AutoNoVsync`
/// lets the renderer go uncapped so the FPS counter reveals real perf headroom.
pub fn apply_vsync_mode(
    mut vsync: ResMut<VsyncMode>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
) {
    if vsync.last_applied == Some(vsync.enabled) { return; }
    vsync.last_applied = Some(vsync.enabled);
    let Ok(mut window) = windows.single_mut() else { return; };
    window.present_mode = if vsync.enabled {
        PresentMode::AutoVsync
    } else {
        PresentMode::AutoNoVsync
    };
}

/// On toggle, write the night-mode override into the live `Palette` so that
/// `apply_palette` propagates the new ocean color to the camera + materials.
pub fn apply_night_mode(
    mut night: ResMut<NightMode>,
    mut palette: ResMut<Palette>,
) {
    if night.last_applied == Some(night.active) { return; }
    night.last_applied = Some(night.active);
    if night.active {
        if night.saved_ocean.is_none() {
            night.saved_ocean = Some(palette.ocean);
        }
        palette.ocean = hex("#1a1c2c");
    } else if let Some(c) = night.saved_ocean.take() {
        palette.ocean = c;
    }
}

/// Push the user's chosen window mode + resolution into the live
/// `Window`. Two settings are folded into one apply system because
/// they interact: switching to fullscreen ignores the resolution
/// pick, and switching back to windowed has to re-apply the cached
/// dimensions. On wasm this is a no-op stub — browser iframes own
/// the canvas size and the user's resolution setting is irrelevant.
#[cfg(target_arch = "wasm32")]
pub fn apply_window_mode_setting() {}

#[cfg(not(target_arch = "wasm32"))]
pub fn apply_window_mode_setting(
    mut win_mode: ResMut<WindowModeSetting>,
    mut res: ResMut<ResolutionSetting>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
) {
    let mode_changed = win_mode.last_applied != Some(win_mode.mode);
    let res_changed  = res.last_applied != Some(res.res);
    if !mode_changed && !res_changed { return; }
    win_mode.last_applied = Some(win_mode.mode);
    res.last_applied = Some(res.res);
    let Ok(mut window) = windows.single_mut() else { return; };
    match win_mode.mode {
        WindowModeKind::Windowed => {
            window.mode = bevy::window::WindowMode::Windowed;
            // Apply the user's chosen preset. `Native` falls through
            // to whatever the OS hands back; we leave the current
            // resolution alone because we can't read the monitor size
            // cheaply from inside an ECS system. The user can pick a
            // specific preset to force a known size.
            if let Some((w, h)) = res.res.dimensions() {
                window.resolution.set(w, h);
            }
        }
        WindowModeKind::Fullscreen => {
            window.mode = bevy::window::WindowMode::BorderlessFullscreen(
                bevy::window::MonitorSelection::Current,
            );
        }
    }
}

/// Drive the play camera's translation each frame:
///   - `CameraFollow.active = true`  → snap to the friendly ship's
///     world position so the view tracks the player.
///   - `CameraFollow.active = false` → reset to the world origin
///     (the default fixed view, framing the whole `PLAY_WORLD`).
///
/// `Without<crate::components::Friendly>` on the camera query keeps
/// it disjoint from the friendly query for Bevy's parameter-conflict
/// checker.
pub fn apply_camera_follow(
    time: Res<Time>,
    follow: Res<CameraFollow>,
    mut shake: ResMut<ScreenShake>,
    friendly: Query<
        &Transform,
        (
            With<crate::components::Friendly>,
            Without<crate::palette::PlayCamera>,
            Without<crate::palette::HudCamera>,
        ),
    >,
    // HudCamera shares the play camera's world projection and must
    // track the same translation so HUD-layer entities (HP bars) line
    // up with their owners in follow mode.
    mut cameras: Query<
        &mut Transform,
        (
            Or<(With<crate::palette::PlayCamera>, With<crate::palette::HudCamera>)>,
            Without<crate::components::Friendly>,
        ),
    >,
) {
    // With `big_arena` the arena is wider than the viewport — the
    // camera HAS to follow the player and clamp to the arena edges,
    // otherwise the screen shows water beyond the playable bounds.
    // Without `big_arena`, the FOLLOW button is the only control.
    let force_follow = crate::balance::arena_overruns_viewport();
    let player_pos = friendly.single().ok()
        .map(|f_tf| f_tf.translation.truncate())
        .unwrap_or(Vec2::ZERO);
    let target = if force_follow || follow.active {
        clamp_camera_to_arena(player_pos)
    } else {
        Vec2::ZERO
    };

    // Trauma-based shake (Linden's method): trauma in [0, 1], offset
    // proportional to trauma². Decays each frame.
    let dt = time.delta_secs();
    shake.trauma = (shake.trauma - dt * SHAKE_DECAY).max(0.0);
    let shake_off = if shake.trauma > 0.0 {
        let s = shake.trauma * shake.trauma;
        use rand::Rng;
        let mut rng = rand::thread_rng();
        Vec2::new(
            rng.gen_range(-1.0..1.0),
            rng.gen_range(-1.0..1.0),
        ) * SHAKE_MAX_OFFSET * s
    } else {
        Vec2::ZERO
    };

    for mut cam_tf in &mut cameras {
        cam_tf.translation.x = target.x + shake_off.x;
        cam_tf.translation.y = target.y + shake_off.y;
    }
}

/// Clamp the camera centre so the viewport stays inside the arena.
/// Half the overscan (arena minus viewport) on each axis is the
/// maximum the camera can drift from origin before the viewport
/// edge would fall outside the arena.
///
/// When the arena equals the viewport (default, no `big_arena`) the
/// clamp collapses to `0` on both axes — the camera sits at origin
/// regardless of player position. That branch is also why this still
/// produces sensible output for the user-toggled FOLLOW mode in the
/// default build: the player moves around within a fixed view.
fn clamp_camera_to_arena(player_pos: bevy::math::Vec2) -> bevy::math::Vec2 {
    let half_overscan_x = ((crate::balance::ARENA_W - crate::balance::PLAY_WORLD_W) * 0.5).max(0.0);
    let half_overscan_y = ((crate::balance::ARENA_H - crate::balance::PLAY_WORLD_H) * 0.5).max(0.0);
    bevy::math::Vec2::new(
        player_pos.x.clamp(-half_overscan_x, half_overscan_x),
        player_pos.y.clamp(-half_overscan_y, half_overscan_y),
    )
}

// ---------- Screen shake ----------

/// Trauma-based camera shake. Callers add trauma via `add_trauma`;
/// `apply_camera_follow` decays it each frame and offsets every camera
/// that follows the play world. Range is [0, 1]; offset scales with
/// `trauma²`, so small kicks barely register and big kicks really
/// punch.
#[derive(Resource, Default)]
pub struct ScreenShake {
    pub trauma: f32,
}

impl ScreenShake {
    pub fn add_trauma(&mut self, amount: f32) {
        self.trauma = (self.trauma + amount).clamp(0.0, 1.0);
    }
}

/// Trauma → 0 in this many seconds when nothing else feeds it.
const SHAKE_DECAY: f32 = 1.6;
/// Peak offset in world units when trauma == 1.0. Bumped up so a
/// solid hit at ~0.7 trauma reads as a real punch (0.7² × 9 ≈ 4.4
/// world units).
const SHAKE_MAX_OFFSET: f32 = 9.0;
