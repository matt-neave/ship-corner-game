//! Tag-based set bonuses ("synergies") for the player's turret loadout.
//!
//! Each `WeaponTag` (Naval, Future, Autonomous, Pirate, Support, Melee)
//! has a 4-tier ladder triggered by having 2 / 4 / 6 / 8 equipped
//! turrets sharing that tag. The active tier per tag lives in the
//! `Synergies` resource, recomputed by `compute_synergies` whenever
//! `TurretConfig` changes.
//!
//! Tier ladders:
//!
//! | Tag | T1 (2) | T2 (4) | T3 (6) | T4 (8) |
//! |---|---|---|---|---|
//! | Naval | +10% global dmg | +20% | +30% | +40% |
//! | Future | 0.1s stun on hit | 0.2s | 0.3s | 0.4s |
//! | Autonomous | +10% rate + +10% speed | +20% / +20% | +30% / +30% | +40% / +40% |
//! | Pirate | +50% scrap drops | +100% | +150% | +200% |
//! | Support | non-Support +10% rate | +20% rate, +10% dmg | +30% / +20% | +40% / +25% |
//! | Melee | +1 HP heal per Melee kill | +2 | +3 | +4 |
//!
//! Consumers:
//! - `turret::sync_turret_config` reads `damage_mult_for` / `fire_rate_mult_for`
//!   each frame and bakes them into the live `TurretSlot` stats.
//! - `enemy::enemy_death_check` reads `pirate_harvest_mult()` to scale
//!   scrap drops.
//! - `blade::blade_tick` reads `melee_heal_per_kill()` to heal the
//!   player on Blade kills.

use bevy::prelude::*;

use crate::turret::TurretConfig;
use crate::weapon::WeaponTag;

/// Per-tag active synergy tier. 0 = none, 1..=4 = T1..=T4.
/// Recomputed each frame `TurretConfig` changes by `compute_synergies`.
#[derive(Resource, Default, Clone, Copy, Debug)]
pub struct Synergies {
    pub naval: u8,
    pub future: u8,
    pub autonomous: u8,
    pub pirate: u8,
    pub support: u8,
    pub melee: u8,
}

/// Convert an equipped-count for a tag into the active synergy tier.
/// 2/4/6/8 → 1/2/3/4. Below 2 → 0 (no synergy active).
fn tier_for(count: u8) -> u8 {
    if count >= 8 { 4 }
    else if count >= 6 { 3 }
    else if count >= 4 { 2 }
    else if count >= 2 { 1 }
    else { 0 }
}

impl Synergies {
    /// Global damage multiplier from Naval. Naval gives +10% damage to
    /// EVERY equipped turret (not just Naval-tagged ones) per tier.
    pub fn naval_damage_mult(&self) -> f32 {
        1.0 + 0.10 * self.naval as f32
    }

    /// Seconds of stun applied to enemies hit by a Future-tagged
    /// weapon. 0.1s per tier — read by `process_damage_events`,
    /// applied as a `Stunned` component on the target.
    pub fn future_stun_duration(&self) -> f32 {
        0.10 * self.future as f32
    }

    /// Autonomous fire-rate multiplier — applied to Autonomous-tagged
    /// turrets. +10% per tier (paired with the speed mult below).
    pub fn autonomous_fire_rate_mult(&self) -> f32 {
        1.0 + 0.10 * self.autonomous as f32
    }

    /// Autonomous unit movement multiplier — applied to helicopter
    /// orbit speed and octopus swim speed. +10% per tier. Lets the
    /// drones keep up with the higher fire cadence the synergy
    /// pushes them toward.
    pub fn autonomous_speed_mult(&self) -> f32 {
        1.0 + 0.10 * self.autonomous as f32
    }

    /// Scrap-drop multiplier from Pirate, applied in
    /// `enemy_death_check`. T1=1.50 (+50%) up to T4=3.00 (+200%).
    pub fn pirate_harvest_mult(&self) -> f32 {
        1.0 + 0.50 * self.pirate as f32
    }

    /// Fire-rate multiplier from Support — applied to every
    /// non-Support turret. +10% per tier.
    pub fn support_fire_rate_mult(&self) -> f32 {
        1.0 + 0.10 * self.support as f32
    }

    /// Damage multiplier from Support — applied to non-Support
    /// turrets, but only kicks in at T2 and up. Stacks multiplicatively
    /// with `naval_damage_mult`.
    pub fn support_damage_mult(&self) -> f32 {
        match self.support {
            0 | 1 => 1.0,
            2 => 1.10,
            3 => 1.20,
            _ => 1.25,
        }
    }

    /// HP healed per Melee-tagged kill. Read by `blade_tick` (the only
    /// current Melee damage source); +1 per tier.
    pub fn melee_heal_per_kill(&self) -> i32 {
        self.melee as i32
    }

    /// Combined damage multiplier applied to a slot whose weapon
    /// carries `tag`. Bakes Naval (global) and Support (non-Support
    /// only) together; Support slots opt out of their own buff.
    pub fn damage_mult_for(&self, tags: &[WeaponTag]) -> f32 {
        let mut m = self.naval_damage_mult();
        // Support's broad buff opts out for any Support-tagged
        // weapon — including multi-tag weapons where Support is
        // just one of the tags, so Support never accidentally buffs
        // itself.
        if !tags.contains(&WeaponTag::Support) {
            m *= self.support_damage_mult();
        }
        m
    }

    /// Combined fire-rate multiplier applied to a slot whose weapon
    /// carries any of `tags`. Bakes Autonomous's tag-specific buff
    /// (applied if Autonomous is in the list) AND Support's broad
    /// buff (opts out if Support is in the list). Future no longer
    /// buffs fire rate — its synergy is the on-hit stun applied in
    /// `process_damage_events`.
    pub fn fire_rate_mult_for(&self, tags: &[WeaponTag]) -> f32 {
        let tag_specific = if tags.contains(&WeaponTag::Autonomous) {
            self.autonomous_fire_rate_mult()
        } else {
            1.0
        };
        let support_buff = if tags.contains(&WeaponTag::Support) {
            1.0
        } else {
            self.support_fire_rate_mult()
        };
        tag_specific * support_buff
    }
}

/// Recompute `Synergies` whenever `TurretConfig` mutates (shop drag,
/// equip cycle, etc.). Cheap — single pass over 8 slots.
pub fn compute_synergies(cfg: Res<TurretConfig>, mut syn: ResMut<Synergies>) {
    if !cfg.is_changed() { return; }
    let mut counts = [0u8; 6];
    for slot in cfg.slots.iter().filter(|s| s.equipped) {
        // Multi-tag weapons (e.g. Harpoon = Pirate + Melee) count
        // toward EVERY one of their tags. Each tag pool tracks its
        // own tier independently.
        for &tag in slot.weapon.tags() {
            let i = match tag {
                WeaponTag::Naval      => 0,
                WeaponTag::Future     => 1,
                WeaponTag::Autonomous => 2,
                WeaponTag::Pirate     => 3,
                WeaponTag::Support    => 4,
                WeaponTag::Melee      => 5,
            };
            counts[i] += 1;
        }
    }
    *syn = Synergies {
        naval:      tier_for(counts[0]),
        future:     tier_for(counts[1]),
        autonomous: tier_for(counts[2]),
        pirate:     tier_for(counts[3]),
        support:    tier_for(counts[4]),
        melee:      tier_for(counts[5]),
    };
}

/// Per-tag active tier read off the `Synergies` resource. Mirrors
/// the lookup in the tooltip; kept here so `discover_synergies`
/// doesn't need to reach into the tooltip module.
fn tier_for_tag(tag: WeaponTag, syn: &Synergies) -> u8 {
    match tag {
        WeaponTag::Naval      => syn.naval,
        WeaponTag::Future     => syn.future,
        WeaponTag::Autonomous => syn.autonomous,
        WeaponTag::Pirate     => syn.pirate,
        WeaponTag::Support    => syn.support,
        WeaponTag::Melee      => syn.melee,
    }
}

/// One-shot discovery hook. Runs after `compute_synergies`; when a
/// tag's tier crosses 0 → ≥1 for the first time this run, marks it
/// in `DiscoveredSynergies` and pops a "DISCOVERED!" banner in the
/// bottom-left notification stack. Idempotent — already-discovered
/// tags are skipped, so de-equipping below T1 and re-equipping
/// later doesn't re-fire the popup.
pub fn discover_synergies(
    mut commands: Commands,
    synergies: Res<Synergies>,
    mut discovered: ResMut<crate::onboarding::DiscoveredSynergies>,
    existing: Query<Entity, With<crate::onboarding::NotificationLifetime>>,
) {
    if !synergies.is_changed() { return; }
    // Seed from the world-visible banner count, then bump locally per
    // spawn — multiple synergies unlocked in one frame (e.g. dragging
    // a turret that crosses two tag thresholds) need to stack rather
    // than overlap. Querying the world per spawn won't work because
    // commands are buffered until the schedule flushes.
    let mut stack_index = existing.iter().count();
    for &tag in WeaponTag::all() {
        if tier_for_tag(tag, &synergies) >= 1 && !discovered.has(tag) {
            discovered.mark(tag);
            crate::onboarding::spawn_synergy_discovered_banner(
                &mut commands, stack_index, tag,
            );
            stack_index += 1;
        }
    }
}
