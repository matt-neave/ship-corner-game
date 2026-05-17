//! Map view — a zoomed-out second view where the player picks where to
//! sail next. The same square play area is reused; we just swap what the
//! play camera renders by flipping its `RenderLayers` between
//! `PLAY_LAYER` (combat) and `MAP_LAYER` (map). One camera, two views.
//!
//! Layout: 10 hand-authored irregular sections. Adjacent sections share
//! their boundary corners exactly + use a deterministic `wobble_for_edge`
//! curve so dividers look hand-drawn but match across regions. Outer
//! edges stay straight so the map fills the square cleanly.
//!
//! Movement reuses the in-game pattern (`approach_angle` toward a desired
//! heading, fixed forward speed) — the destination is set by clicking an
//! adjacent section instead of following the cursor continuously.
//!
//! Module split
//! ------------
//! - `build`     — section authoring + polygon/wobble + meshes + fill image
//!                  + view-mode camera toggle + map-fill refresh.
//! - `setup`     — initial spawn (fill sprite, dividers, slot tiles, boat)
//!                  + slot-visual reconciliation + slot-label syncing.
//! - `buildings` — popup + progress bars + tooltip + click handler +
//!                  per-frame economy tick + level resolution.
//! - `hud`       — currency + level status banner + debug panel.
//! - `anim`      — phase-animation timeline + pulse/beam drivers.
//! - `input`     — map click handling + boat steering.

use bevy::prelude::*;

mod anim;
mod boss_patrol;
mod build;
mod buildings;
mod hud;
mod input;
mod procgen;
mod setup;

pub use anim::{advance_map_anim_timeline, map_begin_phase, update_anim_beams, update_anim_pulses};
pub use boss_patrol::{boss_patrol_movement, spawn_boss_patrols, BossPatrol};
pub use build::{apply_view_mode, refresh_map_fill};
pub use buildings::{
    clear_anims_on_view_change, level_complete_check, queue_next_stage_combat,
    level_fail_check,
};
pub use hud::{setup_level_status_ui, update_level_status_ui, DebugUiVisible};
#[cfg(not(feature = "demo"))]
pub use hud::{
    handle_debug_buttons, setup_debug_ui, sync_debug_panel_visibility,
    toggle_debug_ui_on_hash, update_claim_label, update_debug_button_tints,
};
pub use input::{map_boat_movement, map_click_input};
pub use setup::setup_map;

// ---------- Layer + Z constants ----------

/// Render layer for everything visible only in map view. `apply_view_mode`
/// flips the play camera between `PLAY_LAYER` and this.
pub const MAP_LAYER: usize = 3;

/// Z-band used by map entities so they layer cleanly:
///   0.5 = section fills,    0.7  = boundary segments,
///   0.90 = star marks,
///   1.0  = phase animations (pulses/beams),
///   1.5  = boat token.
pub(crate) const Z_FILL:      f32 = 0.5;
pub(crate) const Z_OUTLINE:   f32 = 0.7;
pub(crate) const Z_SLOT_STAR: f32 = 0.90;
pub(crate) const Z_ANIM:      f32 = 1.0;
pub(crate) const Z_BOAT:      f32 = 1.5;

/// Visual scale of the map boat token relative to its in-combat size.
pub(crate) const MAP_BOAT_SCALE: f32 = 0.5;

/// Star-mark geometry — small filled squares stacked horizontally above
/// each section's centre. With `STAR_SIZE = 2` and `STAR_GAP = 2`,
/// stars render as 2-px filled squares with 2-px gaps.
pub(crate) const STAR_SIZE: f32 = 2.0;
pub(crate) const STAR_GAP:  f32 = 2.0;
pub(crate) const STAR_Y_OFFSET: f32 = 9.0;

// Animation tuning — short, snappy. Tweak here.
pub(crate) const ANIM_PULSE_PEAK_ALPHA: f32 = 0.55;
pub(crate) const ANIM_BEAM_PEAK_ALPHA:  f32 = 0.85;
pub(crate) const ANIM_PULSE_PEAK_SCALE: f32 = 1.30;
pub(crate) const ANIM_PULSE_SIZE: f32 = 14.0;
pub(crate) const ANIM_BEAM_THICKNESS: f32 = 1.4;

// ---------- Resources ----------

#[derive(Resource, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Map,
    Combat,
}
impl Default for ViewMode {
    /// Game starts in combat — the player drops straight into level 1.
    fn default() -> Self { ViewMode::Combat }
}

/// Snapshot of the section that triggered the current combat. Written
/// by `map_boat_movement` when the boat crosses into an unowned zone;
/// `spawn_enemies` reads it to scale enemy density by star rating.
///
/// Wave-based combat: `wave_count` total waves per stage, indexed by
/// `wave_idx`. `wave_phase` drives the spawner's state machine —
/// `Spawning` drips this wave's allotment, `Fighting` waits for the
/// arena to clear, `Cooldown` is the breathe-between-waves timer.
#[derive(Resource)]
pub struct CombatContext {
    pub stars: u8,
    /// Total enemies still to spawn across every remaining wave this
    /// stage. `level_complete_check` fires when this hits 0 and the
    /// arena is empty.
    pub enemy_budget: u32,
    /// Snapshot of `enemy_budget` at level start (HUD bar denom).
    pub enemy_total: u32,
    pub wave_count: u8,
    pub wave_idx: u8,
    /// Enemies left to spawn in the *current* wave. Hits 0 →
    /// `Fighting`.
    pub wave_remaining: u32,
    pub wave_phase: WavePhase,
    /// Between-wave breather timer, ticked while `Cooldown`.
    pub wave_cd: f32,
    /// Tick interval inside `Spawning` so a wave drips in over ~1s
    /// rather than appearing in a single frame.
    pub spawn_tick: f32,
    /// True when the active wave should use the boss variant mix.
    /// `balance::is_boss_wave` is the predicate; currently a stub.
    pub is_boss_wave: bool,
    /// Pre-rolled spawn positions for the current wave + the indicator
    /// entity already showing where each one will appear. Filled on
    /// `Spawning` entry, drained one-per-spawn. Empty in `Fighting` /
    /// `Cooldown`.
    pub pending_spawns: Vec<PendingSpawn>,
    /// `Some(class)` when the active stage was entered on a 5★ section
    /// with a boss assigned. `spawn_enemies` consumes this on the first
    /// frame of the final wave, drops one boss into the arena via
    /// `spawn_boss`, and clears it back to `None` so it doesn't
    /// re-fire each frame.
    pub boss_pending: Option<crate::ally::ShipClass>,
    /// Snapshot of `CampaignProgress.battles_cleared` taken at stage
    /// start. Drives the stage-progression difficulty multiplier in
    /// `balance::wave_size` + the on-screen cap in `enemy_cap`. Held
    /// here (not read live each frame) so the difficulty is fixed for
    /// the whole stage rather than jumping when the player completes
    /// a battle mid-stage.
    pub battles_cleared: u32,
    /// Cooldown timer for the "chaos drip" that runs only while a boss
    /// is alive in the arena. Keeps the fight from devolving into a
    /// 1-v-1 chase, and is especially important for the Tender boss
    /// which has no offensive abilities. Driven by `boss_chaos_spawn`.
    pub boss_chaos_cd: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct PendingSpawn {
    pub pos: Vec2,
    pub indicator: Entity,
    /// Pre-rolled variant. `None` means "let the drip site roll one
    /// from the stage's mix" (the default path). `Some(v)` forces a
    /// specific variant — used by Swarmer cluster expansion so a
    /// rolled Swarmer balloons into 4-7 PendingSpawns that all
    /// spawn the same variant from one direction.
    pub variant: Option<crate::enemy::EnemyVariant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WavePhase {
    /// Drip this wave's enemies in.
    Spawning,
    /// Wave fully spawned; wait for the arena to clear.
    Fighting,
    /// All enemies dead — short pause before the next wave.
    Cooldown,
}

impl WavePhase {
    /// Stable wire-format discriminant for multiplayer wave sync.
    /// Append-only — same rules as the other `to_u8` enums.
    pub fn to_u8(self) -> u8 {
        match self {
            WavePhase::Spawning => 0,
            WavePhase::Fighting => 1,
            WavePhase::Cooldown => 2,
        }
    }
    pub fn from_u8(n: u8) -> Option<Self> {
        Some(match n {
            0 => WavePhase::Spawning,
            1 => WavePhase::Fighting,
            2 => WavePhase::Cooldown,
            _ => return None,
        })
    }
}

/// Seconds of breathing room between waves. Kept short so cleared
/// arenas don't feel like a wait — the player wants to keep shooting.
pub const BETWEEN_WAVES_DURATION: f32 = 0.6;

impl Default for CombatContext {
    fn default() -> Self {
        let mut c = Self {
            stars: 1,
            enemy_budget: 0,
            enemy_total: 0,
            wave_count: 0,
            wave_idx: 0,
            wave_remaining: 0,
            wave_phase: WavePhase::Spawning,
            wave_cd: 0.0,
            spawn_tick: 0.0,
            is_boss_wave: false,
            pending_spawns: Vec::new(),
            boss_pending: None,
            battles_cleared: 0,
            boss_chaos_cd: 0.0,
        };
        c.reset_for(1, 0);
        c
    }
}

impl CombatContext {
    /// On-screen enemy cap for drip spawning. Base `6 × stars`, plus
    /// `+4` per battle cleared up to the 12th stage. Late campaign
    /// 5★ stages run hot (~78 concurrent) so the swarm reads as a
    /// swarm, not a polite queue. Hard cap keeps the renderer safe.
    pub fn enemy_cap(&self) -> usize {
        let base = 6 * self.stars.max(1) as usize;
        let progress = (self.battles_cleared.min(12) as usize) * 4;
        (base + progress).min(100)
    }

    /// Initialise this context for a fresh stage at the given star
    /// tier. Call from every combat-start site (entering combat from
    /// the map, queueing the next round in `level_complete_check`,
    /// etc.) so wave + budget state stays consistent. `battles_cleared`
    /// is snapshotted from `CampaignProgress` so all wave sizes for
    /// THIS stage use the same multiplier even if a battle clears
    /// mid-stage somehow.
    pub fn reset_for(&mut self, stars: u8, battles_cleared: u32) {
        let wave_count = crate::balance::waves_for_stars(stars);
        let total: u32 = (0..wave_count)
            .map(|i| crate::balance::wave_size(i, stars, battles_cleared))
            .sum();
        self.stars = stars;
        self.battles_cleared = battles_cleared;
        self.wave_count = wave_count;
        self.wave_idx = 0;
        self.wave_remaining = crate::balance::wave_size(0, stars, battles_cleared);
        self.wave_phase = WavePhase::Spawning;
        self.wave_cd = 0.0;
        self.spawn_tick = 0.0;
        self.is_boss_wave = crate::balance::is_boss_wave(0, wave_count);
        self.enemy_budget = total;
        self.enemy_total = total;
        self.boss_chaos_cd = 0.0;
        // Pending list is owned by `spawn_enemies`. Caller is
        // responsible for despawning any orphan indicator entities
        // before reset (the OnEnter(Customize) cleanup hook covers
        // the normal flow).
        self.pending_spawns.clear();
    }

    /// Move to the next wave. Sets phase to `Spawning`, refills
    /// `wave_remaining`, and re-evaluates the boss flag. No-op if
    /// already on the last wave (the caller checks that before
    /// calling).
    pub fn advance_wave(&mut self) {
        self.wave_idx = self.wave_idx.saturating_add(1);
        self.wave_remaining = crate::balance::wave_size(self.wave_idx, self.stars, self.battles_cleared);
        self.wave_phase = WavePhase::Spawning;
        self.wave_cd = 0.0;
        self.spawn_tick = 0.0;
        self.is_boss_wave = crate::balance::is_boss_wave(self.wave_idx, self.wave_count);
        // Spawning state will refill on next tick.
        self.pending_spawns.clear();
    }

    /// Pure-logic half of the spawner's Fighting branch. Returns true
    /// iff the phase just transitioned Fighting → Cooldown (caller
    /// then performs the side-effects: grant scrap, queue level-ups).
    ///
    /// The early-return on the non-Fighting phase is intentional —
    /// the caller drives the state machine via a match, but extracting
    /// the transition here lets unit tests assert it without dragging
    /// in the spawner's graphics deps. Catches the regression class
    /// where a stuck wave silently never advances.
    pub fn try_advance_fighting(&mut self, enemy_count: usize) -> bool {
        if self.wave_phase != WavePhase::Fighting { return false; }
        if enemy_count != 0 { return false; }
        self.wave_phase = WavePhase::Cooldown;
        self.wave_cd = BETWEEN_WAVES_DURATION;
        true
    }

    /// Pure-logic half of the spawner's Cooldown branch. Returns true
    /// iff the cooldown finished AND this isn't the last wave — i.e.,
    /// `advance_wave` was called. On the last wave, returns false
    /// (the level-complete check takes over there).
    pub fn try_advance_cooldown(&mut self, dt: f32) -> bool {
        if self.wave_phase != WavePhase::Cooldown { return false; }
        self.wave_cd -= dt;
        if self.wave_cd > 0.0 { return false; }
        if self.wave_idx + 1 < self.wave_count {
            self.advance_wave();
            true
        } else {
            false
        }
    }
}

pub struct MapSection {
    pub id: u32,
    pub corners: Vec<Vec2>,
    pub polygon: Vec<Vec2>,
    pub center: Vec2,
    pub adjacencies: Vec<u32>,
    pub stars: u8,
    /// Boss assigned to this section. Populated for 5★ sections at
    /// `MapState::new` time with a random `ShipClass`; `None` for
    /// every other tier. Drives both the patrol entity rendered on
    /// the map view and the boss spawned during the section's final
    /// combat wave.
    pub boss_class: Option<crate::ally::ShipClass>,
}

#[derive(Resource)]
pub struct MapState {
    pub sections: Vec<MapSection>,
    pub current: u32,
    pub owned: Vec<bool>,
    pub boat_target: Option<Vec2>,
}

/// Player-selectable map topology size. Chosen in the dockyard
/// (hull-select screen) before a run starts; consumed by
/// `MapState::new` to pick the section count.
#[derive(Resource, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MapSize {
    /// 10 sections — tight map, fast runs.
    Small,
    #[default]
    /// 15 sections — middle of the road.
    Medium,
    /// 20 sections — sprawling campaign with more route choices.
    Large,
}

impl MapSize {
    pub fn sections(self) -> usize {
        match self {
            MapSize::Small => 10,
            MapSize::Medium => 15,
            MapSize::Large => 20,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            MapSize::Small => "SMALL",
            MapSize::Medium => "MEDIUM",
            MapSize::Large => "LARGE",
        }
    }
    pub const ALL: &'static [MapSize] = &[
        MapSize::Small,
        MapSize::Medium,
        MapSize::Large,
    ];
}

impl MapState {
    pub fn new(target_sections: usize) -> Self {
        let mut rng = rand::thread_rng();
        use rand::seq::SliceRandom;
        use rand::Rng;
        let mut sections = procgen::build_random_map(&mut rng, target_sections);
        let stars = compute_stars(&sections, 0);
        let boss_pool = [
            crate::ally::ShipClass::PirateShip,
            crate::ally::ShipClass::Carrier,
            crate::ally::ShipClass::Submarine,
            crate::ally::ShipClass::Minelayer,
            crate::ally::ShipClass::Tender,
            crate::ally::ShipClass::Blackbeard,
            crate::ally::ShipClass::OilTanker,
            crate::ally::ShipClass::Viking,
        ];
        for (i, s) in sections.iter_mut().enumerate() {
            s.stars = stars[i];
            // Boss assignment by star tier — 5★ always, 4★ commonly,
            // 3★ occasionally. The sprinkled patrols on lower tiers
            // make the map feel populated with telegraphed threats
            // rather than two fixed end-zone bosses.
            let boss_chance: f32 = match s.stars {
                5 => 1.0,
                4 => 0.25,
                3 => 0.10,
                _ => 0.0,
            };
            s.boss_class = if rng.gen::<f32>() < boss_chance {
                boss_pool.choose(&mut rng).copied()
            } else {
                None
            };
        }
        let mut owned: Vec<bool> = vec![false; sections.len()];
        owned[0] = true;
        Self { sections, current: 0, owned, boat_target: None }
    }

    pub fn section(&self, id: u32) -> &MapSection {
        &self.sections[id as usize]
    }

    /// Section ids that share a boundary with `section_id`.
    #[allow(dead_code)]
    pub fn neighbors(&self, section_id: u32) -> &[u32] {
        &self.sections[section_id as usize].adjacencies
    }
}

/// Tear down every map-view entity and rebuild `MapState` at the
/// player-chosen `MapSize`. Runs on `OnExit(HullSelect)` so the
/// freshly-picked topology is in place by the time the camera
/// switches to the play view.
///
/// Order in the OnExit chain: this fires FIRST, then `setup_map` +
/// `spawn_boss_patrols` rebuild visuals from the regenerated state.
pub fn regenerate_map(
    mut commands: Commands,
    map_size: Res<MapSize>,
    mut state: ResMut<MapState>,
    despawn_q: Query<
        Entity,
        Or<(
            With<MapFillSprite>,
            With<MapSectionBoundary>,
            With<MapSlotStar>,
            With<MapBoat>,
            With<BossPatrol>,
        )>,
    >,
) {
    for e in &despawn_q {
        commands.entity(e).despawn();
    }
    *state = MapState::new(map_size.sections());
}

/// BFS distance from the starting section, then `+1` and clamped to 5,
/// produces a 1..=5 star rating per section.
fn compute_stars(sections: &[MapSection], start: usize) -> Vec<u8> {
    let n = sections.len();
    let mut dist = vec![u8::MAX; n];
    if start >= n { return vec![1; n]; }
    dist[start] = 0;
    let mut q: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    q.push_back(start);
    while let Some(i) = q.pop_front() {
        let d = dist[i];
        for &nbr in &sections[i].adjacencies {
            let nbr = nbr as usize;
            if dist[nbr] == u8::MAX {
                dist[nbr] = d.saturating_add(1);
                q.push_back(nbr);
            }
        }
    }
    dist.iter().map(|&d| d.saturating_add(1).min(5)).collect()
}

// ---------- Animation timeline ----------

#[derive(Resource, Default)]
pub struct MapAnimTimeline {
    pub elapsed: f32,
    pub steps: std::collections::VecDeque<TimelineStep>,
}

pub struct TimelineStep {
    pub at: f32,
    pub action: TimelineAction,
}

// Variants kept around for future per-section map animations even
// though nothing populates the timeline today (the building-driven
// Dockyard pulses that used them are gone).
#[allow(dead_code)]
pub enum TimelineAction {
    Pulse { pos: Vec2, color: Color, duration: f32 },
    Beam { from: Vec2, to: Vec2, color: Color, duration: f32 },
}

// ---------- Debug overlay state ----------

#[derive(Resource, Default)]
pub struct DebugClaimMode {
    pub active: bool,
}

#[derive(Event)]
pub struct TriggerMapPhase;

// ---------- Marker components ----------

#[derive(Component)]
pub struct MapBoat;

/// Marker on the single sprite that displays the pre-rasterized section
/// fill image. We render the entire map fill as one sprite to avoid
/// hairline seams between fan-triangle edges.
#[derive(Component)]
pub struct MapFillSprite;

#[derive(Component)]
pub struct MapSectionBoundary;

#[derive(Component)]
pub struct MapSlotStar;

// ---------- Level status markers ----------

#[derive(Component)]
pub struct LevelStatusUi;

#[derive(Component)]
pub struct LevelStatusText;

#[derive(Component)]
pub struct LevelEnemyBar;

// ---------- Debug panel markers ----------
//
// In demo builds nothing in the active schedule references these
// markers (the spawning system + every reader is feature-gated out of
// `main.rs`'s system registration). They're still compiled so the
// hud.rs definitions type-check; `#[cfg_attr]` suppresses the
// dead-code warning so the demo build stays clean.

#[cfg_attr(feature = "demo", allow(dead_code))]
#[derive(Component)]
pub struct DebugPanel;

#[cfg_attr(feature = "demo", allow(dead_code))]
#[derive(Component, Clone, Copy, PartialEq, Eq)]
pub enum DebugButton {
    ClaimMode,
    Phase,
    SpawnAlly(crate::ally::ShipClass),
    SpawnBoss(crate::ally::ShipClass),
    SpawnEnemy(crate::enemy::EnemyVariant),
    OpenCustomize,
    AddScrap,
}

#[cfg_attr(feature = "demo", allow(dead_code))]
#[derive(Component)]
pub struct DebugClaimLabel;

// ---------- Animation primitive markers ----------

#[derive(Component)]
pub struct AnimPulse {
    pub timer: Timer,
    pub peak_alpha: f32,
}

#[derive(Component)]
pub struct AnimBeam {
    pub timer: Timer,
    pub peak_alpha: f32,
}

// ---------- Cross-cutting helpers ----------

/// `run_if` predicate for systems that should only tick during combat.
/// Reads the top-level `AppState` directly — gameplay only ticks when
/// the player is in `AppState::Playing` AND the active view is combat.
/// Main menu, customize/shop overlay, and pause all park in non-Playing
/// states, so they automatically suspend the sim.
pub fn in_combat_view(
    view: Res<ViewMode>,
    state: Res<State<crate::AppState>>,
) -> bool {
    // Combat sim runs in Playing AND during the StageComplete buffer
    // — the "STAGE COMPLETE" overlay should NOT freeze gameplay so
    // the player can keep moving the ship + their bullets keep
    // flying through the buffer instead of stopping mid-shot. The
    // overlay itself is hosted in its own UI Node above the arena
    // (`stage_complete::enter_stage_complete`).
    *view == ViewMode::Combat
        && matches!(
            *state.get(),
            crate::AppState::Playing | crate::AppState::StageComplete
        )
}

/// Standard ray-casting point-in-polygon. Works for the wobbled (but
/// still non-self-intersecting) polygons we hand-author.
pub(crate) fn point_in_polygon(p: Vec2, poly: &[Vec2]) -> bool {
    let n = poly.len();
    if n < 3 { return false; }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let pi = poly[i];
        let pj = poly[j];
        let crosses = (pi.y > p.y) != (pj.y > p.y);
        if crosses {
            let x_at = (pj.x - pi.x) * (p.y - pi.y) / (pj.y - pi.y) + pi.x;
            if p.x < x_at { inside = !inside; }
        }
        j = i;
    }
    inside
}

#[cfg(test)]
mod wave_state_tests {
    //! Regression tests for the wave state machine. Specifically
    //! the Fighting → Cooldown → AdvanceWave / hold-on-last-wave
    //! transitions that previously lived inside `spawn_enemies` and
    //! were untestable without graphics deps.
    //!
    //! Each test is named for the stuck-state class it would catch.

    use super::*;

    fn fresh_ctx(wave_idx: u8, wave_count: u8) -> CombatContext {
        let mut c = CombatContext::default();
        // Override `reset_for`-supplied values so tests can pin the
        // wave layout independently of `balance::waves_for_stars`.
        c.wave_idx = wave_idx;
        c.wave_count = wave_count;
        c
    }

    #[test]
    fn fighting_advances_to_cooldown_when_field_clear() {
        let mut c = fresh_ctx(0, 3);
        c.wave_phase = WavePhase::Fighting;
        c.wave_cd = 0.0;

        assert!(c.try_advance_fighting(0), "field empty → should transition");
        assert_eq!(c.wave_phase, WavePhase::Cooldown);
        assert_eq!(c.wave_cd, BETWEEN_WAVES_DURATION);
    }

    #[test]
    fn fighting_holds_when_enemies_still_alive() {
        let mut c = fresh_ctx(0, 3);
        c.wave_phase = WavePhase::Fighting;

        for n in 1..=10 {
            assert!(!c.try_advance_fighting(n), "enemy_count={n}: no transition");
            assert_eq!(c.wave_phase, WavePhase::Fighting);
        }
    }

    #[test]
    fn fighting_branch_is_phase_gated() {
        // Calling try_advance_fighting on a Spawning / Cooldown ctx
        // must not silently transition. This guards against
        // accidentally double-running the branch from a stray site.
        for phase in [WavePhase::Spawning, WavePhase::Cooldown] {
            let mut c = fresh_ctx(0, 3);
            c.wave_phase = phase;
            assert!(!c.try_advance_fighting(0));
            assert_eq!(c.wave_phase, phase);
        }
    }

    #[test]
    fn cooldown_advances_next_wave_when_timer_elapses() {
        let mut c = fresh_ctx(0, 3);
        c.wave_phase = WavePhase::Cooldown;
        c.wave_cd = 0.5;

        assert!(!c.try_advance_cooldown(0.3), "cd still positive");
        assert_eq!(c.wave_phase, WavePhase::Cooldown);

        assert!(c.try_advance_cooldown(0.3), "cd elapsed → next wave");
        assert_eq!(c.wave_phase, WavePhase::Spawning);
        assert_eq!(c.wave_idx, 1);
    }

    #[test]
    fn cooldown_holds_on_last_wave() {
        // On the LAST wave, cooldown elapsing must NOT advance the
        // wave (no more waves) — level_complete_check takes over.
        let mut c = fresh_ctx(2, 3);
        c.wave_phase = WavePhase::Cooldown;
        c.wave_cd = 0.1;

        assert!(!c.try_advance_cooldown(0.5), "last wave: no advance");
        assert_eq!(c.wave_phase, WavePhase::Cooldown,
            "phase must NOT silently flip back to Spawning");
        assert_eq!(c.wave_idx, 2, "wave_idx must not advance past wave_count-1");
    }

    #[test]
    fn cooldown_branch_is_phase_gated() {
        // Calling try_advance_cooldown while in Fighting/Spawning is
        // a no-op. Catches the bug where a tick from the wrong branch
        // silently drains wave_cd.
        for phase in [WavePhase::Spawning, WavePhase::Fighting] {
            let mut c = fresh_ctx(0, 3);
            c.wave_phase = phase;
            c.wave_cd = 1.0;
            assert!(!c.try_advance_cooldown(0.5));
            assert_eq!(c.wave_cd, 1.0, "wave_cd must NOT drain in {phase:?}");
        }
    }

    #[test]
    fn full_round_trip_fighting_to_next_wave_spawning() {
        // End-to-end: Fighting (1 enemy) → field clears (0 enemies)
        // → Cooldown ticks down → AdvanceWave → Spawning. This is the
        // stuck-state regression: if any step silently fails to fire,
        // the wave is stuck.
        let mut c = fresh_ctx(0, 3);
        c.wave_phase = WavePhase::Fighting;

        // Frame N: enemy still alive.
        assert!(!c.try_advance_fighting(1));

        // Frame N+1: enemy died.
        assert!(c.try_advance_fighting(0));
        assert_eq!(c.wave_phase, WavePhase::Cooldown);

        // Drain cooldown across a few frames at 30fps.
        let dt = 1.0 / 30.0;
        let mut advanced = false;
        for _ in 0..120 {
            if c.try_advance_cooldown(dt) { advanced = true; break; }
        }
        assert!(advanced, "cooldown never elapsed");
        assert_eq!(c.wave_phase, WavePhase::Spawning);
        assert_eq!(c.wave_idx, 1);
    }
}
