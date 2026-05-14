//! Ammo-overlay status effects ("runes") that any gun can carry.
//!
//! Adding a new rune type:
//! 1. Add a variant to `Rune`.
//! 2. Add rows in `label`, `description`, `proc_coefficient`.
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
    /// Converts any bullet weapon into [AOE]. On impact, splashes
    /// `BLAST_SPLASH_FRAC` of the bullet's damage to every enemy
    /// within `stacks × BLAST_RADIUS_PER_STACK × rune_effect` of the
    /// hit point. Doesn't itself stack the bullet's other runes onto
    /// the splash victims — the primary target still gets the full
    /// rune chain, while splash targets get just the damage. Plays
    /// well with Standard / MG; transformative on single-target
    /// weapons like Sniper.
    Blast,
    /// +100% × stacks × rune_effect movement speed to the deployed
    /// unit of an [Autonomous] turret (HeliPad's helicopter, Cage's
    /// octopus). No effect on non-autonomous slots. Stacks with the
    /// Autonomous synergy's per-tier speed bonus.
    Hustle,
    /// Bullet survives the first hit and continues to the next enemy
    /// in its path, dealing reduced damage on each pierce. Each stack
    /// adds one extra pierce. Rune Effect raises the per-pierce damage
    /// floor (less reduction at higher Rune Effect). Bullet-only — no
    /// effect on Blade, Mortar splash, Beam, or autonomous weapons.
    Pierce,
    /// Long-tail economy rune. Every Nth kill landed by a bullet
    /// carrying Greed drops +1 scrap. N starts at 25 and is reduced
    /// by stacks (more frequent payouts) and by Rune Effect (better
    /// efficiency). Accumulator is global so cross-slot Greeds share
    /// the same kill counter.
    Greed,
    /// +X% damage to enemies below 30% HP. Each stack adds the bonus,
    /// scaled by Rune Effect. Anti-finisher pressure that pairs with
    /// sustained fire / Cascade.
    Executioner,
    /// Bonus damage on the first hit of an engagement: when the target
    /// is at full HP, the hit deals extra damage. Each stack adds the
    /// bonus, scaled by Rune Effect. Reads as "alpha strike" — great
    /// on slow heavy weapons (Sniper, Cannon).
    Opener,
    /// On kill, spawn a small heal pickup at the corpse. The pickup
    /// vanishes on player contact and heals 1 HP per stack scaled by
    /// Rune Effect. See `tick_on_leftovers` for the pickup lifecycle
    /// and the pickup-collision check in `ship.rs`.
    Leftovers,
    /// +25% XP per stack scaled by Rune Effect on every kill credited
    /// to a bullet carrying this rune. Read in `process_damage_event`
    /// at the kill branch.
    Star,
    /// After a kill, the next shot from the SAME turret slot deals
    /// +50% damage per stack (scaled by Rune Effect). Slot-state lives
    /// on `TurretSlot.thirst_bonus`; consumed on the very next fired
    /// shot.
    Thirst,
    /// `[Support]`-only periodic heal. Every 5s, each socketed Medic
    /// on a Support-tagged weapon heals 2 HP per stack scaled by
    /// Rune Effect. Off-slot stacks (Medic on a non-Support weapon)
    /// are inert. Driven by `tick_on_medic`.
    Medic,
    /// `[Melee]`-only stacking move-speed buff. Each kill credited
    /// to a Melee-tagged slot carrying Rally adds a stack worth
    /// `+1% × stacks × Rune Effect` move speed for 5s. Stacks decay
    /// independently; `tick_on_rally` removes expired ones and rolls
    /// the effective buff into `PlayerStats.move_speed`.
    Rally,
    /// Per-side contact-retaliation. When the friendly hull rams an
    /// enemy, the impact maps to one turret slot via
    /// `ship::slot_for_contact` (same mapping `SpikedPlate` uses).
    /// Only Thorns runes on THAT slot fire — each stack adds
    /// `+1 × Rune Effect` chip damage. Rune placement matters as much
    /// as weapon placement: a Thorns on the bow slot only retaliates
    /// for head-on contact.
    Thorns,
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
            Rune::Blast            => tr("rune_blast"),
            Rune::Hustle           => tr("rune_hustle"),
            Rune::Pierce           => tr("rune_pierce"),
            Rune::Greed            => tr("rune_greed"),
            Rune::Executioner      => tr("rune_executioner"),
            Rune::Opener           => tr("rune_opener"),
            Rune::Leftovers        => tr("rune_leftovers"),
            Rune::Star             => tr("rune_star"),
            Rune::Thirst           => tr("rune_thirst"),
            Rune::Medic            => tr("rune_medic"),
            Rune::Rally            => tr("rune_rally"),
            Rune::Thorns           => tr("rune_thorns"),
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
            Rune::Blast            => tr("rune_blast_desc"),
            Rune::Hustle           => tr("rune_hustle_desc"),
            Rune::Pierce           => tr("rune_pierce_desc"),
            Rune::Greed            => tr("rune_greed_desc"),
            Rune::Executioner      => tr("rune_executioner_desc"),
            Rune::Opener           => tr("rune_opener_desc"),
            Rune::Leftovers        => tr("rune_leftovers_desc"),
            Rune::Star             => tr("rune_star_desc"),
            Rune::Thirst           => tr("rune_thirst_desc"),
            Rune::Medic            => tr("rune_medic_desc"),
            Rune::Rally            => tr("rune_rally_desc"),
            Rune::Thorns           => tr("rune_thorns_desc"),
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
            // Blast splash hits don't re-trigger runes — the primary
            // target already ran the full proc chain, splash targets
            // get raw damage only.
            Rune::Blast           => 0.0,
            // Hustle is a passive autonomous-unit speed buff; never
            // procs.
            Rune::Hustle          => 0.0,
            // Pierce is a bullet-survival modifier read at spawn time
            // and inside `bullet_collisions`; never procs.
            Rune::Pierce          => 0.0,
            // Greed fires inline on-kill from `process_damage_event`
            // — no chain payload.
            Rune::Greed           => 0.0,
            // Executioner / Opener are pure damage multipliers folded
            // into the primary hit; they don't trigger chain events.
            Rune::Executioner     => 0.0,
            Rune::Opener          => 0.0,
            // Leftovers fires on-kill (spawns a heal pickup) — no
            // chain payload. Same for Star (XP boost) and Thirst
            // (next-shot bonus). Medic / Rally / Thorns are passive
            // or kill-driven slot effects, never bullet chains.
            Rune::Leftovers       => 0.0,
            Rune::Star            => 0.0,
            Rune::Thirst          => 0.0,
            Rune::Medic           => 0.0,
            Rune::Rally           => 0.0,
            Rune::Thorns          => 0.0,
        }
    }
}

/// Per-slot autonomous-unit speed multiplier from `Hustle` runes on
/// the same socket. Returns 1.0 (no change) if none equipped; each
/// stack adds +1.0 × `rune_effect`. Read by `heli.rs` and
/// `octopus.rs` at the speed calculation site.
pub fn hustle_speed_mult(runes: &[Rune], rune_effect: f32) -> f32 {
    let stacks = runes.iter().filter(|r| matches!(r, Rune::Hustle)).count() as f32;
    1.0 + stacks * rune_effect
}

impl Rune {
    /// Apply this rune's proc effect to a damage event, having already
    /// passed the proc-roll. Per-variant behaviour lives in one match
    /// here so adding a new rune is a single edit (this method + the
    /// label / description / proc_coefficient stubs above), instead
    /// of having to thread changes into both `process_damage_event`'s
    /// match AND `apply_rune_stacked`.
    ///
    /// `stacks` is the number of copies of THIS rune on the bullet —
    /// the proc-resolution loop has already collapsed duplicates so
    /// the count here is meaningful (3 Fire runes = burn 3× as fast).
    ///
    /// `chain` is the same `Vec<DamageEvent>` the proc loop pops from;
    /// arms that fire chain damage (Shock) push back onto it for
    /// same-frame resolution.
    pub fn apply_proc(
        self,
        stacks: u8,
        ev: &crate::bullet::DamageEvent,
        chain: &mut Vec<crate::bullet::DamageEvent>,
        commands: &mut Commands,
        player_stats: &crate::stats::PlayerStats,
        pm: &PaletteMaterials,
        em: &EffectMeshes,
        on_resonate: &Query<&OnResonate>,
        enemy_snap: &[(Entity, Vec2, f32)],
        rng: &mut rand::rngs::ThreadRng,
    ) {
        match self {
            Rune::Fire | Rune::Frost | Rune::Bleed => {
                apply_rune_stacked(commands, ev.target, self, stacks);
            }
            Rune::Shock => {
                // Total chain bolts = `stacks × chains_per_rune`, where
                // `chains_per_rune` comes from the player's Rune Damage
                // stat (rounded, min 1). Default stat = 1.0 keeps the
                // old "one chain per Shock rune" behaviour; pumping
                // Rune Damage scales the chain count linearly so the
                // tooltip's "chain lightning to (Rune Damage) enemies"
                // matches what actually happens.
                let r2 = crate::balance::SHOCK_CHAIN_RANGE
                    * crate::balance::SHOCK_CHAIN_RANGE;
                let chains_per_rune = player_stats
                    .rune_damage_mult()
                    .round()
                    .max(1.0) as u32;
                let total_chains = stacks as u32 * chains_per_rune;
                let mut excluded: Vec<Entity> = vec![ev.target];
                for _ in 0..total_chains {
                    let chain_target = enemy_snap
                        .iter()
                        .filter(|(e, _, _)| !excluded.contains(e))
                        .map(|&(e, p, _)| (e, p, p.distance_squared(ev.hit_pos)))
                        .filter(|(_, _, d2)| *d2 <= r2)
                        .min_by(|a, b| {
                            a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal)
                        });
                    let Some((target, target_pos, _)) = chain_target else { break };
                    excluded.push(target);
                    crate::bullet::spawn_lightning_arc(
                        commands, em, &pm.shock, ev.hit_pos, target_pos,
                    );
                    let mut next_procced = ev.procced.clone();
                    next_procced.push(Rune::Shock);
                    chain.push(crate::bullet::DamageEvent {
                        target,
                        amount: ev.amount, // shock chain = 100% weapon damage
                        hit_pos: target_pos,
                        weapon: ev.weapon,
                        source: None,
                        runes: ev.runes.clone(),
                        procced: next_procced,
                        proc_strength: ev.proc_strength * Rune::Shock.proc_coefficient(),
                    });
                }
                // Suppress the otherwise-unused `rng` warning for the
                // arms that don't need randomness — keeping the
                // parameter on the signature means future runes can
                // reach for it without re-threading.
                let _ = rng;
            }
            Rune::Echo => {
                // One delayed event per Echo stack — 3 Echo runes ⇒
                // 3 follow-up hits on the same target.
                for _ in 0..stacks {
                    commands.spawn(EchoPending {
                        timer: ECHO_DELAY,
                        target: ev.target,
                        damage: ev.amount,
                        source: ev.source,
                        weapon: ev.weapon,
                    });
                }
            }
            Rune::Cascade => {
                // Handled in `process_damage_event`'s lethal branch —
                // Cascade fires *because* the target died, so it sits
                // outside the proc-roll-gated path. No-op here.
            }
            Rune::Conduit => {
                apply_rune_stacked(commands, ev.target, Rune::Conduit, stacks);
                crate::effects::spawn_hit_particles(
                    commands, em, &pm.shock, ev.hit_pos, 4, 35.0, rng,
                );
            }
            Rune::Resonate => {
                // Add `stacks` Resonate stacks on this hit (capped),
                // so a 3-Resonate socket winds the amp up 3× faster
                // than a 1-Resonate socket.
                let current = on_resonate.get(ev.target).map(|r| r.stacks).unwrap_or(0);
                let new_stacks = current
                    .saturating_add(stacks)
                    .min(crate::balance::RESONATE_MAX_STACKS);
                commands.entity(ev.target).insert(OnResonate::new(new_stacks));
                crate::effects::spawn_hit_particles(
                    commands, em, &pm.bullet_sniper, ev.hit_pos, 3, 30.0, rng,
                );
            }
            // Targeting runes are passive — read at aim time by
            // `turret_aim_fire`, never proc on hit.
            Rune::TargetFurthest
            | Rune::TargetHighestHp
            | Rune::TargetLowestHp
            | Rune::TargetCarousel
            | Rune::Splash => {}
            // Vampire/Ward/Blast fire inline upstream in
            // `process_damage_event`, regardless of proc roll. Hustle
            // is a passive autonomous-unit speed buff — never reaches
            // the proc loop. Pierce is read at bullet spawn time.
            // Greed / Executioner / Opener are folded into the primary
            // hit (or the lethal branch for Greed). The new content-
            // batch runes (Leftovers / Star / Thirst / Medic / Rally /
            // Thorns) all fire elsewhere too — on the KillEvent bus,
            // a periodic tick, or the friendly-ram-damage site.
            Rune::Vampire
            | Rune::Ward
            | Rune::Blast
            | Rune::Hustle
            | Rune::Pierce
            | Rune::Greed
            | Rune::Executioner
            | Rune::Opener
            | Rune::Leftovers
            | Rune::Star
            | Rune::Thirst
            | Rune::Medic
            | Rune::Rally
            | Rune::Thorns => {}
        }
    }
}

// ---------- Kill-credit event bus ----------
//
// Emitted from `process_damage_event`'s lethal branch every time a
// damage event drops the target's HP to 0. Carries the dying entity
// + the source slot (if a player turret landed the killing blow) +
// the full rune set on the killing hit. `enemy_death_check`
// consumes the events on the same frame and dispatches each
// on-kill rune effect (XP bonus for Star, heal-pickup spawn for
// Leftovers, future effects). Any other future "I want to react to
// a kill" system reads the same stream — adding a new on-kill rune
// no longer requires a per-rune marker component.

#[derive(Event, Clone, Debug)]
pub struct KillEvent {
    pub target: Entity,
    /// `Some(slot_idx)` when a player turret bullet landed the
    /// lethal hit; `None` for ally / boss / chain damage. Unused by
    /// the current Star / Leftovers readers (they only filter by
    /// rune) but kept on the event so future per-slot on-kill
    /// effects don't have to re-thread it.
    #[allow(dead_code)]
    pub source_slot: Option<u8>,
    /// Runes carried by the bullet that landed the killing blow.
    /// Readers filter by `Rune::Star` etc. — keeping the full slice
    /// here lets future on-kill runes plug in without changes to
    /// the writer.
    pub runes: Vec<Rune>,
}

// ---------- Generic pickup framework ----------
//
// `Magnetic` is the magnet-pull component shared by every drop type.
// Attach it alongside a pickup-specific marker (e.g. `HpPickup`) and
// `tick_magnetic_pickups` will draw the entity toward the player
// once the ship enters `pull_radius`. Future drop kinds (scrap,
// shield top-ups, ally summons) plug in by spawning with the same
// component — no per-drop magnet bookkeeping required.

#[derive(Component, Clone, Copy)]
pub struct Magnetic {
    /// World distance at which the magnet engages. Outside this
    /// radius the pickup is stationary (relying only on its own
    /// lifetime decay).
    pub pull_radius: f32,
    /// Initial draw speed (world units/sec) the moment the ship
    /// enters the radius.
    pub base_speed: f32,
    /// Per-second acceleration applied while the ship stays inside
    /// the radius. Keeps the pull tight at long range and snappy
    /// at close range.
    pub accel: f32,
    /// Internal — current pull speed, updated every frame while in
    /// radius. Reset to `base_speed` when the ship leaves the radius
    /// so a re-entry doesn't keep stale momentum.
    pub current_speed: f32,
}

impl Magnetic {
    /// Default tuning that reads as "tight magnet pull when the ship
    /// passes close by" — ~18 world units of catch range, ~30u/s
    /// initial pull ramping up by 60u/s² while engaged.
    pub fn default_pull() -> Self {
        Self {
            pull_radius: 18.0,
            base_speed: 30.0,
            accel: 60.0,
            current_speed: 30.0,
        }
    }
}

/// Per-frame magnet driver. Iterates every `Magnetic` pickup and,
/// if the friendly ship is within `pull_radius`, slides the entity's
/// Transform toward the ship at an accelerating pace. Doesn't touch
/// the pickup's collision / lifetime — those live on the per-drop
/// systems (e.g. `tick_hp_pickups`).
pub fn tick_magnetic_pickups(
    time: Res<Time>,
    friendly: Query<&Transform, (With<crate::components::Friendly>, Without<Magnetic>)>,
    mut pickups: Query<(&mut Transform, &mut Magnetic), Without<crate::components::Friendly>>,
) {
    let Ok(ftf) = friendly.single() else { return };
    let fp = ftf.translation.truncate();
    let dt = time.delta_secs();
    for (mut tf, mut mag) in &mut pickups {
        let pp = tf.translation.truncate();
        let to = fp - pp;
        let dist = to.length();
        if dist > mag.pull_radius {
            // Out of range — reset speed so a future engagement
            // starts at the configured base instead of compounding
            // stale acceleration.
            if mag.current_speed != mag.base_speed {
                mag.current_speed = mag.base_speed;
            }
            continue;
        }
        // In radius — accelerate toward the ship. Cap step size to
        // the remaining distance so we don't overshoot through the
        // hull on a fast pull.
        mag.current_speed += mag.accel * dt;
        let step = (mag.current_speed * dt).min(dist);
        if dist > 0.001 {
            let dir = to / dist;
            tf.translation.x += dir.x * step;
            tf.translation.y += dir.y * step;
        }
    }
}

// ---------- Heal pickup (Leftovers spawn product) ----------

/// Heal value scaled by `rune_effect` at spawn time. Despawns on
/// player contact (heals up to `heal` HP) or after `lifetime` seconds.
#[derive(Component)]
pub struct HpPickup {
    pub heal: i32,
    pub lifetime: f32,
}

/// World radius of a heal pickup for the collision check. Slightly
/// larger than the player's hit radius so passing-close grabs the
/// drop.
pub const HP_PICKUP_RADIUS: f32 = 4.0;
/// Default lifetime before unclaimed pickups vanish.
pub const HP_PICKUP_LIFETIME: f32 = 8.0;
/// Visual radius — small enough to read as a sub-object, big enough
/// to spot at a glance.
pub const HP_PICKUP_VISUAL_R: f32 = 1.4;

/// Spawn a heal pickup at `pos` worth `heal` HP. Caller is responsible
/// for clamping `heal` to >= 1 (a 0-heal pickup is a waste of a render
/// slot).
pub fn spawn_hp_pickup(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ColorMaterial>,
    pos: Vec2,
    heal: i32,
) {
    let mesh = meshes.add(Circle::new(HP_PICKUP_VISUAL_R));
    let mat = materials.add(Color::srgb(0.40, 0.95, 0.55));
    commands.spawn((
        Mesh2d(mesh),
        MeshMaterial2d(mat),
        Transform::from_xyz(pos.x, pos.y, 4.5),
        HpPickup {
            heal: heal.max(1),
            lifetime: HP_PICKUP_LIFETIME,
        },
        // Magnet pull — sliding a heal toward the ship reads as
        // "the game wants you to take this", and saves the player
        // from chasing partial heals during a clear.
        Magnetic::default_pull(),
        RenderLayers::layer(PLAY_LAYER),
    ));
}

/// Per-frame pickup tick: decay lifetime, despawn expired, heal +
/// despawn on player contact.
pub fn tick_hp_pickups(
    time: Res<Time>,
    mut commands: Commands,
    player_stats: Res<crate::stats::PlayerStats>,
    mut pickups: Query<(Entity, &Transform, &mut HpPickup)>,
    mut friendly: Query<
        (&Transform, &mut Health),
        (With<crate::components::Friendly>, Without<HpPickup>),
    >,
) {
    let dt = time.delta_secs();
    let Ok((ftf, mut fh)) = friendly.single_mut() else { return };
    let fp = ftf.translation.truncate();
    let max = player_stats.max_hp();
    let pickup_r2 = HP_PICKUP_RADIUS * HP_PICKUP_RADIUS;
    for (e, tf, mut pickup) in &mut pickups {
        pickup.lifetime -= dt;
        if pickup.lifetime <= 0.0 {
            commands.entity(e).despawn();
            continue;
        }
        let pp = tf.translation.truncate();
        if pp.distance_squared(fp) < pickup_r2 && fh.0 < max {
            fh.0 = (fh.0 + pickup.heal).min(max);
            commands.entity(e).despawn();
        }
    }
}

// ---------- Generic buff-stacks engine ----------
//
// `BuffStacks` is the central store for any player-wide buff that
// stacks and decays. Each `BuffId` keys a `Vec<f32>` of remaining
// durations; `tick_buff_stacks` decays every stack uniformly and
// drops expired entries. Adding a new stacking buff is two lines
// (variant + a push call) — no per-buff resource + tick system.
//
// Today: `Rally` (Melee-kill move-speed) lives here. ThirstPending
// stays a per-slot one-shot (different shape). MedicTimer stays a
// periodic interval (different shape). Both could fold into this
// family with extensions; the first cut keeps the abstraction tight.

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum BuffId {
    /// Move-speed stack granted by a Melee-tagged slot's Rally kill.
    Rally,
}

#[derive(Resource, Default)]
pub struct BuffStacks {
    map: std::collections::HashMap<BuffId, Vec<f32>>,
}

impl BuffStacks {
    /// Add one stack of `id` with `duration` seconds remaining.
    pub fn push(&mut self, id: BuffId, duration: f32) {
        self.map.entry(id).or_default().push(duration);
    }
    /// Live stack count for `id`. Zero when the buff isn't in the map.
    pub fn count(&self, id: BuffId) -> usize {
        self.map.get(&id).map(|v| v.len()).unwrap_or(0)
    }
    /// Helper for the `Rally` move-speed math — every consumer that
    /// asks for "1 + per_stack × count × rune_effect" goes through
    /// here so the formula lives in one place.
    pub fn linear_mult(&self, id: BuffId, per_stack: f32, rune_effect: f32) -> f32 {
        1.0 + per_stack * self.count(id) as f32 * rune_effect
    }
}

/// Per-frame decay. Drops every expired stack across every `BuffId`
/// and removes empty entries so the map doesn't grow unboundedly
/// with retired buffs.
pub fn tick_buff_stacks(time: Res<Time>, mut buffs: ResMut<BuffStacks>) {
    let dt = time.delta_secs();
    buffs.map.retain(|_, stacks| {
        let mut i = 0;
        while i < stacks.len() {
            stacks[i] -= dt;
            if stacks[i] <= 0.0 {
                stacks.swap_remove(i);
            } else {
                i += 1;
            }
        }
        !stacks.is_empty()
    });
}

pub const RALLY_DURATION: f32 = 5.0;
/// Per-stack move-speed bonus contributed by one Rally stack
/// (pre-`rune_effect` scaling). +1% per stack matches the design
/// note; `friendly_movement` folds in the live Rune Effect stat.
pub const RALLY_PER_STACK: f32 = 0.01;

/// Bundled kill-credit writers used by `process_damage_events`.
/// Keeps the system signature under Bevy's 16-param cap when we
/// thread Thirst / Rally bookkeeping through alongside the existing
/// resources, AND carries the `KillEvent` writer so the lethal
/// branch can broadcast the kill instead of inserting per-rune
/// marker components.
#[derive(bevy::ecs::system::SystemParam)]
pub struct OnKillBookkeeping<'w> {
    pub cfg: Res<'w, crate::turret::TurretConfig>,
    pub thirst: ResMut<'w, ThirstPending>,
    pub buffs: ResMut<'w, BuffStacks>,
    pub kill_writer: EventWriter<'w, KillEvent>,
}

// ---------- Thirst (next-shot damage bonus) ----------

/// Per-slot count of Thirst stacks queued for the next shot. Indexed
/// by `TurretConfig::slots` index (0..8). `process_damage_event`'s
/// lethal branch writes this when the killing bullet's source is
/// `PlayerSlot(idx)` AND that slot carries Thirst runes;
/// `turret_aim_fire` reads + clears at the moment of the bonus shot.
#[derive(Resource, Default)]
pub struct ThirstPending(pub [u8; 8]);

impl ThirstPending {
    /// Set the pending stacks for slot `idx`. No-op if `idx` is out of
    /// range.
    pub fn set(&mut self, idx: usize, stacks: u8) {
        if let Some(slot) = self.0.get_mut(idx) {
            *slot = stacks;
        }
    }
    /// Drain the pending stacks for slot `idx` — returns the count
    /// and clears it.
    pub fn take(&mut self, idx: usize) -> u8 {
        match self.0.get_mut(idx) {
            Some(slot) => {
                let v = *slot;
                *slot = 0;
                v
            }
            None => 0,
        }
    }
}

/// Damage multiplier for `stacks` pending Thirst rolls, scaled by
/// `rune_effect`. Returns 1.0 when no stacks (no bonus). Each stack
/// adds 50% × rune_effect.
pub fn thirst_damage_mult(stacks: u8, rune_effect: f32) -> f32 {
    if stacks == 0 { return 1.0; }
    1.0 + 0.5 * stacks as f32 * rune_effect
}

// ---------- Medic (periodic Support-slot heal) ----------

#[derive(Resource, Default)]
pub struct MedicTimer(pub f32);

pub const MEDIC_INTERVAL: f32 = 5.0;
pub const MEDIC_HEAL_BASE: f32 = 2.0;

/// Every `MEDIC_INTERVAL`s, scan turret config: for each slot whose
/// weapon carries the `Support` tag, sum Medic-rune stacks and heal
/// the friendly hull by `MEDIC_HEAL_BASE × total_stacks × rune_effect`
/// HP (clamped to max). Medic stacks on non-Support weapons are inert.
pub fn tick_on_medic(
    time: Res<Time>,
    mut timer: ResMut<MedicTimer>,
    cfg: Res<crate::turret::TurretConfig>,
    player_stats: Res<crate::stats::PlayerStats>,
    mut friendly: Query<&mut Health, With<crate::components::Friendly>>,
) {
    timer.0 += time.delta_secs();
    if timer.0 < MEDIC_INTERVAL { return; }
    timer.0 -= MEDIC_INTERVAL;

    let mut stacks_total: u32 = 0;
    for slot in &cfg.slots {
        if !slot.equipped { continue; }
        let is_support = slot
            .weapon
            .tags()
            .iter()
            .any(|t| matches!(t, crate::weapon::WeaponTag::Support));
        if !is_support { continue; }
        for r in &slot.runes {
            if matches!(r, Some(Rune::Medic)) {
                stacks_total = stacks_total.saturating_add(1);
            }
        }
    }
    if stacks_total == 0 { return; }
    let heal = (MEDIC_HEAL_BASE * stacks_total as f32 * player_stats.rune_damage_mult())
        .round() as i32;
    if heal <= 0 { return; }
    let max = player_stats.max_hp();
    if let Ok(mut h) = friendly.single_mut() {
        if h.0 < max {
            h.0 = (h.0 + heal).min(max);
        }
    }
}

// ---------- Thorns (ram-contact bonus) ----------

/// Contact-damage bonus from Thorns runes socketed on a specific
/// slot. Mirrors Spike Plate's "the slot on the side you got hit on"
/// rule — the ram-damage caller maps the impact direction to a slot
/// via `ship::slot_for_contact`, then passes THAT slot's index here.
/// Each Thorns stack adds `+1 × rune_effect` chip damage, rounded up
/// so a single stack always reads as +1 minimum.
pub fn thorns_contact_bonus_for_slot(
    cfg: &crate::turret::TurretConfig,
    slot_idx: usize,
    rune_effect: f32,
) -> i32 {
    let Some(slot) = cfg.slots.get(slot_idx) else { return 0 };
    if !slot.equipped { return 0; }
    let stacks: u32 = slot
        .runes
        .iter()
        .filter(|r| matches!(r, Some(Rune::Thorns)))
        .count() as u32;
    if stacks == 0 { return 0; }
    (stacks as f32 * rune_effect).round().max(1.0) as i32
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
        | Rune::Ward
        | Rune::Blast
        | Rune::Hustle
        // Pierce is handled at bullet spawn time; Greed / Executioner /
        // Opener fire inline inside `process_damage_event`. Nothing to
        // attach to the target.
        | Rune::Pierce
        | Rune::Greed
        | Rune::Executioner
        | Rune::Opener
        // Leftovers / Star / Thirst / Medic / Rally / Thorns: no
        // status component attaches to the target. Their effects
        // fire either at the kill-credit site (Star, Leftovers,
        // Thirst, Rally), at a periodic tick (Medic, Rally decay),
        // or at the friendly hull's contact-ram (Thorns).
        | Rune::Leftovers
        | Rune::Star
        | Rune::Thirst
        | Rune::Medic
        | Rune::Rally
        | Rune::Thorns => {}
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
