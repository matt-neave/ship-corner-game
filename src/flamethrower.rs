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
use crate::effects::{EffectMeshes, HitParticle};
use crate::enemy::Enemy;
use crate::palette::PaletteMaterials;
use crate::turret::{TurretConfig, TurretSlot};
use crate::weapon::WeaponType;

/// Cone reach (world units) in front of the slot's barrel direction.
const FLAMETHROWER_REACH: f32 = 16.0;
/// Half-angle of the cone (radians). ~25° each side → 50° total
/// arc. Wide enough to read as a spray, narrow enough that the player
/// has to aim the ship to keep enemies inside.
const FLAMETHROWER_HALF_ANGLE: f32 = 0.45;
/// Duration of the active burn phase, seconds.
const FLAMETHROWER_BURN_DURATION: f32 = 3.0;
/// Duration of the cooldown phase, seconds.
const FLAMETHROWER_COOLDOWN_DURATION: f32 = 3.0;
/// Particles per active tick — sprayed forward through the cone.
const FLAMETHROWER_PARTICLES_PER_TICK: u32 = 8;

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

        // Phase tick — flip Active ⇄ Cooldown on timeout.
        ft.phase_timer -= dt;
        if ft.phase_timer <= 0.0 {
            ft.phase = match ft.phase {
                FlamethrowerPhase::Active => FlamethrowerPhase::Cooldown,
                FlamethrowerPhase::Cooldown => FlamethrowerPhase::Active,
            };
            ft.phase_timer = match ft.phase {
                FlamethrowerPhase::Active => FLAMETHROWER_BURN_DURATION,
                FlamethrowerPhase::Cooldown => FLAMETHROWER_COOLDOWN_DURATION,
            };
            // Reset tick on Active entry so the first burst lands
            // immediately rather than after a half-second pause.
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
        ft.tick_timer -= dt;
        if ft.tick_timer > 0.0 { continue; }
        let rate = slot.fire_rate.max(0.1);
        ft.tick_timer = 1.0 / rate;

        let damage = slot.damage.max(1);
        let source = Some(DamageSource::PlayerSlot(slot.index as u8));
        let reach2 = FLAMETHROWER_REACH * FLAMETHROWER_REACH;
        let cos_half = FLAMETHROWER_HALF_ANGLE.cos();
        for (e, etf, en) in &enemies {
            let ep = etf.translation.truncate();
            let to = ep - slot_world;
            let d2 = to.length_squared();
            let er = 3.5 * en.variant.scale();
            let reach = (FLAMETHROWER_REACH + er) * (FLAMETHROWER_REACH + er);
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

        // Flame particles fanning out from the nozzle. Reuses the
        // Fire-rune material so the visible spit reads as fire even
        // though we don't apply OnFire.
        let _ = reach2; // kept for future range-based fx scaling
        for _ in 0..FLAMETHROWER_PARTICLES_PER_TICK {
            let spread = rng.gen_range(-FLAMETHROWER_HALF_ANGLE..FLAMETHROWER_HALF_ANGLE);
            let pa = total_angle + spread;
            let pdir = Vec2::new(-pa.sin(), pa.cos());
            let speed = rng.gen_range(35.0..55.0);
            let life = rng.gen_range(0.22..0.36);
            let scale = rng.gen_range(0.5..0.9);
            commands.spawn((
                Mesh2d(em.particle.clone()),
                MeshMaterial2d(pm.fire.clone()),
                Transform {
                    translation: Vec3::new(slot_world.x, slot_world.y, 5.5),
                    scale: Vec3::new(scale, scale, 1.0),
                    ..default()
                },
                HitParticle { life, max_life: life, base_scale: scale },
                Velocity(pdir * speed),
                RenderLayers::layer(PLAY_LAYER),
            ));
        }
    }
}
