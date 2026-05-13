//! Rammer variant + its time-fused landmine. Rammer is a kamikaze with
//! a smaller payload than Bomber; on death (any cause) it drops a
//! 3-second-fused mine. Damage applies to friendly + non-submerged
//! allies in `blast_radius` — mirrors the Mortar splash pattern.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;

use crate::ally::{ally_is_submerged, Ally};
use crate::balance::PLAY_LAYER;
use crate::components::{Friendly, Health};
use crate::effects::{spawn_hit_particles, EffectMeshes, HitFx};
use crate::palette::PaletteMaterials;

use super::EnemyLandmine;

/// Time-fuse on the landmine a Rammer drops on death. Long enough to
/// read + walk away, short enough that lingering = pain.
pub const RAMMER_MINE_FUSE: f32 = 3.0;
/// Damage dealt to any unit inside the blast radius when it cooks off.
pub const RAMMER_MINE_DAMAGE: i32 = 6;
/// World-units radius of the Rammer mine's AOE.
pub const RAMMER_MINE_RADIUS: f32 = 9.0;

/// Spawn the time-fused landmine a Rammer leaves behind. Two-tone disc
/// — dark shell + warning-orange dot — so the silhouette reads as
/// "stay clear".
pub fn spawn_rammer_landmine(
    commands: &mut Commands,
    pm: &PaletteMaterials,
    meshes: &mut Assets<Mesh>,
    pos: Vec2,
) {
    let outer_mesh = meshes.add(Circle::new(1.5));
    let inner_mesh = meshes.add(Circle::new(0.6));
    let mine = commands.spawn((
        Mesh2d(outer_mesh),
        MeshMaterial2d(pm.mine_outer.clone()),
        Transform::from_xyz(pos.x, pos.y, 0.5),
        EnemyLandmine {
            fuse: RAMMER_MINE_FUSE,
            damage: RAMMER_MINE_DAMAGE,
            blast_radius: RAMMER_MINE_RADIUS,
        },
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    let dot = commands.spawn((
        Mesh2d(inner_mesh),
        MeshMaterial2d(pm.enemy_mine_dot.clone()),
        Transform::from_xyz(0.0, 0.0, 0.05),
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    commands.entity(dot).insert(ChildOf(mine));
}

/// Tick armed mines. On `fuse <= 0`, AOE damage + particle burst +
/// despawn.
pub fn enemy_landmine_tick(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    mut mines: Query<(Entity, &Transform, &mut EnemyLandmine)>,
    mut friendly: Query<
        (&Transform, &mut Health, &mut HitFx),
        (With<Friendly>, Without<Ally>, Without<EnemyLandmine>),
    >,
    mut allies: Query<
        (&Transform, &Ally, &mut Health, &mut HitFx),
        (With<Ally>, Without<Friendly>, Without<EnemyLandmine>),
    >,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();

    for (entity, tf, mut mine) in &mut mines {
        mine.fuse -= dt;
        let center = tf.translation.truncate();
        let r2 = mine.blast_radius * mine.blast_radius;

        // Impact trigger: any non-submerged friendly/ally inside the
        // blast radius cooks the mine off immediately, even if the
        // fuse hasn't expired yet.
        let mut detonate = mine.fuse <= 0.0;
        if !detonate {
            if let Ok((ftf, _, _)) = friendly.single_mut() {
                if ftf.translation.truncate().distance_squared(center) < r2 {
                    detonate = true;
                }
            }
        }
        if !detonate {
            for (atf, ally, _, _) in &mut allies {
                if ally_is_submerged(ally) { continue; }
                if atf.translation.truncate().distance_squared(center) < r2 {
                    detonate = true;
                    break;
                }
            }
        }
        if !detonate { continue; }

        if let Ok((ftf, mut h, mut fx)) = friendly.single_mut() {
            if ftf.translation.truncate().distance_squared(center) < r2 {
                fx.pulse();
                h.0 = (h.0 - mine.damage).max(0);
            }
        }
        for (atf, ally, mut h, mut fx) in &mut allies {
            if ally_is_submerged(ally) { continue; }
            if atf.translation.truncate().distance_squared(center) >= r2 { continue; }
            fx.pulse();
            h.0 = (h.0 - mine.damage).max(0);
        }

        spawn_hit_particles(&mut commands, &em, &pm.enemy,          center, 16, 90.0,  &mut rng);
        spawn_hit_particles(&mut commands, &em, &pm.enemy_mine_dot, center, 10, 110.0, &mut rng);
        commands.entity(entity).despawn();
    }
}
