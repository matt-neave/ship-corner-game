//! LHS turret-control debug panel.
//!
//! Each turret slot gets a row: a mini-ship schematic highlighting which
//! turret this row controls, an Equip button that cycles weapons, and ±
//! steppers for damage / fire rate / barrel count / first-rune-socket.
//! Plus a per-slot "share of damage" bar pulled from `DamageStats`.
//!
//! The customize overlay is the production loadout UI; this panel is
//! kept around as a debug surface (fast iteration on stats without
//! drag-drop). Hidden in desktop mode + while customize is open.

use bevy::ecs::hierarchy::ChildSpawnerCommands;
use bevy::prelude::*;

use crate::balance::{HULL_LEN, HULL_WIDTH, TURRET_NAME_KEYS, TURRET_POSITIONS};
use crate::i18n::tr;
use crate::palette::{
    UI_ACTIVE_BG, UI_BG, UI_BTN_BG, UI_DOT_ON, UI_EQUIP_BG,
    UI_ROW_BG, UI_ROW_DIV, UI_TEXT, UI_TEXT_DIM, UI_VALUE,
};
use crate::turret::TurretConfig;
use crate::rune::rune_display;

use super::{ButtonKind, DamageStats, SlotButton};

// ---------- Marker components ----------

/// LHS control panel root. Toggled hidden in desktop mode + while the
/// customize overlay is open.
#[derive(Component)]
pub struct UiPanel;

/// Per-slot fill node of the white "share of damage" bar in each slot row.
#[derive(Component)]
pub struct SlotDamageBar { pub slot: usize }

/// Per-slot percentage readout next to the share bar.
#[derive(Component)]
pub struct SlotShareText { pub slot: usize }

/// Tag on a text node whose contents are driven by `update_slot_labels`.
#[derive(Component)]
pub struct SlotLabel { pub slot: usize, pub kind: LabelKind }

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LabelKind { Damage, Rate, Status, Barrels, Rune }

// ---------- Setup ----------

/// Spawn the LHS turret-control panel. Called by `setup_ui` in mod.rs.
pub fn setup_panel(commands: &mut Commands) {
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            top: Val::Px(0.0),
            width: Val::Px(crate::balance::UI_WIDTH),
            height: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            padding: UiRect::all(Val::Px(6.0)),
            row_gap: Val::Px(3.0),
            ..default()
        },
        BackgroundColor(UI_BG),
        UiPanel,
    ))
    .with_children(|p| {
        // Header row: title + stacked WAVE / CRT / NIGHT / DESKTOP toggles.
        p.spawn(Node {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::SpaceBetween,
            ..default()
        })
        .with_children(|h| {
            h.spawn(Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(1.0),
                ..default()
            })
            .with_children(|t| {
                t.spawn((
                    Text::new(tr("title_battleship")),
                    TextFont { font_size: 12.0, ..default() },
                    TextColor(UI_TEXT),
                ));
                t.spawn((
                    Text::new(tr("subtitle_cuniberti")),
                    TextFont { font_size: 8.0, ..default() },
                    TextColor(UI_TEXT_DIM),
                ));
            });
            h.spawn(Node {
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(3.0),
                ..default()
            })
            .with_children(|btns| {
                spawn_header_button(btns, tr("btn_crt"),     ButtonKind::ToggleCrtMode);
                spawn_header_button(btns, tr("btn_night"),   ButtonKind::ToggleNightMode);
                spawn_header_button(btns, tr("btn_desktop"), ButtonKind::ToggleDesktopMode);
            });
        });
        // 1px divider line.
        p.spawn((
            Node {
                width: Val::Percent(100.0),
                height: Val::Px(1.0),
                ..default()
            },
            BackgroundColor(UI_ROW_DIV),
        ));

        // Scrollable list of 8 turret slot rows. flex_grow:1 makes it claim
        // all remaining vertical space below the header; Overflow::scroll_y()
        // lets the player wheel-scroll when the rows don't fit the window.
        p.spawn(Node {
            flex_grow: 1.0,
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(2.0),
            overflow: Overflow::scroll_y(),
            ..default()
        })
        .with_children(|scroll| {
            for slot in 0..8 {
                spawn_slot_row(scroll, slot);
            }
        });
    });
}

// ---------- Slot row builders ----------

fn spawn_slot_row(parent: &mut ChildSpawnerCommands, slot: usize) {
    parent.spawn((
        Node {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Stretch,
            column_gap: Val::Px(6.0),
            padding: UiRect::all(Val::Px(4.0)),
            ..default()
        },
        BackgroundColor(UI_ROW_BG),
    ))
    .with_children(|row| {
        spawn_ship_schematic(row, slot);
        spawn_slot_controls(row, slot);
    });
}

// ---- Mini-ship schematic ----
//
// A scaled silhouette of the actual hull (`HULL_WIDTH × HULL_LEN`) with 8
// cannon indicators dropped onto it at `TURRET_POSITIONS`. Rendered the same
// way the in-world ship is built — capsule hull (rounded ends) + circular
// cannon bases — but at low pixel resolution so the rounded edges read as
// chunky pixel-art arcs rather than smooth curves.
//
// `CANNON_DOT = 8` is sized so adjacent cannons leave roughly 1 px between
// edges, matching the in-game tight packing. Mid-pair turrets sit ~2 px
// outboard of the hull rectangle, echoing the in-game wider mid-beam.
//
// `SCHEMATIC_SCALE = 2` is integer so every `TURRET_POSITIONS` entry maps
// to a whole-pixel UI position. World y points up, UI y points down — the
// mapping flips.
const SCHEMATIC_SCALE: f32 = 2.0;
const SCHEMATIC_HULL_W: f32 = HULL_WIDTH * SCHEMATIC_SCALE;
const SCHEMATIC_HULL_H: f32 = HULL_LEN * SCHEMATIC_SCALE;
const CANNON_DOT: f32 = 8.0;
const SCHEMATIC_CANNON_OVERHANG: f32 = 2.0;
const SCHEMATIC_OUTER_MARGIN: f32 = 2.0;
const SCHEMATIC_W: f32 =
    SCHEMATIC_HULL_W + (SCHEMATIC_CANNON_OVERHANG + SCHEMATIC_OUTER_MARGIN) * 2.0;
const SCHEMATIC_H: f32 = SCHEMATIC_HULL_H + SCHEMATIC_OUTER_MARGIN * 2.0;

fn spawn_ship_schematic(parent: &mut ChildSpawnerCommands, slot: usize) {
    let hull_offset_x = (SCHEMATIC_W - SCHEMATIC_HULL_W) / 2.0;
    let hull_offset_y = (SCHEMATIC_H - SCHEMATIC_HULL_H) / 2.0;
    let center_x = SCHEMATIC_W / 2.0;
    let center_y = SCHEMATIC_H / 2.0;

    parent.spawn(Node {
        width:  Val::Px(SCHEMATIC_W),
        height: Val::Px(SCHEMATIC_H),
        position_type: PositionType::Relative,
        flex_shrink: 0.0,
        ..default()
    })
    .with_children(|s| {
        // Capsule hull — rounded short ends matching the in-game `Capsule2d`.
        s.spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(hull_offset_x),
                top:  Val::Px(hull_offset_y),
                width:  Val::Px(SCHEMATIC_HULL_W),
                height: Val::Px(SCHEMATIC_HULL_H),
                ..default()
            },
            BackgroundColor(UI_TEXT_DIM),
            BorderRadius::all(Val::Px(SCHEMATIC_HULL_W / 2.0)),
        ));

        // Circular cannon indicators — rounded disks at scaled turret positions.
        for (i, &(wx, wy)) in TURRET_POSITIONS.iter().enumerate() {
            let dot_x = center_x + wx * SCHEMATIC_SCALE - CANNON_DOT / 2.0;
            let dot_y = center_y - wy * SCHEMATIC_SCALE - CANNON_DOT / 2.0;
            let color = if i == slot { UI_DOT_ON } else { UI_BG };
            s.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(dot_x),
                    top:  Val::Px(dot_y),
                    width:  Val::Px(CANNON_DOT),
                    height: Val::Px(CANNON_DOT),
                    ..default()
                },
                BackgroundColor(color),
                BorderRadius::all(Val::Px(CANNON_DOT / 2.0)),
            ));
        }
    });
}

fn spawn_slot_controls(parent: &mut ChildSpawnerCommands, slot: usize) {
    parent.spawn(Node {
        flex_direction: FlexDirection::Column,
        flex_grow: 1.0,
        row_gap: Val::Px(2.0),
        ..default()
    })
    .with_children(|c| {
        // Slot title row: "01  BOW".
        c.spawn(Node {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: Val::Px(4.0),
            ..default()
        })
        .with_children(|t| {
            t.spawn((
                Text::new(format!("{:02}", slot + 1)),
                TextFont { font_size: 9.0, ..default() },
                TextColor(UI_TEXT_DIM),
            ));
            t.spawn((
                Text::new(tr(TURRET_NAME_KEYS[slot])),
                TextFont { font_size: 10.0, ..default() },
                TextColor(UI_TEXT),
            ));
        });

        // Equip / Active button. Sharp corners, flat fill — pixel-game feel.
        c.spawn((
            Button,
            Node {
                width: Val::Percent(100.0),
                padding: UiRect::axes(Val::Px(2.0), Val::Px(2.0)),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(if slot == 0 { UI_ACTIVE_BG } else { UI_EQUIP_BG }),
            SlotButton { slot, kind: ButtonKind::Equip },
        ))
        .with_children(|b| {
            b.spawn((
                Text::new(if slot == 0 { tr("weapon_standard") } else { tr("btn_equip_gun") }),
                TextFont { font_size: 9.0, ..default() },
                TextColor(UI_TEXT),
                SlotLabel { slot, kind: LabelKind::Status },
            ));
        });

        // DMG / RATE / BRRL stat rows + share bar.
        spawn_stat_row(c, slot, tr("stat_dmg"),  "1",   LabelKind::Damage,
                       ButtonKind::DamageDown, ButtonKind::DamageUp);
        spawn_stat_row(c, slot, tr("stat_rate"), "4.0", LabelKind::Rate,
                       ButtonKind::RateDown,   ButtonKind::RateUp);
        spawn_stat_row(c, slot, tr("stat_brrl"), "1",   LabelKind::Barrels,
                       ButtonKind::BarrelsDown, ButtonKind::BarrelsUp);
        spawn_stat_row(c, slot, tr("stat_rune"), tr("rune_none"), LabelKind::Rune,
                       ButtonKind::RuneDown,    ButtonKind::RuneUp);
        spawn_share_row(c, slot);
    });
}

fn spawn_share_row(parent: &mut ChildSpawnerCommands, slot: usize) {
    parent.spawn(Node {
        flex_direction: FlexDirection::Row,
        align_items: AlignItems::Center,
        column_gap: Val::Px(3.0),
        ..default()
    })
    .with_children(|r| {
        r.spawn((
            Text::new(tr("stat_share")),
            TextFont { font_size: 8.0, ..default() },
            TextColor(UI_TEXT_DIM),
            Node { width: Val::Px(26.0), ..default() },
        ));
        // Track: dark fill spanning the remaining row width.
        r.spawn((
            Node {
                flex_grow: 1.0,
                height: Val::Px(4.0),
                ..default()
            },
            BackgroundColor(UI_ROW_DIV),
        ))
        .with_children(|track| {
            track.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(0.0),
                    top: Val::Px(0.0),
                    width: Val::Percent(0.0),
                    height: Val::Percent(100.0),
                    ..default()
                },
                BackgroundColor(Color::WHITE),
                SlotDamageBar { slot },
            ));
        });
        r.spawn((
            Text::new("0%"),
            TextFont { font_size: 9.0, ..default() },
            TextColor(UI_TEXT),
            Node { width: Val::Px(22.0), ..default() },
            SlotShareText { slot },
        ));
    });
}

fn spawn_stat_row(
    parent: &mut ChildSpawnerCommands,
    slot: usize,
    label: &str,
    initial: &str,
    label_kind: LabelKind,
    down_kind: ButtonKind,
    up_kind: ButtonKind,
) {
    parent.spawn(Node {
        flex_direction: FlexDirection::Row,
        align_items: AlignItems::Center,
        column_gap: Val::Px(3.0),
        ..default()
    })
    .with_children(|r| {
        r.spawn((
            Text::new(label.to_string()),
            TextFont { font_size: 8.0, ..default() },
            TextColor(UI_TEXT_DIM),
            Node { width: Val::Px(26.0), ..default() },
        ));
        r.spawn((
            Text::new(initial.to_string()),
            TextFont { font_size: 9.0, ..default() },
            TextColor(UI_VALUE),
            SlotLabel { slot, kind: label_kind },
            Node { width: Val::Px(22.0), ..default() },
        ));
        spawn_step_button(r, slot, down_kind, "−");
        spawn_step_button(r, slot, up_kind,   "+");
    });
}

/// Header toggle button — thin 1px outline, transparent fill, sharp corners.
/// Reads as text-on-screen rather than a UI chrome panel.
fn spawn_header_button(parent: &mut ChildSpawnerCommands, label: &str, kind: ButtonKind) {
    parent.spawn((
        Button,
        Node {
            padding: UiRect::axes(Val::Px(4.0), Val::Px(2.0)),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            border: UiRect::all(Val::Px(1.0)),
            ..default()
        },
        BackgroundColor(Color::NONE),
        BorderColor(UI_TEXT_DIM),
        SlotButton { slot: 0, kind },
    ))
    .with_children(|b| {
        b.spawn((
            Text::new(label.to_string()),
            TextFont { font_size: 8.0, ..default() },
            TextColor(UI_TEXT),
        ));
    });
}

/// −/+ button — small flat square, sharp corners.
fn spawn_step_button(parent: &mut ChildSpawnerCommands, slot: usize, kind: ButtonKind, label: &str) {
    parent.spawn((
        Button,
        Node {
            width: Val::Px(14.0),
            height: Val::Px(12.0),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            ..default()
        },
        BackgroundColor(UI_BTN_BG),
        SlotButton { slot, kind },
    ))
    .with_children(|b| {
        b.spawn((
            Text::new(label.to_string()),
            TextFont { font_size: 11.0, ..default() },
            TextColor(UI_TEXT),
        ));
    });
}

// ---------- Update systems ----------

pub fn update_slot_labels(
    cfg: Res<TurretConfig>,
    mut q: Query<(&SlotLabel, &mut Text, &mut TextColor)>,
) {
    if !cfg.is_changed() { return; }
    for (lbl, mut t, mut tc) in &mut q {
        let s = cfg.slots[lbl.slot];
        match lbl.kind {
            LabelKind::Damage  => **t = format!("{}", s.damage),
            LabelKind::Rate    => {
                // Show the LIVE rate the turret actually fires at — base
                // × adjacent-Booster multiplier — not the raw config
                // value. Without this the panel always reads the base
                // rate even when a Booster sits next door, leading the
                // player to think the boost isn't working. Tint green
                // when boosted so the buff is unmistakeable.
                let boost = crate::booster::boost_multiplier_for_slot(&cfg, lbl.slot);
                let rate = s.fire_rate * boost;
                **t = format!("{:.1}", rate);
                let want = if boost > 1.0 {
                    Color::srgb(0.55, 0.95, 0.55)
                } else {
                    UI_TEXT
                };
                if tc.0 != want { tc.0 = want; }
            }
            LabelKind::Barrels => **t = format!("{}", s.barrels),
            LabelKind::Status  => {
                **t = if s.equipped {
                    s.weapon.label().into()
                } else {
                    tr("btn_equip_gun").into()
                };
            }
            LabelKind::Rune    => **t = rune_display(s.runes[0]).into(),
        }
    }
}

pub fn update_damage_bars(
    stats: Res<DamageStats>,
    mut bars: Query<(&SlotDamageBar, &mut Node)>,
    mut texts: Query<(&SlotShareText, &mut Text)>,
) {
    if !stats.is_changed() { return; }
    let total = stats.total.max(1) as f32;
    for (bar, mut node) in &mut bars {
        let dealt = stats.per_slot[bar.slot] as f32;
        let pct = (dealt / total).clamp(0.0, 1.0) * 100.0;
        node.width = Val::Percent(pct);
    }
    for (st, mut text) in &mut texts {
        let dealt = stats.per_slot[st.slot] as f32;
        let pct = ((dealt / total).clamp(0.0, 1.0) * 100.0).round() as u32;
        **text = format!("{}%", pct);
    }
}
