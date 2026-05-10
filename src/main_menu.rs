//! Boot-time main menu. The game launches with the menu open and the
//! play world hidden behind it; clicking PLAY closes the menu and the
//! existing combat / map flow takes over.
//!
//! Visual language mirrors the customize overlay: dark fills with
//! crisp white outlines and pixel-precise text. Built on `bevy_ui`
//! (rather than world-space sprites on `UPSCALE_LAYER`) because the
//! HUD camera's viewport only covers the play area — the menu needs
//! to cover the whole window including the side UI panel.

use bevy::prelude::*;

use crate::ui_kit::theme;

/// True while the main menu is up. Defaults to `true` so the menu is
/// the very first thing the player sees on launch.
#[derive(Resource)]
pub struct MainMenuOpen(pub bool);
impl Default for MainMenuOpen {
    fn default() -> Self { Self(true) }
}

#[derive(Component)]
pub struct MainMenuRoot;

#[derive(Component)]
pub struct PlayButton;

#[derive(Component)]
pub struct SettingsButton;

const TITLE: &str = "BATTLESHIP CONTROL";
const FONT_TITLE: f32 = 56.0;
const FONT_BUTTON: f32 = 22.0;
const BUTTON_W: f32 = 240.0;
const BUTTON_H: f32 = 56.0;
const ROW_GAP: f32 = 14.0;
const TITLE_GAP: f32 = 48.0;

pub fn setup_main_menu(mut commands: Commands) {
    commands
        .spawn((
            // Fullscreen dark backdrop. Same near-black as the
            // customize-camera clear color so the visual language
            // carries across into the rest of the UI.
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(0.0),
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                bottom: Val::Px(0.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                row_gap: Val::Px(TITLE_GAP),
                ..default()
            },
            BackgroundColor(Color::srgb(0.05, 0.06, 0.08)),
            ZIndex(220),
            Visibility::Inherited,
            MainMenuRoot,
            // Button on the root absorbs stray clicks so they don't
            // reach gameplay underneath.
            Button,
        ))
        .with_children(|root| {
            root.spawn((
                Text::new(TITLE),
                TextFont {
                    font_size: FONT_TITLE,
                    font_smoothing: bevy::text::FontSmoothing::None,
                    ..default()
                },
                TextColor(theme::ACCENT),
            ));

            root.spawn(Node {
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(ROW_GAP),
                ..default()
            })
            .with_children(|col| {
                spawn_menu_button::<PlayButton>(col, "PLAY", PlayButton);
                spawn_menu_button::<SettingsButton>(col, "SETTINGS", SettingsButton);
            });
        });
}

fn spawn_menu_button<M: Component>(
    parent: &mut ChildSpawnerCommands,
    label: &str,
    marker: M,
) {
    parent
        .spawn((
            Button,
            Node {
                width: Val::Px(BUTTON_W),
                height: Val::Px(BUTTON_H),
                border: UiRect::all(Val::Px(2.0)),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(Color::srgb(0.13, 0.14, 0.17)),
            // White outline = the "card" frame from the customize
            // overlay, lifted into bevy_ui via `BorderColor`.
            BorderColor(Color::WHITE),
            marker,
        ))
        .with_children(|b| {
            b.spawn((
                Text::new(label),
                TextFont {
                    font_size: FONT_BUTTON,
                    font_smoothing: bevy::text::FontSmoothing::None,
                    ..default()
                },
                TextColor(theme::ON_SURFACE),
            ));
        });
}

/// Drive root visibility from `MainMenuOpen`. Children inherit, so a
/// single write hides/shows the entire menu tree.
pub fn sync_main_menu_visibility(
    open: Res<MainMenuOpen>,
    mut q: Query<&mut Visibility, With<MainMenuRoot>>,
) {
    if !open.is_changed() { return; }
    let want = if open.0 { Visibility::Inherited } else { Visibility::Hidden };
    for mut v in &mut q {
        if *v != want { *v = want; }
    }
}

pub fn handle_play_click(
    interactions: Query<&Interaction, (Changed<Interaction>, With<PlayButton>)>,
    mut next: ResMut<NextState<crate::AppState>>,
) {
    for interaction in &interactions {
        if matches!(*interaction, Interaction::Pressed) {
            next.set(crate::AppState::Playing);
        }
    }
}

pub fn handle_settings_click(
    interactions: Query<&Interaction, (Changed<Interaction>, With<SettingsButton>)>,
) {
    // Placeholder. The existing settings (window/CRT/night/vsync modes)
    // are toggled via keys today; surfacing them as a sub-menu lives in
    // a follow-up.
    for _interaction in &interactions {}
}
