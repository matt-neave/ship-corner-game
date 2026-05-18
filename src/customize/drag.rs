//! Drag-and-drop for the customize overlay.
//!
//! Custom mouse picking — the customize UI lives on a low-res render
//! target (see `render.rs`), so the standard `bevy_ui` cursor →
//! interaction pipeline doesn't apply. Instead we:
//!
//! 1. Convert the window cursor → spec coords each frame via
//!    `CustomizeViewport::window_to_spec`.
//! 2. Test the spec cursor against every entity carrying a `HitArea` +
//!    `DragSourceMarker` / `DropTargetMarker`.
//!
//! State machine
//! -------------
//! - **Press**: hit-test → if the cursor is over a draggable, snapshot
//!   what's being dragged and spawn a `DragGhost` sprite parented under
//!   the customize render (so it's pixelated like everything else).
//! - **Hold**: every frame, move the ghost to the cursor's spec coord.
//! - **Release**: hit-test against drop targets, resolve via merge rules.
//!
//! Merge rules: identical to the previous bevy_ui-based version.

use bevy::input::mouse::MouseButton;
use bevy::prelude::*;
use bevy::render::view::RenderLayers;
use bevy::sprite::MeshMaterial2d;
use bevy::window::PrimaryWindow;
use rand::seq::SliceRandom;
use rand::Rng;

use crate::balance::CUSTOMIZE_LAYER;
use crate::rune::Rune;
use crate::stats::StatKind;
use crate::turret::{SlotCfg, TurretConfig};
use crate::weapon::WeaponType;

use super::render::CustomizeViewport;
use super::setup::{
    rune_color_for, turret_color_for, DragSourceMarker, DropTargetMarker, HitArea,
};
use super::CustomizeOpen;

// ---------- Drag state + payload ----------

#[derive(Resource, Default)]
pub struct DragState {
    pub picked: Option<Picked>,
    /// A press-and-hold candidate, waiting out the debounce window
    /// before becoming an active drag. A quick click → release inside
    /// `DRAG_HOLD_THRESHOLD` resolves as a click-buy with no ghost
    /// ever spawned, so the visual stays still for shop clicks.
    pub pending: Option<Pending>,
    pub ghost: Option<Entity>,
    /// Last known spec cursor — used by the ghost-follow system + hit-test.
    pub spec_cursor: Option<Vec2>,
}

#[derive(Clone)]
pub struct Picked {
    pub source: DragSourceKind,
    pub payload: Payload,
}

/// A draggable hit by the press but not yet promoted to an active
/// drag. `press_time` is `Time::elapsed_secs()` at the press; the
/// promoter checks the gap each frame and converts to `picked` once
/// the threshold passes.
#[derive(Clone)]
pub struct Pending {
    pub source: DragSourceKind,
    pub payload: Payload,
    pub press_time: f32,
}

/// How long a mouse-down must be held on a draggable before a drag
/// ghost actually appears. Below this window we treat the press as a
/// click-buy. Tuned so a deliberate click feels instant but a held
/// press starts the drag without noticeable latency.
pub const DRAG_HOLD_THRESHOLD: f32 = 0.15;

#[derive(Clone, Copy)]
pub enum Payload {
    Turret { weapon: WeaponType, barrels: u8 },
    Rune(Rune),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragSourceKind {
    ShipSlot(usize),
    ShipRune { slot: usize, rune_idx: usize },
    ShopTurret(usize),
    ShopRune(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropTargetKind {
    ShipSlot(usize),
    ShipRune { slot: usize, rune_idx: usize },
    /// Sell panel — drop a ship-sourced turret/rune here to refund a
    /// fraction of its original shop cost. Source ship-slot/socket
    /// is cleared; shop-sourced drops on this panel are no-ops.
    Sell,
}

#[derive(Component)]
pub struct DragGhost;

// ---------- Shop offerings ----------

/// Shop stock. Each slot is `Some(offer)` until the player drags it
/// out — at that point we set it to `None` so it can't be picked again.
/// Keeps slot indices stable across drags so the spawned UI tiles
/// (which carry hard-coded indices 0/1/2) keep pointing at the right
/// stock entry; visual updates render `None` slots as empty.
#[derive(Resource, Default, Clone)]
pub struct CustomizeShop {
    pub turrets: Vec<Option<ShopTurretOffer>>,
    pub runes: Vec<Option<Rune>>,
    /// Stat-modifier cards. Click-to-apply: the delta is added to the
    /// stat's `flat` field and the slot is consumed.
    pub mods: Vec<Option<ShopMod>>,
    /// Per-slot locks — right-click toggles the corresponding flag.
    /// Locked slots survive `reroll_preserving_locked` so the player
    /// can hold an interesting offer while rolling the rest. Fixed
    /// to the shop's authored slot count (3 / 2 / 3); enlarged with
    /// `false` if the lock array got out of sync with the Vec
    /// during a save-load.
    pub turrets_locked: [bool; 3],
    pub runes_locked:   [bool; 2],
    pub mods_locked:    [bool; 3],
}

impl CustomizeShop {
    /// Roll a fresh shop but keep any locked slot's offer in place.
    /// Lock flags themselves persist so the player's lock survives
    /// across rerolls.
    pub fn reroll_preserving_locked(&self) -> Self {
        let mut fresh = roll_fresh_stock();
        // Copy lock flags forward first so the visual badges don't
        // flicker through "unlocked → locked" on the same frame.
        fresh.turrets_locked = self.turrets_locked;
        fresh.runes_locked   = self.runes_locked;
        fresh.mods_locked    = self.mods_locked;
        // Carry over the offer in each locked slot. Skip slots
        // whose live offer was already consumed (`None`) — locking
        // an empty slot is a no-op rather than a guarantee of
        // future offers.
        for (i, &locked) in self.turrets_locked.iter().enumerate() {
            if locked {
                if let Some(existing) = self.turrets.get(i).copied().flatten() {
                    if let Some(slot) = fresh.turrets.get_mut(i) { *slot = Some(existing); }
                }
            }
        }
        for (i, &locked) in self.runes_locked.iter().enumerate() {
            if locked {
                if let Some(existing) = self.runes.get(i).copied().flatten() {
                    if let Some(slot) = fresh.runes.get_mut(i) { *slot = Some(existing); }
                }
            }
        }
        for (i, &locked) in self.mods_locked.iter().enumerate() {
            if locked {
                if let Some(existing) = self.mods.get(i).copied().flatten() {
                    if let Some(slot) = fresh.mods.get_mut(i) { *slot = Some(existing); }
                }
            }
        }
        fresh
    }
}

#[derive(Clone, Copy)]
pub struct ShopTurretOffer {
    pub weapon: WeaponType,
    pub barrels: u8,
}

/// Tier-style rarity for a mod. Drives the card's outline tint
/// so the player can see at a glance which slot is the high-tier
/// pull. Rarity is authored per-mod in [`MOD_LIBRARY`]; doesn't
/// (yet) affect roll weights — every entry rolls with equal
/// probability for now.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)]
pub enum ModRarity {
    /// White border — straight everyday upgrade.
    Common,
    /// Blue border — a touch better or rarer effect family.
    Uncommon,
    /// Purple border — strong but conditional / build-defining.
    Rare,
    /// Red border — top-tier impact or a wild trade-off.
    Legendary,
}

impl ModRarity {
    /// Outline colour used by the mod card. Pick palette values
    /// that pop on the dark `theme::SURFACE_RAISED` card fill and
    /// stay distinct from one another at small sizes.
    pub fn border_color(self) -> Color {
        match self {
            ModRarity::Common    => Color::srgb(0.92, 0.93, 0.96),
            ModRarity::Uncommon  => Color::srgb(0.40, 0.62, 0.95),
            ModRarity::Rare      => Color::srgb(0.78, 0.40, 0.95),
            ModRarity::Legendary => Color::srgb(0.95, 0.32, 0.32),
        }
    }

    /// Short label printed below the mod name on the card. Same
    /// tier hierarchy as the border colour so the player can
    /// cross-reference visually + textually.
    pub fn label(self) -> &'static str {
        match self {
            ModRarity::Common    => "COMMON",
            ModRarity::Uncommon  => "UNCOMMON",
            ModRarity::Rare      => "RARE",
            ModRarity::Legendary => "LEGENDARY",
        }
    }

    /// Per-pick roll weight. Weighted-random sampling without
    /// replacement: the shop draws 3 mods, each pick picks one
    /// from the remaining library proportional to these weights,
    /// then removes the picked entry so it can't duplicate.
    ///
    /// 60 / 25 / 12 / 3 — Diablo-style "uncommon is uncommon,
    /// legendary feels like a moment." Per-pick probabilities,
    /// not per-shop — the rarer-tier draw chance compounds when
    /// the player saves rerolls.
    pub fn weight(self) -> f32 {
        match self {
            ModRarity::Common    => 60.0,
            ModRarity::Uncommon  => 25.0,
            ModRarity::Rare      => 12.0,
            ModRarity::Legendary => 3.0,
        }
    }

    /// Scrap cost to purchase a mod of this rarity. Rarer cards
    /// cost more so legendaries feel earned even if you save
    /// scrap aggressively for the lucky roll.
    pub fn cost(self) -> u32 {
        match self {
            ModRarity::Common    => 2,
            ModRarity::Uncommon  => 3,
            ModRarity::Rare      => 4,
            ModRarity::Legendary => 6,
        }
    }
}

/// One canonical mod entry — a name + an arbitrary list of stat
/// changes + a rarity tier + an optional special effect. The shop
/// rolls indexes into [`MOD_LIBRARY`] so adding a new mod is one
/// struct literal at the bottom of that array.
pub struct ModSpec {
    pub name: &'static str,
    pub rarity: ModRarity,
    /// Each entry is `(stat, delta)`. Positive deltas are buffs,
    /// negative are nerfs; the card-text pass colours them green
    /// vs red automatically by sign.
    pub changes: &'static [(StatKind, f32)],
    /// Build-warping ability that goes beyond a plain stat delta.
    /// `Some` for legendaries that flip the ship's identity (the
    /// MONOMANIAC / PURIST / GHOST class) — `None` for plain stat
    /// stacks. Effects with ongoing conditional logic register a
    /// flag in [`ActiveLegendaries`] when the player buys the mod;
    /// one-shot effects (Turtle) mutate stats directly at the click.
    pub effect: Option<ModEffect>,
}

/// Build-warping ability that a legendary mod can carry on top of
/// (or instead of) its stat changes. Stored as a flag on
/// [`ActiveLegendaries`] for ongoing effects, or applied once at
/// purchase time for one-shot ones (Turtle).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModEffect {
    /// +400% weapon damage; -50% per equipped weapon beyond the
    /// first. Rewards mono-weapon focus.
    Monomaniac,
    /// +250% fire rate while ≤ 2 weapons are equipped. Hard cliff.
    Duelist,
    /// +15% weapon damage per unique weapon type equipped. Caps at 8.
    Harmony,
    /// +100% weapon damage, +100% fire rate, +20% range — but
    /// rune procs are disabled entirely. Pure-bullet pivot.
    Purist,
    /// +75% rune effect + +50% proc strength if every equipped
    /// weapon shares a tag. Synergy-pure builds only.
    Specialist,
    /// One-shot at purchase: convert the player's current
    /// `shield_max` into flat HP and zero shield_max out.
    Turtle,
}

/// Persistent flags for which legendary build-warpers the player
/// has bought this run. Read by [`sync_turret_config`] (damage /
/// fire rate / range folds) and the rune proc pipeline (Specialist
/// scaling, Purist disable). Reset on new run / MainMenu.
#[derive(Resource, Default, Clone, Copy)]
pub struct ActiveLegendaries {
    pub monomaniac: bool,
    pub duelist: bool,
    pub harmony: bool,
    pub purist: bool,
    pub specialist: bool,
}

/// Running tally of which mods the player has bought this run.
/// Duplicates are grouped: one entry per unique mod with its
/// stacked count, in the order the first copy was bought. Drives
/// the equipped-mods grid (one cell per unique mod + a count
/// badge for stacks > 1). Reset on new run / MainMenu alongside
/// [`ActiveLegendaries`].
#[derive(Resource, Default, Clone)]
pub struct PurchasedMods {
    /// `(spec_idx, count)` — indices into [`MOD_LIBRARY`].
    pub entries: Vec<(usize, u32)>,
}

impl PurchasedMods {
    /// Record one purchase of the mod at `spec_idx`. Bumps the
    /// existing entry's count, or appends a new entry at the end.
    pub fn push(&mut self, spec_idx: usize) {
        if let Some(entry) = self.entries.iter_mut().find(|(i, _)| *i == spec_idx) {
            entry.1 = entry.1.saturating_add(1);
        } else {
            self.entries.push((spec_idx, 1));
        }
    }
}

/// Bundled `SystemParam` for the two resources every legendary-aware
/// hook needs (`ActiveLegendaries` + `TurretConfig`). Used by
/// `bullet::process_damage_events` to stay under Bevy 0.16's
/// 16-SystemParam ceiling — adding the resources individually
/// would push it over.
#[derive(bevy::ecs::system::SystemParam)]
pub struct LegendaryContext<'w> {
    pub active: Res<'w, ActiveLegendaries>,
    pub cfg: Res<'w, TurretConfig>,
}

impl ActiveLegendaries {
    /// Multiplier folded into every weapon's base damage in
    /// `sync_turret_config`. Combines Monomaniac (focused build),
    /// Harmony (variety bonus), and any flat damage from Purist —
    /// stacks multiplicatively so two effects compound. Returns
    /// 1.0 when no relevant legendary is active.
    pub fn weapon_damage_mult(&self, cfg: &TurretConfig) -> f32 {
        let mut mult = 1.0_f32;
        if self.monomaniac {
            // 1 wpn = ×5.0 (base 1.0 + bonus 4.0), 2 wpns = ×4.5,
            // 8 wpns = ×1.5. Math: 1.0 + 4.0 - 0.5 * (count - 1).
            let count = cfg.slots.iter().filter(|s| s.equipped).count() as f32;
            let bonus = (4.0 - 0.5 * (count - 1.0).max(0.0)).max(0.0);
            mult *= 1.0 + bonus;
        }
        if self.harmony {
            // +15% per unique weapon type equipped. Caps naturally
            // at 8 (the slot count).
            let mut seen = [false; 32];
            let mut uniq = 0u32;
            for s in cfg.slots.iter().filter(|s| s.equipped) {
                let i = s.weapon as usize;
                if i < seen.len() && !seen[i] {
                    seen[i] = true;
                    uniq += 1;
                }
            }
            mult *= 1.0 + 0.15 * uniq as f32;
        }
        mult
    }

    /// Fire-rate multiplier folded into `slot.fire_rate` in
    /// `sync_turret_config`. Duelist contributes a hard cliff at
    /// ≤2 equipped weapons; Purist adds a flat doubling on top.
    pub fn fire_rate_mult(&self, cfg: &TurretConfig) -> f32 {
        let mut mult = 1.0_f32;
        if self.duelist {
            let count = cfg.slots.iter().filter(|s| s.equipped).count();
            if count <= 2 {
                mult *= 3.5; // +250% fire rate
            }
        }
        if self.purist {
            mult *= 2.0; // +100% fire rate (= -50% cooldown)
        }
        mult
    }

    /// True when every equipped weapon shares at least one tag with
    /// every other — the Specialist precondition. Empty / single-
    /// weapon loadouts trivially qualify.
    pub fn specialist_armed(&self, cfg: &TurretConfig) -> bool {
        if !self.specialist { return false; }
        let mut equipped = cfg.slots.iter().filter(|s| s.equipped);
        let Some(first) = equipped.next() else { return true; };
        // Use the first weapon's tag intersection as the seed; if
        // it ever shrinks to empty, the build isn't pure.
        let mut common: Vec<crate::weapon::WeaponTag> = first.weapon.tags().to_vec();
        for s in equipped {
            common.retain(|t| s.weapon.tags().contains(t));
            if common.is_empty() { return false; }
        }
        true
    }
}

impl ModEffect {
    /// One-line summary for the card body. Appended after the
    /// `name` + stat-change lines so the player sees both the
    /// numeric changes (if any) and the build-warping rule.
    pub fn description(self) -> &'static str {
        match self {
            ModEffect::Monomaniac => "+400% DMG\n-50% PER WPN",
            ModEffect::Duelist    => "+250% RATE\nIF <= 2 WPN",
            ModEffect::Harmony    => "+15% DMG\nPER UNIQ WPN",
            ModEffect::Purist     => "RUNES OFF",
            ModEffect::Specialist => "+75% RUNES\nSAME TAG ONLY",
            ModEffect::Turtle     => "SHIELD -> HP",
        }
    }

    /// Verbose multi-sentence tooltip body. Read by the customize
    /// tooltip when the cursor hovers a mod card — the small card
    /// label is too narrow to spell out what the build-warping
    /// rule actually does, so the tooltip carries the full
    /// explanation in human-readable prose.
    pub fn tooltip_body(self) -> &'static str {
        match self {
            ModEffect::Monomaniac =>
                "All weapons gain +400% damage when you have one equipped. Each additional weapon shaves 50% off that bonus, so two weapons = +350%, three = +300%, eight = +50%. Rewards going all-in on a single weapon type.",
            ModEffect::Duelist =>
                "+250% fire rate while you have 2 or fewer weapons equipped. Hard cliff: equipping a third weapon removes the bonus entirely. Pairs well with high single-shot damage.",
            ModEffect::Harmony =>
                "+15% weapon damage for every UNIQUE weapon type equipped. Caps at 8 types. Two of the same weapon only count once. Reward for filling the boat with variety; punishes stacking duplicates.",
            ModEffect::Purist =>
                "Weapons deal +100% damage, fire +100% faster, and have +20% range. BUT all rune procs (Fire, Frost, Shock, Bleed, Blast, Cascade, etc.) are disabled entirely. Bullets become the only source of damage.",
            ModEffect::Specialist =>
                "+75% rune effect AND +50% proc strength, but only if EVERY equipped weapon shares the same tag (Pirate, Naval, Future, Support, etc.). Even one off-tag weapon disables the bonus. Synergy-pure builds only.",
            ModEffect::Turtle =>
                "On purchase: converts your entire current shield_max into permanent flat HP, then zeroes your shield_max out. One-time, irreversible. Future shield pickups still grant new shield from zero.",
        }
    }
}

/// All shop mods. Pure-buff entries have a single `+` change;
/// trade-off entries pair a buff with a nerf. Add a new mod by
/// appending to this list — the shop roll, the click handler, and
/// the card label all read straight from here.
///
/// Number scale: pure mods are ≈ `StatKind::upgrade_step` (one
/// level-up's worth). Trade-off mods buff at 2× upgrade_step AND
/// nerf at 2× upgrade_step — net zero on paper, so the trade-off
/// is the favoured pick only when the side stat is genuinely
/// dump-worthy for your build (Brotato pattern). Crit is
/// intentionally small (5%) — a build-defining stat shouldn't
/// swing from one card.
pub static MOD_LIBRARY: &[ModSpec] = &[
    // ---- Pure-buff mods (mostly Common, a few Uncommon for the
    //      stats that snowball — luck / runes / harvest) ----
    ModSpec { name: "RELOADED",   rarity: ModRarity::Common,   changes: &[(StatKind::TurretDamage, 5.0)], effect: None },
    ModSpec { name: "FOCUS",      rarity: ModRarity::Common,   changes: &[(StatKind::Crit, 5.0)], effect: None },
    ModSpec { name: "OUTRIDER",   rarity: ModRarity::Common,   changes: &[(StatKind::MoveSpeed, 1.5)], effect: None },
    ModSpec { name: "HOMING",     rarity: ModRarity::Common,   changes: &[(StatKind::Range, 5.0)], effect: None },
    ModSpec { name: "SCOUT",      rarity: ModRarity::Common,   changes: &[(StatKind::TurnSpeed, 0.25)], effect: None },
    ModSpec { name: "PLATING",    rarity: ModRarity::Common,   changes: &[(StatKind::Hp, 5.0)], effect: None },
    ModSpec { name: "PADDING",    rarity: ModRarity::Common,   changes: &[(StatKind::Armour, 3.0)], effect: None },
    ModSpec { name: "DODGER",     rarity: ModRarity::Common,   changes: &[(StatKind::Dodge, 3.0)], effect: None },
    ModSpec { name: "BARRIER",    rarity: ModRarity::Common,   changes: &[(StatKind::ShieldMax, 5.0)], effect: None },
    ModSpec { name: "FIELD KIT",  rarity: ModRarity::Uncommon, changes: &[(StatKind::Harvest, 1.0)], effect: None },
    ModSpec { name: "TUTOR",      rarity: ModRarity::Common,   changes: &[(StatKind::XpHarvest, 3.0)], effect: None },
    ModSpec { name: "ENERGISED",  rarity: ModRarity::Uncommon, changes: &[(StatKind::RuneDamage, 0.05)], effect: None },
    ModSpec { name: "PYRO",       rarity: ModRarity::Common,   changes: &[(StatKind::ProcStrength, 5.0)], effect: None },
    ModSpec { name: "GAMBLER",    rarity: ModRarity::Uncommon, changes: &[(StatKind::Luck, 5.0)], effect: None },

    // ---- Trade-off mods — 2× buff / 2× nerf (zero-sum on paper) ----
    //      All Rare since they're build-defining picks.
    ModSpec { name: "GLASS CANNON", rarity: ModRarity::Rare,
        changes: &[(StatKind::TurretDamage, 10.0), (StatKind::Hp, -10.0)], effect: None },
    ModSpec { name: "STEADY AIM", rarity: ModRarity::Rare,
        changes: &[(StatKind::Crit, 10.0), (StatKind::MoveSpeed, -3.0)], effect: None },
    ModSpec { name: "BERSERKER", rarity: ModRarity::Rare,
        changes: &[(StatKind::TurretDamage, 10.0), (StatKind::Armour, -6.0)], effect: None },
    ModSpec { name: "EVASIVE", rarity: ModRarity::Rare,
        changes: &[(StatKind::Dodge, 6.0), (StatKind::TurretDamage, -10.0)], effect: None },
    ModSpec { name: "JUGGERNAUT", rarity: ModRarity::Rare,
        changes: &[(StatKind::Hp, 10.0), (StatKind::MoveSpeed, -3.0)], effect: None },
    ModSpec { name: "MERCHANT", rarity: ModRarity::Rare,
        changes: &[(StatKind::Harvest, 2.0), (StatKind::TurretDamage, -10.0)], effect: None },
    ModSpec { name: "FAR SHOT", rarity: ModRarity::Rare,
        changes: &[(StatKind::Range, 10.0), (StatKind::ProcStrength, -10.0)], effect: None },

    // ---- Legendary mods — multi-stat picks that define a build,
    //      and pure trade-offs at heroic numbers. Roll weight is
    //      3 per entry vs commons' 60, so any legendary is roughly
    //      a "one shop in twenty" moment.
    ModSpec { name: "OVERCLOCK", rarity: ModRarity::Legendary,
        changes: &[
            (StatKind::TurretDamage, 15.0),
            (StatKind::Crit, 10.0),
            (StatKind::Range, 10.0),
        ], effect: None },
    ModSpec { name: "BULWARK", rarity: ModRarity::Legendary,
        changes: &[
            (StatKind::Hp, 15.0),
            (StatKind::Armour, 5.0),
            (StatKind::ShieldMax, 10.0),
        ], effect: None },
    ModSpec { name: "WARPRIEST", rarity: ModRarity::Legendary,
        changes: &[
            (StatKind::RuneDamage, 0.15),
            (StatKind::ProcStrength, 10.0),
            (StatKind::Luck, 10.0),
        ], effect: None },
    ModSpec { name: "APEX HUNTER", rarity: ModRarity::Legendary,
        changes: &[
            (StatKind::MoveSpeed, 3.0),
            (StatKind::TurretDamage, 10.0),
            (StatKind::Crit, 10.0),
        ], effect: None },
    ModSpec { name: "DEATH WISH", rarity: ModRarity::Legendary,
        changes: &[
            (StatKind::TurretDamage, 30.0),
            (StatKind::Hp, -30.0),
        ], effect: None },
    ModSpec { name: "REGEN", rarity: ModRarity::Legendary,
        changes: &[
            (StatKind::Hp, 15.0),
            (StatKind::ShieldMax, 10.0),
            (StatKind::TurretDamage, -10.0),
        ], effect: None },

    // ---- Build-warping legendaries — these change *how the boat
    //      plays*, not just stat numbers. Each one rewards (or
    //      punishes) a specific loadout commitment.
    ModSpec { name: "MONOMANIAC", rarity: ModRarity::Legendary,
        changes: &[], effect: Some(ModEffect::Monomaniac) },
    ModSpec { name: "DUELIST", rarity: ModRarity::Legendary,
        changes: &[], effect: Some(ModEffect::Duelist) },
    ModSpec { name: "HARMONY", rarity: ModRarity::Legendary,
        changes: &[], effect: Some(ModEffect::Harmony) },
    ModSpec { name: "PURIST", rarity: ModRarity::Legendary,
        // Flat statline AND a build rule (no rune procs). Numbers
        // applied via `changes`; the disable enforced by the effect
        // flag in `ActiveLegendaries`.
        changes: &[
            (StatKind::TurretDamage, 100.0),
            (StatKind::Range, 20.0),
        ], effect: Some(ModEffect::Purist) },
    ModSpec { name: "SPECIALIST", rarity: ModRarity::Legendary,
        changes: &[], effect: Some(ModEffect::Specialist) },
    ModSpec { name: "GHOST", rarity: ModRarity::Legendary,
        // -999 HP clamps to 1 via `max_hp().max(1)`. Damage +300%,
        // dodge +30 — encourages pure-evade play.
        changes: &[
            (StatKind::Hp, -999.0),
            (StatKind::TurretDamage, 300.0),
            (StatKind::Dodge, 30.0),
        ], effect: None },
    ModSpec { name: "TURTLE", rarity: ModRarity::Legendary,
        // One-shot stat conversion at purchase (see the click
        // handler) — no changes, no ongoing flag.
        changes: &[], effect: Some(ModEffect::Turtle) },
];

/// Live shop mod entry — just an index into [`MOD_LIBRARY`]. Kept
/// `Copy` so the existing `Vec<Option<ShopMod>>` slot model works
/// unchanged. The card text, click apply, and tooltip all dispatch
/// through `spec()`.
#[derive(Clone, Copy)]
pub struct ShopMod {
    pub spec_idx: usize,
}

impl ShopMod {
    pub fn spec(&self) -> &'static ModSpec {
        &MOD_LIBRARY[self.spec_idx.min(MOD_LIBRARY.len().saturating_sub(1))]
    }

    /// Card text — name on the first line, a blank spacer line,
    /// then one `±N STAT` line per change. The blank line breaks
    /// the name visually from the change list so the card reads as
    /// "header / body" rather than four tight rows. The colour
    /// pass on the card paints buffs green / nerfs red automatically
    /// from each line's leading character.
    pub fn label(self) -> String {
        let spec = self.spec();
        let mut lines = String::from(spec.name);
        // Rarity tag — sits directly under the name in the same
        // tier-tinted colour as the card border (handled by the
        // card renderer's per-line colourisation). Reads as a
        // sub-header so the player can clock the rarity without
        // pattern-matching the border colour.
        lines.push('\n');
        lines.push_str(spec.rarity.label());
        // Spacer line — the renderer splits on `\n` and emits a
        // blank TextSpan for the empty entry, which displays as a
        // small visible gap below the rarity tag.
        lines.push('\n');
        for &(kind, delta) in spec.changes {
            lines.push('\n');
            lines.push_str(&format!(
                "{} {}",
                kind.format_delta(delta),
                short_stat_label(kind),
            ));
        }
        // Build-warping effect text. Append after the stat lines so
        // the card reads "name / stats / rule". Lines come from
        // `ModEffect::description` and may themselves contain `\n`
        // for multi-line summaries.
        if let Some(eff) = spec.effect {
            lines.push('\n');
            for line in eff.description().split('\n') {
                lines.push('\n');
                lines.push_str(line);
            }
        }
        lines
    }
}

/// Verbose multi-line tooltip body for a shop mod. Composed of:
///   - One line per stat change, in human-readable form
///     (`+10% Weapon Damage`, `-3 Move Speed`).
///   - If the mod carries a build-warping effect, a blank spacer
///     then the effect's full prose explanation.
///   - If the mod has no changes AND no effect, a single
///     "no-op" line so the tooltip body isn't empty.
///
/// Lives next to `ModSpec` so adding a new mod naturally extends
/// this helper through `format_delta` + `StatKind::label`.
pub fn mod_tooltip_body(spec: &ModSpec) -> String {
    let mut out = String::new();
    for &(kind, delta) in spec.changes {
        if !out.is_empty() { out.push('\n'); }
        out.push_str(&format!("{} {}", kind.format_delta(delta), kind.label()));
    }
    if let Some(eff) = spec.effect {
        if !out.is_empty() { out.push('\n'); out.push('\n'); }
        out.push_str(eff.tooltip_body());
    }
    if out.is_empty() {
        out.push_str("No effect.");
    }
    out
}

/// Compact card-friendly label for a stat. The stats panel uses
/// `StatKind::label` for the full form; this trims the longer
/// names down so each `±N% LABEL` line fits the mod card's
/// narrow width without wrapping or overflowing.
///
/// Target length: <= 8 characters so a "+10% LABEL" line stays
/// inside a single card-width line. Anything longer was clipping
/// across into the neighbour card at the typical play resolution.
fn short_stat_label(kind: StatKind) -> &'static str {
    match kind {
        StatKind::Hp                => "HP",
        StatKind::ShieldMax         => "SHIELD",
        StatKind::MoveSpeed         => "SPEED",
        StatKind::TurnSpeed         => "TURN",
        StatKind::TurretTurnSpeed   => "T.TURN",
        StatKind::TurretArcBonus    => "T.ARC",
        StatKind::Range             => "RANGE",
        StatKind::Crit              => "CRIT",
        StatKind::Luck              => "LUCK",
        StatKind::ProcStrength      => "PROCS",
        StatKind::Harvest           => "HARVEST",
        StatKind::XpHarvest         => "XP",
        StatKind::RuneDamage        => "RUNES",
        StatKind::TurretDamage      => "DAMAGE",
        StatKind::Dodge             => "DODGE",
        StatKind::Armour            => "ARMOUR",
        StatKind::Cooldown          => "CDR",
        StatKind::ChestChance       => "CHESTS",
    }
}

/// Scrap cost to re-roll the shop. Refills every slot — sold or not.
pub const SHOP_REROLL_COST: u32 = 1;
/// Scrap cost for a T1 (single-barrel) turret. T2/T3 priced via
/// [`turret_cost_for_barrels`].
pub const SHOP_TURRET_COST: u32 = 2;
/// Scrap cost for a rune purchase.
pub const SHOP_RUNE_COST: u32 = 2;
/// Sell refund fraction — selling returns this share of the
/// original purchase cost (rounded down). `0.5` → 50%: a 4-scrap
/// turret refunds 2; a 2-scrap rune refunds 1.
pub const SHOP_SELL_FRACTION: f32 = 0.5;

/// Per-tier shop turret cost. Barrels = 1 → 2 scrap (vanilla T1);
/// 2 → 5 (T2); 3 → 12 (T3). The shop can roll a higher-tier turret
/// directly, skipping the merge grind for a steep premium.
pub fn turret_cost_for_barrels(barrels: u8) -> u32 {
    match barrels.max(1) {
        1 => 2,
        2 => 5,
        _ => 12,
    }
}

/// Roll a shop turret tier. 89% T1 / 10% T2 / 1% T3. Per-slot,
/// independent — each of the three shop slots rolls separately,
/// so seeing a T2 or T3 is a moment.
fn roll_turret_tier(rng: &mut impl rand::Rng) -> u8 {
    let r = rng.gen::<f32>();
    if r < 0.01 { 3 } else if r < 0.11 { 2 } else { 1 }
}

/// Roll a fresh set of offerings. Used by both the startup init and the
/// runtime reroll button. Always returns a fully-stocked shop (every
/// slot Some(...)), so a reroll restocks anything the player bought.
/// Weighted-random sampling without replacement against
/// [`MOD_LIBRARY`]. Picks one entry from `pool` proportional to
/// each entry's `rarity.weight()`, removes it from the pool, and
/// returns the picked library index. Used by the shop reroll +
/// initial roll so the three slots draw distinct mods with rarity
/// frequencies that match the per-tier weight table.
/// Roll one mod from the full library using the per-rarity weight
/// table. Used by chests to surface a single random pick (the shop
/// uses a 3-pick variant with replacement disabled — see
/// `weighted_pick_without_replacement`).
pub fn roll_one_mod() -> ShopMod {
    let mut rng = rand::thread_rng();
    let mut pool: Vec<usize> = (0..MOD_LIBRARY.len()).collect();
    let idx = weighted_pick_without_replacement(&mut rng, &mut pool)
        .unwrap_or(0);
    ShopMod { spec_idx: idx }
}

fn weighted_pick_without_replacement(
    rng: &mut impl rand::Rng,
    pool: &mut Vec<usize>,
) -> Option<usize> {
    if pool.is_empty() { return None; }
    let total: f32 = pool.iter().map(|&i| MOD_LIBRARY[i].rarity.weight()).sum();
    if total <= 0.0 { return pool.pop(); }
    let mut roll = rng.gen_range(0.0..total);
    for (pos, &idx) in pool.iter().enumerate() {
        let w = MOD_LIBRARY[idx].rarity.weight();
        if roll < w {
            pool.remove(pos);
            return Some(idx);
        }
        roll -= w;
    }
    // Numerical edge — `roll` exhausted the loop. Just take the
    // last entry so the shop slot still gets a card.
    pool.pop()
}

pub fn roll_fresh_stock() -> CustomizeShop {
    let mut rng = rand::thread_rng();
    let weapons = [
        WeaponType::Standard,
        WeaponType::Sniper,
        WeaponType::MachineGun,
        WeaponType::Shotgun,
        WeaponType::Railgun,
        WeaponType::Mortar,
        WeaponType::HeliPad,
        WeaponType::Cannon,
        WeaponType::Booster,
        WeaponType::Blade,
        WeaponType::Cage,
        WeaponType::Harpoon,
        WeaponType::SpreadRockets,
        WeaponType::Flamethrower,
        WeaponType::SpikedPlate,
        WeaponType::Amplifier,
        WeaponType::SharkNet,
        WeaponType::AnchorFlail,
        WeaponType::PlasmaTorpedo,
        WeaponType::CrowsNest,
    ];
    let runes_pool = [
        Rune::Fire,
        Rune::Frost,
        Rune::Shock,
        Rune::Echo,
        Rune::Cascade,
        Rune::Conduit,
        Rune::Resonate,
        Rune::Vampire,
        Rune::Ward,
        Rune::Bleed,
        Rune::Blast,
        Rune::Hustle,
        Rune::Pierce,
        Rune::Greed,
        Rune::Executioner,
        Rune::Opener,
        Rune::Leftovers,
        Rune::Star,
        Rune::Thirst,
        Rune::Medic,
        Rune::Rally,
        Rune::Thorns,
        Rune::TargetFurthest,
        Rune::TargetHighestHp,
        Rune::TargetLowestHp,
        Rune::TargetCarousel,
        Rune::Splash,
    ];
    let mut turrets = Vec::with_capacity(3);
    for _ in 0..3 {
        let w = *weapons.choose(&mut rng).unwrap();
        let barrels = roll_turret_tier(&mut rng);
        turrets.push(Some(ShopTurretOffer { weapon: w, barrels }));
    }
    let mut runes_owned: Vec<_> = runes_pool.to_vec();
    runes_owned.shuffle(&mut rng);
    let runes = runes_owned.into_iter().take(2).map(Some).collect();
    // Three distinct mods per shop offering — weighted sampling
    // without replacement so the same card never appears twice in
    // one row, AND rarer tiers stay rare. See
    // `weighted_pick_without_replacement` for the per-pick logic.
    let mut pool: Vec<usize> = (0..MOD_LIBRARY.len()).collect();
    let mut mods: Vec<Option<ShopMod>> = Vec::with_capacity(3);
    for _ in 0..3 {
        if let Some(idx) = weighted_pick_without_replacement(&mut rng, &mut pool) {
            mods.push(Some(ShopMod { spec_idx: idx }));
        }
    }
    let _ = StatKind::ROLLABLE; // retained for legacy use elsewhere.
    CustomizeShop {
        turrets, runes, mods,
        turrets_locked: [false; 3],
        runes_locked:   [false; 2],
        mods_locked:    [false; 3],
    }
}

pub fn init_customize_shop(
    mut commands: Commands,
    existing: Option<Res<CustomizeShop>>,
    mut peek: ResMut<super::MapPeek>,
) {
    // Return-from-peek path: the player popped over to the map view
    // and came back. Don't reroll — the shop they were looking at
    // is the same shop they wanted to be looking at. Just clear the
    // flag and keep `existing` as-is (the resource is already in
    // the World, so emitting nothing is fine).
    if peek.active {
        peek.active = false;
        return;
    }
    // Cold start (Startup, no resource yet) rolls a fresh shop.
    // Subsequent entries (OnEnter(Customize) between stages) re-roll
    // only the unlocked slots so the player's locked picks survive
    // across the whole run — locks reset only on a full game restart.
    let next = match existing.as_deref() {
        Some(shop) => shop.reroll_preserving_locked(),
        None => roll_fresh_stock(),
    };
    commands.insert_resource(next);
}

// ---------- Cursor tracking ----------

/// Refresh `DragState.spec_cursor` from the window cursor + the live
/// viewport mapping. Runs first in the drag chain so every other system
/// reads the same value within a frame.
pub fn track_customize_cursor(
    open: Res<CustomizeOpen>,
    viewport: Res<CustomizeViewport>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut drag: ResMut<DragState>,
) {
    if !open.open {
        drag.spec_cursor = None;
        return;
    }
    let cursor = windows.single().ok().and_then(|w| w.cursor_position());
    drag.spec_cursor = cursor.and_then(|c| viewport.window_to_spec(c));
}

// ---------- Press → start drag ----------

pub fn start_drag(
    open: Res<CustomizeOpen>,
    cfg: Res<TurretConfig>,
    shop_opt: Option<Res<CustomizeShop>>,
    mouse: Res<ButtonInput<MouseButton>>,
    time: Res<Time>,
    mut drag: ResMut<DragState>,
    sources: Query<(&Transform, &HitArea, &DragSourceMarker)>,
) {
    if !open.open || drag.picked.is_some() || drag.pending.is_some() {
        return;
    }
    if !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Some(cursor) = drag.spec_cursor else { return };
    let Some(shop) = shop_opt else { return };

    // Find the smallest hit-area containing the cursor — when sockets
    // overlap larger panels visually, the more-specific source wins.
    let mut best: Option<(f32, DragSourceKind)> = None;
    for (tf, hit, marker) in &sources {
        if !hit_test(cursor, tf.translation.truncate(), hit.size) {
            continue;
        }
        let area = hit.size.x * hit.size.y;
        if best.map_or(true, |(a, _)| area < a) {
            best = Some((area, marker.0));
        }
    }
    let Some((_, source)) = best else { return };

    // Stash as a *pending* drag rather than spawning the ghost
    // immediately. `promote_pending_drag` upgrades it to an active
    // drag after `DRAG_HOLD_THRESHOLD`; if the player releases first
    // (a click rather than a hold), `complete_drag` resolves the
    // pending as a click-buy with no ghost ever visible.
    let Some(payload) = payload_for(source, &cfg, &shop) else { return };
    drag.pending = Some(Pending {
        source,
        payload,
        press_time: time.elapsed_secs(),
    });
}

/// Promote a held-down pending drag to an active drag once the
/// debounce window elapses. Spawns the ghost only at this point, so
/// a quick click → release before the threshold never causes the
/// drag animation to flash.
pub fn promote_pending_drag(
    mut commands: Commands,
    time: Res<Time>,
    mut drag: ResMut<DragState>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    if drag.picked.is_some() { return; }
    let Some(pending) = drag.pending.as_ref() else { return };
    if time.elapsed_secs() - pending.press_time < DRAG_HOLD_THRESHOLD {
        return;
    }
    let Some(cursor) = drag.spec_cursor else { return };
    let source = pending.source;
    let payload = pending.payload;
    drag.pending = None;
    drag.picked = Some(Picked { source, payload });
    let ghost = spawn_ghost(&mut commands, &mut meshes, &mut materials, payload, cursor);
    drag.ghost = Some(ghost);
}

/// Place a shop offering into the first empty target slot/socket.
/// Returns false silently when there's no room or the player can't
/// afford it — clicks that don't apply leave both the shop slot and
/// the scrap counter untouched. Cost is deducted only on a successful
/// placement.
fn click_buy_shop(
    source: DragSourceKind,
    cfg: &mut TurretConfig,
    shop: &mut CustomizeShop,
    scrap: &mut crate::Scrap,
    hull: crate::hull::Hull,
) -> bool {
    // Cost depends on what the player is buying — turret cost scales
    // with tier (barrels), rune is flat.
    let cost = match source {
        DragSourceKind::ShopTurret(idx) => {
            let Some(offer) = shop.turrets.get(idx).copied().flatten() else { return false };
            // Hull tag-lock check happens early so the cost isn't
            // even sampled when the buy would be rejected anyway.
            if !hull.allows_weapon(offer.weapon) { return false; }
            turret_cost_for_barrels(offer.barrels)
        }
        DragSourceKind::ShopRune(_)   => SHOP_RUNE_COST,
        _ => return false,
    };
    if scrap.0 < cost { return false; }
    let placed = try_place_shop_item(source, cfg, shop, hull);
    if placed {
        scrap.0 = scrap.0.saturating_sub(cost);
    }
    placed
}

fn try_place_shop_item(
    source: DragSourceKind,
    cfg: &mut TurretConfig,
    shop: &mut CustomizeShop,
    hull: crate::hull::Hull,
) -> bool {
    match source {
        DragSourceKind::ShopTurret(idx) => {
            let Some(offering) = shop.turrets.get(idx).and_then(|o| *o) else { return false };
            if !hull.allows_weapon(offering.weapon) { return false; }
            let cap = hull.turret_slot_cap().min(cfg.slots.len());
            let Some(slot_i) = (0..cap).find(|&i| !cfg.slots[i].equipped) else {
                return false;
            };
            cfg.slots[slot_i] = SlotCfg {
                equipped: true,
                weapon: offering.weapon,
                damage: offering.weapon.defaults().0,
                fire_rate: offering.weapon.defaults().1,
                barrels: offering.barrels,
                runes: [None; 3],
            };
            if let Some(slot) = shop.turrets.get_mut(idx) { *slot = None; }
            if let Some(flag) = shop.turrets_locked.get_mut(idx) { *flag = false; }
            true
        }
        DragSourceKind::ShopRune(idx) => {
            let Some(rune) = shop.runes.get(idx).and_then(|o| *o) else { return false };
            for slot_i in 0..cfg.slots.len() {
                if !cfg.slots[slot_i].equipped { continue; }
                for r in 0..3 {
                    if cfg.slots[slot_i].runes[r].is_none() {
                        cfg.slots[slot_i].runes[r] = Some(rune);
                        if let Some(slot) = shop.runes.get_mut(idx) { *slot = None; }
                        if let Some(flag) = shop.runes_locked.get_mut(idx) { *flag = false; }
                        return true;
                    }
                }
            }
            false
        }
        _ => false,
    }
}

fn hit_test(cursor: Vec2, centre: Vec2, size: Vec2) -> bool {
    let half = size * 0.5;
    let min = centre - half;
    let max = centre + half;
    cursor.x >= min.x && cursor.x <= max.x && cursor.y >= min.y && cursor.y <= max.y
}

fn payload_for(
    source: DragSourceKind,
    cfg: &TurretConfig,
    shop: &CustomizeShop,
) -> Option<Payload> {
    match source {
        DragSourceKind::ShipSlot(slot) => {
            let s = cfg.slots[slot];
            if !s.equipped {
                return None;
            }
            Some(Payload::Turret {
                weapon: s.weapon,
                barrels: s.barrels.max(1),
            })
        }
        DragSourceKind::ShipRune { slot, rune_idx } => {
            let s = cfg.slots[slot];
            if !s.equipped {
                return None;
            }
            s.runes[rune_idx].map(Payload::Rune)
        }
        DragSourceKind::ShopTurret(idx) => {
            shop.turrets.get(idx).and_then(|o| o.as_ref()).map(|o| Payload::Turret {
                weapon: o.weapon,
                barrels: o.barrels,
            })
        }
        DragSourceKind::ShopRune(idx) => shop
            .runes
            .get(idx)
            .and_then(|o| o.as_ref())
            .copied()
            .map(Payload::Rune),
    }
}

// ---------- Ghost (follows cursor in spec coords) ----------

fn spawn_ghost(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ColorMaterial>,
    payload: Payload,
    cursor: Vec2,
) -> Entity {
    match payload {
        Payload::Turret { weapon, barrels } => spawn_ghost_turret(commands, meshes, materials, weapon, barrels, cursor),
        Payload::Rune(r) => spawn_ghost_rune(commands, meshes, materials, r, cursor),
    }
}

fn spawn_ghost_turret(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ColorMaterial>,
    weapon: WeaponType,
    barrels: u8,
    cursor: Vec2,
) -> Entity {
    // Mirror the in-game turret: a single coloured Circle. Barrel level
    // is conveyed by a number on the actual slot tiles (and the source
    // visual hides during drag), so the ghost stays minimal.
    let body_mat = materials.add(turret_color_for(weapon));
    let body = meshes.add(Circle::new(6.0));
    let _ = barrels; // level isn't drawn on the ghost itself
    commands.spawn((
        Mesh2d(body),
        MeshMaterial2d(body_mat),
        Transform::from_translation(cursor.extend(9.0)),
        RenderLayers::layer(CUSTOMIZE_LAYER),
        DragGhost,
    )).id()
}

fn spawn_ghost_rune(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ColorMaterial>,
    rune: Rune,
    cursor: Vec2,
) -> Entity {
    // Mirror the in-socket rune visual — a rounded-square built from
    // 4 corner circles + a horizontal rect + a vertical rect, all the
    // same colour. The previous ghost drew a flat 8×8 square, which
    // looked nothing like the actual rune mid-drag. We spawn an empty
    // root entity at the cursor, then parent each shape part under it
    // with local offsets — `update_drag_ghost` moves only the root,
    // and the children follow via Bevy's transform propagation.
    const SOCKET: f32 = 8.0;
    const SOCKET_RADIUS: f32 = 3.0;
    let mat = materials.add(rune_color_for(rune));
    let circle = meshes.add(Circle::new(SOCKET_RADIUS));
    let h_rect = meshes.add(Rectangle::new(SOCKET, SOCKET - 2.0 * SOCKET_RADIUS));
    let v_rect = meshes.add(Rectangle::new(SOCKET - 2.0 * SOCKET_RADIUS, SOCKET));
    let root = commands.spawn((
        Transform::from_translation(cursor.extend(9.0)),
        Visibility::default(),
        RenderLayers::layer(CUSTOMIZE_LAYER),
        DragGhost,
    )).id();
    // Body rects (cross shape) — local (0,0) relative to root.
    for mesh in [Mesh2d(h_rect), Mesh2d(v_rect)] {
        let part = commands.spawn((
            mesh,
            MeshMaterial2d(mat.clone()),
            Transform::from_xyz(0.0, 0.0, 0.0),
            RenderLayers::layer(CUSTOMIZE_LAYER),
        )).id();
        commands.entity(part).insert(ChildOf(root));
    }
    // Corner circles — offset diagonally to round the cross's corners.
    let half = (SOCKET - 2.0 * SOCKET_RADIUS) * 0.5;
    for offset in [
        Vec2::new(-half, -half),
        Vec2::new( half, -half),
        Vec2::new(-half,  half),
        Vec2::new( half,  half),
    ] {
        let part = commands.spawn((
            Mesh2d(circle.clone()),
            MeshMaterial2d(mat.clone()),
            Transform::from_xyz(offset.x, offset.y, 0.0),
            RenderLayers::layer(CUSTOMIZE_LAYER),
        )).id();
        commands.entity(part).insert(ChildOf(root));
    }
    root
}

pub fn update_drag_ghost(
    drag: Res<DragState>,
    mut ghosts: Query<&mut Transform, With<DragGhost>>,
) {
    if drag.picked.is_none() {
        return;
    }
    let Some(cursor) = drag.spec_cursor else { return };
    for mut tf in &mut ghosts {
        tf.translation.x = cursor.x;
        tf.translation.y = cursor.y;
    }
}

// ---------- Release → resolve drop ----------

pub fn complete_drag(
    mut commands: Commands,
    mouse: Res<ButtonInput<MouseButton>>,
    mut drag: ResMut<DragState>,
    mut cfg: ResMut<TurretConfig>,
    mut shop: Option<ResMut<CustomizeShop>>,
    mut scrap: ResMut<crate::Scrap>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    selected_hull: Res<crate::hull::SelectedHull>,
    targets: Query<(&Transform, &HitArea, &DropTargetMarker)>,
    ship_slots: Query<(&Transform, &super::setup::ShipSlotBase)>,
    ghosts: Query<Entity, With<DragGhost>>,
) {
    if !mouse.just_released(MouseButton::Left) {
        return;
    }

    // Release-while-pending = the press never crossed the debounce
    // threshold, so the player meant to *click*, not drag. Skip the
    // drop-target resolution path entirely and run the click-buy
    // directly — no ghost ever spawned, no transient drag visual.
    //
    // Ship-sourced clicks released over the SELL strip should still
    // sell — the debounce shouldn't punish a quick press → release on
    // the sell panel. So we resolve targets here for ship sources too,
    // but only honour `DropTargetKind::Sell`; ship-to-ship rune/turret
    // moves still require an actual drag (gives the player a clear
    // visual contract: "to move, hold and drag").
    if drag.picked.is_none() {
        if let Some(pending) = drag.pending.take() {
            let from_shop = matches!(
                pending.source,
                DragSourceKind::ShopTurret(_) | DragSourceKind::ShopRune(_)
            );
            if from_shop {
                let burst_color = purchase_burst_color(
                    &pending.source,
                    shop.as_deref(),
                );
                if let (Some(mut shop_ref), Some(cursor)) = (shop.as_mut(), drag.spec_cursor) {
                    if click_buy_shop(pending.source, &mut cfg, &mut shop_ref, &mut scrap, selected_hull.0) {
                        if let Some(color) = burst_color {
                            spawn_purchase_burst(
                                &mut commands, &mut meshes, &mut materials, cursor, color,
                            );
                        }
                    }
                }
            } else if let Some(cursor) = drag.spec_cursor {
                // Ship-source click: only the SELL strip honours a
                // click. Everything else requires holding for a drag.
                let on_sell = targets.iter().any(|(tf, hit, marker)| {
                    matches!(marker.0, DropTargetKind::Sell)
                        && hit_test(cursor, tf.translation.truncate(), hit.size)
                });
                if on_sell {
                    let refund = sell_refund_for(&pending.source, &cfg);
                    if refund > 0 {
                        clear_source(&pending.source, &mut cfg);
                        scrap.0 = scrap.0.saturating_add(refund);
                    }
                }
            }
            return;
        }
        // Nothing in flight — clean up any stale ghost just in case.
        for e in &ghosts {
            commands.entity(e).despawn();
        }
        return;
    }

    let picked = drag.picked.take().expect("checked is_none above");
    drag.pending = None;
    drag.ghost = None;
    for e in &ghosts {
        commands.entity(e).despawn();
    }

    let Some(cursor) = drag.spec_cursor else { return };

    // Smallest-area target wins (sockets > slots).
    let mut best: Option<(f32, DropTargetKind)> = None;
    for (tf, hit, marker) in &targets {
        if !hit_test(cursor, tf.translation.truncate(), hit.size) {
            continue;
        }
        let area = hit.size.x * hit.size.y;
        if best.map_or(true, |(a, _)| area < a) {
            best = Some((area, marker.0));
        }
    }

    let from_shop = matches!(
        picked.source,
        DragSourceKind::ShopTurret(_) | DragSourceKind::ShopRune(_)
    );
    let shop_cost = match picked.source {
        DragSourceKind::ShopTurret(idx) => shop
            .as_deref()
            .and_then(|s| s.turrets.get(idx).copied().flatten())
            .map(|o| turret_cost_for_barrels(o.barrels))
            .unwrap_or(SHOP_TURRET_COST),
        DragSourceKind::ShopRune(_) => SHOP_RUNE_COST,
        _ => 0,
    };

    match best {
        Some((_, DropTargetKind::Sell)) => {
            // Sell path — refund scrap proportional to the original
            // purchase cost. Shop-sourced drops here are no-ops
            // (nothing to "sell" — you don't own them yet).
            if from_shop { return; }
            let refund = sell_refund_for(&picked.source, &cfg);
            if refund == 0 { return; }
            clear_source(&picked.source, &mut cfg);
            scrap.0 = scrap.0.saturating_add(refund);
        }
        Some((_, target)) => {
            // Drag-drop path. Shop-sourced drops cost scrap; ship-
            // sourced drops are free (just moving turrets around).
            if from_shop && scrap.0 < shop_cost {
                return; // can't afford — leave shop + cfg untouched
            }
            let shop_ref = shop.as_deref();
            let burst_color = if from_shop {
                purchase_burst_color(&picked.source, shop_ref)
            } else {
                None
            };
            if resolve_drop(&picked, target, &mut cfg, selected_hull.0) {
                if from_shop {
                    scrap.0 = scrap.0.saturating_sub(shop_cost);
                    if let Some(shop) = shop.as_mut() {
                        consume_shop_slot(&picked.source, shop);
                    }
                    if let Some(color) = burst_color {
                        let pos = drop_burst_position(target, cursor, &ship_slots);
                        spawn_purchase_burst(
                            &mut commands, &mut meshes, &mut materials, pos, color,
                        );
                    }
                }
            }
        }
        None => {
            // Released off any drop target. A genuine click (press +
            // release inside `DRAG_HOLD_THRESHOLD`) is handled by the
            // pending branch at the top of this function — by the time
            // we reach the drop-resolution arm, the player has been
            // dragging a ghost. Releasing in empty space then means
            // "cancel this drag", not "auto-place". No purchase, no
            // refund, no movement.
            let _ = from_shop;
            let _ = cursor;
        }
    }
}

/// World-space spec coord to drop the particle burst on. Drag-drops
/// snap to the centre of the resolved slot (so the visual sits on
/// the freshly-equipped turret) rather than the cursor — looks
/// cleaner than a burst that lands wherever the player happened
/// to release.
fn drop_burst_position(
    target: DropTargetKind,
    cursor: Vec2,
    ship_slots: &Query<(&Transform, &super::setup::ShipSlotBase)>,
) -> Vec2 {
    let slot_idx = match target {
        DropTargetKind::ShipSlot(i) => Some(i),
        DropTargetKind::ShipRune { slot, .. } => Some(slot),
        DropTargetKind::Sell => None,
    };
    if let Some(idx) = slot_idx {
        for (tf, base) in ship_slots.iter() {
            if base.slot == idx {
                return tf.translation.truncate();
            }
        }
    }
    cursor
}

/// Resolve the burst colour for a shop-sourced drag — the turret's
/// or rune's own tint, so the particles read as "this thing arrived
/// here". Returns `None` for non-shop sources (ship-to-ship moves
/// don't get a burst).
fn purchase_burst_color(
    source: &DragSourceKind,
    shop: Option<&CustomizeShop>,
) -> Option<Color> {
    let shop = shop?;
    match *source {
        DragSourceKind::ShopTurret(idx) => {
            let offer = shop.turrets.get(idx).copied().flatten()?;
            Some(turret_color_for(offer.weapon))
        }
        DragSourceKind::ShopRune(idx) => {
            let rune = shop.runes.get(idx).copied().flatten()?;
            Some(rune_color_for(rune))
        }
        _ => None,
    }
}

/// Refund a ship-sourced drag (turret or rune) at `SHOP_SELL_FRACTION`
/// of its original buy cost. Multi-barrel turrets refund per-barrel,
/// and socketed runes' cost is included in the turret-sell payout so
/// the player isn't taxed twice for losing them.
///
/// Shop-sourced drags return `0` — can't sell something you don't
/// own yet.
pub fn sell_refund_for(source: &DragSourceKind, cfg: &TurretConfig) -> u32 {
    // Floor the fraction then clamp to >= 1 for any genuinely sellable
    // source — the cost-rebalance to 2 scrap left the old `0.33 × cost
    // floored` math returning 0 for a vanilla 1-barrel turret with no
    // runes, which propagated as "sell does nothing" because
    // `complete_drag` bails on a zero refund. Min-1 ensures sell
    // always feels like it did *something*; empty slots still return 0
    // via the equipped / is_none guards above.
    match *source {
        DragSourceKind::ShipSlot(slot) => {
            let s = cfg.slots[slot];
            if !s.equipped { return 0; }
            // Refund the tier price so a T2 / T3 directly bought from
            // the shop returns ~50% of its 5 / 12 sticker. Stacked-up
            // turrets refund the same — the player has chosen to
            // "compress" their barrels into one slot and the refund
            // reflects the slot's current tier, not the buy history.
            let mut total_cost = turret_cost_for_barrels(s.barrels);
            for _ in s.runes.iter().flatten() {
                total_cost += SHOP_RUNE_COST;
            }
            ((total_cost as f32 * SHOP_SELL_FRACTION).floor() as u32).max(1)
        }
        DragSourceKind::ShipRune { slot, rune_idx } => {
            if cfg.slots[slot].runes[rune_idx].is_none() { return 0; }
            ((SHOP_RUNE_COST as f32 * SHOP_SELL_FRACTION).floor() as u32).max(1)
        }
        _ => 0,
    }
}

/// Clear a ship-sourced slot (or socket) after a successful sell.
fn clear_source(source: &DragSourceKind, cfg: &mut TurretConfig) {
    match *source {
        DragSourceKind::ShipSlot(s) => {
            cfg.slots[s] = SlotCfg::default();
        }
        DragSourceKind::ShipRune { slot, rune_idx } => {
            cfg.slots[slot].runes[rune_idx] = None;
        }
        _ => {}
    }
}

/// Helper for `complete_drag`: blank the shop slot a successful
/// drag-buy came from so the player can't pick the same offering
/// twice. Cost deduction is the caller's responsibility.
fn consume_shop_slot(source: &DragSourceKind, shop: &mut CustomizeShop) {
    match source {
        DragSourceKind::ShopTurret(idx) => {
            if let Some(slot) = shop.turrets.get_mut(*idx) {
                *slot = None;
            }
            if let Some(flag) = shop.turrets_locked.get_mut(*idx) {
                *flag = false;
            }
        }
        DragSourceKind::ShopRune(idx) => {
            if let Some(slot) = shop.runes.get_mut(*idx) {
                *slot = None;
            }
            if let Some(flag) = shop.runes_locked.get_mut(*idx) {
                *flag = false;
            }
        }
        _ => {}
    }
}

/// Returns `true` if the drop changed game state (move / merge / equip).
/// Invalid drops (type mismatch, self-drop, mismatch on occupied target)
/// return `false` so the caller can leave the source untouched and the
/// shop unchanged.
fn resolve_drop(
    picked: &Picked,
    target: DropTargetKind,
    cfg: &mut TurretConfig,
    hull: crate::hull::Hull,
) -> bool {
    match (picked.payload, target) {
        (Payload::Turret { weapon, barrels }, DropTargetKind::ShipSlot(target_slot)) => {
            if let DragSourceKind::ShipSlot(src) = picked.source {
                if src == target_slot {
                    return false;
                }
            }
            // Hull constraints. Slot count cap (Cutter has 4 slots
            // instead of 8) silently rejects drops onto locked slots
            // — the shop side is the source of the turret, the cap
            // is an equip-time gate. Weapon-tag lock (Marauder is
            // Pirate-only) is also enforced here so the equip never
            // commits a banned weapon.
            if target_slot >= hull.turret_slot_cap() {
                return false;
            }
            if !hull.allows_weapon(weapon) {
                return false;
            }
            // Runes carry with the turret: when picking up a ship-
            // slot turret and dropping it on a fresh slot, the
            // socketed runes travel with it. Shop-sourced turrets
            // start with empty sockets.
            let carried_runes = match picked.source {
                DragSourceKind::ShipSlot(src) => cfg.slots[src].runes,
                _ => [None; 3],
            };
            let target_state = cfg.slots[target_slot];
            if !target_state.equipped {
                cfg.slots[target_slot] = SlotCfg {
                    equipped: true,
                    weapon,
                    damage: weapon.defaults().0,
                    fire_rate: weapon.defaults().1,
                    barrels,
                    runes: carried_runes,
                };
                clear_source_if_ship(picked, cfg);
                true
            } else if target_state.weapon == weapon
                && target_state.barrels == barrels
                && target_state.barrels < 3
            {
                // Stack-merge: bump barrels. Source's runes do NOT
                // auto-shove into the target — they stay BEHIND in
                // the source slot as "orphans" so the player picks
                // which ones to keep. `update_orphan_*` systems
                // highlight orphans with a shaking `!` and the
                // tooltip; `clear_orphan_runes_on_exit` wipes any
                // unsaved orphans when the shop closes.
                cfg.slots[target_slot].barrels = (target_state.barrels + 1).min(3);
                if let DragSourceKind::ShipSlot(src) = picked.source {
                    // Preserve runes; clear weapon / damage / fire
                    // rate / barrels / equipped so the turret base
                    // hides but the rune sockets stay visible.
                    let preserved_runes = cfg.slots[src].runes;
                    cfg.slots[src] = SlotCfg {
                        equipped: false,
                        weapon: crate::weapon::WeaponType::Standard,
                        damage: 0,
                        fire_rate: 0.0,
                        barrels: 0,
                        runes: preserved_runes,
                    };
                } else {
                    // Shop-sourced merge: standard cleanup, no
                    // orphans because shop turrets have empty
                    // sockets to start with.
                    clear_source_if_ship(picked, cfg);
                }
                true
            } else if let DragSourceKind::ShipSlot(src) = picked.source {
                // Ship-to-ship swap: target is occupied with a
                // different weapon (or already-maxed barrels of the
                // same weapon). Exchange the two SlotCfgs wholesale —
                // stats, barrels, and runes all travel with their
                // weapon. Both slots stay equipped post-swap.
                cfg.slots.swap(src, target_slot);
                true
            } else {
                false
            }
        }
        (Payload::Rune(rune), DropTargetKind::ShipRune { slot, rune_idx }) => {
            if !cfg.slots[slot].equipped {
                return false;
            }
            // Amplifier sockets beyond its broadcast tier are
            // blocked — a T1 Amp shares only socket 0, so 1 and 2
            // refuse drops. Visible to the player via the hash
            // overlay handled in `update_customize_ship`.
            if matches!(cfg.slots[slot].weapon, WeaponType::Amplifier) {
                let cap = cfg.slots[slot].barrels.clamp(1, 3) as usize;
                if rune_idx >= cap {
                    return false;
                }
            }
            if let DragSourceKind::ShipRune { slot: s, rune_idx: r } = picked.source {
                if s == slot && r == rune_idx {
                    return false;
                }
            }
            let displaced = cfg.slots[slot].runes[rune_idx];
            let ship_src = if let DragSourceKind::ShipRune { slot: s, rune_idx: r } = picked.source {
                Some((s, r))
            } else {
                None
            };
            // Targeting-rune exclusivity on the destination slot. The
            // moving rune takes `rune_idx`, so exclude that socket. If
            // the drag started in the same slot, the rune still sits
            // in its source socket pre-write — exclude that too, or
            // we'd count it as a stale duplicate.
            if rune.is_targeting() {
                for (i, r) in cfg.slots[slot].runes.iter().enumerate() {
                    if i == rune_idx { continue; }
                    if let Some((src_slot, src_idx)) = ship_src {
                        if src_slot == slot && src_idx == i { continue; }
                    }
                    if let Some(other) = r {
                        if other.is_targeting() {
                            return false;
                        }
                    }
                }
            }
            // Cross-slot swap: the displaced rune is about to take the
            // source socket. Enforce the same exclusivity on the source
            // slot so a swap can't smuggle two targeting runes onto one
            // weapon.
            if let (Some(disp), Some((src_slot, src_idx))) = (displaced, ship_src) {
                if src_slot != slot && disp.is_targeting() {
                    for (i, r) in cfg.slots[src_slot].runes.iter().enumerate() {
                        if i == src_idx { continue; }
                        if let Some(other) = r {
                            if other.is_targeting() {
                                return false;
                            }
                        }
                    }
                }
            }
            cfg.slots[slot].runes[rune_idx] = Some(rune);
            if let Some((src_slot, src_idx)) = ship_src {
                // Send the displaced rune back to the source socket so
                // ship-to-ship rune drops behave as a swap, not an
                // overwrite. Empty destination => source clears.
                cfg.slots[src_slot].runes[src_idx] = displaced;
            }
            true
        }
        _ => false,
    }
}

fn clear_source_if_ship(picked: &Picked, cfg: &mut TurretConfig) {
    if let DragSourceKind::ShipSlot(s) = picked.source {
        cfg.slots[s] = SlotCfg::default();
    }
}

// ---------- Purchase-confirmation particles ----------

/// Short-lived sparkle spawned when a shop turret / rune is successfully
/// bought. Lives on `CUSTOMIZE_LAYER` so it renders inside the shop's
/// chunky-pixel render target alongside everything else.
#[derive(Component)]
pub struct PurchaseBurstParticle {
    pub life: f32,
    pub max_life: f32,
    pub velocity: Vec2,
}

/// Burst size + speed — tuned for a "subtle confirm" feel rather than
/// a kill explosion. Particles fly outward radially, fade as they
/// shrink, and despawn in well under half a second.
const BURST_COUNT: u32 = 10;
const BURST_LIFE_MIN: f32 = 0.18;
const BURST_LIFE_MAX: f32 = 0.36;
const BURST_SPEED_MIN: f32 = 10.0;
const BURST_SPEED_MAX: f32 = 22.0;
/// Z high enough to sit above the just-placed turret base but below
/// any popped-up tooltip text.
const BURST_Z: f32 = 6.0;

pub fn spawn_purchase_burst(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ColorMaterial>,
    pos: Vec2,
    color: Color,
) {
    let mesh = meshes.add(Rectangle::new(0.9, 0.9));
    let mat = materials.add(color);
    let mut rng = rand::thread_rng();
    for _ in 0..BURST_COUNT {
        let angle = rng.gen::<f32>() * std::f32::consts::TAU;
        let speed = rng.gen_range(BURST_SPEED_MIN..BURST_SPEED_MAX);
        let velocity = Vec2::new(angle.cos(), angle.sin()) * speed;
        let life = rng.gen_range(BURST_LIFE_MIN..BURST_LIFE_MAX);
        commands.spawn((
            Mesh2d(mesh.clone()),
            MeshMaterial2d(mat.clone()),
            Transform::from_translation(pos.extend(BURST_Z)),
            RenderLayers::layer(CUSTOMIZE_LAYER),
            PurchaseBurstParticle { life, max_life: life, velocity },
        ));
    }
}

/// Advance every `PurchaseBurstParticle`: drift outward, shrink by
/// life fraction, despawn when expired. Independent of `CustomizeOpen`
/// (particles already in flight when the panel closes finish their
/// short life rather than hanging in zombie state).
pub fn tick_purchase_particles(
    time: Res<Time>,
    mut commands: Commands,
    mut q: Query<(Entity, &mut Transform, &mut PurchaseBurstParticle)>,
) {
    let dt = time.delta_secs();
    for (e, mut tf, mut p) in &mut q {
        p.life -= dt;
        if p.life <= 0.0 {
            commands.entity(e).despawn();
            continue;
        }
        tf.translation.x += p.velocity.x * dt;
        tf.translation.y += p.velocity.y * dt;
        // Decelerate so the burst settles instead of flying linearly
        // to the edge of the panel.
        p.velocity *= (1.0 - dt * 4.0).max(0.0);
        let t = (p.life / p.max_life).clamp(0.0, 1.0);
        let s = t * 1.1;
        tf.scale = Vec3::new(s, s, 1.0);
    }
}
