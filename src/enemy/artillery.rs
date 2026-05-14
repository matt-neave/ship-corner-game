//! Artillery variant: arcing lobbed shells with a telegraphed landing
//! reticle. Body heading is decoupled from shot direction — the shell
//! doesn't need the body to point at the target.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;

use crate::ally::{ally_is_submerged, Ally};
use crate::balance::PLAY_LAYER;
use crate::components::{Friendly, Health, Velocity};
use crate::effects::{spawn_hit_particles, EffectMeshes, HitFx};
use crate::palette::PaletteMaterials;

use super::{Enemy, EnemyVariant};

/// Distance the Artillery wants to keep. Same shape as `SNIPER_DESIRED_DIST`
/// but farther back.
pub const ARTILLERY_DESIRED_DIST: f32 = 110.0;

/// Telegraph window — long enough to dodge with reasonable reaction time.
pub const ARTILLERY_TELEGRAPH_TIME: f32 = 1.5;

/// World-units radius of the splash AOE.
pub const ARTILLERY_SPLASH_RADIUS: f32 = 9.6;

/// Landing reticle drawn during the telegraph window. Visual only —
/// `ArtilleryShell::reticle` holds the back-ref so the shell despawns
/// it on impact.
#[derive(Component)]
pub struct ArtilleryReticle {
    pub remaining: f32,
    pub initial: f32,
}

/// In-flight artillery shell. Splash damage applies to friendly +
/// allies on landing.
#[derive(Component)]
pub struct ArtilleryShell {
    pub target: Vec2,
    pub time_of_flight: f32,
    pub elapsed: f32,
    pub damage: i32,
    pub splash_radius: f32,
    pub reticle: Option<Entity>,
}

/// Lock target, spawn the reticle, launch the shell on every cooldown
/// tick. Uses a predicted friendly position so cruising-through gets
/// caught while a turn or slow-down dodges.
pub fn artillery_fire(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    mut meshes: ResMut<Assets<Mesh>>,
    friendly: Query<(&Transform, &Velocity), (With<Friendly>, Without<Enemy>, Without<Ally>)>,
    ally_cache: Res<crate::ally::AllyPositionsCache>,
    mut artilleries: Query<(&Transform, &mut Enemy), Without<crate::harpoon::Harpooned>>,
) {
    let Some(pm) = pm else { return; };
    let dt = time.delta_secs();
    let Ok((ftf, fvel)) = friendly.single() else { return; };
    let fpos = ftf.translation.truncate();
    const ARTILLERY_LEAD_TIME: f32 = 0.9;
    let fpred = fpos + fvel.0 * ARTILLERY_LEAD_TIME;
    let ally_positions = &ally_cache.positions;

    // Reticle meshes — shared across every shell fired this frame. The
    // inner translucent disc gets a harsh annulus outline ring on top so
    // the splash radius reads clearly even over busy backgrounds.
    let mut inner_mesh: Option<Handle<Mesh>> = None;
    let mut outline_mesh: Option<Handle<Mesh>> = None;
    // Outline thickness as a fraction of the splash radius — thick enough
    // to read at small scales during the early telegraph but not so thick
    // it obscures the inner danger zone.
    const ARTILLERY_RETICLE_OUTLINE_THICKNESS: f32 = 1.4;

    for (tf, mut enemy) in &mut artilleries {
        if enemy.variant != EnemyVariant::Artillery { continue; }
        enemy.fire_cd -= dt;
        if enemy.fire_cd > 0.0 { continue; }
        let pos = tf.translation.truncate();
        if !crate::balance::in_play_area(pos) { continue; }
        let target = {
            let mut best = fpred;
            let mut best_d2 = pos.distance_squared(fpred);
            for &ap in ally_positions {
                let d2 = pos.distance_squared(ap);
                if d2 < best_d2 {
                    best = ap;
                    best_d2 = d2;
                }
            }
            best
        };
        let inner = inner_mesh
            .get_or_insert_with(|| meshes.add(Circle::new(ARTILLERY_SPLASH_RADIUS)))
            .clone();
        let outline = outline_mesh
            .get_or_insert_with(|| meshes.add(Annulus::new(
                ARTILLERY_SPLASH_RADIUS - ARTILLERY_RETICLE_OUTLINE_THICKNESS,
                ARTILLERY_SPLASH_RADIUS,
            )))
            .clone();
        let reticle = commands.spawn((
            Mesh2d(inner),
            MeshMaterial2d(pm.artillery_reticle.clone()),
            Transform::from_xyz(target.x, target.y, 0.4),
            ArtilleryReticle {
                remaining: ARTILLERY_TELEGRAPH_TIME,
                initial: ARTILLERY_TELEGRAPH_TIME,
            },
            RenderLayers::layer(PLAY_LAYER),
        )).with_children(|b| {
            b.spawn((
                Mesh2d(outline),
                MeshMaterial2d(pm.artillery_reticle_outline.clone()),
                // Slightly above the inner disc so the outline always
                // wins overlap with the translucent fill.
                Transform::from_xyz(0.0, 0.0, 0.01),
                RenderLayers::layer(PLAY_LAYER),
            ));
        }).id();
        commands.spawn((
            ArtilleryShell {
                target,
                time_of_flight: ARTILLERY_TELEGRAPH_TIME,
                elapsed: 0.0,
                damage: enemy.variant.fire_damage(),
                splash_radius: ARTILLERY_SPLASH_RADIUS,
                reticle: Some(reticle),
            },
        ));
        enemy.fire_cd = 1.0 / enemy.variant.fire_rate().max(0.1);
    }
}

/// Tick + impact for in-flight shells. The reticle GROWS as the shell
/// arcs in (FF-style "warning expanding") rather than shrinking, so the
/// danger reads as the splash zone filling toward the player.
pub fn artillery_shell_tick(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    difficulty: Res<crate::Difficulty>,
    mut shells: Query<(Entity, &mut ArtilleryShell)>,
    mut reticles: Query<(&mut ArtilleryReticle, &mut Transform), Without<ArtilleryShell>>,
    mut friendly: Query<
        (&Transform, &mut Health, &mut HitFx),
        (With<Friendly>, Without<Ally>, Without<ArtilleryShell>, Without<ArtilleryReticle>),
    >,
    mut allies: Query<
        (&Transform, &Ally, &mut Health, &mut HitFx),
        (With<Ally>, Without<Friendly>, Without<ArtilleryShell>, Without<ArtilleryReticle>),
    >,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();

    for (entity, mut shell) in &mut shells {
        shell.elapsed += dt;
        if let Some(r) = shell.reticle {
            if let Ok((mut ret, mut rtf)) = reticles.get_mut(r) {
                ret.remaining = (ret.initial - shell.elapsed).max(0.0);
                let progress = (shell.elapsed / shell.time_of_flight.max(0.0001)).clamp(0.0, 1.0);
                let frac = 0.10 + 0.90 * progress;
                rtf.scale = Vec3::new(frac, frac, 1.0);
            }
        }
        if shell.elapsed < shell.time_of_flight { continue; }

        // Impact — AOE damage to friendly + non-submerged allies.
        // Difficulty scales damage at application time so the same
        // multiplier hits every entity in the AOE.
        let center = shell.target;
        let r2 = shell.splash_radius * shell.splash_radius;
        let damage = difficulty.scale_damage(shell.damage);
        if let Ok((ftf, mut h, mut fx)) = friendly.single_mut() {
            if ftf.translation.truncate().distance_squared(center) < r2 {
                fx.pulse();
                h.0 = (h.0 - damage).max(0);
            }
        }
        for (atf, ally, mut h, mut fx) in &mut allies {
            if ally_is_submerged(ally) { continue; }
            if atf.translation.truncate().distance_squared(center) >= r2 { continue; }
            fx.pulse();
            h.0 = (h.0 - damage).max(0);
        }
        spawn_hit_particles(&mut commands, &em, &pm.enemy,             center, 18, 100.0, &mut rng);
        spawn_hit_particles(&mut commands, &em, &pm.artillery_reticle, center, 10, 130.0, &mut rng);
        if let Some(r) = shell.reticle {
            commands.entity(r).despawn();
        }
        commands.entity(entity).despawn();
    }
}
