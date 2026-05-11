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
use crate::bullet::{apply_damage, credit_damage, Bullet, DamageSource};
use crate::components::{Faction, FactionKind, Friendly, Health, Heading, Velocity};
use crate::effects::{spawn_hit_particles, EffectMeshes, HitFx, MuzzleFlash};
use crate::enemy::Enemy;
use crate::modes::ScreenShake;
use crate::palette::PaletteMaterials;
use crate::pier::{pier_damage_bonus, pier_range_mult, Pier};
use crate::rune::Rune;
use crate::ship::approach_angle;
use crate::ui::DamageStats;
use crate::weapon::{TargetPriority, WeaponType};

// ---------- Mortar tunables ----------

/// Total air time of a mortar shell from muzzle to landing. Independent
/// of distance — short and long shots both take the same flight time so
/// the arc reads as a consistent "lobbed" feel.
pub const MORTAR_TIME_OF_FLIGHT: f32 = 0.65;
/// Radius (world units) of the mortar's splash AoE. Roughly 2× a small
/// enemy hit radius (~3.5 × variant scale) — enough to catch packs.
pub const MORTAR_SPLASH_RADIUS: f32 = 12.0;
/// Peak visual lift of the shell at apex (t = 0.5), expressed as added
/// scale on top of 1.0. Sin-shaped so it grows then shrinks back.
pub const MORTAR_APEX_SCALE: f32 = 0.6;
/// Peak vertical offset of the arc, in world units. The shell's
/// position is lifted along world-+Y by `sin(πt) × MORTAR_ARC_HEIGHT`,
/// so its trajectory visibly bows upward and falls back to the target
/// regardless of flight direction (always-up looks consistent in
/// top-down).
const MORTAR_ARC_HEIGHT: f32 = 12.0;

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

/// One segment of the painted yellow `H` on a HeliPad deck. Spawned as
/// a child of every turret slot at startup; `sync_turret_config`
/// shows it iff the slot's weapon is `HeliPad`. Three segments per
/// slot form the H shape (two posts + a crossbar).
#[derive(Component)]
pub struct HeliPadDecal;

/// In-flight mortar shell. Spawned by `spawn_mortar_shell`, ticked by
/// `mortar_shell_tick`. The shell has no `Bullet` component — it can't
/// be hit en route and doesn't go through `bullet_collisions`. On
/// landing (`elapsed >= time_of_flight`) it explodes, damaging every
/// enemy inside `splash_radius` of `target` and despawning itself.
///
/// `target` is a snapshot of the targeted enemy's position at fire
/// time — the shell can't course-correct, which is intentional (the
/// "predicted shot" feel: a mortar shell that misses is a mortar shell
/// that committed too early).
#[derive(Component)]
pub struct MortarShell {
    pub target: Vec2,
    pub origin: Vec2,
    pub time_of_flight: f32,
    pub elapsed: f32,
    pub damage: i32,
    pub splash_radius: f32,
    pub source: Option<DamageSource>,
    pub weapon: WeaponType,
    /// Companion entity drawn at `target` for the entire flight so the
    /// player can read the landing spot. Despawned with the shell at
    /// impact. `None` if the shadow couldn't be spawned (shouldn't
    /// happen in practice).
    pub shadow: Option<Entity>,
}

/// Player-set per-slot configuration. UI mutates `slots`; `sync_turret_config`
/// pushes those changes (plus any pier adjacency buffs) into each `TurretSlot`.
#[derive(Resource)]
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
        (With<TurretBarrel>, Without<TurretSlot>, Without<HeliPadDecal>),
    >,
    mut decals: Query<
        &mut Visibility,
        (With<HeliPadDecal>, Without<TurretBarrel>, Without<TurretSlot>),
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
        let is_helipad = matches!(s.weapon, WeaponType::HeliPad);
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
            // HeliPad slots never show barrels — the helicopter does
            // the firing, the deck is bare except for the H.
            if is_helipad {
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
    ship_q: Query<(&Transform, &Heading), With<Friendly>>,
    enemies: Query<(&Transform, &Faction, &Health), With<Enemy>>,
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
        // HeliPad slots never fire bullets from the deck — their orbiting
        // helicopter does the firing in `helicopter_ai`. Skip the entire
        // aim/fire path so we don't spawn muzzle flashes from an invisible
        // pad or pointlessly track barrel angle.
        if matches!(slot.weapon, WeaponType::HeliPad) { continue; }
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

        // Find best target in this turret's arc.
        //
        // Default is `Closest`. A targeting rune slotted on this turret
        // overrides — `Rune::TargetFurthest` / `TargetHighestHp` /
        // `TargetLowestHp` swap the score function. The first targeting
        // rune found by socket order wins (multiple are unusual but
        // possible — keeps behaviour deterministic without forbidding it).
        let priority = slot.runes.iter()
            .find_map(|r| r.and_then(|r| r.target_priority()))
            .unwrap_or(TargetPriority::Closest);
        let mut best: Option<(f32, Vec2)> = None;
        // `best_score` always uses "min wins" semantics so the loop
        // body has a single comparator regardless of priority. The
        // `score()` helper below negates as needed.
        let mut best_score: Option<f32> = None;
        for (etf, fac, hp) in &enemies {
            if fac.0 != FactionKind::Enemy { continue; }
            let ep = etf.translation.truncate();
            let to = ep - turret_world;
            let d = to.length();
            if d > effective_range { continue; }
            // Mortar can't shoot anything inside its inner dead-zone.
            if d < effective_min { continue; }
            let world_angle = (-to.x).atan2(to.y);
            let mut local_angle = world_angle - hull_forward_world;
            local_angle = (local_angle + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                - std::f32::consts::PI;
            let mut off = local_angle - slot.mount_angle;
            off = (off + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                - std::f32::consts::PI;
            if off.abs() > half_arc { continue; }
            let score = match priority {
                TargetPriority::Closest   =>  d,
                TargetPriority::Furthest  => -d,
                TargetPriority::HighestHp => -(hp.0 as f32),
                TargetPriority::LowestHp  =>  hp.0 as f32,
            };
            if best_score.map_or(true, |bs| score < bs) {
                best_score = Some(score);
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
                    WeaponType::Mortar => {
                        // Lobbed shell: snapshots the target's position at
                        // fire time and arcs to it over `MORTAR_TIME_OF_FLIGHT`,
                        // then explodes in `splash_radius` AoE. Doesn't
                        // collide en route (no `Bullet` component).
                        let muzzle_pos = turret_world
                            + barrel_forward * (effective_tip + FRIENDLY_BULLET_HALF_LEN)
                            + barrel_right * lateral;
                        let target_pos = best.map(|(_, p)| p).unwrap_or(muzzle_pos);
                        // Each `Splash` rune slotted on this turret
                        // adds +50% to the AoE radius (additive — 2
                        // runes = 200%, 3 = 250%).
                        let splash_runes = slot.runes.iter()
                            .filter(|r| matches!(r, Some(Rune::Splash)))
                            .count() as f32;
                        let splash = MORTAR_SPLASH_RADIUS * (1.0 + 0.5 * splash_runes);
                        spawn_mortar_shell(
                            &mut commands, &em, &outer_mat, &inner_mat,
                            muzzle_pos, target_pos, slot.weapon, slot.damage,
                            splash,
                            Some(DamageSource::PlayerSlot(slot.index as u8)),
                        );
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

/// Spawn a Mortar shell at `pos` headed for `target`. The shell is a
/// stand-alone projectile — no `Bullet` component, so it ignores the
/// regular collision pipeline. Its lifetime is owned by
/// `mortar_shell_tick`, which interpolates position along an arc and
/// detonates at `target` after `MORTAR_TIME_OF_FLIGHT` seconds.
///
/// A faint shadow circle is spawned at `target` for the entire flight
/// so the player can read the landing spot — its entity id is stored on
/// the shell so the tick can despawn it on impact.
pub fn spawn_mortar_shell(
    commands: &mut Commands,
    em: &EffectMeshes,
    outer_mat: &Handle<ColorMaterial>,
    inner_mat: &Handle<ColorMaterial>,
    pos: Vec2,
    target: Vec2,
    weapon: WeaponType,
    damage: i32,
    splash_radius: f32,
    source: Option<DamageSource>,
) {
    // Landing-indicator removed — the player reads the lobbed arc + a
    // visible elongated shell, no shadow circle needed at the target.
    // Tick still handles `shadow: None` cleanly via `if let Some`.

    // Orient the oblong bullet mesh along the flight direction so the
    // shell visually points at its landing spot for the whole arc.
    let flight = target - pos;
    let heading = if flight.length_squared() > 0.0001 {
        (-flight.x).atan2(flight.y)
    } else {
        0.0
    };
    let shell = commands.spawn((
        Mesh2d(em.bullet_friendly_outer.clone()),
        MeshMaterial2d(outer_mat.clone()),
        Transform::from_xyz(pos.x, pos.y, 4.5)
            .with_rotation(Quat::from_rotation_z(heading)),
        MortarShell {
            target,
            origin: pos,
            time_of_flight: MORTAR_TIME_OF_FLIGHT,
            elapsed: 0.0,
            damage,
            splash_radius,
            source,
            weapon,
            shadow: None,
        },
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    let inner = commands.spawn((
        Mesh2d(em.bullet_friendly_inner.clone()),
        MeshMaterial2d(inner_mat.clone()),
        Transform::from_xyz(0.0, 0.0, 0.05),
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    commands.entity(inner).insert(ChildOf(shell));
}

/// Per-frame tick for in-flight `MortarShell`s. Interpolates position
/// along the line origin → target while scaling the shell up then back
/// down (sin envelope) to fake a 3D arc in 2D. On landing, iterates
/// every enemy within `splash_radius` of `target`, applies damage,
/// credits the firing slot, spawns FX, nudges the screen, and
/// despawns the shell + its shadow.
///
/// One crit roll per shell — applied to every enemy in the splash
/// rather than rolled independently per target. Per the brief: a crit
/// mortar is one big satisfying beat, not a scatter of per-enemy luck.
pub fn mortar_shell_tick(
    time: Res<Time>,
    mut commands: Commands,
    mut stats: ResMut<DamageStats>,
    player_stats: Res<crate::stats::PlayerStats>,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    mut shake: ResMut<ScreenShake>,
    mut shells: Query<(Entity, &mut Transform, &mut MortarShell)>,
    mut enemies: Query<(&Transform, &Enemy, &mut Health, &mut HitFx), (With<Enemy>, Without<MortarShell>)>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();

    for (shell_e, mut shell_tf, mut shell) in &mut shells {
        shell.elapsed += dt;
        let tof = shell.time_of_flight.max(0.0001);

        if shell.elapsed < tof {
            // In flight — interpolate along origin → target, then
            // lift along world-+Y by `sin(πt) × MORTAR_ARC_HEIGHT` so
            // the trajectory visibly bows upward into a real arc. The
            // scale envelope on top adds a tiny "closer to camera"
            // illusion at the apex.
            let t = (shell.elapsed / tof).clamp(0.0, 1.0);
            let lift = (std::f32::consts::PI * t).sin();
            let ground = shell.origin.lerp(shell.target, t);
            let pos = ground + Vec2::new(0.0, lift * MORTAR_ARC_HEIGHT);
            let scale = 1.0 + MORTAR_APEX_SCALE * lift;
            shell_tf.translation.x = pos.x;
            shell_tf.translation.y = pos.y;
            // Uniform scale — the shell uses the same bullet visual as
            // every other weapon; the arc trajectory is what reads as
            // "mortar," not the silhouette.
            shell_tf.scale = Vec3::new(scale, scale, 1.0);
            continue;
        }

        // Landed — explode. One crit roll per shell, applied to every
        // enemy caught in the splash so a crit detonation is one
        // collective big-number beat, not a per-enemy lottery.
        let crit_mult = if matches!(shell.source, Some(DamageSource::PlayerSlot(_))) {
            player_stats.roll_crit_mult(&mut rng) as i32
        } else {
            1
        };
        let amount = shell.damage.saturating_mul(crit_mult);
        let target = shell.target;

        for (etf, en, mut h, mut fx) in &mut enemies {
            let ep = etf.translation.truncate();
            // Hit-disc check: enemy is "in splash" if its center is
            // within `splash_radius + enemy_hit_radius` of `target`.
            // Mirrors `bullet_collisions`' enemy radius (3.5 × scale).
            let er = 3.5 * en.variant.scale();
            let reach = shell.splash_radius + er;
            if ep.distance_squared(target) > reach * reach { continue; }
            if h.0 <= 0 { continue; }
            let dealt = apply_damage(&mut h, &mut fx, amount);
            credit_damage(&mut stats, shell.source, dealt);
        }

        // Particle burst at the impact point — weapon-color inner spark
        // for the bright core, plus a wider outer ring of slower
        // particles that reads as the shockwave.
        let inner_mat = pm.bullet_inner_for(shell.weapon);
        let outer_mat = pm.bullet_outer_for(shell.weapon);
        spawn_hit_particles(&mut commands, &em, inner_mat, target, 14, 110.0, &mut rng);
        spawn_hit_particles(&mut commands, &em, outer_mat, target, 10, 60.0, &mut rng);

        shake.add_trauma(0.25);

        // Despawn shell + shadow.
        if let Some(shadow) = shell.shadow {
            commands.entity(shadow).despawn();
        }
        commands.entity(shell_e).despawn();
    }
}

// ---------- HeliPad / Helicopter ----------
//
// A `HeliPad` slot doesn't fire from the deck — instead it ensures a
// single persistent `Helicopter` entity exists per equipped HeliPad slot.
// The helicopter is a free-flying entity (NOT parented to the ship) that
// orbits the ship at a fixed radius and fires forward at the closest
// enemy in range, mirroring the player ally `Plane`'s strafe pattern but
// using the slot's own `damage` / `fire_rate` / `barrels` / `runes`.
//
// Lifecycle invariant maintained by `sync_helipad_helicopters`:
//   "exactly one Helicopter exists per equipped HeliPad slot"
// — slot equipped + matching helicopter? do nothing.
// — slot equipped, no helicopter? spawn one.
// — helicopter whose slot is no longer HeliPad-equipped? despawn it.

/// Orbit radius (world units) — comfortably outside the ship's hull and
/// inside `TURRET_RANGE` so the heli stays in visible play space.
pub const HELI_ORBIT_RADIUS: f32 = 30.0;
/// Constant flight speed (world units / s). The helicopter never
/// snaps to a target — it always moves forward at this speed and
/// turns to align its nose with the desired direction.
pub const HELI_SPEED: f32 = 28.0;
/// Body turn rate (rad/s). Slow enough that the heli has visible
/// inertia — wide turning circles, no instant heading flips.
pub const HELI_TURN_RATE: f32 = 2.5;
/// Bullet speed for helicopter MG fire — punchy enough to read as
/// light arms tracking a moving target.
pub const HELI_BULLET_SPEED: f32 = 120.0;
/// Lateral offset (perp to forward) for each of the three nose barrels.
/// Indexed by `HeliNoseBarrel.idx`; the firing logic uses the same
/// numbers when emitting bullets so muzzle visuals line up with where
/// the projectile actually leaves the chassis.
pub const HELI_BARREL_LATERAL: [f32; 3] = [-1.4, 0.0, 1.4];
/// Bullet lifetime / range cap on a helicopter shot. Bullet's own
/// travel budget — bumped alongside `HELI_BULLET_SPEED` so the
/// effective travel-distance stays reasonable even when the heli
/// detects a far-away enemy.
pub const HELI_BULLET_RANGE: f32 = 120.0;

/// Free-flying helicopter spawned by an equipped `HeliPad` slot. Owns
/// its own world-space heading + fire cooldown so each pad's heli is
/// independent.
#[derive(Component)]
pub struct Helicopter {
    /// Slot index that launched this helicopter. Used by
    /// `sync_helipad_helicopters` to despawn orphans when the slot is
    /// unequipped or weapon-swapped.
    pub owner_slot: usize,
    /// Time until next allowed shot. Tick down by `dt` each frame.
    pub fire_cd: f32,
    /// Current world-space heading angle in radians (0 = facing +Y).
    /// Tracked separately from `Transform::rotation` so we can
    /// approach the desired heading at a fixed turn rate without
    /// extracting the angle back out of a quaternion every frame.
    pub heading: f32,
    /// Which barrel fires next when `barrels > 1` (mirrors
    /// `TurretSlot::next_barrel`'s alternation).
    pub next_barrel: u8,
}

/// One nose-barrel rectangle. Three are spawned per helicopter (idx
/// 0/1/2 = port/centre/stbd); `sync_helipad_helicopters` toggles
/// visibility per slot's `barrels` count using the same rule as
/// regular turrets:
///   - barrels=1 → only idx 1 visible
///   - barrels=2 → idx 0 + idx 2
///   - barrels=3 → all three
#[derive(Component)]
pub struct HeliNoseBarrel { pub heli: Entity, pub idx: u8 }

/// Marks the spinning rotor child of a helicopter. The rotor has its
/// own Transform that we tick each frame for visual flair.
#[derive(Component)]
pub struct HeliRotor;

/// Maintain the "one helicopter per equipped HeliPad slot" invariant.
/// Runs in the combat-sim block before `helicopter_ai` so a freshly
/// spawned heli ticks this frame.
pub fn sync_helipad_helicopters(
    mut commands: Commands,
    cfg: Res<TurretConfig>,
    pm: Option<Res<PaletteMaterials>>,
    mut meshes: ResMut<Assets<Mesh>>,
    ship_q: Query<&Transform, (With<Friendly>, Without<Helicopter>)>,
    helis: Query<(Entity, &Helicopter)>,
) {
    let Some(pm) = pm else { return; };

    // First pass: despawn helicopters whose owning slot is no longer an
    // equipped HeliPad. Bevy's `despawn` recursively despawns children,
    // so the rotor goes with the parent.
    for (e, heli) in &helis {
        let slot = cfg.slots.get(heli.owner_slot).copied().unwrap_or_default();
        let still_valid = slot.equipped && matches!(slot.weapon, WeaponType::HeliPad);
        if !still_valid {
            commands.entity(e).despawn();
        }
    }

    // Second pass: for each equipped HeliPad slot lacking a helicopter,
    // spawn one near the ship. Skip if there's no friendly ship yet
    // (game still booting).
    let Ok(ship_tf) = ship_q.single() else { return; };
    let ship_pos = ship_tf.translation.truncate();

    for (idx, slot) in cfg.slots.iter().enumerate() {
        if !slot.equipped { continue; }
        if !matches!(slot.weapon, WeaponType::HeliPad) { continue; }
        let already = helis.iter().any(|(_, h)| h.owner_slot == idx);
        if already { continue; }

        // Spread initial spawn positions per-slot so multiple HeliPads
        // don't stack their helis on top of each other.
        let phase = (idx as f32) * std::f32::consts::TAU / 8.0;
        let init_pos = ship_pos
            + Vec2::new(phase.cos(), phase.sin()) * HELI_ORBIT_RADIUS;
        // Initial heading faces away from the ship so the heli starts
        // by flying outward into the arena rather than into the hull.
        let outward = (init_pos - ship_pos).try_normalize().unwrap_or(Vec2::Y);
        let init_heading = (-outward.x).atan2(outward.y);

        // Capsule body — long axis along +Y so the helicopter has a
        // clear "nose" direction. `helicopter_ai` rotates the body to
        // face its current desired heading each frame.
        let body_mesh = meshes.add(Capsule2d::new(2.0, 2.5));
        let rotor_mesh = meshes.add(Rectangle::new(8.0, 0.8));
        // Forward turret on the nose: chunky Circle base + 3 long
        // barrel rectangles parented to the body so they rotate with
        // it. Visibility per `slot.barrels` is set every frame in
        // `sync_helipad_nose_barrels`.
        let nose_base_mesh = meshes.add(Circle::new(1.7));
        let nose_barrel_mesh = meshes.add(Rectangle::new(1.0, 3.5));
        let body_mat = pm.helipad_deck.clone();
        let nose_mat = pm.turret.clone();
        // Rotors share the dark-grey turret material so the spinning
        // X reads as a mechanical fitting (motion blur on metal),
        // not a yellow caution stripe.
        let rotor_mat = pm.turret.clone();
        // Tail boom + tail rotor reuse the body deck colour so they
        // read as one continuous chassis. Canopy uses the white flag
        // material so the cockpit pops as a clear front-of-hull mark.
        let tail_mat = pm.helipad_deck.clone();
        let canopy_mat = pm.ally_flag.clone();
        let tail_boom_mesh = meshes.add(Rectangle::new(0.7, 3.6));
        let tail_rotor_mesh = meshes.add(Rectangle::new(2.6, 0.4));
        let canopy_mesh = meshes.add(Circle::new(0.7));

        let heli = commands.spawn((
            Mesh2d(body_mesh),
            MeshMaterial2d(body_mat),
            Transform::from_xyz(init_pos.x, init_pos.y, 2.5)
                .with_rotation(Quat::from_rotation_z(init_heading)),
            Helicopter {
                owner_slot: idx,
                fire_cd: 0.0,
                heading: init_heading,
                next_barrel: 0,
            },
            RenderLayers::layer(PLAY_LAYER),
        )).id();

        // Tail boom — long thin rectangle extending behind the body.
        // Body capsule reaches to y ≈ -3.25; boom starts just inside
        // that and runs back to y ≈ -6.6. Same green as the deck so
        // the silhouette reads as one fuselage.
        let tail_boom = commands.spawn((
            Mesh2d(tail_boom_mesh),
            MeshMaterial2d(tail_mat.clone()),
            Transform::from_xyz(0.0, -4.6, 0.02),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(tail_boom).insert(ChildOf(heli));

        // Tail rotor — short horizontal rectangle at the boom tip.
        // Perpendicular to forward so it reads as the tail-rotor disc
        // from above. Static (doesn't spin) — selling motion through
        // the main rotors is enough.
        let tail_rotor = commands.spawn((
            Mesh2d(tail_rotor_mesh),
            MeshMaterial2d(rotor_mat.clone()),
            Transform::from_xyz(0.0, -6.7, 0.03),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(tail_rotor).insert(ChildOf(heli));

        // Cockpit canopy — small white disc on the forward half of
        // the body. Sits just behind the nose turret base so the
        // pilot's "window" reads against the green hull.
        let canopy = commands.spawn((
            Mesh2d(canopy_mesh),
            MeshMaterial2d(canopy_mat),
            Transform::from_xyz(0.0, 0.4, 0.03),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(canopy).insert(ChildOf(heli));

        // Nose turret base — sits forward of body center. Static
        // relative to the body — no independent rotation.
        let nose_base = commands.spawn((
            Mesh2d(nose_base_mesh),
            MeshMaterial2d(nose_mat.clone()),
            Transform::from_xyz(0.0, 1.5, 0.04),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(nose_base).insert(ChildOf(heli));

        // Three nose barrels (port / centre / stbd). All spawned
        // every time so a `barrels` change can flip visibility
        // without rebuilding the helicopter; firing positions in
        // `helicopter_ai` use the same lateral offsets so the muzzle
        // exit lines up with the rendered barrel.
        for idx in 0u8..3 {
            let lateral = HELI_BARREL_LATERAL[idx as usize];
            let nose_barrel = commands.spawn((
                Mesh2d(nose_barrel_mesh.clone()),
                MeshMaterial2d(nose_mat.clone()),
                Transform::from_xyz(lateral, 3.2, 0.05),
                RenderLayers::layer(PLAY_LAYER),
                Visibility::Hidden,
                HeliNoseBarrel { heli, idx },
            )).id();
            commands.entity(nose_barrel).insert(ChildOf(heli));
        }

        // Two rotors crossed in an X — both spin at the same rate so
        // the 90° offset is preserved, giving a 4-bladed look.
        for rot_offset in [0.0, std::f32::consts::FRAC_PI_2] {
            let rotor = commands.spawn((
                Mesh2d(rotor_mesh.clone()),
                MeshMaterial2d(rotor_mat.clone()),
                Transform::from_xyz(0.0, 0.0, 0.06)
                    .with_rotation(Quat::from_rotation_z(rot_offset)),
                HeliRotor,
                RenderLayers::layer(PLAY_LAYER),
            )).id();
            commands.entity(rotor).insert(ChildOf(heli));
        }
    }
}

/// Toggle each helicopter's nose-barrel visibility from the owning
/// slot's `barrels` count. Mirrors the rule in `sync_turret_config`
/// (centre-only / port+stbd / all three) so the heli's nose reads the
/// same as a fixed turret of equivalent barrel count. Cheap; gated
/// on `cfg.is_changed()` since barrels only flip via shop / debug.
pub fn sync_helipad_nose_barrels(
    cfg: Res<TurretConfig>,
    helis: Query<&Helicopter>,
    mut barrels: Query<(&HeliNoseBarrel, &mut Visibility)>,
) {
    if !cfg.is_changed() { return; }
    for (b, mut vis) in &mut barrels {
        let Ok(heli) = helis.get(b.heli) else { continue; };
        let slot = cfg.slots.get(heli.owner_slot).copied().unwrap_or_default();
        let barrels_count = slot.barrels.max(1);
        let want_visible = match (barrels_count, b.idx) {
            (1, 1)         => true,
            (2, 0) | (2, 2) => true,
            (3, _)         => true,
            _              => false,
        };
        let want = if want_visible { Visibility::Inherited } else { Visibility::Hidden };
        if *vis != want { *vis = want; }
    }
}

/// Per-frame movement + fire for every active helicopter. Steers each
/// heli toward its current orbit target around the ship, picks the
/// nearest enemy in slot-range, and fires forward when in range.
///
/// Bullets carry the slot's `runes` and `DamageSource::PlayerSlot(idx)`
/// so all proc / damage-credit machinery flows through unchanged.
pub fn helicopter_ai(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    cfg: Res<TurretConfig>,
    stats: Res<crate::stats::PlayerStats>,
    ship_q: Query<&Transform, (With<Friendly>, Without<Helicopter>, Without<Enemy>)>,
    enemies: Query<(&Transform, &Faction), (With<Enemy>, Without<Helicopter>)>,
    mut helis: Query<(&mut Transform, &mut Helicopter), Without<HeliRotor>>,
    mut rotors: Query<&mut Transform, (With<HeliRotor>, Without<Helicopter>, Without<Enemy>, Without<Friendly>)>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    let Ok(ship_tf) = ship_q.single() else { return; };
    let ship_pos = ship_tf.translation.truncate();

    // Spin every rotor uniformly — they're all on the same speed.
    for mut rotf in &mut rotors {
        rotf.rotate_z(8.0 * dt);
    }

    for (mut tf, mut heli) in &mut helis {
        let slot_cfg = cfg.slots.get(heli.owner_slot).copied().unwrap_or_default();
        // sync_helipad_helicopters despawns orphans, but be defensive:
        // if a stale heli slips through one frame, just idle it.
        if !slot_cfg.equipped || !matches!(slot_cfg.weapon, WeaponType::HeliPad) {
            continue;
        }

        let cur = tf.translation.truncate();
        let effective_range = TURRET_RANGE * stats.range_mult();

        // Acquire the nearest enemy ANYWHERE on the map — no
        // detection radius, no leash to the boat. The heli will
        // chase across the whole arena, only stopping at the orbit
        // standoff when it gets close enough to fire.
        let mut best: Option<(f32, Vec2)> = None;
        for (etf, fac) in &enemies {
            if fac.0 != FactionKind::Enemy { continue; }
            let ep = etf.translation.truncate();
            let d = ep.distance(cur);
            if best.map_or(true, |(bd, _)| d < bd) {
                best = Some((d, ep));
            }
        }

        // Pick what we're orbiting around: the enemy we're chasing,
        // or the ship if there's nothing to fight. Standoff distance
        // differs per anchor — close enough to engage / loose enough
        // to patrol — but the orbit math is identical (mirrors
        // `ally_ai`'s combat-orbit pattern).
        //
        // Per-slot stagger: even slots orbit CCW, odd CW; each slot
        // also gets a small range offset so multiple helis don't
        // converge on the same circle. Without this, two HeliPads
        // pick the same nearest enemy and trace identical paths.
        let orbit_sign = if heli.owner_slot % 2 == 0 { 1.0 } else { -1.0 };
        let range_offset = (heli.owner_slot as f32) * 1.8;
        let (anchor, anchor_range) = if let Some((_, ep)) = best {
            // Hug the enemy at ~40% of slot range so the helicopter
            // engages aggressively rather than sniping from the orbit
            // edge. With TURRET_RANGE 60 that's ~24u — comfortably
            // inside fire range with room for the ±6u standoff window.
            (ep, effective_range * 0.4 + range_offset)
        } else {
            (ship_pos, HELI_ORBIT_RADIUS + range_offset)
        };
        let to_anchor = anchor - cur;
        let dist = to_anchor.length();
        let unit = to_anchor.try_normalize().unwrap_or(Vec2::Y);
        let target_pos = if dist > anchor_range + 6.0 {
            // Approach, but offset by the orbit-direction perp so
            // multiple helis arrive at the anchor from different
            // angles rather than stacking on the same vector.
            let perp = Vec2::new(-unit.y * orbit_sign, unit.x * orbit_sign);
            anchor + perp * (heli.owner_slot as f32 * 4.0)
        } else if dist < anchor_range - 6.0 {
            cur - unit * 20.0 // back off
        } else {
            // Perpendicular orbit so the heli keeps moving even when
            // it's at the right standoff distance. `orbit_sign` flips
            // direction by slot parity so a pair of helis on the
            // same enemy orbit *opposite* ways instead of chasing
            // each other.
            let perp = Vec2::new(-unit.y * orbit_sign, unit.x * orbit_sign);
            cur + perp * 20.0
        };

        // Two states drive the body's heading:
        //
        // - **Attacking** (an enemy is in detect range): face the
        //   enemy directly so the nose turret is locked on. Movement
        //   vector is *decoupled* — the heli still flies toward
        //   `target_pos` (orbit/standoff math), so the body strafes
        //   sideways/backward around the enemy while keeping the nose
        //   pointed at it.
        // - **Patrolling**: face the direction of travel. Movement
        //   and heading align so the heli flies nose-first.
        let move_dir = (target_pos - cur).try_normalize().unwrap_or_else(|| {
            Vec2::new(-heli.heading.sin(), heli.heading.cos())
        });
        let desired_heading = if let Some((_, ep)) = best {
            let to_enemy = ep - cur;
            if to_enemy.length_squared() > 0.01 {
                (-to_enemy.x).atan2(to_enemy.y)
            } else {
                heli.heading
            }
        } else {
            (-move_dir.x).atan2(move_dir.y)
        };
        heli.heading = approach_angle(heli.heading, desired_heading, HELI_TURN_RATE * dt);
        let new_pos = cur + move_dir * HELI_SPEED * dt;
        tf.translation.x = new_pos.x;
        tf.translation.y = new_pos.y;
        tf.rotation = Quat::from_rotation_z(heli.heading);

        // Tick cooldown + fire when an enemy is in range.
        heli.fire_cd -= dt;
        let Some((_, ep)) = best else { continue; };
        if heli.fire_cd > 0.0 { continue; }
        // Aim-gate: only fire when the heli is actually pointed at
        // the enemy. Bullets exit along the body's forward vector,
        // so mid-turn shots would otherwise spray off-target.
        let to_enemy = ep - new_pos;
        if to_enemy.length_squared() > 0.01 {
            let desired = (-to_enemy.x).atan2(to_enemy.y);
            let delta = (heli.heading - desired + std::f32::consts::PI)
                .rem_euclid(std::f32::consts::TAU)
                - std::f32::consts::PI;
            if delta.abs() > std::f32::consts::FRAC_PI_8 {
                continue;
            }
        }

        // Bullets exit the *nose turret*, not the body centre. The
        // nose barrel is at local y = 2.7 with length 2.5, so the tip
        // sits at y ≈ 3.95 in body-local space. The body is rotated
        // to `heli.heading`, so the world-space tip is offset by that
        // distance along the body's forward vector.
        let body_forward = Vec2::new(-heli.heading.sin(), heli.heading.cos());
        let body_perp = Vec2::new(body_forward.y, -body_forward.x);

        // Twin / triple barrels = N x effective rate (same convention
        // as turret_aim_fire). Reset cooldown using slot's fire_rate.
        let barrels_n = slot_cfg.barrels.max(1) as f32;
        heli.fire_cd = 1.0 / (slot_cfg.fire_rate.max(0.1) * barrels_n);

        // Pick which rendered barrel fires this tick + map to its
        // lateral offset. Visible-barrel index → world-space muzzle
        // position. Mirrors `sync_helipad_nose_barrels` so muzzle
        // and visual line up.
        //
        //   - barrels=1 → only centre (idx 1) ever fires
        //   - barrels=2 → alternate idx 0 / idx 2
        //   - barrels=3 → cycle idx 0 → 1 → 2
        let barrels_count = slot_cfg.barrels.max(1);
        let visible_idx = match (barrels_count, heli.next_barrel) {
            (1, _) => 1,
            (2, 0) => 0,
            (2, _) => 2,
            (3, n) => n % 3,
            _      => 1,
        };
        let lateral = HELI_BARREL_LATERAL[visible_idx as usize];
        heli.next_barrel = (heli.next_barrel + 1) % barrels_count;
        // Body capsule extends to y≈3.75; barrel sits at y=3.2 with
        // length 3.5, so the tip is at y=3.2+1.75=4.95 in body-local
        // space. Bullets exit there.
        const NOSE_TIP_OFFSET: f32 = 4.95;
        let muzzle = new_pos + body_forward * NOSE_TIP_OFFSET + body_perp * lateral;
        // Bullets fly along the body's forward (= toward the enemy
        // since the body is facing it), not directly at the enemy's
        // *current* position — so a turning heli's shots arc with
        // the body rather than tele-corrected to the target.
        let dir = body_forward;
        let _ = ep;

        // Spawn a Standard-flavoured bullet — the SLOT's runes carry
        // over so all procs (Fire/Frost/Shock/etc.) work end-to-end.
        // Use the small plane-bullet meshes so the projectile reads
        // as helicopter MG fire, not main-battery.
        let bullet = commands.spawn((
            Mesh2d(em.bullet_plane_outer.clone()),
            MeshMaterial2d(pm.bullet_friendly_outer.clone()),
            Transform::from_xyz(muzzle.x, muzzle.y, 4.0)
                .with_rotation(Quat::from_rotation_z((-dir.x).atan2(dir.y))),
            Bullet {
                faction: FactionKind::Friendly,
                damage: slot_cfg.damage,
                remaining: HELI_BULLET_RANGE,
                weapon: WeaponType::Standard,
                source: Some(DamageSource::PlayerSlot(heli.owner_slot as u8)),
                runes: slot_cfg.runes,
            },
            Velocity(dir * HELI_BULLET_SPEED),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        let inner = commands.spawn((
            Mesh2d(em.bullet_plane_inner.clone()),
            MeshMaterial2d(pm.bullet_friendly.clone()),
            Transform::from_xyz(0.0, 0.0, 0.05),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(inner).insert(ChildOf(bullet));
    }
}
