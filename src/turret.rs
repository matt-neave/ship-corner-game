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
    BARREL_LATERAL, BEAM_LENGTH, BEAM_LIFETIME, BULLET_SPEED, FRIENDLY_BARREL_TIP,
    FRIENDLY_BULLET_HALF_LEN, PLAY_LAYER, SHOTGUN_PELLETS, SHOTGUN_SPREAD, TURRET_ARC_HALVES,
    TURRET_PIVOT, TURRET_RANGE,
};
use crate::beam::{Beam, BeamHit, BeamPending};
use crate::bullet::Bullet;
use crate::components::{Faction, FactionKind, Friendly, Heading, Velocity};
use crate::effects::{EffectMeshes, MuzzleFlash};
use crate::enemy::Enemy;
use crate::palette::PaletteMaterials;
use crate::pier::{pier_damage_bonus, pier_range_mult, Pier};
use crate::weapon::WeaponType;
use crate::approach_angle;

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
        let new_barrels = s.barrels.max(1);
        if new_barrels != slot.barrels { slot.next_barrel = 0; }
        slot.barrels = new_barrels;
        *vis = if s.equipped { Visibility::Inherited } else { Visibility::Hidden };
        let turret_mat = pm.turret_for(s.weapon).clone();
        if mat.0 != turret_mat { mat.0 = turret_mat.clone(); }
        for c in children.iter() {
            if let Ok((idx, mut bv, mut btf, mut bmat)) = barrels.get_mut(c) {
                let visible = s.equipped && (idx.0 == 0 || s.barrels >= 2);
                *bv = if visible { Visibility::Inherited } else { Visibility::Hidden };
                let lateral = if s.barrels >= 2 {
                    if idx.0 == 0 { -BARREL_LATERAL } else { BARREL_LATERAL }
                } else { 0.0 };
                btf.translation.x = lateral;
                btf.translation.y = 3.0;
                if bmat.0 != turret_mat { bmat.0 = turret_mat.clone(); }
            }
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
    ship_q: Query<(&Transform, &Heading), With<Friendly>>,
    enemies: Query<(&Transform, &Faction), With<Enemy>>,
    mut turrets: Query<
        (Entity, &mut TurretSlot, &mut Transform, &Children, &Visibility),
        (Without<Friendly>, Without<Enemy>, Without<TurretBarrel>),
    >,
    mut barrels: Query<
        &mut Transform,
        (With<TurretBarrel>, Without<TurretSlot>, Without<Friendly>, Without<Enemy>),
    >,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    let Ok((ship_tf, ship_heading)) = ship_q.single() else { return; };
    let ship_pos = ship_tf.translation.truncate();
    let ship_h = ship_heading.0;

    for (turret_entity, mut slot, mut tf, children, vis) in &mut turrets {
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

        // Effective range = base × pier-derived multiplier (Watchtower buffs).
        let effective_range = TURRET_RANGE * slot.range_mult.max(1.0);

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
            if off.abs() > TURRET_ARC_HALVES[slot.index] { continue; }
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

        slot.barrel_angle = approach_angle(slot.barrel_angle, desired_local, TURRET_PIVOT * dt);
        tf.rotation = Quat::from_rotation_z(slot.barrel_angle);

        if best.is_some() {
            let aim_err = (slot.barrel_angle - desired_local).abs();
            if aim_err < 0.1 && slot.fire_cd <= 0.0 {
                let barrels_n = slot.barrels.max(1) as f32;
                // Twin barrels = twice the effective rate (alternating barrels).
                slot.fire_cd = 1.0 / (slot.fire_rate.max(0.1) * barrels_n);

                let total_angle = ship_h + slot.barrel_angle;
                let barrel_forward = Vec2::new(-total_angle.sin(), total_angle.cos());
                let barrel_right = Vec2::new(barrel_forward.y, -barrel_forward.x);

                let lateral = if slot.barrels >= 2 {
                    if slot.next_barrel == 0 { -BARREL_LATERAL } else { BARREL_LATERAL }
                } else { 0.0 };
                slot.next_barrel = (slot.next_barrel + 1) % slot.barrels.max(1);

                let outer_mat = pm.bullet_outer_for(slot.weapon).clone();
                let inner_mat = pm.bullet_inner_for(slot.weapon).clone();

                // Muzzle flash — parented to the turret so it stays glued to
                // the barrel as the ship moves and the turret rotates.
                let flash = commands.spawn((
                    Mesh2d(em.muzzle_flash.clone()),
                    MeshMaterial2d(inner_mat.clone()),
                    Transform::from_xyz(lateral, FRIENDLY_BARREL_TIP, 2.0),
                    MuzzleFlash { life: 0.18, max_life: 0.18 },
                    RenderLayers::layer(PLAY_LAYER),
                )).id();
                commands.entity(flash).insert(ChildOf(turret_entity));

                match slot.weapon {
                    WeaponType::Railgun => {
                        // Beam emanates from the barrel tip; mesh is centered
                        // on its midpoint so position the entity at the line
                        // midpoint and rotate to align local +Y with `barrel_forward`.
                        let beam_origin = turret_world + barrel_forward * FRIENDLY_BARREL_TIP;
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
                            + barrel_forward * (FRIENDLY_BARREL_TIP + FRIENDLY_BULLET_HALF_LEN)
                            + barrel_right * lateral;
                        for _ in 0..SHOTGUN_PELLETS {
                            let off = rng.gen_range(-SHOTGUN_SPREAD..SHOTGUN_SPREAD);
                            let pa = total_angle + off;
                            let pd = Vec2::new(-pa.sin(), pa.cos());
                            spawn_friendly_bullet(
                                &mut commands, &em, &outer_mat, &inner_mat,
                                muzzle_pos, pd, slot.weapon, slot.damage, slot.index as u8,
                                effective_range,
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
                            + barrel_forward * (FRIENDLY_BARREL_TIP + FRIENDLY_BULLET_HALF_LEN)
                            + barrel_right * lateral;
                        spawn_friendly_bullet(
                            &mut commands, &em, &outer_mat, &inner_mat,
                            muzzle_pos, dir, slot.weapon, slot.damage, slot.index as u8,
                            effective_range,
                        );
                    }
                }
            }
        }

        // Suppress unused warnings — these are kept in the query so future
        // turret tweaks can reach into the children/barrels without re-shuffling.
        let _ = children;
        let _ = &mut barrels;
    }
}

/// Spawn a friendly bullet (outer + inner two-tone) traveling in `dir`.
/// `range` is the bullet's max travel distance — pass the firing turret's
/// effective range so Watchtower buffs flow through.
fn spawn_friendly_bullet(
    commands: &mut Commands,
    em: &EffectMeshes,
    outer_mat: &Handle<ColorMaterial>,
    inner_mat: &Handle<ColorMaterial>,
    pos: Vec2,
    dir: Vec2,
    weapon: WeaponType,
    damage: i32,
    slot_idx: u8,
    range: f32,
) {
    let bullet = commands.spawn((
        Mesh2d(em.bullet_friendly_outer.clone()),
        MeshMaterial2d(outer_mat.clone()),
        Transform::from_xyz(pos.x, pos.y, 4.0)
            .with_rotation(Quat::from_rotation_z((-dir.x).atan2(dir.y))),
        Bullet {
            faction: FactionKind::Friendly,
            damage,
            remaining: range,
            weapon,
            slot: Some(slot_idx),
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
