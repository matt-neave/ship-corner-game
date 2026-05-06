//! Wave-mode state machine, batch enemy spawn, arena cleanup, and the dock
//! reset helper. This is the orchestrator that drives the
//! `Idle → Active → Cleared → Drafting → Active …` phase loop and the
//! `Active → Failed → Active` death/respawn loop.

use bevy::prelude::*;
use rand::Rng;

use crate::balance::{
    ENEMY_WAVE_X, FRIENDLY_DOCK_HEADING, FRIENDLY_DOCK_X, FRIENDLY_HP_WAVE, FRIENDLY_SPEED,
    PLAY_WORLD, WAVE_FAIL_DELAY, WAVE_INTRO_DELAY, WAVE_TRANSITION_DELAY,
};
use crate::beam::Beam;
use crate::bullet::Bullet;
use crate::components::{Friendly, Health, Heading, Velocity};
use crate::effects::{spawn_hit_particles, EffectMeshes, HitParticle, MuzzleFlash};
use crate::enemy::{spawn_enemy, Enemy, EnemyVariant};
use crate::modes::GameMode;
use crate::palette::PaletteMaterials;
use crate::pier::{generate_draft, pier_drydock_heal, Pier, WaveDraft};
use crate::trails::{EnemyTrail, ShipPath};

// ---------- State ----------

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum WavePhase {
    /// Mode just toggled — short pause before first wave (or board cleared).
    #[default]
    Idle,
    /// Enemies alive, fight in progress.
    Active,
    /// Wave cleared. Brief pause, then transition to Drafting.
    Cleared,
    /// Player chooses + places one upgrade card. Wave does not start until
    /// placement happens (or pier is full and we skip).
    Drafting,
    /// Friendly destroyed. Pause, then respawn ship + same wave.
    Failed,
}

#[derive(Resource)]
pub struct WaveState {
    pub wave: u32,
    pub phase: WavePhase,
    pub phase_timer: f32,
    /// Tracks the last `GameMode` the orchestrator processed so we run the
    /// "mode entered / mode exited" reset path exactly once on transitions.
    pub last_applied_mode: Option<GameMode>,
}

impl Default for WaveState {
    fn default() -> Self {
        Self {
            wave: 0,
            phase: WavePhase::Idle,
            phase_timer: 0.0,
            last_applied_mode: None,
        }
    }
}

// ---------- Cleanup helpers ----------

/// Filter alias for arena-cleanup despawns. One query covers every entity
/// type that should be wiped on a mode switch / wave failure — saves us six
/// separate query params on `wave_orchestrator` (Bevy caps systems at 16).
pub type ArenaDisposeFilter = Or<(
    With<Enemy>,
    With<EnemyTrail>,
    With<Bullet>,
    With<Beam>,
    With<MuzzleFlash>,
    With<HitParticle>,
)>;

/// Despawn everything mid-fight in one pass — enemies, their trails, bullets,
/// beams, muzzle flashes, hit particles.
fn clear_arena(
    commands: &mut Commands,
    dispose: &Query<Entity, ArenaDisposeFilter>,
) {
    for e in dispose.iter() { commands.entity(e).despawn(); }
}

/// Park the friendly ship at the LHS dock at full Wave-mode HP, facing right
/// toward the incoming enemies. Also clears the trail history so the wake
/// doesn't smear from the previous position.
fn place_friendly_at_dock(
    friendly: &mut Query<
        (&mut Transform, &mut Health, &mut Heading, &mut Velocity, &mut Visibility),
        With<Friendly>,
    >,
    path: &mut ShipPath,
) {
    if let Ok((mut tf, mut h, mut heading, mut vel, mut vis)) = friendly.single_mut() {
        tf.translation.x = FRIENDLY_DOCK_X;
        tf.translation.y = 0.0;
        tf.rotation = Quat::from_rotation_z(FRIENDLY_DOCK_HEADING);
        h.0 = FRIENDLY_HP_WAVE;
        heading.0 = FRIENDLY_DOCK_HEADING;
        let dir = Vec2::new(-FRIENDLY_DOCK_HEADING.sin(), FRIENDLY_DOCK_HEADING.cos());
        vel.0 = dir * FRIENDLY_SPEED;
        *vis = Visibility::Inherited;
    }
    path.points.clear();
    path.sample_timer = 0.0;
}

// ---------- Wave spawn ----------

/// Spawn a wave of `3 + wave` enemies along the RHS edge, facing the dock.
/// Variants come from the same default mix as sandbox; difficulty growth is
/// driven by count, not by stat boosts.
fn spawn_wave(
    commands: &mut Commands,
    pm: &PaletteMaterials,
    em: &EffectMeshes,
    meshes: &mut Assets<Mesh>,
    wave: u32,
) {
    let count = 3 + wave as i32;
    let mut rng = rand::thread_rng();
    let margin = 18.0;
    let span = PLAY_WORLD - margin * 2.0;
    let spacing = span / (count as f32).max(1.0);
    let y_start = -PLAY_WORLD / 2.0 + margin + spacing * 0.5;

    for i in 0..count {
        let jitter_y = rng.gen_range(-3.0..3.0);
        let jitter_x = rng.gen_range(-4.0..4.0);
        let pos = Vec2::new(ENEMY_WAVE_X + jitter_x, y_start + i as f32 * spacing + jitter_y);
        let variant = match rng.gen_range(0u32..100) {
            0..50  => EnemyVariant::Standard,
            50..75 => EnemyVariant::Scout,
            75..90 => EnemyVariant::Heavy,
            _      => EnemyVariant::Bomber,
        };
        // Face left (toward the dock) on spawn.
        let heading = std::f32::consts::FRAC_PI_2;
        spawn_enemy(commands, pm, em, meshes, pos, heading, variant);
    }
}

// ---------- Orchestrator ----------

/// Wave-mode state machine. Detects mode transitions, advances the phase
/// timer, and triggers spawn / clear / respawn / drafting at the right moments.
pub fn wave_orchestrator(
    time: Res<Time>,
    mut state: ResMut<WaveState>,
    mode: Res<GameMode>,
    mut pier: ResMut<Pier>,
    mut draft: ResMut<WaveDraft>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut path: ResMut<ShipPath>,
    enemies: Query<Entity, With<Enemy>>,
    dispose: Query<Entity, ArenaDisposeFilter>,
    mut friendly: Query<
        (&mut Transform, &mut Health, &mut Heading, &mut Velocity, &mut Visibility),
        With<Friendly>,
    >,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();

    // Mode transition handling — runs once when GameMode flips.
    if state.last_applied_mode != Some(*mode) {
        state.last_applied_mode = Some(*mode);
        clear_arena(&mut commands, &dispose);
        // Pier resets between mode toggles (so each Wave session is fresh).
        *pier = Pier::default();
        *draft = WaveDraft::default();
        match *mode {
            GameMode::Wave => {
                state.wave = 1;
                state.phase = WavePhase::Idle;
                state.phase_timer = WAVE_INTRO_DELAY;
                place_friendly_at_dock(&mut friendly, &mut path);
            }
            GameMode::Sandbox => {
                state.phase = WavePhase::Idle;
                state.wave = 0;
                if let Ok((mut tf, mut h, mut heading, _vel, mut vis)) = friendly.single_mut() {
                    tf.translation = Vec3::new(0.0, 0.0, 1.0);
                    tf.rotation = Quat::from_rotation_z(0.0);
                    heading.0 = 0.0;
                    h.0 = 100;
                    *vis = Visibility::Inherited;
                }
                path.points.clear();
            }
        }
        return;
    }

    if *mode != GameMode::Wave { return; }
    state.phase_timer -= dt;

    match state.phase {
        WavePhase::Idle => {
            if state.phase_timer <= 0.0 {
                spawn_wave(&mut commands, &pm, &em, &mut meshes, state.wave);
                state.phase = WavePhase::Active;
            }
        }
        WavePhase::Active => {
            // Friendly killed?
            let friendly_dead = friendly.iter().any(|(_, h, _, _, _)| h.0 <= 0);
            if friendly_dead {
                if let Ok((tf, _h, _heading, _vel, mut vis)) = friendly.single_mut() {
                    let pos = tf.translation.truncate();
                    let mut rng = rand::thread_rng();
                    spawn_hit_particles(&mut commands, &em, &pm.hull,            pos, 22, 90.0,  &mut rng);
                    spawn_hit_particles(&mut commands, &em, &pm.bullet_friendly, pos, 14, 110.0, &mut rng);
                    *vis = Visibility::Hidden;
                }
                state.phase = WavePhase::Failed;
                state.phase_timer = WAVE_FAIL_DELAY;
                return;
            }
            // Wave cleared?
            if enemies.iter().count() == 0 {
                state.phase = WavePhase::Cleared;
                state.phase_timer = WAVE_TRANSITION_DELAY;
                // Apply Drydock heals immediately on clear (before draft).
                let heal = pier_drydock_heal(&pier);
                if heal > 0 {
                    if let Ok((_, mut h, _, _, _)) = friendly.single_mut() {
                        h.0 = (h.0 + heal).min(FRIENDLY_HP_WAVE);
                    }
                }
            }
        }
        WavePhase::Cleared => {
            if state.phase_timer <= 0.0 {
                state.wave += 1;
                // Offer a draft if there's any empty cell. Otherwise skip
                // straight to the next wave (pier full).
                if pier.cells.iter().any(|c| c.is_none()) {
                    let mut rng = rand::thread_rng();
                    draft.options = Some(generate_draft(&mut rng));
                    draft.selected = None;
                    state.phase = WavePhase::Drafting;
                } else {
                    spawn_wave(&mut commands, &pm, &em, &mut meshes, state.wave);
                    state.phase = WavePhase::Active;
                }
            }
        }
        WavePhase::Drafting => {
            // Player input handled in `pier::draft_input` — it clears
            // `draft.options` when a placement is made; we advance from there.
            if draft.options.is_none() {
                spawn_wave(&mut commands, &pm, &em, &mut meshes, state.wave);
                state.phase = WavePhase::Active;
            }
        }
        WavePhase::Failed => {
            if state.phase_timer <= 0.0 {
                clear_arena(&mut commands, &dispose);
                place_friendly_at_dock(&mut friendly, &mut path);
                spawn_wave(&mut commands, &pm, &em, &mut meshes, state.wave);
                state.phase = WavePhase::Active;
            }
        }
    }
}
