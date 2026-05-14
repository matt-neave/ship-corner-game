//! Ship Game — Bevy app entry point. Declares the domain modules and wires
//! up the App (resources + system schedule).

use bevy::diagnostic::FrameTimeDiagnosticsPlugin;
use bevy::prelude::*;

mod ally;
mod anchor_flail;
mod balance;
mod beam;
mod blade;
mod booster;
mod boss_intro;
mod boss_reward;
mod bullet;
mod cannon;
mod components;
mod crows_nest;
mod customize;
mod dockyard_view;
mod effects;
mod enemy;
mod flamethrower;
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
mod sfx;
mod rune;
mod ship;
mod stage_complete;
mod stats;
mod stats_panel_overlay;
mod synergy;
mod trails;
mod turret;
mod ui;
mod ui_kit;
mod weapon;
mod win_screen;
mod xp;

use ally::{
    ally_ai, ally_death_check, ally_turret_aim_fire, boarder_tick,
    boarding_launcher_fire, flash_mine_dots, homing_missile_track,
    mine_layer_drop, mine_tick, missile_launcher_fire,
    oil_slick_burn_tick, oil_slick_grow_tick, oil_tanker_cycle, plane_ai,
    tender_heal_beam, update_ally_positions_cache, update_boarding_ropes,
    viking_ram_damage, AllyPositionsCache,
};
use balance::{WINDOW_H, WINDOW_W};
use beam::{beam_apply_damage, update_beams};
use bullet::{bullet_collisions, bullet_update, process_damage_events, GreedAccumulator, PendingDamageQueue, VampireAccumulator};
use effects::{
    apply_hit_fx_visuals, tick_fire_particles, tick_hit_fx, update_hit_particles,
    update_muzzle_flashes,
};
use enemy::{
    artillery_fire, artillery_shell_tick, bomber_detonate, boss_chaos_spawn,
    enemy_ai,
    enemy_death_check, enemy_fire, enemy_landmine_tick, setup_enemy_hp_bar_assets,
    setup_spawn_indicator_assets, sniper_aim_line_tick, sniper_fire, sniper_turret_aim,
    spawn_enemies, tick_spawn_indicators, track_enemy_damage_for_hp_bars, update_enemy_hp_bars,
};
use map::{
    advance_map_anim_timeline, apply_view_mode, boss_patrol_movement,
    close_popup_on_view_change, handle_building_choice_clicks,
    in_combat_view, level_complete_check, level_fail_check, map_begin_phase,
    map_boat_movement, map_click_input, refresh_map_fill, setup_currency_ui,
    setup_level_status_ui, setup_map,
    setup_progress_assets, spawn_boss_patrols,
    sync_owned_slot_visuals,
    tick_buildings,
    update_anim_beams, update_anim_pulses,
    update_building_button_tints, update_building_description, update_building_hover_tooltip,
    update_building_progress_bars, update_currency_ui,
    update_level_status_ui, update_map_slot_labels,
    update_refined_steel_text, update_scrap_text, update_steel_text,
    BuildingTimers, CombatContext, DebugClaimMode, DebugUiVisible, MapAnimTimeline,
    MapState, TriggerMapPhase, ViewMode,
};
// Debug-panel systems live behind the inverse-demo feature flag.
// Imported separately so the demo-build `use` block doesn't pull in
// names the schedule no longer references.
#[cfg(not(feature = "demo"))]
use map::{
    handle_debug_buttons, setup_debug_ui, sync_debug_panel_visibility,
    toggle_debug_ui_on_hash, update_claim_label, update_debug_button_tints,
};
use modes::{
    apply_camera_follow, apply_crt_mode, apply_night_mode, apply_vsync_mode,
    apply_window_mode_setting,
    CameraFollow, CrtMode, GameMode, NightMode, VsyncMode,
};
use palette::{apply_palette, Palette};
use rendering::{
    resize_upscale_sprite, setup_render, sync_ui_scale, update_hash_image,
    update_hud_camera_viewport,
};
use settings::{apply_loaded_settings, persist_settings_on_change};
use rune::{
    tick_buff_stacks, tick_echoes, tick_hp_pickups, tick_magnetic_pickups, tick_on_bleed,
    tick_on_conduit, tick_on_fire, tick_on_frost, tick_on_medic, tick_on_resonate,
    BuffStacks, MedicTimer, ThirstPending,
};
use ship::{apply_velocity, friendly_movement, friendly_ram_damage, setup_world, tick_stunned};
use trails::{update_enemy_trails, update_trail, ShipPath};
use turret::{
    helicopter_ai, mortar_shell_tick, shark_ai, shark_contact_damage,
    sync_amplifier_decor, sync_crows_nest_decor, sync_flamethrower_decor,
    sync_helipad_helicopters, sync_helipad_nose_barrels, sync_sharknet_decor,
    sync_sharknet_sharks, sync_spiked_decor, sync_turret_config, turret_aim_fire,
    TurretConfig,
};
use ui::{
    setup_damage_panel, setup_ui,
    setup_wave_indicator, sync_ally_hp_bars, sync_damage_panel_visibility,
    ui_button_system, update_ally_hp_values, update_damage_panel,
    update_damage_row_icons, update_fps_text, update_hp_bar_pixel_scale,
    update_hp_subdividers, update_map_button,
    update_score_text, update_vsync_label, update_wave_indicator,
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
    /// Win screen — entered when the player clears a 5★ section boss.
    /// Minimal end-of-run overlay; exiting back to MainMenu runs the
    /// same fresh-run reset as the GameOver path.
    Win,
}

/// Owns the in-combat-view Update schedule. Pulled out of the main
/// `App::new()` chain so adding a new combat-tick system means
/// editing one place with room to spare — not nervously rearranging
/// sub-tuples to dodge Bevy's `IntoSystemConfigs` tuple cap.
///
/// The schedule is split into three groups for readability:
/// - **AI** — ship / enemy movement, AI ticks, spawn drips.
/// - **Projectiles** — turret fire, enemy fire, autonomous units,
///   shell + mine + helicopter ticks.
/// - **Damage** — the bullet → damage-event drain with kill-credit
///   and status decay ticks chained after.
///
/// Every group runs `run_if(in_combat_view)`, so out-of-combat
/// states (Pause, Customize, Map, GameOver) freeze the sim
/// uniformly.
struct CombatSimPlugin;

impl Plugin for CombatSimPlugin {
    fn build(&self, app: &mut App) {
        // ---- AI tick group ----
        app.add_systems(
            Update,
            (
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
            )
                .run_if(in_combat_view),
        );

        // ---- Projectile / turret group ----
        app.add_systems(
            Update,
            (
                sync_turret_config,
                // beam_apply_damage needs the BeamPending entities spawned
                // by turret_aim_fire to be visible this frame.
                (turret_aim_fire, beam_apply_damage).chain(),
                enemy_fire,
                sniper_fire,
                sniper_aim_line_tick,
                sniper_turret_aim,
                artillery_fire,
                artillery_shell_tick,
                enemy_landmine_tick,
                bullet_update,
                mortar_shell_tick,
                // HeliPad slots: sync the "one helicopter per equipped slot"
                // invariant first so a freshly spawned heli ticks this frame
                // in `helicopter_ai`. `.chain()` inserts the command-flush
                // sync point that makes that hand-off safe.
                (sync_helipad_helicopters, sync_helipad_nose_barrels, helicopter_ai).chain(),
                // SharkNet autonomous unit: same shape as HeliPad — sync
                // existence first, then AI tick, then contact-damage
                // collision pass. Chained for the same command-flush
                // reason: freshly-spawned sharks need their Transform
                // visible before the AI moves them this frame.
                (sync_sharknet_sharks, shark_ai, shark_contact_damage).chain(),
            )
                .run_if(in_combat_view),
        );

        // ---- Damage pipeline ----
        //
        // Producers (`bullet_collisions`, `tick_echoes`, blade /
        // octopus / mortar / beam systems run earlier in the
        // schedule) push `DamageEvent`s into `PendingDamageQueue`;
        // `process_damage_events` drains them, applies damage, rolls
        // runes, and chains. `enemy_death_check` despawns anything
        // that hit zero AFTER the drain so chain damage gets the
        // same death pipeline. Kill-credit / passive tick systems
        // (magnet pull, HP pickup collect, Rally decay, Medic
        // interval) run BETWEEN the drain and the death-check so
        // event-driven on-kill effects can see the dying enemy's
        // marker components.
        app.add_systems(
            Update,
            (
                (
                    bullet_collisions,
                    tick_echoes,
                    process_damage_events,
                    tick_on_fire,
                    tick_on_frost,
                    tick_on_bleed,
                    tick_on_conduit,
                    tick_on_resonate,
                    (
                        tick_magnetic_pickups,
                        tick_hp_pickups,
                        tick_buff_stacks,
                        tick_on_medic,
                    )
                        .chain(),
                    enemy_death_check,
                )
                    .chain(),
                // Track damage frame-to-frame to spawn / refresh enemy
                // HP bars. Gated to combat — outside combat there's no
                // damage to detect.
                track_enemy_damage_for_hp_bars,
            )
                .run_if(in_combat_view),
        );
    }
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

/// Run-difficulty selector — picked on the HullSelect setup screen
/// alongside hull + voyage length. Three tiers: `0` is gentler than
/// baseline, `1` is the baseline tuning, `2` is harder. Scales enemy
/// max HP at spawn and outgoing enemy damage; everything else stays
/// constant so the existing wave + variant schedules read the same
/// at every difficulty.
#[derive(Resource, Clone, Copy, PartialEq, Eq, Debug)]
pub struct Difficulty(pub u8);

impl Default for Difficulty {
    /// Default to the baseline tier (1) so a fresh install plays the
    /// originally-tuned campaign.
    fn default() -> Self { Self(1) }
}

impl Difficulty {
    pub const VALUES: &'static [u8] = &[0, 1, 2];

    pub fn label(self) -> &'static str {
        match self.0 {
            0 => "0",
            1 => "1",
            2 => "2",
            _ => "?",
        }
    }

    /// Multiplier applied to enemy max HP at spawn (both regular
    /// variants and bosses). Easier difficulty thins enemies, harder
    /// thickens them.
    pub fn hp_mult(self) -> f32 {
        match self.0 {
            0 => 0.75,
            1 => 1.0,
            2 => 1.5,
            _ => 1.0,
        }
    }

    /// Multiplier applied to outgoing enemy damage at the source
    /// (bullet `damage`, contact `contact_damage`, landmine yield).
    pub fn damage_mult(self) -> f32 {
        match self.0 {
            0 => 0.75,
            1 => 1.0,
            2 => 1.5,
            _ => 1.0,
        }
    }

    /// Apply `hp_mult` to a baseline HP value, rounding to the nearest
    /// int and clamping to >= 1 so a fractional roll never kills the
    /// enemy at spawn.
    pub fn scale_hp(self, hp: i32) -> i32 {
        ((hp as f32) * self.hp_mult()).round().max(1.0) as i32
    }

    /// Apply `damage_mult` to a baseline damage value, rounding and
    /// clamping to >= 0 (damage <= 0 short-circuits the damage queue
    /// in `push_initial`).
    pub fn scale_damage(self, dmg: i32) -> i32 {
        ((dmg as f32) * self.damage_mult()).round().max(0.0) as i32
    }
}

/// Wall-clock seconds since the current run started. Pauses on MainMenu
/// and HullSelect; reset on `OnEnter(HullSelect)`.
#[derive(Resource, Default)]
pub struct RunTimer { pub secs: f32 }

pub(crate) fn reset_run_timer(mut timer: ResMut<RunTimer>) {
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
        .add_plugins(
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "Ship Game".into(),
                        resolution: (WINDOW_W, WINDOW_H).into(),
                        // On wasm, let Bevy auto-resize the canvas to
                        // fill its parent element. itch.io embeds the
                        // game in an iframe whose dimensions are set on
                        // the project page; without this flag the canvas
                        // stays at the desktop default and either
                        // overflows or leaves blank gutters.
                        #[cfg(target_arch = "wasm32")]
                        fit_canvas_to_parent: true,
                        ..default()
                    }),
                    ..default()
                })
                // Pipelined rendering races main-world camera-viewport
                // writes against render-world swap-chain texture reads:
                // during minimize / alt-tab / resize the surface
                // transiently becomes 1×1, and the HUD camera's
                // play-area-sized scissor panics wgpu validation. No
                // main-world guard can fully fix this because the
                // race window is on the render thread. Disabling
                // pipelining serialises the two worlds and removes
                // the race entirely. Cost: a small throughput hit on
                // multi-core; in practice unnoticeable for this game.
                .disable::<bevy::render::pipelined_rendering::PipelinedRenderingPlugin>(),
        )
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
            boss_intro::BossIntroPlugin,
            stage_complete::StageCompletePlugin,
            boss_reward::BossRewardPlugin,
            xp::LevelUpPlugin,
            pause::PausePlugin,
            game_over::GameOverPlugin,
            main_menu::MainMenuPlugin,
            customize::CustomizePlugin,
            hull::HullSelectPlugin,
        ))
        .add_plugins((
            anchor_flail::AnchorFlailPlugin,
            flamethrower::FlamethrowerPlugin,
            stats_panel_overlay::StatsPanelOverlayPlugin,
            win_screen::WinScreenPlugin,
            sfx::SfxPlugin,
            CombatSimPlugin,
        ))
        // Workaround for a Bevy 0.16 + WebGL2/ANGLE bug: the default
        // mesh allocator packs many small meshes into shared "slab"
        // buffers and resizes them as meshes are added/freed. On
        // WebGL the resize path can leave a slab at size 0 while a
        // slice still references offset 0 — panicking inside
        // `wgpu::api::buffer::check_buffer_bounds`. Setting
        // `large_threshold = 0` forces every mesh into its own
        // dedicated buffer, skipping the slab path entirely. Tiny
        // perf cost on native (where the panic doesn't fire) — only
        // applied on wasm so the desktop build keeps the original
        // batching.
        //
        // Reference: panic seen as
        //   "slice offset 0 is out of range for buffer of size 0"
        //   from bevy_render::mesh::allocator::allocate_and_free_meshes
        // on Chrome / ANGLE on itch.io.
        .insert_resource({
            #[cfg(target_arch = "wasm32")]
            {
                bevy::render::mesh::allocator::MeshAllocatorSettings {
                    large_threshold: 0,
                    ..Default::default()
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                bevy::render::mesh::allocator::MeshAllocatorSettings::default()
            }
        })
        .insert_resource(ClearColor(Color::srgb(0.05, 0.05, 0.08)))
        .insert_resource(Score(0))
        .insert_resource(CampaignProgress::default())
        // Player earns scrap from the first wave clear onward; no
        // starting purse.
        .insert_resource(Scrap(0))
        .insert_resource(Steel::default())
        .insert_resource(RefinedSteel::default())
        .insert_resource(SpawnTimer { t: 0.0, elapsed: 0.0 })
        .insert_resource(cfg)
        .insert_resource(DamageStats::default())
        .insert_resource(stats::PlayerStats::default())
        .insert_resource(DebugUiVisible::default())
        .insert_resource(synergy::Synergies::default())
        .insert_resource(PendingDamageQueue::default())
        .insert_resource(VampireAccumulator::default())
        .insert_resource(GreedAccumulator::default())
        .insert_resource(ThirstPending::default())
        .insert_resource(BuffStacks::default())
        .insert_resource(MedicTimer::default())
        .insert_resource(modes::ScreenShake::default())
        .insert_resource(RunTimer::default())
        .init_state::<AppState>()
        .insert_resource(Palette::aap64_naval())
        .insert_resource(ShipPath::default())
        .insert_resource(NightMode::default())
        .insert_resource(CrtMode::default())
        .insert_resource(VsyncMode::default())
        .insert_resource(modes::WindowModeSetting::default())
        .insert_resource(modes::ResolutionSetting::default())
        .insert_resource(GameMode::default())
        .insert_resource(CameraFollow::default())
        .insert_resource(ViewMode::default())
        .insert_resource(map::MapSize::default())
        .insert_resource(Difficulty::default())
        .insert_resource(MapState::new(map::MapSize::default().sections()))
        .insert_resource(BuildingTimers::default())
        .insert_resource(MapAnimTimeline::default())
        .insert_resource(CombatContext::default())
        .insert_resource(DebugClaimMode::default())
        .add_event::<TriggerMapPhase>()
        .add_event::<rune::KillEvent>()
        .insert_resource(AllyPositionsCache::default())
        .add_systems(Startup, (
            setup_render, setup_world, setup_ui, setup_map,
            // After setup_map so 5★ polygons exist for reject-sampling.
            spawn_boss_patrols,
            // Debug panel is stripped from demo builds — call sites
            // that read DebugUiVisible are all gated below, so a
            // missing panel entity is harmless.
            #[cfg(not(feature = "demo"))]
            setup_debug_ui,
            setup_currency_ui, setup_progress_assets,
            setup_level_status_ui, setup_enemy_hp_bar_assets,
            setup_damage_panel,
            setup_wave_indicator, setup_spawn_indicator_assets,
        ).chain())
        // Bridge runs first so the rest of Update sees synced flags.
        .add_systems(Update, sync_state_to_open_resources)
        // Weapon-decor sync — runs unconditionally; each system
        // self-gates on `cfg.is_changed()` so they're cheap when the
        // player isn't editing the loadout. Mirrors the blade-decor
        // registration pattern for the no-base-fire weapons that
        // previously had no deck visual.
        .add_systems(Update, (
            sync_spiked_decor,
            sync_amplifier_decor,
            sync_flamethrower_decor,
            sync_crows_nest_decor,
            sync_sharknet_decor,
        ))
        .add_systems(OnEnter(AppState::Map), enter_map_view)
        .add_systems(OnEnter(AppState::Playing), enter_combat_view)
        // Map→Playing is the canonical stage-start hook: refill HP +
        // wipe arena debris from last stage. (Permanent ally roster
        // respawn is owned by `BossRewardPlugin`, which also hooks
        // OnExit(Map).)
        .add_systems(OnExit(AppState::Map), refill_and_clean_for_next_stage)
        // Run-timer tick — counts wall-clock seconds since the run
        // started, paused on MainMenu/HullSelect.
        .add_systems(Update, tick_run_timer)
        // The map-side half of the StageComplete handoff — the plugin
        // owns the timer + UI; `queue_next_stage_combat` lives in
        // `map` so it stays alongside the rest of `CombatContext`
        // setup.
        .add_systems(OnExit(AppState::StageComplete), map::queue_next_stage_combat)
        .add_systems(Update, (
            // night_mode → palette must order so a toggle propagates
            // to the camera in the same frame.
            (apply_night_mode, apply_palette, update_hash_image).chain(),
        ))
        // In-combat-view schedule lives in `CombatSimPlugin` (above),
        // registered via `add_plugins`. The block was lifted out of
        // this builder chain so each combat-tick group could be a
        // small tuple without the `IntoSystemConfigs` cap forcing
        // sub-tuple bundling here.
        // The bar updater runs unconditionally so orphan bars get
        // cleaned up regardless of state. If the player dies (→
        // GameOver) while an enemy still had a visible bar, leaving
        // this gated to combat freezes the bar in place forever —
        // visible bar with no owner. Letting it tick everywhere
        // also means bars naturally despawn during pause / shop.
        .add_systems(Update, update_enemy_hp_bars)
        // Transition detectors — "we just left Playing". Gated on
        // `in_state(Playing)` rather than the broader `in_combat_view`
        // (which includes `StageComplete`) because the conditions they
        // detect (budget==0 + no enemies, or friendly HP at 0) remain
        // satisfied across the state transition. Running them during
        // `StageComplete` would re-fire `NextState=StageComplete` every
        // frame, racing `tick_stage_complete`'s advance to LevelUp /
        // Customize. Sim-side work (motion, bullets, collisions) still
        // ticks during the buffer; only the detectors are scoped to
        // Playing.
        .add_systems(
            Update,
            (level_complete_check, level_fail_check)
                .run_if(in_state(AppState::Playing)),
        )
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
            tick_fire_particles,
            update_score_text,
            update_fps_text,
            update_vsync_label,
            ui_button_system,
            // Sub-tuple keeps the outer count under Bevy's 20-system cap.
            (
                sync_ui_scale,
                resize_upscale_sprite,
                update_hud_camera_viewport,
                apply_crt_mode,
                apply_vsync_mode,
                apply_window_mode_setting,
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
                // Debug panel + hash-toggle stripped in demo builds.
                #[cfg(not(feature = "demo"))]
                (
                    handle_debug_buttons, update_debug_button_tints, update_claim_label,
                    toggle_debug_ui_on_hash, sync_debug_panel_visibility,
                ),
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
            apply_loaded_settings,
            persist_settings_on_change,
        ))
        // Note: Click handlers are gated to their owning state because
        // Bevy UI picking fires `Interaction::Pressed` on hidden Nodes
        // (overlapping full-screen overlays), so a click on one screen
        // would otherwise silently trigger a transition on another.
        // Per-screen plugins handle that wiring themselves.
        .run();
}
