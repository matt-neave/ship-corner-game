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

use crate::balance::{CUSTOMIZE_LAYER, UPSCALE_LAYER};
use crate::stats::{PlayerStats, StatKind};

use super::setup::{CustomizeText, CustomizeTextSpec, HitArea};

/// Native-pixel font sizes for the panel. One step smaller than
/// the previous 16pt — the row count grew over time (Dodge,
/// Armour, etc.) and the chunkier text was crowding the column.
/// 13pt is the next native multiple of the Pixel Operator 8px
/// design grid down from 16pt that still reads cleanly.
const LABEL_FONT: f32 = 13.0;
const VALUE_FONT: f32 = 13.0;
/// Spec-pixel vertical step between rows. Trimmed alongside the
/// font drop so the panel stays proportional — same ratio of row
/// step to glyph height as before.
const ROW_STEP: f32 = 9.0;
/// Spec-pixel horizontal extent of one row from `label_x` to `value_x`.
const ROW_WIDTH: f32 = 64.0;

/// Marker carrying the `StatKind` whose value this text node displays.
/// `sync_stats_panel` queries this to update text per frame.
#[derive(Component, Clone, Copy)]
pub struct StatPanelValue(pub StatKind);

/// Marker on the LABEL Text2d of a stat row. Used by
/// `apply_stats_label_highlight` to tint the name (not the value)
/// when a hovered mod card targets this row's stat — the user
/// asked for the name to highlight so values keep their
/// buff/nerf/baseline colour grammar.
#[derive(Component, Clone, Copy)]
pub struct StatPanelLabel(pub StatKind);

/// Per-row pop state. Set when the displayed text changes; decays
/// in real time. `apply_stat_pop` reads this AFTER `sync_customize_text`
/// runs and multiplies the baseline glyph scale by a curve so the
/// value "pops" then settles.
#[derive(Component, Default)]
pub struct StatPopState {
    pub remaining: f32,
    /// Cached previous text — `sync_stats_panel` compares against
    /// this to detect a meaningful change.
    pub prev: String,
}

/// Real-time duration of the pop animation.
const POP_DURATION: f32 = 0.22;
/// Peak scale at the midpoint of the pop. 1.4 = +40%.
const POP_PEAK_SCALE: f32 = 1.4;

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
pub fn spawn_stats_panel(commands: &mut Commands, font: &crate::fonts::PixelFont, right_edge_x: f32, top_y: f32) {
    let label_x = right_edge_x - ROW_WIDTH;
    let value_x = right_edge_x;
    // Row-wide hover hit area covers label + value (not the debug
    // buttons, so a click on `+/-` doesn't double as a hover trigger).
    let hover_centre_x = (label_x + value_x) * 0.5;
    let hover_size = Vec2::new(ROW_WIDTH, ROW_STEP);
    for (i, &kind) in StatKind::ALL.iter().enumerate() {
        let y = top_y - i as f32 * ROW_STEP;
        spawn_debug_button(commands, font, Vec2::new(label_x + MINUS_OFFSET, y), kind, -1);
        spawn_debug_button(commands, font, Vec2::new(label_x + PLUS_OFFSET, y), kind, 1);
        spawn_label(commands, font, Vec2::new(label_x, y), kind.label(), kind);
        spawn_value(commands, font, Vec2::new(value_x, y), kind);
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
    highlight: Res<crate::stats_panel_overlay::HighlightedStats>,
    mut commands: Commands,
    mut q: Query<(Entity, &StatPanelValue, &mut Text2d, &mut TextColor, Option<&mut StatPopState>)>,
) {
    if !open.open { return; }
    let baseline = PlayerStats::default();
    for (e, sv, mut text, mut color, pop) in &mut q {
        let cur_str = sv.0.format_value(&stats, Some(&synergies));
        // If the hovered mod / level-up card touches this stat,
        // probe what the value would be after the change and
        // append `(current -> new)` so the player can see the
        // result before clicking.
        let s = if let Some(entry) = highlight.kinds.get(&sv.0) {
            let mut probe = stats.clone();
            let stat = sv.0.stat_mut(&mut probe);
            if entry.to_flat { stat.flat += entry.delta; } else { stat.percent += entry.delta; }
            let new_str = sv.0.format_value(&probe, Some(&synergies));
            // Bracket form rather than arrow — reads as "current
            // value (preview after this mod)" without the visual
            // weight of an arrow glyph.
            format!("{} ({})", cur_str, new_str)
        } else {
            cur_str
        };
        if text.0 != s {
            text.0 = s.clone();
        }
        // Detect a value change via the cached prev string. First
        // sight initialises prev without popping; subsequent changes
        // trigger the animation.
        match pop {
            Some(mut p) => {
                if p.prev != s {
                    if !p.prev.is_empty() {
                        p.remaining = POP_DURATION;
                    }
                    p.prev = s.clone();
                }
            }
            None => {
                commands.entity(e).insert(StatPopState {
                    remaining: 0.0,
                    prev: s.clone(),
                });
            }
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
        // Values always show their buff/nerf/baseline colour
        // grammar — the hover highlight tints the LABEL instead
        // (see `apply_stats_label_highlight`) so the value's
        // numeric reading stays intact.
        let _ = &highlight; // read so the param isn't dead.
        let want = if cur > base + 0.001 {
            crate::ui_kit::theme::BUFF_FG // green: buffed
        } else if cur < base - 0.001 {
            crate::ui_kit::theme::NERF_FG // red: nerfed
        } else {
            Color::srgb(0.70, 0.72, 0.78) // grey: baseline
        };
        if color.0 != want { color.0 = want; }
    }
}

/// Per-frame: paint each stat-label gold when the matching
/// `StatKind` is in [`HighlightedStats`]; otherwise restore the
/// neutral near-white. Mod-card hover producers fill the
/// highlight set so the player can scan affected rows by name.
pub fn apply_stats_label_highlight(
    open: Res<super::CustomizeOpen>,
    highlight: Res<crate::stats_panel_overlay::HighlightedStats>,
    mut q: Query<(&StatPanelLabel, &mut TextColor)>,
) {
    if !open.open { return; }
    const NEUTRAL: Color = Color::srgb(0.92, 0.94, 0.97);
    let buff = crate::ui_kit::theme::BUFF_FG;
    let nerf = crate::ui_kit::theme::NERF_FG;
    for (label, mut color) in &mut q {
        use crate::stats_panel_overlay::HighlightSign;
        let want = match highlight.kinds.get(&label.0).map(|e| e.sign) {
            Some(HighlightSign::Buff) => buff,
            Some(HighlightSign::Nerf) => nerf,
            None => NEUTRAL,
        };
        if color.0 != want { color.0 = want; }
    }
}

/// Run after `sync_customize_text` so it can MULTIPLY the baseline
/// glyph scale by the pop curve. Sine curve from 1.0 → POP_PEAK
/// → 1.0 over POP_DURATION real-time seconds. Decay uses real
/// time so the animation resolves even if a hitstop freezes the
/// world.
pub fn apply_stat_pop(
    real: Res<bevy::time::Time<bevy::time::Real>>,
    open: Res<super::CustomizeOpen>,
    mut q: Query<(&mut StatPopState, &mut Transform), With<StatPanelValue>>,
) {
    if !open.open { return; }
    let dt = real.delta_secs();
    for (mut pop, mut tf) in &mut q {
        if pop.remaining > 0.0 {
            pop.remaining = (pop.remaining - dt).max(0.0);
        }
        let t = if POP_DURATION > 0.0 && pop.remaining > 0.0 {
            1.0 - (pop.remaining / POP_DURATION).clamp(0.0, 1.0)
        } else { 0.0 };
        // 0 at t=0, 1 at t=0.5, 0 at t=1 — bell curve.
        let curve = (t * std::f32::consts::PI).sin();
        let scale_mult = 1.0 + (POP_PEAK_SCALE - 1.0) * curve;
        // Multiply in-place; sync_customize_text wrote the baseline
        // earlier this frame, and we leave Z alone.
        tf.scale.x *= scale_mult;
        tf.scale.y *= scale_mult;
    }
}

fn spawn_label(
    commands: &mut Commands,
    font: &crate::fonts::PixelFont,
    spec_pos: Vec2,
    text: &str,
    kind: StatKind,
) {
    commands.spawn((
        Text2d::new(text),
        crate::fonts::pixel_text_font(font, LABEL_FONT),
        // Brighter near-white so the stat names are easy to read
        // against the dark customize backdrop. Previously a muted
        // mid-gray that washed out at small sizes.
        TextColor(Color::srgb(0.92, 0.94, 0.97)),
        Anchor::CenterLeft,
        Transform::from_xyz(0.0, 0.0, 100.0),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        CustomizeText,
        StatPanelLabel(kind),
        CustomizeTextSpec(spec_pos),
    ));
}

fn spawn_debug_button(commands: &mut Commands, font: &crate::fonts::PixelFont, spec_pos: Vec2, kind: StatKind, dir: i32) {
    let glyph = if dir > 0 { "+" } else { "-" };
    // Visible glyph (text on UPSCALE_LAYER). Toggled visible by the
    // shared `CustomizeText` sync AND by `sync_stat_debug_visibility`
    // — the latter forces Hidden when `DebugUiVisible` is off so the
    // `+/-` controls only appear after the player presses `#`.
    commands.spawn((
        Text2d::new(glyph),
        crate::fonts::pixel_text_font(font, LABEL_FONT),
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

fn spawn_value(commands: &mut Commands, font: &crate::fonts::PixelFont, spec_pos: Vec2, kind: StatKind) {
    commands.spawn((
        Text2d::new(""),
        crate::fonts::pixel_text_font(font, VALUE_FONT),
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
