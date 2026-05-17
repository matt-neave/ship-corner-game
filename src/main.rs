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
mod menu_kit;
mod proc_fx;
#[cfg(not(target_arch = "wasm32"))]
mod multiplayer;
mod octopus;
mod onboarding;
mod fonts;
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
    artillery_fire, artillery_shell_tick, boss_chaos_spawn,
    enemy_ai,
    cull_runaway_enemies, enemy_death_check, enemy_fire, enemy_landmine_tick, setup_enemy_hp_bar_assets,
    setup_spawn_indicator_assets, sniper_aim_line_tick, sniper_fire, sniper_turret_aim,
    spawn_enemies, tick_spawn_indicators, track_enemy_damage_for_hp_bars, update_enemy_hp_bars,
};
use map::{
    advance_map_anim_timeline, apply_view_mode, boss_patrol_movement,
    clear_anims_on_view_change,
    in_combat_view, level_complete_check, level_fail_check, map_begin_phase,
    map_boat_movement, map_click_input, refresh_map_fill,
    setup_level_status_ui, setup_map,
    spawn_boss_patrols,
    update_anim_beams, update_anim_pulses,
    update_level_status_ui,
    CombatContext, DebugClaimMode, DebugUiVisible, MapAnimTimeline,
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
use ship::{
    apply_velocity, despawn_player_world, friendly_movement, friendly_ram_damage,
    setup_world, spawn_player_world, tick_stunned,
};
use trails::{update_enemy_trails, update_trail, ShipPath};
use turret::{
    helicopter_ai, mortar_shell_tick, shark_ai, shark_contact_damage,
    sync_amplifier_decor, sync_crows_nest_decor, sync_flamethrower_decor,
    sync_helipad_helicopters, sync_helipad_nose_barrels, sync_sharknet_decor,
    sync_sharknet_sharks, sync_spiked_decor, sync_turret_config, turret_aim_fire,
    TurretConfig,
};
use ui::{
    setup_ui,
    setup_wave_indicator, sync_ally_hp_bars, sync_hud_dev_buttons_visibility,
    ui_button_system, update_ally_hp_values,
    update_fps_text, update_hp_bar_pixel_scale,
    update_map_button, update_shield_bar,
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
    /// Multiplayer lobby — host + connected clients sit here between
    /// the handshake and the START button. Roster, kick, leave, and
    /// (host-only) START controls live in `multiplayer/lobby.rs`. The
    /// host enters on clicking HOST; clients enter on receiving
    /// Welcome. Both transition to Playing simultaneously when the
    /// host clicks START (via `StateChange` broadcast).
    Lobby,
    /// Multiplayer client-only — host is in a state the client
    /// doesn't participate in (Customize / HullSelect / Map / etc).
    /// Client sees a "WAITING FOR HOST" overlay until host returns
    /// to Playing. Stops the client from accidentally interacting
    /// with host-only menus and frees the client from running their
    /// own divergent menu logic.
    WaitingForHost,
}

impl AppState {
    /// Stable wire-format discriminant for multiplayer state-sync.
    /// Append-only — renumbering breaks compatibility with peers
    /// running an older build. Used by `NetMsg::StateChange` so a
    /// host's state transitions can drive matching transitions on
    /// every client.
    pub fn to_u8(self) -> u8 {
        match self {
            AppState::MainMenu       => 0,
            AppState::Playing        => 1,
            AppState::StageComplete  => 2,
            AppState::LevelUp        => 3,
            AppState::HullSelect     => 4,
            AppState::Customize      => 5,
            AppState::Map            => 6,
            AppState::Paused         => 7,
            AppState::GameOver       => 8,
            AppState::BossReward     => 9,
            AppState::BossIntro      => 10,
            AppState::Win            => 11,
            AppState::Lobby          => 12,
            AppState::WaitingForHost => 13,
        }
    }
    /// Inverse of `to_u8`. `None` for unknown discriminants so older
    /// clients can silently skip unknown future states.
    pub fn from_u8(n: u8) -> Option<Self> {
        Some(match n {
             0 => AppState::MainMenu,
             1 => AppState::Playing,
             2 => AppState::StageComplete,
             3 => AppState::LevelUp,
             4 => AppState::HullSelect,
             5 => AppState::Customize,
             6 => AppState::Map,
             7 => AppState::Paused,
             8 => AppState::GameOver,
             9 => AppState::BossReward,
            10 => AppState::BossIntro,
            11 => AppState::Win,
            12 => AppState::Lobby,
            13 => AppState::WaitingForHost,
             _ => return None,
        })
    }
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
                tick_stunned,
                apply_velocity,
                stats::shield_recharge_system,
                // Host-authoritative enemy simulation. Client mirrors
                // are visual-only — their Transform/HP are driven by
                // EnemySnapshot. Running these on the client too
                // would (a) spawn duplicate enemy bullets from the
                // mirror's position (peer takes ~2x damage), (b)
                // write Velocity that fights `smooth_mirror_transforms`,
                // and (c) cause `enemy_death_check` to grant scrap
                // from a race window between HP=0 snapshot and the
                // omit-from-next-snapshot reconcile.
                #[cfg(not(target_arch = "wasm32"))]
                (
                    enemy_ai,
                    friendly_ram_damage,
                    spawn_enemies,
                    boss_chaos_spawn,
                ).run_if(not(multiplayer::enemies::is_client)),
                #[cfg(target_arch = "wasm32")]
                (
                    enemy_ai,
                    friendly_ram_damage,
                    spawn_enemies,
                    boss_chaos_spawn,
                ),
            )
                .run_if(in_combat_view),
        );

        // ---- Projectile / turret group ----
        //
        // Enemy-side firing (enemy_fire, sniper_*, artillery_*,
        // enemy_landmine_tick) is host-authoritative: the host fires
        // the enemy's bullets, the bullets that hit the local player
        // do the damage locally. The client never spawns enemy
        // bullets from mirrors. If it did, both peers would
        // experience independent firing salvos from the same enemy
        // and the client would take ~double damage.
        //
        // Friendly-side firing (turret_aim_fire, helicopter_ai,
        // shark_ai, mortar_shell_tick, bullet_update) runs on both
        // peers — each peer drives their OWN ship's turrets and
        // bullets locally; damage to mirrors is relayed to host.
        app.add_systems(
            Update,
            (
                sync_turret_config,
                (turret_aim_fire, beam_apply_damage).chain(),
                bullet_update,
                mortar_shell_tick,
                (sync_helipad_helicopters, sync_helipad_nose_barrels, helicopter_ai).chain(),
                (sync_sharknet_sharks, shark_ai, shark_contact_damage).chain(),
            )
                .run_if(in_combat_view),
        );
        // Host-only enemy firing — see comment above.
        app.add_systems(
            Update,
            (
                enemy_fire,
                sniper_fire,
                sniper_aim_line_tick,
                sniper_turret_aim,
                artillery_fire,
                artillery_shell_tick,
                enemy_landmine_tick,
            )
                .run_if(in_combat_view)
                .run_if(not(multiplayer::enemies::is_client)),
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
                    // Host-authoritative: client mirrors are despawned by
                    // `apply_enemy_snapshot`'s reconcile pass when the
                    // host's snapshot omits an id. Running death-check
                    // on client would double-despawn AND grant scrap
                    // from a race window between HP=0 and the omit.
                    #[cfg(not(target_arch = "wasm32"))]
                    enemy_death_check.run_if(not(multiplayer::enemies::is_client)),
                    #[cfg(target_arch = "wasm32")]
                    enemy_death_check,
                    // Safety net: catches enemies that an AI bug
                    // has thrown out of bounds before they stall
                    // try_advance_fighting forever. Also host-only —
                    // mirrors don't have AI to misbehave.
                    #[cfg(not(target_arch = "wasm32"))]
                    cull_runaway_enemies.run_if(not(multiplayer::enemies::is_client)),
                    #[cfg(target_arch = "wasm32")]
                    cull_runaway_enemies,
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
    // LocalPlayer (not just Friendly) — host has two Friendlies in
    // MP (local + remote-peer ghost). `single_mut()` on plain
    // Friendly would Err and silently skip the HP refill, leaving
    // the host's player with whatever HP they had at stage end.
    mut friendly: Query<&mut components::Health, With<components::LocalPlayer>>,
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
/// The only currency. Earned from kills + wave clears + per-stage
/// interest; spent in the customize shop on weapons, runes, mods.
#[derive(Resource, Default)]
pub struct Scrap(pub u32);

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
    /// Brotato / SNKRX convention: tier 0 IS the baseline. Higher
    /// tiers are the "harder than default" progression rungs the
    /// player works up through. No "easier than default" option —
    /// if 0 already feels too hard, the answer is build / hull
    /// choice, not a softer difficulty.
    fn default() -> Self { Self(0) }
}

impl Difficulty {
    /// Five tiers laid out in one row in HullSelect. 0 = the
    /// originally-tuned baseline (1.00× HP / 1.00× damage); each
    /// step adds +30% to both enemy HP and outgoing enemy damage.
    /// Tier 4 = 2.20× — meaningful endgame challenge without
    /// pushing into HP-sponge territory.
    pub const VALUES: &'static [u8] = &[0, 1, 2, 3, 4];

    pub fn label(self) -> &'static str {
        match self.0 {
            0 => "0", 1 => "1", 2 => "2",
            3 => "3", 4 => "4",
            _ => "?",
        }
    }

    /// Multiplier applied to enemy max HP at spawn (both regular
    /// variants and bosses). Tier 0 = 1.00×, each tier above adds
    /// 30% — so tier 4 = 2.20× HP.
    pub fn hp_mult(self) -> f32 {
        let t = self.0.clamp(0, 4) as f32;
        (1.0 + t * 0.3).max(0.1)
    }

    /// Multiplier applied to outgoing enemy damage at the source.
    /// Same shape + step as `hp_mult` so HP and damage scale in
    /// lockstep through the 5 tiers.
    pub fn damage_mult(self) -> f32 {
        let t = self.0.clamp(0, 4) as f32;
        (1.0 + t * 0.3).max(0.1)
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
        // Multiplayer plugin is native-only — UDP sockets don't exist
        // in browsers, and `bincode` / `local-ip-address` are cfg-
        // gated out of the WASM build's deps in Cargo.toml.
        .add_plugins({
            #[cfg(not(target_arch = "wasm32"))]
            { multiplayer::MultiplayerPlugin }
            #[cfg(target_arch = "wasm32")]
            { bevy::app::EmptyPlugin }
        })
        .add_plugins((
            anchor_flail::AnchorFlailPlugin,
            flamethrower::FlamethrowerPlugin,
            stats_panel_overlay::StatsPanelOverlayPlugin,
            win_screen::WinScreenPlugin,
            sfx::SfxPlugin,
            sfx::MusicPlugin,
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
        .insert_resource(modes::BackgroundSetting::default())
        .insert_resource(GameMode::default())
        .insert_resource(CameraFollow::default())
        .insert_resource(ViewMode::default())
        .insert_resource(map::MapSize::default())
        .insert_resource(Difficulty::default())
        .insert_resource(MapState::new(map::MapSize::default().sections()))
        .insert_resource(MapAnimTimeline::default())
        .insert_resource(CombatContext::default())
        .insert_resource(DebugClaimMode::default())
        .add_event::<TriggerMapPhase>()
        .add_event::<rune::KillEvent>()
        .add_event::<proc_fx::ProcFxFired>()
        .add_event::<proc_fx::BulletFiredEvent>()
        .insert_resource(AllyPositionsCache::default())
        .add_systems(Startup, fonts::setup_pixel_font)
        .add_systems(Startup, (
            setup_render, setup_world, setup_ui, setup_map,
            // After setup_map so 5★ polygons exist for reject-sampling.
            spawn_boss_patrols,
            // Debug panel is stripped from demo builds — call sites
            // that read DebugUiVisible are all gated below, so a
            // missing panel entity is harmless.
            #[cfg(not(feature = "demo"))]
            setup_debug_ui,
            setup_level_status_ui, setup_enemy_hp_bar_assets,
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
        // OnEnter(Playing): set the camera-gating ViewMode AND spawn
        // the friendly hull / arena border / wake trail if they don't
        // already exist. `spawn_player_world` is idempotent, so
        // re-entries (Pause → Playing, Map → Playing) short-circuit
        // and leave the alive ship in place.
        .add_systems(OnEnter(AppState::Playing), (enter_combat_view, spawn_player_world))
        // OnEnter(MainMenu): tear the play world down so the menu
        // screen renders against an empty stage — no stale player
        // ship sitting at origin, no border framing the void. The
        // menu owns its own fleet of decorative pirate hulls.
        .add_systems(OnEnter(AppState::MainMenu), despawn_player_world)
        // Map→Playing is the canonical stage-start hook: refill HP +
        // wipe arena debris from last stage. (Permanent ally roster
        // respawn is owned by `BossRewardPlugin`, which also hooks
        // OnExit(Map).)
        // Wipe leftover arena entities + refill HP only on the actual
        // Map → Playing handoff, NOT every Map exit. Without the
        // `OnTransition` gate, pausing from Map (Map → Paused) would
        // fire this on the way down and reset the stage that's
        // queued up to start. Same for the boss-reward path.
        .add_systems(
            OnTransition { exited: AppState::Map, entered: AppState::Playing },
            refill_and_clean_for_next_stage,
        )
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
            ui_kit::update_chunky_button_visuals,
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
            // HP bars are visible in both map and combat view, BUT
            // not on the landing screen. `update_wave_ui` writes
            // Visibility::Inherited on every `ViewMode` change, which
            // would otherwise un-hide the player's HP bar the moment
            // MainMenu sets ViewMode::Combat to wake up the menu fleet
            // camera. Gating the whole block keeps the menu clean.
            update_wave_ui,
            update_shield_bar,
            update_hp_bar_pixel_scale,
            sync_ally_hp_bars,
            update_ally_hp_values,
        ).run_if(not(in_state(AppState::MainMenu))))
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
            (clear_anims_on_view_change, map_begin_phase, advance_map_anim_timeline).chain(),
            update_anim_pulses,
            update_anim_beams,
            map_click_input,
            map_boat_movement,
            // The outer block runs systems in BOTH views; patrol must
            // self-gate to Map so it doesn't tick during combat.
            boss_patrol_movement.run_if(in_state(AppState::Map)),
            refresh_map_fill,
            update_map_button,
            (
                // Debug panel + hash-toggle stripped in demo builds.
                #[cfg(not(feature = "demo"))]
                (
                    handle_debug_buttons, update_debug_button_tints, update_claim_label,
                    toggle_debug_ui_on_hash, sync_debug_panel_visibility,
                ),
                // HUD dev buttons (FPS / VSYNC / FOLLOW) gated on
                // the same `DebugUiVisible` toggle as the debug
                // panel. Outside the `not(demo)` cfg because demo
                // builds should ALSO hide them (the resource
                // default is `false` everywhere now).
                sync_hud_dev_buttons_visibility,
                update_wave_indicator,
                tick_spawn_indicators,
                xp::update_xp_bar,
            ),
            update_level_status_ui,
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
