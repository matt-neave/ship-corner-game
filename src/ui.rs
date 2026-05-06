//! All UI: LHS turret control panel, top score/wave banner, top-left HP bar
//! (Wave mode), bottom draft prompt + cards (Drafting phase). Plus the
//! per-slot damage-share bars and the click handlers that drive everything.
//!
//! UI marker components live here; `modes.rs` and `pier.rs` reach in to
//! toggle visibility on a few of them via `pub` exports (`ScoreText`,
//! `UiPanel`, `WaveHpUi/Fill/Text`, `DraftPanel`).

use bevy::ecs::hierarchy::ChildSpawnerCommands;
use bevy::prelude::*;

use crate::balance::{
    FRIENDLY_HP_WAVE, HULL_LEN, HULL_WIDTH, TURRET_NAME_KEYS, TURRET_POSITIONS, UI_WIDTH,
};
use crate::components::{Friendly, Health};
use crate::i18n::tr;
use crate::modes::{CrtMode, DesktopHint, GameMode, NightMode, WindowMode};
use crate::palette::{
    UI_ACTIVE_BG, UI_BG, UI_BTN_BG, UI_DOT_OFF, UI_DOT_ON, UI_EQUIP_BG, UI_HULL,
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
}

/// Tag on a text node whose contents are driven by `update_slot_labels`.
#[derive(Component)]
pub struct SlotLabel { pub slot: usize, pub kind: LabelKind }

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LabelKind { Damage, Rate, Status, Barrels }

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
    // Score banner over the play area.
    commands.spawn((
        Text::new(format!("{} 0", tr("score_label"))),
        TextFont { font_size: 36.0, ..default() },
        TextColor(UI_VALUE),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(8.0),
            left: Val::Px(UI_WIDTH),
            right: Val::Px(0.0),
            justify_content: JustifyContent::Center,
            ..default()
        },
        ScoreText,
    ));

    // Wave-mode draft panel — three thin-bordered text cards along the
    // bottom of the screen. Hidden unless WavePhase::Drafting.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            bottom: Val::Px(18.0),
            left: Val::Px(UI_WIDTH),
            right: Val::Px(0.0),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            row_gap: Val::Px(6.0),
            ..default()
        },
        Visibility::Hidden,
        DraftPanel,
    ))
    .with_children(|p| {
        p.spawn((
            Text::new(tr("draft_instruction")),
            TextFont { font_size: 11.0, ..default() },
            TextColor(UI_TEXT_DIM),
        ));
        p.spawn(Node {
            flex_direction: FlexDirection::Row,
            column_gap: Val::Px(10.0),
            ..default()
        })
        .with_children(|row| {
            for i in 0..3u8 {
                spawn_draft_card(row, i);
            }
        });
    });

    // Wave-mode HP bar — top-left of the play area, hidden in sandbox.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(14.0),
            left: Val::Px(UI_WIDTH + 14.0),
            width: Val::Px(160.0),
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: Val::Px(6.0),
            ..default()
        },
        Visibility::Hidden,
        WaveHpUi,
    ))
    .with_children(|p| {
        p.spawn((
            Text::new(tr("hp_label")),
            TextFont { font_size: 13.0, ..default() },
            TextColor(UI_TEXT_DIM),
        ));
        // Track + fill — same pattern as the per-slot SHARE bars.
        p.spawn((
            Node {
                flex_grow: 1.0,
                height: Val::Px(8.0),
                ..default()
            },
            BackgroundColor(UI_ROW_DIV),
            BorderRadius::all(Val::Px(2.0)),
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
                BorderRadius::all(Val::Px(2.0)),
                WaveHpFill,
            ));
        });
        p.spawn((
            Text::new(format!("{}/{}", FRIENDLY_HP_WAVE, FRIENDLY_HP_WAVE)),
            TextFont { font_size: 11.0, ..default() },
            TextColor(UI_TEXT),
            WaveHpText,
        ));
    });

    // Desktop-mode hint — small grey text at top-center, hidden by default.
    commands.spawn((
        Text::new(tr("btn_press_esc")),
        TextFont { font_size: 9.0, ..default() },
        TextColor(Color::srgb(0.7, 0.72, 0.78)),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(4.0),
            left: Val::Px(0.0),
            right: Val::Px(0.0),
            justify_content: JustifyContent::Center,
            ..default()
        },
        Visibility::Hidden,
        DesktopHint,
    ));

    // Left control panel.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            top: Val::Px(0.0),
            width: Val::Px(UI_WIDTH),
            height: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            padding: UiRect::all(Val::Px(8.0)),
            row_gap: Val::Px(4.0),
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
            margin: UiRect { bottom: Val::Px(4.0), ..default() },
            ..default()
        })
        .with_children(|h| {
            h.spawn(Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(2.0),
                ..default()
            })
            .with_children(|t| {
                t.spawn((
                    Text::new(tr("title_battleship")),
                    TextFont { font_size: 16.0, ..default() },
                    TextColor(UI_TEXT),
                ));
                t.spawn((
                    Text::new(tr("subtitle_cuniberti")),
                    TextFont { font_size: 10.0, ..default() },
                    TextColor(UI_TEXT_DIM),
                ));
            });
            h.spawn(Node {
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(4.0),
                ..default()
            })
            .with_children(|btns| {
                spawn_header_button(btns, tr("btn_wave"),    ButtonKind::ToggleWaveMode);
                spawn_header_button(btns, tr("btn_crt"),     ButtonKind::ToggleCrtMode);
                spawn_header_button(btns, tr("btn_night"),   ButtonKind::ToggleNightMode);
                spawn_header_button(btns, tr("btn_desktop"), ButtonKind::ToggleDesktopMode);
            });
        });
        // Divider.
        p.spawn((
            Node {
                width: Val::Percent(100.0),
                height: Val::Px(1.0),
                margin: UiRect { bottom: Val::Px(4.0), ..default() },
                ..default()
            },
            BackgroundColor(UI_ROW_DIV),
        ));

        // 8 turret slot rows.
        for slot in 0..8 {
            spawn_slot_row(p, slot);
        }
    });
}

// ---------- Slot row builders ----------

fn spawn_slot_row(parent: &mut ChildSpawnerCommands, slot: usize) {
    parent.spawn((
        Node {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Stretch,
            column_gap: Val::Px(8.0),
            padding: UiRect::all(Val::Px(6.0)),
            ..default()
        },
        BackgroundColor(UI_ROW_BG),
        BorderRadius::all(Val::Px(3.0)),
    ))
    .with_children(|row| {
        spawn_ship_schematic(row, slot);
        spawn_slot_controls(row, slot);
    });
}

/// Mini top-down ship silhouette with all 8 turret dots; the slot's own
/// turret dot is highlighted in `UI_DOT_ON`, the rest are dimmed. Mirrors
/// the layout of `TURRET_POSITIONS` so the panel reads as a real schematic.
fn spawn_ship_schematic(parent: &mut ChildSpawnerCommands, slot: usize) {
    const SIZE: f32 = 56.0;
    const HULL_W: f32 = 12.0;
    const HULL_H: f32 = 38.0;
    let sx = HULL_W / HULL_WIDTH;
    let sy = HULL_H / HULL_LEN;
    let center = SIZE / 2.0;

    parent.spawn((
        Node {
            width: Val::Px(SIZE),
            height: Val::Px(SIZE),
            position_type: PositionType::Relative,
            flex_shrink: 0.0,
            ..default()
        },
        BackgroundColor(UI_BG),
        BorderRadius::all(Val::Px(2.0)),
    ))
    .with_children(|s| {
        // Hull silhouette (rounded rectangle = capsule).
        s.spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(center - HULL_W / 2.0),
                top:  Val::Px(center - HULL_H / 2.0),
                width: Val::Px(HULL_W),
                height: Val::Px(HULL_H),
                ..default()
            },
            BackgroundColor(UI_HULL),
            BorderRadius::all(Val::Px(HULL_W / 2.0)),
        ));

        // 8 turret dots. World +y = bow (up); UI +y = window-down. Flip y.
        for i in 0..8 {
            let (lx, ly) = TURRET_POSITIONS[i];
            let dot_x = center + lx * sx;
            let dot_y = center - ly * sy;
            let dot = 4.0;
            let color = if i == slot { UI_DOT_ON } else { UI_DOT_OFF };
            s.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(dot_x - dot / 2.0),
                    top:  Val::Px(dot_y - dot / 2.0),
                    width: Val::Px(dot),
                    height: Val::Px(dot),
                    ..default()
                },
                BackgroundColor(color),
                BorderRadius::all(Val::Px(dot / 2.0)),
            ));
        }
    });
}

fn spawn_slot_controls(parent: &mut ChildSpawnerCommands, slot: usize) {
    parent.spawn(Node {
        flex_direction: FlexDirection::Column,
        flex_grow: 1.0,
        row_gap: Val::Px(3.0),
        ..default()
    })
    .with_children(|c| {
        // Slot title row: "01  BOW".
        c.spawn(Node {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: Val::Px(6.0),
            ..default()
        })
        .with_children(|t| {
            t.spawn((
                Text::new(format!("{:02}", slot + 1)),
                TextFont { font_size: 11.0, ..default() },
                TextColor(UI_TEXT_DIM),
            ));
            t.spawn((
                Text::new(tr(TURRET_NAME_KEYS[slot])),
                TextFont { font_size: 12.0, ..default() },
                TextColor(UI_TEXT),
            ));
        });

        // Equip / Active button. Always shown; `update_slot_labels` keeps
        // the inner text in sync; the click handler is a no-op when the
        // weapon-cycle hits the unequip step.
        c.spawn((
            Button,
            Node {
                width: Val::Percent(100.0),
                padding: UiRect::axes(Val::Px(4.0), Val::Px(3.0)),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(if slot == 0 { UI_ACTIVE_BG } else { UI_EQUIP_BG }),
            BorderRadius::all(Val::Px(2.0)),
            SlotButton { slot, kind: ButtonKind::Equip },
        ))
        .with_children(|b| {
            b.spawn((
                Text::new(if slot == 0 { tr("weapon_standard") } else { tr("btn_equip_gun") }),
                TextFont { font_size: 11.0, ..default() },
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
        column_gap: Val::Px(4.0),
        ..default()
    })
    .with_children(|r| {
        r.spawn((
            Text::new(tr("stat_share")),
            TextFont { font_size: 10.0, ..default() },
            TextColor(UI_TEXT_DIM),
            Node { width: Val::Px(32.0), ..default() },
        ));
        // Track: dark background filling the remaining row width. Fill child
        // is absolutely positioned so its width can scale 0–100% of the track
        // without disturbing the row's flex layout.
        r.spawn((
            Node {
                flex_grow: 1.0,
                height: Val::Px(6.0),
                ..default()
            },
            BackgroundColor(UI_ROW_DIV),
            BorderRadius::all(Val::Px(2.0)),
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
                BorderRadius::all(Val::Px(2.0)),
                SlotDamageBar { slot },
            ));
        });
        r.spawn((
            Text::new("0%"),
            TextFont { font_size: 11.0, ..default() },
            TextColor(UI_TEXT),
            Node { width: Val::Px(28.0), ..default() },
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
        column_gap: Val::Px(4.0),
        ..default()
    })
    .with_children(|r| {
        r.spawn((
            Text::new(label.to_string()),
            TextFont { font_size: 10.0, ..default() },
            TextColor(UI_TEXT_DIM),
            Node { width: Val::Px(32.0), ..default() },
        ));
        r.spawn((
            Text::new(initial.to_string()),
            TextFont { font_size: 12.0, ..default() },
            TextColor(UI_VALUE),
            SlotLabel { slot, kind: label_kind },
            Node { width: Val::Px(28.0), ..default() },
        ));
        spawn_step_button(r, slot, down_kind, "−");
        spawn_step_button(r, slot, up_kind,   "+");
    });
}

fn spawn_header_button(parent: &mut ChildSpawnerCommands, label: &str, kind: ButtonKind) {
    parent.spawn((
        Button,
        Node {
            padding: UiRect::axes(Val::Px(8.0), Val::Px(4.0)),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            ..default()
        },
        BackgroundColor(UI_BTN_BG),
        BorderRadius::all(Val::Px(2.0)),
        SlotButton { slot: 0, kind },
    ))
    .with_children(|b| {
        b.spawn((
            Text::new(label.to_string()),
            TextFont { font_size: 10.0, ..default() },
            TextColor(UI_TEXT),
        ));
    });
}

fn spawn_step_button(parent: &mut ChildSpawnerCommands, slot: usize, kind: ButtonKind, label: &str) {
    parent.spawn((
        Button,
        Node {
            width: Val::Px(20.0),
            height: Val::Px(18.0),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            ..default()
        },
        BackgroundColor(UI_BTN_BG),
        BorderRadius::all(Val::Px(2.0)),
        SlotButton { slot, kind },
    ))
    .with_children(|b| {
        b.spawn((
            Text::new(label.to_string()),
            TextFont { font_size: 14.0, ..default() },
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
            _ => {}
        }
        let s = &mut cfg.slots[btn.slot];
        match btn.kind {
            ButtonKind::ToggleDesktopMode | ButtonKind::ToggleNightMode
            | ButtonKind::ToggleCrtMode  | ButtonKind::ToggleWaveMode => unreachable!(),
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
            ButtonKind::BarrelsUp   => { if s.equipped && s.barrels < 2 { s.barrels += 1; } }
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
