//! bevy_ui status overlay for the multiplayer pre-connection states.
//! Lives independently of the main-menu chunky-pixel chrome so it can
//! use crisp bevy_ui text without fighting the menu's mesh-based
//! buttons. Single Node, single text child, always present (hidden
//! when `NetMode::Solo` or `Connected`).
//!
//! What it shows:
//! - `Hosting`        : "HOSTING ON x.x.x.x:port — WAITING (ESC to cancel)"
//! - `JoiningEntry`   : "ENTER HOST IP: <typed buf>_" + hint line
//! - `JoiningWait`    : "CONNECTING..."
//! - `Solo` / `Connected` : hidden

use bevy::prelude::*;

use crate::AppState;

use super::{tear_down_session, HostStatus, JoinIpEntry, NetMode, NetSession};

/// Marker on the outer overlay container Node. Lets the update
/// system find it without iterating every UI node in the world.
#[derive(Component)]
pub struct NetStatusOverlay;

/// Marker on the inner text node. Updated each frame with the
/// status string for the current `NetMode`.
#[derive(Component)]
pub struct NetStatusText;

/// Spawn the (initially hidden) overlay once at Startup. We don't gate
/// it on `MainMenu` enter because the menu state can be re-entered
/// many times and respawning the overlay would race the menu's own
/// chrome lifecycle.
pub fn setup_overlay(mut commands: Commands, font: Res<crate::fonts::PixelFont>) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Percent(8.0),
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(4.0),
                ..default()
            },
            Visibility::Hidden,
            // High ZIndex so the banner sits above the menu chrome
            // sprite (which is on its own render layer but the
            // bevy_ui pass sees its ZIndex range too).
            ZIndex(500),
            NetStatusOverlay,
        ))
        .with_children(|p| {
            p.spawn((
                crate::ui_kit::pixel_label(
                    &font,
                    "",
                    16.0,
                    Color::srgb(0.97, 0.98, 0.95),
                ),
                TextShadow {
                    offset: Vec2::splat(1.0),
                    color: Color::srgba(0.0, 0.0, 0.0, 0.85),
                },
                NetStatusText,
            ));
        });
}

/// Per-frame: update overlay visibility + text from `NetMode`. Cheap —
/// short-circuits when nothing changed.
pub fn update_overlay(
    mode: Res<NetMode>,
    host: Res<HostStatus>,
    join: Res<JoinIpEntry>,
    name: Res<super::LocalPlayerName>,
    state: Res<bevy::prelude::State<crate::AppState>>,
    mut overlay_q: Query<&mut Visibility, With<NetStatusOverlay>>,
    mut text_q: Query<&mut Text, With<NetStatusText>>,
) {
    // Solo on the main menu shows the name editor; Solo elsewhere
    // (e.g., mid-game) hides it. Connected hides it (lobby UI owns
    // its own chrome).
    let on_main_menu = *state.get() == crate::AppState::MainMenu;
    let (want_vis, want_text) = match *mode {
        NetMode::Solo if on_main_menu => (
            Visibility::Inherited,
            format!("YOUR NAME: {}_\nA-Z 0-9 TO EDIT — DEFAULT WILL BE OVERWRITTEN ON FIRST KEY",
                    name.0),
        ),
        NetMode::Solo | NetMode::Connected => (Visibility::Hidden, String::new()),
        NetMode::Hosting => (
            Visibility::Inherited,
            format!(
                "HOSTING ON {}:{}\nWAITING FOR A FRIEND TO JOIN — ESC TO CANCEL",
                host.lan_ip, host.port,
            ),
        ),
        NetMode::JoiningEntry => {
            let err = join.last_error.as_deref().unwrap_or("");
            (
                Visibility::Inherited,
                if err.is_empty() {
                    format!(
                        "ENTER HOST IP: {}_\nDIGITS . AND : — ENTER TO CONNECT, ESC TO CANCEL",
                        join.buf,
                    )
                } else {
                    format!(
                        "ENTER HOST IP: {}_\n{}\nDIGITS . AND : — ENTER TO CONNECT, ESC TO CANCEL",
                        join.buf, err,
                    )
                },
            )
        }
        NetMode::JoiningWait => (
            Visibility::Inherited,
            "CONNECTING...".to_string(),
        ),
    };
    if let Ok(mut v) = overlay_q.single_mut() {
        if *v != want_vis { *v = want_vis; }
    }
    if let Ok(mut t) = text_q.single_mut() {
        if t.0 != want_text { t.0 = want_text; }
    }
}

/// ESC handler for the `Hosting` and `JoiningWait` states. (The
/// `JoiningEntry` ESC is handled inside `capture_join_ip_keys`
/// because that system already owns the entry buf.) Tears the
/// session down so the next attempt starts clean.
pub fn cancel_on_esc(
    keys: Res<ButtonInput<KeyCode>>,
    mut commands: Commands,
    mut mode: ResMut<NetMode>,
    session: Option<Res<NetSession>>,
    state: Res<State<AppState>>,
) {
    // Only relevant on the MainMenu — once we're in Playing the
    // gameplay ESC (pause) takes priority.
    if *state.get() != AppState::MainMenu { return; }
    if !keys.just_pressed(KeyCode::Escape) { return; }
    if !matches!(*mode, NetMode::Hosting | NetMode::JoiningWait) { return; }
    tear_down_session(&mut commands, &mut mode, session.as_deref());
    bevy::log::info!("multiplayer: cancelled by ESC");
}
