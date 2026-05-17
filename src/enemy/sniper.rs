//! Sniper variant: independent-rotation turret + aim telegraph + heavy
//! shot. Body heading and barrel direction are decoupled, giving the
//! sniper effective 360° fire while its movement AI keeps it kiting.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;

use crate::ally::Ally;
use crate::balance::{
    BULLET_SPEED, ENEMY_BARREL_TIP, ENEMY_BULLET_HALF_LEN, PLAY_LAYER,
};
use crate::bullet::Bullet;
use crate::components::{FactionKind, Friendly, Heading, Velocity};
use crate::effects::EffectMeshes;
use crate::palette::PaletteMaterials;
use crate::weapon::WeaponType;

use super::{nearest_target, Enemy, EnemyVariant};

/// Distance the Sniper tries to maintain. Inside `desired - 10` it
/// flees, outside `desired + 15` it closes at half speed, in between
/// it orbits slowly.
pub const SNIPER_DESIRED_DIST: f32 = 80.0;

/// Effective firing range. Bigger than `ENEMY_RANGE` so the sniper can
/// engage from past the standard enemy threat ring.
pub const SNIPER_FIRE_RANGE: f32 = 100.0;

/// Aim duration — long enough to dodge with reasonable reaction time,
/// short enough that standing still gets punished.
pub const SNIPER_AIM_TIME: f32 = 1.5;

/// Speed of the sniper's heavy bullet. Faster than `BULLET_SPEED` so
/// the dodge window doesn't last forever once the shot fires.
pub const SNIPER_BULLET_SPEED: f32 = 140.0;

/// Visual scale applied to enemy bullet meshes when rendered as a
/// sniper round. Reads as a heavier shell vs the regular pellets.
pub const SNIPER_BULLET_SCALE: f32 = 1.6;

/// Marker on a Sniper that's currently in its 1.5 s aim phase. Holds
/// the snapshotted target world position (so the bullet flies along
/// the telegraphed line even if the target moves) and the entity ID
/// of the visible aim-line decoration.
#[derive(Component)]
pub struct SniperAim {
    pub remaining: f32,
    pub target_world: Vec2,
    pub line: Entity,
}

/// Free-floating aim-line entity from sniper.position → locked
/// `target_world`. Its `Transform` is rewritten each frame from the
/// live sniper position; the target stays frozen.
#[derive(Component)]
pub struct SniperAimLine {
    pub sniper: Entity,
    pub target_world: Vec2,
    /// Seconds until fire, mirrored from `SniperAim.remaining`.
    pub remaining: f32,
}

/// Marker on the Sniper's independent-rotation turret base. The barrel
/// mesh is parented to this entity (not directly to the body), so
/// rotating this base orbits the barrel while the body heads wherever
/// movement AI dictates.
#[derive(Component)]
pub struct SniperTurret;

/// Sniper firing pipeline — runs separately from `enemy_fire` because
/// the sniper has bespoke aim/telegraph/heavy-shot semantics.
///
/// Two phases:
///   1. Idle (no `SniperAim`): if a target is in `SNIPER_FIRE_RANGE`
///      and cooldown is ready, snapshot the target's world position,
///      insert `SniperAim`, spawn the aim-line decoration.
///   2. Aiming (has `SniperAim`): tick down. On 0, fire along the
///      locked trajectory, remove `SniperAim`, despawn the line,
///      reset the shot cooldown.
pub fn sniper_fire(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    friendly: Query<&Transform, (With<Friendly>, Without<Enemy>, Without<Ally>)>,
    ally_cache: Res<crate::ally::AllyPositionsCache>,
    mut snipers: Query<
        (Entity, &Transform, &mut Enemy, Option<&mut SniperAim>),
        Without<crate::harpoon::Harpooned>,
    >,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    // MP: host has multiple Friendly entities — collect all so the
    // sniper picks whichever player is closer instead of bailing on
    // `single()` Err.
    let friendly_positions: Vec<Vec2> = friendly
        .iter()
        .map(|t| t.translation.truncate())
        .collect();
    if friendly_positions.is_empty() { return; }
    let ally_positions = &ally_cache.positions;

    for (entity, tf, mut enemy, aim) in &mut snipers {
        if enemy.variant != EnemyVariant::Sniper { continue; }
        let pos = tf.translation.truncate();
        enemy.fire_cd -= dt;
        if aim.is_none() && !crate::balance::in_play_area(pos) { continue; }

        if let Some(mut aim) = aim {
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
                    runes: Vec::new(),
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

            commands.entity(aim.line).despawn();
            commands.entity(entity).remove::<SniperAim>();
            enemy.fire_cd = 1.0 / enemy.variant.fire_rate().max(0.1);
            continue;
        }

        if enemy.fire_cd > 0.0 { continue; }
        let target_pos = nearest_target(pos, &friendly_positions, ally_positions);
        let to = target_pos - pos;
        if to.length() > SNIPER_FIRE_RANGE { continue; }

        // Translucent FF-style aim line — sits steady for the full
        // window without flicker or pulse. Spawned as a wrapper with
        // a thicker dark outline child + inner colour main child so
        // it carries the same "danger telegraph" outline vocabulary
        // as the artillery reticle.
        let mid = (pos + target_pos) * 0.5;
        let length = to.length().max(1.0);
        let angle = (-(to.x)).atan2(to.y);
        let length_scale = length / crate::balance::BEAM_LENGTH;
        // Outline is ~1.5× the inner-beam width. Same length so the
        // tip ends register cleanly without a halo past the line.
        const INNER_WIDTH:   f32 = 1.4;
        const OUTLINE_WIDTH: f32 = 2.4;
        let line = commands.spawn((
            Transform::from_xyz(mid.x, mid.y, 3.5)
                .with_rotation(Quat::from_rotation_z(angle)),
            Visibility::default(),
            SniperAimLine {
                sniper: entity,
                target_world: target_pos,
                remaining: SNIPER_AIM_TIME,
            },
            RenderLayers::layer(PLAY_LAYER),
        )).with_children(|p| {
            // Outline strip (back, slightly under the main beam).
            p.spawn((
                Mesh2d(em.beam.clone()),
                MeshMaterial2d(pm.sniper_aim_outline.clone()),
                Transform::from_xyz(0.0, 0.0, -0.01)
                    .with_scale(Vec3::new(OUTLINE_WIDTH, length_scale, 1.0)),
                RenderLayers::layer(PLAY_LAYER),
            ));
            // Inner beam (front).
            p.spawn((
                Mesh2d(em.beam.clone()),
                MeshMaterial2d(pm.sniper_aim.clone()),
                Transform::from_xyz(0.0, 0.0, 0.01)
                    .with_scale(Vec3::new(INNER_WIDTH, length_scale, 1.0)),
                RenderLayers::layer(PLAY_LAYER),
            ));
        }).id();
        commands.entity(entity).insert(SniperAim {
            remaining: SNIPER_AIM_TIME,
            target_world: target_pos,
            line,
        });
    }
}

// Suppress unused-import warning — kept on purpose so anyone touching
// this file finds the standard enemy bullet speed.
#[allow(dead_code)]
const _: f32 = BULLET_SPEED;

/// Per-frame: rotate every Sniper's `SniperTurret` child base so the
/// barrel points at the locked target (during aim) or the live nearest
/// target (idle). Local rotation = world-aim − body-heading.
pub fn sniper_turret_aim(
    snipers: Query<(&Transform, &Heading, &Enemy, Option<&SniperAim>, &Children)>,
    friendly: Query<&Transform, (With<Friendly>, Without<Enemy>, Without<Ally>)>,
    ally_cache: Res<crate::ally::AllyPositionsCache>,
    mut turrets: Query<
        &mut Transform,
        (With<SniperTurret>, Without<Enemy>, Without<Friendly>, Without<Ally>),
    >,
) {
    let friendly_positions: Vec<Vec2> = friendly
        .iter()
        .map(|t| t.translation.truncate())
        .collect();
    if friendly_positions.is_empty() { return; }
    let ally_positions = &ally_cache.positions;

    for (tf, heading, enemy, aim, children) in &snipers {
        if enemy.variant != EnemyVariant::Sniper { continue; }
        let pos = tf.translation.truncate();
        let target = aim
            .map(|a| a.target_world)
            .unwrap_or_else(|| nearest_target(pos, &friendly_positions, ally_positions));
        let to = target - pos;
        if to.length_squared() < 1.0 { continue; }
        let world_aim = (-to.x).atan2(to.y);
        let local = world_aim - heading.0;
        let want = Quat::from_rotation_z(local);
        for c in children.iter() {
            if let Ok(mut t_tf) = turrets.get_mut(c) {
                if t_tf.rotation != want { t_tf.rotation = want; }
            }
        }
    }
}

/// Sync each aim-line entity to its source sniper each frame, despawn
/// orphans, and pulse-tick the remaining duration.
pub fn sniper_aim_line_tick(
    time: Res<Time>,
    mut commands: Commands,
    snipers: Query<(&Transform, Option<&SniperAim>), With<Enemy>>,
    mut lines: Query<(Entity, &mut Transform, &mut SniperAimLine), Without<Enemy>>,
) {
    let dt = time.delta_secs();
    for (line_entity, mut tf, mut line) in &mut lines {
        let Ok((sniper_tf, aim)) = snipers.get(line.sniper) else {
            commands.entity(line_entity).despawn();
            continue;
        };
        let Some(aim) = aim else {
            commands.entity(line_entity).despawn();
            continue;
        };
        line.remaining = (line.remaining - dt).max(0.0);
        let _ = aim;

        let pos = sniper_tf.translation.truncate();
        let to = line.target_world - pos;
        // Render the aim-line at 3× the sniper-to-target distance,
        // extending past the target. The intent is to telegraph the
        // shot direction, not just the impact point, so a player
        // who's about to walk into the line sees it sooner.
        const AIM_LINE_LENGTH_MULT: f32 = 3.0;
        let length = to.length().max(1.0) * AIM_LINE_LENGTH_MULT;
        // Mid-point of the elongated line — sniper is the start,
        // the far end sits `AIM_LINE_LENGTH_MULT × distance` along
        // the direction-to-target.
        let mid = pos + to * (AIM_LINE_LENGTH_MULT * 0.5);
        let angle = (-(to.x)).atan2(to.y);
        tf.translation.x = mid.x;
        tf.translation.y = mid.y;
        tf.rotation = Quat::from_rotation_z(angle);
        tf.scale = Vec3::new(1.4, length / crate::balance::BEAM_LENGTH, 1.0);
    }
}
