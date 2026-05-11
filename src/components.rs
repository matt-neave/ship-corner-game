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
/// Set by AI / control systems each frame; `apply_velocity` reads it
/// (modulated by `OnFrost` slow if present) and translates the entity.
#[derive(Component)]
pub struct Velocity(pub Vec2);

/// Movement-impulse layer applied ON TOP of `Velocity` in `apply_velocity`.
/// Lets effects like cannon knockback shove an entity even though its
/// AI overwrites `Velocity` every frame — `Velocity` is the AI's intent,
/// `Knockedback` is the world's reaction to a hit.
///
/// Composition rules in `apply_velocity`:
/// - Natural movement: `Velocity * frost_mult` (frost slows the AI's intent)
/// - Knockback: `Knockedback.velocity` (NOT slowed by frost — an impulse
///   should still hit you when you're frozen, otherwise frost trivialises
///   crowd-control)
/// - Both contribute to position each frame.
///
/// `velocity` is decayed multiplicatively by `decay_per_sec` per second
/// each frame; the component is removed once the magnitude falls below
/// a small threshold so it doesn't linger as a no-op forever.
#[derive(Component)]
pub struct Knockedback {
    pub velocity: Vec2,
    /// Exponential decay rate. With `d = decay_per_sec`, the magnitude
    /// loses `d * dt` per frame; total displacement before despawn is
    /// roughly `velocity / d`. e.g. `velocity=75, d=4` → ~19 units.
    pub decay_per_sec: f32,
}

/// Heading angle in radians, with 0 = +Y (forward up). Direction vector is
/// `Vec2::new(-sin(h), cos(h))` — keep this convention everywhere.
#[derive(Component)]
pub struct Heading(pub f32);

/// Short-lived "frozen in place" status applied to enemies hit by
/// Future-tagged weapons (when the Future synergy is active). While
/// `remaining > 0`, the enemy's movement AI and firing routines
/// early-return — they hold position and can't shoot. Ticked down
/// in `tick_stunned`; entity loses the component the first frame
/// it would go non-positive.
///
/// Stun duration is set by `Synergies::future_stun_duration` (0.1s
/// per tier). Re-hitting a stunned enemy refreshes the timer to the
/// MAX of the existing remaining and the new duration — multiple
/// rapid hits never shorten the effect.
#[derive(Component)]
pub struct Stunned {
    pub remaining: f32,
}

/// Side allegiance. Drives bullet target selection + collision routing.
#[derive(Component)]
pub struct Faction(pub FactionKind);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FactionKind {
    Friendly,
    Enemy,
}

impl FactionKind {
    /// The other side. Used by faction-parameterized weapons / units to
    /// derive "what should my projectiles' faction be?" from "what
    /// faction do I target?", or vice versa.
    pub fn opposite(self) -> FactionKind {
        match self {
            FactionKind::Friendly => FactionKind::Enemy,
            FactionKind::Enemy    => FactionKind::Friendly,
        }
    }
}
