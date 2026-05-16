//! Homing missile launcher — fires forward from its host hull, then
//! steers each missile toward the nearest faction-matching target every
//! frame. Faction-agnostic so the same launcher works on a submarine
//! (friendly) or a boss-side ship.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;

use crate::balance::PLAY_LAYER;
use crate::bullet::Bullet;
use crate::components::{Faction, FactionKind, Heading, Velocity};
use crate::effects::EffectMeshes;
use crate::palette::PaletteMaterials;
use crate::ship::approach_angle;
use crate::weapon::WeaponType;

/// A homing-missile launcher mounted on a ship. Fires forward at
/// `fire_rate` Hz when a valid target exists.
#[derive(Component)]
pub struct MissileLauncher {
    pub fire_rate: f32,
    pub damage: i32,
    /// Counts down to 0; reset on each fire.
    pub cd: f32,
    /// Hull-local +Y offset where the missile spawns.
    pub muzzle_offset: f32,
    pub target_faction: FactionKind,
    /// Damage-credit source baked at spawn so `bullet_collisions` can
    /// attribute kills correctly. `None` for enemy launchers.
    pub source: Option<crate::bullet::DamageSource>,
}

/// Tag on an in-flight homing missile. The missile is otherwise a
/// regular `Bullet` (faction = opposite of target) routed through the
/// standard collision pipeline; this component just re-aims each frame.
#[derive(Component)]
pub struct HomingMissile {
    pub target: Option<Entity>,
    /// Max angular adjustment per second (rad/s). Smaller = wider
    /// turning circle, more dodgeable.
    pub turn_rate: f32,
    /// Cached at spawn — the missile outlives its launcher so we can't
    /// re-read the launcher's faction mid-flight.
    pub target_faction: FactionKind,
    /// Seconds before the homing tracker engages. Until this hits 0
    /// the missile flies in a straight line on its initial velocity.
    /// Set > 0 on player-fired salvos so a missile launched at point
    /// A doesn't instantly snap toward target B mid-flight — the
    /// volley reads as "fire then track" instead of "homing the
    /// moment it leaves the rack".
    pub homing_delay: f32,
}

/// Slower than a cannonball (`BULLET_SPEED = 110`) so the homing curve
/// is visible and dodgeable.
const MISSILE_SPEED: f32 = 60.0;
/// Generous airborne range so a missile that loses its target mid-flight
/// can still re-home onto a fresh one.
const MISSILE_RANGE: f32 = 300.0;
/// Max angular adjustment per second. Small enough that fast Scouts can
/// break lock by juking at the right moment.
pub const MISSILE_TURN_RATE: f32 = 3.0;

fn spawn_homing_missile(
    commands: &mut Commands,
    em: &EffectMeshes,
    pm: &PaletteMaterials,
    pos: Vec2,
    forward: Vec2,
    damage: i32,
    initial_target: Option<Entity>,
    target_faction: FactionKind,
    source: Option<crate::bullet::DamageSource>,
) {
    spawn_homing_missile_full(
        commands, em, pm, pos, forward, damage, initial_target,
        target_faction, source, WeaponType::Standard,
        // Ally missile launcher: no homing delay — its targeting
        // pick happens at fire time and is already aimed.
        Vec::new(), MISSILE_RANGE, 1.0, 0.0,
    );
}

/// Generalised homing-missile spawner. Used by both the ally
/// `MissileLauncher` (which fires WeaponType::Standard with no runes)
/// and the player's `SpreadRockets` turret (which threads its slot's
/// weapon type + runes + rune-effect through so Pierce and friends
/// behave consistently with normal bullets).
pub fn spawn_homing_missile_full(
    commands: &mut Commands,
    em: &EffectMeshes,
    pm: &PaletteMaterials,
    pos: Vec2,
    forward: Vec2,
    damage: i32,
    initial_target: Option<Entity>,
    target_faction: FactionKind,
    source: Option<crate::bullet::DamageSource>,
    weapon: WeaponType,
    runes: Vec<crate::rune::Rune>,
    range: f32,
    rune_effect: f32,
    homing_delay: f32,
) {
    let heading_rot = (-forward.x).atan2(forward.y);
    // Inspect for Pierce on the slice before the Vec moves into the
    // bundle — same data, no clone.
    let pierce_stacks = crate::bullet::pierce_stacks(&runes);
    let bullet = commands.spawn((
        Mesh2d(em.bullet_missile_outer.clone()),
        MeshMaterial2d(pm.bullet_missile_outer.clone()),
        Transform::from_xyz(pos.x, pos.y, 4.0)
            .with_rotation(Quat::from_rotation_z(heading_rot)),
        Bullet {
            faction: target_faction.opposite(),
            damage,
            remaining: range,
            weapon,
            source,
            runes,
        },
        Velocity(forward * MISSILE_SPEED),
        HomingMissile {
            target: initial_target,
            turn_rate: MISSILE_TURN_RATE,
            target_faction,
            homing_delay,
        },
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    if target_faction == FactionKind::Enemy {
        if let Some(stacks) = pierce_stacks {
            commands.entity(bullet).insert(crate::bullet::make_pierce(stacks, rune_effect));
        }
    }
    let inner = commands.spawn((
        Mesh2d(em.bullet_missile_inner.clone()),
        MeshMaterial2d(pm.bullet_missile_inner.clone()),
        Transform::from_xyz(0.0, 0.0, 0.05),
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    commands.entity(inner).insert(ChildOf(bullet));
}

/// Tick every `MissileLauncher`'s cooldown and fire when due. Skipped
/// if no valid target exists; the cooldown still ticks so as soon as
/// one appears the launcher fires immediately.
pub fn missile_launcher_fire(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    candidates: Query<(Entity, &Transform, &Faction)>,
    mut launchers: Query<
        (&Transform, &Heading, &mut MissileLauncher),
        Without<crate::harpoon::Harpooned>,
    >,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();

    for (tf, heading, mut launcher) in &mut launchers {
        launcher.cd -= dt;

        let target_snap: Vec<(Entity, Vec2)> = candidates
            .iter()
            .filter(|(_, _, f)| f.0 == launcher.target_faction)
            .map(|(e, t, _)| (e, t.translation.truncate()))
            .collect();

        if target_snap.is_empty() { continue; }
        if launcher.cd > 0.0 { continue; }
        launcher.cd = 1.0 / launcher.fire_rate.max(0.001);

        let pos = tf.translation.truncate();
        let h = heading.0;
        let forward = Vec2::new(-h.sin(), h.cos());
        let muzzle = pos + forward * launcher.muzzle_offset;

        let target = target_snap
            .iter()
            .min_by(|a, b| {
                let da = a.1.distance_squared(muzzle);
                let db = b.1.distance_squared(muzzle);
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(e, _)| *e);

        spawn_homing_missile(
            &mut commands, &em, &pm,
            muzzle, forward, launcher.damage, target,
            launcher.target_faction, launcher.source,
        );
    }
}

/// Per-frame steering: re-acquire if the cached target is gone, then
/// rotate `Velocity` toward the target by at most `turn_rate * dt`.
/// Runs before `apply_velocity` so the new direction drives this frame's
/// integration.
pub fn homing_missile_track(
    time: Res<Time>,
    candidates: Query<(Entity, &Transform, &Faction), Without<HomingMissile>>,
    mut missiles: Query<(&mut Transform, &mut Velocity, &mut HomingMissile)>,
) {
    let dt = time.delta_secs();

    for (mut tf, mut vel, mut m) in &mut missiles {
        // Homing delay — fly straight on the spawn velocity for the
        // first `homing_delay` seconds. Lets a player salvo travel
        // visibly before snapping toward a target instead of
        // sharply bending at the muzzle.
        if m.homing_delay > 0.0 {
            m.homing_delay -= dt;
            continue;
        }
        let pos = tf.translation.truncate();
        let target_faction = m.target_faction;

        let cached_pos = m.target.and_then(|t| {
            candidates.iter().find_map(|(e, tf, f)| {
                if e == t && f.0 == target_faction {
                    Some(tf.translation.truncate())
                } else {
                    None
                }
            })
        });

        let target_pos = cached_pos.or_else(|| {
            let nearest = candidates
                .iter()
                .filter(|(_, _, f)| f.0 == target_faction)
                .min_by(|a, b| {
                    let da = a.1.translation.truncate().distance_squared(pos);
                    let db = b.1.translation.truncate().distance_squared(pos);
                    da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                });
            if let Some((e, t, _)) = nearest {
                m.target = Some(e);
                Some(t.translation.truncate())
            } else {
                m.target = None;
                None
            }
        });

        if let Some(tp) = target_pos {
            let to = tp - pos;
            let speed = vel.0.length().max(1.0);
            if to.length_squared() > 0.5 {
                let cur_angle = (-vel.0.x).atan2(vel.0.y);
                let desired_angle = (-to.x).atan2(to.y);
                let new_angle = approach_angle(cur_angle, desired_angle, m.turn_rate * dt);
                let new_dir = Vec2::new(-new_angle.sin(), new_angle.cos());
                vel.0 = new_dir * speed;
                tf.rotation = Quat::from_rotation_z(new_angle);
            }
        }
    }
}
