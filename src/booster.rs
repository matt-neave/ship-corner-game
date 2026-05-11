//! Support `Booster` turret — does not fire. While adjacent to other
//! equipped turret slots, multiplies each neighbour's effective fire
//! rate by 1.30 per adjacent Booster (multiplicative — two adjacent
//! Boosters → ×1.69).
//!
//! Three pieces:
//!   - `boost_multiplier_for_slot` is queried by `turret::sync_turret_config`
//!     when it pushes per-slot config into the live `TurretSlot`s, so the
//!     neighbour's `fire_rate` already reflects every adjacent Booster.
//!   - `sync_booster_decor` keeps the deck visual in sync: each Booster
//!     slot gets one bright `BoosterRing` on top of the deck pad PLUS
//!     `BOOSTER_PULSES_PER_CONNECTION` `BoosterPulse` particles per
//!     adjacent equipped neighbour. Non-Booster slots have neither.
//!   - `tick_booster_pulses` advances each pulse's phase per frame and
//!     interpolates its local position from the booster outward to the
//!     boosted slot, so a continuous trickle of dots flows toward each
//!     neighbour the booster is buffing.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;

use crate::balance::{PLAY_LAYER, TURRET_ADJACENCY, TURRET_POSITIONS};
use crate::palette::PaletteMaterials;
use crate::turret::{TurretConfig, TurretSlot};
use crate::weapon::WeaponType;

/// Per-Booster fire-rate boost applied to each adjacent equipped turret.
/// Stacks multiplicatively — N adjacent Boosters → ×1.30^N.
pub const BOOSTER_FIRE_RATE_MULT: f32 = 1.30;

/// How many pulse particles ride each booster→neighbour connection.
/// 3 reads as a continuous trickle without becoming a solid line.
const BOOSTER_PULSES_PER_CONNECTION: usize = 3;

/// Phase units travelled per second. 1.0 = one full traversal per
/// second; 1.5 = ~0.67s per pulse from booster to target.
const BOOSTER_PULSE_SPEED: f32 = 1.5;

/// Marker for the bright ring child entity rendered on top of a Booster
/// slot's deck pad. `sync_booster_decor` ensures exactly one of these
/// exists per equipped Booster slot.
#[derive(Component)]
pub struct BoosterRing;

/// One traveling dot riding a booster→neighbour connection. Parented
/// to the booster slot entity, so its local frame already inherits the
/// ship's translation/heading and the booster's mount-angle rotation.
/// `target_offset` is pre-computed in BOOSTER-LOCAL coords (i.e. with
/// the mount rotation already undone), so the tick system just lerps
/// the position from origin → `target_offset` as `phase` advances.
#[derive(Component)]
pub struct BoosterPulse {
    /// Booster-local offset to the target slot's centre. The pulse's
    /// local translation interpolates from (0,0) to this point as
    /// `phase` goes from 0 → 1.
    pub target_offset: Vec2,
    /// Phase 0..1, advances each frame; wraps so the dot keeps
    /// flowing along the connection.
    pub phase: f32,
}

pub struct BoosterPlugin;

impl Plugin for BoosterPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (sync_booster_decor, tick_booster_pulses));
    }
}

/// Fire-rate multiplier applied to slot `slot_idx` from every adjacent
/// equipped Booster. Returns 1.0 when no Booster is touching this slot.
/// Multiplicative stacking: two adjacent Boosters → 1.30 × 1.30 ≈ 1.69.
pub fn boost_multiplier_for_slot(cfg: &TurretConfig, slot_idx: usize) -> f32 {
    let Some(neighbours) = TURRET_ADJACENCY.get(slot_idx) else { return 1.0; };
    let mut mult = 1.0_f32;
    for &n in neighbours.iter() {
        let Some(s) = cfg.slots.get(n) else { continue; };
        if s.equipped && matches!(s.weapon, WeaponType::Booster) {
            mult *= BOOSTER_FIRE_RATE_MULT;
        }
    }
    mult
}

/// Maintain the booster decor invariant on every config change:
///   - Equipped Booster slots have exactly one `BoosterRing` child AND
///     `BOOSTER_PULSES_PER_CONNECTION` `BoosterPulse` children per
///     adjacent equipped neighbour.
///   - Non-Booster (or unequipped) slots have neither.
///
/// Pulses are rebuilt from scratch on every config change — cheaper
/// than diffing the adjacency-state set, and config changes are rare
/// (shop interactions only).
pub fn sync_booster_decor(
    mut commands: Commands,
    cfg: Res<TurretConfig>,
    pm: Option<Res<PaletteMaterials>>,
    mut meshes: ResMut<Assets<Mesh>>,
    slots: Query<(Entity, &TurretSlot, Option<&Children>)>,
    rings: Query<Entity, With<BoosterRing>>,
    pulses: Query<Entity, With<BoosterPulse>>,
) {
    if !cfg.is_changed() { return; }
    let Some(pm) = pm else { return; };

    // Lazily build the meshes — only when we actually need to spawn
    // decor. Reused across every Booster slot in the same frame.
    let mut ring_mesh: Option<Handle<Mesh>> = None;
    let mut pulse_mesh: Option<Handle<Mesh>> = None;

    for (slot_entity, slot, children) in &slots {
        let s = cfg.slots[slot.index];
        let want_ring = s.equipped && matches!(s.weapon, WeaponType::Booster);

        // Find existing decor children.
        let existing_ring = children
            .into_iter()
            .flat_map(|c| c.iter())
            .find(|c| rings.get(*c).is_ok());
        let existing_pulses: Vec<Entity> = children
            .into_iter()
            .flat_map(|c| c.iter())
            .filter(|c| pulses.get(*c).is_ok())
            .collect();

        // Pulses always get torn down on config change — easier than
        // diffing per-connection state. They're cheap to respawn.
        for pulse in &existing_pulses {
            commands.entity(*pulse).despawn();
        }

        match (want_ring, existing_ring) {
            (true, None) => {
                // Spawn a small bright dot on top of the deck pad. The
                // base `Circle::new(2.0)` already sits at the slot's z=2;
                // the ring rides slightly above (+0.05) so it always
                // draws on top regardless of barrel z-fighting.
                let mesh = ring_mesh
                    .get_or_insert_with(|| meshes.add(Circle::new(1.0)))
                    .clone();
                let ring = commands.spawn((
                    Mesh2d(mesh),
                    MeshMaterial2d(pm.booster_ring.clone()),
                    Transform::from_xyz(0.0, 0.0, 0.05),
                    BoosterRing,
                    RenderLayers::layer(PLAY_LAYER),
                )).id();
                commands.entity(ring).insert(ChildOf(slot_entity));
            }
            (false, Some(ring)) => {
                commands.entity(ring).despawn();
            }
            _ => { /* already in the desired state */ }
        }

        // Pulse re-spawn — only for equipped Boosters with at least one
        // adjacent equipped neighbour.
        if !want_ring { continue; }
        let Some(neighbours) = TURRET_ADJACENCY.get(slot.index) else { continue; };
        let booster_pos = TURRET_POSITIONS[slot.index];
        // Inverse rotation to convert from hull-frame to booster-local
        // frame (the slot entity is already rotated by `mount_angle`,
        // so children inherit that rotation — we undo it here).
        let inv_a = -slot.mount_angle;
        let cos_i = inv_a.cos();
        let sin_i = inv_a.sin();

        for &n in neighbours.iter() {
            let Some(ns) = cfg.slots.get(n) else { continue; };
            if !ns.equipped { continue; }
            // Don't draw booster→booster pulses — they look like noise
            // and the boost doesn't apply between Boosters anyway.
            if matches!(ns.weapon, WeaponType::Booster) { continue; }

            let target_hull_pos = TURRET_POSITIONS[n];
            let dx = target_hull_pos.0 - booster_pos.0;
            let dy = target_hull_pos.1 - booster_pos.1;
            let local_x = dx * cos_i - dy * sin_i;
            let local_y = dx * sin_i + dy * cos_i;
            let target_offset = Vec2::new(local_x, local_y);

            let mesh = pulse_mesh
                .get_or_insert_with(|| meshes.add(Circle::new(0.4)))
                .clone();

            // Spawn N pulses with evenly-staggered phase so they read
            // as a continuous flow rather than a single dot looping.
            for i in 0..BOOSTER_PULSES_PER_CONNECTION {
                let phase = i as f32 / BOOSTER_PULSES_PER_CONNECTION as f32;
                let pulse = commands.spawn((
                    Mesh2d(mesh.clone()),
                    MeshMaterial2d(pm.booster_ring.clone()),
                    Transform::from_xyz(0.0, 0.0, 0.04),
                    BoosterPulse { target_offset, phase },
                    RenderLayers::layer(PLAY_LAYER),
                )).id();
                commands.entity(pulse).insert(ChildOf(slot_entity));
            }
        }
    }
}

/// Per-frame: advance every pulse's phase and lerp its local position
/// from origin to its pre-computed `target_offset`. Phase wraps at 1.0
/// so the dot snaps back to the booster and starts a fresh traversal —
/// reads as a continuous trickle when staggered with sibling pulses.
pub fn tick_booster_pulses(
    time: Res<Time>,
    mut pulses: Query<(&mut BoosterPulse, &mut Transform)>,
) {
    let dt = time.delta_secs();
    for (mut p, mut tf) in &mut pulses {
        p.phase = (p.phase + dt * BOOSTER_PULSE_SPEED) % 1.0;
        let pos = p.target_offset * p.phase;
        tf.translation.x = pos.x;
        tf.translation.y = pos.y;
    }
}
