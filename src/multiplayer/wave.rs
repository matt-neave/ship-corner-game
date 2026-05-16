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
/// Side-effect: when the host's phase transitions Fighting →
/// Cooldown, grant the client +1 scrap locally. Scrap is fully
/// deterministic per-peer in this codebase (no host-authoritative
/// scrap broadcast); the wave-clear bonus normally fires from the
/// host's `try_advance_fighting` site and we mirror it here so
/// clients earn the same per-wave bounty.
pub fn apply_wave_state(
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    mut pending: ResMut<PendingWaveState>,
    combat: Option<ResMut<CombatContext>>,
    mut scrap_w: crate::stage_complete::ScrapWriter,
) {
    let Some(session) = session else { return };
    if !matches!(*mode, NetMode::Connected) || session.is_host { return; }
    let Some(mut combat) = combat else { return };
    let Some((idx, count, phase_u8, remaining)) = pending.0.take() else { return };

    let prev_phase = combat.wave_phase;
    combat.wave_idx       = idx       as u8;
    combat.wave_count     = count     as u8;
    combat.wave_remaining = remaining;
    if let Some(phase) = WavePhase::from_u8(phase_u8) {
        combat.wave_phase = phase;
        // Wave just cleared on the host — grant the client's local
        // scrap so it matches. Skips the cosmetic flicker case
        // where the same packet arrives twice by gating on the
        // ACTUAL phase change.
        if prev_phase == WavePhase::Fighting && phase == WavePhase::Cooldown {
            scrap_w.grant(1);
        }
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
