//! Signal-driven bullet replication. When the local player fires a
//! turret, a `BulletFiredEvent` is emitted (see
//! `emit_bullet_fired_signals`). `send_bullet_fired` packetises it
//! and broadcasts; receivers spawn a damage=0 visual replica via
//! `spawn_received_bullets`.
//!
//! Why signal-based rather than running a fake turret AI on each
//! peer's view of the remote ship: timing parity. The firing peer
//! is the source of truth for "I fired a bullet right now in this
//! direction." Other peers replay that exact moment instead of
//! drifting on independent RNG / fire-rate clocks.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;

use crate::balance::PLAY_LAYER;
use crate::bullet::Bullet;
use crate::components::{Faction, FactionKind, Velocity};
use crate::proc_fx::BulletFiredEvent;
use crate::weapon::WeaponType;

use super::net::{send_to, NetMsg};
use super::{NetMode, NetSession};

/// Marker on bullets spawned from a received `BulletFired` packet.
/// Two roles:
/// 1. `emit_bullet_fired_signals` excludes them so received bullets
///    don't re-emit a signal and infinite-loop.
/// 2. Visual-only — `damage = 0` already keeps `push_initial` from
///    queueing damage; this tag is just for debugging / inspection.
#[derive(Component, Clone, Copy, Debug)]
pub struct RemoteVisualBullet;

/// Per-frame: detect newly-spawned bullets that belong to the local
/// player and emit `BulletFiredEvent`. `Added<Bullet>` fires once
/// per bullet, the frame it's spawned. We exclude already-tagged
/// `RemoteVisualBullet` so received-bullet spawns don't re-emit.
///
/// Decoupled from the turret-firing code so any future firing path
/// (mortar, spread rockets, plasma torpedo, …) replicates without
/// new wiring.
pub fn emit_bullet_fired_signals(
    mode: Res<NetMode>,
    mut writer: EventWriter<BulletFiredEvent>,
    new_bullets: Query<
        (&Transform, &Bullet, &Velocity, Option<&crate::ally::HomingMissile>),
        (Added<Bullet>, Without<RemoteVisualBullet>),
    >,
    // Look up the target's `NetEntityId` so the receiver can
    // re-attach a `HomingMissile` to its visual replica and curve
    // identically. Bevy 0.16's `Query::get` returns Result.
    net_ids: Query<&crate::multiplayer::enemies::NetEntityId>,
) {
    // Only emit when we're in a multiplayer session — saves work in
    // single-player where the events would have no consumer.
    if matches!(*mode, NetMode::Solo) {
        return;
    }
    for (tf, bullet, vel, homing) in &new_bullets {
        // Only replicate friendly fire (our own player's bullets).
        // Enemy bullets are simulated by the host (authoritative on
        // enemies); replicating them too would double up.
        if bullet.faction != FactionKind::Friendly { continue; }
        let pos = tf.translation.truncate();
        let dir = vel.0.normalize_or_zero();
        if dir.length_squared() < 0.001 { continue; }
        // For homing missiles, look up the target's NetEntityId
        // (only enemies on the host side have one — client mirrors
        // also have one because `apply_enemy_snapshot` carries it
        // through). 0 = sentinel "no target / not a missile."
        let target_net_id = homing
            .and_then(|h| h.target)
            .and_then(|e| net_ids.get(e).ok())
            .map(|id| id.0)
            .unwrap_or(0);
        writer.write(BulletFiredEvent {
            pos,
            dir,
            weapon: bullet.weapon.to_u8(),
            range:  bullet.remaining,
            target_net_id,
        });
    }
}

/// Per-frame: drain `BulletFiredEvent`s and send a `BulletFired`
/// packet for each. Same shape as `send_proc_fx`: clients send to
/// host (id 0), host blasts to every peer. Host's re-broadcast to
/// other peers happens via `relay_bullet_fired_to_peers` so a
/// 3-player session works.
pub fn send_bullet_fired(
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    mut events: EventReader<BulletFiredEvent>,
) {
    if matches!(*mode, NetMode::Solo) { events.read().for_each(drop); return; }
    let Some(session) = session else {
        events.read().for_each(drop);
        return;
    };
    for ev in events.read() {
        let msg = NetMsg::BulletFired {
            pos:    [ev.pos.x, ev.pos.y],
            dir:    [ev.dir.x, ev.dir.y],
            weapon: ev.weapon,
            range:  ev.range,
            target_net_id: ev.target_net_id,
        };
        if session.is_host {
            for &addr in session.peers.values() {
                let _ = send_to(&session.sock, addr, &msg);
            }
        } else {
            if let Some(&host_addr) = session.peers.get(&0) {
                let _ = send_to(&session.sock, host_addr, &msg);
            }
        }
    }
}

/// Inbox of received `BulletFired` packets. Populated by
/// `recv_packets`; drained by `spawn_received_bullets`.
#[derive(Resource, Default)]
pub struct BulletFiredInbox {
    pub events: Vec<ReceivedBulletFired>,
}

/// One queued bullet-fire packet awaiting either local visual spawn
/// or host-side rebroadcast.
#[derive(Clone, Copy, Debug)]
pub struct ReceivedBulletFired {
    pub pos:    Vec2,
    pub dir:    Vec2,
    pub weapon: u8,
    pub range:  f32,
    /// Sender address — host uses this to skip the originator on
    /// re-broadcast (don't echo back).
    pub sender_addr: std::net::SocketAddr,
    /// Target `NetEntityId.0` for homing missiles; `0` = none.
    pub target_net_id: u32,
}

/// Host: re-broadcast incoming `BulletFired` packets to every peer
/// except the sender. Lets a 3-player session see each peer's
/// bullets correctly. Iterates without draining;
/// `spawn_received_bullets` is responsible for the drain.
pub fn relay_bullet_fired(
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    inbox: Res<BulletFiredInbox>,
) {
    if matches!(*mode, NetMode::Solo) { return; }
    let Some(session) = session else { return };
    if !session.is_host { return; }

    for ev in inbox.events.iter() {
        let msg = NetMsg::BulletFired {
            pos:    [ev.pos.x, ev.pos.y],
            dir:    [ev.dir.x, ev.dir.y],
            weapon: ev.weapon,
            range:  ev.range,
            target_net_id: ev.target_net_id,
        };
        for (&_peer_id, &addr) in session.peers.iter() {
            if addr == ev.sender_addr { continue; }
            let _ = send_to(&session.sock, addr, &msg);
        }
    }
}

/// Drain `BulletFiredInbox` and spawn a damage=0 visual bullet for
/// each entry. Uses the production `spawn_combat_bullet` helper so
/// the bullet looks identical to a real shot. The `RemoteVisualBullet`
/// tag prevents `emit_bullet_fired_signals` from re-replicating it.
pub fn spawn_received_bullets(
    mut commands: Commands,
    mode: Res<NetMode>,
    pm: Option<Res<crate::palette::PaletteMaterials>>,
    em: Option<Res<crate::effects::EffectMeshes>>,
    mut inbox: ResMut<BulletFiredInbox>,
    // Look up the local mirror entity for the missile's target so
    // we can attach a `HomingMissile` to the visual bullet — it
    // curves locally toward the same enemy the owning peer's
    // missile is curving toward.
    net_id_lookup: Query<(Entity, &crate::multiplayer::enemies::NetEntityId)>,
) {
    if matches!(*mode, NetMode::Solo) { inbox.events.clear(); return; }
    let (Some(pm), Some(em)) = (pm, em) else { inbox.events.clear(); return };

    for ev in inbox.events.drain(..) {
        let Some(weapon) = WeaponType::from_u8(ev.weapon) else { continue };
        // Weapon-specific materials — Sniper bullets should LOOK
        // like sniper bullets on the peer's screen, not like a
        // generic Standard round. `bullet_outer_for` / `bullet_for`
        // are the same lookups the local turret-fire path uses.
        let outer = pm.bullet_outer_for(weapon).clone();
        let inner = pm.bullet_inner_for(weapon).clone();
        let bullet = commands.spawn((
            Mesh2d(em.bullet_friendly_outer.clone()),
            MeshMaterial2d(outer),
            Transform::from_xyz(ev.pos.x, ev.pos.y, 4.0)
                .with_rotation(Quat::from_rotation_z((-ev.dir.x).atan2(ev.dir.y))),
            Bullet {
                faction: FactionKind::Friendly,
                damage:  0, // visual only — `push_initial` bails on 0
                remaining: ev.range,
                weapon,
                source: None,
                runes: Vec::new(),
            },
            Velocity(ev.dir.normalize_or_zero() * crate::balance::BULLET_SPEED),
            Faction(FactionKind::Friendly),
            RenderLayers::layer(PLAY_LAYER),
            RemoteVisualBullet,
        )).id();
        let inner_e = commands.spawn((
            Mesh2d(em.bullet_friendly_inner.clone()),
            MeshMaterial2d(inner),
            Transform::from_xyz(0.0, 0.0, 0.05),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(inner_e).insert(ChildOf(bullet));

        // Homing missile: look up the local mirror with the same
        // NetEntityId and attach a `HomingMissile` so the visual
        // bullet curves toward the same enemy on this peer's side.
        // The local `homing_missile_track` system handles the
        // per-frame turn; damage stays 0 (visual only).
        if ev.target_net_id != 0 {
            let mut target_entity: Option<Entity> = None;
            for (e, nid) in &net_id_lookup {
                if nid.0 == ev.target_net_id { target_entity = Some(e); break; }
            }
            commands.entity(bullet).insert(crate::ally::HomingMissile {
                target: target_entity,
                turn_rate: crate::ally::missile::MISSILE_TURN_RATE,
                target_faction: FactionKind::Enemy,
                homing_delay: 0.3,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bullet_fired_event_round_trip() {
        let ev = BulletFiredEvent {
            pos: Vec2::new(10.0, 20.0),
            dir: Vec2::new(1.0, 0.0),
            weapon: WeaponType::Sniper.to_u8(),
            range: 100.0,
            target_net_id: 0,
        };
        // Just exercise field access — Event derive doesn't add
        // serde, the wire format goes through NetMsg::BulletFired.
        assert_eq!(ev.weapon, 1);
        assert!((ev.range - 100.0).abs() < f32::EPSILON);
    }
}
