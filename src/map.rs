//! Map view — a zoomed-out second view where the player picks where to
//! sail next. The same square play area is reused; we just swap what the
//! play camera renders by flipping its `RenderLayers` between
//! `PLAY_LAYER` (combat) and `MAP_LAYER` (map). One camera, two views.
//!
//! Layout: 10 hand-authored sections in a 3 + 4 + 3 row split. Adjacent
//! sections share their boundary corners exactly + use a deterministic
//! `wobble_for_edge` curve so the dividers look hand-drawn but match
//! across regions (no slivers or gaps). Outer-edge segments stay straight
//! so the map fills the square cleanly.
//!
//! Movement reuses the in-game pattern (`approach_angle` toward a desired
//! heading, fixed forward speed) — but the destination is set by clicking
//! an adjacent section instead of following the cursor continuously.
//!
//! Currently, entering an unowned section just flips view to combat;
//! "winning" or "capturing" isn't wired yet (per design discussion).

use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::render::render_asset::RenderAssetUsages;
use bevy::render::view::RenderLayers;
use bevy::window::PrimaryWindow;

use crate::balance::{
    FRIENDLY_SPEED, FRIENDLY_TURN_RATE, HULL_LEN, HULL_WIDTH, PLAY_LAYER, PLAY_WORLD,
};
use crate::components::Heading;
use crate::modes::{effective_ui_width, play_area_screen_rect, WindowMode};
use crate::palette::{PaletteMaterials, PlayCamera};
use crate::ship::approach_angle;

/// Render layer for everything visible only in map view. `apply_view_mode`
/// flips the play camera between `PLAY_LAYER` and this.
pub const MAP_LAYER: usize = 3;

/// Z-band used by map entities so they layer cleanly:
///   0.5 = section fills, 0.7 = boundary segments, 1.5 = boat token.
const Z_FILL:    f32 = 0.5;
const Z_OUTLINE: f32 = 0.7;
const Z_BOAT:    f32 = 1.5;

/// Visual scale of the map boat token relative to its in-combat size.
/// Same hull mesh, half the size — implies a zoomed-out world view.
const MAP_BOAT_SCALE: f32 = 0.5;

/// Distance below which the boat is considered to have arrived at its
/// target. Slightly larger than zero so the steer-in tail doesn't loop
/// forever fighting the turn rate.
const ARRIVAL_DIST: f32 = 1.5;

// ---------- Resources ----------

#[derive(Resource, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Map,
    Combat,
}
impl Default for ViewMode {
    fn default() -> Self { ViewMode::Map }
}

pub struct MapSection {
    pub id: u32,
    /// CCW polygon vertices, including curved-boundary intermediate points.
    pub polygon: Vec<Vec2>,
    /// Center point — both visual (boat parking, fan-tri pivot) and used as
    /// the click-target the boat sails to.
    pub center: Vec2,
    pub adjacencies: Vec<u32>,
}

#[derive(Resource)]
pub struct MapState {
    pub sections: Vec<MapSection>,
    pub current: u32,
    /// Indexed by section id.
    pub owned: Vec<bool>,
    pub boat_target: Option<u32>,
}

impl MapState {
    pub fn new() -> Self {
        let sections = build_default_map();
        let mut owned: Vec<bool> = vec![false; sections.len()];
        owned[0] = true; // start owning the top-left section
        Self {
            sections,
            current: 0,
            owned,
            boat_target: None,
        }
    }

    pub fn section(&self, id: u32) -> &MapSection {
        &self.sections[id as usize]
    }

    pub fn is_adjacent(&self, from: u32, to: u32) -> bool {
        self.section(from).adjacencies.contains(&to)
    }
}

// ---------- Marker components ----------

#[derive(Component)]
pub struct MapBoat;

#[derive(Component)]
pub struct MapSectionFill {
    pub id: u32,
}

#[derive(Component)]
pub struct MapSectionBoundary;

// ---------- Map authoring ----------

/// Hand-authored 10-section layout. The topology is still 3 + 4 + 3 (so
/// the player gets a roughly-balanced map with predictable adjacencies),
/// but every interior trijunction is *off* the grid lines — strip cuts
/// tilt by a few units, vertical cuts shift sideways — so the layout
/// reads like irregular regions rather than rectangles. The wobble curves
/// along each interior edge add the final hand-drawn feel.
fn build_default_map() -> Vec<MapSection> {
    let m = PLAY_WORLD / 2.0; // 100

    // Trijunctions — corners shared by 3+ sections. Listed as named locals
    // so each polygon can reference the same point and shared edges line
    // up exactly. y-values along each strip cut, x-values along each
    // vertical cut, all jittered off the grid.
    let s12_l   = v(-m,  30.0);    // strip1/2 line at left edge
    let s12_a   = v(-46.0, 42.0);  // S0/S3/S4
    let s12_b   = v(-22.0, 26.0);  // S0/S1/S4
    let s12_c   = v( 12.0, 44.0);  // S1/S4/S5
    let s12_d   = v( 38.0, 24.0);  // S1/S2/S5
    let s12_e   = v( 58.0, 30.0);  // S2/S5/S6
    let s12_r   = v( m,    32.0);  // strip1/2 at right edge

    let s23_l   = v(-m,   -28.0);  // strip2/3 at left edge
    let s23_a   = v(-52.0, -36.0); // S3/S4/S7
    let s23_b   = v(-26.0, -22.0); // S4/S7/S8
    let s23_c   = v(  6.0, -38.0); // S4/S5/S8
    let s23_d   = v( 32.0, -24.0); // S5/S8/S9
    let s23_e   = v( 56.0, -32.0); // S5/S6/S9
    let s23_r   = v( m,   -30.0);  // strip2/3 at right edge

    let top_v_l = v(-26.0,  m);    // top strip vertical S0/S1 hitting top edge
    let top_v_r = v( 36.0,  m);    // top strip vertical S1/S2 hitting top edge
    let bot_v_l = v(-28.0, -m);    // bot strip vertical S7/S8 hitting bottom edge
    let bot_v_r = v( 32.0, -m);    // bot strip vertical S8/S9 hitting bottom edge

    // Cell corner lists (CCW). Each polygon refers to the trijunctions
    // above, so shared edges have *exactly* matching endpoints.
    let cells: [(u32, Vec<Vec2>, Vec2, &[u32]); 10] = [
        // S0 — top-left. Bottom is the strip1/2 cut, transitioning
        // S3 → S4 → S1 from left to right.
        (0,
         vec![v(-m, m), s12_l, s12_a, s12_b, top_v_l],
         v(-66.0, 66.0),
         &[1, 3, 4]),
        // S1 — top-middle. Bottom transitions S4 → S5.
        (1,
         vec![top_v_l, s12_b, s12_c, s12_d, top_v_r],
         v( 4.0, 70.0),
         &[0, 2, 4, 5]),
        // S2 — top-right. Bottom transitions S5 → S6.
        (2,
         vec![top_v_r, s12_d, s12_e, s12_r, v(m, m)],
         v( 70.0, 65.0),
         &[1, 5, 6]),
        // S3 — mid-left.
        (3,
         vec![s12_l, s23_l, s23_a, s12_a],
         v(-77.0, 0.0),
         &[0, 4, 7]),
        // S4 — mid-second. Concave-ish, transitions on both top + bottom.
        (4,
         vec![s12_a, s23_a, s23_b, s23_c, s12_c, s12_b],
         v(-22.0, 4.0),
         &[0, 1, 3, 5, 7, 8]),
        // S5 — mid-third.
        (5,
         vec![s12_c, s23_c, s23_d, s23_e, s12_e, s12_d],
         v( 30.0, -4.0),
         &[1, 2, 4, 6, 8, 9]),
        // S6 — mid-right.
        (6,
         vec![s12_e, s23_e, s23_r, s12_r],
         v( 78.0, 2.0),
         &[2, 5, 9]),
        // S7 — bottom-left. Top transitions S3 → S4.
        (7,
         vec![s23_l, v(-m, -m), bot_v_l, s23_b, s23_a],
         v(-64.0, -64.0),
         &[3, 4, 8]),
        // S8 — bottom-middle. Top transitions S4 → S5.
        (8,
         vec![s23_b, bot_v_l, bot_v_r, s23_d, s23_c],
         v( 2.0, -68.0),
         &[4, 5, 7, 9]),
        // S9 — bottom-right. Top transitions S5 → S6.
        (9,
         vec![s23_d, bot_v_r, v(m, -m), s23_r, s23_e],
         v( 68.0, -64.0),
         &[5, 6, 8]),
    ];

    cells
        .into_iter()
        .map(|(id, corners, center, adj)| MapSection {
            id,
            polygon: build_section_polygon(&corners),
            center,
            adjacencies: adj.to_vec(),
        })
        .collect()
}

#[inline]
fn v(x: f32, y: f32) -> Vec2 { Vec2::new(x, y) }

/// Build the full polygon vertex list from corner points by inserting the
/// shared deterministic wobble between any two corners that lie on an
/// interior boundary. Outer-square edges stay straight so the map fills
/// the play area cleanly.
fn build_section_polygon(corners: &[Vec2]) -> Vec<Vec2> {
    let n = corners.len();
    let mut pts = Vec::with_capacity(n * 5);
    for i in 0..n {
        let a = corners[i];
        let b = corners[(i + 1) % n];
        pts.push(a);
        if !is_outer_edge(a, b) {
            pts.extend(wobble_for_edge(a, b));
        }
    }
    pts
}

fn is_outer_edge(a: Vec2, b: Vec2) -> bool {
    let m = PLAY_WORLD / 2.0;
    let on_left  = (a.x - (-m)).abs() < 0.01 && (b.x - (-m)).abs() < 0.01;
    let on_right = (a.x -  m  ).abs() < 0.01 && (b.x -  m  ).abs() < 0.01;
    let on_bot   = (a.y - (-m)).abs() < 0.01 && (b.y - (-m)).abs() < 0.01;
    let on_top   = (a.y -  m  ).abs() < 0.01 && (b.y -  m  ).abs() < 0.01;
    on_left || on_right || on_bot || on_top
}

/// Deterministic curve points along an interior edge. Both polygons sharing
/// the edge call this with their own (a, b) order — endpoints are sorted
/// canonically and the result is reversed if needed, so the resulting
/// curve is *identical* on both sides of the boundary.
fn wobble_for_edge(a: Vec2, b: Vec2) -> Vec<Vec2> {
    // Canonical order — lex-smaller endpoint goes first.
    let (p, q, reversed) = if (a.x, a.y) <= (b.x, b.y) {
        (a, b, false)
    } else {
        (b, a, true)
    };

    // Phase derived from the canonical endpoints — same edge always gets
    // the same wobble shape regardless of polygon iteration order.
    let phase = p.x * 0.131 + p.y * 0.317 + q.x * 0.713 + q.y * 1.103;
    // Larger amplitude so boundaries read as natural map dividers, not
    // grid lines. Windowed by sin(πt) at the endpoints so corners stay
    // exact (no kinks), and capped relative to the edge length so short
    // edges don't get over-wiggled.
    let amp = 8.0_f32.min(((q - p).length() * 0.18).max(2.0));
    const STEPS: u32 = 8;

    let dir = q - p;
    let len = dir.length();
    let unit = if len > 0.001 { dir / len } else { Vec2::X };
    let perp = Vec2::new(-unit.y, unit.x);

    let mut pts: Vec<Vec2> = (1..STEPS)
        .map(|i| {
            let t = i as f32 / STEPS as f32;
            let along = p + dir * t;
            // Two superimposed sines for an irregular hand-drawn feel.
            let s = (t * std::f32::consts::PI * 2.5 + phase).sin() * 0.65
                  + (t * std::f32::consts::PI * 1.2 + phase * 1.7).cos() * 0.35;
            // Window the wobble so endpoints are exact (no kink at corners).
            let window = (t * std::f32::consts::PI).sin();
            along + perp * (s * amp * window)
        })
        .collect();

    if reversed { pts.reverse(); }
    pts
}

// ---------- Mesh builders ----------

/// Fan-triangulate from the polygon center. All sections in the layout are
/// star-convex from their center, so this is sound.
fn build_section_fill_mesh(polygon: &[Vec2], center: Vec2) -> Mesh {
    let n = polygon.len();
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(n + 1);
    positions.push([center.x, center.y, 0.0]);
    for p in polygon { positions.push([p.x, p.y, 0.0]); }

    let mut indices: Vec<u32> = Vec::with_capacity(n * 3);
    for i in 0..n as u32 {
        let next = if i + 1 == n as u32 { 1 } else { i + 2 };
        indices.push(0);
        indices.push(i + 1);
        indices.push(next);
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

// ---------- Setup ----------

pub fn setup_map(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    pm: Option<Res<PaletteMaterials>>,
    state: Res<MapState>,
) {
    let Some(pm) = pm else { return; };

    // Section fills. Initial material is a placeholder; `update_map_visuals`
    // sets the right one based on owned/current/locked state next frame.
    for section in &state.sections {
        let mesh = build_section_fill_mesh(&section.polygon, section.center);
        commands.spawn((
            Mesh2d(meshes.add(mesh)),
            MeshMaterial2d(pm.hull_accent.clone()),
            Transform::from_xyz(0.0, 0.0, Z_FILL),
            RenderLayers::layer(MAP_LAYER),
            MapSectionFill { id: section.id },
        ));

        // Boundary lines — one thin rectangle per polygon edge, only on
        // interior segments. Use the subtle map_divider material (translucent
        // dark navy) so dividers read as map ink, not as bright outlines.
        // Each shared interior edge ends up drawn twice (once per adjacent
        // polygon), which actually helps it stay legible since alpha
        // accumulates slightly along the seam.
        let line_mesh = meshes.add(Rectangle::new(1.0, 1.0));
        let n = section.polygon.len();
        for i in 0..n {
            let a = section.polygon[i];
            let b = section.polygon[(i + 1) % n];
            if is_outer_edge(a, b) { continue; }
            let mid = (a + b) * 0.5;
            let delta = b - a;
            let len = delta.length();
            if len < 0.05 { continue; }
            let angle = (-delta.x).atan2(delta.y);
            commands.spawn((
                Mesh2d(line_mesh.clone()),
                MeshMaterial2d(pm.map_divider.clone()),
                Transform::from_xyz(mid.x, mid.y, Z_OUTLINE)
                    .with_rotation(Quat::from_rotation_z(angle))
                    .with_scale(Vec3::new(0.7, len, 1.0)),
                RenderLayers::layer(MAP_LAYER),
                MapSectionBoundary,
            ));
        }
    }

    // Map boat — same hull capsule as the player ship, scaled down to read
    // as zoomed-out. Lives only on `MAP_LAYER`.
    let hull_radius = HULL_WIDTH / 2.0;
    let hull_inner  = HULL_LEN - HULL_WIDTH;
    let boat_mesh   = meshes.add(Capsule2d::new(hull_radius, hull_inner));
    let start = state.section(state.current).center;
    commands.spawn((
        Mesh2d(boat_mesh),
        MeshMaterial2d(pm.hull.clone()),
        Transform::from_xyz(start.x, start.y, Z_BOAT)
            .with_scale(Vec3::splat(MAP_BOAT_SCALE)),
        Heading(0.0),
        MapBoat,
        RenderLayers::layer(MAP_LAYER),
    ));
}

// ---------- Per-frame systems ----------

/// Flip the play camera's `RenderLayers` whenever `ViewMode` changes so it
/// shows either the combat world (`PLAY_LAYER`) or the map (`MAP_LAYER`).
/// Single-camera trick — no extra render pass.
pub fn apply_view_mode(
    view: Res<ViewMode>,
    mut q: Query<&mut RenderLayers, With<PlayCamera>>,
) {
    if !view.is_changed() { return; }
    let Ok(mut layers) = q.single_mut() else { return; };
    *layers = match *view {
        ViewMode::Map    => RenderLayers::layer(MAP_LAYER),
        ViewMode::Combat => RenderLayers::layer(PLAY_LAYER),
    };
}

/// Tint each section by ownership only — translucent green for owned,
/// translucent red for everything else. The boat token alone marks the
/// "current" section (no extra highlight needed).
///
/// Only runs when `MapState` changes, so it costs nothing on idle frames.
pub fn update_map_visuals(
    state: Res<MapState>,
    pm: Option<Res<PaletteMaterials>>,
    mut q: Query<(&MapSectionFill, &mut MeshMaterial2d<ColorMaterial>)>,
) {
    if !state.is_changed() { return; }
    let Some(pm) = pm else { return; };

    for (fill, mut mat) in &mut q {
        let want = if state.owned[fill.id as usize] {
            pm.map_owned.clone()
        } else {
            pm.map_enemy.clone()
        };
        if mat.0 != want { mat.0 = want; }
    }
}

/// Click handling — translate cursor → world → containing section. If the
/// section is adjacent to the boat's current one, set it as the boat's
/// move target. Owned-section moves are silent; unowned-section moves
/// will trigger combat once the boat arrives.
pub fn map_click_input(
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    window_mode: Res<WindowMode>,
    view: Res<ViewMode>,
    mut state: ResMut<MapState>,
) {
    if *view != ViewMode::Map { return; }
    if !mouse.just_pressed(MouseButton::Left) { return; }
    if state.boat_target.is_some() { return; } // already moving

    let Ok(win) = windows.single() else { return; };
    let Some(cursor) = win.cursor_position() else { return; };

    let (left, top, size) =
        play_area_screen_rect(win.width(), win.height(), effective_ui_width(&window_mode));
    if cursor.x < left || cursor.x > left + size || cursor.y < top || cursor.y > top + size {
        return;
    }
    let nx = (cursor.x - left) / size;
    let ny = (cursor.y - top) / size;
    let world = Vec2::new((nx - 0.5) * PLAY_WORLD, (0.5 - ny) * PLAY_WORLD);

    let Some(clicked) = state.sections.iter().find(|s| point_in_polygon(world, &s.polygon))
    else { return; };
    let clicked_id = clicked.id;
    if clicked_id == state.current { return; }
    if !state.is_adjacent(state.current, clicked_id) { return; }

    state.boat_target = Some(clicked_id);
}

/// Steer the map boat toward the centroid of its target section using the
/// same turn-then-forward pattern as the in-game ship — just driven by a
/// fixed click target instead of the live cursor.
///
/// Two arrival modes:
/// - **Owned target (green):** sail all the way to the section center,
///   then snap-update `current`. No combat.
/// - **Unowned target (red):** as soon as the boat *crosses* into the
///   target polygon, snap `current` and drop into combat. The boundary
///   crossing is the trigger, not arrival at the centroid — feels much
///   more "you sailed into enemy waters and got intercepted."
pub fn map_boat_movement(
    time: Res<Time>,
    mut state: ResMut<MapState>,
    mut view: ResMut<ViewMode>,
    mut q: Query<(&mut Transform, &mut Heading), With<MapBoat>>,
) {
    let Some(target_id) = state.boat_target else { return; };
    let Ok((mut tf, mut heading)) = q.single_mut() else { return; };

    let dt = time.delta_secs();
    let target_pos = state.section(target_id).center;
    let pos = tf.translation.truncate();
    let to = target_pos - pos;

    // Steer step (mirrors `friendly_movement`'s turn → advance pattern).
    let desired = (-to.x).atan2(to.y);
    let new_h = approach_angle(heading.0, desired, FRIENDLY_TURN_RATE * dt);
    heading.0 = new_h;
    let dir = Vec2::new(-new_h.sin(), new_h.cos());
    let new_pos = pos + dir * FRIENDLY_SPEED * dt;
    tf.translation.x = new_pos.x;
    tf.translation.y = new_pos.y;
    tf.rotation = Quat::from_rotation_z(new_h);

    let target_unowned = !state.owned[target_id as usize];
    if target_unowned {
        // Cross-into-polygon trigger: the moment the boat enters the red
        // zone, swap to combat. Boat parks at its entry point.
        let target_polygon = &state.section(target_id).polygon;
        if point_in_polygon(new_pos, target_polygon) {
            state.current = target_id;
            state.boat_target = None;
            *view = ViewMode::Combat;
        }
    } else if to.length() < ARRIVAL_DIST {
        // Owned target — sail in fully, snap to center.
        tf.translation.x = target_pos.x;
        tf.translation.y = target_pos.y;
        state.current = target_id;
        state.boat_target = None;
    }
}

// ---------- Geometry helpers ----------

/// Standard ray-casting point-in-polygon. Works for the wobbled (but still
/// non-self-intersecting) polygons we hand-author.
fn point_in_polygon(p: Vec2, poly: &[Vec2]) -> bool {
    let n = poly.len();
    if n < 3 { return false; }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let pi = poly[i];
        let pj = poly[j];
        let crosses = (pi.y > p.y) != (pj.y > p.y);
        if crosses {
            let x_at = (pj.x - pi.x) * (p.y - pi.y) / (pj.y - pi.y) + pi.x;
            if p.x < x_at { inside = !inside; }
        }
        j = i;
    }
    inside
}
