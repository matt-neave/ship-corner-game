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
    /// Octopus cage — a deck cage holding an octopus that swims out
    /// into the water around the ship. The octopus has 8 visible
    /// legs; `slot.barrels` of them (2 / 4 / 6) are "active" and
    /// slap nearby enemies. See `octopus.rs` for the spawn + AI.
    /// Tagged Autonomous (it's a deployed unit, like the HeliPad's
    /// helicopter).
    Cage,
    /// Long-range melee. Fires a single harpoon out to 150% range
    /// for 1 damage, then reels the target back to the hull along
    /// a visible chain — the impaled enemy is dragged into contact
    /// with the friendly ship where `friendly_ram_damage` finishes
    /// the job. See `harpoon.rs` for the projectile + pull system.
    Harpoon,
    /// Salvo launcher — every 2.5s, fires 4 seeking rockets at once.
    /// Each rocket re-homes onto a target picked through the standard
    /// targeting-rune pipeline (TargetCarousel rotates targets, others
    /// pick the same priority enemy). Future-tagged. See `turret/mod.rs`
    /// firing path; rockets reuse `HomingMissile` from `ally::missile`.
    SpreadRockets,
    /// Short-cone burner. Doesn't fire bullets. Ticks 1 damage every
    /// 0.5s for 3s, then a 3s reload. Does NOT auto-apply the Fire
    /// rune — burn is a separate effect from Fire status. See
    /// `flamethrower.rs` for the cone + tick logic.
    Flamethrower,
    /// Spike-armoured deck plate. Doesn't fire anything. Each equipped
    /// slot adds `SPIKED_PLATE_DAMAGE_BONUS` to the ship's contact-ram
    /// damage AND reduces incoming bullet damage by
    /// `SPIKED_PLATE_REDUCTION` when the bullet hits the hull on the
    /// same side this slot occupies. Tagged Melee + Support.
    SpikedPlate,
    /// Rune-share support node. Doesn't fire. Its own three rune
    /// sockets are mirrored into the empty rune slots of every
    /// adjacent equipped turret each frame (see
    /// `sync_turret_config`), so the runes "broadcast" to whatever
    /// fires next door. Tagged `Support`.
    Amplifier,
    /// Lock-and-charge volley. Long cooldown (~3s), then fires
    /// `barrels` heavy "shark" projectiles in parallel along the
    /// turret's aim line. Each shark pierces every enemy on its
    /// path for high damage, persisting until it leaves the arena.
    /// Tagged `Autonomous` for thematic kinship with the deployed-
    /// unit weapons (HeliPad helicopter, Cage octopus), so the
    /// Autonomous synergy and `Hustle` apply uniformly.
    SharkNet,
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
    /// Implicit default when no targeting rune is socketed - the
    /// picker uses nearest-to-fallback semantics directly without
    /// reading the enum. Kept as a named variant for completeness
    /// and so a future "TargetClosest" rune can reuse the slot.
    #[allow(dead_code)]
    Closest,
    Furthest,
    HighestHp,
    LowestHp,
}

/// Unified target picker shared by player turrets AND autonomous
/// units. Returns ONE target position chosen by the slot's
/// targeting rune (or nearest-to-fallback if no rune). The caller
/// pre-filters the candidate slice — turrets gate by arc + range,
/// autonomous units typically pass everything in their roam radius.
///
/// Picker rules:
/// - Targeting rune present → pick the best candidate by the rune's
///   priority, measured relative to `anchor` (the SHIP for autonomous
///   units, the TURRET for player turrets).
/// - No rune → pick nearest to `fallback` (the calling unit's
///   position for autonomous, the turret for player turrets).
///
/// `candidates` is a slice of `(world_pos, hp)` snapshots taken
/// from the enemy query. Returns `None` only when the slice is
/// empty. The autonomous caller adds `offset_for_slot` on top of
/// the return value to spread multiple same-kind units; player
/// turrets don't need that since their arcs already separate them.
pub fn pick_target(
    candidates: &[(bevy::math::Vec2, i32)],
    anchor: bevy::math::Vec2,
    fallback: bevy::math::Vec2,
    runes: &[crate::rune::Rune],
    cycle_idx: Option<u32>,
) -> Option<bevy::math::Vec2> {
    if candidates.is_empty() { return None; }

    // Carousel rune: pick by deterministic rotation, not score. The
    // slot's `cycle_idx` advances once per shot in `turret_aim_fire`,
    // so successive shots step through the in-arc candidates in
    // order. Callers without a cycle source (helis, octopus) pass
    // `None` and the rune degenerates to "first candidate" — not
    // ideal but stable and non-crashing. Stable sorted-by-distance
    // ordering keeps the rotation visually predictable even as the
    // candidate set shifts frame to frame.
    let has_carousel = runes
        .iter()
        .any(|r| matches!(r, crate::rune::Rune::TargetCarousel));
    if has_carousel {
        let mut ordered: Vec<(bevy::math::Vec2, i32)> = candidates.to_vec();
        ordered.sort_by(|a, b| {
            a.0.distance_squared(anchor)
                .partial_cmp(&b.0.distance_squared(anchor))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let n = ordered.len();
        let idx = (cycle_idx.unwrap_or(0) as usize) % n;
        return Some(ordered[idx].0);
    }

    let priority = runes
        .iter()
        .find_map(|r| r.target_priority());
    let best = if let Some(p) = priority {
        candidates
            .iter()
            .min_by(|a, b| {
                score_for(**a, p, anchor)
                    .partial_cmp(&score_for(**b, p, anchor))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    } else {
        candidates
            .iter()
            .min_by(|a, b| {
                a.0.distance_squared(fallback)
                    .partial_cmp(&b.0.distance_squared(fallback))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    };
    best.map(|(pos, _)| *pos)
}

/// Deterministic per-slot world-space offset for autonomous units
/// to add on top of `pick_target`'s result so multiple same-kind
/// units approach a shared target from different angles. Returns
/// 0 for slot 0 so the first unit aims dead-on; subsequent slots
/// rotate around an 8-step compass at a small fixed radius. Not
/// applied for player turrets (their arcs already separate them).
pub fn offset_for_slot(slot_idx: usize) -> bevy::math::Vec2 {
    if slot_idx == 0 {
        return bevy::math::Vec2::ZERO;
    }
    let angle = (slot_idx as f32) * std::f32::consts::TAU / 8.0;
    let radius = 6.0;
    bevy::math::Vec2::new(angle.cos() * radius, angle.sin() * radius)
}

/// Score helper for `pick_target`. Smaller score == higher priority
/// for the picker. Distance-based priorities measure from `anchor`,
/// which the caller chooses (ship for autonomous, turret for player).
fn score_for(
    (pos, hp): (bevy::math::Vec2, i32),
    p: TargetPriority,
    anchor: bevy::math::Vec2,
) -> f32 {
    match p {
        TargetPriority::Closest   =>  pos.distance_squared(anchor),
        TargetPriority::Furthest  => -pos.distance_squared(anchor),
        TargetPriority::HighestHp => -(hp as f32),
        TargetPriority::LowestHp  =>  hp as f32,
    }
}

impl WeaponType {
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
            // Cage: the octopus's legs use this damage per slap and
            // this rate as slap cadence. `barrels` (1/2/3) selects how
            // many legs are active (2/4/6), so total dps =
            // damage × fire_rate × active_legs.
            WeaponType::Cage       => (3, 1.5),
            // Harpoon: low direct damage (1) on the spear itself —
            // the reel-in onto the hull is where the actual hurt
            // happens via `friendly_ram_damage`. Slow cadence so the
            // ship isn't a meat-grinder; one harpoon at a time keeps
            // the chain visual readable.
            WeaponType::Harpoon    => (1, 0.7),
            // Spread Rockets: 4 rockets per shot, fire rate = 0.4Hz
            // (one volley every 2.5s). Damage per rocket — modest so
            // a full salvo lands as a satisfying chunk without being
            // a Mortar-replacement.
            WeaponType::SpreadRockets => (3, 0.4),
            // Flamethrower: damage / tick repurposed by `flamethrower.rs`
            // as "1 damage every 0.5s during the active burn phase".
            // Fire rate = 2.0Hz drives the per-tick cadence; the
            // outer 3-on / 3-off cycle is owned by the `Flamethrower`
            // component itself.
            WeaponType::Flamethrower => (1, 2.0),
            // Spike Plate: doesn't fire. Placeholder zeros so the
            // stats panel reads "0 / 0" cleanly. Damage/reduction
            // numbers live in `balance::SPIKED_PLATE_*`.
            WeaponType::SpikedPlate => (0, 0.0),
            // Amplifier: doesn't fire. Same placeholder zeros — the
            // rune-share logic in `sync_turret_config` is the whole
            // gameplay value of this slot.
            WeaponType::Amplifier => (0, 0.0),
            // SharkNet: heavy damage on a long cooldown. 5 dmg per
            // shark; 0.33Hz = one volley every 3 seconds. Barrels
            // add side-by-side sharks (1 / 2 / 3) for a wider sweep.
            WeaponType::SharkNet => (5, 0.33),
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
            WeaponType::Cage       => tr("weapon_cage"),
            WeaponType::Harpoon    => tr("weapon_harpoon"),
            WeaponType::SpreadRockets => tr("weapon_spread_rockets"),
            WeaponType::Flamethrower => tr("weapon_flamethrower"),
            WeaponType::SpikedPlate => tr("weapon_spiked_plate"),
            WeaponType::Amplifier => tr("weapon_amplifier"),
            WeaponType::SharkNet => tr("weapon_sharknet"),
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
            WeaponType::Cage       => tr("weapon_cage_desc"),
            WeaponType::Harpoon    => tr("weapon_harpoon_desc"),
            WeaponType::SpreadRockets => tr("weapon_spread_rockets_desc"),
            WeaponType::Flamethrower => tr("weapon_flamethrower_desc"),
            WeaponType::SpikedPlate => tr("weapon_spiked_plate_desc"),
            WeaponType::Amplifier => tr("weapon_amplifier_desc"),
            WeaponType::SharkNet => tr("weapon_sharknet_desc"),
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
            // Long-distance king — heavy shots that reach noticeably
            // further than every other direct-fire weapon.
            WeaponType::Sniper     => 2.2,
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
            // Cage: octopus has its own roam radius; the slot itself
            // doesn't have a "range". 1.0 is a placeholder.
            WeaponType::Cage       => 1.0,
            // Harpoon: long reach — the whole point is to spear an
            // enemy at standoff and reel them in.
            WeaponType::Harpoon    => 1.5,
            // Spread Rockets: medium range — the seek logic does the
            // last-mile work, so the firing arc just needs to cover
            // mid-field engagement.
            WeaponType::SpreadRockets => 1.4,
            // Flamethrower: cone reach == `TURRET_RANGE` so the
            // displayed range matches the actual cone depth. The
            // `flamethrower.rs` tick reads a separate `FLAMETHROWER_REACH`
            // constant so the cone fills a meaningful slice of the
            // arena (TURRET_RANGE-equivalent in world units).
            WeaponType::Flamethrower => 1.0,
            // Spike Plate: doesn't project — placeholder for the
            // exhaustive match.
            WeaponType::SpikedPlate => 1.0,
            // Amplifier: no projectile, no range. Placeholder so the
            // exhaustive match builds.
            WeaponType::Amplifier => 1.0,
            // SharkNet: very long reach — the salvo crosses the
            // arena so the sharks have time to mow through the
            // entire width before despawning at the far edge.
            WeaponType::SharkNet => 2.5,
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
    /// HeliPad (helicopter does the firing), Booster (pure support),
    /// Blade (melee aura), Cage (octopus does the slapping), and
    /// Flamethrower (the cone burn is owned by its own tick system).
    /// The aim/fire system early-returns for these so the slot doesn't
    /// try to track a target or spawn muzzle flashes.
    pub fn fires_from_base(self) -> bool {
        // SharkNet is autonomous-deployed (like HeliPad / Cage): the
        // slot spawns a persistent shark unit that hunts independently;
        // the deck pad itself doesn't fire anything.
        !matches!(
            self,
            WeaponType::HeliPad
                | WeaponType::Booster
                | WeaponType::Blade
                | WeaponType::Cage
                | WeaponType::Flamethrower
                | WeaponType::SpikedPlate
                | WeaponType::Amplifier
                | WeaponType::SharkNet
        )
    }

    /// Whether this weapon's turret should show the standard barrel
    /// children. False for HeliPad (deck pad only), Booster (support
    /// platform), Blade (arm + blade decor instead), Cage (cage decor
    /// + remote octopus), Flamethrower (single fat nozzle, not thin
    /// barrels), and SpikedPlate (passive armour, no barrels).
    /// `sync_turret_config` uses this to hide the barrel meshes when
    /// the slot's weapon doesn't have any.
    pub fn has_barrels(self) -> bool {
        !matches!(
            self,
            WeaponType::HeliPad
                | WeaponType::Booster
                | WeaponType::Blade
                | WeaponType::Cage
                | WeaponType::Flamethrower
                | WeaponType::SpikedPlate
                | WeaponType::Amplifier
        )
    }

    /// Gameplay-class tags — every chip rendered in the tooltip and
    /// every synergy pool this weapon contributes to. Most weapons
    /// have a single tag; multi-tag weapons (e.g. `Harpoon`) count
    /// toward two synergies simultaneously, which makes them
    /// natural "bridge" picks for cross-synergy builds. The FIRST
    /// tag is treated as the "primary" by the rest of the codebase
    /// (turret accent colour, banner stacking order). See
    /// `WeaponTag` for the full taxonomy.
    pub fn tags(self) -> &'static [WeaponTag] {
        match self {
            WeaponType::Standard
            | WeaponType::Sniper
            | WeaponType::MachineGun
            | WeaponType::Shotgun
            | WeaponType::Mortar   => &[WeaponTag::Naval],
            WeaponType::Railgun    => &[WeaponTag::Future],
            WeaponType::HeliPad    => &[WeaponTag::Autonomous],
            WeaponType::Cannon     => &[WeaponTag::Pirate],
            WeaponType::Booster    => &[WeaponTag::Support],
            WeaponType::Blade      => &[WeaponTag::Melee],
            WeaponType::Cage       => &[WeaponTag::Autonomous],
            // Pirate spear-thrower — counts as Pirate for scrap
            // synergy AND Melee for the heal-on-kill synergy. The
            // "reel in then ram" loop fits both flavours.
            WeaponType::Harpoon    => &[WeaponTag::Pirate, WeaponTag::Melee],
            // Smart munitions — Future for the guided-rocket fantasy.
            WeaponType::SpreadRockets => &[WeaponTag::Future],
            // Close-range burner — Melee for the heal-on-kill synergy
            // and the in-your-face engagement style.
            WeaponType::Flamethrower => &[WeaponTag::Melee],
            // Spiked deck plate — Melee for the contact-damage buff
            // and Support for the per-side reduction it grants.
            WeaponType::SpikedPlate => &[WeaponTag::Melee, WeaponTag::Support],
            // Rune-share broadcast pad — Support, like Booster.
            WeaponType::Amplifier => &[WeaponTag::Support],
            // SharkNet: heavy charge volley. Tagged Autonomous so it
            // counts toward the autonomous synergy alongside HeliPad
            // and Cage — thematic kin with deployed-unit weapons.
            WeaponType::SharkNet => &[WeaponTag::Autonomous],
        }
    }

    /// Convenience for call sites that only need the primary
    /// (display / colour) tag — currently only the in-world turret
    /// accent. Equivalent to `tags()[0]` and panics only if a weapon
    /// was ever given an empty tag list (which would be a definition
    /// bug). Marked `allow(dead_code)` so adding it as a deliberate
    /// API doesn't have to wait for a second consumer.
    #[allow(dead_code)]
    pub fn primary_tag(self) -> WeaponTag {
        self.tags()[0]
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
            WeaponType::Cage       => &self.turret_cage,
            WeaponType::Harpoon    => &self.turret_harpoon,
            WeaponType::SpreadRockets => &self.turret_spread_rockets,
            WeaponType::Flamethrower => &self.turret_flamethrower,
            WeaponType::SpikedPlate => &self.turret_spiked_plate,
            WeaponType::Amplifier => &self.turret_amplifier,
            WeaponType::SharkNet => &self.turret_sharknet,
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
            // Booster + Blade + Cage never spawn bullets; fall back
            // to the friendly material so the exhaustive match compiles.
            WeaponType::Booster    => &self.bullet_friendly_outer,
            WeaponType::Blade      => &self.bullet_friendly_outer,
            WeaponType::Cage       => &self.bullet_friendly_outer,
            // Harpoon spear uses the bronze launcher tone for the shaft.
            WeaponType::Harpoon    => &self.turret_harpoon,
            // Spread rockets reuse the homing-missile colorway so the
            // rust + flame palette reads consistently with the ally
            // submarine's salvo.
            WeaponType::SpreadRockets => &self.bullet_missile_outer,
            // Flamethrower never spawns bullets; fallback to friendly.
            WeaponType::Flamethrower => &self.bullet_friendly_outer,
            // Spike Plate never spawns bullets; fallback to friendly.
            WeaponType::SpikedPlate => &self.bullet_friendly_outer,
            // Amplifier never spawns bullets; fallback to friendly.
            WeaponType::Amplifier => &self.bullet_friendly_outer,
            // SharkNet: heavy steel-blue outer shell — reads as a
            // hefty piercing projectile distinct from the standard
            // friendly tracer colour.
            WeaponType::SharkNet => &self.bullet_sharknet_outer,
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
            WeaponType::Cage       => &self.bullet_friendly,
            // Bright tip on the spear head.
            WeaponType::Harpoon    => &self.harpoon_head,
            WeaponType::SpreadRockets => &self.bullet_missile_inner,
            WeaponType::Flamethrower => &self.fire,
            WeaponType::SpikedPlate => &self.bullet_friendly,
            WeaponType::Amplifier => &self.bullet_friendly,
            WeaponType::SharkNet => &self.bullet_sharknet,
        }
    }
}
