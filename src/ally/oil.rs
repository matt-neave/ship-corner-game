//! Oil tanker behavior: two-phase spray → ignite → burn loop driven by
//! `OilTankerCycle`. Faction-agnostic — ally tankers burn enemies, boss
//! tankers burn allies + the player.
//!
//! 1. Spraying: every `OIL_DROP_INTERVAL`s a fan of fresh `OilSlick`s
//!    sprays out the stern across a cone, with per-slick scale +
//!    Z-rotation jitter so the swath reads as one continuous pool.
//! 2. Burning: every existing slick whose `target_faction` matches the
//!    tanker is tagged with `OilOnFire` for `OIL_BURN_DURATION`. While
//!    on fire, the slick ticks AOE damage every `OIL_BURN_TICK`s to
//!    every faction-matched unit inside its effective radius.
//!
//! Slicks ignite *en masse* by faction-match rather than by owner
//! tracking — two friendly tankers crossing paths will set each
//! other's pools on fire when either ignites, which reads as a single
//! pooled hazard belt catching all at once.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;
use rand::Rng;

use crate::balance::PLAY_LAYER;
use crate::components::{Faction, FactionKind, Health, Heading, Velocity};
use crate::effects::{EffectMeshes, HitFx, HitParticle};
use crate::palette::PaletteMaterials;

use super::ShipClass;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OilCyclePhase {
    Spraying,
    Burning,
    Cooldown,
}

/// State machine driving the OilTanker spray → ignite → burn → idle
/// loop. Lives on the tanker itself; despawning the tanker doesn't
/// extinguish already-laid slicks.
#[derive(Component)]
pub struct OilTankerCycle {
    pub phase: OilCyclePhase,
    pub timer: f32,
    /// Drop-interval cooldown — only ticks while in `Spraying`.
    pub drop_cd: f32,
    /// Faction whose units take damage from this tanker's burning oil.
    /// Cached at spawn so a sunk tanker's lingering slicks still know
    /// who to hurt.
    pub target_faction: FactionKind,
}

/// One free-standing oil pool. Persists in world space — outlives the
/// tanker if it sinks. Untagged pools are visually-dark and harmless;
/// ignition adds an `OilOnFire` component that drives AOE-burn ticks.
#[derive(Component)]
pub struct OilSlick {
    pub target_faction: FactionKind,
    pub lifetime: f32,
    /// Seconds since spawn. Drives the spread-in animation: visual +
    /// damage radius eases from `OIL_SPREAD_START_SCALE * target_radius`
    /// up to `target_radius` over `OIL_SPREAD_DURATION`.
    pub age: f32,
    pub target_radius: f32,
}

/// Burning state on an `OilSlick`. Ticks AOE damage on a fixed cadence
/// to every faction-mismatched unit inside `OIL_BURN_RADIUS`.
#[derive(Component)]
pub struct OilOnFire {
    pub remaining: f32,
    pub tick_cd: f32,
}

pub const OIL_SPRAY_DURATION:   f32 = 3.0;
const OIL_BURN_DURATION:    f32 = 3.0;
const OIL_COOLDOWN:         f32 = 0.5;
const OIL_DROP_INTERVAL:    f32 = 0.25;
const OIL_SLICK_LIFETIME:   f32 = 8.0;
const OIL_BURN_RADIUS:      f32 = 8.0;
const OIL_BURN_DAMAGE:      i32 = 1;
const OIL_BURN_TICK:        f32 = 0.3;
/// Base radius of one oil pool (before per-spawn scale jitter). Tuned
/// with `OIL_FAN_COUNT` so the laid-down area reads as one continuous
/// pool with no gaps as the tanker moves forward.
const OIL_SLICK_RADIUS:     f32 = 3.0;
/// Slicks per `OIL_DROP_INTERVAL` tick — scaled with cone area so the
/// fan stays seamless.
const OIL_FAN_COUNT:        u32 = 9;
/// Half-angle of the spray cone behind the tanker (~±49° → ~100° fan).
const OIL_FAN_HALF_ANGLE:   f32 = 0.85;
/// Stern offset range — width gives the swath visible depth as well as
/// width.
const OIL_FAN_DIST_MIN:     f32 = 7.0;
const OIL_FAN_DIST_MAX:     f32 = 14.0;
/// Seconds for a freshly-laid slick to spread to its full radius. Visual
/// eases on an out-quadratic so the pool settles instead of snapping.
const OIL_SPREAD_DURATION:  f32 = 1.5;
/// Initial visual scale (fraction of target radius) at spawn.
pub const OIL_SPREAD_START_SCALE: f32 = 0.3;

pub fn oil_tanker_cycle(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    mut tankers: Query<(&Transform, &Heading, &mut OilTankerCycle)>,
    mut slicks: Query<(Entity, &OilSlick), Without<OilOnFire>>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();

    for (tf, heading, mut cycle) in &mut tankers {
        cycle.timer -= dt;

        match cycle.phase {
            OilCyclePhase::Spraying => {
                cycle.drop_cd -= dt;
                if cycle.drop_cd <= 0.0 {
                    cycle.drop_cd = OIL_DROP_INTERVAL;

                    let pos = tf.translation.truncate();
                    let h = heading.0;
                    let forward = Vec2::new(-h.sin(), h.cos());
                    let astern = -forward;

                    for i in 0..OIL_FAN_COUNT {
                        let t = if OIL_FAN_COUNT == 1 {
                            0.0
                        } else {
                            i as f32 / (OIL_FAN_COUNT - 1) as f32
                        };
                        let base_angle =
                            -OIL_FAN_HALF_ANGLE + t * (2.0 * OIL_FAN_HALF_ANGLE);
                        let angle = base_angle + rng.gen_range(-0.08..0.08);

                        let (s, c) = angle.sin_cos();
                        let dir = Vec2::new(
                            astern.x * c - astern.y * s,
                            astern.x * s + astern.y * c,
                        );
                        let dist = rng.gen_range(OIL_FAN_DIST_MIN..OIL_FAN_DIST_MAX);
                        let p = pos + dir * dist;

                        let scale_jitter = rng.gen_range(0.7..1.4);
                        let z_rot = rng.gen_range(0.0..std::f32::consts::TAU);

                        spawn_oil_slick(
                            &mut commands, &em, &pm, p,
                            cycle.target_faction,
                            scale_jitter, z_rot,
                        );
                    }
                }

                if cycle.timer <= 0.0 {
                    // Ignite by faction-match — simplest way to handle
                    // multi-tanker overlaps and a tanker dying mid-cycle.
                    for (slick_e, slick) in &mut slicks {
                        if slick.target_faction != cycle.target_faction {
                            continue;
                        }
                        commands.entity(slick_e).insert(OilOnFire {
                            remaining: OIL_BURN_DURATION,
                            tick_cd: 0.0,
                        });
                    }
                    cycle.phase = OilCyclePhase::Burning;
                    cycle.timer = OIL_BURN_DURATION;
                }
            }
            OilCyclePhase::Burning => {
                if cycle.timer <= 0.0 {
                    cycle.phase = OilCyclePhase::Cooldown;
                    cycle.timer = OIL_COOLDOWN;
                }
            }
            OilCyclePhase::Cooldown => {
                if cycle.timer <= 0.0 {
                    cycle.phase = OilCyclePhase::Spraying;
                    cycle.timer = OIL_SPRAY_DURATION;
                    cycle.drop_cd = 0.0;
                }
            }
        }
    }
}

/// Spawn one oil pool. Reuses the shared `particle` mesh — size is
/// driven via `Transform::scale` so spawning is allocation-free.
fn spawn_oil_slick(
    commands: &mut Commands,
    em: &EffectMeshes,
    pm: &PaletteMaterials,
    pos: Vec2,
    target_faction: FactionKind,
    scale_jitter: f32,
    z_rot: f32,
) {
    let target_radius = OIL_SLICK_RADIUS * scale_jitter;
    let start_r = target_radius * OIL_SPREAD_START_SCALE;
    commands.spawn((
        Mesh2d(em.particle.clone()),
        MeshMaterial2d(pm.oil_slick.clone()),
        Transform {
            translation: Vec3::new(pos.x, pos.y, 0.5),
            rotation: Quat::from_rotation_z(z_rot),
            scale: Vec3::new(start_r, start_r, 1.0),
        },
        OilSlick {
            target_faction,
            lifetime: OIL_SLICK_LIFETIME,
            age: 0.0,
            target_radius,
        },
        RenderLayers::layer(PLAY_LAYER),
    ));
}

/// Per-frame grow-in animation. Eases an out-quadratic factor
/// `start_scale → 1.0` over `OIL_SPREAD_DURATION`, multiplies by
/// `target_radius`, and (if on fire) layers a tiny sinusoidal flame-base
/// shimmer. Damage radius in `oil_slick_burn_tick` reads from this
/// scale so a still-spreading slick can't pre-emptively burn off-disc.
pub fn oil_slick_grow_tick(
    time: Res<Time>,
    mut slicks: Query<(&mut Transform, &mut OilSlick, Option<&OilOnFire>)>,
) {
    let dt = time.delta_secs();
    let t_global = time.elapsed_secs();
    for (mut tf, mut slick, fire_opt) in &mut slicks {
        slick.age += dt;
        let raw = (slick.age / OIL_SPREAD_DURATION).clamp(0.0, 1.0);
        let eased = 1.0 - (1.0 - raw).powi(2);
        let factor = OIL_SPREAD_START_SCALE
            + (1.0 - OIL_SPREAD_START_SCALE) * eased;
        let pulse = if fire_opt.is_some() {
            1.0 + 0.05 * (t_global * 8.0).sin()
        } else {
            1.0
        };
        let r = slick.target_radius * factor * pulse;
        tf.scale = Vec3::new(r, r, 1.0);
    }
}

pub fn oil_slick_burn_tick(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    _materials: Res<Assets<ColorMaterial>>,
    player_stats: Res<crate::stats::PlayerStats>,
    mut victims: Query<(
        Entity, &Transform, &Faction, &mut Health, &mut HitFx,
        Option<&mut crate::stats::Shield>,
        Has<crate::components::LocalPlayer>,
        Has<crate::components::Friendly>,
    )>,
    mut slicks: Query<(
        Entity,
        &Transform,
        &mut OilSlick,
        Option<&mut OilOnFire>,
        &mut MeshMaterial2d<ColorMaterial>,
    )>,
    mut stats: ResMut<crate::ui::DamageStats>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();

    let victim_snap: Vec<(Entity, Vec2, FactionKind)> = victims
        .iter()
        .map(|(e, t, f, _, _, _, _, _)| (e, t.translation.truncate(), f.0))
        .collect();

    for (slick_e, slick_tf, mut slick, fire_opt, mut mat) in &mut slicks {
        slick.lifetime -= dt;
        if slick.lifetime <= 0.0 {
            commands.entity(slick_e).despawn();
            continue;
        }

        let Some(mut fire) = fire_opt else { continue; };

        // Swap to the flame material on first burning frame. Cheap to
        // compare each frame; only re-set if the handle hasn't already
        // flipped so we don't churn the asset id.
        if mat.0.id() != pm.fire.id() {
            mat.0 = pm.fire.clone();
        }

        fire.remaining -= dt;
        if fire.remaining <= 0.0 {
            commands.entity(slick_e).despawn();
            continue;
        }

        fire.tick_cd -= dt;
        if fire.tick_cd > 0.0 { continue; }
        fire.tick_cd = OIL_BURN_TICK;

        // AOE damage scales with the slick's live transform scale so a
        // still-spreading slick can't burn off-disc victims.
        let sp = slick_tf.translation.truncate();
        let visual_factor = (slick_tf.scale.x / OIL_SLICK_RADIUS).max(0.05);
        let eff_radius = OIL_BURN_RADIUS * visual_factor;
        let r2 = eff_radius * eff_radius;
        for &(e, ep, f) in &victim_snap {
            if f != slick.target_faction { continue; }
            if ep.distance_squared(sp) >= r2 { continue; }
            if let Ok((_, _, _, mut h, mut fx, shield_opt, is_local, is_friendly)) = victims.get_mut(e) {
                // Route Friendly victims through `apply_friendly_damage`
                // so dodge / armour / shield all apply. Allies +
                // enemies stay on the plain `apply_damage` path.
                let dealt = if is_friendly {
                    crate::bullet::apply_friendly_damage(
                        &mut h, &mut fx,
                        shield_opt.map(|s| s.into_inner()),
                        &player_stats, &mut rng,
                        OIL_BURN_DAMAGE, is_local,
                    )
                } else {
                    crate::bullet::apply_damage(&mut h, &mut fx, OIL_BURN_DAMAGE)
                };
                crate::bullet::credit_damage(
                    &mut stats,
                    Some(crate::bullet::DamageSource::Ally(ShipClass::OilTanker)),
                    dealt,
                );
            }
        }

        // 2-tone flame burst (bright fire + deep red), matching the
        // game's other particle effects' palette discipline.
        let vis_r = slick_tf.scale.x.max(1.0);
        let mote_count = rng.gen_range(4..=6);
        for _ in 0..mote_count {
            let off = Vec2::new(
                rng.gen_range(-vis_r..vis_r),
                rng.gen_range(-vis_r..vis_r),
            );
            let mat_handle = if rng.gen_range(0..3) == 0 {
                pm.mine_inner.clone()
            } else {
                pm.fire.clone()
            };
            let is_ember = rng.gen_range(0..6) == 0;
            let (life, vy_range, scale_range) = if is_ember {
                (rng.gen_range(0.55..0.85), 36.0..52.0, 0.6..1.0)
            } else {
                (rng.gen_range(0.30..0.70), 18.0..32.0, 0.5..0.9)
            };
            let vel = Vec2::new(
                rng.gen_range(-6.0..6.0),
                rng.gen_range(vy_range),
            );
            let scale = rng.gen_range(scale_range);
            commands.spawn((
                Mesh2d(em.particle.clone()),
                MeshMaterial2d(mat_handle),
                Transform {
                    translation: Vec3::new(sp.x + off.x, sp.y + off.y, 5.5),
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
