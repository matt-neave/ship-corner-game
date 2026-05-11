//! Railgun beam: a long thin rectangle that doesn't travel — damage is
//! resolved once on spawn against every enemy on the line, then the entity
//! lingers for `BEAM_LIFETIME` seconds while its width pulses + fades.

use bevy::prelude::*;

use crate::balance::{BEAM_HIT_RADIUS, BEAM_MAX_WIDTH};
use crate::components::Health;
use crate::effects::{spawn_hit_particles, EffectMeshes};
use crate::enemy::Enemy;
use crate::palette::PaletteMaterials;
use crate::weapon::WeaponType;

/// Beam visual. Width is animated via `Transform.scale.x` — it grows fast
/// then fades out over `max_life`. Damage is resolved once at spawn time
/// (see `BeamHit` + `beam_apply_damage`); the entity then lingers for show.
#[derive(Component)]
pub struct Beam {
    pub life: f32,
    pub max_life: f32,
}

/// Line-segment data attached to a beam at spawn so `beam_apply_damage` can
/// resolve hits without needing access to the firing turret.
#[derive(Component)]
pub struct BeamHit {
    pub origin: Vec2,
    pub dir: Vec2,
    pub length: f32,
    pub damage: i32,
    pub slot: u8,
    pub weapon: WeaponType,
    /// Slot rune sockets snapshotted at fire time. The beam pushes a
    /// `DamageEvent` per pierced enemy carrying these runes so Fire/
    /// Frost/Shock/etc. proc on beam hits the same way they do on a
    /// regular bullet hit.
    pub runes: [Option<crate::rune::Rune>; 3],
}

/// Marker present until the beam's damage has been resolved exactly once.
/// `beam_apply_damage` removes it after processing so we never double-hit.
#[derive(Component)]
pub struct BeamPending;

/// Tick beam lifetime + animate width: a fast grow phase then a long fade.
/// Despawn when life reaches zero.
pub fn update_beams(
    time: Res<Time>,
    mut commands: Commands,
    mut q: Query<(Entity, &mut Beam, &mut Transform)>,
) {
    let dt = time.delta_secs();
    for (e, mut beam, mut tf) in &mut q {
        beam.life -= dt;
        if beam.life <= 0.0 {
            commands.entity(e).despawn();
            continue;
        }
        // progress = 0 (just spawned) → 1 (about to despawn).
        let progress = 1.0 - beam.life / beam.max_life;
        // Grow fast in the first 15% of life, then linearly fade out.
        let w_factor = if progress < 0.15 {
            progress / 0.15
        } else {
            1.0 - (progress - 0.15) / 0.85
        };
        tf.scale.x = BEAM_MAX_WIDTH * w_factor.max(0.0);
    }
}

/// Resolve a railgun beam's damage exactly once after it spawns. Iterates
/// every enemy and tests perpendicular distance from the beam's line segment;
/// every enemy within takes `BeamHit.damage`. Mirrors the hit / destruction
/// effects from `bullet_collisions` so beam kills feel the same.
pub fn beam_apply_damage(
    mut commands: Commands,
    player_stats: Res<crate::stats::PlayerStats>,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    mut queue: ResMut<crate::bullet::PendingDamageQueue>,
    beams: Query<(Entity, &BeamHit), With<BeamPending>>,
    enemies: Query<(Entity, &Transform, &Enemy, &Health), With<Enemy>>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let mut rng = rand::thread_rng();
    for (beam_e, hit) in &beams {
        for (ee, etf, enemy, h) in &enemies {
            let ep = etf.translation.truncate();
            let to = ep - hit.origin;
            let proj = to.dot(hit.dir);
            if proj < 0.0 || proj > hit.length { continue; }
            let perp_v = to - hit.dir * proj;
            if perp_v.length() > BEAM_HIT_RADIUS * enemy.variant.scale() { continue; }
            if h.0 <= 0 { continue; }

            // Crit per enemy hit — beam pierces, so each is its own
            // damage instance per the "crit applies to all ship
            // damage" rule. Push a DamageEvent so runes proc
            // uniformly with bullet hits.
            let crit_mult = player_stats.roll_crit_mult(&mut rng) as i32;
            let damage = hit.damage.saturating_mul(crit_mult);
            queue.push_initial(
                ee, damage, ep, hit.weapon,
                Some(crate::bullet::DamageSource::PlayerSlot(hit.slot)),
                &hit.runes,
            );

            // Per-hit spark; lethal hits get a denser burst applied
            // by `process_damage_events` via the same particle
            // pipeline used for bullet hits. We still spawn a small
            // hit spark here as the beam's "I pierced you" cue.
            let spark_mat = pm.bullet_inner_for(hit.weapon);
            spawn_hit_particles(&mut commands, &em, spark_mat, ep, 3, 35.0, &mut rng);
        }
        // Done with this beam — drop the marker so we never re-process it.
        commands.entity(beam_e).remove::<BeamPending>();
    }
}
