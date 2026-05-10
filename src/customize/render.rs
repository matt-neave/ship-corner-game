//! Render pipeline + cursor mapping for the customize overlay.
//!
//! Pipeline mirrors the in-game two-camera approach:
//! 1. **CustomizeCamera** (order=-2, image target) — renders every
//!    primitive on `CUSTOMIZE_LAYER` to a low-res image.
//! 2. **CustomizeDisplaySprite** (on `UPSCALE_LAYER`) — shows the image
//!    upscaled with nearest-neighbor sampling. That low-res
//!    rasterization is the chunky-pixel look.
//!
//! When the overlay opens we also hide every game-mode UI element
//! (HUD bars, score, debug panels, etc.) and disable the native-res
//! `HudCamera` so floating enemy HP bars + scanlines don't bleed
//! through the customize backdrop.
//!
//! `CustomizeViewport` exposes the live window↔spec coord mapping for
//! drag/tooltip systems.

use bevy::asset::RenderAssetUsages;
use bevy::ecs::query::Or;
use bevy::image::{ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::camera::RenderTarget;
use bevy::render::render_resource::{
    Extent3d, TextureDimension, TextureFormat, TextureUsages,
};
use bevy::render::view::{Msaa, RenderLayers};
use bevy::window::PrimaryWindow;

use crate::balance::{
    CUSTOMIZE_INTERNAL_H, CUSTOMIZE_INTERNAL_W, CUSTOMIZE_LAYER, UPSCALE_LAYER,
};

use super::CustomizeOpen;

#[derive(Component)]
pub struct CustomizeCamera;

#[derive(Component)]
pub struct CustomizeDisplaySprite;

/// Opaque fullscreen sprite that masks off the play-world rendering
/// while customize is open. Sits between the play sprite + hash
/// background (z<1.5) and the customize display sprite (z=2.0).
#[derive(Component)]
pub struct CustomizeBackdropSprite;

#[derive(Resource, Default, Clone, Copy)]
pub struct CustomizeViewport {
    pub display_origin: Vec2,
    pub display_scale: f32,
}

impl CustomizeViewport {
    pub fn window_to_spec(&self, cursor: Vec2) -> Option<Vec2> {
        if self.display_scale <= 0.0 {
            return None;
        }
        let local = (cursor - self.display_origin) / self.display_scale;
        let w = CUSTOMIZE_INTERNAL_W as f32;
        let h = CUSTOMIZE_INTERNAL_H as f32;
        if local.x < 0.0 || local.x > w || local.y < 0.0 || local.y > h {
            return None;
        }
        Some(Vec2::new(local.x - w * 0.5, h * 0.5 - local.y))
    }
}

pub fn setup_customize_render(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
) {
    let size = Extent3d {
        width: CUSTOMIZE_INTERNAL_W,
        height: CUSTOMIZE_INTERNAL_H,
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
            clear_color: ClearColorConfig::Custom(Color::srgb(0.13, 0.14, 0.17)),
            order: -2,
            is_active: false,
            ..default()
        },
        Projection::Orthographic(OrthographicProjection {
            scaling_mode: bevy::render::camera::ScalingMode::Fixed {
                width: CUSTOMIZE_INTERNAL_W as f32,
                height: CUSTOMIZE_INTERNAL_H as f32,
            },
            ..OrthographicProjection::default_2d()
        }),
        RenderLayers::layer(CUSTOMIZE_LAYER),
        Msaa::Off,
        CustomizeCamera,
    ));

    commands.spawn((
        Sprite {
            color: Color::srgb(0.10, 0.11, 0.13),
            custom_size: Some(Vec2::new(4096.0, 4096.0)),
            ..default()
        },
        Transform::from_xyz(0.0, 0.0, 1.5),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        CustomizeBackdropSprite,
    ));

    commands.spawn((
        Sprite {
            image: handle,
            custom_size: Some(Vec2::new(
                CUSTOMIZE_INTERNAL_W as f32 * 4.0,
                CUSTOMIZE_INTERNAL_H as f32 * 4.0,
            )),
            ..default()
        },
        Transform::from_xyz(0.0, 0.0, 2.0),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        CustomizeDisplaySprite,
    ));

    commands.insert_resource(CustomizeViewport::default());
}

/// Single `Or` filter capturing every bevy_ui element that should
/// disappear while customize is open. World-space sprites (HashSprite,
/// ScanlineSprite, UpscaleSprite) don't need toggling — the opaque
/// backdrop sprite at z=1.5 covers them. Toggling their visibility
/// here would race against their own owners (`apply_crt_mode`,
/// `update_hash_image`) which only write on state change.
type GameplayChromeFilter = Or<(
    With<crate::ui::UiPanel>,
    // DebugPanel is owned by `map::hud::sync_debug_panel_visibility`
    // (combines the `#` toggle + customize-open + pause). Listing it
    // here would race the two writers.
    With<crate::ui::ScoreText>,
    With<crate::ui::FpsText>,
    With<crate::ui::ReturnToMapButton>,
    With<crate::ui::CameraFollowButton>,
    With<crate::ui::WaveHpUi>,
    With<crate::ui::AllyHpRow>,
    With<crate::pier::DraftPanel>,
    With<crate::map::LevelStatusUi>,
)>;

/// Toggle every customize-vs-game element on `CustomizeOpen` change.
/// Display sprite + backdrop become visible while customize is open;
/// gameplay chrome hides; HudCamera disables so its overlays don't
/// leak through.
pub fn toggle_customize_render(
    open: Res<CustomizeOpen>,
    mut cam_q: Query<&mut Camera, (With<CustomizeCamera>, Without<crate::palette::HudCamera>)>,
    mut hud_cam_q: Query<&mut Camera, (With<crate::palette::HudCamera>, Without<CustomizeCamera>)>,
    mut display_q: Query<
        &mut Visibility,
        (
            With<CustomizeDisplaySprite>,
            Without<CustomizeBackdropSprite>,
            Without<crate::ui::UiPanel>,
        ),
    >,
    mut backdrop_q: Query<
        &mut Visibility,
        (
            With<CustomizeBackdropSprite>,
            Without<CustomizeDisplaySprite>,
            Without<crate::ui::UiPanel>,
        ),
    >,
    mut chrome_q: Query<
        &mut Visibility,
        (
            GameplayChromeFilter,
            Without<CustomizeDisplaySprite>,
            Without<CustomizeBackdropSprite>,
        ),
    >,
) {
    if !open.is_changed() {
        return;
    }
    for mut cam in &mut cam_q {
        cam.is_active = open.open;
    }
    for mut cam in &mut hud_cam_q {
        cam.is_active = !open.open;
    }
    let show = if open.open { Visibility::Inherited } else { Visibility::Hidden };
    let hide = if open.open { Visibility::Hidden } else { Visibility::Inherited };
    for mut vis in &mut display_q {
        if *vis != show { *vis = show; }
    }
    for mut vis in &mut backdrop_q {
        if *vis != show { *vis = show; }
    }
    for mut vis in &mut chrome_q {
        if *vis != hide { *vis = hide; }
    }
}

/// Resize the display sprite + backdrop to fit the window. Updates
/// `CustomizeViewport` so cursor math stays correct on resize.
pub fn resize_customize_display(
    windows: Query<&Window, With<PrimaryWindow>>,
    mut sprite_q: Query<
        (&mut Sprite, &mut Transform),
        (With<CustomizeDisplaySprite>, Without<CustomizeBackdropSprite>),
    >,
    mut backdrop_q: Query<
        &mut Sprite,
        (With<CustomizeBackdropSprite>, Without<CustomizeDisplaySprite>),
    >,
    mut viewport: ResMut<CustomizeViewport>,
) {
    let Ok(win) = windows.single() else { return };
    let win_w = win.width();
    let win_h = win.height();
    if win_w <= 0.0 || win_h <= 0.0 {
        return;
    }
    let scale_x = (win_w / CUSTOMIZE_INTERNAL_W as f32).floor();
    let scale_y = (win_h / CUSTOMIZE_INTERNAL_H as f32).floor();
    let scale = scale_x.min(scale_y).max(1.0);

    let display_w = CUSTOMIZE_INTERNAL_W as f32 * scale;
    let display_h = CUSTOMIZE_INTERNAL_H as f32 * scale;
    let origin = Vec2::new(
        ((win_w - display_w) * 0.5).max(0.0),
        ((win_h - display_h) * 0.5).max(0.0),
    );

    if (viewport.display_scale - scale).abs() > 0.001 {
        viewport.display_scale = scale;
    }
    if (viewport.display_origin - origin).length_squared() > 0.001 {
        viewport.display_origin = origin;
    }

    for (mut sprite, mut tf) in &mut sprite_q {
        let want = Some(Vec2::new(display_w, display_h));
        if sprite.custom_size != want {
            sprite.custom_size = want;
        }
        if tf.translation != Vec3::new(0.0, 0.0, 2.0) {
            tf.translation = Vec3::new(0.0, 0.0, 2.0);
        }
    }
    let backdrop_size = Some(Vec2::new(win_w + 64.0, win_h + 64.0));
    for mut sprite in &mut backdrop_q {
        if sprite.custom_size != backdrop_size {
            sprite.custom_size = backdrop_size;
        }
    }
}
