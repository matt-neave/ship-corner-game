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
use bevy::window::PrimaryWindow;

use crate::balance::{
    FRIENDLY_HP_WAVE, HULL_LEN, HULL_WIDTH, PLAY_INTERNAL, TURRET_NAME_KEYS,
    TURRET_POSITIONS, UI_WIDTH,
};
use crate::ally::Ally;
use crate::components::{Friendly, Health};
use crate::i18n::tr;
use crate::modes::{
    effective_ui_width, play_area_screen_rect,
    CrtMode, DesktopHint, GameMode, NightMode, VsyncMode, WindowMode,
};
use crate::palette::{
    UI_ACTIVE_BG, UI_BG, UI_BTN_BG, UI_DOT_ON, UI_EQUIP_BG,
    UI_ROW_BG, UI_ROW_DIV, UI_TEXT, UI_TEXT_DIM, UI_VALUE,
};
use crate::ui_kit::theme;
use crate::map::ViewMode;
use crate::pier::{spawn_draft_card, DraftPanel, PierVisual};
use crate::rune::{cycle_next, cycle_prev, rune_display};
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
    RuneUp, RuneDown,
    ToggleDesktopMode,
    ToggleNightMode,
    ToggleCrtMode,
    ToggleWaveMode,
    ToggleVsync,
    /// Click → switch to `ViewMode::Map`. Visible only in Combat view.
    ReturnToMap,
}

/// Tag on a text node whose contents are driven by `update_slot_labels`.
#[derive(Component)]
pub struct SlotLabel { pub slot: usize, pub kind: LabelKind }

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LabelKind { Damage, Rate, Status, Barrels, Rune }

/// Top-right FPS counter, driven by `FrameTimeDiagnosticsPlugin`.
#[derive(Component)]
pub struct FpsText;

/// Label inside the VSYNC toggle button. Updated by `update_vsync_label`
/// whenever `VsyncMode.enabled` flips.
#[derive(Component)]
pub struct VsyncLabel;

/// Marker for the "MAP" button — visibility toggled by `update_map_button`
/// so it only appears while in Combat view.
#[derive(Component)]
pub struct ReturnToMapButton;

/// Top-left HP bar container — outer Node holding the "HP" label +
/// track. Naming kept (vs. `ArcadeHpUi` etc.) to avoid an import-ripple
/// rename now that the bar is always-visible rather than wave-only.
#[derive(Component)]
pub struct WaveHpUi;
/// The bar track itself (dark frame). Marked separately so the
/// subdivider system can find it and add tick lines as children.
#[derive(Component)]
pub struct WaveHpTrack;
/// Red fill inside the track — width animated by `update_wave_ui`.
#[derive(Component)]
pub struct WaveHpFill;
/// Numeric readout overlaid centered inside the track.
#[derive(Component)]
pub struct WaveHpText;
/// Vertical tick line inside the track, one per 50-HP mark. Despawned
/// + respawned by `update_hp_subdividers` whenever max HP changes
/// (mode flip resets `Health` to a different cap).
#[derive(Component)]
pub struct HpBarSubdivider;

/// Container below the main HP bar that holds one bar per live ally.
/// `sync_ally_hp_bars` reconciles its children with the current set of
/// `Ally` entities each frame.
#[derive(Component)]
pub struct AllyHpRow;

/// One ally's HP bar. Carries the ally `Entity` so the update / sync
/// systems can look up that ally's `Health` without walking the parent
/// hierarchy. When the ally despawns, the bar is despawned too.
#[derive(Component)]
pub struct AllyHpBar {
    pub ally: Entity,
}

/// Red fill child of an `AllyHpBar`, animated by `update_ally_hp_values`.
/// Tagged with the same ally `Entity` for direct lookup.
#[derive(Component)]
pub struct AllyHpFill {
    pub ally: Entity,
}

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

    // MAP button — sits below the VSYNC toggle. Visibility is gated by
    // `update_map_button` so it only shows in Combat view (the map view
    // exits via clicking sections, not via this button).
    commands.spawn((
        Button,
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(36.0),
            right: Val::Px(6.0),
            padding: UiRect::axes(Val::Px(6.0), Val::Px(2.0)),
            border: UiRect::all(Val::Px(1.0)),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            ..default()
        },
        BackgroundColor(Color::NONE),
        BorderColor(UI_VALUE),
        Visibility::Hidden,
        SlotButton { slot: 0, kind: ButtonKind::ReturnToMap },
        ReturnToMapButton,
    ))
    .with_children(|b| {
        b.spawn((
            Text::new("MAP"),
            TextFont { font_size: 9.0, ..default() },
            TextColor(UI_VALUE),
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

    // Player HP bar + ally bars — arcade style, anchored inside the
    // play square's top-left. Root is a flex column:
    //   - row 0: main bar (track with fill, ticks, right-aligned readout)
    //   - row 1: ally bars stacked vertically, populated dynamically
    //            by `sync_ally_hp_bars`.
    // Spawned `Visibility::Hidden`; `update_wave_ui` flips on combat
    // view and back.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(0.0),
            left: Val::Px(0.0),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::FlexStart,
            row_gap: Val::Px(3.0),
            ..default()
        },
        Visibility::Hidden,
        WaveHpUi,
    ))
    .with_children(|p| {
        // Main bar — 180×22 track, see top comment for child stack.
        p.spawn((
            Node {
                width: Val::Px(180.0),
                height: Val::Px(22.0),
                border: UiRect::all(Val::Px(1.0)),
                position_type: PositionType::Relative,
                ..default()
            },
            BackgroundColor(theme::BORDER_SUBTLE),
            BorderColor(theme::BORDER_DARK),
            WaveHpTrack,
        ))
        .with_children(|track| {
            // (1) Red fill, width animated by `update_wave_ui`.
            track.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Px(0.0),
                    left: Val::Px(0.0),
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.25, 0.85, 0.30)),
                WaveHpFill,
            ));
            // (2) Subdividers added at runtime by `update_hp_subdividers`.
            // (3) Numeric overlay — right-aligned inside the track,
            // small inset so the digit doesn't kiss the border.
            track.spawn(Node {
                position_type: PositionType::Absolute,
                top: Val::Px(0.0),
                left: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::FlexEnd,
                align_items: AlignItems::Center,
                padding: UiRect::right(Val::Px(6.0)),
                ..default()
            })
            .with_children(|over| {
                over.spawn((
                    Text::new(format!("{}", FRIENDLY_HP_WAVE)),
                    TextFont { font_size: 13.0, ..default() },
                    TextColor(theme::ON_SURFACE),
                    WaveHpText,
                ));
            });
        });

        // Ally HP bar container — flex column, populated by
        // `sync_ally_hp_bars`. Empty at startup; bars appear as ally
        // ships are spawned.
        p.spawn((
            Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(2.0),
                ..default()
            },
            AllyHpRow,
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
    mut view: ResMut<ViewMode>,
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
            ButtonKind::ReturnToMap => {
                *view = ViewMode::Map;
                continue;
            }
            _ => {}
        }
        let s = &mut cfg.slots[btn.slot];
        match btn.kind {
            ButtonKind::ToggleDesktopMode | ButtonKind::ToggleNightMode
            | ButtonKind::ToggleCrtMode  | ButtonKind::ToggleWaveMode
            | ButtonKind::ToggleVsync    | ButtonKind::ReturnToMap => unreachable!(),
            ButtonKind::RuneUp   => { if s.equipped { s.rune = cycle_next(s.rune); } }
            ButtonKind::RuneDown => { if s.equipped { s.rune = cycle_prev(s.rune); } }
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
                            s.rune = None;
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
            LabelKind::Rune    => **t = rune_display(s.rune).into(),
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

/// Show the MAP button only in Combat view — Map view exits via clicking
/// sections, not via this button.
pub fn update_map_button(
    view: Res<ViewMode>,
    mut q: Query<&mut Visibility, With<ReturnToMapButton>>,
) {
    if !view.is_changed() { return; }
    let target = match *view {
        ViewMode::Combat => Visibility::Inherited,
        ViewMode::Map    => Visibility::Hidden,
    };
    for mut v in &mut q {
        if *v != target { *v = target; }
    }
}

/// Toggle pier + HP-bar visibility and drive the bar's fill width +
/// numeric readout from the player's current `Health`.
///
/// - **Pier visibility:** wave-mode-only chrome, gated by `GameMode`.
/// - **HP bar visibility:** combat-only chrome, gated by `ViewMode`.
///   Hidden on the map so the bar doesn't compete with the slot
///   tiles + star ratings.
/// - **Fill width / numeric:** mode-aware max HP (Sandbox = 100,
///   Wave = `FRIENDLY_HP_WAVE`) because `wave.rs` resets `Health.0`
///   on mode flip. Subdividers recompute on the same mode change in
///   `update_hp_subdividers`.
pub fn update_wave_ui(
    mode: Res<GameMode>,
    view: Res<ViewMode>,
    friendly: Query<&Health, With<Friendly>>,
    mut pier_q: Query<
        &mut Visibility,
        (With<PierVisual>, Without<WaveHpUi>),
    >,
    mut hp_root_q: Query<
        &mut Visibility,
        (With<WaveHpUi>, Without<PierVisual>),
    >,
    mut hp_fill_q: Query<&mut Node, With<WaveHpFill>>,
    mut hp_text_q: Query<&mut Text, With<WaveHpText>>,
) {
    if mode.is_changed() {
        let want_pier = matches!(*mode, GameMode::Wave);
        let target = if want_pier { Visibility::Inherited } else { Visibility::Hidden };
        for mut v in &mut pier_q { if *v != target { *v = target; } }
    }

    if view.is_changed() {
        let want = if matches!(*view, ViewMode::Combat) {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        for mut v in &mut hp_root_q {
            if *v != want { *v = want; }
        }
    }

    let Ok(h) = friendly.single() else { return; };
    let max_hp = current_max_hp(&mode);
    let pct = (h.0 as f32 / max_hp as f32).clamp(0.0, 1.0);

    for mut node in &mut hp_fill_q {
        node.width = Val::Percent(pct * 100.0);
    }
    for mut t in &mut hp_text_q {
        // Just the current value — the bar's fill ratio already
        // visualizes the cap, no need to repeat it as text.
        **t = format!("{}", h.0.max(0));
    }
}

/// Despawn + respawn the bar's vertical tick lines whenever max HP
/// changes (currently mode flips: Sandbox 100 ↔ Wave 50). Ticks are
/// children of the track and use the same dark color as the border so
/// they read as part of the chrome — visible against the red fill on
/// the left side and the slightly-lighter empty bg on the right.
pub fn update_hp_subdividers(
    mode: Res<GameMode>,
    mut commands: Commands,
    track_q: Query<Entity, With<WaveHpTrack>>,
    subdivider_q: Query<Entity, With<HpBarSubdivider>>,
) {
    // `mode.is_changed()` is also true on the very first frame the
    // resource is inserted, so initial dividers are spawned on game
    // start (none exist before this fires).
    if !mode.is_changed() { return; }
    for e in &subdivider_q { commands.entity(e).despawn(); }
    let Ok(track) = track_q.single() else { return; };

    let max_hp = current_max_hp(&mode);
    commands.entity(track).with_children(|t| {
        // Tick at every multiple of 50 HP, *exclusive* of 0 and max
        // (those are already implied by the track edges). At Sandbox
        // 100 HP that's one mid-bar tick; at Wave 50 HP, none —
        // halves with a single divider isn't an accident, it's the
        // natural read for a bar this size.
        let step = 50;
        let mut hp = step;
        while hp < max_hp {
            let pct = hp as f32 / max_hp as f32 * 100.0;
            t.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Percent(pct),
                    top: Val::Px(0.0),
                    width: Val::Px(1.0),
                    height: Val::Percent(100.0),
                    ..default()
                },
                BackgroundColor(theme::BORDER_DARK),
                HpBarSubdivider,
            ));
            hp += step;
        }
    });
}

/// Single source of truth for "what is the player's max HP right now".
/// Used by both the bar's fill % math and the subdivider spacing.
/// Mirrors the actual HP cap reset behavior in `wave.rs`.
fn current_max_hp(mode: &GameMode) -> i32 {
    if matches!(mode, GameMode::Wave) { FRIENDLY_HP_WAVE } else { 100 }
}

/// Spawn one small bar for an ally as a child of `AllyHpRow`. Smaller
/// than the main bar (120×14) so it reads as secondary chrome — same
/// arcade language: dark border, dark track bg, green fill.
fn spawn_ally_hp_bar(parent: &mut ChildSpawnerCommands, ally: Entity) {
    parent
        .spawn((
            Node {
                width: Val::Px(120.0),
                height: Val::Px(14.0),
                border: UiRect::all(Val::Px(1.0)),
                position_type: PositionType::Relative,
                ..default()
            },
            BackgroundColor(theme::BORDER_SUBTLE),
            BorderColor(theme::BORDER_DARK),
            AllyHpBar { ally },
        ))
        .with_children(|b| {
            b.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Px(0.0),
                    left: Val::Px(0.0),
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.25, 0.85, 0.30)),
                AllyHpFill { ally },
            ));
        });
}

/// Reconcile the ally HP-bar set with the live `Ally` entities each
/// frame: spawn a bar for every ally that doesn't have one yet, and
/// despawn any bar whose ally is gone. The work is bounded by the ally
/// count (typically ≤ a handful) so a per-frame full reconciliation is
/// cheaper than wiring lifecycle hooks on ally spawn/despawn.
pub fn sync_ally_hp_bars(
    mut commands: Commands,
    container_q: Query<Entity, With<AllyHpRow>>,
    allies: Query<Entity, With<Ally>>,
    bars: Query<(Entity, &AllyHpBar)>,
) {
    use std::collections::HashSet;
    let live: HashSet<Entity> = allies.iter().collect();
    let bar_targets: HashSet<Entity> = bars.iter().map(|(_, b)| b.ally).collect();

    // Despawn orphans — ally despawned, but its bar lingered.
    for (bar_e, bar) in &bars {
        if !live.contains(&bar.ally) {
            commands.entity(bar_e).despawn();
        }
    }

    // Spawn missing bars for new allies.
    let Ok(container) = container_q.single() else { return; };
    let new_allies: Vec<Entity> = allies
        .iter()
        .filter(|e| !bar_targets.contains(e))
        .collect();
    if new_allies.is_empty() { return; }
    commands.entity(container).with_children(|c| {
        for ally in new_allies {
            spawn_ally_hp_bar(c, ally);
        }
    });
}

/// Drive each ally bar's fill width from its ally's current HP. Max HP
/// pulled from the ally's variant so future variants with different
/// caps work without changing this system.
pub fn update_ally_hp_values(
    allies: Query<(&Ally, &Health)>,
    mut fills: Query<(&AllyHpFill, &mut Node)>,
) {
    for (marker, mut node) in &mut fills {
        let Ok((ally, h)) = allies.get(marker.ally) else { continue; };
        let max = ally.variant.hp().max(1);
        let pct = (h.0 as f32 / max as f32).clamp(0.0, 1.0);
        let want = Val::Percent(pct * 100.0);
        if node.width != want { node.width = want; }
    }
}

/// Anchor the HP bar inside the play square's top-left and match its
/// chrome (track outline + tick widths) to one game pixel.
///
/// - **Pixel matching:** UI nodes draw at native resolution while the
///   play area is nearest-neighbor upscaled, so a UI `Val::Px(1)` is
///   one *screen* pixel — thinner than one *game* pixel. Computing
///   `upscale = play_size / PLAY_INTERNAL` and setting border/divider
///   widths to that value keeps the bar's lines on the same grid as
///   the rest of the art.
/// - **Anchoring:** computing `play_left` / `play_top` each frame
///   keeps the bar pinned to the play square through window resizes,
///   UI panel toggles, etc. Margin is in *game pixels* (×upscale) so
///   the offset feels consistent at any scale.
pub fn update_hp_bar_pixel_scale(
    windows: Query<&Window, With<PrimaryWindow>>,
    window_mode: Res<WindowMode>,
    mut root_q: Query<
        &mut Node,
        (
            With<WaveHpUi>,
            Without<WaveHpTrack>,
            Without<HpBarSubdivider>,
            Without<AllyHpBar>,
        ),
    >,
    mut track_q: Query<
        &mut Node,
        (
            With<WaveHpTrack>,
            Without<WaveHpUi>,
            Without<HpBarSubdivider>,
            Without<AllyHpBar>,
        ),
    >,
    mut subdivider_q: Query<
        &mut Node,
        (
            With<HpBarSubdivider>,
            Without<WaveHpUi>,
            Without<WaveHpTrack>,
            Without<AllyHpBar>,
        ),
    >,
    mut ally_bar_q: Query<
        &mut Node,
        (
            With<AllyHpBar>,
            Without<WaveHpUi>,
            Without<WaveHpTrack>,
            Without<HpBarSubdivider>,
        ),
    >,
) {
    let Ok(win) = windows.single() else { return; };
    let (play_left, play_top, size) = play_area_screen_rect(
        win.width(), win.height(), effective_ui_width(&window_mode),
    );
    let upscale = (size / PLAY_INTERNAL as f32).max(1.0);
    let px = Val::Px(upscale);
    let border = UiRect::all(px);

    // Anchor the bar 4 game-pixels in from the play area's top-left
    // corner. Margin scales with `upscale` so it visually sits the
    // same distance from the edge regardless of window size.
    let margin = upscale * 4.0;
    let want_left = Val::Px(play_left + margin);
    let want_top  = Val::Px(play_top  + margin);
    for mut node in &mut root_q {
        if node.left != want_left { node.left = want_left; }
        if node.top  != want_top  { node.top  = want_top;  }
    }

    for mut node in &mut track_q {
        if node.border != border { node.border = border; }
    }
    for mut node in &mut subdivider_q {
        if node.width != px { node.width = px; }
    }
    // Ally bars use the same 1-game-pixel border so chrome stays
    // consistent across the main + ally stack — only their height /
    // length differ.
    for mut node in &mut ally_bar_q {
        if node.border != border { node.border = border; }
    }
}
