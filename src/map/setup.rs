//! Map-view world setup + slot visuals.
//!
//! Spawns the map's render entities at startup (fill sprite, ribbon
//! dividers, slot tiles, stars, the boat token), then keeps slot
//! visuals + labels in sync as ownership flips during play.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;
use bevy::window::PrimaryWindow;

use crate::balance::{
    HULL_LEN, HULL_WIDTH, PLAY_WORLD, TURRET_MOUNTS, TURRET_POSITIONS,
};
use crate::components::Heading;
use crate::modes::{effective_ui_width, play_area_screen_rect, WindowMode};
use crate::palette::{Palette, PaletteMaterials};
use crate::ui_kit::theme;

use super::build::{
    build_map_fill_image, build_ribbon_mesh, is_outer_edge, wobble_for_edge,
};
use super::{
    MapBoat, MapFillSprite, MapSection, MapSectionBoundary, MapSlotBox,
    MapSlotLabel, MapSlotStar, MapState, ViewMode,
    MAP_BOAT_SCALE, MAP_LAYER, SLOT_HALF, SLOT_SIZE, STAR_GAP, STAR_SIZE,
    STAR_Y_OFFSET, Z_BOAT, Z_FILL, Z_OUTLINE, Z_SLOT_BOX, Z_SLOT_STAR,
};

pub fn setup_map(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    pm: Option<Res<PaletteMaterials>>,
    palette: Res<Palette>,
    state: Res<MapState>,
) {
    let Some(pm) = pm else { return; };

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

    // Two passes:
    //   - Stars: on every section so red-zone ratings are visible.
    //   - Slot box + label: only on owned sections.
    let slot_box_mesh = meshes.add(Rectangle::new(SLOT_SIZE, SLOT_SIZE));
    let star_mesh     = meshes.add(Rectangle::new(STAR_SIZE, STAR_SIZE));
    for section in &state.sections {
        spawn_section_stars(&mut commands, section, &star_mesh, &pm);
    }
    for (i, section) in state.sections.iter().enumerate() {
        if !state.owned[i] { continue; }
        spawn_slot_box_and_label(&mut commands, section, &slot_box_mesh, &pm);
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

/// Spawn the slot tile + UI label for each slot of an *owned* section.
/// Stars are handled separately by `spawn_section_stars` so they remain
/// visible on enemy zones too.
pub fn spawn_slot_box_and_label(
    commands: &mut Commands,
    section: &MapSection,
    slot_box_mesh: &Handle<Mesh>,
    pm: &PaletteMaterials,
) {
    let n_slots = section.slots.len();
    if n_slots == 0 { return; }

    for slot_index in 0..n_slots {
        let pos = section.center;

        commands.spawn((
            Mesh2d(slot_box_mesh.clone()),
            MeshMaterial2d(pm.map_slot.clone()),
            Transform::from_xyz(pos.x, pos.y, Z_SLOT_BOX),
            RenderLayers::layer(MAP_LAYER),
            MapSlotBox { section_id: section.id, slot_index },
        ));

        // Label: Bevy UI text node, *not* `Text2d`. The whole map world
        // is rendered to a 200×200 internal buffer that's then nearest-
        // neighbor upscaled — fine for blocky art, blurry for AA glyphs.
        // UI nodes bypass the upscale and render at native resolution.
        commands.spawn((
            Text::new(""),
            TextFont { font_size: theme::FONT_SM, ..default() },
            TextColor(theme::ON_SURFACE),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                ..default()
            },
            Visibility::Hidden,
            MapSlotLabel { section_id: section.id, slot_index },
        ));
    }
}

/// React to ownership flips by spawning slot visuals for newly-owned
/// sections. The initial owned section's slot is spawned by `setup_map`;
/// this picks up everything after.
pub fn sync_owned_slot_visuals(
    mut commands: Commands,
    state: Res<MapState>,
    pm: Option<Res<PaletteMaterials>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut owned_snapshot: Local<Vec<bool>>,
) {
    if owned_snapshot.len() != state.owned.len() {
        *owned_snapshot = state.owned.clone();
        return;
    }
    if owned_snapshot.as_slice() == state.owned.as_slice() { return; }

    let mut newly_owned: Vec<usize> = Vec::new();
    for (i, &now) in state.owned.iter().enumerate() {
        if now && !owned_snapshot[i] { newly_owned.push(i); }
    }
    *owned_snapshot = state.owned.clone();

    if newly_owned.is_empty() { return; }
    let Some(pm) = pm else { return; };
    let slot_box_mesh = meshes.add(Rectangle::new(SLOT_SIZE, SLOT_SIZE));
    for i in newly_owned {
        spawn_slot_box_and_label(
            &mut commands,
            &state.sections[i],
            &slot_box_mesh,
            &pm,
        );
    }
}

/// Drive the slot labels each frame: write the building name, snap the
/// UI node to the slot's screen position, and gate visibility on map view.
pub fn update_map_slot_labels(
    state: Res<MapState>,
    view: Res<ViewMode>,
    windows: Query<&Window, With<PrimaryWindow>>,
    window_mode: Res<WindowMode>,
    mut q: Query<(&MapSlotLabel, &mut Node, &mut Text, &mut Visibility)>,
) {
    let Ok(win) = windows.single() else { return; };
    let (left, top, size) = play_area_screen_rect(
        win.width(), win.height(), effective_ui_width(&window_mode),
    );
    let upscale = size / PLAY_WORLD;
    let in_map = *view == ViewMode::Map;

    for (tag, mut node, mut text, mut vis) in &mut q {
        let section = &state.sections[tag.section_id as usize];

        let label = section.slots
            .get(tag.slot_index)
            .copied()
            .flatten()
            .map(|b| b.label())
            .unwrap_or("");
        if text.0 != label { text.0 = label.to_string(); }

        let want_vis = if in_map && !label.is_empty() {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *vis != want_vis { *vis = want_vis; }

        if !in_map || label.is_empty() { continue; }

        let nx = (section.center.x + PLAY_WORLD / 2.0) / PLAY_WORLD;
        let ny = (PLAY_WORLD / 2.0 - section.center.y) / PLAY_WORLD;
        let slot_x = left + nx * size;
        let slot_y = top  + ny * size;

        // Off by a few pixels for odd-width strings — close enough.
        let approx_w = label.chars().count() as f32 * theme::FONT_SM * 0.55;
        let label_x = slot_x - approx_w * 0.5;
        let label_y = slot_y + (SLOT_HALF + 2.0) * upscale;
        node.left = Val::Px(label_x);
        node.top  = Val::Px(label_y);
    }
}
