//! Per-enemy traits — rolled at spawn, become more frequent as
//! the run-progression coefficient (`battles_cleared`) climbs.
//!
//! Authoring shape — one variant per trait. Adding a new trait
//! is one struct-arm change here plus a `match` arm in every
//! method below (compiler enforces exhaustiveness, so nothing
//! silently goes un-applied):
//!
//!   1. Add the enum variant.
//!   2. Fill in `speed_mult` / `trail_color` / `label` / wire
//!      bytes for the new variant.
//!   3. Adjust [`EnemyTrait::roll`] if it should compete in the
//!      same pool as Frenzy (or add a separate roll path).
//!
//! The trait field on the [`Enemy`](super::Enemy) component is
//! the single source of truth: AI reads it for movement
//! multipliers, the spawn site reads it to pick the right trail
//! material, and the multiplayer snapshot encodes it for peer
//! replication.

use bevy::prelude::*;
use rand::Rng;

/// One trait per equipped enemy. `None` (no trait) is the default
/// and stays the common case — `roll` returns `None` most of the
/// time at low difficulty, then ramps as the run progresses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EnemyTrait {
    /// Move 1.5× faster, with a hot-orange trail. The increased
    /// speed compounds with the variant's base — a Frenzy Swarmer
    /// hits the player nearly twice as fast as a vanilla one, so
    /// the trail tint exists to give the player a frame of
    /// warning before the angle of approach becomes a hit.
    Frenzy,
    /// +50% HP, -20% movement speed. Silver-grey trail. Reads as
    /// "tank, take time" — the trait the player wants to bait,
    /// not race.
    Armored,
    /// Spawns at baseline; gains +50% movement speed when current
    /// HP drops below 50% of `max_hp`. Crimson trail. Encourages
    /// committing to a kill rather than chipping it.
    Berserk,
    /// Aura: every other enemy within `PACK_LEADER_RADIUS` of this
    /// one gets a +20% movement speed bonus. Gold trail. Killing
    /// the leader is the read.
    PackLeader,
}

/// Spec-pixel radius of the Pack Leader's speed aura. Tuned to
/// roughly the typical clump size — big enough that a wave reads
/// as "pack" not "two strays" but small enough that a leader on
/// the far edge doesn't quietly buff the entire arena.
pub const PACK_LEADER_RADIUS: f32 = 40.0;
/// Multiplier applied to nearby enemies when in the leader's aura.
pub const PACK_LEADER_AURA_MULT: f32 = 1.20;
/// HP threshold (fraction of max) at which Berserk activates.
pub const BERSERK_HP_THRESHOLD: f32 = 0.50;
/// Speed bonus applied while Berserk is active.
pub const BERSERK_SPEED_MULT: f32 = 1.50;

impl EnemyTrait {
    /// Constant movement-speed multiplier applied from spawn —
    /// folded into both the initial `Velocity` write and the
    /// per-frame AI speed cap. Berserk's conditional bump is
    /// handled separately via [`Self::berserk_bonus_if_low`] so
    /// the threshold check lives off the live HP, not the spawn-
    /// time roll.
    pub fn speed_mult(self) -> f32 {
        match self {
            EnemyTrait::Frenzy => 1.5,
            EnemyTrait::Armored => 0.80,
            EnemyTrait::Berserk => 1.0,
            EnemyTrait::PackLeader => 1.0,
        }
    }

    /// Spawn-time HP multiplier. Applied AFTER the difficulty HP
    /// scale and BEFORE writing into the Enemy / Health / max_hp
    /// fields so the per-enemy HP bar reflects the buffed pool.
    pub fn hp_mult(self) -> f32 {
        match self {
            EnemyTrait::Frenzy => 1.0,
            EnemyTrait::Armored => 1.5,
            EnemyTrait::Berserk => 1.0,
            EnemyTrait::PackLeader => 1.0,
        }
    }

    /// Live speed bonus for Berserk — returns the extra multiplier
    /// when current HP is below the threshold, else `1.0`. Other
    /// traits return `1.0` so callers can chain unconditionally.
    pub fn berserk_bonus_if_low(self, hp: i32, max_hp: i32) -> f32 {
        if !matches!(self, EnemyTrait::Berserk) {
            return 1.0;
        }
        if max_hp <= 0 { return 1.0; }
        let frac = hp as f32 / max_hp as f32;
        if frac < BERSERK_HP_THRESHOLD {
            BERSERK_SPEED_MULT
        } else {
            1.0
        }
    }

    /// Tint applied to the enemy's wake trail. Trail recolour is
    /// the player's at-a-glance handle on which trait they're
    /// fighting, so each entry picks a saturated colour clearly
    /// distinct from the others + the default white wake.
    pub fn trail_color(self) -> Color {
        match self {
            EnemyTrait::Frenzy => Color::srgb(1.0, 0.45, 0.20),
            EnemyTrait::Armored => Color::srgb(0.72, 0.78, 0.85),
            EnemyTrait::Berserk => Color::srgb(0.85, 0.10, 0.18),
            EnemyTrait::PackLeader => Color::srgb(1.00, 0.82, 0.30),
        }
    }

    /// Display name. Currently only surfaced in debug / future
    /// tooltip uses — kept here so the enum stays the canonical
    /// place to extend (compiler error if a new variant skips it).
    #[allow(dead_code)]
    pub fn label(self) -> &'static str {
        match self {
            EnemyTrait::Frenzy => "FRENZY",
            EnemyTrait::Armored => "ARMORED",
            EnemyTrait::Berserk => "BERSERK",
            EnemyTrait::PackLeader => "PACK LEADER",
        }
    }

    /// Roll a trait at spawn time. Returns `None` most of the
    /// time at low difficulty; the overall trait chance scales
    /// linearly with `battles_cleared` (0 → 0%, +5% per cleared
    /// stage, capped at 60% by stage 12). On a hit, the variant
    /// is sampled uniformly from `ALL` — adjust the weights here
    /// if a specific trait should be rarer than the rest.
    pub fn roll(battles_cleared: u32, rng: &mut impl Rng) -> Option<EnemyTrait> {
        let chance = (0.05 * battles_cleared as f32).min(0.60);
        if rng.gen::<f32>() >= chance {
            return None;
        }
        // Uniform pick across the pool. Pack Leader is intentionally
        // included even though its aura is more impactful — the
        // 60% chance is the overall trait gate, so any single trait
        // is at most 60% / N_TRAITS per spawn.
        let pool = Self::ALL;
        if pool.is_empty() { return None; }
        let idx = rng.gen_range(0..pool.len());
        Some(pool[idx])
    }

    /// Full trait list. Used by `roll` for uniform sampling; also
    /// available for debug spawners that want to iterate every
    /// trait.
    pub const ALL: &'static [EnemyTrait] = &[
        EnemyTrait::Frenzy,
        EnemyTrait::Armored,
        EnemyTrait::Berserk,
        EnemyTrait::PackLeader,
    ];

    /// Stable wire-format discriminant for multiplayer snapshot
    /// sync. `0` is reserved for `None` so unset trait_kind round-
    /// trips through the bit field as 0. Append-only — never
    /// renumber existing variants.
    pub fn to_u8(self) -> u8 {
        match self {
            EnemyTrait::Frenzy => 1,
            EnemyTrait::Armored => 2,
            EnemyTrait::Berserk => 3,
            EnemyTrait::PackLeader => 4,
        }
    }

    /// Inverse of [`Self::to_u8`]. Unknown / `0` returns `None`;
    /// callers treat that as "no trait set."
    pub fn from_u8(n: u8) -> Option<EnemyTrait> {
        match n {
            1 => Some(EnemyTrait::Frenzy),
            2 => Some(EnemyTrait::Armored),
            3 => Some(EnemyTrait::Berserk),
            4 => Some(EnemyTrait::PackLeader),
            _ => None,
        }
    }
}
