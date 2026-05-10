//! Live read-out of every `PlayerStats` value, rendered as a column
//! down the RHS of the customize overlay.
//!
//! Layout is dynamic — the row count is `StatKind::ALL.len()` and rows
//! are spaced by `ROW_STEP` from `top_y` downward. Adding a new stat
//! is a one-spot change in `stats::StatKind::ALL`; the panel picks it
//! up automatically.
//!
//! Each row spawns two text entities sharing the same y:
//! - a left-anchored **label** (muted color) at `label_x`
//! - a right-anchored **value** (highlight color) at `value_x`
//!
//! The value entity carries a `StatPanelValue(StatKind)` marker; the
//! per-frame `sync_stats_panel` system rewrites its text from
//! `PlayerStats` only when the resource has actually changed.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;
use bevy::sprite::Anchor;
use bevy::text::FontSmoothing;

use crate::balance::{CUSTOMIZE_LAYER, UPSCALE_LAYER};
use crate::stats::{PlayerStats, StatKind};

use super::setup::{CustomizeText, CustomizeTextSpec, HitArea};

/// Native-pixel font sizes for the panel.
const LABEL_FONT: f32 = 11.0;
const VALUE_FONT: f32 = 11.0;
/// Spec-pixel vertical step between rows.
const ROW_STEP: f32 = 9.0;
/// Spec-pixel horizontal extent of one row from `label_x` to `value_x`.
const ROW_WIDTH: f32 = 50.0;

/// Marker carrying the `StatKind` whose value this text node displays.
/// `sync_stats_panel` queries this to update text per frame.
#[derive(Component, Clone, Copy)]
pub struct StatPanelValue(pub StatKind);

/// Marker on the row-wide hit area used to drive the hover tooltip
/// (label + description). Read by `update_customize_tooltip`.
#[derive(Component, Clone, Copy)]
pub struct StatHover(pub StatKind);

/// Temporary debug click target. `dir` is `+1` or `-1` and a press
/// inside the hit area nudges that stat's `flat` by `kind.debug_step()`.
#[derive(Component, Clone, Copy)]
pub struct StatDebugButton {
    pub kind: StatKind,
    pub dir: i32,
}

/// Spec-pixel offsets for the debug `-/+` buttons relative to `label_x`.
/// Both buttons sit to the LEFT of the label so the value's right
/// anchor isn't disturbed.
const MINUS_OFFSET: f32 = -14.0;
const PLUS_OFFSET: f32 = -6.0;
/// Hit-area size in spec pixels for one debug button.
const DEBUG_BTN_HIT: f32 = 6.0;

/// Spawn the RHS stats column.
///
/// `right_edge_x` is the x-coordinate of the column's right edge in
/// spec coords; `top_y` is the centre y of the first row. The whole
/// column flows downward by `ROW_STEP` per row.
pub fn spawn_stats_panel(commands: &mut Commands, right_edge_x: f32, top_y: f32) {
    let label_x = right_edge_x - ROW_WIDTH;
    let value_x = right_edge_x;
    // Row-wide hover hit area covers label + value (not the debug
    // buttons, so a click on `+/-` doesn't double as a hover trigger).
    let hover_centre_x = (label_x + value_x) * 0.5;
    let hover_size = Vec2::new(ROW_WIDTH, ROW_STEP);
    for (i, &kind) in StatKind::ALL.iter().enumerate() {
        let y = top_y - i as f32 * ROW_STEP;
        spawn_debug_button(commands, Vec2::new(label_x + MINUS_OFFSET, y), kind, -1);
        spawn_debug_button(commands, Vec2::new(label_x + PLUS_OFFSET, y), kind, 1);
        spawn_label(commands, Vec2::new(label_x, y), kind.label());
        spawn_value(commands, Vec2::new(value_x, y), kind);
        commands.spawn((
            Transform::from_translation(Vec3::new(hover_centre_x, y, 2.0)),
            HitArea { size: hover_size },
            StatHover(kind),
            RenderLayers::layer(CUSTOMIZE_LAYER),
        ));
    }
}

/// Click handler for the temporary `-/+` debug buttons. Mutates the
/// targeted stat's `flat` so the recompute on next read picks it up.
pub fn handle_stat_debug_buttons(
    open: Res<super::CustomizeOpen>,
    mouse: Res<ButtonInput<MouseButton>>,
    drag: Res<super::DragState>,
    mut stats: ResMut<PlayerStats>,
    btn_q: Query<(&Transform, &HitArea, &StatDebugButton)>,
) {
    if !open.open { return; }
    if !mouse.just_pressed(MouseButton::Left) { return; }
    if drag.picked.is_some() { return; }
    let Some(cursor) = drag.spec_cursor else { return };
    for (tf, hit, btn) in &btn_q {
        let centre = tf.translation.truncate();
        let half = hit.size * 0.5;
        if cursor.x < centre.x - half.x
            || cursor.x > centre.x + half.x
            || cursor.y < centre.y - half.y
            || cursor.y > centre.y + half.y
        {
            continue;
        }
        let step = btn.kind.debug_step() * btn.dir as f32;
        let stat = btn.kind.stat_mut(&mut stats);
        stat.flat += step;
        return;
    }
}

/// Per-frame syncer. Writes the formatted value into each row's value
/// text. Skips work when `PlayerStats` hasn't changed.
pub fn sync_stats_panel(
    stats: Res<PlayerStats>,
    open: Res<super::CustomizeOpen>,
    mut q: Query<(&StatPanelValue, &mut Text2d)>,
) {
    if !open.open { return; }
    // 14-ish rows, all formatted strings; the change-detection gate
    // would skip first-open population since `insert_resource` doesn't
    // count as a mutation. Cheaper to always check than to chase that
    // edge case.
    for (sv, mut text) in &mut q {
        let s = sv.0.format_value(&stats);
        if text.0 != s {
            text.0 = s;
        }
    }
}

fn spawn_label(commands: &mut Commands, spec_pos: Vec2, text: &str) {
    commands.spawn((
        Text2d::new(text),
        TextFont {
            font_size: LABEL_FONT,
            font_smoothing: FontSmoothing::None,
            ..default()
        },
        TextColor(Color::srgb(0.55, 0.60, 0.70)),
        Anchor::CenterLeft,
        Transform::from_xyz(0.0, 0.0, 100.0),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        CustomizeText,
        CustomizeTextSpec(spec_pos),
    ));
}

fn spawn_debug_button(commands: &mut Commands, spec_pos: Vec2, kind: StatKind, dir: i32) {
    let glyph = if dir > 0 { "+" } else { "-" };
    // Visible glyph (text on UPSCALE_LAYER). Toggled visible via the
    // shared `CustomizeText` sync.
    commands.spawn((
        Text2d::new(glyph),
        TextFont {
            font_size: LABEL_FONT,
            font_smoothing: FontSmoothing::None,
            ..default()
        },
        TextColor(Color::srgb(0.85, 0.88, 0.94)),
        Anchor::Center,
        Transform::from_xyz(0.0, 0.0, 100.0),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        CustomizeText,
        CustomizeTextSpec(spec_pos),
    ));
    // Click target on the customize layer so cursor-in-HitArea checks
    // see it. Z is irrelevant for hit-testing.
    commands.spawn((
        Transform::from_translation(spec_pos.extend(2.0)),
        HitArea { size: Vec2::splat(DEBUG_BTN_HIT) },
        StatDebugButton { kind, dir },
        RenderLayers::layer(CUSTOMIZE_LAYER),
    ));
}

fn spawn_value(commands: &mut Commands, spec_pos: Vec2, kind: StatKind) {
    commands.spawn((
        Text2d::new(""),
        TextFont {
            font_size: VALUE_FONT,
            font_smoothing: FontSmoothing::None,
            ..default()
        },
        TextColor(Color::srgb(1.0, 0.85, 0.30)),
        Anchor::CenterRight,
        Transform::from_xyz(0.0, 0.0, 100.0),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        CustomizeText,
        CustomizeTextSpec(spec_pos),
        StatPanelValue(kind),
    ));
}
