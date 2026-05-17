//! Map-view world setup.
//!
//! Spawns the static map entities at startup: the fill sprite, the
//! mitered ribbon dividers between sections, the per-section star
//! marks, and the boat token.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;

use crate::balance::{HULL_LEN, HULL_WIDTH, PLAY_WORLD, TURRET_MOUNTS, TURRET_POSITIONS};
use crate::components::Heading;
use crate::palette::{Palette, PaletteMaterials};

use super::build::{
    build_map_fill_image, build_ribbon_mesh, is_outer_edge, wobble_for_edge,
};
use super::{
    MapBoat, MapFillSprite, MapSection, MapSectionBoundary, MapSlotStar,
    MapState, MAP_BOAT_SCALE, MAP_LAYER, STAR_GAP, STAR_SIZE,
    STAR_Y_OFFSET, Z_BOAT, Z_FILL, Z_OUTLINE, Z_SLOT_STAR,
};

pub fn setup_map(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    pm: Option<Res<PaletteMaterials>>,
    palette: Res<Palette>,
    state: Res<MapState>,
) {
    let Some(pm) = pm else { return };

    // Section fills — one pre-rasterized sprite for the entire map.
    let fill_handle = images.add(build_map_fill_image(&state, &palette));
    commands.spawn((
        Sprite {
            image: fill_handle,
            custom_size: Some(Vec2::splat(PLAY_WORLD)),
            ..default()
        },
        Transform::from_xyz(0.0, 0.0, Z_FILL),
        RenderLayers::layer(MAP_LAYER),
        MapFillSprite,
    ));

    // Boundary dividers — one continuous mitered ribbon per *unique*
    // interior edge. Deduped across polygons (each interior edge is
    // shared by exactly two sections) so the divider draws once.
    let q = |v: Vec2| (
        (v.x * 1000.0).round() as i32,
        (v.y * 1000.0).round() as i32,
    );
    let canonical_key = |a: Vec2, b: Vec2| {
        let (p, r) = if (a.x, a.y) <= (b.x, b.y) { (a, b) } else { (b, a) };
        (q(p), q(r))
    };
    let mut seen: std::collections::HashSet<((i32, i32), (i32, i32))>
        = std::collections::HashSet::new();
    for section in &state.sections {
        let n = section.corners.len();
        for i in 0..n {
            let a = section.corners[i];
            let b = section.corners[(i + 1) % n];
            if is_outer_edge(a, b) { continue; }
            if !seen.insert(canonical_key(a, b)) { continue; }

            let mut path = Vec::with_capacity(10);
            path.push(a);
            path.extend(wobble_for_edge(a, b));
            path.push(b);

            let ribbon = build_ribbon_mesh(&path, 1.4);
            commands.spawn((
                Mesh2d(meshes.add(ribbon)),
                MeshMaterial2d(pm.map_divider.clone()),
                Transform::from_xyz(0.0, 0.0, Z_OUTLINE),
                RenderLayers::layer(MAP_LAYER),
                MapSectionBoundary,
            ));
        }
    }

    // Stars on every section so red-zone (high-tier) ratings are
    // visible everywhere — not gated on ownership.
    let star_mesh = meshes.add(Rectangle::new(STAR_SIZE, STAR_SIZE));
    for section in &state.sections {
        spawn_section_stars(&mut commands, section, &star_mesh, &pm);
    }

    // Map boat — same hull + 8-turret rig as the in-combat ship, scaled
    // down. All on `MAP_LAYER`.
    let hull_radius      = HULL_WIDTH / 2.0;
    let hull_inner       = HULL_LEN - HULL_WIDTH;
    let hull_mesh        = meshes.add(Capsule2d::new(hull_radius, hull_inner));
    let turret_base_mesh = meshes.add(Circle::new(2.0));
    let barrel_mesh      = meshes.add(Rectangle::new(1.5, 4.0));

    let start = state.section(state.current).center;
    let boat = commands.spawn((
        Mesh2d(hull_mesh),
        MeshMaterial2d(pm.hull.clone()),
        Transform::from_xyz(start.x, start.y, Z_BOAT)
            .with_scale(Vec3::splat(MAP_BOAT_SCALE)),
        Heading(0.0),
        MapBoat,
        RenderLayers::layer(MAP_LAYER),
    )).id();

    for (i, (lx, ly)) in TURRET_POSITIONS.iter().enumerate() {
        let mount = TURRET_MOUNTS[i];
        let turret = commands.spawn((
            Mesh2d(turret_base_mesh.clone()),
            MeshMaterial2d(pm.turret.clone()),
            Transform::from_xyz(*lx, *ly, 0.5)
                .with_rotation(Quat::from_rotation_z(mount)),
            RenderLayers::layer(MAP_LAYER),
        )).id();
        commands.entity(turret).insert(ChildOf(boat));

        let barrel = commands.spawn((
            Mesh2d(barrel_mesh.clone()),
            MeshMaterial2d(pm.turret.clone()),
            Transform::from_xyz(0.0, 3.0, 0.1),
            RenderLayers::layer(MAP_LAYER),
        )).id();
        commands.entity(barrel).insert(ChildOf(turret));
    }
}

/// Spawn the row of star marks above a section's center. Always-on
/// info-layer visual — every section shows its rating regardless of
/// ownership.
fn spawn_section_stars(
    commands: &mut Commands,
    section: &MapSection,
    star_mesh: &Handle<Mesh>,
    pm: &PaletteMaterials,
) {
    let stars = section.stars as usize;
    if stars == 0 { return; }
    let pitch = STAR_SIZE + STAR_GAP;
    let row_left = section.center.x - (stars as f32 - 1.0) * 0.5 * pitch;
    let star_y = section.center.y + STAR_Y_OFFSET;
    for s in 0..stars {
        commands.spawn((
            Mesh2d(star_mesh.clone()),
            MeshMaterial2d(pm.map_slot_star.clone()),
            Transform::from_xyz(row_left + s as f32 * pitch, star_y, Z_SLOT_STAR),
            RenderLayers::layer(MAP_LAYER),
            MapSlotStar,
        ));
    }
}

