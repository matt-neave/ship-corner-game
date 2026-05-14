//! Pirate `Cannon` weapon — slow-firing heavy cannonball that knocks
//! enemies back on hit.
//!
//! Implementation surface:
//!
//! - `Knockback` is a tag component carried by cannonball bullets.
//!   `bullet::bullet_collisions` looks for it on the bullet at hit
//!   time and inserts a `components::Knockedback` impulse on the
//!   struck enemy.
//! - `spawn_cannonball` mirrors `turret::spawn_combat_bullet` but uses
//!   a chunkier transform scale so the projectile reads as a heavy
//!   iron cannonball rather than a small bullet, and tags the entity
//!   with `Knockback` so the collision system knows to push on hit.
//!
//! The actual movement push lives in `ship::apply_velocity`, which
//! composes `Velocity` (frost-slowed) with `Knockedback` (raw, decays)
//! every frame. Mutating the enemy's `Velocity` from the hit site
//! doesn't work — enemy AI re-clamps `Velocity` each frame, so the
//! impulse must live on its own component.
//!
//! The plugin itself has no systems — the firing path is dispatched
//! from `turret_aim_fire`'s match on `WeaponType::Cannon`, and the
//! `Knockedback` lifecycle (apply + decay + cleanup) is handled by
//! `apply_velocity`. The plugin still exists as the conventional
//! per-weapon registration point for future cannon-specific systems
//! (e.g. impact dust FX).

use bevy::prelude::*;
use bevy::render::view::RenderLayers;

use crate::balance::{BULLET_SPEED, PLAY_LAYER};
use crate::bullet::{Bullet, DamageSource};
use crate::components::{FactionKind, Velocity};
use crate::effects::EffectMeshes;
use crate::rune::Rune;
use crate::weapon::WeaponType;

/// Visual scale applied to the standard friendly bullet meshes when
/// rendered as a cannonball. Reads as a chunky iron sphere without
/// being so large it visually overlaps the muzzle.
const CANNONBALL_SCALE: f32 = 1.2;

/// Initial outward velocity (world units / sec) added to an enemy
/// struck by a cannonball. Lives on a `components::Knockedback`
/// component on the target and decays per `CANNONBALL_KNOCKBACK_DECAY`
/// each second; total displacement before decay-out is roughly
/// `force / decay` ≈ 19 units (about 2 enemy widths).
pub const CANNONBALL_KNOCKBACK_FORCE: f32 = 75.0;

/// Decay rate (per second) applied to the cannonball's knockback
/// impulse each frame. With force=75 and decay=4, half-life is ~0.17s
/// and total displacement is ~19 units — a snappy, weighty shove that
/// doesn't fling targets across the arena. Tuned for "iron impact"
/// vs "gust of wind".
pub const CANNONBALL_KNOCKBACK_DECAY: f32 = 4.0;

/// Marker on a bullet entity that should impart a velocity impulse on
/// the enemy it hits. `force` is the magnitude added along the
/// bullet's normalised travel direction.
#[derive(Component, Clone, Copy)]
pub struct Knockback {
    pub force: f32,
}

pub struct CannonPlugin;

impl Plugin for CannonPlugin {
    fn build(&self, _app: &mut App) {
        // No systems — the firing path lives in `turret_aim_fire` and
        // the on-hit knockback impulse is applied inline inside
        // `bullet::bullet_collisions`.
    }
}

/// Spawn a cannonball bullet. Mirrors `turret::spawn_combat_bullet`
/// (same outer + inner two-tone build, same `Bullet`/`Velocity`
/// components, same render layer + Z) but renders the meshes at
/// `CANNONBALL_SCALE` so it reads as a heavy iron sphere, and tags
/// the entity with `Knockback` so the collision system pushes the
/// target on hit.
pub fn spawn_cannonball(
    commands: &mut Commands,
    em: &EffectMeshes,
    outer_mat: &Handle<ColorMaterial>,
    inner_mat: &Handle<ColorMaterial>,
    pos: Vec2,
    dir: Vec2,
    weapon: WeaponType,
    damage: i32,
    source: Option<DamageSource>,
    range: f32,
    runes: Vec<Rune>,
    faction: FactionKind,
) {
    let bullet = commands.spawn((
        Mesh2d(em.bullet_round_outer.clone()),
        MeshMaterial2d(outer_mat.clone()),
        Transform {
            translation: Vec3::new(pos.x, pos.y, 4.0),
            rotation: Quat::from_rotation_z((-dir.x).atan2(dir.y)),
            scale: Vec3::new(CANNONBALL_SCALE, CANNONBALL_SCALE, 1.0),
        },
        Bullet {
            faction,
            damage,
            remaining: range,
            weapon,
            source,
            runes,
        },
        Velocity(dir * BULLET_SPEED),
        Knockback { force: CANNONBALL_KNOCKBACK_FORCE },
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    let inner = commands.spawn((
        Mesh2d(em.bullet_round_inner.clone()),
        MeshMaterial2d(inner_mat.clone()),
        // Inner z-offset is in the parent's local space; the parent's
        // scale already makes it bigger, so no extra scale needed here.
        Transform::from_xyz(0.0, 0.0, 0.05),
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    commands.entity(inner).insert(ChildOf(bullet));
}
