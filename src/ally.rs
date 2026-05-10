//! Allied units — autonomous friendly ships fighting alongside the player.
//!
//! Built to scale: adding a new ship class is a single-file change here,
//! mirroring how `enemy.rs` handles enemy variants.
//!
//! 1. Add a variant to `ShipClass`.
//! 2. Add rows in `hp`, `speed`, `turn_rate`, `hull_dims`, `turret_layout`,
//!    `fire_rate`, `fire_damage`, `turret_arc_half`, `hit_radius`,
//!    `is_submerged`.
//! 3. Add the body-color in `PaletteMaterials::hull_for_class` (and a hex
//!    in `palette.rs`).
//! 4. Trigger spawns from wherever (currently `setup_world` seeds the fleet).
//!
//! Allies share `FactionKind::Friendly` so their bullets damage enemies and
//! enemy bullets damage them. They have their own marker (`Ally`) and their
//! own AI / turret-aim systems so they don't collide with the player ship's
//! input-driven movement and configurable turret slots.
//!
//! ## Faction-agnostic ship classes (boss-enemy ground-work)
//!
//! `ShipClass` is intentionally faction-agnostic — the per-class data tables
//! (hp / speed / hull / turrets) describe a *type of ship*, not a side.
//! Today every spawn is friendly via `spawn_ally`, but the visual/stat
//! scaffolding is split out into `spawn_ship_chassis` so a future
//! `spawn_boss_ship` can build the same hull with `FactionKind::Enemy` and
//! a `BossEnemy` marker — same chassis, different AI / aiming target /
//! tint. Boss-specific behavior (target selection, turret aim) will need
//! its own systems; only the chassis builder is shared.

use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::render::render_asset::RenderAssetUsages;
use bevy::render::view::RenderLayers;
use rand::Rng;

use crate::balance::{
    FRIENDLY_HP_WAVE, PLAY_LAYER, PLAY_WORLD, TURRET_PIVOT, TURRET_RANGE,
};
use crate::bullet::Bullet;
use crate::components::{Faction, FactionKind, Friendly, Health, Heading, Velocity};
use crate::effects::{spawn_hit_particles, EffectMeshes, HitFx, HitParticle};
use crate::enemy::{Enemy, EnemyState, EnemyVariant};
use crate::modes::GameMode;
use crate::palette::PaletteMaterials;
use crate::rune::FireExtent;
use crate::ship::approach_angle;
use crate::turret::spawn_combat_bullet;
use crate::weapon::WeaponType;

// ---------- Components / classes ----------

#[derive(Component)]
pub struct Ally {
    pub class: ShipClass,
    /// Wander target used when no enemy is in range.
    pub waypoint: Vec2,
    /// Time until next wander re-plan.
    pub waypoint_timer: f32,
}

/// A type of ship — its hull, stats, and turret layout. Faction-agnostic;
/// today only friendly `Ally`s use it, future boss enemies will reuse the
/// same classes (see module docs).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShipClass {
    /// Small retro pirate ship — 4 broadside cannons (2 per side).
    PirateShip,
    /// Slow, large flat-top. No cannons of its own — fights through
    /// 2 patrolling planes that take off, strafe, and land.
    Carrier,
    /// Long, narrow submerged hull. No cannons; fires a homing missile
    /// from the bow every 5 seconds. Untouchable by normal enemy fire
    /// (`is_submerged()` gates the bullet/bomber collision paths) — the
    /// stealth angle is its identity.
    Submarine,
    /// Small, fragile utility boat. No direct weapons — drops timed
    /// proximity sea mines along its path. The mines persist after the
    /// minelayer moves on, creating an emergent area-denial pattern.
    Minelayer,
    /// Hospital tender. No weapons. Follows the player ship and emits
    /// a healing beam at it (or, if the player is full, the most-hurt
    /// ally). Pure support — its identity is "ally only" because the
    /// effect targets *other allies / the player*, not itself or
    /// anything player-mounted.
    Tender,
    /// Blackbeard's flagship. Matte-black pirate hull with no cannons
    /// at all — its only attack is *boarding*: closes to short range
    /// then launches a party of small boarder figures across to the
    /// enemy who tick damage for a few seconds before vanishing. The
    /// closer-than-usual orbit + ranged-attack-less profile carves out
    /// clear mechanical space vs. the regular PirateShip.
    Blackbeard,
    /// Chunky industrial oil tanker. No cannons of its own — its
    /// signature is a two-phase **spray → ignite** cycle:
    ///   1. Spray oil pools out the stern over `OIL_SPRAY_DURATION`
    ///      seconds (one drop every `OIL_DROP_INTERVAL`).
    ///   2. Set every freshly-laid pool on fire for
    ///      `OIL_BURN_DURATION` seconds, ticking AOE damage to the
    ///      opposite faction inside `OIL_BURN_RADIUS` of each pool.
    /// Faction-agnostic: an ally tanker burns enemies, a boss-side
    /// tanker burns the player + allies. Pools persist as
    /// free-standing world entities so they keep burning even if the
    /// tanker is sunk mid-cycle.
    OilTanker,
    /// Viking longship — ram-charge attacker. No turrets, no
    /// projectiles. Charges directly at the nearest opposite-faction
    /// unit at high speed and deals heavy contact damage. Tough hull
    /// so it survives the approach. Identity is "commits to the
    /// collision," distinct from every other class which fights at
    /// range or via deployables.
    Viking,
}

impl ShipClass {
    pub fn hp(self) -> i32 {
        match self {
            ShipClass::PirateShip => 40,
            ShipClass::Carrier    => 200,
            ShipClass::Submarine  => 20,
            ShipClass::Minelayer  => 25,
            ShipClass::Tender     => 35,
            // Blackbeard is the flagship — most HP of the surface
            // fleet so it can survive the close-range orbit boarding
            // requires.
            ShipClass::Blackbeard => 60,
            // Chunky industrial hull — sturdier than the Pirate but
            // not on Carrier scale. The oil-spray fantasy wants the
            // tanker to survive its full cycle under light fire so
            // the player sees the burn payoff.
            ShipClass::OilTanker  => 70,
            // Built for ramming — sturdy enough to survive the charge.
            ShipClass::Viking     => 75,
        }
    }
    pub fn speed(self) -> f32 {
        match self {
            ShipClass::PirateShip => 22.0,
            ShipClass::Carrier    => 12.0,
            ShipClass::Submarine  => 20.0,
            // Reasonably nippy — the minelayer has no direct guns, so it
            // needs to be able to peel off when chased rather than slug
            // it out.
            ShipClass::Minelayer  => 24.0,
            // Slightly faster than the player so the tender can keep up
            // when trailing behind.
            ShipClass::Tender     => 30.0,
            // Same speed as the regular Pirate — Blackbeard relies
            // on the long boarding range, not a chase, to stay
            // reliable.
            ShipClass::Blackbeard => 22.0,
            // Slow industrial — ~0.6× a PirateShip — so the oil
            // trail it lays reads as a deliberate ribbon, not a
            // dotted line of distant smudges.
            ShipClass::OilTanker  => 13.0,
            // Aggressive — fastest in the fleet. Charges have to feel
            // committed and unavoidable.
            ShipClass::Viking     => 28.0,
        }
    }
    pub fn turn_rate(self) -> f32 {
        match self {
            ShipClass::PirateShip => 1.4,
            ShipClass::Carrier    => 0.6,
            ShipClass::Submarine  => 1.0,
            ShipClass::Minelayer  => 1.6,
            ShipClass::Tender     => 2.0,
            ShipClass::Blackbeard => 1.0,
            // Sluggish — matches the slow speed and the chunky
            // silhouette. A nimble tanker would feel wrong.
            ShipClass::OilTanker  => 0.7,
            // Moderate — committed to a charge once it picks one, but
            // quick enough to re-aim between rams.
            ShipClass::Viking     => 1.6,
        }
    }
    /// Hull dimensions: `(width, length)`. Width drives the capsule radius;
    /// length is the long axis.
    pub fn hull_dims(self) -> (f32, f32) {
        match self {
            ShipClass::PirateShip => (5.0, 12.0),
            ShipClass::Carrier    => (7.0, 24.0),
            // Subs are intentionally narrow + long for instant silhouette
            // recognition vs the rounded surface ships.
            ShipClass::Submarine  => (3.5, 18.0),
            // Smallest hull in the fleet — sells the "fragile utility
            // boat" identity without crowding the silhouette gallery.
            ShipClass::Minelayer  => (3.5, 9.0),
            // RNLI-style lifeboat — small + stubby, the smallest
            // surface ally. Stays clearly distinct from every
            // combat hull at a glance.
            ShipClass::Tender     => (4.0, 8.0),
            // Bigger than PirateShip in both axes — it's the
            // flagship and the silhouette should imply that.
            ShipClass::Blackbeard => (6.0, 16.0),
            // Chunky industrial hull — wider than every combat ship
            // and longer than the Carrier-class so the silhouette
            // reads "tanker" instantly.
            ShipClass::OilTanker  => (5.0, 16.0),
            // Long narrow longship silhouette — distinctive prow visual
            // is added on top in the spawn block.
            ShipClass::Viking     => (3.5, 14.0),
        }
    }
    /// Per-turret `(local_x, local_y, mount_angle_radians)` in hull frame.
    /// Mount angle is 0 = +Y forward, ±π/2 = broadside.
    /// X sits just inside the hull half-width (2.5) so the turret
    /// centers anchor to the deck and only the outboard arc + barrel
    /// peek past the gunwale. Z-ordering in the spawn puts them
    /// behind the hull so the hull mesh occludes everything inboard.
    pub fn turret_layout(self) -> &'static [(f32, f32, f32)] {
        use std::f32::consts::FRAC_PI_2;
        match self {
            ShipClass::PirateShip => &[
                (-2.0,  3.0,  FRAC_PI_2), // port forward
                (-2.0, -3.0,  FRAC_PI_2), // port aft
                ( 2.0,  3.0, -FRAC_PI_2), // stbd forward
                ( 2.0, -3.0, -FRAC_PI_2), // stbd aft
            ],
            // Carrier — no turrets; planes do all the work.
            ShipClass::Carrier => &[],
            // Submarine — no cannons; missile launcher does the work.
            ShipClass::Submarine => &[],
            // Minelayer — no cannons; mines do the work.
            ShipClass::Minelayer => &[],
            // Tender — no cannons; the heal beam is its only output.
            ShipClass::Tender => &[],
            // Blackbeard — pure boarding ship, no cannons.
            ShipClass::Blackbeard => &[],
            // OilTanker — pure area-denial via oil spray + ignition.
            ShipClass::OilTanker => &[],
            // Viking — pure ram, no turrets.
            ShipClass::Viking => &[],
        }
    }
    pub fn fire_rate(self) -> f32 {
        match self {
            ShipClass::PirateShip => 2.0,
            ShipClass::Carrier    => 0.0,
            ShipClass::Submarine  => 0.0,
            ShipClass::Minelayer  => 0.0,
            ShipClass::Tender     => 0.0,
            ShipClass::Blackbeard => 0.0,
            ShipClass::OilTanker  => 0.0,
            ShipClass::Viking     => 0.0,
        }
    }
    pub fn fire_damage(self) -> i32 {
        match self {
            ShipClass::PirateShip => 1,
            ShipClass::Carrier    => 0,
            ShipClass::Submarine  => 0,
            ShipClass::Minelayer  => 0,
            ShipClass::Tender     => 0,
            ShipClass::Blackbeard => 0,
            ShipClass::OilTanker  => 0,
            ShipClass::Viking     => 0,
        }
    }
    /// Half-arc per turret (radians).
    pub fn turret_arc_half(self) -> f32 {
        match self {
            // ±60° — generous broadside arc that lets the forward + aft pair
            // share targets without rigidly committing to one quadrant.
            ShipClass::PirateShip => std::f32::consts::FRAC_PI_3,
            ShipClass::Carrier    => 0.0,
            ShipClass::Submarine  => 0.0,
            ShipClass::Minelayer  => 0.0,
            ShipClass::Tender     => 0.0,
            ShipClass::Blackbeard => 0.0,
            ShipClass::OilTanker  => 0.0,
            ShipClass::Viking     => 0.0,
        }
    }
    /// Diameter to use for the bullet/turret hit-radius approximation.
    pub fn hit_radius(self) -> f32 {
        match self {
            ShipClass::PirateShip => 3.0,
            ShipClass::Carrier    => 6.0,
            ShipClass::Submarine  => 2.5,
            ShipClass::Minelayer  => 2.5,
            ShipClass::Tender     => 2.5,
            ShipClass::Blackbeard => 4.0,
            // Chunkier hit radius matching the wider hull.
            ShipClass::OilTanker  => 4.0,
            // Long narrow longship — moderate hit radius.
            ShipClass::Viking     => 3.0,
        }
    }
    /// Desired engagement range for `ally_ai`'s combat orbit. Most
    /// classes orbit at ~70% of `TURRET_RANGE` so their cannons can
    /// bear; Blackbeard orbits much closer because its only attack
    /// (boarding) needs short range.
    pub fn orbit_range(self) -> f32 {
        match self {
            // Inside `BOARDING_RANGE` (25), so the ship sits in
            // boarding range mid-orbit and the launcher cooldown has
            // a steady chance to fire.
            ShipClass::Blackbeard => 18.0,
            // Viking has no projectile range — `ally_ai` special-cases
            // it to charge directly at the target rather than holding
            // an orbit. This value is unused but kept for symmetry.
            ShipClass::Viking     => 0.0,
            _                     => TURRET_RANGE * 0.7,
        }
    }
    /// Whether this class is treated as underwater. Submerged ships are
    /// invisible to normal enemies — bullets, bombers, and target-selection
    /// all skip them. Boss enemies (future) may opt to ignore this gate.
    pub fn is_submerged(self) -> bool {
        matches!(self, ShipClass::Submarine)
    }

    /// HP a boss-side spawn of this class gets, hand-tuned per class
    /// so each plays as a tough but killable mini-boss against a
    /// default-loadout player. Started at 10× the ally HP per the
    /// initial spec, lowered after testing — the carrier's 2000 HP
    /// version felt invincible to a single-cannon ship.
    pub fn boss_hp(self) -> i32 {
        match self {
            ShipClass::PirateShip => 100,
            ShipClass::Carrier    => 400,
            ShipClass::Submarine  => 60,
            ShipClass::Minelayer  => 80,
            ShipClass::Tender     => 100,
            ShipClass::Blackbeard => 200,
            // Tankers in boss form are slow, valuable targets — high
            // HP so the player has to commit to interrupting the
            // burn loop rather than instagibbing it.
            ShipClass::OilTanker  => 250,
            // Viking boss is a ramming juggernaut — high HP since
            // the player has to outmanoeuvre rather than out-DPS it.
            ShipClass::Viking     => 220,
        }
    }

    /// Full label for UI surfaces (damage panel, debug spawn buttons).
    /// No shorthand — keep names recognisable instead of trimming
    /// them for column width.
    pub fn label(self) -> &'static str {
        match self {
            ShipClass::PirateShip => "PIRATE SHIP",
            ShipClass::Carrier    => "CARRIER",
            ShipClass::Submarine  => "SUBMARINE",
            ShipClass::Minelayer  => "MINELAYER",
            ShipClass::Tender     => "TENDER",
            ShipClass::Blackbeard => "BLACKBEARD",
            ShipClass::OilTanker  => "OIL TANKER",
            ShipClass::Viking     => "VIKING",
        }
    }

    /// Convenience iterator over every class — handy for the debug
    /// "spawn one of each" UI so adding a class auto-shows up there.
    pub const ALL: &'static [ShipClass] = &[
        ShipClass::PirateShip,
        ShipClass::Carrier,
        ShipClass::Submarine,
        ShipClass::Minelayer,
        ShipClass::Tender,
        ShipClass::Blackbeard,
        ShipClass::OilTanker,
        ShipClass::Viking,
    ];
    pub const COUNT: usize = Self::ALL.len();

    /// Stable index for slotting per-class data into a fixed array
    /// (e.g. `DamageStats.per_ally`). Order mirrors `ALL`.
    pub fn to_index(self) -> usize {
        match self {
            ShipClass::PirateShip => 0,
            ShipClass::Carrier    => 1,
            ShipClass::Submarine  => 2,
            ShipClass::Minelayer  => 3,
            ShipClass::Tender     => 4,
            ShipClass::Blackbeard => 5,
            ShipClass::OilTanker  => 6,
            ShipClass::Viking     => 7,
        }
    }

}

#[derive(Component)]
pub struct AllyTurret {
    pub barrel_angle: f32,
    pub mount_angle: f32,
    pub fire_cd: f32,
    pub class: ShipClass,
    /// Which faction this turret aims at + damages. For friendly ally
    /// turrets this is `Enemy`; a boss-side spawn would set `Friendly`.
    pub target_faction: FactionKind,
}

/// A homing-missile launcher mounted on a ship. Fires forward from the
/// hull at `fire_rate` Hz when at least one valid target exists. Entity-
/// level — any ship of any faction can carry one.
#[derive(Component)]
pub struct MissileLauncher {
    /// Shots per second. 0.2 = one missile every 5 s.
    pub fire_rate: f32,
    pub damage: i32,
    /// Counts down to 0; a fire pulse resets it to `1.0 / fire_rate`.
    pub cd: f32,
    /// Hull-local +Y offset where the missile spawns (i.e. forward of the
    /// hull center along its facing direction).
    pub muzzle_offset: f32,
    /// Faction the launched missiles seek + damage.
    pub target_faction: FactionKind,
    /// Damage-credit source baked at spawn time so the
    /// `bullet_collisions` pipeline can attribute kills correctly.
    /// `None` for enemy launchers (no per-class tracking on that side).
    pub source: Option<crate::bullet::DamageSource>,
}

/// Tag on an in-flight homing missile. The missile is otherwise a regular
/// `Bullet` (faction = opposite of target) so it routes through
/// `bullet_collisions` for damage, despawn, and hit FX. This component
/// just adds re-targeting each frame.
#[derive(Component)]
pub struct HomingMissile {
    /// Cached target — refreshed if the entity dies or if no target was
    /// chosen at spawn (e.g. fired without a target in sight).
    pub target: Option<Entity>,
    /// Max angular adjustment per second (rad/s). Smaller = wider turning
    /// circle, more dodgeable.
    pub turn_rate: f32,
    /// Faction this missile seeks. Cached at spawn time because the
    /// missile out-lives its launcher — the launcher's faction can't be
    /// re-read from the carrier mid-flight.
    pub target_faction: FactionKind,
}

/// A timed proximity sea mine dropped by a `MineLayer`-equipped ship.
/// Lives as a free-standing entity in world space (not parented), so it
/// persists after the laying ship moves on.
#[derive(Component)]
pub struct Mine {
    pub damage: i32,
    pub blast_radius: f32,
    /// Counts down to 0; the mine is inert while > 0 so the laying ship
    /// can clear the drop point without blowing itself up.
    pub arm_timer: f32,
    /// Counts down to 0; on hitting 0, the mine quietly despawns. Stops
    /// the play area filling up with stale mines if the wave runs long.
    pub lifetime: f32,
    /// Faction the mine detonates against + damages. Cached at drop
    /// time because the mine outlives the laying ship.
    pub target_faction: FactionKind,
}

/// Marker on the red centre dot inside a mine. Drives `flash_mine_dots`
/// to pulse its scale so the dot reads as a blinking warning light.
#[derive(Component)]
pub struct MineDotFlash;

/// Phase of an OilTanker's spray → burn → cooldown loop. Stored on
/// `OilTankerCycle`; transitions are driven by `oil_tanker_cycle`.
///
/// `Spraying` lays new `OilSlick` entities behind the tanker on a
/// short interval. On expiry, every slick whose `owner_faction`
/// matches this tanker is tagged `OilOnFire` for `OIL_BURN_DURATION`
/// seconds — the ignition is implicit (the tanker doesn't track
/// individual slick entities). After the burn, a brief cooldown
/// breath, then back to `Spraying`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OilCyclePhase {
    Spraying,
    Burning,
    Cooldown,
}

/// State machine driving the OilTanker spray → ignite → burn → idle
/// loop. Lives on the tanker itself; despawning the tanker doesn't
/// extinguish already-laid slicks (they keep burning out their own
/// timers).
#[derive(Component)]
pub struct OilTankerCycle {
    pub phase: OilCyclePhase,
    /// Time remaining in the current phase.
    pub timer: f32,
    /// Drop-interval cooldown — only ticks while in `Spraying`.
    pub drop_cd: f32,
    /// Faction whose units take damage from this tanker's burning
    /// oil. Cached at spawn time so a sunk tanker's lingering slicks
    /// still know who to hurt.
    pub target_faction: FactionKind,
}

/// One free-standing oil pool laid by a tanker. Persists in world
/// space (not parented) — outlives the tanker if it sinks. Untagged
/// pools are visually-dark and harmless; ignition adds an
/// `OilOnFire` component that drives the AOE-burn ticks.
#[derive(Component)]
pub struct OilSlick {
    /// Faction *this slick targets when burning*. Equal to the
    /// laying tanker's `target_faction` — i.e. the side the tanker
    /// is fighting against.
    pub target_faction: FactionKind,
    /// Lifetime in seconds before silent despawn. Generous so the
    /// slick survives the spray phase + the full burn.
    pub lifetime: f32,
    /// Seconds since this slick spawned. Drives the spread-in
    /// animation in `oil_slick_grow_tick` — the visual + damage
    /// radius eases from `OIL_SPREAD_START_SCALE * target_radius`
    /// up to `target_radius` over `OIL_SPREAD_DURATION`.
    pub age: f32,
    /// Final settled radius for this slick (post-jitter). The
    /// transform's `scale` field stores `target_radius * factor`
    /// where `factor` ramps `OIL_SPREAD_START_SCALE → 1.0`.
    pub target_radius: f32,
}

/// Burning state on an `OilSlick`. Added when the laying tanker
/// transitions to `Burning`; ticks AOE damage on a fixed cadence to
/// every faction-mismatched unit inside `OIL_BURN_RADIUS`.
#[derive(Component)]
pub struct OilOnFire {
    pub remaining: f32,
    /// Counts down to 0 between damage ticks.
    pub tick_cd: f32,
}

/// A mine launcher mounted on a ship. Drops one mine every
/// `drop_interval` seconds at the ship's current position.
#[derive(Component)]
pub struct MineLayer {
    /// Seconds between drops.
    pub drop_interval: f32,
    /// Counts down to 0; resets on each drop.
    pub cd: f32,
    pub mine_damage: i32,
    pub mine_blast_radius: f32,
    /// Faction passed onto each dropped mine's `target_faction`.
    pub target_faction: FactionKind,
}

/// A continuous healing-beam emitter mounted on a ship. Each frame, the
/// system picks a hurt target of `heal_faction` inside `range`, applies
/// fractional HP regen, and spawns a brief beam visual. Friendly tenders
/// set `heal_faction = Friendly`; a boss-side tender would set `Enemy`.
#[derive(Component)]
pub struct HealBeamEmitter {
    pub range: f32,
    pub hp_per_sec: f32,
    /// Fractional HP carried between frames so a sub-1-HP-per-frame heal
    /// rate still ticks integers up at the right cadence.
    pub accumulator: f32,
    /// Side this emitter heals.
    pub heal_faction: FactionKind,
}

/// Boarding-party launcher mounted on a Blackbeard ship. Each cycle,
/// finds the closest target-faction enemy inside `range` and spawns
/// `party_size` boarder dots at the launcher's position, all bound
/// for that target.
///
/// State machine:
///   - `ready = true`  → waiting for a target. Cooldown is paused;
///                       the boarders are "loaded and ready to deploy"
///                       as soon as something walks into range.
///   - `ready = false` → cd ticks down each frame; when it hits 0 we
///                       flip back to `ready = true` and wait again.
/// Pausing the cooldown when no target exists is the key bit: a
/// Blackbeard with no enemy nearby doesn't waste reload progress, so
/// the very first enemy that appears triggers a launch instantly.
#[derive(Component)]
pub struct BoardingLauncher {
    /// Launches per second when actively engaging. 0.25 ≈ once every 4 s.
    pub fire_rate: f32,
    /// Time left on the current cooldown (only ticks while
    /// `ready == false`).
    pub cd: f32,
    /// `true` when the next launch is loaded; flipped to `false` on
    /// fire and back to `true` once `cd` runs out.
    pub ready: bool,
    /// Maximum distance to consider a target boardable.
    pub range: f32,
    pub party_size: u8,
    pub damage_per_tick: i32,
    pub tick_interval: f32,
    pub attach_duration: f32,
    pub target_faction: FactionKind,
}

/// State of a single boarder dot — first traveling from the launching
/// ship to the enemy, then attached and ticking damage.
#[derive(Clone, Copy)]
pub enum BoarderState {
    /// Lerping from `source`'s current position to `target`'s. `t` is
    /// 0..1 progress; on reaching 1 the boarder transitions to
    /// `Attached`.
    Traveling { t: f32 },
    /// Stuck to `target`, position = target's transform + a small
    /// random offset so multiple boarders don't all overlap. `remaining`
    /// counts down each frame; on hitting 0 the boarder despawns.
    Attached { remaining: f32 },
}

/// Visible rope strung between a Blackbeard and the enemy it's
/// boarding. Lives for the full launch cycle (travel + attach
/// duration) so the boarders read as crew traveling *along the
/// rope* rather than projectiles flying across.
#[derive(Component)]
pub struct BoardingRope {
    pub source: Entity,
    pub target: Entity,
    pub lifetime: f32,
}

/// One boarder dot. Marker entity that hops the gap between two ships
/// and drips damage onto the target. Despawns when its attach timer
/// runs out, or when the source / target despawns mid-flight.
#[derive(Component)]
pub struct Boarder {
    pub source: Entity,
    pub target: Entity,
    pub state: BoarderState,
    /// Random offset from target center used while attached, so
    /// multiple boarders cluster around the enemy instead of stacking.
    pub offset: Vec2,
    pub damage_per_tick: i32,
    pub tick_interval: f32,
    pub tick_cd: f32,
    /// Total time the boarder will stay attached once it lands —
    /// passed in from the launcher so the duration is tunable per
    /// ship without `boarder_tick` reaching back into the launcher.
    pub attach_duration: f32,
}

/// White signal flag drawn across the deck, parented to an ally ship.
/// Marker only — the flag's "wind-caught" look comes from a curved
/// mesh built once at spawn (`build_curved_flag_mesh`), not a
/// per-frame animation.
#[derive(Component)]
pub struct AllyFlag;

/// Per-class hull material lookup. Lives here (not in `palette.rs`) so
/// `ShipClass` stays the only source coupled to ship identities — adding
/// a new class means adding an arm here, not threading it through palette.
impl PaletteMaterials {
    pub fn hull_for_class(&self, class: ShipClass) -> &Handle<ColorMaterial> {
        match class {
            ShipClass::PirateShip => &self.pirate_hull,
            ShipClass::Carrier    => &self.carrier_hull,
            ShipClass::Submarine  => &self.submarine_hull,
            ShipClass::Minelayer  => &self.minelayer_hull,
            ShipClass::Tender     => &self.tender_hull,
            ShipClass::Blackbeard => &self.blackbeard_hull,
            ShipClass::OilTanker  => &self.oil_tanker_hull,
            ShipClass::Viking     => &self.viking_hull,
        }
    }
}

// ---------- Spawn helper ----------

/// Spawn one allied ship of `class` at `pos`. Thin wrapper around
/// `build_ship_for_faction` that also tags the result with the `Ally`
/// AI marker. Use `spawn_boss` for the enemy-side equivalent.
pub fn spawn_ally(
    commands: &mut Commands,
    pm: &PaletteMaterials,
    em: &EffectMeshes,
    meshes: &mut Assets<Mesh>,
    pos: Vec2,
    heading: f32,
    class: ShipClass,
) {
    let ship = build_ship_for_faction(
        commands, pm, em, meshes, pos, heading, class,
        FactionKind::Friendly,
    );
    commands.entity(ship).insert(Ally {
        class,
        waypoint: Vec2::ZERO,
        waypoint_timer: 0.0,
    });
}

/// Spawn one *boss* ship of `class` at `pos`. Same chassis + visual
/// decorations + class-aware launchers as an ally, but on the enemy
/// side: target factions flipped (boarding launchers, missile
/// launchers, ally turrets etc. all point at `Friendly`), HP bumped
/// to 10× the ally value, and tagged with the standard `Enemy` marker
/// so the existing enemy-side systems (bullet collisions, AI, fire,
/// death check) handle it without bespoke wiring.
pub fn spawn_boss(
    commands: &mut Commands,
    pm: &PaletteMaterials,
    em: &EffectMeshes,
    meshes: &mut Assets<Mesh>,
    pos: Vec2,
    heading: f32,
    class: ShipClass,
) {
    let ship = build_ship_for_faction(
        commands, pm, em, meshes, pos, heading, class,
        FactionKind::Enemy,
    );
    // Per-class boss HP from `ShipClass::boss_hp` overrides the
    // chassis default. `Enemy::Standard` is a placeholder variant —
    // the boss's identity is its `ShipClass`, but we need *some*
    // `EnemyVariant` so the existing enemy systems treat it
    // consistently (bullet collisions, AI, fire, death check).
    let boss_hp = class.boss_hp();
    commands.entity(ship).insert((
        crate::components::Health(boss_hp),
        crate::enemy::PreviousHp(boss_hp),
        Enemy {
            variant: EnemyVariant::Standard,
            state: EnemyState::Approach,
            state_timer: 1.0,
            waypoint: Vec2::ZERO,
            fire_cd: 0.5,
            max_hp: boss_hp,
        },
    ));
}

/// Build a ship chassis + class-specific decorations + class-specific
/// gameplay components for either faction. Returns the ship `Entity`
/// so the caller can layer side-specific markers (`Ally` /
/// `Enemy` / future `BossEnemy`) on top.
///
/// `own_faction` drives every "what side am I on" derived value:
///   - `target_faction` (= `own_faction.opposite()`) is what the
///     ship's launchers, turrets, mines, etc. *attack*.
///   - `heal_faction`   (= `own_faction`) is what its tender heals.
///
/// Friendly side gets `target = Enemy / heal = Friendly`; boss side
/// gets `target = Friendly / heal = Enemy`. Same chassis, mirrored
/// targeting — that's the whole point of carving the function out.
fn build_ship_for_faction(
    commands: &mut Commands,
    pm: &PaletteMaterials,
    em: &EffectMeshes,
    meshes: &mut Assets<Mesh>,
    pos: Vec2,
    heading: f32,
    class: ShipClass,
    own_faction: FactionKind,
) -> Entity {
    let target_faction = own_faction.opposite();
    let heal_faction   = own_faction;

    let ship = spawn_ship_chassis(commands, pm, meshes, pos, heading, class, own_faction);

    for &(lx, ly, mount) in class.turret_layout() {
        // Negative local z places the turret *behind* the hull
        // (parent is at z=1; child at z=-0.5 gives global z=0.5).
        // Combined with the wider x in `turret_layout`, the inboard
        // half of each turret is hidden under the deck and the
        // outboard half + barrel pokes out broadside.
        let turret = commands.spawn((
            Mesh2d(em.ally_turret_base.clone()),
            MeshMaterial2d(pm.turret.clone()),
            Transform::from_xyz(lx, ly, -0.5)
                .with_rotation(Quat::from_rotation_z(mount)),
            AllyTurret {
                barrel_angle: mount,
                mount_angle: mount,
                fire_cd: 0.0,
                class,
                target_faction,
            },
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(turret).insert(ChildOf(ship));

        // Single barrel child, pointing along turret +Y (mount-relative).
        // Slightly above turret's local plane but still under the hull.
        let barrel = commands.spawn((
            Mesh2d(em.ally_turret_barrel.clone()),
            MeshMaterial2d(pm.turret.clone()),
            Transform::from_xyz(0.0, 2.2, 0.05),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(barrel).insert(ChildOf(turret));
    }

    let (hull_w, hull_h) = class.hull_dims();

    // Blackbeard's flagship: skull-and-crossbones black flags, plus
    // the boarding launcher. No cannons — placed before the
    // PirateShip flag block so the early-return-style flag spawn
    // pattern stays clean.
    if class == ShipClass::Blackbeard {
        commands.entity(ship).insert(BoardingLauncher {
            // 0.25 ⇒ every 4 s. Tight enough that a missed boarding
            // (target died mid-flight, or wandered out of range)
            // doesn't leave the ship idle for ages.
            fire_rate: 0.25,
            cd: 0.0,
            // Start loaded — the first enemy that drifts inside
            // BOARDING_RANGE triggers an immediate launch.
            ready: true,
            range: BOARDING_RANGE,
            party_size: 5,
            damage_per_tick: 2,
            tick_interval: 0.4,
            attach_duration: 3.0,
            target_faction,
        });

        // Two grey sails — fore and aft of midship, slightly wider
        // than the hull so they overhang as a clean galleon
        // silhouette. Reuses `build_curved_flag_mesh` for the
        // wind-bowed shape so the sails read as taut canvas, not
        // flat panels.
        let sail_specs: [(f32, f32, f32, f32); 2] = [
            // (base_y, width, height, curve_amp)
            ( 3.0, hull_w + 1.5, 3.0, 0.6),
            (-3.0, hull_w + 1.5, 3.0, 0.6),
        ];
        for (base_y, sw, sh, curve) in sail_specs {
            let sail_mesh = meshes.add(build_curved_flag_mesh(sw, sh, curve));
            let sail = commands.spawn((
                Mesh2d(sail_mesh),
                MeshMaterial2d(pm.sail.clone()),
                Transform::from_xyz(0.0, base_y, 0.04),
                RenderLayers::layer(PLAY_LAYER),
            )).id();
            commands.entity(sail).insert(ChildOf(ship));
        }

        // Two black skull-and-crossbones pennants — same overhanging
        // layout as the regular pirate flags but bigger to match
        // the bigger hull. Skull head + crossed bones reuse white
        // `ally_flag` for the detail.
        let bone_mesh  = meshes.add(Rectangle::new(0.16, 1.05));
        let skull_mesh = meshes.add(Circle::new(0.38));
        let flag_specs: [(f32, f32, f32, f32); 2] = [
            // (base_y, width, height, curve_amp)
            (-2.0, hull_w + 5.0, 1.4, 0.5),
            ( 5.0,           5.0, 1.5, 0.3),
        ];
        for (base_y, fw, fh, curve) in flag_specs {
            let flag_mesh = meshes.add(build_curved_flag_mesh(fw, fh, curve));
            let flag = commands.spawn((
                Mesh2d(flag_mesh),
                MeshMaterial2d(pm.skull_flag.clone()),
                Transform::from_xyz(0.0, base_y, 0.3),
                AllyFlag,
                RenderLayers::layer(PLAY_LAYER),
            )).id();
            commands.entity(flag).insert(ChildOf(ship));

            // Crossed bones first (lower z); skull head on top.
            for sign in [1.0_f32, -1.0] {
                let bone = commands.spawn((
                    Mesh2d(bone_mesh.clone()),
                    MeshMaterial2d(pm.ally_flag.clone()),
                    Transform::from_xyz(0.0, 0.0, 0.04)
                        .with_rotation(Quat::from_rotation_z(
                            sign * std::f32::consts::FRAC_PI_4,
                        )),
                    RenderLayers::layer(PLAY_LAYER),
                )).id();
                commands.entity(bone).insert(ChildOf(flag));
            }
            let skull = commands.spawn((
                Mesh2d(skull_mesh.clone()),
                MeshMaterial2d(pm.ally_flag.clone()),
                Transform::from_xyz(0.0, 0.0, 0.05),
                RenderLayers::layer(PLAY_LAYER),
            )).id();
            commands.entity(skull).insert(ChildOf(flag));
        }
    }

    // Flags are part of the pirate-ship silhouette; the carrier
    // doesn't get them. Two flags across the deck, both overhanging
    // the gunwales: aft pennant 1 unit behind midship, smaller bow
    // jack at the front third. Mesh built once with a slight forward
    // bow so it reads wind-caught without per-frame animation.
    if class == ShipClass::PirateShip {
        let flag_specs: [(f32, f32, f32, f32); 2] = [
            // (base_y, width, height, curve_amp)
            (-1.0, hull_w + 4.0, 1.0, 0.5),
            ( 4.0,           4.0, 1.2, 0.3),
        ];
        for (base_y, fw, fh, curve) in flag_specs {
            let mesh = meshes.add(build_curved_flag_mesh(fw, fh, curve));
            let flag = commands.spawn((
                Mesh2d(mesh),
                MeshMaterial2d(pm.ally_flag.clone()),
                Transform::from_xyz(0.0, base_y, 0.3),
                AllyFlag,
                RenderLayers::layer(PLAY_LAYER),
            )).id();
            commands.entity(flag).insert(ChildOf(ship));
        }
    }

    // Carriers launch a small wing of planes — 6 parked across three
    // pairs. Planes start in `Idle` (parked on the deck), spaced by
    // `slot`. Each is its own top-level entity (not a child of the
    // carrier) so it can fly off on patrol freely; the `Plane.carrier`
    // field is the back-reference for tracking the home slot.
    if class == ShipClass::Carrier {
        for slot in 0..6u8 {
            spawn_plane(commands, pm, meshes, ship, slot, pos, heading, target_faction);
        }
    }

    // Submarine arms itself with a forward-firing homing-missile
    // launcher. Muzzle offset = half-hull-length so the missile
    // emerges from the bow tip. `cd` starts at the full interval so
    // the player can see the sub spawn and orient before the first
    // shot fires.
    if class == ShipClass::Submarine {
        let fire_rate = 0.2; // 1 missile per 5 s
        commands.entity(ship).insert(MissileLauncher {
            fire_rate,
            damage: 8,
            cd: 1.0 / fire_rate,
            muzzle_offset: hull_h * 0.5,
            target_faction,
            // Credit submarine missile kills to the SUB row in the
            // damage panel.
            source: Some(crate::bullet::DamageSource::Ally(ShipClass::Submarine)),
        });

        // Conning tower — small light-grey rectangle on top of the
        // hull, slightly aft of midship. Reinforces the submarine
        // silhouette so it's not mistaken for a generic stick.
        let tower_mesh = meshes.add(Rectangle::new(hull_w * 0.7, hull_h * 0.18));
        let tower = commands.spawn((
            Mesh2d(tower_mesh),
            MeshMaterial2d(pm.turret.clone()),
            Transform::from_xyz(0.0, -hull_h * 0.05, 0.05),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(tower).insert(ChildOf(ship));
    }

    // Tender — RNLI-style lifeboat silhouette: bright orange hull
    // (already set via `hull_for_class`) plus a small white
    // wheelhouse cabin on the deck. Carries the heal-beam emitter as
    // its only output.
    if class == ShipClass::Tender {
        commands.entity(ship).insert(HealBeamEmitter {
            range: 50.0,
            hp_per_sec: 3.0,
            accumulator: 0.0,
            heal_faction,
        });

        // White cabin atop the hull — a single rectangle slightly
        // forward of midship for the classic lifeboat silhouette
        // (cabin forward, open deck aft). Reuses the white `ally_flag`
        // material so we don't allocate a fresh handle.
        let cabin_mesh = meshes.add(Rectangle::new(hull_w * 0.55, hull_h * 0.30));
        let cabin = commands.spawn((
            Mesh2d(cabin_mesh),
            MeshMaterial2d(pm.ally_flag.clone()),
            Transform::from_xyz(0.0, hull_h * 0.10, 0.05),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(cabin).insert(ChildOf(ship));
    }

    // Minelayer drops a mine every 3 s at its stern position. Mines
    // self-arm after a short delay (so the layer doesn't sail straight
    // back over its own drop) and expire after a longer lifetime.
    if class == ShipClass::Minelayer {
        commands.entity(ship).insert(MineLayer {
            drop_interval: 3.0,
            cd: 1.5, // half-interval first drop so the player sees one early
            mine_damage: 4,
            mine_blast_radius: 7.0,
            target_faction,
        });

        // Cargo deck — a small flat rectangle on the aft half, in the
        // turret grey color, suggesting a rack of mines staged for
        // deployment.
        let deck_mesh = meshes.add(Rectangle::new(hull_w * 0.6, hull_h * 0.35));
        let deck = commands.spawn((
            Mesh2d(deck_mesh),
            MeshMaterial2d(pm.turret.clone()),
            Transform::from_xyz(0.0, -hull_h * 0.18, 0.05),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(deck).insert(ChildOf(ship));
    }

    // OilTanker — chunky industrial hull. Cycle component drives the
    // spray → burn → cooldown loop in `oil_tanker_cycle`. Visual deck
    // furniture: a wide flat deck cap (cargo manifold) in turret grey
    // and a small forward bridge so the silhouette reads "tanker"
    // instantly.
    // Viking — pure ram-attacker. No turrets / projectiles. The
    // distinctive silhouette is a dragon-head prow (small triangle
    // on the bow) + a central mast (thin dark-wood vertical pole)
    // amidships, sold cheaply with two child meshes.
    if class == ShipClass::Viking {
        commands.entity(ship).insert(VikingRamCharge::default());

        // Dragon prow — pointed triangle just past the bow tip.
        let prow_mesh = meshes.add(Triangle2d::new(
            Vec2::new(0.0, hull_h * 0.62),
            Vec2::new(-hull_w * 0.45, hull_h * 0.42),
            Vec2::new( hull_w * 0.45, hull_h * 0.42),
        ));
        let prow = commands.spawn((
            Mesh2d(prow_mesh),
            MeshMaterial2d(pm.viking_hull.clone()),
            Transform::from_xyz(0.0, 0.0, 0.04),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(prow).insert(ChildOf(ship));

        // Central mast — long dark-wood pole running most of the
        // deck length, shifted forward so its midpoint aligns with
        // the visual centre of the ship including the prow extension.
        let mast_h = hull_h * 0.78;
        let mast_y = hull_h * 0.04;
        let mast_mesh = meshes.add(Rectangle::new(hull_w * 0.22, mast_h));
        let mast = commands.spawn((
            Mesh2d(mast_mesh),
            MeshMaterial2d(pm.mast.clone()),
            Transform::from_xyz(0.0, mast_y, 0.05),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(mast).insert(ChildOf(ship));

        // Square white pennant centred on the mast top — sits flush
        // over the dark wood pole so the silhouette reads as a banner,
        // not a leaning sail.
        let flag_w = hull_w * 1.1;
        let flag_h = hull_h * 0.16;
        let mast_top_y = mast_y + mast_h * 0.5;
        let flag_mesh = meshes.add(Rectangle::new(flag_w, flag_h));
        let flag = commands.spawn((
            Mesh2d(flag_mesh),
            MeshMaterial2d(pm.ally_flag.clone()),
            Transform::from_xyz(0.0, mast_top_y - flag_h * 0.5, 0.06),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(flag).insert(ChildOf(ship));

        // Oars — three pairs of horizontal wood paddles sticking out
        // each gunwale. The shaft length runs across the hull edge so
        // the inner end is hidden by the deck; the outer end is the
        // visible "blade". Shared mesh handle (one Rectangle, six
        // instances) keeps this cheap.
        let oar_len = hull_w * 1.6;
        let oar_thick = 0.55;
        let oar_mesh = meshes.add(Rectangle::new(oar_len, oar_thick));
        // Y-positions span the middle ~70% of the hull, skipping the
        // bow (occupied by the prow) and a small stern margin.
        let oar_ys = [-hull_h * 0.28, -hull_h * 0.04, hull_h * 0.20];
        for &oy in &oar_ys {
            for &side in &[-1.0_f32, 1.0_f32] {
                let oar = commands.spawn((
                    Mesh2d(oar_mesh.clone()),
                    MeshMaterial2d(pm.mast.clone()),
                    Transform::from_xyz(side * (hull_w * 0.5 + oar_len * 0.35), oy, 0.02),
                    RenderLayers::layer(PLAY_LAYER),
                )).id();
                commands.entity(oar).insert(ChildOf(ship));
            }
        }
    }

    if class == ShipClass::OilTanker {
        commands.entity(ship).insert(OilTankerCycle {
            phase: OilCyclePhase::Spraying,
            timer: OIL_SPRAY_DURATION,
            drop_cd: 0.0,
            // The faction this tanker's burning oil damages — i.e.
            // anyone NOT on its side.
            target_faction,
        });

        // Wide cargo-deck slab covering most of the hull length, in
        // the turret-grey color. Sells the "industrial deck" read
        // distinct from the Carrier's flat-top.
        let deck_mesh = meshes.add(Rectangle::new(hull_w * 0.85, hull_h * 0.65));
        let deck = commands.spawn((
            Mesh2d(deck_mesh),
            MeshMaterial2d(pm.turret.clone()),
            Transform::from_xyz(0.0, -hull_h * 0.05, 0.04),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(deck).insert(ChildOf(ship));

        // Forward bridge — small white cabin near the bow.
        let bridge_mesh = meshes.add(Rectangle::new(hull_w * 0.55, hull_h * 0.18));
        let bridge = commands.spawn((
            Mesh2d(bridge_mesh),
            MeshMaterial2d(pm.ally_flag.clone()),
            Transform::from_xyz(0.0, hull_h * 0.32, 0.06),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(bridge).insert(ChildOf(ship));

        // Squared-off stern — the capsule's rounded bottom cap is
        // visually filled in with a hull-colored rectangle the width
        // of the hull and height of the cap radius. Reads as "industrial
        // tanker with a flat transom" instead of a torpedo.
        let stern_h = hull_w * 0.5;
        let stern_mesh = meshes.add(Rectangle::new(hull_w, stern_h));
        let stern = commands.spawn((
            Mesh2d(stern_mesh),
            MeshMaterial2d(pm.oil_tanker_hull.clone()),
            // Sit just above the hull's rounded bottom-cap so the
            // square's bottom edge aligns with the cap's bottom.
            Transform::from_xyz(0.0, -hull_h * 0.5 + stern_h * 0.5, 0.03),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(stern).insert(ChildOf(ship));
    }

    ship
}

/// Build the ship hull entity — mesh, transform, faction, health, velocity,
/// heading, hit-fx, fire-extent. Faction-agnostic: no `Ally` / `BossEnemy`
/// marker is attached. Returns the ship `Entity` so callers can layer
/// faction-specific markers + AI components on top.
///
/// This split is the start of boss-enemy support: the visual + simulation
/// scaffolding for any `ShipClass` is identical regardless of side, so
/// keeping it in one place lets a future `spawn_boss_ship` reuse the same
/// hull math.
pub fn spawn_ship_chassis(
    commands: &mut Commands,
    pm: &PaletteMaterials,
    meshes: &mut Assets<Mesh>,
    pos: Vec2,
    heading: f32,
    class: ShipClass,
    faction: FactionKind,
) -> Entity {
    let (hull_w, hull_h) = class.hull_dims();
    let hull_mesh = meshes.add(Capsule2d::new(hull_w / 2.0, hull_h - hull_w));
    let dir = Vec2::new(-heading.sin(), heading.cos());

    let body_mat = pm.hull_for_class(class).clone();
    commands.spawn((
        Mesh2d(hull_mesh),
        MeshMaterial2d(body_mat.clone()),
        Transform::from_xyz(pos.x, pos.y, 1.0)
            .with_rotation(Quat::from_rotation_z(heading)),
        Visibility::Inherited,
        Faction(faction),
        Health(class.hp()),
        Velocity(dir * class.speed()),
        Heading(heading),
        HitFx::new(body_mat),
        FireExtent(Vec2::new(hull_w * 0.5, hull_h * 0.5)),
        RenderLayers::layer(PLAY_LAYER),
    )).id()
}

/// Build a slightly bowed flag mesh: a wide rectangle of size
/// `fw × fh` whose top + bottom edges are shifted along the local +Y
/// axis by a `sin(πt)` profile, where `t` is the normalized position
/// along the width. Both edges shift by the same amount, so the flag
/// stays a constant `fh` thick — it's not wavy or fluttery, it's a
/// rigid bow as if a steady tailwind were holding it forward in the
/// middle. Endpoints stay anchored at `base_y` so the flag still
/// "attaches" cleanly at its sides.
///
/// Tessellated as a triangle strip with `N_SEGS` segments so the
/// curve reads smoothly without exploding the vertex count.
fn build_curved_flag_mesh(fw: f32, fh: f32, curve_amp: f32) -> Mesh {
    const N_SEGS: u32 = 8;

    let mut positions: Vec<[f32; 3]> = Vec::with_capacity((N_SEGS as usize + 1) * 2);
    for i in 0..=N_SEGS {
        let t = i as f32 / N_SEGS as f32;        // 0..1
        let x = -fw / 2.0 + t * fw;
        let bow = (t * std::f32::consts::PI).sin() * curve_amp;
        positions.push([x,  fh / 2.0 + bow, 0.0]); // top edge
        positions.push([x, -fh / 2.0 + bow, 0.0]); // bottom edge
    }

    let mut indices: Vec<u32> = Vec::with_capacity(N_SEGS as usize * 6);
    for i in 0..N_SEGS {
        let i0 = 2 * i;
        let i1 = 2 * i + 1;
        let i2 = 2 * i + 2;
        let i3 = 2 * i + 3;
        // Two CCW triangles per quad.
        indices.extend_from_slice(&[i0, i1, i2, i1, i3, i2]);
    }

    let normals: Vec<[f32; 3]> = vec![[0.0, 0.0, 1.0]; positions.len()];
    let uvs:     Vec<[f32; 2]> = vec![[0.0, 0.0];      positions.len()];

    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL,   normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0,     uvs);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

// ---------- Systems ----------

/// Movement AI — engage the nearest opposite-faction unit at moderate
/// range; wander toward random waypoints when no target is in sight.
/// Tenders bypass the engage logic and shadow the nearest unit of their
/// `heal_faction` instead, since their job is to hold heal-beam range,
/// not pick fights.
///
/// Faction-aware: the engage target = anything whose `Faction` differs
/// from the ally's own `Faction`. Tenders without a `HealBeamEmitter`
/// fall back to following same-faction units. The candidates query
/// excludes allies via `Without<Ally>`, so other allies are read from
/// a per-frame snapshot to avoid the `&mut Transform` / `&Transform`
/// borrow conflict.
pub fn ally_ai(
    time: Res<Time>,
    candidates: Query<(&Transform, &Faction), Without<Ally>>,
    mut allies: Query<(
        Entity,
        &mut Transform,
        &mut Velocity,
        &mut Heading,
        &mut Ally,
        &Faction,
        Option<&HealBeamEmitter>,
        Option<&mut VikingRamCharge>,
    )>,
) {
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();

    // Snapshot ally id+pos+faction so the tender follow branch can
    // reach other allies' positions without conflicting with the
    // outer `&mut Transform` borrow.
    let ally_snap: Vec<(Entity, Vec2, FactionKind)> = allies
        .iter()
        .map(|(e, tf, _, _, _, fac, _, _)| (e, tf.translation.truncate(), fac.0))
        .collect();

    for (entity, mut tf, mut vel, mut heading, mut ally, faction, emitter, mut viking_charge)
        in &mut allies
    {
        let pos = tf.translation.truncate();
        let mut speed = ally.class.speed();
        let mut turn = ally.class.turn_rate();

        // Tender follows the closest unit of its `heal_faction` so the
        // heal-beam emitter (range 50) stays on target. Falls back to
        // own faction if there's no emitter, and to the centre if no
        // candidate exists at all.
        if matches!(ally.class, ShipClass::Tender) {
            let follow_faction = emitter.map(|e| e.heal_faction).unwrap_or(faction.0);
            let mut nearest: Option<(f32, Vec2)> = None;

            for (otf, ofac) in &candidates {
                if ofac.0 != follow_faction { continue; }
                let op = otf.translation.truncate();
                let d2 = op.distance_squared(pos);
                if nearest.map_or(true, |(bd, _)| d2 < bd) {
                    nearest = Some((d2, op));
                }
            }
            for &(oe, op, ofac) in &ally_snap {
                if oe == entity { continue; }
                if ofac != follow_faction { continue; }
                let d2 = op.distance_squared(pos);
                if nearest.map_or(true, |(bd, _)| d2 < bd) {
                    nearest = Some((d2, op));
                }
            }

            let target = nearest.map(|(_, p)| p).unwrap_or(Vec2::ZERO);
            let to = target - pos;
            if to.length_squared() > 1.0 {
                let desired = (-to.x).atan2(to.y);
                heading.0 = approach_angle(heading.0, desired, turn * dt);
            }
            let dir = Vec2::new(-heading.0.sin(), heading.0.cos());
            // Holding distance: park the tender ~14 units off the unit
            // it's escorting so it doesn't crawl up onto the friendly
            // hull. Heal range is 50, so this is comfortably inside
            // beam range. Past the hold radius, ramp speed back in
            // smoothly over a 6-unit "approach band" so it doesn't
            // jolt to full speed the instant the target drifts away.
            const TENDER_HOLD_DIST: f32 = 14.0;
            const TENDER_APPROACH_BAND: f32 = 6.0;
            let dist = to.length();
            vel.0 = if dist <= TENDER_HOLD_DIST {
                Vec2::ZERO
            } else {
                let factor = ((dist - TENDER_HOLD_DIST) / TENDER_APPROACH_BAND)
                    .clamp(0.0, 1.0);
                dir * speed * factor
            };
            tf.rotation = Quat::from_rotation_z(heading.0);
            continue;
        }

        // Standard combat AI: engage opposite-faction units.
        let target_faction = faction.0.opposite();
        // OilTanker doesn't engage — chasing the player just means the
        // player runs and never enters the oil. Force the wander
        // fallback by leaving `nearest = None`, so the tanker drifts
        // between random waypoints and the player has to navigate
        // around the slick fields it lays.
        let mut nearest: Option<(f32, Vec2)> = None;
        if !matches!(ally.class, ShipClass::OilTanker) {
            for (otf, ofac) in &candidates {
                if ofac.0 != target_faction { continue; }
                let op = otf.translation.truncate();
                let d = op.distance(pos);
                if nearest.map_or(true, |(bd, _)| d < bd) {
                    nearest = Some((d, op));
                }
            }
        }

        // Track whether this tick had a live ram target so the Viking
        // charge ramp below knows whether to build speed or reset.
        let mut viking_has_target = false;
        let target = if let Some((d, ep)) = nearest {
            // Viking commits to a charge — always heads straight for
            // the target. No orbit, no standoff. `viking_ram_damage`
            // delivers the payoff on contact.
            if matches!(ally.class, ShipClass::Viking) {
                viking_has_target = true;
                ep
            } else {
                // Engage: orbit at the class's preferred range. Broadside
                // ships sit at ~70% of TURRET_RANGE so their cannons can
                // bear; Blackbeard sits closer (its `orbit_range` of 18
                // sits comfortably inside the new BOARDING_RANGE = 45).
                let to = ep - pos;
                let unit = to.normalize_or_zero();
                let desired_range = ally.class.orbit_range();
                if d > desired_range + 8.0 {
                    ep
                } else if d < desired_range - 8.0 {
                    pos - unit * 30.0
                } else {
                    let perp = Vec2::new(-unit.y, unit.x);
                    pos + perp * 30.0
                }
            }
        } else {
            // No enemies — wander between random waypoints.
            ally.waypoint_timer -= dt;
            if ally.waypoint_timer <= 0.0 {
                ally.waypoint_timer = rng.gen_range(2.5..5.5);
                ally.waypoint = Vec2::new(
                    rng.gen_range(-PLAY_WORLD * 0.35..PLAY_WORLD * 0.35),
                    rng.gen_range(-PLAY_WORLD * 0.35..PLAY_WORLD * 0.35),
                );
            }
            ally.waypoint
        };

        // Viking charge ramp: while a target is held, build current
        // speed from base toward max over `VIKING_RAM_RAMP_TIME`. As
        // speed climbs the turn rate falls toward `VIKING_RAM_TURN_AT_MAX`,
        // so a Viking at full charge overshoots and has to circle
        // back. Without a target the speed snaps back to base so the
        // next charge starts slow. `viking_ram_damage` resets the
        // component on contact via `Commands` for the same reason.
        if let Some(charge) = viking_charge.as_deref_mut() {
            // Bull-charge gate: only ramp speed when the Viking is
            // already roughly *facing* the target. Mid-turn, the
            // ship slows back toward base — so a missed charge has
            // to re-align before regaining steam, and a hit that
            // throws off the heading naturally costs momentum.
            let aligned = if let Some((_, ep)) = nearest {
                let to_target = ep - pos;
                if to_target.length_squared() > 1.0 {
                    let desired = (-to_target.x).atan2(to_target.y);
                    let delta = (heading.0 - desired + std::f32::consts::PI)
                        .rem_euclid(std::f32::consts::TAU)
                        - std::f32::consts::PI;
                    delta.abs() < VIKING_RAM_ALIGN_THRESHOLD
                } else { false }
            } else { false };

            if viking_has_target && aligned {
                let ramp_per_sec =
                    (VIKING_RAM_MAX_SPEED - VIKING_RAM_BASE_SPEED) / VIKING_RAM_RAMP_TIME;
                charge.current_speed = (charge.current_speed + ramp_per_sec * dt)
                    .min(VIKING_RAM_MAX_SPEED);
            } else {
                // Bleed back to base whenever the Viking isn't
                // pointed at its target — covers turning, target
                // lost, and the recovery beat after a missed ram
                // when the heading swings off-line.
                charge.current_speed = (charge.current_speed - VIKING_RAM_DECAY_PER_SEC * dt)
                    .max(VIKING_RAM_BASE_SPEED);
            }
            let t = ((charge.current_speed - VIKING_RAM_BASE_SPEED)
                / (VIKING_RAM_MAX_SPEED - VIKING_RAM_BASE_SPEED))
                .clamp(0.0, 1.0);
            speed = charge.current_speed;
            turn = ally.class.turn_rate()
                + (VIKING_RAM_TURN_AT_MAX - ally.class.turn_rate()) * t;
        }

        // Keep target inside the play area so we don't crash the wall.
        let margin = 10.0;
        let bound = PLAY_WORLD / 2.0 - margin;
        let target = Vec2::new(target.x.clamp(-bound, bound), target.y.clamp(-bound, bound));

        let to = target - pos;
        if to.length_squared() > 1.0 {
            let desired = (-to.x).atan2(to.y);
            heading.0 = approach_angle(heading.0, desired, turn * dt);
        }
        let dir = Vec2::new(-heading.0.sin(), heading.0.cos());
        vel.0 = dir * speed;
        tf.rotation = Quat::from_rotation_z(heading.0);
    }
}

/// Per-ally-turret targeting + aim + fire. Shape mirrors the player's
/// `turret_aim_fire` but without the per-slot config / weapon-type branching:
/// turrets fire a single Standard bullet at a fixed rate per class.
///
/// Faction-aware: the candidate-target query covers every entity that
/// owns a `Faction`, and each turret keeps only those whose faction
/// matches its `target_faction`. A boss-side spawn that gives the same
/// `AllyTurret` `target_faction = Friendly` reuses this system unchanged.
pub fn ally_turret_aim_fire(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    owners: Query<(&Transform, &Heading), Without<AllyTurret>>,
    owner_class: Query<&Ally>,
    targets: Query<(&Transform, &Faction), Without<AllyTurret>>,
    mut turrets: Query<
        (&ChildOf, &mut AllyTurret, &mut Transform),
        Without<Faction>,
    >,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();

    for (parent, mut turret, mut tf) in &mut turrets {
        let Ok((owner_tf, owner_heading)) = owners.get(parent.0) else { continue; };
        let owner_pos = owner_tf.translation.truncate();
        let owner_h = owner_heading.0;
        turret.fire_cd -= dt;

        // World position of this turret (parent rotation × local offset).
        let local = tf.translation.truncate();
        let cos_h = owner_h.cos();
        let sin_h = owner_h.sin();
        let world_off = Vec2::new(
            local.x * cos_h - local.y * sin_h,
            local.x * sin_h + local.y * cos_h,
        );
        let turret_world = owner_pos + world_off;

        // Find best target inside the turret's arc + range.
        let arc_half = turret.class.turret_arc_half();
        let mut best: Option<(f32, Vec2)> = None;
        for (etf, fac) in &targets {
            if fac.0 != turret.target_faction { continue; }
            let ep = etf.translation.truncate();
            let to = ep - turret_world;
            let d = to.length();
            if d > TURRET_RANGE { continue; }
            let world_angle = (-to.x).atan2(to.y);
            let mut local_angle = world_angle - owner_h;
            local_angle = (local_angle + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                - std::f32::consts::PI;
            let mut off = local_angle - turret.mount_angle;
            off = (off + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                - std::f32::consts::PI;
            if off.abs() > arc_half { continue; }
            if best.map_or(true, |(bd, _)| d < bd) {
                best = Some((d, ep));
            }
        }

        let desired_local = if let Some((_, ep)) = best {
            let to = ep - turret_world;
            let world_angle = (-to.x).atan2(to.y);
            let mut la = world_angle - owner_h;
            la = (la + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                - std::f32::consts::PI;
            la
        } else {
            turret.mount_angle
        };

        turret.barrel_angle = approach_angle(turret.barrel_angle, desired_local, TURRET_PIVOT * dt);
        tf.rotation = Quat::from_rotation_z(turret.barrel_angle);

        // Fire when aimed. Bullet carries the OWN faction (opposite of
        // target) so the existing collision pipeline routes hits onto
        // the correct side automatically.
        if best.is_some() {
            let aim_err = (turret.barrel_angle - desired_local).abs();
            if aim_err < 0.1 && turret.fire_cd <= 0.0 {
                turret.fire_cd = 1.0 / turret.class.fire_rate().max(0.1);
                let total_angle = owner_h + turret.barrel_angle;
                let barrel_forward = Vec2::new(-total_angle.sin(), total_angle.cos());
                let muzzle_pos = turret_world + barrel_forward * 4.0;
                // Credit ally-turret kills to the parent ship's class
                // (e.g. multiple PirateShip cannons all roll up into
                // one PIR row in the damage panel).
                let source = owner_class
                    .get(parent.0)
                    .ok()
                    .map(|a| crate::bullet::DamageSource::Ally(a.class));
                spawn_combat_bullet(
                    &mut commands,
                    &em,
                    &pm.bullet_friendly_outer,
                    &pm.bullet_friendly,
                    muzzle_pos,
                    barrel_forward,
                    WeaponType::Standard,
                    turret.class.fire_damage(),
                    source,
                    TURRET_RANGE,
                    [None; 3], // ally turrets don't currently carry runes
                    turret.target_faction.opposite(),
                );
            }
        }
    }
}

/// Despawn allies that have hit 0 HP, with a destruction burst. Decoupled
/// from the bullet collision system so we can keep the bullet-vs-friendly
/// query simple (it just decrements HP).
/// Boss-side Viking AI — `enemy_ai` doesn't know about
/// `VikingRamCharge`, so a 5★ Viking boss would otherwise sit at
/// `EnemyVariant::Standard` walking pace. This system overrides the
/// boss's heading + velocity each frame to use the same charge-ramp
/// curve as the friendly Viking ally: build speed toward
/// `VIKING_RAM_MAX_SPEED` while a target is held, decay slowly when
/// the target is lost, lerp turn rate toward `VIKING_RAM_TURN_AT_MAX`
/// at peak so a missed charge has to circle back wide.
///
/// Runs on `With<Enemy>` so it only touches boss-side ships;
/// friendly Vikings keep going through `ally_ai`.
pub fn boss_viking_ai(
    time: Res<Time>,
    candidates: Query<(&Transform, &Faction), Without<VikingRamCharge>>,
    mut vikings: Query<
        (&mut Transform, &mut Velocity, &mut Heading, &Faction, &mut VikingRamCharge),
        With<crate::enemy::Enemy>,
    >,
) {
    let dt = time.delta_secs();
    for (mut tf, mut vel, mut heading, faction, mut charge) in &mut vikings {
        let pos = tf.translation.truncate();
        let target_faction = faction.0.opposite();

        let mut nearest: Option<(f32, Vec2)> = None;
        for (otf, ofac) in &candidates {
            if ofac.0 != target_faction { continue; }
            let op = otf.translation.truncate();
            let d = op.distance(pos);
            if nearest.map_or(true, |(bd, _)| d < bd) {
                nearest = Some((d, op));
            }
        }

        // Bull-charge gate — only ramp speed when already aimed at
        // the target. Bleed otherwise so a miss costs momentum.
        let aligned = if let Some((_, ep)) = nearest {
            let to_target = ep - pos;
            if to_target.length_squared() > 1.0 {
                let desired = (-to_target.x).atan2(to_target.y);
                let delta = (heading.0 - desired + std::f32::consts::PI)
                    .rem_euclid(std::f32::consts::TAU)
                    - std::f32::consts::PI;
                delta.abs() < VIKING_RAM_ALIGN_THRESHOLD
            } else { false }
        } else { false };

        if nearest.is_some() && aligned {
            let ramp = (VIKING_RAM_MAX_SPEED - VIKING_RAM_BASE_SPEED) / VIKING_RAM_RAMP_TIME;
            charge.current_speed = (charge.current_speed + ramp * dt)
                .min(VIKING_RAM_MAX_SPEED);
        } else {
            charge.current_speed = (charge.current_speed - VIKING_RAM_DECAY_PER_SEC * dt)
                .max(VIKING_RAM_BASE_SPEED);
        }

        let speed = charge.current_speed;
        let t = ((charge.current_speed - VIKING_RAM_BASE_SPEED)
            / (VIKING_RAM_MAX_SPEED - VIKING_RAM_BASE_SPEED))
            .clamp(0.0, 1.0);
        let base_turn = ShipClass::Viking.turn_rate();
        let turn = base_turn + (VIKING_RAM_TURN_AT_MAX - base_turn) * t;

        if let Some((_, ep)) = nearest {
            let to = ep - pos;
            if to.length_squared() > 1.0 {
                let desired = (-to.x).atan2(to.y);
                heading.0 = approach_angle(heading.0, desired, turn * dt);
            }
        }
        let dir = Vec2::new(-heading.0.sin(), heading.0.cos());
        vel.0 = dir * speed;
        tf.rotation = Quat::from_rotation_z(heading.0);
    }
}

pub fn ally_death_check(
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    allies: Query<(Entity, &Transform, &Ally, &Health)>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let mut rng = rand::thread_rng();
    for (e, tf, ally, h) in &allies {
        if h.0 <= 0 {
            let pos = tf.translation.truncate();
            // Use the ally's own hull color in the death burst so a Pirate
            // explodes brown, future classes explode their own color.
            spawn_hit_particles(&mut commands, &em, pm.hull_for_class(ally.class), pos, 18, 80.0,  &mut rng);
            spawn_hit_particles(&mut commands, &em, &pm.bullet_friendly,           pos, 10, 100.0, &mut rng);
            commands.entity(e).despawn();
        }
    }
}

/// `Ally::hit_radius` exposed as a free function so collision systems don't
/// need to pull the class out themselves.
pub fn ally_hit_radius(ally: &Ally) -> f32 {
    ally.class.hit_radius()
}

/// True if this ally is treated as underwater (currently: Submarine).
/// Surfaced as a free function so bullet / bomber / target-selection code
/// doesn't need to peek inside the variant.
pub fn ally_is_submerged(ally: &Ally) -> bool {
    ally.class.is_submerged()
}

// ---------- Homing missiles ----------

/// Speed of a missile in flight. Slower than a cannonball (`BULLET_SPEED`
/// = 110) so the homing curve is visible and dodgeable.
const MISSILE_SPEED: f32 = 60.0;
/// How long a missile can stay airborne before auto-despawning. Generous
/// so a missile that loses its target mid-flight (the enemy died) can
/// still home onto a fresh one — but not unbounded.
const MISSILE_RANGE: f32 = 300.0;
/// Max angular adjustment per second for the missile's velocity vector.
/// Small enough that fast Scouts can break lock if they juke at the right
/// moment; big enough that lazy Standards can't out-run the lock.
const MISSILE_TURN_RATE: f32 = 3.0;

/// Spawn one homing missile. Hits/damage flow through the standard
/// `Bullet` collision path — `HomingMissile` only adds the per-frame
/// re-aim. Mesh layers (outer rust + inner flame) match the
/// player-bullet two-tone language. The bullet's `faction` is the
/// *opposite* of `target_faction` so it damages the right side via the
/// existing collision pipeline.
fn spawn_homing_missile(
    commands: &mut Commands,
    em: &EffectMeshes,
    pm: &PaletteMaterials,
    pos: Vec2,
    forward: Vec2,
    damage: i32,
    initial_target: Option<Entity>,
    target_faction: FactionKind,
    source: Option<crate::bullet::DamageSource>,
) {
    let heading_rot = (-forward.x).atan2(forward.y);
    let bullet = commands.spawn((
        Mesh2d(em.bullet_missile_outer.clone()),
        MeshMaterial2d(pm.bullet_missile_outer.clone()),
        Transform::from_xyz(pos.x, pos.y, 4.0)
            .with_rotation(Quat::from_rotation_z(heading_rot)),
        Bullet {
            faction: target_faction.opposite(),
            damage,
            remaining: MISSILE_RANGE,
            weapon: WeaponType::Standard,
            source,
            runes: [None; 3],
        },
        Velocity(forward * MISSILE_SPEED),
        HomingMissile {
            target: initial_target,
            turn_rate: MISSILE_TURN_RATE,
            target_faction,
        },
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    let inner = commands.spawn((
        Mesh2d(em.bullet_missile_inner.clone()),
        MeshMaterial2d(pm.bullet_missile_inner.clone()),
        Transform::from_xyz(0.0, 0.0, 0.05),
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    commands.entity(inner).insert(ChildOf(bullet));
}

/// Tick every `MissileLauncher`'s cooldown and fire when due. Skipped if
/// no valid target (one with `Faction == launcher.target_faction`)
/// exists — the cooldown still ticks, so as soon as one shows up the
/// launcher fires immediately (assuming cd already ran out).
pub fn missile_launcher_fire(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    candidates: Query<(Entity, &Transform, &Faction)>,
    mut launchers: Query<(&Transform, &Heading, &mut MissileLauncher)>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();

    for (tf, heading, mut launcher) in &mut launchers {
        launcher.cd -= dt;

        // Snap targets matching the launcher's faction. Done per-launcher
        // so each can have its own target_faction without cross-talk.
        let target_snap: Vec<(Entity, Vec2)> = candidates
            .iter()
            .filter(|(_, _, f)| f.0 == launcher.target_faction)
            .map(|(e, t, _)| (e, t.translation.truncate()))
            .collect();

        if target_snap.is_empty() { continue; }
        if launcher.cd > 0.0 { continue; }
        launcher.cd = 1.0 / launcher.fire_rate.max(0.001);

        let pos = tf.translation.truncate();
        let h = heading.0;
        let forward = Vec2::new(-h.sin(), h.cos());
        let muzzle = pos + forward * launcher.muzzle_offset;

        // Initial target = nearest matching unit. The missile
        // re-acquires each frame in `homing_missile_track`, so this is
        // really just the seed pick.
        let target = target_snap
            .iter()
            .min_by(|a, b| {
                let da = a.1.distance_squared(muzzle);
                let db = b.1.distance_squared(muzzle);
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(e, _)| *e);

        spawn_homing_missile(
            &mut commands, &em, &pm,
            muzzle, forward, launcher.damage, target,
            launcher.target_faction, launcher.source,
        );
    }
}

/// Per-frame steering update for in-flight missiles: if the cached target
/// is gone, snap to the nearest matching-faction unit; then rotate
/// `Velocity` toward the target by at most `turn_rate * dt`. Speed is
/// preserved — only the direction is steered. Runs before `apply_velocity`
/// so the new heading drives the integration step.
pub fn homing_missile_track(
    time: Res<Time>,
    candidates: Query<(Entity, &Transform, &Faction), Without<HomingMissile>>,
    mut missiles: Query<(&mut Transform, &mut Velocity, &mut HomingMissile)>,
) {
    let dt = time.delta_secs();

    for (mut tf, mut vel, mut m) in &mut missiles {
        let pos = tf.translation.truncate();
        let target_faction = m.target_faction;

        // Look up the cached target's current position (and verify it's
        // still on the right faction — a despawn-and-reuse-id scenario
        // is unlikely but cheap to guard).
        let cached_pos = m.target.and_then(|t| {
            candidates.iter().find_map(|(e, tf, f)| {
                if e == t && f.0 == target_faction {
                    Some(tf.translation.truncate())
                } else {
                    None
                }
            })
        });

        // Otherwise pick the nearest fresh target of the right faction.
        let target_pos = cached_pos.or_else(|| {
            let nearest = candidates
                .iter()
                .filter(|(_, _, f)| f.0 == target_faction)
                .min_by(|a, b| {
                    let da = a.1.translation.truncate().distance_squared(pos);
                    let db = b.1.translation.truncate().distance_squared(pos);
                    da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                });
            if let Some((e, t, _)) = nearest {
                m.target = Some(e);
                Some(t.translation.truncate())
            } else {
                m.target = None;
                None
            }
        });

        if let Some(tp) = target_pos {
            let to = tp - pos;
            let speed = vel.0.length().max(1.0);
            if to.length_squared() > 0.5 {
                let cur_angle = (-vel.0.x).atan2(vel.0.y);
                let desired_angle = (-to.x).atan2(to.y);
                let new_angle = approach_angle(cur_angle, desired_angle, m.turn_rate * dt);
                let new_dir = Vec2::new(-new_angle.sin(), new_angle.cos());
                vel.0 = new_dir * speed;
                tf.rotation = Quat::from_rotation_z(new_angle);
            }
        }
    }
}

// ---------- Boarding ----------

/// Maximum distance at which a `BoardingLauncher` will commit a
/// party across the gap. Generous range (≈ same as the heal beam):
/// the boarders themselves are entity-tracking, so as long as the
/// target is inside this radius at *fire time* the party will catch
/// up even if it moves. Lets Blackbeard reliably engage targets
/// that drift through its zone without needing to physically chase.
const BOARDING_RANGE: f32 = 45.0;
/// How fast the travel-state lerp completes — `t` advances by
/// `BOARDER_TRAVEL_RATE × dt` per frame. 1.4 ⇒ ~0.7 s end-to-end so
/// the boarders read as people *crossing* the rope, not a tracer
/// round flashing across the gap.
const BOARDER_TRAVEL_RATE: f32 = 1.4;

/// Tick every `BoardingLauncher`: reset the cooldown each frame, and
/// when it expires AND a target-faction enemy is within range, spawn
/// a `party_size` cluster of `Boarder` entities aimed at it.
pub fn boarding_launcher_fire(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    candidates: Query<(Entity, &Transform, &Faction)>,
    mut launchers: Query<(Entity, &Transform, &mut BoardingLauncher)>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();

    for (launcher_e, launcher_tf, mut launcher) in &mut launchers {
        // Reload only while idle. Once `ready` flips back on, the
        // cooldown stops draining — the boarding party sits cached,
        // waiting for a target to walk into range.
        if !launcher.ready {
            launcher.cd = (launcher.cd - dt).max(0.0);
            if launcher.cd <= 0.0 {
                launcher.ready = true;
            }
        }

        // Find nearest target-faction unit. No target → wait.
        let pos = launcher_tf.translation.truncate();
        let r2 = launcher.range * launcher.range;
        let nearest = candidates.iter()
            .filter(|(_, _, f)| f.0 == launcher.target_faction)
            .map(|(e, t, _)| {
                let p = t.translation.truncate();
                (e, p, p.distance_squared(pos))
            })
            .filter(|(_, _, d2)| *d2 <= r2)
            .min_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));

        let Some((target_e, _, _)) = nearest else { continue; };
        if !launcher.ready { continue; }
        launcher.ready = false;
        launcher.cd = 1.0 / launcher.fire_rate.max(0.001);

        // Visible rope across the gap. Lives until the last boarder
        // would have despawned (travel ≈ 0.4 s + attach_duration +
        // a small grace) so the rope reads as the connection that
        // the crew is currently using, not just an opening flourish.
        let rope_lifetime = 0.4 + launcher.attach_duration + 0.2;
        commands.spawn((
            Mesh2d(em.beam.clone()),
            MeshMaterial2d(pm.boarding_rope.clone()),
            // Z = 4.4 — above bullets/beams (4.0/5.5), below
            // boarders (4.5) so the boarders ride visually on top.
            Transform::from_xyz(pos.x, pos.y, 4.4),
            BoardingRope {
                source: launcher_e,
                target: target_e,
                lifetime: rope_lifetime,
            },
            RenderLayers::layer(PLAY_LAYER),
        ));

        // Spawn the party. Each boarder gets a small random offset
        // so the cluster spreads around the target's hull instead of
        // overlapping. Z = 4.5 puts boarders above bullets but below
        // muzzle flashes — they should read as the dominant on-deck
        // action while attached.
        for _ in 0..launcher.party_size {
            let offset = Vec2::new(
                rng.gen_range(-2.5..2.5),
                rng.gen_range(-2.5..2.5),
            );
            commands.spawn((
                Mesh2d(em.boarder_dot.clone()),
                MeshMaterial2d(pm.boarder.clone()),
                Transform::from_xyz(pos.x, pos.y, 4.5),
                Boarder {
                    source: launcher_e,
                    target: target_e,
                    state: BoarderState::Traveling { t: 0.0 },
                    offset,
                    damage_per_tick: launcher.damage_per_tick,
                    tick_interval: launcher.tick_interval,
                    tick_cd: 0.0,
                    attach_duration: launcher.attach_duration,
                },
                RenderLayers::layer(PLAY_LAYER),
            ));
        }
    }
}

/// Drive every in-flight `Boarder`. Two phases:
///   - `Traveling` — lerp position from source ship to target enemy.
///   - `Attached`  — track the target each frame; tick damage on the
///                  configured cadence; despawn when the attach
///                  timer runs out.
/// Despawns the boarder if either source or target despawn mid-flight
/// so we never end up with orphan dots after a chaotic frame.
pub fn boarder_tick(
    time: Res<Time>,
    mut commands: Commands,
    sources: Query<&Transform, (Without<Boarder>, Without<Enemy>)>,
    mut targets: Query<(&Transform, &mut Health, &mut HitFx), With<Enemy>>,
    // `Without<Enemy>` makes this query provably disjoint from
    // `targets` for Bevy's parameter-conflict checker. Boarders are
    // friendly-side spawned by the launcher, so they never carry the
    // Enemy marker — the filter just teaches the type system that.
    mut boarders: Query<(Entity, &mut Transform, &mut Boarder), Without<Enemy>>,
    mut stats: ResMut<crate::ui::DamageStats>,
) {
    let dt = time.delta_secs();

    for (boarder_e, mut tf, mut boarder) in &mut boarders {
        match boarder.state {
            BoarderState::Traveling { t } => {
                let Ok(src_tf) = sources.get(boarder.source) else {
                    commands.entity(boarder_e).despawn();
                    continue;
                };
                let Ok((target_tf, _, _)) = targets.get(boarder.target) else {
                    commands.entity(boarder_e).despawn();
                    continue;
                };
                let new_t = (t + dt * BOARDER_TRAVEL_RATE).min(1.0);
                let pos = src_tf.translation.truncate()
                    .lerp(target_tf.translation.truncate(), new_t);
                tf.translation.x = pos.x;
                tf.translation.y = pos.y;

                if new_t >= 1.0 {
                    boarder.state = BoarderState::Attached {
                        remaining: boarder.attach_duration,
                    };
                    boarder.tick_cd = 0.0; // bite immediately on arrival
                } else {
                    boarder.state = BoarderState::Traveling { t: new_t };
                }
            }
            BoarderState::Attached { remaining } => {
                let Ok((target_tf, mut h, mut fx)) =
                    targets.get_mut(boarder.target)
                else {
                    commands.entity(boarder_e).despawn();
                    continue;
                };
                let pos = target_tf.translation.truncate() + boarder.offset;
                tf.translation.x = pos.x;
                tf.translation.y = pos.y;

                let new_remaining = remaining - dt;
                boarder.tick_cd -= dt;
                if boarder.tick_cd <= 0.0 {
                    boarder.tick_cd = boarder.tick_interval;
                    let dealt = crate::bullet::apply_damage(&mut h, &mut fx, boarder.damage_per_tick);
                    // Boarders are launched by Blackbeard — credit the
                    // BLK row in the damage panel.
                    crate::bullet::credit_damage(
                        &mut stats,
                        Some(crate::bullet::DamageSource::Ally(ShipClass::Blackbeard)),
                        dealt,
                    );
                }

                if new_remaining <= 0.0 {
                    commands.entity(boarder_e).despawn();
                } else {
                    boarder.state = BoarderState::Attached { remaining: new_remaining };
                }
            }
        }
    }
}

/// Each frame, anchor every active `BoardingRope` between its source
/// (the Blackbeard ship) and its target (the enemy it's boarding) and
/// tick its lifetime. The rope is the existing beam mesh — long axis
/// = +Y, scale.x for thickness, scale.y for length-fraction — repointed
/// each frame so it tracks moving ships.
pub fn update_boarding_ropes(
    time: Res<Time>,
    mut commands: Commands,
    sources: Query<&Transform, (Without<BoardingRope>, Without<Enemy>, Without<Boarder>)>,
    targets: Query<&Transform, (With<Enemy>, Without<BoardingRope>)>,
    mut ropes: Query<(Entity, &mut Transform, &mut BoardingRope)>,
) {
    let dt = time.delta_secs();
    for (rope_e, mut tf, mut rope) in &mut ropes {
        rope.lifetime -= dt;
        if rope.lifetime <= 0.0 {
            commands.entity(rope_e).despawn();
            continue;
        }
        let Ok(src_tf) = sources.get(rope.source) else {
            commands.entity(rope_e).despawn();
            continue;
        };
        let Ok(tgt_tf) = targets.get(rope.target) else {
            commands.entity(rope_e).despawn();
            continue;
        };
        let a = src_tf.translation.truncate();
        let b = tgt_tf.translation.truncate();
        let delta = b - a;
        let len = delta.length();
        if len < 0.5 { continue; }
        let mid = (a + b) * 0.5;
        let angle = (-delta.x).atan2(delta.y);
        tf.translation.x = mid.x;
        tf.translation.y = mid.y;
        tf.rotation = Quat::from_rotation_z(angle);
        // Beam mesh is BEAM_LENGTH long along +Y; scale y by the
        // fraction we want, x = 1.5 so the rope reads as a clear
        // strand at the play-area's nearest-neighbor pixel scale.
        tf.scale = Vec3::new(1.5, len / crate::balance::BEAM_LENGTH, 1.0);
    }
}

// ---------- Sea mines ----------

/// Initial arm delay on a freshly-dropped mine. Long enough that the
/// laying ship can't re-cross its own drop and detonate it, but short
/// enough that an enemy chasing the layer eats the mine within a second
/// of pursuit.
const MINE_ARM_DELAY: f32 = 0.6;
/// How long a mine sits on the water before quietly despawning. Stops
/// long combats from accumulating a dense minefield.
const MINE_LIFETIME: f32 = 18.0;

/// Spawn one mine at `pos`: a dark spherical body with a flashing red
/// warning dot in the middle. The dot pulses its scale via
/// `flash_mine_dots` so the mine reads as a live, armed hazard rather
/// than a static sprite.
///
/// `target_faction` is cached on the mine itself so the proximity
/// check is faction-aware after the laying ship is gone.
fn spawn_mine(
    commands: &mut Commands,
    em: &EffectMeshes,
    pm: &PaletteMaterials,
    pos: Vec2,
    damage: i32,
    blast_radius: f32,
    target_faction: FactionKind,
) {
    let mine = commands.spawn((
        Mesh2d(em.mine_outer.clone()),
        MeshMaterial2d(pm.mine_outer.clone()),
        Transform::from_xyz(pos.x, pos.y, 0.6),
        Mine {
            damage,
            blast_radius,
            arm_timer: MINE_ARM_DELAY,
            lifetime: MINE_LIFETIME,
            target_faction,
        },
        RenderLayers::layer(PLAY_LAYER),
    )).id();

    // Centre warning dot — tagged for the flash system to pulse.
    let dot = commands.spawn((
        Mesh2d(em.mine_inner.clone()),
        MeshMaterial2d(pm.mine_inner.clone()),
        Transform::from_xyz(0.0, 0.0, 0.05),
        MineDotFlash,
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    commands.entity(dot).insert(ChildOf(mine));
}

/// Tick every `MineLayer`'s cooldown and drop a mine when due. Drop
/// position is the laying ship's *stern* (one hull-half behind the
/// hull center along the inverse-forward vector) so the mine appears
/// in the wake instead of right under the keel.
pub fn mine_layer_drop(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    mut layers: Query<(&Transform, &Heading, &Ally, &mut MineLayer)>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();

    for (tf, heading, ally, mut layer) in &mut layers {
        layer.cd -= dt;
        if layer.cd > 0.0 { continue; }
        layer.cd = layer.drop_interval;

        let pos = tf.translation.truncate();
        let h = heading.0;
        let forward = Vec2::new(-h.sin(), h.cos());
        let (_hull_w, hull_h) = ally.class.hull_dims();
        let drop_pos = pos - forward * (hull_h * 0.5 + 1.0);
        spawn_mine(
            &mut commands, &em, &pm, drop_pos,
            layer.mine_damage, layer.mine_blast_radius, layer.target_faction,
        );
    }
}

/// Tick mines: arm timer + lifetime + proximity detonation. A mine
/// detonates when any unit of its `target_faction` is within
/// `blast_radius`, dealing `damage` to every same-faction unit in range
/// (true AOE). Lifetime expiry is silent — no boom — so the player
/// isn't surprised by a stray explosion in the middle of nowhere.
pub fn mine_tick(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    mut victims: Query<(Entity, &Transform, &Faction, &mut Health, &mut HitFx)>,
    mut mines: Query<(Entity, &Transform, &mut Mine)>,
    mut stats: ResMut<crate::ui::DamageStats>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();

    // Snapshot once per frame: every faction-bearing entity's id + pos
    // + faction. Mines filter to their own `target_faction` inline.
    let victim_snap: Vec<(Entity, Vec2, FactionKind)> = victims
        .iter()
        .map(|(e, t, f, _, _)| (e, t.translation.truncate(), f.0))
        .collect();

    for (mine_e, mine_tf, mut mine) in &mut mines {
        mine.arm_timer = (mine.arm_timer - dt).max(0.0);
        mine.lifetime -= dt;
        if mine.lifetime <= 0.0 {
            commands.entity(mine_e).despawn();
            continue;
        }
        if mine.arm_timer > 0.0 { continue; }

        let mp = mine_tf.translation.truncate();
        let r2 = mine.blast_radius * mine.blast_radius;
        let triggered = victim_snap.iter().any(|(_, p, f)| {
            *f == mine.target_faction && p.distance_squared(mp) < r2
        });
        if !triggered { continue; }

        // AOE damage — every same-faction unit within blast radius
        // takes the full hit. No falloff for now; if the radius is
        // small enough the simple model reads cleanly.
        for &(e, ep, f) in &victim_snap {
            if f != mine.target_faction { continue; }
            if ep.distance_squared(mp) >= r2 { continue; }
            if let Ok((_, _, _, mut h, mut fx)) = victims.get_mut(e) {
                let dealt = crate::bullet::apply_damage(&mut h, &mut fx, mine.damage);
                // Mines belong to Minelayer — credit the MIN row.
                crate::bullet::credit_damage(
                    &mut stats,
                    Some(crate::bullet::DamageSource::Ally(ShipClass::Minelayer)),
                    dealt,
                );
            }
        }

        // Two-tone burst — dark shrapnel + red flash — at the blast
        // origin. Generic enemy-color destruction is added by
        // `enemy_death_check` for any enemies that hit zero from this.
        spawn_hit_particles(&mut commands, &em, &pm.mine_outer, mp, 12, 80.0,  &mut rng);
        spawn_hit_particles(&mut commands, &em, &pm.mine_inner, mp, 8,  100.0, &mut rng);
        commands.entity(mine_e).despawn();
    }
}

/// Blink the warning dot fully on / off. Scale-pulsing was unreliable
/// at the play area's nearest-neighbor upscale: a sub-pixel mesh radius
/// can drop below the integer grid and skip rendering on some frames,
/// so the flash looked uneven across mines. A binary `Visibility`
/// toggle leaves the dot at its natural size every "on" frame, so
/// every red pixel flashes uniformly and in sync.
///
/// Pattern: 0.55 s on, 0.25 s off (cycle 0.8 s). Reads as a clear
/// blinking warning beat without feeling stroboscopic.
pub fn flash_mine_dots(
    time: Res<Time>,
    mut q: Query<&mut Visibility, With<MineDotFlash>>,
) {
    const PERIOD:       f32 = 0.8;
    const ON_DURATION:  f32 = 0.55;
    let cycle = time.elapsed_secs().rem_euclid(PERIOD);
    let want = if cycle < ON_DURATION {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    for mut vis in &mut q {
        if *vis != want { *vis = want; }
    }
}

// ---------- Heal visuals ----------

/// Spawn the per-frame heal "stream" between tender and target. Two
/// effects layered together:
///
///   1. **Stream particles** — small motes scattered along the
///      tender→target line, each drifting toward the target with a
///      slight perpendicular wobble. Per-frame respawn paints a
///      flowing-energy ribbon without a hard rectangle anywhere.
///   2. **Target sparkles** — short-lived motes that bloom upward
///      around the target (organic-looking; suggests "healing
///      lifting from the unit").
///
/// All particles route through the existing `HitParticle` ticker
/// (drag + life-fade), so they fit the rest of the FX language.
fn spawn_heal_visual(
    commands: &mut Commands,
    em: &EffectMeshes,
    mat: &Handle<ColorMaterial>,
    tender: Vec2,
    target: Vec2,
    rng: &mut rand::rngs::ThreadRng,
) {
    let to = target - tender;
    let len = to.length();
    if len < 0.5 { return; }
    let dir = to / len;
    let perp = Vec2::new(-dir.y, dir.x);

    // Two stream particles per frame distributed along the line. Spawn
    // location is randomly placed along the line + jittered
    // perpendicularly; velocity points toward the target so each mote
    // visibly travels onward before fading.
    for _ in 0..2 {
        let t = rng.gen_range(0.0..1.0);
        let pos = tender + dir * (t * len) + perp * rng.gen_range(-0.7..0.7);
        let v = dir * rng.gen_range(22.0..42.0)
              + perp * rng.gen_range(-9.0..9.0);
        let life = rng.gen_range(0.18..0.32);
        let scale = rng.gen_range(0.6..1.0);
        let rot = (-v.x).atan2(v.y);
        commands.spawn((
            Mesh2d(em.particle.clone()),
            MeshMaterial2d(mat.clone()),
            Transform {
                translation: Vec3::new(pos.x, pos.y, 5.5),
                rotation: Quat::from_rotation_z(rot),
                scale: Vec3::new(scale, scale, 1.0),
            },
            HitParticle { life, max_life: life, base_scale: scale },
            Velocity(v),
            RenderLayers::layer(PLAY_LAYER),
        ));
    }

    // Target sparkles — small upward-drifting motes around the unit
    // being healed. ~50% chance per frame so the bloom feels organic
    // / occasional rather than a steady stream.
    if rng.gen_bool(0.5) {
        let off = perp * rng.gen_range(-1.6..1.6)
                + dir * rng.gen_range(-1.6..1.6);
        let pos = target + off;
        let v = Vec2::new(
            rng.gen_range(-3.0..3.0),
            rng.gen_range(11.0..22.0),
        );
        let life = rng.gen_range(0.30..0.55);
        let scale = rng.gen_range(0.5..0.9);
        let rot = (-v.x).atan2(v.y);
        commands.spawn((
            Mesh2d(em.particle.clone()),
            MeshMaterial2d(mat.clone()),
            Transform {
                translation: Vec3::new(pos.x, pos.y, 5.5),
                rotation: Quat::from_rotation_z(rot),
                scale: Vec3::new(scale, scale, 1.0),
            },
            HitParticle { life, max_life: life, base_scale: scale },
            Velocity(v),
            RenderLayers::layer(PLAY_LAYER),
        ));
    }
}

/// Drive every `HealBeamEmitter`: pick a target of `heal_faction`,
/// accumulate fractional HP each frame, apply integer increments, and
/// spawn a brief beam visual to make the heal legible. Targeting
/// priority within the matching faction:
///   1. The `Friendly` main ship if it's hurt and in range (only meaningful
///      when `heal_faction == Friendly`; otherwise it's automatically
///      filtered out).
///   2. Otherwise, the most-hurt living `Ally` in range (skipping the
///      emitter itself).
/// If neither exists, the accumulator slowly decays so a fresh target
/// can't get a sudden burst-heal of stockpiled HP.
pub fn tender_heal_beam(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    game_mode: Res<GameMode>,
    mut tenders: Query<(Entity, &Transform, &mut HealBeamEmitter)>,
    mut friendly: Query<
        (Entity, &Transform, &Faction, &mut Health),
        (With<Friendly>, Without<Ally>, Without<HealBeamEmitter>),
    >,
    mut heal_targets: Query<
        (Entity, &Transform, &Ally, &Faction, &mut Health),
        (Without<Friendly>, Without<HealBeamEmitter>),
    >,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();

    let player_max_hp = if matches!(*game_mode, GameMode::Wave) {
        FRIENDLY_HP_WAVE
    } else {
        100
    };

    for (tender_e, tender_tf, mut emitter) in &mut tenders {
        let tender_pos = tender_tf.translation.truncate();
        let range_sq = emitter.range * emitter.range;
        let heal_faction = emitter.heal_faction;

        // (target_entity, target_pos, is_player)
        let mut chosen: Option<(Entity, Vec2, bool)> = None;

        // Player ship priority — only matches when the emitter heals
        // the Friendly side. For boss-side emitters this branch is
        // skipped via the faction check.
        if let Ok((fe, ftf, ffac, fh)) = friendly.single() {
            if ffac.0 == heal_faction
                && fh.0 > 0
                && fh.0 < player_max_hp
            {
                let fp = ftf.translation.truncate();
                if fp.distance_squared(tender_pos) < range_sq {
                    chosen = Some((fe, fp, true));
                }
            }
        }

        // Fallback — most-hurt unit-of-the-right-faction in range,
        // skipping the emitter itself.
        if chosen.is_none() {
            let mut best: Option<(Entity, Vec2, i32)> = None;
            for (ae, atf, ally, afac, h) in &heal_targets {
                if ae == tender_e { continue; }
                if afac.0 != heal_faction { continue; }
                if h.0 <= 0 { continue; }
                let max = ally.class.hp();
                let missing = max - h.0;
                if missing <= 0 { continue; }
                let ap = atf.translation.truncate();
                if ap.distance_squared(tender_pos) >= range_sq { continue; }
                if best.map_or(true, |(_, _, m)| missing > m) {
                    best = Some((ae, ap, missing));
                }
            }
            if let Some((e, p, _)) = best {
                chosen = Some((e, p, false));
            }
        }

        let Some((target_e, target_pos, is_player)) = chosen else {
            emitter.accumulator =
                (emitter.accumulator - dt * emitter.hp_per_sec).max(0.0);
            continue;
        };

        emitter.accumulator += dt * emitter.hp_per_sec;
        let heal_int = emitter.accumulator.floor() as i32;
        if heal_int > 0 {
            emitter.accumulator -= heal_int as f32;
            if is_player {
                if let Ok((_, _, _, mut h)) = friendly.single_mut() {
                    h.0 = (h.0 + heal_int).min(player_max_hp);
                }
            } else if let Ok((_, _, ally, _, mut h)) = heal_targets.get_mut(target_e) {
                let max = ally.class.hp();
                h.0 = (h.0 + heal_int).min(max);
            }
        }

        spawn_heal_visual(&mut commands, &em, &pm.heal, tender_pos, target_pos, &mut rng);
    }
}

// ---------- Planes ----------
//
// Planes are top-level entities (not parented to the carrier) so the
// state machine can move them freely. The `Plane.carrier` back-reference
// is used to:
//   - sit at the parked slot while idle (transform synced each frame),
//   - return to the slot when the patrol finishes, and
//   - despawn the plane if its carrier is gone.
//
// Per the design today, planes have no `Health` / `Ally` markers — they
// can't be shot at. Re-introduce those if you want shootable planes.

/// One launchable / landable plane attached to a Carrier.
#[derive(Component)]
pub struct Plane {
    pub carrier: Entity,
    /// 0 or 1 — which parked spot on the carrier deck.
    pub slot: u8,
    pub state: PlaneState,
    pub fire_cd: f32,
    /// Strafe runs left in the current sortie. Decremented at the end
    /// of each pass; 0 → return to carrier.
    pub runs_remaining: u8,
    /// Faction this plane strafes + damages. Inherited from the
    /// launching carrier's `target_faction`.
    pub target_faction: FactionKind,
}

/// Plane state machine. Transitions:
///   Idle ─(rest_timer 0)─▸ TakingOff ─(t≥1)─▸ Strafing
///   Strafing ─(pass complete; runs left)─▸ Banking ─(t≥1)─▸ Strafing
///   Strafing ─(pass complete; no runs)─▸ Returning
///   Returning ─(near slot)─▸ Landing ─(t≥1)─▸ Idle
pub enum PlaneState {
    /// Sitting on the carrier slot. `rest_timer` ticks down to 0,
    /// then the plane launches.
    Idle { rest_timer: f32 },
    /// Lift-off animation: drifting forward off the bow with a quick
    /// scale-up to imply altitude. `t` is 0..1 progress.
    TakingOff { t: f32 },
    /// Active strafe pass. Plane flies toward `target`, firing forward
    /// when on-axis. Pass ends when `target` is close or behind.
    Strafing { target: Vec2 },
    /// Pull-up between strafes: plane flies forward, turns gently
    /// toward the next engagement, no firing. Without this an inter-
    /// pass transition that picks the same nearby enemy would end the
    /// new pass on the very next frame (`dist < STRAFE_END_DIST`),
    /// burning a sortie's worth of `runs_remaining` in one frame.
    Banking { t: f32 },
    /// Heading back to the parked slot's world position.
    Returning,
    /// Touch-down animation: scale-down + lerp into the slot.
    Landing { t: f32 },
}

/// Plane tuning — gameplay numbers in one place.
const PLANE_SPEED:               f32 = 38.0;
const PLANE_TURN_RATE:           f32 = 2.6;
const PLANE_FIRE_RATE:           f32 = 4.0;
const PLANE_FIRE_DAMAGE:         i32 = 1;
const PLANE_BULLET_SPEED:        f32 = 80.0;
const PLANE_BULLET_RANGE:        f32 = 60.0;
const PLANE_TAKEOFF_DUR:         f32 = 0.7;
const PLANE_LANDING_DUR:         f32 = 1.0;
const PLANE_REST_BASE:           f32 = 2.0;
/// Pull-up duration between strafes — long enough to fly clear of the
/// just-hit enemy (≈ 26 units at PLANE_SPEED) before re-evaluating a
/// new target.
const PLANE_BANKING_DUR:         f32 = 0.7;
/// Plane considers a strafe pass complete when within this distance of
/// the target (or once the target is behind the plane).
const PLANE_STRAFE_END_DIST:     f32 = 12.0;
/// Distance from the carrier slot at which the plane switches from
/// `Returning` to `Landing`.
const PLANE_LAND_TRIGGER_DIST:   f32 = 14.0;
/// Aim cone: only fire when the target is within this many radians
/// of forward. ~25°.
const PLANE_AIM_CONE:            f32 = 0.45;
/// Idle scale = "on deck"; flying scale = full size.
const PLANE_DECK_SCALE:          f32 = 0.6;

/// World-space position of the carrier's parked slot for `slot`.
/// 6 slots laid out in three pairs along the flight deck:
///   0/1: aft pair, 2/3: midship, 4/5: forward pair.
/// Even = port (left), odd = stbd (right) — matches the spawn loop's
/// `slot % 2` mod implicitly via the lookup.
fn carrier_slot_world(carrier_pos: Vec2, carrier_heading: f32, slot: u8) -> Vec2 {
    let local = match slot {
        0 => Vec2::new(-1.8, -7.0), // aft port
        1 => Vec2::new( 1.8, -7.0), // aft stbd
        2 => Vec2::new(-1.8,  0.0), // mid port
        3 => Vec2::new( 1.8,  0.0), // mid stbd
        4 => Vec2::new(-1.8,  7.0), // bow port
        _ => Vec2::new( 1.8,  7.0), // bow stbd
    };
    let forward = Vec2::new(-carrier_heading.sin(), carrier_heading.cos());
    let right   = Vec2::new( carrier_heading.cos(), carrier_heading.sin());
    carrier_pos + right * local.x + forward * local.y
}

fn nearest_position(from: Vec2, positions: &[Vec2]) -> Option<Vec2> {
    positions.iter().copied().min_by(|a, b| {
        let da = from.distance_squared(*a);
        let db = from.distance_squared(*b);
        da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
    })
}

/// Spawn a plane parked at the carrier slot. Mesh: a tall thin
/// fuselage (capsule) with a wider rectangle for the wings, child of
/// the fuselage so they rotate together.
pub fn spawn_plane(
    commands: &mut Commands,
    pm: &PaletteMaterials,
    meshes: &mut Assets<Mesh>,
    carrier: Entity,
    slot: u8,
    init_pos: Vec2,
    init_heading: f32,
    target_faction: FactionKind,
) {
    let fuselage_mesh = meshes.add(Capsule2d::new(0.5, 2.5));   // ~1 wide × 3.5 long
    let wings_mesh    = meshes.add(Rectangle::new(3.0, 0.8));   // 3 wide × 0.8 long

    let plane_mat = pm.plane_hull.clone();
    // Stagger initial rest across the wing so they don't all lift off
    // in lockstep. The slot index gives a coarse aft-to-bow sequence
    // (0.6 s per slot); a small per-plane jitter on top makes the
    // launches feel less mechanical and keeps the wing visibly out of
    // sync after the first sortie.
    let mut rng = rand::thread_rng();
    let rest_jitter = rng.gen_range(-0.25..0.45);
    let plane = commands.spawn((
        Mesh2d(fuselage_mesh),
        MeshMaterial2d(plane_mat.clone()),
        Transform::from_xyz(init_pos.x, init_pos.y, 2.0)
            .with_rotation(Quat::from_rotation_z(init_heading))
            .with_scale(Vec3::splat(PLANE_DECK_SCALE)),
        Plane {
            carrier,
            slot,
            state: PlaneState::Idle {
                rest_timer: (PLANE_REST_BASE + slot as f32 * 0.6 + rest_jitter)
                    .max(0.4),
            },
            fire_cd: 0.0,
            runs_remaining: 0,
            target_faction,
        },
        Heading(init_heading),
        RenderLayers::layer(PLAY_LAYER),
    )).id();

    let wings = commands.spawn((
        Mesh2d(wings_mesh),
        MeshMaterial2d(plane_mat),
        // Slightly forward of fuselage center so the silhouette
        // reads as "high-wing prop" rather than mid-wing.
        Transform::from_xyz(0.0, 0.4, 0.05),
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    commands.entity(wings).insert(ChildOf(plane));
}

/// Spawn the twin forward-firing bullets from a strafing plane. Two
/// offset spawn points (one per wing-mounted gun) emit synchronized
/// shots traveling along the plane's forward vector. `bullet_faction`
/// is the OWN side (opposite of what the plane targets).
fn spawn_plane_bullets(
    commands: &mut Commands,
    pm: &PaletteMaterials,
    em: &EffectMeshes,
    pos: Vec2,
    forward: Vec2,
    heading: f32,
    bullet_faction: FactionKind,
    source: Option<crate::bullet::DamageSource>,
) {
    let perp = Vec2::new(-forward.y, forward.x);
    for side in [-1.0_f32, 1.0] {
        let bullet_pos = pos + forward * 1.8 + perp * (side * 0.9);
        // Use the dedicated small `bullet_plane_*` meshes — the
        // friendly-bullet meshes are sized for the player's main
        // batteries and read as too heavy on a fighter MG.
        let bullet = commands.spawn((
            Mesh2d(em.bullet_plane_outer.clone()),
            MeshMaterial2d(pm.bullet_friendly_outer.clone()),
            Transform::from_xyz(bullet_pos.x, bullet_pos.y, 4.0)
                .with_rotation(Quat::from_rotation_z(heading)),
            Bullet {
                faction: bullet_faction,
                damage: PLANE_FIRE_DAMAGE,
                remaining: PLANE_BULLET_RANGE,
                weapon: WeaponType::Standard,
                source,
                runes: [None; 3],
            },
            Velocity(forward * PLANE_BULLET_SPEED),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        let inner = commands.spawn((
            Mesh2d(em.bullet_plane_inner.clone()),
            MeshMaterial2d(pm.bullet_friendly.clone()),
            Transform::from_xyz(0.0, 0.0, 0.05),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(inner).insert(ChildOf(bullet));
    }
}

/// Drive every plane through its state machine each frame. Movement is
/// applied directly to `Transform` (not via `Velocity`), so planes are
/// free of the `apply_velocity` integrator and don't need to handle
/// frost / status effects.
///
/// Faction-aware: each plane carries its own `target_faction` (inherited
/// from the carrier at launch), and the strafe-target query filters by
/// that faction. Carrier-position lookup is also faction-agnostic — it
/// looks up the entity by id, no marker filter — so a future boss
/// carrier reuses this system unchanged.
pub fn plane_ai(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    candidates: Query<(&Transform, &Faction), Without<Plane>>,
    carriers: Query<&Transform, Without<Plane>>,
    mut planes: Query<(Entity, &mut Transform, &mut Heading, &mut Plane)>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();

    for (entity, mut tf, mut heading, mut plane) in &mut planes {
        let Ok(ctf) = carriers.get(plane.carrier) else {
            // Carrier sunk — clean up the orphan plane.
            commands.entity(entity).despawn();
            continue;
        };
        // Per-plane target snapshot, filtered by THIS plane's faction.
        // Built inside the loop so a mixed fleet (friendly + boss
        // carriers) still works — each plane sees only its own quarry.
        let target_positions: Vec<Vec2> = candidates
            .iter()
            .filter(|(_, f)| f.0 == plane.target_faction)
            .map(|(t, _)| t.translation.truncate())
            .collect();
        let cpos = ctf.translation.truncate();
        let cheading = ctf.rotation.to_euler(EulerRot::XYZ).2;
        let slot_pos = carrier_slot_world(cpos, cheading, plane.slot);

        // Take a copy of the current state to mutate freely without
        // borrow conflicts.
        let mut next_state: Option<PlaneState> = None;

        match plane.state {
            PlaneState::Idle { mut rest_timer } => {
                // Park: snap to slot, mirror carrier heading, scale down.
                tf.translation.x = slot_pos.x;
                tf.translation.y = slot_pos.y;
                heading.0 = cheading;
                tf.rotation = Quat::from_rotation_z(cheading);
                tf.scale = Vec3::splat(PLANE_DECK_SCALE);

                rest_timer -= dt;
                if rest_timer <= 0.0 {
                    plane.runs_remaining = rng.gen_range(2..=3) as u8;
                    next_state = Some(PlaneState::TakingOff { t: 0.0 });
                } else {
                    plane.state = PlaneState::Idle { rest_timer };
                }
            }
            PlaneState::TakingOff { mut t } => {
                t = (t + dt / PLANE_TAKEOFF_DUR).min(1.0);
                let cforward = Vec2::new(-cheading.sin(), cheading.cos());
                // Drift forward off the deck and scale up to "in flight".
                let pos = slot_pos + cforward * (t * 12.0);
                tf.translation.x = pos.x;
                tf.translation.y = pos.y;
                heading.0 = cheading;
                tf.rotation = Quat::from_rotation_z(cheading);
                let scale = PLANE_DECK_SCALE + t * (1.0 - PLANE_DECK_SCALE);
                tf.scale = Vec3::splat(scale);

                if t >= 1.0 {
                    let target = nearest_position(pos, &target_positions)
                        .unwrap_or(pos + cforward * 60.0);
                    next_state = Some(PlaneState::Strafing { target });
                } else {
                    plane.state = PlaneState::TakingOff { t };
                }
            }
            PlaneState::Strafing { target } => {
                let pos = tf.translation.truncate();
                let to = target - pos;
                if to.length_squared() > 0.01 {
                    let desired = (-to.x).atan2(to.y);
                    heading.0 = approach_angle(heading.0, desired, PLANE_TURN_RATE * dt);
                }
                let forward = Vec2::new(-heading.0.sin(), heading.0.cos());
                let new_pos = pos + forward * PLANE_SPEED * dt;
                tf.translation.x = new_pos.x;
                tf.translation.y = new_pos.y;
                tf.rotation = Quat::from_rotation_z(heading.0);
                tf.scale = Vec3::ONE;

                // Fire when the target's roughly in front.
                plane.fire_cd -= dt;
                let aim_diff = forward.angle_to(to.normalize_or_zero()).abs();
                if aim_diff < PLANE_AIM_CONE && plane.fire_cd <= 0.0 {
                    plane.fire_cd = 1.0 / PLANE_FIRE_RATE;
                    // Bullet faction = OWN faction = opposite of target.
                    spawn_plane_bullets(
                        &mut commands, &pm, &em, new_pos, forward, heading.0,
                        plane.target_faction.opposite(),
                        // Carrier-launched planes credit kills to the
                        // CARRIER row in the damage panel.
                        Some(crate::bullet::DamageSource::Ally(ShipClass::Carrier)),
                    );
                }

                // Pass ends when the target is close *or* behind.
                let dist = to.length();
                let passed = forward.dot(to) < 0.0;
                if dist < PLANE_STRAFE_END_DIST || passed {
                    plane.runs_remaining = plane.runs_remaining.saturating_sub(1);
                    if plane.runs_remaining > 0 {
                        // Bank away first — picking a fresh target
                        // here without flying clear would re-trigger
                        // the pass-end check on the very next frame
                        // (the new nearest enemy is often the just-
                        // hit one, still within STRAFE_END_DIST).
                        next_state = Some(PlaneState::Banking { t: 0.0 });
                    } else {
                        next_state = Some(PlaneState::Returning);
                    }
                }
            }
            PlaneState::Banking { mut t } => {
                t = (t + dt / PLANE_BANKING_DUR).min(1.0);
                let pos = tf.translation.truncate();

                // Gentle turn toward the target centroid so the next
                // strafe lines up cleanly without committing to a
                // specific enemy yet. If no targets remain, just
                // maintain current heading.
                if !target_positions.is_empty() {
                    let n = target_positions.len() as f32;
                    let centroid =
                        target_positions.iter().copied().sum::<Vec2>() / n;
                    let to = centroid - pos;
                    if to.length_squared() > 0.01 {
                        let desired = (-to.x).atan2(to.y);
                        // Softer turn than full strafe — a banking
                        // arc, not a snap-around.
                        heading.0 = approach_angle(
                            heading.0, desired, PLANE_TURN_RATE * dt * 0.7,
                        );
                    }
                }
                let forward = Vec2::new(-heading.0.sin(), heading.0.cos());
                let new_pos = pos + forward * PLANE_SPEED * dt;
                tf.translation.x = new_pos.x;
                tf.translation.y = new_pos.y;
                tf.rotation = Quat::from_rotation_z(heading.0);
                tf.scale = Vec3::ONE;

                if t >= 1.0 {
                    let new_target = nearest_position(new_pos, &target_positions)
                        .unwrap_or(new_pos + forward * 80.0);
                    next_state = Some(PlaneState::Strafing { target: new_target });
                } else {
                    plane.state = PlaneState::Banking { t };
                }
            }
            PlaneState::Returning => {
                let pos = tf.translation.truncate();
                let to = slot_pos - pos;
                if to.length_squared() > 0.01 {
                    let desired = (-to.x).atan2(to.y);
                    heading.0 = approach_angle(heading.0, desired, PLANE_TURN_RATE * dt);
                }
                let forward = Vec2::new(-heading.0.sin(), heading.0.cos());
                let new_pos = pos + forward * PLANE_SPEED * dt;
                tf.translation.x = new_pos.x;
                tf.translation.y = new_pos.y;
                tf.rotation = Quat::from_rotation_z(heading.0);
                tf.scale = Vec3::ONE;

                if to.length() < PLANE_LAND_TRIGGER_DIST {
                    next_state = Some(PlaneState::Landing { t: 0.0 });
                }
            }
            PlaneState::Landing { mut t } => {
                t = (t + dt / PLANE_LANDING_DUR).min(1.0);
                let pos = tf.translation.truncate();
                // Smoothly converge to slot — rate shrinks as t→1
                // so the touch-down feels like it settles, not snaps.
                let blend = (dt * 4.0).min(0.5);
                let new_pos = pos.lerp(slot_pos, blend);
                tf.translation.x = new_pos.x;
                tf.translation.y = new_pos.y;
                heading.0 = approach_angle(heading.0, cheading, PLANE_TURN_RATE * dt);
                tf.rotation = Quat::from_rotation_z(heading.0);
                let scale = 1.0 - t * (1.0 - PLANE_DECK_SCALE);
                tf.scale = Vec3::splat(scale);

                if t >= 1.0 {
                    plane.fire_cd = 0.0;
                    next_state = Some(PlaneState::Idle {
                        rest_timer: PLANE_REST_BASE
                            + rng.gen_range(0.0..2.0),
                    });
                } else {
                    plane.state = PlaneState::Landing { t };
                }
            }
        }

        if let Some(s) = next_state {
            plane.state = s;
        }
    }
}

// ---------- Oil tankers ----------
//
// Two-phase loop driven by `OilTankerCycle`:
//   1. Spraying — every `OIL_DROP_INTERVAL`s, a *fan* of fresh
//      `OilSlick`s sprays out the tanker's stern, fanning across
//      a cone behind the hull with per-slick scale + Z-rotation
//      jitter so the laid-down area reads as one continuous pool
//      rather than a sparse breadcrumb trail. Pools persist in
//      world space (not parented) for their `OIL_SLICK_LIFETIME`.
//   2. Burning — every existing slick that targets the tanker's
//      enemy faction is tagged with `OilOnFire` for
//      `OIL_BURN_DURATION`. While on fire, the slick ticks AOE
//      damage every `OIL_BURN_TICK` seconds to anyone of the
//      target faction inside `OIL_BURN_RADIUS`.
// Cooldown is a brief breath before the loop restarts.
//
// Faction-agnostic: the tanker's `target_faction` (cached at
// spawn) flows onto each slick, so an ally-side tanker burns
// enemies and a boss-side tanker burns allies + the player.

const OIL_SPRAY_DURATION:   f32 = 3.0;
const OIL_BURN_DURATION:    f32 = 3.0;
const OIL_COOLDOWN:         f32 = 0.5;
const OIL_DROP_INTERVAL:    f32 = 0.25;
const OIL_SLICK_LIFETIME:   f32 = 8.0;
const OIL_BURN_RADIUS:      f32 = 8.0;
const OIL_BURN_DAMAGE:      i32 = 1;
const OIL_BURN_TICK:        f32 = 0.3;
/// Base radius of one oil-pool mesh in world units (before per-spawn
/// scale jitter). Each spray tick lays a *fan* of slicks behind the
/// stern; with `OIL_FAN_COUNT` slicks per tick spread across
/// `OIL_FAN_HALF_ANGLE` and the tanker moving forward, the laid-down
/// area reads as one continuous dark pool. Bumped 2.2 → 3.0 so the
/// individual stamps are big enough to cover the wider cone without
/// gaps.
const OIL_SLICK_RADIUS:     f32 = 3.0;
/// Slicks sprayed per `OIL_DROP_INTERVAL` tick. Bumped to 9 so the
/// wider cone + greater stern depth still reads as one continuous
/// pool — the per-tick count scales with area so seams stay closed.
const OIL_FAN_COUNT:        u32 = 9;
/// Half-angle of the spray cone behind the tanker (radians). 0.85 ≈
/// ±49°, giving a wide ≈100° spray fan that visibly broadens behind
/// the stern.
const OIL_FAN_HALF_ANGLE:   f32 = 0.85;
/// Stern offset range — each slick spawns this many units behind the
/// tanker's center, randomized so successive ticks don't lay perfectly
/// concentric arcs. Range widened so the swath has visible *depth* as
/// well as width.
const OIL_FAN_DIST_MIN:     f32 = 7.0;
const OIL_FAN_DIST_MAX:     f32 = 14.0;
/// How long (seconds) a freshly-laid slick takes to spread from its
/// initial small footprint up to its full `target_radius`. The visual
/// scale eases on an out-quadratic curve so the pool *settles* rather
/// than snapping. Damage radius tracks the visual scale tightly so
/// a still-spreading slick can't pre-emptively burn off-disc victims.
const OIL_SPREAD_DURATION:  f32 = 1.5;
/// Starting visual scale (as a fraction of target radius) the moment a
/// slick is laid down. 0.3 ≈ a small puddle that visibly grows out to
/// the full pool over `OIL_SPREAD_DURATION`.
const OIL_SPREAD_START_SCALE: f32 = 0.3;

/// Drive every tanker's cycle. State machine:
///   Spraying  → drops a slick every `OIL_DROP_INTERVAL`. On expiry,
///               flips to Burning AND tags every existing slick of
///               this tanker's `target_faction` with `OilOnFire`.
///   Burning   → no new slicks; the slicks are ticking damage on
///               their own (see `oil_slick_burn_tick`). On expiry,
///               flips to Cooldown.
///   Cooldown  → idle breath. On expiry, flips back to Spraying.
///
/// Slicks are ignited *en masse* by faction-match rather than by
/// owner-entity tracking. That means two friendly tankers crossing
/// paths will set each other's pools on fire when either ignites —
/// fine, even nice: visually it reads as a pooled hazard belt
/// catching all at once.
pub fn oil_tanker_cycle(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    mut tankers: Query<(&Transform, &Heading, &mut OilTankerCycle)>,
    mut slicks: Query<(Entity, &OilSlick), Without<OilOnFire>>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();

    for (tf, heading, mut cycle) in &mut tankers {
        cycle.timer -= dt;

        match cycle.phase {
            OilCyclePhase::Spraying => {
                // Spray a fan of slicks on the cooldown beat. Each
                // tick lays `OIL_FAN_COUNT` overlapping pools across a
                // cone behind the stern; together with per-slick
                // scale + rotation jitter and the tanker's forward
                // motion, the union reads as one continuous slick
                // rather than a breadcrumb trail with water seams.
                cycle.drop_cd -= dt;
                if cycle.drop_cd <= 0.0 {
                    cycle.drop_cd = OIL_DROP_INTERVAL;

                    let pos = tf.translation.truncate();
                    let h = heading.0;
                    let forward = Vec2::new(-h.sin(), h.cos());
                    // -forward points astern; the fan is centered here.
                    let astern = -forward;

                    for i in 0..OIL_FAN_COUNT {
                        // Distribute evenly across the cone with a
                        // small angular jitter (±0.08 rad) so the
                        // stamps don't visibly tile.
                        let t = if OIL_FAN_COUNT == 1 {
                            0.0
                        } else {
                            i as f32 / (OIL_FAN_COUNT - 1) as f32
                        };
                        let base_angle =
                            -OIL_FAN_HALF_ANGLE + t * (2.0 * OIL_FAN_HALF_ANGLE);
                        let angle = base_angle + rng.gen_range(-0.08..0.08);

                        // Rotate `astern` by `angle` for the spray
                        // direction.
                        let (s, c) = angle.sin_cos();
                        let dir = Vec2::new(
                            astern.x * c - astern.y * s,
                            astern.x * s + astern.y * c,
                        );
                        let dist = rng.gen_range(OIL_FAN_DIST_MIN..OIL_FAN_DIST_MAX);
                        let p = pos + dir * dist;

                        let scale_jitter = rng.gen_range(0.7..1.4);
                        let z_rot = rng.gen_range(0.0..std::f32::consts::TAU);

                        spawn_oil_slick(
                            &mut commands, &em, &pm, p,
                            cycle.target_faction,
                            scale_jitter, z_rot,
                        );
                    }
                }

                if cycle.timer <= 0.0 {
                    // Ignite every un-burning slick whose target
                    // faction matches this tanker's. Faction-match
                    // (vs. owner-entity tracking) is the simplest
                    // way to handle multi-tanker overlaps + the
                    // tanker dying mid-cycle.
                    for (slick_e, slick) in &mut slicks {
                        if slick.target_faction != cycle.target_faction {
                            continue;
                        }
                        commands.entity(slick_e).insert(OilOnFire {
                            remaining: OIL_BURN_DURATION,
                            tick_cd: 0.0,
                        });
                    }
                    cycle.phase = OilCyclePhase::Burning;
                    cycle.timer = OIL_BURN_DURATION;
                }
            }
            OilCyclePhase::Burning => {
                if cycle.timer <= 0.0 {
                    cycle.phase = OilCyclePhase::Cooldown;
                    cycle.timer = OIL_COOLDOWN;
                }
            }
            OilCyclePhase::Cooldown => {
                if cycle.timer <= 0.0 {
                    cycle.phase = OilCyclePhase::Spraying;
                    cycle.timer = OIL_SPRAY_DURATION;
                    // First drop is immediate so the new spray
                    // phase starts visibly.
                    cycle.drop_cd = 0.0;
                }
            }
        }
    }
}

/// Spawn one oil pool at `pos`. Reuses the `particle` mesh as a
/// stand-in disc so we don't allocate a fresh handle per slick.
/// Z = 0.5 → behind ships (ship hulls sit at z=1.0), so vessels
/// render over the slick.
///
/// `scale_jitter` is multiplied into `OIL_SLICK_RADIUS` so the fan
/// doesn't read as identical stamps; `z_rot` randomizes the disc's
/// rotation so per-slick mesh imperfections (the unit circle is
/// faceted) don't visibly tile across neighbors.
fn spawn_oil_slick(
    commands: &mut Commands,
    em: &EffectMeshes,
    pm: &PaletteMaterials,
    pos: Vec2,
    target_faction: FactionKind,
    scale_jitter: f32,
    z_rot: f32,
) {
    // The mesh handle is shared across every slick — we drive size
    // via `Transform::scale` so spawning is allocation-free. Initial
    // scale is `target_radius * OIL_SPREAD_START_SCALE`; a per-frame
    // tick (`oil_slick_grow_tick`) eases it up to `target_radius`.
    let target_radius = OIL_SLICK_RADIUS * scale_jitter;
    let start_r = target_radius * OIL_SPREAD_START_SCALE;
    commands.spawn((
        Mesh2d(em.particle.clone()),
        MeshMaterial2d(pm.oil_slick.clone()),
        Transform {
            translation: Vec3::new(pos.x, pos.y, 0.5),
            rotation: Quat::from_rotation_z(z_rot),
            scale: Vec3::new(start_r, start_r, 1.0),
        },
        OilSlick {
            target_faction,
            lifetime: OIL_SLICK_LIFETIME,
            age: 0.0,
            target_radius,
        },
        RenderLayers::layer(PLAY_LAYER),
    ));
}

/// Per-frame grow-in animation for every `OilSlick`. Each frame we
/// advance `age`, ease an out-quadratic factor `start_scale → 1.0`
/// over `OIL_SPREAD_DURATION`, multiply by `target_radius`, and
/// (if the slick is on fire) layer a tiny sinusoidal flame-base
/// shimmer on top. The result is written to `Transform::scale`.
///
/// Damage radius in `oil_slick_burn_tick` is derived from this same
/// scale, so a still-spreading slick has a smaller damage footprint.
pub fn oil_slick_grow_tick(
    time: Res<Time>,
    mut slicks: Query<(&mut Transform, &mut OilSlick, Option<&OilOnFire>)>,
) {
    let dt = time.delta_secs();
    let t_global = time.elapsed_secs();
    for (mut tf, mut slick, fire_opt) in &mut slicks {
        slick.age += dt;
        // Ease-out quadratic — `1 - (1 - t)^2` — fast at first, settles
        // gently into the target. Reads as a fluid spread rather than a
        // linear march.
        let raw = (slick.age / OIL_SPREAD_DURATION).clamp(0.0, 1.0);
        let eased = 1.0 - (1.0 - raw).powi(2);
        let factor = OIL_SPREAD_START_SCALE
            + (1.0 - OIL_SPREAD_START_SCALE) * eased;
        // Burning slicks shimmer at their base — a small sinusoidal
        // breathing on top of the grown-in factor so the flame pool
        // pulses like a live fire rather than sitting static.
        let pulse = if fire_opt.is_some() {
            1.0 + 0.05 * (t_global * 8.0).sin()
        } else {
            1.0
        };
        let r = slick.target_radius * factor * pulse;
        tf.scale = Vec3::new(r, r, 1.0);
    }
}

/// Tick every `OilSlick`'s lifetime + (if on fire) damage cadence.
/// Splits cleanly into two passes:
///   1. Lifetime decrement; despawn expired slicks.
///   2. For each `OilOnFire` slick: count down `remaining`, swap the
///      material to `pm.fire` so it reads as a flame pool, and on
///      every `OIL_BURN_TICK` apply AOE damage to every unit of
///      `target_faction` inside `OIL_BURN_RADIUS`.
///
/// Also drives a global flame-color oscillation on the shared
/// `pm.fire` `ColorMaterial`, so every burning slick (and any other
/// FX sharing that handle) shifts between deep orange and bright
/// yellow at ~6 rad/s for a candle-flame look.
pub fn oil_slick_burn_tick(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    _materials: Res<Assets<ColorMaterial>>,
    mut victims: Query<(Entity, &Transform, &Faction, &mut Health, &mut HitFx)>,
    mut slicks: Query<(
        Entity,
        &Transform,
        &mut OilSlick,
        Option<&mut OilOnFire>,
        &mut MeshMaterial2d<ColorMaterial>,
    )>,
    mut stats: ResMut<crate::ui::DamageStats>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();

    // Snapshot once so the inner per-slick AOE pass doesn't conflict
    // with the mutable victims query.
    let victim_snap: Vec<(Entity, Vec2, FactionKind)> = victims
        .iter()
        .map(|(e, t, f, _, _)| (e, t.translation.truncate(), f.0))
        .collect();

    for (slick_e, slick_tf, mut slick, fire_opt, mut mat) in &mut slicks {
        slick.lifetime -= dt;
        if slick.lifetime <= 0.0 {
            commands.entity(slick_e).despawn();
            continue;
        }

        let Some(mut fire) = fire_opt else { continue; };

        // First frame on fire — swap the material to the flame
        // color. Cheap to do every frame (just handle compare),
        // but only re-set if it's actually still the dark oil
        // handle so we don't churn the asset id.
        if mat.0.id() != pm.fire.id() {
            mat.0 = pm.fire.clone();
        }

        fire.remaining -= dt;
        if fire.remaining <= 0.0 {
            commands.entity(slick_e).despawn();
            continue;
        }

        fire.tick_cd -= dt;
        if fire.tick_cd > 0.0 { continue; }
        fire.tick_cd = OIL_BURN_TICK;

        // AOE damage tick — every faction-matched unit inside the
        // *current effective* radius takes the bite, credited to
        // OILER. Effective radius scales with the slick's live
        // Transform::scale so a still-spreading slick can't burn
        // off-disc victims.
        let sp = slick_tf.translation.truncate();
        // `Transform::scale.x` carries `target_radius * grow_factor *
        // pulse`; OIL_SLICK_RADIUS is the un-jittered base radius,
        // so the visual_factor below normalizes back to a
        // dimensionless "how-grown is this stamp" multiplier we can
        // apply to the burn radius. Min 0.05 so we never divide-by-
        // ~zero if a slick spawns at a tiny target_radius.
        let visual_factor = (slick_tf.scale.x / OIL_SLICK_RADIUS).max(0.05);
        let eff_radius = OIL_BURN_RADIUS * visual_factor;
        let r2 = eff_radius * eff_radius;
        for &(e, ep, f) in &victim_snap {
            if f != slick.target_faction { continue; }
            if ep.distance_squared(sp) >= r2 { continue; }
            if let Ok((_, _, _, mut h, mut fx)) = victims.get_mut(e) {
                let dealt = crate::bullet::apply_damage(&mut h, &mut fx, OIL_BURN_DAMAGE);
                crate::bullet::credit_damage(
                    &mut stats,
                    Some(crate::bullet::DamageSource::Ally(ShipClass::OilTanker)),
                    dealt,
                );
            }
        }

        // Flame burst at the slick — 2-color in line with the rest of
        // the game's particle effects (mine bursts, hit sparks, etc.):
        // each mote picks one of two existing material handles
        // (`pm.fire` bright / `pm.mine_inner` deep) rather than mixing
        // a continuous gradient. Velocity / life variance kept since
        // those add motion without breaking the palette discipline.
        // ~1-in-6 ember stays for shape variety; uses the same two
        // materials, just a longer-life / faster-Y profile.
        let vis_r = slick_tf.scale.x.max(1.0);
        let mote_count = rng.gen_range(4..=6);
        for _ in 0..mote_count {
            let off = Vec2::new(
                rng.gen_range(-vis_r..vis_r),
                rng.gen_range(-vis_r..vis_r),
            );
            // 2/3 bright fire, 1/3 deep red — mirrors the inner/outer
            // 2-tone the mine burst uses.
            let mat_handle = if rng.gen_range(0..3) == 0 {
                pm.mine_inner.clone()
            } else {
                pm.fire.clone()
            };
            let is_ember = rng.gen_range(0..6) == 0;
            let (life, vy_range, scale_range) = if is_ember {
                (rng.gen_range(0.55..0.85), 36.0..52.0, 0.6..1.0)
            } else {
                (rng.gen_range(0.30..0.70), 18.0..32.0, 0.5..0.9)
            };
            let vel = Vec2::new(
                rng.gen_range(-6.0..6.0),
                rng.gen_range(vy_range),
            );
            let scale = rng.gen_range(scale_range);
            commands.spawn((
                Mesh2d(em.particle.clone()),
                MeshMaterial2d(mat_handle),
                Transform {
                    translation: Vec3::new(sp.x + off.x, sp.y + off.y, 5.5),
                    scale: Vec3::new(scale, scale, 1.0),
                    ..default()
                },
                HitParticle { life, max_life: life, base_scale: scale },
                Velocity(vel),
                RenderLayers::layer(PLAY_LAYER),
            ));
        }
    }
}

// ---------- Viking ram ----------
//
// Viking longships have no projectiles — their entire damage output
// is delivered by ramming opposite-faction units. The pattern mirrors
// the player ship's `friendly_ram_damage`: per-victim grace prevents
// per-frame multi-tap, and a screen-shake kick punches the impact.

/// Per-Viking ram-charge state. `current` is the current forward speed
/// while charging; it ramps from `VIKING_RAM_BASE_SPEED` up to
/// `VIKING_RAM_MAX_SPEED` over `VIKING_RAM_RAMP_TIME` seconds while a
/// target is held, and resets when the target is lost / the ram lands /
/// the charge is interrupted. Acts as the unique marker `viking_ram_damage`
/// queries on, replacing the old zero-sized `VikingRamCharge` tag.
#[derive(Component)]
pub struct VikingRamCharge {
    pub current_speed: f32,
}

impl Default for VikingRamCharge {
    fn default() -> Self {
        Self { current_speed: VIKING_RAM_BASE_SPEED }
    }
}

/// Per-victim cooldown so a Viking pressed against an enemy doesn't
/// chunk it every frame. Cleared after the grace window expires.
#[derive(Component)]
pub struct VikingRamGrace {
    pub remaining: f32,
}

/// Base damage applied on a low-speed contact ram. Scaled up by the
/// current charge speed in `viking_ram_damage` (up to `VIKING_RAM_DAMAGE_CAP`).
const VIKING_RAM_DAMAGE: i32 = 35;
/// Cap on the charge-speed-scaled damage. Even a perfectly-timed
/// full-speed ram can't exceed this.
const VIKING_RAM_DAMAGE_CAP: i32 = 50;
const VIKING_RAM_GRACE: f32 = 0.55;
const VIKING_RAM_TRAUMA: f32 = 0.5;

/// Charge ramp parameters. The longship starts every charge slow so
/// the player has a window to react, but builds up to a speed that
/// outpaces every other ship in the fleet — at full charge the only
/// safe play is to dodge sideways and let the Viking overshoot.
pub const VIKING_RAM_BASE_SPEED: f32 = 18.0;
/// Cap on charge speed — 2.5× the player's 30 u/s baseline so the
/// Viking is the fastest thing on the field without overshooting
/// the play boundary every time it commits.
pub const VIKING_RAM_MAX_SPEED: f32  = 75.0;
/// Seconds to ramp from base to max while a target is held.
pub const VIKING_RAM_RAMP_TIME: f32  = 1.0;
/// Turn rate at full charge — slower than the base 1.6 rad/s so a
/// missed charge has to circle back, but generous enough to
/// re-acquire before the Viking flies all the way to the edge.
pub const VIKING_RAM_TURN_AT_MAX: f32 = 0.5;
/// Speed bleed (units/sec) while not actively charging (target lost
/// OR mid-turn). Continuous bleed keeps the bull-charge mechanic
/// readable: the Viking only gains speed once it's lined up.
pub const VIKING_RAM_DECAY_PER_SEC: f32 = 60.0;
/// Heading-vs-target tolerance for the bull-charge gate. The Viking
/// only accelerates while its forward vector is within this many
/// radians of the line to its target. ~17° window.
pub const VIKING_RAM_ALIGN_THRESHOLD: f32 = 0.30;

/// Snapshot every Viking's position + the faction it wants to ram,
/// then iterate every faction-bearing entity and apply ram damage on
/// contact. Faction-agnostic — friendly Vikings ram enemies, boss-side
/// Vikings ram allies + the player ship.
pub fn viking_ram_damage(
    time: Res<Time>,
    mut commands: Commands,
    mut shake: ResMut<crate::modes::ScreenShake>,
    mut vikings: Query<(Entity, &Transform, &Faction, &Ally, &mut VikingRamCharge)>,
    mut victims: Query<(
        Entity,
        &Transform,
        &Faction,
        &mut Health,
        &mut HitFx,
        Option<&mut VikingRamGrace>,
    )>,
    mut stats: ResMut<crate::ui::DamageStats>,
) {
    let dt = time.delta_secs();
    // Snapshot first so the inner mut-victims pass doesn't overlap
    // the read-only viking pass on the same components. Carries the
    // Viking entity + its current charge speed so we can both reset
    // on contact and scale damage by impact speed.
    let viking_snap: Vec<(Entity, Vec2, FactionKind, f32)> = vikings
        .iter()
        .map(|(e, tf, fac, _, charge)| {
            (e, tf.translation.truncate(), fac.0.opposite(), charge.current_speed)
        })
        .collect();
    if viking_snap.is_empty() { return; }

    // Track which Vikings landed a hit this frame so we can reset
    // their charge speed back to base after the loop.
    let mut hit_this_frame: Vec<Entity> = Vec::new();

    for (e, vtf, vfac, mut h, mut fx, grace) in &mut victims {
        if let Some(mut g) = grace {
            g.remaining -= dt;
            if g.remaining > 0.0 { continue; }
            commands.entity(e).remove::<VikingRamGrace>();
        }
        if h.0 <= 0 { continue; }
        let vp = vtf.translation.truncate();
        for &(viking_e, viking_pos, target_faction, charge_speed) in &viking_snap {
            if vfac.0 != target_faction { continue; }
            // Combined approximate hit radius (longship hit ~3 +
            // typical victim hit ~3.5, plus a bit of slop so the
            // ram registers as soon as silhouettes overlap).
            let r = 8.0;
            if vp.distance_squared(viking_pos) < r * r {
                // Damage scales with the impact speed: a fresh charge
                // starts at the base damage, a full-charge ram caps
                // at `VIKING_RAM_DAMAGE_CAP`.
                let t = ((charge_speed - VIKING_RAM_BASE_SPEED)
                    / (VIKING_RAM_MAX_SPEED - VIKING_RAM_BASE_SPEED))
                    .clamp(0.0, 1.0);
                let bonus = (VIKING_RAM_DAMAGE_CAP - VIKING_RAM_DAMAGE) as f32 * t;
                let dmg = (VIKING_RAM_DAMAGE + bonus.round() as i32)
                    .min(VIKING_RAM_DAMAGE_CAP);
                let dealt = crate::bullet::apply_damage(&mut h, &mut fx, dmg);
                crate::bullet::credit_damage(
                    &mut stats,
                    Some(crate::bullet::DamageSource::Ally(ShipClass::Viking)),
                    dealt,
                );
                shake.add_trauma(VIKING_RAM_TRAUMA);
                commands.entity(e).insert(VikingRamGrace { remaining: VIKING_RAM_GRACE });
                hit_this_frame.push(viking_e);
                break;
            }
        }
    }

    // Bleed off a bit of charge on impact but DON'T reset to base —
    // a Viking that's locked onto a victim should stay scary across
    // consecutive rams. The full reset to base only happens when the
    // Viking loses its target (handled in `ally_ai`).
    if !hit_this_frame.is_empty() {
        for (e, _, _, _, mut charge) in &mut vikings {
            if hit_this_frame.contains(&e) {
                charge.current_speed = (charge.current_speed * 0.75)
                    .max(VIKING_RAM_BASE_SPEED);
            }
        }
    }
}
