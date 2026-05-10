//! LHS damage-contribution panel.
//!
//! One row per damage source — 8 turret slots + every `ShipClass` for
//! allies. Layout is fully modular: rows are spawned via the
//! `spawn_row` helper and tagged with a `DamageRow { source }` so
//! adding new source kinds (boss-side cannons, player abilities, …)
//! is one match-arm in `DamageSource` + one extra spawn call.
//!
//! Per-row composition:
//! - **Icon column** — for player-slot rows, a single small circle
//!   tinted with the slot's currently-equipped weapon color (live-synced
//!   from `TurretConfig` so re-equipping in the shop updates it). For
//!   ally rows, a small circle in the class's hull color.
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
use crate::bullet::DamageSource;
use crate::customize::{turret_barrel_color_for, turret_color_for};
use crate::turret::TurretConfig;

use super::DamageStats;

#[derive(Component)]
pub struct DamagePanelRoot;

#[derive(Component, Clone, Copy)]
pub struct DamageRow { pub source: DamageSource }

#[derive(Component, Clone, Copy)]
pub struct DamageBarFill { pub source: DamageSource }

#[derive(Component, Clone, Copy)]
pub struct DamagePctText { pub source: DamageSource }

/// Tag for the circular base of a player-slot turret icon. Live-synced
/// to the equipped weapon's color each frame.
#[derive(Component, Clone, Copy)]
pub struct DamageRowSlotIcon { pub slot: u8 }

/// Tag for one of the three barrel rectangles on a player-slot turret
/// icon. Visibility + color synced from `TurretConfig` so single /
/// twin / triple barrels read at a glance.
#[derive(Component, Clone, Copy)]
pub struct DamageRowSlotBarrel { pub slot: u8, pub idx: u8 }

// Sized to match the in-game turret silhouette's proportions
// (`Circle::new(2.0)` base + `Rectangle::new(1.5, 4.0)` barrels) but
// scaled so the barrels visibly *extend past* the base — otherwise
// the icon reads as "a circle with stripes on top" instead of "a
// turret". Barrels are oriented along the +X axis (pointing right)
// so the panel turrets all face the bar — reads as "shooting toward
// the damage bar to the right".
//
// Geometry: barrel rectangles are LONG along X (= `BARREL_LEN`) and
// THIN along Y (= `BARREL_THICK`); the three of them stack on Y at
// `BARREL_Y_SPREAD` offsets. Base disc sits centred vertically and
// left of centre so the barrels can extend past its right edge.
const ROW_HEIGHT: f32 = 26.0;
const ROW_GAP: f32 = 3.0;
const ICON_W: f32 = 32.0;
const ICON_H: f32 = ROW_HEIGHT;
/// Mini-turret base radius. Tuned smaller relative to the barrels so
/// the silhouette doesn't blur into a single dot.
const BASE_R: f32 = 6.0;
/// Barrel dims rotated 90° from the in-game vertical orientation —
/// LEN along X (right-facing), THICK along Y. Length is the long
/// axis of the barrel; thickness is the short axis.
const BARREL_LEN: f32 = 18.0;
const BARREL_THICK: f32 = 4.0;
/// X of the barrel's left edge inside the icon. Tuned so the
/// barrel-tip sits ~6 px past the base's right edge → unmistakably
/// a turret that's facing right.
const BARREL_LEFT: f32 = 1.0;
const BARREL_Y_SPREAD: f32 = 5.5;
/// Ally-row hull marker: capsule shape (rounded rectangle) sized
/// like a top-down ship hull so the row reads as "ally ship" rather
/// than a generic dot. Taller than wide; class-tinted background.
const ALLY_HULL_W: f32 = 8.0;
const ALLY_HULL_H: f32 = 18.0;
const LABEL_W: f32 = 56.0;
const BAR_W: f32 = 90.0;
const BAR_H: f32 = 5.0;
const PCT_W: f32 = 30.0;
const FONT: f32 = 11.0;

pub fn setup_damage_panel(mut commands: Commands) {
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
                spawn_row(root, DamageSource::PlayerSlot(slot));
            }
            for &class in ShipClass::ALL {
                spawn_row(root, DamageSource::Ally(class));
            }
        });
}

/// Spawn one row. Modular — adding a new source kind = a new spawn
/// call. Player-slot rows show a mini turret diagram and skip the
/// label column (the diagram is the identifier); ally rows show a
/// class-tinted dot plus a class label.
fn spawn_row(parent: &mut ChildSpawnerCommands, source: DamageSource) {
    parent
        .spawn((
            Node {
                height: Val::Px(ROW_HEIGHT),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(4.0),
                ..default()
            },
            // Visibility added explicitly so `update_damage_row_icons`
            // can hide unequipped player-slot rows.
            Visibility::Inherited,
            DamageRow { source },
        ))
        .with_children(|row| {
            match source {
                DamageSource::PlayerSlot(slot) => spawn_turret_icon(row, slot),
                DamageSource::Ally(class) => {
                    spawn_ally_icon(row, class);
                    // Ally rows keep their text label — the dot alone
                    // doesn't disambiguate `Carrier` vs `Submarine`.
                    row.spawn((
                        Text::new(class.label().to_string()),
                        TextFont { font_size: FONT, font_smoothing: FontSmoothing::None, ..default() },
                        TextColor(Color::srgb(0.55, 0.60, 0.70)),
                        Node { width: Val::Px(LABEL_W), ..default() },
                    ));
                }
            }
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

/// Mini turret diagram facing right: 3 horizontal barrel rectangles
/// stacked vertically + a circle base on top of their inboard ends.
/// All three barrels are spawned and toggled per-frame based on
/// `slot.barrels` (1 = middle only, 2 = port + stbd, 3 = all).
/// Colors live-sync from the equipped weapon so re-equipping in the
/// shop updates the indicator immediately.
fn spawn_turret_icon(row: &mut ChildSpawnerCommands, slot: u8) {
    row.spawn((
        Node {
            width: Val::Px(ICON_W),
            height: Val::Px(ICON_H),
            position_type: PositionType::Relative,
            ..default()
        },
        BackgroundColor(Color::NONE),
    ))
    .with_children(|icon| {
        // Barrels first so the base renders on top of their rears.
        // Barrels run along +X (right-facing); the three indices
        // stack vertically with the centre barrel on the icon mid-y.
        for idx in 0..3u8 {
            let dy = match idx {
                0 => -BARREL_Y_SPREAD,
                2 =>  BARREL_Y_SPREAD,
                _ =>  0.0,
            };
            let top = ICON_H * 0.5 + dy - BARREL_THICK * 0.5;
            icon.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(BARREL_LEFT),
                    top: Val::Px(top),
                    width: Val::Px(BARREL_LEN),
                    height: Val::Px(BARREL_THICK),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.55, 0.60, 0.70)),
                Visibility::Hidden,
                DamageRowSlotBarrel { slot, idx },
            ));
        }
        // Circular base on top, slightly left of centre so the
        // barrels can extend past the base's right edge.
        let base_cx = BARREL_LEFT + BASE_R * 0.6;
        icon.spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(base_cx - BASE_R),
                top: Val::Px(ICON_H * 0.5 - BASE_R),
                width: Val::Px(BASE_R * 2.0),
                height: Val::Px(BASE_R * 2.0),
                ..default()
            },
            BackgroundColor(Color::srgb(0.55, 0.60, 0.70)),
            BorderRadius::all(Val::Px(BASE_R)),
            DamageRowSlotIcon { slot },
        ));
    });
}

/// Class-tinted capsule "hull" — taller than wide with rounded ends,
/// reading as a top-down ship rather than a generic dot. Distinct
/// from the player-slot turret diagrams which use circle+barrels.
fn spawn_ally_icon(row: &mut ChildSpawnerCommands, class: ShipClass) {
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
                width: Val::Px(ALLY_HULL_W),
                height: Val::Px(ALLY_HULL_H),
                ..default()
            },
            BackgroundColor(class_color(class)),
            // Half-width corner radius makes the rectangle into a
            // proper capsule — rounded top/bottom caps, straight
            // sides — matching the in-game `Capsule2d` hull shape.
            BorderRadius::all(Val::Px(ALLY_HULL_W * 0.5)),
        ));
    });
}

/// Approximate per-class hull tint. Hardcoded per-variant; covers every
/// `ShipClass` so no fallback is needed.
fn class_color(class: ShipClass) -> Color {
    match class {
        ShipClass::PirateShip => Color::srgb(0.55, 0.32, 0.18), // brown
        ShipClass::Blackbeard => Color::srgb(0.10, 0.10, 0.12), // near-black
        ShipClass::Tender     => Color::srgb(0.92, 0.92, 0.95), // white
        ShipClass::Carrier    => Color::srgb(0.45, 0.50, 0.40), // olive
        ShipClass::Submarine  => Color::srgb(0.30, 0.40, 0.50), // steel
        ShipClass::Minelayer  => Color::srgb(0.55, 0.50, 0.20), // dirty yellow
        ShipClass::OilTanker  => Color::srgb(0.55, 0.18, 0.18), // industrial red
        ShipClass::Viking     => Color::srgb(0.45, 0.22, 0.13), // wood-brown
    }
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

/// Live-sync each row's existence-in-layout from current world
/// state. Rows collapse out of the flex column entirely when their
/// source is inactive — `Display::None` (not just `Visibility::Hidden`)
/// so the panel behaves like a `VBoxContainer`: equipped/active rows
/// stack tightly with no gaps where unused slots would sit.
///
/// Row activity rules:
///   - **Player slot N**: shown iff `cfg.slots[N].equipped`.
///   - **Ally class C**: shown iff at least one `Ally` with `class = C`
///     is currently alive AND that class has dealt non-zero damage
///     this stage. Both conditions together avoid the "just-spawned
///     but hasn't contributed yet" and "dead but had a percentage"
///     edge cases.
pub fn update_damage_row_icons(
    cfg: Res<TurretConfig>,
    stats: Res<DamageStats>,
    allies: Query<&crate::ally::Ally>,
    mut bases: Query<
        (&DamageRowSlotIcon, &mut BackgroundColor),
        (Without<DamageRowSlotBarrel>, Without<DamageRow>),
    >,
    mut barrels: Query<
        (&DamageRowSlotBarrel, &mut BackgroundColor, &mut Visibility),
        (Without<DamageRowSlotIcon>, Without<DamageRow>),
    >,
    mut rows: Query<
        (&DamageRow, &mut Node),
        (Without<DamageRowSlotIcon>, Without<DamageRowSlotBarrel>),
    >,
) {
    // Bases.
    for (icon, mut bg) in &mut bases {
        let s = &cfg.slots[icon.slot as usize];
        if s.equipped {
            let want = turret_color_for(s.weapon);
            if bg.0 != want { bg.0 = want; }
        }
    }
    // Barrels.
    for (b, mut bg, mut vis) in &mut barrels {
        let s = &cfg.slots[b.slot as usize];
        let visible = s.equipped && barrel_visible_for(s.barrels.max(1), b.idx);
        let want_v = if visible { Visibility::Inherited } else { Visibility::Hidden };
        if *vis != want_v { *vis = want_v; }
        if visible {
            let want = turret_barrel_color_for(s.weapon);
            if bg.0 != want { bg.0 = want; }
        }
    }

    // Snapshot which ally classes are currently alive.
    let mut alive_class = [false; ShipClass::COUNT];
    for ally in &allies {
        alive_class[ally.class.to_index()] = true;
    }

    // Toggle each row's `display` so collapsed rows take zero space
    // — that's the VBox-container behaviour: visible rows fill the
    // column tightly, inactive rows vanish.
    for (row, mut node) in &mut rows {
        let active = match row.source {
            DamageSource::PlayerSlot(slot) => cfg.slots[slot as usize].equipped,
            DamageSource::Ally(class) => {
                let idx = class.to_index();
                alive_class[idx] && stats.per_ally[idx] > 0
            }
        };
        let want = if active { Display::Flex } else { Display::None };
        if node.display != want { node.display = want; }
    }
}

/// Mirror of `sync_turret_config`'s barrel-visibility table — single
/// barrel uses the middle slot, twin uses port + stbd, triple uses
/// all three.
fn barrel_visible_for(count: u8, idx: u8) -> bool {
    match (count, idx) {
        (1, 1)         => true,
        (2, 0) | (2, 2) => true,
        (3, _)         => true,
        _              => false,
    }
}

fn amount_for(source: DamageSource, stats: &DamageStats) -> u64 {
    match source {
        DamageSource::PlayerSlot(s) => stats.per_slot[s as usize],
        DamageSource::Ally(c) => stats.per_ally[c.to_index()],
    }
}

/// Visibility gate: shown during live combat + the StageComplete
/// buffer so the player can read the just-finished round's totals
/// before the shop opens.
pub fn sync_damage_panel_visibility(
    state: Res<State<crate::AppState>>,
    view: Res<crate::map::ViewMode>,
    mut q: Query<&mut Visibility, With<DamagePanelRoot>>,
) {
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
