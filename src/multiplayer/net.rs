//! UDP socket + wire-format helpers. Tiny enough that we hand-roll
//! the protocol instead of reaching for a replication crate. Two
//! peers exchange position updates at ~30Hz; that's the whole job.
//!
//! Format: `bincode`-serialized [`NetMsg`] enum, one packet per
//! variant. The packets are small (under ~32 bytes each), well below
//! any sane MTU, so we don't bother with fragmentation or windowing.
//! Lost packets are fine because every Transform update fully
//! supersedes the previous one — there's no incremental state to
//! reconstruct.

use std::net::{SocketAddr, UdpSocket};

use serde::{Deserialize, Serialize};

/// Fixed UDP port the host listens on. Picked from the IANA dynamic
/// range; well outside any common service port. The joining client
/// must reach this port on the host's LAN address.
pub const HOST_PORT: u16 = 49333;

/// Wire-format enum. Every packet is one of these variants serialized
/// with `bincode`. Keep it small — `bincode` v1 with default config
/// emits little overhead, but we still want the payload tiny enough
/// to fit comfortably in a UDP datagram even after IP+UDP headers.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum NetMsg {
    /// Client → Host first packet after the UDP socket binds. The
    /// host records the sender's `SocketAddr`, assigns them a peer
    /// id, and replies with [`NetMsg::Welcome`]. Carries the
    /// client's display name so it can populate the host's roster
    /// (and propagate to other peers via `PeerJoined`).
    Hello { name: String },
    /// Host → Client reply to a [`NetMsg::Hello`]. Carries:
    /// - `your_id`: the id the host assigned to this client
    /// - `host_name`: host's display name (so the client's roster
    ///   includes the host)
    /// - `existing_peers`: every OTHER peer already in the lobby
    ///   when this client joins, so the joiner sees them in their
    ///   roster on arrival (host's authoritative view)
    Welcome {
        your_id: u8,
        host_name: String,
        existing_peers: Vec<(u8, String)>,
    },
    /// Host → existing-peer broadcast: a new client just joined.
    /// Existing clients update their roster so they can see the new
    /// arrival in the lobby. New client itself learns who's already
    /// here via `Welcome::existing_peers`.
    PeerJoined { id: u8, name: String },
    /// Host → all-other-peers broadcast: this peer left (clean
    /// disconnect via Bye, or kicked, or timeout). Receivers drop
    /// the id from their local roster.
    PeerLeft { id: u8 },
    /// Host → specific client: "you've been kicked". Client tears
    /// down its session and returns to MainMenu. The `reason` is
    /// shown in the JOIN screen's error field so the player knows
    /// why they were dropped.
    Kicked { reason: String },
    /// Either peer → other peer(s). Latest authoritative position +
    /// heading for the named id. Sent at a fixed cadence (see
    /// `multiplayer::ghost::TRANSFORM_SEND_HZ`). Receivers store the
    /// freshest one per id and snap their ghost entity's `Transform`
    /// to match.
    Transform {
        id: u8,
        pos: [f32; 2],
        /// Z-rotation in radians (matches `Heading` in this codebase).
        rot: f32,
        /// World-space Z-rotation of each turret base (slot 0..7).
        /// Slot not equipped or not visible → arbitrary value
        /// (receivers ignore the slot via `PeerLoadouts`). Lets the
        /// ghost ship's turrets visually track the peer's live aim
        /// instead of sitting at their mount-angle defaults.
        turret_rots: [f32; 8],
    },
    /// Either peer → other peer(s). Voluntary disconnect notice so
    /// the receivers can despawn the ghost immediately instead of
    /// waiting for a timeout heuristic. Not load-bearing — drops with
    /// no Bye still get cleaned up when the peer's Transform stops
    /// arriving for long enough, but the explicit message keeps the
    /// UI responsive on clean exits.
    Bye { id: u8 },
    /// Host → Client snapshot of every authoritative enemy. Sent at
    /// `ENEMY_SNAPSHOT_HZ` (~20Hz). Receiver diffs against its mirror
    /// entities to spawn missing, update existing, and despawn any
    /// id not present in this packet.
    ///
    /// `entries` is a flat `Vec` for simplicity; for ~30 enemies a
    /// packet is well under MTU even after bincode framing.
    EnemySnapshot { entries: Vec<EnemyEntry> },
    /// Client → Host: "my bullet hit your enemy #N for `amount` HP
    /// with this weapon + these runes". Host re-applies through its
    /// authoritative damage pipeline, **including rune procs**, so
    /// host-side `OnFire` / `OnFrost` / `OnBleed` (etc.) get added
    /// to the target. The status bits then ride the next
    /// `EnemySnapshot` back to all clients, which add the matching
    /// proc components on their mirrors so the local DOT tick
    /// systems light up.
    ///
    /// `weapon` and `runes` use the wire-format discriminants from
    /// `WeaponType::to_u8` and `Rune::to_u8` respectively.
    DamageEnemy {
        enemy_id: u32,
        amount: i32,
        /// World position of the hit, used by host-side damage
        /// effects (knockback origin, hit particle spawn).
        hit_pos: [f32; 2],
        weapon: u8,
        runes: Vec<u8>,
    },
    /// Either → Host (then host re-broadcasts to every other peer):
    /// "this transient visual effect happened, render it locally on
    /// receipt". For one-frame visuals like the Shock chain arc
    /// where there's no persistent state to sync via snapshot — the
    /// moment IS the message.
    ///
    /// `from` / `to` are world positions for two-point effects (a
    /// chain arc between two enemies); `to` matches `from` for
    /// single-point effects (an AOE ring at one location).
    ProcFx {
        kind: u8,
        from: [f32; 2],
        to: [f32; 2],
    },
    /// Either direction: "I just transitioned to this `AppState`."
    /// Host pushes every transition; client pushes only Paused /
    /// Playing (so either peer can pause the team). Receivers map
    /// the host state through `client_state_for` to decide their
    /// local target.
    ///
    /// `state` is an `AppState::to_u8` discriminant.
    StateChange { state: u8 },
    /// Host → Client: full snapshot of the player's stats. Sent on
    /// change (host's `PlayerStats.is_changed()` fires). Client
    /// overwrites its local `PlayerStats` with the host's
    /// authoritative values so both peers have parity in HP, range,
    /// crit, shield, etc.
    /// Per-peer broadcast: "my final PlayerStats look like this."
    /// `from_peer` lets receivers store the stats keyed by sender id
    /// without mistaking it for their own. Currently unused for
    /// rendering (stats are gameplay-only, no visual effect), but
    /// kept around so future per-peer UIs (party stat panel, etc.)
    /// have a hook.
    PlayerStatsSync { from_peer: u8, stats: SerializedPlayerStats },
    /// Per-peer broadcast: "my final TurretConfig looks like this."
    /// `from_peer` lets receivers store it keyed by sender id so the
    /// ghost-ship renderer can show the right turrets on each remote
    /// boat. Sent on local TurretConfig change (debounced by Bevy's
    /// `Changed`), so a peer dragging in a new turret during their
    /// own shop pass propagates to other peers' ghost visuals.
    TurretConfigSync { from_peer: u8, slots: [SerializedSlotCfg; 8] },
    /// Host → Client: current wave state. Lets the client's wave
    /// indicator UI show what the host's `CombatContext` actually
    /// is, instead of the client's local zero state.
    WaveStateSync {
        wave_idx:   u32,
        wave_count: u32,
        /// `CombatContext::WavePhase::to_u8` discriminant. See
        /// `multiplayer::wave::WavePhaseWire`.
        phase:      u8,
        remaining:  u32,
    },
    /// Host → Client: authoritative XP + level snapshot for the XP
    /// bar / level readout. Pending level-up picks ride
    /// `LevelUpGranted` separately so each peer drains them
    /// independently.
    XpSync {
        current: u32,
        level: u32,
    },
    /// Either → Host (then host re-broadcasts to other peers): "I
    /// just fired a bullet from this position in this direction".
    /// Receivers spawn a damage=0 visual replica so the firing
    /// peer's bullets appear on every screen. Signal-driven (not
    /// AI-driven on the remote side) so timing matches the actual
    /// firing peer instead of drifting.
    ///
    /// `target_net_id` is set when the bullet is a HomingMissile —
    /// it carries the target enemy's `NetEntityId` so the receiver
    /// can look up the corresponding mirror and attach a local
    /// `HomingMissile` component to the visual bullet. The visual
    /// missile then homes locally on the peer's side, matching the
    /// owner's curving flight path. `0` means "no target" (straight
    /// bullet; not a missile).
    BulletFired {
        pos:    [f32; 2],
        dir:    [f32; 2],
        weapon: u8,
        range:  f32,
        target_net_id: u32,
    },
    /// Either → Host: "my local player just died." Host tracks per-
    /// peer alive state in `TeamDeathTracker` and only triggers a
    /// shared GameOver when EVERY peer is dead. Until then dead
    /// peers spectate.
    PeerDied { id: u8 },
    /// Host → all peers: "you (or you all) revive — fresh ship,
    /// full HP." Broadcast on stage transition so dead spectators
    /// rejoin the action on the next level. Single-target id is
    /// `u8::MAX` for "everyone revives" (the common case).
    PeerRevived { id: u8 },
    /// Either → others: "I've clicked READY in my current per-peer
    /// state." `sender_state` is the sender's `AppState::to_u8` at
    /// send time. Receivers DROP packets where `sender_state` !=
    /// their local state — otherwise stale broadcasts from the
    /// previous state (sent in the frame right before the
    /// all-ready advance fired) repopulate the new state's tracker
    /// the moment recv_packets drains the UDP buffer, and the host
    /// auto-advances on its own click without waiting for the peer.
    PeerReady { id: u8, sender_state: u8 },
    /// Host → all peers: "I just crossed `count` XP threshold(s) —
    /// every peer should add that many picks to their local
    /// `LevelUpsPending`."
    ///
    /// Why separate from `XpSync`: each peer drains their pending
    /// independently as they click level-up cards. If `XpSync` kept
    /// broadcasting `pending`, the host's value would clobber each
    /// peer's local "I've already picked one of these" decrement.
    /// Per-peer LevelUp needs an additive, edge-triggered signal.
    LevelUpGranted { count: u8 },
    /// Either → others: low-rate keepalive. The other side updates
    /// `last_seen` on receipt so `detect_stale_peers` doesn't time
    /// the link out during otherwise-quiet states (Paused, Lobby,
    /// menus). Body-less by design — a "you're still here" signal.
    Heartbeat,
    /// Host → specific peer: "you just took `amount` damage —
    /// apply it to your local player." Triggered by an enemy bullet
    /// hitting the host-side ghost of that peer. The peer's local
    /// `bullet_collisions` never sees enemy bullets (enemy AI is
    /// host-only), so this is the only path damage from enemies
    /// reaches the client's player.
    DamagePlayer {
        amount: i32,
        /// World position of the hit — used for the local hit-flash
        /// particle spawn so the peer sees where they were hit.
        hit_pos: [f32; 2],
    },
    /// Per-peer broadcast: positions + rotations of every autonomous
    /// unit (helicopter / shark / octopus) the sender currently has
    /// deployed. Receivers render visual-only mirrors so the OTHER
    /// peer sees those units around the broadcaster's ghost ship.
    ///
    /// `from_peer` lets receivers filter their own loopback (they
    /// don't want to mirror their own units).
    ///
    /// Snapshot-shaped: the receiver despawns its existing mirrors
    /// for this peer and respawns from the entries. Cheap because
    /// each peer fields at most a small handful of units (Helipad +
    /// SharkNet + Cage slots, usually 0–4 units total).
    FriendlyUnitsSnapshot {
        from_peer: u8,
        units: Vec<FriendlyUnitEntry>,
    },
    /// Either → others: "I just fired a mortar shell from `pos` to
    /// `target`." Receivers spawn a visual-only `MortarShell` that
    /// follows the same arc + explodes at `target`. No damage
    /// applied on the receiver side — the owning peer's local
    /// `mortar_shell_tick` does the damage authoritatively.
    MortarFired {
        pos: [f32; 2],
        target: [f32; 2],
        weapon: u8,
        splash_radius: f32,
    },
    /// Either → others: "I just fired a railgun beam from `origin`
    /// in `dir` for `length` units." Receivers spawn a visual-only
    /// `Beam` that grows then fades over `BEAM_LIFETIME`. No
    /// `BeamHit` / `BeamPending` so receivers don't double-damage.
    BeamFired {
        origin: [f32; 2],
        dir: [f32; 2],
        length: f32,
        weapon: u8,
    },
    /// Either → others: "I emitted a frame of flamethrower puffs
    /// from `pos` in `dir`." One per active flamethrower per frame.
    /// Receivers spawn the same particle pair (dark outer + hot
    /// inner) at the same position so the cone reads identically.
    FlameTick {
        pos: [f32; 2],
        dir: [f32; 2],
    },
    /// Host → all: "an enemy just died worth `scrap` scrap." Every
    /// peer (including host's own self-loop is skipped) grants the
    /// scrap locally. Without this, only the host's `enemy_death_check`
    /// awards per-kill scrap (Greed rune drops, boss bounty); the
    /// client only ever gets wave-clear scrap (+1 each Fighting→Cooldown),
    /// so client + host scrap totals drift over a run.
    ScrapAwarded { scrap: u32 },
    /// Either → others: "an octopus tentacle just emerged at `pos`."
    /// Receivers spawn the same emerge → slap → retreat tentacle
    /// chain visual at that position; the receiver's mirror tentacle
    /// carries `target: None` so the damage branch in `tentacle_tick`
    /// stays no-op (damage runs only on the owner via the real
    /// tentacle).
    TentacleSlap { pos: [f32; 2] },
    /// Either → others: "I (peer `source_peer`) just landed a
    /// HarpoonTip on the enemy with `target_net_id`." Receivers
    /// spawn a `RemoteHarpoonChain` between their ghost of
    /// `source_peer` and their mirror of the target so the chain
    /// pull is visible cross-peer. `is_boss` selects the lifetime
    /// (1s for bosses, 4s otherwise) to match the owner's tether.
    HarpoonAttached {
        source_peer:   u8,
        target_net_id: u32,
        is_boss:       bool,
    },
}

/// One entry in a `FriendlyUnitsSnapshot`. `kind` is a small u8
/// discriminant; see [`FriendlyUnitKind`] for the mapping.
///
/// `seq` is a peer-stable index (sender assigns 0..N in a stable
/// order each frame, partitioned by kind). Receivers key persistent
/// mirror entities by `(from_peer, seq)` so the SAME mirror entity
/// survives across snapshots — required for the per-frame lerp in
/// `smooth_peer_unit_mirrors`. Without `seq`, the apply system
/// despawn-and-respawned every 15Hz tick, which looked like a
/// pop-and-restart of motion (especially visible on the flail head).
#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct FriendlyUnitEntry {
    pub seq: u32,
    pub kind: u8,
    pub pos: [f32; 2],
    pub rot: f32,
}

/// Wire-format discriminants for autonomous unit kinds. Append-only
/// like the other to_u8/from_u8 enums in this module — older clients
/// skip unknown kinds rather than crashing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FriendlyUnitKind {
    Helicopter,
    Shark,
    Octopus,
    /// AnchorFlail head — the orbiting anchor mass at the end of a
    /// flail chain. Visual-only on the receiver side; the owning
    /// peer's tick does the actual hit detection.
    FlailHead,
}

impl FriendlyUnitKind {
    pub fn to_u8(self) -> u8 {
        match self {
            FriendlyUnitKind::Helicopter => 0,
            FriendlyUnitKind::Shark      => 1,
            FriendlyUnitKind::Octopus    => 2,
            FriendlyUnitKind::FlailHead  => 3,
        }
    }
    pub fn from_u8(n: u8) -> Option<Self> {
        Some(match n {
            0 => FriendlyUnitKind::Helicopter,
            1 => FriendlyUnitKind::Shark,
            2 => FriendlyUnitKind::Octopus,
            3 => FriendlyUnitKind::FlailHead,
            _ => return None,
        })
    }
}

/// Plain-data mirror of `crate::stats::PlayerStats`. Each `Stat` has
/// three f32 fields (`base`, `flat`, `percent`). Flat array layout
/// (16 fields × 3 = 48 f32s) so a renamed-but-untyped field on
/// either side immediately fails to compile rather than silently
/// drifting on the wire.
#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct SerializedPlayerStats {
    pub hp:                        [f32; 3],
    pub move_speed:                [f32; 3],
    pub turn_speed:                [f32; 3],
    pub turret_turn_speed:         [f32; 3],
    pub turret_arc_bonus_deg:      [f32; 3],
    pub luck_pct:                  [f32; 3],
    pub proc_strength_pct:         [f32; 3],
    pub crit_pct:                  [f32; 3],
    pub range_pct:                 [f32; 3],
    pub harvest_pct:               [f32; 3],
    pub xp_harvest_pct:            [f32; 3],
    pub shield_max:                [f32; 3],
    pub shield_recharge_rate:      [f32; 3],
    pub shield_recharge_delay:     [f32; 3],
    pub rune_damage:               [f32; 3],
    pub turret_damage_pct:         [f32; 3],
    pub cooldown_pct:              [f32; 3],
    pub dodge_pct:                 [f32; 3],
    pub armour_pct:                [f32; 3],
}

/// Plain-data mirror of `crate::turret::SlotCfg`. `Option<Rune>` is
/// represented by `255` for None, otherwise `Rune::to_u8`. Stable
/// wire format.
#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct SerializedSlotCfg {
    pub equipped:  bool,
    pub weapon:    u8,        // WeaponType::to_u8
    pub damage:    i32,
    pub fire_rate: f32,
    pub barrels:   u8,
    pub runes:     [u8; 3],   // 255 = None, else Rune::to_u8
}

/// One enemy's authoritative state, packaged for an `EnemySnapshot`
/// packet. `kind` is the `EnemyVariant` discriminant as a `u8`
/// (matching `EnemyVariant::from_u8` on the receiving side).
#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct EnemyEntry {
    /// Stable id assigned on host. Stays the same across snapshots so
    /// the client can match an entry to its existing mirror entity.
    pub id: u32,
    /// `EnemyVariant` discriminant. See [`crate::enemy::EnemyVariant::to_u8`]
    /// / `from_u8` for the mapping.
    pub kind: u8,
    pub pos: [f32; 2],
    pub rot: f32,
    pub hp: i32,
    /// Bitmask of stateful proc components currently attached to this
    /// enemy on the host. See `multiplayer::enemies::status_bits` for
    /// the bit definitions. Clients add/remove the matching
    /// components on their mirrors so the local `tick_on_fire` /
    /// `tick_on_frost` / `tick_on_bleed` systems light up the right
    /// DOT visuals + tick damage.
    pub status_flags: u8,
    /// `ShipClass::to_u8` if this entry is a boss; `0xFF` for regular
    /// enemies. Read on the client only at first-sight to decide
    /// whether to spawn a boss-styled mirror (full ship visuals via
    /// `build_ship_for_faction`) instead of the standard variant mesh.
    /// Subsequent updates ignore this field — boss HP / transform are
    /// reconciled through the same path as regular mirrors.
    pub boss_class: u8,
}

/// Sentinel for `EnemyEntry.boss_class` meaning "not a boss." Read by
/// the mirror-spawn path on the client.
pub const NOT_A_BOSS: u8 = 0xFF;

/// Bind a non-blocking UDP socket. Host passes `Some(HOST_PORT)`
/// (listens on a known port for incoming Hellos); client passes
/// `None` (let the OS pick a free ephemeral port, since the host
/// learns the client's address from the Hello's source).
pub fn bind_socket(port: Option<u16>) -> std::io::Result<UdpSocket> {
    let port = port.unwrap_or(0);
    let addr: SocketAddr = format!("0.0.0.0:{}", port).parse().unwrap();
    let sock = UdpSocket::bind(addr)?;
    sock.set_nonblocking(true)?;
    Ok(sock)
}

/// Serialize and send `msg` to `addr`. Errors are logged at the call
/// site (we don't bubble them up — a single dropped packet doesn't
/// warrant a panic, and the next tick will resend the latest state
/// anyway).
pub fn send_to(sock: &UdpSocket, addr: SocketAddr, msg: &NetMsg) -> std::io::Result<()> {
    let bytes = bincode::serialize(msg).expect("NetMsg always serializes");
    sock.send_to(&bytes, addr)?;
    Ok(())
}

/// Drain every pending packet on the socket. Returns each successful
/// decode paired with the source address so the caller can identify
/// the peer. Non-blocking — stops at the first `WouldBlock`.
///
/// Malformed packets are silently dropped (logged at warn) rather
/// than aborting the drain; on a noisy LAN we'd rather skip junk than
/// hang the netloop.
pub fn drain_packets(sock: &UdpSocket) -> Vec<(SocketAddr, NetMsg)> {
    let mut out = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        match sock.recv_from(&mut buf) {
            Ok((n, addr)) => match bincode::deserialize::<NetMsg>(&buf[..n]) {
                Ok(msg) => out.push((addr, msg)),
                Err(e) => {
                    bevy::log::warn!("multiplayer: dropped malformed packet from {addr}: {e}");
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(e) => {
                bevy::log::warn!("multiplayer: socket recv error: {e}");
                break;
            }
        }
    }
    out
}

/// Best-effort LAN IP lookup for the HOST status banner. Returns
/// "unknown" on failure so the UI still has something printable
/// rather than crashing.
pub fn local_lan_ip() -> String {
    match local_ip_address::local_ip() {
        Ok(ip) => ip.to_string(),
        Err(_) => "unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip every `NetMsg` variant through bincode to catch any
    /// future field change that breaks the wire format. If a peer is
    /// running an old build, every shipped variant needs to keep
    /// decoding the same shape — these tests are the canary.
    #[test]
    fn netmsg_hello_round_trip() {
        let msg = NetMsg::Hello { name: "ALICE".to_string() };
        let bytes = bincode::serialize(&msg).unwrap();
        let back: NetMsg = bincode::deserialize(&bytes).unwrap();
        match back {
            NetMsg::Hello { name } => assert_eq!(name, "ALICE"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn netmsg_welcome_round_trip() {
        let msg = NetMsg::Welcome {
            your_id: 42,
            host_name: "BOB".to_string(),
            existing_peers: vec![(1, "CAROL".to_string()), (2, "DAVE".to_string())],
        };
        let bytes = bincode::serialize(&msg).unwrap();
        let back: NetMsg = bincode::deserialize(&bytes).unwrap();
        match back {
            NetMsg::Welcome { your_id, host_name, existing_peers } => {
                assert_eq!(your_id, 42);
                assert_eq!(host_name, "BOB");
                assert_eq!(existing_peers, vec![
                    (1, "CAROL".to_string()),
                    (2, "DAVE".to_string()),
                ]);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn netmsg_peer_joined_round_trip() {
        let msg = NetMsg::PeerJoined { id: 3, name: "EVE".to_string() };
        let bytes = bincode::serialize(&msg).unwrap();
        let back: NetMsg = bincode::deserialize(&bytes).unwrap();
        match back {
            NetMsg::PeerJoined { id, name } => {
                assert_eq!(id, 3);
                assert_eq!(name, "EVE");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn netmsg_peer_left_round_trip() {
        let msg = NetMsg::PeerLeft { id: 7 };
        let bytes = bincode::serialize(&msg).unwrap();
        let back: NetMsg = bincode::deserialize(&bytes).unwrap();
        match back {
            NetMsg::PeerLeft { id } => assert_eq!(id, 7),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn netmsg_kicked_round_trip() {
        let msg = NetMsg::Kicked { reason: "host left".to_string() };
        let bytes = bincode::serialize(&msg).unwrap();
        let back: NetMsg = bincode::deserialize(&bytes).unwrap();
        match back {
            NetMsg::Kicked { reason } => assert_eq!(reason, "host left"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn netmsg_transform_round_trip() {
        let turret_rots = [0.1, 0.2, 0.3, 0.4, -0.1, -0.2, -0.3, -0.4];
        let msg = NetMsg::Transform {
            id: 7,
            pos: [123.5, -456.25],
            rot: 1.5,
            turret_rots,
        };
        let bytes = bincode::serialize(&msg).unwrap();
        let back: NetMsg = bincode::deserialize(&bytes).unwrap();
        match back {
            NetMsg::Transform { id, pos, rot, turret_rots: tr } => {
                assert_eq!(id, 7);
                assert_eq!(pos, [123.5, -456.25]);
                assert_eq!(rot, 1.5);
                assert_eq!(tr, turret_rots);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn netmsg_bye_round_trip() {
        let msg = NetMsg::Bye { id: 3 };
        let bytes = bincode::serialize(&msg).unwrap();
        let back: NetMsg = bincode::deserialize(&bytes).unwrap();
        match back {
            NetMsg::Bye { id } => assert_eq!(id, 3),
            _ => panic!("wrong variant"),
        }
    }

    /// Empty snapshot — host sends one even when there are no
    /// enemies, so the client can despawn its mirrors (every id
    /// disappears from the snapshot).
    #[test]
    fn netmsg_enemy_snapshot_empty_round_trip() {
        let msg = NetMsg::EnemySnapshot { entries: vec![] };
        let bytes = bincode::serialize(&msg).unwrap();
        let back: NetMsg = bincode::deserialize(&bytes).unwrap();
        match back {
            NetMsg::EnemySnapshot { entries } => assert!(entries.is_empty()),
            _ => panic!("wrong variant"),
        }
    }

    /// `DamageEnemy` round-trip — carries weapon + runes alongside
    /// the base damage amount so the host re-rolls procs authoritatively.
    #[test]
    fn netmsg_damage_enemy_round_trip() {
        let msg = NetMsg::DamageEnemy {
            enemy_id: 42,
            amount: 17,
            hit_pos: [100.5, -50.25],
            weapon: 1, // Sniper
            runes: vec![0, 14], // Fire + Bleed
        };
        let bytes = bincode::serialize(&msg).unwrap();
        let back: NetMsg = bincode::deserialize(&bytes).unwrap();
        match back {
            NetMsg::DamageEnemy { enemy_id, amount, hit_pos, weapon, runes } => {
                assert_eq!(enemy_id, 42);
                assert_eq!(amount, 17);
                assert_eq!(hit_pos, [100.5, -50.25]);
                assert_eq!(weapon, 1);
                assert_eq!(runes, vec![0, 14]);
            }
            _ => panic!("wrong variant"),
        }
    }

    /// `DamageEnemy` packet stays small enough to send at the bullet-
    /// fire rate. Floor case: empty rune list, single byte of weapon
    /// + length-prefixed empty Vec.
    #[test]
    fn damage_enemy_packet_is_small() {
        let bytes = bincode::serialize(&NetMsg::DamageEnemy {
            enemy_id: 0, amount: 0, hit_pos: [0.0; 2],
            weapon: 0, runes: vec![],
        }).unwrap();
        // tag (4) + u32 (4) + i32 (4) + 2 * f32 (8) + u8 weapon (1)
        // + u64 length-prefix for empty Vec (8) = 29 bytes.
        assert_eq!(bytes.len(), 29);
    }

    /// `ProcFx` round-trip.
    #[test]
    fn netmsg_proc_fx_round_trip() {
        let msg = NetMsg::ProcFx {
            kind: 0, // SHOCK_ARC
            from: [10.0, 20.0],
            to:   [30.0, 40.0],
        };
        let bytes = bincode::serialize(&msg).unwrap();
        let back: NetMsg = bincode::deserialize(&bytes).unwrap();
        match back {
            NetMsg::ProcFx { kind, from, to } => {
                assert_eq!(kind, 0);
                assert_eq!(from, [10.0, 20.0]);
                assert_eq!(to,   [30.0, 40.0]);
            }
            _ => panic!("wrong variant"),
        }
    }

    /// Snapshot with a mix of enemies — exercises every field of
    /// `EnemyEntry` through bincode.
    #[test]
    fn netmsg_enemy_snapshot_round_trip() {
        let entries = vec![
            EnemyEntry { id: 1, kind: 0, pos: [10.5, -20.0], rot: 0.0,  hp: 8,  status_flags: 0, boss_class: NOT_A_BOSS },
            EnemyEntry { id: 2, kind: 6, pos: [-50.0, 50.0], rot: 3.14, hp: 52, status_flags: 1, boss_class: NOT_A_BOSS },
            EnemyEntry { id: 42, kind: 3, pos: [0.0, 0.0],   rot: -1.5, hp: 2,  status_flags: 7, boss_class: 4 /* Tender boss */ },
        ];
        let msg = NetMsg::EnemySnapshot { entries: entries.clone() };
        let bytes = bincode::serialize(&msg).unwrap();
        let back: NetMsg = bincode::deserialize(&bytes).unwrap();
        match back {
            NetMsg::EnemySnapshot { entries: got } => {
                assert_eq!(got.len(), entries.len());
                for (a, b) in got.iter().zip(entries.iter()) {
                    assert_eq!(a.id, b.id);
                    assert_eq!(a.kind, b.kind);
                    assert_eq!(a.pos, b.pos);
                    assert_eq!(a.rot, b.rot);
                    assert_eq!(a.hp, b.hp);
                }
            }
            _ => panic!("wrong variant"),
        }
    }

    /// `bind_socket(None)` lets the OS pick a port; the assigned port
    /// must be non-zero (else the OS rejected the bind).
    #[test]
    fn bind_socket_ephemeral_gives_nonzero_port() {
        let sock = bind_socket(None).expect("bind ephemeral");
        let port = sock.local_addr().expect("local_addr").port();
        assert_ne!(port, 0, "OS should assign a real port");
    }

    /// Sockets must come back non-blocking so `drain_packets` doesn't
    /// hang the netloop on an empty queue. Verified by checking that
    /// `recv_from` on an empty socket returns `WouldBlock` immediately.
    #[test]
    fn bind_socket_is_nonblocking() {
        let sock = bind_socket(None).expect("bind");
        let mut buf = [0u8; 32];
        match sock.recv_from(&mut buf) {
            Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::WouldBlock),
            Ok(_) => panic!("expected WouldBlock on empty socket"),
        }
    }

    /// End-to-end loopback: bind two sockets on 127.0.0.1, send a
    /// `NetMsg` from one, drain on the other, verify it decodes with
    /// the right source address.
    #[test]
    fn loopback_send_and_drain() {
        let receiver = bind_socket(None).expect("bind receiver");
        let sender = bind_socket(None).expect("bind sender");
        let receiver_addr: SocketAddr = format!(
            "127.0.0.1:{}",
            receiver.local_addr().unwrap().port()
        )
        .parse()
        .unwrap();

        send_to(&sender, receiver_addr, &NetMsg::Welcome {
            your_id: 9,
            host_name: "HOST".to_string(),
            existing_peers: vec![],
        }).expect("send");

        // UDP loopback can be very slightly delayed; spin briefly to
        // avoid a flaky test on slow CI runners.
        let mut packets = Vec::new();
        for _ in 0..50 {
            packets = drain_packets(&receiver);
            if !packets.is_empty() { break; }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        assert_eq!(packets.len(), 1, "exactly one packet should arrive");
        let (src, msg) = &packets[0];
        let expected_sender_port = sender.local_addr().unwrap().port();
        assert_eq!(src.port(), expected_sender_port);
        match msg {
            NetMsg::Welcome { your_id, .. } => assert_eq!(*your_id, 9),
            _ => panic!("wrong variant on the wire"),
        }
    }

    /// Drain on a quiet socket returns empty without error or block.
    #[test]
    fn drain_empty_socket_returns_empty() {
        let sock = bind_socket(None).expect("bind");
        assert!(drain_packets(&sock).is_empty());
    }

    /// `EnemyEntry` round-trips through bincode standalone (separate
    /// from the wrapping `NetMsg::EnemySnapshot`). The host's
    /// snapshot builder writes one of these per live enemy; if the
    /// struct's bincode layout drifts, that builder silently corrupts
    /// the stream.
    #[test]
    fn enemy_entry_round_trip() {
        let e = EnemyEntry {
            id: 12345,
            kind: 5,
            pos: [42.0, -17.5],
            rot: -2.5,
            hp: 9999,
            status_flags: 0b101,
            boss_class: NOT_A_BOSS,
        };
        let bytes = bincode::serialize(&e).unwrap();
        let back: EnemyEntry = bincode::deserialize(&bytes).unwrap();
        assert_eq!(back.id, e.id);
        assert_eq!(back.kind, e.kind);
        assert_eq!(back.pos, e.pos);
        assert_eq!(back.rot, e.rot);
        assert_eq!(back.hp, e.hp);
        assert_eq!(back.status_flags, e.status_flags);
    }

    /// Multiple queued packets all surface in a single drain call —
    /// the loop should run until WouldBlock, not return after the
    /// first packet. Regression guard: an earlier draft used
    /// `if let Ok(...)` instead of a loop and only saw the first
    /// packet per frame.
    #[test]
    fn drain_returns_all_queued_packets() {
        let receiver = bind_socket(None).expect("bind receiver");
        let sender = bind_socket(None).expect("bind sender");
        let receiver_addr: SocketAddr = format!(
            "127.0.0.1:{}",
            receiver.local_addr().unwrap().port()
        )
        .parse()
        .unwrap();

        for id in 0..5u8 {
            send_to(&sender, receiver_addr, &NetMsg::Bye { id }).expect("send");
        }

        // Spin briefly so all five packets land before we drain.
        let mut packets = Vec::new();
        for _ in 0..50 {
            packets = drain_packets(&receiver);
            if packets.len() >= 5 { break; }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        assert_eq!(packets.len(), 5, "all queued packets should surface");
        for (i, (_, msg)) in packets.iter().enumerate() {
            match msg {
                NetMsg::Bye { id } => assert_eq!(*id as usize, i),
                _ => panic!("wrong variant at {i}"),
            }
        }
    }

    /// Two-peer handshake end-to-end over loopback. Client binds an
    /// ephemeral socket, sends Hello to the host's known port,
    /// host drains and sees Hello, host sends Welcome back to the
    /// client's address (learned from recv_from), client drains and
    /// sees Welcome with the assigned id. Mirrors the real flow in
    /// `tick_handshake`.
    #[test]
    fn handshake_hello_welcome_loopback() {
        let host = bind_socket(None).expect("bind host");
        let host_addr: SocketAddr = format!(
            "127.0.0.1:{}",
            host.local_addr().unwrap().port()
        )
        .parse()
        .unwrap();
        let client = bind_socket(None).expect("bind client");

        // 1. Client → host: Hello
        send_to(&client, host_addr, &NetMsg::Hello {
            name: "TESTCLIENT".to_string(),
        }).expect("send hello");

        // 2. Host drains, finds Hello from client's address.
        let mut hello_packets = Vec::new();
        for _ in 0..50 {
            hello_packets = drain_packets(&host);
            if !hello_packets.is_empty() { break; }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(hello_packets.len(), 1);
        let (client_addr, msg) = &hello_packets[0];
        assert!(matches!(msg, NetMsg::Hello { .. }));

        // 3. Host → client: Welcome { your_id: 1, host_name, no existing peers }
        send_to(&host, *client_addr, &NetMsg::Welcome {
            your_id: 1,
            host_name: "HOST".to_string(),
            existing_peers: vec![],
        }).expect("send welcome");

        // 4. Client drains, finds Welcome with the right id.
        let mut welcome_packets = Vec::new();
        for _ in 0..50 {
            welcome_packets = drain_packets(&client);
            if !welcome_packets.is_empty() { break; }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(welcome_packets.len(), 1);
        let (src, msg) = &welcome_packets[0];
        assert_eq!(src.port(), host_addr.port(), "welcome should come from host's port");
        match msg {
            NetMsg::Welcome { your_id, .. } => assert_eq!(*your_id, 1),
            _ => panic!("wrong variant for welcome"),
        }
    }

    /// Wire-format size sanity for the FIXED-size connection-state
    /// messages. Bye + Transform haven't changed shape; Hello and
    /// Welcome now carry variable-length strings (player names) so
    /// they're tested separately for "small enough" rather than
    /// exact bytes.
    #[test]
    fn fixed_connection_messages_have_stable_size() {
        let bye_bytes = bincode::serialize(&NetMsg::Bye { id: 0 }).unwrap();
        let xform_bytes = bincode::serialize(&NetMsg::Transform {
            id: 0, pos: [0.0; 2], rot: 0.0, turret_rots: [0.0; 8],
        }).unwrap();
        assert_eq!(bye_bytes.len(),    5, "Bye     tag + u8");
        // 49 bytes measured; the small over-count vs the naive
        // tag + u8 + 12 + 32 = 45 estimate is bincode framing on
        // the fixed-size arrays. Hardcoded so wire-format drift
        // is caught early.
        assert_eq!(xform_bytes.len(), 49, "Transform packet size drift");
    }

    /// Hello + Welcome carry variable-length names but stay well under
    /// MTU even with generous limits — names should be capped on the
    /// UI side (we assume ≤ 32 chars in practice).
    #[test]
    fn variable_connection_messages_stay_small() {
        let hello_bytes = bincode::serialize(&NetMsg::Hello {
            name: "A".repeat(32),
        }).unwrap();
        let welcome_bytes = bincode::serialize(&NetMsg::Welcome {
            your_id: 0,
            host_name: "A".repeat(32),
            existing_peers: (0..8).map(|i| (i, "A".repeat(32))).collect(),
        }).unwrap();
        // 8-peer roster + 32-char names = generous worst case.
        assert!(hello_bytes.len()   < 100,  "Hello   {} bytes",   hello_bytes.len());
        assert!(welcome_bytes.len() < 600,  "Welcome {} bytes",   welcome_bytes.len());
    }

    /// Empty `EnemySnapshot` should be tiny — host sends one per tick
    /// even when no enemies are alive, so the floor matters. Bincode
    /// length prefix on the Vec is `u64::LE`, so empty = 4 (tag) + 8
    /// (len = 0).
    #[test]
    fn empty_snapshot_byte_size_is_minimal() {
        let bytes = bincode::serialize(&NetMsg::EnemySnapshot { entries: vec![] })
            .unwrap();
        assert_eq!(bytes.len(), 12);
    }

    /// 30-enemy snapshot stays well under a typical 1400-byte UDP MTU
    /// so we don't fragment. Per entry: 4 + 1 + 8 + 4 + 4 + 1 + 1 = 23
    /// bytes after adding status_flags + boss_class; 30 entries = 690
    /// bytes; plus 12 bytes of NetMsg+Vec framing = 702 bytes total.
    /// Generous headroom against a 1400-byte typical MTU.
    #[test]
    fn full_enemy_snapshot_fits_in_one_packet() {
        let entries: Vec<EnemyEntry> = (0..30)
            .map(|i| EnemyEntry {
                id: i, kind: (i as u8) % 7, pos: [i as f32, -(i as f32)],
                rot: 0.0, hp: 100 - i as i32, status_flags: 0,
                boss_class: NOT_A_BOSS,
            })
            .collect();
        let bytes = bincode::serialize(&NetMsg::EnemySnapshot { entries }).unwrap();
        assert!(bytes.len() < 1400, "got {} bytes", bytes.len());
    }

    /// `local_lan_ip` should return SOMETHING printable on any host
    /// with a network interface. Some CI environments don't have one;
    /// we accept the "unknown" fallback rather than asserting it
    /// parses as an IP.
    #[test]
    fn local_lan_ip_returns_non_empty() {
        let ip = local_lan_ip();
        assert!(!ip.is_empty());
    }

    /// Garbage bytes should be silently dropped and the drain should
    /// complete — we don't want one malformed packet to break the
    /// netloop. The good packet sent after the garbage must still
    /// arrive.
    #[test]
    fn drain_skips_malformed_packets() {
        let receiver = bind_socket(None).expect("bind receiver");
        let sender = bind_socket(None).expect("bind sender");
        let receiver_addr: SocketAddr = format!(
            "127.0.0.1:{}",
            receiver.local_addr().unwrap().port()
        )
        .parse()
        .unwrap();

        // Garbage first.
        sender.send_to(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF], receiver_addr).unwrap();
        // Real packet second.
        send_to(&sender, receiver_addr, &NetMsg::Bye { id: 1 }).expect("send");

        let mut packets = Vec::new();
        for _ in 0..50 {
            packets = drain_packets(&receiver);
            if !packets.is_empty() { break; }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        // The garbage drops silently; only the real packet survives.
        assert_eq!(packets.len(), 1);
        assert!(matches!(packets[0].1, NetMsg::Bye { id: 1 }));
    }
}
