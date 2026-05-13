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
    ARENA_H, ARENA_W, BOMBER_DETONATE_DIST, BULLET_SPEED, ENEMY_BARREL_TIP,
    ENEMY_BULLET_HALF_LEN, ENEMY_LEN, ENEMY_RANGE, ENEMY_WIDTH, PLAY_LAYER,
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

// Submodules — variant-specific behaviour (sniper aim, artillery
// telegraph, rammer mine) and the HP-bar visuals are split out so
// this file stays focused on shared state.
pub mod artillery;
pub mod hp_bar;
pub mod rammer;
pub mod sniper;

pub use hp_bar::{
    setup_enemy_hp_bar_assets, track_enemy_damage_for_hp_bars,
    update_enemy_hp_bars,
};

pub use artillery::{
    artillery_fire, artillery_shell_tick, ARTILLERY_DESIRED_DIST,
};
pub use rammer::enemy_landmine_tick;
pub use sniper::{
    sniper_aim_line_tick, sniper_fire, sniper_turret_aim,
    SniperTurret, SNIPER_DESIRED_DIST,
};

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
    /// Default idle state; reserved for future "no current target"
    /// AI without removing the variant when nothing constructs it.
    #[allow(dead_code)]
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
    /// AOE shell lobber. Keeps `ARTILLERY_DESIRED_DIST` away,
    /// telegraphs a landing reticle for `ARTILLERY_TELEGRAPH_TIME`s,
    /// then arcs a shell that explodes for splash damage. The
    /// telegraph is dodgeable — punishes "stand and shoot".
    Artillery,
}

impl EnemyVariant {
    pub fn hp(self) -> i32 {
        // Tuned so HP escalates with introduction order: a player
        // shouldn't see the tankiest variant until they've cleared
        // enough stages to handle it. See `variant_mix_for_stage`
        // for the introduction schedule.
        match self {
            EnemyVariant::Bomber   => 2,   // intro: stage 0 — paper-thin kamikaze
            EnemyVariant::Scout    => 3,   // intro: stage 1
            EnemyVariant::Rammer   => 6,   // intro: stage 3
            EnemyVariant::Standard => 8,   // intro: stage 2
            EnemyVariant::Sniper   => 14,  // intro: stage 4
            EnemyVariant::Heavy    => 104,  // intro: stage 5
            EnemyVariant::Artillery=> 52,  // intro: stage 6 — apex AOE tank
        }
    }
    pub fn speed(self) -> f32 {
        match self {
            EnemyVariant::Standard => 18.0,
            EnemyVariant::Heavy    => 16.0,
            EnemyVariant::Scout    => 28.0,
            EnemyVariant::Bomber   => 39.0,
            EnemyVariant::Rammer   => 45.0,
            EnemyVariant::Sniper   => 26.0,
            EnemyVariant::Artillery=> 6.0,
        }
    }
    pub fn turn_rate(self) -> f32 {
        match self {
            EnemyVariant::Standard => 0.9,
            EnemyVariant::Heavy    => 0.55,
            EnemyVariant::Scout    => 1.7,
            EnemyVariant::Bomber   => 1.4,
            EnemyVariant::Rammer   => 1.4,
            EnemyVariant::Sniper   => 0.5,
            EnemyVariant::Artillery=> 0.4,
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
            EnemyVariant::Artillery=> 1.05,
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
            // Artillery: ~3s between shots (1.5s telegraph + 1.5s
            // recovery). Cooldown gates the next reticle spawn.
            EnemyVariant::Artillery=> 0.33,
        }
    }
    pub fn fire_damage(self) -> i32 {
        // Most enemy cannons do 3 damage. Sniper hits noticeably
        // harder to make the long telegraph feel earned. Artillery
        // does middling damage but in an area.
        match self {
            EnemyVariant::Sniper    => 25,
            EnemyVariant::Artillery => 20,
            _ => 3,
        }
    }
    pub fn has_gun(self) -> bool {
        // Bomber + Rammer have no gun; Artillery has a "gun" only
        // in the loose sense — it has its own bespoke firing path
        // (`artillery_fire`) so the standard `enemy_fire` bullet
        // dispatch must skip it.
        !matches!(
            self,
            EnemyVariant::Bomber | EnemyVariant::Rammer | EnemyVariant::Artillery
        )
    }

    /// Display name used by the first-encounter banner and any UI
    /// that names the variant.
    pub fn label(self) -> &'static str {
        match self {
            EnemyVariant::Standard  => "STANDARD",
            EnemyVariant::Heavy     => "HEAVY",
            EnemyVariant::Scout     => "SCOUT",
            EnemyVariant::Bomber    => "BOMBER",
            EnemyVariant::Rammer    => "RAMMER",
            EnemyVariant::Sniper    => "SNIPER",
            EnemyVariant::Artillery => "ARTILLERY",
        }
    }

}

/// Per-variant spawn weight at this stage of the campaign. Weights
/// move smoothly (rust-SNKRX `variant_mix_for_round` pattern) instead
/// of binary unlock gates — stage 1 is 100% Bomber because everyone
/// else's weight is 0.0; later stages add new threats as small slices
/// while keeping the early variants common.
///
/// Bomber stays ≥30% even at peak progression so the rhythm of "loads
/// of kamikazes plus some spice" remains the through-line.
fn variant_mix_for_stage(battles_cleared: u32, boss_wave: bool) -> [(EnemyVariant, f32); 7] {
    let c = battles_cleared;
    if boss_wave {
        // Boss-wave mix — heavier on tough variants, but Bomber stays
        // a strong presence so the wave reads as "the usual swarm,
        // hardened" rather than a different encounter.
        return [
            (EnemyVariant::Bomber,    boss_bomber_weight(c)),
            (EnemyVariant::Heavy,     boss_heavy_weight(c)),
            (EnemyVariant::Rammer,    boss_rammer_weight(c)),
            (EnemyVariant::Artillery, boss_artillery_weight(c)),
            (EnemyVariant::Sniper,    boss_sniper_weight(c)),
            (EnemyVariant::Standard,  boss_standard_weight(c)),
            (EnemyVariant::Scout,     boss_scout_weight(c)),
        ];
    }
    [
        // Bomber dominates at first; floored at 0.30 forever so the
        // signature kamikaze rhythm survives the variant-mix bloom.
        (EnemyVariant::Bomber,    bomber_weight(c).max(0.30)),
        (EnemyVariant::Scout,     scout_weight(c)),
        (EnemyVariant::Standard,  standard_weight(c)),
        (EnemyVariant::Heavy,     heavy_weight(c)),
        (EnemyVariant::Rammer,    rammer_weight(c)),
        (EnemyVariant::Sniper,    sniper_weight(c)),
        (EnemyVariant::Artillery, artillery_weight(c)),
    ]
}

// ---- Standard-wave per-variant weight schedules ----

fn bomber_weight(c: u32) -> f32 {
    match c {
        0 => 1.00,
        1 => 0.65,
        2 => 0.55,
        3 => 0.45,
        4 => 0.40,
        _ => 0.35,
    }
}
fn scout_weight(c: u32) -> f32 {
    match c {
        0 => 0.0,
        1 => 0.30,
        2 => 0.20,
        3 => 0.18,
        _ => 0.15,
    }
}
fn standard_weight(c: u32) -> f32 {
    match c {
        0 | 1 => 0.0,
        2     => 0.20,
        3     => 0.18,
        _     => 0.15,
    }
}
fn rammer_weight(c: u32) -> f32 {
    match c {
        0..=2 => 0.0,
        3     => 0.10,
        4     => 0.10,
        _     => 0.12,
    }
}
fn sniper_weight(c: u32) -> f32 {
    match c {
        0..=3 => 0.0,
        4     => 0.08,
        5     => 0.10,
        _     => 0.10,
    }
}
fn heavy_weight(c: u32) -> f32 {
    match c {
        0..=4 => 0.0,
        5     => 0.08,
        6     => 0.10,
        _     => 0.10,
    }
}
fn artillery_weight(c: u32) -> f32 {
    match c {
        0..=5 => 0.0,
        6     => 0.05,
        _     => 0.08,
    }
}

// ---- Boss-wave per-variant weight schedules ----

fn boss_bomber_weight(c: u32) -> f32 {
    // Boss waves still kamikaze-heavy — Bomber is the constant.
    match c { 0 => 1.0, 1 => 0.50, _ => 0.30 }
}
fn boss_rammer_weight(c: u32) -> f32 {
    match c {
        0..=2 => 0.0,
        3     => 0.20,
        _     => 0.25,
    }
}
fn boss_sniper_weight(c: u32) -> f32 {
    match c { 0..=3 => 0.0, 4 => 0.12, _ => 0.15 }
}
fn boss_heavy_weight(c: u32) -> f32 {
    match c { 0..=4 => 0.0, _ => 0.20 }
}
fn boss_artillery_weight(c: u32) -> f32 {
    match c { 0..=5 => 0.0, _ => 0.15 }
}
fn boss_standard_weight(c: u32) -> f32 {
    match c { 0..=1 => 0.0, _ => 0.08 }
}
fn boss_scout_weight(c: u32) -> f32 {
    match c { 0 => 0.0, _ => 0.05 }
}

impl EnemyVariant {
    /// Weighted pick from this stage's mix table. Total weights need
    /// not sum to 1.0 — the roll normalises against the live sum so
    /// missing variants just lower the cumulative density rather than
    /// over-weighting whatever remains.
    pub fn roll_weighted(
        battles_cleared: u32,
        boss_wave: bool,
        rng: &mut impl Rng,
    ) -> EnemyVariant {
        let mix = variant_mix_for_stage(battles_cleared, boss_wave);
        let total: f32 = mix.iter().map(|(_, w)| *w).sum();
        if total <= 0.0 { return EnemyVariant::Bomber; }
        let mut roll: f32 = rng.gen_range(0.0..total);
        for (v, w) in &mix {
            if roll < *w { return *v; }
            roll -= *w;
        }
        EnemyVariant::Bomber
    }
}

/// Every variant in declaration order — used by the onboarding
/// banner module to enumerate variants without each module
/// rewriting the list.
pub const ALL_VARIANTS: &[EnemyVariant] = &[
    EnemyVariant::Standard,
    EnemyVariant::Heavy,
    EnemyVariant::Scout,
    EnemyVariant::Bomber,
    EnemyVariant::Rammer,
    EnemyVariant::Sniper,
    EnemyVariant::Artillery,
];

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
        EnemyVariant::Artillery=> pm.enemy_artillery.clone(),
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
///   enemy per `wave_spawn_interval`, despawning each indicator as
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
    mut seen: ResMut<crate::onboarding::SeenVariants>,
    mut boss_intro_pending: ResMut<crate::boss_intro::BossIntroPending>,
    mut scrap_w: crate::stage_complete::ScrapWriter,
    existing_banners: Query<Entity, With<crate::onboarding::NotificationLifetime>>,
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
            //
            // Position mix: single drops + clustered drops (2-4 enemies
            // arriving from roughly the same direction). Stage 1
            // (battles_cleared = 0) skips clusters entirely so the
            // bomber-only opener stays orderly while the player learns
            // the threat. Cluster chance ramps up with progression.
            if combat_ctx.pending_spawns.is_empty() && combat_ctx.wave_remaining > 0 {
                let mut rng = rand::thread_rng();
                let total = combat_ctx.wave_remaining as usize;
                let mut queue = Vec::with_capacity(total);
                let cluster_chance: f32 = match combat_ctx.battles_cleared {
                    0 => 0.0,
                    1 => 0.15,
                    2 => 0.25,
                    _ => 0.35,
                };
                let mut remaining = total;
                while remaining > 0 {
                    let use_cluster = remaining >= 2 && rng.gen::<f32>() < cluster_chance;
                    if use_cluster {
                        let size = rng.gen_range(2..=4).min(remaining);
                        let center = random_edge_pos(&mut rng);
                        for _ in 0..size {
                            let pos = center + Vec2::new(
                                rng.gen_range(-6.0..6.0),
                                rng.gen_range(-6.0..6.0),
                            );
                            let indicator = spawn_indicator(&mut commands, &indicator_assets, pos);
                            queue.push(PendingSpawn { pos, indicator });
                        }
                        remaining -= size;
                    } else {
                        let pos = random_edge_pos(&mut rng);
                        let indicator = spawn_indicator(&mut commands, &indicator_assets, pos);
                        queue.push(PendingSpawn { pos, indicator });
                        remaining -= 1;
                    }
                }
                combat_ctx.pending_spawns = queue;
                combat_ctx.spawn_tick = WAVE_TELEGRAPH_DELAY;

                // Final wave of a 5★ section: queue the Borderlands-
                // style intro instead of dropping the boss in directly.
                // `boss_intro::exit_boss_intro` does the actual
                // `spawn_boss` call after the overlay finishes. Combat
                // freezes for the duration because `BossIntro` isn't in
                // `in_combat_view`'s allow-list.
                if combat_ctx.wave_idx + 1 == combat_ctx.wave_count {
                    if let Some(class) = combat_ctx.boss_pending.take() {
                        let pos = random_edge_pos(&mut rng);
                        let heading = (-pos.x).atan2(pos.y) + std::f32::consts::PI;
                        boss_intro_pending.class = Some(class);
                        boss_intro_pending.pos = pos;
                        boss_intro_pending.heading = heading;
                        // Carry the section's star tier so `spawn_boss`
                        // can scale HP accordingly — 3★ boss vs 5★ boss
                        // of the same class differ by ~1.67× HP.
                        boss_intro_pending.stars = combat_ctx.stars;
                        boss_intro_pending.battles_cleared = combat_ctx.battles_cleared;
                        next_state.set(crate::AppState::BossIntro);
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

            let spawned_variant = spawn_one_at(
                &mut commands, &pm, &em, &mut meshes,
                spawn.pos, combat_ctx.is_boss_wave,
                combat_ctx.battles_cleared,
            );
            // First-encounter banner: if the player hasn't seen this
            // variant yet THIS run, mark it + drop the bottom-left
            // panel so they get a heads-up about the new threat.
            if !seen.has(spawned_variant) {
                seen.mark(spawned_variant);
                // One enemy drips in per Update tick, so the query
                // count is current at this point — no buffered-spawn
                // race like the synergy-discovery path has.
                crate::onboarding::spawn_new_enemy_banner(
                    &mut commands,
                    existing_banners.iter().count(),
                    spawned_variant,
                );
            }
            combat_ctx.spawn_tick =
                crate::balance::wave_spawn_interval(combat_ctx.battles_cleared);
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
                // Fixed +1 scrap per cleared wave — the bulk of the
                // economy alongside interest + boss bounty. `grant`
                // updates the live total + the stage tally together.
                scrap_w.grant(1);
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

/// Pause between indicator-pop and the first drip — gives the player
/// a beat to read the directions before enemies start arriving.
/// Per-tick drip interval is supplied by `balance::wave_spawn_interval`
/// (scales down with `battles_cleared`).
const WAVE_TELEGRAPH_DELAY: f32 = 0.8;

/// Seconds between chaos drips while a boss is alive. Tender bosses
/// can't attack, so they get a faster cadence to keep pressure on the
/// player even when the boss itself is just slow-rolling around.
const BOSS_CHAOS_INTERVAL_DEFAULT: f32 = 3.5;
const BOSS_CHAOS_INTERVAL_TENDER: f32 = 2.2;

/// While at least one boss is alive in the arena, periodically drop a
/// fresh enemy at the edge to keep the chaos up. These spawns do NOT
/// count against `enemy_budget` — they're flavour bonus enemies for
/// the boss phase only, and naturally stop the moment the boss dies.
/// Tender bosses spawn faster because they have no offensive output.
pub fn boss_chaos_spawn(
    time: Res<Time>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    mode: Res<GameMode>,
    view: Res<crate::map::ViewMode>,
    mut combat_ctx: ResMut<crate::map::CombatContext>,
    bosses: Query<&Ally, (With<Enemy>, With<Ally>)>,
    enemies: Query<Entity, With<Enemy>>,
) {
    if *mode != GameMode::Sandbox { return; }
    if !matches!(*view, crate::map::ViewMode::Combat) { return; }
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };

    // No live boss → reset the timer so the first chaos spawn after a
    // boss appears uses the full interval, not an already-expired one.
    let Some(boss_ally) = bosses.iter().next() else {
        combat_ctx.boss_chaos_cd = 0.0;
        return;
    };

    let interval = if matches!(boss_ally.class, crate::ally::ShipClass::Tender) {
        BOSS_CHAOS_INTERVAL_TENDER
    } else {
        BOSS_CHAOS_INTERVAL_DEFAULT
    };

    if combat_ctx.boss_chaos_cd <= 0.0 {
        combat_ctx.boss_chaos_cd = interval;
    }
    combat_ctx.boss_chaos_cd -= time.delta_secs();
    if combat_ctx.boss_chaos_cd > 0.0 { return; }

    // Respect the on-screen cap so we don't snowball into a renderer
    // stall if the player can't keep up.
    if enemies.iter().count() >= combat_ctx.enemy_cap() {
        combat_ctx.boss_chaos_cd = 0.5;
        return;
    }

    let mut rng = rand::thread_rng();
    let pos = random_edge_pos(&mut rng);
    spawn_one_at(
        &mut commands, &pm, &em, &mut meshes,
        pos, /*boss_wave=*/ true, combat_ctx.battles_cleared,
    );
    combat_ctx.boss_chaos_cd = interval;
}

/// If `pos` is past the playable arena edge on either axis, replace
/// the proposed `motion_dir` with one that pushes back inward.
/// Used by stand-off enemies (Sniper, Artillery) so their
/// distance-management AI doesn't accidentally walk them off the
/// map and out of reach. Returns the original direction when the
/// enemy is comfortably inside the bounds.
fn inward_correction(pos: Vec2, motion_dir: Vec2) -> Vec2 {
    // 5-unit margin so the correction kicks in before the enemy
    // visually reaches the wall, not after.
    let margin = 5.0;
    let half_w = ARENA_W * 0.5 - margin;
    let half_h = ARENA_H * 0.5 - margin;
    let outside_right = pos.x > half_w;
    let outside_left = pos.x < -half_w;
    let outside_top = pos.y > half_h;
    let outside_bot = pos.y < -half_h;
    if !(outside_right || outside_left || outside_top || outside_bot) {
        return motion_dir;
    }
    // Build a corrective vector that points back toward the centre
    // along whichever axes are out of bounds.
    let cx = if outside_right { -1.0 } else if outside_left { 1.0 } else { motion_dir.x };
    let cy = if outside_top { -1.0 } else if outside_bot { 1.0 } else { motion_dir.y };
    Vec2::new(cx, cy).normalize_or(motion_dir)
}

fn random_edge_pos(rng: &mut rand::rngs::ThreadRng) -> Vec2 {
    // Spawns sit `+20` past the arena edge so enemies drift INTO view
    // rather than popping in at the wall. With `big_arena` the arena
    // is larger than the viewport — early spawns can appear off-camera
    // and steam toward the player, which is exactly the intent.
    let half_w = ARENA_W * 0.5;
    let half_h = ARENA_H * 0.5;
    let edge = rng.gen_range(0..4);
    match edge {
        0 => Vec2::new(rng.gen_range(-half_w..half_w), half_h + 20.0),
        1 => Vec2::new(rng.gen_range(-half_w..half_w), -half_h - 20.0),
        2 => Vec2::new(half_w + 20.0, rng.gen_range(-half_h..half_h)),
        _ => Vec2::new(-half_w - 20.0, rng.gen_range(-half_h..half_h)),
    }
}

fn spawn_one_at(
    commands: &mut Commands,
    pm: &PaletteMaterials,
    em: &EffectMeshes,
    meshes: &mut Assets<Mesh>,
    pos: Vec2,
    boss_wave: bool,
    battles_cleared: u32,
) -> EnemyVariant {
    let mut rng = rand::thread_rng();
    let variant = EnemyVariant::roll_weighted(battles_cleared, boss_wave, &mut rng);
    let inward = (-pos).normalize_or(Vec2::Y);
    let heading = (-inward.x).atan2(inward.y);
    spawn_enemy(commands, pm, em, meshes, pos, heading, variant);
    variant
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
    let inset = 5.0;
    let inner_x = ARENA_W * 0.5 - inset;
    let inner_y = ARENA_H * 0.5 - inset;
    let pos = Vec2::new(
        spawn_pos.x.clamp(-inner_x, inner_x),
        spawn_pos.y.clamp(-inner_y, inner_y),
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
    ally_cache: Res<crate::ally::AllyPositionsCache>,
    // Stunned enemies skip AI — Velocity holds at last frame's value
    // and `apply_velocity` early-outs for Stunned regardless.
    // `Without<Ally>` excludes bosses — they carry both `Enemy` (for
    // bullet/death routing) and `Ally` (for class-aware AI from
    // `ally_ai`), so generic enemy AI must NOT also fight to drive
    // their velocity each frame.
    mut q: Query<
        (&mut Transform, &mut Velocity, &mut Heading, &mut Enemy),
        (Without<crate::components::Stunned>, Without<Ally>),
    >,
) {
    let dt = time.delta_secs();
    let Ok(ftf) = friendly.single() else { return; };
    let fpos = ftf.translation.truncate();
    let ally_positions = &ally_cache.positions;
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
        let target_pos = nearest_target(pos, fpos, ally_positions);

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
            // Confine to the play area: if the sniper is past the
            // arena edge, override the desired motion direction
            // with one pointing back toward the centre. Otherwise
            // they'd drift offscreen and become unreachable.
            let motion_dir = inward_correction(pos, motion_dir);
            let desired = (-motion_dir.x).atan2(motion_dir.y);
            heading.0 = approach_angle(heading.0, desired, turn * dt);
            let dir = Vec2::new(-heading.0.sin(), heading.0.cos());
            vel.0 = dir * speed * speed_mult;
            tf.rotation = Quat::from_rotation_z(heading.0);
            continue;
        }

        // Artillery — same distance-management shape as Sniper but
        // farther back (`ARTILLERY_DESIRED_DIST = 110`). Body faces
        // the motion direction; the lobbed shell doesn't need the
        // body to point at the target.
        if enemy.variant == EnemyVariant::Artillery {
            let to = target_pos - pos;
            let dist = to.length();
            let unit_to = to.normalize_or(Vec2::Y);
            let (motion_dir, speed_mult) = if dist < ARTILLERY_DESIRED_DIST - 15.0 {
                (-unit_to, 0.7)
            } else if dist > ARTILLERY_DESIRED_DIST + 20.0 {
                (unit_to, 0.5)
            } else {
                (Vec2::new(-unit_to.y, unit_to.x), 0.3)
            };
            // Same arena confinement as Sniper. Without it artillery
            // drifts off the playable rect (they prefer keeping
            // distance) and stops firing because `artillery_fire`
            // gates on `in_play_area`.
            let motion_dir = inward_correction(pos, motion_dir);
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
    ally_cache: Res<crate::ally::AllyPositionsCache>,
    // Stunned enemies hold fire — same gate as movement.
    // `Without<Ally>` excludes bosses — they fire via their
    // class-specific paths (cannons, missiles, boarding, oil, etc.),
    // not the generic Standard-enemy bullet this system spawns.
    mut enemies: Query<
        (Entity, &Transform, &Heading, &mut Enemy),
        (
            Without<crate::components::Stunned>,
            Without<Ally>,
            Without<crate::harpoon::Harpooned>,
        ),
    >,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    let Ok(ftf) = friendly.single() else { return; };
    let fpos = ftf.translation.truncate();
    let ally_positions = &ally_cache.positions;

    for (enemy_entity, tf, heading, mut enemy) in &mut enemies {
        if !enemy.variant.has_gun() { continue; }
        // Sniper has its own firing pipeline (aim → telegraph → heavy).
        if enemy.variant == EnemyVariant::Sniper { continue; }
        enemy.fire_cd -= dt;
        let pos = tf.translation.truncate();
        // Off-screen enemies hold fire so they don't plink from outside
        // the visible arena.
        if !crate::balance::in_play_area(pos) { continue; }
        let target_pos = nearest_target(pos, fpos, ally_positions);
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
    // `Without<Ally>` keeps boss ships (which carry both Enemy and
    // Ally) out of the kamikaze branch — they're not actually
    // Bomber/Rammer variants anyway, but the filter is explicit so
    // future variant changes can't accidentally turn bosses into
    // self-destructors.
    mut bombers: Query<(Entity, &Transform, &Enemy, &mut Health), Without<Ally>>,
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
            EnemyVariant::Bomber => (BOMBER_DETONATE_DIST, 15),
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
    mut scrap_earned: ResMut<crate::stage_complete::ScrapEarnedThisStage>,
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
        // Harvest = chance an enemy drops 1 scrap on death. Pirate
        // synergy multiplies the chance. Most kills give nothing on
        // their own — wave clears, interest, and the boss bounty are
        // the bulk of the economy.
        let scrap_drop = player_stats.roll_harvest_drop(&mut rng, synergies.pirate_harvest_mult());
        scrap.0 = scrap.0.saturating_add(scrap_drop);
        // Mirror the increment into the per-stage tally so the
        // StageComplete transition can render "+N SCRAP" earned
        // this round.
        scrap_earned.0 = scrap_earned.0.saturating_add(scrap_drop);
        // XP grant. Boss-tier detection by max_hp threshold (smallest
        // boss = 60 HP, largest variant = 15 HP).
        let is_boss = enemy.max_hp >= 50;
        crate::xp::grant_kill_xp(&mut xp, &mut pending, &player_stats, is_boss);
        let pos = tf.translation.truncate();
        spawn_hit_particles(&mut commands, &em, &pm.enemy, pos, 10, 60.0, &mut rng);

        // Rammer drops a time-fused landmine on death — regardless
        // of cause (contact-detonation drives HP to 0 via
        // `bomber_detonate`, bullets drive it to 0 via
        // `bullet_collisions`; both flow through here).
        if enemy.variant == EnemyVariant::Rammer {
            rammer::spawn_rammer_landmine(&mut commands, &pm, &mut meshes, pos);
        }
    }
}

