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
use crate::bullet::Bullet;
use crate::components::{Faction, FactionKind, Friendly, Heading, Velocity};
use crate::effects::{EffectMeshes, MuzzleFlash};
use crate::enemy::Enemy;
use crate::palette::PaletteMaterials;
use crate::pier::{pier_damage_bonus, pier_range_mult, Pier};
use crate::rune::Rune;
use crate::ship::approach_angle;
use crate::weapon::WeaponType;

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
    /// Up to three rune sockets per turret. Bullets fired from this
    /// turret inherit *every* equipped rune; the proc system rolls
    /// each independently. `None` slots are inert.
    pub runes: [Option<Rune>; 3],
}

/// Marks a barrel mesh child of a turret base. Index 0 is port-side / single,
/// index 1 is starboard-side and only shown when the slot has twin barrels.
#[derive(Component)]
pub struct BarrelIndex(pub u8);

#[derive(Component)]
pub struct TurretBarrel;

/// Player-set per-slot configuration. UI mutates `slots`; `sync_turret_config`
/// pushes those changes (plus any pier adjacency buffs) into each `TurretSlot`.
#[derive(Resource, Default)]
pub struct TurretConfig {
    pub slots: [SlotCfg; 8],
}

#[derive(Default, Clone, Copy)]
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

/// Push per-slot config + pier adjacency buffs into each live `TurretSlot`.
/// Runs whenever `TurretConfig` or `Pier` changes — covers both player-driven
/// stat tweaks (UI) and upgrade placement (Drafting phase).
pub fn sync_turret_config(
    cfg: Res<TurretConfig>,
    pier: Res<Pier>,
    pm: Option<Res<PaletteMaterials>>,
    mut q: Query<(&mut TurretSlot, &mut Visibility, &mut MeshMaterial2d<ColorMaterial>, &Children)>,
    mut barrels: Query<
        (&BarrelIndex, &mut Visibility, &mut Transform, &mut MeshMaterial2d<ColorMaterial>),
        (With<TurretBarrel>, Without<TurretSlot>),
    >,
) {
    if !cfg.is_changed() && !pier.is_changed() { return; }
    let Some(pm) = pm else { return; };
    for (mut slot, mut vis, mut mat, children) in &mut q {
        let s = cfg.slots[slot.index];
        slot.damage = s.damage + pier_damage_bonus(&pier, slot.index);
        slot.fire_rate = s.fire_rate;
        slot.weapon = s.weapon;
        slot.range_mult = pier_range_mult(&pier, slot.index);
        let new_barrels = s.barrels.clamp(1, 3);
        if new_barrels != slot.barrels { slot.next_barrel = 0; }
        slot.barrels = new_barrels;
        slot.runes = s.runes;
        *vis = if s.equipped { Visibility::Inherited } else { Visibility::Hidden };
        let turret_mat = pm.turret_for(s.weapon).clone();
        if mat.0 != turret_mat { mat.0 = turret_mat.clone(); }
        for c in children.iter() {
            let Ok((idx, mut bv, mut btf, mut bmat)) = barrels.get_mut(c) else { continue; };
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
    ship_q: Query<(&Transform, &Heading), With<Friendly>>,
    enemies: Query<(&Transform, &Faction), With<Enemy>>,
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
        let half_arc = stats.effective_turret_half_arc(TURRET_ARC_HALVES[slot.index]);

        // Find best target in this turret's arc (centered on its mount angle).
        let mut best: Option<(f32, Vec2)> = None;
        for (etf, fac) in &enemies {
            if fac.0 != FactionKind::Enemy { continue; }
            let ep = etf.translation.truncate();
            let to = ep - turret_world;
            let d = to.length();
            if d > effective_range { continue; }
            let world_angle = (-to.x).atan2(to.y);
            let mut local_angle = world_angle - hull_forward_world;
            local_angle = (local_angle + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                - std::f32::consts::PI;
            let mut off = local_angle - slot.mount_angle;
            off = (off + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                - std::f32::consts::PI;
            if off.abs() > half_arc { continue; }
            if best.map_or(true, |(bd, _)| d < bd) {
                best = Some((d, ep));
            }
        }

        let desired_local = if let Some((_, ep)) = best {
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
                                effective_range, slot.runes, FactionKind::Friendly,
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
                            effective_range, slot.runes, FactionKind::Friendly,
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
    runes: [Option<Rune>; 3],
    faction: FactionKind,
) {
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
    let inner = commands.spawn((
        Mesh2d(em.bullet_friendly_inner.clone()),
        MeshMaterial2d(inner_mat.clone()),
        Transform::from_xyz(0.0, 0.0, 0.05),
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    commands.entity(inner).insert(ChildOf(bullet));
}
