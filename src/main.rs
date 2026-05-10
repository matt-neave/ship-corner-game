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
mod bullet;
mod components;
mod customize;
mod effects;
mod enemy;
mod i18n;
mod map;
mod modes;
mod palette;
mod pause;
mod pier;
mod rendering;
mod main_menu;
mod settings;
mod rune;
mod ship;
mod stats;
mod trails;
mod turret;
mod ui;
mod ui_kit;
mod wave;
mod weapon;

use ally::{
    ally_ai, ally_death_check, ally_turret_aim_fire, boarder_tick,
    boarding_launcher_fire, flash_mine_dots, homing_missile_track,
    mine_layer_drop, mine_tick, missile_launcher_fire, plane_ai,
    tender_heal_beam, update_boarding_ropes,
};
use customize::{
    complete_drag, handle_close_click, handle_reroll_button, init_customize_shop,
    resize_customize_display, setup_customize_render, setup_customize_ui, start_drag,
    handle_shop_mod_click, handle_stat_debug_buttons, sync_customize_text, sync_stats_panel,
    toggle_customize_render, track_customize_cursor, update_shop_mod_cards,
    update_customize_ship, update_customize_shop, update_customize_tooltip,
    update_customize_ui, update_drag_ghost, CustomizeOpen, DragState,
};
use balance::{WINDOW_H, WINDOW_W};
use beam::{beam_apply_damage, update_beams};
use bullet::{bullet_collisions, bullet_update};
use effects::{
    apply_hit_fx_visuals, tick_hit_fx, update_hit_particles, update_muzzle_flashes,
};
use enemy::{
    bomber_detonate, enemy_ai, enemy_death_check, enemy_fire,
    setup_enemy_hp_bar_assets, spawn_enemies, track_enemy_damage_for_hp_bars,
    update_enemy_hp_bars,
};
use map::{
    advance_map_anim_timeline, apply_view_mode, close_popup_on_view_change,
    handle_building_choice_clicks, handle_debug_buttons,
    in_combat_view, level_complete_check, level_fail_check, map_begin_phase,
    map_boat_movement, map_click_input, refresh_map_fill, setup_currency_ui,
    setup_debug_ui, setup_level_status_ui, setup_map, setup_progress_assets,
    sync_owned_slot_visuals, tick_buildings,
    update_anim_beams, update_anim_pulses, update_building_button_tints,
    update_building_description, update_building_hover_tooltip,
    update_building_progress_bars, update_claim_label, update_currency_ui,
    update_debug_button_tints, update_level_status_ui, update_map_slot_labels,
    update_refined_steel_text, update_scrap_text, update_steel_text,
    BuildingTimers, CombatContext, DebugClaimMode, MapAnimTimeline, MapState,
    TriggerMapPhase, ViewMode,
};
use modes::{
    apply_camera_follow, apply_crt_mode, apply_night_mode, apply_vsync_mode,
    apply_window_mode, handle_desktop_drag_resize, handle_desktop_escape,
    CameraFollow, CrtMode, GameMode, NightMode, VsyncMode, WindowMode,
};
use palette::{apply_palette, Palette};
use pause::{
    handle_quit_click, handle_resume_click, setup_pause_menu, sync_pause_menu_visibility,
    toggle_pause_on_esc, Paused,
};
use pier::{draft_input, sync_pier_visuals, update_draft_ui, Pier, WaveDraft};
use rendering::{
    resize_upscale_sprite, setup_render, update_hash_image, update_hud_camera_viewport,
};
use settings::{apply_loaded_settings, persist_settings_on_change};
use rune::{tick_echoes, tick_on_conduit, tick_on_fire, tick_on_frost, tick_on_resonate};
use ship::{apply_velocity, friendly_movement, setup_world};
use trails::{update_enemy_trails, update_trail, ShipPath};
use turret::{sync_turret_config, turret_aim_fire, SlotCfg, TurretConfig};
use ui::{
    setup_ui, ui_button_system, update_damage_bars, update_fps_text, update_map_button,
    sync_ally_hp_bars, update_ally_hp_values,
    update_hp_bar_pixel_scale, update_hp_subdividers,
    update_score_text, update_slot_labels, update_vsync_label,
    update_wave_ui, DamageStats,
};
use wave::{wave_orchestrator, WaveState};
use weapon::WeaponType;

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

fn main() {
    let mut cfg = TurretConfig::default();
    cfg.slots[0] = SlotCfg {
        equipped: true,
        weapon: WeaponType::Standard,
        damage: 1,
        fire_rate: 4.0,
        barrels: 1,
        runes: [None; 3],
    };
    for i in 1..8 {
        cfg.slots[i] = SlotCfg {
            equipped: false,
            weapon: WeaponType::Standard,
            damage: 1,
            fire_rate: 4.0,
            barrels: 1,
            runes: [None; 3],
        };
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
        .add_plugins(FrameTimeDiagnosticsPlugin::default())
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
        .insert_resource(Palette::aap64_naval())
        .insert_resource(ShipPath::default())
        .insert_resource(WindowMode::default())
        .insert_resource(NightMode::default())
        .insert_resource(CrtMode::default())
        .insert_resource(VsyncMode::default())
        .insert_resource(GameMode::default())
        .insert_resource(CameraFollow::default())
        .insert_resource(WaveState::default())
        .insert_resource(Pier::default())
        .insert_resource(WaveDraft::default())
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
            setup_debug_ui, setup_currency_ui, setup_progress_assets,
            setup_level_status_ui, setup_enemy_hp_bar_assets,
            init_customize_shop, setup_customize_render, setup_customize_ui,
            setup_pause_menu, main_menu::setup_main_menu,
        ).chain())
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
            // Wave-mode + ally systems live in their own bundle so we don't
            // blow past the 20-system tuple limit on the visuals/UI block.
            // All combat-side; paused with the rest while on the map.
            wave_orchestrator,
            sync_pier_visuals,
            draft_input,
            update_draft_ui,
            ally_ai,
            ally_turret_aim_fire,
            ally_death_check,
            plane_ai,
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
            // Blackbeard: launch boarding parties; boarders travel
            // and tick damage on their targets; rope visual tracks
            // both ends each frame for the connection effect.
            boarding_launcher_fire,
            boarder_tick,
            update_boarding_ropes,
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
            refresh_map_fill,
            sync_owned_slot_visuals,
            update_map_button,
            update_map_slot_labels,
            update_building_button_tints,
            update_building_description,
            handle_building_choice_clicks,
            update_building_hover_tooltip,
            (handle_debug_buttons, update_debug_button_tints, update_claim_label),
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
            (
                toggle_customize_render,
                resize_customize_display,
                track_customize_cursor,
                sync_customize_text,
            ),
            (
                update_customize_ui,
                update_customize_ship,
                update_customize_shop,
                update_customize_tooltip,
                sync_stats_panel,
                handle_stat_debug_buttons,
                update_shop_mod_cards,
                handle_shop_mod_click,
                handle_close_click,
                handle_reroll_button,
            ),
            // Cursor tracking → drag start → ghost follow → drop resolve.
            (start_drag, update_drag_ghost, complete_drag).chain(),
        ))
        .add_systems(Update, (
            // Persistent settings: load once on first frame; persist on
            // any change to NIGHT / CRT / VSYNC.
            apply_loaded_settings,
            persist_settings_on_change,
            // ESC pause overlay. Resume / Quit gated by Changed<Interaction>
            // so they only fire on the press frame.
            toggle_pause_on_esc,
            sync_pause_menu_visibility,
            handle_resume_click,
            handle_quit_click,
            // Boot-time main menu. PLAY closes it; SETTINGS is a stub
            // for now (existing settings live behind keys).
            main_menu::sync_main_menu_visibility,
            main_menu::handle_play_click,
            main_menu::handle_settings_click,
        ))
        .run();
}
