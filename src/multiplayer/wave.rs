//! Replicate host's wave state to the client so the
//! client's wave indicator shows accurate counts. Without this the
//! client UI is stuck at "wave 0/0" because the wave system runs
//! only on the host.
//!
//! Only the four wave-indicator-relevant fields (`wave_idx`,
//! `wave_count`, `wave_phase`, `wave_remaining`) ride the wire. The
//! rest of `CombatContext` (pending_spawns, boss_chaos_cd, etc.)
//! stays host-only — the client doesn't simulate waves, just
//! displays the indicator.

use bevy::prelude::*;

use crate::map::{CombatContext, WavePhase};

use super::net::{send_to, NetMsg};
use super::{NetMode, NetSession};

/// Last broadcasted snapshot of the host's wave state. Used to
/// throttle sends so we only emit on change.
#[derive(Resource, Default, Clone, Copy, PartialEq, Eq)]
pub struct LastBroadcastedWaveState {
    pub wave_idx:    u32,
    pub wave_count:  u32,
    pub phase:       u8,
    pub remaining:   u32,
    pub valid:       bool, // false until first broadcast
}

/// Client receive buffer for the latest wave snapshot. Drained by
/// `apply_wave_state` which writes into the local CombatContext's
/// indicator-relevant fields.
#[derive(Resource, Default, Clone, Copy)]
pub struct PendingWaveState(pub Option<(u32, u32, u8, u32)>);

/// Host-only: broadcast wave state when it changes.
pub fn broadcast_wave_state(
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    combat: Option<Res<CombatContext>>,
    mut last: ResMut<LastBroadcastedWaveState>,
) {
    let Some(session) = session else { return };
    if !matches!(*mode, NetMode::Connected) || !session.is_host { return; }
    let Some(combat) = combat else { return };

    let snapshot = LastBroadcastedWaveState {
        wave_idx:   combat.wave_idx as u32,
        wave_count: combat.wave_count as u32,
        phase:      combat.wave_phase.to_u8(),
        remaining:  combat.wave_remaining,
        valid:      true,
    };
    if *last == snapshot { return; }
    *last = snapshot;

    let msg = NetMsg::WaveStateSync {
        wave_idx:   snapshot.wave_idx,
        wave_count: snapshot.wave_count,
        phase:      snapshot.phase,
        remaining:  snapshot.remaining,
    };
    for &addr in session.peers.values() {
        if let Err(e) = send_to(&session.sock, addr, &msg) {
            bevy::log::warn!("multiplayer: WaveStateSync send failed: {e}");
        }
    }
}

/// Client-only: drain `PendingWaveState` and write the wave-indicator
/// fields into the local `CombatContext` so the wave UI reads the
/// host's authoritative values. Other CombatContext fields stay at
/// their local defaults (the client doesn't simulate waves).
///
/// Scrap parity is handled separately by `broadcast_scrap_delta` +
/// `apply_received_scrap` — when the host's wave-clear path grants
/// +1, the broadcaster sees the rising edge and ships a
/// `ScrapAwarded` packet. We don't grant scrap here or we'd double-
/// count.
pub fn apply_wave_state(
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    mut pending: ResMut<PendingWaveState>,
    combat: Option<ResMut<CombatContext>>,
) {
    let Some(session) = session else { return };
    if !matches!(*mode, NetMode::Connected) || session.is_host { return; }
    let Some(mut combat) = combat else { return };
    let Some((idx, count, phase_u8, remaining)) = pending.0.take() else { return };

    combat.wave_idx       = idx       as u8;
    combat.wave_count     = count     as u8;
    combat.wave_remaining = remaining;
    if let Some(phase) = WavePhase::from_u8(phase_u8) {
        combat.wave_phase = phase;
    }
}

// ---------- Per-kill scrap parity ----------

/// Tracks the host's last-broadcasted `Scrap.0` so `broadcast_scrap_delta`
/// can compute and ship the delta whenever the host's pool grows
/// (per-kill drops, Greed procs, boss bounty, customize spend).
#[derive(Resource, Default, Clone, Copy, Debug)]
pub struct LastBroadcastedScrap {
    pub value: u32,
    pub valid: bool,
}

/// Host-only: detect rising-edges in `Scrap.0` and broadcast the
/// delta to peers as `NetMsg::ScrapAwarded { scrap }`. Without this,
/// only the wave-clear path grants scrap on client; per-kill drops
/// (Greed rune) + boss bounties run host-only and divergent scrap
/// totals accumulate over a run.
///
/// Decrements (player spent in shop) DON'T broadcast — each peer
/// spends their own scrap independently. We only sync gains.
pub fn broadcast_scrap_delta(
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    scrap: Res<crate::Scrap>,
    mut last: ResMut<LastBroadcastedScrap>,
) {
    let Some(session) = session else { return };
    if !matches!(*mode, NetMode::Connected) || !session.is_host { return; }
    if !last.valid {
        // Seed on first run so a fresh session doesn't broadcast
        // the entire starting balance as a "you just earned this."
        last.value = scrap.0;
        last.valid = true;
        return;
    }
    if scrap.0 > last.value {
        let delta = scrap.0 - last.value;
        let msg = NetMsg::ScrapAwarded { scrap: delta };
        for &addr in session.peers.values() {
            let _ = super::net::send_to(&session.sock, addr, &msg);
        }
    }
    last.value = scrap.0;
}

/// Receive buffer for `NetMsg::ScrapAwarded`. Drained by
/// `apply_received_scrap` which adds the amount to the local
/// `Scrap.0` so the client's total tracks the host's gains.
#[derive(Resource, Default)]
pub struct PendingScrapAwards(pub Vec<u32>);

/// Client-only: add each received scrap award to the local pool.
/// Skipped on host (host's own grants already updated local Scrap
/// before the broadcast was emitted).
pub fn apply_received_scrap(
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    mut inbox: ResMut<PendingScrapAwards>,
    mut scrap_w: crate::stage_complete::ScrapWriter,
) {
    let Some(session) = session else { return };
    if !matches!(*mode, NetMode::Connected) || session.is_host {
        inbox.0.clear();
        return;
    }
    for amount in inbox.0.drain(..) {
        scrap_w.grant(amount);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wave_phase_round_trip() {
        for &p in &[WavePhase::Spawning, WavePhase::Fighting, WavePhase::Cooldown] {
            assert_eq!(WavePhase::from_u8(p.to_u8()), Some(p));
        }
    }

    #[test]
    fn wave_phase_unknown_discriminant_is_none() {
        for n in [3u8, 50, 200, 255] {
            assert!(WavePhase::from_u8(n).is_none());
        }
    }
}
