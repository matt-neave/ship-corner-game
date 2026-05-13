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
use crate::components::{Health, Velocity};
use crate::effects::{spawn_hit_particles, EffectMeshes, HitFx, HitParticle};
use crate::enemy::Enemy;
use crate::i18n::tr;
use crate::palette::PaletteMaterials;
use crate::ui::DamageStats;
use crate::weapon::WeaponType;

// ---------- Rune kind ----------

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Rune {
    Fire,
    Frost,
    Shock,
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
    /// Echo) reaps the boosted reliability.
    Conduit,
    /// On hit, applies / refreshes a stack of `OnResonate` (caps at
    /// `RESONATE_MAX_STACKS`). Each stack adds `+RESONATE_DAMAGE_PER_STACK`
    /// to all incoming damage on that target. Rewards focused fire and
    /// sustained-fire weapons; stacks decay after no-hit for
    /// `RESONATE_DECAY` seconds.
    Resonate,
    // ---- Targeting modifiers (passive) ----
    //
    // No proc effect — these runes change which enemy the turret picks
    // out of its in-arc, in-range candidates. `turret_aim_fire` reads
    // the slot's runes directly; if more than one targeting rune is
    // equipped, the first one (by socket index) wins.
    /// Turret prefers the furthest enemy in arc/range.
    TargetFurthest,
    /// Turret prefers the highest-HP enemy in arc/range.
    TargetHighestHp,
    /// Turret prefers the lowest-HP enemy in arc/range — good for
    /// finishing-blow snipers / scrap harvesting.
    TargetLowestHp,
    /// "Carousel" — round-robin targeting. Each shot cycles to the
    /// NEXT enemy in the candidate list instead of dumping every
    /// bullet into the same priority pick. Cycles deterministically
    /// via the slot's `cycle_idx` (advanced once per shot), so a
    /// stable enemy ordering produces a stable rotation. Great on
    /// sustained-fire weapons against grouped waves.
    TargetCarousel,
    /// Passive AoE-radius modifier. Currently scoped to Mortar's
    /// splash; future AoE weapons should multiply by the same factor.
    /// Each socketed `Splash` rune contributes additively (+50% per
    /// rune) so stacks read cleanly.
    Splash,
    /// Heal-on-hit. Each Vampire-tagged hit accumulates a fractional
    /// HP into a global player accumulator; every time the
    /// accumulator crosses 1.0 the player gains 1 HP (capped at
    /// max). Per-hit fraction = `stacks × rune_effect / 10`, so 1
    /// rune at 1× Rune Effect heals 1 HP per 10 hits, 2 runes at 2×
    /// heals 1 HP per ~2.5 hits.
    Vampire,
    /// Shield-on-kill. Each kill landed by a bullet carrying Ward
    /// grants `stacks × rune_effect` shield (capped at the player's
    /// `shield_max`). Pairs cleanly with shield-tank hulls
    /// (Revenant, Dreadnought) and any synergy that keeps the
    /// shield up.
    Ward,
    /// Anti-tank DoT — ticks damage as a percentage of the target's
    /// MAX HP rather than a flat number. Eight ticks over 4s at
    /// `1.5% × stacks × rune_effect` per tick. Hard on bosses
    /// (high max HP) and counter-design for the late-game tank
    /// curve; only meh on swarms (small max HP per body).
    Bleed,
}

impl Rune {
    pub fn label(self) -> &'static str {
        match self {
            Rune::Fire             => tr("rune_fire"),
            Rune::Frost            => tr("rune_frost"),
            Rune::Shock            => tr("rune_shock"),
            Rune::Echo             => tr("rune_echo"),
            Rune::Cascade          => tr("rune_cascade"),
            Rune::Conduit          => tr("rune_conduit"),
            Rune::Resonate         => tr("rune_resonate"),
            Rune::TargetFurthest   => tr("rune_target_furthest"),
            Rune::TargetHighestHp  => tr("rune_target_max_hp"),
            Rune::TargetLowestHp   => tr("rune_target_min_hp"),
            Rune::TargetCarousel   => tr("rune_target_carousel"),
            Rune::Splash           => tr("rune_splash"),
            Rune::Vampire          => tr("rune_vampire"),
            Rune::Ward             => tr("rune_ward"),
            Rune::Bleed            => tr("rune_bleed"),
        }
    }

    /// Long-form description for tooltips, looked up via i18n.
    pub fn description(self) -> &'static str {
        match self {
            Rune::Fire             => tr("rune_fire_desc"),
            Rune::Frost            => tr("rune_frost_desc"),
            Rune::Shock            => tr("rune_shock_desc"),
            Rune::Echo             => tr("rune_echo_desc"),
            Rune::Cascade          => tr("rune_cascade_desc"),
            Rune::Conduit          => tr("rune_conduit_desc"),
            Rune::Resonate         => tr("rune_resonate_desc"),
            Rune::TargetFurthest   => tr("rune_target_furthest_desc"),
            Rune::TargetHighestHp  => tr("rune_target_max_hp_desc"),
            Rune::TargetLowestHp   => tr("rune_target_min_hp_desc"),
            Rune::TargetCarousel   => tr("rune_target_carousel_desc"),
            Rune::Splash           => tr("rune_splash_desc"),
            Rune::Vampire          => tr("rune_vampire_desc"),
            Rune::Ward             => tr("rune_ward_desc"),
            Rune::Bleed            => tr("rune_bleed_desc"),
        }
    }

    /// True for any rune that drives the turret's target-selection
    /// rule. Used both by the gameplay picker and the customize UI
    /// (exclusivity: at most one targeting rune per weapon, plus a
    /// red lockout tint on the remaining sockets when one's
    /// equipped).
    pub fn is_targeting(self) -> bool {
        matches!(
            self,
            Rune::TargetFurthest
                | Rune::TargetHighestHp
                | Rune::TargetLowestHp
                | Rune::TargetCarousel,
        )
    }

    /// If this rune overrides the host turret's target-selection rule,
    /// the corresponding `TargetPriority`. Non-targeting runes return
    /// `None` and the slot falls back to `Closest`.
    pub fn target_priority(self) -> Option<crate::weapon::TargetPriority> {
        use crate::weapon::TargetPriority;
        match self {
            Rune::TargetFurthest  => Some(TargetPriority::Furthest),
            Rune::TargetHighestHp => Some(TargetPriority::HighestHp),
            Rune::TargetLowestHp  => Some(TargetPriority::LowestHp),
            _ => None,
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
            // Targeting runes are passive — `turret_aim_fire` reads
            // them directly; they never enter the proc chain.
            Rune::TargetFurthest  => 0.0,
            Rune::TargetHighestHp => 0.0,
            Rune::TargetLowestHp  => 0.0,
            Rune::TargetCarousel  => 0.0,
            // Splash is a passive AoE-radius modifier read by the
            // mortar firing path; never procs.
            Rune::Splash          => 0.0,
            // Vampire / Ward have no chain payload — once they fire
            // (per-hit heal, per-kill shield) there's no secondary
            // event to roll runes off of.
            Rune::Vampire         => 0.0,
            Rune::Ward            => 0.0,
            // Bleed DoT ticks are like Fire — inert to further
            // procs (otherwise a bleeding enemy would self-shock
            // every half-second).
            Rune::Bleed           => 0.0,
        }
    }
}

/// Cycle a slot's rune forward.
pub fn cycle_next(current: Option<Rune>) -> Option<Rune> {
    match current {
        None                       => Some(Rune::Fire),
        Some(Rune::Fire)           => Some(Rune::Frost),
        Some(Rune::Frost)          => Some(Rune::Shock),
        Some(Rune::Shock)          => Some(Rune::Echo),
        Some(Rune::Echo)           => Some(Rune::Cascade),
        Some(Rune::Cascade)        => Some(Rune::Conduit),
        Some(Rune::Conduit)        => Some(Rune::Resonate),
        Some(Rune::Resonate)       => Some(Rune::TargetFurthest),
        Some(Rune::TargetFurthest) => Some(Rune::TargetHighestHp),
        Some(Rune::TargetHighestHp)=> Some(Rune::TargetLowestHp),
        Some(Rune::TargetLowestHp) => Some(Rune::TargetCarousel),
        Some(Rune::TargetCarousel) => Some(Rune::Splash),
        Some(Rune::Splash)         => Some(Rune::Vampire),
        Some(Rune::Vampire)        => Some(Rune::Ward),
        Some(Rune::Ward)           => Some(Rune::Bleed),
        Some(Rune::Bleed)          => None,
    }
}

/// Cycle backward — reverse of `cycle_next`.
pub fn cycle_prev(current: Option<Rune>) -> Option<Rune> {
    match current {
        None                       => Some(Rune::Bleed),
        Some(Rune::Bleed)          => Some(Rune::Ward),
        Some(Rune::Ward)           => Some(Rune::Vampire),
        Some(Rune::Vampire)        => Some(Rune::Splash),
        Some(Rune::Splash)         => Some(Rune::TargetCarousel),
        Some(Rune::TargetCarousel) => Some(Rune::TargetLowestHp),
        Some(Rune::TargetLowestHp) => Some(Rune::TargetHighestHp),
        Some(Rune::TargetHighestHp)=> Some(Rune::TargetFurthest),
        Some(Rune::TargetFurthest) => Some(Rune::Resonate),
        Some(Rune::Resonate)       => Some(Rune::Conduit),
        Some(Rune::Conduit)        => Some(Rune::Cascade),
        Some(Rune::Cascade)        => Some(Rune::Echo),
        Some(Rune::Echo)           => Some(Rune::Shock),
        Some(Rune::Shock)          => Some(Rune::Frost),
        Some(Rune::Frost)          => Some(Rune::Fire),
        Some(Rune::Fire)           => None,
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
/// (Shock / Echo) — those are handled inline by the bullet
/// damage processor.
/// Stack-aware variant of `apply_rune`. Caller passes the stack
/// count so the bullet proc system can collapse duplicate runes
/// (3 Fire sockets ⇒ 1 apply at stacks = 3) without doing 3
/// Commands inserts that would each overwrite the previous one.
/// Stack count is clamped to `1..=MAX_STATUS_STACKS` inside the
/// per-status constructors.
pub fn apply_rune_stacked(
    commands: &mut Commands,
    entity: Entity,
    rune: Rune,
    stacks: u8,
) {
    let stacks = stacks.max(1);
    match rune {
        Rune::Fire     => { commands.entity(entity).insert(OnFire::new(stacks)); }
        Rune::Frost    => { commands.entity(entity).insert(OnFrost::new(stacks)); }
        Rune::Shock    => { /* no status — chain damage emitted by proc system */ }
        Rune::Echo     => { /* no status — delayed event spawned by proc system */ }
        Rune::Cascade  => { /* no status — on-kill chain emitted inline */ }
        Rune::Conduit  => { commands.entity(entity).insert(OnConduit::new(stacks)); }
        Rune::Resonate => {
            // Stack-aware insert handled inline by the proc system
            // because we need to read current stacks before writing.
        }
        Rune::Bleed => {
            commands.entity(entity).insert(OnBleed::new(stacks));
        }
        // Targeting runes never reach `apply_rune` — they're passive
        // and `turret_aim_fire` reads them straight off the slot.
        // Vampire / Ward fire inline inside `process_damage_events`
        // (per-hit heal accumulator + on-kill shield) so there's
        // nothing to attach to the target.
        Rune::TargetFurthest
        | Rune::TargetHighestHp
        | Rune::TargetLowestHp
        | Rune::TargetCarousel
        | Rune::Splash
        | Rune::Vampire
        | Rune::Ward => {}
    }
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

/// Cap on stackable rune effects. Same value across Fire / Frost /
/// Conduit so duplicate-stacking has a predictable ceiling; higher
/// would let one bullet stack instantly into invincible enemies
/// melting.
pub const MAX_STATUS_STACKS: u8 = 5;

#[derive(Component)]
pub struct OnFire {
    pub remaining: f32,
    pub damage_tick: f32,
    pub particle_tick: f32,
    /// Stack count. Damage per tick scales linearly with this.
    pub stacks: u8,
}

impl OnFire {
    pub fn new(stacks: u8) -> Self {
        Self {
            remaining: FIRE_DURATION,
            damage_tick: 0.0,
            particle_tick: 0.0,
            stacks: stacks.clamp(1, MAX_STATUS_STACKS),
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
/// scales velocity by `frost_speed_mult` while this is present, which
/// compounds with stack count). Counts down via `tick_on_frost` and
/// emits cool-blue mist particles.
#[derive(Component)]
pub struct OnFrost {
    pub remaining: f32,
    pub particle_tick: f32,
    /// Stack count. Speed multiplier compounds: 1 stack = `FROST_SPEED_MULT`,
    /// 2 stacks = `MULT^2`, etc. Capped at `MAX_STATUS_STACKS`.
    pub stacks: u8,
}

impl OnFrost {
    pub fn new(stacks: u8) -> Self {
        Self {
            remaining: FROST_DURATION,
            particle_tick: 0.0,
            stacks: stacks.clamp(1, MAX_STATUS_STACKS),
        }
    }

    /// Compounded speed multiplier from the stack count. Stacks
    /// multiply, capped so a fully-stacked frost can't fully stop a
    /// target (`min_mult = 0.05`).
    pub fn speed_mult(&self) -> f32 {
        let m = crate::balance::FROST_SPEED_MULT.powi(self.stacks as i32);
        m.max(0.05)
    }
}

// ---------- Bleed status ----------

/// Bleed DoT — ticks damage proportional to the target's MAX HP
/// rather than a flat number. Counters tank/boss curves (where a
/// fixed Fire DoT is a drop in the bucket) and weak vs swarms
/// (low max-HP per body = little damage per tick).
#[derive(Component)]
pub struct OnBleed {
    pub remaining: f32,
    pub damage_tick: f32,
    pub particle_tick: f32,
    pub stacks: u8,
}

impl OnBleed {
    pub fn new(stacks: u8) -> Self {
        Self {
            remaining: crate::balance::BLEED_DURATION,
            damage_tick: 0.0,
            particle_tick: 0.0,
            stacks: stacks.clamp(1, MAX_STATUS_STACKS),
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
/// Burns the target every `FIRE_DAMAGE_TICK_INTERVAL`s while `OnFire` is
/// alive. The friendly invincibility branch is gone — Wave mode is no
/// longer a thing and Sandbox lets the player be damaged like any other
/// target. Visual particles continue to play either way.
pub fn tick_on_fire(
    time: Res<Time>,
    mut commands: Commands,
    em: Option<Res<EffectMeshes>>,
    pm: Option<Res<PaletteMaterials>>,
    player_stats: Res<crate::stats::PlayerStats>,
    mut q: Query<(
        Entity,
        &Transform,
        &FireExtent,
        &mut OnFire,
        &mut Health,
        &mut HitFx,
    )>,
) {
    let Some(em) = em else { return; };
    let Some(pm) = pm else { return; };
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();

    for (entity, tf, extent, mut fire, mut hp, mut fx) in &mut q {
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
            // Damage scales linearly with stacks — a triple-Fire
            // socket burns 3× as fast as a single. Multiplied
            // through the rune-damage stat afterwards.
            let scaled = (FIRE_DAMAGE_PER_TICK as f32
                * fire.stacks as f32
                * player_stats.rune_damage_mult())
                .round() as i32;
            apply_damage(&mut hp, &mut fx, scaled.max(1));
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
    /// Stack count. `tick_on_conduit` reads this to scale the proc
    /// strength bonus applied while marked.
    pub stacks: u8,
}

impl OnConduit {
    pub fn new(stacks: u8) -> Self {
        Self {
            remaining: CONDUIT_DURATION,
            stacks: stacks.clamp(1, MAX_STATUS_STACKS),
        }
    }

    /// Effective proc-strength multiplier. Each stack adds
    /// `CONDUIT_PROC_MULT - 1` (currently +10%), scaled by the player's
    /// Rune Effect stat — so 1 stack at 1× Rune Effect = +10%, 3 stacks
    /// at 2× Rune Effect = +60%.
    pub fn proc_mult(&self, rune_effect: f32) -> f32 {
        let extra = crate::balance::CONDUIT_PROC_MULT - 1.0;
        1.0 + extra * self.stacks as f32 * rune_effect
    }
}

// ---------- Per-frame bleed driver ----------

/// Tick bleed damage as a percentage of the target's MAX HP, plus
/// the usual particle visual (small red drips). Mirrors
/// `tick_on_fire`'s structure — separate damage / particle timers
/// — but pulls `max_hp` from the `Enemy` component each tick so a
/// max-HP-scaling effect lands here instead of in
/// `apply_rune_stacked`.
pub fn tick_on_bleed(
    time: Res<Time>,
    mut commands: Commands,
    em: Option<Res<EffectMeshes>>,
    pm: Option<Res<PaletteMaterials>>,
    player_stats: Res<crate::stats::PlayerStats>,
    mut q: Query<(
        Entity,
        &Transform,
        &FireExtent,
        &Enemy,
        &mut OnBleed,
        &mut Health,
        &mut HitFx,
    )>,
) {
    let Some(em) = em else { return; };
    let Some(pm) = pm else { return; };
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();

    for (entity, tf, extent, enemy, mut bleed, mut hp, mut fx) in &mut q {
        bleed.remaining -= dt;
        if bleed.remaining <= 0.0 {
            commands.entity(entity).remove::<OnBleed>();
            continue;
        }

        bleed.damage_tick -= dt;
        if bleed.damage_tick <= 0.0 {
            bleed.damage_tick = crate::balance::BLEED_DAMAGE_TICK_INTERVAL;
            // Per-tick damage = max_hp × per-tick% × stacks ×
            // Rune Effect. Anti-tank: scales linearly with the
            // target's health pool, so the rune does meaningful
            // work against the boss curve.
            let raw = enemy.max_hp as f32
                * crate::balance::BLEED_PCT_PER_TICK
                * bleed.stacks as f32
                * player_stats.rune_damage_mult();
            apply_damage(&mut hp, &mut fx, raw.round().max(1.0) as i32);
        }

        bleed.particle_tick -= dt;
        if bleed.particle_tick > 0.0 { continue; }
        bleed.particle_tick = crate::balance::BLEED_PARTICLE_TICK_INTERVAL;

        // Small red drips. Sink downward (unlike Fire's rising
        // motes) so the visual reads as bleeding, not burning.
        for _ in 0..crate::balance::BLEED_PARTICLES_PER_TICK {
            let pos = random_body_point(tf, extent, &mut rng);
            let vel = Vec2::new(rng.gen_range(-2.0..2.0), rng.gen_range(-12.0..-6.0));
            let life = rng.gen_range(0.30..0.55);
            let scale = rng.gen_range(0.4..0.7);
            commands.spawn((
                Mesh2d(em.particle.clone()),
                MeshMaterial2d(pm.bleed.clone()),
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
