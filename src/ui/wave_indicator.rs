//! Top-right `WAVE X/Y` readout. Reads `CombatContext.wave_idx` +
//! `wave_count` and updates a single text node each frame. Shown only
//! during Playing / StageComplete in Combat view; hidden everywhere
//! else.
//!
//! When `combat_ctx.is_boss_wave` is true the label appends `BOSS`
//! and recolors the text. The trigger predicate currently returns
//! false (`balance::is_boss_wave`); this just keeps the UI pathway in
//! place for when the rule is set.

use bevy::prelude::*;
use bevy::text::FontSmoothing;
use bevy::window::PrimaryWindow;

use crate::map::{CombatContext, ViewMode};
use crate::modes::{effective_ui_width, play_area_screen_rect, WindowMode};
use crate::ui_kit::theme;

#[derive(Component)]
pub struct WaveIndicator;

const ACCENT: Color = Color::srgb(1.0, 0.85, 0.30);
const BOSS_RED: Color = Color::srgb(0.95, 0.30, 0.40);

pub fn setup_wave_indicator(mut commands: Commands) {
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(8.0),
            right: Val::Px(8.0),
            ..default()
        },
        Text::new("WAVE 1/7"),
        TextFont {
            font_size: 14.0,
            font_smoothing: FontSmoothing::None,
            ..default()
        },
        TextColor(ACCENT),
        ZIndex(40),
        Visibility::Hidden,
        WaveIndicator,
    ));
    let _ = theme::ACCENT; // keep import chain stable for future styling
}

pub fn update_wave_indicator(
    state: Res<State<crate::AppState>>,
    view: Res<ViewMode>,
    ctx: Res<CombatContext>,
    windows: Query<&Window, With<PrimaryWindow>>,
    window_mode: Res<WindowMode>,
    mut q: Query<(&mut Visibility, &mut Text, &mut TextColor, &mut Node), With<WaveIndicator>>,
) {
    let s = *state.get();
    let want_vis = if matches!(s, crate::AppState::Playing | crate::AppState::StageComplete)
        && *view == ViewMode::Combat
    {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };

    let label = if ctx.is_boss_wave {
        format!("WAVE {}/{}  BOSS", ctx.wave_idx + 1, ctx.wave_count)
    } else {
        format!("WAVE {}/{}", ctx.wave_idx + 1, ctx.wave_count)
    };
    let want_color = if ctx.is_boss_wave { BOSS_RED } else { ACCENT };

    // Anchor to the play area's top-right corner. With the LHS panel
    // hidden, the play area is centered horizontally — there's
    // letterbox space to either side that we don't want to cover.
    let (anchor_top, anchor_right) = windows
        .single()
        .ok()
        .map(|w| {
            let (left, top, size) = play_area_screen_rect(
                w.width(),
                w.height(),
                effective_ui_width(&window_mode),
            );
            // Distance from the window's right edge to the play area's
            // right edge.
            let right_inset = (w.width() - (left + size)).max(0.0);
            // Sit BELOW the XP bar that runs across the play-area top.
            // Inset (6 px) + XP bar height (22 px) + small gap (4 px).
            let below_xp = crate::xp::XP_BAR_TOP_INSET + crate::xp::XP_BAR_HEIGHT + 4.0;
            (top + below_xp, right_inset + 8.0)
        })
        .unwrap_or((32.0, 8.0));

    for (mut v, mut t, mut c, mut node) in &mut q {
        if *v != want_vis { *v = want_vis; }
        if t.0 != label { t.0 = label.clone(); }
        if c.0 != want_color { c.0 = want_color; }
        let top_val = Val::Px(anchor_top);
        let right_val = Val::Px(anchor_right);
        if node.top != top_val { node.top = top_val; }
        if node.right != right_val { node.right = right_val; }
    }
}
