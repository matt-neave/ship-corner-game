//! Remote player visualization. For each connected peer the host or
//! client receives `Transform` packets for, we spawn a "ghost" entity
//! on `PLAY_LAYER` that mirrors the peer's reported position. The
//! ghost carries `Friendly` on the host side (so enemy AI targets
//! both ships) but no gameplay-driving components — no Velocity, no
//! TurretSlot, no Health — so the local single-player systems
//! (movement, turret AI, HP-bar updates) skip it naturally.
//!
//! Bullets fired by remote peers arrive as `BulletFiredEvent`
//! signals (see `multiplayer::bullets`); turret VISUALS on the ghost
//! come from `PeerLoadouts` (see `multiplayer::loadout`).

use std::collections::HashMap;
use std::time::Instant;

use bevy::prelude::*;
use bevy::render::view::RenderLayers;

use crate::balance::{HULL_LEN, HULL_WIDTH, PLAY_LAYER};

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
///
/// `turret_rots` carries each turret base's local rotation so the
/// ghost ship's turret children visually track the peer's live aim.
/// Index = TurretSlot.index; unequipped slots store 0.0 and are
/// ignored on apply.
#[derive(Clone, Copy)]
pub struct PeerSnapshot {
    pub pos: Vec2,
    pub rot: f32,
    pub turret_rots: [f32; 8],
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

/// Cadence for [`send_heartbeat`]. Low rate (1Hz) — its only job is
/// to keep `NetSession.last_seen` fresh during otherwise-quiet
/// states (Paused, Lobby, menus) so `detect_stale_peers` doesn't
/// time the link out. `PEER_TIMEOUT_SECS = 5.0` gives 5 heartbeats
/// of cushion.
const HEARTBEAT_INTERVAL_SECS: f32 = 1.0;

#[derive(Resource, Default)]
pub struct HeartbeatTimer(pub f32);

/// Send a low-rate `Heartbeat` packet to every known peer whenever
/// we're Connected, regardless of `AppState`. Without this the link
/// goes silent during pause / menus (no enemy snapshots, no
/// transform packets), and `detect_stale_peers` kicks the other
/// peer out after `PEER_TIMEOUT_SECS`.
pub fn send_heartbeat(
    time: Res<Time>,
    mut timer: ResMut<HeartbeatTimer>,
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
) {
    let Some(session) = session else { return };
    if !matches!(*mode, NetMode::Connected) { return };
    if !session.welcomed { return; }
    timer.0 += time.delta_secs();
    if timer.0 < HEARTBEAT_INTERVAL_SECS { return; }
    timer.0 = 0.0;
    for &addr in session.peers.values() {
        let _ = send_to(&session.sock, addr, &NetMsg::Heartbeat);
    }
}

/// Send the local player's `Friendly` transform to every known peer
/// at `TRANSFORM_SEND_HZ`. Reads the local Friendly's Transform +
/// Heading and emits one `NetMsg::Transform` packet per peer addr.
pub fn send_local_transform(
    time: Res<Time>,
    mut timer: ResMut<TransformSendTimer>,
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    // `LocalPlayer` (not just Friendly) — the host has TWO Friendly
    // entities (local + ghost-of-peer). `single()` on Friendly Errs
    // and silently bails, so the host never sends Transform updates
    // and the client's view of the host's ship never moves.
    local: Query<(&Transform, &crate::components::Heading), With<crate::components::LocalPlayer>>,
    turret_slots: Query<(&crate::turret::TurretSlot, &Transform)>,
) {
    if matches!(*mode, NetMode::Solo) { return; }
    let Some(session) = session else { return; };
    timer.0 += time.delta_secs();
    if timer.0 < TRANSFORM_SEND_INTERVAL { return; }
    timer.0 = 0.0;

    let Ok((tf, heading)) = local.single() else { return; };

    // Pack each local turret base's LOCAL rotation (its Transform.rotation
    // is set by `turret_aim_fire` to track the nearest enemy each frame).
    // Slot index → array slot. Unequipped slots stay at 0.0 — receivers
    // ignore them via `PeerLoadouts`.
    let mut turret_rots = [0.0_f32; 8];
    for (slot, ttf) in &turret_slots {
        if slot.index >= 8 { continue; }
        let (z, _, _) = ttf.rotation.to_euler(EulerRot::ZYX);
        turret_rots[slot.index] = z;
    }

    let msg = NetMsg::Transform {
        id: session.my_id,
        pos: [tf.translation.x, tf.translation.y],
        rot: heading.0,
        turret_rots,
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
    mut loadout_inboxes: super::loadout::LoadoutInboxes,
    mut pending_wave: ResMut<super::wave::PendingWaveState>,
    mut bullet_inbox: ResMut<super::bullets::BulletFiredInbox>,
    mut death_relay: DeathRelayInboxes,
    mut xp_inboxes: super::xp_sync::XpInboxes,
    mut pending_ready: ResMut<super::ready::PendingPeerReady>,
) {
    if matches!(*mode, NetMode::Solo) { return; }
    let Some(session) = session.as_mut() else { return; };

    let packets = drain_packets(&session.sock);
    let now = Instant::now();
    for (addr, msg) in packets {
        // Liveness: any packet from a known address marks that peer
        // as alive. Drives `detect_stale_peers`'s timeout sweep so a
        // hard process kill / network drop eventually frees the
        // remaining peers instead of leaving them stuck.
        if let Some((&peer_id, _)) = session.peers.iter().find(|(_, a)| **a == addr) {
            session.last_seen.insert(peer_id, now);
        }
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
            NetMsg::Transform { id, pos, rot, turret_rots } => {
                snapshots.0.insert(
                    id,
                    PeerSnapshot {
                        pos: Vec2::new(pos[0], pos[1]),
                        rot,
                        turret_rots,
                        last_seen: now,
                    },
                );
            }
            NetMsg::Bye { id } => {
                session.peers.remove(&id);
                snapshots.0.remove(&id);
                // Client receiving Bye from host (id 0) → host left
                // the session. Trigger teardown via the existing
                // kick path so handle_received_kick returns us to
                // MainMenu with a clear reason. Without this the
                // client would be stuck in Lobby / Playing /
                // WaitingForHost with no host.
                if !session.is_host && id == 0 {
                    pending_kick.0 = Some("host disconnected".to_string());
                }
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
                // Host only — DamageEnemy is the client→host relay
                // direction; the host is the authority on enemy HP.
                // Silently drop on the client side.
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
                // Either direction: host receives client's pause /
                // unpause requests, client receives every host
                // transition. `apply_state_change` is the gatekeeper
                // — it rejects host-flow states (Customize, Map, …)
                // pushed by a client. Letting all StateChange packets
                // reach the apply system keeps that policy in one
                // place instead of duplicating the filter here.
                if let Some(target) = crate::AppState::from_u8(state) {
                    pending_state.0 = Some(target);
                }
            }
            NetMsg::PlayerStatsSync { from_peer, stats } => {
                // Either direction — each peer broadcasts their own
                // stats and receivers store them under PeerLoadouts.
                // Ignore loopback: if `from_peer == my_id`, the packet
                // is our own (broadcast-to-self). The apply system's
                // overwrite would be a no-op anyway but skipping
                // saves a hash insert.
                if from_peer == session.my_id { continue; }
                loadout_inboxes.stats.0 = Some((from_peer, stats));
            }
            NetMsg::TurretConfigSync { from_peer, slots } => {
                if from_peer == session.my_id { continue; }
                loadout_inboxes.turret.0 = Some((from_peer, slots));
            }
            NetMsg::WaveStateSync { wave_idx, wave_count, phase, remaining } => {
                if session.is_host { continue; }
                pending_wave.0 = Some((wave_idx, wave_count, phase, remaining));
            }
            NetMsg::XpSync { current, level } => {
                if session.is_host { continue; }
                xp_inboxes.xp.0 = Some((current, level));
            }
            NetMsg::LevelUpGranted { count } => {
                // Drained by `apply_received_level_up_grants` which
                // adds to local pending. Host skips the add (its
                // grant_kill_xp already counted).
                xp_inboxes.grants.0.push(count);
            }
            NetMsg::Heartbeat => {
                // No-op: the addr → `last_seen` update above the
                // match already refreshed liveness. The Heartbeat
                // arm exists so the unhandled-variant warning
                // doesn't fire.
            }
            NetMsg::DamagePlayer { amount, hit_pos } => {
                // Client only — only the host emits these (host's
                // ghost-of-peer absorbing damage from local enemy
                // bullets). Silently drop on host.
                if session.is_host { continue; }
                death_relay.damage_player.0.push((amount, Vec2::new(hit_pos[0], hit_pos[1])));
            }
            NetMsg::BulletFired { pos, dir, weapon, range } => {
                bullet_inbox.events.push(super::bullets::ReceivedBulletFired {
                    pos: Vec2::new(pos[0], pos[1]),
                    dir: Vec2::new(dir[0], dir[1]),
                    weapon,
                    range,
                    sender_addr: addr,
                });
            }
            NetMsg::PeerDied { id } => {
                // Host tracks team alive state. Clients ignore —
                // they only need to know about peer deaths if we
                // ever spectate someone (not in this phase).
                if !session.is_host { continue; }
                death_relay.team_tracker.dead_peers.insert(id);
                bevy::log::info!("multiplayer: peer {id} died (team tracker now: {:?})",
                                 death_relay.team_tracker.dead_peers);
            }
            NetMsg::PeerReady { id } => {
                // Every peer tracks the team's ready set so the local
                // shop UI can render "X / N READY". `drain_ready_inbox`
                // moves these into TeamReadyTracker; the host's
                // `host_advance_when_all_ready` reads the same tracker
                // to make the canonical state transition.
                pending_ready.0.push(id);
            }
            NetMsg::PeerRevived { id } => {
                // Clients only (host broadcasts; doesn't receive its
                // own revive). `REVIVE_ALL` sentinel covers the
                // common stage-transition-revives-everyone case.
                if session.is_host { continue; }
                if id == super::death::REVIVE_ALL || id == session.my_id {
                    death_relay.pending_revive.0 = true;
                }
            }
        }
    }
}

/// Spawn a peer-replica ship for each connected peer that has a
/// snapshot but no existing entity. Identical visuals to the local
/// player's ship — same hull mesh, same hull material from the
/// shared `PaletteMaterials.hull`, same turret children (pulled
/// from `PeerLoadouts`). Differentiation between local and remote
/// happens via the `LocalPlayer` marker (only on the local ship)
/// and the eventual name tags above the hull.
///
/// Why no Velocity / Heading / TurretSlot / Health components: this
/// ship is driven by network snapshots, not the local sim. Stripping
/// those components keeps the per-frame friendly_movement /
/// turret_fire / hp-update systems from second-guessing the host's
/// authoritative pose.
pub fn spawn_missing_ghosts(
    mut commands: Commands,
    pm: Option<Res<crate::palette::PaletteMaterials>>,
    loadouts: Res<super::loadout::PeerLoadouts>,
    mut meshes: ResMut<Assets<Mesh>>,
    snapshots: Res<PeerSnapshots>,
    session: Option<Res<super::NetSession>>,
    existing: Query<&RemoteGhost>,
) {
    let known: std::collections::HashSet<u8> = existing.iter().map(|g| g.id).collect();
    let is_host = session.as_deref().map(|s| s.is_host).unwrap_or(false);
    let Some(pm) = pm else { return };

    for (&id, snap) in &snapshots.0 {
        if known.contains(&id) { continue; }

        // Same hull mesh + material as `spawn_player_world` — peers
        // render identically. Use the shared `pm.hull` handle so any
        // future palette change applies to both ships.
        let hull_mesh = meshes.add(Capsule2d::new(HULL_WIDTH * 0.5, HULL_LEN - HULL_WIDTH));

        let mut ec = commands.spawn((
            Mesh2d(hull_mesh),
            MeshMaterial2d(pm.hull.clone()),
            Transform::from_translation(snap.pos.extend(1.0))
                .with_rotation(Quat::from_rotation_z(snap.rot)),
            RenderLayers::layer(PLAY_LAYER),
            RemoteGhost { id },
            crate::components::Faction(crate::components::FactionKind::Friendly),
        ));
        // On the host, every connected client's ship gets the full
        // damage-absorbing kit so enemy bullets actually hit it:
        // - `Friendly` so AI targets + `bullet_collisions`' friendly
        //   query matches.
        // - `Health` + `HitFx` + `Heading` — required by the
        //   `bullet_collisions` enemy-bullet branch's `&mut Health`
        //   etc. (the ghost would otherwise be invisible to bullets,
        //   so enemies aimed at the client peer would never deal
        //   damage anywhere — the bug "non-host peer not receiving
        //   damage").
        // - `GhostDamageRelay { peer_id, last_seen_hp }` — used by
        //   `relay_ghost_damage` to detect HP drops and send a
        //   `DamagePlayer` packet to the corresponding peer, who
        //   applies the delta to its OWN local `Friendly` Health.
        //
        // `GHOST_HP_SENTINEL` is well above any real player HP so
        // the ghost can absorb arbitrary damage without dying on
        // the host side — true HP lives on the peer.
        if is_host {
            ec.insert((
                crate::components::Friendly,
                crate::components::Health(GHOST_HP_SENTINEL),
                crate::components::Heading(snap.rot),
                crate::effects::HitFx::new(pm.hull.clone()),
                GhostDamageRelay { peer_id: id, last_seen_hp: GHOST_HP_SENTINEL },
            ));
        }
        let ship = ec.id();

        // Visual turret children — driven by THIS peer's loadout
        // (from PeerLoadouts), not the local TurretConfig. If we
        // haven't received their loadout yet, spawn no turrets;
        // `refresh_ghost_turrets` will populate them once the
        // TurretConfigSync packet lands.
        if let Some(cfg) = loadouts.0.get(&id).and_then(|l| l.turret.as_ref()) {
            spawn_remote_turret_visuals(&mut commands, &mut meshes, &pm, cfg, ship);
        }
    }
}

/// Marker on the turret base / barrel children spawned by
/// `spawn_remote_turret_visuals`. Lets `refresh_ghost_turrets`
/// find and despawn them when the peer's loadout changes, without
/// touching the hull entity.
///
/// `slot_index` lets `apply_ghost_turret_aims` map a turret base
/// entity back to its slot so it can apply the correct rotation
/// from `PeerSnapshot.turret_rots`. Barrels store the parent base's
/// index too — harmless duplicate, simpler than separate markers.
#[derive(Component)]
pub struct GhostTurretChild {
    pub slot_index: usize,
}

/// Apply per-turret rotations from the latest `PeerSnapshot` to
/// each ghost ship's turret base children. Snaps every frame (no
/// lerp) — at 30Hz Transform cadence the visible step is ~33ms,
/// fine for a turret that's already moving at most a few degrees
/// per frame.
///
/// Only rotates BASE entities (slot_index lookup is identity-based).
/// Barrels are children of bases and inherit the rotation
/// automatically through the Transform hierarchy.
pub fn apply_ghost_turret_aims(
    snapshots: Res<PeerSnapshots>,
    ghosts: Query<(&RemoteGhost, &Children)>,
    mut bases: Query<(&GhostTurretChild, &mut Transform), Without<RemoteGhost>>,
) {
    for (ghost, children) in &ghosts {
        let Some(snap) = snapshots.0.get(&ghost.id) else { continue };
        for child in children.iter() {
            if let Ok((tag, mut tf)) = bases.get_mut(child) {
                let want = snap.turret_rots[tag.slot_index];
                let q = Quat::from_rotation_z(want);
                if tf.rotation != q { tf.rotation = q; }
            }
        }
    }
}

/// Component on the host's ghost-of-peer hull that drives the
/// "damage taken by the ghost → broadcast to peer" relay path. The
/// host's ghost has a sentinel-high `Health` so it absorbs all
/// incoming damage without dying; `relay_ghost_damage` watches for
/// drops, computes the delta, sends a [`super::net::NetMsg::DamagePlayer`]
/// to `peer_id`, and resets `last_seen_hp` to the new value.
///
/// `last_seen_hp` lives inside this component (not as a separate
/// `PreviousHp`) so the production HP-bar systems don't think the
/// ghost just took damage and try to draw an HP bar over it.
#[derive(Component)]
pub struct GhostDamageRelay {
    pub peer_id: u8,
    pub last_seen_hp: i32,
}

/// Sentinel HP for host-side ghost ships. Much higher than any real
/// player HP so the ghost absorbs arbitrary cumulative damage on the
/// host without crossing zero. The actual peer's HP lives on the
/// peer; the host only acts as a damage-attribution router.
pub const GHOST_HP_SENTINEL: i32 = 1_000_000;

/// Receive buffer for `NetMsg::DamagePlayer`. Populated by
/// `recv_packets` on the client side, drained by
/// `apply_received_player_damage` which applies the damage to the
/// local Friendly + spawns local hit fx.
#[derive(Resource, Default)]
pub struct PendingPlayerDamage(pub Vec<(i32, Vec2)>);

/// Bundled SystemParam: the death + revive + damage-relay inboxes,
/// grouped so `recv_packets` stays under Bevy's 16-param cap.
#[derive(bevy::ecs::system::SystemParam)]
pub struct DeathRelayInboxes<'w> {
    pub team_tracker: ResMut<'w, super::death::TeamDeathTracker>,
    pub pending_revive: ResMut<'w, super::death::PendingRevive>,
    pub damage_player: ResMut<'w, PendingPlayerDamage>,
}

/// Client-only: drain incoming `DamagePlayer` packets, apply each
/// to the local Friendly's Health (via the standard damage helper
/// + Shield absorption), and spawn the same hit-particle visual a
/// host-side enemy-bullet impact would.
///
/// Each packet's amount is capped at the player's CURRENT HP so
/// any bug in the host-side ghost damage accumulation can only
/// kill the player once (not insta-kill repeatedly). The cap also
/// papers over the spawn-time race where the ghost might briefly
/// be in a bad position relative to enemies.
pub fn apply_received_player_damage(
    mut commands: Commands,
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    pm: Option<Res<crate::palette::PaletteMaterials>>,
    em: Option<Res<crate::effects::EffectMeshes>>,
    mut inbox: ResMut<PendingPlayerDamage>,
    player_stats: Res<crate::stats::PlayerStats>,
    mut local: Query<(
        &mut crate::components::Health,
        &mut crate::effects::HitFx,
        Option<&mut crate::stats::Shield>,
    ), With<crate::components::LocalPlayer>>,
) {
    let Some(session) = session else { return };
    if !matches!(*mode, NetMode::Connected) || session.is_host { return; }
    if inbox.0.is_empty() { return; }
    let Some(pm) = pm else { inbox.0.clear(); return; };
    let Some(em) = em else { inbox.0.clear(); return; };
    let Ok((mut hp, mut fx, mut shield_opt)) = local.single_mut() else {
        inbox.0.clear();
        return;
    };
    let mut rng = rand::thread_rng();
    let max_hp = player_stats.max_hp();
    for (raw_amount, hit_pos) in inbox.0.drain(..) {
        // Defensive: cap any single packet at max-HP so an
        // accumulation bug on the host side can't insta-kill the
        // peer. A single 1-shot of the player's whole HP bar is
        // still possible (rammer impact, oil pool, etc.) — but
        // never *more* than that from one relay.
        let capped = raw_amount.clamp(0, max_hp);
        if raw_amount > max_hp {
            bevy::log::warn!(
                "apply_received_player_damage: capped oversized relay {} → {} (max_hp); likely a host-side accumulation bug",
                raw_amount, max_hp,
            );
        }
        let after_shield = shield_opt
            .as_mut()
            .map(|s| s.absorb(capped))
            .unwrap_or(capped);
        crate::bullet::apply_damage(&mut hp, &mut fx, after_shield);
        crate::effects::spawn_hit_particles(
            &mut commands, &em, &pm.bullet_enemy, hit_pos, 5, 50.0, &mut rng,
        );
    }
}

/// Host-only: detect HP drops on ghost ships (caused by enemy bullets
/// hitting them via `bullet_collisions`) and forward the damage to
/// the corresponding peer via `DamagePlayer`. Resets the ghost's HP
/// back to the sentinel after each relay so future hits register
/// fresh deltas.
///
/// Why per-frame poll instead of a `Changed<Health>` filter: bullets
/// can hit the ghost multiple times per frame, and we want the SUM
/// of those hits as one packet rather than separate writes.
pub fn relay_ghost_damage(
    session: Option<Res<super::NetSession>>,
    mode: Res<super::NetMode>,
    mut ghosts: Query<(
        &mut GhostDamageRelay,
        &mut crate::components::Health,
        &Transform,
    )>,
) {
    let Some(session) = session else { return };
    if !matches!(*mode, super::NetMode::Connected) || !session.is_host { return; }
    for (mut relay, mut hp, tf) in &mut ghosts {
        if hp.0 >= relay.last_seen_hp { continue; }
        let dmg = relay.last_seen_hp - hp.0;
        let pos = tf.translation.truncate();
        let msg = super::net::NetMsg::DamagePlayer {
            amount: dmg,
            hit_pos: [pos.x, pos.y],
        };
        if let Some(&addr) = session.peers.get(&relay.peer_id) {
            let _ = super::net::send_to(&session.sock, addr, &msg);
        }
        // Reset for next round of accumulation.
        hp.0 = GHOST_HP_SENTINEL;
        relay.last_seen_hp = GHOST_HP_SENTINEL;
    }
}

/// When a peer's [`PeerLoadouts`] entry changes (they bought a new
/// turret in their shop), despawn the old turret children on their
/// ghost and respawn from the new config. Runs each frame; cheap
/// short-circuit on `is_changed()` keeps it negligible.
///
/// `Children` is intentionally optional because a freshly-spawned
/// ghost has no children yet (Bevy doesn't add the component until
/// at least one child exists, and the loadout often arrives a frame
/// or two AFTER the ghost spawns). Without `Option`, the ship would
/// never match the query and turret visuals would never appear for
/// peers whose loadout arrives post-ghost.
///
/// `commands.entity(...).try_despawn()` guards against the Bevy
/// 0.16 warning ("entity does not exist") when a child was already
/// recursively despawned via the parent ghost.
pub fn refresh_ghost_turrets(
    mut commands: Commands,
    pm: Option<Res<crate::palette::PaletteMaterials>>,
    mut meshes: ResMut<Assets<Mesh>>,
    loadouts: Res<super::loadout::PeerLoadouts>,
    all_ghosts: Query<(Entity, &RemoteGhost, Option<&Children>)>,
    turret_children: Query<Entity, With<GhostTurretChild>>,
) {
    if !loadouts.is_changed() { return; }
    let Some(pm) = pm else { return };

    // When PeerLoadouts changes we don't know WHICH peer changed
    // without a separate diff resource. Cheapest correct approach:
    // re-derive every ghost's turret children from the current map.
    // With at most a handful of peers + ~8 turret slots this is a
    // trivial despawn-then-respawn.
    for (ship, ghost, children) in &all_ghosts {
        if let Some(children) = children {
            for child in children.iter() {
                if turret_children.get(child).is_ok() {
                    commands.entity(child).try_despawn();
                }
            }
        }
        if let Some(cfg) = loadouts.0.get(&ghost.id).and_then(|l| l.turret.as_ref()) {
            spawn_remote_turret_visuals(&mut commands, &mut meshes, &pm, cfg, ship);
        }
    }
}

/// Spawn turret base + barrel meshes as children of the remote
/// ship's hull. Mirrors the per-slot loop in `spawn_player_world`
/// (ship.rs) but drops the `TurretSlot` component so no firing AI
/// hooks onto these. Reads from the PEER's `TurretConfig` (passed
/// in by the caller from `PeerLoadouts`), not the local one — each
/// peer mutates their own config independently in their per-peer
/// shop, and the broadcast in `loadout::broadcast_turret_config`
/// keeps every peer's view of every OTHER peer up to date.
///
/// Static visuals only — bullets fired from the remote peer arrive
/// as `BulletFiredEvent` signals and are spawned by
/// `spawn_received_bullets` at the correct world position.
fn spawn_remote_turret_visuals(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    pm: &crate::palette::PaletteMaterials,
    cfg: &crate::turret::TurretConfig,
    ship: Entity,
) {
    use crate::balance::{TURRET_POSITIONS, TURRET_MOUNTS};

    // Same primitives as the local ship's turrets.
    let base_mesh   = meshes.add(Circle::new(2.0));
    let barrel_mesh = meshes.add(Rectangle::new(1.5, 4.0));

    for (i, &(lx, ly)) in TURRET_POSITIONS.iter().enumerate() {
        let slot = cfg.slots[i];
        if !slot.equipped { continue; }
        let mount = TURRET_MOUNTS[i];
        let turret_mat = pm.turret_for(slot.weapon).clone();

        let base = commands.spawn((
            Mesh2d(base_mesh.clone()),
            MeshMaterial2d(turret_mat.clone()),
            Transform::from_xyz(lx, ly, 2.0)
                .with_rotation(Quat::from_rotation_z(mount)),
            Visibility::Inherited,
            RenderLayers::layer(PLAY_LAYER),
            GhostTurretChild { slot_index: i },
        )).id();
        commands.entity(base).insert(ChildOf(ship));

        let barrel = commands.spawn((
            Mesh2d(barrel_mesh.clone()),
            MeshMaterial2d(turret_mat),
            Transform::from_xyz(0.0, 1.8, 0.15),
            Visibility::Inherited,
            RenderLayers::layer(PLAY_LAYER),
            // Barrel inherits its parent base's rotation via the
            // transform hierarchy; `apply_ghost_turret_aims` only
            // touches bases. Still tag with the slot index so
            // `refresh_ghost_turrets`' despawn-by-marker query
            // catches it.
            GhostTurretChild { slot_index: i },
        )).id();
        commands.entity(barrel).insert(ChildOf(base));
    }
}

/// Apply the latest `PeerSnapshot` to each ghost entity's Transform
/// via exponential-decay lerp. At 30Hz Transform packets the raw
/// position snaps would be visibly jerky (~33ms between updates);
/// lerping each frame at `SMOOTH_PER_FRAME = 0.35` reaches ~99% of
/// the target in ~10 frames (~165ms at 60fps), which the eye reads
/// as smooth motion without losing responsiveness when the peer
/// changes direction.
///
/// Rotation uses spherical lerp (`slerp`) so heading interpolation
/// takes the short path around the unit circle.
pub fn apply_snapshots(
    time: Res<Time>,
    snapshots: Res<PeerSnapshots>,
    mut ghosts: Query<(&RemoteGhost, &mut Transform)>,
) {
    // Frame-rate independent exp-decay: same visual speed at 30fps
    // and 144fps. `BASE_PER_FRAME` is "fraction per 60fps frame";
    // we scale by `delta * 60` so a 144fps frame moves a smaller
    // fraction (catches up the same total per second).
    const BASE_PER_FRAME: f32 = 0.35;
    let t = (time.delta_secs() * 60.0 * BASE_PER_FRAME).clamp(0.0, 1.0);

    for (ghost, mut tf) in &mut ghosts {
        if let Some(snap) = snapshots.0.get(&ghost.id) {
            tf.translation.x = lerp_f32(tf.translation.x, snap.pos.x, t);
            tf.translation.y = lerp_f32(tf.translation.y, snap.pos.y, t);
            let target_rot = Quat::from_rotation_z(snap.rot);
            tf.rotation = tf.rotation.slerp(target_rot, t);
        }
    }
}

#[inline]
fn lerp_f32(a: f32, b: f32, t: f32) -> f32 { a + (b - a) * t }

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

/// Per-frame: scan `NetSession.last_seen` and drop peers that have
/// gone silent for longer than [`super::PEER_TIMEOUT_SECS`]. Same
/// outcome as receiving a `Bye` from them, just driven by silence
/// instead of an explicit packet. Catches the hard-kill /
/// network-drop case where no Bye is ever sent.
///
/// On host: removes the stale peer from `session.peers` + `roster`
/// and broadcasts `PeerLeft` to remaining peers so their UI updates.
/// On client: if the host (id 0) is stale, sets `pending_kick` with
/// a "host timed out" reason so `handle_received_kick` tears the
/// session down via the existing path.
pub fn detect_stale_peers(
    mut session: Option<ResMut<NetSession>>,
    mut roster: ResMut<super::LobbyRoster>,
    mut pending_kick: ResMut<super::PendingKick>,
    mode: Res<NetMode>,
) {
    if matches!(*mode, NetMode::Solo) { return; }
    let Some(session) = session.as_mut() else { return; };
    if !session.welcomed { return; }

    let now = Instant::now();
    let stale_ids: Vec<u8> = session.last_seen.iter()
        .filter(|(_, &t)| now.duration_since(t).as_secs_f32() > super::PEER_TIMEOUT_SECS)
        .map(|(&id, _)| id)
        .collect();
    if stale_ids.is_empty() { return; }

    for &id in &stale_ids {
        if session.is_host {
            // Host: drop the peer from our world + tell everyone else.
            session.peers.remove(&id);
            session.last_seen.remove(&id);
            roster.by_id.remove(&id);
            let announce = NetMsg::PeerLeft { id };
            for &peer_addr in session.peers.values() {
                let _ = send_to(&session.sock, peer_addr, &announce);
            }
            bevy::log::info!("multiplayer: peer {id} timed out (no packets for >{}s)",
                super::PEER_TIMEOUT_SECS);
        } else if id == 0 {
            // Client: host went silent. Tear down via the existing
            // kicked-by-host UI path so the player sees a clear
            // reason instead of a stuck waiting screen.
            pending_kick.0 = Some("host timed out".to_string());
            bevy::log::info!("multiplayer: host timed out (no packets for >{}s)",
                super::PEER_TIMEOUT_SECS);
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

#[cfg(test)]
mod tests {
    use super::*;

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
                turret_rots: [0.0; 8],
                last_seen: Instant::now(),
            },
        );
        assert_eq!(snaps.0.len(), 1);
        let s = snaps.0.get(&1).unwrap();
        assert_eq!(s.pos.x, 10.0);
        assert_eq!(s.rot, 0.5);
    }
}
