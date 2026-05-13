//! Hull-select supporting visuals — game-chrome toggle + pixel-art
//! ship preview render target.
//!
//! The HullSelect screen itself is a pure bevy_ui overlay (`hull.rs`).
//! This module owns the two world-side pieces that overlay needs:
//!
//! * `toggle_dockyard_render` hides the gameplay HUD camera + UI
//!   chrome while the menu is up so the panel reads clean, and
//!   restores them on exit.
//! * The hull-preview render target — a tiny camera that draws ONE
//!   pixel-art ship (capsule body + 8 turret dots) at the
//!   `HULL_PREVIEW_INTERNAL_W × _H` internal resolution. `hull.rs`
//!   displays the resulting `Image` in a bevy_ui `ImageNode`, which
//!   nearest-neighbor scales the chunky pixels to the LHS preview
//!   card — same visual language as the in-game ship.

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::camera::RenderTarget;
use bevy::render::render_resource::{
    Extent3d, TextureDimension, TextureFormat, TextureUsages,
};
use bevy::render::view::{Msaa, RenderLayers};

use crate::balance::{
    HULL_LEN, HULL_PREVIEW_INTERNAL_H, HULL_PREVIEW_INTERNAL_W, HULL_PREVIEW_LAYER,
    HULL_WIDTH, TURRET_POSITIONS,
};
use crate::hull::{Hull, PreviewHull, SelectedHull};
use crate::AppState;

// ---------- Game-chrome toggle ----------

/// Gameplay UI nodes that would otherwise bleed through the
/// fullscreen HullSelect overlay (or sit at a higher z-index and
/// poke through). Hidden while HullSelect is up so the menu
/// dominates the frame; restored on exit.
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

/// Toggle the gameplay HUD + chrome whenever the state crosses
/// in/out of `AppState::HullSelect`.
pub fn toggle_dockyard_render(
    state: Res<State<AppState>>,
    mut hud_cam_q: Query<
        &mut Camera,
        (With<crate::palette::HudCamera>, Without<HullPreviewCamera>),
    >,
    mut preview_cam_q: Query<
        &mut Camera,
        (With<HullPreviewCamera>, Without<crate::palette::HudCamera>),
    >,
    mut chrome_q: Query<&mut Visibility, GameplayChromeFilter>,
) {
    if !state.is_changed() { return; }
    let active = *state.get() == AppState::HullSelect;
    // HudCamera renders enemy HP bars + the upscaled scanline
    // overlay. Disable while the menu is up so those don't peek
    // through.
    for mut cam in &mut hud_cam_q {
        let want = !active;
        if cam.is_active != want { cam.is_active = want; }
    }
    // Hull-preview camera renders only while we're on the screen
    // that consumes its output. Keeps the render-target Image stale
    // outside HullSelect, which is fine — nothing reads it then.
    for mut cam in &mut preview_cam_q {
        if cam.is_active != active { cam.is_active = active; }
    }
    let chrome_want = if active { Visibility::Hidden } else { Visibility::Inherited };
    for mut vis in &mut chrome_q {
        if *vis != chrome_want { *vis = chrome_want; }
    }
}

// ---------- Hull preview render target ----------

#[derive(Component)]
pub struct HullPreviewCamera;

#[derive(Component)]
pub struct HullPreviewBody;

#[derive(Component)]
pub struct HullPreviewTurret;

/// Image handle for the hull-preview render target — referenced by
/// `hull.rs::spawn_overlay` to populate the LHS card's `ImageNode`.
#[derive(Resource, Clone)]
pub struct HullPreviewImage(pub Handle<Image>);

/// Build the render-target image, the camera that draws into it,
/// and the persistent mesh entities the camera shows. The hull mesh
/// + turret dots live for the whole app lifetime; `update_hull_preview`
/// re-tints the hull body when the previewed selection changes.
///
/// Runs at Startup. The camera is inactive by default —
/// `toggle_dockyard_render` flips it on while HullSelect is up so
/// the GPU only renders the preview when something is consuming it.
pub fn setup_hull_preview_render(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
) {
    let size = Extent3d {
        width: HULL_PREVIEW_INTERNAL_W,
        height: HULL_PREVIEW_INTERNAL_H,
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
    // Nearest-neighbour sampling — the whole point is the chunky
    // pixel look the in-game ship has. Without this the upscale to
    // the bevy_ui ImageNode would blur the texture.
    img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor::nearest());
    let handle = images.add(img);
    commands.insert_resource(HullPreviewImage(handle.clone()));

    commands.spawn((
        Camera2d,
        Camera {
            target: RenderTarget::Image(handle.into()),
            // Transparent clear so the ImageNode behind it
            // (parent card) shows through the empty corners.
            clear_color: ClearColorConfig::Custom(Color::NONE),
            order: -4,
            is_active: false,
            ..default()
        },
        Projection::Orthographic(OrthographicProjection {
            scaling_mode: bevy::render::camera::ScalingMode::Fixed {
                width: HULL_PREVIEW_INTERNAL_W as f32,
                height: HULL_PREVIEW_INTERNAL_H as f32,
            },
            ..OrthographicProjection::default_2d()
        }),
        RenderLayers::layer(HULL_PREVIEW_LAYER),
        Msaa::Off,
        HullPreviewCamera,
    ));

    // Hull body — same proportions as the in-game ship.
    let hull_mesh = meshes.add(Capsule2d::new(
        HULL_WIDTH * 0.5,
        (HULL_LEN - HULL_WIDTH).max(0.0),
    ));
    let hull_mat = materials.add(preview_hull_color(Hull::default()));
    commands.spawn((
        Mesh2d(hull_mesh),
        MeshMaterial2d(hull_mat),
        Transform::from_xyz(0.0, 0.0, 0.0),
        RenderLayers::layer(HULL_PREVIEW_LAYER),
        HullPreviewBody,
    ));

    // Turret dots — eight small circles at the canonical positions.
    // Shared mesh + material; positions baked in at spawn so no
    // per-frame work is needed.
    let turret_mesh = meshes.add(Circle::new(1.2));
    let turret_mat = materials.add(Color::srgb(0.30, 0.34, 0.42));
    for &(x, y) in TURRET_POSITIONS.iter() {
        commands.spawn((
            Mesh2d(turret_mesh.clone()),
            MeshMaterial2d(turret_mat.clone()),
            Transform::from_xyz(x, y, 0.1),
            RenderLayers::layer(HULL_PREVIEW_LAYER),
            HullPreviewTurret,
        ));
    }
}

/// Per-frame: re-tint the hull body to match the previewed hull
/// (hover overrides committed selection). Only writes when the
/// resource actually changed — the material handle stays the same
/// across runs.
pub fn update_hull_preview(
    selected: Res<SelectedHull>,
    preview: Res<PreviewHull>,
    state: Res<State<AppState>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    body_q: Query<&MeshMaterial2d<ColorMaterial>, With<HullPreviewBody>>,
) {
    if *state.get() != AppState::HullSelect { return; }
    if !selected.is_changed() && !preview.is_changed() && !state.is_changed() {
        return;
    }
    let hull = preview.0.unwrap_or(selected.0);
    let want = preview_hull_color(hull);
    for mat_handle in &body_q {
        if let Some(mat) = materials.get_mut(&mat_handle.0) {
            if mat.color != want { mat.color = want; }
        }
    }
}

/// Per-hull tint. Same palette the `hull.rs` UI fallback uses, kept
/// in sync visually so removing the render target later (or running
/// without it) doesn't change the colour scheme.
fn preview_hull_color(hull: Hull) -> Color {
    match hull {
        Hull::Default     => Color::srgb(0.78, 0.80, 0.86),
        Hull::GlassCannon => Color::srgb(0.55, 0.85, 0.90),
        Hull::Rammer      => Color::srgb(0.78, 0.50, 0.38),
        Hull::Dreadnought => Color::srgb(0.50, 0.55, 0.62),
        Hull::Privateer   => Color::srgb(0.95, 0.55, 0.30),
        Hull::Corsair     => Color::srgb(0.88, 0.78, 0.42),
        Hull::Harpooner   => Color::srgb(0.50, 0.70, 0.95),
        Hull::Revenant    => Color::srgb(0.70, 0.78, 0.88),
    }
}
