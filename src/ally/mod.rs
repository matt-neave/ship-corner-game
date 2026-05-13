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
    ARENA_H, ARENA_W, PLAY_LAYER, TURRET_PIVOT, TURRET_RANGE,
};
use crate::components::{Faction, FactionKind, Health, Heading, Velocity};
use crate::effects::{spawn_hit_particles, EffectMeshes, HitFx};
use crate::enemy::{Enemy, EnemyState, EnemyVariant};
use crate::palette::PaletteMaterials;
use crate::rune::FireExtent;
use crate::ship::approach_angle;
use crate::turret::spawn_combat_bullet;
use crate::weapon::WeaponType;

// Submodules — each owns a coherent slice of ally behavior (planes,
// oil cycle, viking ram, mines, missiles, boarding, heal beam).
pub mod boarding;
pub mod heal;
pub mod mine;
pub mod missile;
pub mod oil;
pub mod plane;
pub mod viking;

pub use boarding::{
    boarder_tick, boarding_launcher_fire, update_boarding_ropes,
    BoardingLauncher, BOARDING_RANGE,
};
pub use heal::{tender_heal_beam, HealBeamEmitter};
pub use mine::{
    flash_mine_dots, mine_layer_drop, mine_tick, MineLayer,
};
pub use missile::{
    homing_missile_track, missile_launcher_fire, spawn_homing_missile_full,
    MissileLauncher,
};
pub use oil::{
    oil_slick_burn_tick, oil_slick_grow_tick, oil_tanker_cycle,
    OilCyclePhase, OilTankerCycle, OIL_SPRAY_DURATION,
};
pub use plane::{plane_ai, spawn_plane};
pub use viking::{
    viking_ram_damage, VikingRamCharge,
    VIKING_RAM_ALIGN_THRESHOLD, VIKING_RAM_BASE_SPEED, VIKING_RAM_DECAY_PER_SEC,
    VIKING_RAM_MAX_SPEED, VIKING_RAM_RAMP_TIME, VIKING_RAM_TURN_AT_MAX,
};

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
        // Boss-tier HP ladder. Ordered roughly by "how late this boss
        // feels in a run". Carrier sits at the top as the apex bullet
        // sponge; Submarine is the easiest first-boss target.
        match self {
            ShipClass::Submarine  =>  35,
            ShipClass::Minelayer  =>  45,
            ShipClass::Tender     =>  60,
            ShipClass::PirateShip =>  80,
            // Blackbeard is the flagship — boarding wants close-range
            // orbit, so the hull has to survive the approach.
            ShipClass::Blackbeard => 120,
            // Chunky industrial hull. Oil-spray fantasy wants the
            // tanker to live its full burn cycle.
            ShipClass::OilTanker  => 150,
            // Built for ramming — sturdy enough to survive the charge.
            ShipClass::Viking     => 170,
            // Apex bullet sponge.
            ShipClass::Carrier    => 300,
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
            ShipClass::PirateShip => 10,
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

    /// Base HP for a boss-side spawn of this class. The actual spawn
    /// multiplies this by the section's star tier (`spawn_boss(stars)`),
    /// so a 5★ boss is 5× as tough as a 3★ boss of the same class.
    pub fn boss_hp(self) -> i32 {
        match self {
            ShipClass::Submarine  => 180,
            ShipClass::Minelayer  => 180,
            ShipClass::Tender     =>  80,
            ShipClass::PirateShip => 180,
            ShipClass::Blackbeard => 220,
            ShipClass::OilTanker  => 220,
            ShipClass::Viking     => 300,
            ShipClass::Carrier    => 300,
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

/// White signal flag drawn across the deck, parented to an ally ship.
/// Marker only — the flag's "wind-caught" look comes from a curved
/// mesh built once at spawn (`build_curved_flag_mesh`), not a
/// per-frame animation.
#[derive(Component)]
pub struct AllyFlag;

/// Once-per-frame snapshot of non-submerged ally world positions.
/// Built by `update_ally_positions_cache` so the enemy AI / fire
/// pipelines (5+ systems that all pick the nearest target from this
/// list) don't each allocate a fresh `Vec<Vec2>` every tick.
#[derive(Resource, Default)]
pub struct AllyPositionsCache {
    pub positions: Vec<Vec2>,
}

/// Refresh the shared ally-positions snapshot. Runs early in Update so
/// every downstream consumer reads a value that matches *this* frame's
/// transforms.
pub fn update_ally_positions_cache(
    mut cache: ResMut<AllyPositionsCache>,
    allies: Query<(&Transform, &Ally)>,
) {
    cache.positions.clear();
    cache.positions.extend(
        allies
            .iter()
            .filter(|(_, a)| !ally_is_submerged(a))
            .map(|(t, _)| t.translation.truncate()),
    );
}

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
/// side: faction flipped, HP bumped to `class.boss_hp()`, and
/// double-tagged with BOTH `Enemy` and `Ally`. The `Enemy` tag routes
/// bullet collisions / scoring / XP through the standard pipeline;
/// the `Ally` tag drives the boss with `ally_ai`'s class-aware
/// movement (orbit, kite, ram, etc.) and gates it OUT of the generic
/// `enemy_ai` / `enemy_fire` / `bomber_detonate` systems via
/// `Without<Ally>` filters there. Net effect: a Submarine boss
/// behaves like the friendly Submarine — fires only its homing
/// missile, kites at range — instead of also firing a "free"
/// Standard enemy bullet straight ahead.
pub fn spawn_boss(
    commands: &mut Commands,
    pm: &PaletteMaterials,
    em: &EffectMeshes,
    meshes: &mut Assets<Mesh>,
    pos: Vec2,
    heading: f32,
    class: ShipClass,
    stars: u8,
    battles_cleared: u32,
) {
    let ship = build_ship_for_faction(
        commands, pm, em, meshes, pos, heading, class,
        FactionKind::Enemy,
    );
    // Boss HP scales with section star tier AND total stages cleared
    // so each successive boss feels meaningfully tankier than the
    // last — Brotato-style escalation that compounds on top of the
    // tier bump every 3 stages. +15% per cleared stage, capped at 12
    // (3.4x at the wall).
    let stage_mult = 1.0 + 0.15 * battles_cleared.min(12) as f32;
    let base_hp = class.boss_hp() * stars.max(1) as i32;
    let boss_hp = ((base_hp as f32) * stage_mult).round() as i32;
    // `EnemyVariant::Standard` is a placeholder — `enemy_ai` /
    // `enemy_fire` / `bomber_detonate` all gate `Without<Ally>` so
    // the variant's stats / firing path never apply to a boss. Kept
    // populated so the death-check XP grant + HP-bar denominator
    // still have a valid `Enemy` to read.
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
        Ally {
            class,
            waypoint: Vec2::ZERO,
            waypoint_timer: 0.0,
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
                    rng.gen_range(-ARENA_W * 0.35..ARENA_W * 0.35),
                    rng.gen_range(-ARENA_H * 0.35..ARENA_H * 0.35),
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
        let bound_x = ARENA_W * 0.5 - margin;
        let bound_y = ARENA_H * 0.5 - margin;
        let target = Vec2::new(target.x.clamp(-bound_x, bound_x), target.y.clamp(-bound_y, bound_y));

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
    harpooned_owners: Query<(), With<crate::harpoon::Harpooned>>,
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
        // Harpooned owner (boss): hold fire while the tether is active.
        if harpooned_owners.get(parent.0).is_ok() { continue; }
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
                    1.0, // no rune effect on ally bullets (no runes)
                );
            }
        }
    }
}

/// Despawn allies that have hit 0 HP, with a destruction burst. Decoupled
/// from the bullet collision system so we can keep the bullet-vs-friendly
/// query simple (it just decrements HP).
pub fn ally_death_check(
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    // `Without<Enemy>` excludes bosses — they share the `Ally` tag
    // for AI purposes but their death is owned by `enemy_death_check`,
    // which awards score/scrap/XP. Letting both fire would double the
    // particle burst on boss death.
    allies: Query<(Entity, &Transform, &Ally, &Health), Without<Enemy>>,
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
