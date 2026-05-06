//! Small generic ECS components shared across many domains. Anything tied
//! to a single domain (turrets, bullets, enemies, UI) lives in its own
//! module — this file is for primitives every system might pull in.

use bevy::prelude::*;

/// Marker for the friendly ship entity. Exactly one exists.
#[derive(Component)]
pub struct Friendly;

/// Hit points. Decremented in collision/detonation systems; the entity is
/// despawned (or hidden, in the case of the friendly in Wave mode) when it
/// reaches zero.
#[derive(Component)]
pub struct Health(pub i32);

/// Per-frame velocity in world units / second. `apply_velocity` integrates.
#[derive(Component)]
pub struct Velocity(pub Vec2);

/// Heading angle in radians, with 0 = +Y (forward up). Direction vector is
/// `Vec2::new(-sin(h), cos(h))` — keep this convention everywhere.
#[derive(Component)]
pub struct Heading(pub f32);

/// Side allegiance. Drives bullet target selection + collision routing.
#[derive(Component)]
pub struct Faction(pub FactionKind);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FactionKind {
    Friendly,
    Enemy,
}
