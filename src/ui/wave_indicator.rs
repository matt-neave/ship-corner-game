//! Top-right `WAVE X/Y` readout. Reads `CombatContext.wave_idx` +
//! `wave_count` and updates a single text node each frame. Shown only
//! during Playing / StageComplete in Combat view; hidden everywhere
//! else.
//!
//! When `combat_ctx.is_boss_wave` is true the label appends `BOSS`
//! and recolors the text. The trigger predicate currently returns
//! false (`balance::is_boss_wave`); this just keeps the UI pathway in
//! place for when the rule is set.
//!
//! The label renders with a 1-pixel black stroke for readability
//! against bright water. bevy_ui's `TextShadow` is a single
//! offset-shadow per entity, so the stroke is faked by spawning four
//! diagonal-offset black Text twins underneath the main coloured
//! Text. All five entities share a parent `WaveIndicator` wrapper so
//! visibility + anchor positioning are inherited.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::map::{CombatContext, ViewMode};
use crate::modes::play_area_screen_rect;
use crate::ui_kit::theme;

/// Wrapper Node for the wave indicator stack. Owns the absolute
/// top/right anchor + the cascading Visibility for its 5 Text
/// children (1 main + 4 stroke twins).
#[derive(Component)]
pub struct WaveIndicator;

/// Tags each Text child so the update system can rewrite its
/// label + paint only the main node with the accent / boss tint.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
pub enum WaveIndicatorPart {
    Stroke,
    Main,
}

const ACCENT: Color = Color::srgb(1.0, 0.85, 0.30);
const BOSS_RED: Color = Color::srgb(0.95, 0.30, 0.40);
const STROKE_COLOR: Color = Color::srgba(0.0, 0.0, 0.0, 0.95);
/// Real-time duration the white flash fades back from on a wave
/// advance. ~350 ms — short enough to feel like a snap, long
/// enough to register at 60 fps.
const WAVE_FLASH_DURATION: f32 = 0.35;
const FLASH_COLOR: Color = Color::WHITE;

/// 8-direction stroke at 2-px radius. Four diagonals + four cardinals
/// for a uniform halo — thick enough to read clearly against the bright
/// water without a TextShadow's directional bias.
const STROKE_OFFSETS: &[(f32, f32)] = &[
    (-2.0, -2.0), ( 2.0, -2.0), (-2.0,  2.0), ( 2.0,  2.0),
    (-2.0,  0.0), ( 2.0,  0.0), ( 0.0, -2.0), ( 0.0,  2.0),
];

pub fn setup_wave_indicator(
    mut commands: Commands,
    thaleah: Res<crate::fonts::ThaleahFont>,
) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(8.0),
                right: Val::Px(8.0),
                ..default()
            },
            ZIndex(40),
            Visibility::Hidden,
            WaveIndicator,
        ))
        .with_children(|p| {
            // Stroke twins first so they sit underneath the main
            // node in declaration order. Each is absolute-positioned
            // at its 8-direction offset relative to the wrapper's
            // anchor — bevy_ui draws absolute siblings independently.
            for &(dx, dy) in STROKE_OFFSETS {
                p.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        top: Val::Px(dy),
                        left: Val::Px(dx),
                        ..default()
                    },
                    Text::new("WAVE 1/7"),
                    crate::fonts::thaleah_text_font(&thaleah, 28.0),
                    TextColor(STROKE_COLOR),
                    WaveIndicatorPart::Stroke,
                ));
            }
            // Main coloured glyph sits on top of the stroke stack.
            p.spawn((
                Text::new("WAVE 1/7"),
                crate::fonts::thaleah_text_font(&thaleah, 28.0),
                TextColor(ACCENT),
                WaveIndicatorPart::Main,
            ));
        });
    let _ = theme::ACCENT;
}

pub fn update_wave_indicator(
    state: Res<State<crate::AppState>>,
    view: Res<ViewMode>,
    ctx: Res<CombatContext>,
    real: Res<bevy::time::Time<bevy::time::Real>>,
    mut last_wave_idx: Local<Option<u8>>,
    mut flash_remaining: Local<f32>,
    windows: Query<&Window, With<PrimaryWindow>>,
    ui_scale: Res<bevy::ui::UiScale>,
    mut root_q: Query<(&mut Visibility, &mut Node), With<WaveIndicator>>,
    mut parts_q: Query<(&mut Text, &mut TextColor, &WaveIndicatorPart)>,
) {
    // Detect wave-index advance and start a brief flash. Real time
    // so it resolves even if a hitstop is freezing the world.
    if last_wave_idx.is_none() {
        *last_wave_idx = Some(ctx.wave_idx);
    } else if *last_wave_idx != Some(ctx.wave_idx) {
        *last_wave_idx = Some(ctx.wave_idx);
        *flash_remaining = WAVE_FLASH_DURATION;
    }
    if *flash_remaining > 0.0 {
        *flash_remaining = (*flash_remaining - real.delta_secs()).max(0.0);
    }
    let flash_t = if WAVE_FLASH_DURATION > 0.0 {
        (*flash_remaining / WAVE_FLASH_DURATION).clamp(0.0, 1.0)
    } else { 0.0 };
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
    let want_main_color = if ctx.is_boss_wave { BOSS_RED } else { ACCENT };

    // Anchor to the play area's top-right corner. With the LHS panel
    // hidden, the play area is centered horizontally — there's
    // letterbox space to either side that we don't want to cover.
    // UiScale-compensated layout. Screen-pixel insets get divided by
    // the scale so the layout pass multiplies them back to actual
    // screen positions. XP bar dimensions are already in design pixels
    // (UiScale handles them), so we add them in design space.
    let ui_s = ui_scale.0.max(0.0001);
    let (anchor_top, anchor_right) = windows
        .single()
        .ok()
        .map(|w| {
            let (left, top, play_w, _play_h) = play_area_screen_rect(w.width(), w.height());
            // Distance from the window's right edge to the play area's
            // right edge, expressed in design pixels.
            let right_inset = ((w.width() - (left + play_w)).max(0.0)) / ui_s;
            // Hug the play-area's top-right corner — same small inset
            // (4 design px) on both axes so the readout sits flush
            // against the chunky frame without crowding it. The
            // HP / XP / shield bars sit in the top-LEFT, so the WAVE
            // glyph has the whole right column to itself.
            let inset = 4.0;
            (top / ui_s + inset, right_inset + inset)
        })
        .unwrap_or((4.0, 4.0));

    for (mut v, mut node) in &mut root_q {
        if *v != want_vis { *v = want_vis; }
        let top_val = Val::Px(anchor_top);
        let right_val = Val::Px(anchor_right);
        if node.top != top_val { node.top = top_val; }
        if node.right != right_val { node.right = right_val; }
    }
    for (mut t, mut c, part) in &mut parts_q {
        if t.0 != label { t.0 = label.clone(); }
        let want_color = match *part {
            WaveIndicatorPart::Main => lerp_color(want_main_color, FLASH_COLOR, flash_t),
            WaveIndicatorPart::Stroke => STROKE_COLOR,
        };
        if c.0 != want_color { c.0 = want_color; }
    }
}

/// Linear RGBA lerp used by the wave-flash blend. Lives here
/// rather than in a shared utility module because the flash is
/// the only consumer.
fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let a = a.to_srgba();
    let b = b.to_srgba();
    let t = t.clamp(0.0, 1.0);
    Color::srgba(
        a.red   + (b.red   - a.red)   * t,
        a.green + (b.green - a.green) * t,
        a.blue  + (b.blue  - a.blue)  * t,
        a.alpha + (b.alpha - a.alpha) * t,
    )
}
