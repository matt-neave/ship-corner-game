//! HeliPad slot: doesn't fire from the deck. Maintains one persistent
//! `Helicopter` entity per equipped HeliPad slot — a free-flying entity
//! that orbits the ship and fires forward at the closest enemy.
//!
//! Lifecycle invariant maintained by `sync_helipad_helicopters`:
//! "exactly one Helicopter exists per equipped HeliPad slot".

use bevy::prelude::*;
use bevy::render::view::RenderLayers;

use crate::balance::{PLAY_LAYER, TURRET_RANGE};
use crate::bullet::{Bullet, DamageSource};
use crate::components::{Faction, FactionKind, Friendly, Health, Velocity};
use crate::effects::EffectMeshes;
use crate::enemy::Enemy;
use crate::palette::PaletteMaterials;
use crate::ship::approach_angle;
use crate::weapon::WeaponType;

use super::TurretConfig;

/// Comfortably outside the ship's hull and inside `TURRET_RANGE` so
/// the heli stays in visible play space.
pub const HELI_ORBIT_RADIUS: f32 = 30.0;
/// Constant flight speed. The helicopter never snaps — it moves forward
/// and turns to align its nose with the desired direction.
pub const HELI_SPEED: f32 = 28.0;
/// Slow enough that the heli has visible inertia.
pub const HELI_TURN_RATE: f32 = 2.5;
pub const HELI_BULLET_SPEED: f32 = 180.0;
/// Lateral offset per nose barrel (port / centre / stbd). The firing
/// path uses the same values so muzzle visuals line up with the bullets.
pub const HELI_BARREL_LATERAL: [f32; 3] = [-1.4, 0.0, 1.4];
pub const HELI_BULLET_RANGE: f32 = 120.0;

/// Free-flying helicopter spawned by an equipped `HeliPad` slot. Owns
/// its own heading + fire cooldown so each pad's heli is independent.
#[derive(Component)]
pub struct Helicopter {
    /// Slot index that launched this helicopter.
    pub owner_slot: usize,
    pub fire_cd: f32,
    /// World-space heading (radians). Tracked separately from
    /// `Transform::rotation` so we can approach the desired heading at
    /// a fixed turn rate without extracting the angle from a quaternion
    /// each frame.
    pub heading: f32,
    /// Round-robin index for `barrels > 1` (mirrors `TurretSlot`).
    pub next_barrel: u8,
}

/// One nose-barrel rectangle. Three are spawned per helicopter (idx
/// 0/1/2 = port/centre/stbd); `sync_helipad_nose_barrels` toggles
/// visibility per slot's `barrels` count.
#[derive(Component)]
pub struct HeliNoseBarrel { pub heli: Entity, pub idx: u8 }

/// Marks the spinning rotor child of a helicopter.
#[derive(Component)]
pub struct HeliRotor;

/// Maintain "one helicopter per equipped HeliPad slot". Despawns
/// orphans first, then spawns missing helis. Runs before
/// `helicopter_ai` so a freshly spawned heli ticks this frame.
pub fn sync_helipad_helicopters(
    mut commands: Commands,
    cfg: Res<TurretConfig>,
    pm: Option<Res<PaletteMaterials>>,
    mut meshes: ResMut<Assets<Mesh>>,
    // LocalPlayer disambiguates from MP's remote-peer ship which is
    // also `Friendly`. `single()` would bail with two friendlies.
    ship_q: Query<&Transform, (With<crate::components::LocalPlayer>, Without<Helicopter>)>,
    helis: Query<(Entity, &Helicopter)>,
) {
    let Some(pm) = pm else { return; };

    for (e, heli) in &helis {
        let slot = cfg.slots.get(heli.owner_slot).copied().unwrap_or_default();
        let still_valid = slot.equipped && matches!(slot.weapon, WeaponType::HeliPad);
        if !still_valid {
            commands.entity(e).despawn();
        }
    }

    let Ok(ship_tf) = ship_q.single() else { return; };
    let ship_pos = ship_tf.translation.truncate();

    for (idx, slot) in cfg.slots.iter().enumerate() {
        if !slot.equipped { continue; }
        if !matches!(slot.weapon, WeaponType::HeliPad) { continue; }
        let already = helis.iter().any(|(_, h)| h.owner_slot == idx);
        if already { continue; }

        let phase = (idx as f32) * std::f32::consts::TAU / 8.0;
        let init_pos = ship_pos
            + Vec2::new(phase.cos(), phase.sin()) * HELI_ORBIT_RADIUS;
        let outward = (init_pos - ship_pos).try_normalize().unwrap_or(Vec2::Y);
        let init_heading = (-outward.x).atan2(outward.y);

        let heli = spawn_helicopter_visual(&mut commands, &pm, &mut meshes, init_pos, init_heading);
        commands.entity(heli).insert(Helicopter {
            owner_slot: idx,
            fire_cd: 0.0,
            heading: init_heading,
            next_barrel: 0,
        });
    }
}

/// Spawn the full helicopter visual hierarchy (body, tail boom +
/// rotor, canopy, nose base + barrels, main rotors) and return the
/// root entity. Pure visuals — no AI / fire / gameplay components.
///
/// Callers layer their gameplay tags on top:
/// - `sync_helipad_helicopters` (owner side) → `insert(Helicopter { … })`
/// - `apply_peer_units_snapshot` (mirror side) → `insert(PeerUnitMirror { … })`
///
/// Single source of truth for the helicopter look — any future
/// chassis change propagates to peer mirrors automatically.
pub fn spawn_helicopter_visual(
    commands: &mut Commands,
    pm: &PaletteMaterials,
    meshes: &mut Assets<Mesh>,
    pos: Vec2,
    heading: f32,
) -> Entity {
    let body_mesh = meshes.add(Capsule2d::new(2.0, 2.5));
    let body_shadow_mesh = body_mesh.clone();
    let rotor_mesh = meshes.add(Rectangle::new(8.0, 0.8));
    let nose_base_mesh = meshes.add(Circle::new(1.7));
    let nose_barrel_mesh = meshes.add(Rectangle::new(1.0, 3.5));
    let body_mat = pm.helipad_deck.clone();
    let nose_mat = pm.turret.clone();
    let rotor_mat = pm.turret.clone();
    let tail_mat = pm.helipad_deck.clone();
    let canopy_mat = pm.ally_flag.clone();
    let tail_boom_mesh = meshes.add(Rectangle::new(0.7, 3.6));
    let tail_rotor_mesh = meshes.add(Rectangle::new(2.6, 0.4));
    let canopy_mesh = meshes.add(Circle::new(0.7));

    let heli = commands.spawn((
        Mesh2d(body_mesh),
        MeshMaterial2d(body_mat),
        Transform::from_xyz(pos.x, pos.y, 2.5)
            .with_rotation(Quat::from_rotation_z(heading)),
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    // Airborne drop-shadow — bigger world-space offset than
    // sea-level entities to fake altitude. The shadow tracks the
    // body capsule only (not tail boom / rotors) so the
    // silhouette reads as the chopper's mass rather than the
    // whole fuselage, matching the top-down arcade convention.
    crate::shadow::spawn_for_with_offset(
        commands,
        pm.shadow.clone(),
        body_shadow_mesh,
        heli,
        1.0,
        crate::shadow::SHADOW_OFFSET_AIR,
        pos,
        Quat::from_rotation_z(heading),
    );

    let tail_boom = commands.spawn((
        Mesh2d(tail_boom_mesh),
        MeshMaterial2d(tail_mat.clone()),
        Transform::from_xyz(0.0, -4.6, 0.02),
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    commands.entity(tail_boom).insert(ChildOf(heli));

    let tail_rotor = commands.spawn((
        Mesh2d(tail_rotor_mesh),
        MeshMaterial2d(rotor_mat.clone()),
        Transform::from_xyz(0.0, -6.7, 0.03),
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    commands.entity(tail_rotor).insert(ChildOf(heli));

    let canopy = commands.spawn((
        Mesh2d(canopy_mesh),
        MeshMaterial2d(canopy_mat),
        Transform::from_xyz(0.0, 0.4, 0.03),
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    commands.entity(canopy).insert(ChildOf(heli));

    let nose_base = commands.spawn((
        Mesh2d(nose_base_mesh),
        MeshMaterial2d(nose_mat.clone()),
        Transform::from_xyz(0.0, 1.5, 0.04),
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    commands.entity(nose_base).insert(ChildOf(heli));

    // Three nose barrels — `sync_helipad_nose_barrels` toggles
    // their visibility on the owner side based on `barrels` tier.
    // For mirrors, leave them all visible (tier sync would need
    // the SlotCfg.barrels in the snapshot; current snapshot is
    // pos+rot only).
    for bi in 0u8..3 {
        let lateral = HELI_BARREL_LATERAL[bi as usize];
        let nose_barrel = commands.spawn((
            Mesh2d(nose_barrel_mesh.clone()),
            MeshMaterial2d(nose_mat.clone()),
            Transform::from_xyz(lateral, 3.2, 0.05),
            RenderLayers::layer(PLAY_LAYER),
            Visibility::Hidden,
            HeliNoseBarrel { heli, idx: bi },
        )).id();
        commands.entity(nose_barrel).insert(ChildOf(heli));
    }

    // Two rotors crossed in an X — 90° offset gives a 4-bladed look.
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

    heli
}

/// Toggle each helicopter's nose-barrel visibility from the owning
/// slot's `barrels` count. Mirrors the rule in `sync_turret_config`.
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
/// heli around the ship (or a chased enemy), and fires forward when
/// aim is on-target.
pub fn helicopter_ai(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    cfg: Res<TurretConfig>,
    stats: Res<crate::stats::PlayerStats>,
    synergies: Res<crate::synergy::Synergies>,
    ship_q: Query<&Transform, (With<crate::components::LocalPlayer>, Without<Helicopter>, Without<HeliRotor>, Without<Enemy>)>,
    enemies: Query<(&Transform, &Faction, &Health), (With<Enemy>, Without<Helicopter>)>,
    mut helis: Query<(&mut Transform, &mut Helicopter), Without<HeliRotor>>,
    mut rotors: Query<&mut Transform, (With<HeliRotor>, Without<Helicopter>, Without<Enemy>, Without<Friendly>)>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    let Ok(ship_tf) = ship_q.single() else { return; };
    let ship_pos = ship_tf.translation.truncate();

    for mut rotf in &mut rotors {
        rotf.rotate_z(8.0 * dt);
    }

    let enemy_snap: Vec<(Vec2, i32)> = enemies
        .iter()
        .filter(|(_, fac, _)| fac.0 == FactionKind::Enemy)
        .map(|(etf, _, hp)| (etf.translation.truncate(), hp.0))
        .collect();

    for (mut tf, mut heli) in &mut helis {
        let slot_cfg = cfg.slots.get(heli.owner_slot).copied().unwrap_or_default();
        if !slot_cfg.equipped || !matches!(slot_cfg.weapon, WeaponType::HeliPad) {
            continue;
        }
        // Flatten the slot's `[Option<Rune>; 3]` config to a `Vec<Rune>`
        // once per slot for the rune-aware picker + hustle math.
        let slot_runes: Vec<crate::rune::Rune> =
            slot_cfg.runes.iter().copied().flatten().collect();

        let cur = tf.translation.truncate();
        let effective_range = TURRET_RANGE * stats.range_mult();

        // Rune priority measured relative to the SHIP (Furthest =
        // furthest from ship, not from this helicopter). Per-slot
        // offset keeps multiple helis from converging on one spot.
        // Helis don't carry their own carousel cursor — passing
        // `None` makes a Carousel rune degenerate to "first
        // candidate" instead of crashing. Future work could thread
        // a per-heli cycle counter through here if Carousel becomes
        // a meaningful HeliPad slot pick.
        let best_pos = crate::weapon::pick_target(
            &enemy_snap, ship_pos, cur, &slot_runes, None,
        )
        .map(|p| p + crate::weapon::offset_for_slot(heli.owner_slot));
        let best: Option<(f32, Vec2)> = best_pos.map(|p| (p.distance(cur), p));

        // Per-slot stagger: even slots orbit CCW, odd CW, plus a small
        // range offset so two HeliPads don't trace identical paths.
        let orbit_sign = if heli.owner_slot % 2 == 0 { 1.0 } else { -1.0 };
        let range_offset = (heli.owner_slot as f32) * 1.8;
        let (anchor, anchor_range) = if let Some((_, ep)) = best {
            // Hug the enemy at ~40% of slot range so the helicopter
            // engages aggressively instead of sniping from the edge.
            (ep, effective_range * 0.4 + range_offset)
        } else {
            (ship_pos, HELI_ORBIT_RADIUS + range_offset)
        };
        let to_anchor = anchor - cur;
        let dist = to_anchor.length();
        let unit = to_anchor.try_normalize().unwrap_or(Vec2::Y);
        let target_pos = if dist > anchor_range + 6.0 {
            // Approach from a perp-offset so multiple helis arrive at
            // the anchor from different angles.
            let perp = Vec2::new(-unit.y * orbit_sign, unit.x * orbit_sign);
            anchor + perp * (heli.owner_slot as f32 * 4.0)
        } else if dist < anchor_range - 6.0 {
            cur - unit * 20.0
        } else {
            // Perpendicular orbit so the heli keeps moving even at the
            // right standoff. `orbit_sign` flips by slot parity so a
            // pair on the same enemy orbits opposite ways.
            let perp = Vec2::new(-unit.y * orbit_sign, unit.x * orbit_sign);
            cur + perp * 20.0
        };

        // Heading is decoupled from movement when attacking — the
        // body faces the enemy (nose turret locked on) while the heli
        // strafes sideways around it.
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
        // Hustle rune: per-slot speed bonus on top of the global
        // Autonomous synergy. Multiplies with the synergy mult, so
        // a 1-Hustle (+100%) HeliPad inside a 4-Autonomous synergy
        // (+40%) ends up at 1.4 × 2.0 = 2.8× speed.
        let hustle = crate::rune::hustle_speed_mult(
            &slot_runes,
            stats.rune_damage_mult(),
        );
        let speed = HELI_SPEED * synergies.autonomous_speed_mult() * hustle;
        let new_pos = cur + move_dir * speed * dt;
        tf.translation.x = new_pos.x;
        tf.translation.y = new_pos.y;
        tf.rotation = Quat::from_rotation_z(heli.heading);

        heli.fire_cd -= dt;
        let Some((_, ep)) = best else { continue; };
        if heli.fire_cd > 0.0 { continue; }
        // Aim-gate. The bullet's flight direction is recomputed
        // muzzle → enemy below, so the heli technically *could*
        // fire 360°, but a generous gate keeps shots looking like
        // they emerge from the front of the body rather than
        // launching backwards from a heli still turning. PI/3 (60°)
        // is wide enough that a mid-orbit heli almost never has to
        // skip a shot, which was the original "heli misses
        // everything" symptom — the gate was at PI/8 (22.5°) and
        // the heli's orbit kept its body offset from the target by
        // more than that for long stretches.
        let to_enemy = ep - new_pos;
        if to_enemy.length_squared() > 0.01 {
            let desired = (-to_enemy.x).atan2(to_enemy.y);
            let delta = (heli.heading - desired + std::f32::consts::PI)
                .rem_euclid(std::f32::consts::TAU)
                - std::f32::consts::PI;
            if delta.abs() > std::f32::consts::FRAC_PI_3 {
                continue;
            }
        }

        let body_forward = Vec2::new(-heli.heading.sin(), heli.heading.cos());
        let body_perp = Vec2::new(body_forward.y, -body_forward.x);

        let barrels_n = slot_cfg.barrels.max(1) as f32;
        heli.fire_cd = 1.0 / (slot_cfg.fire_rate.max(0.1) * barrels_n);

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
        // length 3.5, so the tip is at y=4.95 in body-local space.
        const NOSE_TIP_OFFSET: f32 = 4.95;
        let muzzle = new_pos + body_forward * NOSE_TIP_OFFSET + body_perp * lateral;
        // Aim the bullet straight at the enemy from the muzzle. The
        // aim-gate above guarantees the body is roughly pointing at
        // the target (within ~22°), so the bullet's exit angle stays
        // close to the visible nose direction while compensating for
        // mid-turn drift + sideways orbital motion that would
        // otherwise make the heli "spray".
        let dir = (ep - muzzle).try_normalize().unwrap_or(body_forward);

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
                // Flatten the SlotCfg's 3-fixed sockets to a Vec —
                // helicopter bullets snapshot the player's config
                // directly (HeliPad is its own firing path, not
                // routed through `sync_turret_config`'s merge).
                runes: slot_cfg.runes.iter().copied().flatten().collect(),
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
