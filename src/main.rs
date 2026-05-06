use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use bevy::image::{ImageSampler, ImageSamplerDescriptor};
use bevy::render::camera::RenderTarget;
use bevy::sprite::SpriteImageMode;
use bevy::render::view::{Msaa, RenderLayers};
use bevy::window::PrimaryWindow;

mod balance;
mod beam;
mod bullet;
mod components;
mod effects;
mod enemy;
mod i18n;
mod modes;
mod palette;
mod pier;
mod trails;
mod turret;
mod ui;
mod wave;
mod weapon;

use balance::*;
use beam::{beam_apply_damage, update_beams};
use bullet::{bullet_collisions, bullet_update};
use components::{Faction, FactionKind, Friendly, Health, Heading, Velocity};
use effects::{
    apply_hit_fx_visuals, tick_hit_fx, update_hit_particles, update_muzzle_flashes,
    EffectMeshes, HitFx,
};
use enemy::{bomber_detonate, enemy_ai, enemy_fire, spawn_enemies, Enemy};
use modes::{
    apply_crt_mode, apply_night_mode, apply_window_mode,
    effective_ui_width, handle_desktop_drag_resize, handle_desktop_escape,
    play_area_screen_rect,
    CrtMode, GameMode, NightMode, ScanlineSprite, WindowMode,
};
use palette::{
    Palette, PaletteMaterials, PlayCamera, UpscaleCamera,
    apply_palette, darken, hex,
};
use pier::{draft_input, sync_pier_visuals, update_draft_ui, Pier, PierVisual, WaveDraft};
use trails::{empty_dynamic_mesh, update_enemy_trails, update_trail, ShipPath, Trail};
use turret::{
    sync_turret_config, turret_aim_fire,
    BarrelIndex, SlotCfg, TurretBarrel, TurretConfig, TurretSlot,
};
use ui::{
    setup_ui, ui_button_system, update_damage_bars, update_score_text, update_slot_labels,
    update_wave_ui, DamageStats,
};
use wave::{wave_orchestrator, WaveState};
use weapon::WeaponType;

// All numeric/layout constants live in `balance.rs` (re-exported via
// `use balance::*`). Translation strings live in `data/translations.csv` and
// are looked up via `tr("key")`. Colors live in `palette.rs`.

// `Palette`, `PaletteMaterials`, `apply_palette`, helpers, and weapon hexes
// are in `palette.rs`. The `PaletteMaterials::*_for` weapon-lookup methods
// live with `WeaponType` in `weapon.rs`.

// `EffectMeshes` and FX components live in `effects.rs` now.
// Generic components (Friendly, Health, Velocity, Heading, Faction*) live
// in `components.rs`.

// Enemy / EnemyState / EnemyVariant moved to `enemy.rs`.

// Bomber, wave-mode, and pier-layout tunables moved to `balance.rs`.

// BuildingType + Pier + WaveDraft + pier helpers + draft systems  →  pier.rs

// Health, Velocity, Heading, Faction(Kind), Friendly  →  components.rs
// WeaponType + impl + PaletteMaterials::*_for          →  weapon.rs
// Shotgun + beam tunables                              →  balance.rs

// TurretSlot / BarrelIndex / TurretBarrel  →  turret.rs
// Barrel + bullet geometry constants       →  balance.rs

// Bullet  →  bullet.rs
// Beam / BeamHit / BeamPending  →  beam.rs
// Trail / EnemyTrail / ShipPath + ribbon-mesh helpers + trail systems  →  trails.rs
// MuzzleFlash / HitParticle / HitFx + impls + tickers  →  effects.rs

// ScoreText, UiPanel, DamageStats, all UI marker components, button/label
// enums, setup_ui, and every update_*_text/labels/bars system live in `ui.rs`.

// PlayCamera + UpscaleCamera markers live in `palette.rs` so apply_palette
// can reach them without depending on rendering internals.

#[derive(Component)]
struct UpscaleSprite;

/// Tiled diagonal-hash sprite that fills the full window behind the play
/// area. Visible only in the "letterbox" region around the play sprite —
/// the play sprite covers the centre and the UI panel covers the left.
#[derive(Component)]
struct HashSprite;

#[derive(Resource)]
struct HashImage(Handle<Image>);

// ScanlineSprite  →  modes.rs (lives with the CRT toggle that drives it)

// ---------- Resources ----------
#[derive(Resource)]
pub struct Score(pub u32);

#[derive(Resource)]
pub struct SpawnTimer { pub t: f32, pub elapsed: f32 }

#[derive(Resource)]
struct PlayRenderImage(Handle<Image>);

// TurretConfig + SlotCfg  →  turret.rs

#[derive(Resource, Default)]
struct ConfigDirty(bool);

// DamageStats + all UI marker components + button/label enums  →  ui.rs

#[derive(Resource)]
struct TrailTimer(f32);

// GameMode + WindowMode + NightMode + CrtMode  →  modes.rs
// Their `apply_*_mode` systems + `handle_desktop_*` + layout helpers also
// live there.

// WavePhase + WaveState                            →  wave.rs
// PierVisual + PierBuildingMarker                  →  pier.rs
// DraftPanel + DraftCard{Button,Title,Desc}        →  pier.rs

// WaveHpUi/Fill/Text + UiPanel  →  ui.rs

// DesktopHint  →  modes.rs

// ---------- App ----------
fn main() {
    let mut cfg = TurretConfig::default();
    cfg.slots[0] = SlotCfg { equipped: true, weapon: WeaponType::Standard, damage: 1, fire_rate: 4.0, barrels: 1 };
    for i in 1..8 {
        cfg.slots[i] = SlotCfg { equipped: false, weapon: WeaponType::Standard, damage: 1, fire_rate: 4.0, barrels: 1 };
    }

    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Ship Game".into(),
                resolution: (WINDOW_W, WINDOW_H).into(),
                ..default()
            }),
            ..default()
        }))
        .insert_resource(ClearColor(Color::srgb(0.05, 0.05, 0.08)))
        .insert_resource(Score(0))
        .insert_resource(SpawnTimer { t: 0.0, elapsed: 0.0 })
        .insert_resource(cfg)
        .insert_resource(ConfigDirty(false))
        .insert_resource(DamageStats::default())
        .insert_resource(TrailTimer(0.0))
        .insert_resource(Palette::aap64_naval())
        .insert_resource(ShipPath::default())
        .insert_resource(WindowMode::default())
        .insert_resource(NightMode::default())
        .insert_resource(CrtMode::default())
        .insert_resource(GameMode::default())
        .insert_resource(WaveState::default())
        .insert_resource(Pier::default())
        .insert_resource(WaveDraft::default())
        .add_systems(Startup, (setup_render, setup_world, setup_ui).chain())
        .add_systems(Update, (
            // Sim / movement. apply_night_mode → apply_palette must be ordered
            // so a night-mode toggle propagates to the camera in the same frame.
            (apply_night_mode, apply_palette, update_hash_image).chain(),
            friendly_movement,
            enemy_ai,
            apply_velocity,
            bomber_detonate,
            spawn_enemies,
            sync_turret_config,
            // Beam damage must run AFTER turret_aim_fire so the BeamPending
            // entities it spawns are visible. .chain() inserts the apply-
            // deferred sync point we need to see them this frame.
            (turret_aim_fire, beam_apply_damage).chain(),
            enemy_fire,
            bullet_update,
            bullet_collisions,
        ))
        .add_systems(Update, (
            // Visuals / FX / UI
            update_trail,
            update_enemy_trails,
            tick_hit_fx,
            apply_hit_fx_visuals,
            update_muzzle_flashes,
            update_beams,
            update_hit_particles,
            update_score_text,
            ui_button_system,
            update_slot_labels,
            update_damage_bars,
            resize_upscale_sprite,
            handle_desktop_escape,
            handle_desktop_drag_resize,
            apply_window_mode,
            apply_crt_mode,
        ))
        .add_systems(Update, (
            // Wave-mode systems live in their own bundle so we don't blow
            // past the 20-system tuple limit on the visuals/UI block.
            wave_orchestrator,
            update_wave_ui,
            sync_pier_visuals,
            draft_input,
            update_draft_ui,
        ))
        .run();
}

/// Snap the upscale sprite to an integer multiple of the internal resolution.
/// Without this, fractional sampling (e.g. one internal pixel mapping to 3.5
/// screen pixels) shimmers as objects move — that's the "laggy" feel.
// handle_desktop_escape, handle_desktop_drag_resize, apply_crt_mode  →  modes.rs
/// Escape exits desktop mode back to the windowed UI. No-op in windowed mode.

// ArenaDisposeFilter, clear_arena, place_friendly_at_dock,
// spawn_wave, wave_orchestrator  →  wave.rs


// update_wave_ui  →  ui.rs


// pier_cell_world, pier_cell_at, pier_damage_bonus, pier_range_mult,
// pier_drydock_heal, generate_draft, rebuild_pier_buildings,
// sync_pier_visuals, draft_input, update_draft_ui  →  pier.rs


/// On toggle, write the night-mode override into the live `Palette` so that
// apply_night_mode + apply_window_mode  →  modes.rs
/// `apply_palette` propagates the new ocean color to the camera + materials.

/// CRT scanline overlay: `PLAY_INTERNAL × PLAY_INTERNAL` BGRA texture where
/// every other row is a translucent black band. Sized to match the play-area
/// internal resolution so when nearest-neighbor upscaled, each band lands on
/// exactly one internal pixel of screen height.
fn make_scanline_image() -> Image {
    let w = PLAY_INTERNAL;
    let h = PLAY_INTERNAL;
    // ~38% black on darkened rows — visible scanlines without smothering the
    // colors underneath. Alpha-blended over the play sprite by Bevy's default
    // sprite shader.
    const DARK_ALPHA: u8 = 96;
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let dark = (y % 2) == 0;
        let bgra = if dark { [0u8, 0, 0, DARK_ALPHA] } else { [0u8, 0, 0, 0] };
        for _ in 0..w { data.extend_from_slice(&bgra); }
    }
    let mut img = Image::new(
        Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::Bgra8UnormSrgb,
        bevy::render::render_asset::RenderAssetUsages::default(),
    );
    img.sampler = ImageSampler::nearest();
    img
}

/// Build a 192×192 BGRA tile with equal-width diagonal stripes — `light`
/// stripes on `(x+y) % period < period/2`, otherwise `dark`. Tileable.
/// Band width = 96 px; the 192-period repeats seamlessly along both axes.
fn make_hash_image(light: Color, dark: Color) -> Image {
    const TILE: u32 = 192;
    const HALF: u32 = TILE / 2;
    let to_bgra = |c: Color| {
        let s: bevy::color::Srgba = c.into();
        [
            (s.blue  * 255.0).round() as u8,
            (s.green * 255.0).round() as u8,
            (s.red   * 255.0).round() as u8,
            255u8,
        ]
    };
    let lb = to_bgra(light);
    let db = to_bgra(dark);
    let mut data = Vec::with_capacity((TILE * TILE * 4) as usize);
    for y in 0..TILE {
        for x in 0..TILE {
            let band = ((x + y) % TILE) < HALF;
            let bgra = if band { lb } else { db };
            data.extend_from_slice(&bgra);
        }
    }
    let mut img = Image::new(
        Extent3d { width: TILE, height: TILE, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::Bgra8UnormSrgb,
        bevy::render::render_asset::RenderAssetUsages::default(),
    );
    img.sampler = ImageSampler::nearest();
    img
}

/// Regenerate the hash tile when the palette OR night mode changes so the
/// stripes always match the current ocean. Day-mode dark = #3b5dc9; in
/// night mode the dark stripe is much lower-luminance so the hashing stays
/// subtle against the dark ocean instead of looking like bright stripes.
fn update_hash_image(
    palette: Res<Palette>,
    night: Res<NightMode>,
    hash: Option<Res<HashImage>>,
    mut images: ResMut<Assets<Image>>,
) {
    if !palette.is_changed() && !night.is_changed() { return; }
    let Some(hash) = hash else { return; };
    // Day mode: derive the dark stripe from the ocean (same hue, ~70%
    // luminance) so the hashing reads as a "shaded ocean" instead of a
    // separate saturated blue. Night mode keeps a near-black hash.
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
// play_area_screen_rect + effective_ui_width  →  modes.rs


/// Snap the upscale sprite to an integer multiple of the internal resolution
/// AND reposition it within the window each frame. Without integer snapping
/// one internal pixel can map to 3.5 screen pixels and shimmer as things move.
fn resize_upscale_sprite(
    windows: Query<&Window, With<PrimaryWindow>>,
    mode: Res<WindowMode>,
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
    let (left, _top, size) = play_area_screen_rect(logical_w, logical_h, effective_ui_width(&mode));
    // Play sprite — centred in the available area to the right of the UI.
    let world_x = left + size / 2.0 - logical_w / 2.0;
    let target = Vec2::splat(size);
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

// `apply_palette` lives in `palette.rs`.

// ---------- Setup ----------
fn setup_render(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    palette: Res<Palette>,
) {
    let size = Extent3d { width: PLAY_INTERNAL, height: PLAY_INTERNAL, depth_or_array_layers: 1 };
    let mut img = Image::new_fill(
        size,
        TextureDimension::D2,
        &[0, 0, 0, 255],
        TextureFormat::Bgra8UnormSrgb,
        bevy::render::render_asset::RenderAssetUsages::default(),
    );
    img.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING
        | TextureUsages::COPY_DST
        | TextureUsages::RENDER_ATTACHMENT;
    img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor::nearest());

    let handle = images.add(img);
    commands.insert_resource(PlayRenderImage(handle.clone()));

    // Play camera renders to image, sees only PLAY_LAYER.
    // MSAA off — multi-sampling against a low-res render target softens
    // every primitive edge (sub-pixels get partially-resolved), which kills
    // the chunky-pixel look. Off makes capsule edges fall cleanly on the grid.
    commands.spawn((
        Camera2d,
        Camera {
            target: RenderTarget::Image(handle.clone().into()),
            clear_color: ClearColorConfig::Custom(palette.ocean),
            order: -1,
            ..default()
        },
        Projection::Orthographic(OrthographicProjection {
            scaling_mode: bevy::render::camera::ScalingMode::Fixed { width: PLAY_WORLD, height: PLAY_WORLD },
            ..OrthographicProjection::default_2d()
        }),
        RenderLayers::layer(PLAY_LAYER),
        PlayCamera,
        Msaa::Off,
    ));

    // UI / upscale camera (default layer + upscale layer). Also MSAA off so
    // the upscale sprite samples the internal image without smoothing.
    // Clear color = ocean so any pixels outside the play sprite (e.g. between
    // the UI panel and the play area, or in desktop mode if the window
    // mismatches by 1 px) match the water seamlessly — no black border.
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
    ));

    // Diagonal-hash backdrop, tiled across the entire window. Sits BEHIND the
    // play sprite (z=-1) so the play area covers the centre and the hashing
    // shows in the surrounding letterbox / right-of-UI region.
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

    // Sprite that displays the render target, on UPSCALE_LAYER, positioned in screen space.
    // Initial size/position for frame 0; resize_upscale_sprite refines it
    // every frame using the actual window size.
    let (left0, _top0, size0) = play_area_screen_rect(WINDOW_W, WINDOW_H, UI_WIDTH);
    let world_x0 = left0 + size0 / 2.0 - WINDOW_W / 2.0;
    commands.spawn((
        Sprite {
            image: handle,
            custom_size: Some(Vec2::splat(size0)),
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
            custom_size: Some(Vec2::splat(size0)),
            ..default()
        },
        Transform::from_xyz(world_x0, 0.0, 1.0),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        ScanlineSprite,
    ));
}

fn setup_world(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    cfg: Res<TurretConfig>,
    palette: Res<Palette>,
) {
    // Build palette-material handles once. Every entity in the play world
    // references one of these — runtime palette swaps update them all.
    let pm = PaletteMaterials::build(&palette, &mut materials);

    // --- 1px play-area border, drawn inside the play world ---
    let border_h = meshes.add(Rectangle::new(PLAY_WORLD, 1.0));
    let border_v = meshes.add(Rectangle::new(1.0, PLAY_WORLD));
    let half_w = PLAY_WORLD / 2.0 - 0.5;
    for (m, x, y) in [
        (border_h.clone(), 0.0,  half_w),
        (border_h.clone(), 0.0, -half_w),
        (border_v.clone(),  half_w, 0.0),
        (border_v.clone(), -half_w, 0.0),
    ] {
        commands.spawn((
            Mesh2d(m),
            MeshMaterial2d(pm.border.clone()),
            // Border on top so it always frames the action.
            Transform::from_xyz(x, y, 6.0),
            RenderLayers::layer(PLAY_LAYER),
        ));
    }

    // --- Friendly trail: a ribbon mesh rebuilt every frame from path history.
    // Mesh positions live in world space, so the entity transform stays at origin.
    let trail_mesh = meshes.add(empty_dynamic_mesh());
    commands.spawn((
        Mesh2d(trail_mesh),
        MeshMaterial2d(pm.trail.clone()),
        Transform::from_xyz(0.0, 0.0, 0.5),
        Trail,
        RenderLayers::layer(PLAY_LAYER),
    ));

    // --- Friendly ship: rounded capsule hull ---
    let hull_radius = HULL_WIDTH / 2.0;
    let hull_inner = HULL_LEN - HULL_WIDTH;
    let hull_mesh = meshes.add(Capsule2d::new(hull_radius, hull_inner));

    let ship = commands.spawn((
        Mesh2d(hull_mesh),
        MeshMaterial2d(pm.hull.clone()),
        Transform::from_xyz(0.0, 0.0, 1.0),
        Visibility::Inherited,
        Friendly,
        Faction(FactionKind::Friendly),
        Health(100),
        Velocity(Vec2::new(0.0, FRIENDLY_SPEED)),
        Heading(0.0),
        HitFx::new(pm.hull.clone()),
        RenderLayers::layer(PLAY_LAYER),
    )).id();

    // Pier grid — 8 stacked cells along the LHS wall, drawn as thin grid
    // lines (no filled rectangles). Doubles as the dock visual and the
    // placement surface for upgrade buildings. Hidden until Wave mode.
    let pier_top    = PIER_Y_START - PIER_Y_STEP / 2.0;
    let pier_bottom = PIER_Y_START + (8.0 - 1.0) * PIER_Y_STEP + PIER_Y_STEP / 2.0;
    let pier_height = pier_bottom - pier_top;
    let h_line_mesh = meshes.add(Rectangle::new(PIER_CELL_W, 1.0));
    let v_line_mesh = meshes.add(Rectangle::new(1.0, pier_height));

    // 9 horizontal lines (top + bottom + 7 separators) and 2 vertical edges.
    for i in 0..=8 {
        let y = pier_top + i as f32 * PIER_Y_STEP;
        commands.spawn((
            Mesh2d(h_line_mesh.clone()),
            MeshMaterial2d(pm.border.clone()),
            Transform::from_xyz(PIER_CELL_X, y, 0.4),
            Visibility::Hidden,
            PierVisual,
            RenderLayers::layer(PLAY_LAYER),
        ));
    }
    for x_off in [-PIER_CELL_W / 2.0, PIER_CELL_W / 2.0] {
        commands.spawn((
            Mesh2d(v_line_mesh.clone()),
            MeshMaterial2d(pm.border.clone()),
            Transform::from_xyz(PIER_CELL_X + x_off, (pier_top + pier_bottom) / 2.0, 0.4),
            Visibility::Hidden,
            PierVisual,
            RenderLayers::layer(PLAY_LAYER),
        ));
    }

    // Friendly turrets. Barrel kept ≥1.5 wide so it doesn't alias to zero
    // pixels at off-axis rotations now that MSAA is off — sub-pixel rects
    // were vanishing entirely between integer-grid angles.
    let base_mesh = meshes.add(Circle::new(2.0));
    let barrel_mesh = meshes.add(Rectangle::new(1.5, 4.0));

    for (i, (lx, ly)) in TURRET_POSITIONS.iter().enumerate() {
        let slot = cfg.slots[i];
        let visible = slot.equipped;
        let mount = TURRET_MOUNTS[i];
        let turret_mat = pm.turret_for(slot.weapon).clone();
        let mut ec = commands.spawn((
            Mesh2d(base_mesh.clone()),
            MeshMaterial2d(turret_mat.clone()),
            Transform::from_xyz(*lx, *ly, 2.0).with_rotation(Quat::from_rotation_z(mount)),
            if visible { Visibility::Inherited } else { Visibility::Hidden },
            TurretSlot {
                index: i,
                barrel_angle: mount,
                mount_angle: mount,
                fire_cd: 0.0,
                damage: slot.damage,
                fire_rate: slot.fire_rate,
                weapon: slot.weapon,
                range_mult: 1.0,
                barrels: slot.barrels.max(1),
                next_barrel: 0,
            },
            RenderLayers::layer(PLAY_LAYER),
        ));
        ec.insert(ChildOf(ship));
        let turret_id = ec.id();

        // Spawn TWO barrel children. In single-barrel mode only barrel 0 is
        // shown, centered. In twin mode both are shown, splayed port/stbd.
        // sync_turret_config keeps positions + visibility in sync with cfg.
        for barrel_i in 0..2u8 {
            let initial_visible = visible && (barrel_i == 0 || slot.barrels >= 2);
            let lateral = if slot.barrels >= 2 {
                if barrel_i == 0 { -BARREL_LATERAL } else { BARREL_LATERAL }
            } else { 0.0 };
            let barrel = commands.spawn((
                Mesh2d(barrel_mesh.clone()),
                MeshMaterial2d(turret_mat.clone()),
                Transform::from_xyz(lateral, 3.0, 0.1),
                if initial_visible { Visibility::Inherited } else { Visibility::Hidden },
                TurretBarrel,
                BarrelIndex(barrel_i),
                RenderLayers::layer(PLAY_LAYER),
            )).id();
            commands.entity(barrel).insert(ChildOf(turret_id));
        }
    }

    // Publish the palette material handles so other systems (spawn_enemies,
    // turret_aim_fire, enemy_fire) can reference them.
    commands.insert_resource(pm);

    // Cache effect meshes once so muzzle flashes / hit particles don't allocate.
    // Muzzle flash is a chunky elongated capsule (more obvious than the old slim one).
    // Particle is a small streak capsule, oriented along its velocity vector.
    // Bullet + enemy primitives are also cached here so every bullet / enemy
    // can share the same mesh handle and benefit from Bevy's draw-call batching.
    commands.insert_resource(EffectMeshes {
        muzzle_flash:          meshes.add(Capsule2d::new(1.6, 4.0)),
        particle:              meshes.add(Capsule2d::new(0.7, 1.6)),
        bullet_friendly_outer: meshes.add(Capsule2d::new(2.0, 1.5)),
        bullet_friendly_inner: meshes.add(Capsule2d::new(1.3, 1.5)),
        bullet_enemy_outer:    meshes.add(Capsule2d::new(1.5, 1.5)),
        bullet_enemy_inner:    meshes.add(Capsule2d::new(0.8, 1.5)),
        enemy_body:            meshes.add(Capsule2d::new(ENEMY_WIDTH / 2.0, ENEMY_LEN - ENEMY_WIDTH)),
        enemy_turret_base:     meshes.add(Circle::new(1.0)),
        enemy_turret_barrel:   meshes.add(Rectangle::new(0.9, 3.5)),
        bomber_warhead:        meshes.add(Circle::new(1.4)),
        beam:                  meshes.add(Rectangle::new(1.0, BEAM_LENGTH)),
    });
}

// setup_ui + every UI spawn helper  →  ui.rs
// ---------- UI ----------
// Theme palette for the LHS panel — kept separate from the gameplay Palette
// so the panel stays legible regardless of the in-game color choices.
// UI theme constants moved to `palette.rs` — re-exported via `use palette::*`.


// ---------- Systems ----------
fn friendly_movement(
    time: Res<Time>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mode: Res<WindowMode>,
    game_mode: Res<GameMode>,
    enemies: Query<&Transform, (With<Enemy>, Without<Friendly>)>,
    mut q: Query<(&mut Transform, &mut Velocity, &mut Heading), With<Friendly>>,
) {
    let dt = time.delta_secs();
    let Ok(win) = windows.single() else { return; };
    let cursor = win.cursor_position();

    let (play_left, play_top, play_screen) =
        play_area_screen_rect(win.width(), win.height(), effective_ui_width(&mode));

    // Wave mode is fully auto-battle — ignore the cursor entirely so the
    // enemy-seeking branch below takes over regardless of mouse position.
    let target_world: Option<Vec2> = if matches!(*game_mode, GameMode::Wave) {
        None
    } else {
        cursor.and_then(|c| {
            if c.x >= play_left && c.x <= play_left + play_screen
                && c.y >= play_top && c.y <= play_top + play_screen {
                let nx = (c.x - play_left) / play_screen;
                let ny = (c.y - play_top) / play_screen;
                Some(Vec2::new(
                    (nx - 0.5) * PLAY_WORLD,
                    (0.5 - ny) * PLAY_WORLD,
                ))
            } else { None }
        })
    };

    for (mut tf, mut vel, mut heading) in &mut q {
        let pos = tf.translation.truncate();

        // Pick a steering target. If the cursor is over the play area, follow
        // it. Otherwise compute a "tactical" target that engages the nearest
        // enemy at a comfortable range, or drifts toward the centroid when
        // multiple enemies are around.
        let target = if let Some(t) = target_world {
            t
        } else {
            // Find nearest enemy + the centroid of nearby enemies.
            let mut nearest: Option<(f32, Vec2)> = None;
            let mut centroid_sum = Vec2::ZERO;
            let mut centroid_count = 0u32;
            for etf in &enemies {
                let ep = etf.translation.truncate();
                let d = ep.distance(pos);
                if nearest.map_or(true, |(bd, _)| d < bd) { nearest = Some((d, ep)); }
                if d < TURRET_RANGE * 1.5 {
                    centroid_sum += ep;
                    centroid_count += 1;
                }
            }
            if let Some((d, ep)) = nearest {
                let to = ep - pos;
                let unit = to.normalize_or_zero();
                let desired_range = TURRET_RANGE * 0.7;
                if d > desired_range + 8.0 {
                    // Approach: aim at a point just beyond the enemy.
                    ep
                } else if d < desired_range - 8.0 {
                    // Too close: back away (target behind us).
                    pos - unit * 30.0
                } else {
                    // Hold range: orbit perpendicularly so multiple turrets bear.
                    // Bias the orbit direction toward the enemy centroid so we
                    // sweep toward where the action is.
                    let perp = Vec2::new(-unit.y, unit.x);
                    let bias = if centroid_count > 0 {
                        let c = centroid_sum / centroid_count as f32;
                        if perp.dot(c - pos) >= 0.0 { 1.0 } else { -1.0 }
                    } else { 1.0 };
                    pos + perp * (bias * 30.0)
                }
            } else {
                // No enemies: drift toward play-area center.
                Vec2::ZERO
            }
        };

        // Keep target inside the playable area so we don't crash the wall.
        let margin = HULL_HALF_LEN + 2.0;
        let bound = PLAY_WORLD / 2.0 - margin;
        let target = Vec2::new(target.x.clamp(-bound, bound), target.y.clamp(-bound, bound));

        let to = target - pos;
        if to.length_squared() > 1.0 {
            let desired = to.y.atan2(to.x) - std::f32::consts::FRAC_PI_2;
            heading.0 = approach_angle(heading.0, desired, FRIENDLY_TURN_RATE * dt);
        }
        let dir = Vec2::new(-heading.0.sin(), heading.0.cos());
        vel.0 = dir * FRIENDLY_SPEED;
        tf.rotation = Quat::from_rotation_z(heading.0);
    }
}

/// Steer `cur` toward `tgt` by at most `max` radians, taking the shorter way
/// around the circle. Used by friendly + enemy heading systems.
pub fn approach_angle(cur: f32, tgt: f32, max: f32) -> f32 {
    let mut d = (tgt - cur + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
    if d > max { d = max; }
    if d < -max { d = -max; }
    cur + d
}

// `enemy_ai` is in `enemy.rs`.

fn apply_velocity(time: Res<Time>, mut q: Query<(&mut Transform, &Velocity)>) {
    let dt = time.delta_secs();
    for (mut tf, v) in &mut q {
        tf.translation.x += v.0.x * dt;
        tf.translation.y += v.0.y * dt;
    }
}

// `bomber_detonate`, `spawn_enemies`, `spawn_enemy` live in `enemy.rs`.
// `update_beams`, `beam_apply_damage` live in `beam.rs`.
// FX systems live in `effects.rs`. Trail systems live in `trails.rs`.
// turret block stripped at line 2065

// → turret.rs (sync_turret_config + turret_aim_fire + spawn_friendly_bullet)


// `enemy_fire` is in `enemy.rs`.
// `bullet_update`, `bullet_collisions` are in `bullet.rs`.

// update_score_text + ui_button_system + update_slot_labels + update_damage_bars  →  ui.rs

