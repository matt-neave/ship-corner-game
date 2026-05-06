//! Projectile component and the two systems that touch it: travel/expiry
//! tick, and collision routing (friendly bullets damage enemies; enemy
//! bullets damage the friendly only in Wave mode — Sandbox is invincible).

use bevy::prelude::*;

use crate::components::{FactionKind, Friendly, Health, Velocity};
use crate::effects::{spawn_hit_particles, EffectMeshes, HitFx};
use crate::enemy::Enemy;
use crate::palette::PaletteMaterials;
use crate::modes::GameMode;
use crate::ui::DamageStats;
use crate::weapon::WeaponType;
use crate::Score;

#[derive(Component)]
pub struct Bullet {
    pub faction: FactionKind,
    pub damage: i32,
    pub remaining: f32,
    /// Weapon that fired this bullet — drives spark/impact colors on hit.
    /// Unused for enemy bullets (always `Standard`).
    pub weapon: WeaponType,
    /// Originating turret slot index (0-7) for friendly bullets. None for
    /// enemy bullets — they don't contribute to the slot damage tally.
    pub slot: Option<u8>,
}

pub fn bullet_update(
    time: Res<Time>,
    mut commands: Commands,
    mut q: Query<(Entity, &mut Bullet, &Velocity)>,
) {
    let dt = time.delta_secs();
    for (e, mut b, v) in &mut q {
        b.remaining -= v.0.length() * dt;
        if b.remaining <= 0.0 {
            commands.entity(e).despawn();
        }
    }
}

pub fn bullet_collisions(
    mut commands: Commands,
    mut score: ResMut<Score>,
    mut stats: ResMut<DamageStats>,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    game_mode: Res<GameMode>,
    bullets: Query<(Entity, &Transform, &Bullet)>,
    mut enemies: Query<(Entity, &Transform, &Enemy, &mut Health, &mut HitFx), (With<Enemy>, Without<Friendly>)>,
    mut friendly: Query<(Entity, &Transform, &mut Health, &mut HitFx), (With<Friendly>, Without<Enemy>)>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let mut rng = rand::thread_rng();
    for (be, btf, b) in &bullets {
        let bp = btf.translation.truncate();
        match b.faction {
            FactionKind::Friendly => {
                for (ee, etf, enemy, mut h, mut fx) in &mut enemies {
                    let hit_d = 3.5 * enemy.variant.scale();
                    if etf.translation.truncate().distance(bp) < hit_d {
                        // Credit damage actually dealt (clamped to remaining HP
                        // so overkill doesn't inflate the share).
                        if let Some(s) = b.slot {
                            let dealt = b.damage.min(h.0).max(0) as u64;
                            stats.per_slot[s as usize] += dealt;
                            stats.total += dealt;
                        }
                        h.0 -= b.damage;
                        commands.entity(be).despawn();
                        let hit_pos = etf.translation.truncate();
                        let spark_mat = pm.bullet_inner_for(b.weapon);
                        if h.0 <= 0 {
                            commands.entity(ee).despawn();
                            score.0 += 10;
                            // Larger destruction burst — mix enemy + bullet colors.
                            spawn_hit_particles(&mut commands, &em, &pm.enemy, hit_pos, 10, 60.0, &mut rng);
                            spawn_hit_particles(&mut commands, &em, spark_mat, hit_pos, 6, 75.0, &mut rng);
                        } else {
                            fx.pulse();
                            spawn_hit_particles(&mut commands, &em, spark_mat, hit_pos, 4, 45.0, &mut rng);
                        }
                        break;
                    }
                }
            }
            FactionKind::Enemy => {
                for (_fe, ftf, mut h, mut fx) in &mut friendly {
                    if ftf.translation.truncate().distance(bp) < 5.0 {
                        // In Sandbox the ship is invincible — visual only.
                        // In Wave mode bullets actually subtract HP.
                        commands.entity(be).despawn();
                        fx.pulse();
                        if matches!(*game_mode, GameMode::Wave) {
                            h.0 = (h.0 - b.damage).max(0);
                        }
                        let hit_pos = ftf.translation.truncate();
                        spawn_hit_particles(&mut commands, &em, &pm.bullet_enemy, hit_pos, 5, 50.0, &mut rng);
                        break;
                    }
                }
            }
        }
    }
}
