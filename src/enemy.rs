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
use crate::components::{Faction, FactionKind, Friendly, Health, Heading, Velocity};
use crate::effects::{spawn_hit_particles, EffectMeshes, HitFx, MuzzleFlash};
use crate::palette::PaletteMaterials;
use crate::rune::FireExtent;
use crate::ship::approach_angle;
use crate::trails::{empty_dynamic_mesh, EnemyTrail};
use crate::weapon::WeaponType;
use crate::{GameMode, Score, Scrap, SpawnTimer};

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
}

impl EnemyVariant {
    pub fn hp(self) -> i32 {
        match self {
            EnemyVariant::Standard => 5,
            EnemyVariant::Heavy    => 15,
            EnemyVariant::Scout    => 2,
            EnemyVariant::Bomber   => 4,
        }
    }
    pub fn speed(self) -> f32 {
        match self {
            EnemyVariant::Standard => 18.0,
            EnemyVariant::Heavy    => 12.0,
            EnemyVariant::Scout    => 28.0,
            EnemyVariant::Bomber   => 26.0,
        }
    }
    pub fn turn_rate(self) -> f32 {
        match self {
            EnemyVariant::Standard => 0.9,
            EnemyVariant::Heavy    => 0.55,
            EnemyVariant::Scout    => 1.7,
            EnemyVariant::Bomber   => 0.6,
        }
    }
    /// Visual + collision scale applied via Transform.scale on the parent.
    pub fn scale(self) -> f32 {
        match self {
            EnemyVariant::Standard => 1.0,
            EnemyVariant::Heavy    => 1.5,
            EnemyVariant::Scout    => 0.7,
            EnemyVariant::Bomber   => 1.0,
        }
    }
    pub fn fire_rate(self) -> f32 {
        match self {
            EnemyVariant::Standard => 1.0,
            EnemyVariant::Heavy    => 0.7,
            EnemyVariant::Scout    => 1.5,
            EnemyVariant::Bomber   => 0.0,
        }
    }
    pub fn fire_damage(self) -> i32 {
        // Every enemy cannon does 1 damage. Variants differ in HP, speed,
        // and behaviour — not in shot weight. Re-introduce per-variant
        // damage when we want a clear "this enemy's bullet is scary"
        // archetype again.
        match self {
            _ => 1,
        }
    }
    pub fn has_gun(self) -> bool {
        !matches!(self, EnemyVariant::Bomber)
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

    if variant.has_gun() {
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

/// Sandbox spawner — drips one enemy at a time on a ramping timer from a
/// random edge of the play area. Disabled in Wave mode (the wave orchestrator
/// spawns the whole wave in a batch instead).
pub fn spawn_enemies(
    time: Res<Time>,
    mut timer: ResMut<SpawnTimer>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    mode: Res<GameMode>,
    mut combat_ctx: ResMut<crate::map::CombatContext>,
    enemies: Query<Entity, With<Enemy>>,
) {
    if *mode != GameMode::Sandbox { return; }
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    // Per-level total budget — once exhausted, no more enemies spawn
    // and `level_complete_check` will end the encounter when the
    // arena clears.
    if combat_ctx.enemy_budget == 0 { return; }
    timer.elapsed += time.delta_secs();
    timer.t -= time.delta_secs();
    if timer.t > 0.0 { return; }

    let interval = (3.0 - timer.elapsed * 0.025).max(0.5);
    timer.t = interval;

    // Concurrent on-screen cap scales with the section's stars
    // (set when the player entered this combat from the map).
    if enemies.iter().count() >= combat_ctx.enemy_cap() { return; }

    let mut rng = rand::thread_rng();
    let half = PLAY_WORLD / 2.0;
    let edge = rng.gen_range(0..4);
    let pos = match edge {
        0 => Vec2::new(rng.gen_range(-half..half), half + 20.0),
        1 => Vec2::new(rng.gen_range(-half..half), -half - 20.0),
        2 => Vec2::new(half + 20.0, rng.gen_range(-half..half)),
        _ => Vec2::new(-half - 20.0, rng.gen_range(-half..half)),
    };

    // Distribution: 50% Standard / 25% Scout / 15% Heavy / 10% Bomber.
    // Bombers are the highest-threat outlier so they stay rare.
    let variant = match rng.gen_range(0u32..100) {
        0..50  => EnemyVariant::Standard,
        50..75 => EnemyVariant::Scout,
        75..90 => EnemyVariant::Heavy,
        _      => EnemyVariant::Bomber,
    };

    let inward = (-pos).normalize();
    let heading = (-inward.x).atan2(inward.y);
    spawn_enemy(&mut commands, &pm, &em, &mut meshes, pos, heading, variant);
    combat_ctx.enemy_budget = combat_ctx.enemy_budget.saturating_sub(1);
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

        // Bombers skip the state machine — head straight at their
        // target, no waypoints, no firing. Detonation is handled by
        // `bomber_detonate`.
        if enemy.variant == EnemyVariant::Bomber {
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
                slot: None,
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

/// Bombers don't shoot — they self-destruct on contact with whichever
/// of the friendly ship or an ally is closest. Pulses the hit hull
/// and spawns a bigger-than-usual particle burst so the impact reads.
/// Damage is now applied in both modes (Sandbox + Wave) for parity
/// with the cannon damage path.
pub fn bomber_detonate(
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    bombers: Query<(Entity, &Transform, &Enemy)>,
    mut friendly: Query<
        (&Transform, &mut Health, &mut HitFx),
        (With<Friendly>, Without<Ally>),
    >,
    mut allies: Query<
        (Entity, &Transform, &Ally, &mut Health, &mut HitFx),
        (With<Ally>, Without<Friendly>),
    >,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let mut rng = rand::thread_rng();

    for (be, btf, enemy) in &bombers {
        if enemy.variant != EnemyVariant::Bomber { continue; }
        let bp = btf.translation.truncate();

        // Friendly first — preferred target if in range. Skipped only
        // if the ship has somehow despawned.
        let mut detonated = false;
        if let Ok((ftf, mut h, mut fx)) = friendly.single_mut() {
            if btf.translation.truncate().distance(ftf.translation.truncate())
                < BOMBER_DETONATE_DIST
            {
                fx.pulse();
                h.0 = (h.0 - 5).max(0);
                detonated = true;
            }
        }
        // Otherwise check allies — closest non-submerged one in range
        // eats it. Submarines are stealth, so bombers can't sense them.
        if !detonated {
            let mut best: Option<(Entity, f32)> = None;
            for (ae, atf, ally, _, _) in &allies {
                if ally_is_submerged(ally) { continue; }
                let d = btf.translation.truncate()
                    .distance(atf.translation.truncate());
                if d < BOMBER_DETONATE_DIST
                    && best.map_or(true, |(_, bd)| d < bd)
                {
                    best = Some((ae, d));
                }
            }
            if let Some((ae, _)) = best {
                if let Ok((_, _, _, mut h, mut fx)) = allies.get_mut(ae) {
                    fx.pulse();
                    h.0 = (h.0 - 5).max(0);
                    detonated = true;
                }
            }
        }

        if detonated {
            commands.entity(be).despawn();
            // Two-tone burst: enemy color + bright bomber-warhead color.
            spawn_hit_particles(&mut commands, &em, &pm.enemy,        bp, 14, 80.0,  &mut rng);
            spawn_hit_particles(&mut commands, &em, &pm.bullet_enemy, bp, 8,  100.0, &mut rng);
        }
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
/// (bullet, beam, fire, future debuffs). Awards score and emits the generic
/// enemy-color destruction burst — source-specific flair (weapon-color
/// sparks for bullets) is spawned at the call site before HP hits zero.
pub fn enemy_death_check(
    mut commands: Commands,
    mut score: ResMut<Score>,
    mut scrap: ResMut<Scrap>,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    enemies: Query<(Entity, &Transform, &Health), With<Enemy>>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let mut rng = rand::thread_rng();
    for (e, tf, h) in &enemies {
        if h.0 > 0 { continue; }
        commands.entity(e).despawn();
        score.0 += 10;
        // +1 scrap per kill — spent on map-view building placement.
        scrap.0 = scrap.0.saturating_add(1);
        let pos = tf.translation.truncate();
        spawn_hit_particles(&mut commands, &em, &pm.enemy, pos, 10, 60.0, &mut rng);
    }
}
