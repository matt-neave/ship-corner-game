//! Map authoring + mesh + fill image construction.
//!
//! All the geometry that produces the visible map: hand-authored
//! section corners, deterministic boundary wobble, ribbon dividers,
//! and the pre-rasterized fill sprite that tints owned/enemy zones.
//!
//! Re-rasterization (`refresh_map_fill`) and view-mode camera toggling
//! (`apply_view_mode`) live here too — they're the runtime equivalents
//! of the build-time geometry: same data → same visuals, propagated.

use bevy::image::ImageSampler;
use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::render::render_asset::RenderAssetUsages;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use crate::balance::{PLAY_INTERNAL, PLAY_WORLD};
use crate::palette::{MapCamera, Palette, PlayCamera};

use super::{point_in_polygon, MapFillSprite, MapState, ViewMode};

// Map authoring is procedural — see `procgen::build_random_map`. The
// helpers below (polygon construction, ribbon meshing, boundary
// detection, deterministic wobble) are consumed by procgen and by
// the runtime fill / divider systems.

/// Build the full polygon vertex list from corner points by inserting the
/// shared deterministic wobble between any two corners that lie on an
/// interior boundary. Outer-square edges stay straight so the map fills
/// the play area cleanly.
pub(super) fn build_section_polygon(corners: &[Vec2]) -> Vec<Vec2> {
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

pub fn is_outer_edge(a: Vec2, b: Vec2) -> bool {
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
pub fn wobble_for_edge(a: Vec2, b: Vec2) -> Vec<Vec2> {
    let (p, q, reversed) = if (a.x, a.y) <= (b.x, b.y) {
        (a, b, false)
    } else {
        (b, a, true)
    };

    let phase = p.x * 0.131 + p.y * 0.317 + q.x * 0.713 + q.y * 1.103;
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
            let s = (t * std::f32::consts::PI * 2.5 + phase).sin() * 0.65
                  + (t * std::f32::consts::PI * 1.2 + phase * 1.7).cos() * 0.35;
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
pub fn build_ribbon_mesh(points: &[Vec2], width: f32) -> Mesh {
    let n = points.len();
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    if n < 2 { return mesh; }

    let half_w = width * 0.5;
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(n * 2);

    for i in 0..n {
        let p = points[i];
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
/// image. One pixel per internal-resolution play pixel, color picked by
/// point-in-polygon against the section list.
///
/// Tints are *baked against the current ocean color*: instead of writing a
/// translucent green/red and letting the GPU alpha-blend it over the ocean
/// clear, we pre-mix the tint with `palette.ocean` here and emit opaque
/// pixels. Reason: the GPU blend is hue-shifted by the ocean (e.g. red over
/// daytime light-blue ocean reads as purple), so tints looked
/// palette-dependent. Pre-mixing at a high tint weight keeps the tint
/// dominant and consistent across day/night ocean colors.
pub fn build_map_fill_image(state: &MapState, palette: &Palette) -> Image {
    let w = PLAY_INTERNAL;
    let h = PLAY_INTERNAL;
    let mut data = vec![0u8; (w * h * 4) as usize];

    let owned = blend_to_rgba(palette.ocean, Color::srgb(0.18, 0.98, 0.40), 0.70);
    let enemy = blend_to_rgba(palette.ocean, Color::srgb(1.00, 0.05, 0.15), 0.70);
    let transparent: [u8; 4] = [0, 0, 0, 0];

    for py in 0..h {
        for px in 0..w {
            let world_x = (px as f32 + 0.5) / w as f32 * PLAY_WORLD - PLAY_WORLD / 2.0;
            let world_y = ((h - py) as f32 - 0.5) / h as f32 * PLAY_WORLD - PLAY_WORLD / 2.0;
            let pos = Vec2::new(world_x, world_y);

            let mut color = transparent;
            for section in &state.sections {
                if point_in_polygon(pos, &section.polygon) {
                    color = if state.owned[section.id as usize] { owned } else { enemy };
                    break;
                }
            }

            let i = ((py * w + px) * 4) as usize;
            data[i + 0] = color[0];
            data[i + 1] = color[1];
            data[i + 2] = color[2];
            data[i + 3] = color[3];
        }
    }

    // Rgba8UnormSrgb so WebGL2 / ANGLE can sample this sprite —
    // sampling a Bgra8 sRGB texture is unreliable on those backends
    // and leaves the green / red territory tints invisible on web.
    let mut img = Image::new(
        Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    img.sampler = ImageSampler::nearest();
    img
}

/// Mix `tint` into `base` at weight `t` (0=base, 1=tint) and return RGBA
/// bytes for the `Rgba8UnormSrgb` format.
fn blend_to_rgba(base: Color, tint: Color, t: f32) -> [u8; 4] {
    let b: bevy::color::Srgba = base.into();
    let n: bevy::color::Srgba = tint.into();
    let r  = (b.red   * (1.0 - t) + n.red   * t).clamp(0.0, 1.0);
    let g  = (b.green * (1.0 - t) + n.green * t).clamp(0.0, 1.0);
    let bl = (b.blue  * (1.0 - t) + n.blue  * t).clamp(0.0, 1.0);
    [
        (r  * 255.0).round() as u8,
        (g  * 255.0).round() as u8,
        (bl * 255.0).round() as u8,
        255,
    ]
}

// ---------- Runtime: view mode + fill refresh ----------

/// Toggle which of the two play-target cameras is active. PlayCamera owns
/// `PLAY_LAYER`, MapCamera owns `MAP_LAYER`; both target the same render
/// image. Only the active one renders, so the inactive layer's entities
/// can't bleed through.
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
/// system, but it's the single hook to use later).
#[allow(dead_code)]
pub fn regenerate_map_fill_image(
    state: &MapState,
    palette: &Palette,
    images: &mut Assets<Image>,
    sprite: &mut Sprite,
) {
    let img = build_map_fill_image(state, palette);
    sprite.image = images.add(img);
}

/// Re-rasterize the map fill image whenever the palette changes (the
/// night-mode toggle swaps `palette.ocean`) or when section ownership
/// flips. Necessary because tints are pre-mixed against the ocean color
/// in `build_map_fill_image`, so a stale image would show the old blend
/// after either of those updates.
///
/// Owned-state diffing uses a `Local<Vec<bool>>` snapshot rather than
/// `state.is_changed()` so we don't rebuild the 200×200 image on every
/// frame the boat moves (which mutates `MapState` continuously).
pub fn refresh_map_fill(
    palette: Res<Palette>,
    state: Res<MapState>,
    mut images: ResMut<Assets<Image>>,
    mut q: Query<&mut Sprite, With<MapFillSprite>>,
    mut owned_snapshot: Local<Vec<bool>>,
) {
    let palette_changed = palette.is_changed();
    let owned_changed = if owned_snapshot.len() != state.owned.len() {
        *owned_snapshot = state.owned.clone();
        false
    } else if owned_snapshot.as_slice() != state.owned.as_slice() {
        *owned_snapshot = state.owned.clone();
        true
    } else {
        false
    };
    if !palette_changed && !owned_changed { return; }

    let Ok(mut sprite) = q.single_mut() else { return; };
    let img = build_map_fill_image(&state, &palette);
    sprite.image = images.add(img);
}
