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
        // Don't clamp current down to max — Ward (and any future
        // overflow source) is allowed to sit above shield_max as a
        // one-time buffer. Regen only fires while current < max, so
        // overflow doesn't get topped back up after it absorbs a hit.
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
    /// XP-per-kill bonus, percentage. Same shape as `harvest_pct`
    /// but applied to XP rather than scrap drops. Effective XP
    /// granted = base_xp × (1 + xp_harvest_pct/100), rounded.
    pub xp_harvest_pct: Stat,
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
    /// Chance to ignore an incoming damage instance entirely, in
    /// percent. Capped at [`DEFENSIVE_CAP_PCT`] so the player can't
    /// become invulnerable through stacking. Rolled per damage
    /// instance on the local player; the host's ghost-of-peer skips
    /// the roll so the relayed amount stays correct on the peer.
    pub dodge_pct: Stat,
    /// Flat percentage damage reduction applied after dodge (if not
    /// dodged) and before shield absorption. Capped at
    /// [`DEFENSIVE_CAP_PCT`] for the same invulnerability reason.
    pub armour_pct: Stat,
}

/// Shared cap on both Dodge and Armour. Either stat solo at 100%
/// would make the player invulnerable, and stacking both would let
/// armour negate the un-dodged residual to zero. 60% is the
/// negotiated max — high enough to be a meaningful build target,
/// low enough that bursts still kill an unprepared player.
pub const DEFENSIVE_CAP_PCT: f32 = 60.0;

/// Baseline `move_speed` value. Pulled out as a named constant so
/// `StatKind::format_delta` can express speed mod deltas as a
/// percentage of base ("+10%") rather than as a raw world-units
/// number ("+3"), which doesn't read for the player.
pub const MOVE_SPEED_BASE: f32 = 30.0;
/// Baseline `turn_speed` value. Same rationale as `MOVE_SPEED_BASE`.
pub const TURN_SPEED_BASE: f32 = 5.0;

impl Default for PlayerStats {
    fn default() -> Self {
        Self {
            hp: Stat::new(75.0),
            move_speed: Stat::new(MOVE_SPEED_BASE),
            turn_speed: Stat::new(TURN_SPEED_BASE),
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
            xp_harvest_pct: Stat::new(0.0),
            shield_max: Stat::new(0.0),
            shield_recharge_rate_pct: Stat::new(5.0),
            shield_recharge_delay: Stat::new(3.0),
            rune_damage: Stat::new(1.0),
            turret_damage_pct: Stat::new(0.0),
            dodge_pct: Stat::new(0.0),
            armour_pct: Stat::new(0.0),
        }
    }
}

impl PlayerStats {
    /// Current max HP rounded for the integer Health component.
    /// Floored at 1 so a heavy negative buff (e.g. Glass Cannon's
    /// `-30 HP` super mod stacked twice) can't push the ship into
    /// instant-death territory.
    pub fn max_hp(&self) -> i32 {
        self.hp.effective().round().max(1.0) as i32
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
    /// Roll a single scrap drop for one enemy kill. `harvest_pct` is
    /// the percentage chance the enemy drops 1 scrap; otherwise 0.
    /// Returns 0 or 1.
    pub fn roll_harvest_drop(&self, rng: &mut impl Rng, pirate_mult: f32) -> u32 {
        let chance = (self.harvest_pct.effective() / 100.0 * pirate_mult).clamp(0.0, 1.0);
        if rng.r#gen::<f32>() < chance { 1 } else { 0 }
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

    /// Effective dodge chance as a 0..1 probability, clamped to
    /// [`DEFENSIVE_CAP_PCT`]. UI shows the same clamped value so
    /// the player understands they've hit the cap.
    pub fn dodge_chance(&self) -> f32 {
        (self.dodge_pct.effective().clamp(0.0, DEFENSIVE_CAP_PCT)) / 100.0
    }

    /// Effective armour reduction as a 0..1 fraction of incoming
    /// damage to remove, clamped to [`DEFENSIVE_CAP_PCT`].
    pub fn armour_reduction(&self) -> f32 {
        (self.armour_pct.effective().clamp(0.0, DEFENSIVE_CAP_PCT)) / 100.0
    }

    /// Apply the full defensive stack (dodge → armour) to one
    /// incoming damage instance. Returns the post-mitigation damage
    /// the rest of the pipeline (shield + HP) should consume.
    /// Dodge is binary (full damage or zero); armour is a flat
    /// percentage reduction applied on the un-dodged residual. Both
    /// stats are independently capped at [`DEFENSIVE_CAP_PCT`].
    pub fn mitigate_incoming(&self, rng: &mut impl Rng, damage: i32) -> i32 {
        if damage <= 0 { return damage; }
        if rng.r#gen::<f32>() < self.dodge_chance() { return 0; }
        let reduced = (damage as f32 * (1.0 - self.armour_reduction())).round() as i32;
        reduced.max(0)
    }
}

/// Identifier for one displayable stat. Used by UI panels (the live
/// stat readout, future upgrade cards) to enumerate stats and pull
/// values without the UI hardcoding each one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum StatKind {
    Hp,
    MoveSpeed,
    TurnSpeed,
    /// Hidden from `ALL` / `ROLLABLE` — kept around so a future hull
    /// or skill could surface it without resurrecting the variant.
    TurretTurnSpeed,
    /// Hidden from `ALL` / `ROLLABLE` — same reasoning as above.
    TurretArcBonus,
    Luck,
    ProcStrength,
    Crit,
    Range,
    Harvest,
    /// XP-per-kill bonus. 0% = base XP (1 per kill, 5 per boss);
    /// +50% = 1.5× XP rolled into the granted amount. Computed at
    /// kill time in `enemy_death_check` so the multiplier applies
    /// before threshold checks.
    XpHarvest,
    ShieldMax,
    /// Multiplier on rune effects (Fire DoT, Bleed DoT, Shock
    /// chain count).
    RuneDamage,
    /// Percentage modifier on every turret slot's base damage —
    /// composes multiplicatively with synergies in
    /// `sync_turret_config`. 0% = no change, +25% = 1.25x damage.
    TurretDamage,
    /// Chance per incoming damage instance to take zero damage,
    /// percent. Capped at [`DEFENSIVE_CAP_PCT`].
    Dodge,
    /// Flat percentage reduction on un-dodged damage, applied
    /// before shield. Capped at [`DEFENSIVE_CAP_PCT`].
    Armour,
}

impl StatKind {
    /// Display order for the readout panel. Append new kinds here —
    /// the panel re-flows automatically.
    pub const ALL: &'static [StatKind] = &[
        StatKind::Hp,
        StatKind::Shield,
        StatKind::Dodge,
        StatKind::Armour,
        StatKind::MoveSpeed,
        StatKind::TurnSpeed,
        StatKind::TurretDamage,
        StatKind::Range,
        StatKind::Crit,
        StatKind::Luck,
        StatKind::ProcStrength,
        StatKind::Harvest,
        StatKind::XpHarvest,
        StatKind::RuneDamage,
    ];
    /// Subset of [`ALL`](Self::ALL) that the shop's mod-card and
    /// level-up roll systems pick from. `TurretArcBonus` +
    /// `TurretTurnSpeed` are omitted from both the readout AND the
    /// roll pool — they still exist on `PlayerStats` and apply at
    /// runtime if anything mutates them (e.g. future hull
    /// bonuses), but they aren't shown or rolled for.
    pub const ROLLABLE: &'static [StatKind] = &[
        StatKind::Hp,
        StatKind::Shield,
        StatKind::Dodge,
        StatKind::Armour,
        StatKind::MoveSpeed,
        StatKind::TurnSpeed,
        StatKind::TurretDamage,
        StatKind::Range,
        StatKind::Crit,
        StatKind::Luck,
        StatKind::ProcStrength,
        StatKind::Harvest,
        StatKind::XpHarvest,
        StatKind::RuneDamage,
    ];
    /// Full label rendered in the panel. Plain words, no shorthand.
    pub fn label(self) -> &'static str {
        match self {
            StatKind::Hp => "HP",
            StatKind::MoveSpeed => "MOVE SPEED",
            StatKind::TurnSpeed => "TURN SPEED",
            StatKind::TurretTurnSpeed => "TURRET TURN",
            StatKind::TurretArcBonus => "TURRET ARC BONUS",
            StatKind::Luck => "LUCK",
            StatKind::ProcStrength => "PROC STRENGTH",
            StatKind::Crit => "CRIT CHANCE",
            StatKind::Range => "RANGE",
            StatKind::Harvest => "HARVEST",
            StatKind::XpHarvest => "XP GAIN",
            StatKind::ShieldMax => "SHIELD",
            StatKind::RuneDamage => "RUNE EFFECT",
            StatKind::TurretDamage => "WEAPON DAMAGE",
            StatKind::Dodge => "DODGE",
            StatKind::Armour => "ARMOUR",
        }
    }
    /// Formatted current value (with units / sign where helpful).
    /// `synergies` is optional so non-customize call sites (boss
    /// reward, level-up card stats panel) can still call this
    /// without piping a `Synergies` resource through. When `None`,
    /// any "folded synergy" stat (currently only Weapon Damage)
    /// falls back to the raw player value — the synergy is invisible
    /// there, which is acceptable since those screens are short-
    /// lived informational overlays. The live customize panel
    /// passes `Some(&synergies)` so the headline number always
    /// matches the damage the weapons actually deal.
    pub fn format_value(
        self,
        stats: &PlayerStats,
        synergies: Option<&crate::synergy::Synergies>,
    ) -> String {
        match self {
            StatKind::Hp => format!("{}", stats.max_hp()),
            StatKind::MoveSpeed => {
                let baseline = PlayerStats::default().move_speed.effective();
                let pct = (stats.move_speed.effective() / baseline * 100.0).round() as i32;
                format!("{}%", pct)
            }
            // Relative percentages against the baseline so the
            // stat reads as a tunable knob ("100% = stock") rather
            // than a raw rad/s value. No degree glyph — the default
            // font has no U+00B0 and renders it as a tofu box.
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
                // Always-signed so the 0-baseline reads as a delta
                // ("+0°"), not an absolute arc value of zero. The
                // underlying number is a bonus on top of per-slot
                // defaults that live in `balance::TURRET_ARC_HALVES`.
                let v = stats.turret_arc_bonus_deg.effective();
                format!("{:+.0}°", v)
            }
            StatKind::Luck => format!("{:.0}%", stats.luck_pct.effective()),
            StatKind::ProcStrength => format!("{:.0}%", stats.proc_strength_pct.effective()),
            StatKind::Crit => format!("{:.0}%", stats.crit_pct.effective()),
            StatKind::Range => format!("{:.0}%", stats.range_pct.effective()),
            StatKind::Harvest => {
                // Chance an enemy drops 1 scrap on death. Pirate
                // synergy multiplies the chance (1× / 1.5× / 2× / …).
                let pirate_mult = synergies
                    .map(|s| s.pirate_harvest_mult())
                    .unwrap_or(1.0);
                let chance = (stats.harvest_pct.effective() * pirate_mult).clamp(0.0, 100.0);
                format!("{:.0}%", chance)
            }
            StatKind::XpHarvest => {
                // XP multiplier expressed as a baseline-100%
                // percentage. 100% = stock (1 XP per kill, 5 per
                // boss); +50% reads as 150%.
                let total = 100.0 + stats.xp_harvest_pct.effective();
                format!("{:.0}%", total)
            }
            StatKind::ShieldMax => format!("{:.0}", stats.shield_max.effective()),
            StatKind::RuneDamage => {
                // Default 1.0 reads as 100%; players track Rune
                // Damage as a multiplier, so a percentage cue maps
                // cleaner to "how much extra burn am I doing?"
                let pct = (stats.rune_damage.effective() * 100.0).round() as i32;
                format!("{}%", pct)
            }
            StatKind::TurretDamage => {
                // Total damage multiplier expressed as a baseline-
                // 100% percentage so mods and Naval synergy read
                // additively from the player's mental "what am I
                // doing right now" anchor: 100% = stock, 120% = a
                // Naval T1 build with no mods, 156% = a +30% mod
                // build with Naval T2 active.
                //
                // Naval is folded in because it's the one synergy
                // that buffs damage globally (every tag benefits) —
                // Support's buff is per-slot conditional and not
                // safe to fold into a global readout. Math: combined
                // multiplier = (1 + base%) × naval_mult; displayed
                // total = combined × 100.
                let base_mult = stats.turret_damage_mult();
                let naval_mult = synergies
                    .map(|s| s.naval_damage_mult())
                    .unwrap_or(1.0);
                let total_pct = base_mult * naval_mult * 100.0;
                format!("{:.0}%", total_pct)
            }
            // Both clamped at DEFENSIVE_CAP_PCT so the readout
            // can't disagree with the actual mitigation that
            // `mitigate_incoming` performs.
            StatKind::Dodge => {
                let v = stats.dodge_pct.effective().clamp(0.0, DEFENSIVE_CAP_PCT);
                format!("{:.0}%", v)
            }
            StatKind::Armour => {
                let v = stats.armour_pct.effective().clamp(0.0, DEFENSIVE_CAP_PCT);
                format!("{:.0}%", v)
            }
        }
    }
}

// Alias kept so the panel ordering reads naturally above.
impl StatKind { #[allow(non_upper_case_globals)] pub const Shield: StatKind = StatKind::ShieldMax; }

impl StatKind {
    /// Long-form description for the hover tooltip. Looked up in
    /// `data/translations.csv` so adding a language is one column.
    /// Static — for the "what this stat does" baseline copy with no
    /// numbers folded in. Hover tooltips that want the live value
    /// should call [`dynamic_description`](Self::dynamic_description).
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
            StatKind::XpHarvest => crate::i18n::tr("stat_xp_harvest_desc"),
            StatKind::ShieldMax => crate::i18n::tr("stat_shield_max_desc"),
            StatKind::RuneDamage => crate::i18n::tr("stat_rune_damage_desc"),
            StatKind::TurretDamage => crate::i18n::tr("stat_turret_damage_desc"),
            StatKind::Dodge => crate::i18n::tr("stat_dodge_desc"),
            StatKind::Armour => crate::i18n::tr("stat_armour_desc"),
        }
    }

    /// Per-stat description with the player's CURRENT effective
    /// value baked in. Falls back to the static `description()` for
    /// stats that don't have a useful number to fold (e.g. straight
    /// "raw HP value" already shows the number in the readout).
    ///
    /// `Crit` is the special-case one: the RoR-style tier roll
    /// means a single "N% chance" reading lies. 50% → 50% chance
    /// of 2×; 150% → 50% chance of 3× (with 2× as the floor); etc.
    /// We surface both the floor multiplier and the fractional
    /// chance for the next tier so the player can read what they
    /// actually get on every shot.
    pub fn dynamic_description(self, stats: &PlayerStats) -> String {
        match self {
            StatKind::Dodge => {
                let v = stats.dodge_pct.effective().clamp(0.0, DEFENSIVE_CAP_PCT);
                format!(
                    "{:.0}% chance to negate an incoming hit entirely. Capped at {:.0}%.",
                    v, DEFENSIVE_CAP_PCT,
                )
            }
            StatKind::Armour => {
                let v = stats.armour_pct.effective().clamp(0.0, DEFENSIVE_CAP_PCT);
                format!(
                    "Reduces incoming damage by {:.0}% (after dodge, before shield). Capped at {:.0}%.",
                    v, DEFENSIVE_CAP_PCT,
                )
            }
            StatKind::Crit => {
                let pct = stats.crit_pct.effective().max(0.0);
                let p = pct / 100.0;
                let guaranteed_extra = p.floor() as u32; // tiers above 1× that always fire
                let fraction = (p.fract() * 100.0).round() as u32; // chance for one more tier
                let floor_mult = 1 + guaranteed_extra; // always at least this
                if guaranteed_extra == 0 && fraction == 0 {
                    "No crit chance.".to_string()
                } else if fraction == 0 {
                    // Whole-tier — always crits, no fractional roll.
                    format!("Every hit deals {}x damage.", floor_mult)
                } else if guaranteed_extra == 0 {
                    // Pure chance at 2× (e.g. 50% → "50% chance for 2x").
                    format!("{}% chance to deal {}x damage.", fraction, floor_mult + 1)
                } else {
                    // Always at least floor_mult×, fraction% chance for one more tier.
                    format!(
                        "Every hit deals {}x damage; {}% chance to deal {}x instead.",
                        floor_mult, fraction, floor_mult + 1,
                    )
                }
            }
            StatKind::Range => {
                let pct = stats.range_pct.effective();
                format!(
                    "Turret firing range — currently {:.0}% of baseline.",
                    pct,
                )
            }
            StatKind::Harvest => {
                let chance = stats.harvest_pct.effective().clamp(0.0, 100.0);
                format!(
                    "{:.0}% chance an enemy drops 1 scrap on death. Pirate synergy multiplies the chance.",
                    chance,
                )
            }
            StatKind::Luck => {
                let pct = stats.luck_pct.effective().max(0.0);
                format!(
                    "Free re-rolls on failed rune procs. Currently {:.0}% — every 100% buys one guaranteed reroll per shot.",
                    pct,
                )
            }
            StatKind::ProcStrength => {
                let pct = stats.proc_strength_pct.effective().max(0.0);
                format!(
                    "+{:.0}% to every rune's proc roll on hit.",
                    pct,
                )
            }
            StatKind::XpHarvest => {
                let total = 100.0 + stats.xp_harvest_pct.effective();
                format!(
                    "XP gained per kill — currently {:.0}% of baseline (1 XP per kill, 5 per boss).",
                    total,
                )
            }
            StatKind::RuneDamage => {
                let pct = (stats.rune_damage.effective() * 100.0).round() as i32;
                format!(
                    "Scales rune effects (Fire DoT, Shock chains, AOE radius). Currently {}%.",
                    pct,
                )
            }
            StatKind::TurretDamage => {
                let base_mult = stats.turret_damage_mult();
                let total_pct = (base_mult * 100.0).round() as i32;
                format!(
                    "Damage multiplier on every weapon. Currently {}% before Naval synergy.",
                    total_pct,
                )
            }
            // Stats whose static blurb already reads cleanly (the
            // raw value is visible in the readout column right next
            // to the description tooltip, so no need to fold it in).
            StatKind::Hp
            | StatKind::MoveSpeed
            | StatKind::TurnSpeed
            | StatKind::TurretTurnSpeed
            | StatKind::TurretArcBonus
            | StatKind::ShieldMax => self.description().to_string(),
        }
    }
    /// Step size for one click of the debug `+/-` button. Tuned
    /// per-stat so each click is a meaningful nudge in that stat's
    /// natural unit. Bigger than `upgrade_step` because debug
    /// testing wants to traverse the stat range quickly; player-
    /// facing pickups (level-up + shop mods) use the smaller step.
    pub fn debug_step(self) -> f32 {
        match self {
            StatKind::Hp => 10.0,
            StatKind::MoveSpeed => 3.0,
            StatKind::TurnSpeed => 0.5,           // rad/s
            StatKind::TurretTurnSpeed => 0.5,     // rad/s
            StatKind::TurretArcBonus => 10.0,     // degrees
            StatKind::Luck => 25.0,
            StatKind::ProcStrength => 10.0,
            StatKind::Crit => 25.0,
            StatKind::Range => 10.0,
            StatKind::Harvest => 1.0,
            StatKind::XpHarvest => 10.0,
            StatKind::ShieldMax => 5.0,
            StatKind::RuneDamage => 0.1,
            StatKind::TurretDamage => 10.0, // +10 percentage points / step
            StatKind::Dodge => 5.0,
            StatKind::Armour => 5.0,
        }
    }

    /// Player-facing per-pickup step size — used by level-up card
    /// rolls and as a baseline for shop mod authoring. Smaller than
    /// `debug_step` so a single level-up is a meaningful nudge
    /// rather than a build-defining swing. Tuning targets a build-
    /// up that takes ~10 picks per stat to feel significant.
    pub fn upgrade_step(self) -> f32 {
        match self {
            StatKind::Hp => 5.0,
            StatKind::MoveSpeed => 1.5,           // half of debug
            StatKind::TurnSpeed => 0.25,          // half of debug
            StatKind::TurretTurnSpeed => 0.25,    // unused in rolls
            StatKind::TurretArcBonus => 5.0,      // unused in rolls
            StatKind::Luck => 5.0,                // was 25
            StatKind::ProcStrength => 5.0,
            StatKind::Crit => 5.0,                // was 25 — crit was a build-finisher in one card
            StatKind::Range => 5.0,
            StatKind::Harvest => 1.0,             // already conservative
            StatKind::XpHarvest => 5.0,
            StatKind::ShieldMax => 3.0,
            StatKind::RuneDamage => 0.05,         // half of debug
            StatKind::TurretDamage => 5.0,
            StatKind::Dodge => 2.0,               // 30 picks to hit the 60% cap
            StatKind::Armour => 2.0,
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
            | StatKind::Harvest
            | StatKind::XpHarvest
            | StatKind::Dodge
            | StatKind::Armour => format!("{:+.0}%", delta),
            // Movement / turning are stored as raw world units but a
            // raw "+3" doesn't tell the player anything. Express as
            // a percentage of the baseline so "+3 SPEED" reads as
            // "+10% SPEED". Math stays additive on `.flat`; this is
            // a display-only conversion.
            StatKind::MoveSpeed => {
                format!("{:+.0}%", (delta / MOVE_SPEED_BASE) * 100.0)
            }
            StatKind::TurnSpeed => {
                format!("{:+.0}%", (delta / TURN_SPEED_BASE) * 100.0)
            }
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
            StatKind::XpHarvest => &stats.xp_harvest_pct,
            StatKind::ShieldMax => &stats.shield_max,
            StatKind::RuneDamage => &stats.rune_damage,
            StatKind::TurretDamage => &stats.turret_damage_pct,
            StatKind::Dodge => &stats.dodge_pct,
            StatKind::Armour => &stats.armour_pct,
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
            StatKind::XpHarvest => &mut stats.xp_harvest_pct,
            StatKind::ShieldMax => &mut stats.shield_max,
            StatKind::RuneDamage => &mut stats.rune_damage,
            StatKind::TurretDamage => &mut stats.turret_damage_pct,
            StatKind::Dodge => &mut stats.dodge_pct,
            StatKind::Armour => &mut stats.armour_pct,
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
