//! Per-peer loadout broadcast. Every peer pushes their OWN
//! `PlayerStats` + `TurretConfig` to everyone else, change-detected.
//! Receivers store the data keyed by sender id in [`PeerLoadouts`]
//! — they do NOT overwrite their local resources, so each peer can
//! independently mutate their stats / turret config (shop, rune
//! drops, level-up picks).
//!
//! The TurretConfig broadcast is load-bearing for ghost-ship visuals
//! — `spawn_remote_turret_visuals` reads `PeerLoadouts[peer_id]` to
//! render the right turrets on each remote boat. PlayerStats is
//! shipped too for symmetry / future per-peer UI, but isn't read by
//! anything visual today.
//!
//! Cadence: change-driven. A peer only sends when their local
//! `PlayerStats` / `TurretConfig` fires `is_changed()`. Bandwidth is
//! negligible because the snapshots are tiny.

use bevy::prelude::*;

use crate::rune::Rune;
use crate::stats::{PlayerStats, Stat};
use crate::turret::{SlotCfg, TurretConfig};
use crate::weapon::WeaponType;

use super::net::{send_to, NetMsg, SerializedPlayerStats, SerializedSlotCfg};
use super::{NetMode, NetSession};

/// Wire-format sentinel for "no rune in this socket". 255 chosen
/// because the legitimate rune discriminants go up to 26 (and
/// nowhere near 255), so collision is unrealistic.
const NO_RUNE: u8 = 255;

// ---------- PlayerStats sync ----------

/// Per-peer broadcast. On local `PlayerStats` change, ship a
/// `PlayerStatsSync` tagged with our peer id. Receivers store under
/// [`PeerLoadouts`] keyed by sender id — no overwrite of any local
/// resource.
pub fn broadcast_player_stats(
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    stats: Res<PlayerStats>,
) {
    let Some(session) = session else { return };
    if !matches!(*mode, NetMode::Connected) { return };
    if !stats.is_changed() { return; }

    let msg = NetMsg::PlayerStatsSync {
        from_peer: session.my_id,
        stats: serialize_player_stats(&stats),
    };
    for &addr in session.peers.values() {
        if let Err(e) = send_to(&session.sock, addr, &msg) {
            bevy::log::warn!("multiplayer: PlayerStatsSync send failed: {e}");
        }
    }
}

/// Drain the receive buffer into [`PeerLoadouts`]. Does NOT touch
/// the local `PlayerStats` resource — peers manage their stats
/// independently.
pub fn apply_received_player_stats(
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    mut pending: ResMut<PendingPlayerStats>,
    mut loadouts: ResMut<PeerLoadouts>,
) {
    let Some(_session) = session else { return };
    if !matches!(*mode, NetMode::Connected) { return };
    let Some((from_peer, serialized)) = pending.0.take() else { return };
    let entry = loadouts.0.entry(from_peer).or_default();
    entry.stats = Some(deserialize_player_stats(&serialized));
}

/// Receive buffer for `NetMsg::PlayerStatsSync`. Populated by
/// `recv_packets`, drained by `apply_received_player_stats`.
#[derive(Resource, Default)]
pub struct PendingPlayerStats(pub Option<(u8, SerializedPlayerStats)>);

// ---------- TurretConfig sync ----------

/// Per-peer broadcast. On local `TurretConfig` change, ship a
/// `TurretConfigSync` tagged with our peer id. Receivers store
/// under [`PeerLoadouts`] keyed by sender id so the ghost-ship
/// renderer can show the right turrets on each remote boat.
pub fn broadcast_turret_config(
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    cfg: Res<TurretConfig>,
) {
    let Some(session) = session else { return };
    if !matches!(*mode, NetMode::Connected) { return };
    if !cfg.is_changed() { return; }

    let slots = serialize_turret_config(&cfg);
    let msg = NetMsg::TurretConfigSync { from_peer: session.my_id, slots };
    for &addr in session.peers.values() {
        if let Err(e) = send_to(&session.sock, addr, &msg) {
            bevy::log::warn!("multiplayer: TurretConfigSync send failed: {e}");
        }
    }
}

/// Drain the receive buffer into [`PeerLoadouts`]. Does NOT touch
/// the local `TurretConfig` resource.
pub fn apply_received_turret_config(
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    mut pending: ResMut<PendingTurretConfig>,
    mut loadouts: ResMut<PeerLoadouts>,
) {
    let Some(_session) = session else { return };
    if !matches!(*mode, NetMode::Connected) { return };
    let Some((from_peer, slots)) = pending.0.take() else { return };
    let entry = loadouts.0.entry(from_peer).or_default();
    entry.turret = Some(deserialize_turret_config(&slots));
}

/// Receive buffer for `NetMsg::TurretConfigSync`.
#[derive(Resource, Default)]
pub struct PendingTurretConfig(pub Option<(u8, [SerializedSlotCfg; 8])>);

/// Bundled SystemParam for the two loadout-related inboxes.
/// `recv_packets` is at Bevy's 16-SystemParam cap; bundling these
/// two into one frees a slot for new aggregator inboxes.
#[derive(bevy::ecs::system::SystemParam)]
pub struct LoadoutInboxes<'w> {
    pub stats: ResMut<'w, PendingPlayerStats>,
    pub turret: ResMut<'w, PendingTurretConfig>,
}

// ---------- Initial broadcast ----------

/// Force `PlayerStats` + `TurretConfig` to register as "changed" so
/// `broadcast_player_stats` / `broadcast_turret_config` fire at
/// least once after entering Playing / Lobby. Without this, a peer
/// who never opens the shop never broadcasts their starting loadout,
/// and the other peer's ghost has a hull but no turrets.
///
/// `set_changed()` is the right tool: it bumps the resource's
/// change tick without writing any new data, so the existing
/// broadcast machinery picks it up next frame and we don't need a
/// parallel "first time?" flag anywhere.
pub fn force_initial_loadout_broadcast(
    mut stats: ResMut<PlayerStats>,
    mut turret: ResMut<TurretConfig>,
) {
    stats.set_changed();
    turret.set_changed();
}

// ---------- PeerLoadouts ----------

/// Per-peer loadout snapshot. Populated by the receive systems above.
/// Keyed by sender peer id. Fields are `Option<...>` because each peer
/// broadcasts stats + turret independently — one may arrive before
/// the other. The ghost renderer treats a missing turret as "no
/// equipped turrets yet" (cosmetic-only consequence).
#[derive(Default, Clone, Debug)]
pub struct PeerLoadout {
    pub stats: Option<PlayerStats>,
    pub turret: Option<TurretConfig>,
}

#[derive(Resource, Default, Debug)]
pub struct PeerLoadouts(pub std::collections::HashMap<u8, PeerLoadout>);

// ---------- (De)serialization helpers ----------

fn ser_stat(s: &Stat) -> [f32; 3] { [s.base, s.flat, s.percent] }
fn de_stat(a:  [f32; 3]) -> Stat   { Stat { base: a[0], flat: a[1], percent: a[2] } }

fn serialize_player_stats(s: &PlayerStats) -> SerializedPlayerStats {
    SerializedPlayerStats {
        hp:                       ser_stat(&s.hp),
        move_speed:               ser_stat(&s.move_speed),
        turn_speed:               ser_stat(&s.turn_speed),
        turret_turn_speed:        ser_stat(&s.turret_turn_speed),
        turret_arc_bonus_deg:     ser_stat(&s.turret_arc_bonus_deg),
        luck_pct:                 ser_stat(&s.luck_pct),
        proc_strength_pct:        ser_stat(&s.proc_strength_pct),
        crit_pct:                 ser_stat(&s.crit_pct),
        range_pct:                ser_stat(&s.range_pct),
        harvest_pct:              ser_stat(&s.harvest_pct),
        xp_harvest_pct:           ser_stat(&s.xp_harvest_pct),
        shield_max:               ser_stat(&s.shield_max),
        shield_recharge_rate:     ser_stat(&s.shield_recharge_rate),
        shield_recharge_delay:    ser_stat(&s.shield_recharge_delay),
        rune_damage:              ser_stat(&s.rune_damage),
        turret_damage_pct:        ser_stat(&s.turret_damage_pct),
        dodge_pct:                ser_stat(&s.dodge_pct),
        armour_pct:               ser_stat(&s.armour_pct),
    }
}

fn deserialize_player_stats(s: &SerializedPlayerStats) -> PlayerStats {
    PlayerStats {
        hp:                       de_stat(s.hp),
        move_speed:               de_stat(s.move_speed),
        turn_speed:               de_stat(s.turn_speed),
        turret_turn_speed:        de_stat(s.turret_turn_speed),
        turret_arc_bonus_deg:     de_stat(s.turret_arc_bonus_deg),
        luck_pct:                 de_stat(s.luck_pct),
        proc_strength_pct:        de_stat(s.proc_strength_pct),
        crit_pct:                 de_stat(s.crit_pct),
        range_pct:                de_stat(s.range_pct),
        harvest_pct:              de_stat(s.harvest_pct),
        xp_harvest_pct:           de_stat(s.xp_harvest_pct),
        shield_max:               de_stat(s.shield_max),
        shield_recharge_rate:     de_stat(s.shield_recharge_rate),
        shield_recharge_delay:    de_stat(s.shield_recharge_delay),
        rune_damage:              de_stat(s.rune_damage),
        turret_damage_pct:        de_stat(s.turret_damage_pct),
        dodge_pct:                de_stat(s.dodge_pct),
        armour_pct:               de_stat(s.armour_pct),
    }
}

fn ser_runes(runes: &[Option<Rune>; 3]) -> [u8; 3] {
    let mut out = [NO_RUNE; 3];
    for (i, r) in runes.iter().enumerate() {
        out[i] = match r {
            Some(rn) => rn.to_u8(),
            None     => NO_RUNE,
        };
    }
    out
}

fn de_runes(bytes: [u8; 3]) -> [Option<Rune>; 3] {
    let mut out = [None; 3];
    for (i, b) in bytes.iter().enumerate() {
        out[i] = if *b == NO_RUNE { None } else { Rune::from_u8(*b) };
    }
    out
}

fn serialize_turret_config(cfg: &TurretConfig) -> [SerializedSlotCfg; 8] {
    let mut out = [SerializedSlotCfg {
        equipped: false, weapon: 0, damage: 0, fire_rate: 0.0,
        barrels: 0, runes: [NO_RUNE; 3],
    }; 8];
    for (i, slot) in cfg.slots.iter().enumerate() {
        out[i] = SerializedSlotCfg {
            equipped:  slot.equipped,
            weapon:    slot.weapon.to_u8(),
            damage:    slot.damage,
            fire_rate: slot.fire_rate,
            barrels:   slot.barrels,
            runes:     ser_runes(&slot.runes),
        };
    }
    out
}

fn deserialize_turret_config(slots: &[SerializedSlotCfg; 8]) -> TurretConfig {
    let mut out = TurretConfig { slots: [SlotCfg::default(); 8] };
    for (i, ser) in slots.iter().enumerate() {
        out.slots[i] = SlotCfg {
            equipped:  ser.equipped,
            weapon:    WeaponType::from_u8(ser.weapon).unwrap_or(WeaponType::Standard),
            damage:    ser.damage,
            fire_rate: ser.fire_rate,
            barrels:   ser.barrels,
            runes:     de_runes(ser.runes),
        };
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `PlayerStats` round-trips through serialize / deserialize with
    /// every field intact.
    #[test]
    fn player_stats_round_trip() {
        let mut s = PlayerStats::default();
        // Mutate every field to a distinctive value so a missing
        // serializer for one of them fails loudly.
        s.hp.flat = 17.0;
        s.move_speed.percent = 25.0;
        s.crit_pct.flat = 42.0;
        s.shield_max.base = 100.0;
        s.rune_damage.percent = 150.0;
        s.turret_damage_pct.flat = -10.0;

        let ser = serialize_player_stats(&s);
        let back = deserialize_player_stats(&ser);

        assert_eq!(back.hp.flat, 17.0);
        assert_eq!(back.move_speed.percent, 25.0);
        assert_eq!(back.crit_pct.flat, 42.0);
        assert_eq!(back.shield_max.base, 100.0);
        assert_eq!(back.rune_damage.percent, 150.0);
        assert_eq!(back.turret_damage_pct.flat, -10.0);
    }

    /// `TurretConfig` round-trips with weapons + runes intact.
    #[test]
    fn turret_config_round_trip() {
        let mut cfg = TurretConfig::default();
        cfg.slots[3] = SlotCfg {
            equipped:  true,
            weapon:    WeaponType::Sniper,
            damage:    25,
            fire_rate: 0.5,
            barrels:   2,
            runes:     [Some(Rune::Fire), Some(Rune::Shock), None],
        };

        let ser = serialize_turret_config(&cfg);
        let back = deserialize_turret_config(&ser);

        assert_eq!(back.slots[3].equipped, true);
        assert_eq!(back.slots[3].weapon, WeaponType::Sniper);
        assert_eq!(back.slots[3].damage, 25);
        assert_eq!(back.slots[3].fire_rate, 0.5);
        assert_eq!(back.slots[3].barrels, 2);
        assert_eq!(back.slots[3].runes[0], Some(Rune::Fire));
        assert_eq!(back.slots[3].runes[1], Some(Rune::Shock));
        assert_eq!(back.slots[3].runes[2], None);
        // Slot 0 default (Standard equipped) preserved.
        assert_eq!(back.slots[0].equipped, true);
        assert_eq!(back.slots[0].weapon, WeaponType::Standard);
    }

    /// 255 sentinel for empty rune slot survives the round-trip.
    #[test]
    fn empty_rune_slots_round_trip() {
        let runes = [None, None, None];
        let bytes = ser_runes(&runes);
        assert_eq!(bytes, [NO_RUNE; 3]);
        let back = de_runes(bytes);
        assert_eq!(back, [None, None, None]);
    }
}
