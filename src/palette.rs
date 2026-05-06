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
use bevy::window::PrimaryWindow;

use crate::balance::BULLET_INNER_LIGHTEN;

// ---------- Color helpers ----------
pub fn hex(s: &str) -> Color {
    let s = s.trim_start_matches('#');
    let r = u8::from_str_radix(&s[0..2], 16).unwrap_or(255);
    let g = u8::from_str_radix(&s[2..4], 16).unwrap_or(0);
    let b = u8::from_str_radix(&s[4..6], 16).unwrap_or(255);
    Color::srgb(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
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

// ---------- Pier building tints ----------
pub const BUILDING_MUNITIONS_HEX:  &str = "#ff8a3c";
pub const BUILDING_WATCHTOWER_HEX: &str = "#ffe066";
pub const BUILDING_DRYDOCK_HEX:    &str = "#5cb8ff";

// ---------- Ally hull tints ----------
//
// Each `AllyVariant` gets its own identity color, looked up via
// `PaletteMaterials::ally_hull_for` (in `ally.rs`). Fixed hexes — not
// palette-driven — so an ally type's identity stays consistent when the
// game palette swaps.
pub const PIRATE_HEX: &str = "#7a4a2c"; // aged wood brown

// ---------- Status-effect tints ----------
pub const FIRE_HEX:  &str = "#ff8030"; // bright fire orange
pub const FROST_HEX: &str = "#80d8ff"; // cool sky blue (cyan-ish, distinct from fire)
pub const SHOCK_HEX: &str = "#ffe680"; // electric yellow (lightning arc)

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
    /// Ally hull tints — one per `AllyVariant`. Looked up via
    /// `ally::PaletteMaterials::ally_hull_for`.
    pub pirate_hull: Handle<ColorMaterial>,
    /// Fire-rune particle color (also reused for other future fire FX).
    pub fire:  Handle<ColorMaterial>,
    /// Frost-rune particle color (cyan mist).
    pub frost: Handle<ColorMaterial>,
    /// Shock-rune lightning + particle color (electric yellow).
    pub shock: Handle<ColorMaterial>,
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
}

impl PaletteMaterials {
    /// Build all handles from the active palette + fixed weapon hexes.
    pub fn build(palette: &Palette, materials: &mut Assets<ColorMaterial>) -> Self {
        let sniper  = hex(SNIPER_HEX);
        let mg      = hex(MG_HEX);
        let shotgun = hex(SHOTGUN_HEX);
        let railgun = hex(RAILGUN_HEX);
        Self {
            border:                materials.add(palette.border),
            hull:                  materials.add(palette.hull),
            hull_accent:           materials.add(palette.hull_accent),
            turret:                materials.add(palette.turret),
            enemy:                 materials.add(palette.enemy),
            enemy_accent:          materials.add(palette.enemy_accent),
            enemy_heavy:           materials.add(darken(palette.enemy, 0.55)),
            enemy_scout:           materials.add(lighten(palette.enemy, 0.30)),
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
            pirate_hull:           materials.add(hex(PIRATE_HEX)),
            fire:                  materials.add(hex(FIRE_HEX)),
            frost:                 materials.add(hex(FROST_HEX)),
            shock:                 materials.add(hex(SHOCK_HEX)),
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
    let _ = (); // (Window query used to be here; trimmed during refactor.)
}

// Suppress unused-import lint on PrimaryWindow (kept for future use by
// palette-driven window features).
#[allow(dead_code)]
fn _keep_imports() { let _: Option<Window> = None; let _ = std::any::type_name::<PrimaryWindow>(); }
