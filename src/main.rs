//! Ship Game — Bevy app entry point. Declares the domain modules and wires
//! up the App (resources + system schedule).

use bevy::diagnostic::FrameTimeDiagnosticsPlugin;
use bevy::prelude::*;

mod ally;
mod balance;
mod beam;
mod blade;
mod booster;
mod boss_intro;
mod boss_reward;
mod bullet;
mod cannon;
mod components;
mod customize;
mod dockyard_view;
mod effects;
mod enemy;
mod game_over;
mod harpoon;
mod hull;
mod i18n;
mod map;
mod modes;
mod palette;
mod pause;
mod rendering;
mod main_menu;
mod octopus;
mod onboarding;
mod settings;
mod rune;
mod ship;
mod stage_complete;
mod stats;
mod synergy;
mod trails;
mod turret;
mod ui;
mod ui_kit;
mod weapon;
mod xp;

use ally::{
    ally_ai, ally_death_check, ally_turret_aim_fire, boarder_tick,
    boarding_launcher_fire, flash_mine_dots, homing_missile_track,
    mine_layer_drop, mine_tick, missile_launcher_fire,
    oil_slick_burn_tick, oil_slick_grow_tick, oil_tanker_cycle, plane_ai,
    tender_heal_beam, update_ally_positions_cache, update_boarding_ropes,
    viking_ram_damage, AllyPositionsCache,
};
use customize::{
    complete_drag, handle_close_click, handle_reroll_button, init_customize_shop,
    resize_customize_display, setup_customize_render, setup_customize_ui, start_drag,
    handle_shop_mod_click, handle_stat_debug_buttons, sync_customize_text, sync_stat_debug_visibility, sync_stats_panel,
    toggle_customize_render, track_customize_cursor, update_shop_mod_cards,
    update_customize_ship, update_customize_shop, update_customize_tooltip,
    update_customize_ui, update_drag_ghost, update_sell_label, update_synergy_banner,
    CustomizeOpen, DragState,
};
use balance::{WINDOW_H, WINDOW_W};
use beam::{beam_apply_damage, update_beams};
use bullet::{bullet_collisions, bullet_update, process_damage_events, PendingDamageQueue};
use effects::{
    apply_hit_fx_visuals, tick_hit_fx, update_hit_particles, update_muzzle_flashes,
};
use enemy::{
    artillery_fire, artillery_shell_tick, bomber_detonate, boss_chaos_spawn,
    clear_spawn_indicators, enemy_ai,
    enemy_death_check, enemy_fire, enemy_landmine_tick, setup_enemy_hp_bar_assets,
    setup_spawn_indicator_assets, sniper_aim_line_tick, sniper_fire, sniper_turret_aim,
    spawn_enemies, tick_spawn_indicators, track_enemy_damage_for_hp_bars, update_enemy_hp_bars,
};
use map::{
    advance_map_anim_timeline, apply_view_mode, boss_patrol_movement,
    close_popup_on_view_change, handle_building_choice_clicks, handle_debug_buttons,
    in_combat_view, level_complete_check, level_fail_check, map_begin_phase,
    map_boat_movement, map_click_input, refresh_map_fill, setup_currency_ui,
    setup_debug_ui, setup_level_status_ui, setup_map,
    setup_progress_assets, spawn_boss_patrols,
    sync_debug_panel_visibility, sync_owned_slot_visuals,
    tick_buildings,
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
use ship::{apply_velocity, friendly_movement, friendly_ram_damage, setup_world, tick_stunned};
use trails::{update_enemy_trails, update_trail, ShipPath};
use turret::{
    helicopter_ai, mortar_shell_tick, sync_helipad_helicopters, sync_helipad_nose_barrels,
    sync_turret_config, turret_aim_fire, TurretConfig,
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

/// Single source of truth for which screen the player is on. Combat-sim
/// systems gate on `Playing` so menus actually pause gameplay.
#[derive(States, Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum AppState {
    #[default]
    MainMenu,
    Playing,
    /// "STAGE COMPLETE" beat between a cleared level and the shop.
    StageComplete,
    /// XP-driven level-up screen. Re-enters itself until `LevelUpsPending`
    /// drains, then moves on to `Customize`.
    LevelUp,
    /// Hull selection — applies stat modifiers before `Playing`.
    HullSelect,
    Customize,
    /// Between-stage map screen — player picks the next section.
    Map,
    Paused,
    GameOver,
    /// Boss-section reward pick screen — recruit / bounty / super mod.
    /// Sits between StageComplete and LevelUp, only entered when
    /// `BossRewardPending` is `Some`.
    BossReward,
    /// Borderlands-style boss intro overlay — sweeps in two white bars
    /// + class name for `boss_intro::DURATION` seconds, then drops the
    /// boss into the arena and returns to `Playing`. Combat freezes
    /// while in this state because it isn't in `in_combat_view`.
    BossIntro,
}

/// One-way mirror: drive the boolean overlay flags from the authoritative
/// `AppState` so UI systems that still read the booleans stay in sync.
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

fn enter_map_view(mut view: ResMut<map::ViewMode>) {
    if *view != map::ViewMode::Map { *view = map::ViewMode::Map; }
}

fn enter_combat_view(mut view: ResMut<map::ViewMode>) {
    if *view != map::ViewMode::Combat { *view = map::ViewMode::Combat; }
}

/// Reset XP + queued level-ups on returning to the main menu so a fresh
/// PLAY starts at LV 1 / 0 XP. RESTART from game-over takes a separate
/// path through `reset_run_for_restart`.
fn reset_xp_for_main_menu(
    mut xp: ResMut<xp::Xp>,
    mut pending: ResMut<xp::LevelUpsPending>,
) {
    xp.reset();
    pending.0 = 0;
}

/// Stage-start hook on Map→Playing: refill the friendly hull and despawn
/// arena debris from the previous stage. Doesn't fire on Paused→Playing
/// or GameOver→Playing — those paths don't pass through `Map`.
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

#[derive(Resource)]
pub struct Score(pub u32);

/// Number of map sections the player has cleared this run. Scales wave
/// difficulty so later stages feel weightier than the same star tier
/// picked first.
#[derive(Resource, Default)]
pub struct CampaignProgress {
    pub battles_cleared: u32,
}

/// Currency dropped by killed enemies (+1 per kill). Spent on map-view
/// building placement and consumed by Foundries.
#[derive(Resource, Default)]
pub struct Scrap(pub u32);

/// Refined currency produced by Foundries. Consumed by Cranes for their
/// adjacency speed boost.
#[derive(Resource, Default)]
pub struct Steel(pub u32);

/// Top-tier refined output, produced by Refineries from steel.
#[derive(Resource, Default)]
pub struct RefinedSteel(pub u32);

#[derive(Resource)]
pub struct SpawnTimer { pub t: f32, pub elapsed: f32 }

/// Wall-clock seconds since the current run started. Pauses on MainMenu
/// and HullSelect; reset on `OnEnter(HullSelect)`.
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
        .add_plugins((
            cannon::CannonPlugin,
            booster::BoosterPlugin,
            blade::BladePlugin,
            octopus::OctopusPlugin,
            harpoon::HarpoonPlugin,
            onboarding::OnboardingPlugin,
        ))
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
        .insert_resource(stage_complete::ScrapEarnedThisStage::default())
        .insert_resource(boss_intro::BossIntroTimer::default())
        .insert_resource(boss_intro::BossIntroPending::default())
        .insert_resource(xp::Xp::default())
        .insert_resource(xp::LevelUpsPending::default())
        .insert_resource(xp::LevelUpReturn::default())
        .insert_resource(synergy::Synergies::default())
        .insert_resource(PendingDamageQueue::default())
        .insert_resource(xp::LevelUpChoices::default())
        .insert_resource(hull::SelectedHull::default())
        .insert_resource(hull::PreviewHull::default())
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
        .insert_resource(map::MapSize::default())
        .insert_resource(MapState::new(map::MapSize::default().sections()))
        .insert_resource(BuildingTimers::default())
        .insert_resource(MapAnimTimeline::default())
        .insert_resource(CombatContext::default())
        .insert_resource(DebugClaimMode::default())
        .add_event::<TriggerMapPhase>()
        .insert_resource(CustomizeOpen::default())
        .insert_resource(DragState::default())
        .insert_resource(customize::TooltipLayout::default())
        .insert_resource(Paused::default())
        .insert_resource(AllyPositionsCache::default())
        .insert_resource(boss_reward::BossRewardPending::default())
        .insert_resource(boss_reward::RecruitedAllies::default())
        .insert_resource(boss_reward::BossRewardOffer::default())
        .add_systems(Startup, (
            setup_render, setup_world, setup_ui, setup_map,
            // After setup_map so 5★ polygons exist for reject-sampling.
            spawn_boss_patrols,
            setup_debug_ui, setup_currency_ui, setup_progress_assets,
            setup_level_status_ui, setup_enemy_hp_bar_assets,
            init_customize_shop, setup_customize_render, setup_customize_ui,
            setup_pause_menu, main_menu::setup_main_menu, setup_damage_panel,
            setup_wave_indicator, setup_spawn_indicator_assets,
        ).chain())
        // Dockyard pipeline — its own Startup block to stay under
        // Bevy's 20-system tuple limit. No external ordering needed
        // (the existing Startup chain doesn't depend on dockyard
        // entities), and within this block render-setup must run
        // before the scene spawn so the camera/target exist.
        // Render-target plumbing (camera + image + backdrop + display
        // sprite) is created once at startup and kept warm. The scene
        // *contents* (walkways, finger piers, ships, labels) live in a
        // per-visit lifecycle — spawned OnEnter(HullSelect), despawned
        // OnExit(HullSelect) — so the dockyard and the game share zero
        // entities.
        .add_systems(Startup, dockyard_view::setup_dockyard_render)
        .add_systems(
            OnEnter(AppState::HullSelect),
            (
                dockyard_view::spawn_dockyard_scene,
                dockyard_view::spawn_dockyard_labels,
            ),
        )
        // Bridge runs first so the rest of Update sees synced flags.
        .add_systems(Update, sync_state_to_open_resources)
        .add_systems(OnEnter(AppState::Customize), init_customize_shop)
        .add_systems(OnExit(AppState::MainMenu), (reset_damage_stats, clear_spawn_indicators))
        .add_systems(OnEnter(AppState::MainMenu), main_menu::clear_arena_on_main_menu)
        .add_systems(OnExit(AppState::Customize), reset_damage_stats)
        .add_systems(OnEnter(AppState::Customize), clear_spawn_indicators)
        .add_systems(OnEnter(AppState::Map), enter_map_view)
        .add_systems(OnEnter(AppState::Playing), enter_combat_view)
        // Map→Playing is the canonical stage-start hook: refill HP,
        // wipe arena debris from last stage, and respawn the
        // permanent ally roster at full HP.
        .add_systems(
            OnExit(AppState::Map),
            (refill_and_clean_for_next_stage, boss_reward::respawn_allies_for_stage),
        )
        .add_systems(OnEnter(AppState::HullSelect), (hull::enter_hull_select, reset_run_timer))
        // OnExit(HullSelect) chain: tear down the dockyard UI, regenerate
        // the map with the player's chosen `MapSize`, then re-run the
        // map-view setup so the new topology has its visuals. Chained so
        // the new MapState is in place before setup_map reads it.
        .add_systems(
            OnExit(AppState::HullSelect),
            (
                hull::exit_hull_select,
                dockyard_view::despawn_dockyard_scene,
                map::regenerate_map,
                setup_map,
                spawn_boss_patrols,
            ).chain(),
        )
        .add_systems(OnEnter(AppState::GameOver), game_over::enter_game_over)
        .add_systems(
            OnExit(AppState::GameOver),
            (
                game_over::exit_game_over,
                game_over::reset_run_for_restart,
                boss_reward::reset_boss_reward_state,
            ),
        )
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
                hull::handle_map_size_click,
                hull::handle_play_click,
                hull::handle_back_click,
                hull::handle_back_on_esc,
                hull::sync_hull_select_on_change,
                hull::sync_hull_apply,
                // Dockyard pixel-scene driving — hover preview, click
                // commit, per-frame highlight + label positioning.
                dockyard_view::handle_dockyard_hover,
                dockyard_view::handle_dockyard_click,
                dockyard_view::update_dockyard_highlight,
                dockyard_view::update_dockyard_labels,
            ).run_if(in_state(AppState::HullSelect)),
        )
        .add_systems(
            OnEnter(AppState::MainMenu),
            (
                reset_xp_for_main_menu,
                game_over::reset_run_for_restart,
                boss_reward::reset_boss_reward_state,
            ),
        )
        // Clamp HP to max each frame so stat changes never leave a
        // "100/50" readout when max HP dropped below current HP.
        .add_systems(Update, (hull::clamp_hp_to_max, tick_run_timer))
        .add_systems(
            OnEnter(AppState::Playing),
            stage_complete::reset_scrap_earned_on_play,
        )
        .add_systems(OnEnter(AppState::StageComplete), stage_complete::enter_stage_complete)
        // Stage advances on EXIT so the in-buffer readout still shows
        // the just-finished stage, not the next stage's "WAVE 1/N".
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
        // Boss-reward pick screen — sits between StageComplete and
        // LevelUp when `BossRewardPending` is `Some`. The click
        // handler advances onward (LevelUp if any pending, else
        // Customize), matching the gate in `tick_stage_complete`.
        .add_systems(OnEnter(AppState::BossReward), boss_reward::enter_boss_reward)
        .add_systems(OnExit(AppState::BossReward), boss_reward::exit_boss_reward)
        .add_systems(
            Update,
            boss_reward::handle_boss_reward_click.run_if(in_state(AppState::BossReward)),
        )
        .add_systems(
            Update,
            boss_reward::update_boss_reward_stat_tooltip
                .run_if(in_state(AppState::BossReward)),
        )
        // Boss intro overlay — Borderlands-style streaks + class name.
        // Combat is frozen while this state is active because it isn't
        // in `in_combat_view`'s allow-list.
        .add_systems(OnEnter(AppState::BossIntro), boss_intro::enter_boss_intro)
        .add_systems(OnExit(AppState::BossIntro), boss_intro::exit_boss_intro)
        .add_systems(
            Update,
            boss_intro::tick_boss_intro.run_if(in_state(AppState::BossIntro)),
        )
        .add_systems(Update, (
            // night_mode → palette must order so a toggle propagates
            // to the camera in the same frame.
            (apply_night_mode, apply_palette, update_hash_image).chain(),
        ))
        .add_systems(Update, (
            // Refresh the shared ally-positions snapshot before any
            // enemy AI / fire system reads it this frame.
            update_ally_positions_cache,
            friendly_movement,
            enemy_ai,
            tick_stunned,
            apply_velocity,
            friendly_ram_damage,
            stats::shield_recharge_system,
            bomber_detonate,
            (spawn_enemies, boss_chaos_spawn),
            sync_turret_config,
            // beam_apply_damage needs the BeamPending entities spawned
            // by turret_aim_fire to be visible this frame.
            (turret_aim_fire, beam_apply_damage).chain(),
            // Sub-tuple keeps the outer count under Bevy's 20-system cap.
            (
                enemy_fire,
                sniper_fire, sniper_aim_line_tick, sniper_turret_aim,
                artillery_fire, artillery_shell_tick,
                enemy_landmine_tick,
            ),
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
            // Damage application chain. Producers (`bullet_collisions`,
            // `tick_echoes`, blade/octopus/mortar/beam systems run earlier
            // in the schedule) push `DamageEvent`s into
            // `PendingDamageQueue`; `process_damage_events` drains
            // them, applies damage, rolls runes, and chains.
            // `enemy_death_check` despawns anything that hit zero AFTER
            // the drain so chain damage gets the same death pipeline.
            // `tick_on_*` decay status components — no damage — but
            // live in the chain to keep all status-related work in one
            // ordered block.
            (
                bullet_collisions, tick_echoes,
                process_damage_events,
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
            // Sub-tuple keeps the outer count under Bevy's 20-system cap.
            (
                resize_upscale_sprite,
                update_hud_camera_viewport,
                handle_desktop_escape,
                handle_desktop_drag_resize,
                apply_window_mode,
                apply_crt_mode,
                apply_vsync_mode,
                apply_camera_follow,
            ),
        ))
        .add_systems(Update, (
            // HP bars are visible in both map and combat view.
            update_wave_ui,
            update_hp_subdividers,
            update_hp_bar_pixel_scale,
            sync_ally_hp_bars,
            update_ally_hp_values,
        ))
        .add_systems(Update, (
            (ally_ai, ally_turret_aim_fire, ally_death_check, plane_ai),
            // homing_missile_track runs before apply_velocity so the
            // re-aimed direction drives this frame's integration.
            missile_launcher_fire,
            homing_missile_track,
            mine_layer_drop,
            mine_tick,
            flash_mine_dots,
            tender_heal_beam,
            // Boss Vikings now carry the `Ally` tag, so `ally_ai`'s
            // Viking branch drives their charge — no need for a
            // separate boss-side AI. `viking_ram_damage` still runs
            // for both (it iterates anything with `VikingRamCharge`).
            viking_ram_damage,
            boarding_launcher_fire,
            boarder_tick,
            update_boarding_ropes,
            oil_tanker_cycle,
            oil_slick_grow_tick,
            oil_slick_burn_tick,
        ).run_if(in_combat_view))
        .add_systems(Update, (
            apply_view_mode,
            // cleanup → begin_phase → advance: a Map-bound view change
            // wipes the timeline + stale anims, then refills with the
            // new sequence — all in the same frame.
            (close_popup_on_view_change, map_begin_phase, advance_map_anim_timeline).chain(),
            update_anim_pulses,
            update_anim_beams,
            map_click_input,
            map_boat_movement,
            // The outer block runs systems in BOTH views; patrol must
            // self-gate to Map so it doesn't tick during combat.
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
            update_level_status_ui,
            // Production economy ticks in both views so cycle timers
            // don't desync when the player drops into combat.
            tick_buildings,
            update_building_progress_bars,
        ))
        .add_systems(Update, (
            // Every customize system self-gates on `CustomizeOpen`.
            toggle_customize_render,
            resize_customize_display,
            track_customize_cursor,
            sync_customize_text,
            update_customize_ui,
            update_customize_ship,
            update_customize_shop,
            update_customize_tooltip,
            update_synergy_banner,
            sync_stats_panel,
            // After sync_customize_text so the debug-only Hidden write
            // isn't overwritten by the generic Inherited.
            sync_stat_debug_visibility.after(sync_customize_text),
            handle_stat_debug_buttons,
            update_shop_mod_cards,
            handle_shop_mod_click,
            handle_close_click,
            handle_reroll_button,
        ))
        // Drag chain in its own add_systems — chained tuples nested
        // inside the block above hit a Bevy trait-impl limit.
        // `.after(sync_customize_text)` makes `update_sell_label` the
        // final writer for the preview's visibility on the strip.
        .add_systems(
            Update,
            (start_drag, update_drag_ghost, complete_drag, update_sell_label)
                .chain()
                .after(sync_customize_text),
        )
        // Unconditional: discovery must fire while customize is open
        // so equipping a 2nd tagged turret pops the banner immediately.
        .add_systems(
            Update,
            (synergy::compute_synergies, synergy::discover_synergies).chain(),
        )
        .add_systems(Update, (
            // Dockyard render-target activation + display-sprite
            // sizing — both self-gate, so cheap to leave always-on.
            dockyard_view::toggle_dockyard_render,
            dockyard_view::resize_dockyard_display,
            apply_loaded_settings,
            persist_settings_on_change,
            // ESC toggle only fires Playing↔Paused.
            toggle_pause_on_esc,
            sync_pause_menu_visibility,
            main_menu::sync_main_menu_visibility,
            main_menu::sync_main_menu_view,
            main_menu::handle_settings_item_click,
            main_menu::update_settings_labels,
        ))
        // Click handlers are gated to their owning state because Bevy
        // UI picking fires `Interaction::Pressed` on hidden Nodes
        // (overlapping full-screen overlays), so a click on one screen
        // would otherwise silently trigger a transition on another.
        .add_systems(Update, (
            handle_resume_click,
            handle_pause_main_menu_click,
            handle_quit_click,
        ).run_if(in_state(AppState::Paused)))
        .add_systems(Update, (
            main_menu::handle_play_click,
            main_menu::handle_settings_click,
        ).run_if(in_state(AppState::MainMenu)))
        .add_systems(Update, (
            game_over::handle_restart_click,
            game_over::handle_main_menu_click,
            game_over::handle_quit_click,
        ).run_if(in_state(AppState::GameOver)))
        .run();
}
