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
    BEAM_LENGTH, FRIENDLY_HP_WAVE, PLAY_LAYER, PLAY_WORLD, TURRET_PIVOT, TURRET_RANGE,
};
use crate::beam::Beam;
use crate::bullet::Bullet;
use crate::components::{Faction, FactionKind, Friendly, Health, Heading, Velocity};
use crate::effects::{spawn_hit_particles, EffectMeshes, HitFx};
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
}

impl ShipClass {
    pub fn hp(self) -> i32 {
        match self {
            ShipClass::PirateShip => 40,
            ShipClass::Carrier    => 200,
            ShipClass::Submarine  => 20,
            ShipClass::Minelayer  => 25,
            ShipClass::Tender     => 35,
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
        }
    }
    pub fn turn_rate(self) -> f32 {
        match self {
            ShipClass::PirateShip => 1.4,
            ShipClass::Carrier    => 0.6,
            ShipClass::Submarine  => 1.0,
            ShipClass::Minelayer  => 1.6,
            ShipClass::Tender     => 2.0,
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
            // Mid-size, slightly stocky proportions — reads as a
            // utility/medical hull rather than a slim warship.
            ShipClass::Tender     => (5.0, 12.0),
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
        }
    }
    pub fn fire_rate(self) -> f32 {
        match self {
            ShipClass::PirateShip => 2.0,
            ShipClass::Carrier    => 0.0,
            ShipClass::Submarine  => 0.0,
            ShipClass::Minelayer  => 0.0,
            ShipClass::Tender     => 0.0,
        }
    }
    pub fn fire_damage(self) -> i32 {
        match self {
            ShipClass::PirateShip => 1,
            ShipClass::Carrier    => 0,
            ShipClass::Submarine  => 0,
            ShipClass::Minelayer  => 0,
            ShipClass::Tender     => 0,
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
        }
    }
    /// Diameter to use for the bullet/turret hit-radius approximation.
    pub fn hit_radius(self) -> f32 {
        match self {
            ShipClass::PirateShip => 3.0,
            ShipClass::Carrier    => 6.0,
            ShipClass::Submarine  => 2.5,
            ShipClass::Minelayer  => 2.5,
            ShipClass::Tender     => 3.0,
        }
    }
    /// Whether this class is treated as underwater. Submerged ships are
    /// invisible to normal enemies — bullets, bombers, and target-selection
    /// all skip them. Boss enemies (future) may opt to ignore this gate.
    pub fn is_submerged(self) -> bool {
        matches!(self, ShipClass::Submarine)
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
        }
    }
}

// ---------- Spawn helper ----------

/// Spawn one allied ship of `class` at `pos`. Wraps `spawn_ship_chassis`
/// (which is faction-agnostic — see module docs) and adds the friendly-
/// only bits: the `Ally` AI marker, broadside turrets with `AllyTurret`
/// markers, decorative pirate flags, the carrier's plane wing, and the
/// submarine's missile launcher.
pub fn spawn_ally(
    commands: &mut Commands,
    pm: &PaletteMaterials,
    em: &EffectMeshes,
    meshes: &mut Assets<Mesh>,
    pos: Vec2,
    heading: f32,
    class: ShipClass,
) {
    // Friendly side: combat-targeting components shoot the opposite
    // faction; the tender heals the same faction. A future
    // `spawn_boss_ship` would mirror this — calling `spawn_ship_chassis`
    // with `FactionKind::Enemy` and flipping these accordingly.
    let own_faction    = FactionKind::Friendly;
    let target_faction = own_faction.opposite();
    let heal_faction   = own_faction;

    let ship = spawn_ship_chassis(commands, pm, meshes, pos, heading, class, own_faction);
    commands.entity(ship).insert(Ally {
        class,
        waypoint: Vec2::ZERO,
        waypoint_timer: 0.0,
    });

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
            damage: 4,
            cd: 1.0 / fire_rate,
            muzzle_offset: hull_h * 0.5,
            target_faction,
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

    // Tender carries a healing-beam emitter and wears a red cross on
    // the deck for instant role recognition. The emitter is the only
    // output — no cannons, no mines, no missiles.
    if class == ShipClass::Tender {
        commands.entity(ship).insert(HealBeamEmitter {
            range: 50.0,
            hp_per_sec: 3.0,
            accumulator: 0.0,
            heal_faction,
        });

        // Red cross deck mark: two perpendicular bars centered on the
        // hull. Drawn slightly above the hull (z=0.05) so it sits on
        // the deck rather than under it.
        let bar_v = meshes.add(Rectangle::new(0.9, 4.0));
        let bar_h = meshes.add(Rectangle::new(4.0, 0.9));
        let cross_mat = pm.mine_inner.clone();
        for mesh in [bar_v, bar_h] {
            let bar = commands.spawn((
                Mesh2d(mesh),
                MeshMaterial2d(cross_mat.clone()),
                Transform::from_xyz(0.0, 0.0, 0.05),
                RenderLayers::layer(PLAY_LAYER),
            )).id();
            commands.entity(bar).insert(ChildOf(ship));
        }
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
    )>,
) {
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();

    // Snapshot ally id+pos+faction so the tender follow branch can
    // reach other allies' positions without conflicting with the
    // outer `&mut Transform` borrow.
    let ally_snap: Vec<(Entity, Vec2, FactionKind)> = allies
        .iter()
        .map(|(e, tf, _, _, _, fac, _)| (e, tf.translation.truncate(), fac.0))
        .collect();

    for (entity, mut tf, mut vel, mut heading, mut ally, faction, emitter) in &mut allies {
        let pos = tf.translation.truncate();
        let speed = ally.class.speed();
        let turn = ally.class.turn_rate();

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
            // Slow when close so the tender doesn't ram the unit it's
            // meant to support.
            let dist = to.length();
            let slow = (dist / 12.0).clamp(0.0, 1.0);
            vel.0 = dir * speed * slow;
            tf.rotation = Quat::from_rotation_z(heading.0);
            continue;
        }

        // Standard combat AI: engage opposite-faction units.
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

        let target = if let Some((d, ep)) = nearest {
            // Engage: orbit at desired range so broadside turrets can bear.
            let to = ep - pos;
            let unit = to.normalize_or_zero();
            let desired_range = TURRET_RANGE * 0.7;
            if d > desired_range + 8.0 {
                ep
            } else if d < desired_range - 8.0 {
                pos - unit * 30.0
            } else {
                let perp = Vec2::new(-unit.y, unit.x);
                pos + perp * 30.0
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
                spawn_combat_bullet(
                    &mut commands,
                    &em,
                    &pm.bullet_friendly_outer,
                    &pm.bullet_friendly,
                    muzzle_pos,
                    barrel_forward,
                    WeaponType::Standard,
                    turret.class.fire_damage(),
                    None, // not a player slot — skip damage-stat crediting
                    TURRET_RANGE,
                    None, // ally turrets don't currently carry runes
                    turret.target_faction.opposite(),
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
            slot: None,
            rune: None,
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
            launcher.target_faction,
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

// ---------- Sea mines ----------

/// Initial arm delay on a freshly-dropped mine. Long enough that the
/// laying ship can't re-cross its own drop and detonate it, but short
/// enough that an enemy chasing the layer eats the mine within a second
/// of pursuit.
const MINE_ARM_DELAY: f32 = 0.6;
/// How long a mine sits on the water before quietly despawning. Stops
/// long combats from accumulating a dense minefield.
const MINE_LIFETIME: f32 = 18.0;

/// Spawn one mine at `pos`. Two-tone — dark shell with a small red
/// warning dot. Lives at z=0.6 so it sits above the trail (z=0.4) but
/// well below ship hulls (z=1.0+). `target_faction` is cached on the
/// mine itself so the proximity check is faction-aware after the laying
/// ship is gone.
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
    let dot = commands.spawn((
        Mesh2d(em.mine_inner.clone()),
        MeshMaterial2d(pm.mine_inner.clone()),
        Transform::from_xyz(0.0, 0.0, 0.05),
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
                crate::bullet::apply_damage(&mut h, &mut fx, mine.damage);
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

// ---------- Heal beams ----------

/// How long each spawned heal-beam visual lasts. Short enough that
/// per-frame respawns blend into one continuous beam, long enough that
/// the beam reads as a steady tether even on a slow frame.
const HEAL_BEAM_LIFE: f32 = 0.08;

/// Spawn one short-lived heal-beam visual between two world points.
/// Reuses the railgun beam mesh + `Beam` lifetime/width animator — no
/// `BeamHit` / `BeamPending` so the visual carries no damage. Mirrors
/// `spawn_lightning_arc` in `bullet.rs`.
fn spawn_heal_beam(
    commands: &mut Commands,
    em: &EffectMeshes,
    mat: &Handle<ColorMaterial>,
    a: Vec2,
    b: Vec2,
) {
    let delta = b - a;
    let len = delta.length();
    if len < 0.5 { return; }
    let mid = (a + b) * 0.5;
    let angle = (-delta.x).atan2(delta.y);
    commands.spawn((
        Mesh2d(em.beam.clone()),
        MeshMaterial2d(mat.clone()),
        Transform {
            translation: Vec3::new(mid.x, mid.y, 5.5),
            rotation: Quat::from_rotation_z(angle),
            // y scales the BEAM_LENGTH-long mesh down to the actual span.
            // x is animated by `update_beams` so spawn at 0.
            scale: Vec3::new(0.0, len / BEAM_LENGTH, 1.0),
        },
        Beam { life: HEAL_BEAM_LIFE, max_life: HEAL_BEAM_LIFE },
        RenderLayers::layer(PLAY_LAYER),
    ));
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

        spawn_heal_beam(&mut commands, &em, &pm.heal, tender_pos, target_pos);
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
                slot: None,
                rune: None,
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
