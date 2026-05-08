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
    BEAM_LENGTH, CASCADE_RANGE, CONDUIT_PROC_MULT, PLAY_LAYER, RESONATE_MAX_STACKS,
    SHOCK_CHAIN_RANGE, SHOCK_VISUAL_LIFE,
};
use crate::beam::Beam;
use crate::components::{FactionKind, Friendly, Health, Velocity};
use crate::effects::{spawn_hit_particles, EffectMeshes, HitFx};
use crate::enemy::Enemy;
use crate::palette::PaletteMaterials;
use crate::rune::{
    apply_rune, detonate_consume, resonate_multiplier, EchoPending, OnConduit, OnFire,
    OnFrost, OnResonate, Rune, ECHO_DELAY,
};
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

#[derive(Component)]
pub struct Bullet {
    pub faction: FactionKind,
    pub damage: i32,
    pub remaining: f32,
    /// Weapon that fired this bullet — drives spark/impact colors on hit.
    /// Unused for enemy bullets (always `Standard`).
    pub weapon: WeaponType,
    /// Originating turret slot index (0-7) for friendly bullets. None for
    /// enemy bullets — they don't contribute to the slot damage tally.
    pub slot: Option<u8>,
    /// Rune carried by the bullet (inherited from the firing slot's `rune`).
    /// On hit, the rune is rolled through the proc system and may trigger
    /// status applies / chain damage. Always `None` for enemy bullets.
    pub rune: Option<Rune>,
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

/// One unit of pending damage + rune resolution. Pushed onto a queue inside
/// `bullet_collisions` (initial bullet hits and any subsequent shock
/// chains). The drain loop processes events one at a time.
struct DamageEvent {
    target: Entity,
    amount: i32,
    hit_pos: Vec2,
    weapon: WeaponType,
    /// Slot index for `DamageStats` crediting. `None` for chain hops so the
    /// secondary damage doesn't inflate the originating slot's share.
    source_slot: Option<u8>,
    /// All runes attached to the source bullet — each one may proc on this
    /// hit (gated by `procced` + `proc_strength`).
    runes: Vec<Rune>,
    /// Runes already triggered upstream in this chain — preventing the same
    /// effect from firing twice within one chain. Risk-of-Rain-style.
    procced: Vec<Rune>,
    /// Strength multiplier for proc rolls on this hit. Initial bullet hits
    /// are `1.0`; secondary hits inherit `parent_strength * rune.coeff`.
    proc_strength: f32,
}

pub fn bullet_collisions(
    mut commands: Commands,
    mut stats: ResMut<DamageStats>,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    bullets: Query<(Entity, &Transform, &Bullet)>,
    mut enemies: Query<(Entity, &Transform, &Enemy, &mut Health, &mut HitFx), (With<Enemy>, Without<Friendly>, Without<Ally>)>,
    mut friendly: Query<(Entity, &Transform, &mut Health, &mut HitFx), (With<Friendly>, Without<Enemy>, Without<Ally>)>,
    mut allies: Query<(Entity, &Transform, &Ally, &mut Health, &mut HitFx), (With<Ally>, Without<Enemy>, Without<Friendly>)>,
    on_fire: Query<&OnFire>,
    on_frost: Query<&OnFrost>,
    on_conduit: Query<&OnConduit>,
    on_resonate: Query<&OnResonate>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let mut rng = rand::thread_rng();

    // Snapshot enemies (entity, position, hit-radius) once. Used both for
    // bullet hit detection and for shock-chain target picking — keeps the
    // mutable `enemies` query free until we need `get_mut` during the drain.
    let enemy_snap: Vec<(Entity, Vec2, f32)> = enemies
        .iter()
        .map(|(e, tf, en, _, _)| (e, tf.translation.truncate(), 3.5 * en.variant.scale()))
        .collect();

    let mut chain: Vec<DamageEvent> = Vec::new();

    for (be, btf, b) in &bullets {
        let bp = btf.translation.truncate();
        match b.faction {
            FactionKind::Friendly => {
                // First overlapping enemy from the snapshot wins.
                if let Some(&(ee, ep, _)) =
                    enemy_snap.iter().find(|(_, ep, hd)| ep.distance(bp) < *hd)
                {
                    commands.entity(be).despawn();
                    chain.push(DamageEvent {
                        target: ee,
                        amount: b.damage,
                        hit_pos: ep,
                        weapon: b.weapon,
                        source_slot: b.slot,
                        runes: b.rune.into_iter().collect(),
                        procced: Vec::new(),
                        proc_strength: 1.0,
                    });
                }
            }
            FactionKind::Enemy => {
                // Enemy bullets carry no runes — keep the original inline
                // hit logic and skip the proc queue.
                let mut consumed = false;
                for (_fe, ftf, mut h, mut fx) in &mut friendly {
                    if ftf.translation.truncate().distance(bp) < 5.0 {
                        commands.entity(be).despawn();
                        // Friendly ship now takes damage in both modes.
                        // Sandbox invincibility was useful while the
                        // map / capture loop was being designed; with
                        // ally HP also live, parity makes the sandbox
                        // feel coherent.
                        apply_damage(&mut h, &mut fx, b.damage);
                        let hit_pos = ftf.translation.truncate();
                        spawn_hit_particles(&mut commands, &em, &pm.bullet_enemy, hit_pos, 5, 50.0, &mut rng);
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
                    if atf.translation.truncate().distance(bp) < hit_d {
                        commands.entity(be).despawn();
                        apply_damage(&mut h, &mut fx, b.damage);
                        let hit_pos = atf.translation.truncate();
                        spawn_hit_particles(&mut commands, &em, &pm.bullet_enemy, hit_pos, 5, 50.0, &mut rng);
                        break;
                    }
                }
            }
        }
    }

    // Drain the proc chain. Bounded — every shock chain consumes a slot in
    // `procced`, and `runes` is finite, so the queue can't loop forever.
    while let Some(ev) = chain.pop() {
        process_damage_event(
            ev, &mut chain,
            &mut commands, &mut stats, &pm, &em,
            &enemy_snap, &mut enemies, &on_fire, &on_frost,
            &on_conduit, &on_resonate, &mut rng,
        );
    }
}

fn process_damage_event(
    ev: DamageEvent,
    chain: &mut Vec<DamageEvent>,
    commands: &mut Commands,
    stats: &mut DamageStats,
    pm: &PaletteMaterials,
    em: &EffectMeshes,
    enemy_snap: &[(Entity, Vec2, f32)],
    enemies: &mut Query<(Entity, &Transform, &Enemy, &mut Health, &mut HitFx), (With<Enemy>, Without<Friendly>, Without<Ally>)>,
    on_fire: &Query<&OnFire>,
    on_frost: &Query<&OnFrost>,
    on_conduit: &Query<&OnConduit>,
    on_resonate: &Query<&OnResonate>,
    rng: &mut rand::rngs::ThreadRng,
) {
    let Ok((_, _, _, mut h, mut fx)) = enemies.get_mut(ev.target) else { return; };
    if h.0 <= 0 { return; } // already dead from an earlier hit this frame

    // Resonate damage amplifier — cross-slot debuff that boosts every
    // damage source on this target. Read once per event so all damage
    // applied below (initial + Detonate burst) uses the same multiplier.
    let resonate_mult = resonate_multiplier(on_resonate.get(ev.target).ok());
    let amplify = |dmg: i32| -> i32 {
        (dmg as f32 * resonate_mult).round() as i32
    };

    let dealt = apply_damage(&mut h, &mut fx, amplify(ev.amount));
    if let Some(s) = ev.source_slot {
        stats.per_slot[s as usize] += dealt as u64;
        stats.total            += dealt as u64;
    }

    // Weapon-color flair burst (more on lethal). The generic enemy-color
    // destruction burst is added by `enemy_death_check` once HP hits zero.
    let spark_mat = pm.bullet_inner_for(ev.weapon);
    let count = if h.0 <= 0 { 6 } else { 4 };
    let speed = if h.0 <= 0 { 75.0 } else { 45.0 };
    spawn_hit_particles(commands, em, spark_mat, ev.hit_pos, count, speed, rng);

    // Lethal-only branch: Cascade is the one rune that fires *because*
    // the target died. Other runes don't fan out from a kill (saves a
    // frame of FX on something already despawning).
    if h.0 <= 0 {
        if ev.proc_strength <= 0.0 { return; }
        for &rune in &ev.runes {
            if rune != Rune::Cascade { continue; }
            // Cascade is intentionally NOT added to `procced` —
            // proc_strength decay (× 0.7 per hop) is what eventually
            // caps the snowball, not a one-hop limit.
            if rng.gen::<f32>() >= ev.proc_strength { continue; }

            let r2 = CASCADE_RANGE * CASCADE_RANGE;
            let next = enemy_snap
                .iter()
                .filter(|(e, _, _)| *e != ev.target)
                .map(|&(e, p, _)| (e, p, p.distance_squared(ev.hit_pos)))
                .filter(|(_, _, d2)| *d2 <= r2)
                .min_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));

            if let Some((target, target_pos, _)) = next {
                // Visual trail in the friendly-bullet color so a
                // cascade reads as "your kill snowballing forward",
                // distinct from Shock's cyan arc.
                spawn_lightning_arc(commands, em, &pm.bullet_friendly_outer, ev.hit_pos, target_pos);
                chain.push(DamageEvent {
                    target,
                    amount: ev.amount,
                    hit_pos: target_pos,
                    weapon: ev.weapon,
                    source_slot: None, // chained damage doesn't credit the slot
                    runes: ev.runes.clone(),
                    procced: ev.procced.clone(),
                    proc_strength: ev.proc_strength * Rune::Cascade.proc_coefficient(),
                });
            }
        }
        return;
    }

    if ev.proc_strength <= 0.0 { return; }

    // Conduit proc-strength buff — if the target carries OnConduit,
    // every rune attached to this hit gets a more reliable proc roll.
    // Capped at 1.0 in the eventual `>=` comparison so the visible
    // effect is "chain hops at full strength" rather than "absurd
    // super-procs on initial hits (already 100%)."
    let effective_strength = if on_conduit.get(ev.target).is_ok() {
        (ev.proc_strength * CONDUIT_PROC_MULT).min(1.0)
    } else {
        ev.proc_strength
    };

    // Roll each rune attached to the source bullet. `proc_strength` gates
    // the chance: initial hits at 1.0 always pass, chain hits at 0.5 are
    // 50/50, and the procced list ensures each rune fires at most once
    // per chain.
    for &rune in &ev.runes {
        if ev.procced.contains(&rune) { continue; }
        if rng.gen::<f32>() >= effective_strength { continue; }

        match rune {
            Rune::Fire | Rune::Frost => {
                apply_rune(commands, ev.target, rune);
            }
            Rune::Shock => {
                // Closest other enemy in `SHOCK_CHAIN_RANGE` of the hit pos.
                let r2 = SHOCK_CHAIN_RANGE * SHOCK_CHAIN_RANGE;
                let chain_target = enemy_snap
                    .iter()
                    .filter(|(e, _, _)| *e != ev.target)
                    .map(|&(e, p, _)| (e, p, p.distance_squared(ev.hit_pos)))
                    .filter(|(_, _, d2)| *d2 <= r2)
                    .min_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));

                if let Some((target, target_pos, _)) = chain_target {
                    spawn_lightning_arc(commands, em, &pm.shock, ev.hit_pos, target_pos);
                    let mut next_procced = ev.procced.clone();
                    next_procced.push(Rune::Shock);
                    chain.push(DamageEvent {
                        target,
                        amount: ev.amount, // shock chain = 100% weapon damage
                        hit_pos: target_pos,
                        weapon: ev.weapon,
                        // Don't credit chain damage back to the firing slot —
                        // keeps share bars representing "your turret killed it"
                        // rather than "your turret rune chained it elsewhere".
                        source_slot: None,
                        runes: ev.runes.clone(),
                        procced: next_procced,
                        proc_strength: ev.proc_strength * Rune::Shock.proc_coefficient(),
                    });
                }
            }
            Rune::Detonate => {
                // Pop any primer status (Fire/Frost) on the target. The
                // burst is itself routed through `apply_damage` so it
                // pulses HitFx and counts toward the firing slot's
                // share — Detonate is *your* damage, not a chain hop.
                let fire_ref = on_fire.get(ev.target).ok();
                let frost_ref = on_frost.get(ev.target).ok();
                let burst = detonate_consume(commands, ev.target, fire_ref, frost_ref);
                if burst > 0 {
                    let dealt = apply_damage(&mut h, &mut fx, amplify(burst));
                    if let Some(s) = ev.source_slot {
                        stats.per_slot[s as usize] += dealt as u64;
                        stats.total            += dealt as u64;
                    }
                    // Two-tone flair: weapon spark + a flame puff so
                    // the consumed status reads as "popped".
                    spawn_hit_particles(commands, em, spark_mat, ev.hit_pos, 8, 90.0, rng);
                    spawn_hit_particles(commands, em, &pm.fire,  ev.hit_pos, 6, 70.0, rng);
                }
            }
            Rune::Echo => {
                // Schedule a delayed re-damage event on the same target.
                // Spawned as a free-standing entity so its lifetime is
                // independent of the bullet (which has already despawned).
                commands.spawn(EchoPending {
                    timer: ECHO_DELAY,
                    target: ev.target,
                    damage: ev.amount,
                    source_slot: ev.source_slot,
                    weapon: ev.weapon,
                });
            }
            Rune::Cascade => {
                // Handled in the lethal branch above — skip here.
            }
            Rune::Conduit => {
                apply_rune(commands, ev.target, Rune::Conduit);
                spawn_hit_particles(commands, em, &pm.shock, ev.hit_pos, 4, 35.0, rng);
            }
            Rune::Resonate => {
                // Stack-aware insert: read current stacks, increment,
                // refresh decay timer. Cap at `RESONATE_MAX_STACKS` so
                // a single target can't be wound up to ridiculous
                // damage by a sustained-fire weapon.
                let current = on_resonate.get(ev.target).map(|r| r.stacks).unwrap_or(0);
                let new_stacks = (current + 1).min(RESONATE_MAX_STACKS);
                commands.entity(ev.target).insert(OnResonate::new(new_stacks));
                // Sniper-pink flair so a Resonate stack reads as a
                // distinct on-hit beat without conflicting with the
                // weapon-color spark.
                spawn_hit_particles(commands, em, &pm.bullet_sniper, ev.hit_pos, 3, 30.0, rng);
            }
        }
    }
}

/// Spawn a short-lived lightning bolt visual between two world points.
/// Reuses the railgun beam mesh + `Beam` lifetime/width animator — the only
/// per-instance differences are color (`mat`) and length (scaled).
fn spawn_lightning_arc(
    commands: &mut Commands,
    em: &EffectMeshes,
    mat: &Handle<ColorMaterial>,
    a: Vec2,
    b: Vec2,
) {
    let delta = b - a;
    let len = delta.length();
    if len < 0.5 { return; }
    let mid = (a + b) * 0.5;
    let angle = (-delta.x).atan2(delta.y); // 0 = +Y convention
    commands.spawn((
        Mesh2d(em.beam.clone()),
        MeshMaterial2d(mat.clone()),
        Transform {
            translation: Vec3::new(mid.x, mid.y, 5.5),
            rotation: Quat::from_rotation_z(angle),
            // y scales the BEAM_LENGTH-long mesh down to actual arc length.
            // x is animated by `update_beams` so spawn at 0 to start invisible.
            scale: Vec3::new(0.0, len / BEAM_LENGTH, 1.0),
        },
        Beam { life: SHOCK_VISUAL_LIFE, max_life: SHOCK_VISUAL_LIFE },
        RenderLayers::layer(PLAY_LAYER),
    ));
}
