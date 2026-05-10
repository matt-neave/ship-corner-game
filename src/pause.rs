//! ESC pause overlay with RESUME / QUIT.
//!
//! Spawned hidden at startup; ESC toggles it, but only when no other
//! modal is in the way (customize overlay, desktop drag mode). The
//! `Paused` resource gates `in_combat_view` so combat freezes while
//! the menu is up.

use bevy::app::AppExit;
use bevy::prelude::*;

use crate::modes::WindowMode;
use crate::ui_kit::{self, theme};

/// True while the pause menu is up. Read by `in_combat_view` (in `map`)
/// so combat-side systems freeze for the duration.
#[derive(Resource, Default)]
pub struct Paused(pub bool);

#[derive(Component)]
pub struct PauseRoot;

#[derive(Component)]
pub struct ResumeButton;

#[derive(Component)]
pub struct MainMenuButton;

#[derive(Component)]
pub struct QuitButton;

pub fn setup_pause_menu(mut commands: Commands) {
    commands
        .spawn((
            // Full-screen dim layer. `Button` absorbs clicks so they
            // don't fall through to gameplay when paused.
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
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.78)),
            // ZIndex above everything except the customize overlay (200).
            ZIndex(180),
            Visibility::Hidden,
            PauseRoot,
            Button,
        ))
        .with_children(|root| {
            root.spawn(ui_kit::label("PAUSED", theme::FONT_LG * 1.6, theme::ACCENT));

            root.spawn((
                ui_kit::button(theme::SURFACE_RAISED),
                ResumeButton,
            ))
            .with_children(|b| {
                b.spawn(ui_kit::label("RESUME", theme::FONT_LG, theme::ON_SURFACE));
            });

            root.spawn((
                ui_kit::button(theme::SURFACE_RAISED),
                MainMenuButton,
            ))
            .with_children(|b| {
                b.spawn(ui_kit::label("MAIN MENU", theme::FONT_LG, theme::ON_SURFACE));
            });

            root.spawn((
                ui_kit::button(theme::SURFACE_RAISED),
                QuitButton,
            ))
            .with_children(|b| {
                b.spawn(ui_kit::label("QUIT", theme::FONT_LG, theme::ON_SURFACE));
            });
        });
}

/// Toggle pause on ESC. Reads `AppState` directly so the toggle only
/// fires when the player is mid-game — pressing ESC on the main menu
/// or while the customize overlay is up is a no-op (those screens own
/// their own dismissal). Desktop drag mode also blocks the toggle so
/// its own ESC handler can claim the press to exit desktop mode.
pub fn toggle_pause_on_esc(
    keys: Res<ButtonInput<KeyCode>>,
    state: Res<State<crate::AppState>>,
    window_mode: Res<WindowMode>,
    mut next: ResMut<NextState<crate::AppState>>,
) {
    if !keys.just_pressed(KeyCode::Escape) {
        return;
    }
    if window_mode.desktop {
        return;
    }
    match *state.get() {
        crate::AppState::Playing => next.set(crate::AppState::Paused),
        crate::AppState::Paused => next.set(crate::AppState::Playing),
        // Main menu / customize manage their own input; ESC stays inert.
        _ => {}
    }
}

/// Drive the pause-menu visibility from `Paused`.
pub fn sync_pause_menu_visibility(
    paused: Res<Paused>,
    mut q: Query<&mut Visibility, With<PauseRoot>>,
) {
    if !paused.is_changed() {
        return;
    }
    let want = if paused.0 { Visibility::Inherited } else { Visibility::Hidden };
    for mut vis in &mut q {
        if *vis != want {
            *vis = want;
        }
    }
}

pub fn handle_resume_click(
    interactions: Query<&Interaction, (Changed<Interaction>, With<ResumeButton>)>,
    mut next: ResMut<NextState<crate::AppState>>,
) {
    for interaction in &interactions {
        if matches!(*interaction, Interaction::Pressed) {
            next.set(crate::AppState::Playing);
        }
    }
}

pub fn handle_quit_click(
    interactions: Query<&Interaction, (Changed<Interaction>, With<QuitButton>)>,
    mut exit: EventWriter<AppExit>,
) {
    for interaction in &interactions {
        if matches!(*interaction, Interaction::Pressed) {
            exit.write(AppExit::Success);
        }
    }
}

pub fn handle_main_menu_click(
    interactions: Query<&Interaction, (Changed<Interaction>, With<MainMenuButton>)>,
    mut next: ResMut<NextState<crate::AppState>>,
) {
    for interaction in &interactions {
        if matches!(*interaction, Interaction::Pressed) {
            next.set(crate::AppState::MainMenu);
        }
    }
}
