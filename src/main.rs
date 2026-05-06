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

use bevy::prelude::*;

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
mod rendering;
mod ship;
mod trails;
mod turret;
mod ui;
mod wave;
mod weapon;

use balance::{WINDOW_H, WINDOW_W};
use beam::{beam_apply_damage, update_beams};
use bullet::{bullet_collisions, bullet_update};
use effects::{
    apply_hit_fx_visuals, tick_hit_fx, update_hit_particles, update_muzzle_flashes,
};
use enemy::{bomber_detonate, enemy_ai, enemy_fire, spawn_enemies};
use modes::{
    apply_crt_mode, apply_night_mode, apply_window_mode,
    handle_desktop_drag_resize, handle_desktop_escape,
    CrtMode, GameMode, NightMode, WindowMode,
};
use palette::{apply_palette, Palette};
use pier::{draft_input, sync_pier_visuals, update_draft_ui, Pier, WaveDraft};
use rendering::{resize_upscale_sprite, setup_render, update_hash_image};
use ship::{apply_velocity, friendly_movement, setup_world};
use trails::{update_enemy_trails, update_trail, ShipPath};
use turret::{sync_turret_config, turret_aim_fire, SlotCfg, TurretConfig};
use ui::{
    setup_ui, ui_button_system, update_damage_bars, update_score_text, update_slot_labels,
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
    };
    for i in 1..8 {
        cfg.slots[i] = SlotCfg {
            equipped: false,
            weapon: WeaponType::Standard,
            damage: 1,
            fire_rate: 4.0,
            barrels: 1,
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
        .insert_resource(ClearColor(Color::srgb(0.05, 0.05, 0.08)))
        .insert_resource(Score(0))
        .insert_resource(SpawnTimer { t: 0.0, elapsed: 0.0 })
        .insert_resource(cfg)
        .insert_resource(DamageStats::default())
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
