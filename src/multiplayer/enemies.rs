//! Host streams enemy snapshots, client mirrors them.
//!
//! Authority model: the **host** runs every enemy spawn / AI / death
//! system unchanged. Every ~50ms it walks the live enemies, packages
//! their stable id + variant + transform + HP into an
//! [`crate::multiplayer::net::EnemySnapshot`] packet, and broadcasts
//! to every connected peer.
//!
//! On the client, the local enemy spawn pipeline is gated off (see
//! `crate::multiplayer::is_client`). Instead, [`apply_enemy_snapshot`]
//! diffs the incoming entries against a local map of mirror entities
//! by `NetEntityId`:
//! - id present in snapshot, no mirror yet → spawn one
//! - id present, mirror exists → update transform + HP
//! - mirror exists, id absent → despawn (enemy died on host)
//!
//! Mirror entities are deliberately dumb — body mesh + `Enemy` tag +
//! `Health` + `Faction(Enemy)` + `NetEntityId`. **No `Velocity`** (so
//! `apply_velocity` skips them), **no per-kind AI components** (so
//! none of the AI systems write to them). All motion comes from the
//! next snapshot.

use std::collections::HashMap;

use bevy::prelude::*;
use bevy::render::view::RenderLayers;

use crate::balance::{ENEMY_LEN, ENEMY_WIDTH, PLAY_LAYER};
use crate::components::{Faction, FactionKind, Health};
use crate::effects::EffectMeshes;
use crate::enemy::{Enemy, EnemyState, EnemyVariant};
use crate::palette::PaletteMaterials;

use super::net::{send_to, EnemyEntry, NetMsg};
use super::{NetMode, NetSession};

/// How often the host broadcasts a full enemy snapshot. 20Hz keeps
/// motion smooth without saturating the network — at ~30 enemies
/// each at ~24 bytes serialized, a packet is ~720 bytes plus
/// framing, well under MTU.
pub const ENEMY_SNAPSHOT_HZ: f32 = 20.0;
const ENEMY_SNAPSHOT_INTERVAL: f32 = 1.0 / ENEMY_SNAPSHOT_HZ;

/// Stable network id for a replicated enemy. Assigned on the host by
/// [`assign_net_ids`] the first frame an enemy is spawned. Client
/// mirrors carry the same id so subsequent snapshots route to the
/// right entity.
#[derive(Component, Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NetEntityId(pub u32);

/// Host-only monotonic counter for the next id to hand out. Starts at
/// 1 so 0 is reserved as "unassigned" if we ever need a sentinel.
#[derive(Resource, Default)]
pub struct NextNetEntityId(pub u32);

/// Host-side throttle timer for the snapshot broadcast.
#[derive(Resource, Default)]
pub struct EnemySnapshotTimer(pub f32);

/// Per-frame on the host (only while connected): every enemy without
/// a `NetEntityId` gets the next id from `NextNetEntityId`. Idempotent
/// because the filter `Without<NetEntityId>` skips already-tagged
/// enemies on subsequent ticks.
pub fn assign_net_ids(
    mut commands: Commands,
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    mut next_id: ResMut<NextNetEntityId>,
    fresh: Query<Entity, (With<Enemy>, Without<NetEntityId>)>,
) {
    if !is_host_connected(&mode, session.as_deref()) { return; }
    for e in &fresh {
        next_id.0 += 1;
        commands.entity(e).insert(NetEntityId(next_id.0));
    }
}

/// Host: build an `EnemySnapshot` from every tagged enemy, broadcast
/// to every peer. Runs at `ENEMY_SNAPSHOT_HZ`. Reads the proc-status
/// components via `Has<...>` filters (Bevy 0.13+) and packs them into
/// `status_flags` per [`status_bits`].
pub fn send_enemy_snapshot(
    time: Res<Time>,
    mut timer: ResMut<EnemySnapshotTimer>,
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    enemies: Query<(
        &NetEntityId,
        &Enemy,
        &Transform,
        &Health,
        bevy::ecs::query::Has<crate::rune::OnFire>,
        bevy::ecs::query::Has<crate::rune::OnFrost>,
        bevy::ecs::query::Has<crate::rune::OnBleed>,
        Option<&crate::ally::Ally>,
    )>,
) {
    if !is_host_connected(&mode, session.as_deref()) { return; }
    timer.0 += time.delta_secs();
    if timer.0 < ENEMY_SNAPSHOT_INTERVAL { return; }
    timer.0 = 0.0;

    let Some(session) = session else { return; };
    let entries: Vec<EnemyEntry> = enemies
        .iter()
        .map(|(id, en, tf, hp, on_fire, on_frost, on_bleed, ally)| {
            let mut flags = 0u8;
            if on_fire  { flags |= status_bits::ON_FIRE;  }
            if on_frost { flags |= status_bits::ON_FROST; }
            if on_bleed { flags |= status_bits::ON_BLEED; }
            // Boss detection: a hostile entity that ALSO carries the
            // `Ally` component is a boss (see `ally::spawn_boss`).
            // Carries the ShipClass so client can build the right
            // boss-tier visuals on first sight.
            let boss_class = ally
                .map(|a| a.class.to_u8())
                .unwrap_or(super::net::NOT_A_BOSS);
            EnemyEntry {
                id: id.0,
                kind: en.variant.to_u8(),
                pos: [tf.translation.x, tf.translation.y],
                rot: tf.rotation.to_euler(EulerRot::ZYX).0,
                hp: hp.0,
                status_flags: flags,
                boss_class,
            }
        })
        .collect();

    let msg = NetMsg::EnemySnapshot { entries };
    for &addr in session.peers.values() {
        if let Err(e) = send_to(&session.sock, addr, &msg) {
            bevy::log::warn!("multiplayer: failed to send EnemySnapshot to {addr}: {e}");
        }
    }
}

/// Latest received snapshot on the client side, drained by the
/// gameplay netloop into here so `apply_enemy_snapshot` can read it
/// without re-doing socket I/O. Wiped after each apply.
#[derive(Resource, Default)]
pub struct LatestEnemySnapshot(pub Option<Vec<EnemyEntry>>);

// ---------- Status bitmask (stateful procs) ----------

/// Bit positions in `EnemyEntry::status_flags`. Stateful proc
/// components live as bits in the snapshot so client mirrors can
/// add/remove the matching components and let the existing tick
/// systems (`tick_on_fire`, etc.) drive local visuals + DOT damage.
pub mod status_bits {
    pub const ON_FIRE:  u8 = 1 << 0;
    pub const ON_FROST: u8 = 1 << 1;
    pub const ON_BLEED: u8 = 1 << 2;
}

// ---------- ProcFx (transient effect broadcast) ----------

/// One transient-effect packet received by the local netloop,
/// awaiting either local visual spawn or host-side rebroadcast.
#[derive(Clone, Copy, Debug)]
pub struct ReceivedProcFx {
    pub kind: u8,
    pub from: Vec2,
    pub to:   Vec2,
    /// Source `SocketAddr` of this packet. Host uses it to skip the
    /// originator when re-broadcasting (don't echo back to sender).
    pub sender_addr: std::net::SocketAddr,
}

/// Inbox of received `ProcFx` packets. Drained each frame by the
/// `spawn_proc_fx_visuals` system on the consume side and
/// `relay_proc_fx_to_peers` on the host side.
#[derive(Resource, Default)]
pub struct ProcFxInbox {
    pub events: Vec<ReceivedProcFx>,
}

/// Latest known authoritative pose for a mirror entity. Written by
/// `apply_enemy_snapshot` (every snapshot — 20Hz = every 50ms) and
/// lerped-toward each frame by `smooth_mirror_transforms`. Without
/// the per-frame lerp, mirrors would visibly teleport every 50ms.
#[derive(Component, Clone, Copy, Debug)]
pub struct MirrorTarget {
    pub pos: Vec2,
    pub rot: f32,
}

/// Wire-format discriminants — re-exported from `crate::proc_fx::kind`
/// so existing call sites under the multiplayer module keep working.
/// The canonical home is `proc_fx::kind` (non-multiplayer-gated so
/// gameplay code can reference it without cfg pain).
#[allow(unused_imports)]
pub mod proc_fx_kind {
    pub use crate::proc_fx::kind::*;
}

/// Drain the `ProcFxFired` event channel and send each event over
/// the wire. Clients send to the host (who'll re-broadcast); the
/// host sends directly to every peer. Reading via `EventReader`
/// means gameplay code can fire events without depending on
/// multiplayer state — Bevy auto-drops the events after a couple of
/// frames on single-player builds.
pub fn send_proc_fx(
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    mut events: bevy::ecs::event::EventReader<crate::proc_fx::ProcFxFired>,
) {
    if matches!(*mode, NetMode::Solo) {
        // Still drain so events don't accumulate beyond Bevy's
        // 2-frame retention default — `read()` advances the cursor.
        events.read().for_each(drop);
        return;
    }
    let Some(session) = session else {
        events.read().for_each(drop);
        return;
    };

    for ev in events.read() {
        let msg = NetMsg::ProcFx {
            kind: ev.kind,
            from: [ev.from.x, ev.from.y],
            to:   [ev.to.x,   ev.to.y],
        };
        if session.is_host {
            // Host: blast to every connected client.
            for &addr in session.peers.values() {
                let _ = send_to(&session.sock, addr, &msg);
            }
        } else {
            // Client: send to host (id 0). Host's relay system will
            // fan it out to other clients.
            if let Some(&host_addr) = session.peers.get(&0) {
                let _ = send_to(&session.sock, host_addr, &msg);
            }
        }
    }
}

/// Host: re-broadcast incoming `ProcFx` packets to every peer
/// except the sender. Keeps two clients' transient visuals visible
/// to each other when peer-to-peer broadcast isn't direct. Iterates
/// without draining so `spawn_proc_fx_visuals` (which runs after)
/// can render the same events locally.
pub fn relay_proc_fx_to_peers(
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    inbox: Res<ProcFxInbox>,
) {
    if !is_host_connected(&mode, session.as_deref()) { return; }
    let Some(session) = session else { return; };

    for ev in inbox.events.iter() {
        let msg = NetMsg::ProcFx {
            kind: ev.kind,
            from: [ev.from.x, ev.from.y],
            to:   [ev.to.x,   ev.to.y],
        };
        for (&_peer_id, &addr) in session.peers.iter() {
            if addr == ev.sender_addr { continue; }
            let _ = send_to(&session.sock, addr, &msg);
        }
    }
}

/// Every peer: drain `ProcFxInbox` and spawn the local visual entity
/// for each transient proc that came in over the wire. Mirrors the
/// host-side spawn path used at the original proc site so remote
/// procs look identical to local ones.
pub fn spawn_proc_fx_visuals(
    mut commands: Commands,
    mode: Res<NetMode>,
    pm: Option<Res<crate::palette::PaletteMaterials>>,
    em: Option<Res<crate::effects::EffectMeshes>>,
    mut inbox: ResMut<ProcFxInbox>,
) {
    if matches!(*mode, NetMode::Solo) { inbox.events.clear(); return; }
    let (Some(pm), Some(em)) = (pm, em) else { inbox.events.clear(); return; };

    for ev in inbox.events.drain(..) {
        match ev.kind {
            k if k == crate::proc_fx::kind::SHOCK_ARC => {
                crate::bullet::spawn_lightning_arc(
                    &mut commands, &em, &pm.shock, ev.from, ev.to,
                );
            }
            k if k == crate::proc_fx::kind::CASCADE => {
                crate::bullet::spawn_lightning_arc(
                    &mut commands, &em, &pm.bullet_friendly_outer, ev.from, ev.to,
                );
            }
            k if k == crate::proc_fx::kind::BLAST_RING => {
                // `spawn_blast_ring` is private to bullet.rs — call
                // the public re-export when one exists. For now, a
                // placeholder lightning arc at the origin so the
                // event isn't silent.
                crate::bullet::spawn_lightning_arc(
                    &mut commands, &em, &pm.blast, ev.from, ev.from,
                );
            }
            k if k == crate::proc_fx::kind::CONDUIT => {
                let mut rng = rand::thread_rng();
                crate::effects::spawn_hit_particles(
                    &mut commands, &em, &pm.shock, ev.from, 4, 35.0, &mut rng,
                );
            }
            k if k == crate::proc_fx::kind::RESONATE => {
                let mut rng = rand::thread_rng();
                crate::effects::spawn_hit_particles(
                    &mut commands, &em, &pm.bullet_sniper, ev.from, 3, 30.0, &mut rng,
                );
            }
            _ => {
                bevy::log::warn!("multiplayer: unknown ProcFx kind {}", ev.kind);
            }
        }
    }
}

/// Client: spawn missing mirrors / update existing / despawn vanished
/// based on the latest snapshot. Diffs by `NetEntityId`. Also
/// reconciles stateful proc components (`OnFire` / `OnFrost` /
/// `OnBleed`) against the snapshot's `status_flags` so client-side
/// DOT visuals + tick damage stay in sync with the host's truth.
pub fn apply_enemy_snapshot(
    mut commands: Commands,
    mode: Res<NetMode>,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    meshes: Option<ResMut<Assets<Mesh>>>,
    mut latest: ResMut<LatestEnemySnapshot>,
    mut mirrors: Query<(
        Entity,
        &NetEntityId,
        &mut Transform,
        &mut Health,
        bevy::ecs::query::Has<crate::rune::OnFire>,
        bevy::ecs::query::Has<crate::rune::OnFrost>,
        bevy::ecs::query::Has<crate::rune::OnBleed>,
    ), With<Enemy>>,
    mut targets: Query<&mut MirrorTarget>,
) {
    if !is_client_connected(&mode) { return; }
    let Some(entries) = latest.0.take() else { return };
    // PM/EM are only needed to SPAWN new mirror entities — HP /
    // transform / status updates on already-spawned mirrors don't
    // need graphics resources. Headless tests use this split: they
    // pre-spawn mirrors manually and assert on the HP-update path
    // without dragging in the asset stack.
    let graphics = pm.zip(em);
    // `meshes` is also only needed for the boss-mirror spawn path
    // (build_ship_for_faction loads a procedural hull mesh). Keep it
    // optional alongside PM/EM so the headless fixture stays light.
    let mut meshes = meshes;

    // Index existing mirrors by id for O(1) lookup. The tuple holds
    // everything we'll need to mutate per-entry. `tf` is updated
    // *only* for new-mirror inserts (initial spawn position);
    // per-snapshot pose updates are stored in `MirrorTarget` so
    // `smooth_mirror_transforms` can lerp toward them each frame.
    struct MirrorState<'w> {
        entity: Entity,
        tf:     Mut<'w, Transform>,
        hp:     Mut<'w, Health>,
        on_fire:  bool,
        on_frost: bool,
        on_bleed: bool,
    }
    let mut by_id: HashMap<u32, MirrorState<'_>> = HashMap::new();
    for (e, id, tf, hp, on_fire, on_frost, on_bleed) in mirrors.iter_mut() {
        by_id.insert(id.0, MirrorState {
            entity: e, tf, hp, on_fire, on_frost, on_bleed,
        });
    }

    for entry in entries {
        let Some(variant) = EnemyVariant::from_u8(entry.kind) else {
            // Peer running a newer build with a variant we don't know
            // about. Skip silently — the wire format is forward-
            // compatible per `from_u8`.
            continue;
        };
        let want_fire  = entry.status_flags & status_bits::ON_FIRE  != 0;
        let want_frost = entry.status_flags & status_bits::ON_FROST != 0;
        let want_bleed = entry.status_flags & status_bits::ON_BLEED != 0;
        match by_id.remove(&entry.id) {
            Some(mut state) => {
                // Don't snap the transform — write to MirrorTarget
                // and let `smooth_mirror_transforms` lerp each frame.
                // Eliminates the every-50ms pop the snapshot cadence
                // would cause otherwise.
                if let Ok(mut target) = targets.get_mut(state.entity) {
                    target.pos = Vec2::new(entry.pos[0], entry.pos[1]);
                    target.rot = entry.rot;
                }
                if state.hp.0 != entry.hp { state.hp.0 = entry.hp; }
                // Suppress dead-code warning on the unused `tf`
                // binding now that we don't snap directly. The Mut<>
                // borrow still needs to be released for the world.
                let _ = &mut state.tf;
                // Reconcile proc components against the snapshot's
                // bits. Add when missing-but-wanted; remove when
                // present-but-not-wanted. Stacks default to 1 on the
                // mirror — host owns the real stack count for damage,
                // mirror only needs the component for local visuals.
                if want_fire  && !state.on_fire  { commands.entity(state.entity).insert(crate::rune::OnFire::new(1));  }
                if !want_fire &&  state.on_fire  { commands.entity(state.entity).remove::<crate::rune::OnFire>();  }
                if want_frost && !state.on_frost { commands.entity(state.entity).insert(crate::rune::OnFrost::new(1)); }
                if !want_frost &&  state.on_frost { commands.entity(state.entity).remove::<crate::rune::OnFrost>(); }
                if want_bleed && !state.on_bleed { commands.entity(state.entity).insert(crate::rune::OnBleed::new(1)); }
                if !want_bleed &&  state.on_bleed { commands.entity(state.entity).remove::<crate::rune::OnBleed>(); }
            }
            None => {
                // Headless tests run without graphics resources;
                // they pre-spawn mirrors manually so the new-mirror
                // spawn path here is a no-op. Production has PM/EM
                // and uses this path on first sight of each new
                // enemy id.
                let Some((pm, em)) = graphics.as_ref() else { continue };
                // Branch on boss vs regular: bosses use the full
                // `build_ship_for_faction` visuals so the client sees
                // the right ShipClass-specific chassis; regular enemies
                // get the stripped-down variant mesh mirror.
                let boss = (entry.boss_class != super::net::NOT_A_BOSS)
                    .then(|| crate::ally::ShipClass::from_u8(entry.boss_class))
                    .flatten();
                let e = match (boss, meshes.as_mut()) {
                    (Some(class), Some(meshes)) => spawn_boss_mirror(
                        &mut commands,
                        pm,
                        em,
                        meshes,
                        Vec2::new(entry.pos[0], entry.pos[1]),
                        entry.rot,
                        class,
                        entry.hp,
                        entry.id,
                    ),
                    _ => spawn_enemy_mirror(
                        &mut commands,
                        pm,
                        em,
                        Vec2::new(entry.pos[0], entry.pos[1]),
                        entry.rot,
                        variant,
                        entry.hp,
                        entry.id,
                    ),
                };
                // Attach MirrorTarget at the spawn pose so the
                // smoother has a target to lerp toward from frame 1.
                commands.entity(e).insert(MirrorTarget {
                    pos: Vec2::new(entry.pos[0], entry.pos[1]),
                    rot: entry.rot,
                });
                if want_fire  { commands.entity(e).insert(crate::rune::OnFire::new(1));  }
                if want_frost { commands.entity(e).insert(crate::rune::OnFrost::new(1)); }
                if want_bleed { commands.entity(e).insert(crate::rune::OnBleed::new(1)); }
            }
        }
    }

    // Anything left in `by_id` wasn't in the snapshot → enemy died on
    // the host. Despawn its mirror.
    for state in by_id.into_values() {
        commands.entity(state.entity).despawn();
    }
}

/// Spawn a boss mirror on the client using the same chassis builder
/// the host uses. Borrows `build_ship_for_faction` so the client sees
/// the full ShipClass-specific visuals (right hull shape + class
/// decorations + colour) instead of the standard variant placeholder.
///
/// Note: boss AI / turret / faction behaviour doesn't run on the
/// client (no `Without<Ally>` paths fire here), so this is purely a
/// visual + collision-target mirror. HP / transform / status come
/// through the same `apply_enemy_snapshot` path as a regular mirror.
fn spawn_boss_mirror(
    commands: &mut Commands,
    pm: &PaletteMaterials,
    em: &EffectMeshes,
    meshes: &mut Assets<Mesh>,
    pos: Vec2,
    heading: f32,
    class: crate::ally::ShipClass,
    hp: i32,
    id: u32,
) -> Entity {
    let ship = crate::ally::build_ship_for_faction(
        commands, pm, em, meshes, pos, heading, class, FactionKind::Enemy,
    );
    // Layer the marker components the regular mirror has so collision
    // queries (`With<Enemy>`) + the snapshot-reconcile path see this
    // entity. `Ally { class, .. }` is kept so the snapshot's boss
    // detection path also flags the mirror — useful for any future
    // boss-specific rendering tweaks gated by Ally on client too.
    commands.entity(ship).insert((
        Enemy {
            variant: EnemyVariant::Standard,
            state: EnemyState::Approach,
            state_timer: 0.0,
            waypoint: Vec2::ZERO,
            fire_cd: 0.0,
            max_hp: hp.max(1),
        },
        crate::ally::Ally {
            class,
            waypoint: Vec2::ZERO,
            waypoint_timer: 0.0,
        },
        Health(hp),
        Faction(FactionKind::Enemy),
        NetEntityId(id),
    ));
    ship
}

/// Spawn a stripped-down "mirror" enemy on the client. Only the body
/// mesh + tags needed for collision and rendering — no turret child,
/// no warhead, no trail, no Velocity, no AI components. Its
/// Transform + Health are driven entirely by snapshots. Returns the
/// spawned entity so the caller can attach further components (e.g.
/// initial proc-status components from the same snapshot entry).
fn spawn_enemy_mirror(
    commands: &mut Commands,
    pm: &PaletteMaterials,
    em: &EffectMeshes,
    pos: Vec2,
    heading: f32,
    variant: EnemyVariant,
    hp: i32,
    id: u32,
) -> Entity {
    let body_mat = match variant {
        EnemyVariant::Standard  => pm.enemy.clone(),
        EnemyVariant::Heavy     => pm.enemy_heavy.clone(),
        EnemyVariant::Scout     => pm.enemy_scout.clone(),
        EnemyVariant::Bomber    => pm.enemy_accent.clone(),
        EnemyVariant::Rammer    => pm.enemy_rammer.clone(),
        EnemyVariant::Sniper    => pm.enemy_sniper.clone(),
        EnemyVariant::Artillery => pm.enemy_artillery.clone(),
    };
    let scale = variant.scale();
    let id_entity = commands.spawn((
        Mesh2d(em.enemy_body.clone()),
        MeshMaterial2d(body_mat.clone()),
        Transform::from_xyz(pos.x, pos.y, 1.0)
            .with_rotation(Quat::from_rotation_z(heading))
            .with_scale(Vec3::splat(scale)),
        // Carry the same Enemy struct as a real enemy so the existing
        // bullet-collision query (`With<Enemy>`) matches. AI fields
        // are present but never written — no AI system runs on the
        // client (gated to not-is_client in main.rs).
        Enemy {
            variant,
            state: EnemyState::Approach,
            state_timer: 0.0,
            waypoint: Vec2::ZERO,
            fire_cd: 0.0,
            max_hp: hp.max(1),
        },
        Health(hp),
        // PreviousHp lets the local `track_enemy_damage_for_hp_bars`
        // system spawn / refresh the HP-bar overlay when the mirror's
        // Health drops via snapshot.
        crate::enemy::PreviousHp(hp),
        Faction(FactionKind::Enemy),
        NetEntityId(id),
        // HitFx drives the white-flash visual on damage.
        crate::effects::HitFx::new(body_mat.clone()).with_rest_scale(scale),
        // Velocity is REQUIRED by `bullet_collisions`' enemies query
        // (`&mut Velocity`). Without it the mirror is invisible to
        // friendly bullets — collisions silently miss → no damage
        // events → `relay_damage_to_host` has nothing to relay →
        // client peer can't damage anything. Initial value zero;
        // never mutated on the client (enemy_ai is host-gated).
        crate::components::Velocity(Vec2::ZERO),
        // Heading is read by spike-plate damage reduction even though
        // it's an enemy entity in some queries. Cheap to include.
        crate::components::Heading(heading),
        // Tight bounding box matches the body footprint so client-side
        // bullet collisions detect the mirror.
        crate::rune::FireExtent(Vec2::new(
            ENEMY_WIDTH * 0.5 * scale,
            ENEMY_LEN * 0.5 * scale,
        )),
        RenderLayers::layer(PLAY_LAYER),
    )).id();

    // Variant-specific turret children — same hierarchy as
    // `spawn_enemy` in `enemy/mod.rs`. Gives the mirror the right
    // silhouette (gun barrel) instead of just the bare body.
    // Sniper's `SniperTurret` marker is NOT added here — the
    // independent-rotation aim logic only runs on the host (where
    // the real Sniper lives) and would conflict with the snapshot-
    // driven transform if it ran on the mirror.
    if variant == EnemyVariant::Sniper || variant.has_gun() {
        let base = commands.spawn((
            Mesh2d(em.enemy_turret_base.clone()),
            MeshMaterial2d(pm.enemy_accent.clone()),
            Transform::from_xyz(0.0, 0.0, 0.1),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(base).insert(ChildOf(id_entity));

        let barrel = commands.spawn((
            Mesh2d(em.enemy_turret_barrel.clone()),
            MeshMaterial2d(pm.enemy_accent.clone()),
            Transform::from_xyz(0.0, 1.8, 0.15),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(barrel).insert(ChildOf(base));
    }

    // Bomber: bright warhead at the bow — same visual telegraph as
    // the host-side spawn. Without this the mirror reads as a
    // generic enemy and the player can't tell it's a ram threat.
    if variant == EnemyVariant::Bomber {
        let warhead = commands.spawn((
            Mesh2d(em.bomber_warhead.clone()),
            MeshMaterial2d(pm.bullet_enemy.clone()),
            Transform::from_xyz(0.0, ENEMY_LEN / 2.0 - 1.0, 0.2),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(warhead).insert(ChildOf(id_entity));
    }

    // Rammer: smaller warhead, same mesh shrunk down.
    if variant == EnemyVariant::Rammer {
        let warhead = commands.spawn((
            Mesh2d(em.bomber_warhead.clone()),
            MeshMaterial2d(pm.bullet_enemy.clone()),
            Transform::from_xyz(0.0, ENEMY_LEN / 2.0 - 1.5, 0.2)
                .with_scale(Vec3::splat(0.6)),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(warhead).insert(ChildOf(id_entity));
    }

    id_entity
}

// ---------- Damage relay ----------

/// Host-side inbox for client → host damage events. `recv_packets`
/// pushes one entry per incoming [`NetMsg::DamageEnemy`]; the
/// `apply_relayed_damage` system drains it each frame, looks up the
/// target enemy by `NetEntityId`, and pushes onto the existing
/// `PendingDamageQueue` so the normal damage pipeline runs.
///
/// Lives as a resource so the recv side doesn't need a Bevy event
/// system — events would also work but the buffer-resource pattern
/// is consistent with `LatestEnemySnapshot` / `PeerSnapshots`.
#[derive(Resource, Default)]
pub struct PendingDamageRelay {
    pub entries: Vec<RelayedDamage>,
}

/// One queued client → host damage event, awaiting application.
/// Carries the weapon + rune list so the host re-runs the full
/// proc pipeline, not just base damage.
#[derive(Clone, Debug)]
pub struct RelayedDamage {
    pub enemy_id: u32,
    pub amount:   i32,
    pub hit_pos:  Vec2,
    pub weapon:   crate::weapon::WeaponType,
    pub runes:    Vec<crate::rune::Rune>,
}

/// Client → Host: for each event in the local `PendingDamageQueue`
/// that targets a mirror enemy (has `NetEntityId`), send a
/// `DamageEnemy` packet to the host AND **clear the event's runes**
/// in place. The event stays in the queue so `process_damage_events`
/// runs it locally for visual feedback (hit FX, hit flash, HP-bar
/// flash, particles) — but with empty runes, no procs roll on the
/// client side. The host is the authoritative side for procs: it
/// receives the relayed event with the full rune list, rolls procs,
/// applies chain damage, and broadcasts `ProcFx` visuals back via
/// the existing channel.
///
/// Without the rune-clear, client + host both roll procs for the
/// same hit → chain targets take damage twice, visuals double up.
/// The fix is single-sourced authority: host owns proc rolling.
pub fn relay_damage_to_host(
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    mut queue: ResMut<crate::bullet::PendingDamageQueue>,
    mirrors: Query<&NetEntityId, With<crate::enemy::Enemy>>,
) {
    if !is_client_connected(&mode) { return; }
    let Some(session) = session else { return };
    let host_addr = match session.peers.get(&0) {
        Some(addr) => *addr,
        None => return,
    };

    for ev in queue.0.iter_mut() {
        let Ok(net_id) = mirrors.get(ev.target) else { continue };

        // Only relay INITIAL hits — `procced.is_empty()`. Events
        // with non-empty `procced` are themselves the result of a
        // local proc roll (chain damage etc.). Since procs shouldn't
        // run on the client anymore (we clear runes below), this
        // should be empty in practice; the check is a safety net
        // for any future proc path that pre-populates `procced`.
        if !ev.procced.is_empty() {
            // Local-only proc damage that snuck in — drop it on the
            // client side so it doesn't apply phantom damage to the
            // mirror that the snapshot is about to overwrite.
            ev.amount = 0;
            ev.runes.clear();
            continue;
        }

        let runes_u8: Vec<u8> = ev.runes.iter().map(|r| r.to_u8()).collect();
        let msg = NetMsg::DamageEnemy {
            enemy_id: net_id.0,
            amount:   ev.amount,
            hit_pos:  [ev.hit_pos.x, ev.hit_pos.y],
            weapon:   ev.weapon.to_u8(),
            runes:    runes_u8,
        };
        if let Err(e) = send_to(&session.sock, host_addr, &msg) {
            bevy::log::warn!("multiplayer: failed to relay damage: {e}");
        }
        // Clear runes locally so `process_damage_event` runs visuals
        // (HitFx, sparks, HP-bar flash) but doesn't double-roll
        // procs. The host re-rolls them authoritatively from the
        // relayed runes; visuals propagate back via `ProcFx`.
        ev.runes.clear();
    }
}

/// Host: drain `PendingDamageRelay` and push each entry into the
/// real `PendingDamageQueue` so the normal damage pipeline runs.
/// Looks up the target entity by `NetEntityId`; silently drops
/// events for ids the host doesn't know about (the enemy died on
/// host between the client firing and the packet arriving — a few
/// ms of jitter is enough to produce these).
pub fn apply_relayed_damage(
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    mut buffer: ResMut<PendingDamageRelay>,
    mut queue: ResMut<crate::bullet::PendingDamageQueue>,
    enemies: Query<(Entity, &NetEntityId), With<crate::enemy::Enemy>>,
) {
    if !is_host_connected(&mode, session.as_deref()) { return; }
    if buffer.entries.is_empty() { return; }

    // Build an id → Entity lookup once per frame.
    let by_id: std::collections::HashMap<u32, Entity> = enemies
        .iter()
        .map(|(e, id)| (id.0, e))
        .collect();

    for relayed in buffer.entries.drain(..) {
        if let Some(&target) = by_id.get(&relayed.enemy_id) {
            // Host re-runs the full damage pipeline with the
            // client's weapon + runes. Procs (OnFire / OnFrost /
            // OnBleed / OnConduit / OnResonate / Shock chain /
            // Cascade) all roll authoritatively here, so the next
            // EnemySnapshot carries the resulting status bits and
            // HP deltas back to every peer.
            queue.push_initial(
                target,
                relayed.amount,
                relayed.hit_pos,
                relayed.weapon,
                None,
                &relayed.runes,
            );
        }
    }
}

/// Per-frame: lerp each mirror's `Transform` toward its
/// `MirrorTarget`. Frame-rate independent exp decay so the visual
/// speed is the same at 30fps and 144fps. Eliminates the every-50ms
/// pop the raw snapshot cadence would otherwise cause.
///
/// Tuned to reach ~95% of the target in ~80ms — fast enough that
/// the visual stays close to the host's authoritative pose, slow
/// enough that direction changes don't snap.
pub fn smooth_mirror_transforms(
    time: Res<Time>,
    mut q: Query<(&mut Transform, &MirrorTarget)>,
) {
    // `BASE_PER_FRAME` is the fraction per 60fps frame; we scale by
    // `delta * 60` so the visual speed is fps-independent.
    const BASE_PER_FRAME: f32 = 0.45;
    let t = (time.delta_secs() * 60.0 * BASE_PER_FRAME).clamp(0.0, 1.0);
    for (mut tf, target) in &mut q {
        tf.translation.x += (target.pos.x - tf.translation.x) * t;
        tf.translation.y += (target.pos.y - tf.translation.y) * t;
        let target_rot = Quat::from_rotation_z(target.rot);
        tf.rotation = tf.rotation.slerp(target_rot, t);
    }
}

/// Despawn every mirror on `OnExit(Playing)` and reset the snapshot
/// buffer. Pairs with `despawn_all_ghosts` so leaving gameplay tears
/// down both halves of the multiplayer chrome at once.
pub fn despawn_all_mirrors(
    mut commands: Commands,
    mut latest: ResMut<LatestEnemySnapshot>,
    mut next_id: ResMut<NextNetEntityId>,
    mirrors: Query<Entity, (With<Enemy>, With<NetEntityId>)>,
) {
    for e in &mirrors {
        commands.entity(e).despawn();
    }
    latest.0 = None;
    // Reset the host's id counter too so the next session starts at 1.
    next_id.0 = 0;
}

// ---------- Connection-state predicates ----------

/// `Solo` or `Host + welcomed`. True when the local sim is the
/// authoritative one for enemies.
pub fn is_host_connected(mode: &NetMode, session: Option<&NetSession>) -> bool {
    match (mode, session) {
        (NetMode::Connected, Some(s)) if s.is_host && s.welcomed => true,
        _ => false,
    }
}

/// `Client + welcomed`. True when the local sim should suppress its
/// enemy spawn/AI and rely on host snapshots.
pub fn is_client_connected(mode: &NetMode) -> bool {
    matches!(mode, NetMode::Connected)
        // Solo doesn't count as client.
}

/// `run_if` helper exposing `is_client_connected` as a Bevy condition.
/// Used to gate the host-authoritative spawn systems off on the
/// client side.
pub fn is_client(mode: Res<NetMode>, session: Option<Res<NetSession>>) -> bool {
    match (mode.as_ref(), session.as_deref()) {
        (NetMode::Connected, Some(s)) if !s.is_host => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enemy::EnemyVariant;

    /// Every variant survives a `to_u8` → `from_u8` round-trip.
    /// Adding a new variant without keeping the discriminant stable
    /// would silently break wire-format compatibility with peers
    /// running an old build — this guards against that.
    #[test]
    fn enemy_variant_u8_round_trip() {
        for v in [
            EnemyVariant::Standard,
            EnemyVariant::Heavy,
            EnemyVariant::Scout,
            EnemyVariant::Bomber,
            EnemyVariant::Rammer,
            EnemyVariant::Sniper,
            EnemyVariant::Artillery,
        ] {
            let n = v.to_u8();
            let back = EnemyVariant::from_u8(n).expect("known variant");
            assert_eq!(back, v, "variant {:?} round-tripped to {:?}", v, back);
        }
    }

    /// Discriminants must stay distinct — duplicate numbers would
    /// silently merge two variants on the wire.
    #[test]
    fn enemy_variant_discriminants_are_unique() {
        let all = [
            EnemyVariant::Standard,
            EnemyVariant::Heavy,
            EnemyVariant::Scout,
            EnemyVariant::Bomber,
            EnemyVariant::Rammer,
            EnemyVariant::Sniper,
            EnemyVariant::Artillery,
        ];
        let mut seen = std::collections::HashSet::new();
        for v in all {
            assert!(seen.insert(v.to_u8()), "duplicate discriminant for {:?}", v);
        }
    }

    /// Unknown discriminants must yield `None` rather than panic, so
    /// a peer running a newer build with a variant we don't know
    /// about doesn't crash the client.
    #[test]
    fn enemy_variant_from_unknown_u8_is_none() {
        for n in [7u8, 8, 100, 255] {
            assert!(EnemyVariant::from_u8(n).is_none(), "n={n} should be None");
        }
    }

    /// `is_host_connected` only fires for `Connected + is_host + welcomed`.
    /// Every other state combination should return false.
    #[test]
    fn is_host_connected_truth_table() {
        // No session = false regardless of mode.
        assert!(!is_host_connected(&NetMode::Solo, None));
        assert!(!is_host_connected(&NetMode::Hosting, None));
        assert!(!is_host_connected(&NetMode::Connected, None));

        // Session but mode != Connected = false.
        let host_unwelcomed_session = NetSession {
            sock: super::super::net::bind_socket(None).unwrap(),
            my_id: 0,
            peers: std::collections::HashMap::new(),
            next_peer_id: 1,
            welcomed: false,
            is_host: true,
            last_seen: std::collections::HashMap::new(),
        };
        assert!(!is_host_connected(&NetMode::Hosting, Some(&host_unwelcomed_session)));

        // Welcomed but not host = false.
        let client_session = NetSession {
            sock: super::super::net::bind_socket(None).unwrap(),
            my_id: 1,
            peers: std::collections::HashMap::new(),
            next_peer_id: 0,
            welcomed: true,
            is_host: false,
            last_seen: std::collections::HashMap::new(),
        };
        assert!(!is_host_connected(&NetMode::Connected, Some(&client_session)));

        // The one true case: Connected + host + welcomed.
        let host_session = NetSession {
            sock: super::super::net::bind_socket(None).unwrap(),
            my_id: 0,
            peers: std::collections::HashMap::new(),
            next_peer_id: 1,
            welcomed: true,
            is_host: true,
            last_seen: std::collections::HashMap::new(),
        };
        assert!(is_host_connected(&NetMode::Connected, Some(&host_session)));
    }

    /// `is_client_connected` fires only on `Connected` (regardless of
    /// session being present — it's a pure mode check).
    #[test]
    fn is_client_connected_truth_table() {
        assert!(!is_client_connected(&NetMode::Solo));
        assert!(!is_client_connected(&NetMode::Hosting));
        assert!(!is_client_connected(&NetMode::JoiningEntry));
        assert!(!is_client_connected(&NetMode::JoiningWait));
        assert!( is_client_connected(&NetMode::Connected));
    }

    /// `LatestEnemySnapshot::take()` should yield the stored entries
    /// once and `None` thereafter — same as `Option::take`. The apply
    /// system relies on this consume-once semantics to avoid
    /// re-spawning mirrors every frame from a stale buffer.
    #[test]
    fn latest_enemy_snapshot_consume_once() {
        let mut latest = LatestEnemySnapshot::default();
        latest.0 = Some(vec![EnemyEntry {
            id: 1, kind: 0, pos: [0.0; 2], rot: 0.0, hp: 1, status_flags: 0,
            boss_class: super::super::net::NOT_A_BOSS,
        }]);
        assert!(latest.0.take().is_some(), "first take returns the buffer");
        assert!(latest.0.take().is_none(), "second take is None");
    }
}
