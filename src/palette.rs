//! Single source of truth for every color in the game. Three layers:
//!
//! 1. **`Palette`** resource — recolorable game-world tones (ocean, hull,
//!    enemy, bullets, trail). Swap presets by mutating the resource at
//!    runtime; `apply_palette` propagates to all shared materials.
//! 2. **Weapon-identity hexes** — fixed colors for Sniper / MG / Shotgun /
//!    Railgun. Not in `Palette` because they define the weapon, not the theme.
//! 3. **UI theme** — flat colors for the LHS panel chrome. Independent of the
//!    play-area palette so the panel stays legible when game colors change.
//!
//! Materials handed to entities live in `PaletteMaterials`; they share
//! handles so a single `Assets<ColorMaterial>::get_mut` updates every entity
//! that uses that color.

use bevy::prelude::*;

use crate::balance::BULLET_INNER_LIGHTEN;

// ---------- Color helpers ----------
pub fn hex(s: &str) -> Color {
    let s = s.trim_start_matches('#');
    let r = u8::from_str_radix(&s[0..2], 16).unwrap_or(255);
    let g = u8::from_str_radix(&s[2..4], 16).unwrap_or(0);
    let b = u8::from_str_radix(&s[4..6], 16).unwrap_or(255);
    Color::srgb(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
}

/// Reapply `c`'s RGB with a custom alpha. Used for telegraph
/// overlays so the warning fade reads as a hint, not a wall.
pub fn translucent(c: Color, alpha: f32) -> Color {
    let s: bevy::color::Srgba = c.into();
    Color::srgba(s.red, s.green, s.blue, alpha.clamp(0.0, 1.0))
}

/// Mix `c` toward white by `amount` (0=unchanged, 1=white).
pub fn lighten(c: Color, amount: f32) -> Color {
    let s: bevy::color::Srgba = c.into();
    Color::srgb(
        s.red   + (1.0 - s.red)   * amount,
        s.green + (1.0 - s.green) * amount,
        s.blue  + (1.0 - s.blue)  * amount,
    )
}

/// Multiply rgb channels by `factor` (0..1) — preserves hue, lowers luminance.
pub fn darken(c: Color, factor: f32) -> Color {
    let s: bevy::color::Srgba = c.into();
    Color::srgb(s.red * factor, s.green * factor, s.blue * factor)
}

// ---------- Weapon-identity colors ----------
//
// Hex pairs per weapon:
// - `*_HEX`        = the dark base (turret + bullet outer ring).
// - `*_BRIGHT_HEX` = the vivid inner core + muzzle flash. Hand-picked because
//   mechanically lightening the dark base tones produces muddy mid-tones that
//   wash out on the cobalt ocean.
pub const SNIPER_HEX:         &str = "#5d275d";
pub const SNIPER_BRIGHT_HEX:  &str = "#ff70d4";
pub const MG_HEX:             &str = "#29366f";
pub const MG_BRIGHT_HEX:      &str = "#6bd5ff";
pub const SHOTGUN_HEX:        &str = "#6e2333";
pub const SHOTGUN_BRIGHT_HEX: &str = "#ff7080";
pub const RAILGUN_HEX:        &str = "#1e555c";
pub const RAILGUN_BRIGHT_HEX: &str = "#5cf2e8";
// Mortar: sandy / desert-orange identity. Dark base reads as "earth";
// bright core is a saturated amber so the long-range shells pop on
// either ocean tone. Distinct from MG cyan, Shotgun red, and Sniper
// magenta — sandy/orange is the unclaimed slot.
pub const MORTAR_HEX:         &str = "#7a4a1c";
pub const MORTAR_BRIGHT_HEX:  &str = "#ffb04a";

// ---------- HeliPad ----------
//
// Olive-green deck pad with a bright yellow `H` painted on. Reused
// by the helicopter body so the airborne unit reads as "belongs to
// that pad" without inventing a third tint.
pub const HELIPAD_DECK_HEX:   &str = "#3a7a3c";
pub const HELIPAD_H_HEX:      &str = "#ffd84a";
/// Pirate cannon — dark wood-brown carriage, pure-black iron
/// cannonballs. Reads as a hand-cast iron piece bolted to a timber
/// slide. The cannonball uses a single black for both inner and outer
/// (see `bullet_cannon` / `bullet_cannon_outer` in PaletteMaterials)
/// so the projectile reads as a solid sphere with no two-tone halo.
pub const CANNON_HEX:         &str = "#5a3520";
pub const CANNON_BRIGHT_HEX:  &str = "#000000";
/// Support booster — warm amber deck pad. Distinct from the helipad
/// green so adjacent slots read at a glance. Ring core is a brighter
/// gold so the "broadcasting" centre pops against the pad.
pub const BOOSTER_HEX:        &str = "#8a6a20";
pub const BOOSTER_BRIGHT_HEX: &str = "#ffe88a";
/// Melee blade — gun-metal arm + bright steel edge. The two-tone
/// keeps the slow-rotating arm legible against the bright spinning
/// edge at the tip.
pub const BLADE_HEX:          &str = "#4a4a55";
pub const BLADE_BRIGHT_HEX:   &str = "#d8e0e8";
/// Harpoon turret — bronze launcher base with a thick rope/chain
/// line trailing back to the impaled target. The launcher reads as
/// heavy mechanical, the chain as a taut industrial cable.
pub const HARPOON_HEX:        &str = "#9a6a32";
pub const HARPOON_BRIGHT_HEX: &str = "#e0b878";
pub const HARPOON_CHAIN_HEX:  &str = "#4a3a2a";

/// Octopus cage — dark iron bars on the deck. The cage is a cube of
/// dim metal so the bright purple octopus inside reads as the main
/// silhouette.
pub const CAGE_HEX:           &str = "#3a3a44";
/// Octopus body — saturated purple, distinct from the friendly
/// Sniper purple by being warmer / pinker. Reads as a sea creature
/// against the cool ocean.
pub const OCTOPUS_BODY_HEX:   &str = "#8a2db8";
/// Octopus active legs — bright magenta, the colour the player will
/// associate with "this leg is currently slapping things". Inactive
/// legs render the same body purple so the dim/bright contrast tells
/// the player which legs are doing work.
pub const OCTOPUS_LEG_HEX:    &str = "#ff5cd0";

/// Rammer hull — bright warning orange so the kamikaze role reads at
/// a glance against the standard red enemy palette. Same warm hue as
/// the muzzle-flash so it parses as "explosive threat".
pub const ENEMY_RAMMER_HEX:   &str = "#ff7a1f";
/// Sniper hull — deep purple. Distinct silhouette colour vs the red
/// enemy family so the player can pick out who's the long-range
/// threat at a glance. Borrows the friendly-Sniper hue for the
/// "precision weapon" association.
pub const ENEMY_SNIPER_HEX:   &str = "#5d275d";
/// Sniper aim-line — bright crimson, threading red. Used for the
/// telegraph beam the sniper draws while charging its 1.5s aim.
// Dark amber for the sniper aim line and artillery splash reticle.
// Less aggressive than bright red; reads as a warning without
// shouting "blood imminent".
pub const SNIPER_AIM_HEX:     &str = "#c89018";
/// Time-fused enemy landmine — same dark shell + red dot as the
/// friendly mine so the player parses the silhouette as "stay clear".
/// Kept in this section so future fuse-FX (pulsing dot, expanding
/// ring) inherit the same colour family.
pub const ENEMY_MINE_DOT_HEX: &str = "#ff8a3c";
/// Artillery hull — dark olive. Reads as "siege engine" against the
/// red enemy family without blending into the warning-orange Rammer
/// or the deep-purple Sniper.
pub const ENEMY_ARTILLERY_HEX: &str = "#5a6a2c";
/// Artillery landing reticle — bright crimson. The ring telegraphs
/// where the lobbed shell will hit; warm hot colour reads as "danger
/// zone" against the cool blue ocean.
pub const ARTILLERY_RETICLE_HEX: &str = "#c89018";

// ---------- Ship-class hull tints ----------
//
// Each `ShipClass` gets its own identity color, looked up via
// `PaletteMaterials::hull_for_class` (in `ally.rs`). Fixed hexes — not
// palette-driven — so a class's identity stays consistent when the
// game palette swaps. Shared between allies today and future boss
// enemies (which will reuse the same chassis at a different faction).
pub const PIRATE_HEX:    &str = "#7a4a2c"; // aged wood brown
pub const CARRIER_HEX:   &str = "#4d5663"; // naval grey, slightly cool
pub const PLANE_HEX:     &str = "#3a7a3c"; // olive/forest green — reads as a fighter against the deck
// Submarine: dark teal-blue with enough lightness/saturation to read as
// "submerged" against the bright day ocean while still being clearly
// distinct from the near-black night ocean (#1a1c2c).
pub const SUBMARINE_HEX: &str = "#3a5c70";
// Minelayer: dark steel grey with a faint warm cast — reads as a small
// utility boat next to the warmer Pirate brown and cooler Carrier grey.
pub const MINELAYER_HEX: &str = "#4a4438";
// Tender: RNLI-lifeboat vermilion — bright red-orange hull pops
// against every ocean tone and is unmistakable next to the muted
// warship hulls. Pairs with a small white wheelhouse cabin (built in
// `spawn_ally`) for the canonical orange-and-white silhouette.
pub const TENDER_HEX: &str = "#e85021";
// Blackbeard's flagship — dark charcoal hull (lifted a few notches
// out of near-black so the body reads against night-mode ocean and
// the sails sit cleanly on top). Boarding ship: no cannons, only
// sends pirate boarders across.
pub const BLACKBEARD_HEX: &str = "#2e2e3c";
// Oil tanker — chunky industrial hull. Muted olive-green so it
// reads as an industrial vessel against the warmer pirate / cooler
// carrier hulls, and pairs visually with the dark-oil slicks it
// drops behind itself.
pub const OIL_TANKER_HEX: &str = "#7a1d1a";
/// Viking longship hull — dark wood / russet brown.
pub const VIKING_HEX:         &str = "#5e2a16";
/// Viking mast — darker wood than the hull so the central pole reads
/// as a separate piece of furniture sitting on the deck.
pub const MAST_HEX:           &str = "#3a1c0e";
/// Oil slick body — near-black with a faint sheen, sits flat on the
/// water before ignition.
pub const OIL_SLICK_HEX: &str = "#0d0e12";
/// Black flag base for the skull-and-crossbones pennants. Pure dark
/// so the white skull detail (re-uses `pm.ally_flag`) reads strongly.
pub const SKULL_FLAG_HEX: &str = "#0a0a12";
/// Boarder figures — bright cream-tan so the small dots read clearly
/// as crew silhouettes against both the dark Blackbeard hull and the
/// bright ocean. Saturated enough to pop without going neon.
pub const BOARDER_HEX:      &str = "#e5c084";
/// Saturated wood-brown for the boarding-rope strung between
/// Blackbeard and its target. Bright enough to clearly span the gap
/// against both the dark Blackbeard hull and the bright ocean; the
/// boarders riding it are even brighter so they still pop on top.
pub const BOARDING_ROPE_HEX: &str = "#a87038";
/// Weathered grey for Blackbeard's sails. Mid-grey + slight cool
/// cast: bright enough to pop off the dark hull, dull enough to not
/// look ceremonial.
pub const SAIL_HEX:       &str = "#a4a4ac";

// ---------- Heal-beam color ----------
//
// Vivid green for the tender's healing beam. Picked to pop against both
// the bright day ocean and the dark night ocean while staying obviously
// distinct from the railgun/shock cyan-yellow family.
pub const HEAL_HEX: &str = "#5cf26b";

// ---------- Missile colors ----------
//
// Submarine homing missile: dark rust body + bright orange flame core.
// Distinct from the player's yellow cannonballs so missiles read as a
// different weapon class instantly.
pub const MISSILE_HEX:        &str = "#cc4422";
pub const MISSILE_BRIGHT_HEX: &str = "#ffaa55";

// ---------- Mine colors ----------
//
// Sea mines dropped by the Minelayer. Near-black outer "shell" with a
// red warning dot in the center — the shell stays low-key (it's a
// hazard sitting on the water) but the red center keeps the player
// aware of where the deathzones are.
pub const MINE_OUTER_HEX: &str = "#1a1a22";
pub const MINE_INNER_HEX: &str = "#ff4d4d";

// ---------- Status-effect tints ----------
pub const FIRE_HEX:  &str = "#ff8030"; // bright fire orange
pub const FROST_HEX: &str = "#80d8ff"; // cool sky blue (cyan-ish, distinct from fire)
pub const SHOCK_HEX: &str = "#ffe680"; // electric yellow (lightning arc)
pub const BLEED_HEX: &str = "#b21030"; // deep crimson (DoT blood drips)

// ---------- UI theme (LHS panel + draft cards) ----------
pub const UI_BG:        Color = Color::srgb(0.07, 0.08, 0.11);
pub const UI_ROW_BG:    Color = Color::srgb(0.12, 0.13, 0.17);
pub const UI_ROW_DIV:   Color = Color::srgb(0.22, 0.24, 0.30);
pub const UI_TEXT:      Color = Color::srgb(0.92, 0.93, 0.96);
pub const UI_TEXT_DIM:  Color = Color::srgb(0.55, 0.60, 0.70);
pub const UI_VALUE:     Color = Color::srgb(1.00, 0.85, 0.30);
pub const UI_BTN_BG:    Color = Color::srgb(0.22, 0.24, 0.30);
pub const UI_EQUIP_BG:  Color = Color::srgb(0.18, 0.40, 0.26);
pub const UI_ACTIVE_BG: Color = Color::srgb(0.20, 0.28, 0.40);
pub const UI_DOT_ON:    Color = Color::srgb(1.00, 0.85, 0.30);

// ---------- Palette resource ----------
#[derive(Resource, Clone, Debug)]
pub struct Palette {
    pub ocean:           Color,
    pub border:          Color,
    pub hull:            Color,
    pub hull_accent:     Color,
    pub turret:          Color,
    pub enemy:           Color,
    pub enemy_accent:    Color,
    pub bullet_friendly: Color,
    pub bullet_enemy:    Color,
    pub trail:           Color,
}

impl Palette {
    /// Selection from the AAP-64 palette — dark naval hull + arcade bullets.
    pub fn aap64_naval() -> Self {
        Self {
            ocean:           hex("#41a6f6"),
            border:          hex("#c7cfdd"),
            hull:            hex("#94b0c2"),
            hull_accent:     hex("#333c57"),
            turret:          hex("#566c86"),
            enemy:           hex("#b13e53"),
            enemy_accent:    hex("#571c27"),
            bullet_friendly: hex("#ffcd75"),
            bullet_enemy:    hex("#ff5000"),
            trail:           hex("#c7cfdd"),
        }
    }

    /// Previous palette — kept around so swapping is one line.
    #[allow(dead_code)]
    pub fn iris() -> Self {
        Self {
            ocean:           hex("#7194f0"),
            border:          hex("#abc9f1"),
            hull:            hex("#280732"),
            hull_accent:     hex("#5b4f6e"),
            turret:          hex("#b2b1c0"),
            enemy:           hex("#e15e6e"),
            enemy_accent:    hex("#280732"),
            bullet_friendly: hex("#f3a8a8"),
            bullet_enemy:    hex("#e15e6e"),
            trail:           hex("#e6ecef"),
        }
    }
}

// ---------- Shared material handles ----------
#[derive(Resource)]
pub struct PaletteMaterials {
    pub border: Handle<ColorMaterial>,
    pub hull: Handle<ColorMaterial>,
    pub hull_accent: Handle<ColorMaterial>,
    pub turret: Handle<ColorMaterial>,
    pub enemy: Handle<ColorMaterial>,
    pub enemy_accent: Handle<ColorMaterial>,
    pub enemy_heavy: Handle<ColorMaterial>,
    pub enemy_scout: Handle<ColorMaterial>,
    /// Rammer (kamikaze) hull material — warning orange. Spawn in
    /// `enemy::spawn_enemy` for the Rammer variant.
    pub enemy_rammer: Handle<ColorMaterial>,
    /// Sniper hull material — deep purple. Spawn in `enemy::spawn_enemy`
    /// for the Sniper variant.
    pub enemy_sniper: Handle<ColorMaterial>,
    /// Artillery hull — dark olive siege piece. Spawn in
    /// `enemy::spawn_enemy` for the Artillery variant.
    pub enemy_artillery: Handle<ColorMaterial>,
    /// Tint for the artillery landing-reticle ring. Bright crimson
    /// "danger zone" colour painted over the impact point during the
    /// 1.5s telegraph.
    pub artillery_reticle: Handle<ColorMaterial>,
    /// Harsh, near-opaque outline drawn as an annulus around the
    /// translucent inner reticle so the splash radius reads clearly
    /// at a glance even on busy backgrounds.
    pub artillery_reticle_outline: Handle<ColorMaterial>,
    /// Sniper aim-line tint. Used by the trajectory telegraph that
    /// renders during the sniper's 1.5s aim phase.
    pub sniper_aim: Handle<ColorMaterial>,
    /// Bright dot painted on the time-fused enemy landmine the Rammer
    /// drops on death. Distinguishes it from the friendly proximity
    /// mine's dot (`mine_inner`).
    pub enemy_mine_dot: Handle<ColorMaterial>,
    pub bullet_friendly: Handle<ColorMaterial>,
    pub bullet_enemy: Handle<ColorMaterial>,
    pub bullet_friendly_outer: Handle<ColorMaterial>,
    pub bullet_enemy_outer: Handle<ColorMaterial>,
    pub trail: Handle<ColorMaterial>,
    pub flash: Handle<ColorMaterial>,
    pub turret_sniper: Handle<ColorMaterial>,
    pub bullet_sniper: Handle<ColorMaterial>,
    pub bullet_sniper_outer: Handle<ColorMaterial>,
    pub turret_mg: Handle<ColorMaterial>,
    pub bullet_mg: Handle<ColorMaterial>,
    pub bullet_mg_outer: Handle<ColorMaterial>,
    pub turret_shotgun: Handle<ColorMaterial>,
    pub bullet_shotgun: Handle<ColorMaterial>,
    pub bullet_shotgun_outer: Handle<ColorMaterial>,
    pub turret_railgun: Handle<ColorMaterial>,
    pub bullet_railgun: Handle<ColorMaterial>,
    pub bullet_railgun_outer: Handle<ColorMaterial>,
    pub turret_mortar: Handle<ColorMaterial>,
    pub bullet_mortar: Handle<ColorMaterial>,
    pub bullet_mortar_outer: Handle<ColorMaterial>,
    /// Deck-pad colour for the HeliPad slot (gray) and the yellow `H`
    /// painted on top. Both materials are also reused by the in-flight
    /// helicopter's body / nose-turret.
    pub helipad_deck: Handle<ColorMaterial>,
    pub helipad_h: Handle<ColorMaterial>,
    /// Pirate `Cannon` weapon — dark wood carriage + iron cannonball
    /// projectile. The bullet outer/inner pair drives the muzzle flash
    /// and the cannonball mesh in `cannon.rs`.
    pub turret_cannon: Handle<ColorMaterial>,
    pub bullet_cannon: Handle<ColorMaterial>,
    pub bullet_cannon_outer: Handle<ColorMaterial>,
    /// Support `Booster` — amber deck pad + bright gold pulse ring.
    /// No bullet material; the booster doesn't fire. The ring is a
    /// child entity drawn on top of the base in `booster.rs`.
    pub turret_booster: Handle<ColorMaterial>,
    pub booster_ring: Handle<ColorMaterial>,
    /// Melee `Blade` — gun-metal slot base + arm, bright steel blade
    /// at the tip. Two-tone so the slow arm reads as separate from
    /// the spinning edge. See `blade.rs` for the rotating-arm spawn.
    pub turret_blade: Handle<ColorMaterial>,
    pub blade_edge: Handle<ColorMaterial>,
    /// Autonomous `Cage` — dark iron deck cage + the in-water
    /// octopus's body / leg materials. `octopus.rs` spawns the
    /// in-water unit; `turret_cage` is the deck visual.
    pub turret_cage: Handle<ColorMaterial>,
    pub octopus_body: Handle<ColorMaterial>,
    pub octopus_leg: Handle<ColorMaterial>,
    /// Melee `Harpoon` — bronze launcher base + matching projectile +
    /// dark-rope chain rendered behind the in-flight harpoon and
    /// across the tether to the impaled target.
    pub turret_harpoon: Handle<ColorMaterial>,
    pub harpoon_head: Handle<ColorMaterial>,
    pub harpoon_chain: Handle<ColorMaterial>,
    /// Ship-class hull tints — one per `ShipClass`. Looked up via
    /// `ally::PaletteMaterials::hull_for_class`.
    pub pirate_hull: Handle<ColorMaterial>,
    pub carrier_hull: Handle<ColorMaterial>,
    pub submarine_hull: Handle<ColorMaterial>,
    pub minelayer_hull: Handle<ColorMaterial>,
    pub tender_hull: Handle<ColorMaterial>,
    pub blackbeard_hull: Handle<ColorMaterial>,
    pub oil_tanker_hull: Handle<ColorMaterial>,
    /// Viking longship hull — wood-brown / russet, deliberately dark
    /// so the central mast pole reads as a separate piece of furniture
    /// sitting on top of the deck.
    pub viking_hull: Handle<ColorMaterial>,
    /// Viking mast — darker wood than the hull, used for the central
    /// vertical pole on the longship. Doubles as the oar-shaft material.
    pub mast: Handle<ColorMaterial>,
    /// Dark oil slick material — used by the OilTanker's freshly-laid
    /// pools before ignition. Burning slicks swap their material to
    /// `pm.fire` for the flaming-pool look.
    pub oil_slick: Handle<ColorMaterial>,
    /// Black flag for Blackbeard's skull-and-crossbones pennants.
    pub skull_flag: Handle<ColorMaterial>,
    /// Boarder crew dot color (cream-tan).
    pub boarder: Handle<ColorMaterial>,
    /// Boarding rope color — dark wood brown.
    pub boarding_rope: Handle<ColorMaterial>,
    /// Grey sail color used by Blackbeard's deck sails.
    pub sail: Handle<ColorMaterial>,
    /// Sea-mine materials. Two-tone: dark shell + red warning dot.
    pub mine_outer: Handle<ColorMaterial>,
    pub mine_inner: Handle<ColorMaterial>,
    /// Tender healing-beam color (vivid green).
    pub heal: Handle<ColorMaterial>,
    /// Plane fuselage / wings. Shared across every plane regardless of
    /// which carrier launched it (planes don't have variants today).
    pub plane_hull: Handle<ColorMaterial>,
    /// Submarine homing-missile materials. Outer = rust body, inner = the
    /// brighter flame/tip color. Stored on the shared `PaletteMaterials`
    /// because the missile is a regular Friendly bullet — collision and
    /// damage flow through `bullet_collisions` like any other bullet.
    pub bullet_missile_outer: Handle<ColorMaterial>,
    pub bullet_missile_inner: Handle<ColorMaterial>,
    /// Fire-rune particle color (also reused for other future fire FX).
    pub fire:  Handle<ColorMaterial>,
    /// Frost-rune particle color (cyan mist).
    pub frost: Handle<ColorMaterial>,
    /// Shock-rune lightning + particle color (electric yellow).
    pub shock: Handle<ColorMaterial>,
    /// Bleed-rune particle color (dark crimson drip motes).
    pub bleed: Handle<ColorMaterial>,
    /// Translucent green tint for owned territory. Currently unused —
    /// section fills are rendered via a pre-rasterized sprite in `map.rs`
    /// (single-quad rendering avoids alpha-blend triangle seams). Left in
    /// place because `apply_palette` still tracks them for future re-use.
    #[allow(dead_code)]
    pub map_owned:   Handle<ColorMaterial>,
    /// Translucent red tint for unowned territory. See `map_owned`.
    #[allow(dead_code)]
    pub map_enemy:   Handle<ColorMaterial>,
    /// Subtle dark divider lines between map sections.
    pub map_divider: Handle<ColorMaterial>,
    /// Pure white — used for the ally ship's signal flag (drawn between
    /// its turrets, fluttering via `wave_ally_flags`). Shared so every
    /// ally pulls from the same handle and benefits from batching.
    pub ally_flag: Handle<ColorMaterial>,
    /// Neutral grey square for an upgrade slot at a section's center.
    /// Reads as a placeholder/build-here affordance regardless of the
    /// underlying section tint.
    pub map_slot: Handle<ColorMaterial>,
    /// Small filled mark used for the per-section star rating, drawn in a
    /// row above each slot. Yellow so it pops on both day and night ocean.
    pub map_slot_star: Handle<ColorMaterial>,
    /// Light blue/white spray color for the splash burst spawned when
    /// the player clicks empty water on the map view to set a sail target.
    pub splash: Handle<ColorMaterial>,
}

impl PaletteMaterials {
    /// Build all handles from the active palette + fixed weapon hexes.
    pub fn build(palette: &Palette, materials: &mut Assets<ColorMaterial>) -> Self {
        let sniper  = hex(SNIPER_HEX);
        let mg      = hex(MG_HEX);
        let shotgun = hex(SHOTGUN_HEX);
        let railgun = hex(RAILGUN_HEX);
        let mortar  = hex(MORTAR_HEX);
        Self {
            border:                materials.add(palette.border),
            hull:                  materials.add(palette.hull),
            hull_accent:           materials.add(palette.hull_accent),
            turret:                materials.add(palette.turret),
            enemy:                 materials.add(palette.enemy),
            enemy_accent:          materials.add(palette.enemy_accent),
            enemy_heavy:           materials.add(darken(palette.enemy, 0.55)),
            enemy_scout:           materials.add(lighten(palette.enemy, 0.30)),
            enemy_rammer:          materials.add(hex(ENEMY_RAMMER_HEX)),
            enemy_sniper:          materials.add(hex(ENEMY_SNIPER_HEX)),
            enemy_artillery:       materials.add(hex(ENEMY_ARTILLERY_HEX)),
            // Telegraphs are intentionally faded: a Final-Fantasy-style
            // warning, not a solid colour wall. The sniper line + the
            // artillery reticle both sit on top of gameplay so a low
            // alpha keeps them readable as overlays without occluding
            // what's underneath.
            artillery_reticle:     materials.add(translucent(hex(ARTILLERY_RETICLE_HEX), 0.40)),
            artillery_reticle_outline: materials.add(translucent(hex(ARTILLERY_RETICLE_HEX), 0.95)),
            sniper_aim:            materials.add(translucent(hex(SNIPER_AIM_HEX), 0.35)),
            enemy_mine_dot:        materials.add(hex(ENEMY_MINE_DOT_HEX)),
            bullet_friendly:       materials.add(lighten(palette.bullet_friendly, BULLET_INNER_LIGHTEN)),
            bullet_enemy:          materials.add(lighten(palette.bullet_enemy, BULLET_INNER_LIGHTEN)),
            bullet_friendly_outer: materials.add(palette.bullet_friendly),
            bullet_enemy_outer:    materials.add(palette.bullet_enemy),
            trail:                 materials.add(palette.trail),
            flash:                 materials.add(Color::WHITE),
            turret_sniper:         materials.add(sniper),
            bullet_sniper:         materials.add(hex(SNIPER_BRIGHT_HEX)),
            bullet_sniper_outer:   materials.add(sniper),
            turret_mg:             materials.add(mg),
            bullet_mg:             materials.add(hex(MG_BRIGHT_HEX)),
            bullet_mg_outer:       materials.add(mg),
            turret_shotgun:        materials.add(shotgun),
            bullet_shotgun:        materials.add(hex(SHOTGUN_BRIGHT_HEX)),
            bullet_shotgun_outer:  materials.add(shotgun),
            turret_railgun:        materials.add(railgun),
            bullet_railgun:        materials.add(hex(RAILGUN_BRIGHT_HEX)),
            bullet_railgun_outer:  materials.add(railgun),
            turret_mortar:         materials.add(mortar),
            helipad_deck:          materials.add(hex(HELIPAD_DECK_HEX)),
            helipad_h:             materials.add(hex(HELIPAD_H_HEX)),
            turret_cannon:         materials.add(hex(CANNON_HEX)),
            // Pure-black cannonball — both outer and inner use the
            // same near-black so the projectile reads as one solid
            // iron sphere rather than a brown ring around a dark core.
            bullet_cannon:         materials.add(hex(CANNON_BRIGHT_HEX)),
            bullet_cannon_outer:   materials.add(hex(CANNON_BRIGHT_HEX)),
            turret_booster:        materials.add(hex(BOOSTER_HEX)),
            booster_ring:          materials.add(hex(BOOSTER_BRIGHT_HEX)),
            turret_blade:          materials.add(hex(BLADE_HEX)),
            blade_edge:            materials.add(hex(BLADE_BRIGHT_HEX)),
            turret_cage:           materials.add(hex(CAGE_HEX)),
            octopus_body:          materials.add(hex(OCTOPUS_BODY_HEX)),
            octopus_leg:           materials.add(hex(OCTOPUS_LEG_HEX)),
            turret_harpoon:        materials.add(hex(HARPOON_HEX)),
            harpoon_head:          materials.add(hex(HARPOON_BRIGHT_HEX)),
            harpoon_chain:         materials.add(hex(HARPOON_CHAIN_HEX)),
            bullet_mortar:         materials.add(hex(MORTAR_BRIGHT_HEX)),
            bullet_mortar_outer:   materials.add(mortar),
            pirate_hull:           materials.add(hex(PIRATE_HEX)),
            carrier_hull:          materials.add(hex(CARRIER_HEX)),
            submarine_hull:        materials.add(hex(SUBMARINE_HEX)),
            minelayer_hull:        materials.add(hex(MINELAYER_HEX)),
            tender_hull:           materials.add(hex(TENDER_HEX)),
            blackbeard_hull:       materials.add(hex(BLACKBEARD_HEX)),
            oil_tanker_hull:       materials.add(hex(OIL_TANKER_HEX)),
            viking_hull:           materials.add(hex(VIKING_HEX)),
            mast:                  materials.add(hex(MAST_HEX)),
            oil_slick:             materials.add(hex(OIL_SLICK_HEX)),
            skull_flag:            materials.add(hex(SKULL_FLAG_HEX)),
            boarder:               materials.add(hex(BOARDER_HEX)),
            boarding_rope:         materials.add(hex(BOARDING_ROPE_HEX)),
            sail:                  materials.add(hex(SAIL_HEX)),
            plane_hull:            materials.add(hex(PLANE_HEX)),
            bullet_missile_outer:  materials.add(hex(MISSILE_HEX)),
            bullet_missile_inner:  materials.add(hex(MISSILE_BRIGHT_HEX)),
            mine_outer:            materials.add(hex(MINE_OUTER_HEX)),
            mine_inner:            materials.add(hex(MINE_INNER_HEX)),
            heal:                  materials.add(hex(HEAL_HEX)),
            ally_flag:             materials.add(Color::WHITE),
            fire:                  materials.add(hex(FIRE_HEX)),
            frost:                 materials.add(hex(FROST_HEX)),
            shock:                 materials.add(hex(SHOCK_HEX)),
            bleed:                 materials.add(hex(BLEED_HEX)),
            // Map tints: opaque pre-blended colors. Alpha-blended translucent
            // tints over a fan-triangulated mesh leave faint visible "rays"
            // along each fan-edge (the alpha math doesn't perfectly
            // reconcile at triangle seams in Bevy 2D), so we bake the blend
            // result into a solid color and render Opaque — same look,
            // zero seam artifacts.
            //
            // Each color = `lerp(ocean, target, 0.30)`, computed against the
            // AAP-64 naval ocean (0.255, 0.651, 0.965). If the palette
            // changes at runtime these stay frozen — a follow-up could
            // recompute them in `apply_palette` if that becomes important.
            // Map tints: alpha-blended so the ocean reads through. Vibrant
            // source colors at moderate alpha — green needs less because
            // it lands closer to ocean's hue, red needs more because the
            // ocean's blue otherwise drags it toward purple.
            map_owned:             materials.add(ColorMaterial {
                color: Color::srgba(0.18, 0.98, 0.40, 0.45),
                alpha_mode: bevy::sprite::AlphaMode2d::Blend,
                ..default()
            }),
            map_enemy:             materials.add(ColorMaterial {
                color: Color::srgba(1.00, 0.05, 0.15, 0.55),
                alpha_mode: bevy::sprite::AlphaMode2d::Blend,
                ..default()
            }),
            // Divider: kept opaque for clean seams — it's a thin ribbon, so
            // there's no perceived "block" feel even without translucence.
            map_divider:           materials.add(ColorMaterial {
                color: Color::srgb(0.18, 0.42, 0.60),
                alpha_mode: bevy::sprite::AlphaMode2d::Opaque,
                ..default()
            }),
            // Slot box: neutral mid-grey so it reads as a "placeholder"
            // affordance over either the green-owned or red-enemy tint.
            map_slot:              materials.add(ColorMaterial {
                color: Color::srgb(0.30, 0.32, 0.36),
                alpha_mode: bevy::sprite::AlphaMode2d::Opaque,
                ..default()
            }),
            // Star marks above the slot: gold/yellow to match `UI_VALUE`
            // and pop on both ocean tones.
            map_slot_star:         materials.add(ColorMaterial {
                color: Color::srgb(1.00, 0.85, 0.30),
                alpha_mode: bevy::sprite::AlphaMode2d::Opaque,
                ..default()
            }),
            // Splash spray: very-light blue, almost white. Reads as water
            // mist against the deep-ocean fill. Opaque since the particles
            // are tiny enough that translucency isn't needed.
            splash:                materials.add(ColorMaterial {
                color: Color::srgb(0.85, 0.95, 1.00),
                alpha_mode: bevy::sprite::AlphaMode2d::Opaque,
                ..default()
            }),
        }
    }
}

/// Camera markers needed by `apply_palette` to set clear color. Re-exported
/// here so the system signature doesn't depend on rendering internals.
#[derive(Component)]
pub struct PlayCamera;
#[derive(Component)]
pub struct UpscaleCamera;
/// Dedicated camera for the map view — spawned alongside `PlayCamera` and
/// targets the same render image, but only renders `MAP_LAYER` entities.
/// Toggled active/inactive by `apply_view_mode`; combat and map never
/// render simultaneously, so there's no z-conflict at the target.
#[derive(Component)]
pub struct MapCamera;
/// Native-resolution HUD camera. Lives here (not in `rendering`) so
/// systems in `modes` / `rune` can query it for camera-follow without
/// pulling a circular dependency on the rendering module.
#[derive(Component)]
pub struct HudCamera;

/// Push the current `Palette` into shared materials + camera clear color
/// whenever the resource is changed (and once on first frame).
pub fn apply_palette(
    palette: Res<Palette>,
    pm: Option<Res<PaletteMaterials>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut cameras: Query<&mut Camera, Or<(With<PlayCamera>, With<UpscaleCamera>, With<MapCamera>)>>,
) {
    if !palette.is_changed() { return; }
    let Some(pm) = pm else { return; };
    let pairs: [(&Handle<ColorMaterial>, Color); 13] = [
        (&pm.border,                palette.border),
        (&pm.hull,                  palette.hull),
        (&pm.hull_accent,           palette.hull_accent),
        (&pm.turret,                palette.turret),
        (&pm.enemy,                 palette.enemy),
        (&pm.enemy_accent,          palette.enemy_accent),
        (&pm.enemy_heavy,           darken(palette.enemy, 0.55)),
        (&pm.enemy_scout,           lighten(palette.enemy, 0.30)),
        (&pm.bullet_friendly,       lighten(palette.bullet_friendly, BULLET_INNER_LIGHTEN)),
        (&pm.bullet_enemy,          lighten(palette.bullet_enemy, BULLET_INNER_LIGHTEN)),
        (&pm.bullet_friendly_outer, palette.bullet_friendly),
        (&pm.bullet_enemy_outer,    palette.bullet_enemy),
        (&pm.trail,                 palette.trail),
    ];
    for (h, c) in pairs {
        if let Some(m) = materials.get_mut(h) { m.color = c; }
    }
    for mut cam in &mut cameras {
        cam.clear_color = ClearColorConfig::Custom(palette.ocean);
    }
}
