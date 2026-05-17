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

// ---------- Background pattern (HashSprite kind) ----------

/// Pattern type painted on the diagonal-tiled backdrop that sits
/// behind the play sprite. Player can cycle this from settings.
/// Each variant maps to a generator in `rendering::make_background_image`.
///
/// The first six variants are "themed" — they take their colours from the
/// live palette's `ocean` (and a darkened-derived companion), so they shift
/// when night mode toggles or the palette changes. Every variant below
/// the divider hard-codes its own colour pair, giving the player a wide
/// gallery of distinct looks beyond the default blue.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BackgroundKind {
    // ---- Themed (palette-driven) ----
    /// Diagonal hash with the existing 192-tile period.
    HashDiagonal,
    /// Diagonal hash mirrored to the opposite slope.
    HashDiagonalReverse,
    /// Fine 45-degree hash, much shorter period than the default.
    HashFine,
    /// Vertical bars.
    VerticalStripes,
    /// Horizontal bands.
    HorizontalStripes,
    /// 32-tile crosshatch — busier than the default hash.
    Checker,
    /// Large 96-tile checker.
    LargeChecker,
    /// Thin crosshatch grid lines on the ocean colour.
    GridDefault,
    /// Solid ocean colour, no pattern.
    Plain,

    // ---- Sand / warm ----
    /// Cream dots on warm sand.
    DotsWarm,
    /// Sinusoidal warm-beige wave bands.
    WavesSand,
    /// Terracotta brick pattern.
    BricksRed,
    /// Sunset diagonal hash (orange / dusky red).
    HashSunset,
    /// Warm amber diagonal hash on deep brown.
    HashAmber,
    /// Sparse amber stars on near-black brown — campfire embers.
    StarsAmber,
    /// Copper sinusoidal waves over deep brown.
    WavesCopper,

    /// Thin forest crosshatch grid — bright lime on deep forest.
    GridGreen,
    /// Large checker in two forest greens.
    CheckerForest,
    /// Bright leaf-green dots stippled on a near-black pine ground.
    DotsPine,
    /// Sinusoidal moss bands — yellow-green over deep olive.
    WavesMoss,
    /// Diagonal hash in saturated jungle green over deep shadow.
    HashJungle,
    /// Sparse leaf-green specks on near-black forest — same star-field
    /// pattern as `StarsNoir` but with a deep-green palette.
    StarsLeaf,

    // ---- Purple / neon ----
    /// Magenta dots on deep purple.
    DotsNeon,
    /// Purple sinusoidal waves.
    WavesPurple,
    /// Light-violet diagonal hash on deep purple.
    HashPurple,
    /// Bright violet stars on near-black violet — twilight sky.
    StarsViolet,
    /// Hot-pink grid lines on near-black — synthwave vibe.
    GridMagenta,

    // ---- Teal / cool ----
    /// Concentric teal rings.
    RadialTeal,
    /// Tiny 8-tile teal checker.
    MicroCheckerTeal,
    /// Diagonal hash in mid-teal on deep teal.
    HashTeal,
    /// Sinusoidal teal swells.
    WavesTeal,
    /// Pale-teal dots stippled on near-black teal — bioluminescent.
    DotsTeal,

    // ---- Crimson / blood ----
    /// Bright red diagonal hash on deep blood.
    HashCrimson,
    /// Pink dots scattered on near-black red — wound spatter.
    DotsCrimson,

    // ---- Gold / brass ----
    /// Bright gold diagonal hash on deep brown-gold.
    HashGold,

    // ---- Monochrome / noir ----
    /// Sparse white stars on near-black.
    StarsNoir,
    /// Deterministic monochrome noise.
    NoiseGrey,
}

impl Default for BackgroundKind {
    fn default() -> Self { Self::HashDiagonal }
}

impl BackgroundKind {
    /// Walk through related variants together: all themed first, then the
    /// sand group, forest, purple, teal, monochrome — so a player tapping
    /// the cycle button sees related colour schemes adjacent to each other.
    pub fn cycle(self) -> Self {
        match self {
            // Themed
            Self::HashDiagonal        => Self::HashDiagonalReverse,
            Self::HashDiagonalReverse => Self::HashFine,
            Self::HashFine            => Self::VerticalStripes,
            Self::VerticalStripes     => Self::HorizontalStripes,
            Self::HorizontalStripes   => Self::Checker,
            Self::Checker             => Self::LargeChecker,
            Self::LargeChecker        => Self::GridDefault,
            Self::GridDefault         => Self::Plain,
            // Sand — extended with amber + copper variants.
            Self::Plain               => Self::DotsWarm,
            Self::DotsWarm            => Self::WavesSand,
            Self::WavesSand           => Self::BricksRed,
            Self::BricksRed           => Self::HashSunset,
            Self::HashSunset          => Self::HashAmber,
            Self::HashAmber           => Self::StarsAmber,
            Self::StarsAmber          => Self::WavesCopper,
            // Forest — six variants spanning lime / pine / moss /
            // jungle / starfield palettes.
            Self::WavesCopper         => Self::GridGreen,
            Self::GridGreen           => Self::CheckerForest,
            Self::CheckerForest       => Self::DotsPine,
            Self::DotsPine            => Self::WavesMoss,
            Self::WavesMoss           => Self::HashJungle,
            Self::HashJungle          => Self::StarsLeaf,
            // Purple — five variants: dots → waves → hash → stars → grid.
            Self::StarsLeaf           => Self::DotsNeon,
            Self::DotsNeon            => Self::WavesPurple,
            Self::WavesPurple         => Self::HashPurple,
            Self::HashPurple          => Self::StarsViolet,
            Self::StarsViolet         => Self::GridMagenta,
            // Teal — five variants: rings → micro-checker → hash → waves → dots.
            Self::GridMagenta         => Self::RadialTeal,
            Self::RadialTeal          => Self::MicroCheckerTeal,
            Self::MicroCheckerTeal    => Self::HashTeal,
            Self::HashTeal            => Self::WavesTeal,
            Self::WavesTeal           => Self::DotsTeal,
            // Crimson / blood
            Self::DotsTeal            => Self::HashCrimson,
            Self::HashCrimson         => Self::DotsCrimson,
            // Gold / brass
            Self::DotsCrimson         => Self::HashGold,
            // Monochrome
            Self::HashGold            => Self::StarsNoir,
            Self::StarsNoir           => Self::NoiseGrey,
            Self::NoiseGrey           => Self::HashDiagonal,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::HashDiagonal        => "HASH",
            Self::HashDiagonalReverse => "HASH-R",
            Self::HashFine            => "HASH-F",
            Self::VerticalStripes     => "VERT",
            Self::HorizontalStripes   => "HORZ",
            Self::Checker             => "CHECKER",
            Self::LargeChecker        => "BIGCHECK",
            Self::GridDefault         => "GRID",
            Self::Plain               => "PLAIN",
            Self::DotsWarm            => "DOTSWARM",
            Self::WavesSand           => "WAVESAND",
            Self::BricksRed           => "BRICKS",
            Self::HashSunset          => "SUNSET",
            Self::HashAmber           => "AMBER",
            Self::StarsAmber          => "EMBERS",
            Self::WavesCopper         => "COPPER",
            Self::GridGreen           => "GRIDGRN",
            Self::CheckerForest       => "FOREST",
            Self::DotsPine            => "PINE",
            Self::WavesMoss           => "MOSS",
            Self::HashJungle          => "JUNGLE",
            Self::StarsLeaf           => "LEAVES",
            Self::DotsNeon            => "NEON",
            Self::WavesPurple         => "WAVEPRP",
            Self::HashPurple          => "VIOLET",
            Self::StarsViolet         => "TWILIGHT",
            Self::GridMagenta         => "MAGENTA",
            Self::RadialTeal          => "RINGS",
            Self::MicroCheckerTeal    => "MICROTL",
            Self::HashTeal            => "TEAL",
            Self::WavesTeal           => "WAVETL",
            Self::DotsTeal            => "BIOLUME",
            Self::HashCrimson         => "CRIMSON",
            Self::DotsCrimson         => "SPATTER",
            Self::HashGold            => "GOLD",
            Self::StarsNoir           => "STARS",
            Self::NoiseGrey           => "NOISE",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "hash"      | "hash_diagonal"         => Some(Self::HashDiagonal),
            "hash_rev"  | "hash_diagonal_reverse" => Some(Self::HashDiagonalReverse),
            "hash_fine" | "hash_f"                => Some(Self::HashFine),
            "vertical"  | "vert"                  => Some(Self::VerticalStripes),
            "horizontal"| "horz"                  => Some(Self::HorizontalStripes),
            "checker"                             => Some(Self::Checker),
            "large_checker"  | "bigcheck"         => Some(Self::LargeChecker),
            "grid"      | "grid_default"          => Some(Self::GridDefault),
            "plain"                               => Some(Self::Plain),
            "dots_warm"      | "dotswarm"         => Some(Self::DotsWarm),
            "waves_sand"     | "wavesand"         => Some(Self::WavesSand),
            "bricks"    | "bricks_red"            => Some(Self::BricksRed),
            "sunset"    | "hash_sunset"           => Some(Self::HashSunset),
            "hash_amber"     | "amber"            => Some(Self::HashAmber),
            "stars_amber"    | "embers"           => Some(Self::StarsAmber),
            "waves_copper"   | "copper"           => Some(Self::WavesCopper),
            "grid_green"     | "gridgrn"          => Some(Self::GridGreen),
            "checker_forest" | "forest"           => Some(Self::CheckerForest),
            "dots_pine"      | "pine"             => Some(Self::DotsPine),
            "waves_moss"     | "moss"             => Some(Self::WavesMoss),
            "hash_jungle"    | "jungle"           => Some(Self::HashJungle),
            "stars_leaf"     | "leaves"           => Some(Self::StarsLeaf),
            "dots_neon"      | "neon"             => Some(Self::DotsNeon),
            "waves_purple"   | "waveprp"          => Some(Self::WavesPurple),
            "hash_purple"    | "violet"           => Some(Self::HashPurple),
            "stars_violet"   | "twilight"         => Some(Self::StarsViolet),
            "grid_magenta"   | "magenta"          => Some(Self::GridMagenta),
            "radial_teal"    | "rings"            => Some(Self::RadialTeal),
            "micro_checker_teal" | "microtl"      => Some(Self::MicroCheckerTeal),
            "hash_teal"      | "teal"             => Some(Self::HashTeal),
            "waves_teal"     | "wavetl"           => Some(Self::WavesTeal),
            "dots_teal"      | "biolume"          => Some(Self::DotsTeal),
            "hash_crimson"   | "crimson"          => Some(Self::HashCrimson),
            "dots_crimson"   | "spatter"          => Some(Self::DotsCrimson),
            "hash_gold"      | "gold"             => Some(Self::HashGold),
            "stars_noir"     | "stars"            => Some(Self::StarsNoir),
            "noise_grey"     | "noise"            => Some(Self::NoiseGrey),
            _ => None,
        }
    }
    pub fn serialize(self) -> &'static str {
        match self {
            Self::HashDiagonal        => "hash",
            Self::HashDiagonalReverse => "hash_rev",
            Self::HashFine            => "hash_fine",
            Self::VerticalStripes     => "vertical",
            Self::HorizontalStripes   => "horizontal",
            Self::Checker             => "checker",
            Self::LargeChecker        => "large_checker",
            Self::GridDefault         => "grid",
            Self::Plain               => "plain",
            Self::DotsWarm            => "dots_warm",
            Self::WavesSand           => "waves_sand",
            Self::BricksRed           => "bricks",
            Self::HashSunset          => "sunset",
            Self::HashAmber           => "hash_amber",
            Self::StarsAmber          => "stars_amber",
            Self::WavesCopper         => "waves_copper",
            Self::GridGreen           => "grid_green",
            Self::CheckerForest       => "checker_forest",
            Self::DotsPine            => "dots_pine",
            Self::WavesMoss           => "waves_moss",
            Self::HashJungle          => "hash_jungle",
            Self::StarsLeaf           => "stars_leaf",
            Self::DotsNeon            => "dots_neon",
            Self::WavesPurple         => "waves_purple",
            Self::HashPurple          => "hash_purple",
            Self::StarsViolet         => "stars_violet",
            Self::GridMagenta         => "grid_magenta",
            Self::RadialTeal          => "radial_teal",
            Self::MicroCheckerTeal    => "micro_checker_teal",
            Self::HashTeal            => "hash_teal",
            Self::WavesTeal           => "waves_teal",
            Self::DotsTeal            => "dots_teal",
            Self::HashCrimson         => "hash_crimson",
            Self::DotsCrimson         => "dots_crimson",
            Self::HashGold            => "hash_gold",
            Self::StarsNoir           => "stars_noir",
            Self::NoiseGrey           => "noise_grey",
        }
    }
}

/// Resource holding the player's chosen background pattern. The apply
/// system in `rendering.rs` watches `kind` + the palette and rebuilds
/// the hash image whenever the chosen variant changes.
#[derive(Resource, Default, Clone, Copy, Debug)]
pub struct BackgroundSetting {
    pub kind: BackgroundKind,
    pub last_applied: Option<BackgroundKind>,
}

// ---------- Marker components ----------

/// CRT scanline overlay sprite — sized to match the play sprite, hidden
/// unless `CrtMode.active`. Spawned by the rendering setup.
#[derive(Component)]
pub struct ScanlineSprite;

// ---------- Layout helpers (used by mode + UI + ship systems) ----------

/// Vertical breathing room reserved above + below the play area as a
/// fraction of the window's logical height. A small inset keeps the
/// play square / map from hitting the top and bottom edges of the
/// window, which felt cramped on the design layout. Tuned to leave
/// ~6% margin per side at any window size; play area scales to fit
/// the remaining height. Tweak this constant if the breathing room
/// feels off.
const PLAY_VERTICAL_PAD_RATIO: f32 = 0.06;

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
    // Reserve vertical breathing room so the play area doesn't touch
    // the top / bottom of the window. Horizontal padding falls out
    // naturally because the scale is fit-on-min: capping height
    // shrinks the square so the sides grow too.
    let avail_h = (logical_h * (1.0 - 2.0 * PLAY_VERTICAL_PAD_RATIO)).max(0.0);
    let scale_x = logical_w / PLAY_INTERNAL_W as f32;
    let scale_y = avail_h    / PLAY_INTERNAL_H as f32;
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
    // LocalPlayer (not Friendly) so MP doesn't bail — host has two
    // Friendlies, single() would fall back to Vec2::ZERO. Camera
    // should track the local player's ship, never the remote's.
    friendly: Query<
        &Transform,
        (
            With<crate::components::LocalPlayer>,
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
            Without<crate::components::LocalPlayer>,
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

// ---------- Camera zoom punch ----------

/// One-shot camera-zoom punch (level-up, boss kill, etc.). Lerps
/// the play camera's projection scale toward `1.0 - peak_zoom_in`
/// at the midpoint of `duration`, easing back to 1.0 at the end.
/// Sine curve so the punch starts soft, peaks hard, and recovers
/// without a visible snap.
#[derive(Resource, Default)]
pub struct CameraPunch {
    pub remaining: f32,
    pub duration: f32,
    /// Fraction to subtract from baseline scale at peak. 0.08 =
    /// zoom in 8%. Larger reads as a bigger event.
    pub peak_zoom_in: f32,
}

impl CameraPunch {
    /// Start a punch. Later pushes during an active punch take the
    /// stronger of (current, new) so a small kick can't shrink a
    /// big one already in flight. Currently no callers — kept
    /// because the infra (resource, apply system, registration)
    /// is wired in and future juice triggers can call this without
    /// re-plumbing.
    #[allow(dead_code)]
    pub fn punch(&mut self, duration: f32, peak_zoom_in: f32) {
        if duration > self.remaining {
            self.remaining = duration;
            self.duration = duration;
        }
        if peak_zoom_in > self.peak_zoom_in {
            self.peak_zoom_in = peak_zoom_in;
        }
    }
}

/// Apply [`CameraPunch`] to PlayCamera + HudCamera each frame. Decays
/// in real time so the punch resolves even when virtual time is
/// frozen by [`crate::hitstop::HitStopController`].
pub fn apply_camera_punch(
    real: Res<bevy::time::Time<bevy::time::Real>>,
    mut punch: ResMut<CameraPunch>,
    mut cams: Query<
        &mut Projection,
        Or<(
            With<crate::palette::PlayCamera>,
            With<crate::palette::HudCamera>,
        )>,
    >,
) {
    if punch.remaining > 0.0 {
        punch.remaining = (punch.remaining - real.delta_secs()).max(0.0);
    }
    let t = if punch.duration > 0.0 && punch.remaining > 0.0 {
        1.0 - (punch.remaining / punch.duration).clamp(0.0, 1.0)
    } else {
        0.0
    };
    // Sine curve: 0 at t=0, 1 at t=0.5, 0 at t=1.
    let curve = (t * std::f32::consts::PI).sin();
    let scale = (1.0 - punch.peak_zoom_in * curve).max(0.1);
    for mut p in &mut cams {
        if let Projection::Orthographic(o) = p.as_mut() {
            o.scale = scale;
        }
    }
    if punch.remaining <= 0.0 {
        punch.peak_zoom_in = 0.0;
    }
}
