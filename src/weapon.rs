//! Weapon archetypes for player turrets.
//!
//! Adding a new weapon is a five-spot change:
//! 1. Add a variant to `WeaponType`.
//! 2. Add rows in `defaults`, `label`, `spread`, `next` (cycle order),
//!    and `tag` (gameplay class for synergies).
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
    /// Pirate cannon — slow, heavy cannonball that knocks enemies
    /// back on hit. See `cannon.rs` for the knockback application.
    Cannon,
    /// Support booster — fires nothing. While adjacent to other
    /// turret slots, multiplies each neighbour's effective fire rate.
    /// Adjacency graph: `balance::TURRET_ADJACENCY`. Boost applied
    /// in `sync_turret_config`.
    Booster,
    /// Melee blade — extends a rotating arm from the slot. Damages
    /// enemies inside the blade's reach on a tick rather than firing
    /// a projectile. See `blade.rs` for the rotating-arm spawn + damage
    /// system.
    Blade,
}

/// Gameplay-class tag attached to each weapon. Used by the tooltip to
/// render a coloured `[TAG]` chip under the title, and intended as the
/// hook for future "all Naval turrets gain X" / "Pirate weapons +Y" type
/// synergies. Each `WeaponType` carries exactly one tag (see `tag()`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WeaponTag {
    /// Conventional ship-mounted artillery — the baseline navy roster.
    Naval,
    /// Energy / sci-fi weaponry (rails, beams, plasma).
    Future,
    /// Deploys an autonomous unit that fights independently.
    Autonomous,
    /// Crude, brutal, knock-em-back weaponry — pirate flavour.
    Pirate,
    /// Doesn't fight directly; buffs adjacent turrets.
    Support,
    /// Close-quarters melee weapons that don't fire projectiles.
    Melee,
}

impl WeaponTag {
    /// Every `WeaponTag` variant in declaration order. Used by the
    /// tooltip to look a tag up by its label without a string match.
    pub fn all() -> &'static [WeaponTag] {
        &[
            WeaponTag::Naval,
            WeaponTag::Future,
            WeaponTag::Autonomous,
            WeaponTag::Pirate,
            WeaponTag::Support,
            WeaponTag::Melee,
        ]
    }

    /// Display label — looked up in `data/translations.csv`. Same string
    /// is what the tooltip wraps in `[ ]` brackets when rendering the chip.
    pub fn label(self) -> &'static str {
        match self {
            WeaponTag::Naval      => tr("weapon_tag_naval"),
            WeaponTag::Future     => tr("weapon_tag_future"),
            WeaponTag::Autonomous => tr("weapon_tag_autonomous"),
            WeaponTag::Pirate     => tr("weapon_tag_pirate"),
            WeaponTag::Support    => tr("weapon_tag_support"),
            WeaponTag::Melee      => tr("weapon_tag_melee"),
        }
    }

    /// Chip colour for the tooltip rendering. Picked to be visually
    /// distinct from the buff-green / nerf-red used by `colorize_bonuses`
    /// so a tag chip is never confused with a +/- numeric token.
    pub fn color(self) -> Color {
        match self {
            // Steel blue — reads as the "default navy" baseline.
            WeaponTag::Naval      => Color::srgb(0.50, 0.70, 0.95),
            // Bright cyan — sci-fi energy hue.
            WeaponTag::Future     => Color::srgb(0.45, 0.90, 0.95),
            // Army green — matches the helipad deck colour.
            WeaponTag::Autonomous => Color::srgb(0.55, 0.80, 0.45),
            // Wood / gold brown — pirate flavour.
            WeaponTag::Pirate     => Color::srgb(0.95, 0.70, 0.30),
            // Soft warm yellow — distinct from the gold title colour
            // by being lower saturation.
            WeaponTag::Support    => Color::srgb(0.95, 0.85, 0.55),
            // Crimson — visceral / blade flavour, distinct from the
            // pure red used for nerf tokens.
            WeaponTag::Melee      => Color::srgb(0.95, 0.45, 0.50),
        }
    }
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
    /// represents wrapping back to "unequipped". New tag-flavour weapons
    /// (Cannon → Booster → Blade) sit at the end of the cycle so the
    /// classic Naval roster comes first.
    pub fn next(self) -> Option<Self> {
        match self {
            WeaponType::Standard   => Some(WeaponType::Sniper),
            WeaponType::Sniper     => Some(WeaponType::MachineGun),
            WeaponType::MachineGun => Some(WeaponType::Shotgun),
            WeaponType::Shotgun    => Some(WeaponType::Railgun),
            WeaponType::Railgun    => Some(WeaponType::Mortar),
            WeaponType::Mortar     => Some(WeaponType::HeliPad),
            WeaponType::HeliPad    => Some(WeaponType::Cannon),
            WeaponType::Cannon     => Some(WeaponType::Booster),
            WeaponType::Booster    => Some(WeaponType::Blade),
            WeaponType::Blade      => None,
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
            // Cannon: pirate-grade. Heavy single shot, slow cadence,
            // hits like a wrecking ball. Fire rate intentionally lower
            // than Sniper since each shot also knocks the target back.
            WeaponType::Cannon     => (8, 0.6),
            // Booster: doesn't fire. Defaults are placeholders so the
            // stats panel reads sensibly; `damage` is unused and
            // `fire_rate` is what gets multiplied across to neighbours.
            WeaponType::Booster    => (0, 0.0),
            // Blade: also doesn't fire bullets. The "fire_rate" value
            // is repurposed by `blade.rs` as the damage tick frequency
            // (hits per second), so the slot's UI still drives the
            // cadence. `damage` is per tick. Twin/triple barrels spawn
            // multiple physical blades, each ticking independently —
            // so total dps = damage × fire_rate × barrels.
            WeaponType::Blade      => (5, 6.0),
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
            WeaponType::Cannon     => tr("weapon_cannon"),
            WeaponType::Booster    => tr("weapon_booster"),
            WeaponType::Blade      => tr("weapon_blade"),
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
            WeaponType::Cannon     => tr("weapon_cannon_desc"),
            WeaponType::Booster    => tr("weapon_booster_desc"),
            WeaponType::Blade      => tr("weapon_blade_desc"),
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
            // Cannon: a touch shorter than Standard — heavy projectile,
            // close-to-mid engagement.
            WeaponType::Cannon     => 0.9,
            // Booster + Blade: no projectile range. 1.0 is a placeholder
            // for the exhaustive match — the firing pipeline skips both.
            WeaponType::Booster    => 1.0,
            WeaponType::Blade      => 1.0,
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

    /// Whether this weapon fires anything from the turret base. False for
    /// HeliPad (helicopter does the firing), Booster (pure support), and
    /// Blade (melee aura). The aim/fire system early-returns for these so
    /// the slot doesn't try to track a target or spawn muzzle flashes.
    pub fn fires_from_base(self) -> bool {
        !matches!(self, WeaponType::HeliPad | WeaponType::Booster | WeaponType::Blade)
    }

    /// Whether this weapon's turret should show the standard barrel
    /// children. False for HeliPad (deck pad only), Booster (support
    /// platform), and Blade (arm + blade decor instead). `sync_turret_config`
    /// uses this to hide the barrel meshes when the slot's weapon doesn't
    /// have any.
    pub fn has_barrels(self) -> bool {
        !matches!(self, WeaponType::HeliPad | WeaponType::Booster | WeaponType::Blade)
    }

    /// Gameplay-class tag — the chip rendered in the tooltip and the
    /// hook for future synergies. See `WeaponTag` for the full taxonomy.
    pub fn tag(self) -> WeaponTag {
        match self {
            WeaponType::Standard
            | WeaponType::Sniper
            | WeaponType::MachineGun
            | WeaponType::Shotgun
            | WeaponType::Mortar   => WeaponTag::Naval,
            WeaponType::Railgun    => WeaponTag::Future,
            WeaponType::HeliPad    => WeaponTag::Autonomous,
            WeaponType::Cannon     => WeaponTag::Pirate,
            WeaponType::Booster    => WeaponTag::Support,
            WeaponType::Blade      => WeaponTag::Melee,
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
            WeaponType::Cannon     => &self.turret_cannon,
            WeaponType::Booster    => &self.turret_booster,
            WeaponType::Blade      => &self.turret_blade,
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
            WeaponType::Cannon     => &self.bullet_cannon_outer,
            // Booster + Blade never spawn bullets; fall back to the
            // friendly material so the exhaustive match compiles.
            WeaponType::Booster    => &self.bullet_friendly_outer,
            WeaponType::Blade      => &self.bullet_friendly_outer,
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
            WeaponType::Cannon     => &self.bullet_cannon,
            WeaponType::Booster    => &self.bullet_friendly,
            WeaponType::Blade      => &self.bullet_friendly,
        }
    }
}
