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
    CONDUIT_DURATION, FIRE_DAMAGE_PER_TICK, FIRE_DAMAGE_TICK_INTERVAL, FIRE_DURATION,
    FIRE_PARTICLES_PER_TICK, FIRE_PARTICLE_TICK_INTERVAL, FROST_DURATION,
    FROST_PARTICLES_PER_TICK, FROST_PARTICLE_TICK_INTERVAL, PLAY_LAYER, RESONATE_DECAY,
    RESONATE_DAMAGE_PER_STACK,
};
use crate::bullet::apply_damage;
use crate::components::{Friendly, Health, Velocity};
use crate::effects::{spawn_hit_particles, EffectMeshes, HitFx, HitParticle};
use crate::enemy::Enemy;
use crate::i18n::tr;
use crate::modes::GameMode;
use crate::palette::PaletteMaterials;
use crate::ui::DamageStats;
use crate::weapon::WeaponType;

// ---------- Rune kind ----------

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Rune {
    Fire,
    Frost,
    Shock,
    /// Combo-popper. On hit, consumes any `OnFire` / `OnFrost` on the
    /// target for a burst of damage scaled by what's left of the
    /// status. Useless on a clean target — needs Fire/Frost from
    /// another slot to be the primer. Forces "prime then pop"
    /// gameplay across slots.
    Detonate,
    /// Proc multiplier — schedules a delayed second damage event 0.3s
    /// after impact on the same target. Total throughput = ~2× damage
    /// per shot, with built-in spacing so the rhythm is visible. Pairs
    /// with sustained-fire weapons (MG/Standard) and stacks visibly
    /// with target-status runes from other slots.
    Echo,
    /// On-kill snowball. When a hit kills its target, fires the same
    /// damage event at the nearest other enemy with `proc_strength ×
    /// 0.7`. Cascade isn't itself in the procced list, so a kill chain
    /// can keep cascading until proc strength decays out — naturally
    /// caps the snowball without an explicit hop limit.
    Cascade,
    /// On hit, applies the `OnConduit` status: future hits on this
    /// target proc at `× CONDUIT_PROC_MULT` strength. Doesn't damage
    /// or chain itself — pure proc-strength enabler. Cross-slot
    /// synergy: Conduit slot primes the target, proc-heavy slot (Shock,
    /// Detonate, Echo) reaps the boosted reliability.
    Conduit,
    /// On hit, applies / refreshes a stack of `OnResonate` (caps at
    /// `RESONATE_MAX_STACKS`). Each stack adds `+RESONATE_DAMAGE_PER_STACK`
    /// to all incoming damage on that target. Rewards focused fire and
    /// sustained-fire weapons; stacks decay after no-hit for
    /// `RESONATE_DECAY` seconds.
    Resonate,
}

impl Rune {
    pub fn label(self) -> &'static str {
        match self {
            Rune::Fire     => tr("rune_fire"),
            Rune::Frost    => tr("rune_frost"),
            Rune::Shock    => tr("rune_shock"),
            Rune::Detonate => tr("rune_detonate"),
            Rune::Echo     => tr("rune_echo"),
            Rune::Cascade  => tr("rune_cascade"),
            Rune::Conduit  => tr("rune_conduit"),
            Rune::Resonate => tr("rune_resonate"),
        }
    }

    /// Long-form description for tooltips, looked up via i18n.
    pub fn description(self) -> &'static str {
        match self {
            Rune::Fire     => tr("rune_fire_desc"),
            Rune::Frost    => tr("rune_frost_desc"),
            Rune::Shock    => tr("rune_shock_desc"),
            Rune::Detonate => tr("rune_detonate_desc"),
            Rune::Echo     => tr("rune_echo_desc"),
            Rune::Cascade  => tr("rune_cascade_desc"),
            Rune::Conduit  => tr("rune_conduit_desc"),
            Rune::Resonate => tr("rune_resonate_desc"),
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
            Rune::Fire     => 0.0,
            Rune::Frost    => 0.0,
            Rune::Shock    => 0.5,
            // Detonate's burst is a terminal effect — it's already a
            // payoff from primer runes; chaining it would double-dip.
            Rune::Detonate => 0.0,
            // Echo's delayed re-damage doesn't re-roll runes (the
            // second event runs through `tick_echoes`, not the proc
            // chain). 0 keeps semantics consistent with the
            // "secondary damage from this rune doesn't cascade" rule.
            Rune::Echo     => 0.0,
            // Cascade chain hits decay at 0.7 per hop — softer than
            // Shock's 0.5 since Cascade only fires on lethal so the
            // chain is already gated by kill density.
            Rune::Cascade  => 0.7,
            // Conduit / Resonate are status applies — same "no
            // cascade" rule as Fire / Frost.
            Rune::Conduit  => 0.0,
            Rune::Resonate => 0.0,
        }
    }
}

/// Cycle a slot's rune forward.
pub fn cycle_next(current: Option<Rune>) -> Option<Rune> {
    match current {
        None                 => Some(Rune::Fire),
        Some(Rune::Fire)     => Some(Rune::Frost),
        Some(Rune::Frost)    => Some(Rune::Shock),
        Some(Rune::Shock)    => Some(Rune::Detonate),
        Some(Rune::Detonate) => Some(Rune::Echo),
        Some(Rune::Echo)     => Some(Rune::Cascade),
        Some(Rune::Cascade)  => Some(Rune::Conduit),
        Some(Rune::Conduit)  => Some(Rune::Resonate),
        Some(Rune::Resonate) => None,
    }
}

/// Cycle backward — reverse of `cycle_next`.
pub fn cycle_prev(current: Option<Rune>) -> Option<Rune> {
    match current {
        None                 => Some(Rune::Resonate),
        Some(Rune::Resonate) => Some(Rune::Conduit),
        Some(Rune::Conduit)  => Some(Rune::Cascade),
        Some(Rune::Cascade)  => Some(Rune::Echo),
        Some(Rune::Echo)     => Some(Rune::Detonate),
        Some(Rune::Detonate) => Some(Rune::Shock),
        Some(Rune::Shock)    => Some(Rune::Frost),
        Some(Rune::Frost)    => Some(Rune::Fire),
        Some(Rune::Fire)     => None,
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
/// refreshes its duration. Does nothing for instant-effect runes
/// (Shock / Detonate / Echo) — those are handled inline by the bullet
/// damage processor.
pub fn apply_rune(commands: &mut Commands, entity: Entity, rune: Rune) {
    match rune {
        Rune::Fire     => { commands.entity(entity).insert(OnFire::new()); }
        Rune::Frost    => { commands.entity(entity).insert(OnFrost::new()); }
        Rune::Shock    => { /* no status — chain damage emitted by proc system */ }
        Rune::Detonate => { /* no status — burst applied inline by proc system */ }
        Rune::Echo     => { /* no status — delayed event spawned by proc system */ }
        Rune::Cascade  => { /* no status — on-kill chain emitted inline */ }
        Rune::Conduit  => { commands.entity(entity).insert(OnConduit::new()); }
        Rune::Resonate => {
            // Stack-aware insert handled inline by the proc system
            // because we need to read current stacks before writing.
        }
    }
}

/// Compute Detonate's burst damage from the target's current statuses
/// and clear those statuses. Returns the burst HP to apply (0 if the
/// target had nothing to detonate). The caller is responsible for
/// applying the damage and any visual flair.
///
/// Tuning rationale:
/// - Fire burst converts the *remaining* DoT into ~2× instant damage,
///   so popping early (lots of duration left) is high-value, popping
///   late is low-value.
/// - Frost has no DoT, so the burst is a flat 3 + remaining-seconds,
///   capped naturally by `FROST_DURATION`.
pub fn detonate_consume(
    commands: &mut Commands,
    target: Entity,
    on_fire: Option<&OnFire>,
    on_frost: Option<&OnFrost>,
) -> i32 {
    let mut burst = 0;
    if let Some(fire) = on_fire {
        let remaining_ticks =
            (fire.remaining / FIRE_DAMAGE_TICK_INTERVAL).max(0.0).floor() as i32;
        burst += remaining_ticks * FIRE_DAMAGE_PER_TICK * 2;
        commands.entity(target).remove::<OnFire>();
    }
    if let Some(frost) = on_frost {
        burst += (3.0 + frost.remaining.max(0.0)).round() as i32;
        commands.entity(target).remove::<OnFrost>();
    }
    burst
}

// ---------- Echo (delayed re-damage) ----------

/// Delay between an Echo proc and the second damage event landing.
/// Long enough that the player sees a distinct "second hit" rhythm,
/// short enough that the target hasn't usually moved out of relevance.
pub const ECHO_DELAY: f32 = 0.3;

/// Pending delayed damage event scheduled by an Echo proc. Lives as a
/// standalone entity so the lifetime is independent of the original
/// bullet (which despawns immediately on hit).
#[derive(Component)]
pub struct EchoPending {
    pub timer: f32,
    pub target: Entity,
    pub damage: i32,
    pub source: Option<crate::bullet::DamageSource>,
    pub weapon: WeaponType,
}

/// Tick every `EchoPending` and apply its damage when the timer expires.
/// Damage is applied directly via `apply_damage` — no proc chain re-
/// entry, since Echo's `proc_coefficient` is 0 (the bullet's other runes
/// already procced on the original hit). If the target is dead or
/// despawned by the time the echo fires, the event is silently dropped.
pub fn tick_echoes(
    time: Res<Time>,
    mut commands: Commands,
    mut stats: ResMut<DamageStats>,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    mut echoes: Query<(Entity, &mut EchoPending)>,
    mut targets: Query<(&Transform, &mut Health, &mut HitFx), With<Enemy>>,
    on_resonate: Query<&OnResonate>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();

    for (echo_e, mut echo) in &mut echoes {
        echo.timer -= dt;
        if echo.timer > 0.0 { continue; }

        if let Ok((tf, mut h, mut fx)) = targets.get_mut(echo.target) {
            if h.0 > 0 {
                // Resonate stacks (potentially applied by another slot
                // since the echo was scheduled) amplify the delayed
                // damage. Cross-slot Resonate + Echo is part of the
                // intended combo space.
                let mult = resonate_multiplier(on_resonate.get(echo.target).ok());
                let amount = (echo.damage as f32 * mult).round() as i32;
                let dealt = apply_damage(&mut h, &mut fx, amount);
                crate::bullet::credit_damage(&mut stats, echo.source, dealt);
                let pos = tf.translation.truncate();
                // Two-tone burst: weapon spark + a second cooler ring
                // so the echo reads as a distinct beat rather than a
                // duplicate impact.
                let spark_mat = pm.bullet_inner_for(echo.weapon);
                spawn_hit_particles(&mut commands, &em, spark_mat, pos, 5, 55.0, &mut rng);
                spawn_hit_particles(&mut commands, &em, &pm.shock,  pos, 4, 35.0, &mut rng);
            }
        }
        commands.entity(echo_e).despawn();
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
    player_stats: Res<crate::stats::PlayerStats>,
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
                let scaled = (FIRE_DAMAGE_PER_TICK as f32
                    * player_stats.rune_damage_mult())
                    .round() as i32;
                apply_damage(&mut hp, &mut fx, scaled.max(1));
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

// ---------- Conduit status (proc-strength buff) ----------

/// Boosts incoming proc rolls' strength on this target — the proc
/// system reads this component when computing `effective_proc_strength`.
/// Lifetime ticks down via `tick_on_conduit`; re-applying via Conduit
/// proc refreshes the duration (Bevy's `insert` overwrites).
#[derive(Component)]
pub struct OnConduit {
    pub remaining: f32,
}

impl OnConduit {
    pub fn new() -> Self {
        Self { remaining: CONDUIT_DURATION }
    }
}

/// Decay-only tick — Conduit has no per-frame effects (the proc-system
/// does the runtime work); this just removes the component when its
/// duration expires.
pub fn tick_on_conduit(
    time: Res<Time>,
    mut commands: Commands,
    mut q: Query<(Entity, &mut OnConduit)>,
) {
    let dt = time.delta_secs();
    for (e, mut c) in &mut q {
        c.remaining -= dt;
        if c.remaining <= 0.0 {
            commands.entity(e).remove::<OnConduit>();
        }
    }
}

// ---------- Resonate status (stacking damage amplifier) ----------

/// Stacking damage amplifier on the target. Each stack adds
/// `RESONATE_DAMAGE_PER_STACK` to all incoming damage. The proc system
/// reads `stacks` when computing the effective amount; `decay` is reset
/// to `RESONATE_DECAY` on every fresh stack-add and ticks down via
/// `tick_on_resonate`.
#[derive(Component)]
pub struct OnResonate {
    pub stacks: u8,
    pub decay: f32,
}

impl OnResonate {
    pub fn new(stacks: u8) -> Self {
        Self { stacks, decay: RESONATE_DECAY }
    }
}

/// Damage multiplier helper: turns an `OnResonate` stack count into the
/// scalar that should multiply incoming damage. Pulled out so every
/// damage source (bullet, echo, future heat-based runes) can share the
/// same amplification logic without each re-deriving the formula.
pub fn resonate_multiplier(on_resonate: Option<&OnResonate>) -> f32 {
    match on_resonate {
        Some(r) => 1.0 + r.stacks as f32 * RESONATE_DAMAGE_PER_STACK,
        None    => 1.0,
    }
}

/// Decay-only tick — Resonate has no per-frame visuals or damage; the
/// stacks just sit on the target until they decay out.
pub fn tick_on_resonate(
    time: Res<Time>,
    mut commands: Commands,
    mut q: Query<(Entity, &mut OnResonate)>,
) {
    let dt = time.delta_secs();
    for (e, mut r) in &mut q {
        r.decay -= dt;
        if r.decay <= 0.0 {
            commands.entity(e).remove::<OnResonate>();
        }
    }
}
