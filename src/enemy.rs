//! Enemy archetypes, spawning, AI, firing, and bomber detonation.
//!
//! Adding a new enemy variant is a single-file change here:
//! 1. Add a variant to `EnemyVariant`.
//! 2. Add rows in `hp`, `speed`, `turn_rate`, `scale`, `fire_rate`,
//!    `fire_damage`, `has_gun`.
//! 3. (Optional) Add a body-color material in `palette::PaletteMaterials` if
//!    the variant should have its own tint, and pick it up in `body_mat`.
//! 4. Update the spawn-distribution roll in `spawn_enemies` if needed.
//!
//! Per-enemy stats are kept here as `match` tables so the compiler enforces
//! exhaustiveness — forget a row and it won't build.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;
use rand::Rng;
use std::collections::VecDeque;

use crate::ally::{ally_is_submerged, Ally};
use crate::balance::{
    BOMBER_DETONATE_DIST, BULLET_SPEED, ENEMY_BARREL_TIP, ENEMY_BULLET_HALF_LEN, ENEMY_LEN,
    ENEMY_RANGE, ENEMY_WIDTH, HUD_LAYER, PLAY_LAYER, PLAY_WORLD,
};
use crate::bullet::Bullet;
use bevy::sprite::MeshMaterial2d;

use crate::components::{Faction, FactionKind, Friendly, Health, Heading, Velocity};
use crate::effects::{spawn_hit_particles, EffectMeshes, HitFx, MuzzleFlash};
use crate::map::PendingSpawn;
use crate::palette::PaletteMaterials;
use crate::rune::FireExtent;
use crate::ship::approach_angle;
use crate::trails::{empty_dynamic_mesh, EnemyTrail};
use crate::weapon::WeaponType;
use crate::{GameMode, Score, Scrap};

// ---------- Variant tunables ----------

/// Distance (world units) the Sniper tries to maintain from its
/// current target. Inside `desired - 10` it sprints away; outside
/// `desired + 15` it closes at half speed; in between it orbits
/// slowly so the player can't pre-aim the dodge.
pub const SNIPER_DESIRED_DIST: f32 = 80.0;

/// Sniper effective firing range. Bigger than `ENEMY_RANGE` (45) so
/// the sniper can engage from past the standard enemy threat ring.
pub const SNIPER_FIRE_RANGE: f32 = 100.0;

/// Aim duration in seconds — the trajectory line is visible for this
/// long, then the bullet fires. Long enough to dodge with reasonable
/// reaction; short enough that sitting still gets punished.
pub const SNIPER_AIM_TIME: f32 = 1.5;

/// Speed (world units / sec) of the sniper's heavy bullet. A touch
/// faster than the standard `BULLET_SPEED` (110) so the dodge
/// window doesn't last forever once the shot fires.
pub const SNIPER_BULLET_SPEED: f32 = 140.0;

/// Visual scale applied to the standard enemy bullet meshes when
/// rendered as a sniper round. Reads as a heavier shell vs the
/// regular pellets.
pub const SNIPER_BULLET_SCALE: f32 = 1.6;

/// Time-fuse on the landmine a Rammer drops on death. Long enough
/// that the player can read the threat and walk away, short enough
/// that lingering = pain.
pub const RAMMER_MINE_FUSE: f32 = 3.0;
/// Damage dealt by the Rammer's landmine to any unit inside the
/// blast radius when it cooks off.
pub const RAMMER_MINE_DAMAGE: i32 = 6;
/// World-units radius of the Rammer mine's AOE.
pub const RAMMER_MINE_RADIUS: f32 = 9.0;

// ---------- Components / enums ----------

#[derive(Component)]
pub struct Enemy {
    pub variant: EnemyVariant,
    pub state: EnemyState,
    pub state_timer: f32,
    pub waypoint: Vec2,
    pub fire_cd: f32,
    /// Snapshot of HP at spawn (`variant.hp()` for regular enemies,
    /// `class.boss_hp()` for bosses spawned via `spawn_boss`). Used
    /// as the denominator for the on-hit HP bar so the bar's fill
    /// stays accurate even for bosses with custom HP overrides.
    pub max_hp: i32,
}

/// Per-enemy snapshot of last frame's HP. The HP-bar system reads this
/// every frame: a strict decrease means the enemy was damaged this
/// frame, which (re)spawns the bar and resets its 3-second fade timer.
#[derive(Component)]
pub struct PreviousHp(pub i32);

/// Small red HP bar floating above an enemy. Spawned the first time an
/// enemy takes damage; the timer is reset to `HP_BAR_SHOW_TIME` on
/// each subsequent hit. When the timer runs out (or the target enemy
/// despawns), the bar vanishes.
#[derive(Component)]
pub struct EnemyHpBar {
    pub enemy: Entity,
    pub remaining: f32,
}

/// Marks a Sniper that's currently in its 1.5s aim phase. Holds the
/// snapshotted target world position (so the bullet flies along the
/// telegraphed line even if the target moves), the time remaining
/// until fire, and the entity ID of the visible aim-line decoration.
/// Removed when the shot fires (or when the sniper dies and Bevy
/// auto-cleans the line via the back-ref system).
#[derive(Component)]
pub struct SniperAim {
    pub remaining: f32,
    pub target_world: Vec2,
    pub line: Entity,
}

/// Free-floating aim-line entity drawn from a Sniper's current
/// position to the Sniper's locked target world position. Its
/// `Transform` is rewritten every frame from the live sniper
/// position; the `target_world` snapshot stays fixed for the
/// duration of the aim. Auto-despawns when the source sniper is
/// gone.
#[derive(Component)]
pub struct SniperAimLine {
    pub sniper: Entity,
    pub target_world: Vec2,
    /// Total aim duration this line was spawned with — used to
    /// drive the width pulse over the aim period.
    pub aim_total: f32,
    /// Seconds until fire, mirrored from `SniperAim.remaining`.
    pub remaining: f32,
}

/// Marker on the Sniper's independent-rotation turret base. The
/// barrel mesh is parented to this entity (not directly to the body),
/// so rotating this base — driven by `sniper_turret_aim` — orbits
/// the barrel around the body centre while the body itself heads
/// wherever its movement AI dictates. Decouples body heading from
/// shot direction, giving the sniper effective 360° fire.
#[derive(Component)]
pub struct SniperTurret;

/// Time-fused enemy landmine dropped by a Rammer on death. Counts
/// down `fuse`; on 0, applies `damage` to every friendly/ally inside
/// `blast_radius` and despawns with a particle burst.
#[derive(Component)]
pub struct EnemyLandmine {
    pub fuse: f32,
    pub damage: i32,
    pub blast_radius: f32,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum EnemyState {
    Wander,
    Approach,
    Attack,
    Reposition,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EnemyVariant {
    Standard,
    Heavy,
    Scout,
    Bomber,
    /// Small fast kamikaze. Beelines like Bomber but smaller payload;
    /// drops a 3-second-fused landmine on death (any cause). Carries
    /// no gun.
    Rammer,
    /// Long-range artillery. Keeps `SNIPER_DESIRED_DIST` from the
    /// player, takes `SNIPER_AIM_TIME`s to aim with a visible
    /// trajectory telegraph, then fires a heavy slow shot. Has 360°
    /// fire — body heading and shot direction are decoupled.
    Sniper,
}

impl EnemyVariant {
    pub fn hp(self) -> i32 {
        match self {
            EnemyVariant::Standard => 5,
            EnemyVariant::Heavy    => 15,
            EnemyVariant::Scout    => 2,
            EnemyVariant::Bomber   => 4,
            EnemyVariant::Rammer   => 3,
            EnemyVariant::Sniper   => 8,
        }
    }
    pub fn speed(self) -> f32 {
        match self {
            EnemyVariant::Standard => 18.0,
            EnemyVariant::Heavy    => 12.0,
            EnemyVariant::Scout    => 28.0,
            EnemyVariant::Bomber   => 26.0,
            EnemyVariant::Rammer   => 34.0,
            EnemyVariant::Sniper   => 16.0,
        }
    }
    pub fn turn_rate(self) -> f32 {
        match self {
            EnemyVariant::Standard => 0.9,
            EnemyVariant::Heavy    => 0.55,
            EnemyVariant::Scout    => 1.7,
            EnemyVariant::Bomber   => 0.6,
            EnemyVariant::Rammer   => 1.4,
            EnemyVariant::Sniper   => 0.5,
        }
    }
    /// Visual + collision scale applied via Transform.scale on the parent.
    pub fn scale(self) -> f32 {
        match self {
            EnemyVariant::Standard => 1.0,
            EnemyVariant::Heavy    => 1.5,
            EnemyVariant::Scout    => 0.7,
            EnemyVariant::Bomber   => 1.0,
            EnemyVariant::Rammer   => 0.65,
            EnemyVariant::Sniper   => 0.95,
        }
    }
    pub fn fire_rate(self) -> f32 {
        match self {
            EnemyVariant::Standard => 1.0,
            EnemyVariant::Heavy    => 0.7,
            EnemyVariant::Scout    => 1.5,
            EnemyVariant::Bomber   => 0.0,
            EnemyVariant::Rammer   => 0.0,
            // Sniper "fires" infrequently — its real cadence is the
            // aim phase + this cooldown gating when the next aim can
            // start. Tuned to a slow, careful shot.
            EnemyVariant::Sniper   => 0.5,
        }
    }
    pub fn fire_damage(self) -> i32 {
        // Most enemy cannons do 1 damage. Sniper hits noticeably
        // harder to make the long telegraph feel earned.
        match self {
            EnemyVariant::Sniper => 4,
            _ => 1,
        }
    }
    pub fn has_gun(self) -> bool {
        !matches!(self, EnemyVariant::Bomber | EnemyVariant::Rammer)
    }
}

// ---------- Spawn helper ----------

/// Spawn one enemy entity (body + turret children + trail) at `pos`. Shared
/// between the sandbox drip-spawner and the wave-mode batch spawn so both
/// paths produce visually identical enemies.
pub fn spawn_enemy(
    commands: &mut Commands,
    pm: &PaletteMaterials,
    em: &EffectMeshes,
    meshes: &mut Assets<Mesh>,
    pos: Vec2,
    heading: f32,
    variant: EnemyVariant,
) {
    let body_mat = match variant {
        EnemyVariant::Standard => pm.enemy.clone(),
        EnemyVariant::Heavy    => pm.enemy_heavy.clone(),
        EnemyVariant::Scout    => pm.enemy_scout.clone(),
        EnemyVariant::Bomber   => pm.enemy_accent.clone(),
        EnemyVariant::Rammer   => pm.enemy_rammer.clone(),
        EnemyVariant::Sniper   => pm.enemy_sniper.clone(),
    };
    let scale = variant.scale();
    let dir = Vec2::new(-heading.sin(), heading.cos());

    let id = commands.spawn((
        Mesh2d(em.enemy_body.clone()),
        MeshMaterial2d(body_mat.clone()),
        Transform::from_xyz(pos.x, pos.y, 1.0)
            .with_rotation(Quat::from_rotation_z(heading))
            .with_scale(Vec3::splat(scale)),
        Enemy {
            variant,
            state: EnemyState::Approach,
            state_timer: 1.0,
            waypoint: Vec2::ZERO,
            fire_cd: 0.5,
            max_hp: variant.hp(),
        },
        Health(variant.hp()),
        PreviousHp(variant.hp()),
        Velocity(dir * variant.speed()),
        Heading(heading),
        Faction(FactionKind::Enemy),
        HitFx::new(body_mat).with_rest_scale(scale),
        FireExtent(Vec2::new(ENEMY_WIDTH * 0.5 * scale, ENEMY_LEN * 0.5 * scale)),
        RenderLayers::layer(PLAY_LAYER),
    )).id();

    if variant == EnemyVariant::Sniper {
        // Sniper: independent-rotation turret. Base sits at the body
        // centre as a pivot (marker `SniperTurret`); barrel is a
        // child of the BASE, offset to +Y. Rotating the base spins
        // the barrel around the body centre — `sniper_turret_aim`
        // drives that rotation each frame. Body and turret are
        // decoupled, giving the sniper 360° fire regardless of
        // which way the hull is moving.
        let base = commands.spawn((
            Mesh2d(em.enemy_turret_base.clone()),
            MeshMaterial2d(pm.enemy_accent.clone()),
            Transform::from_xyz(0.0, 0.0, 0.1),
            SniperTurret,
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(base).insert(ChildOf(id));

        let barrel = commands.spawn((
            Mesh2d(em.enemy_turret_barrel.clone()),
            MeshMaterial2d(pm.enemy_accent.clone()),
            Transform::from_xyz(0.0, 1.8, 0.15),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(barrel).insert(ChildOf(base));
    } else if variant.has_gun() {
        let base = commands.spawn((
            Mesh2d(em.enemy_turret_base.clone()),
            MeshMaterial2d(pm.enemy_accent.clone()),
            Transform::from_xyz(0.0, 0.0, 0.1),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(base).insert(ChildOf(id));

        let barrel = commands.spawn((
            Mesh2d(em.enemy_turret_barrel.clone()),
            MeshMaterial2d(pm.enemy_accent.clone()),
            Transform::from_xyz(0.0, 1.8, 0.15),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(barrel).insert(ChildOf(id));
    }

    if variant == EnemyVariant::Bomber {
        // Bright warhead at the bow — visual telegraph that this one rams.
        let warhead = commands.spawn((
            Mesh2d(em.bomber_warhead.clone()),
            MeshMaterial2d(pm.bullet_enemy.clone()),
            Transform::from_xyz(0.0, ENEMY_LEN / 2.0 - 1.0, 0.2),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(warhead).insert(ChildOf(id));
    }

    if variant == EnemyVariant::Rammer {
        // Smaller bright warhead — telegraphs "kamikaze" without
        // looking like a Bomber. Reuses the bomber-warhead mesh
        // shrunk a little; the orange hull does the rest of the
        // visual differentiation.
        let warhead = commands.spawn((
            Mesh2d(em.bomber_warhead.clone()),
            MeshMaterial2d(pm.bullet_enemy.clone()),
            Transform::from_xyz(0.0, ENEMY_LEN / 2.0 - 1.5, 0.2)
                .with_scale(Vec3::splat(0.6)),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(warhead).insert(ChildOf(id));
    }

    // Short white wake trail behind the enemy (lives on its own entity in
    // world space; despawns when the enemy is gone — see `update_enemy_trails`).
    let trail_mesh = meshes.add(empty_dynamic_mesh());
    commands.spawn((
        Mesh2d(trail_mesh),
        MeshMaterial2d(pm.trail.clone()),
        Transform::from_xyz(0.0, 0.0, 0.4),
        EnemyTrail { enemy: id, points: VecDeque::new(), sample_timer: 0.0 },
        RenderLayers::layer(PLAY_LAYER),
    ));
}

// ---------- Systems ----------

/// Wave-based spawner. State machine on `combat_ctx.wave_phase`:
///
/// - **Spawning**: pre-rolls every enemy's spawn position on entry +
///   spawns a flashing on-screen indicator at each pre-clamped edge
///   point so the player can see what's incoming. Then drips one
///   enemy per `WAVE_SPAWN_INTERVAL`, despawning each indicator as
///   its enemy lands. Empties → `Fighting`.
/// - **Fighting**: wait for the arena to clear → `Cooldown`.
/// - **Cooldown**: short breather; advance to next wave or sit idle
///   for `level_complete_check` to take over on the last wave.
///
/// Boss waves swap variant distribution to favour Heavy + Bomber.
pub fn spawn_enemies(
    time: Res<Time>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    indicator_assets: Option<Res<SpawnIndicatorAssets>>,
    mode: Res<GameMode>,
    mut combat_ctx: ResMut<crate::map::CombatContext>,
    pending: Res<crate::xp::LevelUpsPending>,
    mut return_state: ResMut<crate::xp::LevelUpReturn>,
    mut next_state: ResMut<NextState<crate::AppState>>,
    enemies: Query<Entity, With<Enemy>>,
) {
    if *mode != GameMode::Sandbox { return; }
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let Some(indicator_assets) = indicator_assets else { return; };
    if combat_ctx.enemy_budget == 0 { return; }

    let dt = time.delta_secs();

    match combat_ctx.wave_phase {
        crate::map::WavePhase::Spawning => {
            // First Spawning frame after `advance_wave`/`reset_for`:
            // pre-roll every position for this wave + spawn the
            // flashing indicators. Brief telegraph delay before the
            // first drip so the player gets a moment to react.
            if combat_ctx.pending_spawns.is_empty() && combat_ctx.wave_remaining > 0 {
                let mut rng = rand::thread_rng();
                let mut queue = Vec::with_capacity(combat_ctx.wave_remaining as usize);
                for _ in 0..combat_ctx.wave_remaining {
                    let pos = random_edge_pos(&mut rng);
                    let indicator = spawn_indicator(&mut commands, &indicator_assets, pos);
                    queue.push(PendingSpawn { pos, indicator });
                }
                combat_ctx.pending_spawns = queue;
                combat_ctx.spawn_tick = WAVE_TELEGRAPH_DELAY;

                // Final wave of a 5★ section drops the boss into the
                // arena alongside the normal wave roster. The boss is
                // tagged `Enemy` (via `spawn_boss`) so the existing
                // death / collision / level-complete plumbing handles
                // it without bespoke wiring; budget isn't decremented
                // for the boss, so `level_complete_check` keeps
                // waiting for it to die after the last regular spawn.
                if combat_ctx.wave_idx + 1 == combat_ctx.wave_count {
                    if let Some(class) = combat_ctx.boss_pending.take() {
                        let pos = random_edge_pos(&mut rng);
                        let heading = (-pos.x).atan2(pos.y) + std::f32::consts::PI;
                        crate::ally::spawn_boss(
                            &mut commands, &pm, &em, &mut meshes, pos, heading, class,
                        );
                    }
                }
                return;
            }

            combat_ctx.spawn_tick -= dt;
            if combat_ctx.spawn_tick > 0.0 { return; }
            // Concurrent cap still applies.
            if enemies.iter().count() >= combat_ctx.enemy_cap() { return; }
            let Some(spawn) = (!combat_ctx.pending_spawns.is_empty())
                .then(|| combat_ctx.pending_spawns.remove(0)) else { return };
            // Indicator served its purpose — remove the visual.
            commands.entity(spawn.indicator).despawn();

            spawn_one_at(&mut commands, &pm, &em, &mut meshes, spawn.pos, combat_ctx.is_boss_wave);
            combat_ctx.spawn_tick = WAVE_SPAWN_INTERVAL;
            combat_ctx.wave_remaining = combat_ctx.wave_remaining.saturating_sub(1);
            combat_ctx.enemy_budget = combat_ctx.enemy_budget.saturating_sub(1);

            if combat_ctx.wave_remaining == 0 {
                combat_ctx.wave_phase = crate::map::WavePhase::Fighting;
            }
        }
        crate::map::WavePhase::Fighting => {
            if enemies.iter().count() == 0 {
                combat_ctx.wave_phase = crate::map::WavePhase::Cooldown;
                combat_ctx.wave_cd = crate::map::BETWEEN_WAVES_DURATION;
                // Drain queued level-ups in the breather between waves
                // — but ONLY when there's another wave coming. On the
                // last wave we let the existing StageComplete →
                // LevelUp → Customize chain handle any remaining
                // drains, which avoids racing `level_complete_check`
                // (it can fire on the same frame `enemy_budget` hits 0
                // and the order of those two systems isn't pinned).
                if pending.0 > 0 && combat_ctx.wave_idx + 1 < combat_ctx.wave_count {
                    return_state.0 = Some(crate::AppState::Playing);
                    next_state.set(crate::AppState::LevelUp);
                }
            }
        }
        crate::map::WavePhase::Cooldown => {
            combat_ctx.wave_cd -= dt;
            if combat_ctx.wave_cd > 0.0 { return; }
            if combat_ctx.wave_idx + 1 < combat_ctx.wave_count {
                combat_ctx.advance_wave();
            }
        }
    }
}

/// Per-tick interval between drips inside a `Spawning` phase.
const WAVE_SPAWN_INTERVAL: f32 = 0.15;
/// Pause between indicator-pop and the first drip — gives the player
/// a beat to read the directions before enemies start arriving.
const WAVE_TELEGRAPH_DELAY: f32 = 0.8;

fn random_edge_pos(rng: &mut rand::rngs::ThreadRng) -> Vec2 {
    let half = PLAY_WORLD / 2.0;
    let edge = rng.gen_range(0..4);
    match edge {
        0 => Vec2::new(rng.gen_range(-half..half), half + 20.0),
        1 => Vec2::new(rng.gen_range(-half..half), -half - 20.0),
        2 => Vec2::new(half + 20.0, rng.gen_range(-half..half)),
        _ => Vec2::new(-half - 20.0, rng.gen_range(-half..half)),
    }
}

fn spawn_one_at(
    commands: &mut Commands,
    pm: &PaletteMaterials,
    em: &EffectMeshes,
    meshes: &mut Assets<Mesh>,
    pos: Vec2,
    boss_wave: bool,
) {
    let mut rng = rand::thread_rng();
    let variant = if boss_wave {
        match rng.gen_range(0u32..100) {
            0..30  => EnemyVariant::Heavy,
            30..55 => EnemyVariant::Bomber,
            55..70 => EnemyVariant::Rammer,
            70..85 => EnemyVariant::Sniper,
            _      => EnemyVariant::Standard,
        }
    } else {
        match rng.gen_range(0u32..100) {
            0..35  => EnemyVariant::Standard,
            35..55 => EnemyVariant::Scout,
            55..70 => EnemyVariant::Heavy,
            70..82 => EnemyVariant::Bomber,
            82..92 => EnemyVariant::Rammer,
            _      => EnemyVariant::Sniper,
        }
    };

    let inward = (-pos).normalize_or(Vec2::Y);
    let heading = (-inward.x).atan2(inward.y);
    spawn_enemy(commands, pm, em, meshes, pos, heading, variant);
}

// ---------- Spawn indicators ----------
//
// Flashing on-screen markers showing where the next wave is about to
// drop in. Each indicator is a small triangle clamped to the play
// area edge, rotated to point outward toward its associated spawn
// position (which sits just outside the play area). All indicators
// share one mesh + material handle so the per-frame alpha pulse only
// touches a single asset entry.

#[derive(Component)]
pub struct SpawnIndicator;

#[derive(Resource)]
pub struct SpawnIndicatorAssets {
    /// Solid filled triangle pointing outward toward the spawn site.
    /// Broad base, tall tip — reads as a simple wedge of incoming
    /// trouble without the steep / pointy chevron silhouette.
    pub mesh: Handle<Mesh>,
    pub material: Handle<ColorMaterial>,
}

pub fn setup_spawn_indicator_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
) {
    // Solid wedge — wider base + taller tip than the original tiny
    // triangle so it reads at a glance even at low alpha. Tip along
    // +Y; the spawner rotates per-instance to point outward.
    let mesh = meshes.add(Triangle2d::new(
        Vec2::new(-4.5, -2.8),
        Vec2::new( 4.5, -2.8),
        Vec2::new( 0.0,  5.6),
    ));
    // Deeper blood-red at peak alpha — the previous pinkish hue read
    // as a generic UI accent. Darker reds carry "danger / spawn"
    // weight better and contrast cleanly with the bright ocean.
    let material = materials.add(Color::srgba(0.72, 0.10, 0.12, 0.85));
    commands.insert_resource(SpawnIndicatorAssets { mesh, material });
}

fn spawn_indicator(
    commands: &mut Commands,
    assets: &SpawnIndicatorAssets,
    spawn_pos: Vec2,
) -> Entity {
    let half = PLAY_WORLD / 2.0;
    let inset = 5.0;
    let inner = half - inset;
    let pos = Vec2::new(
        spawn_pos.x.clamp(-inner, inner),
        spawn_pos.y.clamp(-inner, inner),
    );
    let outward = (spawn_pos - pos).normalize_or(Vec2::Y);
    let angle = (-outward.x).atan2(outward.y);

    commands.spawn((
        Mesh2d(assets.mesh.clone()),
        MeshMaterial2d(assets.material.clone()),
        Transform::from_xyz(pos.x, pos.y, 5.5)
            .with_rotation(Quat::from_rotation_z(angle)),
        SpawnIndicator,
        RenderLayers::layer(PLAY_LAYER),
    )).id()
}

/// Pulse the shared indicator-material alpha. All indicators share
/// the asset, so this single write animates every visible arrow.
/// Faster + wider-range than the original sine so the marker reads
/// as an active warning rather than a passive blip.
pub fn tick_spawn_indicators(
    time: Res<Time>,
    assets: Option<Res<SpawnIndicatorAssets>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
) {
    let Some(assets) = assets else { return };
    if let Some(mat) = materials.get_mut(&assets.material) {
        let t = time.elapsed_secs();
        // 12 rad/s ≈ 1.9 Hz; range 0.18→1.0 so the dim end nearly
        // disappears for a punchier flash.
        let alpha = 0.18 + 0.82 * (0.5 + 0.5 * (t * 12.0).sin());
        mat.color = mat.color.with_alpha(alpha);
    }
}

/// Defensive cleanup. Despawns every live indicator and clears the
/// pending list so the next combat starts with a clean slate. Hooked
/// to `OnEnter(Customize)` and `OnExit(MainMenu)`.
pub fn clear_spawn_indicators(
    mut commands: Commands,
    mut combat_ctx: ResMut<crate::map::CombatContext>,
    q: Query<Entity, With<SpawnIndicator>>,
) {
    for e in &q {
        commands.entity(e).despawn();
    }
    combat_ctx.pending_spawns.clear();
}

pub fn enemy_ai(
    time: Res<Time>,
    friendly: Query<&Transform, (With<Friendly>, Without<Enemy>, Without<Ally>)>,
    allies: Query<(&Transform, &Ally), (With<Ally>, Without<Enemy>, Without<Friendly>)>,
    mut q: Query<(&mut Transform, &mut Velocity, &mut Heading, &mut Enemy)>,
) {
    let dt = time.delta_secs();
    let Ok(ftf) = friendly.single() else { return; };
    let fpos = ftf.translation.truncate();
    // Snapshot ally positions once per frame — all enemies pick the
    // nearest target from the same list. Submerged allies are filtered
    // out here so normal enemies don't try to chase a sub they can't hit.
    let ally_positions: Vec<Vec2> = allies
        .iter()
        .filter(|(_, a)| !ally_is_submerged(a))
        .map(|(t, _)| t.translation.truncate())
        .collect();
    let mut rng = rand::thread_rng();

    for (mut tf, mut vel, mut heading, mut enemy) in &mut q {
        let pos = tf.translation.truncate();
        enemy.state_timer -= dt;
        enemy.fire_cd -= dt;
        let speed = enemy.variant.speed();
        let turn  = enemy.variant.turn_rate();

        // Per-enemy nearest-target pick — chooses among
        // {friendly, allies}. Re-evaluated every frame so a closer
        // ally that drifts into range pulls aggro naturally.
        let target_pos = nearest_target(pos, fpos, &ally_positions);

        // Bombers + Rammers skip the state machine — head straight
        // at their target, no waypoints, no firing. Contact damage
        // and despawn handled by `bomber_detonate` (which also
        // covers Rammer). On Rammer death `enemy_death_check` drops
        // the time-fused landmine.
        if matches!(enemy.variant, EnemyVariant::Bomber | EnemyVariant::Rammer) {
            let to = target_pos - pos;
            if to.length_squared() > 1.0 {
                let desired = (-to.x).atan2(to.y);
                heading.0 = approach_angle(heading.0, desired, turn * dt);
            }
            let dir = Vec2::new(-heading.0.sin(), heading.0.cos());
            vel.0 = dir * speed;
            tf.rotation = Quat::from_rotation_z(heading.0);
            continue;
        }

        // Sniper — actively keeps `SNIPER_DESIRED_DIST` away. Closer
        // than the inner band → flee directly away (sprint). Farther
        // than the outer band → close at half speed. In the sweet
        // spot → drift slowly perpendicular to the player to make
        // the shot harder to dodge. Body heading tracks motion (not
        // target) — the sniper's 360° fire (driven by
        // `sniper_turret_aim`) decouples body from aim, so the
        // sniper keeps moving even during the 1.5s charge phase.
        if enemy.variant == EnemyVariant::Sniper {
            let to = target_pos - pos;
            let dist = to.length();
            let unit_to = to.normalize_or(Vec2::Y);
            let (motion_dir, speed_mult) = if dist < SNIPER_DESIRED_DIST - 10.0 {
                (-unit_to, 1.0)
            } else if dist > SNIPER_DESIRED_DIST + 15.0 {
                (unit_to, 0.5)
            } else {
                // Orbit slowly: rotate the to-target vector 90° so
                // the sniper drifts sideways.
                (Vec2::new(-unit_to.y, unit_to.x), 0.25)
            };
            let desired = (-motion_dir.x).atan2(motion_dir.y);
            heading.0 = approach_angle(heading.0, desired, turn * dt);
            let dir = Vec2::new(-heading.0.sin(), heading.0.cos());
            vel.0 = dir * speed * speed_mult;
            tf.rotation = Quat::from_rotation_z(heading.0);
            continue;
        }

        let dist = pos.distance(target_pos);

        if enemy.state_timer <= 0.0 {
            enemy.state = if dist > 75.0 {
                EnemyState::Approach
            } else if dist > 35.0 {
                if rng.gen_bool(0.6) { EnemyState::Attack } else { EnemyState::Reposition }
            } else {
                EnemyState::Reposition
            };
            // Scouts re-plan twice as often, giving them a jittery feel.
            let timer_range = if enemy.variant == EnemyVariant::Scout {
                0.6..1.5
            } else {
                1.5..3.5
            };
            enemy.state_timer = rng.gen_range(timer_range);
            let off = Vec2::new(rng.gen_range(-30.0..30.0), rng.gen_range(-30.0..30.0));
            enemy.waypoint = target_pos + off;
        }

        let active_target = match enemy.state {
            EnemyState::Wander | EnemyState::Reposition => enemy.waypoint,
            EnemyState::Approach | EnemyState::Attack   => target_pos,
        };
        let to = active_target - pos;
        if to.length_squared() > 1.0 {
            let desired = (-to.x).atan2(to.y);
            heading.0 = approach_angle(heading.0, desired, turn * dt);
        }
        let dir = Vec2::new(-heading.0.sin(), heading.0.cos());
        vel.0 = dir * speed;
        tf.rotation = Quat::from_rotation_z(heading.0);
    }
}

/// Pick the closest of `friendly_pos` and any `ally_positions` to
/// `enemy_pos`. Friendly is the default — guarantees a target even
/// when no allies are alive — and an ally only displaces it if it's
/// strictly closer. Used by `enemy_ai` and `enemy_fire` so steering
/// and aiming agree on a single target each frame.
fn nearest_target(enemy_pos: Vec2, friendly_pos: Vec2, ally_positions: &[Vec2]) -> Vec2 {
    let mut best = friendly_pos;
    let mut best_d2 = enemy_pos.distance_squared(friendly_pos);
    for &ap in ally_positions {
        let d2 = enemy_pos.distance_squared(ap);
        if d2 < best_d2 {
            best = ap;
            best_d2 = d2;
        }
    }
    best
}

pub fn enemy_fire(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    friendly: Query<&Transform, (With<Friendly>, Without<Enemy>, Without<Ally>)>,
    allies: Query<(&Transform, &Ally), (With<Ally>, Without<Enemy>, Without<Friendly>)>,
    mut enemies: Query<(Entity, &Transform, &Heading, &mut Enemy)>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    let Ok(ftf) = friendly.single() else { return; };
    let fpos = ftf.translation.truncate();
    // Skip submerged allies — `enemy_ai` already excludes them from
    // steering, and aiming at them here would waste shots that would
    // pass through anyway.
    let ally_positions: Vec<Vec2> = allies
        .iter()
        .filter(|(_, a)| !ally_is_submerged(a))
        .map(|(t, _)| t.translation.truncate())
        .collect();

    for (enemy_entity, tf, heading, mut enemy) in &mut enemies {
        if !enemy.variant.has_gun() { continue; }
        // Sniper has its own bespoke firing pipeline (aim phase +
        // telegraph + heavy bullet) — `sniper_fire` owns its
        // cooldown and shot path. Skip it here so it doesn't also
        // fire a regular straight-ahead shot.
        if enemy.variant == EnemyVariant::Sniper { continue; }
        enemy.fire_cd -= dt;
        let pos = tf.translation.truncate();
        // Aim at the closest of {friendly, allies}.
        let target_pos = nearest_target(pos, fpos, &ally_positions);
        let to = target_pos - pos;
        if to.length() > ENEMY_RANGE { continue; }
        let forward = Vec2::new(-heading.0.sin(), heading.0.cos());
        let aim = forward.angle_to(to.normalize_or_zero()).abs();
        if aim > 0.2 { continue; }
        if enemy.fire_cd > 0.0 { continue; }
        enemy.fire_cd = 1.0 / enemy.variant.fire_rate().max(0.1);
        let fire_damage = enemy.variant.fire_damage();
        let dir = forward;
        // Bullet spawn — push forward by half its length so its BACK sits at
        // the barrel tip and the bullet appears to emerge from the muzzle.
        let bullet_pos = pos + forward * (ENEMY_BARREL_TIP + ENEMY_BULLET_HALF_LEN);
        let bullet = commands.spawn((
            Mesh2d(em.bullet_enemy_outer.clone()),
            MeshMaterial2d(pm.bullet_enemy_outer.clone()),
            Transform::from_xyz(bullet_pos.x, bullet_pos.y, 4.0)
                .with_rotation(Quat::from_rotation_z(heading.0)),
            Bullet {
                faction: FactionKind::Enemy,
                damage: fire_damage,
                remaining: ENEMY_RANGE,
                weapon: WeaponType::Standard,
                source: None,
                runes: [None; 3],
            },
            Velocity(dir * BULLET_SPEED),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        let inner = commands.spawn((
            Mesh2d(em.bullet_enemy_inner.clone()),
            MeshMaterial2d(pm.bullet_enemy.clone()),
            Transform::from_xyz(0.0, 0.0, 0.05),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(inner).insert(ChildOf(bullet));

        // Parent to the enemy so the flash follows the ship; local +Y axis
        // matches the enemy's forward direction (turret is fixed forward).
        let flash = commands.spawn((
            Mesh2d(em.muzzle_flash.clone()),
            MeshMaterial2d(pm.bullet_enemy.clone()),
            Transform::from_xyz(0.0, ENEMY_BARREL_TIP, 4.0),
            MuzzleFlash { life: 0.18, max_life: 0.18 },
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(flash).insert(ChildOf(enemy_entity));
    }
}

/// Bombers + Rammers don't shoot — they self-destruct on contact
/// with the closest of the friendly ship or an ally. Pulses the hit
/// hull and spawns a particle burst. Bomber hits hard (5 dmg) at
/// `BOMBER_DETONATE_DIST`; Rammer is a smaller threat (3 dmg, 60%
/// of the radius) but its real punch is the time-fused landmine
/// dropped by `enemy_death_check` after this drives HP to 0.
pub fn bomber_detonate(
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    mut bombers: Query<(Entity, &Transform, &Enemy, &mut Health)>,
    mut friendly: Query<
        (&Transform, &mut Health, &mut HitFx),
        (With<Friendly>, Without<Ally>, Without<Enemy>),
    >,
    mut allies: Query<
        (Entity, &Transform, &Ally, &mut Health, &mut HitFx),
        (With<Ally>, Without<Friendly>, Without<Enemy>),
    >,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let mut rng = rand::thread_rng();

    for (_be, btf, enemy, mut be_hp) in &mut bombers {
        let (radius, contact_damage) = match enemy.variant {
            EnemyVariant::Bomber => (BOMBER_DETONATE_DIST, 5),
            EnemyVariant::Rammer => (BOMBER_DETONATE_DIST * 0.6, 3),
            _ => continue,
        };
        let bp = btf.translation.truncate();

        // Friendly first — preferred target if in range.
        let mut detonated = false;
        if let Ok((ftf, mut h, mut fx)) = friendly.single_mut() {
            if bp.distance(ftf.translation.truncate()) < radius {
                fx.pulse();
                h.0 = (h.0 - contact_damage).max(0);
                detonated = true;
            }
        }
        if !detonated {
            let mut best: Option<(Entity, f32)> = None;
            for (ae, atf, ally, _, _) in &allies {
                if ally_is_submerged(ally) { continue; }
                let d = bp.distance(atf.translation.truncate());
                if d < radius && best.map_or(true, |(_, bd)| d < bd) {
                    best = Some((ae, d));
                }
            }
            if let Some((ae, _)) = best {
                if let Ok((_, _, _, mut h, mut fx)) = allies.get_mut(ae) {
                    fx.pulse();
                    h.0 = (h.0 - contact_damage).max(0);
                    detonated = true;
                }
            }
        }

        if detonated {
            // Drive HP to 0 instead of direct-despawn so
            // `enemy_death_check` runs the unified death path —
            // particles, score, scrap, XP, AND the Rammer's
            // landmine drop. Saves a duplicate landmine-spawn site.
            be_hp.0 = 0;
            // Bomber gets a heftier two-tone burst; Rammer keeps the
            // sparkle small so the visual cue stays "small bang +
            // mine left behind" rather than "bomber-grade boom".
            let (n1, n2, sp1, sp2) = match enemy.variant {
                EnemyVariant::Bomber => (14, 8, 80.0, 100.0),
                EnemyVariant::Rammer => (8, 4, 60.0, 80.0),
                _ => unreachable!(),
            };
            spawn_hit_particles(&mut commands, &em, &pm.enemy,        bp, n1, sp1, &mut rng);
            spawn_hit_particles(&mut commands, &em, &pm.bullet_enemy, bp, n2, sp2, &mut rng);
        }
    }
}

/// Tick every armed enemy landmine. When `fuse <= 0` the mine
/// explodes: damages anything in `blast_radius` (friendly + allies),
/// spawns a two-tone particle burst, and despawns. Mirrors the
/// Mortar splash damage pattern so the rules feel consistent across
/// AOE sources.
pub fn enemy_landmine_tick(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    mut mines: Query<(Entity, &Transform, &mut EnemyLandmine)>,
    mut friendly: Query<
        (&Transform, &mut Health, &mut HitFx),
        (With<Friendly>, Without<Ally>, Without<EnemyLandmine>),
    >,
    mut allies: Query<
        (&Transform, &Ally, &mut Health, &mut HitFx),
        (With<Ally>, Without<Friendly>, Without<EnemyLandmine>),
    >,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();

    for (entity, tf, mut mine) in &mut mines {
        mine.fuse -= dt;
        if mine.fuse > 0.0 { continue; }
        let center = tf.translation.truncate();
        let r2 = mine.blast_radius * mine.blast_radius;

        if let Ok((ftf, mut h, mut fx)) = friendly.single_mut() {
            if ftf.translation.truncate().distance_squared(center) < r2 {
                fx.pulse();
                h.0 = (h.0 - mine.damage).max(0);
            }
        }
        for (atf, ally, mut h, mut fx) in &mut allies {
            if ally_is_submerged(ally) { continue; }
            if atf.translation.truncate().distance_squared(center) >= r2 { continue; }
            fx.pulse();
            h.0 = (h.0 - mine.damage).max(0);
        }

        spawn_hit_particles(&mut commands, &em, &pm.enemy,        center, 16, 90.0,  &mut rng);
        spawn_hit_particles(&mut commands, &em, &pm.enemy_mine_dot, center, 10, 110.0, &mut rng);
        commands.entity(entity).despawn();
    }
}

/// Sniper firing pipeline — runs separately from `enemy_fire` because
/// the sniper has bespoke aim/telegraph/heavy-shot semantics.
///
/// Two phases driven by the optional `SniperAim` component:
///   1. **Idle** (no `SniperAim`): if a target is in `SNIPER_FIRE_RANGE`
///      and the shot cooldown is ready, snapshot the target's world
///      position, insert `SniperAim`, and spawn the visible aim-line
///      decoration.
///   2. **Aiming** (has `SniperAim`): tick `remaining` down. On 0,
///      spawn the heavy bullet flying along the locked trajectory,
///      remove `SniperAim`, despawn the line, and reset the slot's
///      shot cooldown via `enemy.fire_cd`.
pub fn sniper_fire(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    friendly: Query<&Transform, (With<Friendly>, Without<Enemy>, Without<Ally>)>,
    allies: Query<(&Transform, &Ally), (With<Ally>, Without<Enemy>, Without<Friendly>)>,
    mut snipers: Query<(Entity, &Transform, &mut Enemy, Option<&mut SniperAim>)>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    let Ok(ftf) = friendly.single() else { return; };
    let fpos = ftf.translation.truncate();
    let ally_positions: Vec<Vec2> = allies
        .iter()
        .filter(|(_, a)| !ally_is_submerged(a))
        .map(|(t, _)| t.translation.truncate())
        .collect();

    for (entity, tf, mut enemy, aim) in &mut snipers {
        if enemy.variant != EnemyVariant::Sniper { continue; }
        let pos = tf.translation.truncate();
        enemy.fire_cd -= dt;

        if let Some(mut aim) = aim {
            // Aiming — tick down. On expiry, fire along the locked
            // trajectory and clean up.
            aim.remaining -= dt;
            if aim.remaining > 0.0 { continue; }
            let to = aim.target_world - pos;
            let dir = to.normalize_or(Vec2::Y);
            let bullet_pos = pos + dir * (ENEMY_BARREL_TIP + ENEMY_BULLET_HALF_LEN);
            let bullet = commands.spawn((
                Mesh2d(em.bullet_enemy_outer.clone()),
                MeshMaterial2d(pm.bullet_enemy_outer.clone()),
                Transform::from_xyz(bullet_pos.x, bullet_pos.y, 4.0)
                    .with_rotation(Quat::from_rotation_z((-dir.x).atan2(dir.y)))
                    .with_scale(Vec3::splat(SNIPER_BULLET_SCALE)),
                Bullet {
                    faction: FactionKind::Enemy,
                    damage: enemy.variant.fire_damage(),
                    remaining: SNIPER_FIRE_RANGE * 1.4,
                    weapon: WeaponType::Standard,
                    source: None,
                    runes: [None; 3],
                },
                Velocity(dir * SNIPER_BULLET_SPEED),
                RenderLayers::layer(PLAY_LAYER),
            )).id();
            let inner = commands.spawn((
                Mesh2d(em.bullet_enemy_inner.clone()),
                MeshMaterial2d(pm.bullet_enemy.clone()),
                Transform::from_xyz(0.0, 0.0, 0.05),
                RenderLayers::layer(PLAY_LAYER),
            )).id();
            commands.entity(inner).insert(ChildOf(bullet));

            // Tear down the aim — line is a free entity, so the
            // `sniper_aim_line_tick` system will catch it next
            // frame via the back-ref. Despawn here too for
            // immediate cleanup.
            commands.entity(aim.line).despawn();
            commands.entity(entity).remove::<SniperAim>();
            // Reset the shot cooldown so the next aim cycle can't
            // start until `1/fire_rate` seconds have passed.
            enemy.fire_cd = 1.0 / enemy.variant.fire_rate().max(0.1);
            continue;
        }

        // Idle — start a fresh aim if the cooldown is ready and a
        // target is in range.
        if enemy.fire_cd > 0.0 { continue; }
        let target_pos = nearest_target(pos, fpos, &ally_positions);
        let to = target_pos - pos;
        if to.length() > SNIPER_FIRE_RANGE { continue; }

        // Spawn the telegraph line. Free entity so the live transform
        // can span sniper → locked target without inheriting the
        // sniper's body rotation.
        let mid = (pos + target_pos) * 0.5;
        let length = to.length().max(1.0);
        let angle = (-(to.x)).atan2(to.y);
        let line = commands.spawn((
            Mesh2d(em.beam.clone()),
            MeshMaterial2d(pm.sniper_aim.clone()),
            Transform::from_xyz(mid.x, mid.y, 3.5)
                .with_rotation(Quat::from_rotation_z(angle))
                // Beam mesh is `Rectangle::new(1.0, BEAM_LENGTH)` —
                // scale Y to match the sniper-target distance and X
                // narrow to a hairline that grows during aim.
                .with_scale(Vec3::new(0.25, length / crate::balance::BEAM_LENGTH, 1.0)),
            SniperAimLine {
                sniper: entity,
                target_world: target_pos,
                aim_total: SNIPER_AIM_TIME,
                remaining: SNIPER_AIM_TIME,
            },
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(entity).insert(SniperAim {
            remaining: SNIPER_AIM_TIME,
            target_world: target_pos,
            line,
        });
    }
}

/// Per-frame: rotate every Sniper's `SniperTurret` child base so the
/// barrel points at the locked target (during aim) or the live
/// nearest target (idle). Local rotation = world-aim − body-heading,
/// so the barrel's WORLD orientation tracks the target regardless of
/// which way the body is moving.
pub fn sniper_turret_aim(
    snipers: Query<(&Transform, &Heading, &Enemy, Option<&SniperAim>, &Children)>,
    friendly: Query<&Transform, (With<Friendly>, Without<Enemy>, Without<Ally>)>,
    allies: Query<(&Transform, &Ally), (With<Ally>, Without<Enemy>, Without<Friendly>)>,
    mut turrets: Query<
        &mut Transform,
        (With<SniperTurret>, Without<Enemy>, Without<Friendly>, Without<Ally>),
    >,
) {
    let Ok(ftf) = friendly.single() else { return; };
    let fpos = ftf.translation.truncate();
    let ally_positions: Vec<Vec2> = allies
        .iter()
        .filter(|(_, a)| !ally_is_submerged(a))
        .map(|(t, _)| t.translation.truncate())
        .collect();

    for (tf, heading, enemy, aim, children) in &snipers {
        if enemy.variant != EnemyVariant::Sniper { continue; }
        let pos = tf.translation.truncate();
        // Aim phase locks the target — keep the barrel glued to the
        // telegraphed line so the visual matches the bullet path.
        let target = aim
            .map(|a| a.target_world)
            .unwrap_or_else(|| nearest_target(pos, fpos, &ally_positions));
        let to = target - pos;
        if to.length_squared() < 1.0 { continue; }
        let world_aim = (-to.x).atan2(to.y);
        // Body's transform.rotation == Heading.0 (set by enemy_ai).
        // Local turret rotation = world_aim - body_heading so the
        // child's WORLD rotation = body_heading + local = world_aim.
        let local = world_aim - heading.0;
        let want = Quat::from_rotation_z(local);
        for c in children.iter() {
            if let Ok(mut t_tf) = turrets.get_mut(c) {
                if t_tf.rotation != want { t_tf.rotation = want; }
            }
        }
    }
}

/// Per-frame sync of every aim-line entity:
///   - Despawn it if its source sniper is gone (e.g. shot dead mid-aim).
///   - Otherwise rewrite its transform to span sniper.position →
///     locked target_world (target stays frozen for the duration —
///     telegraph is a commitment).
///   - Pulse the line's width as the aim timer counts down so the
///     thread thickens at the moment of fire.
pub fn sniper_aim_line_tick(
    time: Res<Time>,
    mut commands: Commands,
    snipers: Query<(&Transform, Option<&SniperAim>), With<Enemy>>,
    mut lines: Query<(Entity, &mut Transform, &mut SniperAimLine), Without<Enemy>>,
) {
    let dt = time.delta_secs();
    for (line_entity, mut tf, mut line) in &mut lines {
        let Ok((sniper_tf, aim)) = snipers.get(line.sniper) else {
            // Sniper despawned — clean up the orphan line.
            commands.entity(line_entity).despawn();
            continue;
        };
        // If the SniperAim was removed before the line caught up
        // (e.g. fire path despawned us), drop the line.
        let Some(aim) = aim else {
            commands.entity(line_entity).despawn();
            continue;
        };
        line.remaining = (line.remaining - dt).max(0.0);
        let _ = aim; // keeping the back-ref consistent; aim.remaining
                     // is the canonical timer, mirrored above.

        let pos = sniper_tf.translation.truncate();
        let to = line.target_world - pos;
        let mid = (pos + line.target_world) * 0.5;
        let length = to.length().max(1.0);
        let angle = (-(to.x)).atan2(to.y);
        // Width pulse: starts hairline, grows to full as fire approaches.
        let progress = 1.0 - (line.remaining / line.aim_total).clamp(0.0, 1.0);
        let width = 0.25 + 0.75 * progress;
        tf.translation.x = mid.x;
        tf.translation.y = mid.y;
        tf.rotation = Quat::from_rotation_z(angle);
        tf.scale = Vec3::new(width, length / crate::balance::BEAM_LENGTH, 1.0);
    }
}

// ---------- Enemy HP bars (on-damage, fade after 3 s) ----------

/// How long the bar stays visible after the most recent damage tick.
const HP_BAR_SHOW_TIME: f32 = 3.0;
/// World-units offset above the enemy's center where the bar sits.
/// Tuned to sit just above standard enemies (~half-length 5–6) — boss
/// hulls (Carrier at ~12) will overlap the lower edge slightly, which
/// is fine: a bar that "hugs" the ship reads more clearly than one
/// floating in empty water above it.
const HP_BAR_Y_OFFSET:  f32 = 7.0;
/// Bar dimensions in world units (= internal pixels at this play
/// area's nearest-neighbor scale).
const HP_BAR_W: f32 = 8.0;
const HP_BAR_H: f32 = 1.0;

/// Cached mesh + material for the red HP bars. Built once at startup
/// so spawning a bar is a transform-and-component insert, no asset
/// alloc.
#[derive(Resource)]
pub struct EnemyHpBarAssets {
    pub mesh: Handle<Mesh>,
    pub fill: Handle<ColorMaterial>,
}

/// Build the cached HP-bar mesh + material once at startup.
pub fn setup_enemy_hp_bar_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
) {
    let mesh = meshes.add(Rectangle::new(HP_BAR_W, HP_BAR_H));
    let fill = materials.add(Color::srgb(0.92, 0.18, 0.22));
    commands.insert_resource(EnemyHpBarAssets { mesh, fill });
}

/// Detect HP drops and spawn / refresh the floating bar. Reads
/// `Health` against `PreviousHp`; on a strict decrease either
/// resets an existing bar's timer or spawns a new one. Runs in the
/// damage-application chain so it sees the new HP for the same
/// frame the hit landed.
pub fn track_enemy_damage_for_hp_bars(
    mut commands: Commands,
    assets: Option<Res<EnemyHpBarAssets>>,
    mut enemies: Query<(Entity, &Health, &mut PreviousHp), With<Enemy>>,
    mut bars: Query<&mut EnemyHpBar>,
) {
    let Some(assets) = assets else { return; };
    for (e, h, mut prev) in &mut enemies {
        if h.0 < prev.0 {
            // Took damage this frame. Find an existing bar for this
            // enemy and bump its timer; otherwise spawn one.
            let mut found = false;
            for mut bar in &mut bars {
                if bar.enemy == e {
                    bar.remaining = HP_BAR_SHOW_TIME;
                    found = true;
                    break;
                }
            }
            if !found {
                commands.spawn((
                    Mesh2d(assets.mesh.clone()),
                    MeshMaterial2d(assets.fill.clone()),
                    // World-space placement sync'd each frame by
                    // `update_enemy_hp_bars`. z = 5.5 sits above
                    // hulls (1.0) and bullets (4.0) so the bar
                    // doesn't get visually buried. Lives on
                    // `HUD_LAYER` so the HudCamera renders it at
                    // native resolution — the chunky-pixel filter
                    // doesn't apply, keeping the bar crisp.
                    Transform::from_xyz(0.0, 0.0, 5.5),
                    EnemyHpBar { enemy: e, remaining: HP_BAR_SHOW_TIME },
                    RenderLayers::layer(HUD_LAYER),
                ));
            }
        }
        prev.0 = h.0;
    }
}

/// Per-frame visual update for HP bars: ticks the fade timer, snaps
/// position to the enemy's current location, and writes the fill
/// scale + offset so the bar shrinks left-anchored as HP drops. Bars
/// despawn when their timer expires or their target enemy is gone.
pub fn update_enemy_hp_bars(
    time: Res<Time>,
    mut commands: Commands,
    enemies: Query<(&Transform, &Health, &Enemy), Without<EnemyHpBar>>,
    mut bars: Query<(Entity, &mut EnemyHpBar, &mut Transform)>,
) {
    let dt = time.delta_secs();
    for (bar_e, mut bar, mut tf) in &mut bars {
        bar.remaining -= dt;
        if bar.remaining <= 0.0 {
            commands.entity(bar_e).despawn();
            continue;
        }
        let Ok((e_tf, h, enemy)) = enemies.get(bar.enemy) else {
            commands.entity(bar_e).despawn();
            continue;
        };
        let max = enemy.max_hp.max(1) as f32;
        let ratio = (h.0 as f32 / max).clamp(0.0, 1.0);
        // Anchor the bar's left edge: the centered Rectangle scales
        // around its midpoint, so we shift the center by half the
        // empty width to keep the left edge fixed under the enemy.
        let world = e_tf.translation.truncate();
        tf.translation.x = world.x + HP_BAR_W * (ratio - 1.0) * 0.5;
        tf.translation.y = world.y + HP_BAR_Y_OFFSET;
        tf.scale.x = ratio;
        tf.scale.y = 1.0;
    }
}

/// Despawn enemies whose HP has dropped to 0, regardless of damage source
/// (bullet, beam, fire, future debuffs). Awards score, scrap, and XP, and
/// emits the generic enemy-color destruction burst — source-specific flair
/// (weapon-color sparks for bullets) is spawned at the call site before HP
/// hits zero.
///
/// XP grant: 1 per normal enemy, 5 per boss-tier kill. We detect boss-tier
/// by `Enemy.max_hp >= 50` since the smallest boss HP (Submarine, 60)
/// dwarfs the largest non-boss variant (Heavy, 15).
pub fn enemy_death_check(
    mut commands: Commands,
    mut score: ResMut<Score>,
    mut scrap: ResMut<Scrap>,
    mut xp: ResMut<crate::xp::Xp>,
    mut pending: ResMut<crate::xp::LevelUpsPending>,
    player_stats: Res<crate::stats::PlayerStats>,
    synergies: Res<crate::synergy::Synergies>,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    mut meshes: ResMut<Assets<Mesh>>,
    enemies: Query<(Entity, &Transform, &Health, &Enemy)>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let mut rng = rand::thread_rng();
    for (e, tf, h, enemy) in &enemies {
        if h.0 > 0 { continue; }
        commands.entity(e).despawn();
        score.0 += 10;
        // Base +1 scrap per kill, multiplied by the harvest tier roll
        // (RoR-style: 0% → 1, 50% → 50/50 between 1 and 2, 100% → 2, …).
        // The Pirate synergy then scales the result by 1.5×/2×/2.5×/3×
        // at T1/T2/T3/T4.
        let base_scrap = player_stats.roll_harvest_mult(&mut rng);
        let scrap_drop = (base_scrap as f32 * synergies.pirate_harvest_mult())
            .round()
            .max(0.0) as u32;
        scrap.0 = scrap.0.saturating_add(scrap_drop);
        // XP grant. Boss-tier detection by max_hp threshold (smallest
        // boss = 60 HP, largest variant = 15 HP).
        let is_boss = enemy.max_hp >= 50;
        crate::xp::grant_kill_xp(&mut xp, &mut pending, is_boss);
        let pos = tf.translation.truncate();
        spawn_hit_particles(&mut commands, &em, &pm.enemy, pos, 10, 60.0, &mut rng);

        // Rammer drops a time-fused landmine on death — regardless
        // of cause (contact-detonation drives HP to 0 via
        // `bomber_detonate`, bullets drive it to 0 via
        // `bullet_collisions`; both flow through here).
        if enemy.variant == EnemyVariant::Rammer {
            spawn_rammer_landmine(&mut commands, &pm, &mut meshes, pos);
        }
    }
}

/// Spawn the time-fused landmine a Rammer leaves behind. Two-tone
/// disc — dark shell + warning-orange dot — so the silhouette reads
/// as "stay clear" against the play area. Component-driven fuse and
/// AOE damage handled by `enemy_landmine_tick`.
fn spawn_rammer_landmine(
    commands: &mut Commands,
    pm: &PaletteMaterials,
    meshes: &mut Assets<Mesh>,
    pos: Vec2,
) {
    let outer_mesh = meshes.add(Circle::new(1.5));
    let inner_mesh = meshes.add(Circle::new(0.6));
    let mine = commands.spawn((
        Mesh2d(outer_mesh),
        MeshMaterial2d(pm.mine_outer.clone()),
        Transform::from_xyz(pos.x, pos.y, 0.5),
        EnemyLandmine {
            fuse: RAMMER_MINE_FUSE,
            damage: RAMMER_MINE_DAMAGE,
            blast_radius: RAMMER_MINE_RADIUS,
        },
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    let dot = commands.spawn((
        Mesh2d(inner_mesh),
        MeshMaterial2d(pm.enemy_mine_dot.clone()),
        Transform::from_xyz(0.0, 0.0, 0.05),
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    commands.entity(dot).insert(ChildOf(mine));
}
