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

use bevy::image::ImageSampler;
use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::render::render_asset::RenderAssetUsages;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::render::view::RenderLayers;
use bevy::window::PrimaryWindow;

use crate::balance::{
    FRIENDLY_SPEED, FRIENDLY_TURN_RATE, HULL_LEN, HULL_WIDTH, PLAY_INTERNAL, PLAY_WORLD,
};
use crate::components::Heading;
use crate::modes::{effective_ui_width, play_area_screen_rect, WindowMode};
use crate::palette::{MapCamera, PaletteMaterials, PlayCamera};
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
    /// Original CCW corner points (no wobble). Used to enumerate distinct
    /// boundary edges for ribbon-divider rendering and adjacency-deriving.
    pub corners: Vec<Vec2>,
    /// CCW polygon vertices, including curved-boundary intermediate points.
    /// This is the mesh-fill polygon (corners + per-edge wobble baked in).
    pub polygon: Vec<Vec2>,
    /// Center point — both visual (fan-tri pivot) and the boat's start
    /// position when the section is first owned.
    pub center: Vec2,
    /// Sections this one shares a boundary with. Currently unused at
    /// runtime — kept for future gating (e.g., fog-of-war, AI movement,
    /// "must be adjacent to capture") without re-deriving from geometry.
    #[allow(dead_code)]
    pub adjacencies: Vec<u32>,
}

#[derive(Resource)]
pub struct MapState {
    pub sections: Vec<MapSection>,
    /// Section the boat is *currently inside*. Updated each frame by
    /// `map_boat_movement` based on point-in-polygon containment.
    pub current: u32,
    /// Indexed by section id.
    pub owned: Vec<bool>,
    /// World-space click target the boat is sailing toward, if any. Cleared
    /// on arrival or when the boat enters an unowned (red) zone.
    pub boat_target: Option<Vec2>,
}

impl MapState {
    pub fn new() -> Self {
        let sections = build_default_map();
        let mut owned: Vec<bool> = vec![false; sections.len()];
        owned[0] = true; // start owning the top-left section
        Self { sections, current: 0, owned, boat_target: None }
    }

    pub fn section(&self, id: u32) -> &MapSection {
        &self.sections[id as usize]
    }
}

// ---------- Marker components ----------

#[derive(Component)]
pub struct MapBoat;

/// Marker on the single sprite that displays the pre-rasterized section
/// fill image. We render the entire map fill as one sprite (one quad,
/// one draw call) instead of per-section meshes — that way alpha
/// rendering can't produce hairline seams between fan-triangle edges,
/// which is what was causing visible "rays" through the tints.
#[derive(Component)]
pub struct MapFillSprite;

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
        .map(|(id, corners, center, adj)| {
            let polygon = build_section_polygon(&corners);
            MapSection {
                id,
                corners,
                polygon,
                center,
                adjacencies: adj.to_vec(),
            }
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

/// Build a single mitered triangle-strip "ribbon" tracing `points` with
/// uniform `width`. Each interior vertex uses the average of incoming and
/// outgoing segment perpendiculars, so the ribbon bends smoothly along the
/// curve instead of reading as rotated rectangles meeting at sharp corners.
fn build_ribbon_mesh(points: &[Vec2], width: f32) -> Mesh {
    let n = points.len();
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    if n < 2 { return mesh; }

    let half_w = width * 0.5;
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(n * 2);

    for i in 0..n {
        let p = points[i];
        // Perpendicular at this point. Endpoints use the single adjacent
        // segment's perpendicular; interior points average incoming +
        // outgoing — that's a simple miter join, smooth for shallow curves.
        let perp = if i == 0 {
            let d = (points[1] - points[0]).normalize_or_zero();
            Vec2::new(-d.y, d.x)
        } else if i == n - 1 {
            let d = (points[n - 1] - points[n - 2]).normalize_or_zero();
            Vec2::new(-d.y, d.x)
        } else {
            let d_in  = (points[i] - points[i - 1]).normalize_or_zero();
            let d_out = (points[i + 1] - points[i]).normalize_or_zero();
            let d_avg = (d_in + d_out).normalize_or_zero();
            Vec2::new(-d_avg.y, d_avg.x)
        };

        positions.push([p.x + perp.x * half_w, p.y + perp.y * half_w, 0.0]);
        positions.push([p.x - perp.x * half_w, p.y - perp.y * half_w, 0.0]);
    }

    let mut indices: Vec<u32> = Vec::with_capacity((n - 1) * 6);
    for i in 0..(n - 1) as u32 {
        let i0 = 2 * i;
        let i1 = 2 * i + 1;
        let i2 = 2 * i + 2;
        let i3 = 2 * i + 3;
        // Two triangles per quad, both CCW.
        indices.extend_from_slice(&[i0, i1, i2, i1, i3, i2]);
    }

    let normals: Vec<[f32; 3]> = vec![[0.0, 0.0, 1.0]; positions.len()];
    let uvs:     Vec<[f32; 2]> = vec![[0.0, 0.0];      positions.len()];
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL,   normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0,     uvs);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// Pre-rasterize the section fills into a single `PLAY_INTERNAL × PLAY_INTERNAL`
/// sRGBA image. One pixel per internal-resolution play pixel, color +
/// alpha picked by point-in-polygon against the section list. The result
/// is shown via a single sprite, so alpha-blending happens once per pixel
/// over the camera clear (ocean) — no fan-triangle seams, ever.
///
/// State changes (e.g., capturing a section) should rebuild this image
/// and re-set the sprite's texture; that hook isn't wired yet because no
/// capture mechanic exists, but `regenerate_map_fill_image` is shaped so
/// it can be called from a state-change handler later.
fn build_map_fill_image(state: &MapState) -> Image {
    let w = PLAY_INTERNAL;
    let h = PLAY_INTERNAL;
    let mut data = vec![0u8; (w * h * 4) as usize];

    // Tint colors as sRGBA bytes. Match the previous `map_owned` /
    // `map_enemy` ColorMaterial values exactly.
    let owned: [u8; 4] = [
        (0.18_f32 * 255.0).round() as u8, // R
        (0.98_f32 * 255.0).round() as u8, // G
        (0.40_f32 * 255.0).round() as u8, // B
        (0.35_f32 * 255.0).round() as u8, // A
    ];
    let enemy: [u8; 4] = [
        (1.00_f32 * 255.0).round() as u8,
        (0.05_f32 * 255.0).round() as u8,
        (0.15_f32 * 255.0).round() as u8,
        (0.35_f32 * 255.0).round() as u8,
    ];

    for py in 0..h {
        for px in 0..w {
            // Pixel center → world coords. Image y=0 is the top row, world y
            // is up, so flip y.
            let world_x = (px as f32 + 0.5) / w as f32 * PLAY_WORLD - PLAY_WORLD / 2.0;
            let world_y = ((h - py) as f32 - 0.5) / h as f32 * PLAY_WORLD - PLAY_WORLD / 2.0;
            let pos = Vec2::new(world_x, world_y);

            let mut color = [0u8, 0, 0, 0]; // transparent (lets ocean show)
            for section in &state.sections {
                if point_in_polygon(pos, &section.polygon) {
                    color = if state.owned[section.id as usize] { owned } else { enemy };
                    break;
                }
            }

            // BGRA byte order to match `Bgra8UnormSrgb`.
            let i = ((py * w + px) * 4) as usize;
            data[i + 0] = color[2]; // B
            data[i + 1] = color[1]; // G
            data[i + 2] = color[0]; // R
            data[i + 3] = color[3]; // A
        }
    }

    let mut img = Image::new(
        Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::Bgra8UnormSrgb,
        RenderAssetUsages::default(),
    );
    img.sampler = ImageSampler::nearest();
    img
}

// ---------- Setup ----------

pub fn setup_map(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    pm: Option<Res<PaletteMaterials>>,
    state: Res<MapState>,
) {
    let Some(pm) = pm else { return; };

    // Section fills — one pre-rasterized sprite for the entire map. Single
    // quad rendering = no per-triangle seams in the alpha blend, no matter
    // the alpha value or section shape.
    let fill_handle = images.add(build_map_fill_image(&state));
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
    // shared by exactly two sections) so the divider draws once and the
    // wobble curve looks like a single hand-drawn line instead of a
    // staircase of rotated rectangles. Quantizes corner coordinates to
    // sidestep floating-point key drift in the dedupe set.
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

            // Path through the wobble: [a, w0..wN, b].
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

/// `run_if` predicate for systems that should only tick during combat.
/// Pauses enemy spawning, AI, bullets, fire/frost ticks, etc. while the
/// player is on the map — keeps the world frozen until they re-enter.
pub fn in_combat_view(view: Res<ViewMode>) -> bool {
    *view == ViewMode::Combat
}

/// Toggle which of the two play-target cameras is active. PlayCamera owns
/// `PLAY_LAYER`, MapCamera owns `MAP_LAYER`; both target the same render
/// image. Only the active one renders, so the inactive layer's entities
/// can't bleed through (any seeming bleed-through under `RenderLayers`
/// swap was traced to using a single camera + change-detection).
pub fn apply_view_mode(
    view: Res<ViewMode>,
    mut play_q: Query<&mut Camera, (With<PlayCamera>, Without<MapCamera>)>,
    mut map_q:  Query<&mut Camera, (With<MapCamera>, Without<PlayCamera>)>,
) {
    let want_combat = matches!(*view, ViewMode::Combat);
    if let Ok(mut cam) = play_q.single_mut() {
        if cam.is_active != want_combat { cam.is_active = want_combat; }
    }
    if let Ok(mut cam) = map_q.single_mut() {
        let want_map = !want_combat;
        if cam.is_active != want_map { cam.is_active = want_map; }
    }
}

/// Rebuild the map fill sprite's image. Call this when `MapState.owned`
/// changes (no capture mechanic exists yet, so it isn't wired into a
/// system, but it's the single hook to use later — re-rasterize and swap
/// the texture handle on the existing `MapFillSprite` entity).
#[allow(dead_code)]
pub fn regenerate_map_fill_image(
    state: &MapState,
    images: &mut Assets<Image>,
    sprite: &mut Sprite,
) {
    let img = build_map_fill_image(state);
    sprite.image = images.add(img);
}

/// Click handler — set the boat's world-space target wherever the player
/// clicked inside the play area. No adjacency/section restriction: you
/// can click anywhere on the map and the boat will sail there directly.
/// Crossing into a red section along the way will trigger combat.
pub fn map_click_input(
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    window_mode: Res<WindowMode>,
    view: Res<ViewMode>,
    mut state: ResMut<MapState>,
) {
    if *view != ViewMode::Map { return; }
    if !mouse.just_pressed(MouseButton::Left) { return; }
    let Ok(win) = windows.single() else { return; };
    let Some(c) = win.cursor_position() else { return; };

    let (left, top, size) =
        play_area_screen_rect(win.width(), win.height(), effective_ui_width(&window_mode));
    if c.x < left || c.x > left + size || c.y < top || c.y > top + size { return; }
    let nx = (c.x - left) / size;
    let ny = (c.y - top) / size;
    state.boat_target = Some(Vec2::new((nx - 0.5) * PLAY_WORLD, (0.5 - ny) * PLAY_WORLD));
}

/// Steer the boat toward `state.boat_target` using the same turn-then-
/// advance pattern as the in-combat ship. Click sets the target; the boat
/// sails there *only* — it doesn't continuously chase the cursor.
///
/// Each frame, after moving, point-in-polygon-test the boat against the
/// section list. On a *transition* (boat crossed into a different section
/// than `state.current`), update `state.current`. If the new section is
/// unowned (red), drop into combat immediately and clear the target so
/// the boat doesn't auto-resume sailing when the player returns to map.
pub fn map_boat_movement(
    time: Res<Time>,
    mut state: ResMut<MapState>,
    mut view: ResMut<ViewMode>,
    mut q: Query<(&mut Transform, &mut Heading), With<MapBoat>>,
) {
    if *view != ViewMode::Map { return; }
    let Ok((mut tf, mut heading)) = q.single_mut() else { return; };
    let dt = time.delta_secs();

    if let Some(tgt) = state.boat_target {
        let pos = tf.translation.truncate();
        let to = tgt - pos;
        if to.length() < 1.0 {
            // Arrived — stop, clear target.
            state.boat_target = None;
        } else {
            let desired = (-to.x).atan2(to.y);
            let new_h = approach_angle(heading.0, desired, FRIENDLY_TURN_RATE * dt);
            heading.0 = new_h;
            let dir = Vec2::new(-new_h.sin(), new_h.cos());
            let new_pos = pos + dir * FRIENDLY_SPEED * dt;
            let half = PLAY_WORLD / 2.0;
            tf.translation.x = new_pos.x.clamp(-half, half);
            tf.translation.y = new_pos.y.clamp(-half, half);
            tf.rotation = Quat::from_rotation_z(new_h);
        }
    }

    // Section-transition check.
    let now_pos = tf.translation.truncate();
    let containing = state
        .sections
        .iter()
        .find(|s| point_in_polygon(now_pos, &s.polygon))
        .map(|s| s.id);
    if let Some(id) = containing {
        if id != state.current {
            state.current = id;
            if !state.owned[id as usize] {
                state.boat_target = None;
                *view = ViewMode::Combat;
            }
        }
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
