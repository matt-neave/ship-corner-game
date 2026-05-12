//! Pixel-art dockyard scene for the hull-select screen.
//!
//! Mirrors the customize-overlay render pipeline: a dedicated camera
//! draws everything on `DOCKYARD_LAYER` to a low-res image, which is
//! then displayed on `UPSCALE_LAYER` via a nearest-neighbor upscaled
//! sprite. The same chunky-pixel rasterization that the in-game
//! combat view uses, so ships in the dockyard read as the same
//! visual language as ships in battle.
//!
//! Scene layout (in dockyard-internal coords, centred at origin):
//!
//!   * Solid ocean-blue clear colour fills the canvas.
//!   * Two horizontal wooden walkways span the left ~60% of the
//!     canvas; the right ~40% is intentionally bare — the Bevy UI
//!     parchment manifest overlays it.
//!   * Eight `DockyardBerth` ships sit alongside the walkways in a
//!     4×2 grid (one per `Hull` variant), each with a body capsule,
//!     a couple of turret discs, and a sail/banner accent.
//!   * Bevy UI text labels float above each berth at the right
//!     screen position; the rebuild on selection change swaps the
//!     active-ship border colour.
//!
//! Click handling: a per-berth `HitArea` + the `DockyardViewport`
//! window→spec converter lets a normal click on a parked ship resolve
//! to a `Hull`, which is committed to `SelectedHull`.

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::camera::RenderTarget;
use bevy::render::render_resource::{
    Extent3d, TextureDimension, TextureFormat, TextureUsages,
};
use bevy::render::view::{Msaa, RenderLayers};
use bevy::window::PrimaryWindow;

use crate::balance::{
    DOCKYARD_INTERNAL_H, DOCKYARD_INTERNAL_W, DOCKYARD_LAYER, UPSCALE_LAYER,
};
use crate::hull::{Hull, PreviewHull, SelectedHull};
use crate::modes::{effective_ui_width, WindowMode};
use crate::AppState;

/// Visible width of the dockyard scene as a fraction of the upscale
/// target. The right slice is left empty so the parchment manifest
/// (Bevy UI) covers it cleanly.
const SCENE_LEFT_FRACTION: f32 = 0.65;

/// Internal pixels of margin around each ship's hit area beyond its
/// rendered silhouette. Generous so clicks just outside the hull
/// still register — the alternative is missing the small Corsair /
/// Glass Cannon hulls.
const SHIP_HIT_PAD: f32 = 6.0;

/// Marker for the dockyard render-target camera.
#[derive(Component)]
pub struct DockyardCamera;

/// Marker for the upscaled display sprite (on `UPSCALE_LAYER`).
#[derive(Component)]
pub struct DockyardDisplaySprite;

/// Marker for the opaque backdrop that hides the play-world sprite
/// while the dockyard is up.
#[derive(Component)]
pub struct DockyardBackdrop;

/// Marker on every scene entity owned by the dockyard pixel view
/// (walkways, finger piers, ship bodies, turrets, banners). Used by
/// the OnExit(HullSelect) despawn so the scene contents live only
/// while the player is on the dockyard — game and dockyard stay
/// fully distinct rather than relying on visibility toggles.
///
/// Excludes the render-target plumbing (camera, image, display sprite,
/// backdrop) which is created once at startup and kept warm.
#[derive(Component)]
pub struct DockyardSceneEntity;

/// Per-ship marker. The hull this berth represents + a cached hit
/// half-extent (in internal/spec pixels) so `handle_dockyard_click`
/// doesn't have to re-derive it from `hull_silhouette`.
#[derive(Component, Clone, Copy)]
pub struct DockyardBerth {
    pub hull: Hull,
    pub hit_half: Vec2,
}

/// Marker on the berth body capsule — re-tinted on selection change
/// so the active ship glows.
#[derive(Component, Clone, Copy)]
pub struct DockyardBerthBody(pub Hull);

/// Marker on the floating Bevy UI text node above each berth. Driven
/// by `update_dockyard_labels` to follow the ship's screen position
/// as the window resizes.
#[derive(Component, Clone, Copy)]
pub struct DockyardLabel(pub Hull);

/// Window↔dockyard-spec coord mapping. Refreshed by
/// `resize_dockyard_display` each frame the window resizes so the
/// click + label-position code stay aligned with the upscaled sprite.
#[derive(Resource, Default, Clone, Copy)]
pub struct DockyardViewport {
    pub display_origin: Vec2,
    pub display_scale: f32,
}

impl DockyardViewport {
    /// Convert a window-space cursor position to dockyard-spec coords
    /// (centred at origin, +Y up, in internal pixels). Returns `None`
    /// when the cursor is outside the rendered area.
    pub fn window_to_spec(&self, cursor: Vec2) -> Option<Vec2> {
        if self.display_scale <= 0.0 { return None; }
        let local = (cursor - self.display_origin) / self.display_scale;
        let w = DOCKYARD_INTERNAL_W as f32;
        let h = DOCKYARD_INTERNAL_H as f32;
        if local.x < 0.0 || local.x > w || local.y < 0.0 || local.y > h {
            return None;
        }
        Some(Vec2::new(local.x - w * 0.5, h * 0.5 - local.y))
    }

    /// Inverse of `window_to_spec`. Used by `update_dockyard_labels`
    /// to pin Bevy UI text above each berth.
    pub fn spec_to_window(&self, spec: Vec2) -> Vec2 {
        let local = Vec2::new(
            spec.x + DOCKYARD_INTERNAL_W as f32 * 0.5,
            DOCKYARD_INTERNAL_H as f32 * 0.5 - spec.y,
        );
        self.display_origin + local * self.display_scale
    }
}

// ---------- Layout helpers ----------

/// Per-hull berth position in spec coords. Marina layout: 4 columns ×
/// 2 rows of vertical slips, sitting between top/middle/bottom
/// horizontal quay walkways. Top row's ships moor bow-up; bottom
/// row's moor bow-down so both rows face an adjacent main pier.
fn berth_pos(hull: Hull) -> Vec2 {
    let canvas_w = DOCKYARD_INTERNAL_W as f32;
    let x_min = -canvas_w * 0.5 + 28.0;
    let x_max = canvas_w * (SCENE_LEFT_FRACTION - 0.5) - 18.0;
    let cols = 4.0;
    let col_step = (x_max - x_min) / (cols - 1.0);
    let (col, row) = grid_cell(hull);
    let x = x_min + col as f32 * col_step;
    // Row centers chosen to sit between the top↔middle and middle↔bottom
    // quay walkways (see `spawn_dockyard_scene` for the y values).
    let y = if row == 0 { 42.0 } else { -42.0 };
    Vec2::new(x, y)
}

/// Y positions of the three horizontal quay walkways (top, middle,
/// bottom) and the half-thickness of each. Used both by
/// `spawn_dockyard_scene` (mesh placement) and `berth_pos` (slip
/// alignment). Kept in one place so a quay-spacing tweak doesn't
/// drift out of sync.
const QUAY_Y_TOP: f32 = 84.0;
const QUAY_Y_MID: f32 = 0.0;
const QUAY_Y_BOT: f32 = -84.0;
const QUAY_THICKNESS: f32 = 8.0;
/// Vertical finger-pier thickness (X width) and length (Y span).
const FINGER_W: f32 = 3.0;
/// Half-length of a finger pier — reaches from a row's quay edge to
/// just past the ship hull on the other side.
const FINGER_HALF_LEN: f32 = 38.0;

/// (column, row) for a hull's berth — declaration order over a 4×2
/// grid. Tier-1 hulls sit on the top wharf, tier-2 below.
fn grid_cell(hull: Hull) -> (u8, u8) {
    match hull {
        Hull::Default     => (0, 0),
        Hull::GlassCannon => (1, 0),
        Hull::Rammer      => (2, 0),
        Hull::Dreadnought => (3, 0),
        Hull::Privateer   => (0, 1),
        Hull::Corsair     => (1, 1),
        Hull::Harpooner   => (2, 1),
        Hull::Revenant    => (3, 1),
    }
}

/// Hull silhouette dims in dockyard-spec pixels: `(body_color,
/// length, width)`. Smaller than the in-game hull (~22 long) so all
/// eight fit comfortably; relative size differences match the
/// `hull::hull_silhouette` baseline.
fn berth_silhouette(hull: Hull) -> (Color, f32, f32) {
    match hull {
        Hull::Default     => (Color::srgb(0.75, 0.78, 0.84), 18.0, 5.0),
        Hull::GlassCannon => (Color::srgb(0.55, 0.85, 0.90), 20.0, 4.0),
        Hull::Rammer      => (Color::srgb(0.65, 0.55, 0.45), 19.0, 6.5),
        Hull::Dreadnought => (Color::srgb(0.40, 0.45, 0.50), 22.0, 7.5),
        Hull::Privateer   => (Color::srgb(0.90, 0.55, 0.30), 19.0, 5.0),
        Hull::Corsair     => (Color::srgb(0.85, 0.80, 0.40), 21.0, 4.0),
        Hull::Harpooner   => (Color::srgb(0.50, 0.70, 0.95), 22.0, 4.5),
        Hull::Revenant    => (Color::srgb(0.60, 0.75, 0.85), 19.0, 4.5),
    }
}

/// Border tint for an inactive vs active berth body. Active glows
/// rope-yellow to match the manifest panel's accent.
fn body_tint(hull: Hull, selected: bool, hover: bool) -> Color {
    if selected {
        Color::srgb(0.95, 0.78, 0.30)
    } else if hover {
        // Slightly brighter than base on hover so the cursor feedback
        // reads without committing to a selection.
        let (c, _, _) = berth_silhouette(hull);
        let bevy::color::Srgba { red, green, blue, .. } = c.into();
        Color::srgb(
            (red + 0.10).min(1.0),
            (green + 0.10).min(1.0),
            (blue + 0.10).min(1.0),
        )
    } else {
        berth_silhouette(hull).0
    }
}

// ---------- Setup ----------

pub fn setup_dockyard_render(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
) {
    let size = Extent3d {
        width: DOCKYARD_INTERNAL_W,
        height: DOCKYARD_INTERNAL_H,
        depth_or_array_layers: 1,
    };
    let mut img = Image::new_fill(
        size,
        TextureDimension::D2,
        &[0, 0, 0, 0],
        TextureFormat::Bgra8UnormSrgb,
        RenderAssetUsages::default(),
    );
    img.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING
        | TextureUsages::COPY_DST
        | TextureUsages::RENDER_ATTACHMENT;
    img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor::nearest());
    let handle = images.add(img);

    commands.spawn((
        Camera2d,
        Camera {
            target: RenderTarget::Image(handle.clone().into()),
            // Harbour blue — slightly cooler than the play-world ocean
            // so the dockyard reads as a sheltered port distinct from
            // open sea.
            clear_color: ClearColorConfig::Custom(Color::srgb(0.18, 0.32, 0.48)),
            order: -3,
            is_active: false,
            ..default()
        },
        Projection::Orthographic(OrthographicProjection {
            scaling_mode: bevy::render::camera::ScalingMode::Fixed {
                width: DOCKYARD_INTERNAL_W as f32,
                height: DOCKYARD_INTERNAL_H as f32,
            },
            ..OrthographicProjection::default_2d()
        }),
        RenderLayers::layer(DOCKYARD_LAYER),
        Msaa::Off,
        DockyardCamera,
    ));

    // Opaque backdrop so the play sprite + HUD don't bleed through
    // when the dockyard is up. Lives on UPSCALE_LAYER between the
    // play sprite (z<2) and the dockyard display (z=2.5).
    commands.spawn((
        Sprite {
            color: Color::srgb(0.10, 0.11, 0.13),
            custom_size: Some(Vec2::new(4096.0, 4096.0)),
            ..default()
        },
        Transform::from_xyz(0.0, 0.0, 2.2),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        DockyardBackdrop,
    ));

    commands.spawn((
        Sprite {
            image: handle,
            custom_size: Some(Vec2::new(
                DOCKYARD_INTERNAL_W as f32 * 4.0,
                DOCKYARD_INTERNAL_H as f32 * 4.0,
            )),
            ..default()
        },
        Transform::from_xyz(0.0, 0.0, 2.5),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        DockyardDisplaySprite,
    ));

    commands.insert_resource(DockyardViewport::default());
}

/// Spawn the static dock scene (marina-style finger-pier slips) + a
/// vertical ship per hull. Runs once at startup so the entities are
/// warm when the player first enters HullSelect.
///
/// Layout (top-down marina): three horizontal quay walkways (top,
/// middle, bottom) span the scene; between each adjacent pair of
/// quays sits a row of four slips, each bounded on left and right by
/// a short vertical finger pier. Ships are moored bow-toward the
/// closer quay (top row bow-up, bottom row bow-down) so the harbour
/// reading is unambiguous.
pub fn spawn_dockyard_scene(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
) {
    let plank_dark = materials.add(Color::srgb(0.32, 0.20, 0.10));
    let plank_light = materials.add(Color::srgb(0.48, 0.32, 0.16));
    let finger_mat = materials.add(Color::srgb(0.38, 0.24, 0.12));
    let canvas_w = DOCKYARD_INTERNAL_W as f32;

    // ---- Horizontal quay walkways (top, middle, bottom) ----
    let walkway_w = canvas_w * SCENE_LEFT_FRACTION;
    let walkway_mesh = meshes.add(Rectangle::new(walkway_w, QUAY_THICKNESS));
    let walkway_x = -canvas_w * 0.5 + walkway_w * 0.5;
    for (y, mat) in [
        (QUAY_Y_TOP, plank_light.clone()),
        (QUAY_Y_MID, plank_dark.clone()),
        (QUAY_Y_BOT, plank_light.clone()),
    ] {
        commands.spawn((
            Mesh2d(walkway_mesh.clone()),
            MeshMaterial2d(mat),
            Transform::from_xyz(walkway_x, y, 0.5),
            RenderLayers::layer(DOCKYARD_LAYER),
            DockyardSceneEntity,
        ));
    }

    // ---- Vertical finger piers between adjacent slips ----
    // 5 piers per row (left wall of slip 0, between 0/1, 1/2, 2/3,
    // right wall of slip 3). Same x positions for both rows.
    let cols = 4i32;
    let x_min = -canvas_w * 0.5 + 28.0;
    let x_max = canvas_w * (SCENE_LEFT_FRACTION - 0.5) - 18.0;
    let col_step = (x_max - x_min) / (cols as f32 - 1.0);
    let finger_mesh = meshes.add(Rectangle::new(FINGER_W, FINGER_HALF_LEN * 2.0));
    for col in 0..=cols {
        // Halfway between adjacent ships — `col == 0` and `col == cols`
        // give the outer walls of the row.
        let x = x_min + (col as f32 - 0.5) * col_step;
        for &row_y in &[42.0, -42.0] {
            commands.spawn((
                Mesh2d(finger_mesh.clone()),
                MeshMaterial2d(finger_mat.clone()),
                Transform::from_xyz(x, row_y, 0.6),
                RenderLayers::layer(DOCKYARD_LAYER),
                DockyardSceneEntity,
            ));
        }
    }

    // ---- Per-hull ship meshes ----
    // Vertical orientation: the capsule's default +Y long axis IS the
    // ship's bow direction — no rotation needed. Turrets stack along
    // the deck (body-local Y), banner sits centred on the mast.
    let turret_mesh = meshes.add(Circle::new(1.2));
    let turret_mat = materials.add(Color::srgb(0.30, 0.32, 0.38));
    let banner_mat = materials.add(Color::srgb(0.90, 0.90, 0.95));
    let banner_mesh = meshes.add(Rectangle::new(0.8, 3.0));

    for &hull in HULL_ORDER {
        let pos = berth_pos(hull);
        let (color, length, width) = berth_silhouette(hull);
        let body_mat = materials.add(color);
        // Vertical hit area: x = ship width (+ pad), y = ship length (+ pad).
        let hit_half = Vec2::new(width * 0.5 + SHIP_HIT_PAD, length * 0.5 + SHIP_HIT_PAD);
        let (_, row) = grid_cell(hull);
        // Bow toward the nearer quay: row 0 (upper) faces up, row 1
        // faces down. Capsule mesh is symmetric so this is purely
        // for the turret/banner stacking direction below.
        let bow_up = row == 0;

        let body_mesh = meshes.add(Capsule2d::new(width * 0.5, length - width));
        let ship = commands.spawn((
            Mesh2d(body_mesh),
            MeshMaterial2d(body_mat),
            Transform::from_xyz(pos.x, pos.y, 1.0),
            RenderLayers::layer(DOCKYARD_LAYER),
            DockyardBerth { hull, hit_half },
            DockyardBerthBody(hull),
            DockyardSceneEntity,
        )).id();

        // Two turret bumps along the deck. Sign flips for stern-side
        // mooring so the turrets sit toward the open water, not into
        // the quay.
        let sign = if bow_up { 1.0 } else { -1.0 };
        for &mag in &[-length * 0.20, length * 0.20] {
            let turret = commands.spawn((
                Mesh2d(turret_mesh.clone()),
                MeshMaterial2d(turret_mat.clone()),
                Transform::from_xyz(0.0, mag * sign, 0.1),
                RenderLayers::layer(DOCKYARD_LAYER),
            )).id();
            commands.entity(turret).insert(ChildOf(ship));
        }

        // Mast banner — small white flag at deck centre.
        let banner = commands.spawn((
            Mesh2d(banner_mesh.clone()),
            MeshMaterial2d(banner_mat.clone()),
            Transform::from_xyz(0.0, 0.0, 0.15),
            RenderLayers::layer(DOCKYARD_LAYER),
        )).id();
        commands.entity(banner).insert(ChildOf(ship));
    }
}

/// Hull declaration order — mirrors `hull::HULL_ORDER` but kept local
/// so this module compiles without exposing the constant. Out-of-sync
/// would mean a hull is missing from the dockyard, which an unused-
/// variant `#[deny]` would catch on the click handler's `match`.
const HULL_ORDER: &[Hull] = &[
    Hull::Default,
    Hull::GlassCannon,
    Hull::Rammer,
    Hull::Dreadnought,
    Hull::Privateer,
    Hull::Corsair,
    Hull::Harpooner,
    Hull::Revenant,
];

// ---------- Per-frame systems ----------

/// Gameplay UI nodes that would otherwise bleed through the
/// transparent left half of the dockyard overlay. Hidden while
/// HullSelect is up so the pixel scene reads clean.
type GameplayChromeFilter = bevy::ecs::query::Or<(
    bevy::ecs::query::With<crate::ui::UiPanel>,
    bevy::ecs::query::With<crate::ui::ScoreText>,
    bevy::ecs::query::With<crate::ui::FpsText>,
    bevy::ecs::query::With<crate::ui::ReturnToMapButton>,
    bevy::ecs::query::With<crate::ui::CameraFollowButton>,
    bevy::ecs::query::With<crate::ui::WaveHpUi>,
    bevy::ecs::query::With<crate::ui::AllyHpRow>,
    bevy::ecs::query::With<crate::map::LevelStatusUi>,
)>;

/// Toggle the dockyard pipeline + hide-the-game-chrome on
/// `AppState::HullSelect` enter/exit. Mirrors how customize hides
/// gameplay HUD when its own overlay opens.
pub fn toggle_dockyard_render(
    state: Res<State<AppState>>,
    mut cam_q: Query<&mut Camera, (With<DockyardCamera>, Without<crate::palette::HudCamera>)>,
    mut hud_cam_q: Query<&mut Camera, (With<crate::palette::HudCamera>, Without<DockyardCamera>)>,
    mut display_q: Query<
        &mut Visibility,
        (With<DockyardDisplaySprite>, Without<DockyardBackdrop>),
    >,
    mut backdrop_q: Query<
        &mut Visibility,
        (With<DockyardBackdrop>, Without<DockyardDisplaySprite>),
    >,
    mut chrome_q: Query<
        &mut Visibility,
        (
            GameplayChromeFilter,
            Without<DockyardDisplaySprite>,
            Without<DockyardBackdrop>,
        ),
    >,
) {
    if !state.is_changed() { return; }
    let active = *state.get() == AppState::HullSelect;
    for mut cam in &mut cam_q {
        if cam.is_active != active { cam.is_active = active; }
    }
    // HudCamera renders enemy HP bars + the upscaled scanline overlay.
    // Disable while the dockyard is up so they don't leak through the
    // transparent overlay region.
    for mut cam in &mut hud_cam_q {
        let want_hud = !active;
        if cam.is_active != want_hud { cam.is_active = want_hud; }
    }
    let display_want = if active { Visibility::Inherited } else { Visibility::Hidden };
    for mut vis in &mut display_q {
        if *vis != display_want { *vis = display_want; }
    }
    for mut vis in &mut backdrop_q {
        if *vis != display_want { *vis = display_want; }
    }
    let chrome_want = if active { Visibility::Hidden } else { Visibility::Inherited };
    for mut vis in &mut chrome_q {
        if *vis != chrome_want { *vis = chrome_want; }
    }
}

/// Resize / re-position the upscaled display sprite each frame so it
/// fills the window at an integer scale, and update `DockyardViewport`
/// for cursor + label coord conversion.
pub fn resize_dockyard_display(
    windows: Query<&Window, With<PrimaryWindow>>,
    window_mode: Res<WindowMode>,
    mut sprite_q: Query<
        (&mut Sprite, &mut Transform),
        (With<DockyardDisplaySprite>, Without<DockyardBackdrop>),
    >,
    mut viewport: ResMut<DockyardViewport>,
) {
    let Ok(win) = windows.single() else { return; };
    let logical_w = win.width();
    let logical_h = win.height();
    let avail_w = (logical_w - effective_ui_width(&window_mode)).max(0.0);
    let scale_x = (avail_w / DOCKYARD_INTERNAL_W as f32).floor();
    let scale_y = (logical_h / DOCKYARD_INTERNAL_H as f32).floor();
    let scale = scale_x.min(scale_y).max(1.0);
    let w = DOCKYARD_INTERNAL_W as f32 * scale;
    let h = DOCKYARD_INTERNAL_H as f32 * scale;
    // Display-origin = top-left of the upscaled image in window
    // coords. The sprite itself is centred on (0, 0) in world space
    // but the UpscaleCamera shares the window's centre, so the
    // top-left of the sprite is `(win_center - size/2)`.
    let display_origin = Vec2::new(
        (logical_w - w) * 0.5,
        (logical_h - h) * 0.5,
    );
    viewport.display_origin = display_origin;
    viewport.display_scale = scale;
    for (mut sprite, mut tf) in &mut sprite_q {
        let want_size = Vec2::new(w, h);
        if sprite.custom_size != Some(want_size) {
            sprite.custom_size = Some(want_size);
        }
        // Keep z = 2.5 from setup; x/y stay 0 (centred by UpscaleCamera).
        tf.translation.z = 2.5;
    }
}

/// Re-tint each berth body each frame: selected = rope-yellow accent,
/// hover preview = brighter base, otherwise = base silhouette colour.
/// Reads `SelectedHull` + `PreviewHull` so the highlight tracks both
/// committed pick and hover state.
pub fn update_dockyard_highlight(
    selected: Res<SelectedHull>,
    preview: Res<PreviewHull>,
    state: Res<State<AppState>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    bodies: Query<(&DockyardBerthBody, &MeshMaterial2d<ColorMaterial>)>,
) {
    if *state.get() != AppState::HullSelect { return; }
    if !selected.is_changed() && !preview.is_changed() && !state.is_changed() {
        return;
    }
    for (body, mat) in &bodies {
        let h = body.0;
        let sel = h == selected.0;
        let hov = preview.0 == Some(h) && !sel;
        let color = body_tint(h, sel, hov);
        if let Some(m) = materials.get_mut(&mat.0) {
            if m.color != color { m.color = color; }
        }
    }
}

/// Drive the floating Bevy UI name labels above each berth — anchored
/// to the ship's screen position via `DockyardViewport::spec_to_window`.
pub fn update_dockyard_labels(
    viewport: Res<DockyardViewport>,
    state: Res<State<AppState>>,
    mut labels: Query<(&DockyardLabel, &mut Node, &mut Visibility)>,
) {
    let active = *state.get() == AppState::HullSelect;
    for (label, mut node, mut vis) in &mut labels {
        let want_vis = if active { Visibility::Inherited } else { Visibility::Hidden };
        if *vis != want_vis { *vis = want_vis; }
        if !active { continue; }
        let pos = berth_pos(label.0);
        // Vertical ships are ~22 spec px tall, so the label needs to
        // clear half-length plus breathing room to sit above the bow
        // (top row) or above the stern (bottom row — close enough).
        let above = pos + Vec2::new(0.0, 16.0);
        let win = viewport.spec_to_window(above);
        // Centre-justify by nudging left by half the approximate text
        // width. Bevy UI text auto-measures, but for absolute-positioned
        // nodes we approximate from the label length.
        let label_text = label.0.label();
        let approx_w = label_text.chars().count() as f32 * 7.0;
        node.left = Val::Px(win.x - approx_w * 0.5);
        node.top = Val::Px(win.y);
    }
}

/// Cursor-click handler — find the berth under the cursor and commit
/// its `Hull` to `SelectedHull`.
pub fn handle_dockyard_click(
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    viewport: Res<DockyardViewport>,
    berths: Query<&DockyardBerth>,
    mut selected: ResMut<SelectedHull>,
) {
    if !mouse.just_pressed(MouseButton::Left) { return; }
    let Ok(win) = windows.single() else { return; };
    let Some(cursor) = win.cursor_position() else { return; };
    let Some(spec) = viewport.window_to_spec(cursor) else { return; };
    for berth in &berths {
        let centre = berth_pos(berth.hull);
        let half = berth.hit_half;
        if (spec.x - centre.x).abs() <= half.x
            && (spec.y - centre.y).abs() <= half.y
        {
            if selected.0 != berth.hull {
                selected.0 = berth.hull;
            }
            return;
        }
    }
}

/// Cursor-hover handler — track which berth the cursor is over and
/// publish to `PreviewHull` so `update_dockyard_highlight` + the
/// manifest panel rebuild reflect the preview.
pub fn handle_dockyard_hover(
    state: Res<State<AppState>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    viewport: Res<DockyardViewport>,
    berths: Query<&DockyardBerth>,
    mut preview: ResMut<PreviewHull>,
) {
    if *state.get() != AppState::HullSelect {
        if preview.0.is_some() { preview.0 = None; }
        return;
    }
    let Ok(win) = windows.single() else { return; };
    let cursor = match win.cursor_position() {
        Some(c) => c,
        None => {
            if preview.0.is_some() { preview.0 = None; }
            return;
        }
    };
    let want = viewport.window_to_spec(cursor).and_then(|spec| {
        berths.iter().find_map(|berth| {
            let centre = berth_pos(berth.hull);
            let half = berth.hit_half;
            ((spec.x - centre.x).abs() <= half.x
                && (spec.y - centre.y).abs() <= half.y)
                .then_some(berth.hull)
        })
    });
    if preview.0 != want { preview.0 = want; }
}

/// OnExit(HullSelect) — despawn every dockyard scene entity + floating
/// label so the dockyard and the game stay fully distinct. The
/// render-target plumbing (camera, image, backdrop, display sprite)
/// is created once at startup and kept warm; only the scene
/// *contents* live in a per-visit lifecycle.
pub fn despawn_dockyard_scene(
    mut commands: Commands,
    scene: Query<Entity, With<DockyardSceneEntity>>,
    labels: Query<Entity, With<DockyardLabel>>,
) {
    for e in &scene {
        commands.entity(e).despawn();
    }
    for e in &labels {
        commands.entity(e).despawn();
    }
}

/// Spawn the floating Bevy UI name labels — one per hull, initially
/// hidden. `update_dockyard_labels` repositions + shows them while
/// HullSelect is active.
pub fn spawn_dockyard_labels(mut commands: Commands) {
    for &hull in HULL_ORDER {
        commands.spawn((
            Text::new(hull.label()),
            TextFont {
                font_size: 14.0,
                font_smoothing: bevy::text::FontSmoothing::None,
                ..default()
            },
            TextColor(Color::srgb(0.95, 0.92, 0.78)),
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(0.0),
                left: Val::Px(0.0),
                ..default()
            },
            Visibility::Hidden,
            // Ensure labels render above the upscaled dockyard sprite.
            ZIndex(155),
            DockyardLabel(hull),
        ));
    }
}
