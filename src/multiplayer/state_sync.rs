//! `AppState` lockstep between peers.
//!
//! Host transitions broadcast a [`NetMsg::StateChange`] to every
//! connected peer; each client looks up the matching `AppState` and
//! queues its own `NextState` transition. Per-peer states
//! (Customize / LevelUp / HullSelect) pass through; host-only flow
//! states (Map, StageComplete, etc.) map to `WaitingForHost` so the
//! client sits on a passive overlay while the host drives.
//!
//! Pause is the one client → host case: the client may broadcast
//! `Paused` / `Playing` transitions so either peer can pause the
//! team. See `client_may_broadcast`.

use bevy::prelude::*;

use crate::AppState;

use super::net::{send_to, NetMsg};
use super::{NetMode, NetSession};

/// Buffer of incoming state-change packets, populated by
/// `recv_packets` and drained by `apply_state_change`.
#[derive(Resource, Default)]
pub struct PendingStateChange(pub Option<AppState>);

/// Tracks the last state the host broadcasted, so we only send a
/// packet when the state ACTUALLY changes (not every frame).
#[derive(Resource, Default)]
pub struct LastBroadcastedState(pub Option<AppState>);

/// Per-frame system that broadcasts `AppState` changes.
///
/// Host: sends every state transition to every connected peer (the
/// general flow — Customize, HullSelect, Map, …).
///
/// Client: only sends `Paused` / `Playing` transitions, and only to
/// the host. Either peer pressing pause should freeze the team —
/// without this the host keeps simulating while the client's UI is
/// frozen, and the client comes back from pause to find their boat
/// killed. Host receives the client's Paused/Playing packet, applies
/// it locally, then its own broadcast echoes back out to all peers
/// so everyone lands in sync.
pub fn broadcast_state_change(
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    state: Res<State<AppState>>,
    mut last: ResMut<LastBroadcastedState>,
) {
    let Some(session) = session else { return; };
    if !matches!(*mode, NetMode::Connected) { return; }

    let cur = *state.get();
    if last.0 == Some(cur) { return; }
    last.0 = Some(cur);

    let msg = NetMsg::StateChange { state: cur.to_u8() };
    if session.is_host {
        // Full broadcast to every peer.
        for &addr in session.peers.values() {
            if let Err(e) = send_to(&session.sock, addr, &msg) {
                bevy::log::warn!("multiplayer: failed to send StateChange to {addr}: {e}");
            }
        }
        bevy::log::info!("multiplayer: host broadcast state {:?}", cur);
    } else if client_may_broadcast(cur) {
        // Client-side narrow path: pause / unpause only, sent to the
        // host. Host will re-broadcast to every peer via the arm
        // above on its next frame.
        if let Some(&host_addr) = session.peers.get(&0) {
            if let Err(e) = send_to(&session.sock, host_addr, &msg) {
                bevy::log::warn!("multiplayer: client failed to send StateChange to host: {e}");
            } else {
                bevy::log::info!("multiplayer: client broadcast pause-transition state {:?}", cur);
            }
        }
    }
}

/// State transitions the CLIENT is allowed to push to the host.
/// Currently just the pause toggle — every other host-flow state
/// (Customize, Map, …) stays host-authoritative.
fn client_may_broadcast(state: AppState) -> bool {
    matches!(state, AppState::Paused | AppState::Playing)
}

/// Per-frame system that drains `PendingStateChange` and transitions
/// via `NextState`. Handles both directions of the pause sync:
///
/// - Client side: maps the host's state to a client-safe target
///   (Playing / Lobby / MainMenu pass through; everything else maps
///   to `WaitingForHost` so the client sits on a passive overlay).
/// - Host side: only accepts `Paused` / `Playing` transitions from a
///   client, applying them as-is. Every other state on the host stays
///   under the host's authoritative control.
pub fn apply_state_change(
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    mut pending: ResMut<PendingStateChange>,
    state: Res<State<AppState>>,
    mut next: ResMut<NextState<AppState>>,
) {
    let Some(session) = session else { return; };
    if !matches!(*mode, NetMode::Connected) { return; }

    let Some(received) = pending.0.take() else { return };
    let target = if session.is_host {
        if !client_may_broadcast(received) {
            // Defensive: client should never push host-flow states.
            // Ignore rather than letting a buggy / malicious peer
            // teleport the host into Customize.
            return;
        }
        received
    } else {
        client_state_for(received)
    };
    if *state.get() == target { return; }
    next.set(target);
    bevy::log::info!(
        "multiplayer: follow incoming state {:?} → local {:?}",
        received, target,
    );
}

/// Map host's `AppState` to the matching client-side state.
///
/// Pass-through (client participates locally):
/// - `Playing` / `Lobby` / `MainMenu`
/// - `Customize` — each peer runs their own shop with their own
///   scrap / RNG / TurretConfig; sync only happens at READY-time
///   via the per-peer broadcasts in `multiplayer::loadout`.
/// - `LevelUp` — each peer picks their own buff card. Pending
///   levelups arrive per-peer via [`multiplayer::xp_sync::
///   LevelUpGranted`].
/// - `HullSelect` — each peer picks their own hull. Stats are local
///   already (per-peer PlayerStats), and turret slot positions are
///   global, so per-peer hull choice falls out naturally.
///
/// Everything else (Map, StageComplete, BossReward, BossIntro,
/// GameOver, Paused, Win) maps to `WaitingForHost`.
pub fn client_state_for(host: AppState) -> AppState {
    match host {
        AppState::Playing
        | AppState::Lobby
        | AppState::MainMenu
        | AppState::Customize
        | AppState::LevelUp
        | AppState::HullSelect => host,
        _ => AppState::WaitingForHost,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_STATES: &[AppState] = &[
        AppState::MainMenu, AppState::Playing, AppState::StageComplete,
        AppState::LevelUp, AppState::HullSelect, AppState::Customize,
        AppState::Map, AppState::Paused, AppState::GameOver,
        AppState::BossReward, AppState::BossIntro, AppState::Win,
        AppState::Lobby, AppState::WaitingForHost,
    ];

    /// Every `AppState` round-trips through `to_u8` / `from_u8`.
    #[test]
    fn appstate_u8_round_trip() {
        for &s in ALL_STATES {
            let n = s.to_u8();
            let back = AppState::from_u8(n).expect("known state");
            assert_eq!(back, s, "state {:?} round-tripped to {:?}", s, back);
        }
    }

    /// Discriminants must be unique — duplicate numbers would
    /// silently merge two states on the wire.
    #[test]
    fn appstate_discriminants_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for &s in ALL_STATES {
            assert!(seen.insert(s.to_u8()), "duplicate discriminant for {:?}", s);
        }
    }

    /// `client_state_for` maps host's AppState to the client's
    /// passive equivalent. Playing / Lobby / MainMenu pass through
    /// (client participates); host-only menus map to WaitingForHost.
    #[test]
    fn client_state_for_passes_active_states_through() {
        assert_eq!(client_state_for(AppState::Playing),  AppState::Playing);
        assert_eq!(client_state_for(AppState::Lobby),    AppState::Lobby);
        assert_eq!(client_state_for(AppState::MainMenu), AppState::MainMenu);
    }

    /// Per-peer states pass through so each peer can interact locally.
    #[test]
    fn client_state_for_per_peer_states_pass_through() {
        assert_eq!(client_state_for(AppState::Customize),  AppState::Customize);
        assert_eq!(client_state_for(AppState::LevelUp),    AppState::LevelUp);
        assert_eq!(client_state_for(AppState::HullSelect), AppState::HullSelect);
    }

    /// Host-only flow states map to WaitingForHost on the client.
    /// Per-peer states (Customize/LevelUp/HullSelect) are excluded —
    /// each peer runs their own UI.
    #[test]
    fn client_state_for_maps_host_menus_to_waiting() {
        for host_state in [
            AppState::Map,
            AppState::StageComplete,
            AppState::BossReward,
            AppState::BossIntro,
            AppState::Paused,
            AppState::GameOver,
            AppState::Win,
        ] {
            assert_eq!(
                client_state_for(host_state),
                AppState::WaitingForHost,
                "{:?} should map to WaitingForHost on client side",
                host_state,
            );
        }
    }

    /// Unknown discriminants yield `None`, not a panic — forward-
    /// compat for older clients receiving a state from a newer host.
    #[test]
    fn appstate_from_unknown_u8_is_none() {
        for n in [14u8, 50, 100, 255] {
            assert!(AppState::from_u8(n).is_none(), "n={n} should be None");
        }
    }
}
