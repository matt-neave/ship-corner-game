//! All gameplay tunables in one place. Adjust here to balance the game
//! without touching system code.
//!
//! Per-weapon and per-enemy-variant stats live on their respective enums
//! (`weapon::WeaponType::defaults` / `enemy::EnemyVariant::*`) — those are
//! still data tables, just colocated with the type.

use std::f32::consts::{FRAC_PI_2, FRAC_PI_3, FRAC_PI_4, PI};

// ---------- Window / layout ----------
pub const WINDOW_W: f32 = 1280.0;
pub const WINDOW_H: f32 = 800.0;
pub const UI_WIDTH: f32 = 280.0;

// ---------- Play area dimensions ----------
//
// The play arena is a centred axis-aligned rectangle. The default
// build keeps it square at 200×200; building with the `wide_play`
// Cargo feature swaps to 360×200 for AB-testing wider stages.
// `_W` / `_H` are authoritative; the unqualified `PLAY_INTERNAL` /
// `PLAY_WORLD` aliases point at the HEIGHT axis (kept for sites
// that genuinely don't care about which axis — e.g. HUD layout
// helpers that target the vertical extent).

#[cfg(feature = "wide_play")]
pub const PLAY_INTERNAL_W: u32 = 360;
#[cfg(not(feature = "wide_play"))]
pub const PLAY_INTERNAL_W: u32 = 200;
pub const PLAY_INTERNAL_H: u32 = 200;

#[cfg(feature = "wide_play")]
pub const PLAY_WORLD_W: f32 = 360.0;
#[cfg(not(feature = "wide_play"))]
pub const PLAY_WORLD_W: f32 = 200.0;
pub const PLAY_WORLD_H: f32 = 200.0;

/// Legacy aliases — kept so call sites that don't distinguish axes
/// still compile. New code should prefer the `_W` / `_H` pair.
pub const PLAY_INTERNAL: u32 = PLAY_INTERNAL_H;
pub const PLAY_WORLD: f32 = PLAY_WORLD_H;

/// Is this world-space position inside the play arena (a centred
/// `PLAY_WORLD_W × PLAY_WORLD_H` rectangle)? Used by enemy firing
/// systems to avoid taking pot-shots from outside the visible play
/// field — off-screen enemies should drift back IN before they shoot.
#[inline]
pub fn in_play_area(p: bevy::math::Vec2) -> bool {
    let hw = PLAY_WORLD_W * 0.5;
    let hh = PLAY_WORLD_H * 0.5;
    p.x.abs() < hw && p.y.abs() < hh
}

pub const PLAY_LAYER:    usize = 1;
pub const UPSCALE_LAYER: usize = 2;
/// Layer for native-resolution HUD overlays drawn on top of the
/// upscaled play sprite (e.g. floating enemy HP bars). Rendered by
/// `HudCamera` directly to the screen with the viewport clipped to
/// the play-area screen rect, so the chunky-pixel filter does *not*
/// apply to anything on this layer.
pub const HUD_LAYER:     usize = 4;

/// Layer for the customize-overlay primitive entities (sprite hull,
/// turret tiles, rune sockets, tooltips). A dedicated `CustomizeCamera`
/// (in `customize::render`) renders only this layer to a low-res image,
/// which is then upscaled nearest-neighbor onto a fullscreen sprite on
/// `UPSCALE_LAYER`. That's where the chunky-pixel look comes from —
/// every primitive is rasterized at the internal resolution before being
/// scaled up.
pub const CUSTOMIZE_LAYER: usize = 5;

/// Internal resolution of the customize render target. Picked so 4×
/// upscale equals the default 1280×800 window — keeps things pixel-
/// perfect on the default size, and any other window size still gets an
/// integer-multiple upscale (centred + letterboxed).
pub const CUSTOMIZE_INTERNAL_W: u32 = 320;
pub const CUSTOMIZE_INTERNAL_H: u32 = 200;

// ---------- Friendly ship ----------
pub const FRIENDLY_TURN_RATE: f32 = 3.6; // rad/s
pub const HULL_LEN:           f32 = 22.0;
pub const HULL_WIDTH:         f32 = 8.0;
pub const HULL_HALF_LEN:      f32 = HULL_LEN / 2.0;

// ---------- Turret geometry ----------
pub const TURRET_RANGE: f32 = 60.0;
pub const TURRET_PIVOT: f32 = 145.0 * PI / 180.0; // 145°/s

pub const PI_2: f32 = FRAC_PI_2;
pub const PI_3: f32 = FRAC_PI_3;
pub const PI_4: f32 = FRAC_PI_4;
pub const PI_F: f32 = PI;

/// Turret mount positions in hull-local coords (ship faces +Y).
/// 0=bow, 7=stern, 1-6 wing pairs (port/starboard); mid pair widest beam.
pub const TURRET_POSITIONS: [(f32, f32); 8] = [
    ( 0.0,  9.0),  // bow centerline
    (-2.0,  5.0),  // fore wing pair (port)
    ( 2.0,  5.0),  //                  (stbd)
    (-3.0,  0.0),  // mid wing pair  (port)
    ( 3.0,  0.0),  //                  (stbd)
    (-2.0, -5.0),  // aft wing pair  (port)
    ( 2.0, -5.0),  //                  (stbd)
    ( 0.0, -9.0),  // stern centerline
];

/// Mount centerline angle per turret in hull frame, 0 = +Y forward.
pub const TURRET_MOUNTS: [f32; 8] = [
     0.0,         // bow centerline → forward
     PI_4,        // fore port wing → NW diagonal
    -PI_4,        // fore stbd wing → NE diagonal
     PI_2,        // mid port → port
    -PI_2,        // mid stbd → starboard
     3.0 * PI_4,  // aft port wing → SW diagonal
    -3.0 * PI_4,  // aft stbd wing → SE diagonal
     PI_F,        // stern centerline → backward
];

/// Half-arc per turret. Default is the full 360° cone (π half-arc)
/// for every slot — turrets can fire in any direction at base. The
/// `T.ARC` stat is headroom on top, capped at π by
/// `effective_turret_half_arc`.
pub const TURRET_ARC_HALVES: [f32; 8] = [
    PI_F, PI_F, PI_F, PI_F, PI_F, PI_F, PI_F, PI_F,
];

/// Adjacency graph between turret slots — each slot lists the indices
/// of its physically-neighbouring slots on the ship deck. Drives
/// turret-to-turret synergies (e.g. the `Booster` support turret
/// multiplies adjacent turrets' fire rate).
///
/// Layout, from `TURRET_POSITIONS`:
///
/// ```text
///         0 (bow)
///        /  \
///       1 ── 2
///       │    │
///       3 ── 4
///       │    │
///       5 ── 6
///        \  /
///         7 (stern)
/// ```
///
/// Bow + stern connect to both wings on their end. Wings connect
/// fore-aft along their own side AND port↔starboard at the same
/// section, so a centreline-Booster reaches both sides at once.
pub const TURRET_ADJACENCY: [&[usize]; 8] = [
    &[1, 2],          // 0 bow → fore wings
    &[0, 2, 3],       // 1 fore port
    &[0, 1, 4],       // 2 fore stbd
    &[1, 4, 5],       // 3 mid port
    &[2, 3, 6],       // 4 mid stbd
    &[3, 6, 7],       // 5 aft port
    &[4, 5, 7],       // 6 aft stbd
    &[5, 6],          // 7 stern → aft wings
];

/// i18n keys for each turret name (cell index 0..7). Display strings live in
/// `data/translations.csv`; this array maps slot index → CSV key.
pub const TURRET_NAME_KEYS: [&str; 8] = [
    "turret_bow",
    "turret_fore_port",
    "turret_fore_stbd",
    "turret_mid_port",
    "turret_mid_stbd",
    "turret_aft_port",
    "turret_aft_stbd",
    "turret_stern",
];

// ---------- Barrels & bullets ----------
pub const BARREL_LATERAL:           f32 = 1.15;
pub const FRIENDLY_BARREL_TIP:      f32 = 5.0;
/// Triple-barrel upgrade: the middle barrel sits this many world units
/// (= internal pixels) longer than the port + starboard pair, giving a visual
/// marker of the upgrade and pushing the middle muzzle flash + bullet spawn
/// forward.
pub const BARREL_MIDDLE_EXTEND:     f32 = 1.0;
pub const ENEMY_BARREL_TIP:         f32 = 3.55;
pub const FRIENDLY_BULLET_HALF_LEN: f32 = 2.75;
pub const ENEMY_BULLET_HALF_LEN:    f32 = 2.25;
pub const BULLET_SPEED:             f32 = 110.0;

// ---------- Enemy chassis ----------
pub const ENEMY_RANGE:           f32 = 45.0;
pub const ENEMY_LEN:             f32 = 10.0;
pub const ENEMY_WIDTH:           f32 = 5.0;
pub const BOMBER_DETONATE_DIST:  f32 = 8.0;

// ---------- Trails ----------
pub const ENEMY_TRAIL_SAMPLE_HZ:  f32   = 25.0;
pub const ENEMY_TRAIL_MAX_POINTS: usize = 18;
pub const ENEMY_TRAIL_HEAD_WIDTH: f32   = 4.0;
pub const TRAIL_SAMPLE_HZ:        f32   = 30.0;
pub const TRAIL_MAX_POINTS:       usize = 30;
pub const TRAIL_HEAD_WIDTH:       f32   = 6.0;

// ---------- Hit FX ----------
pub const HIT_PULSE:      f32 = 0.5;
pub const HIT_K:          f32 = 200.0;
pub const HIT_D:          f32 = 10.0;
pub const FLASH_DURATION: f32 = 0.12;

// ---------- Weapons ----------
pub const SHOTGUN_PELLETS:    u32 = 6;
pub const SHOTGUN_SPREAD:     f32 = 0.43; // ~±25°
pub const BEAM_LENGTH:        f32 = 100.0;
pub const BEAM_LIFETIME:      f32 = 0.22;
pub const BEAM_MAX_WIDTH:     f32 = 3.5;
pub const BEAM_HIT_RADIUS:    f32 = 3.0;
pub const BULLET_INNER_LIGHTEN: f32 = 0.55;

// ---------- Runes (status effects) ----------
pub const FIRE_DURATION:               f32 = 4.0;   // total burn time
pub const FIRE_DAMAGE_TICK_INTERVAL:   f32 = 0.5;   // damage applied every tick
pub const FIRE_DAMAGE_PER_TICK:        i32 = 1;     // 1 dmg every 0.5 s → 8 dmg total
pub const FIRE_PARTICLE_TICK_INTERVAL: f32 = 0.15;  // visual particle spawn rate
pub const FIRE_PARTICLES_PER_TICK:     u32 = 3;     // particles per visual tick

pub const FROST_DURATION:               f32 = 3.0;   // total slow duration
pub const FROST_SPEED_MULT:             f32 = 0.4;   // 60% slow
pub const FROST_PARTICLE_TICK_INTERVAL: f32 = 0.20;  // slower than fire — frost is calm
pub const FROST_PARTICLES_PER_TICK:     u32 = 2;     // sparser than fire

/// Max distance a shock arc can reach for its chain target. World units.
pub const SHOCK_CHAIN_RANGE:  f32 = 32.0;
/// How long the lightning bolt visual lingers on screen.
pub const SHOCK_VISUAL_LIFE:  f32 = 0.18;

/// Max distance a Cascade chain can reach for its on-kill hop. Wider
/// than a Shock chain because Cascade only fires on lethal hits, so
/// we want it to sometimes find a target across a small gap.
pub const CASCADE_RANGE:        f32 = 40.0;
/// How long a `OnConduit` status persists on a target after a Conduit
/// proc — long enough that a follow-up shot from another slot can
/// benefit, short enough that it doesn't linger forever.
pub const CONDUIT_DURATION:     f32 = 3.0;
/// Multiplier applied to incoming proc rolls' strength while a target
/// is conducted. Caps at 1.0 in the rolled comparison, so the visible
/// effect is "chain hops at full strength" rather than "initial hits
/// have super-procs".
pub const CONDUIT_PROC_MULT:    f32 = 1.5;
/// How long `OnResonate` stacks linger after the most recent hit.
/// Short enough that you have to keep hitting the same target to
/// stack up; long enough that bursty weapons (Shotgun) still benefit.
pub const RESONATE_DECAY:           f32 = 2.0;
/// Damage bonus per Resonate stack, multiplicative.
pub const RESONATE_DAMAGE_PER_STACK: f32 = 0.20;
pub const RESONATE_MAX_STACKS:      u8  = 5;

// (Old Wave-mode constants — FRIENDLY_HP_WAVE, WAVE_*_DELAY,
// FRIENDLY_DOCK_*, ENEMY_WAVE_X, PIER_* — were removed with the
// retired Wave game mode + pier drafting system.)

// ---------- Wave structure ----------
//
// A combat encounter is split into discrete waves. Star tier picks the
// wave count (more stars = longer encounter); wave index inside the
// stage picks the size (later waves are bigger). Boss waves swap the
// variant distribution but reuse the size formula for now — the
// designer-facing trigger lives in `is_boss_wave` and currently
// returns `false` so the path is wired but inert.

/// Total waves in a stage. Each star tier adds two waves on top of
/// the 1★ baseline.
pub fn waves_for_stars(stars: u8) -> u8 {
    match stars {
        1 => 4,
        2 => 6,
        3 => 8,
        4 => 10,
        _ => 12,
    }
}

/// Enemy count for a single wave. Composed from three terms:
///
/// - **Base wave ramp**: `4 + wave_idx / 2` — within a stage, later
///   waves feel weightier than the opener.
/// - **Stage progression**: `× (1 + 0.25 × min(battles_cleared, 6))` —
///   gentler ramp than before AND capped, so a long campaign can't
///   compound the multiplier into chaos. Stage 1 = ×1.0, Stage 2 =
///   ×1.25, …, Stage 7+ caps at ×2.5.
/// - **Star density**: `× (1 + 0.2 × (stars - 1))` — higher-star
///   sections are denser per wave on top of having more waves total
///   (`waves_for_stars`). 5★ ≈ ×1.8 vs 1★ baseline.
///
/// Maximum theoretical wave size with everything maxed: last wave of
/// a 5★ stage 7+ ≈ `(4+5) × 2.5 × 1.8 ≈ 40`. Concurrent on-screen cap
/// (`CombatContext::enemy_cap`) drips overflow in as existing enemies
/// die so the arena stays legible.
pub fn wave_size(wave_idx: u8, stars: u8, battles_cleared: u32) -> u32 {
    let base = 4.0 + (wave_idx as u32 / 2) as f32;
    let capped_cleared = battles_cleared.min(6) as f32;
    let progress_mult = 1.0 + 0.25 * capped_cleared;
    let star_mult = 1.0 + 0.2 * (stars.saturating_sub(1) as f32);
    (base * progress_mult * star_mult).round().max(1.0) as u32
}

/// True when this wave should swap to the boss-style variant mix.
/// Currently inert — placeholder for the designer-facing trigger
/// (likely "last wave of a 3★+ stage" or a campaign-progress gate).
pub fn is_boss_wave(_wave_idx: u8, _wave_count: u8) -> bool {
    false
}

// ---------- Map-view economy ----------
//
// Production-tick intervals + boost factor for the Foundry / Crane
// economy. These are wall-clock seconds, ticked by `tick_buildings`
// in `map.rs`; an active adjacent Crane shrinks a Foundry's effective
// interval by `CRANE_SPEED_MULT` (1.30 → ~46 s/cycle instead of 60).

/// Foundry: every cycle, consumes 1 scrap and produces 1 steel.
pub const FOUNDRY_INTERVAL:   f32 = 60.0;
/// Crane: every cycle, consumes 1 steel; while fueled it boosts each
/// adjacent production building's speed by `CRANE_SPEED_MULT`.
pub const CRANE_INTERVAL:     f32 = 120.0;
pub const CRANE_SPEED_MULT:   f32 = 1.30;
/// Refinery (tier 3): every cycle, consumes 10 steel and produces 1
/// refined steel.
pub const REFINERY_INTERVAL:  f32 = 300.0;
pub const REFINERY_INPUT:     u32 = 10;
