//! Weapon archetypes for player turrets.
//!
//! Adding a new weapon is a five-spot change:
//! 1. Add a variant to `WeaponType`.
//! 2. Add rows in `defaults`, `label`, `spread`, `next` (cycle order).
//! 3. Add new material handles in `palette::PaletteMaterials` + `build`.
//! 4. Add match arms in the `*_for` impls below.
//! 5. Handle the new variant's firing path in the turret-fire system if it
//!    has special behaviour (e.g., shotgun pellet loop, railgun beam spawn).
//!
//! The per-weapon stats are kept here as `match` tables rather than a HashMap
//! so the compiler enforces exhaustiveness — if you forget a variant, it
//! won't build.

use bevy::prelude::*;

use crate::i18n::tr;
use crate::palette::PaletteMaterials;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum WeaponType {
    #[default]
    Standard,
    Sniper,
    MachineGun,
    Shotgun,
    Railgun,
    Mortar,
    /// Deck launchpad — does not fire bullets itself. While equipped, a
    /// persistent helicopter entity orbits the ship and shoots using this
    /// slot's stats (damage / fire_rate / barrels / runes). See
    /// `sync_helipad_helicopters` and `helicopter_ai` in `turret.rs`.
    HeliPad,
}

/// How a turret picks a target among in-arc, in-range candidates.
/// Default is `Closest` (kill the immediate threat); a "targeting"
/// rune slotted on the turret overrides — see `Rune::target_priority`
/// in `rune.rs`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TargetPriority {
    Closest,
    Furthest,
    HighestHp,
    LowestHp,
}

impl WeaponType {
    /// Forward-cycle through equipped types (for the EQUIP button). `None`
    /// represents wrapping back to "unequipped". Mortar sits at the tail
    /// of the cycle — it's the longest-range weapon and rounds out the
    /// roster after Railgun.
    pub fn next(self) -> Option<Self> {
        match self {
            WeaponType::Standard   => Some(WeaponType::Sniper),
            WeaponType::Sniper     => Some(WeaponType::MachineGun),
            WeaponType::MachineGun => Some(WeaponType::Shotgun),
            WeaponType::Shotgun    => Some(WeaponType::Railgun),
            WeaponType::Railgun    => Some(WeaponType::Mortar),
            WeaponType::Mortar     => Some(WeaponType::HeliPad),
            WeaponType::HeliPad    => None,
        }
    }

    /// Default `(damage, fire_rate)` snapped on when this weapon is selected.
    /// For Shotgun the damage is per pellet; for Railgun it's per enemy
    /// hit by the beam (which pierces).
    pub fn defaults(self) -> (i32, f32) {
        match self {
            WeaponType::Standard   => (1, 4.0),
            WeaponType::Sniper     => (10, 0.25),
            WeaponType::MachineGun => (1, 8.0),
            WeaponType::Shotgun    => (1, 1.5),
            WeaponType::Railgun    => (6, 0.5),
            // Mortar: lobbed-shell pacing — slower fire rate than
            // direct-fire weapons since each shot is an arced AoE.
            WeaponType::Mortar     => (4, 0.4),
            // HeliPad: the slot's `fire_rate` drives the orbiting
            // helicopter's MG cadence. Sustained-harasser numbers —
            // small-bore damage at a steady rhythm.
            WeaponType::HeliPad    => (2, 3.0),
        }
    }

    /// Display label — looked up in `data/translations.csv`.
    pub fn label(self) -> &'static str {
        match self {
            WeaponType::Standard   => tr("weapon_standard"),
            WeaponType::Sniper     => tr("weapon_sniper"),
            WeaponType::MachineGun => tr("weapon_mg"),
            WeaponType::Shotgun    => tr("weapon_shotgun"),
            WeaponType::Railgun    => tr("weapon_railgun"),
            WeaponType::Mortar     => tr("weapon_mortar"),
            WeaponType::HeliPad    => tr("weapon_helipad"),
        }
    }

    /// Long-form description for tooltips. Looked up in
    /// `data/translations.csv` so adding a language is one column.
    pub fn description(self) -> &'static str {
        match self {
            WeaponType::Standard   => tr("weapon_standard_desc"),
            WeaponType::Sniper     => tr("weapon_sniper_desc"),
            WeaponType::MachineGun => tr("weapon_mg_desc"),
            WeaponType::Shotgun    => tr("weapon_shotgun_desc"),
            WeaponType::Railgun    => tr("weapon_railgun_desc"),
            WeaponType::Mortar     => tr("weapon_mortar_desc"),
            WeaponType::HeliPad    => tr("weapon_helipad_desc"),
        }
    }

    /// Half-angle (rad) of random firing cone. 0 means perfectly accurate.
    pub fn spread(self) -> f32 {
        match self {
            WeaponType::MachineGun => 0.18, // ~±10°
            _ => 0.0,
        }
    }

    /// Per-weapon range multiplier. Multiplied with `PlayerStats.range_pct`
    /// and any pier buff when computing a turret's effective range. Lets
    /// the sniper read as "150% range" relative to a 100% baseline weapon.
    pub fn range_mult(self) -> f32 {
        match self {
            WeaponType::Standard   => 1.0,
            WeaponType::Sniper     => 1.5,
            WeaponType::MachineGun => 0.9,
            WeaponType::Shotgun    => 0.6,
            WeaponType::Railgun    => 1.6,
            WeaponType::Mortar     => 3.0,
            // HeliPad slot itself never shoots; its helicopter carries
            // its own range. 1.0 is a placeholder so the match is exhaustive.
            WeaponType::HeliPad    => 1.0,
        }
    }

    /// Per-weapon *minimum* range multiplier — applied as an inner dead-zone
    /// the turret can't shoot inside. 0.0 for nearly every weapon (no dead
    /// zone); 1.0 for Mortar (can't shoot anything closer than the base
    /// `TURRET_RANGE`). Combined with the same `stats.range_mult()` and
    /// pier buff that scale the outer range, so a buffed turret's inner
    /// and outer rings expand together — keeping the playable annulus
    /// roughly the same shape rather than collapsing to a sliver.
    pub fn min_range_mult(self) -> f32 {
        match self {
            WeaponType::Mortar => 1.0,
            _ => 0.0,
        }
    }

}

/// Per-weapon material lookups. Lives in this module (not in palette) so
/// adding a weapon variant is a single-file change here — palette only needs
/// the material handles to exist.
impl PaletteMaterials {
    pub fn turret_for(&self, w: WeaponType) -> &Handle<ColorMaterial> {
        match w {
            WeaponType::Standard   => &self.turret,
            WeaponType::Sniper     => &self.turret_sniper,
            WeaponType::MachineGun => &self.turret_mg,
            WeaponType::Shotgun    => &self.turret_shotgun,
            WeaponType::Railgun    => &self.turret_railgun,
            WeaponType::Mortar     => &self.turret_mortar,
            // HeliPad gets its own gray deck-pad material; the yellow
            // `H` decal is added as a child entity in `setup_world`.
            WeaponType::HeliPad    => &self.helipad_deck,
        }
    }

    pub fn bullet_outer_for(&self, w: WeaponType) -> &Handle<ColorMaterial> {
        match w {
            WeaponType::Standard   => &self.bullet_friendly_outer,
            WeaponType::Sniper     => &self.bullet_sniper_outer,
            WeaponType::MachineGun => &self.bullet_mg_outer,
            WeaponType::Shotgun    => &self.bullet_shotgun_outer,
            WeaponType::Railgun    => &self.bullet_railgun_outer,
            WeaponType::Mortar     => &self.bullet_mortar_outer,
            // Helicopter bullets reuse the standard friendly bullet look.
            WeaponType::HeliPad    => &self.bullet_friendly_outer,
        }
    }

    pub fn bullet_inner_for(&self, w: WeaponType) -> &Handle<ColorMaterial> {
        match w {
            WeaponType::Standard   => &self.bullet_friendly,
            WeaponType::Sniper     => &self.bullet_sniper,
            WeaponType::MachineGun => &self.bullet_mg,
            WeaponType::Shotgun    => &self.bullet_shotgun,
            WeaponType::Railgun    => &self.bullet_railgun,
            WeaponType::Mortar     => &self.bullet_mortar,
            WeaponType::HeliPad    => &self.bullet_friendly,
        }
    }
}
