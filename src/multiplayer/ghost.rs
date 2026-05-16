//! Remote player visualization. For each connected peer the host or
//! client receives `Transform` packets for, we spawn a "ghost" entity
//! on `PLAY_LAYER` that mirrors the peer's reported position. The
//! ghost is purely cosmetic — no `Friendly` tag, so the existing
//! single-player systems (movement, turret aim, HP bar, AI) don't
//! touch it. It's just a colored hull at the synced position.
//!
//! This is the Phase 1 contract: clients see *each other's boats
//! moving*. They do not share enemy state, XP, or customize state —
//! each player runs the full single-player simulation locally.

use std::collections::HashMap;
use std::time::Instant;

use bevy::prelude::*;
use bevy::render::view::RenderLayers;

use crate::balance::{HULL_LEN, HULL_WIDTH, PLAY_LAYER};
use crate::palette::Palette;

use super::net::{drain_packets, send_to, NetMsg};
use super::{NetMode, NetSession};

/// How many `Transform` packets per second each peer broadcasts to
/// every other peer. 30Hz is enough for visibly-smooth ghost motion
/// without flooding the network — the ship's max speed is ~120 px/s,
/// so one packet's worth of motion is ~4 px, well below the visible
/// jump threshold at the menu's chunky-pixel scale.
pub const TRANSFORM_SEND_HZ: f32 = 30.0;
const TRANSFORM_SEND_INTERVAL: f32 = 1.0 / TRANSFORM_SEND_HZ;

/// How long (seconds) a peer's ghost survives without a fresh
/// Transform packet before we assume the peer dropped and despawn
/// it. Generous — packet loss + Wi-Fi hiccups can easily produce
/// half-second silences on a healthy connection.
const GHOST_TIMEOUT_SECS: f32 = 5.0;

/// Marker on each ghost entity. `id` identifies which remote peer it
/// represents so we can find & update its Transform on each incoming
/// snapshot without iterating every entity in the world.
#[derive(Component, Clone, Copy)]
pub struct RemoteGhost {
    pub id: u8,
}

/// Latest known transform for a peer. Updated by `recv_packets`; read
/// by `apply_snapshots` to drive the ghost entity. `last_seen` drives
/// the timeout cleanup in `cull_stale_ghosts`.
#[derive(Clone, Copy)]
pub struct PeerSnapshot {
    pub pos: Vec2,
    pub rot: f32,
    pub last_seen: Instant,
}

/// Map of peer id → latest received snapshot. Lives in this module
/// (not in `mod.rs`) since the ghost-sync systems are the only thing
/// that read it.
#[derive(Resource, Default)]
pub struct PeerSnapshots(pub HashMap<u8, PeerSnapshot>);

/// Per-frame timer for the local-transform broadcast. Lives as a
/// resource (instead of `Local<f32>`) so the cadence is observable
/// for debugging and survives system reordering.
#[derive(Resource, Default)]
pub struct TransformSendTimer(pub f32);

/// Send the local player's `Friendly` transform to every known peer
/// at `TRANSFORM_SEND_HZ`. Reads the local Friendly's Transform +
/// Heading and emits one `NetMsg::Transform` packet per peer addr.
pub fn send_local_transform(
    time: Res<Time>,
    mut timer: ResMut<TransformSendTimer>,
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    local: Query<(&Transform, &crate::components::Heading), With<crate::components::Friendly>>,
) {
    if matches!(*mode, NetMode::Solo) { return; }
    let Some(session) = session else { return; };
    timer.0 += time.delta_secs();
    if timer.0 < TRANSFORM_SEND_INTERVAL { return; }
    timer.0 = 0.0;

    let Ok((tf, heading)) = local.single() else { return; };
    let msg = NetMsg::Transform {
        id: session.my_id,
        pos: [tf.translation.x, tf.translation.y],
        rot: heading.0,
    };
    for &addr in session.peers.values() {
        if let Err(e) = send_to(&session.sock, addr, &msg) {
            bevy::log::warn!("multiplayer: failed to send Transform to {addr}: {e}");
        }
    }
}

/// Drain every pending packet on the socket. Updates `PeerSnapshots`
/// for Transform messages, registers new peers on Hello (host only),
/// removes peers on Bye, and stashes incoming EnemySnapshots in
/// `LatestEnemySnapshot` for `apply_enemy_snapshot` to consume.
pub fn recv_packets(
    mode: Res<NetMode>,
    mut session: Option<ResMut<NetSession>>,
    mut snapshots: ResMut<PeerSnapshots>,
    mut latest_enemy: ResMut<super::enemies::LatestEnemySnapshot>,
    mut damage_relay: ResMut<super::enemies::PendingDamageRelay>,
    mut proc_fx: ResMut<super::enemies::ProcFxInbox>,
    mut pending_state: ResMut<super::state_sync::PendingStateChange>,
    mut roster: ResMut<super::LobbyRoster>,
    mut pending_kick: ResMut<super::PendingKick>,
) {
    if matches!(*mode, NetMode::Solo) { return; }
    let Some(session) = session.as_mut() else { return; };

    let packets = drain_packets(&session.sock);
    let now = Instant::now();
    for (addr, msg) in packets {
        match msg {
            NetMsg::Hello { name } => {
                // Hosts only: assign the next id, remember the addr, reply with Welcome,
                // and announce the joiner to existing peers. The full handshake is also
                // run by `tick_handshake` while in MainMenu/Hosting/JoiningWait — this
                // branch handles late joins that arrive once we're already in Lobby
                // (mode = Connected), when `tick_handshake` bails out.
                if !session.is_host { continue; }
                if !session.peers.values().any(|a| *a == addr) {
                    let next_id = session.next_peer_id;
                    session.next_peer_id += 1;
                    session.peers.insert(next_id, addr);
                    // Existing roster minus the new joiner.
                    let existing: Vec<(u8, String)> = roster.by_id.iter()
                        .map(|(&id, n)| (id, n.clone()))
                        .collect();
                    let reply = NetMsg::Welcome {
                        your_id: next_id,
                        host_name: roster.by_id.get(&0).cloned()
                            .unwrap_or_else(|| "HOST".to_string()),
                        existing_peers: existing,
                    };
                    if let Err(e) = send_to(&session.sock, addr, &reply) {
                        bevy::log::warn!("multiplayer: failed to send Welcome to {addr}: {e}");
                    } else {
                        bevy::log::info!("multiplayer: peer {next_id} '{name}' connected from {addr}");
                    }
                    // Announce to every existing peer (skip the new
                    // joiner; they just got `Welcome`).
                    let announce = NetMsg::PeerJoined { id: next_id, name: name.clone() };
                    for (&peer_id, &peer_addr) in session.peers.iter() {
                        if peer_id == next_id { continue; }
                        let _ = send_to(&session.sock, peer_addr, &announce);
                    }
                    roster.by_id.insert(next_id, name);
                }
            }
            NetMsg::Welcome { your_id, host_name, existing_peers } => {
                // Clients only.
                if session.is_host { continue; }
                session.my_id = your_id;
                session.peers.insert(0, addr);
                session.welcomed = true;
                roster.by_id.clear();
                roster.by_id.insert(0, host_name);
                for (id, name) in existing_peers {
                    roster.by_id.insert(id, name);
                }
                // Our own entry isn't sent over the wire (would echo
                // our own name back to us); insert it locally.
                // Placeholder name — local-name resource isn't here
                // as a param, but the lobby UI reads roster anyway,
                // so a missing self-entry just means our own name
                // won't show in the list. Acceptable; future polish
                // could inject local_name via another param.
                bevy::log::info!("multiplayer: connected to host {addr} as id {your_id}");
            }
            NetMsg::PeerJoined { id, name } => {
                // Clients only — host already updated its roster
                // when it sent the Welcome.
                if session.is_host { continue; }
                roster.by_id.insert(id, name);
            }
            NetMsg::PeerLeft { id } => {
                // Clients only.
                if session.is_host { continue; }
                roster.by_id.remove(&id);
            }
            NetMsg::Kicked { reason } => {
                // Clients only — host should never receive its own
                // kick packet (it sends them to others).
                if session.is_host { continue; }
                pending_kick.0 = Some(reason);
            }
            NetMsg::Transform { id, pos, rot } => {
                snapshots.0.insert(
                    id,
                    PeerSnapshot { pos: Vec2::new(pos[0], pos[1]), rot, last_seen: now },
                );
            }
            NetMsg::Bye { id } => {
                session.peers.remove(&id);
                snapshots.0.remove(&id);
                bevy::log::info!("multiplayer: peer {id} disconnected");
            }
            NetMsg::EnemySnapshot { entries } => {
                // Client only — host should never receive these. If a
                // misconfigured peer sends one to the host, silently
                // drop it (the host's authoritative state wins anyway).
                if session.is_host { continue; }
                latest_enemy.0 = Some(entries);
            }
            NetMsg::DamageEnemy { enemy_id, amount, hit_pos, weapon, runes } => {
                // Host only — clients can't damage other clients in
                // Phase 2.5 (no mirror-vs-client damage path exists
                // yet). Silently drop on the client side.
                if !session.is_host { continue; }
                let weapon = crate::weapon::WeaponType::from_u8(weapon)
                    .unwrap_or(crate::weapon::WeaponType::Standard);
                let runes: Vec<crate::rune::Rune> = runes
                    .iter()
                    .filter_map(|n| crate::rune::Rune::from_u8(*n))
                    .collect();
                damage_relay.entries.push(super::enemies::RelayedDamage {
                    enemy_id,
                    amount,
                    hit_pos: Vec2::new(hit_pos[0], hit_pos[1]),
                    weapon,
                    runes,
                });
            }
            NetMsg::ProcFx { kind, from, to } => {
                // Forwarded to a separate buffer for the visual-spawn
                // system. Host re-broadcasts to other peers in a
                // dedicated relay system; non-host peers just consume
                // and render.
                proc_fx.events.push(super::enemies::ReceivedProcFx {
                    kind,
                    from: Vec2::new(from[0], from[1]),
                    to:   Vec2::new(to[0], to[1]),
                    sender_addr: addr,
                });
            }
            NetMsg::StateChange { state } => {
                // Client only — clients don't tell the host what
                // state to be in. Silently drop on host side.
                if session.is_host { continue; }
                if let Some(target) = crate::AppState::from_u8(state) {
                    pending_state.0 = Some(target);
                }
            }
        }
    }
}

/// Spawn a ghost entity for each peer that has a snapshot but no
/// existing `RemoteGhost`. Cheap — only triggers on first sight of
/// each id.
pub fn spawn_missing_ghosts(
    mut commands: Commands,
    palette: Res<Palette>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    snapshots: Res<PeerSnapshots>,
    session: Option<Res<super::NetSession>>,
    existing: Query<&RemoteGhost>,
) {
    let known: std::collections::HashSet<u8> = existing.iter().map(|g| g.id).collect();
    let is_host = session.as_deref().map(|s| s.is_host).unwrap_or(false);
    for (&id, snap) in &snapshots.0 {
        if known.contains(&id) { continue; }
        let hull_mesh = meshes.add(Capsule2d::new(HULL_WIDTH * 0.5, HULL_LEN - HULL_WIDTH));
        // Tint the ghost so it reads as "another player" — desaturate
        // and shift toward cyan from the friendly hull colour so the
        // two boats are distinguishable at a glance.
        let mat = materials.add(ghost_tint(palette.hull));
        let mut ec = commands.spawn((
            Mesh2d(hull_mesh),
            MeshMaterial2d(mat),
            Transform::from_translation(snap.pos.extend(0.0))
                .with_rotation(Quat::from_rotation_z(snap.rot)),
            RenderLayers::layer(PLAY_LAYER),
            RemoteGhost { id },
        ));
        // On the host, every connected client's ghost gets the
        // `Friendly` marker so the host's enemy-AI targeting queries
        // (`With<Friendly>, Without<Ally>`) see two valid player
        // ships instead of one. The ghost has no Velocity / Heading
        // / turret children, so movement and turret-fire systems
        // skip it naturally — only the targeting half of the
        // friendly contract activates.
        //
        // Client doesn't need this tag — its local sim has no enemy
        // AI running (mirrors are inert), so targeting is moot.
        if is_host {
            ec.insert(crate::components::Friendly);
        }
    }
}

/// Apply the latest `PeerSnapshot` to each ghost entity's Transform.
/// No interpolation in Phase 1 — straight snap to the most-recent
/// position. Visible jitter on bad networks; can layer smoothing on
/// later if it bothers anyone.
pub fn apply_snapshots(
    snapshots: Res<PeerSnapshots>,
    mut ghosts: Query<(&RemoteGhost, &mut Transform)>,
) {
    for (ghost, mut tf) in &mut ghosts {
        if let Some(snap) = snapshots.0.get(&ghost.id) {
            tf.translation.x = snap.pos.x;
            tf.translation.y = snap.pos.y;
            tf.rotation = Quat::from_rotation_z(snap.rot);
        }
    }
}

/// Remove ghost entities whose snapshots haven't refreshed for
/// `GHOST_TIMEOUT_SECS`. Catches drop-out cases where a peer
/// disappeared without sending Bye (laptop closed, network died).
pub fn cull_stale_ghosts(
    mut commands: Commands,
    mut snapshots: ResMut<PeerSnapshots>,
    ghosts: Query<(Entity, &RemoteGhost)>,
) {
    let now = Instant::now();
    let stale: Vec<u8> = snapshots
        .0
        .iter()
        .filter(|(_, s)| now.duration_since(s.last_seen).as_secs_f32() > GHOST_TIMEOUT_SECS)
        .map(|(id, _)| *id)
        .collect();
    for id in &stale {
        snapshots.0.remove(id);
    }
    for (e, ghost) in &ghosts {
        if !snapshots.0.contains_key(&ghost.id) {
            commands.entity(e).despawn();
        }
    }
}

/// Despawn every ghost + clear snapshots. Run on `OnExit(Playing)`
/// (and on any clean disconnect path) so a fresh session doesn't
/// re-show last session's ghosts.
pub fn despawn_all_ghosts(
    mut commands: Commands,
    mut snapshots: ResMut<PeerSnapshots>,
    ghosts: Query<Entity, With<RemoteGhost>>,
) {
    for e in &ghosts {
        commands.entity(e).despawn();
    }
    snapshots.0.clear();
}

/// Shift the friendly hull colour toward cyan so a remote ghost reads
/// as a distinct player at a glance. Lowers red, lifts blue, keeps
/// luminance roughly equal so it doesn't look like an enemy ship.
fn ghost_tint(hull: Color) -> Color {
    let s: bevy::color::Srgba = hull.into();
    Color::srgb(
        (s.red * 0.55).clamp(0.0, 1.0),
        (s.green * 0.80).clamp(0.0, 1.0),
        (s.blue + (1.0 - s.blue) * 0.45).clamp(0.0, 1.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tint should be deterministic — same input twice yields the
    /// same output. Guards against accidental dependence on RNG / time
    /// if anyone refactors this later.
    #[test]
    fn ghost_tint_is_deterministic() {
        let hull = Color::srgb(0.75, 0.40, 0.20);
        let a = ghost_tint(hull);
        let b = ghost_tint(hull);
        let sa: bevy::color::Srgba = a.into();
        let sb: bevy::color::Srgba = b.into();
        assert_eq!(sa.red,   sb.red);
        assert_eq!(sa.green, sb.green);
        assert_eq!(sa.blue,  sb.blue);
    }

    /// The tint must visibly differ from the source so the ghost reads
    /// as a different player. Compares each channel against the input.
    #[test]
    fn ghost_tint_differs_from_hull() {
        let hull = Color::srgb(0.75, 0.40, 0.20);
        let tinted = ghost_tint(hull);
        let h: bevy::color::Srgba = hull.into();
        let t: bevy::color::Srgba = tinted.into();
        assert!(t.red   < h.red,   "red should drop ({} → {})", h.red,   t.red);
        assert!(t.green < h.green, "green should drop ({} → {})", h.green, t.green);
        assert!(t.blue  > h.blue,  "blue should lift ({} → {})", h.blue,  t.blue);
    }

    /// All channels must stay within `[0.0, 1.0]` even for extreme
    /// hulls (white, black). Out-of-range colors render as garbled
    /// glitches in wgpu.
    #[test]
    fn ghost_tint_clamps_to_valid_range() {
        for &hull in &[
            Color::srgb(0.0, 0.0, 0.0),
            Color::srgb(1.0, 1.0, 1.0),
            Color::srgb(0.5, 0.5, 0.5),
        ] {
            let t: bevy::color::Srgba = ghost_tint(hull).into();
            assert!(t.red.is_finite()   && (0.0..=1.0).contains(&t.red));
            assert!(t.green.is_finite() && (0.0..=1.0).contains(&t.green));
            assert!(t.blue.is_finite()  && (0.0..=1.0).contains(&t.blue));
        }
    }

    /// PeerSnapshots is just a wrapper; verify default + insert work.
    #[test]
    fn peer_snapshots_default_and_insert() {
        let mut snaps = PeerSnapshots::default();
        assert!(snaps.0.is_empty());
        snaps.0.insert(
            1,
            PeerSnapshot {
                pos: Vec2::new(10.0, 20.0),
                rot: 0.5,
                last_seen: Instant::now(),
            },
        );
        assert_eq!(snaps.0.len(), 1);
        let s = snaps.0.get(&1).unwrap();
        assert_eq!(s.pos.x, 10.0);
        assert_eq!(s.rot, 0.5);
    }
}
