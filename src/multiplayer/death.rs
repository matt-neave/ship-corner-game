//! Co-op death model. Solo gameplay: local death → GameOver
//! (unchanged). Multiplayer: local death sends `PeerDied` to host;
//! host tracks team-wide alive state and only triggers GameOver when
//! every peer is dead. Dead peers spectate (their ship despawned,
//! waiting overlay shown) until the next stage transition, at which
//! point the host broadcasts `PeerRevived` and all dead peers respawn.
//!
//! Why peer-tracked rather than host-only authoritative: each peer
//! locally checks their own ship's HP every frame already (via
//! `level_complete_check` etc.). Adding "tell host I died" on top of
//! that is cheaper than streaming HP for every peer at high rate and
//! having host do the check. The host's role is the AGGREGATION
//! (track who's dead), not the detection.

use std::collections::HashSet;

use bevy::prelude::*;

use crate::components::{Health, LocalPlayer};
use crate::AppState;

use super::net::{send_to, NetMsg};
use super::{NetMode, NetSession};

/// Sentinel for "revive everyone, not just one peer." Used in
/// `NetMsg::PeerRevived::id` because that's the common case (stage
/// transition clears the dead list).
pub const REVIVE_ALL: u8 = u8::MAX;

/// Local death state for this peer. `dead=true` once the local
/// player's ship hits 0 HP; cleared by `apply_received_revive` when
/// the host broadcasts a revive.
///
/// In Solo mode this stays false forever — the single-player death
/// path (level_complete_check → GameOver state) runs unchanged.
#[derive(Resource, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalDeathState {
    pub dead: bool,
}

/// Host-only: per-peer alive tracker. Maps `peer_id` → `is_dead`.
/// Populated from `PeerDied` packets + the host's own death check.
/// `is_team_wiped()` returns true when every peer (including host)
/// is in the dead set AND the roster has at least one peer.
#[derive(Resource, Default, Debug)]
pub struct TeamDeathTracker {
    pub dead_peers: HashSet<u8>,
}

impl TeamDeathTracker {
    /// True iff every peer in `roster` appears in `dead_peers`.
    /// Roster includes the host (id 0) — if host's id 0 is missing
    /// from `dead_peers`, returns false (host still alive).
    pub fn is_team_wiped(&self, roster: &super::LobbyRoster) -> bool {
        if roster.by_id.is_empty() { return false; }
        roster.by_id.keys().all(|id| self.dead_peers.contains(id))
    }
}

/// Receive buffer for `NetMsg::PeerRevived`. Drained by
/// `apply_received_revive` which clears `LocalDeathState` + triggers
/// the respawn-friendly path.
#[derive(Resource, Default)]
pub struct PendingRevive(pub bool);

/// Per-frame: detect local-player death and route it correctly.
/// - Solo: existing single-player path handles it (don't touch).
/// - Multiplayer alive → dead transition: set `LocalDeathState.dead`,
///   despawn the local Friendly, send `PeerDied` to host.
/// - Already-dead: no-op (idempotent).
pub fn detect_local_death(
    mut commands: Commands,
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    mut local_death: ResMut<LocalDeathState>,
    local: Query<(Entity, &Health), With<LocalPlayer>>,
) {
    // Solo: single-player death path (level_complete_check) handles
    // it. Don't double-process.
    if matches!(*mode, NetMode::Solo) { return; }
    // Need a session to send PeerDied through.
    let Some(session) = session else { return };

    // Already-dead → idempotent.
    if local_death.dead { return; }

    // Local Friendly query → check HP. Missing entity means we
    // haven't spawned the local ship yet (e.g., in menu); treat as
    // alive.
    let Ok((local_entity, health)) = local.single() else { return };
    if health.0 > 0 { return; }

    // Transition alive → dead.
    local_death.dead = true;
    commands.entity(local_entity).despawn();

    // Tell the host we died. (Self-send included if host is
    // local — host's recv_packets handles the host's own dying
    // by also adding their id to TeamDeathTracker via the
    // host_self_death system below.)
    if !session.is_host {
        if let Some(&host_addr) = session.peers.get(&0) {
            let _ = send_to(&session.sock, host_addr, &NetMsg::PeerDied {
                id: session.my_id,
            });
        }
    }
    bevy::log::info!("multiplayer: local player died (id={})", session.my_id);
}

/// Host-only: when host's own local ship dies, mark the host's id in
/// the team tracker. Mirror of what `recv_packets` does for clients'
/// `PeerDied` packets.
pub fn host_track_own_death(
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    local_death: Res<LocalDeathState>,
    mut tracker: ResMut<TeamDeathTracker>,
) {
    let Some(session) = session else { return };
    if !matches!(*mode, NetMode::Connected) || !session.is_host { return; }
    if local_death.dead {
        tracker.dead_peers.insert(session.my_id);
    }
}

/// Host-only: check the team tracker each frame; if everyone is
/// dead, transition to GameOver. State sync broadcasts to clients
/// via the existing `broadcast_state_change` path.
pub fn host_check_team_wipe(
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    tracker: Res<TeamDeathTracker>,
    roster: Res<super::LobbyRoster>,
    state: Res<State<AppState>>,
    mut next: ResMut<NextState<AppState>>,
) {
    let Some(session) = session else { return };
    if !matches!(*mode, NetMode::Connected) || !session.is_host { return; }
    // Only trigger from Playing — don't fire if we're already in
    // GameOver or some menu.
    if *state.get() != AppState::Playing { return; }
    if !tracker.is_team_wiped(&roster) { return; }
    bevy::log::info!("multiplayer: team wipe — all peers dead, GameOver");
    next.set(AppState::GameOver);
}

/// Host-only: when host enters a new stage / level (StageComplete is
/// the transition point), broadcast `PeerRevived(REVIVE_ALL)` and
/// clear the team tracker. Dead peers receive and respawn.
pub fn host_broadcast_revive_on_stage_complete(
    mode: Res<NetMode>,
    session: Option<Res<NetSession>>,
    mut tracker: ResMut<TeamDeathTracker>,
    state: Res<State<AppState>>,
) {
    let Some(session) = session else { return };
    if !matches!(*mode, NetMode::Connected) || !session.is_host { return; }
    // Trigger ON ENTRY to StageComplete — use Bevy's state change
    // detection via `is_changed`.
    if !state.is_changed() { return; }
    if *state.get() != AppState::StageComplete { return; }
    if tracker.dead_peers.is_empty() { return; }

    bevy::log::info!("multiplayer: stage complete — broadcasting revive");
    tracker.dead_peers.clear();
    let msg = NetMsg::PeerRevived { id: REVIVE_ALL };
    for &addr in session.peers.values() {
        let _ = send_to(&session.sock, addr, &msg);
    }
}

/// Drain `PendingRevive` — if it fired, clear local death state and
/// re-spawn the local Friendly via the standard `spawn_player_world`
/// system (which is idempotent — checks for existing Friendly). Runs
/// on every peer (host and client).
pub fn apply_received_revive(
    mut pending: ResMut<PendingRevive>,
    mut local_death: ResMut<LocalDeathState>,
) {
    if !pending.0 { return };
    pending.0 = false;
    if local_death.dead {
        local_death.dead = false;
        bevy::log::info!("multiplayer: revived");
        // `spawn_player_world` runs on `OnEnter(Playing)` and is
        // idempotent; it'll respawn our Friendly the next time the
        // game enters Playing. Until then the local boat stays
        // gone — typical case is "revive on next stage entry"
        // which IS an OnEnter(Playing) → fresh ship.
    }
}

/// Marker on the spectator overlay root entity.
#[derive(Component)]
pub struct SpectatorOverlay;

/// Per-frame: show / hide the "YOU DIED — WAITING FOR PARTNER"
/// overlay based on `LocalDeathState`. Spawns/despawns rather than
/// toggling visibility so the UI doesn't ghost the overlay between
/// rounds.
pub fn sync_spectator_overlay(
    mut commands: Commands,
    state: Res<State<AppState>>,
    local_death: Res<LocalDeathState>,
    font: Option<Res<crate::fonts::PixelFont>>,
    thaleah: Option<Res<crate::fonts::ThaleahFont>>,
    existing: Query<Entity, With<SpectatorOverlay>>,
) {
    // Show only when dead AND in Playing (don't bleed into menus).
    let should_show = local_death.dead && *state.get() == AppState::Playing;
    let already_shown = !existing.is_empty();

    if should_show && !already_shown {
        let (Some(font), Some(thaleah)) = (font, thaleah) else { return };
        commands.spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(0.0),
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                bottom: Val::Px(0.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                row_gap: Val::Px(crate::ui_kit::theme::GAP_LG),
                ..default()
            },
            // Translucent dark wash so the live world below stays
            // visible but the overlay clearly reads as "you're not
            // playing right now."
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.55)),
            ZIndex(180),
            Visibility::Inherited,
            SpectatorOverlay,
        ))
        .with_children(|root| {
            root.spawn((
                Text::new("YOU DIED"),
                crate::fonts::thaleah_text_font(&thaleah, 64.0),
                TextColor(Color::srgb(0.95, 0.35, 0.30)),
                TextShadow {
                    offset: Vec2::splat(2.0),
                    color: Color::srgba(0.0, 0.0, 0.0, 0.85),
                },
            ));
            root.spawn(crate::ui_kit::pixel_label(
                &font,
                "WAITING FOR YOUR PARTNER...",
                crate::ui_kit::theme::FONT_LG,
                crate::ui_kit::theme::ON_SURFACE_DIM,
            ));
            root.spawn(crate::ui_kit::pixel_label(
                &font,
                "RESPAWNS NEXT STAGE",
                crate::ui_kit::theme::FONT_MD,
                crate::ui_kit::theme::ON_SURFACE_DIM,
            ));
        });
    } else if !should_show && already_shown {
        for e in &existing {
            commands.entity(e).despawn();
        }
    }
}

/// On `OnExit(Playing)` clear death/team state so a fresh round
/// starts clean.
pub fn reset_death_state(
    mut local_death: ResMut<LocalDeathState>,
    mut tracker: ResMut<TeamDeathTracker>,
) {
    local_death.dead = false;
    tracker.dead_peers.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn team_wipe_detects_all_dead() {
        let mut roster = super::super::LobbyRoster::default();
        roster.by_id.insert(0, "HOST".into());
        roster.by_id.insert(1, "CLIENT".into());

        let mut tracker = TeamDeathTracker::default();
        assert!(!tracker.is_team_wiped(&roster), "nobody dead → not a wipe");

        tracker.dead_peers.insert(0);
        assert!(!tracker.is_team_wiped(&roster), "only host dead → not a wipe");

        tracker.dead_peers.insert(1);
        assert!(tracker.is_team_wiped(&roster), "all dead → wipe");
    }

    #[test]
    fn team_wipe_false_on_empty_roster() {
        let roster = super::super::LobbyRoster::default();
        let tracker = TeamDeathTracker::default();
        // Pre-handshake: roster empty. Don't trigger wipe with no
        // peers — otherwise the host would game-over the moment
        // they start hosting.
        assert!(!tracker.is_team_wiped(&roster));
    }
}
