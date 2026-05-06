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
}

impl WeaponType {
    /// Forward-cycle through equipped types (for the EQUIP button). `None`
    /// represents wrapping back to "unequipped".
    pub fn next(self) -> Option<Self> {
        match self {
            WeaponType::Standard   => Some(WeaponType::Sniper),
            WeaponType::Sniper     => Some(WeaponType::MachineGun),
            WeaponType::MachineGun => Some(WeaponType::Shotgun),
            WeaponType::Shotgun    => Some(WeaponType::Railgun),
            WeaponType::Railgun    => None,
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
        }
    }

    /// Half-angle (rad) of random firing cone. 0 means perfectly accurate.
    pub fn spread(self) -> f32 {
        match self {
            WeaponType::MachineGun => 0.18, // ~±10°
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
        }
    }

    pub fn bullet_outer_for(&self, w: WeaponType) -> &Handle<ColorMaterial> {
        match w {
            WeaponType::Standard   => &self.bullet_friendly_outer,
            WeaponType::Sniper     => &self.bullet_sniper_outer,
            WeaponType::MachineGun => &self.bullet_mg_outer,
            WeaponType::Shotgun    => &self.bullet_shotgun_outer,
            WeaponType::Railgun    => &self.bullet_railgun_outer,
        }
    }

    pub fn bullet_inner_for(&self, w: WeaponType) -> &Handle<ColorMaterial> {
        match w {
            WeaponType::Standard   => &self.bullet_friendly,
            WeaponType::Sniper     => &self.bullet_sniper,
            WeaponType::MachineGun => &self.bullet_mg,
            WeaponType::Shotgun    => &self.bullet_shotgun,
            WeaponType::Railgun    => &self.bullet_railgun,
        }
    }
}
