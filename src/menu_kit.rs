//! Mesh2d-based UI primitives for chunky-pixel render targets.
//!
//! Parallel to `ui_kit` (which targets bevy_ui Nodes). The main menu
//! and any future screen that renders into a low-resolution image and
//! upscales it (the same pipeline customize uses) draws its buttons /
//! panels as `Mesh2d` rounded rectangles. This module owns the
//! primitives for that: a rounded panel with optional outline ring,
//! and a small palette type for hover/press tinting.
//!
//! Composition split: the spawn helper here is *visual only* — it
//! returns the entities + material handles. Callers attach their own
//! marker components (identifying *which* button this is), spawn the
//! label, and own the hit-test entity. That keeps this module free of
//! menu-specific enums and lets pause / level-up / etc. reuse it later
//! with their own routing.
//!
//! `RenderLayers` is a parameter, not baked in, so the same helper
//! works on the main-menu layer, the customize layer, or anywhere else
//! the same chunky-pixel render target convention applies.

#![allow(dead_code)]

use bevy::prelude::*;
use bevy::render::view::RenderLayers;

/// Look of a chunky rounded panel — fill color, corner radius, and an
/// optional outline ring drawn behind the fill at a slightly larger
/// size. Set `outline_thickness <= 0.0` to skip the outline.
#[derive(Clone, Copy)]
pub struct ChunkyPanelStyle {
    pub fill:              Color,
    pub radius:            f32,
    pub outline_color:     Color,
    /// Outline thickness in spec units. The outline is rendered by
    /// drawing a second rounded panel of `size + 2 * thickness` behind
    /// the fill, so this number is the visible border width on each
    /// side. `0.0` (or negative) disables the outline entirely.
    pub outline_thickness: f32,
}

/// Idle / hover / press fill colors for a button. The outline color
/// gets its own pair too so hover can brighten both the fill *and* the
/// frame in lock-step.
#[derive(Clone, Copy)]
pub struct ChunkyButtonPalette {
    pub idle_fill:     Color,
    pub hover_fill:    Color,
    pub press_fill:    Color,
    pub idle_outline:  Color,
    pub hover_outline: Color,
    pub press_outline: Color,
}

/// Three-way visual state of a button. The caller drives this from
/// its own hover/press detection (cursor source varies per screen) and
/// passes it to `tint_*` to get the matching fill / outline color.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonVisualState { Idle, Hover, Press }

impl ChunkyButtonPalette {
    pub fn tint_fill(self, state: ButtonVisualState) -> Color {
        match state {
            ButtonVisualState::Idle  => self.idle_fill,
            ButtonVisualState::Hover => self.hover_fill,
            ButtonVisualState::Press => self.press_fill,
        }
    }
    pub fn tint_outline(self, state: ButtonVisualState) -> Color {
        match state {
            ButtonVisualState::Idle  => self.idle_outline,
            ButtonVisualState::Hover => self.hover_outline,
            ButtonVisualState::Press => self.press_outline,
        }
    }
}

/// Handles + entities returned by `spawn_chunky_panel`. Callers store
/// these so the per-frame hover/press system can retint the materials
/// (one `materials.get_mut` updates all six meshes that share the
/// handle) and find the children by entity id if they want to inject
/// further tags.
pub struct ChunkyPanelHandles {
    pub fill_material:    Handle<ColorMaterial>,
    pub outline_material: Option<Handle<ColorMaterial>>,
    pub fill_entities:    Vec<Entity>,
    pub outline_entities: Vec<Entity>,
}

/// Spawn a chunky rounded-rect panel. Six meshes share one fill
/// material handle (so a single `materials.get_mut` retints the whole
/// rounded shape on hover). When `style.outline_thickness > 0.0`, six
/// more meshes are spawned *behind* the fill at `size + 2 * thickness`
/// to form the outline ring, sharing their own material handle.
///
/// `z_bg` is the z used by the fill; the outline sits at `z_bg - 0.01`
/// so it never visibly overlaps the fill but the painter's-algorithm
/// renderer still draws them in the right order.
///
/// Caller is responsible for spawning the click label and the
/// dimensionless hit-test entity — this helper deals only with the
/// rounded-rect chrome.
pub fn spawn_chunky_panel(
    commands:     &mut Commands,
    meshes:       &mut Assets<Mesh>,
    materials:    &mut Assets<ColorMaterial>,
    centre:       Vec2,
    size:         Vec2,
    style:        &ChunkyPanelStyle,
    render_layer: usize,
    z_bg:         f32,
) -> ChunkyPanelHandles {
    let radius = style.radius;
    let fill_mat = materials.add(style.fill);
    let layer    = RenderLayers::layer(render_layer);

    let fill_entities    = spawn_rounded_rect(commands, meshes, &fill_mat, centre, size, radius, z_bg, layer.clone());
    let mut outline_material = None;
    let mut outline_entities = Vec::new();
    if style.outline_thickness > 0.0 {
        let t = style.outline_thickness;
        let outer_size   = size + Vec2::splat(t * 2.0);
        let outer_radius = radius + t;
        let outline_mat  = materials.add(style.outline_color);
        outline_entities = spawn_rounded_rect(
            commands, meshes, &outline_mat,
            centre, outer_size, outer_radius,
            z_bg - 0.01,
            layer,
        );
        outline_material = Some(outline_mat);
    }

    ChunkyPanelHandles {
        fill_material:    fill_mat,
        outline_material,
        fill_entities,
        outline_entities,
    }
}

/// Build a rounded rectangle out of six meshes: two crossed rectangles
/// (one full-width minus the corner radius vertically, one full-height
/// minus the corner radius horizontally) plus four corner circles.
/// All six share `mat` so a single material write retints the whole
/// shape.
fn spawn_rounded_rect(
    commands: &mut Commands,
    meshes:   &mut Assets<Mesh>,
    mat:      &Handle<ColorMaterial>,
    centre:   Vec2,
    size:     Vec2,
    radius:   f32,
    z:        f32,
    layer:    RenderLayers,
) -> Vec<Entity> {
    let h_rect_h = (size.y - 2.0 * radius).max(0.0);
    let v_rect_w = (size.x - 2.0 * radius).max(0.0);
    let circle   = meshes.add(Circle::new(radius));
    let h_rect   = meshes.add(Rectangle::new(size.x, h_rect_h));
    let v_rect   = meshes.add(Rectangle::new(v_rect_w, size.y));
    let half     = ((size - Vec2::splat(2.0 * radius)).max(Vec2::ZERO)) * 0.5;

    let mut out = Vec::with_capacity(6);
    let mut push = |mesh: Handle<Mesh>, offset: Vec2| {
        let e = commands.spawn((
            Mesh2d(mesh),
            MeshMaterial2d(mat.clone()),
            Transform::from_translation((centre + offset).extend(z)),
            layer.clone(),
            Visibility::Inherited,
        )).id();
        out.push(e);
    };
    push(h_rect, Vec2::ZERO);
    push(v_rect, Vec2::ZERO);
    for offset in [
        Vec2::new(-half.x, -half.y),
        Vec2::new( half.x, -half.y),
        Vec2::new(-half.x,  half.y),
        Vec2::new( half.x,  half.y),
    ] {
        push(circle.clone(), offset);
    }
    out
}

/// Retint a panel's fill and outline materials to match `state`. Cheap
/// — does a `materials.get_mut` per material and only writes when the
/// color actually changes (so it's safe to call per-frame).
pub fn apply_button_visual_state(
    materials: &mut Assets<ColorMaterial>,
    handles:   &ChunkyPanelHandles,
    palette:   &ChunkyButtonPalette,
    state:     ButtonVisualState,
) {
    let want_fill = palette.tint_fill(state);
    if let Some(m) = materials.get_mut(&handles.fill_material) {
        if m.color != want_fill { m.color = want_fill; }
    }
    if let Some(outline_handle) = handles.outline_material.as_ref() {
        let want_outline = palette.tint_outline(state);
        if let Some(m) = materials.get_mut(outline_handle) {
            if m.color != want_outline { m.color = want_outline; }
        }
    }
}
