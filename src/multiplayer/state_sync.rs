//! Phase 3 foundation — `AppState` lockstep between peers.
//!
//! When the host transitions `AppState` (MainMenu → HullSelect →
//! Playing → Customize → …), it broadcasts a [`NetMsg::StateChange`]
//! to every connected peer. Each client receives the packet, looks
//! up the matching `AppState`, and queues its own `NextState`
//! transition. Both peers land on the same screen on the next frame.
//!
//! This is the load-bearing primitive that future Phase 3 work
//! (shared customize loadout, shared scrap, shared XP) will build
//! on. Without it, the host going to Customize strands the client
//! in `Playing` and the two screens diverge.
//!
//! Authority model: only the **host** broadcasts transitions. Client
//! transitions stay local — if a client opens its pause menu, the
//! host doesn't follow. This asymmetry matches the rest of the
//! multiplayer design: host owns shared world flow, client owns
//! their own input + viewport.

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

/// Host-only per-frame system. On `AppState` change, broadcasts a
/// `StateChange` packet to every connected client. Cheap to run on
/// solo / client too because the host check short-circuits.
pub fn broadcast_state_change(
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    state: Res<State<AppState>>,
    mut last: ResMut<LastBroadcastedState>,
) {
    // Host-only.
    let Some(session) = session else { return; };
    if !matches!(*mode, NetMode::Connected) || !session.is_host { return; }

    let cur = *state.get();
    if last.0 == Some(cur) { return; }
    last.0 = Some(cur);

    let msg = NetMsg::StateChange { state: cur.to_u8() };
    for &addr in session.peers.values() {
        if let Err(e) = send_to(&session.sock, addr, &msg) {
            bevy::log::warn!("multiplayer: failed to send StateChange to {addr}: {e}");
        }
    }
    bevy::log::info!("multiplayer: host broadcast state {:?}", cur);
}

/// Client-only per-frame system. Drains `PendingStateChange` and
/// transitions to the host's reported state via `NextState`. Skips
/// if the local state already matches — avoids redundant state
/// transitions when the host re-broadcasts.
pub fn apply_state_change(
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    mut pending: ResMut<PendingStateChange>,
    state: Res<State<AppState>>,
    mut next: ResMut<NextState<AppState>>,
) {
    // Client-only.
    let Some(session) = session else { return; };
    if !matches!(*mode, NetMode::Connected) || session.is_host { return; }

    let Some(target) = pending.0.take() else { return };
    if *state.get() == target { return; }
    next.set(target);
    bevy::log::info!("multiplayer: client follows host into {:?}", target);
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_STATES: &[AppState] = &[
        AppState::MainMenu, AppState::Playing, AppState::StageComplete,
        AppState::LevelUp, AppState::HullSelect, AppState::Customize,
        AppState::Map, AppState::Paused, AppState::GameOver,
        AppState::BossReward, AppState::BossIntro, AppState::Win,
        AppState::Lobby,
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

    /// Unknown discriminants yield `None`, not a panic — forward-
    /// compat for older clients receiving a state from a newer host.
    #[test]
    fn appstate_from_unknown_u8_is_none() {
        for n in [13u8, 50, 100, 255] {
            assert!(AppState::from_u8(n).is_none(), "n={n} should be None");
        }
    }
}
