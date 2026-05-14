//! Win screen — minimal end-of-run overlay shown when the player
//! defeats a 5★ section boss. Styled to match the shop's chunky-pixel
//! aesthetic: opaque shop-backdrop colour fills the screen, a single
//! large `VICTORY` title in accent yellow, and MAIN MENU / QUIT
//! buttons underneath.
//!
//! `level_complete_check` (in `map::buildings`) is the only path into
//! this state — when the cleared section's `stars == 5`, the
//! transition routes here instead of `StageComplete`, ending the run
//! before the shop/map cycle would otherwise resume.
//!
//! MAIN MENU runs the same fresh-run reset as the game-over
//! RESTART → MainMenu hand-off; QUIT exits the app.

use bevy::app::AppExit;
use bevy::prelude::*;

use crate::ui_kit::{self, theme};
use crate::AppState;

pub struct WinScreenPlugin;

impl Plugin for WinScreenPlugin {
    fn build(&self, app: &mut App) {
        app
            .add_systems(OnEnter(AppState::Win), enter_win)
            .add_systems(
                OnExit(AppState::Win),
                (exit_win, crate::game_over::reset_run_for_restart),
            )
            .add_systems(
                Update,
                (handle_main_menu_click, handle_quit_click)
                    .run_if(in_state(AppState::Win)),
            );
    }
}

#[derive(Component)]
pub struct WinRoot;

#[derive(Component)]
pub struct WinMainMenuButton;

#[derive(Component)]
pub struct WinQuitButton;

pub fn enter_win(mut commands: Commands, mut sfx: crate::sfx::SfxPlayer) {
    sfx.play(crate::sfx::Sfx::Victory);
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
                row_gap: Val::Px(theme::GAP_LG),
                ..default()
            },
            // Opaque shop-backdrop colour — same as the customize
            // screen's camera clear, so the win screen reads as the
            // same "screen" as the shop, just with different content.
            BackgroundColor(Color::srgb(0.13, 0.14, 0.17)),
            ZIndex(190),
            Visibility::Inherited,
            WinRoot,
            Button,
        ))
        .with_children(|root| {
            root.spawn(ui_kit::label("VICTORY", theme::FONT_LG * 2.4, theme::ACCENT));

            root.spawn((ui_kit::button(theme::SURFACE_RAISED), WinMainMenuButton))
                .with_children(|b| {
                    b.spawn(ui_kit::label("MAIN MENU", theme::FONT_LG, theme::ON_SURFACE));
                });

            root.spawn((ui_kit::button(theme::SURFACE_RAISED), WinQuitButton))
                .with_children(|b| {
                    b.spawn(ui_kit::label("QUIT", theme::FONT_LG, theme::ON_SURFACE));
                });
        });
}

pub fn exit_win(mut commands: Commands, q: Query<Entity, With<WinRoot>>) {
    for e in &q {
        commands.entity(e).despawn();
    }
}

pub fn handle_main_menu_click(
    interactions: Query<&Interaction, (Changed<Interaction>, With<WinMainMenuButton>)>,
    mut next: ResMut<NextState<AppState>>,
) {
    for interaction in &interactions {
        if matches!(*interaction, Interaction::Pressed) {
            next.set(AppState::MainMenu);
        }
    }
}

pub fn handle_quit_click(
    interactions: Query<&Interaction, (Changed<Interaction>, With<WinQuitButton>)>,
    mut exit: EventWriter<AppExit>,
) {
    for interaction in &interactions {
        if matches!(*interaction, Interaction::Pressed) {
            exit.write(AppExit::Success);
        }
    }
}
