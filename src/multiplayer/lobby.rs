//! Lobby waiting-room UI. Players land here after handshake; host
//! clicks START to launch into `Playing`. Visual vocabulary matches
//! the main menu (`ui_kit::chunky_*` + `pixel_label`) so the
//! transition feels continuous.
//!
//! Layout (top-down):
//! - `LOBBY` header (Thaleah font, accent gold)
//! - Host status banner: `HOSTING ON 192.168.x.x:49333` (host only)
//! - Player list — one row per peer in `LobbyRoster`:
//!   - Name + `[HOST]` badge (if id 0)
//!   - KICK button (host only, on every non-host row)
//! - Bottom row: START (host only, disabled if no peers) + LEAVE (both)
//!
//! Click handlers:
//! - START → host transitions `Lobby → Playing`; `broadcast_state_change`
//!   propagates to clients.
//! - KICK → host sends `NetMsg::Kicked` to the target peer, removes
//!   them from peers + roster, broadcasts `PeerLeft` to others.
//! - LEAVE → tear down session, transition `Lobby → MainMenu`.
//! - ESC → same as LEAVE.

use bevy::prelude::*;
use std::net::SocketAddr;

use crate::ui_kit::{self, theme, ChunkyButtonStyle};
use crate::AppState;

use super::net::{send_to, NetMsg};
use super::{LobbyRoster, NetMode, NetSession, PendingKick};

/// Marker on the root overlay node. Used for OnExit cleanup so the
/// whole tree drops on leaving the Lobby.
#[derive(Component)]
pub struct LobbyOverlay;

/// Marker on the parent of the player-list rows. Per-frame system
/// rebuilds children when the roster changes.
#[derive(Component)]
pub struct LobbyRosterColumn;

/// Marker on the START button so the click handler can find it.
#[derive(Component)]
pub struct StartButton;

/// Marker on the LEAVE button.
#[derive(Component)]
pub struct LeaveButton;

/// Marker on per-row KICK buttons. Carries the peer id so the click
/// handler knows who to boot.
#[derive(Component, Clone, Copy)]
pub struct KickButton { pub peer_id: u8 }

/// Cache of the roster snapshot the UI was last rebuilt from. If the
/// live roster differs, we tear down the rows and respawn — simple +
/// robust against join/leave race conditions.
#[derive(Resource, Default)]
pub struct RenderedRosterRev(pub Vec<(u8, String)>);

/// Spawn the lobby overlay on entry. Layout uses bevy_ui with the
/// chunky vocabulary so it reads as a sibling of the main-menu chrome.
pub fn setup_lobby(
    mut commands: Commands,
    font: Res<crate::fonts::PixelFont>,
    thaleah: Res<crate::fonts::ThaleahFont>,
    host_status: Res<super::HostStatus>,
    session: Option<Res<NetSession>>,
) {
    let is_host = session.as_deref().map(|s| s.is_host).unwrap_or(false);

    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(0.0),
            left: Val::Px(0.0),
            right: Val::Px(0.0),
            bottom: Val::Px(0.0),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            padding: UiRect::all(Val::Px(theme::PAD_LG * 2.0)),
            row_gap: Val::Px(theme::GAP_LG),
            ..default()
        },
        BackgroundColor(theme::SURFACE),
        ZIndex(150),
        Visibility::Inherited,
        LobbyOverlay,
    ))
    .with_children(|root| {
        // ---------- Header: "LOBBY" in Thaleah ----------
        root.spawn((
            Text::new("LOBBY"),
            crate::fonts::thaleah_text_font(&thaleah, 56.0),
            TextColor(theme::ACCENT),
            TextShadow {
                offset: Vec2::splat(2.0),
                color: Color::srgba(0.0, 0.0, 0.0, 0.85),
            },
        ));

        // ---------- Host status banner ----------
        if is_host {
            root.spawn((
                ui_kit::pixel_label(
                    &font,
                    format!("HOSTING ON {}:{}", host_status.lan_ip, host_status.port),
                    theme::FONT_LG,
                    theme::ON_SURFACE_DIM,
                ),
            ));
        } else {
            root.spawn((
                ui_kit::pixel_label(
                    &font,
                    "WAITING FOR HOST TO START...",
                    theme::FONT_LG,
                    theme::ON_SURFACE_DIM,
                ),
            ));
        }

        // ---------- Player list panel ----------
        root.spawn((
            Node {
                width: Val::Px(420.0),
                min_height: Val::Px(180.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Stretch,
                padding: UiRect::all(Val::Px(theme::PAD_LG)),
                border: UiRect::all(Val::Px(theme::CHUNKY_BORDER_W)),
                row_gap: Val::Px(theme::GAP_SM),
                ..default()
            },
            BackgroundColor(theme::SURFACE_RAISED),
            BorderColor(theme::CHUNKY_OUTLINE),
            BorderRadius::all(Val::Px(theme::CHUNKY_RADIUS)),
            LobbyRosterColumn,
        ));

        // ---------- Bottom action row: START + LEAVE ----------
        root.spawn((
            Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                column_gap: Val::Px(theme::GAP_LG),
                margin: UiRect::top(Val::Px(theme::PAD_LG)),
                ..default()
            },
            BackgroundColor(Color::NONE),
        ))
        .with_children(|row| {
            if is_host {
                spawn_start_button(row, &font);
            }
            spawn_leave_button(row, &font);
        });
    });

    // Reset the rendered-roster cache so the first refresh repopulates.
    commands.insert_resource(RenderedRosterRev::default());
}

/// Tear down the overlay on exit.
pub fn teardown_lobby(
    mut commands: Commands,
    overlays: Query<Entity, With<LobbyOverlay>>,
) {
    for e in &overlays {
        commands.entity(e).despawn();
    }
    commands.remove_resource::<RenderedRosterRev>();
}

/// Per-frame: if the roster snapshot differs from what's rendered,
/// despawn and respawn the row children. Cheap — only fires on
/// join/leave/kick.
pub fn refresh_roster(
    mut commands: Commands,
    font: Res<crate::fonts::PixelFont>,
    roster: Res<LobbyRoster>,
    session: Option<Res<NetSession>>,
    column_q: Query<Entity, With<LobbyRosterColumn>>,
    mut rendered: Option<ResMut<RenderedRosterRev>>,
) {
    let Some(rendered) = rendered.as_mut() else { return };
    let Ok(column) = column_q.single() else { return };

    // Snapshot the current roster, sorted by id for stable ordering.
    let mut current: Vec<(u8, String)> = roster.by_id.iter()
        .map(|(&id, n)| (id, n.clone()))
        .collect();
    current.sort_by_key(|(id, _)| *id);

    if current == rendered.0 { return; }
    rendered.0 = current.clone();

    // Wipe + repopulate.
    commands.entity(column).despawn_related::<Children>();
    let is_host_local = session.as_deref().map(|s| s.is_host).unwrap_or(false);
    commands.entity(column).with_children(|col| {
        if current.is_empty() {
            col.spawn(ui_kit::pixel_label(
                &font, "WAITING FOR PLAYERS...",
                theme::FONT_LG, theme::ON_SURFACE_DIM,
            ));
            return;
        }
        for (id, name) in current.iter() {
            spawn_roster_row(col, &font, *id, name, is_host_local);
        }
    });
}

fn spawn_roster_row(
    parent: &mut ChildSpawnerCommands,
    font: &crate::fonts::PixelFont,
    id: u8,
    name: &str,
    viewer_is_host: bool,
) {
    parent.spawn((
        Node {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::SpaceBetween,
            padding: UiRect::axes(Val::Px(theme::PAD_MD), Val::Px(theme::PAD_SM)),
            border: UiRect::all(Val::Px(theme::CHUNKY_BORDER_W)),
            ..default()
        },
        BackgroundColor(theme::CHUNKY_FILL),
        BorderColor(theme::CHUNKY_OUTLINE),
        BorderRadius::all(Val::Px(theme::CHUNKY_RADIUS)),
    ))
    .with_children(|row| {
        // Left side: name + [HOST] badge if id 0.
        row.spawn((
            Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(theme::GAP_SM),
                ..default()
            },
            BackgroundColor(Color::NONE),
        ))
        .with_children(|left| {
            left.spawn(ui_kit::pixel_label(
                font, name, theme::FONT_LG, theme::ON_SURFACE,
            ));
            if id == 0 {
                left.spawn(ui_kit::pixel_label(
                    font, "[HOST]", theme::FONT_MD, theme::ACCENT,
                ));
            }
        });
        // Right side: KICK button (host only, never on the host row itself).
        if viewer_is_host && id != 0 {
            let style = ChunkyButtonStyle::neutral();
            row.spawn((
                Button,
                Node {
                    padding: UiRect::axes(Val::Px(theme::PAD_MD), Val::Px(theme::PAD_SM)),
                    border: UiRect::all(Val::Px(theme::CHUNKY_BORDER_W)),
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::Center,
                    ..default()
                },
                BackgroundColor(style.idle_fill),
                BorderColor(style.idle_outline),
                BorderRadius::all(Val::Px(theme::CHUNKY_RADIUS)),
                style,
                KickButton { peer_id: id },
            ))
            .with_children(|b| {
                b.spawn(ui_kit::pixel_label(font, "KICK", theme::FONT_MD, theme::ON_SURFACE));
            });
        }
    });
}

fn spawn_start_button(parent: &mut ChildSpawnerCommands, font: &crate::fonts::PixelFont) {
    let style = ChunkyButtonStyle::cta();
    parent.spawn((
        Button,
        Node {
            padding: UiRect::axes(Val::Px(theme::PAD_LG * 1.5), Val::Px(theme::PAD_MD)),
            border: UiRect::all(Val::Px(theme::CHUNKY_BORDER_W)),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            ..default()
        },
        BackgroundColor(style.idle_fill),
        BorderColor(style.idle_outline),
        BorderRadius::all(Val::Px(theme::CHUNKY_RADIUS)),
        style,
        StartButton,
    ))
    .with_children(|b| {
        b.spawn(ui_kit::pixel_label(font, "START", theme::FONT_LG, theme::ON_CTA));
    });
}

fn spawn_leave_button(parent: &mut ChildSpawnerCommands, font: &crate::fonts::PixelFont) {
    let style = ChunkyButtonStyle::neutral();
    parent.spawn((
        Button,
        Node {
            padding: UiRect::axes(Val::Px(theme::PAD_LG), Val::Px(theme::PAD_MD)),
            border: UiRect::all(Val::Px(theme::CHUNKY_BORDER_W)),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            ..default()
        },
        BackgroundColor(style.idle_fill),
        BorderColor(style.idle_outline),
        BorderRadius::all(Val::Px(theme::CHUNKY_RADIUS)),
        style,
        LeaveButton,
    ))
    .with_children(|b| {
        b.spawn(ui_kit::pixel_label(font, "LEAVE", theme::FONT_LG, theme::ON_SURFACE));
    });
}

/// START click → host transitions Lobby → Playing. State sync
/// (`broadcast_state_change`) carries clients along.
pub fn handle_start_click(
    interactions: Query<&Interaction, (Changed<Interaction>, With<StartButton>)>,
    mut next: ResMut<NextState<AppState>>,
) {
    for interaction in &interactions {
        if matches!(interaction, Interaction::Pressed) {
            next.set(AppState::Playing);
            return;
        }
    }
}

/// KICK click → host sends `Kicked { reason }` to the target peer,
/// removes them from the peer table + roster, broadcasts `PeerLeft`
/// to other clients so their rosters update too.
pub fn handle_kick_click(
    interactions: Query<(&Interaction, &KickButton), Changed<Interaction>>,
    mut session: Option<ResMut<NetSession>>,
    mut roster: ResMut<LobbyRoster>,
) {
    let Some(session) = session.as_mut() else { return };
    if !session.is_host { return };

    for (interaction, btn) in &interactions {
        if !matches!(interaction, Interaction::Pressed) { continue; }
        let peer_id = btn.peer_id;
        let Some(&peer_addr) = session.peers.get(&peer_id) else { continue };

        // Send the kick notice to the target.
        let _ = send_to(&session.sock, peer_addr, &NetMsg::Kicked {
            reason: "kicked by host".to_string(),
        });
        // Drop the peer from the host's tables.
        session.peers.remove(&peer_id);
        roster.by_id.remove(&peer_id);
        // Tell the remaining peers the kicked one is gone.
        let announce = NetMsg::PeerLeft { id: peer_id };
        let remaining: Vec<SocketAddr> = session.peers.values().copied().collect();
        for addr in remaining {
            let _ = send_to(&session.sock, addr, &announce);
        }
        bevy::log::info!("multiplayer: host kicked peer {peer_id}");
    }
}

/// Client-side: if `PendingKick` was set by `recv_packets`, tear
/// down the session and return to MainMenu (carrying the reason
/// into `JoinIpEntry.last_error` so the player sees why).
pub fn handle_received_kick(
    mut commands: Commands,
    mut pending_kick: ResMut<PendingKick>,
    mut mode: ResMut<NetMode>,
    mut roster: ResMut<LobbyRoster>,
    mut join_entry: ResMut<super::JoinIpEntry>,
    session: Option<Res<NetSession>>,
    state: Res<State<AppState>>,
    mut next: ResMut<NextState<AppState>>,
) {
    let Some(reason) = pending_kick.0.take() else { return };
    // Only act if we're actually in an MP-active state — stale
    // kicks from a previous session shouldn't fire.
    if !matches!(*state.get(),
                 AppState::Lobby | AppState::Playing | AppState::WaitingForHost)
    {
        return;
    }

    super::tear_down_session(&mut commands, &mut mode, session.as_deref());
    roster.by_id.clear();
    join_entry.last_error = Some(format!("kicked: {reason}"));
    next.set(AppState::MainMenu);
}

/// Host: detect that the last peer left (roster has only host's own
/// entry remaining AND peers table is empty) and... actually we
/// don't auto-do anything; host can just keep waiting in lobby. This
/// system exists as a hook for future polish (auto-close lobby if
/// alone for N seconds, etc.). Currently a no-op.
#[allow(dead_code)]
pub fn detect_empty_lobby_host(_session: Option<Res<NetSession>>) {}

// ---------- WaitingForHost overlay ----------

/// Marker on the WaitingForHost overlay root entity.
#[derive(Component)]
pub struct WaitingOverlay;

/// Spawn the "WAITING FOR HOST" overlay on entry to the state.
/// Chunky vocab; centered text + a LEAVE button so the client can
/// bail if the host is taking too long or stuck.
pub fn setup_waiting_overlay(
    mut commands: Commands,
    font: Res<crate::fonts::PixelFont>,
    thaleah: Res<crate::fonts::ThaleahFont>,
    roster: Res<super::LobbyRoster>,
) {
    let host_name = roster.by_id.get(&0).cloned()
        .unwrap_or_else(|| "HOST".to_string());

    commands.spawn((
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
        BackgroundColor(theme::SURFACE),
        ZIndex(160),
        Visibility::Inherited,
        WaitingOverlay,
    ))
    .with_children(|root| {
        // Big Thaleah header
        root.spawn((
            Text::new("WAITING FOR HOST"),
            crate::fonts::thaleah_text_font(&thaleah, 48.0),
            TextColor(theme::ACCENT),
            TextShadow {
                offset: Vec2::splat(2.0),
                color: Color::srgba(0.0, 0.0, 0.0, 0.85),
            },
        ));
        // Subtle subtitle showing the host's name
        root.spawn(ui_kit::pixel_label(
            &font,
            format!("{} IS MANAGING THE SHOP / MAP", host_name),
            theme::FONT_LG,
            theme::ON_SURFACE_DIM,
        ));
        // LEAVE button — same shape as the lobby's so it reads
        // consistently. Reuses the existing LeaveButton marker so
        // handle_leave_click already routes it (the handler is
        // gated to Lobby; we extend that below).
        let style = ChunkyButtonStyle::neutral();
        root.spawn((
            Button,
            Node {
                padding: UiRect::axes(Val::Px(theme::PAD_LG), Val::Px(theme::PAD_MD)),
                border: UiRect::all(Val::Px(theme::CHUNKY_BORDER_W)),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                margin: UiRect::top(Val::Px(theme::PAD_LG)),
                ..default()
            },
            BackgroundColor(style.idle_fill),
            BorderColor(style.idle_outline),
            BorderRadius::all(Val::Px(theme::CHUNKY_RADIUS)),
            style,
            LeaveButton,
        ))
        .with_children(|b| {
            b.spawn(ui_kit::pixel_label(&font, "LEAVE", theme::FONT_LG, theme::ON_SURFACE));
        });
    });
}

/// Tear down the overlay on exit.
pub fn teardown_waiting_overlay(
    mut commands: Commands,
    overlays: Query<Entity, With<WaitingOverlay>>,
) {
    for e in &overlays {
        commands.entity(e).despawn();
    }
}

/// Generalised LEAVE handler — runs in Lobby + WaitingForHost. ESC
/// also leaves. Tears down session, returns to MainMenu. Extends the
/// original Lobby-only `handle_leave_click` so the WaitingForHost
/// overlay's LEAVE button + ESC work without a duplicate system.
pub fn handle_leave_click_any_mp(
    interactions: Query<&Interaction, (Changed<Interaction>, With<LeaveButton>)>,
    keys: Res<ButtonInput<KeyCode>>,
    state: Res<State<AppState>>,
    mut commands: Commands,
    mut mode: ResMut<NetMode>,
    mut roster: ResMut<super::LobbyRoster>,
    session: Option<Res<NetSession>>,
    mut next: ResMut<NextState<AppState>>,
) {
    let in_mp_screen = matches!(
        *state.get(),
        AppState::Lobby | AppState::WaitingForHost,
    );
    if !in_mp_screen { return; }

    let clicked = interactions.iter().any(|i| matches!(i, Interaction::Pressed));
    let esc     = keys.just_pressed(KeyCode::Escape);
    if !clicked && !esc { return; }

    super::tear_down_session(&mut commands, &mut mode, session.as_deref());
    roster.by_id.clear();
    next.set(AppState::MainMenu);
}
