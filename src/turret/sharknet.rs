//! Autonomous SharkNet weapon — each equipped `SharkNet` slot spawns
//! a persistent unit of `barrels` shark sub-bodies that hunt
//! independently of the deck. Closer kin to `HeliPad` / `Cage` than to
//! a normal turret; the slot itself never fires anything.
//!
//! State machine on each shark:
//! - `Wander`: drift around a per-slot anchor near the ship for
//!   `WANDER_DURATION` seconds.
//! - `Charge`: pick a target (nearest enemy to the SHIP, shared across
//!   every shark in the slot so they charge in parallel), face that
//!   direction, then move in a straight line at `CHARGE_SPEED` until
//!   exiting the playable arena. Each contact in the path applies
//!   `slot.damage` once per enemy per charge (per-shark grace list).
//!
//! Barrels (`1/2/3`) decides how many sharks ride in the unit.
//! Lateral offset spreads them side-by-side so the charge sweep is
//! visibly wider. Each shark damages independently — a wide salvo
//! through a clump can hit multiple enemies in one pass.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;
use rand::Rng;

use crate::balance::PLAY_LAYER;
use crate::bullet::{DamageSource, PendingDamageQueue};
use crate::components::{Friendly, Health};
use crate::effects::HitFx;
use crate::enemy::Enemy;
use crate::palette::PaletteMaterials;
use crate::rune::Rune;
use crate::turret::TurretConfig;
use crate::weapon::WeaponType;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SharkState {
    Wander,
    Charge,
}

#[derive(Component)]
pub struct Shark {
    pub owner_slot: usize,
    /// 0 = port (or only), 1 = middle (3-barrel), 2 = starboard.
    /// Drives per-shark phase offsets so multi-barrel units stay
    /// visually separated as they roam.
    pub lateral_idx: u8,
    pub state: SharkState,
    pub state_timer: f32,
    /// Charge direction (world-space unit vector). Set at the
    /// Wander → Charge transition; held until the shark exits the
    /// play area.
    pub charge_dir: Vec2,
    /// Current wander heading (world-space unit vector). Re-rolled
    /// every `WANDER_TURN_INTERVAL` seconds so a wandering shark
    /// drifts along semi-coherent paths instead of jittering.
    pub wander_dir: Vec2,
    /// Countdown to the next wander-direction re-roll. Decrements
    /// every frame while the shark is in Wander.
    pub wander_turn_timer: f32,
    /// Per-charge grace — enemies this shark has already damaged on
    /// the current charge. Cleared on every fresh Wander → Charge
    /// transition. Without this a slow-traversing shark would chunk
    /// the same enemy multiple times in a single pass.
    pub hit_this_charge: Vec<Entity>,
}

/// Seconds the shark drifts before locking a target and charging.
pub const WANDER_DURATION: f32 = 3.0;
/// World units per second during a charge.
pub const CHARGE_SPEED: f32 = 80.0;
/// Maximum charge duration. After this many seconds the shark
/// stops the sprint and returns to Wander even if it hasn't left
/// the arena. Previously the charge ran until the shark crossed
/// the play-area edge, which let it travel the full width (~200u).
pub const CHARGE_MAX_DURATION: f32 = 0.7;
/// Wander cruising speed — slower than the charge so the shift in
/// pace at lock-on is unmistakable.
pub const WANDER_SPEED: f32 = 22.0;
/// Contact-hit radius — slightly larger than visual body so a
/// glancing pass still registers.
pub const SHARK_HIT_RADIUS: f32 = 3.5;
/// Side-by-side spacing between sharks in a 2/3-barrel unit.
pub const SHARK_GAP: f32 = 4.0;
/// Seconds before the wander direction can re-roll. The shark
/// commits to a heading for this long before idle re-orientation,
/// so it cruises in semi-coherent paths instead of jittering.
const WANDER_TURN_INTERVAL: f32 = 1.4;
/// Half the playable arena (with a small inset) used to bounce the
/// shark back inward when it nears an edge during wander.
const WANDER_BOUNDS_INSET: f32 = 6.0;
/// How aggressively a wandering shark steers back toward the play
/// area when it hits the soft boundary. Higher = sharper turn.
const WANDER_BOUNCE_BLEND: f32 = 0.6;

/// Spawn / despawn shark entities to match the live `TurretConfig`.
/// Idempotent — runs every frame; only creates / destroys when the
/// observed shark count for a slot doesn't match the equipped
/// `barrels`. Mirrors the HeliPad helicopter-sync pattern.
pub fn sync_sharknet_sharks(
    mut commands: Commands,
    cfg: Res<TurretConfig>,
    pm: Option<Res<PaletteMaterials>>,
    mut meshes: ResMut<Assets<Mesh>>,
    ship_q: Query<&Transform, (With<Friendly>, Without<Shark>)>,
    sharks: Query<(Entity, &Shark)>,
) {
    let Some(pm) = pm else { return };

    // Despawn sharks whose slot is no longer a SharkNet (weapon
    // swapped or slot unequipped), AND any extra sharks beyond the
    // slot's current `barrels` count.
    for (e, shark) in &sharks {
        let slot = cfg.slots.get(shark.owner_slot).copied().unwrap_or_default();
        let valid_slot = slot.equipped && matches!(slot.weapon, WeaponType::SharkNet);
        let valid_idx = shark.lateral_idx < slot.barrels.max(1);
        if !valid_slot || !valid_idx {
            commands.entity(e).despawn();
        }
    }

    let Ok(ship_tf) = ship_q.single() else { return };
    let ship_pos = ship_tf.translation.truncate();

    for (slot_idx, slot) in cfg.slots.iter().enumerate() {
        if !slot.equipped { continue; }
        if !matches!(slot.weapon, WeaponType::SharkNet) { continue; }
        let n = slot.barrels.max(1);
        for lateral in 0..n {
            let already = sharks
                .iter()
                .any(|(_, s)| s.owner_slot == slot_idx && s.lateral_idx == lateral);
            if already { continue; }

            // Drop the shark in offset from the ship by a phase tied
            // to the slot index so multiple SharkNets don't stack on
            // top of each other at spawn — once the AI takes over
            // they roam the whole play area independently.
            let phase = (slot_idx as f32) * std::f32::consts::TAU / 8.0;
            let initial_pos = ship_pos
                + Vec2::new(phase.cos(), phase.sin()) * 18.0
                + Vec2::new(lateral_x_for(n, lateral), 0.0);

            // Body: wider, blunter capsule than a regular bullet so
            // the silhouette reads as a fish, not a torpedo.
            // (radius=2.0, half-length=3.0 → 4×6 outline.)
            let body_mesh = meshes.add(Capsule2d::new(2.0, 3.0));
            let body_mat = pm.shark_body.clone();
            // Triangle tail fluke — apex tucked into the body's rear
            // hemisphere (local +Y is swim-forward; the capsule's
            // cylinder ends at y=-3 and the rear cap bottoms out at
            // y=-5), wide trailing edge at y=-8. This reads as a
            // proper fish fin: narrow attachment, spreading fluke.
            let tail_mesh = meshes.add(Triangle2d::new(
                Vec2::new( 0.0, -3.0),
                Vec2::new(-3.0, -8.0),
                Vec2::new( 3.0, -8.0),
            ));

            let shark_entity = commands.spawn((
                Mesh2d(body_mesh),
                MeshMaterial2d(body_mat.clone()),
                Transform::from_xyz(initial_pos.x, initial_pos.y, 1.5),
                Shark {
                    owner_slot: slot_idx,
                    lateral_idx: lateral,
                    state: SharkState::Wander,
                    state_timer: WANDER_DURATION,
                    charge_dir: Vec2::Y,
                    wander_dir: Vec2::Y,
                    wander_turn_timer: 0.0,
                    hit_this_charge: Vec::new(),
                },
                RenderLayers::layer(PLAY_LAYER),
                Visibility::Inherited,
            )).id();

            let tail = commands.spawn((
                Mesh2d(tail_mesh),
                MeshMaterial2d(body_mat),
                Transform::from_xyz(0.0, 0.0, -0.05),
                RenderLayers::layer(PLAY_LAYER),
            )).id();
            commands.entity(tail).insert(ChildOf(shark_entity));
        }
    }
}

/// Per-frame state machine. Free-roam: during Wander each shark
/// picks a heading and cruises across the play area; it doesn't
/// orbit the friendly ship. Targets at lock-on are picked relative
/// to the shark's own position so a wide-roaming shark can engage
/// whatever it happens to be near.
pub fn shark_ai(
    time: Res<Time>,
    enemies: Query<&Transform, (With<Enemy>, Without<Shark>)>,
    mut sharks: Query<(&mut Transform, &mut Shark)>,
) {
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();
    let half_w = crate::balance::ARENA_W * 0.5;
    let half_h = crate::balance::ARENA_H * 0.5;
    let bound_w = (half_w - WANDER_BOUNDS_INSET).max(0.0);
    let bound_h = (half_h - WANDER_BOUNDS_INSET).max(0.0);

    // Snapshot enemy positions once so each shark can search
    // independently without a per-shark query borrow.
    let enemy_positions: Vec<Vec2> = enemies
        .iter()
        .map(|t| t.translation.truncate())
        .collect();

    // Count sharks per owner_slot so lateral offsets in pass 2 match
    // the formation that `lateral_x_for` expects. Sharks from the
    // same slot move as a unit; different slots are independent.
    let mut group_counts: std::collections::HashMap<usize, u8> =
        std::collections::HashMap::new();
    for (_, shark) in &sharks {
        *group_counts.entry(shark.owner_slot).or_insert(0) += 1;
    }

    // Pass 1: leaders (lateral_idx == 0) run the full state machine.
    // Followers are skipped here — they're driven by the leader in
    // pass 2 so the entire group shares wander direction, charge
    // target, and timing.
    for (mut tf, mut shark) in &mut sharks {
        if shark.lateral_idx != 0 { continue; }
        match shark.state {
            SharkState::Wander => {
                shark.state_timer -= dt;
                shark.wander_turn_timer -= dt;

                // Periodically pick a fresh wander direction — random
                // unit vector. Stays committed to the heading for
                // WANDER_TURN_INTERVAL so the path reads as deliberate
                // cruising, not noise.
                if shark.wander_turn_timer <= 0.0 {
                    let theta = rng.gen_range(0.0..std::f32::consts::TAU);
                    shark.wander_dir = Vec2::new(theta.cos(), theta.sin());
                    shark.wander_turn_timer = WANDER_TURN_INTERVAL;
                }

                // Soft-bounce off the playable bounds — when the
                // shark is past the inset border, blend in an
                // inward push so it doesn't escape the arena. The
                // blend is partial so the bounce feels organic.
                let pos = tf.translation.truncate();
                let mut inward = Vec2::ZERO;
                if pos.x > bound_w { inward.x -= 1.0; }
                if pos.x < -bound_w { inward.x += 1.0; }
                if pos.y > bound_h { inward.y -= 1.0; }
                if pos.y < -bound_h { inward.y += 1.0; }
                if inward.length_squared() > 0.0001 {
                    shark.wander_dir = shark
                        .wander_dir
                        .lerp(inward.normalize(), WANDER_BOUNCE_BLEND)
                        .try_normalize()
                        .unwrap_or(Vec2::Y);
                    // Force a re-roll soon so the shark commits to
                    // the new inward heading rather than oscillating.
                    shark.wander_turn_timer = WANDER_TURN_INTERVAL;
                }

                let step = shark.wander_dir * WANDER_SPEED * dt;
                tf.translation.x += step.x;
                tf.translation.y += step.y;
                let h = (-shark.wander_dir.x).atan2(shark.wander_dir.y);
                tf.rotation = Quat::from_rotation_z(h);

                if shark.state_timer <= 0.0 {
                    // Target lock: nearest enemy to THIS shark's
                    // current position — sharks roaming far apart
                    // will independently lock different enemies,
                    // which is the intended free-hunt feel.
                    let pos = tf.translation.truncate();
                    let target = enemy_positions
                        .iter()
                        .min_by(|a, b| {
                            a.distance_squared(pos)
                                .partial_cmp(&b.distance_squared(pos))
                                .unwrap_or(std::cmp::Ordering::Equal)
                        })
                        .copied();
                    if let Some(t) = target {
                        let dir = (t - pos).normalize_or(Vec2::Y);
                        shark.charge_dir = dir;
                        shark.state = SharkState::Charge;
                        // Re-use state_timer as the charge clock —
                        // ticks down to 0 over CHARGE_MAX_DURATION,
                        // capping the sprint to a short burst.
                        shark.state_timer = CHARGE_MAX_DURATION;
                        shark.hit_this_charge.clear();
                        let heading = (-dir.x).atan2(dir.y);
                        tf.rotation = Quat::from_rotation_z(heading);
                    } else {
                        // No targets — restart the wander cycle so
                        // we re-check next interval.
                        shark.state_timer = WANDER_DURATION;
                    }
                }
            }
            SharkState::Charge => {
                shark.state_timer -= dt;
                let step = shark.charge_dir * CHARGE_SPEED * dt;
                tf.translation.x += step.x;
                tf.translation.y += step.y;
                let limit_w = half_w + 5.0;
                let limit_h = half_h + 5.0;
                let out_of_bounds = tf.translation.x.abs() > limit_w
                    || tf.translation.y.abs() > limit_h;
                let charge_over = shark.state_timer <= 0.0;
                if out_of_bounds || charge_over {
                    shark.state = SharkState::Wander;
                    shark.state_timer = WANDER_DURATION;
                    // Reset the wander direction to push back inside
                    // the arena so the post-charge shark doesn't keep
                    // drifting away.
                    let inward = (-tf.translation.truncate())
                        .try_normalize()
                        .unwrap_or(Vec2::Y);
                    shark.wander_dir = inward;
                    shark.wander_turn_timer = WANDER_TURN_INTERVAL;
                }
            }
        }
    }

    // Snapshot leader state per owner_slot so followers can mirror it.
    #[derive(Clone, Copy)]
    struct LeaderSnap {
        pos: Vec2,
        wander_dir: Vec2,
        charge_dir: Vec2,
        state: SharkState,
        state_timer: f32,
        wander_turn_timer: f32,
        rot: Quat,
    }
    let mut leader_snap: std::collections::HashMap<usize, LeaderSnap> =
        std::collections::HashMap::new();
    for (tf, shark) in &sharks {
        if shark.lateral_idx != 0 { continue; }
        leader_snap.insert(
            shark.owner_slot,
            LeaderSnap {
                pos: tf.translation.truncate(),
                wander_dir: shark.wander_dir,
                charge_dir: shark.charge_dir,
                state: shark.state,
                state_timer: shark.state_timer,
                wander_turn_timer: shark.wander_turn_timer,
                rot: tf.rotation,
            },
        );
    }

    // Pass 2: followers snap to leader pos + lateral offset, in the
    // perpendicular of the leader's current forward direction. State,
    // timers, and direction are copied so contact damage and the
    // wander/charge cycle run in lockstep with the leader.
    for (mut tf, mut shark) in &mut sharks {
        if shark.lateral_idx == 0 { continue; }
        let Some(snap) = leader_snap.get(&shark.owner_slot).copied() else { continue; };
        let forward = match snap.state {
            SharkState::Wander => snap.wander_dir,
            SharkState::Charge => snap.charge_dir,
        }
        .try_normalize()
        .unwrap_or(Vec2::Y);
        let perp_right = Vec2::new(forward.y, -forward.x);
        let n = group_counts.get(&shark.owner_slot).copied().unwrap_or(1);
        let offset = perp_right * lateral_x_for(n, shark.lateral_idx);
        tf.translation.x = snap.pos.x + offset.x;
        tf.translation.y = snap.pos.y + offset.y;
        tf.rotation = snap.rot;
        let was_wander = shark.state == SharkState::Wander;
        let entered_charge = was_wander && snap.state == SharkState::Charge;
        shark.state = snap.state;
        shark.state_timer = snap.state_timer;
        shark.wander_dir = snap.wander_dir;
        shark.charge_dir = snap.charge_dir;
        shark.wander_turn_timer = snap.wander_turn_timer;
        if entered_charge {
            shark.hit_this_charge.clear();
        }
    }
}

/// Contact-damage during the Charge phase. Per-shark grace list
/// (`hit_this_charge`) prevents a single shark from chunking the
/// same enemy more than once on the same pass.
pub fn shark_contact_damage(
    cfg: Res<TurretConfig>,
    mut queue: ResMut<PendingDamageQueue>,
    mut sharks: Query<(&Transform, &mut Shark)>,
    mut enemies: Query<(Entity, &Transform, &Health, &mut HitFx), With<Enemy>>,
) {
    let r2 = SHARK_HIT_RADIUS * SHARK_HIT_RADIUS;
    for (tf, mut shark) in &mut sharks {
        if shark.state != SharkState::Charge { continue; }
        // Snapshot slot-side config — damage + runes carry into the
        // damage event so proc systems (Fire/Shock/etc.) fire off
        // the bite the same way they would off a bullet hit.
        let slot = cfg.slots.get(shark.owner_slot).copied().unwrap_or_default();
        let damage = slot.damage.max(1);
        let slot_runes: Vec<Rune> = slot.runes.iter().copied().flatten().collect();
        let pos = tf.translation.truncate();
        for (e, etf, h, mut fx) in &mut enemies {
            if h.0 <= 0 { continue; }
            if shark.hit_this_charge.contains(&e) { continue; }
            let dist2 = etf.translation.truncate().distance_squared(pos);
            if dist2 >= r2 { continue; }
            shark.hit_this_charge.push(e);
            queue.push_initial(
                e,
                damage,
                etf.translation.truncate(),
                WeaponType::SharkNet,
                Some(DamageSource::PlayerSlot(shark.owner_slot as u8)),
                &slot_runes,
            );
            fx.pulse();
        }
    }
}

/// Lateral offset per sub-shark inside a unit. `n` is the total
/// count (1/2/3); `idx` is which sub-shark. Mirrors the deck-barrel
/// layout for consistency with other multi-barrel weapons.
fn lateral_x_for(n: u8, idx: u8) -> f32 {
    match (n, idx) {
        (1, _) => 0.0,
        (2, 0) => -SHARK_GAP * 0.5,
        (2, _) =>  SHARK_GAP * 0.5,
        (3, 0) => -SHARK_GAP,
        (3, 1) =>  0.0,
        (3, _) =>  SHARK_GAP,
        _ => 0.0,
    }
}
