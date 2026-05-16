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
        (&Transform, &Bullet, &Velocity),
        (Added<Bullet>, Without<RemoteVisualBullet>),
    >,
) {
    // Only emit when we're in a multiplayer session — saves work in
    // single-player where the events would have no consumer.
    if matches!(*mode, NetMode::Solo) {
        // Drain the iterator anyway via `for _ in &new_bullets` is
        // not necessary — events accumulate only on emit. Just return.
        return;
    }
    for (tf, bullet, vel) in &new_bullets {
        // Only replicate friendly fire (our own player's bullets).
        // Enemy bullets are simulated by the host (authoritative on
        // enemies); replicating them too would double up.
        if bullet.faction != FactionKind::Friendly { continue; }
        let pos = tf.translation.truncate();
        let dir = vel.0.normalize_or_zero();
        if dir.length_squared() < 0.001 { continue; }
        writer.write(BulletFiredEvent {
            pos,
            dir,
            weapon: bullet.weapon.to_u8(),
            range:  bullet.remaining,
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
) {
    if matches!(*mode, NetMode::Solo) { inbox.events.clear(); return; }
    let (Some(pm), Some(em)) = (pm, em) else { inbox.events.clear(); return };

    for ev in inbox.events.drain(..) {
        let Some(weapon) = WeaponType::from_u8(ev.weapon) else { continue };
        let outer = pm.bullet_friendly_outer.clone();
        let inner = pm.bullet_friendly.clone();
        // spawn_combat_bullet doesn't return the entity id, so we
        // can't attach RemoteVisualBullet after the fact. Use the
        // raw bullet-spawn shape inline to mark it.
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
        // Inner bright-core child — same as production combat bullets.
        let inner_e = commands.spawn((
            Mesh2d(em.bullet_friendly_inner.clone()),
            MeshMaterial2d(inner),
            Transform::from_xyz(0.0, 0.0, 0.05),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(inner_e).insert(ChildOf(bullet));
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
        };
        // Just exercise field access — Event derive doesn't add
        // serde, the wire format goes through NetMsg::BulletFired.
        assert_eq!(ev.weapon, 1);
        assert!((ev.range - 100.0).abs() < f32::EPSILON);
    }
}
