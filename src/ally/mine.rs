//! Sea mines: a `MineLayer`-equipped ship drops timed proximity mines
//! in its wake. Mines persist after the laying ship moves on, creating
//! an emergent area-denial pattern. Faction-agnostic via cached
//! `target_faction`.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;

use crate::balance::PLAY_LAYER;
use crate::components::{Faction, FactionKind, Health, Heading};
use crate::effects::{spawn_hit_particles, EffectMeshes, HitFx};
use crate::palette::PaletteMaterials;

use super::{Ally, ShipClass};

/// A timed proximity sea mine. Free-standing entity in world space —
/// outlives the laying ship.
#[derive(Component)]
pub struct Mine {
    pub damage: i32,
    pub blast_radius: f32,
    /// Inert while > 0 so the laying ship can clear the drop point
    /// without blowing itself up.
    pub arm_timer: f32,
    /// Silent despawn when this hits 0 — stops long combats from
    /// accumulating a dense minefield.
    pub lifetime: f32,
    /// Cached at drop time because the mine outlives the laying ship.
    pub target_faction: FactionKind,
}

/// Marker on the red centre dot inside a mine. Drives `flash_mine_dots`
/// to toggle visibility so the dot reads as a blinking warning light.
#[derive(Component)]
pub struct MineDotFlash;

/// Mine launcher mounted on a ship. Drops one mine every
/// `drop_interval` seconds at the ship's stern position.
#[derive(Component)]
pub struct MineLayer {
    pub drop_interval: f32,
    pub cd: f32,
    pub mine_damage: i32,
    pub mine_blast_radius: f32,
    pub target_faction: FactionKind,
}

/// Initial arm delay — long enough to prevent self-detonation, short
/// enough to punish chasing.
const MINE_ARM_DELAY: f32 = 0.6;
/// Silent despawn at this age.
const MINE_LIFETIME: f32 = 18.0;

fn spawn_mine(
    commands: &mut Commands,
    em: &EffectMeshes,
    pm: &PaletteMaterials,
    pos: Vec2,
    damage: i32,
    blast_radius: f32,
    target_faction: FactionKind,
) {
    let mine = commands.spawn((
        Mesh2d(em.mine_outer.clone()),
        MeshMaterial2d(pm.mine_outer.clone()),
        Transform::from_xyz(pos.x, pos.y, 0.6),
        Mine {
            damage,
            blast_radius,
            arm_timer: MINE_ARM_DELAY,
            lifetime: MINE_LIFETIME,
            target_faction,
        },
        RenderLayers::layer(PLAY_LAYER),
    )).id();

    let dot = commands.spawn((
        Mesh2d(em.mine_inner.clone()),
        MeshMaterial2d(pm.mine_inner.clone()),
        Transform::from_xyz(0.0, 0.0, 0.05),
        MineDotFlash,
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    commands.entity(dot).insert(ChildOf(mine));
}

/// Tick each `MineLayer` and drop at the stern position when due.
pub fn mine_layer_drop(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    mut layers: Query<(&Transform, &Heading, &Ally, &mut MineLayer)>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();

    for (tf, heading, ally, mut layer) in &mut layers {
        layer.cd -= dt;
        if layer.cd > 0.0 { continue; }
        layer.cd = layer.drop_interval;

        let pos = tf.translation.truncate();
        let h = heading.0;
        let forward = Vec2::new(-h.sin(), h.cos());
        let (_hull_w, hull_h) = ally.class.hull_dims();
        let drop_pos = pos - forward * (hull_h * 0.5 + 1.0);
        spawn_mine(
            &mut commands, &em, &pm, drop_pos,
            layer.mine_damage, layer.mine_blast_radius, layer.target_faction,
        );
    }
}

/// Tick mines: arm timer + lifetime + proximity detonation. A mine
/// detonates when any unit of its `target_faction` enters
/// `blast_radius`, dealing `damage` to every same-faction unit in
/// range. Lifetime expiry is silent — no boom — to avoid a stray
/// explosion surprising the player.
pub fn mine_tick(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    mut victims: Query<(Entity, &Transform, &Faction, &mut Health, &mut HitFx)>,
    mut mines: Query<(Entity, &Transform, &mut Mine)>,
    mut stats: ResMut<crate::ui::DamageStats>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();

    let victim_snap: Vec<(Entity, Vec2, FactionKind)> = victims
        .iter()
        .map(|(e, t, f, _, _)| (e, t.translation.truncate(), f.0))
        .collect();

    for (mine_e, mine_tf, mut mine) in &mut mines {
        mine.arm_timer = (mine.arm_timer - dt).max(0.0);
        mine.lifetime -= dt;
        if mine.lifetime <= 0.0 {
            commands.entity(mine_e).despawn();
            continue;
        }
        if mine.arm_timer > 0.0 { continue; }

        let mp = mine_tf.translation.truncate();
        let r2 = mine.blast_radius * mine.blast_radius;
        let triggered = victim_snap.iter().any(|(_, p, f)| {
            *f == mine.target_faction && p.distance_squared(mp) < r2
        });
        if !triggered { continue; }

        // AOE damage — every same-faction unit within blast radius
        // takes the full hit. No falloff for now.
        for &(e, ep, f) in &victim_snap {
            if f != mine.target_faction { continue; }
            if ep.distance_squared(mp) >= r2 { continue; }
            if let Ok((_, _, _, mut h, mut fx)) = victims.get_mut(e) {
                let dealt = crate::bullet::apply_damage(&mut h, &mut fx, mine.damage);
                crate::bullet::credit_damage(
                    &mut stats,
                    Some(crate::bullet::DamageSource::Ally(ShipClass::Minelayer)),
                    dealt,
                );
            }
        }

        spawn_hit_particles(&mut commands, &em, &pm.mine_outer, mp, 12, 80.0,  &mut rng);
        spawn_hit_particles(&mut commands, &em, &pm.mine_inner, mp, 8,  100.0, &mut rng);
        commands.entity(mine_e).despawn();
    }
}

/// Blink the warning dot fully on / off. Scale-pulsing was unreliable
/// at the play area's nearest-neighbor upscale — sub-pixel radii drop
/// below the integer grid and skip rendering on some frames, so the
/// flash looked uneven. A binary `Visibility` toggle keeps every red
/// pixel flashing uniformly across mines.
///
/// Pattern: 0.55 s on, 0.25 s off (0.8 s cycle).
pub fn flash_mine_dots(
    time: Res<Time>,
    mut q: Query<&mut Visibility, With<MineDotFlash>>,
) {
    const PERIOD:       f32 = 0.8;
    const ON_DURATION:  f32 = 0.55;
    let cycle = time.elapsed_secs().rem_euclid(PERIOD);
    let want = if cycle < ON_DURATION {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    for mut vis in &mut q {
        if *vis != want { *vis = want; }
    }
}
