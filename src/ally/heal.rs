//! Tender heal-beam: pick a hurt target of the emitter's `heal_faction`
//! in range, accumulate fractional HP, apply integer increments, and
//! paint a flowing-particle ribbon for visual feedback.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;
use rand::Rng;

use crate::balance::PLAY_LAYER;
use crate::components::{Faction, FactionKind, Friendly, Health, Velocity};
use crate::effects::{EffectMeshes, HitParticle};
use crate::enemy::Enemy;
use crate::modes::GameMode;
use crate::palette::PaletteMaterials;

use super::Ally;

/// Continuous healing-beam emitter. Each frame picks a hurt target of
/// `heal_faction` inside `range`, accumulates fractional HP, and spawns
/// brief particle visuals. Friendly tenders set `Friendly`; boss tenders
/// would set `Enemy`.
#[derive(Component)]
pub struct HealBeamEmitter {
    pub range: f32,
    pub hp_per_sec: f32,
    /// Fractional HP carried between frames so a sub-1-HP-per-frame
    /// rate still ticks integers up at the right cadence.
    pub accumulator: f32,
    pub heal_faction: FactionKind,
}

/// Spawn per-frame heal stream particles between tender and target.
/// Stream motes drift along the line; a sparse upward sparkle blooms
/// around the target to suggest "healing lifting from the unit".
fn spawn_heal_visual(
    commands: &mut Commands,
    em: &EffectMeshes,
    mat: &Handle<ColorMaterial>,
    tender: Vec2,
    target: Vec2,
    rng: &mut rand::rngs::ThreadRng,
) {
    let to = target - tender;
    let len = to.length();
    if len < 0.5 { return; }
    let dir = to / len;
    let perp = Vec2::new(-dir.y, dir.x);

    for _ in 0..2 {
        let t = rng.gen_range(0.0..1.0);
        let pos = tender + dir * (t * len) + perp * rng.gen_range(-0.7..0.7);
        let v = dir * rng.gen_range(22.0..42.0)
              + perp * rng.gen_range(-9.0..9.0);
        let life = rng.gen_range(0.18..0.32);
        let scale = rng.gen_range(0.6..1.0);
        let rot = (-v.x).atan2(v.y);
        commands.spawn((
            Mesh2d(em.particle.clone()),
            MeshMaterial2d(mat.clone()),
            Transform {
                translation: Vec3::new(pos.x, pos.y, 5.5),
                rotation: Quat::from_rotation_z(rot),
                scale: Vec3::new(scale, scale, 1.0),
            },
            HitParticle { life, max_life: life, base_scale: scale },
            Velocity(v),
            RenderLayers::layer(PLAY_LAYER),
        ));
    }

    // ~50% chance per frame so the bloom reads as organic / occasional
    // rather than a steady stream.
    if rng.gen_bool(0.5) {
        let off = perp * rng.gen_range(-1.6..1.6)
                + dir * rng.gen_range(-1.6..1.6);
        let pos = target + off;
        let v = Vec2::new(
            rng.gen_range(-3.0..3.0),
            rng.gen_range(11.0..22.0),
        );
        let life = rng.gen_range(0.30..0.55);
        let scale = rng.gen_range(0.5..0.9);
        let rot = (-v.x).atan2(v.y);
        commands.spawn((
            Mesh2d(em.particle.clone()),
            MeshMaterial2d(mat.clone()),
            Transform {
                translation: Vec3::new(pos.x, pos.y, 5.5),
                rotation: Quat::from_rotation_z(rot),
                scale: Vec3::new(scale, scale, 1.0),
            },
            HitParticle { life, max_life: life, base_scale: scale },
            Velocity(v),
            RenderLayers::layer(PLAY_LAYER),
        ));
    }
}

/// Targeting priority within `heal_faction`:
///   1. Player ship if hurt + in range (only matches when
///      `heal_faction == Friendly`).
///   2. Otherwise the most-hurt living ally in range, skipping self.
/// If neither exists the accumulator slowly decays so a fresh target
/// can't burst-heal stockpiled HP.
pub fn tender_heal_beam(
    time: Res<Time>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    game_mode: Res<GameMode>,
    mut tenders: Query<(Entity, &Transform, &mut HealBeamEmitter)>,
    mut friendly: Query<
        (Entity, &Transform, &Faction, &mut Health),
        (With<Friendly>, Without<Ally>, Without<HealBeamEmitter>),
    >,
    // Pure allies (Friendly side ally ships). `Without<Enemy>` keeps
    // boss-side ships out — those are handled by the enemy query below
    // because their max-HP authority is `Enemy.max_hp`, not `class.hp()`.
    mut ally_targets: Query<
        (Entity, &Transform, &Ally, &Faction, &mut Health),
        (Without<Friendly>, Without<Enemy>, Without<HealBeamEmitter>),
    >,
    // Anything with `Enemy` — regular enemy variants AND boss ships
    // (which carry both `Enemy` and `Ally`). Max HP comes from
    // `Enemy.max_hp`, set per-variant for regulars and per-class
    // (`boss_hp`) for bosses, so a single lookup works for both.
    mut enemy_targets: Query<
        (Entity, &Transform, &Enemy, &Faction, &mut Health),
        (Without<Friendly>, Without<HealBeamEmitter>),
    >,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();

    let player_max_hp = 100;
    let _ = game_mode;

    for (tender_e, tender_tf, mut emitter) in &mut tenders {
        let tender_pos = tender_tf.translation.truncate();
        let range_sq = emitter.range * emitter.range;
        let heal_faction = emitter.heal_faction;

        // (entity, pos, is_player, max_hp). max_hp is stored at pick
        // time so the heal-apply branch doesn't have to re-derive it
        // from the original source query.
        let mut chosen: Option<(Entity, Vec2, bool, i32)> = None;

        if let Ok((fe, ftf, ffac, fh)) = friendly.single() {
            if ffac.0 == heal_faction
                && fh.0 > 0
                && fh.0 < player_max_hp
            {
                let fp = ftf.translation.truncate();
                if fp.distance_squared(tender_pos) < range_sq {
                    chosen = Some((fe, fp, true, player_max_hp));
                }
            }
        }

        if chosen.is_none() {
            // (entity, pos, missing_hp, max_hp). Pick the unit with
            // the most missing HP across both ally and enemy pools.
            let mut best: Option<(Entity, Vec2, i32, i32)> = None;

            for (ae, atf, ally, afac, h) in &ally_targets {
                if ae == tender_e { continue; }
                if afac.0 != heal_faction { continue; }
                if h.0 <= 0 { continue; }
                let max = ally.class.hp();
                let missing = max - h.0;
                if missing <= 0 { continue; }
                let ap = atf.translation.truncate();
                if ap.distance_squared(tender_pos) >= range_sq { continue; }
                if best.map_or(true, |(_, _, m, _)| missing > m) {
                    best = Some((ae, ap, missing, max));
                }
            }

            for (ee, etf, enemy, efac, h) in &enemy_targets {
                if ee == tender_e { continue; }
                if efac.0 != heal_faction { continue; }
                if h.0 <= 0 { continue; }
                let max = enemy.max_hp;
                let missing = max - h.0;
                if missing <= 0 { continue; }
                let ep = etf.translation.truncate();
                if ep.distance_squared(tender_pos) >= range_sq { continue; }
                if best.map_or(true, |(_, _, m, _)| missing > m) {
                    best = Some((ee, ep, missing, max));
                }
            }

            if let Some((e, p, _, max)) = best {
                chosen = Some((e, p, false, max));
            }
        }

        let Some((target_e, target_pos, is_player, max)) = chosen else {
            emitter.accumulator =
                (emitter.accumulator - dt * emitter.hp_per_sec).max(0.0);
            continue;
        };

        emitter.accumulator += dt * emitter.hp_per_sec;
        let heal_int = emitter.accumulator.floor() as i32;
        if heal_int > 0 {
            emitter.accumulator -= heal_int as f32;
            if is_player {
                if let Ok((_, _, _, mut h)) = friendly.single_mut() {
                    h.0 = (h.0 + heal_int).min(max);
                }
            } else if let Ok((_, _, _, _, mut h)) = ally_targets.get_mut(target_e) {
                h.0 = (h.0 + heal_int).min(max);
            } else if let Ok((_, _, _, _, mut h)) = enemy_targets.get_mut(target_e) {
                h.0 = (h.0 + heal_int).min(max);
            }
        }

        spawn_heal_visual(&mut commands, &em, &pm.heal, tender_pos, target_pos, &mut rng);
    }
}
