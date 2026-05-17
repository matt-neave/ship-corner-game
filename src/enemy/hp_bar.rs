//! On-damage floating HP bars for enemies. Spawned the first time a
//! variant takes damage, refreshed on each subsequent hit, despawned
//! after 3 s of no damage or when the target enemy is gone.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;

use crate::balance::HUD_LAYER;
use crate::components::Health;

use super::{Enemy, EnemyHpBar, PreviousHp};

const HP_BAR_SHOW_TIME: f32 = 3.0;
/// World-units offset above the enemy's center. Tuned for standard
/// enemies (~half-length 5–6); boss hulls overlap the lower edge
/// slightly, which reads better than a bar floating in empty water.
const HP_BAR_Y_OFFSET:  f32 = 7.0;
const HP_BAR_W: f32 = 8.0;
const HP_BAR_H: f32 = 1.0;

/// Cached mesh + material — built once so spawning a bar is just a
/// transform + component insert, no asset alloc.
#[derive(Resource)]
pub struct EnemyHpBarAssets {
    pub mesh: Handle<Mesh>,
    pub fill: Handle<ColorMaterial>,
}

pub fn setup_enemy_hp_bar_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
) {
    let mesh = meshes.add(Rectangle::new(HP_BAR_W, HP_BAR_H));
    let fill = materials.add(Color::srgb(0.92, 0.18, 0.22));
    commands.insert_resource(EnemyHpBarAssets { mesh, fill });
}

/// Detect HP drops and spawn / refresh the floating bar. Runs in the
/// damage-application chain so it sees the new HP for the same frame
/// the hit landed.
///
/// Newly-spawned bars are positioned and scaled to their final values
/// *at spawn time* — not deferred to `update_enemy_hp_bars` on the
/// next frame. Deferring caused a one-shot-kill glitch: the enemy
/// despawned the same frame the bar was born, so the bar's first
/// (and only) render used the placeholder transform — a full-scale
/// red rectangle at world origin.
pub fn track_enemy_damage_for_hp_bars(
    mut commands: Commands,
    assets: Option<Res<EnemyHpBarAssets>>,
    mut enemies: Query<(Entity, &Transform, &Health, &Enemy, &mut PreviousHp), With<Enemy>>,
    mut bars: Query<&mut EnemyHpBar>,
) {
    let Some(assets) = assets else { return; };
    for (e, e_tf, h, enemy, mut prev) in &mut enemies {
        if h.0 < prev.0 {
            let mut found = false;
            for mut bar in &mut bars {
                if bar.enemy == e {
                    bar.remaining = HP_BAR_SHOW_TIME;
                    found = true;
                    break;
                }
            }
            if !found {
                let max = enemy.max_hp.max(1) as f32;
                let ratio = (h.0 as f32 / max).clamp(0.0, 1.0);
                let world = e_tf.translation.truncate();
                let mut bar_tf = Transform::from_xyz(
                    world.x + HP_BAR_W * (ratio - 1.0) * 0.5,
                    world.y + HP_BAR_Y_OFFSET,
                    5.5,
                );
                bar_tf.scale = Vec3::new(ratio, 1.0, 1.0);
                commands.spawn((
                    Mesh2d(assets.mesh.clone()),
                    MeshMaterial2d(assets.fill.clone()),
                    // HUD_LAYER so the HudCamera renders this bar at
                    // native resolution — the chunky-pixel filter
                    // doesn't apply, keeping it crisp.
                    bar_tf,
                    EnemyHpBar { enemy: e, remaining: HP_BAR_SHOW_TIME },
                    RenderLayers::layer(HUD_LAYER),
                ));
            }
        }
        prev.0 = h.0;
    }
}

/// Per-frame: tick the fade timer, snap position to the enemy, write
/// the fill scale + offset so the bar shrinks left-anchored as HP drops.
/// Hidden while paused / customizing so the bars don't poke through
/// the pause backdrop or shop card.
pub fn update_enemy_hp_bars(
    time: Res<Time>,
    mut commands: Commands,
    paused: Res<crate::pause::Paused>,
    customize_open: Res<crate::customize::CustomizeOpen>,
    enemies: Query<(&Transform, &Health, &Enemy), Without<EnemyHpBar>>,
    mut bars: Query<(Entity, &mut EnemyHpBar, &mut Transform, &mut Visibility)>,
) {
    let dt = time.delta_secs();
    let hide_all = paused.0 || customize_open.open;
    for (bar_e, mut bar, mut tf, mut vis) in &mut bars {
        let want_vis = if hide_all { Visibility::Hidden } else { Visibility::Inherited };
        if *vis != want_vis { *vis = want_vis; }
        bar.remaining -= dt;
        if bar.remaining <= 0.0 {
            commands.entity(bar_e).despawn();
            continue;
        }
        let Ok((e_tf, h, enemy)) = enemies.get(bar.enemy) else {
            commands.entity(bar_e).despawn();
            continue;
        };
        let max = enemy.max_hp.max(1) as f32;
        let ratio = (h.0 as f32 / max).clamp(0.0, 1.0);
        // Centered rectangles scale around their midpoint; shift the
        // center by half the empty width to keep the left edge fixed.
        let world = e_tf.translation.truncate();
        tf.translation.x = world.x + HP_BAR_W * (ratio - 1.0) * 0.5;
        tf.translation.y = world.y + HP_BAR_Y_OFFSET;
        tf.scale.x = ratio;
        tf.scale.y = 1.0;
    }
}
