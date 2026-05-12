//! Top-level mode toggles and their visual side-effects:
//!
//! - **`GameMode`** — Sandbox vs Wave (read by lots of systems).
//! - **`WindowMode`** — desktop overlay vs windowed UI.
//! - **`NightMode`** — overrides `Palette.ocean` with a near-black tone.
//! - **`CrtMode`** — toggles the scanline overlay sprite.
//!
//! Each toggle holds a `last_applied` field so its `apply_*_mode` system can
//! flip-detect and only do work when the resource actually changes.

use bevy::prelude::*;
use bevy::window::{PresentMode, PrimaryWindow};
// Direct winit imports are only used by the borderless-desktop window
// systems below, which are cfg-gated off the wasm target (a browser
// embed has no movable / resizable OS window for these to drive).
#[cfg(not(target_arch = "wasm32"))]
use bevy::winit::WinitWindows;
#[cfg(not(target_arch = "wasm32"))]
use winit::window::ResizeDirection;

use crate::balance::{PLAY_INTERNAL_H, PLAY_INTERNAL_W, WINDOW_H, WINDOW_W};
use crate::palette::{hex, Palette};
use crate::ui::{ScoreText, UiPanel};

// ---------- Resources ----------

/// Top-level game-mode resource. Single-variant for now (Sandbox); the
/// enum lets future modes plug in without re-wiring resource insertion.
#[derive(Resource, Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum GameMode {
    #[default]
    Sandbox,
}

/// Toggled by the DESKTOP button. `desktop = true` hides the LHS UI panel,
/// shrinks the window to play-area-only, and snaps to the bottom-right of
/// the monitor the window is currently on.
#[derive(Resource, Default, Clone, Copy)]
pub struct WindowMode {
    pub desktop: bool,
    pub last_applied: Option<bool>,
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

// ---------- Marker components ----------

/// CRT scanline overlay sprite — sized to match the play sprite, hidden
/// unless `CrtMode.active`. Spawned by the rendering setup.
#[derive(Component)]
pub struct ScanlineSprite;

/// Hint text shown only in desktop mode. Player presses Escape to return.
#[derive(Component)]
pub struct DesktopHint;

// ---------- Layout helpers (used by mode + UI + ship systems) ----------

/// Authoritative play-area screen rect for the current window size. Both
/// the upscale sprite placement and cursor→world mapping read this so
/// they can't drift out of sync as the window resizes. `ui_width` is 0
/// in desktop mode. Returns `(left, top, width, height)`.
pub fn play_area_screen_rect(logical_w: f32, logical_h: f32, ui_width: f32) -> (f32, f32, f32, f32) {
    let avail_w = (logical_w - ui_width).max(0.0);
    let scale_x = (avail_w / PLAY_INTERNAL_W as f32).floor();
    let scale_y = (logical_h / PLAY_INTERNAL_H as f32).floor();
    let scale = scale_x.min(scale_y).max(1.0);
    let w = PLAY_INTERNAL_W as f32 * scale;
    let h = PLAY_INTERNAL_H as f32 * scale;
    let left = ui_width + (avail_w - w) / 2.0;
    let top = (logical_h - h) / 2.0;
    (left, top, w, h)
}

pub fn effective_ui_width(_mode: &WindowMode) -> f32 {
    // LHS UI panel is hidden for the prototype — keep the play area
    // centered in the window rather than nudged right by a phantom
    // panel. Restoring the panel = revert this to the desktop-vs-
    // windowed branch on `mode.desktop`.
    0.0
}

// ---------- Systems ----------

/// Esc exits desktop mode back to the windowed UI. No-op in windowed mode.
pub fn handle_desktop_escape(
    keys: Res<ButtonInput<KeyCode>>,
    mut mode: ResMut<WindowMode>,
) {
    if mode.desktop && keys.just_pressed(KeyCode::Escape) {
        mode.desktop = false;
    }
}

/// In desktop mode the window has no decorations, so the OS won't drag or
/// resize it for us. On LMB press we hand the gesture off to winit:
/// near a corner / edge → start a system-resize; anywhere else → drag.
///
/// WASM build provides a no-op stub: a browser-embedded canvas can't
/// be dragged/resized via winit's API, and the surrounding desktop
/// chrome (decorations, monitor positioning) doesn't exist there.
#[cfg(target_arch = "wasm32")]
pub fn handle_desktop_drag_resize() {}

#[cfg(not(target_arch = "wasm32"))]
pub fn handle_desktop_drag_resize(
    mode: Res<WindowMode>,
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<(Entity, &Window), With<PrimaryWindow>>,
    winit_windows: NonSend<WinitWindows>,
) {
    if !mode.desktop { return; }
    if !mouse.just_pressed(MouseButton::Left) { return; }
    let Ok((entity, window)) = windows.single() else { return; };
    let Some(cursor) = window.cursor_position() else { return; };
    let Some(winit_win) = winit_windows.get_window(entity) else { return; };

    let w = window.width();
    let h = window.height();
    let m = 8.0;
    let near_left   = cursor.x < m;
    let near_right  = cursor.x > w - m;
    let near_top    = cursor.y < m;
    let near_bottom = cursor.y > h - m;

    let dir = match (near_left, near_right, near_top, near_bottom) {
        (true,  false, true,  false) => Some(ResizeDirection::NorthWest),
        (false, true,  true,  false) => Some(ResizeDirection::NorthEast),
        (true,  false, false, true ) => Some(ResizeDirection::SouthWest),
        (false, true,  false, true ) => Some(ResizeDirection::SouthEast),
        (true,  false, false, false) => Some(ResizeDirection::West),
        (false, true,  false, false) => Some(ResizeDirection::East),
        (false, false, true,  false) => Some(ResizeDirection::North),
        (false, false, false, true ) => Some(ResizeDirection::South),
        _ => None,
    };
    let _ = match dir {
        Some(d) => winit_win.drag_resize_window(d),
        None    => winit_win.drag_window(),
    };
}

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

/// Toggle between full-window mode (UI panel + play area) and "desktop"
/// mode (play area only, no decorations, snapped to bottom-right of the
/// current monitor). Runs only when `WindowMode` flips.
///
/// WASM build is a no-op stub for the same reason as
/// `handle_desktop_drag_resize` above — the canvas is whatever size
/// the embedding page allows, no monitor metrics, no decorations to
/// strip.
#[cfg(target_arch = "wasm32")]
pub fn apply_window_mode() {}

#[cfg(not(target_arch = "wasm32"))]
pub fn apply_window_mode(
    mut mode: ResMut<WindowMode>,
    mut windows: Query<(Entity, &mut Window), With<PrimaryWindow>>,
    winit_windows: NonSend<WinitWindows>,
    mut panels: Query<&mut Visibility, (With<UiPanel>, Without<ScoreText>, Without<DesktopHint>)>,
    mut score:  Query<&mut Visibility, (With<ScoreText>, Without<UiPanel>, Without<DesktopHint>)>,
    mut hint:   Query<&mut Visibility, (With<DesktopHint>, Without<UiPanel>, Without<ScoreText>)>,
) {
    if mode.last_applied == Some(mode.desktop) { return; }
    mode.last_applied = Some(mode.desktop);
    let Ok((entity, mut window)) = windows.single_mut() else { return; };

    if mode.desktop {
        for mut v in &mut panels { *v = Visibility::Hidden; }
        for mut v in &mut score  { *v = Visibility::Hidden; }
        for mut v in &mut hint   { *v = Visibility::Inherited; }

        // Borderless desktop window sized to fit the largest integer
        // multiple of PLAY_INTERNAL_H (the shorter axis) ≤ ~480 px,
        // then widened proportionally for the wide-play feature flag.
        let target_logical: u32 = 480;
        let scale_int = (target_logical as f32 / PLAY_INTERNAL_H as f32).floor().max(1.0) as u32;
        let logical_h = (PLAY_INTERNAL_H * scale_int) as f32;
        let logical_w = (PLAY_INTERNAL_W * scale_int) as f32;

        // Drop decorations FIRST so the size we set is the actual content
        // size — otherwise winit shrinks the content to fit a phantom title bar.
        window.decorations = false;
        window.resolution.set(logical_w, logical_h);
        window.window_level = bevy::window::WindowLevel::AlwaysOnTop;
        window.resizable = true;

        // Snap to the bottom-right of the current monitor in physical pixels —
        // Bevy's `MonitorSelection::Current` only works for `Centered`, not
        // for absolute placement, and we need physical pixels anyway.
        if let Some(winit_win) = winit_windows.get_window(entity) {
            let monitor = winit_win.current_monitor()
                .or_else(|| winit_win.primary_monitor());
            if let Some(monitor) = monitor {
                let mon_pos  = monitor.position();
                let mon_size = monitor.size();
                let scale_f  = winit_win.scale_factor() as f32;
                let phys_w   = (logical_w * scale_f).round() as i32;
                let phys_h   = (logical_h * scale_f).round() as i32;
                const MARGIN: i32 = 16;
                let x = mon_pos.x + mon_size.width  as i32 - phys_w - MARGIN;
                let y = mon_pos.y + mon_size.height as i32 - phys_h - MARGIN;
                window.position = bevy::window::WindowPosition::At(IVec2::new(x, y));
            }
        }
    } else {
        for mut v in &mut panels { *v = Visibility::Inherited; }
        for mut v in &mut score  { *v = Visibility::Inherited; }
        for mut v in &mut hint   { *v = Visibility::Hidden; }
        window.resolution.set(WINDOW_W, WINDOW_H);
        window.decorations = true;
        window.window_level = bevy::window::WindowLevel::Normal;
        // Stay on the monitor the user is on rather than jumping to Primary.
        window.position = bevy::window::WindowPosition::Centered(
            bevy::window::MonitorSelection::Current,
        );
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
