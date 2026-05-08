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

/// Internal render resolution of the play area. Upscaled to the on-screen
/// rect with nearest-neighbor sampling — the chunky-pixel look depends on it.
pub const PLAY_INTERNAL: u32 = 200;
pub const PLAY_WORLD:    f32 = 200.0;

pub const PLAY_LAYER:    usize = 1;
pub const UPSCALE_LAYER: usize = 2;

// ---------- Friendly ship ----------
pub const FRIENDLY_SPEED:     f32 = 28.0;
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

/// Half-arc per turret. Wing turrets get 120° (±60°) so they sweep from
/// fully-forward through diagonal to fully-sideways; axials get 90° (±45°).
pub const TURRET_ARC_HALVES: [f32; 8] = [
    PI_4, PI_3, PI_3, PI_4, PI_4, PI_3, PI_3, PI_4,
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

// ---------- Wave mode ----------
pub const FRIENDLY_HP_WAVE:      i32 = 50;
pub const WAVE_TRANSITION_DELAY: f32 = 1.4;
pub const WAVE_FAIL_DELAY:       f32 = 2.5;
pub const WAVE_INTRO_DELAY:      f32 = 0.6;
pub const FRIENDLY_DOCK_X:       f32 = -PLAY_WORLD / 2.0 + 24.0;
pub const ENEMY_WAVE_X:          f32 =  PLAY_WORLD / 2.0 - 22.0;
pub const FRIENDLY_DOCK_HEADING: f32 = -FRAC_PI_2; // facing +X (right)

// ---------- Pier ----------
pub const PIER_CELL_X:  f32 = -PLAY_WORLD / 2.0 + 6.0;
pub const PIER_CELL_W:  f32 = 9.0;
pub const PIER_CELL_H:  f32 = 21.0;
pub const PIER_Y_STEP:  f32 = 22.0;
pub const PIER_Y_START: f32 = -PIER_Y_STEP * 3.5;

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
