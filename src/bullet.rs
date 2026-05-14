//! Projectile component, the systems that touch it (travel/expiry tick,
//! collision routing), and the proc-coefficient damage chain that resolves
//! rune effects.
//!
//! ## Damage flow (friendly bullets vs enemies)
//!
//! 1. `bullet_collisions` snapshots all enemy positions, then iterates
//!    bullets and finds the first enemy each one overlaps.
//! 2. On hit, the bullet despawns and a `DamageEvent` is queued with
//!    `proc_strength = 1.0` (initial hit), the bullet's runes attached,
//!    and an empty procced list.
//! 3. After the bullet loop, the queue is drained one event at a time.
//!    Each event calls `apply_damage`, spawns a weapon-color flair, then
//!    rolls each unprocced rune attached to the source: a roll under
//!    `proc_strength` triggers it (initial hits are 100%; chains roll less).
//! 4. `Shock` procs queue another `DamageEvent` on the nearest other enemy
//!    in chain range — its `proc_strength` is multiplied by Shock's
//!    coefficient (`0.5`), and `Shock` joins the procced list so it can't
//!    re-trigger inside the same chain. Other runes attached to the bullet
//!    *can* still proc on the chain hit (e.g., a Shock+Fire bullet's
//!    chain hop can roll the Fire proc at 50%).
//! 5. `Fire` / `Frost` are status applies — they `insert` their status
//!    component on the target and don't queue further events. (Their DoT
//!    damage routes through `apply_damage` directly from `tick_on_fire`,
//!    not through this queue, which is why `Fire.proc_coefficient = 0.0`.)
//!
//! Enemy bullets carry no rune (player-only mechanic), so their hit path
//! stays inline and skips the proc system entirely.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;
use rand::Rng;

use crate::ally::{ally_hit_radius, ally_is_submerged, Ally};
use crate::balance::{
    BEAM_LENGTH, CASCADE_RANGE, PLAY_LAYER, SHOCK_VISUAL_LIFE,
};
use crate::beam::Beam;
use crate::cannon::Knockback;
use crate::components::{FactionKind, Friendly, Health, Velocity};
use crate::effects::{spawn_hit_particles, EffectMeshes, HitFx};
use crate::enemy::Enemy;
use crate::palette::PaletteMaterials;
use crate::rune::{resonate_multiplier, OnConduit, OnResonate, Rune};
use crate::ui::DamageStats;
use crate::weapon::WeaponType;

/// Single damage entry-point shared by every source (bullets, beams, fire,
/// detonations). Subtracts `amount` from `h`, pulses the hit-flash, and
/// returns the amount actually dealt (clamped to remaining HP, so overkill
/// doesn't inflate stat tallies).
///
/// Future damage modifiers (per-target debuffs, +N% crit, fire-vulnerable
/// armor, …) belong here so every source compounds them automatically.
pub fn apply_damage(h: &mut Health, fx: &mut HitFx, amount: i32) -> i32 {
    let dealt = amount.min(h.0).max(0);
    h.0 = (h.0 - amount).max(0);
    fx.pulse();
    dealt
}

/// Squared distance from point `p` to the line segment `[a, b]`.
/// Used by `bullet_collisions` for swept hit-tests so a fast bullet
/// can't tunnel through a small enemy between frames.
fn point_segment_dist_sq(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let len_sq = ab.length_squared();
    if len_sq < 1e-6 {
        return p.distance_squared(a);
    }
    let t = ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0);
    let proj = a + ab * t;
    p.distance_squared(proj)
}

/// Credit damage to the right `DamageStats` row by source. `None` is a
/// no-op (chain hops, enemy fire). Public so direct-`apply_damage`
/// sites (mines, boarders, missile direct hits) can credit themselves
/// without going through the bullet pipeline.
pub fn credit_damage(stats: &mut crate::ui::DamageStats, source: Option<DamageSource>, dealt: i32) {
    if dealt <= 0 { return; }
    let dealt_u = dealt as u64;
    match source {
        Some(DamageSource::PlayerSlot(s)) => {
            stats.per_slot[s as usize] = stats.per_slot[s as usize].saturating_add(dealt_u);
            stats.total = stats.total.saturating_add(dealt_u);
        }
        Some(DamageSource::Ally(class)) => {
            let i = class.to_index();
            stats.per_ally[i] = stats.per_ally[i].saturating_add(dealt_u);
            stats.total = stats.total.saturating_add(dealt_u);
        }
        None => {}
    }
}

/// Who fired a friendly damage instance. Used to credit kills and
/// damage to the right row in `DamageStats` (and in the LHS damage
/// panel). `None` = damage that shouldn't be credited (enemy fire,
/// chain hops, or untracked sources).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DamageSource {
    /// Player-ship turret slot (0..7).
    PlayerSlot(u8),
    /// Ally damage, aggregated by class so multiple PirateShips share
    /// one row in the panel.
    Ally(crate::ally::ShipClass),
}

/// Map an impact world-space position to the turret slot whose mount
/// angle is closest. Returns the resulting damage after a Spike Plate
/// chip-down (or the original damage when the matching slot isn't a
/// Spike Plate). Both ship-position `fp` and impact-position `ip` are
/// in world coords; `heading` is the ship's hull yaw.
fn spiked_plate_reduction(
    cfg: &crate::turret::TurretConfig,
    fp: Vec2,
    ip: Vec2,
    heading: f32,
    damage: i32,
) -> i32 {
    let dir = ip - fp;
    if dir.length_squared() < 0.001 { return damage; }
    let world_angle = (-dir.x).atan2(dir.y);
    let mut local_angle = world_angle - heading;
    local_angle = (local_angle + std::f32::consts::PI)
        .rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
    let mut best: Option<(usize, f32)> = None;
    for (i, &mount) in crate::balance::TURRET_MOUNTS.iter().enumerate() {
        let mut delta = local_angle - mount;
        delta = (delta + std::f32::consts::PI)
            .rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
        let abs = delta.abs();
        if best.map_or(true, |(_, b)| abs < b) {
            best = Some((i, abs));
        }
    }
    let Some((idx, _)) = best else { return damage };
    let slot = cfg.slots[idx];
    if slot.equipped
        && matches!(slot.weapon, crate::weapon::WeaponType::SpikedPlate)
    {
        (damage - crate::balance::SPIKED_PLATE_REDUCTION).max(0)
    } else {
        damage
    }
}

#[derive(Component)]
pub struct Bullet {
    pub faction: FactionKind,
    pub damage: i32,
    pub remaining: f32,
    /// Weapon that fired this bullet — drives spark/impact colors on hit.
    /// Unused for enemy bullets (always `Standard`).
    pub weapon: WeaponType,
    /// Who fired the bullet, for damage-attribution. `None` for enemy
    /// fire and untracked allied sources.
    pub source: Option<DamageSource>,
    /// Effective rune list snapshotted from the firing slot's
    /// `TurretSlot.runes` (which already folds in any neighbour
    /// `Amplifier` runes). On hit, the proc system rolls each rune
    /// independently. Empty for enemy bullets.
    pub runes: Vec<Rune>,
}

/// Attached to bullets fired with the Pierce rune. Lets the bullet
/// survive `max` hits before despawning, dealing geometrically-falling
/// damage on each subsequent hit. Tracks which enemies have already
/// been hit so the same bullet can't re-damage the same body on
/// successive frames while it's inside the hit-disc.
#[derive(Component)]
pub struct Pierce {
    /// Number of extra hits this bullet is allowed beyond the first.
    pub max: u8,
    /// Hits used so far. Damage on each new hit scales as
    /// `base × falloff^used`.
    pub used: u8,
    /// Per-pierce damage multiplier in [0, 1]. Resolved at spawn time
    /// from the base falloff floor and the player's Rune Effect stat.
    pub falloff: f32,
    /// Entities already hit by this bullet — prevents same-frame
    /// re-damage to the same body before it leaves the hit-disc.
    pub hit_targets: Vec<Entity>,
}

/// Helper: scan a slot's runes for Pierce stacks. Returns `Some(stacks)`
/// when there's at least one, `None` otherwise. Used by bullet-spawn
/// sites so they can attach the `Pierce` component without duplicating
/// the filter in every caller.
pub fn pierce_stacks(runes: &[Rune]) -> Option<u8> {
    let n = runes.iter().filter(|r| matches!(r, Rune::Pierce)).count() as u8;
    if n > 0 { Some(n) } else { None }
}

/// Build a `Pierce` from stack count + Rune Effect. Falloff floors at
/// `PIERCE_BASE_FALLOFF` and rises linearly with Rune Effect above 1×,
/// capped at 1.0 (no falloff).
pub fn make_pierce(stacks: u8, rune_effect: f32) -> Pierce {
    let base = crate::balance::PIERCE_BASE_FALLOFF;
    let falloff = (base + (1.0 - base) * (rune_effect - 1.0).max(0.0)).clamp(base, 1.0);
    Pierce {
        max: stacks,
        used: 0,
        falloff,
        hit_targets: Vec::new(),
    }
}

pub fn bullet_update(
    time: Res<Time>,
    mut commands: Commands,
    mut q: Query<(Entity, &mut Bullet, &Velocity)>,
) {
    let dt = time.delta_secs();
    for (e, mut b, v) in &mut q {
        b.remaining -= v.0.length() * dt;
        if b.remaining <= 0.0 {
            commands.entity(e).despawn();
        }
    }
}

/// One unit of pending damage + rune resolution. Producers (bullet
/// collisions, blade tick, octopus tentacle slap, mortar splash,
/// beam pierce, …) push onto `PendingDamageQueue`; the
/// `process_damage_events` system drains the queue, applies damage,
/// rolls runes, and pushes any chain events (Shock / Cascade) back
/// onto it for same-frame resolution.
pub struct DamageEvent {
    pub target: Entity,
    pub amount: i32,
    pub hit_pos: Vec2,
    pub weapon: WeaponType,
    /// Originating source for `DamageStats` crediting. `None` for chain
    /// hops so the secondary damage doesn't inflate the originating
    /// source's share.
    pub source: Option<DamageSource>,
    /// Effective rune list at the moment the source fired — already
    /// flattened (no Options) and Amplifier-merged at sync time, so
    /// duplicate entries are intentional stack counts (3 Fire = stack
    /// 3). Each rune may proc on this hit, gated by `procced` +
    /// `proc_strength`.
    pub runes: Vec<Rune>,
    /// Runes already triggered upstream in this chain — preventing the same
    /// effect from firing twice within one chain. Risk-of-Rain-style.
    pub procced: Vec<Rune>,
    /// Strength multiplier for proc rolls on this hit. Initial hits are
    /// `1.0`; secondary chain hits inherit `parent_strength * rune.coeff`.
    pub proc_strength: f32,
}

/// Shared push-and-drain queue for `DamageEvent`. Every weapon that
/// applies damage to enemies pushes here; `process_damage_events`
/// drains. Centralising this means runes work uniformly for ANY
/// weapon, not just the ones that happen to fire bullets.
#[derive(Resource, Default)]
pub struct PendingDamageQueue(pub Vec<DamageEvent>);

/// Fractional heal counter used by the Vampire rune. Each Vampire-
/// carrying hit accumulates `stacks × rune_effect / 10` here; whenever
/// the value crosses 1.0 the player gains 1 HP (and we subtract the
/// integer part). Living in a Resource (not a Component on the ship)
/// keeps the accounting global — every bullet from every turret
/// contributes to the same pool, so partial heals add up cleanly
/// across frames and shots.
#[derive(Resource, Default)]
pub struct VampireAccumulator(pub f32);

/// Kill counter for the Greed rune. Increments on every kill scored
/// by a bullet carrying Greed (regardless of how many Greed stacks
/// the bullet had — stacks reduce the threshold, not the increment).
/// When it crosses the per-bullet threshold the accumulator decrements
/// by that threshold and +1 scrap is granted. Global so cross-slot
/// Greeds share the same counter.
#[derive(Resource, Default)]
pub struct GreedAccumulator(pub u32);

impl PendingDamageQueue {
    /// Convenience helper — push a fresh DamageEvent (proc_strength
    /// 1.0, no procced yet) onto the queue. `runes` is a slice of
    /// the effective rune list (already flattened — Amplifier merge
    /// happens at `sync_turret_config` time, so what arrives here
    /// is the full set the proc system should roll).
    pub fn push_initial(
        &mut self,
        target: Entity,
        amount: i32,
        hit_pos: Vec2,
        weapon: WeaponType,
        source: Option<DamageSource>,
        runes: &[Rune],
    ) {
        if amount <= 0 { return; }
        self.0.push(DamageEvent {
            target,
            amount,
            hit_pos,
            weapon,
            source,
            runes: runes.to_vec(),
            procced: Vec::new(),
            proc_strength: 1.0,
        });
    }
}

pub fn bullet_collisions(
    time: Res<Time>,
    mut commands: Commands,
    player_stats: Res<crate::stats::PlayerStats>,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    cfg: Res<crate::turret::TurretConfig>,
    difficulty: Res<crate::Difficulty>,
    mut sfx: crate::sfx::SfxPlayer,
    mut queue: ResMut<PendingDamageQueue>,
    bullets: Query<
        (
            Entity, &Transform, &Bullet, &Velocity,
            Option<&Knockback>,
            Option<&crate::harpoon::HarpoonTip>,
        ),
        (Without<Enemy>, Without<Friendly>, Without<Ally>),
    >,
    mut pierces: Query<&mut Pierce>,
    enemies: Query<(Entity, &Transform, &Enemy, &mut Health, &mut HitFx, &mut Velocity), (With<Enemy>, Without<Friendly>)>,
    mut friendly: Query<(Entity, &Transform, &mut Health, &mut HitFx, Option<&mut crate::stats::Shield>, &crate::components::Heading), (With<Friendly>, Without<Enemy>, Without<Ally>)>,
    mut allies: Query<(Entity, &Transform, &Ally, &mut Health, &mut HitFx), (With<Ally>, Without<Enemy>, Without<Friendly>)>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let mut rng = rand::thread_rng();

    // Snapshot enemies (entity, position, hit-radius) once for swept-
    // segment hit-tests below. The drain (`process_damage_events`)
    // builds its own snapshot from a separate query when it runs.
    let enemy_snap: Vec<(Entity, Vec2, f32)> = enemies
        .iter()
        .map(|(e, tf, en, _, _, _)| (e, tf.translation.truncate(), 3.5 * en.variant.scale()))
        .collect();

    let dt = time.delta_secs();

    for (be, btf, b, bv, kb, harpoon_tip) in &bullets {
        let bp = btf.translation.truncate();
        // Swept segment for this frame. `apply_velocity` already moved
        // the bullet to `bp` this tick; rewind by `velocity * dt` for
        // the pre-step position. Testing the whole segment against
        // each enemy radius eliminates tunneling — fast bullets
        // (railgun-speed shotguns, MG with high stride) used to skip
        // small enemies between frames.
        let prev_bp = bp - bv.0 * dt;
        match b.faction {
            FactionKind::Friendly => {
                // First enemy whose hit-disc the bullet's swept
                // segment grazes wins — except Pierce bullets, which
                // skip any enemy they've already hit on a prior frame.
                let already_hit: &[Entity] = pierces
                    .get(be)
                    .map(|p| p.hit_targets.as_slice())
                    .unwrap_or(&[]);
                if let Some(&(ee, ep, _)) = enemy_snap.iter().find(|(e, ep, hd)| {
                    if already_hit.contains(e) { return false; }
                    point_segment_dist_sq(*ep, prev_bp, bp) < *hd * *hd
                }) {
                    // Pierce branch: scale damage by `falloff^used`,
                    // record the hit, decide whether the bullet
                    // survives. Despawn only when out of pierces.
                    let pierce_damage = if let Ok(mut p) = pierces.get_mut(be) {
                        let scaled = (b.damage as f32 * p.falloff.powi(p.used as i32)).round() as i32;
                        p.hit_targets.push(ee);
                        p.used = p.used.saturating_add(1);
                        if p.used > p.max {
                            commands.entity(be).despawn();
                        }
                        scaled.max(1)
                    } else {
                        commands.entity(be).despawn();
                        b.damage
                    };
                    // Cannonball knockback: insert a `Knockedback`
                    // component on the struck enemy. `apply_velocity`
                    // composes this on top of the AI's per-frame
                    // `Velocity` (which would otherwise overwrite a
                    // direct velocity nudge every tick), and decays it
                    // out over time. Mutating `Velocity` directly
                    // here was the previous approach and didn't work
                    // — enemy AI re-clamps `Velocity` each frame.
                    if let Some(kb) = kb {
                        let dir = bv.0.try_normalize().unwrap_or(Vec2::Y);
                        commands.entity(ee).insert(crate::components::Knockedback {
                            velocity: dir * kb.force,
                            decay_per_sec: crate::cannon::CANNONBALL_KNOCKBACK_DECAY,
                        });
                    }
                    if harpoon_tip.is_some() {
                        // Attach a tether + spawn the chain visual. The
                        // tether's source is the player ship — taken
                        // from the same `friendly` query the enemy-bullet
                        // path uses below. Bosses (Enemy + Ally) break
                        // free in 1s instead of the standard 4s.
                        let is_boss = allies.get(ee).is_ok();
                        if let Ok((fe, _, _, _, _, _)) = friendly.single() {
                            crate::harpoon::attach_harpoon(
                                &mut commands, &em, &pm, fe, ee, is_boss,
                            );
                        }
                    }
                    // Crits only roll for player-sourced bullets — ally
                    // damage uses its baseline number for share-bar
                    // accounting (and so allies don't piggyback on the
                    // player's crit stat).
                    let crit_mult = if matches!(b.source, Some(DamageSource::PlayerSlot(_))) {
                        player_stats.roll_crit_mult(&mut rng) as i32
                    } else {
                        1
                    };
                    queue.push_initial(
                        ee,
                        pierce_damage.saturating_mul(crit_mult),
                        ep,
                        b.weapon,
                        b.source,
                        &b.runes,
                    );
                }
            }
            FactionKind::Enemy => {
                // Enemy bullets carry no runes — keep the original inline
                // hit logic and skip the proc queue. Difficulty scales the
                // incoming damage at the entry point (before Spike Plate
                // chip-down + shield absorption) so the multiplier
                // compounds with the player's mitigations.
                let incoming = difficulty.scale_damage(b.damage);
                let mut consumed = false;
                for (_fe, ftf, mut h, mut fx, shield_opt, fheading) in &mut friendly {
                    let fp = ftf.translation.truncate();
                    // Swept segment vs ship hit radius (5).
                    if point_segment_dist_sq(fp, prev_bp, bp) < 5.0 * 5.0 {
                        commands.entity(be).despawn();
                        // Spike Plate per-side reduction: find which
                        // slot's mount angle is closest to the impact
                        // direction and chip 1 off the incoming damage
                        // if that slot holds a Spike Plate. This is
                        // pre-shield so an incoming hit absorbed by
                        // the shield still gets the chip-down (matches
                        // the "deflected by spikes before reaching the
                        // shield" mental model).
                        let dmg = spiked_plate_reduction(&cfg, fp, bp, fheading.0, incoming);
                        let after_shield = shield_opt
                            .map(|mut s| s.absorb(dmg))
                            .unwrap_or(dmg);
                        apply_damage(&mut h, &mut fx, after_shield);
                        let hit_pos = ftf.translation.truncate();
                        spawn_hit_particles(&mut commands, &em, &pm.bullet_enemy, hit_pos, 5, 50.0, &mut rng);
                        // Only fire SFX if damage actually landed
                        // (shield-absorbed shots stay silent).
                        if after_shield > 0 {
                            sfx.play(crate::sfx::Sfx::PlayerHit);
                        }
                        consumed = true;
                        break;
                    }
                }
                if consumed { continue; }
                for (_ae, atf, ally, mut h, mut fx) in &mut allies {
                    // Submerged allies (subs) are invisible to normal
                    // enemy bullets — they pass right through. Stealth is
                    // the sub's identity, so this is enforced at the
                    // collision check, not just at target-selection.
                    if ally_is_submerged(ally) { continue; }
                    let hit_d = ally_hit_radius(ally);
                    let ap = atf.translation.truncate();
                    if point_segment_dist_sq(ap, prev_bp, bp) < hit_d * hit_d {
                        commands.entity(be).despawn();
                        apply_damage(&mut h, &mut fx, incoming);
                        let hit_pos = atf.translation.truncate();
                        spawn_hit_particles(&mut commands, &em, &pm.bullet_enemy, hit_pos, 5, 50.0, &mut rng);
                        break;
                    }
                }
            }
        }
    }

    // Drain happens elsewhere — `process_damage_events` runs after
    // every damage producer (this system, blade tick, mortar splash,
    // beam pierce, etc.) and processes the queue uniformly so every
    // weapon's runes get a turn through `process_damage_event`.
}

/// Drain `PendingDamageQueue` — runs ONCE per frame after every damage
/// producer (bullet collisions, blade tick, octopus tentacle slap,
/// mortar splash, beam pierce, ...) so every weapon's runes flow
/// through the same `process_damage_event` pipeline. Chained events
/// (Shock / Cascade) get pushed back onto the queue and processed
/// in the same drain loop, so chains resolve same-frame.
pub fn process_damage_events(
    mut commands: Commands,
    mut queue: ResMut<PendingDamageQueue>,
    mut stats: ResMut<DamageStats>,
    player_stats: Res<crate::stats::PlayerStats>,
    synergies: Res<crate::synergy::Synergies>,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    mut vampire_acc: ResMut<VampireAccumulator>,
    mut greed_acc: ResMut<GreedAccumulator>,
    mut scrap_w: crate::stage_complete::ScrapWriter,
    mut on_kill: crate::rune::OnKillBookkeeping,
    mut friendly: Query<
        (&mut Health, Option<&mut crate::stats::Shield>),
        (With<Friendly>, Without<Enemy>),
    >,
    mut enemies: Query<
        (Entity, &Transform, &Enemy, &mut Health, &mut HitFx, &mut Velocity),
        (With<Enemy>, Without<Friendly>),
    >,
    on_conduit: Query<&OnConduit>,
    on_resonate: Query<&OnResonate>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    if queue.0.is_empty() { return; }
    let mut rng = rand::thread_rng();
    // Future synergy: weapons carrying `WeaponTag::Future` stun their
    // target for this many seconds per hit. 0 when no Future tiers
    // are active so the per-event check below is a free branch.
    let future_stun = synergies.future_stun_duration();

    // Snapshot for chain-target picking (Shock / Cascade). Built from
    // the mutable enemies query's read-only iteration before any
    // mutation runs.
    let enemy_snap: Vec<(Entity, Vec2, f32)> = enemies
        .iter()
        .map(|(e, tf, en, _, _, _)| (e, tf.translation.truncate(), 3.5 * en.variant.scale()))
        .collect();

    // Drain. `process_damage_event` may push chain events back onto
    // `queue.0` (Shock fans out, Cascade fires on lethal). Bounded
    // by `procced` accumulating per chain hop.
    while let Some(ev) = queue.0.pop() {
        // Snapshot the fields the post-process needs (target, source,
        // rune list) before `ev` is consumed by the inner call. The
        // rune list is cloned for the `KillEvent` broadcast; the
        // bookkeeping for Thirst / Rally reuses the same source data.
        let target = ev.target;
        let source = ev.source;
        let event_runes = ev.runes.clone();

        process_damage_event(
            ev, &mut queue.0,
            &mut commands, &mut stats, &player_stats, &pm, &em,
            future_stun,
            &enemy_snap, &mut enemies, &mut friendly, &mut vampire_acc,
            &mut greed_acc, &mut *scrap_w.total, &mut *scrap_w.earned,
            &on_conduit, &on_resonate, &mut rng,
        );

        // Was the hit lethal? Read the target's HP back via the
        // mutable query — entity is still alive (despawn happens
        // next frame in `enemy_death_check`), so `.get` succeeds and
        // HP <= 0 marks the kill.
        let died = enemies
            .get(target)
            .map(|(_, _, _, h, _, _)| h.0 <= 0)
            .unwrap_or(false);
        if !died { continue; }

        let source_slot = match source {
            Some(crate::bullet::DamageSource::PlayerSlot(idx)) => Some(idx),
            _ => None,
        };

        // Broadcast a single KillEvent. Every on-kill subscriber
        // (Star / Leftovers / future runes / future achievements)
        // reads this stream — no per-rune marker components.
        on_kill.kill_writer.write(crate::rune::KillEvent {
            target,
            source_slot,
            runes: event_runes.clone(),
        });

        // Slot-bound on-kill effects (Thirst / Rally) still need
        // their resource writes here because they affect future-
        // frame behaviour (next-shot bonus / move-speed stacks).
        // Once `RuneEffect::on_kill` lands these can move out and
        // dispatch through the same event stream.
        let Some(idx) = source_slot else { continue };
        let slot_idx = idx as usize;
        let thirst_stacks = event_runes
            .iter()
            .filter(|&&r| r == Rune::Thirst)
            .count() as u8;
        let rally_stacks = event_runes
            .iter()
            .filter(|&&r| r == Rune::Rally)
            .count() as u8;
        if thirst_stacks > 0 {
            on_kill.thirst.set(slot_idx, thirst_stacks);
        }
        if rally_stacks > 0 {
            // Rally requires the firing slot to carry the `Melee`
            // tag — kills from non-Melee weapons don't grant stacks
            // even if the slot is socketed with Rally runes.
            let melee = on_kill
                .cfg
                .slots
                .get(slot_idx)
                .map(|s| s.weapon.tags().iter().any(|t| matches!(t, crate::weapon::WeaponTag::Melee)))
                .unwrap_or(false);
            if melee {
                for _ in 0..rally_stacks {
                    on_kill.buffs.push(
                        crate::rune::BuffId::Rally,
                        crate::rune::RALLY_DURATION,
                    );
                }
            }
        }
    }
}

fn process_damage_event(
    ev: DamageEvent,
    chain: &mut Vec<DamageEvent>,
    commands: &mut Commands,
    stats: &mut DamageStats,
    player_stats: &crate::stats::PlayerStats,
    pm: &PaletteMaterials,
    em: &EffectMeshes,
    future_stun: f32,
    enemy_snap: &[(Entity, Vec2, f32)],
    enemies: &mut Query<(Entity, &Transform, &Enemy, &mut Health, &mut HitFx, &mut Velocity), (With<Enemy>, Without<Friendly>)>,
    friendly: &mut Query<(&mut Health, Option<&mut crate::stats::Shield>), (With<Friendly>, Without<Enemy>)>,
    vampire_acc: &mut VampireAccumulator,
    greed_acc: &mut GreedAccumulator,
    scrap: &mut crate::Scrap,
    scrap_earned: &mut crate::stage_complete::ScrapEarnedThisStage,
    on_conduit: &Query<&OnConduit>,
    on_resonate: &Query<&OnResonate>,
    rng: &mut rand::rngs::ThreadRng,
) {
    let Ok((_, _, en, mut h, mut fx, _)) = enemies.get_mut(ev.target) else { return; };
    if h.0 <= 0 { return; } // already dead from an earlier hit this frame
    let max_hp_pre = en.max_hp;
    let hp_pre = h.0;

    // Future synergy stun — apply BEFORE damage so a lethal hit still
    // briefly freezes a corpse that's about to despawn (cheap; the
    // entity is removed next frame regardless). Only Future-tagged
    // weapons proc the stun, and only when at least one Future
    // synergy tier is active (`future_stun > 0`).
    if future_stun > 0.0 && ev.weapon.tags().contains(&crate::weapon::WeaponTag::Future) {
        commands
            .entity(ev.target)
            .insert(crate::components::Stunned { remaining: future_stun });
    }

    // Resonate damage amplifier — cross-slot debuff that boosts every
    // damage source on this target. Read once per event so all damage
    // applied below uses the same multiplier.
    let resonate_mult = resonate_multiplier(on_resonate.get(ev.target).ok());

    // Executioner + Opener: conditional damage multipliers folded into
    // the primary hit before Resonate and before `apply_damage`. Both
    // scale linearly with stacks AND with Rune Effect, so the player's
    // rune-damage stat improves these on top of stacking.
    let rune_eff = player_stats.rune_damage_mult();
    let exec_stacks = ev.runes.iter().filter(|&&r| r == Rune::Executioner).count() as f32;
    let opener_stacks = ev.runes.iter().filter(|&&r| r == Rune::Opener).count() as f32;
    let mut conditional_mult = 1.0_f32;
    if exec_stacks > 0.0 {
        let frac = hp_pre as f32 / max_hp_pre.max(1) as f32;
        if frac <= crate::balance::EXECUTIONER_HP_THRESHOLD {
            conditional_mult += crate::balance::EXECUTIONER_BONUS_PER_STACK
                * exec_stacks
                * rune_eff;
        }
    }
    if opener_stacks > 0.0 && hp_pre >= max_hp_pre {
        conditional_mult += crate::balance::OPENER_BONUS_PER_STACK
            * opener_stacks
            * rune_eff;
    }

    let amplify = |dmg: i32| -> i32 {
        (dmg as f32 * resonate_mult * conditional_mult).round() as i32
    };

    let dealt = apply_damage(&mut h, &mut fx, amplify(ev.amount));
    credit_damage(stats, ev.source, dealt);

    // Vampire heal-on-hit (fires regardless of proc roll — no proc
    // strength check). Per-hit fraction = stacks × rune_effect / 10.
    // Accumulates globally; every time it crosses 1.0 the player
    // gains 1 HP. With 1 Vampire and base Rune Effect (1.0), that's
    // ~1 HP per 10 hits — small but constant; high stacks + Rune
    // Effect scaling turns it into meaningful sustain.
    let vampire_stacks = ev.runes.iter().filter(|&&r| r == Rune::Vampire).count();
    if vampire_stacks > 0 {
        let frac = vampire_stacks as f32 * player_stats.rune_damage_mult() / 10.0;
        vampire_acc.0 += frac;
        if vampire_acc.0 >= 1.0 {
            let whole = vampire_acc.0.floor() as i32;
            vampire_acc.0 -= whole as f32;
            let hp_max = player_stats.hp.effective().round() as i32;
            for (mut ph, _) in friendly.iter_mut() {
                if ph.0 < hp_max {
                    ph.0 = (ph.0 + whole).min(hp_max);
                }
            }
        }
    }

    // Weapon-color flair burst (more on lethal). The generic enemy-color
    // destruction burst is added by `enemy_death_check` once HP hits zero.
    let spark_mat = pm.bullet_inner_for(ev.weapon);
    let count = if h.0 <= 0 { 6 } else { 4 };
    let speed = if h.0 <= 0 { 75.0 } else { 45.0 };
    spawn_hit_particles(commands, em, spark_mat, ev.hit_pos, count, speed, rng);

    // Blast on-impact AOE — turns any bullet into a mini explosion.
    // Fires on every hit (no proc roll). Guarded by `procced` so a
    // splash event can't re-trigger Blast on its own splash victims
    // (infinite recursion). Splash damage = bullet damage × frac;
    // splash victims receive NO further rune procs (events pushed
    // with empty `runes`) so a heavy-rune bullet doesn't pepper
    // half the screen with Fire/Frost/Shock.
    //
    // Stacking is internal: more Blast runes = more radius (linear
    // with stack count). Player-facing tooltip just calls it [AOE],
    // hiding the stack mechanic so the rune reads as a transformation
    // rather than a numeric upgrade.
    //
    // Splash rune synergy: each Splash rune on the same slot widens
    // the radius by +50% × rune_effect (identical formula to Splash's
    // effect on Mortar), so Splash now actually does something on
    // non-Mortar weapons when paired with Blast.
    let blast_stacks = ev.runes.iter().filter(|&&r| r == Rune::Blast).count() as f32;
    if blast_stacks > 0.0 && !ev.procced.contains(&Rune::Blast) {
        let splash_stacks = ev.runes.iter().filter(|&&r| r == Rune::Splash).count() as f32;
        let rune_effect = player_stats.rune_damage_mult();
        let splash_mult = 1.0 + 0.5 * splash_stacks * rune_effect;
        let radius = blast_stacks
            * crate::balance::BLAST_RADIUS
            * rune_effect
            * splash_mult;
        let splash_dmg = (ev.amount as f32 * crate::balance::BLAST_SPLASH_FRAC)
            .round()
            .max(1.0) as i32;
        let mut next_procced = ev.procced.clone();
        next_procced.push(Rune::Blast);
        for &(e, ep, er) in enemy_snap.iter() {
            if e == ev.target { continue; }
            let reach = radius + er;
            if ep.distance_squared(ev.hit_pos) > reach * reach { continue; }
            chain.push(DamageEvent {
                target: e,
                amount: splash_dmg,
                hit_pos: ep,
                weapon: ev.weapon,
                source: ev.source,
                runes: Vec::new(),
                procced: next_procced.clone(),
                proc_strength: 0.0,
            });
        }
        // Visible flair sized to the actual splash radius. A
        // single-point hit-particle burst (the previous version)
        // scattered randomly regardless of how big the splash
        // really was, so the player couldn't tell the radius from
        // the visual. Instead we drop particles in a ring at the
        // resolved radius and use a fixed palette orange so the
        // Blast AOE reads as a distinct "explosive" cue independent
        // of the host weapon's bullet colour.
        spawn_blast_ring(commands, em, &pm.blast, ev.hit_pos, radius, rng);
    }

    // Lethal-only branch: Cascade is the one rune that fires *because*
    // the target died. Other runes don't fan out from a kill (saves a
    // frame of FX on something already despawning).
    //
    // Kill-credit rune bookkeeping (Star / Leftovers / Thirst / Rally)
    // is broadcast as a `KillEvent` by the wrapper system
    // `process_damage_events` AFTER this returns — that way every
    // on-kill subscriber (current and future) reads one event stream
    // instead of each rune adding a marker component on the dying
    // entity. See `crate::rune::KillEvent`.
    if h.0 <= 0 {
        // Greed scrap-on-Nth-kill (fires regardless of proc roll).
        // Stacks reduce the threshold (more frequent payouts), Rune
        // Effect divides the remainder. Increment runs once per kill
        // regardless of how many Greed stacks the bullet has — the
        // payout *cadence* is what stacking buys, not extra scrap
        // per kill.
        let greed_stacks = ev.runes.iter().filter(|&&r| r == Rune::Greed).count() as u32;
        if greed_stacks > 0 {
            greed_acc.0 = greed_acc.0.saturating_add(1);
            let raw = crate::balance::GREED_BASE_KILLS
                .saturating_sub(greed_stacks * crate::balance::GREED_KILLS_PER_STACK);
            let eff = (raw as f32 / player_stats.rune_damage_mult().max(0.01))
                .ceil() as u32;
            let needed = eff.max(1);
            while greed_acc.0 >= needed {
                greed_acc.0 -= needed;
                scrap.0 = scrap.0.saturating_add(1);
                scrap_earned.0 = scrap_earned.0.saturating_add(1);
            }
        }

        // Ward shield-on-kill (fires regardless of proc roll). Each
        // Ward stack on the killing bullet adds `rune_effect` shield,
        // allowed to push the current shield ABOVE `shield_max` as a
        // one-time overflow buffer. Natural shield regen never refills
        // above max, so the overflow only decays by absorbing hits.
        let ward_stacks = ev.runes.iter().filter(|&&r| r == Rune::Ward).count();
        if ward_stacks > 0 {
            let gain = ward_stacks as f32 * player_stats.rune_damage_mult();
            for (_, sh_opt) in friendly.iter_mut() {
                if let Some(mut sh) = sh_opt {
                    sh.current += gain;
                }
            }
        }

        if ev.proc_strength <= 0.0 { return; }
        // Count Cascade stacks on this bullet — each stack hits one
        // additional nearest enemy. With 3 Cascade runes a kill
        // fans out to the 3 nearest unique enemies (one chain
        // event each).
        let cascade_stacks = ev.runes.iter().filter(|&&r| r == Rune::Cascade).count();
        if cascade_stacks == 0 { return; }
        if rng.gen::<f32>() >= ev.proc_strength { return; }

        let r2 = CASCADE_RANGE * CASCADE_RANGE;
        let mut excluded: Vec<Entity> = vec![ev.target];
        for _ in 0..cascade_stacks {
            let next = enemy_snap
                .iter()
                .filter(|(e, _, _)| !excluded.contains(e))
                .map(|&(e, p, _)| (e, p, p.distance_squared(ev.hit_pos)))
                .filter(|(_, _, d2)| *d2 <= r2)
                .min_by(|a, b| {
                    a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal)
                });
            let Some((target, target_pos, _)) = next else { break };
            excluded.push(target);
            spawn_lightning_arc(commands, em, &pm.bullet_friendly_outer, ev.hit_pos, target_pos);
            chain.push(DamageEvent {
                target,
                amount: ev.amount,
                hit_pos: target_pos,
                weapon: ev.weapon,
                source: None,
                runes: ev.runes.clone(),
                procced: ev.procced.clone(),
                proc_strength: ev.proc_strength * Rune::Cascade.proc_coefficient(),
            });
        }
        return;
    }

    if ev.proc_strength <= 0.0 { return; }

    // Conduit proc-strength buff — if the target carries OnConduit,
    // every rune attached to this hit gets a more reliable proc roll.
    // Capped at 1.0 in the eventual `>=` comparison so the visible
    // effect is "chain hops at full strength" rather than "absurd
    // super-procs on initial hits (already 100%)."
    // Compose the per-roll proc probability:
    //   1. Conduit on target multiplies base proc strength × 1.5
    //   2. PROC stat adds a flat bonus
    //   3. Clamp to 1.0 (you can't exceed certainty)
    //
    // LUCK is applied separately as RoR-style rerolls on failure
    // inside `proc_roll_with_luck`, not folded into this number.
    let bonus = player_stats.proc_strength_bonus();
    // Conduit-on-target proc-strength multiplier scales with the
    // target's Conduit stack count (1 stack = original CONDUIT_PROC_MULT).
    let conduit_mult = on_conduit
        .get(ev.target)
        .map(|c| c.proc_mult(player_stats.rune_damage_mult()))
        .unwrap_or(1.0);
    let strength = (ev.proc_strength * conduit_mult + bonus).min(1.0);

    // Group the bullet's runes by kind first so duplicates (3 Fire,
    // 2 Shock, etc.) get rolled + applied ONCE per kind with the
    // count as a stack value. Without this the loop applies Fire/
    // Frost/Conduit three times in a row via `Commands.insert` —
    // which all overwrite the same component, so the duplicates
    // would be invisible in those branches but stack properly for
    // Shock/Echo. Counting up front makes every rune kind behave
    // consistently with the player's expectation of "3 of X stacks".
    let mut counts: Vec<(Rune, u8)> = Vec::new();
    for &rune in &ev.runes {
        if let Some(entry) = counts.iter_mut().find(|(r, _)| *r == rune) {
            entry.1 += 1;
        } else {
            counts.push((rune, 1));
        }
    }

    for (rune, stacks) in counts {
        if ev.procced.contains(&rune) { continue; }
        if !player_stats.proc_roll_with_luck(&mut *rng, strength) { continue; }
        // Per-rune dispatch lives in `Rune::apply_proc` (rune.rs) so
        // adding a new rune is a single match-arm there, not a fresh
        // arm in this loop. Future on-kill / on-tick dispatch will
        // sit alongside it as parallel methods.
        rune.apply_proc(
            stacks, &ev, chain, commands, player_stats, pm, em,
            on_resonate, enemy_snap, rng,
        );
    }
}

/// Spawn a short-lived **zig-zag** lightning bolt visual between two
/// Visual for the Blast on-impact AOE: a bright central pop plus a
/// burst of dots that fly outward from the centre, reaching the splash
/// `radius` over their lifetime. The player reads the actual reach off
/// the visual instead of guessing from a scatter of motes. Dot count
/// scales with radius so a small splash still reads as an explosion
/// and a wide one isn't over-crowded.
fn spawn_blast_ring(
    commands: &mut Commands,
    em: &EffectMeshes,
    mat: &Handle<ColorMaterial>,
    centre: Vec2,
    radius: f32,
    rng: &mut rand::rngs::ThreadRng,
) {
    use std::f32::consts::TAU;
    if radius <= 0.5 { return; }

    // Soft puff — a single quick flash at the impact, sized about
    // a third of the splash radius. Reads as displaced air rather
    // than fire / explosion.
    {
        let life = 0.08;
        let pop_scale = (radius * 0.35).max(1.5);
        commands.spawn((
            Mesh2d(em.boarder_dot.clone()),
            MeshMaterial2d(mat.clone()),
            Transform {
                translation: Vec3::new(centre.x, centre.y, 5.6),
                scale: Vec3::new(pop_scale, pop_scale, 1.0),
                ..default()
            },
            crate::effects::HitParticle { life, max_life: life, base_scale: pop_scale },
            Velocity(Vec2::ZERO),
            bevy::render::view::RenderLayers::layer(crate::balance::PLAY_LAYER),
        ));
    }

    // A small handful of dots drift outward to the rim. Count
    // intentionally low so the visual reads as a quiet shockwave
    // rather than a particle explosion.
    let count = (radius * 0.6).round().clamp(5.0, 16.0) as u32;
    let base_life = 0.28;
    for i in 0..count {
        let base_a = (i as f32 / count as f32) * TAU;
        let a = base_a + rng.gen_range(-0.30..0.30);
        let dir = Vec2::new(a.cos(), a.sin());
        let life = base_life * rng.gen_range(0.80..1.20);
        let speed = (radius / base_life) * rng.gen_range(2.4..3.2);
        let v = dir * speed;
        let scale = rng.gen_range(0.6..1.0);
        commands.spawn((
            Mesh2d(em.boarder_dot.clone()),
            MeshMaterial2d(mat.clone()),
            Transform {
                translation: Vec3::new(centre.x, centre.y, 5.5),
                scale: Vec3::new(scale, scale, 1.0),
                ..default()
            },
            crate::effects::HitParticle { life, max_life: life, base_scale: scale },
            Velocity(v),
            bevy::render::view::RenderLayers::layer(crate::balance::PLAY_LAYER),
        ));
    }
}

/// world points. Built from `SEGMENTS` straight beam-mesh segments
/// strung between sample points along the line `a → b`; interior
/// points get a random perpendicular jitter so the bolt forks like
/// real lightning instead of a flat ruler-line. Used by both Shock
/// (cyan chain) and Cascade (gold on-kill snowball).
pub(crate) fn spawn_lightning_arc(
    commands: &mut Commands,
    em: &EffectMeshes,
    mat: &Handle<ColorMaterial>,
    a: Vec2,
    b: Vec2,
) {
    let total = b - a;
    let total_len = total.length();
    if total_len < 0.5 { return; }
    let dir  = total / total_len;
    let perp = Vec2::new(-dir.y, dir.x);

    // 5 segments → 4 interior break points. Enough to read as
    // forky without becoming noisy at small scales.
    const SEGMENTS: usize = 5;
    // Perpendicular jitter scales with the total span — a long arc
    // forks more dramatically than a short hop. Clamped so a tiny
    // hop doesn't deflate to a straight line and a huge one doesn't
    // wander off into loops.
    let jitter_amp = (total_len * 0.12).clamp(0.7, 3.5);

    let mut rng = rand::thread_rng();
    let mut points: Vec<Vec2> = Vec::with_capacity(SEGMENTS + 1);
    points.push(a);
    for i in 1..SEGMENTS {
        let t = i as f32 / SEGMENTS as f32;
        let base = a + dir * (total_len * t);
        let off  = rng.gen_range(-jitter_amp..jitter_amp);
        points.push(base + perp * off);
    }
    points.push(b);

    // One beam segment per consecutive pair of points. They share
    // the same `life` so all segments fade together as one bolt.
    for w in points.windows(2) {
        let p0 = w[0];
        let p1 = w[1];
        let seg = p1 - p0;
        let seg_len = seg.length();
        if seg_len < 0.1 { continue; }
        let seg_mid   = (p0 + p1) * 0.5;
        let seg_angle = (-seg.x).atan2(seg.y);
        commands.spawn((
            Mesh2d(em.beam.clone()),
            MeshMaterial2d(mat.clone()),
            Transform {
                translation: Vec3::new(seg_mid.x, seg_mid.y, 5.5),
                rotation: Quat::from_rotation_z(seg_angle),
                // y scales the BEAM_LENGTH-long mesh to the segment length.
                // x is animated by `update_beams` so spawn at 0.
                scale: Vec3::new(0.0, seg_len / BEAM_LENGTH, 1.0),
            },
            Beam { life: SHOCK_VISUAL_LIFE, max_life: SHOCK_VISUAL_LIFE },
            RenderLayers::layer(PLAY_LAYER),
        ));
    }
}
