//! 5-second "STAGE COMPLETE" buffer between clearing a level and the
//! shop opening.
//!
//! Architected as its own `AppState` variant so combat sim freezes for
//! the duration (gameplay-affecting systems are already gated on
//! `state == Playing`, so they idle automatically). The screen is a
//! transparent overlay with centred accent text — no dark backdrop, so
//! the player can still see their ship sitting in the cleared arena.
//!
//! Lifecycle:
//! - `OnEnter(StageComplete)` spawns the UI + resets the timer.
//! - `tick_stage_complete` increments while the state is active.
//! - At `DURATION` seconds the system queues `NextState(Customize)`.
//! - `OnExit(StageComplete)` despawns the UI; the next-round combat
//!   budget was already queued by `level_complete_check` so the shop
//!   has work to do as soon as it closes.

use bevy::prelude::*;
use bevy::text::FontSmoothing;

use crate::ui_kit::theme;

/// Total buffer length in seconds.
pub const DURATION: f32 = 5.0;
/// Wavey title — vertical bob amplitude per character (px).
const WAVE_AMP: f32 = 8.0;
/// Wavey title — angular frequency of the bob (rad/s).
const WAVE_SPEED: f32 = 5.0;
/// Wavey title — phase offset between adjacent characters (rad).
/// Bigger value = tighter ripple, smaller = the whole word moves
/// closer to in-sync.
const WAVE_PHASE_PER_CHAR: f32 = 0.45;

/// Time elapsed since `OnEnter(StageComplete)` fired. Reset on entry,
/// ticked during the state, ignored otherwise.
#[derive(Resource, Default)]
pub struct StageCompleteTimer(pub f32);

#[derive(Component)]
pub struct StageCompleteUi;

/// Per-character marker on each glyph in the wavey title. `idx` drives
/// the per-char phase offset so the bob ripples left-to-right.
#[derive(Component)]
pub struct StageCompleteWaveChar { pub idx: usize }

pub fn enter_stage_complete(mut commands: Commands, mut timer: ResMut<StageCompleteTimer>) {
    timer.0 = 0.0;
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(0.0),
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                bottom: Val::Px(0.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(Color::NONE),
            ZIndex(180),
            Visibility::Inherited,
            StageCompleteUi,
        ))
        .with_children(|root| {
            // Per-character glyphs in a flex row so each one can bob
            // independently. `tick_stage_complete_wave` updates each
            // glyph's `Node.top` from its `idx` each frame, producing
            // a left-to-right ripple. Splitting the title into N
            // entities forfeits cross-glyph kerning, which is fine
            // for the chunky pixel font.
            root.spawn(Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                ..default()
            })
            .with_children(|row| {
                for (i, ch) in "STAGE COMPLETE".chars().enumerate() {
                    // Use a non-breaking space for the gap so the
                    // glyph node doesn't collapse / get trimmed.
                    let s = if ch == ' ' { "\u{00A0}".to_string() } else { ch.to_string() };
                    row.spawn((
                        Text::new(s),
                        TextFont {
                            font_size: 48.0,
                            font_smoothing: FontSmoothing::None,
                            ..default()
                        },
                        TextColor(theme::ACCENT),
                        Node {
                            position_type: PositionType::Relative,
                            ..default()
                        },
                        StageCompleteWaveChar { idx: i },
                    ));
                }
            });
        });
}

/// Bob each glyph along Y based on `(time × WAVE_SPEED + idx ×
/// WAVE_PHASE_PER_CHAR)`. Negative `top` lifts the glyph above its
/// natural flex position; positive drops it below.
pub fn tick_stage_complete_wave(
    time: Res<Time>,
    mut q: Query<(&StageCompleteWaveChar, &mut Node)>,
) {
    let t = time.elapsed_secs();
    for (c, mut node) in &mut q {
        let phase = c.idx as f32 * WAVE_PHASE_PER_CHAR;
        let bob = -(t * WAVE_SPEED + phase).sin() * WAVE_AMP;
        let want = Val::Px(bob);
        if node.top != want { node.top = want; }
    }
}

pub fn exit_stage_complete(
    mut commands: Commands,
    q: Query<Entity, With<StageCompleteUi>>,
) {
    for e in &q {
        commands.entity(e).despawn();
    }
}

pub fn tick_stage_complete(
    time: Res<Time>,
    mut timer: ResMut<StageCompleteTimer>,
    pending: Res<crate::xp::LevelUpsPending>,
    mut next: ResMut<NextState<crate::AppState>>,
) {
    timer.0 += time.delta_secs();
    if timer.0 >= DURATION {
        // Level-ups earned this stage get spent before the shop opens.
        // Each pick decrements the queue; the click handler routes back
        // here-or-onward to Customize when it's drained.
        if pending.0 > 0 {
            next.set(crate::AppState::LevelUp);
        } else {
            next.set(crate::AppState::Customize);
        }
    }
}
