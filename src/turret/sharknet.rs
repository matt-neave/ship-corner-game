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
use crate::components::Health;
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

/// Marker on the triangular tail-fluke child of a `Shark`. Lets
/// `shark_ai`'s tail-wiggle pass find the right Transform without
/// walking the Children list every frame. `shark` carries the parent
/// Entity so the tail can pick up its parent's swim phase.
#[derive(Component)]
pub struct SharkTail {
    pub shark: Entity,
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
    /// Locked-on enemy during Charge. While this entity is alive and
    /// the shark hasn't yet drawn blood, `charge_dir` is re-aimed
    /// toward it every frame (homing). Cleared on Charge end or once
    /// the shark connects.
    pub target: Option<Entity>,
    /// Set to true on the first damaging contact of the current
    /// charge. After that the shark stops homing and continues
    /// straight in its current direction for the rest of the charge.
    pub made_contact: bool,
}

/// Seconds the shark drifts before locking a target and charging.
pub const WANDER_DURATION: f32 = 3.0;
/// World units per second during a charge.
pub const CHARGE_SPEED: f32 = 65.0;
/// Maximum charge duration. After this many seconds the shark
/// stops the sprint and returns to Wander even if it hasn't left
/// the arena. Long enough that the burst actually reaches a target
/// at reasonable wander-pack range at `CHARGE_SPEED`.
pub const CHARGE_MAX_DURATION: f32 = 1.4;
/// Extra seconds added to `state_timer` on the first damaging
/// contact of a charge. Gives the shark a satisfying over-shoot
/// past its target instead of stopping the instant it draws blood.
pub const CHARGE_POST_CONTACT_BONUS: f32 = 0.3;
/// Wander cruising speed — slower than the charge so the shift in
/// pace at lock-on is unmistakable.
pub const WANDER_SPEED: f32 = 22.0;
/// Contact-hit radius — slightly larger than visual body so a
/// glancing pass still registers.
pub const SHARK_HIT_RADIUS: f32 = 3.5;
/// Side-by-side spacing between sharks in a 2/3-barrel unit. The
/// shark body capsule is 4u wide, so a gap larger than that leaves
/// clear water between adjacent sharks instead of having them touch.
pub const SHARK_GAP: f32 = 8.0;
/// Backward offset applied to follower sharks during a synced charge
/// so the leader nudges ahead of its wingmen — reads as a flying-V
/// pack instead of a perfectly straight rank.
pub const SHARK_BACK_GAP: f32 = 3.5;
/// Seconds before the wander direction can re-roll. The shark
/// commits to a heading for this long before idle re-orientation,
/// so it cruises in semi-coherent paths instead of jittering.
const WANDER_TURN_INTERVAL: f32 = 1.4;
/// Side-to-side wiggle frequency (rad/s) of the shark's swim cycle.
/// Body + tail share this frequency so they stay phase-locked.
const WIGGLE_FREQ: f32 = 14.0;
/// Peak yaw of the body itself — small, around the shark's centre.
/// A real fish's body undulates less than the tail; keeping this low
/// stops the whole silhouette from skidding side to side and reads
/// as the spine flexing while the head holds heading.
const WIGGLE_AMP_BODY: f32 = 0.06;
/// Extra yaw on the tail BEYOND the body's, counter-phase. The tail
/// rotates around its junction with the body (mesh authored with the
/// apex at local origin) so the wide trailing edge sweeps further
/// than the apex — same swing arc you see on a real shark's caudal
/// fin from above.
const WIGGLE_AMP_TAIL_EXTRA: f32 = 0.32;
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
    mut materials: ResMut<Assets<ColorMaterial>>,
    ship_q: Query<&Transform, (With<crate::components::LocalPlayer>, Without<Shark>)>,
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

            let shark_entity = spawn_shark_visual(
                &mut commands, &pm, &mut meshes, &mut materials,
                initial_pos, true,
            );
            commands.entity(shark_entity).insert(Shark {
                owner_slot: slot_idx,
                lateral_idx: lateral,
                state: SharkState::Wander,
                state_timer: WANDER_DURATION,
                charge_dir: Vec2::Y,
                wander_dir: Vec2::Y,
                wander_turn_timer: 0.0,
                hit_this_charge: Vec::new(),
                target: None,
                made_contact: false,
            });
        }
    }
}

/// Spawn the full shark visual hierarchy (body capsule, tail fluke,
/// dorsal fin) and return the root entity. Pure visuals — no AI /
/// damage / state components.
///
/// Callers layer their gameplay tags on top:
/// - `sync_sharknet_sharks` (owner side) → `insert(Shark { … })`
/// - `apply_peer_units_snapshot` (mirror side) → `insert(PeerUnitMirror { … })`
///
/// `with_tail_marker` controls whether the tail child carries the
/// `SharkTail { shark }` component that drives the counter-wiggle
/// system. Mirrors don't need the wiggle (they're driven by snapshot
/// position only), so pass `false` there. Owner side passes `true`.
pub fn spawn_shark_visual(
    commands: &mut Commands,
    pm: &PaletteMaterials,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ColorMaterial>,
    pos: Vec2,
    with_tail_marker: bool,
) -> Entity {
    let body_mesh = meshes.add(Capsule2d::new(2.0, 3.0));
    let body_mat = pm.shark_body.clone();
    let tail_mesh = meshes.add(Triangle2d::new(
        Vec2::new( 0.0,  0.0),
        Vec2::new(-3.0, -5.0),
        Vec2::new( 3.0, -5.0),
    ));
    let dorsal_mesh = meshes.add(Triangle2d::new(
        Vec2::new(-0.8, -0.6),
        Vec2::new( 0.8, -0.6),
        Vec2::new( 0.0,  1.4),
    ));
    let dorsal_mat = materials.add(Color::srgb(0.26, 0.28, 0.32));

    let shark_entity = commands.spawn((
        Mesh2d(body_mesh),
        MeshMaterial2d(body_mat.clone()),
        Transform::from_xyz(pos.x, pos.y, 1.5),
        RenderLayers::layer(PLAY_LAYER),
        Visibility::Inherited,
    )).id();

    let tail = commands.spawn((
        Mesh2d(tail_mesh),
        MeshMaterial2d(body_mat),
        Transform::from_xyz(0.0, -2.0, -0.05),
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    if with_tail_marker {
        commands.entity(tail).insert(SharkTail { shark: shark_entity });
    }
    commands.entity(tail).insert(ChildOf(shark_entity));

    let dorsal = commands.spawn((
        Mesh2d(dorsal_mesh),
        MeshMaterial2d(dorsal_mat),
        Transform::from_xyz(0.0, 0.2, 0.05),
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    commands.entity(dorsal).insert(ChildOf(shark_entity));

    shark_entity
}

/// Per-frame state machine. Free-roam: during Wander each shark
/// picks a heading and cruises across the play area; it doesn't
/// orbit the friendly ship. Targets at lock-on are picked relative
/// to the shark's own position so a wide-roaming shark can engage
/// whatever it happens to be near.
pub fn shark_ai(
    time: Res<Time>,
    enemies: Query<(Entity, &Transform), (With<Enemy>, Without<Shark>, Without<SharkTail>)>,
    // Without<SharkTail> proves the sharks/tails queries are archetype-
    // disjoint so the two `&mut Transform` accesses don't conflict.
    mut sharks: Query<(Entity, &mut Transform, &mut Shark), Without<SharkTail>>,
    mut tails: Query<(&SharkTail, &mut Transform), Without<Shark>>,
) {
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();
    let half_w = crate::balance::ARENA_W * 0.5;
    let half_h = crate::balance::ARENA_H * 0.5;
    let bound_w = (half_w - WANDER_BOUNDS_INSET).max(0.0);
    let bound_h = (half_h - WANDER_BOUNDS_INSET).max(0.0);

    // Snapshot (entity, position) pairs so target-locked sharks can
    // re-aim toward the same enemy each frame during the homing
    // portion of a charge.
    let enemy_snapshot: Vec<(Entity, Vec2)> = enemies
        .iter()
        .map(|(e, t)| (e, t.translation.truncate()))
        .collect();

    // Count sharks per owner_slot so lateral offsets in pass 2 match
    // the formation that `lateral_x_for` expects. Sharks from the
    // same slot move as a unit; different slots are independent.
    let mut group_counts: std::collections::HashMap<usize, u8> =
        std::collections::HashMap::new();
    for (_, _, shark) in &sharks {
        *group_counts.entry(shark.owner_slot).or_insert(0) += 1;
    }

    // Pass 1: only the leader (lateral_idx == 0) runs the state
    // machine. Followers stay glued to the leader's formation via
    // pass 2 — both during Wander (loose ranks) and during Charge
    // (tighter V) — so charging never causes a teleport.
    for (_, mut tf, mut shark) in &mut sharks {
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
                    // Target lock: nearest enemy to this leader's
                    // current position. Entity captured so we can
                    // re-aim each frame until first contact.
                    let pos = tf.translation.truncate();
                    let target = enemy_snapshot
                        .iter()
                        .min_by(|a, b| {
                            a.1.distance_squared(pos)
                                .partial_cmp(&b.1.distance_squared(pos))
                                .unwrap_or(std::cmp::Ordering::Equal)
                        })
                        .copied();
                    if let Some((e, t)) = target {
                        let dir = (t - pos).normalize_or(Vec2::Y);
                        shark.charge_dir = dir;
                        shark.target = Some(e);
                        shark.made_contact = false;
                        shark.state = SharkState::Charge;
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
                // Pre-contact: re-aim toward the locked target every
                // frame so the shark hunts a juking enemy instead of
                // sailing through where they used to be. Post-contact:
                // direction is frozen — the shark continues in a
                // straight line for the rest of the burst, blood
                // already in the water.
                if !shark.made_contact {
                    if let Some(target_entity) = shark.target {
                        let pos = tf.translation.truncate();
                        let target_pos = enemy_snapshot
                            .iter()
                            .find(|(e, _)| *e == target_entity)
                            .map(|(_, p)| *p);
                        if let Some(tp) = target_pos {
                            let dir = (tp - pos).try_normalize();
                            if let Some(d) = dir {
                                shark.charge_dir = d;
                                let heading = (-d.x).atan2(d.y);
                                tf.rotation = Quat::from_rotation_z(heading);
                            }
                        } else {
                            // Target dead or gone — drop the lock so
                            // the shark commits to its current
                            // heading for the rest of the charge.
                            shark.target = None;
                        }
                    }
                }
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
                    shark.target = None;
                    shark.made_contact = false;
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
        rot: Quat,
    }
    let mut leader_snap: std::collections::HashMap<usize, LeaderSnap> =
        std::collections::HashMap::new();
    for (_, tf, shark) in &sharks {
        if shark.lateral_idx != 0 { continue; }
        leader_snap.insert(
            shark.owner_slot,
            LeaderSnap {
                pos: tf.translation.truncate(),
                wander_dir: shark.wander_dir,
                charge_dir: shark.charge_dir,
                state: shark.state,
                state_timer: shark.state_timer,
                rot: tf.rotation,
            },
        );
    }

    // Pass 2: every follower is glued to the leader's formation
    // position so the pack stays together. Lateral spread always
    // applies (ranked beside leader during wander); a backward
    // offset is added during Charge to form a V with the leader
    // at the tip. State/direction is copied so contact damage,
    // homing, and the charge timer all run in lockstep with the
    // leader.
    for (_, mut tf, mut shark) in &mut sharks {
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
        let lat = perp_right * lateral_x_for(n, shark.lateral_idx);
        let back = match snap.state {
            SharkState::Charge => -forward * SHARK_BACK_GAP,
            SharkState::Wander => Vec2::ZERO,
        };
        tf.translation.x = snap.pos.x + lat.x + back.x;
        tf.translation.y = snap.pos.y + lat.y + back.y;
        tf.rotation = snap.rot;
        let was_charge = shark.state == SharkState::Charge;
        let entered_charge = !was_charge && snap.state == SharkState::Charge;
        let entered_wander = was_charge && snap.state == SharkState::Wander;
        shark.state = snap.state;
        shark.state_timer = snap.state_timer;
        shark.wander_dir = snap.wander_dir;
        shark.charge_dir = snap.charge_dir;
        if entered_charge {
            shark.hit_this_charge.clear();
            shark.made_contact = false;
        }
        if entered_wander {
            shark.made_contact = false;
        }
    }

    // Pass 3a: small body yaw around the heading. Each shark gets a
    // unique phase offset derived from its owner slot + lateral index
    // so a school doesn't oscillate in lockstep.
    let t = time.elapsed_secs();
    for (_, mut tf, shark) in &mut sharks {
        let forward = match shark.state {
            SharkState::Wander => shark.wander_dir,
            SharkState::Charge => shark.charge_dir,
        }
        .try_normalize()
        .unwrap_or(Vec2::Y);
        let base_h = (-forward.x).atan2(forward.y);
        let phase = (shark.owner_slot as f32) * 1.7 + (shark.lateral_idx as f32) * 0.9;
        let wig = (t * WIGGLE_FREQ + phase).sin();
        tf.rotation = Quat::from_rotation_z(base_h + wig * WIGGLE_AMP_BODY);
    }

    // Pass 3b: tail counter-wiggle. Each tail's mesh is authored with
    // its apex at local origin, so its Transform position sits at the
    // body junction and its rotation pivots around that junction. We
    // want the tail's WORLD rotation to be in counter-phase with the
    // body's at amplitude `WIGGLE_AMP_BODY + WIGGLE_AMP_TAIL_EXTRA`:
    //
    //   world_tail = body_world + tail_local
    //              = (base_h + wig * body_amp) + tail_local
    //   target world_tail = base_h - wig * (body_amp + tail_extra)
    //
    // Solving: tail_local = -wig * (2 * body_amp + tail_extra) ...
    // but since we only care about counter-phase from the heading
    // (not from the body), the simpler formulation
    //   tail_local = -wig * (body_amp + tail_extra)
    // gives the trailing edge a clear counter-sweep against the
    // body's small yaw — visually reads as a fish flexing its
    // spine, head + tail going opposite ways.
    let mut shark_phase: std::collections::HashMap<Entity, f32> =
        std::collections::HashMap::with_capacity(8);
    for (e, _, shark) in &sharks {
        let phase = (shark.owner_slot as f32) * 1.7 + (shark.lateral_idx as f32) * 0.9;
        shark_phase.insert(e, phase);
    }
    let tail_amp = WIGGLE_AMP_BODY + WIGGLE_AMP_TAIL_EXTRA;
    for (tail, mut tail_tf) in &mut tails {
        let Some(phase) = shark_phase.get(&tail.shark) else { continue; };
        let wig = (t * WIGGLE_FREQ + phase).sin();
        tail_tf.rotation = Quat::from_rotation_z(-wig * tail_amp);
    }
}

/// Contact-damage during the Charge phase. Per-shark grace list
/// (`hit_this_charge`) prevents a single shark from chunking the
/// same enemy more than once on the same pass. First contact flips
/// `made_contact` so the AI stops homing (the rest of the burst
/// runs straight) and sprays a small red blood-particle burst.
pub fn shark_contact_damage(
    mut commands: Commands,
    cfg: Res<TurretConfig>,
    mut queue: ResMut<PendingDamageQueue>,
    em: Option<Res<crate::effects::EffectMeshes>>,
    pm: Option<Res<crate::palette::PaletteMaterials>>,
    mut sharks: Query<(&Transform, &mut Shark)>,
    mut enemies: Query<(Entity, &Transform, &Health, &mut HitFx), With<Enemy>>,
) {
    let r2 = SHARK_HIT_RADIUS * SHARK_HIT_RADIUS;
    let mut rng = rand::thread_rng();
    for (tf, mut shark) in &mut sharks {
        if shark.state != SharkState::Charge { continue; }
        let slot = cfg.slots.get(shark.owner_slot).copied().unwrap_or_default();
        let damage = slot.damage.max(1);
        let slot_runes: Vec<Rune> = slot.runes.iter().copied().flatten().collect();
        let pos = tf.translation.truncate();
        for (e, etf, h, mut fx) in &mut enemies {
            if h.0 <= 0 { continue; }
            if shark.hit_this_charge.contains(&e) { continue; }
            let ep = etf.translation.truncate();
            let dist2 = ep.distance_squared(pos);
            if dist2 >= r2 { continue; }
            shark.hit_this_charge.push(e);
            let was_first_contact = !shark.made_contact;
            shark.made_contact = true;
            if was_first_contact {
                shark.state_timer += CHARGE_POST_CONTACT_BONUS;
            }
            queue.push_initial(
                e,
                damage,
                ep,
                WeaponType::SharkNet,
                Some(DamageSource::PlayerSlot(shark.owner_slot as u8)),
                &slot_runes,
            );
            fx.pulse();
            if let (Some(em), Some(pm)) = (em.as_ref(), pm.as_ref()) {
                crate::effects::spawn_hit_particles(
                    &mut commands, em, &pm.bleed, ep, 6, 60.0, &mut rng,
                );
            }
        }
    }
}

/// Lateral offset per sub-shark inside a unit, RELATIVE TO THE
/// LEADER (`lateral_idx == 0`). The group is leader-centred — the
/// leader's free wander position drives where the formation sits,
/// and followers flank it. Earlier this mapping was leader=left
/// with followers offsetting from "absolute" formation slots,
/// which made follower 1 in a triple unit land on top of the
/// leader (both at offset 0) — you'd see two sharks instead of
/// three.
fn lateral_x_for(_n: u8, idx: u8) -> f32 {
    match idx {
        0 => 0.0,
        1 => -SHARK_GAP,
        _ => SHARK_GAP,
    }
}
