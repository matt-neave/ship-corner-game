//! Build-summary panel — a compact read-only roll-up of the player's
//! current loadout: every equipped turret (weapon + barrels + runes)
//! and every purchased mod (with stack count). Reused by the pause
//! overlay, the game-over screen, and the win screen so the same
//! visual vocabulary follows the player across run-end moments.
//!
//! Visual style is borrowed from the lobby / main-menu chrome
//! (`ui_kit::chunky_*` + `SURFACE_RAISED` cards, `CHUNKY_BORDER_W`
//! outlines, `CHUNKY_RADIUS` corners) so the panel reads as a
//! sibling of those polished screens rather than ad-hoc UI.
//!
//! API: [`spawn_build_summary`] takes a `ChildSpawnerCommands` parent
//! and the run state (turret config + purchased mods) and emits the
//! entire panel as a single bevy_ui subtree. No systems, no markers
//! that outlive the parent — the caller's existing teardown (e.g.
//! `exit_game_over`) sweeps the panel away alongside its other
//! children.

use bevy::prelude::*;
use bevy::text::FontSmoothing;
use bevy::window::PrimaryWindow;

use crate::customize::drag::{mod_tooltip_body, ModRarity, PurchasedMods, MOD_LIBRARY};
use crate::customize::{rune_color_for, turret_color_for};
use crate::rune::Rune;
use crate::turret::{SlotCfg, TurretConfig};
use crate::ui_kit::theme;
use crate::weapon::WeaponType;

// ---------- Tooltip markers ----------

/// Marker on any hoverable element inside a build-summary panel.
/// Carries enough info for the global tooltip system to look up
/// the right title + body when the cursor lands on the entity.
#[derive(Component, Clone, Copy)]
pub enum BuildSummaryTip {
    Weapon(WeaponType),
    Rune(Rune),
    /// Index into [`MOD_LIBRARY`] — same convention as `ShopMod`.
    Mod(usize),
}

/// Root of the singleton tooltip overlay. Spawned once at startup;
/// the per-frame update system writes title + body + position +
/// visibility based on which (if any) `BuildSummaryTip` element
/// is currently hovered.
#[derive(Component)]
pub struct BuildSummaryTooltipRoot;

#[derive(Component)]
pub struct BuildSummaryTooltipTitle;

#[derive(Component)]
pub struct BuildSummaryTooltipBody;

/// Spawn the build-summary panel as a child of `parent`. Pass the
/// live [`TurretConfig`] + [`PurchasedMods`] snapshot; the panel
/// reads once and is static thereafter (callers that need
/// "live" behaviour should despawn and respawn on change).
///
/// The fonts are `Option` because most caller sites take them as
/// `Option<Res<...>>` (fonts can be mid-load on the first frame
/// after a state transition); we fall back to default Bevy text
/// rather than skipping the panel entirely.
pub fn spawn_build_summary(
    parent: &mut bevy::ecs::hierarchy::ChildSpawnerCommands,
    cfg: &TurretConfig,
    purchased: &PurchasedMods,
    pixel: Option<&crate::fonts::PixelFont>,
    thaleah: Option<&crate::fonts::ThaleahFont>,
) {
    // Outer chunky card — matches the lobby's `SURFACE_RAISED` slab
    // with a `CHUNKY_OUTLINE` border and a comfortable inner padding.
    parent
        .spawn((
            Node {
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Stretch,
                padding: UiRect::all(Val::Px(theme::PAD_LG)),
                border: UiRect::all(Val::Px(theme::CHUNKY_BORDER_W)),
                row_gap: Val::Px(theme::GAP_MD),
                min_width: Val::Px(440.0),
                max_width: Val::Px(520.0),
                ..default()
            },
            BackgroundColor(theme::SURFACE_RAISED),
            BorderColor(theme::CHUNKY_OUTLINE),
            BorderRadius::all(Val::Px(theme::CHUNKY_RADIUS)),
        ))
        .with_children(|card| {
            // ---------- Title row ----------
            // Thaleah accent title — same treatment as LOBBY /
            // VICTORY headings. Underline is a thin rule below.
            card.spawn(Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Baseline,
                justify_content: JustifyContent::SpaceBetween,
                column_gap: Val::Px(theme::GAP_MD),
                ..default()
            })
            .with_children(|hdr| {
                hdr.spawn(title_text("BUILD", 26.0, theme::ACCENT, thaleah));
                let equipped_count = cfg.slots.iter().filter(|s| s.equipped).count();
                let mods_count: u32 = purchased.entries.iter().map(|(_, c)| *c).sum();
                hdr.spawn(body_text(
                    format!("{} TURRETS  |  {} MODS", equipped_count, mods_count),
                    theme::FONT_MD,
                    theme::ON_SURFACE_DIM,
                    pixel,
                ));
            });

            // ---------- TURRETS section ----------
            card.spawn(body_text("TURRETS", theme::FONT_MD, theme::ON_SURFACE_DIM, pixel));
            // 4x2 grid of slot tiles. Always all 8 — unequipped
            // slots render as a dim "EMPTY" placeholder so the
            // grid stays a stable shape across the run.
            for row_start in [0usize, 4] {
                card.spawn(Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Stretch,
                    column_gap: Val::Px(theme::GAP_SM),
                    ..default()
                })
                .with_children(|row| {
                    for slot_idx in row_start..(row_start + 4) {
                        spawn_slot_tile(row, slot_idx, &cfg.slots[slot_idx], pixel);
                    }
                });
            }

            // ---------- MODS section ----------
            card.spawn(body_text("MODS", theme::FONT_MD, theme::ON_SURFACE_DIM, pixel));
            card.spawn(Node {
                flex_direction: FlexDirection::Row,
                flex_wrap: FlexWrap::Wrap,
                align_items: AlignItems::Center,
                column_gap: Val::Px(theme::GAP_SM),
                row_gap: Val::Px(theme::GAP_SM),
                ..default()
            })
            .with_children(|flow| {
                if purchased.entries.is_empty() {
                    flow.spawn(body_text(
                        "NONE",
                        theme::FONT_SM,
                        theme::ON_SURFACE_DIM,
                        pixel,
                    ));
                    return;
                }
                for &(spec_idx, count) in &purchased.entries {
                    spawn_mod_chip(flow, spec_idx, count, pixel);
                }
            });
        });
}

// ---------- Internals ----------

fn spawn_slot_tile(
    row: &mut bevy::ecs::hierarchy::ChildSpawnerCommands,
    slot_idx: usize,
    slot: &SlotCfg,
    pixel: Option<&crate::fonts::PixelFont>,
) {
    let (border, fill, label, dim_label) = if slot.equipped {
        let c = turret_color_for(slot.weapon);
        (c, theme::CHUNKY_FILL, short_weapon_label(slot.weapon), false)
    } else {
        (theme::BORDER_SUBTLE, theme::CHUNKY_FILL, "EMPTY".to_string(), true)
    };
    let label_color = if dim_label { theme::ON_SURFACE_DIM } else { border };
    // Tile bundle. Equipped slots get a `Button` + tooltip marker
    // so hovering pops the weapon description; unequipped slots
    // stay inert (no tooltip needed — "EMPTY" is self-evident).
    let mut tile = row.spawn((
        Node {
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::FlexStart,
            padding: UiRect::axes(Val::Px(theme::PAD_SM), Val::Px(theme::PAD_SM)),
            border: UiRect::all(Val::Px(theme::CHUNKY_BORDER_W)),
            row_gap: Val::Px(2.0),
            // Equal-share width via flex_grow so 4 tiles fill the row.
            flex_grow: 1.0,
            flex_basis: Val::Px(0.0),
            min_width: Val::Px(0.0),
            ..default()
        },
        BackgroundColor(fill),
        BorderColor(border),
        BorderRadius::all(Val::Px(theme::CHUNKY_RADIUS)),
    ));
    if slot.equipped {
        tile.insert((Button, BuildSummaryTip::Weapon(slot.weapon)));
    }
    tile.with_children(|tile| {
        // Slot index pill - tiny dim header so the player can map
        // tiles to the boat layout if they care.
        tile.spawn(body_text(
            format!("SLOT {}", slot_idx + 1),
            theme::FONT_XS,
            theme::ON_SURFACE_DIM,
            pixel,
        ));
        // Weapon name (or "EMPTY") tinted by weapon family.
        tile.spawn(body_text(label, theme::FONT_SM, label_color, pixel));
        if slot.equipped {
            // Barrel level (×N).
            tile.spawn(body_text(
                format!("\u{00D7}{}", slot.barrels.max(1)),
                theme::FONT_MD,
                theme::ON_SURFACE,
                pixel,
            ));
        }
        // Rune dots row — three slots, filled ones tinted by rune
        // colour, empty ones a near-invisible outline. Always 3
        // dots so tiles stay vertically aligned.
        tile.spawn(Node {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            column_gap: Val::Px(3.0),
            margin: UiRect::top(Val::Px(2.0)),
            ..default()
        })
        .with_children(|dots| {
            for &rune in slot.runes.iter() {
                spawn_rune_dot(dots, rune);
            }
        });
    });
}

fn spawn_rune_dot(
    dots: &mut bevy::ecs::hierarchy::ChildSpawnerCommands,
    rune: Option<Rune>,
) {
    let (fill, border) = match rune {
        Some(r) => (rune_color_for(r), Color::srgba(0.0, 0.0, 0.0, 0.6)),
        None => (Color::srgba(0.0, 0.0, 0.0, 0.0), Color::srgba(1.0, 1.0, 1.0, 0.18)),
    };
    let mut dot = dots.spawn((
        Node {
            width: Val::Px(8.0),
            height: Val::Px(8.0),
            border: UiRect::all(Val::Px(1.0)),
            ..default()
        },
        BackgroundColor(fill),
        BorderColor(border),
        BorderRadius::all(Val::Px(2.0)),
    ));
    // Filled dots get hover tooltips; empty dots stay inert.
    if let Some(r) = rune {
        dot.insert((Button, BuildSummaryTip::Rune(r)));
    }
}

fn spawn_mod_chip(
    flow: &mut bevy::ecs::hierarchy::ChildSpawnerCommands,
    spec_idx: usize,
    count: u32,
    pixel: Option<&crate::fonts::PixelFont>,
) {
    let Some(spec) = MOD_LIBRARY.get(spec_idx) else { return };
    let border = spec.rarity.border_color();
    flow.spawn((
        Button,
        BuildSummaryTip::Mod(spec_idx),
        Node {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: Val::Px(4.0),
            padding: UiRect::axes(Val::Px(theme::PAD_MD), Val::Px(theme::PAD_SM)),
            border: UiRect::all(Val::Px(theme::CHUNKY_BORDER_W * 0.66)),
            ..default()
        },
        BackgroundColor(theme::CHUNKY_FILL),
        BorderColor(border),
        BorderRadius::all(Val::Px(theme::CHUNKY_RADIUS * 0.5)),
    ))
    .with_children(|chip| {
        chip.spawn(body_text(spec.name, theme::FONT_SM, theme::ON_SURFACE, pixel));
        if count > 1 {
            chip.spawn(body_text(
                format!("\u{00D7}{}", count),
                theme::FONT_SM,
                theme::ACCENT,
                pixel,
            ));
        }
        // Rarity-color tag dot on the right — tiny visual handle
        // so the player can scan rarity composition at a glance.
        let _ = ModRarity::Common; // ensure ModRarity import isn't dead
        chip.spawn((
            Node {
                width: Val::Px(6.0),
                height: Val::Px(6.0),
                ..default()
            },
            BackgroundColor(border),
            BorderRadius::all(Val::Px(2.0)),
        ));
    });
}

// ---------- Helpers ----------

fn body_text(
    text: impl Into<String>,
    size: f32,
    color: Color,
    pixel: Option<&crate::fonts::PixelFont>,
) -> (Text, TextFont, TextColor) {
    let font = match pixel {
        Some(p) => crate::fonts::pixel_text_font(p, size),
        None => TextFont {
            font_size: size,
            font_smoothing: FontSmoothing::None,
            ..default()
        },
    };
    (Text::new(text), font, TextColor(color))
}

fn title_text(
    text: impl Into<String>,
    size: f32,
    color: Color,
    thaleah: Option<&crate::fonts::ThaleahFont>,
) -> (Text, TextFont, TextColor, TextShadow) {
    let font = match thaleah {
        Some(t) => crate::fonts::thaleah_text_font(t, size),
        None => TextFont {
            font_size: size,
            font_smoothing: FontSmoothing::None,
            ..default()
        },
    };
    (
        Text::new(text),
        font,
        TextColor(color),
        TextShadow {
            offset: Vec2::splat(1.5),
            color: Color::srgba(0.0, 0.0, 0.0, 0.85),
        },
    )
}

// ---------- Tooltip overlay ----------

/// Z-index for the tooltip overlay. Sits above the pause (180),
/// game-over (170), and win (190) screens so it floats over any
/// caller of `spawn_build_summary`. Stays below the customize
/// overlay (200) — the build summary isn't shown there anyway,
/// but the layering invariant matters if the resource is ever
/// queried via a separate path.
const TOOLTIP_Z: i32 = 220;

/// Maximum width of the tooltip in design pixels. Body text wraps
/// at word boundaries when it would exceed this; titles stay on
/// a single line.
const TOOLTIP_MAX_W: f32 = 320.0;
/// Pixel offset between the cursor and the nearest tooltip edge.
const TOOLTIP_CURSOR_OFFSET: f32 = 16.0;

/// Spawn the singleton tooltip overlay. One per app instance —
/// the update system writes its title / body / position /
/// visibility each frame. Hidden until a tip-tagged element is
/// hovered.
pub fn setup_build_summary_tooltip(
    mut commands: Commands,
    pixel: Option<Res<crate::fonts::PixelFont>>,
    thaleah: Option<Res<crate::fonts::ThaleahFont>>,
) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(0.0),
                left: Val::Px(0.0),
                max_width: Val::Px(TOOLTIP_MAX_W),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::FlexStart,
                padding: UiRect::all(Val::Px(theme::PAD_MD)),
                border: UiRect::all(Val::Px(theme::CHUNKY_BORDER_W)),
                row_gap: Val::Px(theme::GAP_SM),
                ..default()
            },
            BackgroundColor(theme::SURFACE_RAISED),
            BorderColor(theme::CHUNKY_OUTLINE),
            BorderRadius::all(Val::Px(theme::CHUNKY_RADIUS)),
            ZIndex(TOOLTIP_Z),
            Visibility::Hidden,
            BuildSummaryTooltipRoot,
        ))
        .with_children(|root| {
            // Title — Thaleah accent for the heading line.
            root.spawn((
                title_text("", 18.0, theme::ACCENT, thaleah.as_deref()),
                BuildSummaryTooltipTitle,
            ));
            // Body — pixel font, near-white, wrappable.
            let body = body_text(
                "",
                theme::FONT_MD,
                Color::srgb(0.92, 0.94, 0.97),
                pixel.as_deref(),
            );
            root.spawn((
                body,
                Node {
                    max_width: Val::Px(TOOLTIP_MAX_W - 2.0 * theme::PAD_MD),
                    ..default()
                },
                BuildSummaryTooltipBody,
            ));
        });
}

/// Per-frame: find the first hovered `BuildSummaryTip` target,
/// describe it into the singleton tooltip, and pin the tooltip
/// to the cursor. Hides the tooltip whenever no tip-tagged
/// element is hovered.
pub fn update_build_summary_tooltip(
    windows: Query<&Window, With<PrimaryWindow>>,
    ui_scale: Res<bevy::ui::UiScale>,
    targets: Query<(&Interaction, &BuildSummaryTip)>,
    mut root_q: Query<
        (&mut Visibility, &mut Node),
        (
            With<BuildSummaryTooltipRoot>,
            Without<BuildSummaryTooltipTitle>,
            Without<BuildSummaryTooltipBody>,
        ),
    >,
    mut title_q: Query<
        &mut Text,
        (
            With<BuildSummaryTooltipTitle>,
            Without<BuildSummaryTooltipBody>,
        ),
    >,
    mut body_q: Query<
        &mut Text,
        (
            With<BuildSummaryTooltipBody>,
            Without<BuildSummaryTooltipTitle>,
        ),
    >,
) {
    // Pick the first Hovered (or Pressed) target. Multiple hovers
    // can coexist with overlapping nested elements; iterator order
    // is deterministic enough for a tooltip — and the user can
    // always nudge the cursor.
    let hovered = targets.iter().find_map(|(i, t)| match i {
        Interaction::Hovered | Interaction::Pressed => Some(*t),
        Interaction::None => None,
    });
    let Some(tip) = hovered else {
        for (mut v, _) in &mut root_q {
            if *v != Visibility::Hidden {
                *v = Visibility::Hidden;
            }
        }
        return;
    };
    let (title, body) = describe_tip(tip);
    for mut t in &mut title_q {
        if t.0 != title {
            t.0 = title.clone();
        }
    }
    for mut t in &mut body_q {
        if t.0 != body {
            t.0 = body.clone();
        }
    }
    // Pin to cursor — cursor coords are screen pixels, so divide
    // by UiScale per the `/ ui_scale` rule when writing into a
    // bevy_ui `Val::Px`. Offset by `TOOLTIP_CURSOR_OFFSET` so the
    // box doesn't sit directly under the cursor (which would
    // re-trigger hover on the tooltip itself if it had Button).
    let Ok(win) = windows.single() else { return };
    let Some(cursor) = win.cursor_position() else {
        for (mut v, _) in &mut root_q {
            if *v != Visibility::Hidden {
                *v = Visibility::Hidden;
            }
        }
        return;
    };
    let s = ui_scale.0.max(0.0001);
    let win_w_design = win.width() / s;
    let win_h_design = win.height() / s;
    let cursor_x_design = cursor.x / s;
    let cursor_y_design = cursor.y / s;
    // Flip to the left/up if the default right/down position
    // would overflow the window. The box still gets clamped by
    // the window edge if it'd run off, but flipping keeps it
    // anchored to the cursor instead of jamming into the corner.
    let mut left = cursor_x_design + TOOLTIP_CURSOR_OFFSET;
    if left + TOOLTIP_MAX_W > win_w_design {
        left = (cursor_x_design - TOOLTIP_MAX_W - TOOLTIP_CURSOR_OFFSET).max(4.0);
    }
    let mut top = cursor_y_design + TOOLTIP_CURSOR_OFFSET;
    // Conservative height clamp — body wraps, so we don't know
    // the actual height; assume ~160 design px is a safe ceiling.
    if top + 160.0 > win_h_design {
        top = (cursor_y_design - 160.0 - TOOLTIP_CURSOR_OFFSET).max(4.0);
    }
    for (mut v, mut node) in &mut root_q {
        if *v != Visibility::Inherited {
            *v = Visibility::Inherited;
        }
        let want_left = Val::Px(left);
        let want_top = Val::Px(top);
        if node.left != want_left {
            node.left = want_left;
        }
        if node.top != want_top {
            node.top = want_top;
        }
    }
}

fn describe_tip(tip: BuildSummaryTip) -> (String, String) {
    match tip {
        BuildSummaryTip::Weapon(w) => (w.label().to_string(), w.description().to_string()),
        BuildSummaryTip::Rune(r) => (r.label().to_string(), r.description().to_string()),
        BuildSummaryTip::Mod(idx) => {
            let Some(spec) = MOD_LIBRARY.get(idx) else {
                return (String::new(), String::new());
            };
            let title = format!("{}  \u{2014} {}", spec.name, spec.rarity.label());
            (title, mod_tooltip_body(spec))
        }
    }
}

/// Compact 8-char-or-less weapon label for the slot tile. The
/// full `WeaponType::label` strings (e.g. "Spread Rockets") are
/// too long to read inside a 90px-wide tile at FONT_SM, so we
/// short them per-variant. Keep in sync with `WeaponType` —
/// non-exhaustive match would be a build error.
fn short_weapon_label(weapon: WeaponType) -> String {
    match weapon {
        WeaponType::Standard      => "STANDARD",
        WeaponType::Sniper        => "SNIPER",
        WeaponType::MachineGun    => "MG",
        WeaponType::Shotgun       => "SHOTGUN",
        WeaponType::Railgun       => "RAILGUN",
        WeaponType::Mortar        => "MORTAR",
        WeaponType::HeliPad       => "HELIPAD",
        WeaponType::Cannon        => "CANNON",
        WeaponType::Booster       => "BOOSTER",
        WeaponType::Blade         => "BLADE",
        WeaponType::Cage          => "CAGE",
        WeaponType::Harpoon       => "HARPOON",
        WeaponType::SpreadRockets => "ROCKETS",
        WeaponType::Flamethrower  => "FLAME",
        WeaponType::SpikedPlate   => "SPIKES",
        WeaponType::Amplifier     => "AMP",
        WeaponType::SharkNet      => "SHARK",
        WeaponType::AnchorFlail   => "FLAIL",
        WeaponType::PlasmaTorpedo => "PLASMA",
        WeaponType::CrowsNest     => "NEST",
    }.to_string()
}
