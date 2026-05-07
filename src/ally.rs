//! Allied units — autonomous friendly ships fighting alongside the player.
//!
//! Built to scale: adding a new ally type is a single-file change here,
//! mirroring how `enemy.rs` handles enemy variants.
//!
//! 1. Add a variant to `AllyVariant`.
//! 2. Add rows in `hp`, `speed`, `turn_rate`, `hull_dims`, `turret_layout`,
//!    `fire_rate`, `fire_damage`, `turret_arc_half`.
//! 3. (Optional) extend `spawn_ally`'s body-color match if the new variant
//!    shouldn't reuse `palette.hull_accent`.
//! 4. Trigger spawns from wherever (currently `setup_world` seeds one).
//!
//! Allies share `FactionKind::Friendly` so their bullets damage enemies and
//! enemy bullets damage them. They have their own marker (`Ally`) and their
//! own AI / turret-aim systems so they don't collide with the player ship's
//! input-driven movement and configurable turret slots.

use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::render::render_asset::RenderAssetUsages;
use bevy::render::view::RenderLayers;
use rand::Rng;

use crate::balance::{PLAY_LAYER, PLAY_WORLD, TURRET_PIVOT, TURRET_RANGE};
use crate::bullet::Bullet;
use crate::components::{Faction, FactionKind, Health, Heading, Velocity};
use crate::effects::{spawn_hit_particles, EffectMeshes, HitFx};
use crate::enemy::Enemy;
use crate::palette::PaletteMaterials;
use crate::rune::FireExtent;
use crate::ship::approach_angle;
use crate::turret::spawn_friendly_bullet;
use crate::weapon::WeaponType;

// ---------- Components / variants ----------

#[derive(Component)]
pub struct Ally {
    pub variant: AllyVariant,
    /// Wander target used when no enemy is in range.
    pub waypoint: Vec2,
    /// Time until next wander re-plan.
    pub waypoint_timer: f32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AllyVariant {
    /// Small retro pirate ship — 4 broadside cannons (2 per side).
    PirateShip,
    /// Slow, large flat-top. No cannons of its own — fights through
    /// 2 patrolling planes that take off, strafe, and land.
    Carrier,
}

impl AllyVariant {
    pub fn hp(self) -> i32 {
        match self {
            AllyVariant::PirateShip => 40,
            AllyVariant::Carrier    => 200,
        }
    }
    pub fn speed(self) -> f32 {
        match self {
            AllyVariant::PirateShip => 22.0,
            AllyVariant::Carrier    => 12.0,
        }
    }
    pub fn turn_rate(self) -> f32 {
        match self {
            AllyVariant::PirateShip => 1.4,
            AllyVariant::Carrier    => 0.6,
        }
    }
    /// Hull dimensions: `(width, length)`. Width drives the capsule radius;
    /// length is the long axis.
    pub fn hull_dims(self) -> (f32, f32) {
        match self {
            AllyVariant::PirateShip => (5.0, 12.0),
            AllyVariant::Carrier    => ( 7.0, 24.0),
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
            AllyVariant::PirateShip => &[
                (-2.0,  3.0,  FRAC_PI_2), // port forward
                (-2.0, -3.0,  FRAC_PI_2), // port aft
                ( 2.0,  3.0, -FRAC_PI_2), // stbd forward
                ( 2.0, -3.0, -FRAC_PI_2), // stbd aft
            ],
            // Carrier — no turrets; planes do all the work.
            AllyVariant::Carrier => &[],
        }
    }
    pub fn fire_rate(self) -> f32 {
        match self {
            AllyVariant::PirateShip => 2.0,
            AllyVariant::Carrier    => 0.0,
        }
    }
    pub fn fire_damage(self) -> i32 {
        match self {
            AllyVariant::PirateShip => 1,
            AllyVariant::Carrier    => 0,
        }
    }
    /// Half-arc per turret (radians).
    pub fn turret_arc_half(self) -> f32 {
        match self {
            // ±60° — generous broadside arc that lets the forward + aft pair
            // share targets without rigidly committing to one quadrant.
            AllyVariant::PirateShip => std::f32::consts::FRAC_PI_3,
            AllyVariant::Carrier    => 0.0,
        }
    }
    /// Diameter to use for the bullet/turret hit-radius approximation.
    pub fn hit_radius(self) -> f32 {
        match self {
            AllyVariant::PirateShip => 3.0,
            AllyVariant::Carrier    => 6.0,
        }
    }
}

#[derive(Component)]
pub struct AllyTurret {
    pub barrel_angle: f32,
    pub mount_angle: f32,
    pub fire_cd: f32,
    pub variant: AllyVariant,
}

/// White signal flag drawn across the deck, parented to an ally ship.
/// Marker only — the flag's "wind-caught" look comes from a curved
/// mesh built once at spawn (`build_curved_flag_mesh`), not a
/// per-frame animation.
#[derive(Component)]
pub struct AllyFlag;

/// Per-variant hull material lookup. Lives here (not in `palette.rs`) so
/// `AllyVariant` stays the only source coupled to ally identities — adding
/// a new variant means adding an arm here, not threading it through palette.
impl PaletteMaterials {
    pub fn ally_hull_for(&self, variant: AllyVariant) -> &Handle<ColorMaterial> {
        match variant {
            AllyVariant::PirateShip => &self.pirate_hull,
            AllyVariant::Carrier    => &self.carrier_hull,
        }
    }
}

// ---------- Spawn helper ----------

pub fn spawn_ally(
    commands: &mut Commands,
    pm: &PaletteMaterials,
    em: &EffectMeshes,
    meshes: &mut Assets<Mesh>,
    pos: Vec2,
    heading: f32,
    variant: AllyVariant,
) {
    let (hull_w, hull_h) = variant.hull_dims();
    let hull_mesh = meshes.add(Capsule2d::new(hull_w / 2.0, hull_h - hull_w));
    let dir = Vec2::new(-heading.sin(), heading.cos());

    let body_mat = pm.ally_hull_for(variant).clone();
    let ship = commands.spawn((
        Mesh2d(hull_mesh),
        MeshMaterial2d(body_mat.clone()),
        Transform::from_xyz(pos.x, pos.y, 1.0)
            .with_rotation(Quat::from_rotation_z(heading)),
        Visibility::Inherited,
        Ally { variant, waypoint: Vec2::ZERO, waypoint_timer: 0.0 },
        Faction(FactionKind::Friendly),
        Health(variant.hp()),
        Velocity(dir * variant.speed()),
        Heading(heading),
        HitFx::new(body_mat),
        FireExtent(Vec2::new(hull_w * 0.5, hull_h * 0.5)),
        RenderLayers::layer(PLAY_LAYER),
    )).id();

    for &(lx, ly, mount) in variant.turret_layout() {
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
                variant,
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

    // Flags are part of the pirate-ship silhouette; the carrier
    // doesn't get them. Two flags across the deck, both overhanging
    // the gunwales: aft pennant 1 unit behind midship, smaller bow
    // jack at the front third. Mesh built once with a slight forward
    // bow so it reads wind-caught without per-frame animation.
    if variant == AllyVariant::PirateShip {
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

    // Carriers launch a small wing of planes — 2 by default. Planes
    // start in `Idle` (parked on the deck), spaced by `slot`. Each
    // is its own top-level entity (not a child of the carrier) so
    // it can fly off on patrol freely; the `Plane.carrier` field is
    // the back-reference for tracking the home slot.
    if variant == AllyVariant::Carrier {
        // Six parked planes in three pairs (forward / mid / aft).
        // `carrier_slot_world` does the layout math.
        for slot in 0..6u8 {
            spawn_plane(commands, pm, meshes, ship, slot, pos, heading);
        }
    }
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

/// Movement AI — engage the nearest enemy at moderate range; wander toward
/// random waypoints when no enemy is in sight. Same shape as the friendly
/// ship's tactical mode but tuned by `AllyVariant` stats.
pub fn ally_ai(
    time: Res<Time>,
    enemies: Query<&Transform, (With<Enemy>, Without<Ally>)>,
    mut allies: Query<(&mut Transform, &mut Velocity, &mut Heading, &mut Ally), Without<Enemy>>,
) {
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();

    for (mut tf, mut vel, mut heading, mut ally) in &mut allies {
        let pos = tf.translation.truncate();
        let speed = ally.variant.speed();
        let turn = ally.variant.turn_rate();

        // Find nearest enemy.
        let mut nearest: Option<(f32, Vec2)> = None;
        for etf in &enemies {
            let ep = etf.translation.truncate();
            let d = ep.distance(pos);
            if nearest.map_or(true, |(bd, _)| d < bd) {
                nearest = Some((d, ep));
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
/// allies fire a single Standard bullet at a fixed rate per variant.
pub fn ally_turret_aim_fire(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    allies: Query<(&Transform, &Heading), (With<Ally>, Without<Enemy>, Without<AllyTurret>)>,
    enemies: Query<&Transform, (With<Enemy>, Without<Ally>, Without<AllyTurret>)>,
    mut turrets: Query<
        (&ChildOf, &mut AllyTurret, &mut Transform),
        (Without<Ally>, Without<Enemy>),
    >,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();

    for (parent, mut turret, mut tf) in &mut turrets {
        let Ok((ally_tf, ally_heading)) = allies.get(parent.0) else { continue; };
        let ally_pos = ally_tf.translation.truncate();
        let ally_h = ally_heading.0;
        turret.fire_cd -= dt;

        // World position of this turret (parent rotation × local offset).
        let local = tf.translation.truncate();
        let cos_h = ally_h.cos();
        let sin_h = ally_h.sin();
        let world_off = Vec2::new(
            local.x * cos_h - local.y * sin_h,
            local.x * sin_h + local.y * cos_h,
        );
        let turret_world = ally_pos + world_off;

        // Find best target inside the turret's arc + range.
        let arc_half = turret.variant.turret_arc_half();
        let mut best: Option<(f32, Vec2)> = None;
        for etf in &enemies {
            let ep = etf.translation.truncate();
            let to = ep - turret_world;
            let d = to.length();
            if d > TURRET_RANGE { continue; }
            let world_angle = (-to.x).atan2(to.y);
            let mut local_angle = world_angle - ally_h;
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
            let mut la = world_angle - ally_h;
            la = (la + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                - std::f32::consts::PI;
            la
        } else {
            turret.mount_angle
        };

        turret.barrel_angle = approach_angle(turret.barrel_angle, desired_local, TURRET_PIVOT * dt);
        tf.rotation = Quat::from_rotation_z(turret.barrel_angle);

        // Fire when aimed.
        if best.is_some() {
            let aim_err = (turret.barrel_angle - desired_local).abs();
            if aim_err < 0.1 && turret.fire_cd <= 0.0 {
                turret.fire_cd = 1.0 / turret.variant.fire_rate().max(0.1);
                let total_angle = ally_h + turret.barrel_angle;
                let barrel_forward = Vec2::new(-total_angle.sin(), total_angle.cos());
                // Spawn just past the barrel tip (~ length 3 from base + a bit).
                let muzzle_pos = turret_world + barrel_forward * 4.0;
                spawn_friendly_bullet(
                    &mut commands,
                    &em,
                    &pm.bullet_friendly_outer,
                    &pm.bullet_friendly,
                    muzzle_pos,
                    barrel_forward,
                    WeaponType::Standard,
                    turret.variant.fire_damage(),
                    None, // not a player slot — skip damage-stat crediting
                    TURRET_RANGE,
                    None, // ally turrets don't currently carry runes
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
            // explodes brown, future variants explode their own color.
            spawn_hit_particles(&mut commands, &em, pm.ally_hull_for(ally.variant), pos, 18, 80.0,  &mut rng);
            spawn_hit_particles(&mut commands, &em, &pm.bullet_friendly,            pos, 10, 100.0, &mut rng);
            commands.entity(e).despawn();
        }
    }
}

/// `Ally::hit_radius` exposed as a free function so collision systems don't
/// need to pull the variant out themselves.
pub fn ally_hit_radius(ally: &Ally) -> f32 {
    ally.variant.hit_radius()
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
}

/// Plane state machine. Transitions:
///   Idle ─(rest_timer 0)─▸ TakingOff ─(t≥1)─▸ Strafing
///   Strafing ─(pass complete; runs left)─▸ Strafing (new target)
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
) {
    let fuselage_mesh = meshes.add(Capsule2d::new(0.5, 2.5));   // ~1 wide × 3.5 long
    let wings_mesh    = meshes.add(Rectangle::new(3.0, 0.8));   // 3 wide × 0.8 long

    let plane_mat = pm.plane_hull.clone();
    let plane = commands.spawn((
        Mesh2d(fuselage_mesh),
        MeshMaterial2d(plane_mat.clone()),
        Transform::from_xyz(init_pos.x, init_pos.y, 2.0)
            .with_rotation(Quat::from_rotation_z(init_heading))
            .with_scale(Vec3::splat(PLANE_DECK_SCALE)),
        Plane {
            carrier,
            slot,
            // Stagger initial rest across the wing so they don't all
            // lift off in lockstep — 0.6s per slot gives a clean
            // sequenced launch from aft to bow.
            state: PlaneState::Idle {
                rest_timer: PLANE_REST_BASE + slot as f32 * 0.6,
            },
            fire_cd: 0.0,
            runs_remaining: 0,
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
/// shots traveling along the plane's forward vector.
fn spawn_plane_bullets(
    commands: &mut Commands,
    pm: &PaletteMaterials,
    em: &EffectMeshes,
    pos: Vec2,
    forward: Vec2,
    heading: f32,
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
                faction: FactionKind::Friendly,
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
pub fn plane_ai(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    enemies: Query<&Transform, (With<Enemy>, Without<Plane>, Without<Ally>)>,
    carriers: Query<&Transform, (With<Ally>, Without<Plane>, Without<Enemy>)>,
    mut planes: Query<(Entity, &mut Transform, &mut Heading, &mut Plane)>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();

    // Snapshot enemy positions once for nearest-target lookups.
    let enemy_positions: Vec<Vec2> =
        enemies.iter().map(|t| t.translation.truncate()).collect();

    for (entity, mut tf, mut heading, mut plane) in &mut planes {
        let Ok(ctf) = carriers.get(plane.carrier) else {
            // Carrier sunk — clean up the orphan plane.
            commands.entity(entity).despawn();
            continue;
        };
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
                    let target = nearest_position(pos, &enemy_positions)
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
                    spawn_plane_bullets(&mut commands, &pm, &em, new_pos, forward, heading.0);
                }

                // Pass ends when the target is close *or* behind.
                let dist = to.length();
                let passed = forward.dot(to) < 0.0;
                if dist < PLANE_STRAFE_END_DIST || passed {
                    plane.runs_remaining = plane.runs_remaining.saturating_sub(1);
                    if plane.runs_remaining > 0 {
                        // Pick a fresh target — a different position
                        // means the plane curves around naturally.
                        let new_target = nearest_position(new_pos, &enemy_positions)
                            .unwrap_or(new_pos + forward * 80.0);
                        next_state = Some(PlaneState::Strafing { target: new_target });
                    } else {
                        next_state = Some(PlaneState::Returning);
                    }
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
