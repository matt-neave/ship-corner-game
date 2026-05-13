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
const TENTACLE_WIDTH:  f32 = 1.4;
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

/// One tentacle bursting out of the water. Free entity in world
/// space — visually disconnected from the octopus body (which lurks
/// below the surface). Each tentacle homes on a specific enemy
/// (`target`), so a fast-moving enemy can't outrun the strike by
/// stepping out of the spawn radius before the slap connects.
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
    /// Slot rune sockets snapshotted at spawn time so the tentacle
    /// keeps its rune loadout even if the source slot's weapon is
    /// swapped during the 0.6s lifecycle.
    pub runes: [Option<crate::rune::Rune>; 3],
    /// Resting rotation (in radians) the tentacle holds during the
    /// Emerge phase — slight per-spawn tilt so multiple tentacles
    /// don't read as identical pillars. The Slap phase whips
    /// `whip_from + WHIP_ARC` so the tip arcs through the strike,
    /// then snaps back for Retreat.
    pub whip_from: f32,
}

/// Whip arc in radians — how far the tentacle rotates through the
/// Slap phase. Big enough that the tip motion reads as a strike
/// rather than a static pole, small enough that the silhouette
/// still reads as the same tentacle through the action.
const TENTACLE_WHIP_ARC: f32 = 0.9;

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
                update_octopus_trail,
            ),
        );
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
    ship_q: Query<&Transform, (With<Friendly>, Without<Octopus>, Without<Enemy>)>,
    enemies: Query<(&Transform, &crate::components::Health), (With<Enemy>, Without<Octopus>)>,
    mut octopuses: Query<(&mut Transform, &Octopus)>,
) {
    let dt = time.delta_secs();
    let ship_pos = ship_q.single().map(|t| t.translation.truncate()).unwrap_or(Vec2::ZERO);
    let speed_mult = synergies.autonomous_speed_mult();
    let snapshot: Vec<(Vec2, i32)> = enemies
        .iter()
        .map(|(t, h)| (t.translation.truncate(), h.0))
        .collect();
    for (mut tf, oct) in &mut octopuses {
        let pos = tf.translation.truncate();
        let runes = cfg.slots.get(oct.owner_slot)
            .map(|s| s.runes)
            .unwrap_or([None; 3]);
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
        // Autonomous-tagged turrets.
        let max_step = OCTOPUS_HUNT_SPEED * speed_mult * dt;
        // Stop at the standoff ring, not at the enemy itself.
        let close_to = (dist - OCTOPUS_STANDOFF).max(0.0);
        let step_len = close_to.min(max_step);
        let step = dir * step_len;
        tf.translation.x += step.x;
        tf.translation.y += step.y;
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
    mut materials: ResMut<Assets<ColorMaterial>>,
    ship_q: Query<&Transform, (With<Friendly>, Without<Octopus>, Without<Enemy>)>,
    enemies: Query<(Entity, &Transform, &crate::components::Health), (With<Enemy>, Without<Octopus>)>,
    tentacles: Query<&OctopusTentacle>,
    mut octopuses: Query<(Entity, &Transform, &mut Octopus)>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();
    // Tapered capsule shape reads as a "tentacle column rising out
    // of the water" much better than a flat rectangle. Half-width =
    // TENTACLE_WIDTH/2, body length = TENTACLE_LENGTH - WIDTH so
    // the rounded ends are factored in (Capsule2d adds them on top
    // of the body height).
    let tentacle_mesh = meshes.add(Capsule2d::new(
        TENTACLE_WIDTH * 0.5,
        (TENTACLE_LENGTH - TENTACLE_WIDTH).max(0.0),
    ));
    // Suction-cup dots — small white circles spaced along the
    // tentacle to suggest segmentation / underside texture. Children
    // of the tentacle so they inherit the whip rotation.
    let cup_mesh = meshes.add(Circle::new(0.45));
    let cup_mat = materials.add(Color::srgb(0.95, 0.92, 0.86));
    let ship_pos = ship_q.single()
        .map(|t| t.translation.truncate())
        .unwrap_or(Vec2::ZERO);

    for (oct_entity, tf, mut oct) in &mut octopuses {
        oct.spawn_cd -= dt;
        let s = cfg.slots.get(oct.owner_slot).copied().unwrap_or_default();
        if !matches!(s.weapon, WeaponType::Cage) || !s.equipped { continue; }
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
            &positions_only, ship_pos, body_pos, &s.runes, None,
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

        let tentacle_entity = commands.spawn((
            Mesh2d(tentacle_mesh.clone()),
            MeshMaterial2d(pm.octopus_leg.clone()),
            Transform {
                translation: Vec3::new(spawn_pos.x, spawn_pos.y, 1.6),
                rotation: Quat::from_rotation_z(whip_from),
                // Start "underwater" — Y scale 0 grows to 1 during
                // the Emerge phase via `tentacle_tick`.
                scale: Vec3::new(1.0, 0.0, 1.0),
            },
            OctopusTentacle {
                source: oct_entity,
                target: Some(target_entity),
                phase: TentaclePhase::Emerge,
                timer: TENTACLE_EMERGE_TIME,
                damage: s.damage.max(1),
                damage_dealt: false,
                runes: s.runes,
                whip_from,
            },
            RenderLayers::layer(PLAY_LAYER),
        )).id();

        // Suction-cup dots — two small white circles along the
        // upper half of the tentacle. Children so the whip rotation
        // takes them along. Local Y offsets are in tentacle-frame
        // (the +Y of the capsule is "up"), so the cups sit toward
        // the tip where the eye looks during a strike.
        for cup_y in [TENTACLE_LENGTH * 0.18, TENTACLE_LENGTH * 0.36] {
            let cup = commands.spawn((
                Mesh2d(cup_mesh.clone()),
                MeshMaterial2d(cup_mat.clone()),
                Transform::from_xyz(0.0, cup_y, 0.02),
                RenderLayers::layer(PLAY_LAYER),
            )).id();
            commands.entity(cup).insert(ChildOf(tentacle_entity));
        }

        // Splash burst at the spawn point — a small pink puff
        // sells "something just came out of the water near you".
        spawn_hit_particles(&mut commands, &em, &pm.octopus_leg, spawn_pos, 6, 35.0, &mut rng);

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
    octopus_q: Query<&Octopus, Without<OctopusTentacle>>,
    mut tentacles: Query<(Entity, &mut Transform, &mut OctopusTentacle), Without<Enemy>>,
    enemies: Query<
        (&Transform, &crate::components::Health),
        (With<Enemy>, Without<OctopusTentacle>, Without<Octopus>),
    >,
) {
    let dt = time.delta_secs();
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

        // Per-phase emerge/retreat Y-scale (0..1). Slap holds at
        // full extension while the rotation whip plays out below.
        let s = match tent.phase {
            TentaclePhase::Emerge =>
                (1.0 - tent.timer / TENTACLE_EMERGE_TIME).clamp(0.0, 1.0),
            TentaclePhase::Slap => 1.0,
            TentaclePhase::Retreat =>
                (tent.timer / TENTACLE_RETREAT_TIME).clamp(0.0, 1.0),
        };
        tf.scale.y = s;

        // Rotation whip — the headline visual cue that this is a
        // *strike*, not just a column rising and falling. Emerge
        // holds `whip_from`; Slap arcs through `WHIP_ARC` to
        // produce a clean tip swing; Retreat holds the end of the
        // arc as the tentacle retracts mid-strike-pose. Without
        // this, the slap phase had zero visible motion (just a
        // 0.14s static frame), which is why the attack didn't read.
        let rot = match tent.phase {
            TentaclePhase::Emerge => tent.whip_from,
            TentaclePhase::Slap => {
                // 1.0 at phase start → 0.0 at end.
                let progress = 1.0 - (tent.timer / TENTACLE_SLAP_TIME).clamp(0.0, 1.0);
                tent.whip_from + TENTACLE_WHIP_ARC * progress
            }
            TentaclePhase::Retreat => tent.whip_from + TENTACLE_WHIP_ARC,
        };
        tf.rotation = Quat::from_rotation_z(rot);

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
