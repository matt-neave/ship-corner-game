//! Procedural red dithered ring around the play area, alpha-faded
//! based on the player's current HP fraction. Comes in below the
//! `LOW_HP_THRESHOLD` and pulses with a heartbeat cadence.
//!
//! Implementation
//! --------------
//! * One sprite on the UPSCALE layer, sized + positioned each frame
//!   to match the on-screen play area (mirrors `UpscaleSprite`).
//! * Texture is a Bayer-dithered red ring generated once at startup:
//!   solid alpha at the edges, fading to transparent toward the
//!   centre, with a 4×4 Bayer matrix punching the gradient into a
//!   pixel-art-friendly stipple.
//! * Per-frame: read player HP, write the sprite's `Sprite.color.alpha`
//!   based on `1 - hp_pct` × heartbeat curve. Hidden above threshold
//!   so it doesn't cost a draw call in the common case.

use bevy::prelude::*;
use bevy::render::render_asset::RenderAssetUsages;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::image::ImageSampler;
use bevy::render::view::RenderLayers;
use bevy::window::PrimaryWindow;

use crate::balance::{PLAY_INTERNAL_H, PLAY_INTERNAL_W, UPSCALE_LAYER};
use crate::components::{Friendly, Health, LocalPlayer};
use crate::modes::play_area_screen_rect;
use crate::map::ViewMode;

/// Below this HP fraction the vignette starts to bleed in. 0.25 =
/// kicks in at 25% HP. Matches the "low HP" intuition without
/// nagging the player at every minor hit.
const LOW_HP_THRESHOLD: f32 = 0.25;
/// Max alpha at 0 HP. 0.75 = clearly visible but the play area is
/// still readable through the dither.
const PEAK_ALPHA: f32 = 0.75;
/// Beats per second of the heartbeat pulse modulating alpha. Faster
/// at lower HP would be cool but adds state — pick a fixed rate that
/// reads as "tense" without being annoying.
const HEARTBEAT_HZ: f32 = 1.4;

#[derive(Resource)]
pub struct LowHpVignetteImage(pub Handle<Image>);

#[derive(Component)]
pub struct LowHpVignetteSprite;

pub struct LowHpVignettePlugin;

impl Plugin for LowHpVignettePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, (setup_vignette_image, spawn_vignette_sprite).chain())
            .add_systems(Update, update_vignette);
    }
}

fn setup_vignette_image(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let img = make_dithered_ring_image();
    let handle = images.add(img);
    commands.insert_resource(LowHpVignetteImage(handle));
}

fn spawn_vignette_sprite(
    mut commands: Commands,
    image: Res<LowHpVignetteImage>,
) {
    commands.spawn((
        Sprite {
            image: image.0.clone(),
            color: Color::srgba(1.0, 0.18, 0.18, 0.0),
            custom_size: Some(Vec2::new(
                PLAY_INTERNAL_W as f32 * 4.0,
                PLAY_INTERNAL_H as f32 * 4.0,
            )),
            ..default()
        },
        // z above the play sprite (z=2 in rendering.rs) so the
        // ring paints on top of the world. Below HUD camera so it
        // sits under the HP / WAVE chrome.
        Transform::from_xyz(0.0, 0.0, 3.0),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        LowHpVignetteSprite,
    ));
}

/// Build the dithered red ring texture once. Edge pixels are full
/// alpha; centre pixels are fully transparent. A 4×4 Bayer matrix
/// pushes the gradient into a stipple — pure alpha at the rim,
/// scattering toward zero as you approach the inner clear zone.
///
/// Uses distance from the nearest play-area edge (not radial distance
/// from centre) so the ring hugs all four edges with uniform
/// thickness regardless of aspect — works for both the default
/// square play area and the `wide_play` 360×200 rectangle.
fn make_dithered_ring_image() -> Image {
    let w = PLAY_INTERNAL_W;
    let h = PLAY_INTERNAL_H;
    // Bayer 4x4 — classic ordered-dither matrix, 0..15 / 16.
    const BAYER: [[u8; 4]; 4] = [
        [ 0,  8,  2, 10],
        [12,  4, 14,  6],
        [ 3, 11,  1,  9],
        [15,  7, 13,  5],
    ];
    // Ring band thickness in internal pixels. Anchored to the
    // shorter axis so a wide play area doesn't get a band that
    // eats the whole vertical extent. 0.225 × short-axis ≈ 45 px
    // on both layouts — matches the previous radial design's
    // band width on the square play area.
    let thickness = (w.min(h) as f32) * 0.225;
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            // Distance to the nearest edge of the rectangle.
            // min(left, right, top, bottom).
            let dx = (x as f32).min((w - 1 - x) as f32);
            let dy = (y as f32).min((h - 1 - y) as f32);
            let edge_d = dx.min(dy);
            // 1.0 right at the edge, 0.0 at thickness depth.
            let raw = (1.0 - edge_d / thickness.max(0.001)).clamp(0.0, 1.0);
            // Dither: compare gradient * 16 against the Bayer
            // threshold. Pixel passes (full alpha) if it's
            // above threshold.
            let threshold = BAYER[(y as usize) & 3][(x as usize) & 3] as f32;
            let alpha = if raw * 16.0 > threshold { 255 } else { 0 };
            data.extend_from_slice(&[255, 60, 60, alpha]);
        }
    }
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

fn update_vignette(
    time: Res<Time<bevy::time::Real>>,
    state: Res<State<crate::AppState>>,
    view: Res<ViewMode>,
    stats: Res<crate::stats::PlayerStats>,
    windows: Query<&Window, With<PrimaryWindow>>,
    player: Query<&Health, (With<Friendly>, With<LocalPlayer>)>,
    mut sprite_q: Query<(&mut Sprite, &mut Transform, &mut Visibility), With<LowHpVignetteSprite>>,
) {
    let in_combat = matches!(*state.get(), crate::AppState::Playing)
        && *view == ViewMode::Combat;
    let Ok(h) = player.single() else {
        for (_, _, mut v) in &mut sprite_q {
            if *v != Visibility::Hidden { *v = Visibility::Hidden; }
        }
        return;
    };
    let max_hp = stats.max_hp().max(1) as f32;
    let hp_pct = (h.0 as f32 / max_hp).clamp(0.0, 1.0);
    let active = in_combat && hp_pct < LOW_HP_THRESHOLD;

    // Distance below threshold drives the alpha base. At
    // hp_pct=0 → tension=1.0; at hp_pct=threshold → tension=0.0.
    let tension = ((LOW_HP_THRESHOLD - hp_pct) / LOW_HP_THRESHOLD).clamp(0.0, 1.0);
    // Heartbeat: sin² so the curve rests at 0 most of the time and
    // pulses up — same shape as a heart-monitor blip.
    let phase = time.elapsed_secs() * HEARTBEAT_HZ * std::f32::consts::TAU;
    let pulse = 0.7 + 0.3 * (phase.sin() * 0.5 + 0.5);
    let alpha = if active { PEAK_ALPHA * tension * pulse } else { 0.0 };

    // Resize + reposition each frame to match the on-screen play area.
    let (left, top, play_w, play_h) = windows
        .single()
        .ok()
        .map(|w| play_area_screen_rect(w.width(), w.height()))
        .unwrap_or((0.0, 0.0, 0.0, 0.0));
    let centre_x = left + play_w * 0.5
        - windows.single().ok().map(|w| w.width() * 0.5).unwrap_or(0.0);
    let centre_y = -(top + play_h * 0.5
        - windows.single().ok().map(|w| w.height() * 0.5).unwrap_or(0.0));

    for (mut sprite, mut tf, mut vis) in &mut sprite_q {
        let want_vis = if active { Visibility::Inherited } else { Visibility::Hidden };
        if *vis != want_vis { *vis = want_vis; }
        if !active { continue; }
        let want_color = Color::srgba(1.0, 0.18, 0.18, alpha);
        if sprite.color != want_color { sprite.color = want_color; }
        let want_size = Vec2::new(play_w, play_h);
        if sprite.custom_size != Some(want_size) { sprite.custom_size = Some(want_size); }
        tf.translation.x = centre_x;
        tf.translation.y = centre_y;
    }
}
