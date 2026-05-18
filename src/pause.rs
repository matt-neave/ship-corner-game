//! ESC pause overlay with RESUME / QUIT.
//!
//! Spawned hidden at startup; ESC toggles it, but only when no other
//! modal is in the way (customize overlay, desktop drag mode). The
//! `Paused` resource gates `in_combat_view` so combat freezes while
//! the menu is up.

use bevy::app::AppExit;
use bevy::prelude::*;

use crate::build_summary::spawn_build_summary;
use crate::customize::drag::PurchasedMods;
use crate::main_menu::{SettingsItem, SettingsItemLabel};
use crate::turret::TurretConfig;
use crate::ui_kit::{self, theme};
use crate::AppState;

/// Owns the pause overlay: the `Paused` resource, the one-time menu
/// spawn at startup, the ESC toggle + visibility sync that run
/// unconditionally each frame, and the three click handlers gated on
/// `AppState::Paused`.
pub struct PausePlugin;

impl Plugin for PausePlugin {
    fn build(&self, app: &mut App) {
        app
            .insert_resource(Paused::default())
            .insert_resource(PrePauseState::default())
            .add_systems(Startup, setup_pause_menu)
            .add_systems(Update, (toggle_pause_on_esc, sync_pause_menu_visibility))
            // Rebuild the embedded build-summary every time the
            // player pauses so the panel reflects the *current*
            // loadout / mod stacks instead of whatever snapshot
            // was active when the pause overlay was first spawned.
            .add_systems(OnEnter(AppState::Paused), refresh_pause_build_summary)
            .add_systems(
                Update,
                (handle_resume_click, handle_main_menu_click, handle_quit_click)
                    .run_if(in_state(AppState::Paused)),
            );
    }
}

/// Captures the `AppState` we were in when pause opened, so the
/// resume path can drop the player back exactly where they were.
/// `None` outside a pause cycle. Lets pause work as a dynamic
/// overlay over Playing / Map / Customize / etc. instead of being
/// hard-coded to "only Playing → Paused → Playing."
#[derive(Resource, Default)]
pub struct PrePauseState(pub Option<AppState>);

/// True while the pause menu is up. Read by `in_combat_view` (in `map`)
/// so combat-side systems freeze for the duration.
#[derive(Resource, Default)]
pub struct Paused(pub bool);

#[derive(Component)]
pub struct PauseRoot;

/// Empty container slotted between the PAUSED title and the
/// RESUME button. `refresh_pause_build_summary` despawns its
/// children on every pause-enter and respawns from the live
/// TurretConfig + PurchasedMods — keeps the panel in sync with
/// purchases made mid-run.
#[derive(Component)]
pub struct PauseBuildSummary;

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

            // Build-summary container — populated `OnEnter(Paused)`
            // by `refresh_pause_build_summary`. Positioned ABSOLUTE
            // and anchored to the LEFT edge of the overlay so the
            // central column (PAUSED title + buttons) stays at the
            // screen's vertical centre. `justify_content: Center`
            // on this wrapper vertically centres the build card
            // against the full overlay height.
            root.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(theme::PAD_LG * 2.0),
                    top: Val::Px(0.0),
                    bottom: Val::Px(0.0),
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::FlexStart,
                    justify_content: JustifyContent::Center,
                    ..default()
                },
                BackgroundColor(Color::NONE),
                PauseBuildSummary,
            ));

            root.spawn((
                ui_kit::button(theme::SURFACE_RAISED),
                ResumeButton,
            ))
            .with_children(|b| {
                b.spawn(ui_kit::label("RESUME", theme::FONT_LG, theme::ON_SURFACE));
            });

            // Settings toggles, reusing the main-menu `SettingsItem`
            // markers so `handle_settings_item_click` (registered by
            // MainMenuPlugin, runs unconditionally) flips the matching
            // mode resource, and `update_settings_labels` keeps the
            // ON/OFF / cycle text current.
            spawn_pause_settings_button(root, SettingsItem::Night,      "NIGHT");
            spawn_pause_settings_button(root, SettingsItem::Crt,        "CRT");
            spawn_pause_settings_button(root, SettingsItem::Vsync,      "VSYNC");
            spawn_pause_settings_button(root, SettingsItem::Bloom,      "BLOOM");
            spawn_pause_settings_button(root, SettingsItem::Identify,   "IDENTIFY");
            spawn_pause_settings_button(root, SettingsItem::WindowMode, "WINDOW");
            spawn_pause_settings_button(root, SettingsItem::Resolution, "RES");
            spawn_pause_settings_button(root, SettingsItem::SfxVolume,  "SFX");
            spawn_pause_settings_button(root, SettingsItem::MusicVolume, "MUSIC");
            spawn_pause_settings_button(root, SettingsItem::Background, "BG");

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

/// One toggle button in the pause overlay. Same `ui_kit` styling as
/// RESUME / MAIN MENU / QUIT (so the column reads as one family), but
/// carries `SettingsItem` + `SettingsItemLabel` so the existing
/// main-menu click handler + label syncer drive its behaviour.
fn spawn_pause_settings_button(
    parent: &mut bevy::ecs::hierarchy::ChildSpawnerCommands,
    item: SettingsItem,
    base_label: &str,
) {
    parent
        .spawn((ui_kit::button(theme::SURFACE_RAISED), item))
        .with_children(|b| {
            b.spawn((
                ui_kit::label(base_label, theme::FONT_LG, theme::ON_SURFACE),
                SettingsItemLabel(item),
            ));
        });
}

/// Toggle pause on ESC from any in-run screen. Stashes the previous
/// AppState in `PrePauseState` so resume drops back exactly where
/// the player was — Playing / Map / Customize / LevelUp / etc all
/// pause and resume cleanly.
///
/// MainMenu / HullSelect / GameOver / Win / Lobby / WaitingForHost
/// own their own dismissal flow (or have no meaningful "underneath"
/// to return to) so ESC stays inert there.
pub fn toggle_pause_on_esc(
    keys: Res<ButtonInput<KeyCode>>,
    state: Res<State<crate::AppState>>,
    mut next: ResMut<NextState<crate::AppState>>,
    mut pre: ResMut<PrePauseState>,
) {
    if !keys.just_pressed(KeyCode::Escape) {
        return;
    }
    match *state.get() {
        crate::AppState::Paused => {
            // Resume back to whatever opened the pause overlay.
            // Fallback to Playing if nothing was stashed (e.g. a
            // direct state push during dev/testing).
            let target = pre.0.take().unwrap_or(crate::AppState::Playing);
            next.set(target);
        }
        // Pausable in-run states. Excludes the per-modal screens
        // (Customize / LevelUp / HullSelect) because their OnEnter
        // hooks would re-init the modal on resume (rolling fresh
        // shop items, losing drag state). Those screens own their
        // own ESC handling.
        s @ (crate::AppState::Playing
            | crate::AppState::Map
            | crate::AppState::StageComplete
            | crate::AppState::BossReward) => {
            pre.0 = Some(s);
            next.set(crate::AppState::Paused);
        }
        // MainMenu / HullSelect / Customize / LevelUp / GameOver /
        // Win / Lobby / WaitingForHost / BossIntro own their own
        // ESC handling.
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
    mut pre: ResMut<PrePauseState>,
) {
    for interaction in &interactions {
        if matches!(*interaction, Interaction::Pressed) {
            // Restore to the state we paused from. Falls back to
            // Playing if nothing was stashed — same logic as the
            // ESC handler above.
            let target = pre.0.take().unwrap_or(crate::AppState::Playing);
            next.set(target);
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

/// Rebuild the build-summary panel inside the pause overlay
/// whenever the player opens it. Despawns any prior children of
/// the `PauseBuildSummary` container and spawns a fresh tree from
/// the live [`TurretConfig`] + [`PurchasedMods`] snapshots.
///
/// Cheap to run — only fires on the pause-state transition, not
/// every frame. Returns silently if the container hasn't been
/// spawned yet (very-early-frame race after Startup).
pub fn refresh_pause_build_summary(
    mut commands: Commands,
    container_q: Query<Entity, With<PauseBuildSummary>>,
    cfg: Res<TurretConfig>,
    purchased: Res<PurchasedMods>,
    pixel: Option<Res<crate::fonts::PixelFont>>,
    thaleah: Option<Res<crate::fonts::ThaleahFont>>,
) {
    let Ok(container) = container_q.single() else { return };
    commands.entity(container).despawn_related::<Children>();
    commands.entity(container).with_children(|parent| {
        spawn_build_summary(
            parent,
            cfg.as_ref(),
            purchased.as_ref(),
            pixel.as_deref(),
            thaleah.as_deref(),
        );
    });
}
