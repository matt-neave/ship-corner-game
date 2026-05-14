//! Support `CrowsNest` weapon — passive lookout that boosts the range
//! of every adjacent equipped turret. Doesn't fire. Mirrors the
//! `booster.rs` adjacency pattern; queried by `turret::sync_turret_config`
//! when it writes each slot's effective range multiplier.
//!
//! Stacking: per-neighbour bonus is `+15% × barrels` (so a T3 Crow's
//! Nest gives +45% range). Two adjacent Crow's Nests apply
//! multiplicatively — same shape as the Booster's fire-rate stack.
//! The deck visual (mast + lookout platform) lives in
//! `turret/decor.rs` alongside the other no-base-fire weapon decor.

use crate::balance::TURRET_ADJACENCY;
use crate::turret::TurretConfig;
use crate::weapon::WeaponType;

/// Per-tier range bonus contributed by ONE adjacent CrowsNest. With
/// the convention `tier = barrels`, a T1 Nest grants +15%, T2 grants
/// +30%, T3 grants +45% range to each touching slot.
pub const CROWS_NEST_RANGE_PER_TIER: f32 = 0.15;

/// Range multiplier applied to slot `slot_idx` from every adjacent
/// equipped CrowsNest. Returns 1.0 when no Nest is touching this
/// slot. Stacks multiplicatively across multiple adjacent Nests.
pub fn range_multiplier_for_slot(cfg: &TurretConfig, slot_idx: usize) -> f32 {
    let Some(neighbours) = TURRET_ADJACENCY.get(slot_idx) else { return 1.0; };
    let mut mult = 1.0_f32;
    for &n in neighbours.iter() {
        let Some(s) = cfg.slots.get(n) else { continue; };
        if s.equipped && matches!(s.weapon, WeaponType::CrowsNest) {
            let tier = s.barrels.max(1) as f32;
            mult *= 1.0 + CROWS_NEST_RANGE_PER_TIER * tier;
        }
    }
    mult
}
