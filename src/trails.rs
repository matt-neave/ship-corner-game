//! Ribbon wakes for the friendly ship + every enemy. Each frame we sample
//! the entity's stern position into a `VecDeque`, then rebuild a tapering
//! ribbon mesh through those points (full width at the head, zero at the tail).
//!
//! Friendly trail is a single global resource (`ShipPath`); enemies each have
//! their own `EnemyTrail` component pointing back to their host entity.

use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::render::render_asset::RenderAssetUsages;
use std::collections::VecDeque;

use crate::balance::{
    ENEMY_LEN, ENEMY_TRAIL_HEAD_WIDTH, ENEMY_TRAIL_MAX_POINTS, ENEMY_TRAIL_SAMPLE_HZ,
    HULL_HALF_LEN, TRAIL_HEAD_WIDTH, TRAIL_MAX_POINTS, TRAIL_SAMPLE_HZ,
};
use crate::components::Friendly;
use crate::enemy::Enemy;

/// Marker for the single friendly trail entity. Mesh positions live in world
/// space, so the entity transform stays at origin.
#[derive(Component)]
pub struct Trail;

/// Per-enemy ribbon. Mesh positions live in world space (not parented), so
/// when the enemy despawns, `update_enemy_trails` cleans the orphan.
#[derive(Component)]
pub struct EnemyTrail {
    pub enemy: Entity,
    pub points: VecDeque<Vec2>,
    pub sample_timer: f32,
}

/// Sampled history of the friendly ship's stern. Index 0 = newest, n-1 = oldest.
#[derive(Resource, Default)]
pub struct ShipPath {
    pub points: VecDeque<Vec2>,
    pub sample_timer: f32,
}

/// Empty mesh skeleton with all the attribute buffers a ribbon needs, ready
/// to be rewritten in place by `rebuild_ribbon_mesh` each frame.
pub fn empty_dynamic_mesh() -> Mesh {
    let mut m = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    m.insert_attribute(Mesh::ATTRIBUTE_POSITION, Vec::<[f32; 3]>::new());
    m.insert_attribute(Mesh::ATTRIBUTE_NORMAL, Vec::<[f32; 3]>::new());
    m.insert_attribute(Mesh::ATTRIBUTE_UV_0, Vec::<[f32; 2]>::new());
    m.insert_indices(Indices::U32(Vec::new()));
    m
}

/// Rewrite `mesh` in place as a tapering ribbon through `points`. Index 0 is
/// the head (full width), the last index is the tail (zero width).
pub fn rebuild_ribbon_mesh(mesh: &mut Mesh, points: &VecDeque<Vec2>, head_width: f32) {
    let n = points.len();
    if n < 2 {
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, Vec::<[f32; 3]>::new());
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, Vec::<[f32; 3]>::new());
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, Vec::<[f32; 2]>::new());
        mesh.insert_indices(Indices::U32(Vec::new()));
        return;
    }

    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(n * 2);
    let mut normals:   Vec<[f32; 3]> = Vec::with_capacity(n * 2);
    let mut uvs:       Vec<[f32; 2]> = Vec::with_capacity(n * 2);
    let mut indices:   Vec<u32>      = Vec::with_capacity((n - 1) * 6);

    for i in 0..n {
        let t = 1.0 - (i as f32 / (n - 1) as f32);
        let half_w = head_width * 0.5 * t;
        let prev = if i + 1 < n { points[i + 1] } else { points[i] };
        let next = if i > 0      { points[i - 1] } else { points[i] };
        let mut tangent = next - prev;
        if tangent.length_squared() < 1e-6 { tangent = Vec2::Y; }
        let tangent = tangent.normalize();
        let normal = Vec2::new(-tangent.y, tangent.x);
        let p = points[i];
        let left  = p + normal * half_w;
        let right = p - normal * half_w;
        positions.push([left.x,  left.y,  0.0]);
        positions.push([right.x, right.y, 0.0]);
        normals.push([0.0, 0.0, 1.0]);
        normals.push([0.0, 0.0, 1.0]);
        uvs.push([0.0, t]);
        uvs.push([1.0, t]);
    }
    for i in 0..n - 1 {
        let a = (i * 2) as u32;
        indices.extend_from_slice(&[a, a + 1, a + 2, a + 1, a + 3, a + 2]);
    }
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
}

// ---------- Systems ----------

/// Sample the friendly ship's stern into ShipPath. The ribbon mesh is
/// rebuilt **only on sample-tick frames** — between samples the trail head
/// trails the ship by at most one sample period (≈33 ms at 30 Hz, ~1 px at
/// friendly speed), which is imperceptible and saves a mesh rewrite + GPU
/// upload + four Vec allocations per non-sample frame.
pub fn update_trail(
    time: Res<Time>,
    mut path: ResMut<ShipPath>,
    ship_q: Query<&Transform, (With<Friendly>, Without<Trail>)>,
    trail_q: Query<&Mesh2d, With<Trail>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let Ok(ship_tf) = ship_q.single() else { return; };
    // Anchor 4 px inside the stern so the ribbon attaches to the hull
    // rather than floating a gap behind it.
    let stern_offset = ship_tf.rotation * Vec3::new(0.0, -(HULL_HALF_LEN - 4.0), 0.0);
    let head = (ship_tf.translation + stern_offset).truncate();

    path.sample_timer -= time.delta_secs();
    if path.sample_timer > 0.0 { return; }
    path.sample_timer = 1.0 / TRAIL_SAMPLE_HZ;
    path.points.push_front(head);
    while path.points.len() > TRAIL_MAX_POINTS {
        path.points.pop_back();
    }

    let Ok(Mesh2d(handle)) = trail_q.single() else { return; };
    let Some(mesh) = meshes.get_mut(handle) else { return; };
    rebuild_ribbon_mesh(mesh, &path.points, TRAIL_HEAD_WIDTH);
}

/// Per-enemy ribbon trail. Same sample-tick gating as `update_trail` — at
/// 25 Hz vs. 60 FPS that's a ~2.4× reduction in mesh rewrites + GPU
/// uploads + Vec allocations across the whole enemy fleet, the biggest
/// per-frame allocator pressure source in the previous version.
pub fn update_enemy_trails(
    time: Res<Time>,
    mut commands: Commands,
    enemy_q: Query<&Transform, (With<Enemy>, Without<EnemyTrail>)>,
    mut trail_q: Query<(Entity, &mut EnemyTrail, &Mesh2d)>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let dt = time.delta_secs();
    for (trail_e, mut trail, mesh2d) in &mut trail_q {
        let Ok(enemy_tf) = enemy_q.get(trail.enemy) else {
            commands.entity(trail_e).despawn();
            continue;
        };
        trail.sample_timer -= dt;
        if trail.sample_timer > 0.0 { continue; }
        trail.sample_timer = 1.0 / ENEMY_TRAIL_SAMPLE_HZ;

        let stern = enemy_tf.rotation * Vec3::new(0.0, -(ENEMY_LEN / 2.0 - 1.0), 0.0);
        let head = (enemy_tf.translation + stern).truncate();
        trail.points.push_front(head);
        while trail.points.len() > ENEMY_TRAIL_MAX_POINTS {
            trail.points.pop_back();
        }

        if let Some(mesh) = meshes.get_mut(&mesh2d.0) {
            rebuild_ribbon_mesh(mesh, &trail.points, ENEMY_TRAIL_HEAD_WIDTH);
        }
    }
}
