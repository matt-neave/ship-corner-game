//! Player-wide stat baseline + modifier accumulator.
//!
//! Every gameplay system that scales with player power reads from the
//! single `PlayerStats` resource here (HP, move/turn speeds, turret
//! turn/range/arc, crit, luck, harvest, shield, rune damage). Items,
//! upgrades, and future modifier cards write into the per-stat `Stat`
//! `flat`/`percent` fields — the resource itself never gets replaced
//! mid-run, only mutated.
//!
//! Stacking model is Brotato-style per stat:
//!     effective = (base + Σ flat) × (1 + Σ percent)
//!
//! Crit and harvest both use the same Risk-of-Rain-style multiplier
//! tier formula: each 100% of the source stat guarantees a +1 tier
//! over the base 1× outcome, and the fractional remainder is the
//! roll for the next tier (see `roll_ror_tier`).

use bevy::prelude::*;
use rand::Rng;

/// Recharging hp buffer carried by the friendly ship. Sits in front of
/// `Health` — incoming damage is consumed by `current` first, leftover
/// hits HP. Recharge starts after `time_since_damage` exceeds the
/// player's `shield_recharge_delay` stat.
#[derive(Component, Default, Debug)]
pub struct Shield {
    pub current: f32,
    pub time_since_damage: f32,
}

impl Shield {
    /// Absorb up to `damage` from the shield buffer. Returns the
    /// leftover damage that should fall through to HP.
    pub fn absorb(&mut self, damage: i32) -> i32 {
        if damage <= 0 || self.current <= 0.0 {
            return damage;
        }
        let absorbed = self.current.min(damage as f32);
        self.current -= absorbed;
        self.time_since_damage = 0.0;
        (damage as f32 - absorbed).round() as i32
    }
}

/// Tick the friendly ship's shield: clamp to current max, advance the
/// post-damage timer, and recharge once the delay has elapsed.
pub fn shield_recharge_system(
    time: Res<Time>,
    stats: Res<PlayerStats>,
    mut q: Query<&mut Shield>,
) {
    let dt = time.delta_secs();
    let max = stats.shield_max.effective().max(0.0);
    let delay = stats.shield_recharge_delay.effective().max(0.0);
    let rate_per_sec = (stats.shield_recharge_rate_pct.effective() / 100.0).max(0.0) * max;
    for mut shield in &mut q {
        if shield.current > max {
            shield.current = max;
        }
        shield.time_since_damage += dt;
        if shield.time_since_damage >= delay && shield.current < max {
            shield.current = (shield.current + rate_per_sec * dt).min(max);
        }
    }
}

/// One scalar stat with `(base + flat) × (1 + percent)` accumulation.
/// `percent` is expressed as a 0..N multiplier delta — 0.5 == +50%.
#[derive(Clone, Copy, Debug)]
pub struct Stat {
    pub base: f32,
    pub flat: f32,
    pub percent: f32,
}

impl Stat {
    pub const fn new(base: f32) -> Self {
        Self { base, flat: 0.0, percent: 0.0 }
    }
    #[inline]
    pub fn effective(&self) -> f32 {
        (self.base + self.flat) * (1.0 + self.percent)
    }
}

/// Single source of truth for player-wide stats. Read by ship movement,
/// turret aim, bullet damage, scrap drops, rune effects, shield system.
#[derive(Resource, Clone, Debug)]
pub struct PlayerStats {
    pub hp: Stat,
    pub move_speed: Stat,
    pub turn_speed: Stat,
    pub turret_turn_speed: Stat,
    /// Additive bonus to each turret slot's half-arc, in degrees. The
    /// per-slot baseline (45° axial / 60° wing) lives in `balance.rs`;
    /// this just shifts every slot by the same delta. Final half-arc is
    /// clamped to 180° (= 360° total cone).
    pub turret_arc_bonus_deg: Stat,
    /// RoR-style reroll tier on a failed proc. 100% = one guaranteed
    /// reroll on failure, 200% = two, 150% = one guaranteed + 50%
    /// chance for a second.
    pub luck_pct: Stat,
    /// Additive % bonus to a proc's roll strength (clamped at 1.0
    /// before the dice roll). Layered onto the base proc strength
    /// before luck rerolls.
    pub proc_strength_pct: Stat,
    /// RoR-tier crit. The roll itself decides both whether and how
    /// hard: 0% = no crit, 50% = 50/50 between 1× and 2×, 100% =
    /// always 2×, 150% = 50/50 between 2× and 3×, etc.
    pub crit_pct: Stat,
    pub range_pct: Stat,
    pub harvest_pct: Stat,
    pub shield_max: Stat,
    pub shield_recharge_rate_pct: Stat,
    pub shield_recharge_delay: Stat,
    /// Raw rune-damage scalar (default 1.0). Each rune declares its
    /// effect as a percentage of this value — Fire's "100% rune damage
    /// per tick" means `1.0 × rune_damage` per tick. The two halves of
    /// the description (the rune's % and the player's raw value)
    /// multiply.
    pub rune_damage: Stat,
    /// Percentage modifier on every turret slot's base damage —
    /// composes multiplicatively with synergy multipliers inside
    /// `sync_turret_config`. 0.0 = +0% (no change); +25.0 = ×1.25.
    /// All consumers go through `turret_damage_mult()` below.
    pub turret_damage_pct: Stat,
}

impl Default for PlayerStats {
    fn default() -> Self {
        Self {
            hp: Stat::new(75.0),
            move_speed: Stat::new(30.0),
            turn_speed: Stat::new(5.0),
            // Default = the cap (360°/s = 2π rad/s). Bumping T.TURN
            // beyond this clamps in `effective_turret_turn_speed`.
            turret_turn_speed: Stat::new(std::f32::consts::TAU),
            turret_arc_bonus_deg: Stat::new(0.0),
            luck_pct: Stat::new(0.0),
            proc_strength_pct: Stat::new(0.0),
            // 1% baseline: average 1.01× damage from crits at start.
            crit_pct: Stat::new(1.0),
            range_pct: Stat::new(100.0),
            harvest_pct: Stat::new(0.0),
            shield_max: Stat::new(0.0),
            shield_recharge_rate_pct: Stat::new(20.0),
            shield_recharge_delay: Stat::new(3.0),
            rune_damage: Stat::new(1.0),
            turret_damage_pct: Stat::new(0.0),
        }
    }
}

impl PlayerStats {
    /// Current max HP rounded for the integer Health component.
    pub fn max_hp(&self) -> i32 {
        self.hp.effective().round() as i32
    }
    /// Range multiplier (1.0 at 100% baseline). Multiplied with each
    /// weapon's intrinsic `range_mult` and any pier/slot buff.
    pub fn range_mult(&self) -> f32 {
        self.range_pct.effective() / 100.0
    }
    /// Rune-damage scalar (default 1.0). Each rune coefficient
    /// multiplies this directly — fire's "100%" → `1.0 × rune_damage`.
    pub fn rune_damage_mult(&self) -> f32 {
        self.rune_damage.effective().max(0.0)
    }
    /// Turret-damage multiplier (default 1.0). Computed from the
    /// `turret_damage_pct` stat — `1.0 + pct/100`, clamped at 0 so
    /// a stack of nerfs can't flip damage negative. Composed into
    /// every slot's `slot.damage` inside `sync_turret_config`, so
    /// all downstream consumers (bullets / beam / blade / octopus /
    /// helicopter / mortar / cannon) inherit the buff via
    /// `slot.damage` without per-system plumbing.
    pub fn turret_damage_mult(&self) -> f32 {
        (1.0 + self.turret_damage_pct.effective() / 100.0).max(0.0)
    }
    /// Additive bonus to rune proc strength, expressed as 0..1.
    pub fn proc_strength_bonus(&self) -> f32 {
        (self.proc_strength_pct.effective() / 100.0).max(0.0)
    }
    /// Roll a crit multiplier for one ship-sourced damage instance.
    /// `crit_pct` is the single source of truth — RoR tiers decide
    /// both "does it crit" and "how hard" in one roll.
    pub fn roll_crit_mult(&self, rng: &mut impl Rng) -> u32 {
        roll_ror_tier(rng, self.crit_pct.effective())
    }
    /// Roll one proc with luck-driven rerolls on failure.
    ///
    /// `base_strength` is the per-roll success probability (already
    /// includes Conduit / proc-strength bonus / clamp). On failure, the
    /// player's `luck_pct` adds rerolls: each whole 100% is one
    /// guaranteed reroll, and any fractional remainder is a chance for
    /// one extra. Returns true the moment any roll passes.
    pub fn proc_roll_with_luck(&self, rng: &mut impl Rng, base_strength: f32) -> bool {
        let p = base_strength.clamp(0.0, 1.0);
        if rng.r#gen::<f32>() < p { return true; }
        let luck = (self.luck_pct.effective() / 100.0).max(0.0);
        let guaranteed = luck.floor() as u32;
        for _ in 0..guaranteed {
            if rng.r#gen::<f32>() < p { return true; }
        }
        let remainder = luck.fract();
        if remainder > 0.0 && rng.r#gen::<f32>() < remainder {
            if rng.r#gen::<f32>() < p { return true; }
        }
        false
    }
    /// Roll the scrap multiplier for one pickup. Always ≥1.
    pub fn roll_harvest_mult(&self, rng: &mut impl Rng) -> u32 {
        roll_ror_tier(rng, self.harvest_pct.effective())
    }
    /// Effective half-arc for a turret slot, in radians, clamped to
    /// 180° (= 360° total cone). `slot_base_half_rad` is the per-slot
    /// baseline from `balance::TURRET_ARC_HALVES`.
    pub fn effective_turret_half_arc(&self, slot_base_half_rad: f32) -> f32 {
        let bonus = self.turret_arc_bonus_deg.effective().to_radians();
        (slot_base_half_rad + bonus).min(std::f32::consts::PI)
    }
    /// Effective turret rotation speed in rad/s, capped at 360°/s
    /// (= one full rotation per second). The cap lives here so the
    /// readout panel and the aim/fire system agree.
    pub fn effective_turret_turn_speed(&self) -> f32 {
        let max = std::f32::consts::TAU; // 360°/s
        self.turret_turn_speed.effective().min(max)
    }
}

/// Identifier for one displayable stat. Used by UI panels (the live
/// stat readout, future upgrade cards) to enumerate stats and pull
/// values without the UI hardcoding each one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatKind {
    Hp,
    MoveSpeed,
    TurnSpeed,
    TurretTurnSpeed,
    TurretArcBonus,
    Luck,
    ProcStrength,
    Crit,
    Range,
    Harvest,
    ShieldMax,
    /// Multiplier on rune effects (Fire DoT, Detonate burst, Shock
    /// chain count).
    RuneDamage,
    /// Percentage modifier on every turret slot's base damage —
    /// composes multiplicatively with synergies in
    /// `sync_turret_config`. 0% = no change, +25% = 1.25x damage.
    TurretDamage,
}

impl StatKind {
    /// Display order for the readout panel. Append new kinds here —
    /// the panel re-flows automatically.
    pub const ALL: &'static [StatKind] = &[
        StatKind::Hp,
        StatKind::Shield,
        StatKind::MoveSpeed,
        StatKind::TurnSpeed,
        StatKind::TurretTurnSpeed,
        StatKind::TurretArcBonus,
        StatKind::TurretDamage,
        StatKind::Range,
        StatKind::Crit,
        StatKind::Luck,
        StatKind::ProcStrength,
        StatKind::Harvest,
        StatKind::RuneDamage,
    ];
    /// Full label rendered in the panel. Plain words, no shorthand.
    pub fn label(self) -> &'static str {
        match self {
            StatKind::Hp => "HP",
            StatKind::MoveSpeed => "MOVE SPEED",
            StatKind::TurnSpeed => "TURN SPEED",
            StatKind::TurretTurnSpeed => "TURRET TURN",
            StatKind::TurretArcBonus => "TURRET ARC",
            StatKind::Luck => "LUCK",
            StatKind::ProcStrength => "PROC STRENGTH",
            StatKind::Crit => "CRIT CHANCE",
            StatKind::Range => "RANGE",
            StatKind::Harvest => "HARVEST",
            StatKind::ShieldMax => "SHIELD",
            StatKind::RuneDamage => "RUNE DAMAGE",
            StatKind::TurretDamage => "TURRET DAMAGE",
        }
    }
    /// Formatted current value (with units / sign where helpful).
    pub fn format_value(self, stats: &PlayerStats) -> String {
        match self {
            StatKind::Hp => format!("{}", stats.max_hp()),
            StatKind::MoveSpeed => {
                let baseline = PlayerStats::default().move_speed.effective();
                let pct = (stats.move_speed.effective() / baseline * 100.0).round() as i32;
                format!("{}%", pct)
            }
            // Relative percentages against the baseline so the
            // stat reads as a tunable knob ("100% = stock") rather
            // than a raw rad/s value the player has to translate.
            // Skips the degree glyph (default font has no U+00B0)
            // which previously rendered as a tofu box that looked
            // like the digit "1".
            StatKind::TurnSpeed => {
                let baseline = PlayerStats::default().turn_speed.effective();
                let pct = (stats.turn_speed.effective() / baseline * 100.0).round() as i32;
                format!("{}%", pct)
            }
            StatKind::TurretTurnSpeed => {
                let baseline = PlayerStats::default().effective_turret_turn_speed();
                let pct = (stats.effective_turret_turn_speed() / baseline * 100.0).round() as i32;
                format!("{}%", pct)
            }
            StatKind::TurretArcBonus => {
                let v = stats.turret_arc_bonus_deg.effective();
                if v > 0.0 { format!("+{:.0} deg", v) } else { format!("{:.0} deg", v) }
            }
            StatKind::Luck => format!("{:.0}%", stats.luck_pct.effective()),
            StatKind::ProcStrength => format!("{:.0}%", stats.proc_strength_pct.effective()),
            StatKind::Crit => format!("{:.0}%", stats.crit_pct.effective()),
            StatKind::Range => format!("{:.0}%", stats.range_pct.effective()),
            StatKind::Harvest => format!("{:.0}%", stats.harvest_pct.effective()),
            StatKind::ShieldMax => format!("{:.0}", stats.shield_max.effective()),
            StatKind::RuneDamage => {
                // Default 1.0 reads as 100%; players track Rune
                // Damage as a multiplier, so a percentage cue maps
                // cleaner to "how much extra burn am I doing?"
                let pct = (stats.rune_damage.effective() * 100.0).round() as i32;
                format!("{}%", pct)
            }
            StatKind::TurretDamage => {
                let v = stats.turret_damage_pct.effective();
                if v >= 0.0 { format!("+{:.0}%", v) } else { format!("{:.0}%", v) }
            }
        }
    }
}

// Alias kept so the panel ordering reads naturally above.
impl StatKind { #[allow(non_upper_case_globals)] pub const Shield: StatKind = StatKind::ShieldMax; }

impl StatKind {
    /// Long-form description for the hover tooltip. Looked up in
    /// `data/translations.csv` so adding a language is one column.
    pub fn description(self) -> &'static str {
        match self {
            StatKind::Hp => crate::i18n::tr("stat_hp_desc"),
            StatKind::MoveSpeed => crate::i18n::tr("stat_move_speed_desc"),
            StatKind::TurnSpeed => crate::i18n::tr("stat_turn_speed_desc"),
            StatKind::TurretTurnSpeed => crate::i18n::tr("stat_turret_turn_speed_desc"),
            StatKind::TurretArcBonus => crate::i18n::tr("stat_turret_arc_bonus_desc"),
            StatKind::Luck => crate::i18n::tr("stat_luck_desc"),
            StatKind::ProcStrength => crate::i18n::tr("stat_proc_strength_desc"),
            StatKind::Crit => crate::i18n::tr("stat_crit_desc"),
            StatKind::Range => crate::i18n::tr("stat_range_desc"),
            StatKind::Harvest => crate::i18n::tr("stat_harvest_desc"),
            StatKind::ShieldMax => crate::i18n::tr("stat_shield_max_desc"),
            StatKind::RuneDamage => crate::i18n::tr("stat_rune_damage_desc"),
            StatKind::TurretDamage => crate::i18n::tr("stat_turret_damage_desc"),
        }
    }
    /// Step size for one click of the debug `+/-` button. Tuned per-stat
    /// so each click is a meaningful nudge in that stat's natural unit.
    pub fn debug_step(self) -> f32 {
        match self {
            StatKind::Hp => 25.0,
            StatKind::MoveSpeed => 3.0,
            StatKind::TurnSpeed => 0.5,           // rad/s
            StatKind::TurretTurnSpeed => 0.5,     // rad/s
            StatKind::TurretArcBonus => 10.0,     // degrees
            StatKind::Luck => 25.0,
            StatKind::ProcStrength => 10.0,
            StatKind::Crit => 25.0,
            StatKind::Range => 10.0,
            StatKind::Harvest => 25.0,
            StatKind::ShieldMax => 10.0,
            StatKind::RuneDamage => 0.5,
            StatKind::TurretDamage => 10.0, // +10 percentage points / step
        }
    }

    /// Format a delta value (typically the mod-card upgrade amount)
    /// in the same units the stats panel renders the live value in.
    /// Percent stats show `+25%`, multiplier stats like Rune Damage
    /// re-express the raw 0.5 delta as `+50%`, raw-number stats
    /// (Hp, MoveSpeed, etc.) show the bare signed number.
    pub fn format_delta(self, delta: f32) -> String {
        match self {
            // 0.5 multiplier delta reads as +50%.
            StatKind::RuneDamage => format!("{:+.0}%", delta * 100.0),
            // Stats already stored as percent points - just append %.
            StatKind::TurretDamage
            | StatKind::Crit
            | StatKind::Luck
            | StatKind::ProcStrength
            | StatKind::Range
            | StatKind::Harvest => format!("{:+.0}%", delta),
            // Everything else - bare signed number, drop the .0
            // when the value is an integer so it doesn't read as a
            // float (e.g. "+3" not "+3.0").
            _ => {
                if delta.fract().abs() < 0.01 {
                    format!("{:+.0}", delta)
                } else {
                    format!("{:+.1}", delta)
                }
            }
        }
    }
    /// Read-only handle on this kind's `Stat` slot. Used by the
    /// stats-panel value coloring to compare current value vs the
    /// baseline (`PlayerStats::default()`) to decide green / red /
    /// grey. Mirrors `stat_mut` below — keep both arms in sync.
    pub fn stat(self, stats: &PlayerStats) -> &Stat {
        match self {
            StatKind::Hp => &stats.hp,
            StatKind::MoveSpeed => &stats.move_speed,
            StatKind::TurnSpeed => &stats.turn_speed,
            StatKind::TurretTurnSpeed => &stats.turret_turn_speed,
            StatKind::TurretArcBonus => &stats.turret_arc_bonus_deg,
            StatKind::Luck => &stats.luck_pct,
            StatKind::ProcStrength => &stats.proc_strength_pct,
            StatKind::Crit => &stats.crit_pct,
            StatKind::Range => &stats.range_pct,
            StatKind::Harvest => &stats.harvest_pct,
            StatKind::ShieldMax => &stats.shield_max,
            StatKind::RuneDamage => &stats.rune_damage,
            StatKind::TurretDamage => &stats.turret_damage_pct,
        }
    }

    /// Mutable handle on this kind's `Stat` slot inside `PlayerStats`.
    /// The debug buttons + future upgrade cards both write through here.
    pub fn stat_mut(self, stats: &mut PlayerStats) -> &mut Stat {
        match self {
            StatKind::Hp => &mut stats.hp,
            StatKind::MoveSpeed => &mut stats.move_speed,
            StatKind::TurnSpeed => &mut stats.turn_speed,
            StatKind::TurretTurnSpeed => &mut stats.turret_turn_speed,
            StatKind::TurretArcBonus => &mut stats.turret_arc_bonus_deg,
            StatKind::Luck => &mut stats.luck_pct,
            StatKind::ProcStrength => &mut stats.proc_strength_pct,
            StatKind::Crit => &mut stats.crit_pct,
            StatKind::Range => &mut stats.range_pct,
            StatKind::Harvest => &mut stats.harvest_pct,
            StatKind::ShieldMax => &mut stats.shield_max,
            StatKind::RuneDamage => &mut stats.rune_damage,
            StatKind::TurretDamage => &mut stats.turret_damage_pct,
        }
    }
}

/// RoR-style tier roll. Each 100% adds a guaranteed +1 over the base
/// 1× outcome; the fractional remainder is the chance for one extra
/// tier. Result is always ≥1.
///
/// 0% → 1 always · 50% → 50/50 between 1 and 2 · 100% → 2 always ·
/// 150% → 50/50 between 2 and 3 · etc.
pub fn roll_ror_tier(rng: &mut impl Rng, percent: f32) -> u32 {
    let p = (percent / 100.0).max(0.0);
    let guaranteed = 1 + p.floor() as u32;
    let remainder = p.fract();
    if rng.r#gen::<f32>() < remainder {
        guaranteed + 1
    } else {
        guaranteed
    }
}
