//! Ship Game — Bevy app entry point.
//!
//! All gameplay logic lives in domain modules; this file only declares them
//! and wires up the App (resources + system schedule). Module map:
//!
//! - `balance`     — gameplay tunables (HP, damage, ranges, layout dims)
//! - `i18n`        — translation lookup (`tr()`) backed by `data/translations.csv`
//! - `palette`     — colors, material handles, palette presets, `apply_palette`
//! - `components`  — small generic ECS components (Health, Velocity, …)
//! - `effects`     — HitFx, particles, muzzle flashes, cached effect meshes
//! - `trails`      — friendly + enemy ribbon trails
//! - `weapon`      — `WeaponType` + per-weapon stats + material lookups
//! - `bullet`      — projectile component + travel + collisions
//! - `beam`        — railgun beam + line-segment damage resolution
//! - `enemy`       — variants + spawn + AI + fire + bomber detonation
//! - `turret`      — turret slots + barrels + aim/fire dispatch
//! - `ship`        — friendly hull setup + movement + `approach_angle`
//! - `pier`        — port upgrades: buildings + adjacency + drafting UI
//! - `wave`        — wave-mode state machine + arena cleanup
//! - `modes`       — Game / Window / Night / CRT mode toggles
//! - `rendering`   — pixel-perfect render pipeline (cameras + upscale + scanline)
//! - `ui`          — score banner, HP bar, LHS turret panel, draft cards

use bevy::diagnostic::FrameTimeDiagnosticsPlugin;
use bevy::prelude::*;

mod ally;
mod balance;
mod beam;
mod blade;
mod booster;
mod bullet;
mod cannon;
mod components;
mod customize;
mod effects;
mod enemy;
mod game_over;
mod hull;
mod i18n;
mod map;
mod modes;
mod palette;
mod pause;
mod rendering;
mod main_menu;
mod settings;
mod rune;
mod ship;
mod stage_complete;
mod stats;
mod trails;
mod turret;
mod ui;
mod ui_kit;
mod weapon;
mod xp;

use ally::{
    ally_ai, ally_death_check, ally_turret_aim_fire, boarder_tick,
    boarding_launcher_fire, boss_viking_ai, flash_mine_dots, homing_missile_track,
    mine_layer_drop, mine_tick, missile_launcher_fire,
    oil_slick_burn_tick, oil_slick_grow_tick, oil_tanker_cycle, plane_ai,
    tender_heal_beam, update_boarding_ropes, viking_ram_damage,
};
use customize::{
    complete_drag, handle_close_click, handle_reroll_button, init_customize_shop,
    resize_customize_display, setup_customize_render, setup_customize_ui, start_drag,
    handle_shop_mod_click, handle_stat_debug_buttons, sync_customize_text, sync_stats_panel,
    toggle_customize_render, track_customize_cursor, update_shop_mod_cards,
    update_customize_ship, update_customize_shop, update_customize_tooltip,
    update_customize_ui, update_drag_ghost, update_sell_label, CustomizeOpen, DragState,
};
use balance::{WINDOW_H, WINDOW_W};
use beam::{beam_apply_damage, update_beams};
use bullet::{bullet_collisions, bullet_update};
use effects::{
    apply_hit_fx_visuals, tick_hit_fx, update_hit_particles, update_muzzle_flashes,
};
use enemy::{
    bomber_detonate, clear_spawn_indicators, enemy_ai, enemy_death_check, enemy_fire,
    setup_enemy_hp_bar_assets, setup_spawn_indicator_assets, spawn_enemies,
    tick_spawn_indicators, track_enemy_damage_for_hp_bars, update_enemy_hp_bars,
};
use map::{
    advance_map_anim_timeline, apply_view_mode, boss_patrol_movement,
    close_popup_on_view_change, handle_building_choice_clicks, handle_debug_buttons,
    in_combat_view, level_complete_check, level_fail_check, map_begin_phase,
    map_boat_movement, map_click_input, refresh_map_fill, setup_currency_ui,
    setup_debug_ui, setup_level_status_ui, setup_map, setup_progress_assets,
    spawn_boss_patrols,
    sync_debug_panel_visibility, sync_owned_slot_visuals, tick_buildings,
    toggle_debug_ui_on_hash, update_anim_beams, update_anim_pulses,
    update_building_button_tints, update_building_description, update_building_hover_tooltip,
    update_building_progress_bars, update_claim_label, update_currency_ui,
    update_debug_button_tints, update_level_status_ui, update_map_slot_labels,
    update_refined_steel_text, update_scrap_text, update_steel_text,
    BuildingTimers, CombatContext, DebugClaimMode, DebugUiVisible, MapAnimTimeline,
    MapState, TriggerMapPhase, ViewMode,
};
use modes::{
    apply_camera_follow, apply_crt_mode, apply_night_mode, apply_vsync_mode,
    apply_window_mode, handle_desktop_drag_resize, handle_desktop_escape,
    CameraFollow, CrtMode, GameMode, NightMode, VsyncMode, WindowMode,
};
use palette::{apply_palette, Palette};
use pause::{
    handle_main_menu_click as handle_pause_main_menu_click, handle_quit_click,
    handle_resume_click, setup_pause_menu, sync_pause_menu_visibility,
    toggle_pause_on_esc, Paused,
};
use rendering::{
    resize_upscale_sprite, setup_render, update_hash_image, update_hud_camera_viewport,
};
use settings::{apply_loaded_settings, persist_settings_on_change};
use rune::{tick_echoes, tick_on_conduit, tick_on_fire, tick_on_frost, tick_on_resonate};
use ship::{apply_velocity, friendly_movement, friendly_ram_damage, setup_world};
use trails::{update_enemy_trails, update_trail, ShipPath};
use turret::{
    helicopter_ai, mortar_shell_tick, sync_helipad_helicopters, sync_helipad_nose_barrels,
    sync_turret_config, turret_aim_fire, SlotCfg, TurretConfig,
};
use ui::{
    force_hide_ui_panel, reset_damage_stats, setup_damage_panel, setup_ui,
    setup_wave_indicator, sync_ally_hp_bars, sync_damage_panel_visibility,
    ui_button_system, update_ally_hp_values, update_damage_bars, update_damage_panel,
    update_damage_row_icons, update_fps_text, update_hp_bar_pixel_scale,
    update_hp_subdividers, update_map_button,
    update_score_text, update_slot_labels, update_vsync_label, update_wave_indicator,
    update_wave_ui, DamageStats,
};
use weapon::WeaponType;

// ---------- Top-level screen state ----------
//
// One enum, one source of truth for "which screen is the player on".
// Game-sim systems gate on `AppState::Playing` so they idle during the
// main menu, the customize/shop overlay, and the pause menu — that's
// what makes the menus actually pause gameplay rather than just cover
// it. Existing `MainMenuOpen` / `CustomizeOpen` / `Paused` resources
// are still around (lots of UI systems read them); the bridge below
// drives them from the state each frame.

#[derive(States, Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum AppState {
    #[default]
    MainMenu,
    /// Active combat in a section. The combat-sim run-conditions all
    /// gate on this state, so anything else (Map / Customize / Paused
    /// / etc.) freezes gameplay.
    Playing,
    /// 5-second "STAGE COMPLETE" beat between a cleared level and the
    /// shop opening. Combat-sim systems idle the same way they do in
    /// `Customize` / `Paused`.
    StageComplete,
    /// XP-driven level-up screen. Sits between StageComplete and
    /// Customize when the player has unspent levels in the queue.
    /// Each pick decrements `LevelUpsPending`; the screen re-enters
    /// itself (re-rolling buffs) until the queue drains, then moves
    /// on to `Customize`.
    LevelUp,
    /// Hull selection — sits between MainMenu and Playing. PLAY
    /// click on the main menu lands here; clicking a hull applies
    /// its stat modifiers and transitions to Playing. Game-over
    /// RESTART re-applies the same hull without re-prompting; only
    /// returning to MainMenu re-shows this screen on the next PLAY.
    HullSelect,
    Customize,
    /// Between-stage map screen. Player navigates the boat to an
    /// unowned section to start the next combat. Reached by closing
    /// the shop; left when the boat crosses into a section, which
    /// transitions to `Playing`.
    Map,
    Paused,
    /// Player died — shows a transparent overlay with RESTART / MAIN
    /// MENU / QUIT controls. The dead ship + frozen arena read through
    /// the backdrop. Combat sim idles (gated on `Playing`).
    GameOver,
}

/// One-way mirror: write the existing boolean overlay flags from the
/// authoritative `AppState`. Click handlers and ESC toggle now call
/// `NextState::set`, and this system propagates the resulting transition
/// to every UI system that still reads the boolean resources.
fn sync_state_to_open_resources(
    state: Res<State<AppState>>,
    mut menu: ResMut<main_menu::MainMenuOpen>,
    mut customize: ResMut<customize::CustomizeOpen>,
    mut paused: ResMut<pause::Paused>,
) {
    let s = *state.get();
    let menu_want = matches!(s, AppState::MainMenu);
    let customize_want = matches!(s, AppState::Customize);
    let paused_want = matches!(s, AppState::Paused);
    if menu.0 != menu_want { menu.0 = menu_want; }
    if customize.open != customize_want { customize.open = customize_want; }
    if paused.0 != paused_want { paused.0 = paused_want; }
}

/// `OnEnter(Map)` — flip the ViewMode to Map so the cameras swap to the
/// map view. ViewMode used to be set imperatively in click handlers and
/// `map_boat_movement`; with Map now its own `AppState`, the state
/// transition is the single source of truth and ViewMode follows it.
fn enter_map_view(mut view: ResMut<map::ViewMode>) {
    if *view != map::ViewMode::Map { *view = map::ViewMode::Map; }
}

/// `OnEnter(Playing)` — flip ViewMode back to Combat. Covers every path
/// into combat (boot's MainMenu→Playing, Map→Playing on section click,
/// GameOver→Playing on RESTART, Paused→Playing on resume).
fn enter_combat_view(mut view: ResMut<map::ViewMode>) {
    if *view != map::ViewMode::Combat { *view = map::ViewMode::Combat; }
}

/// `OnExit(Map)` — running this set on the Map→Playing transition is
/// the canonical "stage starting" hook. Refills the friendly hull to
/// max and despawns lingering bullets / mines / oil slicks / particles
/// from the previous stage so each new combat starts clean.
///
/// Doesn't fire on Paused→Playing or GameOver→Playing — those don't
/// pass through `Map` — so a mid-fight resume keeps the player's HP
/// and a death-restart already gets a full reset via
/// `game_over::reset_run_for_restart`.
/// `OnEnter(MainMenu)` — reset XP + queued level-ups so a fresh PLAY
/// session starts at LV 1 / 0 XP. RESTART from the game-over screen
/// also resets via `reset_run_for_restart`; this covers the
/// quit-to-menu path.
fn reset_xp_for_main_menu(
    mut xp: ResMut<xp::Xp>,
    mut pending: ResMut<xp::LevelUpsPending>,
) {
    xp.reset();
    pending.0 = 0;
}

fn refill_and_clean_for_next_stage(
    stats: Res<stats::PlayerStats>,
    mut friendly: Query<&mut components::Health, With<components::Friendly>>,
    arena: Query<
        Entity,
        Or<(
            With<enemy::Enemy>,
            With<trails::EnemyTrail>,
            With<bullet::Bullet>,
            With<beam::Beam>,
            With<effects::MuzzleFlash>,
            With<effects::HitParticle>,
        )>,
    >,
    mut commands: Commands,
) {
    if let Ok(mut h) = friendly.single_mut() {
        h.0 = stats.max_hp();
    }
    for e in &arena {
        commands.entity(e).despawn();
    }
}

// ---------- Cross-cutting resources ----------
//
// Score and SpawnTimer live here because they're touched by multiple domains
// (bullet/beam credit kills; enemy reads SpawnTimer; UI reads Score). They're
// trivial wrappers — moving them to their own module would be more friction
// than it's worth.

#[derive(Resource)]
pub struct Score(pub u32);

/// Cumulative campaign progress — number of map sections the player
/// has cleared so far. Read by `level_enemy_budget` to scale every
/// new level harder than the last, so a 2★ fought after 5 wins is
/// noticeably tougher than a 2★ picked first.
#[derive(Resource, Default)]
pub struct CampaignProgress {
    pub battles_cleared: u32,
}

/// Currency dropped by killed enemies (+1 per kill). Spent on map-view
/// building placement and as the input resource for the Foundry. Default 0.
#[derive(Resource, Default)]
pub struct Scrap(pub u32);

/// Refined currency produced by Foundries (1 steel per cycle, see
/// `FOUNDRY_INTERVAL`). Consumed by Cranes to maintain their adjacency
/// speed boost. Default 0.
#[derive(Resource, Default)]
pub struct Steel(pub u32);

/// Top-tier refined output. Produced by Refineries (1 refined steel per
/// cycle in exchange for `REFINERY_INPUT` steel, see `REFINERY_INTERVAL`).
#[derive(Resource, Default)]
pub struct RefinedSteel(pub u32);

#[derive(Resource)]
pub struct SpawnTimer { pub t: f32, pub elapsed: f32 }

/// Wall-clock seconds since the current run started (PLAY → HullSelect
/// → Playing). Ticks while the player is *in* a run; pauses on the
/// MainMenu and HullSelect screens. Reset on `OnEnter(HullSelect)` so
/// each new run starts from `00:00`.
#[derive(Resource, Default)]
pub struct RunTimer { pub secs: f32 }

fn reset_run_timer(mut timer: ResMut<RunTimer>) {
    timer.secs = 0.0;
}

fn tick_run_timer(
    time: Res<Time>,
    state: Res<State<AppState>>,
    mut timer: ResMut<RunTimer>,
) {
    let s = *state.get();
    let counts = !matches!(s, AppState::MainMenu | AppState::HullSelect);
    if counts {
        timer.secs += time.delta_secs();
    }
}

fn main() {
    // Starting loadout lives in `TurretConfig::default()` so every
    // reset path (MainMenu, GameOver RESTART, etc.) re-derives the
    // same baseline.
    let cfg = TurretConfig::default();

    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Ship Game".into(),
                resolution: (WINDOW_W, WINDOW_H).into(),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(FrameTimeDiagnosticsPlugin::default())
        // Per-weapon plugins for the new tag-flavour weapons. Each
        // owns its own decoration entities + tick systems so the
        // turret-fire dispatcher stays small. Added here near the
        // top so their Startup/Update systems get sequenced with the
        // rest of the schedule below.
        .add_plugins((cannon::CannonPlugin, booster::BoosterPlugin, blade::BladePlugin))
        .insert_resource(ClearColor(Color::srgb(0.05, 0.05, 0.08)))
        .insert_resource(Score(0))
        .insert_resource(CampaignProgress::default())
        // Starter purse — enough to build one tier-1 (10) plus a small
        // buffer so the first map decision isn't a forced no-op.
        .insert_resource(Scrap(15))
        .insert_resource(Steel::default())
        .insert_resource(RefinedSteel::default())
        .insert_resource(SpawnTimer { t: 0.0, elapsed: 0.0 })
        .insert_resource(cfg)
        .insert_resource(DamageStats::default())
        .insert_resource(stats::PlayerStats::default())
        .insert_resource(main_menu::MainMenuOpen::default())
        .insert_resource(main_menu::MainMenuView::default())
        .insert_resource(DebugUiVisible::default())
        .insert_resource(stage_complete::StageCompleteTimer::default())
        .insert_resource(xp::Xp::default())
        .insert_resource(xp::LevelUpsPending::default())
        .insert_resource(xp::LevelUpReturn::default())
        .insert_resource(xp::LevelUpChoices::default())
        .insert_resource(hull::SelectedHull::default())
        .insert_resource(modes::ScreenShake::default())
        .insert_resource(RunTimer::default())
        .init_state::<AppState>()
        .insert_resource(Palette::aap64_naval())
        .insert_resource(ShipPath::default())
        .insert_resource(WindowMode::default())
        .insert_resource(NightMode::default())
        .insert_resource(CrtMode::default())
        .insert_resource(VsyncMode::default())
        .insert_resource(GameMode::default())
        .insert_resource(CameraFollow::default())
        .insert_resource(ViewMode::default())
        .insert_resource(MapState::new())
        .insert_resource(BuildingTimers::default())
        .insert_resource(MapAnimTimeline::default())
        .insert_resource(CombatContext::default())
        .insert_resource(DebugClaimMode::default())
        .add_event::<TriggerMapPhase>()
        .insert_resource(CustomizeOpen::default())
        .insert_resource(DragState::default())
        .insert_resource(Paused::default())
        .add_systems(Startup, (
            setup_render, setup_world, setup_ui, setup_map,
            // Spawn boss patrol icons after `setup_map` has populated
            // `MapState.sections` so 5★ section polygons are available
            // when `spawn_boss_patrols` reject-samples patrol targets.
            spawn_boss_patrols,
            setup_debug_ui, setup_currency_ui, setup_progress_assets,
            setup_level_status_ui, setup_enemy_hp_bar_assets,
            init_customize_shop, setup_customize_render, setup_customize_ui,
            setup_pause_menu, main_menu::setup_main_menu, setup_damage_panel,
            setup_wave_indicator, setup_spawn_indicator_assets,
        ).chain())
        // State→resources bridge. Runs first so every other system in
        // the same Update sees the freshly-synced flags.
        .add_systems(Update, sync_state_to_open_resources)
        // Auto-reroll the shop every time the customize overlay
        // opens — fresh turrets/runes/mods on each visit. The
        // Startup init still seeds it once so any pre-first-open
        // queries see a populated resource.
        .add_systems(OnEnter(AppState::Customize), init_customize_shop)
        // Wipe per-round damage tallies whenever a new combat is
        // about to start. Combat → Customize keeps last round's
        // bars visible during the shop; closing the shop fires
        // OnExit(Customize) and zeros the slate. PLAY-from-menu
        // also resets via OnExit(MainMenu).
        .add_systems(OnExit(AppState::MainMenu), (reset_damage_stats, clear_spawn_indicators))
        // Returning to the main menu mid-run leaves a frozen battlefield
        // behind it. Despawn enemies/bullets/allies on entry so PLAY
        // starts on an empty stage.
        .add_systems(OnEnter(AppState::MainMenu), main_menu::clear_arena_on_main_menu)
        .add_systems(OnExit(AppState::Customize), reset_damage_stats)
        .add_systems(OnEnter(AppState::Customize), clear_spawn_indicators)
        // ViewMode follows AppState. The OnEnter hooks are the single
        // source of truth for camera/view swaps; old call sites that
        // imperatively set `*view = ViewMode::*` have moved to setting
        // the state instead.
        .add_systems(OnEnter(AppState::Map), enter_map_view)
        .add_systems(OnEnter(AppState::Playing), enter_combat_view)
        // Stage-start hook: HP refill + arena cleanup on the Map→Playing
        // transition. Keyed off `OnExit(Map)` so it doesn't accidentally
        // fire on Paused→Playing or GameOver→Playing.
        .add_systems(OnExit(AppState::Map), refill_and_clean_for_next_stage)
        // Game over: spawn the transparent end-screen overlay on entry,
        // despawn it + run the full fresh-run reset on exit. Both the
        // RESTART and MAIN MENU click paths leave through OnExit, so
        // the reset runs once for either choice.
        // Hull selection: full-screen overlay between MainMenu and
        // Playing. PLAY click on the main menu sets state to
        // HullSelect; clicking a card applies stats + flips to
        // Playing.
        .add_systems(OnEnter(AppState::HullSelect), (hull::enter_hull_select, reset_run_timer))
        .add_systems(OnExit(AppState::HullSelect), hull::exit_hull_select)
        .add_systems(OnEnter(AppState::GameOver), game_over::enter_game_over)
        .add_systems(
            OnExit(AppState::GameOver),
            (game_over::exit_game_over, game_over::reset_run_for_restart),
        )
        // Level-up overlay: spawn the buff cards on entry, despawn on
        // exit. Click handler runs while the state is active.
        .add_systems(OnEnter(AppState::LevelUp), xp::enter_level_up)
        .add_systems(OnExit(AppState::LevelUp), xp::exit_level_up)
        .add_systems(
            Update,
            xp::handle_level_up_click.run_if(in_state(AppState::LevelUp)),
        )
        .add_systems(
            Update,
            (
                hull::handle_card_click,
                hull::handle_play_click,
                hull::handle_back_click,
                hull::handle_back_on_esc,
                hull::sync_hull_select_on_change,
                hull::sync_hull_apply,
            ).run_if(in_state(AppState::HullSelect)),
        )
        // Returning to the main menu fully restarts the run — any
        // path that lands on MainMenu (pause→menu, game-over→menu,
        // hull-select BACK) wipes stats / scrap / campaign / turret
        // config / map ownership / friendly HP via the same hook
        // RESTART uses. Coupled with the XP reset, a fresh PLAY
        // from the menu always begins from a clean baseline.
        .add_systems(
            OnEnter(AppState::MainMenu),
            (reset_xp_for_main_menu, game_over::reset_run_for_restart),
        )
        // Global safety net: clamp the friendly's `Health.0` to
        // `stats.max_hp()` every frame so a stale stat-vs-HP mismatch
        // (e.g. picking Glass Cannon's -50 HP after the ship spawned
        // with 100) never paints a "100/50" readout on the bar.
        .add_systems(Update, (hull::clamp_hp_to_max, tick_run_timer))
        // Stage-complete buffer: spawn the overlay on entry, despawn
        // on exit, tick the timer while the state is active.
        .add_systems(OnEnter(AppState::StageComplete), stage_complete::enter_stage_complete)
        // Refill the next stage's enemy budget at exit so the wave
        // readout shows the just-finished stage during the buffer
        // (rather than the next stage's "WAVE 1/N").
        .add_systems(
            OnExit(AppState::StageComplete),
            (stage_complete::exit_stage_complete, map::queue_next_stage_combat),
        )
        .add_systems(
            Update,
            (
                stage_complete::tick_stage_complete,
                stage_complete::tick_stage_complete_wave,
            )
                .run_if(in_state(AppState::StageComplete)),
        )
        .add_systems(Update, (
            // Always-on visual setup. apply_night_mode → apply_palette must
            // be ordered so a night-mode toggle propagates to the camera in
            // the same frame.
            (apply_night_mode, apply_palette, update_hash_image).chain(),
        ))
        .add_systems(Update, (
            // Combat sim — paused while on the map view.
            friendly_movement,
            enemy_ai,
            apply_velocity,
            friendly_ram_damage,
            stats::shield_recharge_system,
            bomber_detonate,
            spawn_enemies,
            sync_turret_config,
            // Beam damage must run AFTER turret_aim_fire so the BeamPending
            // entities it spawns are visible. .chain() inserts the apply-
            // deferred sync point we need to see them this frame.
            (turret_aim_fire, beam_apply_damage).chain(),
            enemy_fire,
            bullet_update,
            mortar_shell_tick,
            // HeliPad slots: sync the "one helicopter per equipped slot"
            // invariant first so a freshly spawned heli ticks this frame
            // in `helicopter_ai`. Both gate themselves on slot config so
            // they idle harmlessly when no HeliPad is equipped.
            // `sync_helipad_helicopters` first so freshly-spawned heli
            // entities exist when `sync_helipad_nose_barrels` looks up
            // their owning slot for visibility. `.chain()` inserts the
            // command-flush sync point that makes that hand-off safe.
            (sync_helipad_helicopters, sync_helipad_nose_barrels, helicopter_ai).chain(),
            // Damage application chain: every source writes Health, then
            // `enemy_death_check` despawns anything that hit zero. Chained so
            // sources see consistent HP and only one despawn fires per kill.
            // `tick_echoes` slots in here because echo events are also a
            // damage source — its hits need to be visible to the death check.
            // `tick_on_conduit` / `tick_on_resonate` only decay their status
            // components — no damage — but live in the chain to keep all
            // status-related work in one ordered block.
            (
                bullet_collisions, tick_echoes,
                tick_on_fire, tick_on_frost,
                tick_on_conduit, tick_on_resonate,
                enemy_death_check,
            ).chain(),
            // Once the section's enemy budget is drained AND every
            // remaining enemy is dead, claim the section + flip back
            // to map view. Order vs `enemy_death_check` is best-effort —
            // worst case is a 1-frame delay before the transition.
            level_complete_check,
            // Mirror system for the failure side: friendly HP at 0 →
            // wipe arena, restore HP, back to map (no claim).
            level_fail_check,
            // Track damage frame-to-frame to spawn / refresh enemy
            // HP bars; visual updater positions + scales them.
            track_enemy_damage_for_hp_bars,
            update_enemy_hp_bars,
        ).run_if(in_combat_view))
        .add_systems(Update, (
            // Visuals / FX / UI. Split from the sim block so we don't blow
            // past Bevy's 20-system tuple limit.
            update_trail,
            update_enemy_trails,
            tick_hit_fx,
            apply_hit_fx_visuals,
            update_muzzle_flashes,
            update_beams,
            update_hit_particles,
            update_score_text,
            update_fps_text,
            update_vsync_label,
            ui_button_system,
            update_slot_labels,
            update_damage_bars,
            // Rendering pipeline housekeeping — sized as a sub-tuple
            // so the outer tuple stays under Bevy's 20-system cap.
            (
                resize_upscale_sprite,
                update_hud_camera_viewport,
                handle_desktop_escape,
                handle_desktop_drag_resize,
                apply_window_mode,
                apply_crt_mode,
                apply_vsync_mode,
                // Updates the play camera's translation each frame —
                // either tracks the friendly ship or holds at origin
                // depending on `CameraFollow.active`.
                apply_camera_follow,
            ),
        ))
        .add_systems(Update, (
            // HP bar runs in both map and combat view since the bar is
            // always visible. (Pier-visibility toggling inside
            // `update_wave_ui` is gated by `mode.is_changed()` so it
            // still only fires meaningfully on a Wave⇄Sandbox flip.)
            // Own block to stay under Bevy's 20-system tuple limit.
            update_wave_ui,
            update_hp_subdividers,
            update_hp_bar_pixel_scale,
            sync_ally_hp_bars,
            update_ally_hp_values,
        ))
        .add_systems(Update, (
            // Ally systems. Wave/pier orchestration is gone (Sandbox is
            // the only remaining mode); allies stay in their own bundle
            // so we don't blow past Bevy's 20-system tuple limit.
            // Sub-tuple to keep the outer count under the cap.
            (ally_ai, ally_turret_aim_fire, ally_death_check, plane_ai),
            // Missile launcher fires forward; missile track re-aims in
            // flight. Tracking runs *before* `apply_velocity` so the
            // updated direction drives this frame's integration.
            missile_launcher_fire,
            homing_missile_track,
            // Mines: drop at intervals from minelayers, then tick arm /
            // lifetime / proximity-detonation each frame. `flash_mine_dots`
            // is a pure visual effect so it can run alongside the
            // proximity check without ordering.
            mine_layer_drop,
            mine_tick,
            flash_mine_dots,
            // Tender: pick heal target + apply HP regen + spawn beam visual.
            tender_heal_beam,
            // Viking: boss-side AI overrides the standard `enemy_ai`
            // chase with the same charge-ramp curve as the friendly
            // ally Viking. Runs before `viking_ram_damage` so the
            // hit-frame snapshot already reflects this frame's speed.
            boss_viking_ai,
            // Viking: ram damage on contact with opposite-faction units.
            viking_ram_damage,
            // Blackbeard: launch boarding parties; boarders travel
            // and tick damage on their targets; rope visual tracks
            // both ends each frame for the connection effect.
            boarding_launcher_fire,
            boarder_tick,
            update_boarding_ropes,
            // OilTanker: drive the spray → ignite → burn cycle on
            // each tanker; per-slick lifetime + AOE-burn ticks
            // damage opposite-faction units inside their radius.
            oil_tanker_cycle,
            oil_slick_grow_tick,
            oil_slick_burn_tick,
        ).run_if(in_combat_view))
        .add_systems(Update, (
            // Map view — camera toggle, click target, boat steering, and
            // re-rasterize fills when the palette changes (so night-mode
            // toggle keeps the green/red tints recognizable instead of
            // hue-shifted by the new ocean color). Slot/popup systems
            // live in the same set since they share `MapState`.
            apply_view_mode,
            // Cleanup must run before begin_phase: on a Map-bound view
            // change, cleanup clears the timeline + stale anims, then
            // begin_phase repopulates with the new sequence — same frame.
            (close_popup_on_view_change, map_begin_phase, advance_map_anim_timeline).chain(),
            update_anim_pulses,
            update_anim_beams,
            map_click_input,
            map_boat_movement,
            // Patrol icons on 5★ sections — wander inside their
            // polygon while the player is on the map. Gated below
            // via the block-level `run_if` would be wrong (this
            // outer block also drives Combat-view systems like
            // `apply_view_mode`); patrol gates itself on
            // `AppState::Map` instead.
            boss_patrol_movement.run_if(in_state(AppState::Map)),
            refresh_map_fill,
            sync_owned_slot_visuals,
            update_map_button,
            update_map_slot_labels,
            update_building_button_tints,
            update_building_description,
            handle_building_choice_clicks,
            update_building_hover_tooltip,
            (
                handle_debug_buttons, update_debug_button_tints, update_claim_label,
                toggle_debug_ui_on_hash, sync_debug_panel_visibility,
                force_hide_ui_panel,
                update_damage_panel, update_damage_row_icons, sync_damage_panel_visibility,
                update_wave_indicator,
                tick_spawn_indicators,
                xp::update_xp_bar,
            ),
            (
                update_currency_ui,
                update_scrap_text, update_steel_text, update_refined_steel_text,
            ),
            // Combat-only level banner (top of play area). Self-gates
            // on view + mode internally; cheap to run every frame.
            update_level_status_ui,
            // Production economy ticks in both views — the Foundry /
            // Crane cycle keeps running while the player is in combat
            // so wave timers and cycle timers don't desync.
            tick_buildings,
            // Visual update for the per-converter progress bars.
            // Cheap (≤ 10 sections) and reads BuildingTimers, so it
            // sits next to `tick_buildings` for cache locality.
            update_building_progress_bars,
        ))
        .add_systems(Update, (
            // Customize overlay — primitives on a low-res render target,
            // upscaled with nearest-neighbor for chunky pixels. Bundled
            // into sub-tuples so the outer add_systems tuple stays
            // under Bevy's 20-cap. Every system self-gates on
            // `CustomizeOpen` so it idles while the overlay is closed.
            toggle_customize_render,
            resize_customize_display,
            track_customize_cursor,
            sync_customize_text,
            update_customize_ui,
            update_customize_ship,
            update_customize_shop,
            update_customize_tooltip,
            update_sell_label,
            sync_stats_panel,
            handle_stat_debug_buttons,
            update_shop_mod_cards,
            handle_shop_mod_click,
            handle_close_click,
            handle_reroll_button,
        ))
        // Cursor tracking → drag start → ghost follow → drop resolve.
        // Kept in its own `add_systems` so the schedule-tuple in the
        // block above stays a flat 14-item tuple (Bevy's trait impl
        // gets unhappy with nested chained tuples past a certain
        // shape).
        .add_systems(Update, (start_drag, update_drag_ghost, complete_drag).chain())
        .add_systems(Update, (
            // Persistent settings: load once on first frame; persist on
            // any change to NIGHT / CRT / VSYNC.
            apply_loaded_settings,
            persist_settings_on_change,
            // ESC pause overlay. Toggle is state-aware (only fires
            // Playing↔Paused). Visibility sync mirrors the Paused flag.
            toggle_pause_on_esc,
            sync_pause_menu_visibility,
            main_menu::sync_main_menu_visibility,
            main_menu::sync_main_menu_view,
            main_menu::handle_settings_item_click,
            main_menu::update_settings_labels,
        ))
        // Pause-menu click handlers — gated on `Paused`. Bevy UI's
        // picking still drives `Interaction::Pressed` on hidden Nodes
        // (full-screen overlay child positions overlap whatever's
        // underneath), so without this gate a click in the customize
        // shop that lands on a hidden Resume / Main Menu button
        // position would silently transition state — the "shop
        // randomly closes" bug.
        .add_systems(Update, (
            handle_resume_click,
            handle_pause_main_menu_click,
            handle_quit_click,
        ).run_if(in_state(AppState::Paused)))
        // Main-menu click handlers — same picking-respects-visibility
        // problem. Gate to MainMenu state only.
        .add_systems(Update, (
            main_menu::handle_play_click,
            main_menu::handle_settings_click,
        ).run_if(in_state(AppState::MainMenu)))
        // Game-over overlay click handlers — gated to GameOver. Was
        // previously safe due to its rare/clear visibility, but the
        // same picking-on-hidden bug applies; gating future-proofs.
        .add_systems(Update, (
            game_over::handle_restart_click,
            game_over::handle_main_menu_click,
            game_over::handle_quit_click,
        ).run_if(in_state(AppState::GameOver)))
        .run();
}
