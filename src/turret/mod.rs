//! Friendly-ship turrets: per-slot configuration, target acquisition, aiming,
//! and weapon-specific firing (single bullet / shotgun spread / railgun beam).
//!
//! Each turret slot has a configurable `WeaponType`. Per-weapon firing logic
//! branches on `slot.weapon` in `turret_aim_fire`; adding a weapon means a new
//! arm there plus the weapon-stats / material rows in `weapon.rs` / `palette.rs`.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;
use rand::Rng;

use crate::balance::{
    BARREL_LATERAL, BARREL_MIDDLE_EXTEND, BEAM_LENGTH, BEAM_LIFETIME, BULLET_SPEED,
    FRIENDLY_BARREL_TIP, FRIENDLY_BULLET_HALF_LEN, PLAY_LAYER, SHOTGUN_PELLETS, SHOTGUN_SPREAD,
    TURRET_ARC_HALVES, TURRET_RANGE,
};
use crate::beam::{Beam, BeamHit, BeamPending};
use crate::bullet::{Bullet, DamageSource};
use crate::components::{Faction, FactionKind, Friendly, Health, Heading, Velocity};
use crate::effects::{EffectMeshes, MuzzleFlash};
use crate::enemy::Enemy;
use crate::palette::PaletteMaterials;
use crate::rune::Rune;
use crate::ship::approach_angle;
use crate::weapon::WeaponType;

// Submodules — HeliPad helicopter behaviour and mortar arc/AOE live
// in their own files so this module stays focused on the per-slot
// aim/fire path. SharkNet sits beside them — another autonomous-unit
// pattern that owns its own world entities + ticks.
pub mod decor;
pub mod heli;
pub mod mortar;
pub mod sharknet;

pub use decor::{
    sync_amplifier_decor, sync_crows_nest_decor, sync_flamethrower_decor,
    sync_sharknet_decor, sync_spiked_decor,
};
pub use heli::{
    helicopter_ai, sync_helipad_helicopters, sync_helipad_nose_barrels,
};
pub use mortar::{
    mortar_shell_tick, spawn_mortar_shell, MORTAR_SPLASH_RADIUS,
};
pub use sharknet::{shark_ai, shark_contact_damage, sync_sharknet_sharks};

// ---------- Components & resources ----------

#[derive(Component)]
pub struct TurretSlot {
    pub index: usize,
    /// Current local rotation rel to hull (0 = forward = +Y).
    pub barrel_angle: f32,
    /// Arc center / rest direction in hull frame.
    pub mount_angle: f32,
    pub fire_cd: f32,
    pub damage: i32,
    pub fire_rate: f32,
    pub weapon: WeaponType,
    /// Effective range multiplier; 1.0 by default, scaled up by adjacent
    /// Watchtower buildings via `sync_turret_config`.
    pub range_mult: f32,
    /// 1 = single barrel, 2 = twin (fires twice as fast, alternating barrels).
    pub barrels: u8,
    /// Which barrel fires next (0 or 1) when `barrels == 2`.
    pub next_barrel: u8,
    /// Effective rune list for this slot — `SlotCfg.runes` flattened
    /// (Nones stripped) PLUS any runes shared in by adjacent
    /// `Amplifier` slots. Built fresh each frame by
    /// `sync_turret_config`. Bullets fired from this turret carry a
    /// clone of this Vec so every downstream proc / on-hit / on-kill
    /// reader sees the same effective set.
    pub runes: Vec<Rune>,
    /// Carousel cursor — advances by 1 every time this slot fires a
    /// shot. Only read when a `TargetCarousel` rune is socketed:
    /// `pick_target` indexes into the sorted candidate list with
    /// `cycle_idx % len` so successive shots step through targets in
    /// a stable rotation. Wraps at `u32::MAX` (never an issue in
    /// practice).
    pub cycle_idx: u32,
}

/// Marks a barrel mesh child of a turret base. Index 0 is port-side / single,
/// index 1 is starboard-side and only shown when the slot has twin barrels.
#[derive(Component)]
pub struct BarrelIndex(pub u8);

#[derive(Component)]
pub struct TurretBarrel;

/// One segment of the painted yellow `H` on a HeliPad deck. Spawned as
/// a child of every turret slot at startup; `sync_turret_config`
/// shows it iff the slot's weapon is `HeliPad`. Three segments per
/// slot form the H shape (two posts + a crossbar).
#[derive(Component)]
pub struct HeliPadDecal;

/// Player-set per-slot configuration. UI mutates `slots`; `sync_turret_config`
/// pushes those changes (plus any pier adjacency buffs) into each `TurretSlot`.
#[derive(Resource, Clone, Debug)]
pub struct TurretConfig {
    pub slots: [SlotCfg; 8],
}

impl Default for TurretConfig {
    /// Starting loadout: slot 0 (bow) has a Standard 1-barrel turret
    /// equipped so a fresh-run player isn't dropped into combat with
    /// nothing to shoot. Every reset path (`reset_run_for_restart`,
    /// initial `insert_resource`, etc.) routes through this — so the
    /// starting loadout stays consistent.
    fn default() -> Self {
        let mut slots = [SlotCfg::default(); 8];
        slots[0] = SlotCfg {
            equipped: true,
            weapon: WeaponType::Standard,
            damage: 1,
            fire_rate: 4.0,
            barrels: 1,
            runes: [None; 3],
        };
        Self { slots }
    }
}

#[derive(Default, Clone, Copy, Debug)]
pub struct SlotCfg {
    pub equipped: bool,
    pub weapon: WeaponType,
    pub damage: i32,
    pub fire_rate: f32,
    pub barrels: u8,
    /// Up to three runes equipped on this turret. Each socket is
    /// independent — the order is just for stable UI rendering.
    pub runes: [Option<Rune>; 3],
}

// ---------- Systems ----------

/// Push per-slot config + adjacency buffs + tag synergies into each
/// live `TurretSlot`. Runs whenever `TurretConfig` changes — chained
/// after `compute_synergies` so the synergy snapshot is fresh.
pub fn sync_turret_config(
    cfg: Res<TurretConfig>,
    synergies: Res<crate::synergy::Synergies>,
    stats: Res<crate::stats::PlayerStats>,
    pm: Option<Res<PaletteMaterials>>,
    mut q: Query<(&mut TurretSlot, &mut Visibility, &mut Transform, &mut MeshMaterial2d<ColorMaterial>, &Children)>,
    mut barrels: Query<
        (&BarrelIndex, &mut Visibility, &mut Transform, &mut MeshMaterial2d<ColorMaterial>),
        (With<TurretBarrel>, Without<TurretSlot>, Without<HeliPadDecal>),
    >,
    mut decals: Query<
        &mut Visibility,
        (With<HeliPadDecal>, Without<TurretBarrel>, Without<TurretSlot>),
    >,
) {
    if !cfg.is_changed() && !stats.is_changed() { return; }
    let Some(pm) = pm else { return; };
    let turret_damage_mult = stats.turret_damage_mult();
    for (mut slot, mut vis, mut tf, mut mat, children) in &mut q {
        let s = cfg.slots[slot.index];
        let tags = s.weapon.tags();
        // Synergy multipliers — Naval is global damage, Future /
        // Autonomous boost their own fire rate, Support boosts every
        // non-Support turret. Multi-tag weapons pass the full slice
        // so Support/Autonomous opt-in / opt-out checks see every
        // tag, not just the primary. See `synergy::Synergies` for
        // the full ladder.
        let damage_mult = synergies.damage_mult_for(tags);
        let synergy_rate_mult = synergies.fire_rate_mult_for(tags);
        // Final damage = base × player TurretDamage % × tag synergy.
        // Live damage flows through `slot.damage` so every downstream
        // consumer (bullets, beams, blades, octopus, helipad heli,
        // mortar shells, cannonballs) inherits the multipliers
        // without each system reaching back to the stats resource.
        slot.damage = (s.damage as f32 * turret_damage_mult * damage_mult).round() as i32;
        slot.fire_rate = s.fire_rate
            * crate::booster::boost_multiplier_for_slot(&cfg, slot.index)
            * synergy_rate_mult;
        slot.weapon = s.weapon;
        // Non-firing weapons (HeliPad / Booster / Blade) skip
        // `turret_aim_fire`, so without an explicit reset here a slot
        // that swaps from Standard → Blade keeps the stale aim angle.
        // Resetting `barrel_angle` + the transform makes child decor
        // (Blade arm, Booster ring) inherit the mount-angle frame.
        if !s.weapon.fires_from_base() {
            let want = Quat::from_rotation_z(slot.mount_angle);
            if tf.rotation != want { tf.rotation = want; }
            slot.barrel_angle = slot.mount_angle;
        }
        slot.range_mult = crate::crows_nest::range_multiplier_for_slot(&cfg, slot.index);
        let new_barrels = s.barrels.clamp(1, 3);
        if new_barrels != slot.barrels { slot.next_barrel = 0; }
        slot.barrels = new_barrels;
        // Build the effective rune Vec: this slot's own runes
        // flattened (drop Nones) plus every adjacent Amplifier's
        // socketed runes. No fixed-3 cap on the result — Amplifier
        // can broadcast its full 3 sockets into every neighbour.
        // Amplifier slots themselves don't merge (they never fire).
        slot.runes.clear();
        for r in s.runes.iter().copied().flatten() {
            slot.runes.push(r);
        }
        if !matches!(s.weapon, WeaponType::Amplifier) {
            for &nbr in crate::balance::TURRET_ADJACENCY[slot.index] {
                let nbr_cfg = &cfg.slots[nbr];
                if !nbr_cfg.equipped { continue; }
                if !matches!(nbr_cfg.weapon, WeaponType::Amplifier) { continue; }
                // Tier gates broadcast capacity: lvl 1 shares the
                // first socketed rune only, lvl 3 shares all three.
                let cap = nbr_cfg.barrels.clamp(1, 3) as usize;
                for r in nbr_cfg.runes.iter().copied().flatten().take(cap) {
                    slot.runes.push(r);
                }
            }
        }
        *vis = if s.equipped { Visibility::Inherited } else { Visibility::Hidden };
        let turret_mat = pm.turret_for(s.weapon).clone();
        if mat.0 != turret_mat { mat.0 = turret_mat.clone(); }
        let is_helipad = matches!(s.weapon, WeaponType::HeliPad);
        let has_barrels = s.weapon.has_barrels();
        for c in children.iter() {
            // H-decal first — only the HeliPad slot shows it.
            if let Ok(mut dvis) = decals.get_mut(c) {
                let want = if s.equipped && is_helipad {
                    Visibility::Inherited
                } else {
                    Visibility::Hidden
                };
                if *dvis != want { *dvis = want; }
                continue;
            }
            let Ok((idx, mut bv, mut btf, mut bmat)) = barrels.get_mut(c) else { continue; };
            // Non-barrel weapons (HeliPad / Booster / Blade) hide the
            // standard cannon barrels — those slots use their own
            // bespoke decoration entities (helicopter / pulse ring /
            // rotating arm) spawned elsewhere.
            if !has_barrels {
                if *bv != Visibility::Hidden { *bv = Visibility::Hidden; }
                continue;
            }
            // Visibility: single → middle only; twin → port+stbd; triple → all.
            let cell_visible = match (s.barrels, idx.0) {
                (1, 1)         => true,
                (2, 0) | (2, 2) => true,
                (3, _)         => true,
                _              => false,
            };
            *bv = if s.equipped && cell_visible { Visibility::Inherited } else { Visibility::Hidden };
            // Lateral offset by index — middle stays centered, port/stbd splay.
            let lateral = match idx.0 {
                0 => -BARREL_LATERAL,
                2 =>  BARREL_LATERAL,
                _ =>  0.0,
            };
            btf.translation.x = lateral;
            // Middle barrel of a triple sits a touch forward AND scales 1px
            // longer (stretching only the front; back stays aligned with the
            // other barrels' rears).
            let middle_in_triple = s.barrels == 3 && idx.0 == 1;
            btf.translation.y = if middle_in_triple {
                3.0 + BARREL_MIDDLE_EXTEND / 2.0
            } else {
                3.0
            };
            btf.scale.y = if middle_in_triple {
                (4.0 + BARREL_MIDDLE_EXTEND) / 4.0
            } else {
                1.0
            };
            if bmat.0 != turret_mat { bmat.0 = turret_mat.clone(); }
        }
    }
}

/// Per-turret targeting + aim + fire loop. Picks the closest enemy in the
/// turret's arc + range, eases the barrel toward it, and fires when locked
/// on. Firing branches on `slot.weapon`: railgun spawns a Beam, shotgun
/// spawns N pellets, others spawn a single bullet.
pub fn turret_aim_fire(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    cfg: Res<TurretConfig>,
    stats: Res<crate::stats::PlayerStats>,
    mut thirst: ResMut<crate::rune::ThirstPending>,
    // LocalPlayer (not just Friendly) — the host has two Friendly
    // entities in MP (local + remote-peer ghost). `single()` would
    // Err and the system would bail every frame → the host's
    // turrets never aim or fire.
    // `Without<TurretSlot>` + `Without<TurretBarrel>` make this
    // statically disjoint from the mut Transform `turrets` query
    // below (Bevy's parameter-conflict checker doesn't know
    // LocalPlayer implies Friendly).
    ship_q: Query<
        (&Transform, &Heading),
        (With<crate::components::LocalPlayer>, Without<TurretSlot>, Without<TurretBarrel>),
    >,
    enemies: Query<(Entity, &Transform, &Faction, &Health), With<Enemy>>,
    mut turrets: Query<
        (Entity, &mut TurretSlot, &mut Transform, &Visibility),
        (Without<Friendly>, Without<Enemy>, Without<TurretBarrel>),
    >,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    let Ok((ship_tf, ship_heading)) = ship_q.single() else { return; };
    let ship_pos = ship_tf.translation.truncate();
    let ship_h = ship_heading.0;

    for (turret_entity, mut slot, mut tf, vis) in &mut turrets {
        if matches!(*vis, Visibility::Hidden) { continue; }
        if !cfg.slots[slot.index].equipped { continue; }
        // Some weapon types never fire bullets from the deck — their
        // damage comes from elsewhere (HeliPad's helicopter,
        // Booster's adjacency aura, Blade's melee tick). Skip the
        // entire aim/fire path so we don't spawn muzzle flashes from
        // an invisible barrel or pointlessly track an angle.
        if !slot.weapon.fires_from_base() { continue; }
        slot.fire_cd -= dt;

        // World position of turret.
        let local = tf.translation.truncate();
        let cos_h = ship_h.cos();
        let sin_h = ship_h.sin();
        let world_off = Vec2::new(
            local.x * cos_h - local.y * sin_h,
            local.x * sin_h + local.y * cos_h,
        );
        let turret_world = ship_pos + world_off;

        let hull_forward_world = ship_h;

        // Effective range = base × weapon profile × pier buff × player stat.
        let weapon_range_mult = slot.weapon.range_mult();
        let effective_range = TURRET_RANGE
            * weapon_range_mult
            * slot.range_mult.max(1.0)
            * stats.range_mult();
        // Inner dead-zone (Mortar). Scaled by the same factors as the outer
        // ring so a buffed turret's playable annulus expands proportionally
        // — keeps the ring's shape steady rather than collapsing it to a
        // sliver. 0 for non-Mortar weapons (no inner dead-zone).
        let effective_min = TURRET_RANGE
            * slot.weapon.min_range_mult()
            * slot.range_mult.max(1.0)
            * stats.range_mult();
        let half_arc = stats.effective_turret_half_arc(TURRET_ARC_HALVES[slot.index]);

        // Build the in-arc / in-range candidate list, then defer the
        // priority pick to `weapon::pick_target`. Single source of
        // truth for targeting-rune semantics — same picker player
        // turrets and autonomous units (helis, octopuses) use.
        // Anchor + fallback are both the turret itself: rune-priority
        // distance measures from the turret, and no-rune nearest-to
        // also resolves to the turret.
        // Build candidates with Entity alongside (pos, hp) so the
        // SpreadRockets path can resolve `pick_target`'s Vec2 result
        // back to an entity for the homing missile's initial target.
        // Normal weapons strip the Entity off after building it; the
        // extra allocation is negligible.
        let candidates_full: Vec<(Vec2, i32, Entity)> = enemies
            .iter()
            .filter_map(|(ee, etf, fac, hp)| {
                if fac.0 != FactionKind::Enemy { return None; }
                let ep = etf.translation.truncate();
                let to = ep - turret_world;
                let d = to.length();
                if d > effective_range { return None; }
                // Mortar can't shoot anything inside its inner dead-zone.
                if d < effective_min { return None; }
                let world_angle = (-to.x).atan2(to.y);
                let mut local_angle = world_angle - hull_forward_world;
                local_angle = (local_angle + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                    - std::f32::consts::PI;
                let mut off = local_angle - slot.mount_angle;
                off = (off + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                    - std::f32::consts::PI;
                if off.abs() > half_arc { return None; }
                Some((ep, hp.0, ee))
            })
            .collect();
        let candidates: Vec<(Vec2, i32)> =
            candidates_full.iter().map(|&(p, h, _)| (p, h)).collect();
        let best = crate::weapon::pick_target(
            &candidates,
            turret_world,
            turret_world,
            &slot.runes,
            Some(slot.cycle_idx),
        );

        let desired_local = if let Some(ep) = best {
            let to = ep - turret_world;
            let world_angle = (-to.x).atan2(to.y);
            let mut la = world_angle - hull_forward_world;
            la = (la + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                - std::f32::consts::PI;
            la
        } else {
            slot.mount_angle
        };

        slot.barrel_angle = approach_angle(
            slot.barrel_angle,
            desired_local,
            stats.effective_turret_turn_speed() * dt,
        );
        tf.rotation = Quat::from_rotation_z(slot.barrel_angle);

        if best.is_some() {
            // Wrap-aware aim error. `slot.barrel_angle` accumulates
            // unbounded over time (each `approach_angle` call returns
            // `cur + d` without renormalising), so a raw subtraction
            // can yield ~2π even when the barrel visually points at
            // the target. Normalise the delta into [-π, π] before the
            // tolerance check, otherwise turrets get "stuck" facing an
            // enemy without firing.
            let delta = slot.barrel_angle - desired_local;
            let aim_err = ((delta + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                - std::f32::consts::PI).abs();
            if aim_err < 0.1 && slot.fire_cd <= 0.0 {
                let barrels_n = slot.barrels.max(1) as f32;
                // Twin barrels = twice the effective rate (alternating barrels).
                slot.fire_cd = 1.0 / (slot.fire_rate.max(0.1) * barrels_n);
                // Advance the Carousel cursor every shot. Wraps at
                // u32::MAX which is unreachable in any real game.
                slot.cycle_idx = slot.cycle_idx.wrapping_add(1);

                // Thirst: consume any pending stacks queued from a
                // prior kill landed by THIS slot and inflate this
                // shot's `slot.damage`. The mutation is overwritten
                // by `sync_turret_config` next frame, so the bonus
                // applies exclusively to this fire pass and any
                // multi-barrel volley spawned by it.
                let thirst_stacks = thirst.take(slot.index);
                if thirst_stacks > 0 {
                    let mult = crate::rune::thirst_damage_mult(
                        thirst_stacks,
                        stats.rune_damage_mult(),
                    );
                    slot.damage = ((slot.damage as f32) * mult).round() as i32;
                }

                let total_angle = ship_h + slot.barrel_angle;
                let barrel_forward = Vec2::new(-total_angle.sin(), total_angle.cos());
                let barrel_right = Vec2::new(barrel_forward.y, -barrel_forward.x);

                // Map (barrels, next_barrel) → lateral offset for the
                // firing barrel, mirroring `sync_turret_config`'s visibility:
                //   single  → centre;
                //   twin    → next_barrel 0 = port, 1 = stbd;
                //   triple  → next_barrel 0 = port, 1 = middle, 2 = stbd.
                let lateral = match (slot.barrels, slot.next_barrel) {
                    (1, _)         => 0.0,
                    (2, 0)         => -BARREL_LATERAL,
                    (2, _)         =>  BARREL_LATERAL,
                    (3, 0)         => -BARREL_LATERAL,
                    (3, 1)         =>  0.0,
                    (3, _)         =>  BARREL_LATERAL,
                    _              =>  0.0,
                };
                // Middle barrel of a triple is `BARREL_MIDDLE_EXTEND` longer,
                // so its muzzle / bullet spawn is pushed forward to match.
                let firing_middle = slot.barrels == 3 && slot.next_barrel == 1;
                let effective_tip = FRIENDLY_BARREL_TIP
                    + if firing_middle { BARREL_MIDDLE_EXTEND } else { 0.0 };
                slot.next_barrel = (slot.next_barrel + 1) % slot.barrels.max(1);

                let outer_mat = pm.bullet_outer_for(slot.weapon).clone();
                let inner_mat = pm.bullet_inner_for(slot.weapon).clone();

                // Muzzle flash — parented to the turret so it stays glued to
                // the barrel as the ship moves and the turret rotates.
                let flash = commands.spawn((
                    Mesh2d(em.muzzle_flash.clone()),
                    MeshMaterial2d(inner_mat.clone()),
                    Transform::from_xyz(lateral, effective_tip, 2.0),
                    MuzzleFlash { life: 0.18, max_life: 0.18 },
                    RenderLayers::layer(PLAY_LAYER),
                )).id();
                commands.entity(flash).insert(ChildOf(turret_entity));

                match slot.weapon {
                    WeaponType::HeliPad => {
                        // No-op: the deck pad never fires bullets itself.
                        // The orbiting helicopter spawned by
                        // `sync_helipad_helicopters` does the firing in
                        // `helicopter_ai`.
                    }
                    WeaponType::Railgun => {
                        // Beam emanates from the barrel tip; mesh is centered
                        // on its midpoint so position the entity at the line
                        // midpoint and rotate to align local +Y with `barrel_forward`.
                        let beam_origin = turret_world + barrel_forward * effective_tip;
                        let mid_pos = beam_origin + barrel_forward * (BEAM_LENGTH / 2.0);
                        let beam_angle = (-barrel_forward.x).atan2(barrel_forward.y);
                        commands.spawn((
                            Mesh2d(em.beam.clone()),
                            MeshMaterial2d(inner_mat.clone()),
                            Transform::from_xyz(mid_pos.x, mid_pos.y, 5.5)
                                .with_rotation(Quat::from_rotation_z(beam_angle))
                                .with_scale(Vec3::new(0.0, 1.0, 1.0)),
                            Beam { life: BEAM_LIFETIME, max_life: BEAM_LIFETIME },
                            BeamHit {
                                origin: beam_origin,
                                dir: barrel_forward,
                                length: BEAM_LENGTH,
                                damage: slot.damage,
                                slot: slot.index as u8,
                                weapon: slot.weapon,
                                runes: slot.runes.clone(),
                            },
                            BeamPending,
                            RenderLayers::layer(PLAY_LAYER),
                        ));
                    }
                    WeaponType::Shotgun => {
                        // N pellets per trigger pull, each randomized within
                        // the shotgun's spread cone. Single muzzle flash.
                        let mut rng = rand::thread_rng();
                        let muzzle_pos = turret_world
                            + barrel_forward * (effective_tip + FRIENDLY_BULLET_HALF_LEN)
                            + barrel_right * lateral;
                        for _ in 0..SHOTGUN_PELLETS {
                            let off = rng.gen_range(-SHOTGUN_SPREAD..SHOTGUN_SPREAD);
                            let pa = total_angle + off;
                            let pd = Vec2::new(-pa.sin(), pa.cos());
                            spawn_combat_bullet(
                                &mut commands, &em, &outer_mat, &inner_mat,
                                muzzle_pos, pd, slot.weapon, slot.damage,
                                Some(crate::bullet::DamageSource::PlayerSlot(slot.index as u8)),
                                effective_range, slot.runes.clone(), FactionKind::Friendly,
                                stats.rune_damage_mult(),
                            );
                        }
                    }
                    WeaponType::Mortar => {
                        // Lobbed shell: snapshots the target's position at
                        // fire time and arcs to it over `MORTAR_TIME_OF_FLIGHT`,
                        // then explodes in `splash_radius` AoE. Doesn't
                        // collide en route (no `Bullet` component).
                        let muzzle_pos = turret_world
                            + barrel_forward * (effective_tip + FRIENDLY_BULLET_HALF_LEN)
                            + barrel_right * lateral;
                        let target_pos = best.unwrap_or(muzzle_pos);
                        // Each `Splash` rune slotted on this turret
                        // adds +50% to the AoE radius (additive — 2
                        // runes = +100%, 3 = +150%). Rune Effect
                        // scales the per-rune bonus, NOT the base
                        // radius — a mortar without any Splash rune
                        // must explode at its natural radius regardless
                        // of Rune Effect upgrades; otherwise the player
                        // sees the blast widen "for free" any time they
                        // buy Rune Effect, which reads as a phantom
                        // rune.
                        let splash_runes = slot.runes.iter()
                            .filter(|r| matches!(r, Rune::Splash))
                            .count() as f32;
                        let splash = MORTAR_SPLASH_RADIUS
                            * (1.0 + 0.5 * splash_runes * stats.rune_damage_mult());
                        spawn_mortar_shell(
                            &mut commands, &em, &outer_mat, &inner_mat,
                            muzzle_pos, target_pos, slot.weapon, slot.damage,
                            splash,
                            Some(DamageSource::PlayerSlot(slot.index as u8)),
                            slot.runes.clone(),
                        );
                    }
                    WeaponType::Cannon => {
                        // Heavy cannonball: bigger projectile with a
                        // velocity-impulse knockback tag so
                        // `bullet_collisions` shoves the target on hit.
                        // No spread — the cannon fires straight.
                        let muzzle_pos = turret_world
                            + barrel_forward * (effective_tip + FRIENDLY_BULLET_HALF_LEN)
                            + barrel_right * lateral;
                        crate::cannon::spawn_cannonball(
                            &mut commands, &em, &outer_mat, &inner_mat,
                            muzzle_pos, barrel_forward, slot.weapon, slot.damage,
                            Some(crate::bullet::DamageSource::PlayerSlot(slot.index as u8)),
                            effective_range, slot.runes.clone(), FactionKind::Friendly,
                        );
                    }
                    WeaponType::Harpoon => {
                        // Spear flies straight — `bullet_collisions`
                        // detects the `HarpoonTip` marker on hit and
                        // attaches a `Harpooned` tether to the target
                        // plus a chain visual.
                        let muzzle_pos = turret_world
                            + barrel_forward * (effective_tip + FRIENDLY_BULLET_HALF_LEN)
                            + barrel_right * lateral;
                        crate::harpoon::spawn_harpoon_spear(
                            &mut commands, &em, &outer_mat, &inner_mat,
                            muzzle_pos, barrel_forward, slot.weapon, slot.damage,
                            Some(crate::bullet::DamageSource::PlayerSlot(slot.index as u8)),
                            effective_range, slot.runes.clone(), FactionKind::Friendly,
                        );
                    }
                    WeaponType::SpreadRockets => {
                        // Salvo: 4 seeking rockets per trigger pull. Each
                        // rocket fans out at a small angle off the barrel
                        // forward so the volley reads as a spread, then
                        // homes via `homing_missile_track`. Each rocket
                        // picks its OWN target via `pick_target` with an
                        // advanced cycle_idx so a TargetCarousel slot
                        // naturally distributes the 4 rockets across 4
                        // enemies. Other targeting runes (Furthest /
                        // MaxHP / MinHP) make all 4 stack on the same
                        // priority enemy, which is fine — the seek will
                        // overlap their flight paths but they still land.
                        let muzzle_pos = turret_world
                            + barrel_forward * (effective_tip + FRIENDLY_BULLET_HALF_LEN)
                            + barrel_right * lateral;
                        const ROCKET_COUNT: usize = 4;
                        // Wider fan than the original 0.35rad (~20°)
                        // so the salvo visibly splays before the seek
                        // logic curves each rocket onto its target.
                        // 0.7rad (~40°) makes the volley read as four
                        // distinct trajectories at launch rather than
                        // four overlapping streaks.
                        const FAN_HALF: f32 = 0.70;
                        // Cycle the carousel cursor an extra (count - 1)
                        // steps so each rocket gets a unique target
                        // index when Carousel is socketed.
                        let base_cycle = slot.cycle_idx;
                        slot.cycle_idx = slot.cycle_idx.wrapping_add(ROCKET_COUNT as u32 - 1);
                        for i in 0..ROCKET_COUNT {
                            let t = if ROCKET_COUNT > 1 {
                                (i as f32 / (ROCKET_COUNT - 1) as f32) * 2.0 - 1.0
                            } else {
                                0.0
                            };
                            let rocket_angle = total_angle + t * FAN_HALF;
                            let rocket_forward =
                                Vec2::new(-rocket_angle.sin(), rocket_angle.cos());
                            // Per-rocket target pick: advance cycle_idx
                            // so a Carousel slot spreads across the 4
                            // rockets. Resolve the picker's Vec2 back
                            // to an Entity via candidates_full so the
                            // homing tracker locks on immediately
                            // instead of waiting to re-acquire.
                            let per_rocket_cycle = base_cycle.wrapping_add(i as u32);
                            let picked_pos = crate::weapon::pick_target(
                                &candidates,
                                turret_world,
                                turret_world,
                                &slot.runes,
                                Some(per_rocket_cycle),
                            );
                            let initial_target = picked_pos.and_then(|p| {
                                candidates_full
                                    .iter()
                                    .find(|c| c.0 == p)
                                    .map(|c| c.2)
                            });
                            // Rockets fly past the slot's nominal range
                            // so the seek arc has room to land hits, but
                            // not so far that mis-fired rockets loiter
                            // halfway across the arena.
                            const ROCKET_RANGE_MULT: f32 = 1.5;
                            crate::ally::spawn_homing_missile_full(
                                &mut commands, &em, &pm,
                                muzzle_pos, rocket_forward, slot.damage,
                                initial_target, FactionKind::Enemy,
                                Some(crate::bullet::DamageSource::PlayerSlot(slot.index as u8)),
                                slot.weapon, slot.runes.clone(),
                                effective_range * ROCKET_RANGE_MULT,
                                stats.rune_damage_mult(),
                                // Player salvos fly straight briefly before
                                // homing kicks in — short enough that the
                                // seek feels responsive, long enough that
                                // each rocket reads as "fan out then chase"
                                // rather than snapping at the muzzle.
                                0.3,
                            );
                        }
                    }
                    _ => {
                        // Single-bullet path. MG applies an accuracy spread;
                        // others fire straight.
                        let spread = slot.weapon.spread();
                        let dir = if spread > 0.0 {
                            let mut rng = rand::thread_rng();
                            let a = rng.gen_range(-spread..spread);
                            let fa = total_angle + a;
                            Vec2::new(-fa.sin(), fa.cos())
                        } else {
                            barrel_forward
                        };
                        let muzzle_pos = turret_world
                            + barrel_forward * (effective_tip + FRIENDLY_BULLET_HALF_LEN)
                            + barrel_right * lateral;
                        spawn_combat_bullet(
                            &mut commands, &em, &outer_mat, &inner_mat,
                            muzzle_pos, dir, slot.weapon, slot.damage,
                            Some(crate::bullet::DamageSource::PlayerSlot(slot.index as u8)),
                            effective_range, slot.runes.clone(), FactionKind::Friendly,
                            stats.rune_damage_mult(),
                        );
                    }
                }
            }
        }

    }
}

/// Spawn a turret bullet (outer + inner two-tone) traveling in `dir`.
/// `faction` is the side that *owns* the bullet — friendly turrets pass
/// `Friendly`, future boss turrets would pass `Enemy`. `range` is the
/// bullet's max travel distance — pass the firing turret's effective
/// range so Watchtower buffs flow through. `slot` identifies the
/// originating turret slot (0-7) for damage-stat crediting; pass `None`
/// for non-player turrets (allies/bosses) so they don't pollute per-slot
/// stats. `rune` is inherited from the firing slot and applied on hit.
pub fn spawn_combat_bullet(
    commands: &mut Commands,
    em: &EffectMeshes,
    outer_mat: &Handle<ColorMaterial>,
    inner_mat: &Handle<ColorMaterial>,
    pos: Vec2,
    dir: Vec2,
    weapon: WeaponType,
    damage: i32,
    source: Option<crate::bullet::DamageSource>,
    range: f32,
    runes: Vec<Rune>,
    faction: FactionKind,
    rune_effect: f32,
) {
    // Pierce inspection borrows the slice before the Vec moves into
    // the bullet bundle — same data, no clone.
    let rune_pierce = crate::bullet::pierce_stacks(&runes).unwrap_or(0);
    let bullet = commands.spawn((
        Mesh2d(em.bullet_friendly_outer.clone()),
        MeshMaterial2d(outer_mat.clone()),
        Transform::from_xyz(pos.x, pos.y, 4.0)
            .with_rotation(Quat::from_rotation_z((-dir.x).atan2(dir.y))),
        Bullet {
            faction,
            damage,
            remaining: range,
            weapon,
            source,
            runes,
        },
        Velocity(dir * BULLET_SPEED),
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    // Pierce socket: insert the survive-on-hit component so the
    // bullet keeps flying after impact. Only meaningful on
    // straight-flying bullets — Mortar's shell + the autonomous
    // helicopter / octopus paths spawn from their own helpers and
    // don't touch this function, so Pierce stays bullet-only.
    if faction == FactionKind::Friendly {
        if rune_pierce > 0 {
            commands.entity(bullet).insert(crate::bullet::make_pierce(rune_pierce, rune_effect));
        }
    }
    let inner = commands.spawn((
        Mesh2d(em.bullet_friendly_inner.clone()),
        MeshMaterial2d(inner_mat.clone()),
        Transform::from_xyz(0.0, 0.0, 0.05),
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    commands.entity(inner).insert(ChildOf(bullet));
}
