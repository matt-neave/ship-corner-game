//! Carrier-launched planes. Top-level entities (not parented to the
//! carrier) so the state machine can move them freely. `Plane.carrier`
//! is the back-reference for: parking at the deck slot, returning to
//! it after a sortie, and despawning if the carrier sinks.
//!
//! Planes carry no `Health` / `Ally` markers — they can't be shot at.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;
use rand::Rng;

use crate::balance::PLAY_LAYER;
use crate::bullet::Bullet;
use crate::components::{Faction, FactionKind, Heading, Velocity};
use crate::effects::EffectMeshes;
use crate::palette::PaletteMaterials;
use crate::ship::approach_angle;
use crate::weapon::WeaponType;

use super::ShipClass;

/// One launchable / landable plane attached to a Carrier.
#[derive(Component)]
pub struct Plane {
    pub carrier: Entity,
    /// 0..5 — which parked spot on the carrier deck.
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
    Idle { rest_timer: f32 },
    TakingOff { t: f32 },
    Strafing { target: Vec2 },
    /// Pull-up between strafes — without this, picking the same nearby
    /// enemy after a pass would re-trigger the pass-end check on the
    /// next frame and burn a sortie in a single tick.
    Banking { t: f32 },
    Returning,
    Landing { t: f32 },
}

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
/// just-hit enemy (≈ 26 units at PLANE_SPEED) before re-evaluating.
const PLANE_BANKING_DUR:         f32 = 0.7;
const PLANE_STRAFE_END_DIST:     f32 = 12.0;
const PLANE_LAND_TRIGGER_DIST:   f32 = 14.0;
/// Aim cone half-angle (radians). ~25°.
const PLANE_AIM_CONE:            f32 = 0.45;
/// Idle (on-deck) scale; flying scale is 1.0.
const PLANE_DECK_SCALE:          f32 = 0.6;

/// World-space position of the carrier's parked slot for `slot`.
/// Six slots laid out in three pairs along the flight deck — even =
/// port, odd = starboard.
fn carrier_slot_world(carrier_pos: Vec2, carrier_heading: f32, slot: u8) -> Vec2 {
    let local = match slot {
        0 => Vec2::new(-1.8, -7.0),
        1 => Vec2::new( 1.8, -7.0),
        2 => Vec2::new(-1.8,  0.0),
        3 => Vec2::new( 1.8,  0.0),
        4 => Vec2::new(-1.8,  7.0),
        _ => Vec2::new( 1.8,  7.0),
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

/// Spawn one plane parked at the carrier slot.
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
    let fuselage_mesh = meshes.add(Capsule2d::new(0.5, 2.5));
    let wings_mesh    = meshes.add(Rectangle::new(3.0, 0.8));

    let plane_mat = pm.plane_hull.clone();
    // Stagger initial rest so they don't all lift off in lockstep —
    // slot index gives a coarse aft-to-bow sequence, jittered.
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
        // Slightly forward so the silhouette reads "high-wing prop".
        Transform::from_xyz(0.0, 0.4, 0.05),
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    commands.entity(wings).insert(ChildOf(plane));
}

/// Spawn the twin forward-firing bullets from a strafing plane.
/// `bullet_faction` is the OWN side (opposite of what the plane targets).
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
                runes: Vec::new(),
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
/// applied directly to `Transform` — planes are outside the
/// `apply_velocity` integrator and don't carry status effects.
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
            commands.entity(entity).despawn();
            continue;
        };
        // Per-plane target snapshot filtered by THIS plane's faction —
        // built inside the loop so a mixed fleet of friendly + boss
        // carriers each see only their own quarry.
        let target_positions: Vec<Vec2> = candidates
            .iter()
            .filter(|(_, f)| f.0 == plane.target_faction)
            .map(|(t, _)| t.translation.truncate())
            .collect();
        let cpos = ctf.translation.truncate();
        let cheading = ctf.rotation.to_euler(EulerRot::XYZ).2;
        let slot_pos = carrier_slot_world(cpos, cheading, plane.slot);

        let mut next_state: Option<PlaneState> = None;

        match plane.state {
            PlaneState::Idle { mut rest_timer } => {
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

                plane.fire_cd -= dt;
                let aim_diff = forward.angle_to(to.normalize_or_zero()).abs();
                if aim_diff < PLANE_AIM_CONE && plane.fire_cd <= 0.0 {
                    plane.fire_cd = 1.0 / PLANE_FIRE_RATE;
                    spawn_plane_bullets(
                        &mut commands, &pm, &em, new_pos, forward, heading.0,
                        plane.target_faction.opposite(),
                        Some(crate::bullet::DamageSource::Ally(ShipClass::Carrier)),
                    );
                }

                let dist = to.length();
                let passed = forward.dot(to) < 0.0;
                if dist < PLANE_STRAFE_END_DIST || passed {
                    plane.runs_remaining = plane.runs_remaining.saturating_sub(1);
                    if plane.runs_remaining > 0 {
                        // Bank away first — re-picking a target here
                        // would re-trigger the pass-end check next frame.
                        next_state = Some(PlaneState::Banking { t: 0.0 });
                    } else {
                        next_state = Some(PlaneState::Returning);
                    }
                }
            }
            PlaneState::Banking { mut t } => {
                t = (t + dt / PLANE_BANKING_DUR).min(1.0);
                let pos = tf.translation.truncate();

                // Gentle centroid-turn so the next strafe lines up
                // cleanly without committing to a specific enemy yet.
                if !target_positions.is_empty() {
                    let n = target_positions.len() as f32;
                    let centroid =
                        target_positions.iter().copied().sum::<Vec2>() / n;
                    let to = centroid - pos;
                    if to.length_squared() > 0.01 {
                        let desired = (-to.x).atan2(to.y);
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
                // Smooth converge — rate shrinks as t→1 so touch-down
                // settles instead of snapping.
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
                        rest_timer: PLANE_REST_BASE + rng.gen_range(0.0..2.0),
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
