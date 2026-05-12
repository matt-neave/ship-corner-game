//! Blackbeard's boarding system. Launches a cluster of small boarder
//! figures across to the target, which tick damage for a few seconds
//! before vanishing. The "loaded and ready" cooldown gate means the
//! very first enemy that walks into range triggers an immediate launch
//! — no wasted reload progress while idle.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;
use rand::Rng;

use crate::balance::PLAY_LAYER;
use crate::components::{Faction, FactionKind, Health};
use crate::effects::{EffectMeshes, HitFx};
use crate::enemy::Enemy;
use crate::palette::PaletteMaterials;

use super::ShipClass;

/// Boarding-party launcher (Blackbeard). When a target-faction enemy is
/// within `range`, spawns `party_size` boarders aimed at it.
///
/// `ready = true` pauses the cooldown — the party is loaded and ready
/// to deploy as soon as something walks into range, so an idle
/// Blackbeard never wastes reload progress.
#[derive(Component)]
pub struct BoardingLauncher {
    pub fire_rate: f32,
    pub cd: f32,
    pub ready: bool,
    pub range: f32,
    pub party_size: u8,
    pub damage_per_tick: i32,
    pub tick_interval: f32,
    pub attach_duration: f32,
    pub target_faction: FactionKind,
}

#[derive(Clone, Copy)]
pub enum BoarderState {
    /// Lerping from source to target. `t` is 0..1 progress.
    Traveling { t: f32 },
    /// Stuck to the target with a random offset; ticks damage every
    /// `tick_interval` until `remaining` runs out.
    Attached { remaining: f32 },
}

/// Visible rope strung between source and target. Lives for the full
/// launch cycle so the boarders read as crew traveling along the rope.
#[derive(Component)]
pub struct BoardingRope {
    pub source: Entity,
    pub target: Entity,
    pub lifetime: f32,
}

/// One boarder dot. Hops the gap, sticks to the target, drips damage.
/// Despawns when the attach timer expires or when source/target are
/// gone mid-flight.
#[derive(Component)]
pub struct Boarder {
    pub source: Entity,
    pub target: Entity,
    pub state: BoarderState,
    /// Random offset around the target so multiple boarders cluster
    /// instead of stacking.
    pub offset: Vec2,
    pub damage_per_tick: i32,
    pub tick_interval: f32,
    pub tick_cd: f32,
    /// Total attach lifetime — passed in from the launcher so the
    /// duration is per-ship without `boarder_tick` reaching back.
    pub attach_duration: f32,
}

/// Maximum range at which a `BoardingLauncher` commits a party. Boarders
/// track the target entity, so as long as it's inside this radius at
/// fire time the party will catch up even if it moves.
pub const BOARDING_RANGE: f32 = 45.0;
/// Travel-state lerp rate — 1.4 ≈ 0.7 s end-to-end so boarders read as
/// people *crossing* the rope, not a tracer flashing across.
const BOARDER_TRAVEL_RATE: f32 = 1.4;

pub fn boarding_launcher_fire(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    candidates: Query<(Entity, &Transform, &Faction)>,
    mut launchers: Query<(Entity, &Transform, &mut BoardingLauncher)>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();

    for (launcher_e, launcher_tf, mut launcher) in &mut launchers {
        // Reload only while idle. Once `ready` flips on, the cooldown
        // stops draining — boarders sit cached, waiting for a target.
        if !launcher.ready {
            launcher.cd = (launcher.cd - dt).max(0.0);
            if launcher.cd <= 0.0 {
                launcher.ready = true;
            }
        }

        let pos = launcher_tf.translation.truncate();
        let r2 = launcher.range * launcher.range;
        let nearest = candidates.iter()
            .filter(|(_, _, f)| f.0 == launcher.target_faction)
            .map(|(e, t, _)| {
                let p = t.translation.truncate();
                (e, p, p.distance_squared(pos))
            })
            .filter(|(_, _, d2)| *d2 <= r2)
            .min_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));

        let Some((target_e, _, _)) = nearest else { continue; };
        if !launcher.ready { continue; }
        launcher.ready = false;
        launcher.cd = 1.0 / launcher.fire_rate.max(0.001);

        // Rope lives until the last boarder would have despawned, so
        // it reads as the connection the crew is currently using.
        let rope_lifetime = 0.4 + launcher.attach_duration + 0.2;
        commands.spawn((
            Mesh2d(em.beam.clone()),
            MeshMaterial2d(pm.boarding_rope.clone()),
            // Z = 4.4 — above bullets/beams (4.0/5.5), below boarders
            // (4.5) so the boarders ride visually on top.
            Transform::from_xyz(pos.x, pos.y, 4.4),
            BoardingRope {
                source: launcher_e,
                target: target_e,
                lifetime: rope_lifetime,
            },
            RenderLayers::layer(PLAY_LAYER),
        ));

        for _ in 0..launcher.party_size {
            let offset = Vec2::new(
                rng.gen_range(-2.5..2.5),
                rng.gen_range(-2.5..2.5),
            );
            commands.spawn((
                Mesh2d(em.boarder_dot.clone()),
                MeshMaterial2d(pm.boarder.clone()),
                Transform::from_xyz(pos.x, pos.y, 4.5),
                Boarder {
                    source: launcher_e,
                    target: target_e,
                    state: BoarderState::Traveling { t: 0.0 },
                    offset,
                    damage_per_tick: launcher.damage_per_tick,
                    tick_interval: launcher.tick_interval,
                    tick_cd: 0.0,
                    attach_duration: launcher.attach_duration,
                },
                RenderLayers::layer(PLAY_LAYER),
            ));
        }
    }
}

/// Drive every boarder. Despawns on source/target gone mid-flight so
/// we never end up with orphan dots after a chaotic frame.
pub fn boarder_tick(
    time: Res<Time>,
    mut commands: Commands,
    sources: Query<&Transform, (Without<Boarder>, Without<Enemy>)>,
    mut targets: Query<(&Transform, &mut Health, &mut HitFx), With<Enemy>>,
    // `Without<Enemy>` makes this query provably disjoint from
    // `targets` — boarders are friendly-side spawned, so the filter
    // just teaches the type system that.
    mut boarders: Query<(Entity, &mut Transform, &mut Boarder), Without<Enemy>>,
    mut stats: ResMut<crate::ui::DamageStats>,
) {
    let dt = time.delta_secs();

    for (boarder_e, mut tf, mut boarder) in &mut boarders {
        match boarder.state {
            BoarderState::Traveling { t } => {
                let Ok(src_tf) = sources.get(boarder.source) else {
                    commands.entity(boarder_e).despawn();
                    continue;
                };
                let Ok((target_tf, _, _)) = targets.get(boarder.target) else {
                    commands.entity(boarder_e).despawn();
                    continue;
                };
                let new_t = (t + dt * BOARDER_TRAVEL_RATE).min(1.0);
                let pos = src_tf.translation.truncate()
                    .lerp(target_tf.translation.truncate(), new_t);
                tf.translation.x = pos.x;
                tf.translation.y = pos.y;

                if new_t >= 1.0 {
                    boarder.state = BoarderState::Attached {
                        remaining: boarder.attach_duration,
                    };
                    boarder.tick_cd = 0.0;
                } else {
                    boarder.state = BoarderState::Traveling { t: new_t };
                }
            }
            BoarderState::Attached { remaining } => {
                let Ok((target_tf, mut h, mut fx)) =
                    targets.get_mut(boarder.target)
                else {
                    commands.entity(boarder_e).despawn();
                    continue;
                };
                let pos = target_tf.translation.truncate() + boarder.offset;
                tf.translation.x = pos.x;
                tf.translation.y = pos.y;

                let new_remaining = remaining - dt;
                boarder.tick_cd -= dt;
                if boarder.tick_cd <= 0.0 {
                    boarder.tick_cd = boarder.tick_interval;
                    let dealt = crate::bullet::apply_damage(&mut h, &mut fx, boarder.damage_per_tick);
                    crate::bullet::credit_damage(
                        &mut stats,
                        Some(crate::bullet::DamageSource::Ally(ShipClass::Blackbeard)),
                        dealt,
                    );
                }

                if new_remaining <= 0.0 {
                    commands.entity(boarder_e).despawn();
                } else {
                    boarder.state = BoarderState::Attached { remaining: new_remaining };
                }
            }
        }
    }
}

/// Anchor every active rope between source and target each frame and
/// tick its lifetime. The rope reuses the existing beam mesh (long
/// axis = +Y, scale.x = thickness, scale.y = length-fraction).
pub fn update_boarding_ropes(
    time: Res<Time>,
    mut commands: Commands,
    sources: Query<&Transform, (Without<BoardingRope>, Without<Enemy>, Without<Boarder>)>,
    targets: Query<&Transform, (With<Enemy>, Without<BoardingRope>)>,
    mut ropes: Query<(Entity, &mut Transform, &mut BoardingRope)>,
) {
    let dt = time.delta_secs();
    for (rope_e, mut tf, mut rope) in &mut ropes {
        rope.lifetime -= dt;
        if rope.lifetime <= 0.0 {
            commands.entity(rope_e).despawn();
            continue;
        }
        let Ok(src_tf) = sources.get(rope.source) else {
            commands.entity(rope_e).despawn();
            continue;
        };
        let Ok(tgt_tf) = targets.get(rope.target) else {
            commands.entity(rope_e).despawn();
            continue;
        };
        let a = src_tf.translation.truncate();
        let b = tgt_tf.translation.truncate();
        let delta = b - a;
        let len = delta.length();
        if len < 0.5 { continue; }
        let mid = (a + b) * 0.5;
        let angle = (-delta.x).atan2(delta.y);
        tf.translation.x = mid.x;
        tf.translation.y = mid.y;
        tf.rotation = Quat::from_rotation_z(angle);
        tf.scale = Vec3::new(1.5, len / crate::balance::BEAM_LENGTH, 1.0);
    }
}

// `_faction` parameter is only referenced via `BoardingLauncher` so the
// import lives there; no further uses inside this module.
#[allow(dead_code)]
type _UseFactionKindMarker = FactionKind;
