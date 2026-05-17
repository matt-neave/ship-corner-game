//! Cone-burner `Flamethrower` turret. Damages every non-Friendly /
//! non-Ally entity inside a short cone in front of the slot — no
//! bullets, no projectiles. Damage runs in burst cycles:
//!
//!   * 3 seconds of ACTIVE burn — 1 damage (× synergies / runes) every
//!     0.5 seconds to every enemy inside the cone.
//!   * 3 seconds of COOLDOWN — no damage, no visuals.
//!
//! Does NOT auto-apply the Fire rune. The visible flame and the
//! status-Fire are deliberately separate mechanics — slotting Fire on
//! a Flamethrower stacks both effects.
//!
//! Two systems:
//!   - `sync_flamethrower_state` keeps a `Flamethrower` component on
//!     every equipped Flamethrower slot (and tears it off when the
//!     slot changes weapon).
//!   - `flamethrower_tick` advances the active/cooldown phase, ticks
//!     damage on cadence, and emits flame particles during the active
//!     phase.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;
use rand::Rng;

use crate::balance::PLAY_LAYER;
use crate::bullet::{DamageSource, PendingDamageQueue};
use crate::components::{Friendly, Heading, Velocity};
use crate::effects::{EffectMeshes, FireParticle, HitParticle};
use crate::enemy::Enemy;
use crate::palette::PaletteMaterials;
use crate::turret::{TurretConfig, TurretSlot};
use crate::weapon::WeaponType;

/// Damage cone reach (world units) in front of the slot's mount
/// direction. Matched to the upper end of the particle travel
/// distance so the visible flame and the damage area line up.
const FLAMETHROWER_REACH: f32 = 80.0;
/// Half-angle of the cone (radians). ~10° each side → ~20° total
/// arc. Tight focus reads as a directed flame spear; the slot's
/// mount direction is FIXED (like Blade) so the player aims by
/// rotating the SHIP.
const FLAMETHROWER_HALF_ANGLE: f32 = 0.175;
/// Duration of the active burn phase, seconds.
const FLAMETHROWER_BURN_DURATION: f32 = 3.0;
/// Cooldown phase length by tier (`barrels` 1..=3). T1 = full 3s
/// reload; T2 = halved; T3 = no cooldown (burns continuously, with
/// the Cooldown phase skipped entirely).
pub fn flamethrower_cooldown_for_tier(barrels: u8) -> f32 {
    match barrels.clamp(1, 3) {
        1 => 3.0,
        2 => 1.5,
        _ => 0.0,
    }
}
/// Pair-emissions PER FRAME during the active phase. Each emission
/// spawns TWO particles at the same position: a larger dark outer
/// stroke and a smaller bright inner core. The size difference reads
/// as a dark outline around a glowing core (pixel-art fire pattern).
/// Spew is continuous — independent of the damage cadence — so the
/// flame visibly fills the cone the entire time the burner is on.
const FLAMETHROWER_PARTICLES_PER_FRAME: u32 = 2;
/// Outer-stroke radius multiplier relative to the inner core. >1.0
/// means the dark stroke wraps the bright inner with a visible rim.
const FLAMETHROWER_STROKE_SCALE: f32 = 1.55;
/// Particle velocity range. Tuned so `speed × life ≈ FLAMETHROWER_REACH`
/// — particles visibly travel the full damage cone rather than
/// dying within the first quarter. Scaled with `FLAMETHROWER_REACH`
/// (×1.6 from the original 110/160 tuning) so the flame visibly
/// covers the longer cone in roughly the same particle lifetime.
const FLAMETHROWER_PARTICLE_SPEED_MIN: f32 = 175.0;
const FLAMETHROWER_PARTICLE_SPEED_MAX: f32 = 260.0;
/// Particle lifetime range. Combined with the speed above, particles
/// reach the cone's outer edge before fading.
const FLAMETHROWER_PARTICLE_LIFE_MIN: f32 = 0.22;
const FLAMETHROWER_PARTICLE_LIFE_MAX: f32 = 0.32;

/// Phase of a Flamethrower slot. `Active` burns; `Cooldown` is
/// idle. Phase swaps when `phase_timer` reaches 0.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FlamethrowerPhase {
    Active,
    Cooldown,
}

#[derive(Component)]
pub struct Flamethrower {
    pub phase: FlamethrowerPhase,
    /// Seconds remaining in the current phase.
    pub phase_timer: f32,
    /// Seconds until the next damage tick (only consulted in Active).
    pub tick_timer: f32,
}

impl Default for Flamethrower {
    fn default() -> Self {
        Self {
            phase: FlamethrowerPhase::Active,
            phase_timer: FLAMETHROWER_BURN_DURATION,
            tick_timer: 0.0, // fire on the first frame for snappy feel
        }
    }
}

pub struct FlamethrowerPlugin;

impl Plugin for FlamethrowerPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (sync_flamethrower_state, flamethrower_tick));
    }
}

/// Attach a `Flamethrower` component to every equipped Flamethrower
/// slot, and strip it off any slot that has been re-equipped to a
/// different weapon. Mirrors the blade-decor pattern.
pub fn sync_flamethrower_state(
    mut commands: Commands,
    cfg: Res<TurretConfig>,
    slots: Query<(Entity, &TurretSlot, Option<&Flamethrower>)>,
) {
    if !cfg.is_changed() { return; }
    for (slot_entity, slot, ft) in &slots {
        let s = cfg.slots[slot.index];
        let want = s.equipped && matches!(s.weapon, WeaponType::Flamethrower);
        match (want, ft.is_some()) {
            (true, false) => {
                commands.entity(slot_entity).insert(Flamethrower::default());
            }
            (false, true) => {
                commands.entity(slot_entity).remove::<Flamethrower>();
            }
            _ => {}
        }
    }
}

/// Per-frame: advance each Flamethrower's phase + tick timer. While
/// in Active, every `1.0 / fire_rate` seconds, push a `DamageEvent`
/// for every enemy inside the cone, and spray flame particles forward.
pub fn flamethrower_tick(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    mut queue: ResMut<PendingDamageQueue>,
    cfg: Res<TurretConfig>,
    stats: Res<crate::stats::PlayerStats>,
    ship_q: Query<(&Transform, &Heading), With<Friendly>>,
    mut slots: Query<
        (&TurretSlot, &Transform, &mut Flamethrower),
        (Without<Friendly>, Without<Enemy>),
    >,
    enemies: Query<(Entity, &Transform, &Enemy), With<Enemy>>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    let Ok((ship_tf, ship_heading)) = ship_q.single() else { return; };
    let ship_pos = ship_tf.translation.truncate();
    let ship_h = ship_heading.0;
    let mut rng = rand::thread_rng();

    for (slot, slot_tf, mut ft) in &mut slots {
        if !cfg.slots[slot.index].equipped { continue; }
        if !matches!(cfg.slots[slot.index].weapon, WeaponType::Flamethrower) { continue; }

        // Phase tick — flip Active ⇄ Cooldown on timeout. T3
        // (`cooldown == 0`) skips the Cooldown phase entirely: the
        // Active timer just refreshes so the burn never stops.
        ft.phase_timer -= dt;
        if ft.phase_timer <= 0.0 {
            let cooldown = flamethrower_cooldown_for_tier(slot.barrels);
            ft.phase = match ft.phase {
                FlamethrowerPhase::Active => {
                    if cooldown <= 0.0 {
                        FlamethrowerPhase::Active
                    } else {
                        FlamethrowerPhase::Cooldown
                    }
                }
                FlamethrowerPhase::Cooldown => FlamethrowerPhase::Active,
            };
            ft.phase_timer = match ft.phase {
                FlamethrowerPhase::Active => FLAMETHROWER_BURN_DURATION,
                FlamethrowerPhase::Cooldown => cooldown,
            };
            if matches!(ft.phase, FlamethrowerPhase::Active) {
                ft.tick_timer = 0.0;
            }
        }

        if !matches!(ft.phase, FlamethrowerPhase::Active) { continue; }

        // World position + forward of this slot. Slot transforms are
        // local to the ship (rotated by mount + barrel_angle) — compose
        // with the ship's heading to get a world-space firing axis.
        let local = slot_tf.translation.truncate();
        let cos_h = ship_h.cos();
        let sin_h = ship_h.sin();
        let world_off = Vec2::new(
            local.x * cos_h - local.y * sin_h,
            local.x * sin_h + local.y * cos_h,
        );
        let slot_world = ship_pos + world_off;
        let total_angle = ship_h + slot.barrel_angle;
        let forward = Vec2::new(-total_angle.sin(), total_angle.cos());

        // Damage tick on `1.0 / fire_rate` cadence. With default
        // fire_rate=2 this fires every 0.5s during the active phase.
        // The particle spew below runs UNCONDITIONALLY each frame so
        // the visible flame is continuous even on non-damage frames —
        // visual cadence and damage cadence are decoupled.
        ft.tick_timer -= dt;
        let damage_frame = ft.tick_timer <= 0.0;
        if damage_frame {
            let rate = slot.fire_rate.max(0.1);
            ft.tick_timer = 1.0 / rate;

            let damage = slot.damage.max(1);
            let source = Some(DamageSource::PlayerSlot(slot.index as u8));
            let cos_half = FLAMETHROWER_HALF_ANGLE.cos();
            // Folds the player Range stat + any Crow's Nest range
            // adjacency into the cone reach, matching how every other
            // weapon's range scales.
            let range_factor = stats.range_mult() * slot.range_mult.max(1.0);
            let scaled_reach = FLAMETHROWER_REACH * range_factor;
            for (e, etf, en) in &enemies {
                let ep = etf.translation.truncate();
                let to = ep - slot_world;
                let d2 = to.length_squared();
                let er = 3.5 * en.variant.scale();
                let reach = (scaled_reach + er) * (scaled_reach + er);
                if d2 > reach { continue; }
                if d2 < 0.001 {
                    queue.push_initial(e, damage, ep, WeaponType::Flamethrower, source, &slot.runes);
                    continue;
                }
                let dir = to / d2.sqrt();
                // Cone check via cosine of the angle between forward and
                // direction-to-enemy. cosθ ≥ cos(half) ⇔ within the cone.
                if dir.dot(forward) < cos_half { continue; }
                queue.push_initial(e, damage, ep, WeaponType::Flamethrower, source, &slot.runes);
            }
        }

        // Flame puffs every frame the burner is Active. Each puff is
        // a PAIR — a larger dark outer stroke at z=5.4 and a smaller
        // bright inner core at z=5.5, both moving on the same
        // velocity vector. The size differential reads as a dark
        // rim around a glowing core (the pixel-art fire silhouette).
        // The inner carries `FireParticle` so its material cycles
        // through hot → mid → cool over its life; the outer stays
        // a constant dark stroke. Both shrink together via the
        // shared `HitParticle` fade.
        for _ in 0..FLAMETHROWER_PARTICLES_PER_FRAME {
            let spread = rng.gen_range(-FLAMETHROWER_HALF_ANGLE..FLAMETHROWER_HALF_ANGLE);
            let pa = total_angle + spread;
            let pdir = Vec2::new(-pa.sin(), pa.cos());
            let speed = rng.gen_range(
                FLAMETHROWER_PARTICLE_SPEED_MIN..FLAMETHROWER_PARTICLE_SPEED_MAX,
            );
            let life = rng.gen_range(
                FLAMETHROWER_PARTICLE_LIFE_MIN..FLAMETHROWER_PARTICLE_LIFE_MAX,
            );
            let inner_scale = rng.gen_range(0.6..1.1);
            let outer_scale = inner_scale * FLAMETHROWER_STROKE_SCALE;
            let vel_vec = pdir * speed;
            let pos = Vec3::new(slot_world.x, slot_world.y, 5.4);

            // OUTER dark stroke — constant cool colour, no
            // FireParticle so it stays the rim while the inner
            // cycles through bright stages.
            commands.spawn((
                Mesh2d(em.particle.clone()),
                MeshMaterial2d(pm.fire_cool.clone()),
                Transform {
                    translation: pos,
                    scale: Vec3::new(outer_scale, outer_scale, 1.0),
                    ..default()
                },
                HitParticle { life, max_life: life, base_scale: outer_scale },
                Velocity(vel_vec),
                RenderLayers::layer(PLAY_LAYER),
            ));

            // INNER bright core — spawns at the HOT tip colour;
            // `tick_fire_particles` swaps to `fire` then `fire_cool`
            // as the puff ages. Rendered slightly in front of the
            // outer so the bright pixel sits on top of the rim.
            commands.spawn((
                Mesh2d(em.particle.clone()),
                MeshMaterial2d(pm.fire_hot.clone()),
                Transform {
                    translation: Vec3::new(pos.x, pos.y, 5.5),
                    scale: Vec3::new(inner_scale, inner_scale, 1.0),
                    ..default()
                },
                HitParticle { life, max_life: life, base_scale: inner_scale },
                FireParticle {
                    mid: pm.fire.clone(),
                    cool: pm.fire_cool.clone(),
                    at_mid: false,
                    at_cool: false,
                },
                Velocity(vel_vec),
                RenderLayers::layer(PLAY_LAYER),
            ));
        }
    }
}
