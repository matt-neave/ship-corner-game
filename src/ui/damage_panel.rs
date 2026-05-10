//! LHS damage-contribution panel.
//!
//! One row per damage source — 8 turret slots + every `ShipClass` for
//! allies. Layout is fully modular: rows are spawned via the
//! `spawn_row` helper and tagged with a `DamageRow { source }` so
//! adding new source kinds (boss-side cannons, player abilities, …)
//! is one match-arm in `DamageSource` + one extra spawn call.
//!
//! Per-row composition:
//! - **Icon column** — for player-slot rows, a tiny ship hull (capsule)
//!   with a single bright dot at the slot's position so a glance tells
//!   you which gun is talking. For ally rows, a small square in the
//!   class's hull color.
//! - **Label column** — short text abbreviation (`BOW`, `F.P`,
//!   `PIRATE`, `BLKBEARD`, …).
//! - **Bar track** — thin grey bar with a white fill child whose
//!   width-percent is rewritten each frame from `DamageStats`.
//! - **Percent column** — exact readout next to the bar.
//!
//! Rows for ally classes always exist but their bar shows 0% until
//! that class actually deals damage. Reset on `OnExit(MainMenu)` and
//! `OnExit(Customize)` keeps the slate clean per round while still
//! showing the just-finished round's totals during the shop.

use bevy::prelude::*;
use bevy::text::FontSmoothing;

use crate::ally::ShipClass;
use crate::balance::TURRET_NAME_KEYS;
use crate::bullet::DamageSource;
use crate::i18n::tr;
use crate::palette::Palette;

use super::DamageStats;

#[derive(Component)]
pub struct DamagePanelRoot;

#[derive(Component, Clone, Copy)]
pub struct DamageRow { pub source: DamageSource }

#[derive(Component, Clone, Copy)]
pub struct DamageBarFill { pub source: DamageSource }

#[derive(Component, Clone, Copy)]
pub struct DamagePctText { pub source: DamageSource }

const ROW_HEIGHT: f32 = 14.0;
const ROW_GAP: f32 = 2.0;
const ICON_W: f32 = 28.0;
const ICON_H: f32 = ROW_HEIGHT;
const HULL_W: f32 = 24.0;
const HULL_H: f32 = 6.0;
/// Highlighted (active-slot) dot diameter. The other 7 slots render
/// at `DOT_DIM_SIZE` so the full layout is legible but the row's
/// own slot reads at a glance.
const DOT_ACTIVE_SIZE: f32 = 4.0;
const DOT_DIM_SIZE: f32 = 2.0;
const ALLY_MARK: f32 = 6.0;
const LABEL_W: f32 = 38.0;
const BAR_W: f32 = 70.0;
const BAR_H: f32 = 4.0;
const PCT_W: f32 = 26.0;
const FONT: f32 = 9.0;

pub fn setup_damage_panel(mut commands: Commands, palette: Res<Palette>) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(72.0),
                left: Val::Px(8.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(ROW_GAP),
                ..default()
            },
            BackgroundColor(Color::NONE),
            ZIndex(40),
            Visibility::Hidden,
            DamagePanelRoot,
        ))
        .with_children(|root| {
            for slot in 0..8u8 {
                spawn_row(root, DamageSource::PlayerSlot(slot), &palette);
            }
            for &class in ShipClass::ALL {
                spawn_row(root, DamageSource::Ally(class), &palette);
            }
        });
}

/// Spawn one row. Modular — adding a new source kind = a new spawn
/// call. The icon column branches on the source so player-slot rows
/// get the mini ship + slot dot and ally rows get a class-color
/// square; everything else (label, bar, percent) is identical.
fn spawn_row(parent: &mut ChildSpawnerCommands, source: DamageSource, palette: &Palette) {
    parent
        .spawn((
            Node {
                height: Val::Px(ROW_HEIGHT),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(4.0),
                ..default()
            },
            DamageRow { source },
        ))
        .with_children(|row| {
            spawn_icon(row, source, palette);
            // Label.
            row.spawn((
                Text::new(label_for(source)),
                TextFont { font_size: FONT, font_smoothing: FontSmoothing::None, ..default() },
                TextColor(Color::srgb(0.55, 0.60, 0.70)),
                Node { width: Val::Px(LABEL_W), ..default() },
            ));
            // Bar track + white fill.
            row.spawn((
                Node {
                    width: Val::Px(BAR_W),
                    height: Val::Px(BAR_H),
                    ..default()
                },
                BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.10)),
            ))
            .with_children(|track| {
                track.spawn((
                    Node {
                        width: Val::Percent(0.0),
                        height: Val::Percent(100.0),
                        ..default()
                    },
                    BackgroundColor(Color::WHITE),
                    DamageBarFill { source },
                ));
            });
            // Percent text.
            row.spawn((
                Text::new("0%"),
                TextFont { font_size: FONT, font_smoothing: FontSmoothing::None, ..default() },
                TextColor(Color::srgb(0.94, 0.95, 0.97)),
                Node { width: Val::Px(PCT_W), ..default() },
                DamagePctText { source },
            ));
        });
}

fn spawn_icon(row: &mut ChildSpawnerCommands, source: DamageSource, palette: &Palette) {
    match source {
        DamageSource::PlayerSlot(slot) => spawn_ship_icon(row, slot, palette),
        DamageSource::Ally(class) => spawn_ally_icon(row, class, palette),
    }
}

/// Mini diagram of the player's ship, mirroring the customize-shop
/// layout: capsule hull + all 8 turret slots at their real positions.
/// The row's own slot is the bright accent dot; the other 7 are dim
/// so the layout reads as a ship while the active slot still pops.
fn spawn_ship_icon(row: &mut ChildSpawnerCommands, active_slot: u8, palette: &Palette) {
    row.spawn((
        Node {
            width: Val::Px(ICON_W),
            height: Val::Px(ICON_H),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        },
        BackgroundColor(Color::NONE),
    ))
    .with_children(|icon| {
        icon.spawn((
            Node {
                width: Val::Px(HULL_W),
                height: Val::Px(HULL_H),
                position_type: PositionType::Relative,
                ..default()
            },
            BackgroundColor(palette.hull),
            BorderRadius::all(Val::Px(HULL_H * 0.5)),
        ))
        .with_children(|hull| {
            for slot in 0..8u8 {
                spawn_slot_dot(hull, slot, slot == active_slot);
            }
        });
    });
}

fn spawn_slot_dot(hull: &mut ChildSpawnerCommands, slot: u8, active: bool) {
    let (dx, dy) = slot_dot_offset(slot);
    let (size, color) = if active {
        (DOT_ACTIVE_SIZE, Color::srgb(1.0, 0.85, 0.30))
    } else {
        (DOT_DIM_SIZE,    Color::srgba(0.94, 0.95, 0.97, 0.55))
    };
    // Hull's own coord space has top-left at (0, 0) and centre at
    // (HULL_W/2, HULL_H/2). Slot dots position themselves around the
    // centre via the signed offsets.
    let cx = HULL_W * 0.5 + dx - size * 0.5;
    let cy = HULL_H * 0.5 + dy - size * 0.5;
    hull.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(cx),
            top: Val::Px(cy),
            width: Val::Px(size),
            height: Val::Px(size),
            ..default()
        },
        BackgroundColor(color),
        BorderRadius::all(Val::Px(size * 0.5)),
    ));
}

/// Class-color square in lieu of a per-class silhouette. Compact and
/// visually distinct from the player-ship hull marker.
fn spawn_ally_icon(row: &mut ChildSpawnerCommands, class: ShipClass, palette: &Palette) {
    row.spawn((
        Node {
            width: Val::Px(ICON_W),
            height: Val::Px(ICON_H),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        },
        BackgroundColor(Color::NONE),
    ))
    .with_children(|icon| {
        icon.spawn((
            Node {
                width: Val::Px(ALLY_MARK),
                height: Val::Px(ALLY_MARK),
                ..default()
            },
            BackgroundColor(class_color(class, palette)),
            BorderRadius::all(Val::Px(ALLY_MARK * 0.5)),
        ));
    });
}

/// Approximate per-class hull tint. Falls back to the player palette's
/// hull color when the class doesn't carry an explicit override (the
/// ones that *do* are PirateShip/Blackbeard/Tender per
/// `palette::PaletteMaterials::hull_for_class`, but pulling that
/// requires the materials handles — `Palette` is cheaper here).
fn class_color(class: ShipClass, palette: &Palette) -> Color {
    match class {
        ShipClass::PirateShip => Color::srgb(0.55, 0.32, 0.18), // brown
        ShipClass::Blackbeard => Color::srgb(0.10, 0.10, 0.12), // near-black
        ShipClass::Tender     => Color::srgb(0.92, 0.92, 0.95), // white
        ShipClass::Carrier    => Color::srgb(0.45, 0.50, 0.40), // olive
        ShipClass::Submarine  => Color::srgb(0.30, 0.40, 0.50), // steel
        ShipClass::Minelayer  => Color::srgb(0.55, 0.50, 0.20), // dirty yellow
        // _ => palette.hull, // every variant covered, kept for future-proofing
        #[allow(unreachable_patterns)]
        _ => palette.hull,
    }
}

/// Slot positions inside the 24×6 mini hull. Source values live in
/// `balance::TURRET_POSITIONS` (game coords, +Y bow / +X starboard);
/// the mini orients bow → +X UI and starboard → +Y UI (bevy_ui +Y is
/// down), so we map `(gx, gy)` → `(gy * sx, gx * sy)`.
fn slot_dot_offset(slot: u8) -> (f32, f32) {
    let (gx, gy) = crate::balance::TURRET_POSITIONS[slot as usize];
    let sx = HULL_W / 18.0; // hull length in game units
    let sy = HULL_H / 4.0;  // hull width in game units
    (gy * sx, gx * sy)
}

fn label_for(source: DamageSource) -> String {
    match source {
        DamageSource::PlayerSlot(s) => slot_short_label(s),
        DamageSource::Ally(c) => c.short_label().to_string(),
    }
}

/// Read translated slot name and compress multi-word names to
/// initials so the column stays narrow ("FORE PORT" → "F.P").
fn slot_short_label(slot: u8) -> String {
    let s = tr(TURRET_NAME_KEYS[slot as usize]);
    let words: Vec<&str> = s.split_whitespace().collect();
    if words.len() <= 1 { return s.to_string(); }
    words
        .iter()
        .filter_map(|w| w.chars().next())
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(".")
}

pub fn update_damage_panel(
    stats: Res<DamageStats>,
    mut bars: Query<(&DamageBarFill, &mut Node), Without<DamagePctText>>,
    mut texts: Query<(&DamagePctText, &mut Text), Without<DamageBarFill>>,
) {
    let total = stats.total.max(1) as f32;
    for (fill, mut node) in &mut bars {
        let pct = amount_for(fill.source, &stats) as f32 / total * 100.0;
        let want = Val::Percent(pct);
        if node.width != want { node.width = want; }
    }
    for (txt, mut text) in &mut texts {
        let pct = (amount_for(txt.source, &stats) as f32 / total * 100.0).round() as i32;
        let s = format!("{}%", pct);
        if text.0 != s { text.0 = s; }
    }
}

fn amount_for(source: DamageSource, stats: &DamageStats) -> u64 {
    match source {
        DamageSource::PlayerSlot(s) => stats.per_slot[s as usize],
        DamageSource::Ally(c) => stats.per_ally[c.to_index()],
    }
}

/// Visibility gate: panel shows only during live combat. Hidden in
/// menus, shop, pause, and on the (deprecated) map view.
pub fn sync_damage_panel_visibility(
    state: Res<State<crate::AppState>>,
    view: Res<crate::map::ViewMode>,
    mut q: Query<&mut Visibility, With<DamagePanelRoot>>,
) {
    // Visible during live combat AND during the StageComplete buffer
    // so the player can read the just-finished round's totals before
    // the shop opens.
    let s = *state.get();
    let want = if matches!(s, crate::AppState::Playing | crate::AppState::StageComplete)
        && *view == crate::map::ViewMode::Combat
    {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    for mut v in &mut q {
        if *v != want { *v = want; }
    }
}

/// Reset hook. Called as a Bevy system; takes only `DamageStats`.
pub fn reset_damage_stats(mut stats: ResMut<DamageStats>) {
    stats.per_slot = [0; 8];
    stats.per_ally = [0; ShipClass::COUNT];
    stats.total = 0;
}
