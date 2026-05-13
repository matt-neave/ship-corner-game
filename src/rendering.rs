//! Two-camera pixel-perfect render pipeline.
//!
//! - **Play camera** renders the gameplay world to a `PLAY_INTERNAL × PLAY_INTERNAL`
//!   image at 1 unit / 1 internal pixel. MSAA off so primitive edges fall on the grid.
//! - **Upscale camera** draws sprites in screen space. The play render-target
//!   image is shown via a sprite, sized to an integer multiple of the internal
//!   resolution and sampled with nearest-neighbor — that's the chunky pixel look.
//! - A tiled diagonal-hash backdrop fills the letterbox region around the play sprite.
//! - A scanline overlay sits in front of the play sprite, hidden unless `CrtMode`.

use bevy::image::{ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::camera::RenderTarget;
use bevy::render::render_asset::RenderAssetUsages;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use bevy::render::view::{Msaa, RenderLayers};
use bevy::sprite::SpriteImageMode;
use bevy::window::PrimaryWindow;

use crate::balance::{
    HUD_LAYER, PLAY_INTERNAL_H, PLAY_INTERNAL_W, PLAY_LAYER, PLAY_WORLD_H, PLAY_WORLD_W,
    UPSCALE_LAYER, WINDOW_H, WINDOW_W,
};
use crate::map::MAP_LAYER;
use crate::modes::{play_area_screen_rect, NightMode, ScanlineSprite};
use crate::palette::{darken, hex, HudCamera, MapCamera, Palette, PlayCamera, UpscaleCamera};

use bevy::render::camera::Viewport;

// ---------- Components & resources for the upscale pipeline ----------

#[derive(Component)]
pub struct UpscaleSprite;

/// Tiled diagonal-hash sprite that fills the full window behind the play
/// area. Visible only in the "letterbox" region around the play sprite.
#[derive(Component)]
pub struct HashSprite;

#[derive(Resource)]
pub struct HashImage(pub Handle<Image>);

/// Holds the play render target so other systems (none currently) can read it.
#[derive(Resource)]
pub struct PlayRenderImage(#[allow(dead_code)] pub Handle<Image>);

// ---------- Image generators ----------

/// CRT scanline overlay: `PLAY_INTERNAL × PLAY_INTERNAL` BGRA texture where
/// every other row is a translucent black band. Sized to match the play-area
/// internal resolution so when nearest-neighbor upscaled, each band lands on
/// exactly one internal pixel of screen height.
pub fn make_scanline_image() -> Image {
    let w = PLAY_INTERNAL_W;
    let h = PLAY_INTERNAL_H;
    // ~12% black on darkened rows. Half the rows × 12% alpha works out to
    // ~6% average darkening — enough that scanlines read at a glance
    // without dimming the whole scene the way a 38% overlay did.
    const DARK_ALPHA: u8 = 32;
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let dark = (y % 2) == 0;
        let rgba = if dark { [0u8, 0, 0, DARK_ALPHA] } else { [0u8, 0, 0, 0] };
        for _ in 0..w { data.extend_from_slice(&rgba); }
    }
    // Rgba8UnormSrgb (not Bgra8) for sampled procedural textures —
    // WebGL2/ANGLE doesn't reliably support sampling Bgra8 sRGB
    // textures, so on the web build those sprites render as blank.
    // Render-target images keep Bgra8 because that's the swap-chain
    // format wgpu requests on most platforms.
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

/// Build an `N×N` RGBA tile with equal-width diagonal stripes —
/// `light` stripes on `(x+y) % tile < tile/2`, otherwise `dark`.
/// Tileable. `tile` controls the diagonal-stripe period; smaller
/// tiles repeat the pattern more times within a sprite.
pub fn make_hash_image_with_tile(light: Color, dark: Color, tile: u32) -> Image {
    let half = tile / 2;
    let to_rgba = |c: Color| {
        let s: bevy::color::Srgba = c.into();
        [
            (s.red   * 255.0).round() as u8,
            (s.green * 255.0).round() as u8,
            (s.blue  * 255.0).round() as u8,
            255u8,
        ]
    };
    let lb = to_rgba(light);
    let db = to_rgba(dark);
    let mut data = Vec::with_capacity((tile * tile * 4) as usize);
    for y in 0..tile {
        for x in 0..tile {
            let band = ((x + y) % tile) < half;
            let rgba = if band { lb } else { db };
            data.extend_from_slice(&rgba);
        }
    }
    // Rgba8UnormSrgb so the texture samples correctly on WebGL2 /
    // ANGLE — see `make_scanline_image` for the same constraint.
    let mut img = Image::new(
        Extent3d { width: tile, height: tile, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    img.sampler = ImageSampler::nearest();
    img
}

/// 192×192 tile — what the BG backdrop uses (wide stripes for the
/// big window-fill quad).
pub fn make_hash_image(light: Color, dark: Color) -> Image {
    make_hash_image_with_tile(light, dark, 192)
}

// ---------- Setup ----------

pub fn setup_render(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    palette: Res<Palette>,
) {
    let size = Extent3d { width: PLAY_INTERNAL_W, height: PLAY_INTERNAL_H, depth_or_array_layers: 1 };
    let mut img = Image::new_fill(
        size,
        TextureDimension::D2,
        &[0, 0, 0, 255],
        TextureFormat::Bgra8UnormSrgb,
        RenderAssetUsages::default(),
    );
    img.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING
        | TextureUsages::COPY_DST
        | TextureUsages::RENDER_ATTACHMENT;
    img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor::nearest());

    let handle = images.add(img);
    commands.insert_resource(PlayRenderImage(handle.clone()));

    // Two cameras both render to this same image, but only one is active at
    // a time (toggled by `apply_view_mode`). PlayCamera renders combat
    // (PLAY_LAYER); MapCamera renders the map view (MAP_LAYER). Default
    // ViewMode is Map, so we start with PlayCamera disabled. MSAA off —
    // multi-sampling against a low-res render target softens every
    // primitive edge, killing the chunky-pixel look.
    let proj = || Projection::Orthographic(OrthographicProjection {
        scaling_mode: bevy::render::camera::ScalingMode::Fixed {
            width: PLAY_WORLD_W,
            height: PLAY_WORLD_H,
        },
        ..OrthographicProjection::default_2d()
    });

    commands.spawn((
        Camera2d,
        Camera {
            target: RenderTarget::Image(handle.clone().into()),
            clear_color: ClearColorConfig::Custom(palette.ocean),
            order: -1,
            is_active: false, // map view is the default
            ..default()
        },
        proj(),
        RenderLayers::layer(PLAY_LAYER),
        PlayCamera,
        Msaa::Off,
    ));

    commands.spawn((
        Camera2d,
        Camera {
            target: RenderTarget::Image(handle.clone().into()),
            clear_color: ClearColorConfig::Custom(palette.ocean),
            order: -1,
            is_active: true,
            ..default()
        },
        proj(),
        RenderLayers::layer(MAP_LAYER),
        MapCamera,
        Msaa::Off,
    ));

    // UI / upscale camera (default layer + upscale layer). Clear color = ocean
    // so any pixels outside the play sprite (between UI panel and play area,
    // or 1-px misalignments in desktop mode) match the water seamlessly.
    //
    // `IsDefaultUiCamera` pins Bevy UI rendering to *this* camera. Without
    // it, Bevy UI defaults to the highest-order camera with the default
    // render layer — and `HudCamera` (order=1) would steal that role,
    // funneling the entire HUD through the play-area-clipped viewport.
    commands.spawn((
        Camera2d,
        Camera {
            order: 0,
            clear_color: ClearColorConfig::Custom(palette.ocean),
            ..default()
        },
        RenderLayers::from_layers(&[0, UPSCALE_LAYER]),
        Msaa::Off,
        UpscaleCamera,
        bevy::ui::IsDefaultUiCamera,
    ));

    // Native-resolution HUD camera. Same world projection as PlayCamera,
    // viewport clipped to the play-area screen rect, transparent clear so
    // it composites on top of the upscaled play sprite. `update_hud_camera_viewport`
    // re-snaps the viewport every frame as the window resizes / desktop mode
    // toggles. Entities placed on `HUD_LAYER` render here at native resolution.
    commands.spawn((
        Camera2d,
        Camera {
            // Order > UpscaleCamera so HUD draws on top of the upscaled view.
            order: 1,
            // Transparent — never clears the framebuffer, just composites.
            clear_color: ClearColorConfig::None,
            ..default()
        },
        proj(),
        RenderLayers::layer(HUD_LAYER),
        Msaa::Off,
        HudCamera,
    ));

    // Diagonal-hash backdrop, tiled across the entire window. Sits BEHIND the
    // play sprite (z=-1) so it shows in the surrounding letterbox region.
    let hash_image = images.add(make_hash_image(palette.ocean, hex("#3b5dc9")));
    commands.insert_resource(HashImage(hash_image.clone()));
    commands.spawn((
        Sprite {
            image: hash_image,
            custom_size: Some(Vec2::new(WINDOW_W, WINDOW_H)),
            image_mode: SpriteImageMode::Tiled { tile_x: true, tile_y: true, stretch_value: 1.0 },
            ..default()
        },
        Transform::from_xyz(0.0, 0.0, -1.0),
        RenderLayers::layer(UPSCALE_LAYER),
        HashSprite,
    ));

    // Sprite that displays the play render target, on UPSCALE_LAYER, positioned
    // in screen space. Initial size/position for frame 0; `resize_upscale_sprite`
    // refines it every frame using the actual window size.
    let (left0, _top0, w0, h0) = play_area_screen_rect(WINDOW_W, WINDOW_H);
    let world_x0 = left0 + w0 / 2.0 - WINDOW_W / 2.0;
    commands.spawn((
        Sprite {
            image: handle,
            custom_size: Some(Vec2::new(w0, h0)),
            ..default()
        },
        Transform::from_xyz(world_x0, 0.0, 0.0),
        RenderLayers::layer(UPSCALE_LAYER),
        UpscaleSprite,
    ));

    // Scanline overlay — same size + position as the play sprite, layered
    // just in front (z=1.0). Hidden until CrtMode is toggled on.
    let scanline_handle = images.add(make_scanline_image());
    commands.spawn((
        Sprite {
            image: scanline_handle,
            custom_size: Some(Vec2::new(w0, h0)),
            ..default()
        },
        Transform::from_xyz(world_x0, 0.0, 1.0),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        ScanlineSprite,
    ));
}

// ---------- Per-frame systems ----------

/// Regenerate the hash tile when the palette OR night mode changes so the
/// stripes always match the current ocean. Day-mode dark = derived from ocean
/// (~70% luminance); night mode keeps a near-black hash.
pub fn update_hash_image(
    palette: Res<Palette>,
    night: Res<NightMode>,
    hash: Option<Res<HashImage>>,
    mut images: ResMut<Assets<Image>>,
) {
    if !palette.is_changed() && !night.is_changed() { return; }
    let Some(hash) = hash else { return; };
    let dark = if night.active {
        hex("#0c0e1a")
    } else {
        darken(palette.ocean, 0.7)
    };
    let new_img = make_hash_image(palette.ocean, dark);
    if let Some(img) = images.get_mut(&hash.0) {
        *img = new_img;
    }
}

/// Per-frame: scale every bevy_ui `Val::Px` value to the live window
/// size by writing `bevy_ui::UiScale`. Bevy's layout pass multiplies
/// fixed `Val::Px` by this resource — so all our `ui_kit::theme`
/// constants (`GAP_SM/MD/LG`, `PAD_*`, `FONT_*`, `BORDER_W`) scale
/// uniformly as the window resizes, and CHROME overlays
/// (level-up cards, boss-reward panel, hull-select panel, pause menu,
/// HP/XP bars, etc.) stay proportional instead of overflowing on a
/// small window or floating in a sea of margin on a big one.
///
/// Fit-mode math: `min(w / DESIGN_W, h / DESIGN_H)` — the design
/// layout (`WINDOW_W × WINDOW_H`) never overflows the window edges,
/// regardless of aspect. Clamped so very-small windows stay readable
/// and very-large ones don't make a button fill half the screen.
///
/// Note: `Val::Percent` is unaffected — only `Val::Px` is multiplied.
/// `Val::Px` callsites *inside* a render-target pipeline (the shop's
/// `Text2d` on `CUSTOMIZE_LAYER`) are not bevy_ui at all and stay at
/// their internal-resolution sizes — those want to look chunky-pixel,
/// not window-scaled.
pub fn sync_ui_scale(
    windows: Query<&Window, With<PrimaryWindow>>,
    mut ui_scale: ResMut<UiScale>,
) {
    let Ok(window) = windows.single() else { return; };
    let w = window.width();
    let h = window.height();
    if w <= 0.0 || h <= 0.0 { return; }
    // Fit (not fill) so the design layout fits both axes; never
    // clipped on the short axis if the player's window is wider than
    // the design ratio (or vice versa).
    let raw = (w / WINDOW_W).min(h / WINDOW_H);
    // Floors at 0.5 so a tiny 640×400 window doesn't render
    // microscopic; ceiling at 3.0 so a 4K window doesn't blow buttons
    // up to filling the viewport. Tuned conservatively — we can lift
    // the ceiling later once we're sure the layouts adapt cleanly.
    let want = raw.clamp(0.5, 3.0);
    if (ui_scale.0 - want).abs() > 0.001 {
        ui_scale.0 = want;
    }
}

/// Snap the upscale sprite to an integer multiple of the internal resolution
/// AND reposition it within the window each frame. Without integer snapping,
/// one internal pixel can map to 3.5 screen pixels and shimmer as things move.
pub fn resize_upscale_sprite(
    windows: Query<&Window, With<PrimaryWindow>>,
    mut play_sprites: Query<
        (&mut Sprite, &mut Transform),
        (With<UpscaleSprite>, Without<HashSprite>, Without<ScanlineSprite>),
    >,
    mut hash_sprites: Query<
        &mut Sprite,
        (With<HashSprite>, Without<UpscaleSprite>, Without<ScanlineSprite>),
    >,
    mut scanline_sprites: Query<
        (&mut Sprite, &mut Transform),
        (With<ScanlineSprite>, Without<UpscaleSprite>, Without<HashSprite>),
    >,
) {
    let Ok(window) = windows.single() else { return; };
    let logical_w = window.width();
    let logical_h = window.height();
    let (left, _top, play_w, play_h) = play_area_screen_rect(logical_w, logical_h);
    // Play sprite — centred in the available area to the right of the UI.
    let world_x = left + play_w / 2.0 - logical_w / 2.0;
    let target = Vec2::new(play_w, play_h);
    for (mut s, mut tf) in &mut play_sprites {
        if s.custom_size != Some(target) { s.custom_size = Some(target); }
        if (tf.translation.x - world_x).abs() > 0.001 { tf.translation.x = world_x; }
        if tf.translation.y != 0.0 { tf.translation.y = 0.0; }
    }
    // Scanline overlay — locked to the play sprite's screen rect.
    for (mut s, mut tf) in &mut scanline_sprites {
        if s.custom_size != Some(target) { s.custom_size = Some(target); }
        if (tf.translation.x - world_x).abs() > 0.001 { tf.translation.x = world_x; }
        if tf.translation.y != 0.0 { tf.translation.y = 0.0; }
    }
    // Hash backdrop — covers the entire window. Tiled mode handles the rest.
    let win_size = Vec2::new(logical_w, logical_h);
    for mut s in &mut hash_sprites {
        if s.custom_size != Some(win_size) { s.custom_size = Some(win_size); }
    }
}

/// Snap the HUD camera's viewport to the play-area screen rect each
/// frame. Viewport is in *physical* pixels (Bevy Camera API), so we
/// scale by the window's DPI factor. Sister system to `resize_upscale_sprite`
/// — they consume the same `play_area_screen_rect` and must stay aligned.
pub fn update_hud_camera_viewport(
    windows: Query<&Window, With<PrimaryWindow>>,
    mut hud: Query<&mut Camera, With<HudCamera>>,
    mut last_phys: Local<UVec2>,
    mut steady_frames: Local<u8>,
) {
    let Ok(window) = windows.single() else { return; };
    let phys_target = window.physical_size();

    // Wipe the viewport in three cases so wgpu never sees a scissor
    // larger than the swap-chain target:
    //
    //   1. Window is degenerate (≤ 1 px on either axis) — minimize,
    //      mid-resize, iframe collapse.
    //   2. Physical size changed since last frame — the surface has
    //      to reconfigure, and the first re-rendered frame might
    //      still see the old 1×1 placeholder texture.
    //   3. We haven't yet seen the current size stable for two
    //      frames — guards against startup races where the window
    //      reports its final size before the surface is configured.
    //
    // While the viewport is `None`, the HUD camera draws to the
    // whole target — visually fine for one frame (the HUD just
    // covers the full window briefly) and crash-free.
    let bail = |hud: &mut Query<&mut Camera, With<HudCamera>>| {
        for mut cam in hud {
            if cam.viewport.is_some() { cam.viewport = None; }
        }
    };
    if phys_target.x <= 1 || phys_target.y <= 1 {
        *steady_frames = 0;
        bail(&mut hud);
        return;
    }
    if phys_target != *last_phys {
        *last_phys = phys_target;
        *steady_frames = 1;
        bail(&mut hud);
        return;
    }
    if *steady_frames < 3 {
        *steady_frames = steady_frames.saturating_add(1);
        bail(&mut hud);
        return;
    }

    let logical_w = window.width();
    let logical_h = window.height();
    let scale = window.scale_factor();
    let (left, top, play_w, play_h) = play_area_screen_rect(logical_w, logical_h);

    let raw_x = (left * scale).round().max(0.0) as u32;
    let raw_y = (top  * scale).round().max(0.0) as u32;
    let raw_w = (play_w * scale).round().max(1.0) as u32;
    let raw_h = (play_h * scale).round().max(1.0) as u32;
    // Clamp so the viewport always lives inside the swap-chain
    // target. `window.physical_size()` can transiently report a
    // larger value than the surface texture during resize (the
    // window reports its target dimensions before the swapchain
    // reconfigures), so prefer the camera's actual target size
    // when it's available — that's what wgpu validates against.
    let cam_target = hud
        .iter()
        .next()
        .and_then(|c| c.physical_target_size());
    let bound = cam_target.unwrap_or(phys_target);
    let phys_pos = UVec2::new(
        raw_x.min(bound.x.saturating_sub(1)),
        raw_y.min(bound.y.saturating_sub(1)),
    );
    let phys_size = UVec2::new(
        raw_w.min(bound.x.saturating_sub(phys_pos.x)),
        raw_h.min(bound.y.saturating_sub(phys_pos.y)),
    );
    if phys_size.x == 0 || phys_size.y == 0 {
        bail(&mut hud);
        return;
    }

    // Viewport doesn't impl PartialEq, so compare its fields manually
    // before writing to avoid spamming change detection every frame.
    for mut cam in &mut hud {
        let needs_update = match &cam.viewport {
            Some(v) => v.physical_position != phys_pos || v.physical_size != phys_size,
            None => true,
        };
        if needs_update {
            cam.viewport = Some(Viewport {
                physical_position: phys_pos,
                physical_size: phys_size,
                depth: 0.0..1.0,
            });
        }
    }
}
