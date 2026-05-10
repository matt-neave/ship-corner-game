//! Boss patrol entities — small ship icons that wander inside their
//! 5★ section on the map view, telegraphing what the player will face
//! in that section's final wave. Spawned once at startup (one per
//! 5★ section), parented to nothing, rendered on `MAP_LAYER` so the
//! map camera owns them.
//!
//! Movement is simple random-walk-within-polygon: pick a target
//! inside the section, sail toward it, pick a fresh one when arrived
//! or after `RETARGET_TIMER` seconds. The system is gated on
//! `AppState::Map` so it freezes during combat / shop / stage-complete
//! — the patrol is a navigation aid, not a live world simulation.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;
use rand::Rng;

use crate::ally::ShipClass;
use crate::components::Heading;
use crate::palette::PaletteMaterials;
use crate::ship::approach_angle;

use super::{point_in_polygon, MapState, MAP_LAYER};

/// Per-patrol state. `class` is what the boss IS — used both to pick
/// the visual material and to look up `boss_hp` / dimensions when the
/// player crosses into the section. `target` is the current sail
/// destination inside the section's polygon; `timer` forces a re-roll
/// after `RETARGET_TIMER` even if the boat hasn't reached the target
/// (keeps the patrol from hugging the polygon edge if a target lands
/// just outside reachable distance).
#[derive(Component)]
pub struct BossPatrol {
    pub section_id: u32,
    /// Boss class this patrol represents. Pub so HUD / tooltip systems
    /// can read it later — currently nothing inside this module
    /// touches it after the patrol is spawned.
    #[allow(dead_code)]
    pub class: ShipClass,
    pub target: Vec2,
    pub timer: f32,
}

const PATROL_SPEED: f32 = 5.0;
const PATROL_TURN: f32 = 1.6;
const RETARGET_TIMER: f32 = 4.0;
const ARRIVE_RADIUS: f32 = 1.0;
/// Patrol ship rendered scale relative to the class hull dims.
const PATROL_SCALE: f32 = 1.5;

pub fn spawn_boss_patrols(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    pm: Option<Res<PaletteMaterials>>,
    state: Res<MapState>,
) {
    let Some(pm) = pm else { return; };
    let mut rng = rand::thread_rng();

    for section in &state.sections {
        let Some(class) = section.boss_class else { continue; };
        let (hull_w, hull_h) = class.hull_dims();
        // Shrink the chassis a bit so it fits the section nicely; sections
        // are ~30-50 units across, full-size hulls (Carrier hits 18 long)
        // would dominate.
        let w = hull_w * PATROL_SCALE * 0.4;
        let h = hull_h * PATROL_SCALE * 0.4;
        let mesh = meshes.add(Capsule2d::new(w * 0.5, (h - w).max(1.0)));
        let target = pick_random_in_polygon(&section.polygon, &mut rng);
        commands.spawn((
            Mesh2d(mesh),
            MeshMaterial2d(pm.hull_for_class(class).clone()),
            Transform::from_xyz(section.center.x, section.center.y, 1.5),
            RenderLayers::layer(MAP_LAYER),
            // Owned-section gating in `boss_patrol_movement` flips
            // visibility rather than despawning so a `RESTART` (which
            // resets `MapState.owned`) brings the patrol back without
            // having to re-spawn entities.
            Visibility::Inherited,
            Heading(0.0),
            BossPatrol {
                section_id: section.id,
                class,
                target,
                timer: rng.gen_range(0.0..RETARGET_TIMER),
            },
        ));
    }
}

/// Step every patrol toward its current target. Re-rolls the target
/// when the boat arrives or its timer expires; rotates the ship to
/// face its travel direction. Gated on `AppState::Map` by the caller's
/// `run_if`, so during combat / shop the patrols freeze in place.
pub fn boss_patrol_movement(
    time: Res<Time>,
    state: Res<MapState>,
    mut q: Query<(&mut Transform, &mut Heading, &mut Visibility, &mut BossPatrol)>,
) {
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();
    for (mut tf, mut heading, mut vis, mut patrol) in &mut q {
        // Section already claimed — hide the patrol so the cleared
        // zone reads as cleared. Don't despawn (RESTART resets
        // `owned`, and we want the patrols back).
        let owned = state
            .owned
            .get(patrol.section_id as usize)
            .copied()
            .unwrap_or(false);
        let want_vis = if owned { Visibility::Hidden } else { Visibility::Inherited };
        if *vis != want_vis { *vis = want_vis; }
        if owned { continue; }

        let pos = tf.translation.truncate();
        let to = patrol.target - pos;
        patrol.timer -= dt;

        if to.length() < ARRIVE_RADIUS || patrol.timer <= 0.0 {
            let section = &state.sections[patrol.section_id as usize];
            patrol.target = pick_random_in_polygon(&section.polygon, &mut rng);
            patrol.timer = RETARGET_TIMER;
            continue;
        }

        let desired = (-to.x).atan2(to.y);
        let new_h = approach_angle(heading.0, desired, PATROL_TURN * dt);
        heading.0 = new_h;
        let dir = Vec2::new(-new_h.sin(), new_h.cos());
        let new_pos = pos + dir * PATROL_SPEED * dt;
        tf.translation.x = new_pos.x;
        tf.translation.y = new_pos.y;
        tf.rotation = Quat::from_rotation_z(new_h);
    }
}

/// Reject-sample a point inside the section polygon. Bounded loop —
/// 24 attempts is overkill for the convex-ish hand-authored sections
/// this map uses, but the fallback to `polygon[0]` keeps the system
/// total-frame-cost bounded even if a future map ships a pathological
/// section.
fn pick_random_in_polygon(polygon: &[Vec2], rng: &mut impl Rng) -> Vec2 {
    if polygon.is_empty() { return Vec2::ZERO; }
    let mut xmin = f32::INFINITY;
    let mut xmax = -f32::INFINITY;
    let mut ymin = f32::INFINITY;
    let mut ymax = -f32::INFINITY;
    for p in polygon {
        if p.x < xmin { xmin = p.x; }
        if p.x > xmax { xmax = p.x; }
        if p.y < ymin { ymin = p.y; }
        if p.y > ymax { ymax = p.y; }
    }
    for _ in 0..24 {
        let x = rng.gen_range(xmin..=xmax);
        let y = rng.gen_range(ymin..=ymax);
        let p = Vec2::new(x, y);
        if point_in_polygon(p, polygon) {
            return p;
        }
    }
    polygon[0]
}
