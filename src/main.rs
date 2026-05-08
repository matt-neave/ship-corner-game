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
mod effects;
mod enemy;
mod i18n;
mod map;
mod modes;
mod palette;
mod pier;
mod rendering;
mod rune;
mod ship;
mod trails;
mod turret;
mod ui;
mod ui_kit;
mod wave;
mod weapon;

use ally::{
    ally_ai, ally_death_check, ally_turret_aim_fire, homing_missile_track,
    mine_layer_drop, mine_tick, missile_launcher_fire, plane_ai, tender_heal_beam,
};
use balance::{WINDOW_H, WINDOW_W};
use beam::{beam_apply_damage, update_beams};
use bullet::{bullet_collisions, bullet_update};
use effects::{
    apply_hit_fx_visuals, tick_hit_fx, update_hit_particles, update_muzzle_flashes,
};
use enemy::{bomber_detonate, enemy_ai, enemy_death_check, enemy_fire, spawn_enemies};
use map::{
    advance_map_anim_timeline, apply_view_mode, close_popup_on_view_change,
    handle_building_choice_clicks, handle_debug_buttons,
    in_combat_view, map_begin_phase, map_boat_movement, map_click_input,
    refresh_map_fill, setup_currency_ui, setup_debug_ui, setup_map,
    sync_owned_slot_visuals, tick_buildings,
    update_anim_beams, update_anim_pulses,
    update_building_button_tints, update_building_description,
    update_claim_label, update_currency_ui_visibility, update_debug_button_tints,
    update_map_slot_labels, update_refined_steel_text, update_scrap_text,
    update_steel_text,
    BuildingTimers, CombatContext, DebugClaimMode, MapAnimTimeline, MapState,
    TriggerMapPhase, ViewMode,
};
use modes::{
    apply_crt_mode, apply_night_mode, apply_vsync_mode, apply_window_mode,
    handle_desktop_drag_resize, handle_desktop_escape,
    CrtMode, GameMode, NightMode, VsyncMode, WindowMode,
};
use palette::{apply_palette, Palette};
use pier::{draft_input, sync_pier_visuals, update_draft_ui, Pier, WaveDraft};
use rendering::{resize_upscale_sprite, setup_render, update_hash_image};
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
        rune: None,
    };
    for i in 1..8 {
        cfg.slots[i] = SlotCfg {
            equipped: false,
            weapon: WeaponType::Standard,
            damage: 1,
            fire_rate: 4.0,
            barrels: 1,
            rune: None,
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
        .insert_resource(Scrap::default())
        .insert_resource(Steel::default())
        .insert_resource(RefinedSteel::default())
        .insert_resource(SpawnTimer { t: 0.0, elapsed: 0.0 })
        .insert_resource(cfg)
        .insert_resource(DamageStats::default())
        .insert_resource(Palette::aap64_naval())
        .insert_resource(ShipPath::default())
        .insert_resource(WindowMode::default())
        .insert_resource(NightMode::default())
        .insert_resource(CrtMode::default())
        .insert_resource(VsyncMode::default())
        .insert_resource(GameMode::default())
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
        .add_systems(Startup, (setup_render, setup_world, setup_ui, setup_map, setup_debug_ui, setup_currency_ui).chain())
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
            resize_upscale_sprite,
            handle_desktop_escape,
            handle_desktop_drag_resize,
            apply_window_mode,
            apply_crt_mode,
            apply_vsync_mode,
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
            // lifetime / proximity-detonation each frame.
            mine_layer_drop,
            mine_tick,
            // Tender: pick heal target + apply HP regen + spawn beam visual.
            tender_heal_beam,
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
            (handle_debug_buttons, update_debug_button_tints, update_claim_label),
            (
                update_currency_ui_visibility,
                update_scrap_text, update_steel_text, update_refined_steel_text,
            ),
            // Production economy ticks in both views — the Foundry /
            // Crane cycle keeps running while the player is in combat
            // so wave timers and cycle timers don't desync.
            tick_buildings,
        ))
        .run();
}
