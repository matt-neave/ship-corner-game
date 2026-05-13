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

/// Native-pixel font sizes for the panel. Bigger than the rest of
/// the customize chrome so the live stat readout is the thing the
/// eye lands on and rows are comfortably readable.
const LABEL_FONT: f32 = 16.0;
const VALUE_FONT: f32 = 16.0;
/// Spec-pixel vertical step between rows. Tall enough that the
/// 16pt fonts above don't visually crowd one another.
const ROW_STEP: f32 = 11.0;
/// Spec-pixel horizontal extent of one row from `label_x` to `value_x`.
const ROW_WIDTH: f32 = 64.0;

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
    debug_visible: Res<crate::map::DebugUiVisible>,
    mouse: Res<ButtonInput<MouseButton>>,
    drag: Res<super::DragState>,
    mut stats: ResMut<PlayerStats>,
    btn_q: Query<(&Transform, &HitArea, &StatDebugButton)>,
) {
    if !open.open { return; }
    // Click-through gate: the visible glyphs are hidden when debug
    // mode is off, but the click-target HitArea entities stay alive
    // so the player can't accidentally drive the stats by clicking
    // where the buttons WOULD be. Block here as well so it's robust.
    if !debug_visible.0 { return; }
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
/// text AND tints it green / red / grey by comparing the current
/// effective value to the baseline (`PlayerStats::default()`). The
/// stats panel is the central place to see "how do I compare to a
/// fresh ship?", so the colour cue is more useful there than the
/// raw number alone.
pub fn sync_stats_panel(
    stats: Res<PlayerStats>,
    synergies: Res<crate::synergy::Synergies>,
    open: Res<super::CustomizeOpen>,
    mut q: Query<(&StatPanelValue, &mut Text2d, &mut TextColor)>,
) {
    if !open.open { return; }
    let baseline = PlayerStats::default();
    for (sv, mut text, mut color) in &mut q {
        let s = sv.0.format_value(&stats, Some(&synergies));
        if text.0 != s {
            text.0 = s;
        }
        // For most stats, the buffed/nerfed comparison runs on the
        // raw `stat.effective()`. The synergy-folded stats (WEAPON
        // DAMAGE bakes in Naval, HARVEST bakes in Pirate) have to
        // fold the same multiplier into the colour comparison too
        // — otherwise a Naval / Pirate active build shows the
        // boosted percentage in grey because the underlying `.flat`
        // is still at baseline.
        let (cur, base) = match sv.0 {
            StatKind::TurretDamage => (
                stats.turret_damage_mult() * synergies.naval_damage_mult(),
                1.0,
            ),
            StatKind::Harvest => (
                (1.0 + stats.harvest_pct.effective() / 100.0)
                    * synergies.pirate_harvest_mult(),
                1.0,
            ),
            _ => (sv.0.stat(&stats).effective(), sv.0.stat(&baseline).effective()),
        };
        let want = if cur > base + 0.001 {
            Color::srgb(0.55, 0.95, 0.55) // green: buffed
        } else if cur < base - 0.001 {
            Color::srgb(1.00, 0.55, 0.55) // red: nerfed
        } else {
            Color::srgb(0.70, 0.72, 0.78) // grey: baseline
        };
        if color.0 != want { color.0 = want; }
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
        // Brighter near-white so the stat names are easy to read
        // against the dark customize backdrop. Previously a muted
        // mid-gray that washed out at small sizes.
        TextColor(Color::srgb(0.92, 0.94, 0.97)),
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
    // Visible glyph (text on UPSCALE_LAYER). Toggled visible by the
    // shared `CustomizeText` sync AND by `sync_stat_debug_visibility`
    // — the latter forces Hidden when `DebugUiVisible` is off so the
    // `+/-` controls only appear after the player presses `#`.
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
        StatDebugGlyph,
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

/// Marker on the visible `+/-` text glyphs (NOT the click-target
/// entity, which carries `StatDebugButton`). `sync_stat_debug_visibility`
/// queries this to gate them on `DebugUiVisible`.
#[derive(Component)]
pub struct StatDebugGlyph;

/// Per-frame: the `+/-` debug glyphs are only visible when BOTH
/// the customize overlay is open AND the `#` debug toggle is on.
/// Writes the resolved state unconditionally each frame so a
/// toggle back ON re-reveals them (an early-return would have left
/// them stuck Hidden after the first toggle-off). Must run AFTER
/// `sync_customize_text` so its Inherited write doesn't undo the
/// Hidden — see the `.after()` ordering in `main.rs`.
pub fn sync_stat_debug_visibility(
    visible: Res<crate::map::DebugUiVisible>,
    open: Res<super::CustomizeOpen>,
    mut q: Query<&mut Visibility, With<StatDebugGlyph>>,
) {
    let want = if open.open && visible.0 {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    for mut v in &mut q {
        if *v != want { *v = want; }
    }
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
