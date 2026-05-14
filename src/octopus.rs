//! Autonomous `Cage` weapon — a deck cage that releases an octopus
//! into the water. The octopus is a free-roaming hunter, not a
//! ship-orbit drone.
//!
//! Visual model:
//!   - Body = a small dark-purple blob in the water. Picks the
//!     nearest enemy and swims toward it; idles when nothing's around.
//!   - Tentacles are NOT attached to the body — instead they
//!     `emerge` from the water near each engaged enemy as their own
//!     short-lived entities (`OctopusTentacle`). Lifecycle: rise →
//!     slap → sink → despawn. One damage hit per tentacle, applied
//!     on the emerge → slap transition.
//!   - `slot.barrels` (1/2/3) caps the number of simultaneously
//!     ACTIVE tentacles per octopus at 2/4/6. Spawn cooldown scales
//!     with that cap so total dps ≈ `damage × fire_rate × cap`.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;
use rand::Rng;
use std::collections::VecDeque;

use crate::balance::PLAY_LAYER;
use crate::bullet::{DamageSource, PendingDamageQueue};
use crate::components::Friendly;
use crate::effects::{spawn_hit_particles, EffectMeshes};
use crate::rune::Rune;
use crate::enemy::Enemy;
use crate::palette::PaletteMaterials;
use crate::trails::{empty_dynamic_mesh, rebuild_ribbon_mesh};
use crate::turret::{TurretConfig, TurretSlot};
use crate::weapon::WeaponType;

/// Visual radius of the octopus body blob.
const OCTOPUS_BODY_RADIUS: f32 = 2.4;

/// World-units / sec the body swims toward its current target.
/// Faster than most enemies so the octopus reads as a predator
/// hunting them down.
const OCTOPUS_HUNT_SPEED: f32 = 32.0;

/// Body has to be within this distance of an enemy to spawn a
/// tentacle. The tentacle itself emerges from the WATER at the
/// enemy (not from the body) — the body's proximity is purely a
/// gameplay gate, not a visual reach. Scaled per `slot.barrels` by
/// `engage_range_for` so upgrading the Cage materially extends
/// the octopus's reach, not just its tentacle count.
const OCTOPUS_ENGAGE_RANGE_BASE: f32 = 30.0;
/// Standoff ring — `octopus_ai` stops closing this far from the
/// target instead of sitting on top of it. The tentacle-spawn loop
/// still gates engagement on the per-barrel engage range, so
/// standoff must comfortably fit inside the smallest (B1) ring.
const OCTOPUS_STANDOFF: f32 = 18.0;

/// Per-barrel engage range. B1 / B2 / B3 → base / +25% / +50%.
/// More-barrel cages reach further AND deploy more tentacles, so
/// the upgrade is a clear bite-radius bump on top of the dps gain.
fn engage_range_for(barrels: u8) -> f32 {
    let mult = match barrels.clamp(1, 3) {
        1 => 1.00,
        2 => 1.25,
        _ => 1.50,
    };
    OCTOPUS_ENGAGE_RANGE_BASE * mult
}

/// Cap mapping — `slot.barrels` to max simultaneous tentacles.
/// Stepped 3 / 6 / 9 (was 2 / 4 / 6) so each barrel upgrade adds
/// three concurrent tentacles instead of two — a more legible
/// per-tier gain plus a stronger ceiling at max barrels.
fn max_tentacles(barrels: u8) -> usize {
    match barrels.clamp(1, 3) {
        1 => 3,
        2 => 6,
        _ => 9,
    }
}

/// Tentacle lifecycle — animation phases. Total visible time per
/// tentacle = EMERGE + SLAP + RETREAT (~0.6s).
const TENTACLE_EMERGE_TIME: f32 = 0.18;
const TENTACLE_SLAP_TIME:   f32 = 0.14;
const TENTACLE_RETREAT_TIME: f32 = 0.22;
/// Length (world units) of the visible tentacle when fully out.
const TENTACLE_LENGTH: f32 = 6.0;
/// Damage radius around the tentacle's spawn point when it slaps.
/// Small — the slap is a focused strike, not an AOE.
const TENTACLE_SLAP_RADIUS: f32 = 3.0;

/// In-water octopus body. One per equipped Cage slot. Hunts the
/// nearest enemy and triggers tentacle slaps when in engage range.
#[derive(Component)]
pub struct Octopus {
    pub owner_slot: usize,
    /// Seconds until this octopus may try to spawn another tentacle.
    pub spawn_cd: f32,
}

#[derive(Component, Clone, Copy, PartialEq, Eq)]
pub enum TentaclePhase {
    Emerge,
    Slap,
    Retreat,
}

/// One tentacle bursting out of the water. The visual is a chain
/// of N segment circles — children of this entity — laid out along
/// a hand-tuned question-mark / J curve in the Emerge phase, then
/// linearly interpolated toward a straight line during Slap so the
/// motion reads as an actual *whip-and-strike* rather than a pole
/// pivoting on its base.
///
/// Each tentacle homes on a specific enemy (`target`), so a fast-
/// moving enemy can't outrun the strike by stepping out of the
/// spawn radius before the slap connects.
#[derive(Component)]
pub struct OctopusTentacle {
    /// Owning octopus — used to credit damage to the right slot.
    pub source: Entity,
    /// The enemy this tentacle is locked onto. The tentacle's world
    /// position tracks the target each frame, so it doesn't slap
    /// empty water if the enemy moved during the Emerge phase. Set
    /// to `None` once the target despawns; the tentacle then
    /// finishes its phase in place and despawns.
    pub target: Option<Entity>,
    pub phase: TentaclePhase,
    pub timer: f32,
    pub damage: i32,
    /// Set the moment the slap-phase damage hits — guards against a
    /// second damage application across phase transitions.
    pub damage_dealt: bool,
    /// Effective rune list snapshotted at spawn time so the tentacle
    /// keeps its rune loadout even if the source slot's weapon is
    /// swapped during the 0.6s lifecycle.
    pub runes: Vec<crate::rune::Rune>,
    /// Resting rotation (in radians) the tentacle holds. Small
    /// per-spawn jitter so multiple tentacles striking the same
    /// target don't read as identical poses.
    pub whip_from: f32,
}

/// Number of segment circles per tentacle. Six is enough for a
/// readable question-mark curl in the Emerge phase without making
/// the chain feel beadier than tentacular.
const TENTACLE_SEGMENTS: usize = 6;
/// Per-segment circle radius. Slightly smaller than half the
/// historical `TENTACLE_WIDTH` so adjacent segments overlap a
/// touch and the chain reads as one continuous shape, not a row
/// of beads.
const TENTACLE_SEG_RADIUS: f32 = 0.85;

/// Tag on each child segment circle so the per-frame pose update
/// can index into the curled/straight tables and place it.
#[derive(Component, Clone, Copy)]
pub struct TentacleSegment {
    pub idx: u8,
}

/// Hand-tuned local positions for each segment when the tentacle is
/// fully curled (Emerge / Retreat poses). Forms a question-mark / J
/// shape: stalk rises along +Y, then the tip hooks right-and-back.
/// In tentacle-local space; the parent transform rotates / scales
/// the whole chain.
const CURLED_SEGMENTS: [(f32, f32); TENTACLE_SEGMENTS] = [
    (0.0, 0.0),
    (0.4, 1.1),
    (1.4, 2.0),
    (2.4, 2.6),
    (2.4, 3.4),
    (1.4, 3.9),
];

/// Local positions for each segment when the tentacle is fully
/// extended (end of Slap). Straight column along +Y, slightly
/// longer than the curled height so the strike covers more reach
/// than the rest pose suggests.
const STRAIGHT_SEGMENTS: [(f32, f32); TENTACLE_SEGMENTS] = [
    (0.0, 0.0),
    (0.0, 1.2),
    (0.0, 2.4),
    (0.0, 3.6),
    (0.0, 4.8),
    (0.0, 6.0),
];

/// Marker on the deck-side cage decoration child. `sync_cage_decor`
/// owns its lifecycle.
#[derive(Component)]
pub struct CageDecor;

/// Per-octopus drift trail — flat, low-key wake behind the body.
/// Same ribbon mesh shape as the boat trail, just shorter/thinner so
/// the visual hint reads as "subtle drift" not "big wake".
#[derive(Component)]
pub struct OctopusTrail {
    pub octopus: Entity,
    pub points: VecDeque<Vec2>,
    pub sample_timer: f32,
}

const OCTOPUS_TRAIL_SAMPLE_HZ: f32 = 25.0;
const OCTOPUS_TRAIL_MAX_POINTS: usize = 14;
const OCTOPUS_TRAIL_HEAD_WIDTH: f32 = 2.4;

pub struct OctopusPlugin;

impl Plugin for OctopusPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                sync_cage_decor,
                sync_octopus_units,
                octopus_ai,
                octopus_spawn_tentacles,
                tentacle_tick,
                // Must run after `tentacle_tick` so segments read
                // the just-decremented timer + phase to compute the
                // current extension — without `.after`, Bevy can
                // freely run them in parallel and segment poses lag
                // a frame behind the phase transition.
                update_tentacle_segments.after(tentacle_tick),
                update_octopus_trail,
            ),
        );
    }
}

/// Per-frame: place every tentacle segment along the curled →
/// straight interpolation based on its parent tentacle's current
/// phase. Emerge holds curled; Slap lerps to straight; Retreat
/// holds straight. The lerp IS the strike animation — the chain
/// uncurls and lashes out at the target.
pub fn update_tentacle_segments(
    tentacles: Query<&OctopusTentacle, Without<TentacleSegment>>,
    mut segments: Query<(&TentacleSegment, &ChildOf, &mut Transform)>,
) {
    for (seg, parent, mut tf) in &mut segments {
        let Ok(tent) = tentacles.get(parent.parent()) else { continue; };
        let ext = match tent.phase {
            TentaclePhase::Emerge => 0.0,
            // 0.0 at slap-phase start → 1.0 at end. Linear is
            // fine; the slap is short (~0.14s) so easing wouldn't
            // read anyway.
            TentaclePhase::Slap => {
                1.0 - (tent.timer / TENTACLE_SLAP_TIME).clamp(0.0, 1.0)
            }
            TentaclePhase::Retreat => 1.0,
        };
        let idx = seg.idx as usize;
        let (cx, cy) = CURLED_SEGMENTS[idx];
        let (sx, sy) = STRAIGHT_SEGMENTS[idx];
        let x = cx + (sx - cx) * ext;
        let y = cy + (sy - cy) * ext;
        tf.translation.x = x;
        tf.translation.y = y;
    }
}

/// Cage deck decoration — single dark square child per equipped Cage
/// slot. Mirrors `sync_booster_decor` / `sync_blade_decor`.
pub fn sync_cage_decor(
    mut commands: Commands,
    cfg: Res<TurretConfig>,
    pm: Option<Res<PaletteMaterials>>,
    mut meshes: ResMut<Assets<Mesh>>,
    slots: Query<(Entity, &TurretSlot, Option<&Children>)>,
    cages: Query<Entity, With<CageDecor>>,
) {
    if !cfg.is_changed() { return; }
    let Some(pm) = pm else { return; };

    let mut cage_mesh: Option<Handle<Mesh>> = None;

    for (slot_entity, slot, children) in &slots {
        let s = cfg.slots[slot.index];
        let want = s.equipped && matches!(s.weapon, WeaponType::Cage);

        let existing = children
            .into_iter()
            .flat_map(|c| c.iter())
            .find(|c| cages.get(*c).is_ok());

        match (want, existing) {
            (true, None) => {
                let mesh = cage_mesh
                    .get_or_insert_with(|| meshes.add(Rectangle::new(2.6, 2.6)))
                    .clone();
                let cage = commands.spawn((
                    Mesh2d(mesh),
                    MeshMaterial2d(pm.turret_cage.clone()),
                    Transform::from_xyz(0.0, 0.0, 0.05),
                    CageDecor,
                    RenderLayers::layer(PLAY_LAYER),
                )).id();
                commands.entity(cage).insert(ChildOf(slot_entity));
            }
            (false, Some(cage)) => {
                commands.entity(cage).despawn();
            }
            _ => {}
        }
    }
}

/// Maintain "exactly one Octopus per equipped Cage slot". Despawn
/// stale, spawn missing. The body is a free world-space entity so it
/// can hunt independently of the ship.
pub fn sync_octopus_units(
    mut commands: Commands,
    cfg: Res<TurretConfig>,
    pm: Option<Res<PaletteMaterials>>,
    mut meshes: ResMut<Assets<Mesh>>,
    ship_q: Query<&Transform, (With<Friendly>, Without<Octopus>)>,
    octopuses: Query<(Entity, &Octopus)>,
) {
    let Some(pm) = pm else { return; };

    for (e, oct) in &octopuses {
        let s = cfg.slots.get(oct.owner_slot).copied().unwrap_or_default();
        let valid = s.equipped && matches!(s.weapon, WeaponType::Cage);
        if !valid {
            commands.entity(e).despawn();
        }
    }

    let Ok(ship_tf) = ship_q.single() else { return; };
    let ship_pos = ship_tf.translation.truncate();
    let body_mesh = meshes.add(Circle::new(OCTOPUS_BODY_RADIUS));

    for (idx, slot) in cfg.slots.iter().enumerate() {
        if !slot.equipped { continue; }
        if !matches!(slot.weapon, WeaponType::Cage) { continue; }
        let already = octopuses.iter().any(|(_, o)| o.owner_slot == idx);
        if already { continue; }

        // Stagger initial spawn around the ship so multiple cages
        // don't overlap blobs at frame 1.
        let phase = (idx as f32) * std::f32::consts::TAU / 8.0;
        let init_pos = ship_pos + Vec2::new(phase.cos(), phase.sin()) * 14.0;

        let body = commands.spawn((
            Mesh2d(body_mesh.clone()),
            MeshMaterial2d(pm.octopus_body.clone()),
            Transform::from_xyz(init_pos.x, init_pos.y, 1.5),
            Octopus { owner_slot: idx, spawn_cd: 0.0 },
            RenderLayers::layer(PLAY_LAYER),
        )).id();

        // Spawn a flat drift trail behind the body — same ribbon
        // shape as the boat trail but shorter / thinner, so the
        // direction of travel reads at a glance without dominating
        // the visual.
        let trail_mesh = meshes.add(empty_dynamic_mesh());
        commands.spawn((
            Mesh2d(trail_mesh),
            MeshMaterial2d(pm.splash.clone()),
            Transform::from_xyz(0.0, 0.0, 1.4),
            OctopusTrail {
                octopus: body,
                points: VecDeque::new(),
                sample_timer: 0.0,
            },
            RenderLayers::layer(PLAY_LAYER),
        ));
    }
}

/// Sample-and-rebuild the octopus drift ribbon. Despawn orphan
/// trails when their octopus is gone (mirrors `update_enemy_trails`).
pub fn update_octopus_trail(
    time: Res<Time>,
    mut commands: Commands,
    octopus_q: Query<&Transform, (With<Octopus>, Without<OctopusTrail>)>,
    mut trail_q: Query<(Entity, &mut OctopusTrail, &Mesh2d)>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let dt = time.delta_secs();
    for (trail_e, mut trail, mesh2d) in &mut trail_q {
        let Ok(oct_tf) = octopus_q.get(trail.octopus) else {
            commands.entity(trail_e).despawn();
            continue;
        };
        trail.sample_timer -= dt;
        if trail.sample_timer > 0.0 { continue; }
        trail.sample_timer = 1.0 / OCTOPUS_TRAIL_SAMPLE_HZ;

        let head = oct_tf.translation.truncate();
        trail.points.push_front(head);
        while trail.points.len() > OCTOPUS_TRAIL_MAX_POINTS {
            trail.points.pop_back();
        }
        if let Some(mesh) = meshes.get_mut(&mesh2d.0) {
            rebuild_ribbon_mesh(mesh, &trail.points, OCTOPUS_TRAIL_HEAD_WIDTH);
        }
    }
}

/// Hunt-an-enemy AI for the body. Target selection goes through the
/// autonomous-unit picker — honours the slot's targeting runes
/// (relative to the SHIP) and applies modulo slot-spread so
/// multiple cages chase DIFFERENT enemies instead of dogpiling the
/// closest. Default (no rune) = nearest-to-this-octopus.
pub fn octopus_ai(
    time: Res<Time>,
    cfg: Res<TurretConfig>,
    synergies: Res<crate::synergy::Synergies>,
    stats: Res<crate::stats::PlayerStats>,
    ship_q: Query<&Transform, (With<Friendly>, Without<Octopus>, Without<Enemy>)>,
    enemies: Query<(&Transform, &crate::components::Health), (With<Enemy>, Without<Octopus>)>,
    mut octopuses: Query<(&mut Transform, &Octopus)>,
) {
    let dt = time.delta_secs();
    let ship_pos = ship_q.single().map(|t| t.translation.truncate()).unwrap_or(Vec2::ZERO);
    let speed_mult = synergies.autonomous_speed_mult();
    let rune_effect = stats.rune_damage_mult();
    let snapshot: Vec<(Vec2, i32)> = enemies
        .iter()
        .map(|(t, h)| (t.translation.truncate(), h.0))
        .collect();
    for (mut tf, oct) in &mut octopuses {
        let pos = tf.translation.truncate();
        // SlotCfg.runes is still the player's 3-fixed-socket config;
        // flatten Nones to get a `Vec<Rune>` slice for the picker /
        // hustle math.
        let runes: Vec<Rune> = cfg
            .slots
            .get(oct.owner_slot)
            .map(|s| s.runes.iter().copied().flatten().collect())
            .unwrap_or_default();
        let Some(target) = crate::weapon::pick_target(
            &snapshot, ship_pos, pos, &runes, None,
        )
        .map(|t| t + crate::weapon::offset_for_slot(oct.owner_slot))
        else { continue; };
        let to = target - pos;
        let dist = to.length();
        if dist < 0.1 { continue; }
        // Standoff: don't sit directly on top of the target. The
        // tentacles emerge AT the enemy, so the body itself wants
        // to hover a few units away (looks better, doesn't crowd
        // the enemy sprite). Once inside the standoff ring we stop
        // closing — actual engagement is the tentacle spawn loop's
        // job, gated on OCTOPUS_ENGAGE_RANGE.
        if dist <= OCTOPUS_STANDOFF { continue; }
        let dir = to / dist;
        // Autonomous synergy multiplies swim speed — octopuses
        // close the gap to their target faster with more equipped
        // Autonomous-tagged turrets. Hustle rune adds per-slot
        // speed on top.
        let hustle = crate::rune::hustle_speed_mult(&runes, rune_effect);
        let max_step = OCTOPUS_HUNT_SPEED * speed_mult * hustle * dt;
        // Stop at the standoff ring, not at the enemy itself.
        let close_to = (dist - OCTOPUS_STANDOFF).max(0.0);
        let step_len = close_to.min(max_step);
        let step = dir * step_len;
        tf.translation.x += step.x;
        tf.translation.y += step.y;
        // Clamp to the visible viewport so an octopus chasing an
        // edge-spawned enemy can't slip into the off-camera buffer
        // (especially relevant with `big_arena`). Body radius
        // margin keeps the blob fully on-screen, not half-clipped.
        let half_w = crate::balance::PLAY_WORLD_W * 0.5 - OCTOPUS_BODY_RADIUS - 1.0;
        let half_h = crate::balance::PLAY_WORLD_H * 0.5 - OCTOPUS_BODY_RADIUS - 1.0;
        tf.translation.x = tf.translation.x.clamp(-half_w, half_w);
        tf.translation.y = tf.translation.y.clamp(-half_h, half_h);
    }
}

/// When the octopus body is within `OCTOPUS_ENGAGE_RANGE` of an
/// enemy AND has fewer than its cap of active tentacles AND its
/// spawn cooldown is ready, spawn a fresh tentacle at the enemy's
/// position. Splash particles cue the emergence.
pub fn octopus_spawn_tentacles(
    time: Res<Time>,
    mut commands: Commands,
    cfg: Res<TurretConfig>,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    mut meshes: ResMut<Assets<Mesh>>,
    ship_q: Query<&Transform, (With<Friendly>, Without<Octopus>, Without<Enemy>)>,
    enemies: Query<(Entity, &Transform, &crate::components::Health), (With<Enemy>, Without<Octopus>)>,
    tentacles: Query<&OctopusTentacle>,
    mut octopuses: Query<(Entity, &Transform, &mut Octopus)>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();
    // Tentacle visual is a chain of `TENTACLE_SEGMENTS` small
    // circles, arranged along a question-mark curl during Emerge
    // and linearly interpolated to a straight column over Slap.
    // The chain reads as a real tentacle silhouette (curl + strike)
    // far better than a single capsule pivoting on its base.
    let seg_mesh = meshes.add(Circle::new(TENTACLE_SEG_RADIUS));
    let ship_pos = ship_q.single()
        .map(|t| t.translation.truncate())
        .unwrap_or(Vec2::ZERO);

    for (oct_entity, tf, mut oct) in &mut octopuses {
        oct.spawn_cd -= dt;
        let s = cfg.slots.get(oct.owner_slot).copied().unwrap_or_default();
        if !matches!(s.weapon, WeaponType::Cage) || !s.equipped { continue; }
        // Cage reads its runes from SlotCfg directly (the cage isn't
        // routed through `sync_turret_config`'s effective-rune merge,
        // since the octopus is its own entity, not a turret bullet).
        // Flatten once for the picker + tentacle snapshot below.
        let s_runes: Vec<Rune> = s.runes.iter().copied().flatten().collect();
        let cap = max_tentacles(s.barrels);
        let active_count = tentacles.iter().filter(|t| t.source == oct_entity).count();
        if active_count >= cap { continue; }
        if oct.spawn_cd > 0.0 { continue; }

        let body_pos = tf.translation.truncate();
        // Filter to enemies within engage range first, then run the
        // autonomous-unit picker (which honours the slot's
        // targeting runes relative to the SHIP). This keeps the
        // "I have to be close to slap" gate while letting
        // Furthest/HighestHp/LowestHp runes choose WHICH in-range
        // enemy to hit.
        let engage = engage_range_for(s.barrels);
        // Snapshot in-range enemies WITH their entity so the picked
        // target can be tracked across frames — previously the
        // tentacle only knew the target's spawn-time position, so
        // a fast enemy moving away during the 0.18s Emerge phase
        // would step out of the slap radius and the strike landed
        // on empty water.
        let in_range: Vec<(Entity, Vec2, i32)> = enemies
            .iter()
            .filter(|(_, t, h)| {
                h.0 > 0
                    && t.translation.truncate().distance_squared(body_pos) < engage * engage
            })
            .map(|(e, t, h)| (e, t.translation.truncate(), h.0))
            .collect();
        // Reuse the shared rune-aware picker by handing it a
        // position-only view, then map the picked position back to
        // its source entity. Positions came directly from the
        // snapshot above so an exact `==` compare resolves the
        // entity unambiguously.
        let positions_only: Vec<(Vec2, i32)> = in_range
            .iter()
            .map(|(_, p, h)| (*p, *h))
            .collect();
        let Some(target_pos) = crate::weapon::pick_target(
            &positions_only, ship_pos, body_pos, &s_runes, None,
        ) else { continue; };
        let Some(target_entity) = in_range
            .iter()
            .find(|(_, p, _)| *p == target_pos)
            .map(|(e, _, _)| *e)
        else { continue; };

        // Tentacle EMERGES FROM THE WATER right next to the enemy.
        // Small jitter avoids perfect stacking when multiple
        // tentacles converge on the same target. Whip-from angle is
        // randomized so the strike doesn't always swing the same
        // way — `tentacle_tick` then arcs through `WHIP_ARC` during
        // the Slap phase.
        let jitter = Vec2::new(rng.gen_range(-1.5..1.5), rng.gen_range(-1.5..1.5));
        let spawn_pos = target_pos + jitter;
        let whip_from = rng.gen_range(-0.6_f32..0.6);

        // Parent is the logical tentacle — invisible itself (no
        // mesh of its own), it just anchors world position +
        // rotation and propagates Y-scale to its segment children
        // for the rise/sink animation. Per-segment in-frame
        // positions are driven each tick by
        // `update_tentacle_segments`.
        let tentacle_entity = commands.spawn((
            Transform {
                translation: Vec3::new(spawn_pos.x, spawn_pos.y, 1.6),
                rotation: Quat::from_rotation_z(whip_from),
                // Start "underwater" — Y scale 0 grows to 1 during
                // Emerge so the chain seems to rise from below.
                scale: Vec3::new(1.0, 0.0, 1.0),
            },
            // Required for child-transform propagation on entities
            // that don't carry a mesh of their own.
            Visibility::Inherited,
            OctopusTentacle {
                source: oct_entity,
                target: Some(target_entity),
                phase: TentaclePhase::Emerge,
                timer: TENTACLE_EMERGE_TIME,
                damage: s.damage.max(1),
                damage_dealt: false,
                runes: s_runes.clone(),
                whip_from,
            },
        )).id();

        // Segment circles. Each starts in its CURLED pose; the
        // per-tick segment updater lerps toward STRAIGHT during the
        // Slap phase. Shared mesh + material so spawning all 6 is
        // cheap.
        for idx in 0..TENTACLE_SEGMENTS {
            let (cx, cy) = CURLED_SEGMENTS[idx];
            let seg = commands.spawn((
                Mesh2d(seg_mesh.clone()),
                MeshMaterial2d(pm.octopus_leg.clone()),
                Transform::from_xyz(cx, cy, 0.0),
                RenderLayers::layer(PLAY_LAYER),
                TentacleSegment { idx: idx as u8 },
            )).id();
            commands.entity(seg).insert(ChildOf(tentacle_entity));
        }

        // Light water-bubble cue at the emerge point — kept small
        // (3 motes at low speed) so it reads as "something's
        // surfacing" not "an explosion just went off". The real
        // impact-burst happens later in `tentacle_tick` on the
        // slap-damage frame.
        spawn_hit_particles(&mut commands, &em, &pm.splash, spawn_pos, 3, 20.0, &mut rng);

        // Cooldown scales with cap so total throughput stays linear:
        // `slaps/sec ≈ fire_rate × cap`. With cap=2 + fire_rate=1.5:
        // spawn_cd ≈ 0.33s → ~3 spawns/sec. With cap=6 same rate:
        // ~9 spawns/sec.
        let rate = s.fire_rate.max(0.1);
        oct.spawn_cd = 1.0 / (rate * cap as f32);
    }
}

/// Per-frame: animate each tentacle's emerge → slap → retreat
/// scale and apply the slap-phase damage at its spawn position.
/// Tentacles stay put in the water through their lifecycle — the
/// body is not visually connected.
pub fn tentacle_tick(
    time: Res<Time>,
    mut commands: Commands,
    mut queue: ResMut<PendingDamageQueue>,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    octopus_q: Query<&Octopus, Without<OctopusTentacle>>,
    mut tentacles: Query<(Entity, &mut Transform, &mut OctopusTentacle), Without<Enemy>>,
    enemies: Query<
        (&Transform, &crate::components::Health),
        (With<Enemy>, Without<OctopusTentacle>, Without<Octopus>),
    >,
) {
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();
    for (e, mut tf, mut tent) in &mut tentacles {
        tent.timer -= dt;

        // Follow the target during Emerge + Slap so the visual sits
        // on the enemy through the strike. Retreat phase locks the
        // last position (target may have died — keep retracting
        // from where the slap landed). If the target despawned
        // mid-Emerge, clear the lock so the tentacle finishes its
        // current phase in place.
        if matches!(tent.phase, TentaclePhase::Emerge | TentaclePhase::Slap) {
            if let Some(t) = tent.target {
                if let Ok((etf, h)) = enemies.get(t) {
                    if h.0 > 0 {
                        let p = etf.translation.truncate();
                        tf.translation.x = p.x;
                        tf.translation.y = p.y;
                    } else {
                        tent.target = None;
                    }
                } else {
                    tent.target = None;
                }
            }
        }

        // Per-phase Y-scale on the parent (children inherit).
        // Emerge stretches from underwater (scale 0) to full
        // height (scale 1) WHILE the segments stay in the curled
        // pose. Slap holds the height steady — the visual whip
        // comes from segments lerping to STRAIGHT, driven by
        // `update_tentacle_segments`. Retreat retracts back to 0
        // with segments held straight, like the tentacle's diving
        // back under after the strike.
        let s = match tent.phase {
            TentaclePhase::Emerge =>
                (1.0 - tent.timer / TENTACLE_EMERGE_TIME).clamp(0.0, 1.0),
            TentaclePhase::Slap => 1.0,
            TentaclePhase::Retreat =>
                (tent.timer / TENTACLE_RETREAT_TIME).clamp(0.0, 1.0),
        };
        tf.scale.y = s;
        // Rotation pinned to the per-spawn jitter — segment
        // straightening replaces the old rotation-whip animation.
        tf.rotation = Quat::from_rotation_z(tent.whip_from);

        // Phase transitions + damage hit.
        if tent.timer <= 0.0 {
            match tent.phase {
                TentaclePhase::Emerge => {
                    tent.phase = TentaclePhase::Slap;
                    tent.timer = TENTACLE_SLAP_TIME;
                    if !tent.damage_dealt {
                        let source = octopus_q
                            .get(tent.source)
                            .ok()
                            .map(|o| DamageSource::PlayerSlot(o.owner_slot as u8));
                        // Damage the tracked target entity directly
                        // — no radius check at the slap position
                        // anymore. A small reach guard still applies
                        // so a target that teleported (e.g. boss
                        // ability) doesn't get hit from off-screen.
                        if let Some(target) = tent.target {
                            if let Ok((etf, h)) = enemies.get(target) {
                                if h.0 > 0 {
                                    let slap_pos = tf.translation.truncate();
                                    let ep = etf.translation.truncate();
                                    let reach = TENTACLE_SLAP_RADIUS + TENTACLE_LENGTH;
                                    if ep.distance_squared(slap_pos) <= reach * reach {
                                        // Impact burst — the actual
                                        // moment of the slap. Small
                                        // directional flick (8 motes
                                        // at moderate speed) reads as
                                        // a strike connecting, not a
                                        // generic explosion.
                                        if let (Some(pm), Some(em)) = (pm.as_deref(), em.as_deref()) {
                                            spawn_hit_particles(
                                                &mut commands, em, &pm.octopus_leg,
                                                ep, 8, 55.0, &mut rng,
                                            );
                                        }
                                        queue.push_initial(
                                            target, tent.damage, slap_pos,
                                            WeaponType::Cage, source, &tent.runes,
                                        );
                                        tent.damage_dealt = true;
                                    }
                                }
                            }
                        }
                    }
                }
                TentaclePhase::Slap => {
                    tent.phase = TentaclePhase::Retreat;
                    tent.timer = TENTACLE_RETREAT_TIME;
                }
                TentaclePhase::Retreat => {
                    commands.entity(e).despawn();
                }
            }
        }
    }
}
