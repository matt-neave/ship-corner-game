//! Boot-time main menu — chunky-pixel UI mirroring the shop's render
//! pipeline.
//!
//! Architecture
//! ------------
//! - **`MainMenuCamera`** renders every primitive on `MAIN_MENU_LAYER`
//!   into a low-res image (320×200), which a `MainMenuDisplaySprite` on
//!   `UPSCALE_LAYER` shows nearest-neighbour upscaled. That low-res
//!   rasterisation is where the chunky-pixel button bodies + title
//!   backdrop come from — identical to how the customize overlay reads.
//! - **Clear colour is transparent**: gaps between the menu chrome are
//!   transparent pixels in the menu image, so the play render's ocean +
//!   the menu fleet (real `spawn_ally` ships drifting + firing at each
//!   other) show through. This is the only place in the codebase where
//!   one chunky-pixel screen *layers* on top of another.
//! - **Text labels** stay on `UPSCALE_LAYER` (native res) so glyphs
//!   stay crisp. Their *positions* track the chunky display's scale so
//!   they land on the buttons; their *transform scale* tracks
//!   `UiScale` so font size reads consistent with bevy_ui chrome.
//!
//! Click resolution is custom — no bevy_ui involvement. The menu's
//! `track_menu_cursor` system writes spec coords into
//! `MainMenuViewport::spec_cursor` each frame, and each click handler
//! tests the cursor against its button's centre + `HitArea`.

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::camera::RenderTarget;
use bevy::render::render_resource::{
    Extent3d, TextureDimension, TextureFormat, TextureUsages,
};
use bevy::render::view::{Msaa, RenderLayers};
use bevy::sprite::MeshMaterial2d;
use bevy::text::FontSmoothing;
use bevy::window::PrimaryWindow;
use rand::Rng;

use crate::ally::{spawn_ally, Ally, ShipClass};
use crate::balance::{
    MAIN_MENU_INTERNAL_H, MAIN_MENU_INTERNAL_W, MAIN_MENU_LAYER, PLAY_LAYER, UPSCALE_LAYER,
};
use crate::components::Velocity;
use crate::effects::EffectMeshes;
use crate::modes::{CrtMode, NightMode, VsyncMode};
use crate::palette::PaletteMaterials;
use crate::ui_kit::theme;
use crate::AppState;

/// Owns the main-menu screen end-to-end: the chunky-pixel render
/// target, the chrome primitives, the menu-fleet skirmish behind it,
/// the cursor tracker, and every click handler.
pub struct MainMenuPlugin;

impl Plugin for MainMenuPlugin {
    fn build(&self, app: &mut App) {
        app
            .insert_resource(MainMenuOpen::default())
            .insert_resource(MainMenuView::default())
            .insert_resource(MainMenuAnim::default())
            .insert_resource(MainMenuViewport::default())
            .add_systems(
                Startup,
                (setup_main_menu_render, setup_main_menu_chrome).chain(),
            )
            // First-boot menu fleet spawn. Has to be in **PostStartup**
            // (not Startup with `.after(setup_world)`) because `setup_world`
            // calls `commands.insert_resource(pm)` — a QUEUED command that
            // doesn't materialize in the World until the next
            // `apply_deferred` boundary. `.after()` only adds ordering, not
            // a flush; so a Startup-ordered spawn would see
            // `Option<Res<PaletteMaterials>>` as `None` and silently bail
            // (which is exactly the bug that made the pirates only appear
            // after a HullSelect→MainMenu round-trip — by then, Startup's
            // commands had long since flushed).
            //
            // PostStartup runs once, after the end-of-Startup apply_deferred,
            // so every Startup-inserted Resource is visible. The
            // idempotency check in `spawn_menu_fleet` makes the
            // `OnEnter(MainMenu)` registration a no-op on first boot
            // (already spawned here) and the load-bearing path on
            // re-entries (the fleet got despawned on `OnExit(MainMenu)`).
            .add_systems(PostStartup, spawn_menu_fleet)
            // Same OnEnter-doesn't-fire-on-initial-state workaround for
            // the HUD chrome hide. Without this, the WaveHpUi / XP bar
            // / FPS / map buttons are visible on the first boot's
            // landing screen — only hidden after a round-trip into
            // HullSelect and back. Setting Visibility at PostStartup
            // (after the chrome entities were Startup-spawned, but
            // before any Update frame can repaint them) is the
            // load-bearing fix.
            .add_systems(PostStartup, hide_gameplay_chrome_for_menu)
            .add_systems(
                OnEnter(AppState::MainMenu),
                (
                    // `reset_run_for_restart` despawns every `Ally`-
                    // tagged entity (it's wiping the dead run's
                    // recruited fleet) — including the menu pirates
                    // we're about to spawn. Chaining it FIRST and the
                    // spawn LAST is what guarantees the fleet survives
                    // on initial boot. Without the chain, the two
                    // systems race in parallel and the spawn loses
                    // about half the time. (`clear_arena_on_main_menu`
                    // also despawns Allies and is in the same chain
                    // for the same reason.)
                    (
                        crate::game_over::reset_run_for_restart,
                        clear_arena_on_main_menu,
                        spawn_menu_fleet,
                    ).chain(),
                    reset_xp_on_main_menu,
                    // One-shot hide for the in-combat HUD chrome.
                    // No per-frame enforcement needed now that the
                    // HUD writers are state-gated to skip MainMenu.
                    hide_gameplay_chrome_for_menu,
                    // PlayCamera gates on `ViewMode::Combat`. Boot
                    // default IS Combat, but returning from Map
                    // would leave `ViewMode::Map` and freeze the menu
                    // fleet's render. One-shot — nothing else writes
                    // ViewMode during MainMenu.
                    set_combat_view_for_menu,
                ),
            )
            .add_systems(
                OnExit(AppState::MainMenu),
                (
                    crate::ui::reset_damage_stats,
                    crate::enemy::clear_spawn_indicators,
                    despawn_menu_fleet,
                    show_gameplay_chrome_after_menu,
                ),
            )
            // Cursor tracker on its own so every click handler can
            // .after() it without re-listing every system here.
            .add_systems(Update, track_menu_cursor)
            // Re-assert chrome hidden every frame while on the menu.
            // Several Update systems (`update_map_button`, `update_wave_ui`,
            // etc.) flip Visibility on resource-changed; without this
            // gate, the HP bar / corner buttons re-appear the first
            // time their dependent resources fire `is_changed()`.
            // Only writes when `*v != Hidden`, so cost is negligible.
            .add_systems(
                Update,
                hide_gameplay_chrome_for_menu.run_if(in_state(AppState::MainMenu)),
            )
            .add_systems(
                Update,
                (
                    toggle_menu_render,
                    resize_menu_display,
                    sync_menu_view_visibility,
                    sync_menu_text,
                    update_menu_label_text,
                    update_menu_button_visuals,
                    tick_title_pulse,
                )
                    .after(track_menu_cursor),
            )
            .add_systems(
                Update,
                (
                    handle_menu_click,
                    play_menu_click_sound.after(handle_menu_click),
                    tick_menu_ships,
                    tick_menu_bullets.after(tick_menu_ships),
                )
                    .run_if(in_state(AppState::MainMenu))
                    .after(track_menu_cursor),
            )
            // Multiplayer click router — native-only sibling of
            // handle_menu_click. Splitting it out keeps both systems
            // under Bevy's 16-SystemParam ceiling.
            .add_systems(
                Update,
                {
                    #[cfg(not(target_arch = "wasm32"))]
                    {
                        handle_mp_menu_clicks
                            .run_if(in_state(AppState::MainMenu))
                            .after(track_menu_cursor)
                    }
                    #[cfg(target_arch = "wasm32")]
                    { || {} }
                },
            )
            // Shared bevy_ui settings-button handlers — used by the
            // pause-menu's settings panel (which is still bevy_ui).
            // Registered here, not in `pause`, because the click
            // router + the label refresher are the same logic both
            // menus would otherwise duplicate. Run unconditionally
            // so each owning menu's visibility determines what's
            // clickable.
            .add_systems(
                Update,
                (
                    handle_settings_item_click,
                    play_settings_click_sound.after(handle_settings_item_click),
                    update_settings_labels,
                ),
            );
    }
}

// ---------- Resources ----------

/// True while the main menu is up. Defaults to `true` so the menu is
/// the very first thing the player sees on launch. Driven by the
/// AppState bridge in `main.rs`.
#[derive(Resource)]
pub struct MainMenuOpen(pub bool);
impl Default for MainMenuOpen {
    fn default() -> Self { Self(true) }
}

/// Which sub-page of the main menu is showing — root (PLAY / SETTINGS)
/// or the settings panel. Resets to `Root` whenever the menu closes.
#[derive(Resource, Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum MainMenuView {
    #[default]
    Root,
    Settings,
}

/// Elapsed seconds since the menu opened. Used by the title's slow
/// colour pulse so the landing page reads as "alive".
#[derive(Resource, Default)]
pub struct MainMenuAnim {
    pub elapsed: f32,
}

/// Display rect of the chunky menu render on the window + current
/// cursor in spec coords. Spec coords are the menu's internal
/// 320×200 system centred on (0, 0) with +Y up (matches the shop's
/// `CustomizeViewport`).
#[derive(Resource, Default, Clone, Copy)]
pub struct MainMenuViewport {
    pub display_origin: Vec2,
    pub display_scale: f32,
    pub spec_cursor: Option<Vec2>,
}

impl MainMenuViewport {
    /// Convert a window-space cursor position to menu-spec coords.
    /// `None` if the cursor is outside the menu display rect.
    pub fn window_to_spec(&self, cursor: Vec2) -> Option<Vec2> {
        if self.display_scale <= 0.0 { return None; }
        let local = (cursor - self.display_origin) / self.display_scale;
        let w = MAIN_MENU_INTERNAL_W as f32;
        let h = MAIN_MENU_INTERNAL_H as f32;
        if local.x < 0.0 || local.x > w || local.y < 0.0 || local.y > h {
            return None;
        }
        Some(Vec2::new(local.x - w * 0.5, h * 0.5 - local.y))
    }
}

// ---------- Components / markers ----------

#[derive(Component)] pub struct MainMenuCamera;
#[derive(Component)] pub struct MainMenuDisplaySprite;

/// Every chunky primitive that belongs to the menu chrome carries
/// this so the visibility / view toggles can address them in bulk
/// without re-listing per-marker queries.
#[derive(Component, Clone)] pub struct MenuChrome;

/// Tag on chrome that's part of the root view (title, PLAY,
/// SETTINGS). Hidden when the player drills into the settings page.
#[derive(Component, Clone)] pub struct RootViewChrome;

/// Tag on chrome that's part of the settings sub-page. Hidden by
/// default; toggled visible when SETTINGS is clicked.
#[derive(Component, Clone)] pub struct SettingsViewChrome;

/// Spec-space (menu internal coord) position for a chunky-rendered
/// text label. `sync_menu_text` reads this each frame and writes the
/// world position on the `UPSCALE_LAYER` text entity.
#[derive(Component, Clone, Copy)]
pub struct MenuTextSpec(pub Vec2);

/// Hit-area for a menu button (in spec units). Centred on the entity's
/// Transform; the click router tests cursor against centre ± size/2.
#[derive(Component, Clone, Copy)]
pub struct HitArea { pub size: Vec2 }

/// Material + label handles for a single menu button. Lives on the
/// hit-test entity (alongside `HitArea` + `MenuButton`) so the visual
/// sync system can retint the fill / outline materials and nudge the
/// label on press without re-querying the menu's mesh entities.
#[derive(Component)]
pub struct MenuButtonVisuals {
    pub fill:          Handle<ColorMaterial>,
    pub outline:       Option<Handle<ColorMaterial>>,
    pub label:         Entity,
    /// Spec-space Y of the label at rest. The press nudge writes
    /// `MenuTextSpec.y = label_base_y - PRESS_NUDGE`.
    pub label_base_y:  f32,
}

/// Marker on the single hit-test entity per button. Holds the
/// `HitArea` + the `MenuButtonItem` so the click router can route
/// presses to the right action.
#[derive(Component, Clone, Copy)]
pub struct MenuButton { pub item: MenuButtonItem }

/// Marker on the title text so `tick_title_pulse` can lerp its
/// colour. Distinct from `MenuLabel` so the title doesn't get
/// overwritten by the per-frame label refresh.
#[derive(Component)]
pub struct PulsingTitle;

/// Per-letter index inside the wavy title so `sync_menu_text` can
/// stagger a sine bob across glyphs. Both the main copy of a letter
/// AND its four stroke copies carry the same `idx` so they wave as
/// a single column.
#[derive(Component, Clone, Copy)]
pub struct MenuWaveChar { pub idx: u8 }

/// Marker on text labels that show live state (the settings
/// ON/OFF / value labels). Driven by `update_menu_label_text`.
#[derive(Component, Clone, Copy)]
pub struct MenuLabel(pub MenuButtonItem);

/// Everything you can click in the menu. Settings flips boolean
/// modes; WindowMode / Resolution / Background cycle through their
/// presets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuButtonItem {
    Play,
    Host,
    Join,
    Settings,
    Quit,
    Night,
    Crt,
    Vsync,
    Bloom,
    WindowMode,
    Resolution,
    SfxVolume,
    MusicVolume,
    Background,
    Back,
}

impl MenuButtonItem {
    fn is_root(self) -> bool {
        matches!(self, Self::Play | Self::Host | Self::Join | Self::Settings | Self::Quit)
    }
}

/// Marker on each menu-fleet hull sailing in the play world. Same
/// chassis output as `spawn_ally` / `spawn_boss`; the menu owns
/// only the per-ship orbital AI + firing cooldown.
///
/// Movement model: each ship orbits the OTHER ship at a fixed engagement
/// range. Orbit direction is fixed per ship (+1 = CCW, −1 = CW) and
/// the two ships pick opposing directions so they pass each other
/// instead of locked in lockstep. Velocity = perpendicular tangent
/// (orbit) + radial correction (pull in when too far, push out when
/// too close). The hull rotates each frame to face its current
/// velocity, just like a real ship maneuvering.
///
/// `faction` drives the bullet visual so the friendly pirate's rounds
/// read yellow and the enemy boss's read red — same colour grammar
/// as in-combat fire.
#[derive(Component)]
pub struct MenuShip {
    pub faction: MenuFaction,
    pub next_fire_at: f32,
    /// +1.0 for counter-clockwise orbit, −1.0 for clockwise. Both
    /// ships share the same value so they actually orbit a common
    /// midpoint instead of drifting parallel.
    pub orbit_dir: f32,
    /// Current AI behavior. The state machine cycles Orbit → (Charge
    /// xor Retreat) → Orbit → ... so the engagement reads as ships
    /// maneuvering rather than rails-riding a circle.
    pub phase: MenuShipPhase,
    /// Anim-clock time at which the current phase expires.
    pub phase_end_at: f32,
    /// Last frame's world position. Used to estimate this frame's
    /// velocity for lead targeting in the fire pass — bullets aim at
    /// the predicted future target position so they actually connect
    /// instead of phasing past a moving hull.
    pub last_pos: Vec2,
}

/// Behavior state machine for the menu fleet. Each ship cycles
/// between holding the engagement range (Orbit) and short bursts of
/// closing (Charge) or peeling away (Retreat). The bursts give the
/// fight visible variance without a full combat sim.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuShipPhase {
    Orbit,
    Charge,
    Retreat,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuFaction { Friendly, Enemy }

/// Cosmetic bullet in flight between menu-fleet ships. Carries its
/// own lifetime so the menu's bullet tick can despawn it without
/// touching the regular bullet pipeline. `arm_remaining` is a brief
/// invulnerability window after firing so the round doesn't blow
/// up against its own ship's hull the instant it spawns — the real
/// game's bullet pipeline doesn't need this because the firer's
/// muzzle position is already past the hull cap, but our cosmetic
/// hits use a generous proximity radius that overlaps the firer.
#[derive(Component)]
pub struct MenuBullet {
    pub velocity: Vec2,
    pub lifetime: f32,
    pub arm_remaining: f32,
}

// ---------- Layout constants (spec units = menu internal pixels) ----------

const Z_BG: f32 = 1.0;
const Z_FG: f32 = 2.0;

/// Y where each row of root-view chrome sits, in spec units (0 = centre,
/// +Y = up). Title above zero, then PLAY / HOST / JOIN / SETTINGS
/// stacked below it. Title is naked text (drop-shadowed) — no backdrop
/// slab.
const TITLE_Y: f32     =  60.0;
const PLAY_Y: f32      =  30.0;
const HOST_Y: f32      =  10.0;
const JOIN_Y: f32      = -10.0;
const SETTINGS_Y: f32  = -30.0;
const QUIT_Y: f32      = -50.0;

const BUTTON_W: f32 = 72.0;
const BUTTON_H: f32 = 16.0;

const SETTINGS_BUTTON_W: f32 = 110.0;
const SETTINGS_BUTTON_H: f32 =  14.0;
const SETTINGS_ROW_GAP: f32  =   3.0;

const TITLE_FONT: f32   = 40.0;
const BUTTON_FONT: f32  = 20.0;
const SETTINGS_FONT: f32 = 14.0;

// Corner radius for the chunky button containers. Title is plain
// text now so it has no radius.
const BUTTON_RADIUS: f32 = 3.0;

/// Spec-unit offset for each stroke copy of the title text. At
/// display_scale ≈ 4 (default window) this paints a clean 2-pixel
/// outline; at higher scales the outline thickens proportionally so
/// it reads consistent across resolutions.
const TITLE_STROKE_OFFSET: f32 = 0.5;

/// Spec-pixel x-spacing between adjacent glyphs in the wavy title.
/// Tuned for `TITLE_FONT = 28.0` with the default Bevy font — at
/// smaller values neighbouring letters overlap, at larger ones the
/// title reads as widely-spaced characters instead of a word.
const TITLE_CHAR_SPACING: f32 = 5.0;

/// Peak vertical bob (spec units) of each glyph in the wave. Half
/// the stroke thickness so the wave reads as a gentle ripple, not a
/// jumping-letter strobe.
const TITLE_WAVE_AMP: f32 = 1.0;
/// Wave frequency in rad/sec.
const TITLE_WAVE_RATE: f32 = 4.0;
/// Phase offset between adjacent characters in the wave. ~0.55 rad
/// puts each subsequent letter about a third of a cycle behind the
/// previous one, giving a clean travelling-wave feel rather than the
/// chaotic look of a too-large phase step.
const TITLE_WAVE_PHASE_STEP: f32 = 0.55;

// Menu-fleet spawn positions (PLAY_LAYER world coords). One friendly
// pirate up top, one enemy boss pirate down low. After spawn, the
// orbital AI in `tick_menu_ships` takes over and the two converge to
// circle each other around the play-area centre.
const MENU_BAND_UPPER_Y: f32 =  55.0;
const MENU_BAND_LOWER_Y: f32 = -55.0;

/// Distance (world units) each ship tries to hold from its target.
/// Around the play-area's 200×200 box this puts the orbit radius at
/// ~35u centred on the midpoint between the two hulls — well inside
/// the visible play area.
const MENU_ENGAGEMENT_RANGE: f32 = 70.0;
/// World units/sec each ship sweeps along its orbital tangent.
const MENU_ORBIT_SPEED: f32 = 22.0;
/// Forward speed during a Charge phase — fast closing run.
const MENU_CHARGE_SPEED: f32 = 60.0;
/// Backward speed during a Retreat phase — peel-out from target.
const MENU_RETREAT_SPEED: f32 = 50.0;
/// Duration range (seconds) for an Orbit phase before flipping to a
/// burst (Charge or Retreat). Slow base — the orbit is the calm
/// middle ground.
const MENU_ORBIT_DURATION_MIN: f32 = 3.0;
const MENU_ORBIT_DURATION_MAX: f32 = 5.0;
/// Duration range (seconds) for a burst phase. Short bursts so the
/// engagement keeps moving without either hull running away.
const MENU_BURST_DURATION_MIN: f32 = 1.2;
const MENU_BURST_DURATION_MAX: f32 = 1.8;
/// Strength of the radial correction that holds the engagement
/// range. A pure tangent (0.0) lets the orbit drift; a stiff value
/// (>1.0) snaps it taut. 0.4 lands in the middle — visible breathing
/// but never spiraling away.
const MENU_RADIAL_PULL: f32 = 0.4;

/// Distance from world origin past which a ship gets pulled back
/// toward the centre. Inside this radius the orbit runs free; past
/// it, the leash kicks in proportional to overshoot. Sized so the
/// engagement (orbit radius ≈ 35u centred on origin) stays well
/// inside the play area's ±100u bounds with breathing room.
const MENU_LEASH_RADIUS: f32 = 70.0;
/// Strength of the centre-pull leash. The pull is `over * STRENGTH`
/// per second, so a hull 30u past the leash gets pulled inward at
/// 18 u/s — enough to dominate the tangential orbit speed (30 u/s)
/// and bend the orbit back on-screen without snapping rigid.
const MENU_LEASH_STRENGTH: f32 = 0.6;

/// Speed of cosmetic cannonballs lobbed between the two pirates.
const MENU_BULLET_SPEED: f32 = 90.0;

// ---------- Colours ----------

fn bg_button_color()       -> Color { Color::srgb(0.20, 0.22, 0.28) }
fn bg_button_hover_color() -> Color { Color::srgb(0.28, 0.31, 0.40) }
fn bg_button_press_color() -> Color { Color::srgb(0.35, 0.40, 0.52) }

/// Outline ring color at rest — same dark-navy as the title's drop
/// shadow stroke, so the button frame shares the title's visual
/// vocabulary instead of inventing a third dark tone.
fn outline_button_color()       -> Color { Color::srgb(0.102, 0.110, 0.173) }
/// Hover lifts the outline to a mid-blue so the frame "wakes up"
/// alongside the lightened fill.
fn outline_button_hover_color() -> Color { Color::srgb(0.45, 0.55, 0.70) }
/// Press dims the outline back toward the resting dark — the press
/// state reads as "the button accepts the click" through the fill
/// brightening and the label dropping, not the outline glowing
/// further.
fn outline_button_press_color() -> Color { Color::srgb(0.18, 0.22, 0.30) }

/// Outline thickness in spec units (= chunky-pixel units). 1.0 paints
/// a clean 1-pixel frame at the menu's native resolution.
const BUTTON_OUTLINE_THICKNESS: f32 = 1.0;

/// Spec-space Y nudge applied to the button label when its button is
/// pressed. One chunky pixel down — same vocabulary as classic
/// pixel-art tactile buttons.
const BUTTON_PRESS_LABEL_NUDGE: f32 = 1.0;

/// Returns the palette every chunky menu button shares — fill +
/// outline tints for idle / hover / press. Single definition so the
/// settings-panel buttons can adopt the same look without duplication.
fn menu_button_palette() -> crate::menu_kit::ChunkyButtonPalette {
    crate::menu_kit::ChunkyButtonPalette {
        idle_fill:     bg_button_color(),
        hover_fill:    bg_button_hover_color(),
        press_fill:    bg_button_press_color(),
        idle_outline:  outline_button_color(),
        hover_outline: outline_button_hover_color(),
        press_outline: outline_button_press_color(),
    }
}

// ---------- Render pipeline setup ----------

/// Build the menu's render target + camera + display sprite. Mirrors
/// `customize::render::setup_customize_render` so the same chunky-pixel
/// upscale behaviour applies. Difference: clear colour is *transparent*
/// so the play render with the menu fleet shows through the gaps
/// between buttons.
pub fn setup_main_menu_render(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
) {
    let size = Extent3d {
        width: MAIN_MENU_INTERNAL_W,
        height: MAIN_MENU_INTERNAL_H,
        depth_or_array_layers: 1,
    };
    let mut img = Image::new_fill(
        size,
        TextureDimension::D2,
        // Initial fill alpha=0 so any pixel never touched by the
        // menu camera reads as transparent in the upscaled sprite.
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
            // Transparent clear so the play upscale sprite behind us
            // (showing the menu fleet) is visible in the gaps between
            // chrome primitives.
            clear_color: ClearColorConfig::Custom(Color::srgba(0.0, 0.0, 0.0, 0.0)),
            order: -3,
            is_active: false,
            ..default()
        },
        Projection::Orthographic(OrthographicProjection {
            scaling_mode: bevy::render::camera::ScalingMode::Fixed {
                width: MAIN_MENU_INTERNAL_W as f32,
                height: MAIN_MENU_INTERNAL_H as f32,
            },
            ..OrthographicProjection::default_2d()
        }),
        RenderLayers::layer(MAIN_MENU_LAYER),
        Msaa::Off,
        MainMenuCamera,
    ));

    // Display sprite — fits the window via `resize_menu_display`. z=2.5
    // so we sit above the customize stack (z=2.0) even though they
    // shouldn't both be active at once.
    commands.spawn((
        Sprite {
            image: handle,
            custom_size: Some(Vec2::new(
                MAIN_MENU_INTERNAL_W as f32 * 4.0,
                MAIN_MENU_INTERNAL_H as f32 * 4.0,
            )),
            ..default()
        },
        Transform::from_xyz(0.0, 0.0, 2.5),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        MainMenuDisplaySprite,
    ));
}

/// Activate / deactivate the menu camera + display sprite based on
/// `MainMenuOpen`. Same pattern as `toggle_customize_render`.
pub fn toggle_menu_render(
    open: Res<MainMenuOpen>,
    mut cam_q: Query<&mut Camera, With<MainMenuCamera>>,
    mut display_q: Query<&mut Visibility, With<MainMenuDisplaySprite>>,
) {
    if !open.is_changed() { return; }
    for mut cam in &mut cam_q {
        cam.is_active = open.0;
    }
    let want = if open.0 { Visibility::Inherited } else { Visibility::Hidden };
    for mut vis in &mut display_q {
        if *vis != want { *vis = want; }
    }
}

/// Fit the display sprite to the window each frame and update the
/// viewport so cursor → spec math stays in sync on resize.
pub fn resize_menu_display(
    windows: Query<&Window, With<PrimaryWindow>>,
    mut sprite_q: Query<(&mut Sprite, &mut Transform), With<MainMenuDisplaySprite>>,
    mut viewport: ResMut<MainMenuViewport>,
) {
    let Ok(win) = windows.single() else { return };
    let win_w = win.width();
    let win_h = win.height();
    if win_w <= 0.0 || win_h <= 0.0 { return; }
    // Fit-mode: max integer-friendly scale that still fits the window.
    let scale_x = win_w / MAIN_MENU_INTERNAL_W as f32;
    let scale_y = win_h / MAIN_MENU_INTERNAL_H as f32;
    let scale = scale_x.min(scale_y).max(0.5);

    let display_w = MAIN_MENU_INTERNAL_W as f32 * scale;
    let display_h = MAIN_MENU_INTERNAL_H as f32 * scale;
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
        if sprite.custom_size != want { sprite.custom_size = want; }
        if tf.translation != Vec3::new(0.0, 0.0, 2.5) {
            tf.translation = Vec3::new(0.0, 0.0, 2.5);
        }
    }
}

/// Update `MainMenuViewport.spec_cursor` from the OS cursor + the live
/// display rect. Runs first in the Update schedule so click handlers
/// read a fresh value.
pub fn track_menu_cursor(
    open: Res<MainMenuOpen>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut viewport: ResMut<MainMenuViewport>,
) {
    if !open.0 {
        viewport.spec_cursor = None;
        return;
    }
    let cursor = windows.single().ok().and_then(|w| w.cursor_position());
    viewport.spec_cursor = cursor.and_then(|c| viewport.window_to_spec(c));
}

// ---------- Chrome spawning ----------

/// One-shot Startup spawn for every menu chrome entity (title slab,
/// PLAY / SETTINGS buttons, settings sub-page). The fleet ships and
/// click router live in their own systems; this is purely the static
/// visual primitives.
pub fn setup_main_menu_chrome(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    thaleah: Res<crate::fonts::ThaleahFont>,
) {
    // ---- Title (no backdrop — per-letter wave with stroke) ----
    // Each glyph is its own Text2d entity tagged with a MenuWaveChar
    // index, so `sync_menu_text` can stagger a sin-bob across them
    // and we get a clean horizontal wave rippling through GUNBOAT-8.
    // Uses Thaleah Fat for the display-weight title silhouette.
    spawn_wavy_title(
        &mut commands,
        Vec2::new(0.0, TITLE_Y),
        "GUNBOAT-8",
        theme::ACCENT,
        Color::srgb(0.04, 0.05, 0.07),
        TITLE_FONT,
        TITLE_CHAR_SPACING,
        &thaleah,
    );

    // ---- PLAY / HOST / JOIN / SETTINGS buttons (root view) ----
    spawn_menu_button(
        &mut commands, &mut meshes, &mut materials,
        Vec2::new(0.0, PLAY_Y), Vec2::new(BUTTON_W, BUTTON_H),
        MenuButtonItem::Play, "PLAY", BUTTON_FONT, true,
    );
    spawn_menu_button(
        &mut commands, &mut meshes, &mut materials,
        Vec2::new(0.0, HOST_Y), Vec2::new(BUTTON_W, BUTTON_H),
        MenuButtonItem::Host, "HOST", BUTTON_FONT, true,
    );
    spawn_menu_button(
        &mut commands, &mut meshes, &mut materials,
        Vec2::new(0.0, JOIN_Y), Vec2::new(BUTTON_W, BUTTON_H),
        MenuButtonItem::Join, "JOIN", BUTTON_FONT, true,
    );
    spawn_menu_button(
        &mut commands, &mut meshes, &mut materials,
        Vec2::new(0.0, SETTINGS_Y), Vec2::new(BUTTON_W, BUTTON_H),
        MenuButtonItem::Settings, "SETTINGS", BUTTON_FONT, true,
    );
    spawn_menu_button(
        &mut commands, &mut meshes, &mut materials,
        Vec2::new(0.0, QUIT_Y), Vec2::new(BUTTON_W, BUTTON_H),
        MenuButtonItem::Quit, "QUIT", BUTTON_FONT, true,
    );

    // ---- Settings sub-page (hidden until SETTINGS clicked) ----
    // Stack from top of menu downward so all seven rows fit inside the
    // 200px height with the title cleared away.
    let settings_items = [
        MenuButtonItem::Night,
        MenuButtonItem::Crt,
        MenuButtonItem::Vsync,
        MenuButtonItem::Bloom,
        MenuButtonItem::WindowMode,
        MenuButtonItem::Resolution,
        MenuButtonItem::SfxVolume,
        MenuButtonItem::MusicVolume,
        MenuButtonItem::Background,
        MenuButtonItem::Back,
    ];
    let total_h = settings_items.len() as f32 * (SETTINGS_BUTTON_H + SETTINGS_ROW_GAP)
        - SETTINGS_ROW_GAP;
    let top_y = total_h * 0.5;
    for (idx, item) in settings_items.iter().enumerate() {
        let y = top_y - idx as f32 * (SETTINGS_BUTTON_H + SETTINGS_ROW_GAP)
            - SETTINGS_BUTTON_H * 0.5;
        spawn_menu_button(
            &mut commands, &mut meshes, &mut materials,
            Vec2::new(0.0, y),
            Vec2::new(SETTINGS_BUTTON_W, SETTINGS_BUTTON_H),
            *item,
            initial_label_for(*item),
            SETTINGS_FONT,
            false,
        );
    }
}

/// Spawn one menu button: chunky rounded panel + outline ring + label,
/// plus a dimensionless hit-test entity that carries the routing
/// markers and the material/label handles the visual sync system
/// needs. The visual chrome itself is delegated to `menu_kit` so other
/// screens can reuse it; this function is the *menu-specific*
/// composition (routing enum, view tag, label spec).
fn spawn_menu_button(
    commands:  &mut Commands,
    meshes:    &mut Assets<Mesh>,
    materials: &mut Assets<ColorMaterial>,
    centre:    Vec2,
    size:      Vec2,
    item:      MenuButtonItem,
    label:     &str,
    font_size: f32,
    is_root_view: bool,
) {
    let palette = menu_button_palette();
    let style = crate::menu_kit::ChunkyPanelStyle {
        fill:              palette.idle_fill,
        radius:            BUTTON_RADIUS,
        outline_color:     palette.idle_outline,
        outline_thickness: BUTTON_OUTLINE_THICKNESS,
    };
    let handles = crate::menu_kit::spawn_chunky_panel(
        commands, meshes, materials, centre, size, &style, MAIN_MENU_LAYER, Z_BG,
    );
    // The panel helper doesn't know about menu chrome tags; stamp them
    // on every spawned mesh so the view-visibility + cleanup queries
    // still cover them.
    for &e in handles.fill_entities.iter().chain(handles.outline_entities.iter()) {
        commands.entity(e).insert(MenuChrome);
        if is_root_view {
            commands.entity(e).insert(RootViewChrome);
        } else {
            commands.entity(e).insert(SettingsViewChrome);
        }
    }

    // Crisp text label on UPSCALE_LAYER. Spawned before the hit entity
    // so the hit can store the label's entity id.
    let label_extra = (MenuLabel(item),);
    let label_entity = spawn_menu_text_with_view_returning(
        commands, centre, label, Color::srgb(0.96, 0.96, 0.96),
        font_size, label_extra, is_root_view,
    );

    // Hit-test entity: dimensionless transform at the button centre
    // carrying the HitArea + routing marker + visuals handles.
    let hit = commands.spawn((
        Transform::from_translation(centre.extend(Z_FG)),
        HitArea { size },
        MenuButton { item },
        MenuButtonVisuals {
            fill:          handles.fill_material,
            outline:       handles.outline_material,
            label:         label_entity,
            label_base_y:  centre.y,
        },
        MenuChrome,
        Visibility::Inherited,
    )).id();
    if is_root_view {
        commands.entity(hit).insert(RootViewChrome);
    } else {
        commands.entity(hit).insert(SettingsViewChrome);
    }
}

/// Spawn the wavy title as a stack of per-letter Text2d entities. Each
/// glyph gets a unique `MenuWaveChar` index so `sync_menu_text` can
/// stagger a sin-bob across them — adjacent letters lag one another
/// by `TITLE_WAVE_PHASE_STEP` rad, giving a left-to-right travelling
/// ripple. Around each main glyph sit four dark stroke copies offset
/// N/S/E/W; they carry the same wave index so they bob in lockstep
/// with their parent letter and the outline reads as a single
/// chunky-pixel border around the moving glyph.
fn spawn_wavy_title(
    commands: &mut Commands,
    centre: Vec2,
    text: &str,
    color: Color,
    stroke_color: Color,
    font_size: f32,
    char_spacing: f32,
    thaleah: &crate::fonts::ThaleahFont,
) {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len() as f32;
    let total_w = (n - 1.0).max(0.0) * char_spacing;
    let start_x = centre.x - total_w * 0.5;

    for (idx, ch) in chars.iter().enumerate() {
        let x = start_x + idx as f32 * char_spacing;
        let pos = Vec2::new(x, centre.y);
        let glyph = ch.to_string();
        let wave = MenuWaveChar { idx: idx as u8 };

        // Stroke (4 dark copies offset N/S/E/W). Same wave index so
        // they ripple along with the main glyph.
        for (dx, dy) in [
            ( TITLE_STROKE_OFFSET,  0.0),
            (-TITLE_STROKE_OFFSET,  0.0),
            (0.0,  TITLE_STROKE_OFFSET),
            (0.0, -TITLE_STROKE_OFFSET),
        ] {
            commands.spawn((
                Text2d::new(glyph.clone()),
                crate::fonts::thaleah_text_font(thaleah, font_size),
                TextColor(stroke_color),
                Transform::from_xyz(0.0, 0.0, 99.0),
                Visibility::Hidden,
                RenderLayers::layer(UPSCALE_LAYER),
                MenuTextSpec(Vec2::new(pos.x + dx, pos.y + dy)),
                MenuChrome,
                RootViewChrome,
                wave,
            ));
        }

        // Main glyph — pulses + waves.
        commands.spawn((
            Text2d::new(glyph),
            crate::fonts::thaleah_text_font(thaleah, font_size),
            TextColor(color),
            Transform::from_xyz(0.0, 0.0, 100.0),
            Visibility::Hidden,
            RenderLayers::layer(UPSCALE_LAYER),
            MenuTextSpec(pos),
            MenuChrome,
            RootViewChrome,
            PulsingTitle,
            wave,
        ));
    }
}

/// Variant of `spawn_menu_text` that also stamps the view tag —
/// `RootViewChrome` or `SettingsViewChrome` — so the per-view
/// visibility toggle hides text on the right page.
/// Spawn one chunky-pixel text entity on the upscale render layer.
/// `spec_pos` is in spec units (menu internal coords); `sync_menu_text`
/// reads it each frame to project onto the upscale layer. Returns the
/// spawned entity so callers (like `spawn_menu_button`) can keep a
/// handle to it for per-state effects like the press-down nudge.
fn spawn_menu_text_with_view_returning<B: Bundle>(
    commands: &mut Commands,
    spec_pos: Vec2,
    text: impl Into<String>,
    color: Color,
    font_size: f32,
    extra: B,
    is_root_view: bool,
) -> Entity {
    let e = commands.spawn((
        Text2d::new(text),
        TextFont {
            font_size,
            font_smoothing: FontSmoothing::None,
            ..default()
        },
        TextColor(color),
        Transform::from_xyz(0.0, 0.0, 100.0),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        MenuTextSpec(spec_pos),
        MenuChrome,
        extra,
    )).id();
    if is_root_view {
        commands.entity(e).insert(RootViewChrome);
    } else {
        commands.entity(e).insert(SettingsViewChrome);
    }
    e
}

// ---------- Per-frame UI sync ----------

/// Position + scale every menu text entity each frame: position from
/// `MenuTextSpec * viewport.display_scale`, glyph scale from
/// `UiScale` so font sizes read consistent with bevy_ui chrome.
/// Mirrors `customize::setup::sync_customize_text`.
pub fn sync_menu_text(
    open: Res<MainMenuOpen>,
    viewport: Res<MainMenuViewport>,
    ui_scale: Res<bevy::ui::UiScale>,
    anim: Res<MainMenuAnim>,
    mut q: Query<(&MenuTextSpec, &mut Transform, Option<&MenuWaveChar>), With<MenuChrome>>,
) {
    if !open.0 { return; }
    let s = viewport.display_scale;
    let scale = ui_scale.0;
    let want_scale = Vec3::new(scale, scale, 1.0);
    let t = anim.elapsed;
    for (spec, mut tf, wave) in &mut q {
        // Wave-tagged glyphs (the title) get an extra per-letter Y bob
        // staggered by their index. Letters without the tag (button
        // labels) read zero offset and sit still.
        let wave_y = match wave {
            Some(w) => (t * TITLE_WAVE_RATE + w.idx as f32 * TITLE_WAVE_PHASE_STEP).sin()
                       * TITLE_WAVE_AMP,
            None    => 0.0,
        };
        tf.translation.x = spec.0.x * s;
        tf.translation.y = (spec.0.y + wave_y) * s;
        if tf.scale != want_scale { tf.scale = want_scale; }
    }
}

/// Push the open / view state into every chrome entity's `Visibility`.
/// Root-view entities visible in `MainMenuView::Root`, settings
/// entities visible in `Settings`. Hidden everywhere if the menu
/// itself is closed.
pub fn sync_menu_view_visibility(
    open: Res<MainMenuOpen>,
    mut view: ResMut<MainMenuView>,
    mut root: Query<&mut Visibility, (With<RootViewChrome>, Without<SettingsViewChrome>)>,
    mut settings: Query<&mut Visibility, (With<SettingsViewChrome>, Without<RootViewChrome>)>,
) {
    // Closing the menu rewinds to Root so the next open lands the
    // player on the front page.
    if !open.0 && *view != MainMenuView::Root {
        *view = MainMenuView::Root;
    }
    let (root_want, settings_want) = if !open.0 {
        (Visibility::Hidden, Visibility::Hidden)
    } else {
        match *view {
            MainMenuView::Root     => (Visibility::Inherited, Visibility::Hidden),
            MainMenuView::Settings => (Visibility::Hidden, Visibility::Inherited),
        }
    };
    for mut v in &mut root {
        if *v != root_want { *v = root_want; }
    }
    for mut v in &mut settings {
        if *v != settings_want { *v = settings_want; }
    }
}

/// Per-frame: tint each button's fill + outline materials based on
/// hover / press, and nudge the label down 1 spec-pixel on press. One
/// material per button (shared across the rounded panel's six meshes,
/// and another for the outline ring), so a single `materials.get_mut`
/// retints the whole shape.
pub fn update_menu_button_visuals(
    open: Res<MainMenuOpen>,
    view: Res<MainMenuView>,
    viewport: Res<MainMenuViewport>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    buttons: Query<(&Transform, &HitArea, &MenuButton, &MenuButtonVisuals)>,
    mut text_specs: Query<&mut MenuTextSpec>,
) {
    if !open.0 { return; }
    use crate::menu_kit::{apply_button_visual_state, ChunkyPanelHandles, ButtonVisualState};

    let active_view_root = *view == MainMenuView::Root;
    let mut hovered_item: Option<MenuButtonItem> = None;
    if let Some(cursor) = viewport.spec_cursor {
        for (tf, hit, btn, _) in &buttons {
            let in_view = if active_view_root { btn.item.is_root() } else { !btn.item.is_root() };
            if !in_view { continue; }
            let c = tf.translation.truncate();
            let half = hit.size * 0.5;
            if (cursor.x - c.x).abs() <= half.x && (cursor.y - c.y).abs() <= half.y {
                hovered_item = Some(btn.item);
                break;
            }
        }
    }
    let pressed = mouse.pressed(MouseButton::Left);
    let palette = menu_button_palette();

    for (_, _, btn, visuals) in &buttons {
        let in_view = if active_view_root { btn.item.is_root() } else { !btn.item.is_root() };
        let state = if !in_view {
            ButtonVisualState::Idle
        } else if Some(btn.item) == hovered_item {
            if pressed { ButtonVisualState::Press } else { ButtonVisualState::Hover }
        } else {
            ButtonVisualState::Idle
        };
        // Reconstruct a thin handles view so the kit helper can do the
        // retint without re-querying the world. The Vec fields are
        // unused by `apply_button_visual_state` — only the material
        // handles matter.
        let handles_view = ChunkyPanelHandles {
            fill_material:    visuals.fill.clone(),
            outline_material: visuals.outline.clone(),
            fill_entities:    Vec::new(),
            outline_entities: Vec::new(),
        };
        apply_button_visual_state(&mut materials, &handles_view, &palette, state);

        // Label nudge on press — write through `MenuTextSpec` so
        // `sync_menu_text` picks up the new Y on the same frame.
        if let Ok(mut spec) = text_specs.get_mut(visuals.label) {
            let want_y = if matches!(state, ButtonVisualState::Press) {
                visuals.label_base_y - BUTTON_PRESS_LABEL_NUDGE
            } else {
                visuals.label_base_y
            };
            if (spec.0.y - want_y).abs() > f32::EPSILON {
                spec.0.y = want_y;
            }
        }
    }
}

/// Rewrite each live-state label (NIGHT: ON, RES: 1280X800, etc.)
/// from the current mode resources. Same role as the old
/// `update_settings_labels`, but rewriting Text2d on the upscale
/// layer instead of bevy_ui `Text`.
pub fn update_menu_label_text(
    night: Res<NightMode>,
    crt: Res<CrtMode>,
    vsync: Res<VsyncMode>,
    bloom: Res<crate::modes::BloomMode>,
    win_mode: Res<crate::modes::WindowModeSetting>,
    res: Res<crate::modes::ResolutionSetting>,
    sfx_vol: Res<crate::sfx::SfxVolume>,
    music_vol: Res<crate::sfx::MusicVolume>,
    bg: Res<crate::modes::BackgroundSetting>,
    mut q: Query<(&MenuLabel, &mut Text2d)>,
) {
    for (label, mut text) in &mut q {
        let s = match label.0 {
            MenuButtonItem::Play        => "PLAY".to_string(),
            MenuButtonItem::Host        => "HOST".to_string(),
            MenuButtonItem::Join        => "JOIN".to_string(),
            MenuButtonItem::Settings    => "SETTINGS".to_string(),
            MenuButtonItem::Quit        => "QUIT".to_string(),
            MenuButtonItem::Night       => format!("NIGHT: {}", on_off(night.active)),
            MenuButtonItem::Crt         => format!("CRT: {}",   on_off(crt.active)),
            MenuButtonItem::Vsync       => format!("VSYNC: {}", on_off(vsync.enabled)),
            MenuButtonItem::Bloom       => format!("BLOOM: {}", on_off(bloom.active)),
            MenuButtonItem::WindowMode  => format!("WINDOW: {}", win_mode.mode.label()),
            MenuButtonItem::Resolution  => format!("RES: {}",    res.res.label()),
            MenuButtonItem::SfxVolume   => format!("SFX: {}",    sfx_vol.label()),
            MenuButtonItem::MusicVolume => format!("MUSIC: {}",  music_vol.label()),
            MenuButtonItem::Background  => format!("BG: {}",     bg.kind.label()),
            MenuButtonItem::Back        => "BACK".to_string(),
        };
        if text.0 != s { text.0 = s; }
    }
}

/// Pulse the title's TextColor between accent yellow and a brighter
/// near-white on a slow heartbeat so the landing page reads as alive.
pub fn tick_title_pulse(
    time: Res<Time>,
    open: Res<MainMenuOpen>,
    mut anim: ResMut<MainMenuAnim>,
    mut title: Query<&mut TextColor, With<PulsingTitle>>,
) {
    if !open.0 { return; }
    anim.elapsed += time.delta_secs();
    let t = anim.elapsed;
    let pulse = ((t * 1.4).sin() * 0.5 + 0.5).powf(2.0);
    let accent: bevy::color::Srgba = theme::ACCENT.into();
    let bright = bevy::color::Srgba::new(1.0, 0.95, 0.78, 1.0);
    let mix = 0.35 * pulse;
    let r = accent.red   + (bright.red   - accent.red)   * mix;
    let g = accent.green + (bright.green - accent.green) * mix;
    let b = accent.blue  + (bright.blue  - accent.blue)  * mix;
    let pulsed = Color::srgb(r, g, b);
    for mut tc in &mut title {
        tc.0 = pulsed;
    }
}

// ---------- Click handling ----------

/// One click router for every menu button. Tests the cursor against
/// each button's HitArea; on a hit, performs the action (start game /
/// open settings / flip a mode). Runs only in `AppState::MainMenu`.
pub fn handle_menu_click(
    mouse: Res<ButtonInput<MouseButton>>,
    viewport: Res<MainMenuViewport>,
    mut view: ResMut<MainMenuView>,
    mut next_state: ResMut<NextState<AppState>>,
    mut night: ResMut<NightMode>,
    mut crt: ResMut<CrtMode>,
    mut vsync: ResMut<VsyncMode>,
    mut bloom: ResMut<crate::modes::BloomMode>,
    mut win_mode: ResMut<crate::modes::WindowModeSetting>,
    mut res: ResMut<crate::modes::ResolutionSetting>,
    mut sfx_vol: ResMut<crate::sfx::SfxVolume>,
    mut music_vol: ResMut<crate::sfx::MusicVolume>,
    mut bg: ResMut<crate::modes::BackgroundSetting>,
    mut exit: EventWriter<bevy::app::AppExit>,
    buttons: Query<(&Transform, &HitArea, &MenuButton)>,
) {
    if !mouse.just_pressed(MouseButton::Left) { return; }
    let Some(cursor) = viewport.spec_cursor else { return };
    let active_view_root = *view == MainMenuView::Root;

    for (tf, hit, btn) in &buttons {
        let in_view = if active_view_root { btn.item.is_root() } else { !btn.item.is_root() };
        if !in_view { continue; }
        let c = tf.translation.truncate();
        let half = hit.size * 0.5;
        if (cursor.x - c.x).abs() > half.x { continue; }
        if (cursor.y - c.y).abs() > half.y { continue; }

        match btn.item {
            MenuButtonItem::Play       => next_state.set(AppState::HullSelect),
            // HOST / JOIN routing lives in `handle_mp_menu_clicks` —
            // splitting it out keeps `handle_menu_click` under Bevy's
            // 16-`SystemParam` limit (the MP resources push us over).
            MenuButtonItem::Host       => { /* see handle_mp_menu_clicks */ }
            MenuButtonItem::Join       => { /* see handle_mp_menu_clicks */ }
            MenuButtonItem::Settings   => *view = MainMenuView::Settings,
            MenuButtonItem::Quit       => { exit.write(bevy::app::AppExit::Success); }
            MenuButtonItem::Night      => night.active = !night.active,
            MenuButtonItem::Crt        => crt.active = !crt.active,
            MenuButtonItem::Vsync      => vsync.enabled = !vsync.enabled,
            MenuButtonItem::Bloom      => bloom.active = !bloom.active,
            MenuButtonItem::WindowMode => win_mode.mode = win_mode.mode.cycle(),
            MenuButtonItem::Resolution => res.res = res.res.cycle(),
            MenuButtonItem::SfxVolume   => *sfx_vol = sfx_vol.cycle(),
            MenuButtonItem::MusicVolume => *music_vol = music_vol.cycle(),
            MenuButtonItem::Background  => bg.kind = bg.kind.cycle(),
            MenuButtonItem::Back        => *view = MainMenuView::Root,
        }
        return;
    }
}

/// Tactile click sound on any menu button press. Split from the click
/// router because the router takes `ResMut<SfxVolume>` for the SFX
/// cycle, which conflicts with `SfxPlayer`'s `Res<SfxVolume>` at
/// Bevy's system-param check. Ordered .after() the router so the new
/// volume is in effect when the Switch sound plays.
pub fn play_menu_click_sound(
    mouse: Res<ButtonInput<MouseButton>>,
    viewport: Res<MainMenuViewport>,
    view: Res<MainMenuView>,
    buttons: Query<(&Transform, &HitArea, &MenuButton)>,
    mut sfx: crate::sfx::SfxPlayer,
) {
    if !mouse.just_pressed(MouseButton::Left) { return; }
    let Some(cursor) = viewport.spec_cursor else { return };
    let active_view_root = *view == MainMenuView::Root;
    for (tf, hit, btn) in &buttons {
        let in_view = if active_view_root { btn.item.is_root() } else { !btn.item.is_root() };
        if !in_view { continue; }
        let c = tf.translation.truncate();
        let half = hit.size * 0.5;
        if (cursor.x - c.x).abs() <= half.x && (cursor.y - c.y).abs() <= half.y {
            sfx.play(crate::sfx::Sfx::Switch);
            return;
        }
    }
}

/// Sibling click router for HOST / JOIN — split out of
/// `handle_menu_click` because the multiplayer resources push the
/// main handler over Bevy's 16-`SystemParam` limit. Same hit-test
/// shape, narrower button match. Native-only signature; the WASM
/// build still has the buttons in the chrome but this system
/// isn't registered there (see `MainMenuPlugin::build`).
#[cfg(not(target_arch = "wasm32"))]
pub fn handle_mp_menu_clicks(
    mouse: Res<ButtonInput<MouseButton>>,
    viewport: Res<MainMenuViewport>,
    view: Res<MainMenuView>,
    mut net_mode: ResMut<crate::multiplayer::NetMode>,
    buttons: Query<(&Transform, &HitArea, &MenuButton)>,
) {
    if !mouse.just_pressed(MouseButton::Left) { return; }
    let Some(cursor) = viewport.spec_cursor else { return };
    let active_view_root = *view == MainMenuView::Root;
    for (tf, hit, btn) in &buttons {
        let in_view = if active_view_root { btn.item.is_root() } else { !btn.item.is_root() };
        if !in_view { continue; }
        let c = tf.translation.truncate();
        let half = hit.size * 0.5;
        if (cursor.x - c.x).abs() > half.x { continue; }
        if (cursor.y - c.y).abs() > half.y { continue; }

        match btn.item {
            MenuButtonItem::Host => {
                // Open the name-entry popover; actual socket bind +
                // state transition happens after the user confirms
                // their name with Enter (see `capture_name_keys`).
                *net_mode = crate::multiplayer::NetMode::NamingForHost;
            }
            MenuButtonItem::Join => {
                // Same two-step flow as Host: name first, IP entry
                // second. `capture_name_keys` advances to
                // `JoiningEntry` on Enter.
                *net_mode = crate::multiplayer::NetMode::NamingForJoin;
            }
            _ => continue,
        }
        return;
    }
}

fn initial_label_for(item: MenuButtonItem) -> &'static str {
    match item {
        MenuButtonItem::Play       => "PLAY",
        MenuButtonItem::Host       => "HOST",
        MenuButtonItem::Join       => "JOIN",
        MenuButtonItem::Settings   => "SETTINGS",
        MenuButtonItem::Quit       => "QUIT",
        MenuButtonItem::Night      => "NIGHT",
        MenuButtonItem::Crt        => "CRT",
        MenuButtonItem::Vsync      => "VSYNC",
        MenuButtonItem::Bloom      => "BLOOM",
        MenuButtonItem::WindowMode => "WINDOW",
        MenuButtonItem::Resolution => "RES",
        MenuButtonItem::SfxVolume   => "SFX",
        MenuButtonItem::MusicVolume => "MUSIC",
        MenuButtonItem::Background  => "BG",
        MenuButtonItem::Back        => "BACK",
    }
}

fn on_off(v: bool) -> &'static str { if v { "ON" } else { "OFF" } }

// ---------- Menu state hooks (state-transition systems) ----------

/// Wipe queued XP + level-ups on every return to the menu.
fn reset_xp_on_main_menu(
    mut xp: ResMut<crate::xp::Xp>,
    mut pending: ResMut<crate::xp::LevelUpsPending>,
) {
    xp.reset();
    pending.0 = 0;
}

/// Clean the arena when the player returns to the main menu mid-run.
pub fn clear_arena_on_main_menu(
    mut commands: Commands,
    enemies: Query<Entity, With<crate::enemy::Enemy>>,
    bullets: Query<Entity, With<crate::bullet::Bullet>>,
    allies: Query<Entity, With<crate::ally::Ally>>,
) {
    for e in &enemies { commands.entity(e).despawn(); }
    for e in &bullets { commands.entity(e).despawn(); }
    for e in &allies  { commands.entity(e).despawn(); }
}

/// Set `ViewMode = Combat` once when entering MainMenu so PlayCamera
/// activates and the menu fleet renders. Boot default IS Combat
/// (per `ViewMode::default()`), but returning from a Map view would
/// leave `ViewMode::Map` and freeze our fleet's render. The check
/// keeps `view.is_changed()` quiet when the value is already right,
/// so we don't waste a write or trigger downstream change-detectors.
pub fn set_combat_view_for_menu(mut view: ResMut<crate::map::ViewMode>) {
    if *view != crate::map::ViewMode::Combat {
        *view = crate::map::ViewMode::Combat;
    }
}

/// Or-filter listing every in-combat HUD element we want hidden
/// while the landing page is showing. The hide / show systems below
/// share this so the two operations always address the same set.
type MenuChromeHidden = Or<(
    With<crate::xp::XpBarRoot>,
    With<crate::ui::ScoreText>,
    With<crate::ui::FpsText>,
    With<crate::ui::VsyncButton>,
    With<crate::ui::WaveHpUi>,
    With<crate::ui::AllyHpRow>,
    With<crate::ui::ReturnToMapButton>,
    With<crate::ui::CameraFollowButton>,
    With<crate::map::LevelStatusUi>,
)>;

/// One-shot hide of the in-combat HUD chrome on entry to MainMenu.
/// `update_wave_ui` and friends are state-gated to skip MainMenu
/// (registered with `run_if(not(in_state(MainMenu)))` in `main.rs`),
/// so nothing flips these back visible while the menu is up — a
/// single set-on-entry is sufficient.
pub fn hide_gameplay_chrome_for_menu(
    mut q: Query<&mut Visibility, MenuChromeHidden>,
) {
    for mut v in &mut q { *v = Visibility::Hidden; }
}

/// Restore HUD visibility on the way out of the menu so HullSelect
/// / Playing have their chrome back. Nothing else flips this back
/// to Inherited automatically (the HUD writers only react to
/// ViewMode *changes*), so we need an explicit one-shot here.
pub fn show_gameplay_chrome_after_menu(
    mut q: Query<&mut Visibility, MenuChromeHidden>,
) {
    for mut v in &mut q { *v = Visibility::Inherited; }
}

// ---------- Menu fleet: real in-game chassis behind the chrome ----------

/// Spawn the cosmetic skirmish behind the menu: one friendly pirate
/// hull on the upper band, one enemy boss-pirate hull on the lower
/// band, facing each other across the middle of the play render.
/// Their chunky-pixel art reads through the transparent gaps in the
/// menu chrome. The custom `tick_menu_ships` AI drives both because
/// the regular combat sim is gated off in `AppState::MainMenu`.
pub fn spawn_menu_fleet(
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    mut meshes: ResMut<Assets<Mesh>>,
    difficulty: Res<crate::Difficulty>,
    // Idempotent: skip if the fleet's already alive. Lets us call
    // this from BOTH `Startup` (for the initial MainMenu — Bevy 0.16
    // doesn't reliably fire `OnEnter(InitialState)` early enough for
    // PaletteMaterials to be visible) AND `OnEnter(MainMenu)` (for
    // re-entries after gameplay). On the second invocation in either
    // path, `existing` is non-empty and we no-op.
    existing: Query<Entity, With<Ally>>,
) {
    if !existing.is_empty() { return; }
    let (Some(pm), Some(em)) = (pm, em) else { return; };

    // Friendly pirate, upper band, drifting right. Heading -π/2 ⇒
    // the in-game spawn-helper orientation puts the bow on +X.
    spawn_ally(
        &mut commands, &pm, &em, &mut meshes,
        Vec2::new(-50.0, MENU_BAND_UPPER_Y),
        -std::f32::consts::FRAC_PI_2,
        ShipClass::PirateShip,
    );

    // Enemy boss pirate, lower band, drifting left (bow on -X via
    // heading +π/2). stars=1 + battles_cleared=0 = a base-tier boss;
    // we never check the boss's HP here so the numbers don't
    // matter for visuals.
    crate::ally::spawn_boss(
        &mut commands, &pm, &em, &mut meshes,
        Vec2::new(50.0, MENU_BAND_LOWER_Y),
        std::f32::consts::FRAC_PI_2,
        ShipClass::PirateShip,
        1,
        0,
        *difficulty,
    );
}

/// Despawn every menu-fleet entity on the way out of the menu.
/// Catches `Ally`-tagged hulls (both the friendly pirate and the
/// boss pirate carry it via `spawn_ally` / `spawn_boss`), not just
/// `MenuShip` — that protects against the corner case where the
/// player clicks PLAY before `tick_menu_ships` has had a chance to
/// stamp the menu marker on the freshly-spawned hulls.
pub fn despawn_menu_fleet(
    mut commands: Commands,
    ships: Query<Entity, With<Ally>>,
    bullets: Query<Entity, With<MenuBullet>>,
) {
    for e in &ships { commands.entity(e).despawn(); }
    for e in &bullets { commands.entity(e).despawn(); }
}

/// Drive each menu ship's Transform: linear drift + sine bob + wrap.
/// Stamps the `MenuShip` marker onto freshly-spawned `Ally` entities
/// on the first tick after spawn — `spawn_ally` doesn't return an
/// Entity, so we wait for Commands to flush and then tag whatever
/// Allies appear without a marker.
pub fn tick_menu_ships(
    time: Res<Time>,
    mut anim: ResMut<MainMenuAnim>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    untagged: Query<Entity, (With<Ally>, Without<MenuShip>)>,
    mut ships: Query<(Entity, &mut MenuShip, &mut Transform)>,
) {
    let dt = time.delta_secs();
    anim.elapsed += dt;
    let t = anim.elapsed;

    // First-tick tag pass: stamp `MenuShip` onto the freshly-spawned
    // hulls. Order MUST match `spawn_menu_fleet` (friendly first,
    // boss second). Both ships share the SAME `orbit_dir` so they
    // sweep the same angular direction around the midpoint — that's
    // what produces an actual orbit. Phases stagger by ship so the
    // two hulls don't burst in lockstep.
    if !untagged.is_empty() {
        // (faction, first_fire_offset_secs, initial_phase_duration)
        let fleet: [(MenuFaction, f32, f32); 2] = [
            (MenuFaction::Friendly, 1.0, 3.5),
            (MenuFaction::Enemy,    2.2, 5.0),
        ];
        for (idx, e) in untagged.iter().enumerate() {
            let i = idx.min(fleet.len() - 1);
            let (faction, first_fire, initial_orbit) = fleet[i];
            commands.entity(e).insert(MenuShip {
                faction,
                orbit_dir: 1.0,
                next_fire_at: t + first_fire,
                phase: MenuShipPhase::Orbit,
                phase_end_at: t + initial_orbit,
                last_pos: Vec2::ZERO,
            });
        }
    }

    // Snapshot positions AND estimated per-frame velocities so the
    // fire pass can lead the target (aim where it WILL be after the
    // bullet's flight). Velocity comes from `(current_pos -
    // last_pos) / dt`, with last_pos tracked on each MenuShip and
    // refreshed at the end of the movement pass.
    let dt_safe = dt.max(1e-6);
    let snapshots: Vec<(Entity, Vec2, Vec2)> = ships
        .iter()
        .map(|(e, ship, tf)| {
            let pos = tf.translation.truncate();
            let vel = if ship.last_pos == Vec2::ZERO {
                // First tick after spawn — last_pos hasn't been
                // populated yet, so don't claim a velocity. Lead
                // targeting falls back to "aim at current pos".
                Vec2::ZERO
            } else {
                (pos - ship.last_pos) / dt_safe
            };
            (e, pos, vel)
        })
        .collect();

    let mut rng = rand::thread_rng();

    // Movement pass with phase state machine. Each ship runs Orbit
    // (calm middle ground) interspersed with short Charge / Retreat
    // bursts to break up the predictable circle. A soft pull-toward-
    // origin leash keeps any phase from walking the engagement off-
    // screen. Hull rotation tracks current velocity so the silhouette
    // faces its direction of travel.
    for (e, mut ship, mut tf) in &mut ships {
        let pos = tf.translation.truncate();
        let target_pos = snapshots
            .iter()
            .find(|(other, _, _)| *other != e)
            .map(|(_, p, _)| *p);

        // Phase transition.
        if t >= ship.phase_end_at {
            let (next_phase, next_duration) = match ship.phase {
                MenuShipPhase::Orbit => {
                    // 50/50 charge vs retreat after each orbit slice.
                    let burst = if rng.gen_bool(0.5) {
                        MenuShipPhase::Charge
                    } else {
                        MenuShipPhase::Retreat
                    };
                    (burst, rng.gen_range(MENU_BURST_DURATION_MIN..MENU_BURST_DURATION_MAX))
                }
                _ => (
                    MenuShipPhase::Orbit,
                    rng.gen_range(MENU_ORBIT_DURATION_MIN..MENU_ORBIT_DURATION_MAX),
                ),
            };
            ship.phase = next_phase;
            ship.phase_end_at = t + next_duration;
        }

        // Velocity by phase.
        let velocity = match (target_pos, ship.phase) {
            (Some(tp), MenuShipPhase::Orbit) => {
                let to_target = tp - pos;
                let dist = to_target.length().max(0.5);
                let to_unit = to_target / dist;
                let tangent = Vec2::new(-to_unit.y, to_unit.x) * ship.orbit_dir;
                let range_error = (dist - MENU_ENGAGEMENT_RANGE) * MENU_RADIAL_PULL;
                tangent * MENU_ORBIT_SPEED + to_unit * range_error + leash_force(pos)
            }
            (Some(tp), MenuShipPhase::Charge) => {
                // Close in fast. No range-correction — we want the
                // overshoot so the next Orbit phase has to recover.
                let to_unit = (tp - pos).normalize_or_zero();
                to_unit * MENU_CHARGE_SPEED + leash_force(pos)
            }
            (Some(tp), MenuShipPhase::Retreat) => {
                // Peel away from target. Leash keeps the retreat from
                // walking off-screen — it'll get pulled back inward
                // once it's past the leash radius.
                let to_unit = (tp - pos).normalize_or_zero();
                -to_unit * MENU_RETREAT_SPEED + leash_force(pos)
            }
            (None, _) => Vec2::ZERO,
        };

        tf.translation.x += velocity.x * dt;
        tf.translation.y += velocity.y * dt;
        if velocity.length_squared() > 0.0001 {
            // spawn_ally's heading convention: 0 = bow on +Y. Match
            // that here so the hull's prow leads its motion.
            let heading = (-velocity.x).atan2(velocity.y);
            tf.rotation = Quat::from_rotation_z(heading);
        }

        // Refresh last_pos so the next frame's velocity estimate is
        // accurate. Without this, the fire pass would always read a
        // zero target velocity and lead-targeting would degrade to
        // "aim at current position".
        ship.last_pos = tf.translation.truncate();
    }

    // Firing pass: lead the target so bullets actually connect.
    // Estimated flight time = distance / bullet speed; predicted
    // target pos = target.pos + target.vel * flight_time. With
    // ships orbiting at ~22 u/s and bullets at 90 u/s, a 60-unit
    // shot has ~0.67s flight time → ~15 unit lead at the orbit
    // speed. Without leading, every bullet phases past the target.
    let (Some(pm), Some(em)) = (pm, em) else { return; };
    for (firer_e, mut ship, _) in &mut ships {
        if t < ship.next_fire_at { continue; }
        let firer_snap = snapshots.iter().find(|(e, _, _)| *e == firer_e);
        let target_snap = snapshots.iter().find(|(other, _, _)| *other != firer_e);
        let (Some((_, firer_pos, _)), Some((_, target_pos, target_vel))) =
            (firer_snap, target_snap) else { continue; };
        let separation = *target_pos - *firer_pos;
        let flight_time = (separation.length() / MENU_BULLET_SPEED).clamp(0.0, 2.0);
        let predicted = *target_pos + *target_vel * flight_time;
        let dir = (predicted - *firer_pos).normalize_or_zero();
        if dir == Vec2::ZERO { continue; }
        spawn_menu_bullet(&mut commands, &pm, &em, *firer_pos, dir, ship.faction);
        ship.next_fire_at = t + rng.gen_range(2.0..3.5);
    }
}

/// Soft pull-toward-origin force. Past `MENU_LEASH_RADIUS` from
/// world origin the returned vector points inward at strength
/// proportional to overshoot, so any phase that would otherwise walk
/// the engagement off-screen gets bent back inward. Inside the leash
/// it returns zero — the orbit runs unconstrained.
fn leash_force(pos: Vec2) -> Vec2 {
    let dist_from_origin = pos.length();
    if dist_from_origin > MENU_LEASH_RADIUS {
        let over = dist_from_origin - MENU_LEASH_RADIUS;
        -pos.normalize_or_zero() * over * MENU_LEASH_STRENGTH
    } else {
        Vec2::ZERO
    }
}

/// Spawn one cosmetic bullet on `PLAY_LAYER` (same render path as
/// combat fire) so the round reads as the same chunky cannonball the
/// player will see in-game. Faction picks the bullet colour pair —
/// yellow for the friendly pirate, red for the enemy boss — same
/// grammar as in-combat fire.
fn spawn_menu_bullet(
    commands: &mut Commands,
    pm: &PaletteMaterials,
    em: &EffectMeshes,
    pos: Vec2,
    dir: Vec2,
    faction: MenuFaction,
) {
    let v = dir * MENU_BULLET_SPEED;
    let lifetime = 3.0;

    let (outer_mat, inner_mat) = match faction {
        MenuFaction::Friendly => (pm.bullet_friendly_outer.clone(), pm.bullet_friendly.clone()),
        MenuFaction::Enemy    => (pm.bullet_enemy_outer.clone(),    pm.bullet_enemy.clone()),
    };

    let bullet = commands.spawn((
        Mesh2d(em.bullet_friendly_outer.clone()),
        MeshMaterial2d(outer_mat),
        Transform::from_xyz(pos.x, pos.y, 4.0)
            .with_rotation(Quat::from_rotation_z((-dir.x).atan2(dir.y))),
        Velocity(v),
        MenuBullet {
            velocity: v,
            lifetime,
            // 0.15s of "fly through hulls" so the round clears its
            // own firer before proximity-hit checks engage. At
            // MENU_BULLET_SPEED (90 u/s) that's ~13 world units of
            // travel — past the firer's hull by a comfortable margin.
            arm_remaining: 0.15,
        },
        RenderLayers::layer(PLAY_LAYER),
    )).id();

    let inner = commands.spawn((
        Mesh2d(em.bullet_friendly_inner.clone()),
        MeshMaterial2d(inner_mat),
        Transform::from_xyz(0.0, 0.0, 0.05),
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    commands.entity(inner).insert(ChildOf(bullet));
}

/// Advance each MenuBullet along its velocity, despawn at lifetime,
/// AND cosmetically "hit" the first menu-fleet hull the bullet comes
/// near once its arming delay has elapsed. No real damage / kill
/// events / pending-damage-queue churn — just a particle burst and
/// despawn so the cannonade reads as connecting instead of phasing
/// through the target.
pub fn tick_menu_bullets(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    mut bullets: Query<(Entity, &mut MenuBullet, &mut Transform), Without<MenuShip>>,
    ships: Query<&Transform, With<MenuShip>>,
) {
    let dt = time.delta_secs();
    // Cosmetic hit radius in world units. Slightly larger than the
    // pirate hull's half-length so glancing passes still register —
    // the bullet has no homing, so a tight radius would make most
    // shots phase straight through during ship drift.
    const MENU_HIT_RADIUS: f32 = 7.0;
    let r2 = MENU_HIT_RADIUS * MENU_HIT_RADIUS;
    let mut rng = rand::thread_rng();
    for (e, mut bullet, mut tf) in &mut bullets {
        bullet.lifetime -= dt;
        if bullet.lifetime <= 0.0 {
            commands.entity(e).despawn();
            continue;
        }
        bullet.arm_remaining = (bullet.arm_remaining - dt).max(0.0);
        tf.translation.x += bullet.velocity.x * dt;
        tf.translation.y += bullet.velocity.y * dt;

        // Skip hit checks during the arming window so the bullet
        // can clear its own firer's hull.
        if bullet.arm_remaining > 0.0 { continue; }

        let bp = tf.translation.truncate();
        let mut hit = false;
        for ship_tf in &ships {
            let sp = ship_tf.translation.truncate();
            if bp.distance_squared(sp) < r2 {
                hit = true;
                break;
            }
        }
        if hit {
            if let (Some(pm), Some(em)) = (pm.as_ref(), em.as_ref()) {
                // Six small red sparks — same `bleed` material the
                // combat shark uses on its first contact, so the
                // cosmetic hit reads in the same visual language.
                crate::effects::spawn_hit_particles(
                    &mut commands, em, &pm.bleed, bp, 6, 50.0, &mut rng,
                );
            }
            commands.entity(e).despawn();
        }
    }
}

// ---------- Shared SettingsItem infrastructure (pause-menu uses these) ----------
//
// The pause-menu's settings panel is still bevy_ui (see `pause.rs`), so
// these types + handlers stay around to support its existing
// `SettingsItem`-tagged buttons. The main menu's settings sub-page uses
// chunky `MenuButtonItem` buttons routed through `handle_menu_click`
// above; the two systems coexist because each only sees buttons tagged
// with its own marker.

/// Tag on each bevy_ui settings-button (currently used by the pause
/// menu). Drives both the click handler (toggle the matching mode) and
/// the per-frame label updater (show ON/OFF or current value).
#[derive(Component, Clone, Copy)]
pub enum SettingsItem {
    Night,
    Crt,
    Vsync,
    Bloom,
    WindowMode,
    Resolution,
    SfxVolume,
    MusicVolume,
    Background,
}

#[derive(Component)]
pub struct SettingsItemLabel(pub SettingsItem);

/// Click router for bevy_ui settings buttons (pause menu). Boolean
/// toggles flip; `WindowMode` / `Resolution` cycle through their
/// preset lists; `BACK` is a no-op here (the pause menu handles its
/// own dismiss elsewhere).
pub fn handle_settings_item_click(
    interactions: Query<(&Interaction, &SettingsItem), Changed<Interaction>>,
    mut night: ResMut<NightMode>,
    mut crt: ResMut<CrtMode>,
    mut vsync: ResMut<VsyncMode>,
    mut bloom: ResMut<crate::modes::BloomMode>,
    mut win_mode: ResMut<crate::modes::WindowModeSetting>,
    mut res: ResMut<crate::modes::ResolutionSetting>,
    mut sfx_vol: ResMut<crate::sfx::SfxVolume>,
    mut music_vol: ResMut<crate::sfx::MusicVolume>,
    mut bg: ResMut<crate::modes::BackgroundSetting>,
) {
    for (interaction, item) in &interactions {
        if !matches!(*interaction, Interaction::Pressed) { continue; }
        match *item {
            SettingsItem::Night       => night.active = !night.active,
            SettingsItem::Crt         => crt.active = !crt.active,
            SettingsItem::Vsync       => vsync.enabled = !vsync.enabled,
            SettingsItem::Bloom       => bloom.active = !bloom.active,
            SettingsItem::WindowMode  => win_mode.mode = win_mode.mode.cycle(),
            SettingsItem::Resolution  => res.res = res.res.cycle(),
            SettingsItem::SfxVolume   => *sfx_vol = sfx_vol.cycle(),
            SettingsItem::MusicVolume => *music_vol = music_vol.cycle(),
            SettingsItem::Background  => bg.kind = bg.kind.cycle(),
        }
    }
}

/// Tactile feedback for bevy_ui settings-button presses. Split from
/// the click router because the router takes `ResMut<SfxVolume>` for
/// the SFX cycle, which conflicts with `SfxPlayer`'s `Res<SfxVolume>`
/// at Bevy's system-param check. Ordered .after() the router so the
/// new volume is in effect when the Switch sound plays.
pub fn play_settings_click_sound(
    interactions: Query<&Interaction, (Changed<Interaction>, With<SettingsItem>)>,
    mut sfx: crate::sfx::SfxPlayer,
) {
    for interaction in &interactions {
        if matches!(*interaction, Interaction::Pressed) {
            sfx.play(crate::sfx::Sfx::Switch);
        }
    }
}

/// Rewrites each bevy_ui settings-button label with the live mode
/// state so the player can see what's on without trial-and-clicking.
pub fn update_settings_labels(
    night: Res<NightMode>,
    crt: Res<CrtMode>,
    vsync: Res<VsyncMode>,
    bloom: Res<crate::modes::BloomMode>,
    win_mode: Res<crate::modes::WindowModeSetting>,
    res: Res<crate::modes::ResolutionSetting>,
    sfx_vol: Res<crate::sfx::SfxVolume>,
    music_vol: Res<crate::sfx::MusicVolume>,
    bg: Res<crate::modes::BackgroundSetting>,
    mut q: Query<(&SettingsItemLabel, &mut Text)>,
) {
    for (label, mut text) in &mut q {
        let s = match label.0 {
            SettingsItem::Night       => format!("NIGHT: {}", on_off(night.active)),
            SettingsItem::Crt         => format!("CRT: {}",   on_off(crt.active)),
            SettingsItem::Vsync       => format!("VSYNC: {}", on_off(vsync.enabled)),
            SettingsItem::Bloom       => format!("BLOOM: {}", on_off(bloom.active)),
            SettingsItem::WindowMode  => format!("WINDOW: {}", win_mode.mode.label()),
            SettingsItem::Resolution  => format!("RES: {}",    res.res.label()),
            SettingsItem::SfxVolume   => format!("SFX: {}",    sfx_vol.label()),
            SettingsItem::MusicVolume => format!("MUSIC: {}",  music_vol.label()),
            SettingsItem::Background  => format!("BG: {}",     bg.kind.label()),
        };
        if text.0 != s { text.0 = s; }
    }
}

// ---------- Tests ----------

#[cfg(test)]
mod tests {
    //! Headless tests for the menu-fleet spawn pipeline.
    //!
    //! These don't exercise the render pipeline (no `bevy_render`
    //! plugin), so we can't check the camera / display sprite. What
    //! we *can* check is the ECS state produced by `spawn_menu_fleet`
    //! and `despawn_menu_fleet`: how many hulls land in the world,
    //! which factions they carry, and that the cleanup hook clears
    //! them all. That's the load-bearing contract — if those entities
    //! exist with the right tags, the play-camera renders them.
    //!
    //! Test scaffolding is minimal because `spawn_menu_fleet`'s
    //! dependency surface is small: `PaletteMaterials`,
    //! `EffectMeshes`, `Assets<Mesh>`, `Difficulty`. We build them by
    //! hand here rather than calling the real `setup_world` (which
    //! drags in a renderer's worth of side-effects).
    use super::*;
    use bevy::ecs::system::RunSystemOnce;
    use crate::effects::EffectMeshes;
    use crate::enemy::Enemy;
    use crate::palette::{Palette, PaletteMaterials};
    use crate::Difficulty;

    /// Build every `EffectMeshes` field from a single placeholder mesh.
    /// The test never rasterises anything, so the actual geometry
    /// doesn't matter — only that the handles are non-default so
    /// `spawn_ally` / `spawn_boss` don't panic on a stale lookup.
    fn stub_effect_meshes(meshes: &mut Assets<Mesh>) -> EffectMeshes {
        let m: Handle<Mesh> = meshes.add(Rectangle::new(1.0, 1.0));
        EffectMeshes {
            muzzle_flash:          m.clone(),
            particle:              m.clone(),
            bullet_friendly_outer: m.clone(),
            bullet_friendly_inner: m.clone(),
            bullet_round_outer:    m.clone(),
            bullet_round_inner:    m.clone(),
            bullet_enemy_outer:    m.clone(),
            bullet_enemy_inner:    m.clone(),
            enemy_body:            m.clone(),
            enemy_turret_base:     m.clone(),
            enemy_turret_barrel:   m.clone(),
            bomber_warhead:        m.clone(),
            ally_turret_base:      m.clone(),
            ally_turret_barrel:    m.clone(),
            bullet_plane_outer:    m.clone(),
            bullet_plane_inner:    m.clone(),
            bullet_missile_outer:  m.clone(),
            bullet_missile_inner:  m.clone(),
            mine_outer:            m.clone(),
            mine_inner:            m.clone(),
            boarder_dot:           m.clone(),
            beam:                  m,
        }
    }

    /// Headless App with just enough resources for `spawn_menu_fleet`
    /// to run. `MinimalPlugins` skips the renderer + audio; we layer
    /// on `AssetPlugin` so `Assets<T>` storage works and `init_asset`
    /// can register Mesh + ColorMaterial. Real `PaletteMaterials::build`
    /// is used so the spawned hulls get honest material handles.
    fn test_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(bevy::asset::AssetPlugin::default());
        app.init_asset::<Mesh>();
        app.init_asset::<ColorMaterial>();

        let palette = Palette::aap64_naval();
        {
            let world = app.world_mut();
            let pm = {
                let mut materials = world.resource_mut::<Assets<ColorMaterial>>();
                PaletteMaterials::build(&palette, &mut materials)
            };
            world.insert_resource(pm);
        }
        {
            let world = app.world_mut();
            let em = {
                let mut meshes = world.resource_mut::<Assets<Mesh>>();
                stub_effect_meshes(&mut meshes)
            };
            world.insert_resource(em);
        }
        app.insert_resource(palette);
        app.insert_resource(Difficulty::default());
        app
    }

    /// Count helpers expressed as one-shot systems — cleaner than
    /// wrestling Bevy's QueryState lifetimes by hand at the test site.
    fn count_allies(q: Query<&Ally>) -> usize { q.iter().count() }
    fn count_boss_pirates(q: Query<Entity, (With<Ally>, With<Enemy>)>) -> usize {
        q.iter().count()
    }

    #[test]
    fn spawn_menu_fleet_creates_two_pirate_hulls() {
        let mut app = test_app();
        app.world_mut().run_system_once(spawn_menu_fleet).unwrap();
        let count = app.world_mut().run_system_once(count_allies).unwrap();
        assert_eq!(
            count, 2,
            "menu fleet should spawn exactly two pirate hulls (friendly + boss)",
        );
    }

    #[test]
    fn spawn_menu_fleet_marks_one_pirate_as_an_enemy_boss() {
        // `spawn_ally` adds only the Ally tag; `spawn_boss` adds Ally
        // + Enemy. So exactly one of the two pirates should also
        // carry Enemy — that's what gives the boss its red-hull
        // materials.
        let mut app = test_app();
        app.world_mut().run_system_once(spawn_menu_fleet).unwrap();
        let bosses = app.world_mut().run_system_once(count_boss_pirates).unwrap();
        assert_eq!(
            bosses, 1,
            "exactly one menu-fleet hull should carry the Enemy tag (the boss)",
        );
    }

    #[test]
    fn despawn_menu_fleet_clears_every_hull() {
        // The OnExit cleanup must catch BOTH the friendly pirate (Ally
        // only) and the boss pirate (Ally + Enemy). If either survives
        // into HullSelect / Playing, it would tangle the real combat
        // sim. The despawn query targets `With<Ally>` for this reason.
        let mut app = test_app();
        app.world_mut().run_system_once(spawn_menu_fleet).unwrap();
        app.world_mut().run_system_once(despawn_menu_fleet).unwrap();
        let remaining = app.world_mut().run_system_once(count_allies).unwrap();
        assert_eq!(
            remaining, 0,
            "every menu-fleet pirate should be despawned on OnExit(MainMenu)",
        );
    }
}
