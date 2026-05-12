//! Viking longship — pure ram attacker. No projectiles; damage comes
//! from contact with opposite-faction units at high charge speed.
//! Mirrors the player's `friendly_ram_damage` pattern: per-victim
//! grace prevents per-frame multi-tap, and a screen-shake kick punches
//! the impact.

use bevy::prelude::*;

use crate::components::{Faction, FactionKind, Health};
use crate::effects::HitFx;

use super::{Ally, ShipClass};

/// Per-Viking ram-charge state. `current_speed` ramps from
/// `VIKING_RAM_BASE_SPEED` up to `VIKING_RAM_MAX_SPEED` over
/// `VIKING_RAM_RAMP_TIME` while a target is held, and bleeds back
/// when the target is lost or mid-turn.
#[derive(Component)]
pub struct VikingRamCharge {
    pub current_speed: f32,
}

impl Default for VikingRamCharge {
    fn default() -> Self {
        Self { current_speed: VIKING_RAM_BASE_SPEED }
    }
}

/// Per-victim cooldown so a Viking pressed against an enemy doesn't
/// chunk it every frame.
#[derive(Component)]
pub struct VikingRamGrace {
    pub remaining: f32,
}

/// Base damage on a low-speed contact ram. Scaled up by current charge
/// speed (capped at `VIKING_RAM_DAMAGE_CAP`).
const VIKING_RAM_DAMAGE: i32 = 35;
const VIKING_RAM_DAMAGE_CAP: i32 = 50;
const VIKING_RAM_GRACE: f32 = 0.55;
const VIKING_RAM_TRAUMA: f32 = 0.5;

/// Starts slow so the player has a reaction window, then outpaces every
/// other ship at full charge.
pub const VIKING_RAM_BASE_SPEED: f32 = 18.0;
/// 2.5× the player's 30 u/s baseline — fastest thing on the field
/// without overshooting the play boundary every commit.
pub const VIKING_RAM_MAX_SPEED: f32  = 75.0;
pub const VIKING_RAM_RAMP_TIME: f32  = 1.0;
/// Turn rate at full charge — generous enough to re-acquire after a
/// miss, slow enough that the Viking has to circle back wide.
pub const VIKING_RAM_TURN_AT_MAX: f32 = 0.5;
/// Speed bleed (u/s) when not actively charging. Continuous bleed keeps
/// the bull-charge mechanic readable — speed only builds when aligned.
pub const VIKING_RAM_DECAY_PER_SEC: f32 = 60.0;
/// Heading-vs-target tolerance for the bull-charge gate. ~17° window.
pub const VIKING_RAM_ALIGN_THRESHOLD: f32 = 0.30;

/// Apply ram damage on contact. Faction-agnostic — friendly Vikings
/// ram enemies, boss-side Vikings ram allies + the player.
pub fn viking_ram_damage(
    time: Res<Time>,
    mut commands: Commands,
    mut shake: ResMut<crate::modes::ScreenShake>,
    mut vikings: Query<(Entity, &Transform, &Faction, &Ally, &mut VikingRamCharge)>,
    mut victims: Query<(
        Entity,
        &Transform,
        &Faction,
        &mut Health,
        &mut HitFx,
        Option<&mut VikingRamGrace>,
    )>,
    mut stats: ResMut<crate::ui::DamageStats>,
) {
    let dt = time.delta_secs();
    let viking_snap: Vec<(Entity, Vec2, FactionKind, f32)> = vikings
        .iter()
        .map(|(e, tf, fac, _, charge)| {
            (e, tf.translation.truncate(), fac.0.opposite(), charge.current_speed)
        })
        .collect();
    if viking_snap.is_empty() { return; }

    let mut hit_this_frame: Vec<Entity> = Vec::new();

    for (e, vtf, vfac, mut h, mut fx, grace) in &mut victims {
        if let Some(mut g) = grace {
            g.remaining -= dt;
            if g.remaining > 0.0 { continue; }
            commands.entity(e).remove::<VikingRamGrace>();
        }
        if h.0 <= 0 { continue; }
        let vp = vtf.translation.truncate();
        for &(viking_e, viking_pos, target_faction, charge_speed) in &viking_snap {
            if vfac.0 != target_faction { continue; }
            // Hit radius = longship + typical victim plus a bit of slop.
            let r = 8.0;
            if vp.distance_squared(viking_pos) < r * r {
                let t = ((charge_speed - VIKING_RAM_BASE_SPEED)
                    / (VIKING_RAM_MAX_SPEED - VIKING_RAM_BASE_SPEED))
                    .clamp(0.0, 1.0);
                let bonus = (VIKING_RAM_DAMAGE_CAP - VIKING_RAM_DAMAGE) as f32 * t;
                let dmg = (VIKING_RAM_DAMAGE + bonus.round() as i32)
                    .min(VIKING_RAM_DAMAGE_CAP);
                let dealt = crate::bullet::apply_damage(&mut h, &mut fx, dmg);
                crate::bullet::credit_damage(
                    &mut stats,
                    Some(crate::bullet::DamageSource::Ally(ShipClass::Viking)),
                    dealt,
                );
                shake.add_trauma(VIKING_RAM_TRAUMA);
                commands.entity(e).insert(VikingRamGrace { remaining: VIKING_RAM_GRACE });
                hit_this_frame.push(viking_e);
                break;
            }
        }
    }

    // Partial bleed on impact instead of a hard reset — a Viking locked
    // onto a victim stays scary across consecutive rams. The full reset
    // to base happens when the Viking loses its target (in `ally_ai`).
    if !hit_this_frame.is_empty() {
        for (e, _, _, _, mut charge) in &mut vikings {
            if hit_this_frame.contains(&e) {
                charge.current_speed = (charge.current_speed * 0.75)
                    .max(VIKING_RAM_BASE_SPEED);
            }
        }
    }
}
