//! Host → Client XP / level sync.
//!
//! `XpSync` carries the host's `Xp::current` + `Xp::level` so the
//! client's XP bar / level readout matches. Pending level-up picks
//! ride a separate `LevelUpGranted` message — each peer drains them
//! independently in per-peer LevelUp. See module doc on
//! [`broadcast_level_up_grants`] below.
//!
//! Authority: host is canonical for the bar display. For pending
//! picks, the host detects the rising edge of its own grants and
//! emits one or more `LevelUpGranted` messages; receivers (including
//! the host's own self-loop is skipped) add to their local pending.

use bevy::prelude::*;

use crate::xp::{LevelUpsPending, Xp};

use super::net::{send_to, NetMsg};
use super::{NetMode, NetSession};

/// Last broadcasted XP snapshot. Throttles sends so the packet only
/// goes out when the host's XP state actually changes.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct LastBroadcastedXp {
    pub current: u32,
    pub level: u32,
    pub valid: bool,
}

/// Client receive buffer for the latest XP packet. Drained by
/// `apply_received_xp` which writes into the local `Xp` resource.
#[derive(Resource, Default, Clone, Copy)]
pub struct PendingXpSync(pub Option<(u32, u32)>);

/// Tracks the last value of `LevelUpsPending` the host has SEEN so
/// `broadcast_level_up_grants` can detect rising edges and emit one
/// `LevelUpGranted` per crossing. Without an edge tracker, a single
/// kill that pushes through two thresholds in one frame would only
/// emit one signal.
#[derive(Resource, Default, Debug)]
pub struct LastSeenLocalLevelUps {
    pub value: u32,
}

/// Host-only: emit an XpSync whenever the host's XP state changes.
/// Cheap — short-circuits on no-op and on solo / client.
pub fn broadcast_xp(
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    xp: Res<Xp>,
    mut last: ResMut<LastBroadcastedXp>,
) {
    let Some(session) = session else { return };
    if !matches!(*mode, NetMode::Connected) || !session.is_host { return; }

    let snapshot = LastBroadcastedXp {
        current: xp.current,
        level: xp.level,
        valid: true,
    };
    if *last == snapshot { return; }
    *last = snapshot;

    let msg = NetMsg::XpSync {
        current: snapshot.current,
        level: snapshot.level,
    };
    for &addr in session.peers.values() {
        if let Err(e) = send_to(&session.sock, addr, &msg) {
            bevy::log::warn!("multiplayer: XpSync send failed: {e}");
        }
    }
}

/// Host-only: detect rising-edges of `LevelUpsPending` and emit one
/// `LevelUpGranted { count }` per batch so every peer gets the
/// same number of cards to pick. Each peer's local
/// `LevelUpsPending` then drains independently as they click.
///
/// Edge-triggered, not snapshot, so a peer who picks a card and
/// drops their local pending isn't immediately re-set by the next
/// broadcast.
pub fn broadcast_level_up_grants(
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    pending: Res<LevelUpsPending>,
    mut last: ResMut<LastSeenLocalLevelUps>,
) {
    let Some(session) = session else { return };
    if !matches!(*mode, NetMode::Connected) || !session.is_host { return; }

    let now = pending.0;
    let prev = last.value;
    last.value = now;
    if now <= prev { return; }
    let delta = (now - prev).min(u8::MAX as u32) as u8;
    let msg = NetMsg::LevelUpGranted { count: delta };
    for &addr in session.peers.values() {
        if let Err(e) = send_to(&session.sock, addr, &msg) {
            bevy::log::warn!("multiplayer: LevelUpGranted send failed: {e}");
        }
    }
}

/// Client-only: drain `PendingXpSync` into the local `Xp` resource
/// so the client's XP bar mirrors the host's. Does NOT touch
/// `LevelUpsPending` — that's driven by `LevelUpGranted` per-peer.
pub fn apply_received_xp(
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    mut pending_buf: ResMut<PendingXpSync>,
    mut xp: ResMut<Xp>,
) {
    let Some(session) = session else { return };
    if !matches!(*mode, NetMode::Connected) || session.is_host { return; }
    let Some((current, level)) = pending_buf.0.take() else { return };

    xp.current = current;
    xp.level = level;
}

/// Drain incoming `LevelUpGranted` deltas into the local
/// `LevelUpsPending` counter. Runs on both peers — the host needs
/// it for its own local grants only? No: the host increments its
/// own pending via the existing `grant_kill_xp` path, not via the
/// broadcast. So the host SKIPS draining its own outgoing
/// LevelUpGranted (would double-count). Client adds normally.
pub fn apply_received_level_up_grants(
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    mut inbox: ResMut<PendingLevelUpGrants>,
    mut pending: ResMut<LevelUpsPending>,
) {
    let Some(session) = session else { return };
    if !matches!(*mode, NetMode::Connected) { return };
    let drained: u32 = inbox.0.drain(..).map(|c| c as u32).sum();
    if drained == 0 { return; }
    // Host already counted its own grants locally — skip the
    // self-loop. Only clients should bump pending from the network.
    if session.is_host { return; }
    pending.0 = pending.0.saturating_add(drained);
}

/// Receive buffer for `NetMsg::LevelUpGranted`. Populated by
/// `recv_packets`, drained by `apply_received_level_up_grants`.
#[derive(Resource, Default)]
pub struct PendingLevelUpGrants(pub Vec<u8>);

/// Bundled SystemParam for the two XP-related inboxes. `recv_packets`
/// is at Bevy's 16-SystemParam cap; bundling these two into one
/// keeps the cap from being hit.
#[derive(bevy::ecs::system::SystemParam)]
pub struct XpInboxes<'w> {
    pub xp: ResMut<'w, PendingXpSync>,
    pub grants: ResMut<'w, PendingLevelUpGrants>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_broadcasted_xp_equality_skips_resend() {
        let a = LastBroadcastedXp { current: 5, level: 3, valid: true };
        let b = LastBroadcastedXp { current: 5, level: 3, valid: true };
        assert_eq!(a, b, "identical state must compare equal so broadcast skips");
    }

    #[test]
    fn last_broadcasted_xp_changes_when_any_field_differs() {
        let base = LastBroadcastedXp { current: 5, level: 3, valid: true };
        assert_ne!(base, LastBroadcastedXp { current: 6, ..base });
        assert_ne!(base, LastBroadcastedXp { level: 4, ..base });
    }
}
