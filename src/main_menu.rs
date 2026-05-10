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

use crate::modes::{CrtMode, NightMode, VsyncMode};
use crate::ui_kit::theme;

/// Clean the arena when the player returns to the main menu mid-run.
/// Despawns every enemy + bullet + ally so PLAY-from-menu starts on
/// an empty stage rather than the snapshot left over from the run
/// they just bailed on.
pub fn clear_arena_on_main_menu(
    mut commands: Commands,
    enemies: Query<Entity, With<crate::enemy::Enemy>>,
    bullets: Query<Entity, With<crate::bullet::Bullet>>,
    allies: Query<Entity, With<crate::ally::Ally>>,
) {
    for e in &enemies { commands.entity(e).despawn(); }
    for e in &bullets { commands.entity(e).despawn(); }
    for e in &allies  { commands.entity(e).despawn(); }
}

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

/// Which sub-page of the main menu is showing — root (PLAY/SETTINGS)
/// or the settings panel. Resets to `Root` whenever the menu closes.
#[derive(Resource, Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum MainMenuView {
    #[default]
    Root,
    Settings,
}

#[derive(Component)]
pub struct RootButtons;

#[derive(Component)]
pub struct SettingsButtons;

/// Tag on each button inside the settings sub-panel. Drives both the
/// click handler (toggle the matching mode) and the per-frame label
/// updater (show ON/OFF).
#[derive(Component, Clone, Copy)]
pub enum SettingsItem {
    Night,
    Crt,
    Vsync,
    Back,
}

#[derive(Component)]
pub struct SettingsItemLabel(pub SettingsItem);

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

            // Root buttons column: PLAY / SETTINGS. Visible by default.
            root.spawn((
                Node {
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Center,
                    row_gap: Val::Px(ROW_GAP),
                    ..default()
                },
                RootButtons,
            ))
            .with_children(|col| {
                spawn_menu_button::<PlayButton>(col, "PLAY", PlayButton);
                spawn_menu_button::<SettingsButton>(col, "SETTINGS", SettingsButton);
            });

            // Settings sub-panel column. Hidden until SETTINGS is
            // clicked. Each toggle's label is rewritten per frame by
            // `update_settings_labels` so it reads `NIGHT: ON`/`OFF`
            // live.
            root.spawn((
                Node {
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Center,
                    row_gap: Val::Px(ROW_GAP),
                    ..default()
                },
                Visibility::Hidden,
                SettingsButtons,
            ))
            .with_children(|col| {
                spawn_settings_button(col, SettingsItem::Night, "NIGHT");
                spawn_settings_button(col, SettingsItem::Crt, "CRT");
                spawn_settings_button(col, SettingsItem::Vsync, "VSYNC");
                spawn_settings_button(col, SettingsItem::Back, "BACK");
            });
        });
}

fn spawn_settings_button(
    parent: &mut ChildSpawnerCommands,
    item: SettingsItem,
    base_label: &str,
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
            BorderColor(Color::WHITE),
            item,
        ))
        .with_children(|b| {
            b.spawn((
                Text::new(base_label),
                TextFont {
                    font_size: FONT_BUTTON,
                    font_smoothing: bevy::text::FontSmoothing::None,
                    ..default()
                },
                TextColor(theme::ON_SURFACE),
                SettingsItemLabel(item),
            ));
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

/// Open the settings sub-page when the home-screen SETTINGS button is
/// pressed.
pub fn handle_settings_click(
    interactions: Query<&Interaction, (Changed<Interaction>, With<SettingsButton>)>,
    mut view: ResMut<MainMenuView>,
) {
    for interaction in &interactions {
        if matches!(*interaction, Interaction::Pressed) {
            *view = MainMenuView::Settings;
        }
    }
}

/// Click router for the four settings buttons (NIGHT / CRT / VSYNC /
/// BACK). Each toggles its mode resource; BACK returns to the root
/// view.
pub fn handle_settings_item_click(
    interactions: Query<(&Interaction, &SettingsItem), Changed<Interaction>>,
    mut view: ResMut<MainMenuView>,
    mut night: ResMut<NightMode>,
    mut crt: ResMut<CrtMode>,
    mut vsync: ResMut<VsyncMode>,
) {
    for (interaction, item) in &interactions {
        if !matches!(*interaction, Interaction::Pressed) { continue; }
        match *item {
            SettingsItem::Night => night.active = !night.active,
            SettingsItem::Crt   => crt.active = !crt.active,
            SettingsItem::Vsync => vsync.enabled = !vsync.enabled,
            SettingsItem::Back  => *view = MainMenuView::Root,
        }
    }
}

/// Per-frame visibility sync for the two button columns. Reads
/// `MainMenuView`. Independent of `MainMenuOpen` — when the menu
/// closes, the root visibility hides the whole tree; on reopen the
/// view defaults to Root.
pub fn sync_main_menu_view(
    open: Res<MainMenuOpen>,
    mut view: ResMut<MainMenuView>,
    mut root_q: Query<&mut Visibility, (With<RootButtons>, Without<SettingsButtons>)>,
    mut settings_q: Query<&mut Visibility, (With<SettingsButtons>, Without<RootButtons>)>,
) {
    // Closing the menu always rewinds back to the Root page.
    if !open.0 && *view != MainMenuView::Root {
        *view = MainMenuView::Root;
    }
    let (root_want, settings_want) = match *view {
        MainMenuView::Root => (Visibility::Inherited, Visibility::Hidden),
        MainMenuView::Settings => (Visibility::Hidden, Visibility::Inherited),
    };
    for mut v in &mut root_q {
        if *v != root_want { *v = root_want; }
    }
    for mut v in &mut settings_q {
        if *v != settings_want { *v = settings_want; }
    }
}

/// Rewrites each settings-button label with the live mode state so
/// the player can see what's on without trial-and-clicking.
pub fn update_settings_labels(
    night: Res<NightMode>,
    crt: Res<CrtMode>,
    vsync: Res<VsyncMode>,
    mut q: Query<(&SettingsItemLabel, &mut Text)>,
) {
    for (label, mut text) in &mut q {
        let s = match label.0 {
            SettingsItem::Night => format!("NIGHT: {}", on_off(night.active)),
            SettingsItem::Crt   => format!("CRT: {}",   on_off(crt.active)),
            SettingsItem::Vsync => format!("VSYNC: {}", on_off(vsync.enabled)),
            SettingsItem::Back  => "BACK".to_string(),
        };
        if text.0 != s { text.0 = s; }
    }
}

fn on_off(v: bool) -> &'static str { if v { "ON" } else { "OFF" } }
