//! All UI: LHS turret control panel, top score/wave banner, top-left HP bar
//! (Wave mode), bottom draft prompt + cards (Drafting phase). Plus the
//! per-slot damage-share bars and the click handlers that drive everything.
//!
//! UI marker components live here; `modes.rs` and `pier.rs` reach in to
//! toggle visibility on a few of them via `pub` exports (`ScoreText`,
//! `UiPanel`, `WaveHpUi/Fill/Text`, `DraftPanel`).

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::ecs::hierarchy::ChildSpawnerCommands;
use bevy::prelude::*;

use crate::balance::{
    FRIENDLY_HP_WAVE, HULL_LEN, HULL_WIDTH, TURRET_NAME_KEYS, TURRET_POSITIONS, UI_WIDTH,
};
use crate::components::{Friendly, Health};
use crate::i18n::tr;
use crate::modes::{CrtMode, DesktopHint, GameMode, NightMode, VsyncMode, WindowMode};
use crate::palette::{
    UI_ACTIVE_BG, UI_BG, UI_BTN_BG, UI_DOT_ON, UI_EQUIP_BG,
    UI_ROW_BG, UI_ROW_DIV, UI_TEXT, UI_TEXT_DIM, UI_VALUE,
};
use crate::pier::{spawn_draft_card, DraftPanel, PierVisual};
use crate::turret::TurretConfig;
use crate::wave::WaveState;
use crate::weapon::WeaponType;
use crate::Score;

// ---------- Marker components ----------

/// Top-center "SCORE N" / "WAVE N" text. Toggled visible by `apply_window_mode`.
#[derive(Component)]
pub struct ScoreText;

/// LHS control panel root. Toggled hidden in desktop mode.
#[derive(Component)]
pub struct UiPanel;

/// Per-slot fill node of the white "share of damage" bar in each slot row.
#[derive(Component)]
pub struct SlotDamageBar { pub slot: usize }

/// Per-slot percentage readout next to the share bar.
#[derive(Component)]
pub struct SlotShareText { pub slot: usize }

/// Tag on every clickable button in the LHS panel + header. `slot` is the
/// turret index for slot-specific buttons; ignored (use 0) for header toggles.
#[derive(Component)]
pub struct SlotButton { pub slot: usize, pub kind: ButtonKind }

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    Equip,
    DamageUp, DamageDown,
    RateUp, RateDown,
    BarrelsUp, BarrelsDown,
    ToggleDesktopMode,
    ToggleNightMode,
    ToggleCrtMode,
    ToggleWaveMode,
    ToggleVsync,
}

/// Tag on a text node whose contents are driven by `update_slot_labels`.
#[derive(Component)]
pub struct SlotLabel { pub slot: usize, pub kind: LabelKind }

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LabelKind { Damage, Rate, Status, Barrels }

/// Top-right FPS counter, driven by `FrameTimeDiagnosticsPlugin`.
#[derive(Component)]
pub struct FpsText;

/// Label inside the VSYNC toggle button. Updated by `update_vsync_label`
/// whenever `VsyncMode.enabled` flips.
#[derive(Component)]
pub struct VsyncLabel;

/// Top-left HP-bar container; visible only in Wave mode.
#[derive(Component)]
pub struct WaveHpUi;
#[derive(Component)]
pub struct WaveHpFill;
#[derive(Component)]
pub struct WaveHpText;

// ---------- Resources ----------

/// Per-slot tally of damage actually dealt to enemies (overkill is clamped to
/// the enemy's remaining HP so a sniper one-shotting a 1-HP enemy doesn't
/// inflate its share). The LHS UI bars read this each frame; bullets / beams
/// write to it.
#[derive(Resource, Default)]
pub struct DamageStats {
    pub per_slot: [u64; 8],
    pub total: u64,
}

// ---------- Setup ----------

pub fn setup_ui(mut commands: Commands) {
    // Score / wave banner — pixel-game compact, top-center over the play area.
    commands.spawn((
        Text::new(format!("{} 0", tr("score_label"))),
        TextFont { font_size: 22.0, ..default() },
        TextColor(UI_VALUE),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(6.0),
            left: Val::Px(UI_WIDTH),
            right: Val::Px(0.0),
            justify_content: JustifyContent::Center,
            ..default()
        },
        ScoreText,
    ));

    // FPS counter — small, dim, top-right corner. Driven by
    // `FrameTimeDiagnosticsPlugin` (registered in main.rs).
    commands.spawn((
        Text::new("FPS --"),
        TextFont { font_size: 9.0, ..default() },
        TextColor(UI_TEXT_DIM),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(4.0),
            right: Val::Px(6.0),
            ..default()
        },
        FpsText,
    ));

    // VSYNC toggle — directly below the FPS counter, same minimal pixel
    // styling as the LHS panel header buttons. Click to flip
    // `VsyncMode.enabled`; `apply_vsync_mode` updates the window's
    // present_mode and `update_vsync_label` rewrites the text below.
    commands.spawn((
        Button,
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(18.0),
            right: Val::Px(6.0),
            padding: UiRect::axes(Val::Px(4.0), Val::Px(2.0)),
            border: UiRect::all(Val::Px(1.0)),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            ..default()
        },
        BackgroundColor(Color::NONE),
        BorderColor(UI_TEXT_DIM),
        SlotButton { slot: 0, kind: ButtonKind::ToggleVsync },
    ))
    .with_children(|b| {
        b.spawn((
            Text::new("VSYNC OFF"),
            TextFont { font_size: 8.0, ..default() },
            TextColor(UI_TEXT),
            VsyncLabel,
        ));
    });

    // Wave-mode draft panel — bottom of screen, hidden unless WavePhase::Drafting.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            bottom: Val::Px(14.0),
            left: Val::Px(UI_WIDTH),
            right: Val::Px(0.0),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            row_gap: Val::Px(4.0),
            ..default()
        },
        Visibility::Hidden,
        DraftPanel,
    ))
    .with_children(|p| {
        p.spawn((
            Text::new(tr("draft_instruction")),
            TextFont { font_size: 9.0, ..default() },
            TextColor(UI_TEXT_DIM),
        ));
        p.spawn(Node {
            flex_direction: FlexDirection::Row,
            column_gap: Val::Px(8.0),
            ..default()
        })
        .with_children(|row| {
            for i in 0..3u8 {
                spawn_draft_card(row, i);
            }
        });
    });

    // Wave-mode HP bar — top-left of the play area, hidden in sandbox.
    // No rounding anywhere; thin pixel-grid feel.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(10.0),
            left: Val::Px(UI_WIDTH + 10.0),
            width: Val::Px(140.0),
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: Val::Px(5.0),
            ..default()
        },
        Visibility::Hidden,
        WaveHpUi,
    ))
    .with_children(|p| {
        p.spawn((
            Text::new(tr("hp_label")),
            TextFont { font_size: 10.0, ..default() },
            TextColor(UI_TEXT_DIM),
        ));
        // Track + fill — sharp pixel rectangle, no border-radius.
        p.spawn((
            Node {
                flex_grow: 1.0,
                height: Val::Px(5.0),
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
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    ..default()
                },
                BackgroundColor(Color::WHITE),
                WaveHpFill,
            ));
        });
        p.spawn((
            Text::new(format!("{}/{}", FRIENDLY_HP_WAVE, FRIENDLY_HP_WAVE)),
            TextFont { font_size: 9.0, ..default() },
            TextColor(UI_TEXT),
            WaveHpText,
        ));
    });

    // Desktop-mode hint — small grey text at top-center, hidden by default.
    commands.spawn((
        Text::new(tr("btn_press_esc")),
        TextFont { font_size: 8.0, ..default() },
        TextColor(Color::srgb(0.7, 0.72, 0.78)),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(3.0),
            left: Val::Px(0.0),
            right: Val::Px(0.0),
            justify_content: JustifyContent::Center,
            ..default()
        },
        Visibility::Hidden,
        DesktopHint,
    ));

    // Left control panel — fixed width, full window height, sharp corners.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            top: Val::Px(0.0),
            width: Val::Px(UI_WIDTH),
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
                spawn_header_button(btns, tr("btn_wave"),    ButtonKind::ToggleWaveMode);
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
// **Sizing logic**: `CANNON_DOT = 8` is chosen so adjacent cannons (bow ↔
// fore wing pair, stern ↔ aft wing pair — closest neighbours at √(4²+8²)
// ≈ 8.94 px center-to-center) leave roughly **1 px between turret edges**,
// matching the in-game tight packing. The wider mid-pair (12 px apart) ends
// up with a few px of gap by design — that's the ship's wider mid-beam.
//
// Mid-pair turrets sit ~2 px **outboard of the hull rectangle** in the
// schematic — this echoes the in-game ship where the mid turret bases hang
// off the hull's wider beam. The schematic width adds 2 px of overhang
// per side + 2 px outer margin so cannons aren't flush with the edge.
//
// `SCHEMATIC_SCALE = 2` is integer so every `TURRET_POSITIONS` entry (all
// integers) maps to a whole-pixel UI position. Cannons stay crisp on the
// pixel grid.
//
// World y points up; UI y points down, so the mapping flips.
//
// Active slot's cannon is `UI_DOT_ON` (yellow); the other 7 are `UI_BG` so
// they read as dark studs against the hull color.
const SCHEMATIC_SCALE:        f32 = 2.0;
const SCHEMATIC_HULL_W:       f32 = HULL_WIDTH * SCHEMATIC_SCALE;        // 16
const SCHEMATIC_HULL_H:       f32 = HULL_LEN   * SCHEMATIC_SCALE;        // 44
const CANNON_DOT:             f32 = 8.0;
/// Mid-pair turret half-width minus hull half-width (in scaled px). Tells us
/// how far cannons stick out past the hull on each side: `3*scale + 4 - 8 = 2`.
const SCHEMATIC_CANNON_OVERHANG: f32 = 2.0;
const SCHEMATIC_OUTER_MARGIN:    f32 = 2.0;
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
        // `border_radius = SCHEMATIC_HULL_W/2` makes the ends full semicircles.
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

        // Circular cannon indicators — rounded disks at scaled turret
        // positions. `border_radius = CANNON_DOT/2` gives a full circle.
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
        // `update_slot_labels` keeps the inner text in sync; the click handler
        // cycles weapon types.
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
        // Track: dark fill spanning the remaining row width. Sharp pixel rect.
        // Fill child is absolutely positioned so width can scale 0–100% of the
        // track without disturbing flex layout.
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

pub fn update_score_text(
    score: Res<Score>,
    mode: Res<GameMode>,
    wave: Res<WaveState>,
    mut q: Query<&mut Text, With<ScoreText>>,
) {
    if !score.is_changed() && !mode.is_changed() && !wave.is_changed() { return; }
    for mut t in &mut q {
        **t = match *mode {
            GameMode::Sandbox => format!("{} {}", tr("score_label"), score.0),
            GameMode::Wave    => format!("{} {}", tr("wave_label"),  wave.wave.max(1)),
        };
    }
}

pub fn ui_button_system(
    mut interactions: Query<(&Interaction, &SlotButton), Changed<Interaction>>,
    mut cfg: ResMut<TurretConfig>,
    mut window_mode: ResMut<WindowMode>,
    mut night: ResMut<NightMode>,
    mut crt: ResMut<CrtMode>,
    mut game_mode: ResMut<GameMode>,
    mut vsync: ResMut<VsyncMode>,
) {
    for (interaction, btn) in &mut interactions {
        if !matches!(*interaction, Interaction::Pressed) { continue; }
        match btn.kind {
            ButtonKind::ToggleDesktopMode => {
                window_mode.desktop = !window_mode.desktop;
                continue;
            }
            ButtonKind::ToggleNightMode => {
                night.active = !night.active;
                continue;
            }
            ButtonKind::ToggleCrtMode => {
                crt.active = !crt.active;
                continue;
            }
            ButtonKind::ToggleWaveMode => {
                *game_mode = match *game_mode {
                    GameMode::Sandbox => GameMode::Wave,
                    GameMode::Wave    => GameMode::Sandbox,
                };
                continue;
            }
            ButtonKind::ToggleVsync => {
                vsync.enabled = !vsync.enabled;
                continue;
            }
            _ => {}
        }
        let s = &mut cfg.slots[btn.slot];
        match btn.kind {
            ButtonKind::ToggleDesktopMode | ButtonKind::ToggleNightMode
            | ButtonKind::ToggleCrtMode  | ButtonKind::ToggleWaveMode
            | ButtonKind::ToggleVsync => unreachable!(),
            ButtonKind::Equip => {
                // Cycle: unequipped → Standard → Sniper → MG → Shotgun →
                // Railgun → unequipped. On each step, snap stats to the new
                // weapon's defaults so the type's identity is felt immediately.
                // Player can still tweak with ±.
                if !s.equipped {
                    s.equipped = true;
                    s.weapon = WeaponType::Standard;
                    let (d, r) = s.weapon.defaults();
                    s.damage = d; s.fire_rate = r; s.barrels = 1;
                } else {
                    match s.weapon.next() {
                        Some(next) => {
                            s.weapon = next;
                            let (d, r) = next.defaults();
                            s.damage = d; s.fire_rate = r; s.barrels = 1;
                        }
                        None => {
                            s.equipped = false;
                            s.weapon = WeaponType::Standard;
                            let (d, r) = s.weapon.defaults();
                            s.damage = d; s.fire_rate = r; s.barrels = 1;
                        }
                    }
                }
            }
            ButtonKind::DamageUp    => { if s.equipped { s.damage += 1; } }
            ButtonKind::DamageDown  => { if s.equipped && s.damage > 1 { s.damage -= 1; } }
            ButtonKind::RateUp      => { if s.equipped { s.fire_rate += 0.1; } }
            ButtonKind::RateDown    => { if s.equipped && s.fire_rate > 0.2 { s.fire_rate -= 0.1; } }
            ButtonKind::BarrelsUp   => { if s.equipped && s.barrels < 3 { s.barrels += 1; } }
            ButtonKind::BarrelsDown => { if s.equipped && s.barrels > 1 { s.barrels -= 1; } }
        }
    }
}

pub fn update_slot_labels(
    cfg: Res<TurretConfig>,
    mut q: Query<(&SlotLabel, &mut Text)>,
) {
    if !cfg.is_changed() { return; }
    for (lbl, mut t) in &mut q {
        let s = cfg.slots[lbl.slot];
        match lbl.kind {
            LabelKind::Damage  => **t = format!("{}", s.damage),
            LabelKind::Rate    => **t = format!("{:.1}", s.fire_rate),
            LabelKind::Barrels => **t = format!("{}", s.barrels),
            LabelKind::Status  => {
                **t = if s.equipped {
                    s.weapon.label().into()
                } else {
                    tr("btn_equip_gun").into()
                };
            }
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

/// FPS counter. Computes a true arithmetic mean over Bevy's diagnostic
/// history and writes it to the top-right text node at 4 Hz.
///
/// Why not `Diagnostic::smoothed()`? That's an EMA with a per-**frame**
/// smoothing factor (1/60 by default). At 240 fps it fully converges in
/// ~0.25 s, so it tracks recent noise rather than a stable average — a
/// single 33 ms hitch swings the displayed value to "30 fps" for the next
/// reading even though perceptually nothing happened. A simple mean over
/// the full history (120 samples by default → 0.5–2 s of frames) damps
/// that out cleanly.
///
/// **1% low**: 1st-percentile of the FPS history. Catches stutters the
/// average hides — same metric modern benchmarks report.
pub fn update_fps_text(
    diagnostics: Res<DiagnosticsStore>,
    time: Res<Time>,
    mut refresh_timer: Local<f32>,
    mut q: Query<&mut Text, With<FpsText>>,
) {
    *refresh_timer -= time.delta_secs();
    if *refresh_timer > 0.0 { return; }
    *refresh_timer = 0.25;

    let Some(diag) = diagnostics.get(&FrameTimeDiagnosticsPlugin::FPS) else { return; };
    let history: Vec<f64> = diag.values().copied().collect();
    if history.is_empty() { return; }

    let avg = history.iter().sum::<f64>() / history.len() as f64;

    // 1% low — sort ascending, take the value at the 1st percentile index.
    let mut sorted = history;
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((sorted.len() as f64 * 0.01) as usize).min(sorted.len() - 1);
    let one_pct_low = sorted[idx];

    for mut t in &mut q {
        **t = format!("FPS {:.0}\n1%LOW {:.0}", avg, one_pct_low);
    }
}

/// Rewrite the VSYNC button label whenever the resource flips. Only does
/// work on flips (cheap to check otherwise).
pub fn update_vsync_label(
    vsync: Res<VsyncMode>,
    mut q: Query<&mut Text, With<VsyncLabel>>,
) {
    if !vsync.is_changed() { return; }
    for mut t in &mut q {
        **t = if vsync.enabled { "VSYNC ON".into() } else { "VSYNC OFF".into() };
    }
}

/// Show / hide the pier grid + HP bar based on game mode, and update the HP
/// fill width + numeric readout each frame in Wave mode.
pub fn update_wave_ui(
    mode: Res<GameMode>,
    friendly: Query<&Health, With<Friendly>>,
    mut pier_q: Query<&mut Visibility, (With<PierVisual>, Without<WaveHpUi>)>,
    mut hp_ui_q: Query<&mut Visibility, (With<WaveHpUi>, Without<PierVisual>)>,
    mut hp_fill_q: Query<&mut Node, With<WaveHpFill>>,
    mut hp_text_q: Query<&mut Text, With<WaveHpText>>,
) {
    let want = matches!(*mode, GameMode::Wave);
    if mode.is_changed() {
        let target = if want { Visibility::Inherited } else { Visibility::Hidden };
        for mut v in &mut pier_q  { if *v != target { *v = target; } }
        for mut v in &mut hp_ui_q { if *v != target { *v = target; } }
    }
    if !want { return; }
    let Ok(h) = friendly.single() else { return; };
    let pct = (h.0 as f32 / FRIENDLY_HP_WAVE as f32 * 100.0).clamp(0.0, 100.0);
    for mut n in &mut hp_fill_q {
        n.width = Val::Percent(pct);
    }
    for mut t in &mut hp_text_q {
        **t = format!("{}/{}", h.0.max(0), FRIENDLY_HP_WAVE);
    }
}
