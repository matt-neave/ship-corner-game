//! Ammo-overlay status effects ("runes") that any gun can carry.
//!
//! Adding a new rune type:
//! 1. Add a variant to `Rune`.
//! 2. Add rows in `label`, `proc_coefficient`, `cycle_next`, `cycle_prev`.
//! 3. Add the on-hit branch in `apply_rune` (insert a status component) OR
//!    add a chain-style branch in `bullet::process_damage_event` (instant
//!    effect that spawns a follow-up damage event — mirror Shock).
//! 4. Add the per-tick driver system if the new effect needs one (mirror
//!    `tick_on_fire` / `tick_on_frost`).
//! 5. Add a translation key in `data/translations.csv` and a particle
//!    material in `palette::PaletteMaterials`.
//!
//! Runes propagate through the firing path:
//!   `SlotCfg.rune` → `TurretSlot.rune` (via `sync_turret_config`)
//!     → `Bullet.rune` (set by `spawn_friendly_bullet`)
//!     → on-hit proc evaluation (`bullet::bullet_collisions` queues a
//!       `DamageEvent`; `process_damage_event` resolves the rune).
//!
//! `FireExtent` lives on every entity that should be able to burn / frost
//! / etc. — particle systems read it to spread their FX across the body.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;
use rand::Rng;

use crate::balance::{
    FIRE_DAMAGE_PER_TICK, FIRE_DAMAGE_TICK_INTERVAL, FIRE_DURATION, FIRE_PARTICLES_PER_TICK,
    FIRE_PARTICLE_TICK_INTERVAL, FROST_DURATION, FROST_PARTICLES_PER_TICK,
    FROST_PARTICLE_TICK_INTERVAL, PLAY_LAYER,
};
use crate::bullet::apply_damage;
use crate::components::{Friendly, Health, Velocity};
use crate::effects::{EffectMeshes, HitFx, HitParticle};
use crate::i18n::tr;
use crate::modes::GameMode;
use crate::palette::PaletteMaterials;

// ---------- Rune kind ----------

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Rune {
    Fire,
    Frost,
    Shock,
}

impl Rune {
    pub fn label(self) -> &'static str {
        match self {
            Rune::Fire  => tr("rune_fire"),
            Rune::Frost => tr("rune_frost"),
            Rune::Shock => tr("rune_shock"),
        }
    }

    /// Risk-of-Rain-style proc coefficient: how strongly THIS rune's secondary
    /// damage events can trigger further runes. Multiplied into the rolling
    /// proc strength on each hop.
    ///
    /// - `1.0` = fully proc-capable (default for primary bullet hits).
    /// - `0.5` = halved chance for downstream procs (Shock chain).
    /// - `0.0` = inert; secondary damage from this rune cannot proc anything
    ///   (Fire DoT ticks, Frost — both should not cascade).
    pub fn proc_coefficient(self) -> f32 {
        match self {
            Rune::Fire  => 0.0,
            Rune::Frost => 0.0,
            Rune::Shock => 0.5,
        }
    }
}

/// Cycle a slot's rune forward: `None → Fire → Frost → Shock → None → …`.
pub fn cycle_next(current: Option<Rune>) -> Option<Rune> {
    match current {
        None              => Some(Rune::Fire),
        Some(Rune::Fire)  => Some(Rune::Frost),
        Some(Rune::Frost) => Some(Rune::Shock),
        Some(Rune::Shock) => None,
    }
}

/// Cycle backward — reverse of `cycle_next`.
pub fn cycle_prev(current: Option<Rune>) -> Option<Rune> {
    match current {
        None              => Some(Rune::Shock),
        Some(Rune::Shock) => Some(Rune::Frost),
        Some(Rune::Frost) => Some(Rune::Fire),
        Some(Rune::Fire)  => None,
    }
}

/// Display string for a rune slot — "NONE" or the rune's own label.
pub fn rune_display(rune: Option<Rune>) -> &'static str {
    match rune {
        None    => tr("rune_none"),
        Some(r) => r.label(),
    }
}

// ---------- On-hit application ----------

/// Insert / refresh the *status*-style status component matching `rune` on
/// `entity`. Bevy's `insert` overwrites, so re-applying a status just
/// refreshes its duration. Does nothing for instant-effect runes (Shock) —
/// those are handled inline by the bullet damage processor.
pub fn apply_rune(commands: &mut Commands, entity: Entity, rune: Rune) {
    match rune {
        Rune::Fire  => { commands.entity(entity).insert(OnFire::new()); }
        Rune::Frost => { commands.entity(entity).insert(OnFrost::new()); }
        Rune::Shock => { /* no status — chain damage emitted by proc system */ }
    }
}

// ---------- Fire status ----------

#[derive(Component)]
pub struct OnFire {
    pub remaining: f32,
    pub damage_tick: f32,
    pub particle_tick: f32,
}

impl OnFire {
    pub fn new() -> Self {
        Self {
            remaining: FIRE_DURATION,
            damage_tick: 0.0,
            particle_tick: 0.0,
        }
    }
}

/// Half-extent of an entity's body, in **local** units (assuming forward =
/// +Y). Particle systems (`tick_on_fire`, `tick_on_frost`) read this to
/// spread their FX across the silhouette. Added to every entity that
/// should be able to burn.
#[derive(Component, Clone, Copy)]
pub struct FireExtent(pub Vec2);

/// Particle spawn-box inset. The full half-extent describes a rectangle,
/// but every body in the game is a capsule — its rounded corners are
/// outside that rectangle. Sampling at 80% of the half-extent keeps
/// particles comfortably inside the capsule silhouette.
const PARTICLE_BODY_INSET: f32 = 0.8;

/// Pick a random world-space point inside the rotated body silhouette,
/// shrunk by `PARTICLE_BODY_INSET` so corners don't poke past the capsule.
fn random_body_point(
    tf: &Transform,
    extent: &FireExtent,
    rng: &mut rand::rngs::ThreadRng,
) -> Vec2 {
    let local = Vec2::new(
        rng.gen_range(-extent.0.x..extent.0.x),
        rng.gen_range(-extent.0.y..extent.0.y),
    ) * PARTICLE_BODY_INSET;
    let world = tf.rotation.mul_vec3(local.extend(0.0)).truncate();
    tf.translation.truncate() + world
}

// ---------- Frost status ----------

/// Slows the entity's movement (see `apply_velocity` in `ship.rs` — it
/// scales velocity by `FROST_SPEED_MULT` while this is present). Counts
/// down via `tick_on_frost` and emits cool-blue mist particles.
#[derive(Component)]
pub struct OnFrost {
    pub remaining: f32,
    pub particle_tick: f32,
}

impl OnFrost {
    pub fn new() -> Self {
        Self {
            remaining: FROST_DURATION,
            particle_tick: 0.0,
        }
    }
}

// ---------- Per-frame fire driver ----------

/// Tick fire damage + spawn body-wide flame particles. Damage and particle
/// rates are independent timers so visuals stay continuous while damage
/// applies on the slower 0.5s cadence.
///
/// Damage routes through `bullet::apply_damage`, the shared damage entry-
/// point, so target HitFx flashes on each tick and any future damage
/// modifiers compound automatically.
///
/// Skips damage on a `Friendly` (player) entity in Sandbox mode — the player
/// is invincible there and fire shouldn't override that. Visual particles
/// still play so the on-fire state is visible.
pub fn tick_on_fire(
    time: Res<Time>,
    mut commands: Commands,
    em: Option<Res<EffectMeshes>>,
    pm: Option<Res<PaletteMaterials>>,
    game_mode: Res<GameMode>,
    mut q: Query<(
        Entity,
        &Transform,
        &FireExtent,
        &mut OnFire,
        &mut Health,
        &mut HitFx,
        Option<&Friendly>,
    )>,
) {
    let Some(em) = em else { return; };
    let Some(pm) = pm else { return; };
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();

    for (entity, tf, extent, mut fire, mut hp, mut fx, friendly) in &mut q {
        fire.remaining -= dt;
        if fire.remaining <= 0.0 {
            commands.entity(entity).remove::<OnFire>();
            continue;
        }

        // Damage tick — routes through the shared `apply_damage` helper so
        // the target flashes and any compounding modifiers (future) apply.
        // Player is invincible in Sandbox; skip the call entirely.
        fire.damage_tick -= dt;
        if fire.damage_tick <= 0.0 {
            fire.damage_tick = FIRE_DAMAGE_TICK_INTERVAL;
            let invincible_player =
                friendly.is_some() && !matches!(*game_mode, GameMode::Wave);
            if !invincible_player {
                apply_damage(&mut hp, &mut fx, FIRE_DAMAGE_PER_TICK);
            }
        }

        // Particle tick — flame motes scattered across the body, drifting up.
        fire.particle_tick -= dt;
        if fire.particle_tick > 0.0 { continue; }
        fire.particle_tick = FIRE_PARTICLE_TICK_INTERVAL;

        for _ in 0..FIRE_PARTICLES_PER_TICK {
            let pos = random_body_point(tf, extent, &mut rng);
            let vel = Vec2::new(rng.gen_range(-3.0..3.0), rng.gen_range(12.0..20.0));
            let life = rng.gen_range(0.25..0.45);
            let scale = rng.gen_range(0.6..1.0);
            commands.spawn((
                Mesh2d(em.particle.clone()),
                MeshMaterial2d(pm.fire.clone()),
                Transform {
                    translation: Vec3::new(pos.x, pos.y, 5.5),
                    scale: Vec3::new(scale, scale, 1.0),
                    ..default()
                },
                HitParticle { life, max_life: life, base_scale: scale },
                Velocity(vel),
                RenderLayers::layer(PLAY_LAYER),
            ));
        }
    }
}

// ---------- Per-frame frost driver ----------

/// Tick frost duration + spawn cool-blue mist particles. The slow itself
/// is applied in `ship::apply_velocity` (scales velocity while `OnFrost`
/// is present) — this system only handles duration + visuals.
///
/// Visual is intentionally distinct from fire:
/// - Cooler color (`pm.frost`).
/// - Sinking / settling motion (small downward drift, mostly horizontal
///   sway) instead of fire's strong upward plume.
/// - Smaller scale and longer life — looks like a quiet mist clinging to
///   the hull rather than a roaring flame.
pub fn tick_on_frost(
    time: Res<Time>,
    mut commands: Commands,
    em: Option<Res<EffectMeshes>>,
    pm: Option<Res<PaletteMaterials>>,
    mut q: Query<(Entity, &Transform, &FireExtent, &mut OnFrost)>,
) {
    let Some(em) = em else { return; };
    let Some(pm) = pm else { return; };
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();

    for (entity, tf, extent, mut frost) in &mut q {
        frost.remaining -= dt;
        if frost.remaining <= 0.0 {
            commands.entity(entity).remove::<OnFrost>();
            continue;
        }

        frost.particle_tick -= dt;
        if frost.particle_tick > 0.0 { continue; }
        frost.particle_tick = FROST_PARTICLE_TICK_INTERVAL;

        for _ in 0..FROST_PARTICLES_PER_TICK {
            let pos = random_body_point(tf, extent, &mut rng);
            // Gentle sway, slight sink — opposite of fire's strong updraft.
            let vel = Vec2::new(rng.gen_range(-4.0..4.0), rng.gen_range(-6.0..-1.0));
            let life = rng.gen_range(0.45..0.80);
            let scale = rng.gen_range(0.4..0.7);
            commands.spawn((
                Mesh2d(em.particle.clone()),
                MeshMaterial2d(pm.frost.clone()),
                Transform {
                    translation: Vec3::new(pos.x, pos.y, 5.5),
                    scale: Vec3::new(scale, scale, 1.0),
                    ..default()
                },
                HitParticle { life, max_life: life, base_scale: scale },
                Velocity(vel),
                RenderLayers::layer(PLAY_LAYER),
            ));
        }
    }
}
