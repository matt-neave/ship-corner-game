use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use bevy::image::{ImageSampler, ImageSamplerDescriptor};
use bevy::render::camera::RenderTarget;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::render::render_asset::RenderAssetUsages;
use bevy::render::view::{Msaa, RenderLayers};
use bevy::window::PrimaryWindow;
use rand::Rng;
use std::collections::VecDeque;

// ---------- Config ----------
const WINDOW_W: f32 = 1280.0;
const WINDOW_H: f32 = 800.0;
const UI_WIDTH: f32 = 280.0;
// Low-res render target — 1:1 world unit per internal pixel.
// Display upscales ~3.6x → chunky stair-stepped pixel edges.
const PLAY_INTERNAL: u32 = 200;
const PLAY_WORLD: f32 = 200.0;

const PLAY_LAYER: usize = 1;
const UPSCALE_LAYER: usize = 2;

const FRIENDLY_SPEED: f32 = 28.0;
const FRIENDLY_TURN_RATE: f32 = 3.6; // rad/s
const ENEMY_SPEED: f32 = 18.0;
const ENEMY_TURN_RATE: f32 = 0.9;
const TURRET_RANGE: f32 = 60.0;
const TURRET_PIVOT: f32 = std::f32::consts::FRAC_PI_2; // 90°/s
const ENEMY_RANGE: f32 = 45.0;
const ENEMY_FIRE_RATE: f32 = 1.0;
const ENEMY_HP: i32 = 10;
const BULLET_SPEED: f32 = 110.0;

// Palette is defined as a Resource — see Palette / PaletteMaterials below.
// Swap palettes by mutating the Palette resource; apply_palette propagates.

// Hull dimensions (world units == internal pixels).
// Spec target: enemy ~10px long, friendly larger but still chunky.
const HULL_LEN: f32 = 22.0;
const HULL_WIDTH: f32 = 8.0;
const HULL_HALF_LEN: f32 = HULL_LEN / 2.0;
const ENEMY_LEN: f32 = 10.0;
const ENEMY_WIDTH: f32 = 5.0;

// Cuniberti turret layout. Local hull coords: ship faces +Y (up).
// 0=bow, 7=stern, 1-6 wings (port/starboard pairs); mid pair widest beam.
const TURRET_POSITIONS: [(f32, f32); 8] = [
    ( 0.0,  9.0),  // bow centerline
    (-2.0,  5.0),  // fore wing pair (port)
    ( 2.0,  5.0),  //                  (stbd)
    (-3.0,  0.0),  // mid wing pair  (port, widest beam)
    ( 3.0,  0.0),  //                  (stbd)
    (-2.0, -5.0),  // aft wing pair  (port)
    ( 2.0, -5.0),  //                  (stbd)
    ( 0.0, -9.0),  // stern centerline
];

// Mount angle per turret + half-arc per turret. Hull frame, 0 = +Y forward.
// Heading convention: dir = (-sin(a), cos(a)). +PI/2 = port (-X), -PI/2 = stbd (+X).
//
// The 4 wing turrets (idx 1,2,5,6) sit on diagonals from the ship center and
// rest along that diagonal. They get a 120° firing arc (±60°), so each can
// swing from "fully forward" through its diagonal to "fully sideways".
// The 4 axial turrets (bow, stern, mid port/stbd) keep a 90° arc.
const PI_2: f32 = std::f32::consts::FRAC_PI_2;
const PI_3: f32 = std::f32::consts::FRAC_PI_3;
const PI_4: f32 = std::f32::consts::FRAC_PI_4;
const PI_F: f32 = std::f32::consts::PI;
const TURRET_MOUNTS: [f32; 8] = [
     0.0,         // bow centerline → forward
     PI_4,        // fore port wing → forward-port (NW diagonal)
    -PI_4,        // fore stbd wing → forward-stbd (NE diagonal)
     PI_2,        // mid port → port (sideways left)
    -PI_2,        // mid stbd → starboard
     3.0 * PI_4,  // aft port wing → backward-port (SW diagonal)
    -3.0 * PI_4,  // aft stbd wing → backward-stbd (SE diagonal)
     PI_F,        // stern centerline → backward
];
const TURRET_NAMES: [&str; 8] = [
    "BOW",
    "FORE PORT",
    "FORE STBD",
    "MID PORT",
    "MID STBD",
    "AFT PORT",
    "AFT STBD",
    "STERN",
];

const TURRET_ARC_HALVES: [f32; 8] = [
    PI_4, // bow: ±45°
    PI_3, // fore port: ±60°
    PI_3, // fore stbd: ±60°
    PI_4, // mid port: ±45°
    PI_4, // mid stbd: ±45°
    PI_3, // aft port: ±60°
    PI_3, // aft stbd: ±60°
    PI_4, // stern: ±45°
];

// ---------- Palette ----------
// Each role names a single semantic color. To recolor the game, mutate the
// `Palette` resource at runtime (or change the active palette below at compile
// time). `apply_palette` watches for changes and propagates them to all
// shared materials + the play camera's clear color in one place.
#[derive(Resource, Clone, Debug)]
struct Palette {
    ocean: Color,
    border: Color,
    hull: Color,
    hull_accent: Color,
    turret: Color,
    enemy: Color,
    enemy_accent: Color,
    bullet_friendly: Color,
    bullet_enemy: Color,
    trail: Color,
}

fn hex(s: &str) -> Color {
    let s = s.trim_start_matches('#');
    let r = u8::from_str_radix(&s[0..2], 16).unwrap_or(255);
    let g = u8::from_str_radix(&s[2..4], 16).unwrap_or(0);
    let b = u8::from_str_radix(&s[4..6], 16).unwrap_or(255);
    Color::srgb(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
}

impl Palette {
    /// Selection from the AAP-64 palette — dark naval hull + arcadey bullets.
    fn aap64_naval() -> Self {
        Self {
            ocean:           hex("#41a6f6"), //  deep cobalt water
            border:          hex("#c7cfdd"), //  cool pale grey-blue frame
            hull:            hex("#94b0c2"), //  jet black ship body
            hull_accent:     hex("#333c57"), //  steel blue (reserved)
            turret:          hex("#566c86"), //  light grey turrets — pops on jet hull
            enemy:           hex("#b13e53"), //  oxide red
            enemy_accent:    hex("#571c27"), //  dark wine superstructure
            bullet_friendly: hex("#ffcd75"), //  vivid warm gold
            bullet_enemy:    hex("#ff5000"), //  vivid orange
            trail:           hex("#c7cfdd"), //  cool foam white
        }
    }

    /// Previous palette — kept around so swapping is one line.
    #[allow(dead_code)]
    fn iris() -> Self {
        Self {
            ocean:           hex("#7194f0"),
            border:          hex("#abc9f1"),
            hull:            hex("#280732"),
            hull_accent:     hex("#5b4f6e"),
            turret:          hex("#b2b1c0"),
            enemy:           hex("#e15e6e"),
            enemy_accent:    hex("#280732"),
            bullet_friendly: hex("#f3a8a8"),
            bullet_enemy:    hex("#e15e6e"),
            trail:           hex("#e6ecef"),
        }
    }
}

/// Material handles, one per palette role. Entities reference these so a
/// runtime palette change updates every existing entity in one place.
#[derive(Resource)]
struct PaletteMaterials {
    border: Handle<ColorMaterial>,
    hull: Handle<ColorMaterial>,
    hull_accent: Handle<ColorMaterial>,
    turret: Handle<ColorMaterial>,
    enemy: Handle<ColorMaterial>,
    enemy_accent: Handle<ColorMaterial>,
    bullet_friendly: Handle<ColorMaterial>,
    bullet_enemy: Handle<ColorMaterial>,
    /// Darker outer ring — surrounds the bright inner core.
    bullet_friendly_outer: Handle<ColorMaterial>,
    bullet_enemy_outer: Handle<ColorMaterial>,
    trail: Handle<ColorMaterial>,
    /// Always-white material used for hit-flashes; not driven by the palette.
    flash: Handle<ColorMaterial>,
}

/// Inner core is lighter than the base by this factor (0 = unchanged, 1 = white).
const BULLET_INNER_LIGHTEN: f32 = 0.55;

fn lighten(c: Color, amount: f32) -> Color {
    let s: bevy::color::Srgba = c.into();
    Color::srgb(
        s.red   + (1.0 - s.red)   * amount,
        s.green + (1.0 - s.green) * amount,
        s.blue  + (1.0 - s.blue)  * amount,
    )
}

/// Cached mesh handles for short-lived effects so we don't allocate per spawn.
#[derive(Resource)]
struct EffectMeshes {
    muzzle_flash: Handle<Mesh>,
    particle: Handle<Mesh>,
}

// ---------- Components ----------
#[derive(Component)]
struct Friendly;

#[derive(Component)]
struct Enemy {
    state: EnemyState,
    state_timer: f32,
    waypoint: Vec2,
    fire_cd: f32,
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum EnemyState { Wander, Approach, Attack, Reposition }

#[derive(Component)]
struct Health(i32);

#[derive(Component)]
struct Velocity(Vec2);

#[derive(Component)]
struct Heading(f32); // radians, 0 = +Y up

#[derive(Component)]
struct Faction(FactionKind);
#[derive(Clone, Copy, PartialEq, Eq)]
enum FactionKind { Friendly, Enemy }

#[derive(Component)]
struct TurretSlot {
    index: usize,
    barrel_angle: f32,  // current local rotation rel to hull (0 = forward = +Y)
    mount_angle: f32,   // arc center / rest direction in hull frame
    fire_cd: f32,
    damage: i32,
    fire_rate: f32,
    /// 1 = single barrel, 2 = twin (fires twice as fast, alternating barrels).
    barrels: u8,
    /// Which barrel fires next (0 or 1) when `barrels == 2`. Alternates so
    /// the visible left/right barrel matches the bullet that comes out.
    next_barrel: u8,
}

/// Marks a barrel mesh child of a turret base. Index 0 is port-side / single,
/// index 1 is starboard-side and only shown when the slot has twin barrels.
#[derive(Component)]
struct BarrelIndex(u8);

const BARREL_LATERAL: f32 = 0.9; // lateral offset in twin-barrel mode

#[derive(Component)]
struct TurretBarrel; // child barrel rectangle entity

#[derive(Component)]
struct Bullet {
    faction: FactionKind,
    damage: i32,
    remaining: f32,
}

#[derive(Component)]
struct Trail;

/// Per-enemy trail. Each enemy gets its own short ribbon mesh; this carries
/// the back-reference + sampled path. The trail entity is not parented to
/// the enemy (mesh positions live in world space) — when the enemy
/// despawns, `update_enemy_trails` cleans the orphan up.
#[derive(Component)]
struct EnemyTrail {
    enemy: Entity,
    points: VecDeque<Vec2>,
    sample_timer: f32,
}

const ENEMY_TRAIL_SAMPLE_HZ: f32 = 25.0;
const ENEMY_TRAIL_MAX_POINTS: usize = 18;   // ~0.7 s of history → readable but short
const ENEMY_TRAIL_HEAD_WIDTH: f32 = 4.0;    // wide enough to register on the water

/// Short-lived burst placed at a turret muzzle when it fires; fades + shrinks.
#[derive(Component)]
struct MuzzleFlash {
    life: f32,
    max_life: f32,
}

/// Hit particle that drifts and fades after an enemy takes damage / is destroyed.
#[derive(Component)]
struct HitParticle {
    life: f32,
    max_life: f32,
    /// Per-particle base scale so the fade keeps the random spawn size variation.
    base_scale: f32,
}

/// Per-entity hit feedback — damped spring drives a render-scale pulse, plus
/// a brief white-flash by swapping the entity's material handle.
/// `a = -k(x-1) - dv` snaps the spring back; `pulse()` adds an impulse.
#[derive(Component)]
struct HitFx {
    spring_x: f32,
    spring_v: f32,
    flash_remaining: f32,
    base_material: Handle<ColorMaterial>,
}

const HIT_PULSE: f32 = 0.5;
const HIT_K: f32 = 200.0;
const HIT_D: f32 = 10.0;
const FLASH_DURATION: f32 = 0.12;

impl HitFx {
    fn new(base_material: Handle<ColorMaterial>) -> Self {
        Self { spring_x: 1.0, spring_v: 0.0, flash_remaining: 0.0, base_material }
    }
    fn pulse(&mut self) {
        self.spring_x += HIT_PULSE;
        self.flash_remaining = FLASH_DURATION;
    }
}

/// Sampled history of the friendly ship's position; rebuilt into a ribbon mesh.
/// Index 0 = newest sample, index n-1 = oldest. Width tapers from front to back.
#[derive(Resource, Default)]
struct ShipPath {
    points: VecDeque<Vec2>,
    sample_timer: f32,
}

const TRAIL_SAMPLE_HZ: f32 = 30.0;
const TRAIL_MAX_POINTS: usize = 30;
const TRAIL_HEAD_WIDTH: f32 = 6.0;

#[derive(Component)]
struct ScoreText;

#[derive(Component)]
struct PlayCamera;

#[derive(Component)]
struct UpscaleSprite;

// ---------- Resources ----------
#[derive(Resource)]
struct Score(u32);

#[derive(Resource)]
struct SpawnTimer { t: f32, elapsed: f32 }

#[derive(Resource)]
struct PlayRenderImage(Handle<Image>);

#[derive(Resource, Default)]
struct TurretConfig {
    // [slot] -> (equipped, damage, fire_rate)
    slots: [SlotCfg; 8],
}
#[derive(Default, Clone, Copy)]
struct SlotCfg { equipped: bool, damage: i32, fire_rate: f32, barrels: u8 }

#[derive(Resource, Default)]
struct ConfigDirty(bool);

#[derive(Component)]
struct SlotButton { slot: usize, kind: ButtonKind }
#[derive(Clone, Copy, PartialEq, Eq)]
enum ButtonKind {
    Equip,
    DamageUp, DamageDown,
    RateUp, RateDown,
    BarrelsUp, BarrelsDown,
    ToggleDesktopMode,
}

#[derive(Component)]
struct SlotLabel { slot: usize, kind: LabelKind }
#[derive(Clone, Copy, PartialEq, Eq)]
enum LabelKind { Damage, Rate, Status, Barrels }

#[derive(Resource)]
struct TrailTimer(f32);

/// Toggled by the DESKTOP button. When `desktop` is true, the LHS UI panel
/// is hidden, the window shrinks to play-area-only, and is repositioned to
/// the bottom-right corner of the primary monitor.
#[derive(Resource, Default, Clone, Copy)]
struct WindowMode {
    desktop: bool,
    last_applied: Option<bool>,
}

#[derive(Component)]
struct UiPanel;

// ---------- App ----------
fn main() {
    let mut cfg = TurretConfig::default();
    cfg.slots[0] = SlotCfg { equipped: true, damage: 1, fire_rate: 4.0, barrels: 1 };
    for i in 1..8 { cfg.slots[i] = SlotCfg { equipped: false, damage: 1, fire_rate: 4.0, barrels: 1 }; }

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
        .insert_resource(TrailTimer(0.0))
        .insert_resource(Palette::aap64_naval())
        .insert_resource(ShipPath::default())
        .insert_resource(WindowMode::default())
        .add_systems(Startup, (setup_render, setup_world, setup_ui).chain())
        .add_systems(Update, (
            // Sim / movement
            apply_palette,
            friendly_movement,
            enemy_ai,
            apply_velocity,
            spawn_enemies,
            sync_turret_config,
            turret_aim_fire,
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
            update_hit_particles,
            update_score_text,
            ui_button_system,
            update_slot_labels,
            resize_upscale_sprite,
            apply_window_mode,
        ))
        .run();
}

/// Snap the upscale sprite to an integer multiple of the internal resolution.
/// Without this, fractional sampling (e.g. one internal pixel mapping to 3.5
/// screen pixels) shimmers as objects move — that's the "laggy" feel.
/// Toggle between full-window mode (UI panel + play area) and "desktop"
/// mode (play area only, window shrunk + repositioned to the bottom-right of
/// the primary monitor with no decorations). Runs only when WindowMode flips.
fn apply_window_mode(
    mut mode: ResMut<WindowMode>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
    monitors: Query<&bevy::window::Monitor>,
    mut panels: Query<&mut Visibility, (With<UiPanel>, Without<ScoreText>)>,
    mut score: Query<&mut Visibility, (With<ScoreText>, Without<UiPanel>)>,
) {
    if mode.last_applied == Some(mode.desktop) { return; }
    mode.last_applied = Some(mode.desktop);
    let Ok(mut window) = windows.single_mut() else { return; };

    if mode.desktop {
        // Hide UI panel + score banner.
        for mut v in &mut panels { *v = Visibility::Hidden; }
        for mut v in &mut score  { *v = Visibility::Hidden; }
        // Pick a square play size that's an integer multiple of the internal res.
        let target_logical: u32 = 480; // 200 internal × 2.4 → 200 internal × floor(480/200)=2 → 400 px play
        // Actually compute the largest integer multiple ≤ target_logical.
        let scale = (target_logical as f32 / PLAY_INTERNAL as f32).floor().max(1.0) as u32;
        let logical_size = (PLAY_INTERNAL * scale) as f32;
        window.resolution.set(logical_size, logical_size);
        window.decorations = false;
        window.window_level = bevy::window::WindowLevel::AlwaysOnTop;
        // Bottom-right of the primary monitor (in physical pixels).
        if let Some(monitor) = monitors.iter().next() {
            let phys_w = monitor.physical_size().x as i32;
            let phys_h = monitor.physical_size().y as i32;
            let win_phys_w = (logical_size * window.scale_factor()) as i32;
            let win_phys_h = (logical_size * window.scale_factor()) as i32;
            let pad = 12;
            window.position = bevy::window::WindowPosition::At(IVec2::new(
                phys_w - win_phys_w - pad,
                phys_h - win_phys_h - pad,
            ));
        }
    } else {
        for mut v in &mut panels { *v = Visibility::Inherited; }
        for mut v in &mut score  { *v = Visibility::Inherited; }
        window.resolution.set(WINDOW_W, WINDOW_H);
        window.decorations = true;
        window.window_level = bevy::window::WindowLevel::Normal;
        window.position = bevy::window::WindowPosition::Centered(
            bevy::window::MonitorSelection::Primary,
        );
    }
}

/// Authoritative layout: play area's screen-space rect for the current
/// window size. Both the upscale sprite placement and cursor→world mapping
/// use this so they can't drift out of sync as the window resizes.
/// `ui_width` is 0 in desktop mode (panel hidden) and `UI_WIDTH` otherwise.
fn play_area_screen_rect(logical_w: f32, logical_h: f32, ui_width: f32) -> (f32, f32, f32) {
    let avail_w = (logical_w - ui_width).max(0.0);
    let scale_x = (avail_w / PLAY_INTERNAL as f32).floor();
    let scale_y = (logical_h / PLAY_INTERNAL as f32).floor();
    let scale = scale_x.min(scale_y).max(1.0);
    let size = PLAY_INTERNAL as f32 * scale;
    let left = ui_width + (avail_w - size) / 2.0;
    let top = (logical_h - size) / 2.0;
    (left, top, size)
}

fn effective_ui_width(mode: &WindowMode) -> f32 {
    if mode.desktop { 0.0 } else { UI_WIDTH }
}

/// Snap the upscale sprite to an integer multiple of the internal resolution
/// AND reposition it within the window each frame. Without integer snapping
/// one internal pixel can map to 3.5 screen pixels and shimmer as things move.
fn resize_upscale_sprite(
    windows: Query<&Window, With<PrimaryWindow>>,
    mode: Res<WindowMode>,
    mut sprites: Query<(&mut Sprite, &mut Transform), With<UpscaleSprite>>,
) {
    let Ok(window) = windows.single() else { return; };
    let logical_w = window.width();
    let logical_h = window.height();
    let (left, _top, size) = play_area_screen_rect(logical_w, logical_h, effective_ui_width(&mode));
    // 2D camera puts world (0,0) at window center; sprite sits centered in
    // the available area to the right of the UI panel.
    let world_x = left + size / 2.0 - logical_w / 2.0;
    let target = Vec2::splat(size);
    for (mut s, mut tf) in &mut sprites {
        if s.custom_size != Some(target) { s.custom_size = Some(target); }
        if (tf.translation.x - world_x).abs() > 0.001 { tf.translation.x = world_x; }
        if tf.translation.y != 0.0 { tf.translation.y = 0.0; }
    }
}

/// Push the current `Palette` into shared materials + camera clear color
/// whenever the resource is changed (and once on first frame).
fn apply_palette(
    palette: Res<Palette>,
    pm: Option<Res<PaletteMaterials>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut cameras: Query<&mut Camera, With<PlayCamera>>,
) {
    if !palette.is_changed() { return; }
    let Some(pm) = pm else { return; };
    let pairs: [(&Handle<ColorMaterial>, Color); 11] = [
        (&pm.border,                palette.border),
        (&pm.hull,                  palette.hull),
        (&pm.hull_accent,           palette.hull_accent),
        (&pm.turret,                palette.turret),
        (&pm.enemy,                 palette.enemy),
        (&pm.enemy_accent,          palette.enemy_accent),
        (&pm.bullet_friendly,       lighten(palette.bullet_friendly, BULLET_INNER_LIGHTEN)),
        (&pm.bullet_enemy,          lighten(palette.bullet_enemy, BULLET_INNER_LIGHTEN)),
        (&pm.bullet_friendly_outer, palette.bullet_friendly),
        (&pm.bullet_enemy_outer,    palette.bullet_enemy),
        (&pm.trail,                 palette.trail),
    ];
    for (h, c) in pairs {
        if let Some(m) = materials.get_mut(h) { m.color = c; }
    }
    for mut cam in &mut cameras {
        cam.clear_color = ClearColorConfig::Custom(palette.ocean);
    }
}

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
    commands.spawn((
        Camera2d,
        Camera { order: 0, ..default() },
        RenderLayers::from_layers(&[0, UPSCALE_LAYER]),
        Msaa::Off,
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
    // Bullets are two-tone: base color = outer ring, lighter version = bright
    // inner core. Muzzle flashes / hit sparks reuse the bright inner color.
    let pm = PaletteMaterials {
        border:                materials.add(palette.border),
        hull:                  materials.add(palette.hull),
        hull_accent:           materials.add(palette.hull_accent),
        turret:                materials.add(palette.turret),
        enemy:                 materials.add(palette.enemy),
        enemy_accent:          materials.add(palette.enemy_accent),
        bullet_friendly:       materials.add(lighten(palette.bullet_friendly, BULLET_INNER_LIGHTEN)),
        bullet_enemy:          materials.add(lighten(palette.bullet_enemy, BULLET_INNER_LIGHTEN)),
        bullet_friendly_outer: materials.add(palette.bullet_friendly),
        bullet_enemy_outer:    materials.add(palette.bullet_enemy),
        trail:                 materials.add(palette.trail),
        flash:                 materials.add(Color::WHITE),
    };

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
        Friendly,
        Faction(FactionKind::Friendly),
        Health(100),
        Velocity(Vec2::new(0.0, FRIENDLY_SPEED)),
        Heading(0.0),
        HitFx::new(pm.hull.clone()),
        RenderLayers::layer(PLAY_LAYER),
    )).id();

    // Friendly turrets. Barrel kept ≥1.5 wide so it doesn't alias to zero
    // pixels at off-axis rotations now that MSAA is off — sub-pixel rects
    // were vanishing entirely between integer-grid angles.
    let base_mesh = meshes.add(Circle::new(2.0));
    let barrel_mesh = meshes.add(Rectangle::new(1.5, 4.0));

    for (i, (lx, ly)) in TURRET_POSITIONS.iter().enumerate() {
        let slot = cfg.slots[i];
        let visible = slot.equipped;
        let mount = TURRET_MOUNTS[i];
        let mut ec = commands.spawn((
            Mesh2d(base_mesh.clone()),
            MeshMaterial2d(pm.turret.clone()),
            Transform::from_xyz(*lx, *ly, 2.0).with_rotation(Quat::from_rotation_z(mount)),
            if visible { Visibility::Inherited } else { Visibility::Hidden },
            TurretSlot {
                index: i,
                barrel_angle: mount,
                mount_angle: mount,
                fire_cd: 0.0,
                damage: slot.damage,
                fire_rate: slot.fire_rate,
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
                MeshMaterial2d(pm.turret.clone()),
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
    commands.insert_resource(EffectMeshes {
        muzzle_flash: meshes.add(Capsule2d::new(1.6, 4.0)),
        particle:     meshes.add(Capsule2d::new(0.7, 1.6)),
    });
}

// ---------- UI ----------
// Theme palette for the LHS panel — kept separate from the gameplay Palette
// so the panel stays legible regardless of the in-game color choices.
const UI_BG:        Color = Color::srgb(0.07, 0.08, 0.11);
const UI_ROW_BG:    Color = Color::srgb(0.12, 0.13, 0.17);
const UI_ROW_DIV:   Color = Color::srgb(0.22, 0.24, 0.30);
const UI_TEXT:      Color = Color::srgb(0.92, 0.93, 0.96);
const UI_TEXT_DIM:  Color = Color::srgb(0.55, 0.60, 0.70);
const UI_VALUE:     Color = Color::srgb(1.00, 0.85, 0.30);
const UI_BTN_BG:    Color = Color::srgb(0.22, 0.24, 0.30);
const UI_EQUIP_BG:  Color = Color::srgb(0.18, 0.40, 0.26);
const UI_ACTIVE_BG: Color = Color::srgb(0.20, 0.28, 0.40);
const UI_HULL:      Color = Color::srgb(0.30, 0.34, 0.42);
const UI_DOT_OFF:   Color = Color::srgb(0.32, 0.35, 0.42);
const UI_DOT_ON:    Color = Color::srgb(1.00, 0.85, 0.30);

fn setup_ui(mut commands: Commands) {
    // Score banner over the play area.
    commands.spawn((
        Text::new("SCORE 0"),
        TextFont { font_size: 36.0, ..default() },
        TextColor(UI_VALUE),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(8.0),
            left: Val::Px(UI_WIDTH),
            right: Val::Px(0.0),
            justify_content: JustifyContent::Center,
            ..default()
        },
        ScoreText,
    ));

    // Left control panel.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            top: Val::Px(0.0),
            width: Val::Px(UI_WIDTH),
            height: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            padding: UiRect::all(Val::Px(8.0)),
            row_gap: Val::Px(4.0),
            ..default()
        },
        BackgroundColor(UI_BG),
        UiPanel,
    ))
    .with_children(|p| {
        // --- Header ---
        p.spawn(Node {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::SpaceBetween,
            margin: UiRect { bottom: Val::Px(4.0), ..default() },
            ..default()
        })
        .with_children(|h| {
            h.spawn(Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(2.0),
                ..default()
            })
            .with_children(|t| {
                t.spawn((
                    Text::new("BATTLESHIP CONTROL"),
                    TextFont { font_size: 16.0, ..default() },
                    TextColor(UI_TEXT),
                ));
                t.spawn((
                    Text::new("Cuniberti refit · 8 turret slots"),
                    TextFont { font_size: 10.0, ..default() },
                    TextColor(UI_TEXT_DIM),
                ));
            });
            // Desktop-mode toggle. `slot: 0` is unused by this button kind.
            h.spawn((
                Button,
                Node {
                    padding: UiRect::axes(Val::Px(8.0), Val::Px(4.0)),
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::Center,
                    ..default()
                },
                BackgroundColor(UI_BTN_BG),
                BorderRadius::all(Val::Px(2.0)),
                SlotButton { slot: 0, kind: ButtonKind::ToggleDesktopMode },
            ))
            .with_children(|b| {
                b.spawn((
                    Text::new("DESKTOP"),
                    TextFont { font_size: 10.0, ..default() },
                    TextColor(UI_TEXT),
                ));
            });
        });
        // Divider
        p.spawn((
            Node {
                width: Val::Percent(100.0),
                height: Val::Px(1.0),
                margin: UiRect { bottom: Val::Px(4.0), ..default() },
                ..default()
            },
            BackgroundColor(UI_ROW_DIV),
        ));

        // --- 8 turret slot rows ---
        for slot in 0..8 {
            spawn_slot_row(p, slot);
        }
    });
}

fn spawn_slot_row(parent: &mut ChildSpawnerCommands, slot: usize) {
    parent.spawn((
        Node {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Stretch,
            column_gap: Val::Px(8.0),
            padding: UiRect::all(Val::Px(6.0)),
            ..default()
        },
        BackgroundColor(UI_ROW_BG),
        BorderRadius::all(Val::Px(3.0)),
    ))
    .with_children(|row| {
        spawn_ship_schematic(row, slot);
        spawn_slot_controls(row, slot);
    });
}

/// Mini top-down ship silhouette with all 8 turret dots; the slot's own
/// turret dot is highlighted in `UI_DOT_ON`, the rest are dimmed. Mirrors
/// the layout of `TURRET_POSITIONS` so the panel reads as a real schematic.
fn spawn_ship_schematic(parent: &mut ChildSpawnerCommands, slot: usize) {
    const SIZE: f32 = 56.0;
    const HULL_W: f32 = 12.0;
    const HULL_H: f32 = 38.0;
    // Scale from world hull units to schematic pixels.
    let sx = HULL_W / HULL_WIDTH;       // 12 / 8 = 1.5
    let sy = HULL_H / HULL_LEN;         // 38 / 22 ≈ 1.73
    let center = SIZE / 2.0;

    parent.spawn((
        Node {
            width: Val::Px(SIZE),
            height: Val::Px(SIZE),
            position_type: PositionType::Relative,
            flex_shrink: 0.0,
            ..default()
        },
        BackgroundColor(UI_BG),
        BorderRadius::all(Val::Px(2.0)),
    ))
    .with_children(|s| {
        // Hull silhouette (rounded rectangle = capsule).
        s.spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(center - HULL_W / 2.0),
                top:  Val::Px(center - HULL_H / 2.0),
                width: Val::Px(HULL_W),
                height: Val::Px(HULL_H),
                ..default()
            },
            BackgroundColor(UI_HULL),
            BorderRadius::all(Val::Px(HULL_W / 2.0)),
        ));

        // 8 turret dots (4×4 px, rounded). Active slot gets the bright color.
        for i in 0..8 {
            let (lx, ly) = TURRET_POSITIONS[i];
            // World +y = bow (up); UI +y = window-down. Flip y.
            let dot_x = center + lx * sx;
            let dot_y = center - ly * sy;
            let dot = 4.0;
            let color = if i == slot { UI_DOT_ON } else { UI_DOT_OFF };
            s.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(dot_x - dot / 2.0),
                    top:  Val::Px(dot_y - dot / 2.0),
                    width: Val::Px(dot),
                    height: Val::Px(dot),
                    ..default()
                },
                BackgroundColor(color),
                BorderRadius::all(Val::Px(dot / 2.0)),
            ));
        }
    });
}

fn spawn_slot_controls(parent: &mut ChildSpawnerCommands, slot: usize) {
    parent.spawn(Node {
        flex_direction: FlexDirection::Column,
        flex_grow: 1.0,
        row_gap: Val::Px(3.0),
        ..default()
    })
    .with_children(|c| {
        // Slot title row: "01  BOW"
        c.spawn(Node {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: Val::Px(6.0),
            ..default()
        })
        .with_children(|t| {
            t.spawn((
                Text::new(format!("{:02}", slot + 1)),
                TextFont { font_size: 11.0, ..default() },
                TextColor(UI_TEXT_DIM),
            ));
            t.spawn((
                Text::new(TURRET_NAMES[slot]),
                TextFont { font_size: 12.0, ..default() },
                TextColor(UI_TEXT),
            ));
        });

        // Equip / Active button (always shown; the button text is updated by
        // update_slot_labels and the click handler is a no-op when equipped).
        c.spawn((
            Button,
            Node {
                width: Val::Percent(100.0),
                padding: UiRect::axes(Val::Px(4.0), Val::Px(3.0)),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(if slot == 0 { UI_ACTIVE_BG } else { UI_EQUIP_BG }),
            BorderRadius::all(Val::Px(2.0)),
            SlotButton { slot, kind: ButtonKind::Equip },
        ))
        .with_children(|b| {
            b.spawn((
                Text::new(if slot == 0 { "ACTIVE" } else { "EQUIP GUN" }),
                TextFont { font_size: 11.0, ..default() },
                TextColor(UI_TEXT),
                SlotLabel { slot, kind: LabelKind::Status },
            ));
        });

        // DMG / RATE / BRRL stat rows.
        spawn_stat_row(c, slot, "DMG",  "1",   LabelKind::Damage,
                       ButtonKind::DamageDown, ButtonKind::DamageUp);
        spawn_stat_row(c, slot, "RATE", "4.0", LabelKind::Rate,
                       ButtonKind::RateDown,   ButtonKind::RateUp);
        spawn_stat_row(c, slot, "BRRL", "1",   LabelKind::Barrels,
                       ButtonKind::BarrelsDown, ButtonKind::BarrelsUp);
    });
}

fn spawn_stat_row(
    parent: &mut ChildSpawnerCommands,
    slot: usize,
    label: &str,
    initial: &str,
    label_kind: LabelKind,
    down_kind: ButtonKind,
    up_kind: ButtonKind,
) {
    parent.spawn(Node {
        flex_direction: FlexDirection::Row,
        align_items: AlignItems::Center,
        column_gap: Val::Px(4.0),
        ..default()
    })
    .with_children(|r| {
        r.spawn((
            Text::new(label.to_string()),
            TextFont { font_size: 10.0, ..default() },
            TextColor(UI_TEXT_DIM),
            Node { width: Val::Px(32.0), ..default() },
        ));
        r.spawn((
            Text::new(initial.to_string()),
            TextFont { font_size: 12.0, ..default() },
            TextColor(UI_VALUE),
            SlotLabel { slot, kind: label_kind },
            Node { width: Val::Px(28.0), ..default() },
        ));
        spawn_step_button(r, slot, down_kind, "−");
        spawn_step_button(r, slot, up_kind,   "+");
    });
}

fn spawn_step_button(parent: &mut ChildSpawnerCommands, slot: usize, kind: ButtonKind, label: &str) {
    parent.spawn((
        Button,
        Node {
            width: Val::Px(20.0),
            height: Val::Px(18.0),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            ..default()
        },
        BackgroundColor(UI_BTN_BG),
        BorderRadius::all(Val::Px(2.0)),
        SlotButton { slot, kind },
    ))
    .with_children(|b| {
        b.spawn((
            Text::new(label.to_string()),
            TextFont { font_size: 14.0, ..default() },
            TextColor(UI_TEXT),
        ));
    });
}

// ---------- Systems ----------
fn friendly_movement(
    time: Res<Time>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mode: Res<WindowMode>,
    enemies: Query<&Transform, (With<Enemy>, Without<Friendly>)>,
    mut q: Query<(&mut Transform, &mut Velocity, &mut Heading), With<Friendly>>,
) {
    let dt = time.delta_secs();
    let Ok(win) = windows.single() else { return; };
    let cursor = win.cursor_position();

    let (play_left, play_top, play_screen) =
        play_area_screen_rect(win.width(), win.height(), effective_ui_width(&mode));

    let target_world: Option<Vec2> = cursor.and_then(|c| {
        if c.x >= play_left && c.x <= play_left + play_screen
            && c.y >= play_top && c.y <= play_top + play_screen {
            let nx = (c.x - play_left) / play_screen;
            let ny = (c.y - play_top) / play_screen;
            Some(Vec2::new(
                (nx - 0.5) * PLAY_WORLD,
                (0.5 - ny) * PLAY_WORLD,
            ))
        } else { None }
    });

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

fn approach_angle(cur: f32, tgt: f32, max: f32) -> f32 {
    let mut d = (tgt - cur + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
    if d > max { d = max; }
    if d < -max { d = -max; }
    cur + d
}

fn enemy_ai(
    time: Res<Time>,
    friendly: Query<&Transform, (With<Friendly>, Without<Enemy>)>,
    mut q: Query<(&mut Transform, &mut Velocity, &mut Heading, &mut Enemy)>,
) {
    let dt = time.delta_secs();
    let Ok(ftf) = friendly.single() else { return; };
    let fpos = ftf.translation.truncate();
    let mut rng = rand::thread_rng();

    for (mut tf, mut vel, mut heading, mut enemy) in &mut q {
        let pos = tf.translation.truncate();
        enemy.state_timer -= dt;
        enemy.fire_cd -= dt;
        let dist = pos.distance(fpos);

        if enemy.state_timer <= 0.0 {
            enemy.state = if dist > 75.0 {
                EnemyState::Approach
            } else if dist > 35.0 {
                if rng.gen_bool(0.6) { EnemyState::Attack } else { EnemyState::Reposition }
            } else {
                EnemyState::Reposition
            };
            enemy.state_timer = rng.gen_range(1.5..3.5);
            let off = Vec2::new(rng.gen_range(-30.0..30.0), rng.gen_range(-30.0..30.0));
            enemy.waypoint = fpos + off;
        }

        let target = match enemy.state {
            EnemyState::Wander | EnemyState::Reposition => enemy.waypoint,
            EnemyState::Approach | EnemyState::Attack => fpos,
        };
        let to = target - pos;
        if to.length_squared() > 1.0 {
            let desired = (-to.x).atan2(to.y);
            heading.0 = approach_angle(heading.0, desired, ENEMY_TURN_RATE * dt);
        }
        let dir = Vec2::new(-heading.0.sin(), heading.0.cos());
        vel.0 = dir * ENEMY_SPEED;
        tf.rotation = Quat::from_rotation_z(heading.0);
    }
}

fn apply_velocity(time: Res<Time>, mut q: Query<(&mut Transform, &Velocity)>) {
    let dt = time.delta_secs();
    for (mut tf, v) in &mut q {
        tf.translation.x += v.0.x * dt;
        tf.translation.y += v.0.y * dt;
    }
}

fn spawn_hit_particles(
    commands: &mut Commands,
    em: &EffectMeshes,
    mat: &Handle<ColorMaterial>,
    pos: Vec2,
    count: u32,
    speed: f32,
    rng: &mut rand::rngs::ThreadRng,
) {
    use std::f32::consts::TAU;
    for _ in 0..count {
        let a = rng.gen_range(0.0..TAU);
        let s = rng.gen_range(speed * 0.4..speed);
        let v = Vec2::new(a.cos(), a.sin()) * s;
        let life = rng.gen_range(0.3..0.6);
        // Particle is a streak capsule oriented along its velocity. Our
        // particle mesh has its long axis on +Y, so convert (cos,sin) → angle
        // in our 0=+Y / +PI/2=-X frame: rot = (-vx).atan2(vy).
        let rot = (-v.x).atan2(v.y);
        // Random initial scale so the burst looks chunky rather than uniform.
        let scale = rng.gen_range(0.8..1.4);
        commands.spawn((
            Mesh2d(em.particle.clone()),
            MeshMaterial2d(mat.clone()),
            Transform {
                translation: Vec3::new(pos.x, pos.y, 5.5),
                rotation: Quat::from_rotation_z(rot),
                scale: Vec3::new(scale, scale, 1.0),
            },
            HitParticle { life, max_life: life, base_scale: scale },
            Velocity(v),
            RenderLayers::layer(PLAY_LAYER),
        ));
    }
}

fn update_muzzle_flashes(
    time: Res<Time>,
    mut commands: Commands,
    mut q: Query<(Entity, &mut Transform, &mut MuzzleFlash)>,
) {
    let dt = time.delta_secs();
    for (e, mut tf, mut f) in &mut q {
        f.life -= dt;
        if f.life <= 0.0 {
            commands.entity(e).despawn();
            continue;
        }
        let t = (f.life / f.max_life).clamp(0.0, 1.0);
        // Pop in then ease out: scale peaks at spawn, shrinks to 0.4 by end.
        let s = 0.4 + 0.7 * t;
        tf.scale.x = s;
        tf.scale.y = s;
        tf.scale.z = 1.0;
    }
}

fn tick_hit_fx(time: Res<Time>, mut q: Query<(&mut HitFx, &mut Transform)>) {
    let dt = time.delta_secs();
    for (mut fx, mut tf) in &mut q {
        // Damped-spring snap-back to rest position 1.0.
        let a = -HIT_K * (fx.spring_x - 1.0) - HIT_D * fx.spring_v;
        fx.spring_v += a * dt;
        fx.spring_x += fx.spring_v * dt;
        if fx.flash_remaining > 0.0 {
            fx.flash_remaining = (fx.flash_remaining - dt).max(0.0);
        }
        // Apply scale uniformly. Other systems write rotation/translation only.
        let s = fx.spring_x.max(0.0);
        tf.scale.x = s;
        tf.scale.y = s;
        tf.scale.z = 1.0;
    }
}

fn apply_hit_fx_visuals(
    pm: Option<Res<PaletteMaterials>>,
    mut q: Query<(&HitFx, &mut MeshMaterial2d<ColorMaterial>)>,
) {
    let Some(pm) = pm else { return; };
    for (fx, mut mat) in &mut q {
        let want = if fx.flash_remaining > 0.0 { &pm.flash } else { &fx.base_material };
        if mat.0 != *want {
            mat.0 = want.clone();
        }
    }
}

fn update_hit_particles(
    time: Res<Time>,
    mut commands: Commands,
    mut q: Query<(Entity, &mut Transform, &mut HitParticle, &mut Velocity)>,
) {
    let dt = time.delta_secs();
    let drag = 0.88_f32.powf(60.0 * dt); // ~12% velocity loss per frame at 60Hz
    for (e, mut tf, mut p, mut v) in &mut q {
        p.life -= dt;
        if p.life <= 0.0 {
            commands.entity(e).despawn();
            continue;
        }
        v.0 *= drag;
        let t = (p.life / p.max_life).clamp(0.0, 1.0);
        // Shrink toward 30% of the per-particle base scale; preserve rotation.
        let s = p.base_scale * (0.3 + 0.7 * t);
        tf.scale.x = s;
        tf.scale.y = s;
        tf.scale.z = 1.0;
    }
}

fn empty_dynamic_mesh() -> Mesh {
    let mut m = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    m.insert_attribute(Mesh::ATTRIBUTE_POSITION, Vec::<[f32; 3]>::new());
    m.insert_attribute(Mesh::ATTRIBUTE_NORMAL, Vec::<[f32; 3]>::new());
    m.insert_attribute(Mesh::ATTRIBUTE_UV_0, Vec::<[f32; 2]>::new());
    m.insert_indices(Indices::U32(Vec::new()));
    m
}

/// Sample the friendly ship's position into ShipPath, then rebuild the trail
/// ribbon mesh. The ribbon is widest at the ship and tapers to a point at the
/// oldest sample, so it visually traces the path the ship just took.
fn update_trail(
    time: Res<Time>,
    mut path: ResMut<ShipPath>,
    ship_q: Query<&Transform, (With<Friendly>, Without<Trail>)>,
    trail_q: Query<&Mesh2d, With<Trail>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let Ok(ship_tf) = ship_q.single() else { return; };
    // Anchor 4 px inside the stern so the ribbon attaches to the hull
    // rather than floating a gap behind it.
    let stern_offset = ship_tf.rotation * Vec3::new(0.0, -(HULL_HALF_LEN - 4.0), 0.0);
    let head = (ship_tf.translation + stern_offset).truncate();

    // Sample at a fixed rate, regardless of frame rate.
    path.sample_timer -= time.delta_secs();
    if path.sample_timer <= 0.0 {
        path.sample_timer = 1.0 / TRAIL_SAMPLE_HZ;
        path.points.push_front(head);
        while path.points.len() > TRAIL_MAX_POINTS {
            path.points.pop_back();
        }
    } else if let Some(front) = path.points.front_mut() {
        // Keep the front sample glued to the stern between sample ticks so the
        // ribbon's head doesn't lag visibly behind the moving ship.
        *front = head;
    }

    let Ok(Mesh2d(handle)) = trail_q.single() else { return; };
    let Some(mesh) = meshes.get_mut(handle) else { return; };
    rebuild_ribbon_mesh(mesh, &path.points, TRAIL_HEAD_WIDTH);
}

/// Rewrite `mesh` in place as a tapering ribbon through `points`. Index 0 is
/// the head (full width), the last index is the tail (zero width).
fn rebuild_ribbon_mesh(mesh: &mut Mesh, points: &VecDeque<Vec2>, head_width: f32) {
    let n = points.len();
    if n < 2 {
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, Vec::<[f32; 3]>::new());
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, Vec::<[f32; 3]>::new());
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, Vec::<[f32; 2]>::new());
        mesh.insert_indices(Indices::U32(Vec::new()));
        return;
    }

    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(n * 2);
    let mut normals: Vec<[f32; 3]> = Vec::with_capacity(n * 2);
    let mut uvs: Vec<[f32; 2]> = Vec::with_capacity(n * 2);
    let mut indices: Vec<u32> = Vec::with_capacity((n - 1) * 6);

    for i in 0..n {
        let t = 1.0 - (i as f32 / (n - 1) as f32);
        let half_w = head_width * 0.5 * t;
        let prev = if i + 1 < n { points[i + 1] } else { points[i] };
        let next = if i > 0      { points[i - 1] } else { points[i] };
        let mut tangent = next - prev;
        if tangent.length_squared() < 1e-6 { tangent = Vec2::Y; }
        let tangent = tangent.normalize();
        let normal = Vec2::new(-tangent.y, tangent.x);
        let p = points[i];
        let left  = p + normal * half_w;
        let right = p - normal * half_w;
        positions.push([left.x,  left.y,  0.0]);
        positions.push([right.x, right.y, 0.0]);
        normals.push([0.0, 0.0, 1.0]);
        normals.push([0.0, 0.0, 1.0]);
        uvs.push([0.0, t]);
        uvs.push([1.0, t]);
    }
    for i in 0..n - 1 {
        let a = (i * 2) as u32;
        indices.extend_from_slice(&[a, a + 1, a + 2, a + 1, a + 3, a + 2]);
    }
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
}

/// Per-enemy version of `update_trail` — samples each enemy's stern position
/// into its own short ribbon. Despawns trail entities whose enemy is gone.
fn update_enemy_trails(
    time: Res<Time>,
    mut commands: Commands,
    enemy_q: Query<&Transform, (With<Enemy>, Without<EnemyTrail>)>,
    mut trail_q: Query<(Entity, &mut EnemyTrail, &Mesh2d)>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let dt = time.delta_secs();
    for (trail_e, mut trail, mesh2d) in &mut trail_q {
        let Ok(enemy_tf) = enemy_q.get(trail.enemy) else {
            commands.entity(trail_e).despawn();
            continue;
        };
        // Anchor at the enemy's stern (~5 units back from center).
        let stern = enemy_tf.rotation * Vec3::new(0.0, -(ENEMY_LEN / 2.0 - 1.0), 0.0);
        let head = (enemy_tf.translation + stern).truncate();

        trail.sample_timer -= dt;
        if trail.sample_timer <= 0.0 {
            trail.sample_timer = 1.0 / ENEMY_TRAIL_SAMPLE_HZ;
            trail.points.push_front(head);
            while trail.points.len() > ENEMY_TRAIL_MAX_POINTS {
                trail.points.pop_back();
            }
        } else if let Some(front) = trail.points.front_mut() {
            *front = head;
        }

        if let Some(mesh) = meshes.get_mut(&mesh2d.0) {
            rebuild_ribbon_mesh(mesh, &trail.points, ENEMY_TRAIL_HEAD_WIDTH);
        }
    }
}

fn spawn_enemies(
    time: Res<Time>,
    mut timer: ResMut<SpawnTimer>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    pm: Option<Res<PaletteMaterials>>,
    enemies: Query<Entity, With<Enemy>>,
) {
    let Some(pm) = pm else { return; };
    timer.elapsed += time.delta_secs();
    timer.t -= time.delta_secs();
    if timer.t > 0.0 { return; }

    let interval = (3.0 - timer.elapsed * 0.025).max(0.5);
    timer.t = interval;

    if enemies.iter().count() >= 30 { return; }

    let mut rng = rand::thread_rng();
    let half = PLAY_WORLD / 2.0;
    let edge = rng.gen_range(0..4);
    let pos = match edge {
        0 => Vec2::new(rng.gen_range(-half..half), half + 20.0),
        1 => Vec2::new(rng.gen_range(-half..half), -half - 20.0),
        2 => Vec2::new(half + 20.0, rng.gen_range(-half..half)),
        _ => Vec2::new(-half - 20.0, rng.gen_range(-half..half)),
    };

    // Red enemy hull (rounded capsule) + small dark turret (circle base + barrel).
    let body = meshes.add(Capsule2d::new(ENEMY_WIDTH / 2.0, ENEMY_LEN - ENEMY_WIDTH));
    let turret_base_mesh = meshes.add(Circle::new(1.0));     // smaller than friendly's 1.5
    let turret_barrel_mesh = meshes.add(Rectangle::new(0.9, 3.5));

    let inward = (-pos).normalize();
    let heading = (-inward.x).atan2(inward.y);

    let id = commands.spawn((
        Mesh2d(body),
        MeshMaterial2d(pm.enemy.clone()),
        Transform::from_xyz(pos.x, pos.y, 1.0).with_rotation(Quat::from_rotation_z(heading)),
        Enemy {
            state: EnemyState::Approach,
            state_timer: 1.0,
            waypoint: Vec2::ZERO,
            fire_cd: 0.5,
        },
        Health(ENEMY_HP),
        Velocity(inward * ENEMY_SPEED),
        Heading(heading),
        Faction(FactionKind::Enemy),
        HitFx::new(pm.enemy.clone()),
        RenderLayers::layer(PLAY_LAYER),
    )).id();

    let base = commands.spawn((
        Mesh2d(turret_base_mesh),
        MeshMaterial2d(pm.enemy_accent.clone()),
        Transform::from_xyz(0.0, 0.0, 0.1),
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    commands.entity(base).insert(ChildOf(id));

    let barrel = commands.spawn((
        Mesh2d(turret_barrel_mesh),
        MeshMaterial2d(pm.enemy_accent.clone()),
        Transform::from_xyz(0.0, 1.8, 0.15),
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    commands.entity(barrel).insert(ChildOf(id));

    // Short white wake trail behind the enemy.
    let trail_mesh = meshes.add(empty_dynamic_mesh());
    commands.spawn((
        Mesh2d(trail_mesh),
        MeshMaterial2d(pm.trail.clone()),
        Transform::from_xyz(0.0, 0.0, 0.4),
        EnemyTrail { enemy: id, points: VecDeque::new(), sample_timer: 0.0 },
        RenderLayers::layer(PLAY_LAYER),
    ));
}

fn sync_turret_config(
    cfg: Res<TurretConfig>,
    mut q: Query<(&mut TurretSlot, &mut Visibility, &Children)>,
    mut barrels: Query<
        (&BarrelIndex, &mut Visibility, &mut Transform),
        (With<TurretBarrel>, Without<TurretSlot>),
    >,
) {
    if !cfg.is_changed() { return; }
    for (mut slot, mut vis, children) in &mut q {
        let s = cfg.slots[slot.index];
        slot.damage = s.damage;
        slot.fire_rate = s.fire_rate;
        let new_barrels = s.barrels.max(1);
        if new_barrels != slot.barrels { slot.next_barrel = 0; }
        slot.barrels = new_barrels;
        *vis = if s.equipped { Visibility::Inherited } else { Visibility::Hidden };
        for c in children.iter() {
            if let Ok((idx, mut bv, mut btf)) = barrels.get_mut(c) {
                let visible = s.equipped && (idx.0 == 0 || s.barrels >= 2);
                *bv = if visible { Visibility::Inherited } else { Visibility::Hidden };
                let lateral = if s.barrels >= 2 {
                    if idx.0 == 0 { -BARREL_LATERAL } else { BARREL_LATERAL }
                } else { 0.0 };
                btf.translation.x = lateral;
                btf.translation.y = 3.0;
            }
        }
    }
}

fn turret_aim_fire(
    time: Res<Time>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    cfg: Res<TurretConfig>,
    ship_q: Query<(&Transform, &Heading), With<Friendly>>,
    enemies: Query<(&Transform, &Faction), With<Enemy>>,
    mut turrets: Query<(&mut TurretSlot, &mut Transform, &Children, &Visibility), (Without<Friendly>, Without<Enemy>, Without<TurretBarrel>)>,
    mut barrels: Query<&mut Transform, (With<TurretBarrel>, Without<TurretSlot>, Without<Friendly>, Without<Enemy>)>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    let Ok((ship_tf, ship_heading)) = ship_q.single() else { return; };
    let ship_pos = ship_tf.translation.truncate();
    let ship_h = ship_heading.0;

    for (mut slot, mut tf, children, vis) in &mut turrets {
        if matches!(*vis, Visibility::Hidden) { continue; }
        if !cfg.slots[slot.index].equipped { continue; }
        slot.fire_cd -= dt;

        // World position of turret
        let local = tf.translation.truncate();
        let cos_h = ship_h.cos();
        let sin_h = ship_h.sin();
        let world_off = Vec2::new(local.x * cos_h - local.y * sin_h, local.x * sin_h + local.y * cos_h);
        let turret_world = ship_pos + world_off;

        // Forward direction relative to hull (default barrel up = +y in hull frame).
        let hull_forward_world = ship_h; // angle of hull forward (heading uses 0=+Y)
        // We use angles where 0 = +Y (up).

        // Find best target in this turret's arc (centered on its mount angle).
        let mut best: Option<(f32, Vec2)> = None;
        for (etf, fac) in &enemies {
            if fac.0 != FactionKind::Enemy { continue; }
            let ep = etf.translation.truncate();
            let to = ep - turret_world;
            let d = to.length();
            if d > TURRET_RANGE { continue; }
            let world_angle = (-to.x).atan2(to.y);
            let mut local_angle = world_angle - hull_forward_world;
            local_angle = (local_angle + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
            // Offset relative to mount centerline.
            let mut off = local_angle - slot.mount_angle;
            off = (off + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
            if off.abs() > TURRET_ARC_HALVES[slot.index] { continue; }
            if best.map_or(true, |(bd, _)| d < bd) {
                best = Some((d, ep));
            }
        }

        let desired_local = if let Some((_, ep)) = best {
            let to = ep - turret_world;
            let world_angle = (-to.x).atan2(to.y);
            let mut la = world_angle - hull_forward_world;
            la = (la + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
            la
        } else {
            // Rest at mount angle when no target.
            slot.mount_angle
        };

        slot.barrel_angle = approach_angle(slot.barrel_angle, desired_local, TURRET_PIVOT * dt);

        // Apply rotation to base (so its child barrel rotates with it)
        tf.rotation = Quat::from_rotation_z(slot.barrel_angle);

        // Fire if aimed and target exists
        if let Some((_d, ep)) = best {
            let aim_err = (slot.barrel_angle - desired_local).abs();
            if aim_err < 0.1 && slot.fire_cd <= 0.0 {
                let barrels = slot.barrels.max(1) as f32;
                // Twin barrels = twice the effective rate (alternating barrels).
                slot.fire_cd = 1.0 / (slot.fire_rate.max(0.1) * barrels);
                // Spawn bullet from turret world pos toward ep
                let dir = (ep - turret_world).normalize_or_zero();
                if dir.length_squared() > 0.0 {
                    // Lateral offset to the active barrel: 0 in single mode,
                    // ±BARREL_LATERAL in twin mode, alternating per shot.
                    let lateral = if slot.barrels >= 2 {
                        if slot.next_barrel == 0 { -BARREL_LATERAL } else { BARREL_LATERAL }
                    } else { 0.0 };
                    let right = Vec2::new(dir.y, -dir.x);
                    // Two-tone bullet: darker outer + bright inner core.
                    // Spawn at the active barrel's tip so the muzzle flash
                    // and bullet line up with the barrel that fired.
                    let muzzle_pos = turret_world + dir * 5.0 + right * lateral;
                    slot.next_barrel = (slot.next_barrel + 1) % slot.barrels.max(1);
                    let bullet = commands.spawn((
                        Mesh2d(meshes.add(Capsule2d::new(2.0, 1.5))),
                        MeshMaterial2d(pm.bullet_friendly_outer.clone()),
                        Transform::from_xyz(muzzle_pos.x, muzzle_pos.y, 4.0)
                            .with_rotation(Quat::from_rotation_z((-dir.x).atan2(dir.y))),
                        Bullet { faction: FactionKind::Friendly, damage: slot.damage, remaining: TURRET_RANGE },
                        Velocity(dir * BULLET_SPEED),
                        RenderLayers::layer(PLAY_LAYER),
                    )).id();
                    let inner = commands.spawn((
                        Mesh2d(meshes.add(Capsule2d::new(1.3, 1.5))),
                        MeshMaterial2d(pm.bullet_friendly.clone()),
                        Transform::from_xyz(0.0, 0.0, 0.05),
                        RenderLayers::layer(PLAY_LAYER),
                    )).id();
                    commands.entity(inner).insert(ChildOf(bullet));

                    // Muzzle flash at the same barrel tip the bullet emerged from.
                    commands.spawn((
                        Mesh2d(em.muzzle_flash.clone()),
                        MeshMaterial2d(pm.bullet_friendly.clone()),
                        Transform::from_xyz(muzzle_pos.x, muzzle_pos.y, 5.0)
                            .with_rotation(Quat::from_rotation_z((-dir.x).atan2(dir.y))),
                        MuzzleFlash { life: 0.18, max_life: 0.18 },
                        RenderLayers::layer(PLAY_LAYER),
                    ));
                }
            }
        }

        // Suppress unused warning
        let _ = children;
        let _ = &mut barrels;
    }
}

fn enemy_fire(
    time: Res<Time>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    friendly: Query<&Transform, (With<Friendly>, Without<Enemy>)>,
    mut enemies: Query<(&Transform, &Heading, &mut Enemy)>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    let Ok(ftf) = friendly.single() else { return; };
    let fpos = ftf.translation.truncate();

    for (tf, heading, mut enemy) in &mut enemies {
        enemy.fire_cd -= dt;
        let pos = tf.translation.truncate();
        let to = fpos - pos;
        if to.length() > ENEMY_RANGE { continue; }
        // Forward dir (heading 0 = +Y)
        let forward = Vec2::new(-heading.0.sin(), heading.0.cos());
        let aim = forward.angle_to(to.normalize_or_zero()).abs();
        if aim > 0.2 { continue; }
        if enemy.fire_cd > 0.0 { continue; }
        enemy.fire_cd = 1.0 / ENEMY_FIRE_RATE;
        let dir = forward;
        // Spawn at the barrel tip (enemy barrel offset 1.8 + half-length 1.75 ≈ 3.5).
        let muzzle_pos = pos + forward * 3.5;
        let bullet = commands.spawn((
            Mesh2d(meshes.add(Capsule2d::new(1.5, 1.5))),
            MeshMaterial2d(pm.bullet_enemy_outer.clone()),
            Transform::from_xyz(muzzle_pos.x, muzzle_pos.y, 4.0)
                .with_rotation(Quat::from_rotation_z(heading.0)),
            Bullet { faction: FactionKind::Enemy, damage: 1, remaining: ENEMY_RANGE },
            Velocity(dir * BULLET_SPEED),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        let inner = commands.spawn((
            Mesh2d(meshes.add(Capsule2d::new(0.8, 1.5))),
            MeshMaterial2d(pm.bullet_enemy.clone()),
            Transform::from_xyz(0.0, 0.0, 0.05),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(inner).insert(ChildOf(bullet));

        // Muzzle flash at the same barrel tip the bullet emerged from.
        commands.spawn((
            Mesh2d(em.muzzle_flash.clone()),
            MeshMaterial2d(pm.bullet_enemy.clone()),
            Transform::from_xyz(muzzle_pos.x, muzzle_pos.y, 5.0)
                .with_rotation(Quat::from_rotation_z(heading.0)),
            MuzzleFlash { life: 0.18, max_life: 0.18 },
            RenderLayers::layer(PLAY_LAYER),
        ));
    }
}

fn bullet_update(
    time: Res<Time>,
    mut commands: Commands,
    mut q: Query<(Entity, &mut Bullet, &Velocity)>,
) {
    let dt = time.delta_secs();
    for (e, mut b, v) in &mut q {
        b.remaining -= v.0.length() * dt;
        if b.remaining <= 0.0 {
            commands.entity(e).despawn();
        }
    }
}

fn bullet_collisions(
    mut commands: Commands,
    mut score: ResMut<Score>,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    bullets: Query<(Entity, &Transform, &Bullet)>,
    mut enemies: Query<(Entity, &Transform, &mut Health, &mut HitFx), (With<Enemy>, Without<Friendly>)>,
    mut friendly: Query<(Entity, &Transform, &mut HitFx), (With<Friendly>, Without<Enemy>)>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let mut rng = rand::thread_rng();
    for (be, btf, b) in &bullets {
        let bp = btf.translation.truncate();
        match b.faction {
            FactionKind::Friendly => {
                for (ee, etf, mut h, mut fx) in &mut enemies {
                    if etf.translation.truncate().distance(bp) < 3.5 {
                        h.0 -= b.damage;
                        commands.entity(be).despawn();
                        let hit_pos = etf.translation.truncate();
                        if h.0 <= 0 {
                            commands.entity(ee).despawn();
                            score.0 += 10;
                            // Larger destruction burst — mix enemy + bullet colors.
                            spawn_hit_particles(&mut commands, &em, &pm.enemy, hit_pos, 10, 60.0, &mut rng);
                            spawn_hit_particles(&mut commands, &em, &pm.bullet_friendly, hit_pos, 6, 75.0, &mut rng);
                        } else {
                            // Pulse the survivor and spawn small impact sparks.
                            fx.pulse();
                            spawn_hit_particles(&mut commands, &em, &pm.bullet_friendly, hit_pos, 4, 45.0, &mut rng);
                        }
                        break;
                    }
                }
            }
            FactionKind::Enemy => {
                for (_fe, ftf, mut fx) in &mut friendly {
                    if ftf.translation.truncate().distance(bp) < 5.0 {
                        // Friendly is invincible — bullet is consumed but the
                        // ship still pulses + flashes for visual feedback.
                        commands.entity(be).despawn();
                        fx.pulse();
                        let hit_pos = ftf.translation.truncate();
                        spawn_hit_particles(&mut commands, &em, &pm.bullet_enemy, hit_pos, 5, 50.0, &mut rng);
                        break;
                    }
                }
            }
        }
    }
}

fn update_score_text(score: Res<Score>, mut q: Query<&mut Text, With<ScoreText>>) {
    if !score.is_changed() { return; }
    for mut t in &mut q {
        **t = format!("SCORE {}", score.0);
    }
}

fn ui_button_system(
    mut interactions: Query<(&Interaction, &SlotButton), Changed<Interaction>>,
    mut cfg: ResMut<TurretConfig>,
    mut window_mode: ResMut<WindowMode>,
) {
    for (interaction, btn) in &mut interactions {
        if !matches!(*interaction, Interaction::Pressed) { continue; }
        match btn.kind {
            ButtonKind::ToggleDesktopMode => {
                window_mode.desktop = !window_mode.desktop;
                continue;
            }
            _ => {}
        }
        let s = &mut cfg.slots[btn.slot];
        match btn.kind {
            ButtonKind::ToggleDesktopMode => unreachable!(),
            ButtonKind::Equip       => { if !s.equipped { s.equipped = true; } }
            ButtonKind::DamageUp    => { if s.equipped { s.damage += 1; } }
            ButtonKind::DamageDown  => { if s.equipped && s.damage > 1 { s.damage -= 1; } }
            ButtonKind::RateUp      => { if s.equipped { s.fire_rate += 0.1; } }
            ButtonKind::RateDown    => { if s.equipped && s.fire_rate > 0.2 { s.fire_rate -= 0.1; } }
            ButtonKind::BarrelsUp   => { if s.equipped && s.barrels < 2 { s.barrels += 1; } }
            ButtonKind::BarrelsDown => { if s.equipped && s.barrels > 1 { s.barrels -= 1; } }
        }
    }
}

fn update_slot_labels(
    cfg: Res<TurretConfig>,
    mut q: Query<(&SlotLabel, &mut Text)>,
) {
    if !cfg.is_changed() { return; }
    for (lbl, mut t) in &mut q {
        let s = cfg.slots[lbl.slot];
        match lbl.kind {
            LabelKind::Damage   => **t = format!("{}", s.damage),
            LabelKind::Rate     => **t = format!("{:.1}", s.fire_rate),
            LabelKind::Barrels  => **t = format!("{}", s.barrels),
            LabelKind::Status   => **t = if s.equipped { "ACTIVE".into() } else { "EQUIP GUN".into() },
        }
    }
}
