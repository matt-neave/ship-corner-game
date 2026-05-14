//! Harpoon weapon — long-range melee. Fires a single spear with a
//! `HarpoonTip` tag; on hit the bullet pipeline applies 1 damage and
//! attaches a `Harpooned` tether to the target. The tether overrides
//! the enemy's AI velocity each frame, dragging them toward the ship
//! along a visible `HarpoonChain` beam until they make contact —
//! `friendly_ram_damage` finishes the job.
//!
//! Implementation surface:
//!
//! * `HarpoonTip` is a marker on the in-flight bullet — read by
//!   `bullet::bullet_collisions` to fork into harpoon-on-hit logic
//!   instead of the regular damage queue path (same pattern as
//!   `cannon::Knockback`).
//! * `Harpooned` is the tether component on the target. The pull
//!   tick runs between `enemy_ai` and `apply_velocity` so it gets the
//!   final say on the enemy's per-frame velocity.
//! * `HarpoonChain` is the chain visual entity. `update_harpoon_chains`
//!   re-anchors it between source ship and impaled enemy each frame
//!   and despawns it when either endpoint is gone or the tether
//!   expires.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;

use crate::balance::{BEAM_LENGTH, BULLET_SPEED, PLAY_LAYER};
use crate::bullet::{Bullet, DamageSource};
use crate::components::{FactionKind, Velocity};
use crate::effects::EffectMeshes;
use crate::palette::PaletteMaterials;
use crate::rune::Rune;
use crate::weapon::WeaponType;

/// Visual scale on the spear projectile — slimmer than a cannonball,
/// stretched a touch on the +Y axis so the silhouette reads as a
/// thrown spear rather than a bullet.
const HARPOON_SCALE_X: f32 = 0.6;
const HARPOON_SCALE_Y: f32 = 1.8;

/// Projectile speed multiplier vs the standard `BULLET_SPEED`. A
/// spear should feel snappier than a regular bullet — long range +
/// faster travel makes the "reach out and grab" fantasy land.
const HARPOON_SPEED_MULT: f32 = 1.5;

/// World units / sec the impaled enemy is dragged toward the ship.
/// Faster than the toughest enemy's natural speed so the pull always
/// wins. Tuned so a 60-unit harpoon shot reels in over ~1.2 s.
pub const HARPOON_PULL_SPEED: f32 = 50.0;

/// Maximum tether time before the harpoon dissolves on its own. Stops
/// a tether from outliving the engagement if the ship sails away.
const HARPOON_TETHER_LIFETIME: f32 = 4.0;

/// Bosses break free much faster — 1 second of grace before they shrug
/// the chain off. Long enough to interrupt one of their attacks; short
/// enough that the harpoon can't permanently lock down a boss fight.
const HARPOON_TETHER_LIFETIME_BOSS: f32 = 1.0;

/// Distance at which the harpoon considers the target "landed" and
/// removes the tether so `friendly_ram_damage` and any normal AI take
/// over. Big enough that the target stops mid-deck rather than
/// embedding into the centre of the hull.
const HARPOON_LANDED_DIST: f32 = 6.0;

/// Marker on a harpoon spear projectile. `bullet_collisions` reads
/// this to fork into the on-hit tether attach path.
#[derive(Component, Clone, Copy)]
pub struct HarpoonTip;

/// Tether component on an impaled enemy. The pull tick reads this each
/// frame; `update_harpoon_chains` reads it to find which target the
/// chain visual is anchored to.
#[derive(Component)]
pub struct Harpooned {
    /// The ship dragging this enemy in (the player's `Friendly`).
    pub source: Entity,
    /// Constant pull speed in world units / sec.
    pub pull_speed: f32,
    /// Seconds remaining before the tether auto-releases. Counts down
    /// each frame; on hitting 0 (or after a close-enough proximity
    /// check) the `Harpooned` component is removed.
    pub remaining: f32,
}

/// Chain visual entity. Re-anchored each frame between `source` and
/// `target`. Despawns when either entity is gone.
#[derive(Component)]
pub struct HarpoonChain {
    pub source: Entity,
    pub target: Entity,
}

pub struct HarpoonPlugin;

impl Plugin for HarpoonPlugin {
    fn build(&self, app: &mut App) {
        // `tick_harpoons` must have the final word on a harpooned
        // enemy's per-frame velocity. Both `enemy_ai` and
        // `tick_harpoons` write `Velocity`; without explicit
        // ordering Bevy can run them in either order, and if
        // `enemy_ai` runs LAST it overwrites the pull velocity and
        // the harpoon effectively does nothing. Pin tick_harpoons
        // after the AI systems and before `apply_velocity` (which
        // integrates Velocity into Transform) so the pull always
        // wins. The chain visual sync runs alongside other per-frame
        // visual updates and self-gates on the presence of a chain.
        app.add_systems(
            Update,
            (
                tick_harpoons
                    .after(crate::enemy::enemy_ai)
                    .after(crate::ally::ally_ai)
                    .before(crate::ship::apply_velocity),
                update_harpoon_chains,
            ),
        );
    }
}

/// Spawn a harpoon spear at `pos` headed `dir`. Mirrors
/// `turret::spawn_combat_bullet` so it routes through the standard
/// bullet/collision pipeline, plus a `HarpoonTip` marker that flags
/// the on-hit behaviour fork.
pub fn spawn_harpoon_spear(
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
        Mesh2d(em.bullet_friendly_outer.clone()),
        MeshMaterial2d(outer_mat.clone()),
        Transform {
            translation: Vec3::new(pos.x, pos.y, 4.0),
            rotation: Quat::from_rotation_z((-dir.x).atan2(dir.y)),
            scale: Vec3::new(HARPOON_SCALE_X, HARPOON_SCALE_Y, 1.0),
        },
        Bullet {
            faction,
            damage,
            remaining: range,
            weapon,
            source,
            runes,
        },
        Velocity(dir * BULLET_SPEED * HARPOON_SPEED_MULT),
        HarpoonTip,
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    let inner = commands.spawn((
        Mesh2d(em.bullet_friendly_inner.clone()),
        MeshMaterial2d(inner_mat.clone()),
        Transform::from_xyz(0.0, 0.0, 0.05),
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    commands.entity(inner).insert(ChildOf(bullet));
}

/// Attach a tether to `target` and spawn the chain visual. Called from
/// `bullet_collisions` when a `HarpoonTip` bullet lands. Idempotent —
/// re-harpooning a target replaces the existing component (any old
/// chain self-cleans when its target is no longer harpooned... or
/// would, if we tracked it; for now a duplicate chain just lingers
/// briefly and despawns on lifetime end, which is acceptable).
pub fn attach_harpoon(
    commands: &mut Commands,
    em: &EffectMeshes,
    pm: &PaletteMaterials,
    source: Entity,
    target: Entity,
    is_boss: bool,
) {
    let lifetime = if is_boss {
        HARPOON_TETHER_LIFETIME_BOSS
    } else {
        HARPOON_TETHER_LIFETIME
    };
    commands.entity(target).insert(Harpooned {
        source,
        pull_speed: HARPOON_PULL_SPEED,
        remaining: lifetime,
    });
    commands.spawn((
        Mesh2d(em.beam.clone()),
        MeshMaterial2d(pm.harpoon_chain.clone()),
        // Z = 4.4 — above bullets/beams (4.0) but below muzzle flashes
        // (5+) so the chain reads as a foreground object between
        // ship and target without occluding combat FX.
        Transform::from_xyz(0.0, 0.0, 4.4),
        HarpoonChain { source, target },
        RenderLayers::layer(PLAY_LAYER),
    ));
}

/// Per-frame tether tick. Overrides each harpooned enemy's velocity
/// to a constant pull toward `source`. Removes the tether when the
/// lifetime runs out, when the target reaches the ship, or when the
/// source ship is gone.
pub fn tick_harpoons(
    time: Res<Time>,
    mut commands: Commands,
    sources: Query<&Transform, Without<Harpooned>>,
    mut harpooned: Query<(Entity, &Transform, &mut Velocity, &mut Harpooned)>,
) {
    let dt = time.delta_secs();
    for (entity, tf, mut vel, mut h) in &mut harpooned {
        // Source despawned (player died, restart, etc.) → release.
        let Ok(src_tf) = sources.get(h.source) else {
            commands.entity(entity).remove::<Harpooned>();
            continue;
        };
        let to = src_tf.translation.truncate() - tf.translation.truncate();
        let dist = to.length();
        if dist < HARPOON_LANDED_DIST {
            // Close enough — friendly_ram_damage takes over from here.
            commands.entity(entity).remove::<Harpooned>();
            continue;
        }
        let dir = if dist > 0.001 { to / dist } else { Vec2::Y };
        // Hard override: AI's per-frame Velocity is replaced rather
        // than added to, so kiting variants (Sniper, Artillery) can't
        // fight the pull.
        vel.0 = dir * h.pull_speed;
        h.remaining -= dt;
        if h.remaining <= 0.0 {
            commands.entity(entity).remove::<Harpooned>();
        }
    }
}

/// Re-anchor each chain visual between its source and target each
/// frame, scaled to span the gap. Despawns when either endpoint is
/// gone OR the target no longer carries `Harpooned` (so the chain
/// vanishes the instant the tether releases).
pub fn update_harpoon_chains(
    mut commands: Commands,
    transforms: Query<&Transform, Without<HarpoonChain>>,
    harpooned: Query<&Harpooned>,
    mut chains: Query<(Entity, &mut Transform, &HarpoonChain)>,
) {
    for (chain_e, mut tf, chain) in &mut chains {
        let Ok(src_tf) = transforms.get(chain.source) else {
            commands.entity(chain_e).despawn();
            continue;
        };
        let Ok(tgt_tf) = transforms.get(chain.target) else {
            commands.entity(chain_e).despawn();
            continue;
        };
        if harpooned.get(chain.target).is_err() {
            // Tether released (landed, expired, or source gone).
            commands.entity(chain_e).despawn();
            continue;
        }
        let a = src_tf.translation.truncate();
        let b = tgt_tf.translation.truncate();
        let delta = b - a;
        let len = delta.length();
        if len < 0.5 { continue; }
        let mid = (a + b) * 0.5;
        let angle = (-delta.x).atan2(delta.y);
        tf.translation.x = mid.x;
        tf.translation.y = mid.y;
        tf.rotation = Quat::from_rotation_z(angle);
        // Beam mesh is `Rectangle::new(1.0, BEAM_LENGTH)` (long axis
        // +Y); scale y to the gap, x to a chunky chain thickness.
        tf.scale = Vec3::new(1.2, len / BEAM_LENGTH, 1.0);
    }
}
