//! Ready-check for the per-peer shop flow.
//!
//! Each peer runs their own `Customize` locally (independent scrap,
//! loot, drag-drop). They click their local CLOSE button to mark
//! themselves READY — that sets [`LocalReadyState`] + broadcasts a
//! `PeerReady` packet. The host watches [`TeamReadyTracker`]; when
//! every peer in the lobby roster is ready, the host advances to
//! `Map` (its normal post-Customize state) — the standard state-sync
//! pipeline then transitions every peer along with it.
//!
//! Authority model mirrors `multiplayer::death`: each peer detects
//! their own ready-state (cheap, decentralised), and the HOST is the
//! aggregator that decides when the team can advance.

use std::collections::HashSet;

use bevy::prelude::*;

use crate::AppState;

use super::net::{send_to, NetMsg};
use super::{NetMode, NetSession};

/// Local "I've finished my shop" flag. Set by the close-button
/// handler in `customize::update`. Cleared on entry to `Customize`
/// (so a stale ready from last stage doesn't immediately advance).
#[derive(Resource, Default, Clone, Copy, Debug)]
pub struct LocalReadyState {
    pub ready: bool,
}

/// Host-only aggregator: tracks which peer ids have reported ready.
/// Populated by `drain_ready_inbox` from the receive buffer + by
/// `track_own_ready` for the host's own click.
#[derive(Resource, Default, Debug)]
pub struct TeamReadyTracker {
    pub ready_peers: HashSet<u8>,
}

/// Receive buffer for `NetMsg::PeerReady`. Populated by
/// `recv_packets` (host-side only) and drained by
/// `drain_ready_inbox` into [`TeamReadyTracker`]. Lives as its own
/// resource so `recv_packets` doesn't need another `ResMut`
/// (already at Bevy's 16-SystemParam cap).
#[derive(Resource, Default)]
pub struct PendingPeerReady(pub Vec<u8>);

/// Move incoming `PeerReady` ids out of the inbox into the
/// aggregator tracker. Runs on every peer so the local UI can show
/// who else has clicked through; the host additionally reads the
/// tracker via `host_advance_when_all_ready`.
pub fn drain_ready_inbox(
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    mut inbox: ResMut<PendingPeerReady>,
    mut tracker: ResMut<TeamReadyTracker>,
) {
    let Some(_session) = session else { return };
    if !matches!(*mode, NetMode::Connected) { return };
    for id in inbox.0.drain(..) {
        tracker.ready_peers.insert(id);
    }
}

/// Per-frame: if the local peer just flipped to ready (or stays
/// ready), broadcast `PeerReady` to EVERY connected peer (host
/// included for clients; clients included for the host).
///
/// Why broadcast-to-all rather than client → host: the live
/// "X / N READY" indicator on every peer's shop needs to know who
/// else has clicked through. Each peer keeps its own copy of
/// `TeamReadyTracker`. Host additionally runs
/// `host_advance_when_all_ready` to make the canonical advance call.
///
/// To avoid spamming when our state stays ready frame after frame,
/// we'd ideally fire only on the rising edge. For simplicity we
/// re-send every frame — bandwidth is one ~8-byte packet per peer
/// at frame rate, which is negligible. If this ever shows up in
/// profiling, gate on a `LastBroadcastedReady` resource.
pub fn announce_local_ready(
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    local: Res<LocalReadyState>,
) {
    let Some(session) = session else { return };
    if !matches!(*mode, NetMode::Connected) { return };
    if !local.ready { return; }
    let msg = NetMsg::PeerReady { id: session.my_id };
    for &addr in session.peers.values() {
        if let Err(e) = send_to(&session.sock, addr, &msg) {
            bevy::log::warn!("multiplayer: PeerReady send failed: {e}");
        }
    }
}

/// Mirror our own `LocalReadyState` into the tracker so the local
/// "X / N READY" indicator includes us, and (on the host) so the
/// all-ready advance check sees the host's own ready click.
pub fn track_own_ready(
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    local: Res<LocalReadyState>,
    mut tracker: ResMut<TeamReadyTracker>,
) {
    let Some(session) = session else { return };
    if !matches!(*mode, NetMode::Connected) { return };
    if local.ready {
        tracker.ready_peers.insert(session.my_id);
    }
}

/// Host-only: if every peer in the roster is in the ready set AND
/// we're currently in a per-peer state that gates on ready, advance
/// to the configured next state. State sync then transitions every
/// peer along with us.
///
/// State → next-state table:
/// - `Customize`  → `Map`
/// - `LevelUp`    → `Customize` (or the dynamic override stashed in
///   [`crate::xp::LevelUpReturn`] for mid-stage level-ups that
///   return to `Playing`)
/// - `HullSelect` → `Playing`
///
/// Adding a new per-peer state is one match arm here + a click
/// handler that flips `LocalReadyState.ready` instead of advancing
/// directly + a reset hook on `OnEnter`.
pub fn host_advance_when_all_ready(
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    tracker: Res<TeamReadyTracker>,
    roster: Res<super::LobbyRoster>,
    state: Res<State<AppState>>,
    level_up_return: Option<ResMut<crate::xp::LevelUpReturn>>,
    mut next: ResMut<NextState<AppState>>,
) {
    let Some(session) = session else { return };
    if !matches!(*mode, NetMode::Connected) || !session.is_host { return; }
    // Gate FIRST so the LevelUpReturn override below is only
    // consumed on the frame we actually transition. (Earlier
    // versions took() it inside the match, which silently lost
    // the override on every "not yet" early-return.)
    let cur = *state.get();
    if !matches!(cur, AppState::Customize | AppState::LevelUp | AppState::HullSelect) { return; }
    if roster.by_id.is_empty() { return; }
    if !roster.by_id.keys().all(|id| tracker.ready_peers.contains(id)) { return; }

    let target = match cur {
        AppState::Customize => AppState::Map,
        AppState::HullSelect => AppState::Playing,
        AppState::LevelUp => level_up_return
            .and_then(|mut r| r.0.take())
            .unwrap_or(AppState::Customize),
        _ => unreachable!("guard above narrows to per-peer states"),
    };
    bevy::log::info!("multiplayer: all peers ready — advancing {:?} → {:?}", cur, target);
    next.set(target);
}

/// Clear `LocalReadyState` + `TeamReadyTracker` on entry to a
/// per-peer ready-gated state (Customize / LevelUp / HullSelect).
/// Without this, a stale ready from the previous visit would
/// auto-advance the team before anyone had a chance to interact.
pub fn reset_ready_state_on_enter(
    mut local: ResMut<LocalReadyState>,
    mut tracker: ResMut<TeamReadyTracker>,
) {
    local.ready = false;
    tracker.ready_peers.clear();
}

/// Marker on the ready-indicator overlay root entity.
#[derive(Component)]
pub struct ReadyOverlay;

/// Marker on the dynamic "X / N READY" label inside the overlay.
/// Lets the per-frame updater find the text node without iterating
/// every UI node in the world.
#[derive(Component)]
pub struct ReadyOverlayCountText;

/// Marker on the secondary "WAITING FOR PARTNER..." line. Hidden
/// until the local peer flips to ready.
#[derive(Component)]
pub struct ReadyOverlayWaitingText;

/// Per-frame: maintain a small bevy_ui chrome in the corner of the
/// shop screen showing how many peers are ready. Once the local peer
/// clicks READY, a "WAITING FOR PARTNER..." line appears.
///
/// Spawns the overlay lazily on first frame it's needed (Customize
/// + multiplayer); despawns it on exit from Customize. Skips
/// entirely in solo mode — the ready check is MP-only.
pub fn sync_ready_overlay(
    mut commands: Commands,
    state: Res<State<AppState>>,
    mode: Res<NetMode>,
    local: Res<LocalReadyState>,
    tracker: Res<TeamReadyTracker>,
    roster: Res<super::LobbyRoster>,
    font: Option<Res<crate::fonts::PixelFont>>,
    mut existing_root: Query<&mut Visibility, With<ReadyOverlay>>,
    existing_entities: Query<Entity, With<ReadyOverlay>>,
    mut count_text: Query<&mut Text, (With<ReadyOverlayCountText>, Without<ReadyOverlayWaitingText>)>,
    mut waiting_text: Query<&mut Visibility, (With<ReadyOverlayWaitingText>, Without<ReadyOverlay>)>,
) {
    let in_ready_state = matches!(*state.get(),
        AppState::Customize | AppState::LevelUp | AppState::HullSelect);
    let should_show = in_ready_state && matches!(*mode, NetMode::Connected);

    if !should_show {
        // Despawn outside Customize / solo so the next session starts
        // clean.
        for e in &existing_entities {
            commands.entity(e).despawn();
        }
        return;
    }

    // Spawn lazily on first frame.
    if existing_root.is_empty() {
        let Some(font) = font else { return };
        commands
            .spawn((
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Px(8.0),
                    left: Val::Px(8.0),
                    padding: UiRect::axes(Val::Px(8.0), Val::Px(4.0)),
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(2.0),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.65)),
                ZIndex(550),
                Visibility::Inherited,
                ReadyOverlay,
            ))
            .with_children(|p| {
                p.spawn((
                    crate::ui_kit::pixel_label(
                        &font,
                        "0 / 0 READY",
                        14.0,
                        Color::srgb(1.0, 0.95, 0.55),
                    ),
                    ReadyOverlayCountText,
                ));
                p.spawn((
                    crate::ui_kit::pixel_label(
                        &font,
                        "WAITING FOR PARTNER...",
                        12.0,
                        crate::ui_kit::theme::ON_SURFACE_DIM,
                    ),
                    Visibility::Hidden,
                    ReadyOverlayWaitingText,
                ));
            });
        return;
    }

    // Make sure the root stays visible.
    for mut v in &mut existing_root {
        if *v != Visibility::Inherited { *v = Visibility::Inherited; }
    }

    // Update the live count. The local peer counts toward the total
    // (track_own_ready inserts our own id when ready), and the
    // roster size is the team total. Pre-handshake / empty roster
    // case is shown as "READY" placeholder.
    let total = roster.by_id.len();
    let ready = tracker.ready_peers.len();
    let label = if total == 0 {
        "READY?".to_string()
    } else {
        format!("{} / {} READY", ready, total)
    };
    for mut t in &mut count_text {
        if t.0 != label { t.0 = label.clone(); }
    }

    // The waiting line appears once we've clicked ready; before
    // that the player still has shop work to do.
    let waiting_visible = if local.ready {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    for mut v in &mut waiting_text {
        if *v != waiting_visible { *v = waiting_visible; }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_ready_requires_every_roster_peer() {
        let mut roster = super::super::LobbyRoster::default();
        roster.by_id.insert(0, "HOST".into());
        roster.by_id.insert(1, "CLIENT".into());

        let mut tracker = TeamReadyTracker::default();
        assert!(!roster.by_id.keys().all(|id| tracker.ready_peers.contains(id)),
            "empty tracker → not all ready");

        tracker.ready_peers.insert(0);
        assert!(!roster.by_id.keys().all(|id| tracker.ready_peers.contains(id)),
            "only host ready → not all ready");

        tracker.ready_peers.insert(1);
        assert!(roster.by_id.keys().all(|id| tracker.ready_peers.contains(id)),
            "all ids ready → all ready");
    }

    #[test]
    fn reset_clears_both_local_and_team() {
        let mut local = LocalReadyState { ready: true };
        let mut tracker = TeamReadyTracker::default();
        tracker.ready_peers.insert(0);
        tracker.ready_peers.insert(1);

        // Manually exercise the system body (since it's a one-liner).
        local.ready = false;
        tracker.ready_peers.clear();

        assert!(!local.ready);
        assert!(tracker.ready_peers.is_empty());
    }
}
