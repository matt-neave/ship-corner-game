//! Allied units — autonomous friendly ships fighting alongside the player.
//!
//! Built to scale: adding a new ally type is a single-file change here,
//! mirroring how `enemy.rs` handles enemy variants.
//!
//! 1. Add a variant to `AllyVariant`.
//! 2. Add rows in `hp`, `speed`, `turn_rate`, `hull_dims`, `turret_layout`,
//!    `fire_rate`, `fire_damage`, `turret_arc_half`.
//! 3. (Optional) extend `spawn_ally`'s body-color match if the new variant
//!    shouldn't reuse `palette.hull_accent`.
//! 4. Trigger spawns from wherever (currently `setup_world` seeds one).
//!
//! Allies share `FactionKind::Friendly` so their bullets damage enemies and
//! enemy bullets damage them. They have their own marker (`Ally`) and their
//! own AI / turret-aim systems so they don't collide with the player ship's
//! input-driven movement and configurable turret slots.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;
use rand::Rng;

use crate::balance::{PLAY_LAYER, PLAY_WORLD, TURRET_PIVOT, TURRET_RANGE};
use crate::components::{Faction, FactionKind, Health, Heading, Velocity};
use crate::effects::{spawn_hit_particles, EffectMeshes, HitFx};
use crate::enemy::Enemy;
use crate::palette::PaletteMaterials;
use crate::rune::FireExtent;
use crate::ship::approach_angle;
use crate::turret::spawn_friendly_bullet;
use crate::weapon::WeaponType;

// ---------- Components / variants ----------

#[derive(Component)]
pub struct Ally {
    pub variant: AllyVariant,
    /// Wander target used when no enemy is in range.
    pub waypoint: Vec2,
    /// Time until next wander re-plan.
    pub waypoint_timer: f32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AllyVariant {
    /// Small retro pirate ship — 4 broadside cannons (2 per side).
    PirateShip,
}

impl AllyVariant {
    pub fn hp(self) -> i32 {
        match self {
            AllyVariant::PirateShip => 20,
        }
    }
    pub fn speed(self) -> f32 {
        match self {
            AllyVariant::PirateShip => 22.0,
        }
    }
    pub fn turn_rate(self) -> f32 {
        match self {
            AllyVariant::PirateShip => 1.4,
        }
    }
    /// Hull dimensions: `(width, length)`. Width drives the capsule radius;
    /// length is the long axis.
    pub fn hull_dims(self) -> (f32, f32) {
        match self {
            AllyVariant::PirateShip => (5.0, 12.0),
        }
    }
    /// Per-turret `(local_x, local_y, mount_angle_radians)` in hull frame.
    /// Mount angle is 0 = +Y forward, ±π/2 = broadside.
    pub fn turret_layout(self) -> &'static [(f32, f32, f32)] {
        use std::f32::consts::FRAC_PI_2;
        match self {
            AllyVariant::PirateShip => &[
                (-1.5,  3.0,  FRAC_PI_2), // port forward
                (-1.5, -3.0,  FRAC_PI_2), // port aft
                ( 1.5,  3.0, -FRAC_PI_2), // stbd forward
                ( 1.5, -3.0, -FRAC_PI_2), // stbd aft
            ],
        }
    }
    pub fn fire_rate(self) -> f32 {
        match self {
            AllyVariant::PirateShip => 2.0,
        }
    }
    pub fn fire_damage(self) -> i32 {
        match self {
            AllyVariant::PirateShip => 1,
        }
    }
    /// Half-arc per turret (radians).
    pub fn turret_arc_half(self) -> f32 {
        match self {
            // ±60° — generous broadside arc that lets the forward + aft pair
            // share targets without rigidly committing to one quadrant.
            AllyVariant::PirateShip => std::f32::consts::FRAC_PI_3,
        }
    }
    /// Diameter to use for the bullet/turret hit-radius approximation.
    pub fn hit_radius(self) -> f32 {
        match self {
            AllyVariant::PirateShip => 3.0,
        }
    }
}

#[derive(Component)]
pub struct AllyTurret {
    pub barrel_angle: f32,
    pub mount_angle: f32,
    pub fire_cd: f32,
    pub variant: AllyVariant,
}

/// Per-variant hull material lookup. Lives here (not in `palette.rs`) so
/// `AllyVariant` stays the only source coupled to ally identities — adding
/// a new variant means adding an arm here, not threading it through palette.
impl PaletteMaterials {
    pub fn ally_hull_for(&self, variant: AllyVariant) -> &Handle<ColorMaterial> {
        match variant {
            AllyVariant::PirateShip => &self.pirate_hull,
        }
    }
}

// ---------- Spawn helper ----------

pub fn spawn_ally(
    commands: &mut Commands,
    pm: &PaletteMaterials,
    em: &EffectMeshes,
    meshes: &mut Assets<Mesh>,
    pos: Vec2,
    heading: f32,
    variant: AllyVariant,
) {
    let (hull_w, hull_h) = variant.hull_dims();
    let hull_mesh = meshes.add(Capsule2d::new(hull_w / 2.0, hull_h - hull_w));
    let dir = Vec2::new(-heading.sin(), heading.cos());

    let body_mat = pm.ally_hull_for(variant).clone();
    let ship = commands.spawn((
        Mesh2d(hull_mesh),
        MeshMaterial2d(body_mat.clone()),
        Transform::from_xyz(pos.x, pos.y, 1.0)
            .with_rotation(Quat::from_rotation_z(heading)),
        Visibility::Inherited,
        Ally { variant, waypoint: Vec2::ZERO, waypoint_timer: 0.0 },
        Faction(FactionKind::Friendly),
        Health(variant.hp()),
        Velocity(dir * variant.speed()),
        Heading(heading),
        HitFx::new(body_mat),
        FireExtent(Vec2::new(hull_w * 0.5, hull_h * 0.5)),
        RenderLayers::layer(PLAY_LAYER),
    )).id();

    for &(lx, ly, mount) in variant.turret_layout() {
        let turret = commands.spawn((
            Mesh2d(em.ally_turret_base.clone()),
            MeshMaterial2d(pm.turret.clone()),
            Transform::from_xyz(lx, ly, 0.1)
                .with_rotation(Quat::from_rotation_z(mount)),
            AllyTurret {
                barrel_angle: mount,
                mount_angle: mount,
                fire_cd: 0.0,
                variant,
            },
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(turret).insert(ChildOf(ship));

        // Single barrel child, pointing along turret +Y (mount-relative).
        let barrel = commands.spawn((
            Mesh2d(em.ally_turret_barrel.clone()),
            MeshMaterial2d(pm.turret.clone()),
            Transform::from_xyz(0.0, 2.2, 0.05),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(barrel).insert(ChildOf(turret));
    }
}

// ---------- Systems ----------

/// Movement AI — engage the nearest enemy at moderate range; wander toward
/// random waypoints when no enemy is in sight. Same shape as the friendly
/// ship's tactical mode but tuned by `AllyVariant` stats.
pub fn ally_ai(
    time: Res<Time>,
    enemies: Query<&Transform, (With<Enemy>, Without<Ally>)>,
    mut allies: Query<(&mut Transform, &mut Velocity, &mut Heading, &mut Ally), Without<Enemy>>,
) {
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();

    for (mut tf, mut vel, mut heading, mut ally) in &mut allies {
        let pos = tf.translation.truncate();
        let speed = ally.variant.speed();
        let turn = ally.variant.turn_rate();

        // Find nearest enemy.
        let mut nearest: Option<(f32, Vec2)> = None;
        for etf in &enemies {
            let ep = etf.translation.truncate();
            let d = ep.distance(pos);
            if nearest.map_or(true, |(bd, _)| d < bd) {
                nearest = Some((d, ep));
            }
        }

        let target = if let Some((d, ep)) = nearest {
            // Engage: orbit at desired range so broadside turrets can bear.
            let to = ep - pos;
            let unit = to.normalize_or_zero();
            let desired_range = TURRET_RANGE * 0.7;
            if d > desired_range + 8.0 {
                ep
            } else if d < desired_range - 8.0 {
                pos - unit * 30.0
            } else {
                let perp = Vec2::new(-unit.y, unit.x);
                pos + perp * 30.0
            }
        } else {
            // No enemies — wander between random waypoints.
            ally.waypoint_timer -= dt;
            if ally.waypoint_timer <= 0.0 {
                ally.waypoint_timer = rng.gen_range(2.5..5.5);
                ally.waypoint = Vec2::new(
                    rng.gen_range(-PLAY_WORLD * 0.35..PLAY_WORLD * 0.35),
                    rng.gen_range(-PLAY_WORLD * 0.35..PLAY_WORLD * 0.35),
                );
            }
            ally.waypoint
        };

        // Keep target inside the play area so we don't crash the wall.
        let margin = 10.0;
        let bound = PLAY_WORLD / 2.0 - margin;
        let target = Vec2::new(target.x.clamp(-bound, bound), target.y.clamp(-bound, bound));

        let to = target - pos;
        if to.length_squared() > 1.0 {
            let desired = (-to.x).atan2(to.y);
            heading.0 = approach_angle(heading.0, desired, turn * dt);
        }
        let dir = Vec2::new(-heading.0.sin(), heading.0.cos());
        vel.0 = dir * speed;
        tf.rotation = Quat::from_rotation_z(heading.0);
    }
}

/// Per-ally-turret targeting + aim + fire. Shape mirrors the player's
/// `turret_aim_fire` but without the per-slot config / weapon-type branching:
/// allies fire a single Standard bullet at a fixed rate per variant.
pub fn ally_turret_aim_fire(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    allies: Query<(&Transform, &Heading), (With<Ally>, Without<Enemy>, Without<AllyTurret>)>,
    enemies: Query<&Transform, (With<Enemy>, Without<Ally>, Without<AllyTurret>)>,
    mut turrets: Query<
        (&ChildOf, &mut AllyTurret, &mut Transform),
        (Without<Ally>, Without<Enemy>),
    >,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();

    for (parent, mut turret, mut tf) in &mut turrets {
        let Ok((ally_tf, ally_heading)) = allies.get(parent.0) else { continue; };
        let ally_pos = ally_tf.translation.truncate();
        let ally_h = ally_heading.0;
        turret.fire_cd -= dt;

        // World position of this turret (parent rotation × local offset).
        let local = tf.translation.truncate();
        let cos_h = ally_h.cos();
        let sin_h = ally_h.sin();
        let world_off = Vec2::new(
            local.x * cos_h - local.y * sin_h,
            local.x * sin_h + local.y * cos_h,
        );
        let turret_world = ally_pos + world_off;

        // Find best target inside the turret's arc + range.
        let arc_half = turret.variant.turret_arc_half();
        let mut best: Option<(f32, Vec2)> = None;
        for etf in &enemies {
            let ep = etf.translation.truncate();
            let to = ep - turret_world;
            let d = to.length();
            if d > TURRET_RANGE { continue; }
            let world_angle = (-to.x).atan2(to.y);
            let mut local_angle = world_angle - ally_h;
            local_angle = (local_angle + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                - std::f32::consts::PI;
            let mut off = local_angle - turret.mount_angle;
            off = (off + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                - std::f32::consts::PI;
            if off.abs() > arc_half { continue; }
            if best.map_or(true, |(bd, _)| d < bd) {
                best = Some((d, ep));
            }
        }

        let desired_local = if let Some((_, ep)) = best {
            let to = ep - turret_world;
            let world_angle = (-to.x).atan2(to.y);
            let mut la = world_angle - ally_h;
            la = (la + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                - std::f32::consts::PI;
            la
        } else {
            turret.mount_angle
        };

        turret.barrel_angle = approach_angle(turret.barrel_angle, desired_local, TURRET_PIVOT * dt);
        tf.rotation = Quat::from_rotation_z(turret.barrel_angle);

        // Fire when aimed.
        if best.is_some() {
            let aim_err = (turret.barrel_angle - desired_local).abs();
            if aim_err < 0.1 && turret.fire_cd <= 0.0 {
                turret.fire_cd = 1.0 / turret.variant.fire_rate().max(0.1);
                let total_angle = ally_h + turret.barrel_angle;
                let barrel_forward = Vec2::new(-total_angle.sin(), total_angle.cos());
                // Spawn just past the barrel tip (~ length 3 from base + a bit).
                let muzzle_pos = turret_world + barrel_forward * 4.0;
                spawn_friendly_bullet(
                    &mut commands,
                    &em,
                    &pm.bullet_friendly_outer,
                    &pm.bullet_friendly,
                    muzzle_pos,
                    barrel_forward,
                    WeaponType::Standard,
                    turret.variant.fire_damage(),
                    None, // not a player slot — skip damage-stat crediting
                    TURRET_RANGE,
                    None, // ally turrets don't currently carry runes
                );
            }
        }
    }
}

/// Despawn allies that have hit 0 HP, with a destruction burst. Decoupled
/// from the bullet collision system so we can keep the bullet-vs-friendly
/// query simple (it just decrements HP).
pub fn ally_death_check(
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    allies: Query<(Entity, &Transform, &Ally, &Health)>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let mut rng = rand::thread_rng();
    for (e, tf, ally, h) in &allies {
        if h.0 <= 0 {
            let pos = tf.translation.truncate();
            // Use the ally's own hull color in the death burst so a Pirate
            // explodes brown, future variants explode their own color.
            spawn_hit_particles(&mut commands, &em, pm.ally_hull_for(ally.variant), pos, 18, 80.0,  &mut rng);
            spawn_hit_particles(&mut commands, &em, &pm.bullet_friendly,            pos, 10, 100.0, &mut rng);
            commands.entity(e).despawn();
        }
    }
}

/// `Ally::hit_radius` exposed as a free function so collision systems don't
/// need to pull the variant out themselves.
pub fn ally_hit_radius(ally: &Ally) -> f32 {
    ally.variant.hit_radius()
}
