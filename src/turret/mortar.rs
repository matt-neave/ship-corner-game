//! Mortar firing pipeline: spawn an arcing shell that explodes for
//! splash damage on landing. Mirrors the enemy `ArtilleryShell` but
//! routes damage through the player's rune + crit pipeline.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;

use crate::balance::PLAY_LAYER;
use crate::bullet::DamageSource;
use crate::components::Health;
use crate::effects::{spawn_hit_particles, EffectMeshes};
use crate::enemy::Enemy;
use crate::modes::ScreenShake;
use crate::palette::PaletteMaterials;
use crate::rune::Rune;
use crate::weapon::WeaponType;

/// Total air time from muzzle to landing — independent of distance so
/// the arc reads as a consistent "lobbed" feel for short + long shots.
pub const MORTAR_TIME_OF_FLIGHT: f32 = 0.65;
/// Splash AoE radius. ~2× a small enemy hit radius — catches packs.
pub const MORTAR_SPLASH_RADIUS: f32 = 12.0;
/// Peak visual lift at apex (t = 0.5), expressed as added scale.
const MORTAR_APEX_SCALE: f32 = 0.6;
/// Peak vertical offset of the arc. The shell is lifted along world-+Y
/// by `sin(πt) × MORTAR_ARC_HEIGHT`, so the trajectory always bows
/// upward regardless of flight direction (consistent in top-down).
const MORTAR_ARC_HEIGHT: f32 = 12.0;

/// In-flight mortar shell. No `Bullet` component — can't be hit en
/// route. On `elapsed >= time_of_flight` it explodes and despawns.
///
/// `target` is snapshotted at fire time; the shell can't course-correct,
/// which is intentional (a miss is a shell that committed too early).
#[derive(Component)]
pub struct MortarShell {
    pub target: Vec2,
    pub origin: Vec2,
    pub time_of_flight: f32,
    pub elapsed: f32,
    pub damage: i32,
    pub splash_radius: f32,
    pub source: Option<DamageSource>,
    pub weapon: WeaponType,
    pub runes: [Option<Rune>; 3],
    pub shadow: Option<Entity>,
}

/// Spawn the shell aimed at `target`. No landing-shadow visual — the
/// arc + elongated shell silhouette is enough.
pub fn spawn_mortar_shell(
    commands: &mut Commands,
    em: &EffectMeshes,
    outer_mat: &Handle<ColorMaterial>,
    inner_mat: &Handle<ColorMaterial>,
    pos: Vec2,
    target: Vec2,
    weapon: WeaponType,
    damage: i32,
    splash_radius: f32,
    source: Option<DamageSource>,
    runes: [Option<Rune>; 3],
) {
    // Orient the oblong bullet mesh along the flight direction so the
    // shell visually points at its landing spot for the whole arc.
    let flight = target - pos;
    let heading = if flight.length_squared() > 0.0001 {
        (-flight.x).atan2(flight.y)
    } else {
        0.0
    };
    let shell = commands.spawn((
        Mesh2d(em.bullet_friendly_outer.clone()),
        MeshMaterial2d(outer_mat.clone()),
        Transform::from_xyz(pos.x, pos.y, 4.5)
            .with_rotation(Quat::from_rotation_z(heading)),
        MortarShell {
            target,
            origin: pos,
            time_of_flight: MORTAR_TIME_OF_FLIGHT,
            elapsed: 0.0,
            damage,
            splash_radius,
            source,
            weapon,
            runes,
            shadow: None,
        },
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    let inner = commands.spawn((
        Mesh2d(em.bullet_friendly_inner.clone()),
        MeshMaterial2d(inner_mat.clone()),
        Transform::from_xyz(0.0, 0.0, 0.05),
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    commands.entity(inner).insert(ChildOf(shell));
}

/// Tick the arc and resolve impact. One crit roll per shell, applied
/// to every enemy in the splash — a crit detonation is one collective
/// big-number beat, not a per-enemy lottery.
pub fn mortar_shell_tick(
    time: Res<Time>,
    mut commands: Commands,
    player_stats: Res<crate::stats::PlayerStats>,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    mut shake: ResMut<ScreenShake>,
    mut queue: ResMut<crate::bullet::PendingDamageQueue>,
    mut shells: Query<(Entity, &mut Transform, &mut MortarShell)>,
    enemies: Query<(Entity, &Transform, &Enemy, &Health), (With<Enemy>, Without<MortarShell>)>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();

    for (shell_e, mut shell_tf, mut shell) in &mut shells {
        shell.elapsed += dt;
        let tof = shell.time_of_flight.max(0.0001);

        if shell.elapsed < tof {
            // In flight — interpolate along origin → target, lift by
            // `sin(πt) × MORTAR_ARC_HEIGHT`, scale envelope on top for
            // a tiny "closer to camera" illusion at apex.
            let t = (shell.elapsed / tof).clamp(0.0, 1.0);
            let lift = (std::f32::consts::PI * t).sin();
            let ground = shell.origin.lerp(shell.target, t);
            let pos = ground + Vec2::new(0.0, lift * MORTAR_ARC_HEIGHT);
            let scale = 1.0 + MORTAR_APEX_SCALE * lift;
            shell_tf.translation.x = pos.x;
            shell_tf.translation.y = pos.y;
            shell_tf.scale = Vec3::new(scale, scale, 1.0);
            continue;
        }

        // Landed — one crit roll per shell, applied to every enemy.
        let crit_mult = if matches!(shell.source, Some(DamageSource::PlayerSlot(_))) {
            player_stats.roll_crit_mult(&mut rng) as i32
        } else {
            1
        };
        let amount = shell.damage.saturating_mul(crit_mult);
        let target = shell.target;

        for (e, etf, en, h) in &enemies {
            if h.0 <= 0 { continue; }
            let ep = etf.translation.truncate();
            let er = 3.5 * en.variant.scale();
            let reach = shell.splash_radius + er;
            if ep.distance_squared(target) > reach * reach { continue; }
            queue.push_initial(e, amount, ep, shell.weapon, shell.source, &shell.runes);
        }

        let inner_mat = pm.bullet_inner_for(shell.weapon);
        let outer_mat = pm.bullet_outer_for(shell.weapon);
        spawn_hit_particles(&mut commands, &em, inner_mat, target, 14, 110.0, &mut rng);
        spawn_hit_particles(&mut commands, &em, outer_mat, target, 10, 60.0, &mut rng);

        shake.add_trauma(0.25);

        if let Some(shadow) = shell.shadow {
            commands.entity(shadow).despawn();
        }
        commands.entity(shell_e).despawn();
    }
}
