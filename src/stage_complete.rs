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

/// Time elapsed since `OnEnter(StageComplete)` fired. Reset on entry,
/// ticked during the state, ignored otherwise.
#[derive(Resource, Default)]
pub struct StageCompleteTimer(pub f32);

#[derive(Component)]
pub struct StageCompleteUi;

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
            root.spawn((
                Text::new("STAGE COMPLETE"),
                TextFont {
                    font_size: 48.0,
                    font_smoothing: FontSmoothing::None,
                    ..default()
                },
                TextColor(theme::ACCENT),
            ));
        });
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
    mut next: ResMut<NextState<crate::AppState>>,
) {
    timer.0 += time.delta_secs();
    if timer.0 >= DURATION {
        next.set(crate::AppState::Customize);
    }
}
