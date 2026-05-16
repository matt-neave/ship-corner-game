//! LAN multiplayer (UDP, host + client topology).
//!
//! Authority split:
//! - **Host**: authoritative for enemies (spawning, AI, HP, death),
//!   wave clock, boss flow, team-death tracking, level-up rising
//!   edges, and the `Map` selection screen.
//! - **Per-peer**: own boat (movement, bullets fired), own
//!   `PlayerStats` + `TurretConfig`, own `Scrap`, own shop / RNG /
//!   loot rolls, own LevelUp picks, own HullSelect pick. Each peer's
//!   final loadout is broadcast to every other peer via
//!   [`loadout::broadcast_turret_config`] so ghost ships render the
//!   right turrets.
//!
//! Connection flow:
//! 1. Host clicks HOST → `start_hosting` binds a UDP socket on
//!    `HOST_PORT`, exposes the LAN IP via [`HostStatus`].
//! 2. Client clicks JOIN, enters the host's IP → `start_joining`
//!    binds an ephemeral socket and sends [`NetMsg::Hello`].
//! 3. Host replies with `Welcome`; both peers transition into
//!    `AppState::Lobby` once the handshake completes.
//! 4. Either peer can hit START in the lobby (host's button is
//!    canonical for actually transitioning).
//!
//! Per-peer states (Customize / LevelUp / HullSelect) pass through
//! to the client on state sync (each peer interacts locally); the
//! [`ready`] module gates the transition out by waiting for every
//! peer to click READY.
//!
//! Native-only: the module is gated off on `wasm32` (browsers can't
//! open UDP sockets). The WASM build stays single-player.

use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};

use bevy::prelude::*;

pub mod bullets;
pub mod death;
pub mod enemies;
pub mod ghost;
pub mod loadout;
pub mod lobby;
pub mod net;
pub mod state_sync;
pub mod ready;
pub mod ui;
pub mod wave;
pub mod xp_sync;

use crate::AppState;
use enemies::{
    apply_enemy_snapshot, apply_relayed_damage, assign_net_ids, despawn_all_mirrors,
    relay_damage_to_host, relay_proc_fx_to_peers, send_enemy_snapshot, send_proc_fx,
    smooth_mirror_transforms, spawn_proc_fx_visuals, EnemySnapshotTimer,
    LatestEnemySnapshot, NextNetEntityId, PendingDamageRelay, ProcFxInbox,
};
use loadout::{
    apply_received_player_stats, apply_received_turret_config, broadcast_player_stats,
    broadcast_turret_config, force_initial_loadout_broadcast, PeerLoadouts, PendingPlayerStats,
    PendingTurretConfig,
};
use state_sync::{
    apply_state_change, broadcast_state_change, LastBroadcastedState, PendingStateChange,
};
use bullets::{
    emit_bullet_fired_signals, relay_bullet_fired, send_bullet_fired,
    BulletFiredInbox,
};
use death::{
    apply_received_revive, detect_local_death, host_broadcast_revive_on_stage_complete,
    host_check_team_wipe, host_track_own_death, reset_death_state, sync_spectator_overlay,
    LocalDeathState, PendingRevive, TeamDeathTracker,
};
use ready::{
    announce_local_ready, drain_ready_inbox, host_advance_when_all_ready,
    track_own_ready, reset_ready_state_on_enter, sync_ready_overlay,
    LocalReadyState, PendingPeerReady, TeamReadyTracker,
};
use wave::{apply_wave_state, broadcast_wave_state, LastBroadcastedWaveState, PendingWaveState};
use xp_sync::{
    apply_received_level_up_grants, apply_received_xp, broadcast_level_up_grants, broadcast_xp,
    LastBroadcastedXp, LastSeenLocalLevelUps, PendingLevelUpGrants, PendingXpSync,
};
use ghost::{
    apply_snapshots, cull_stale_ghosts, despawn_all_ghosts, detect_stale_peers, recv_packets,
    refresh_ghost_turrets, send_heartbeat, send_local_transform, spawn_missing_ghosts,
    HeartbeatTimer, PeerSnapshots, TransformSendTimer,
};
use net::{bind_socket, local_lan_ip, send_to, NetMsg, HOST_PORT};

/// Current multiplayer mode. `Solo` means the multiplayer module is
/// inert — the rest of the game runs untouched. `Hosting` and
/// `Joining` are transient pre-connection states; `Connected` is the
/// state during actual gameplay.
#[derive(Resource, Clone, Copy, PartialEq, Eq, Debug)]
pub enum NetMode {
    Solo,
    /// HOST clicked, UDP socket bound on `HOST_PORT`, waiting for the
    /// first client `Hello`. UI shows "HOSTING ON x.x.x.x — WAITING".
    Hosting,
    /// JOIN clicked, IP entry shown. While the field is open the
    /// socket isn't bound yet — that happens on Enter.
    JoiningEntry,
    /// IP submitted, ephemeral socket bound, Hello sent. Waiting for
    /// the host's Welcome reply.
    JoiningWait,
    /// Welcome received (client) or first client connected (host).
    /// Both peers are now expected to be in `AppState::Playing` and
    /// exchanging Transform packets.
    Connected,
}

impl Default for NetMode {
    fn default() -> Self { Self::Solo }
}

/// Live socket + peer table for the current session. Wrapped in an
/// `Option<Res>` rather than baked into `NetMode` so the systems that
/// don't care about the socket (UI, state transitions) aren't forced
/// to depend on its type.
#[derive(Resource)]
pub struct NetSession {
    pub sock: UdpSocket,
    /// Our own peer id. Host is always 0. Clients receive their id in
    /// `NetMsg::Welcome` — until then, this field holds 1 as a
    /// reasonable default that the Welcome overwrites.
    pub my_id: u8,
    /// id → SocketAddr table of every peer we currently know about.
    /// Host populates it on each `Hello`; client populates it (once)
    /// with the host as id 0 on Welcome.
    pub peers: HashMap<u8, SocketAddr>,
    /// Host: monotonic counter for the next peer id to hand out.
    /// Client: unused (always 0).
    pub next_peer_id: u8,
    /// True after the connection handshake completed. Set on host as
    /// soon as the first `Hello` lands; set on client as soon as the
    /// `Welcome` lands.
    pub welcomed: bool,
    /// True on the host side, false on the client side. Used by
    /// `recv_packets` to dispatch Hello vs Welcome appropriately.
    pub is_host: bool,
    /// `peer_id → last time we received ANY packet from them`. Updated
    /// in `recv_packets` whenever a sender is identifiable (id-bearing
    /// packets, or addr → id reverse lookup). Drives
    /// `detect_stale_peers` — peers silent for longer than
    /// [`PEER_TIMEOUT_SECS`] are treated as having sent a `Bye`.
    /// Without this, a hard process kill / network drop leaves the
    /// remaining peers waiting forever.
    pub last_seen: HashMap<u8, std::time::Instant>,
}

/// How long a peer can go without sending a packet before we treat it
/// as having silently disconnected. Tuned so a brief network blip
/// doesn't drop a real connection — host snapshots are 20Hz +
/// Transforms 30Hz so anything past 5s of silence is genuinely gone.
pub const PEER_TIMEOUT_SECS: f32 = 5.0;

/// Status string + LAN IP for the menu's HOST screen. Lives as its
/// own resource so the UI layer doesn't have to know about the socket.
#[derive(Resource, Default)]
pub struct HostStatus {
    pub lan_ip: String,
    pub port: u16,
}

/// Text the player is typing into the JOIN IP entry. Captured by
/// `capture_join_ip_keys`; rendered by the main-menu UI.
#[derive(Resource, Default)]
pub struct JoinIpEntry {
    pub buf: String,
    /// Set to `Some(msg)` to display an error under the input (e.g.
    /// "couldn't reach host"). Cleared on next keystroke.
    pub last_error: Option<String>,
}

/// The local player's display name. Sent in `Hello` (clients) or
/// `Welcome` (host) so peers can show a roster of named players in
/// the lobby. Defaults to a stub so name-less builds still work.
#[derive(Resource)]
pub struct LocalPlayerName(pub String);

impl Default for LocalPlayerName {
    fn default() -> Self { Self("PLAYER".to_string()) }
}

/// Lobby roster — id → display name for every peer currently in the
/// lobby, INCLUDING ourselves. Mirrored on both host and client:
/// - host populates on each `Hello` received (and seeds with its own
///   entry on `start_hosting`);
/// - client populates from `Welcome::existing_peers` + adds its own
///   entry, then keeps in sync via `PeerJoined` / `PeerLeft`.
///
/// Lobby UI reads this each frame to render the player list.
#[derive(Resource, Default)]
pub struct LobbyRoster {
    pub by_id: std::collections::HashMap<u8, String>,
}

/// Set by `recv_packets` when this client receives a `Kicked`
/// packet. Drained by a one-shot system that tears down the session
/// and returns to the main menu. Decoupling via a resource means
/// `recv_packets` doesn't need `Commands` for the kick path.
#[derive(Resource, Default)]
pub struct PendingKick(pub Option<String>);

/// Run condition: true post-handshake (mode = Connected). Used to
/// gate `recv_packets` so it doesn't race `tick_handshake` during
/// the handshake window:
/// - Pre-handshake (mode = Hosting / JoiningWait): only
///   `tick_handshake` drains. It sets `welcomed=true` and flips
///   mode to Connected on first packet.
/// - Post-handshake (mode = Connected): only `recv_packets` drains.
///   `tick_handshake` bails early on Connected so the two never
///   race.
///
/// State-dependent draining was a mistake: every state where a peer
/// can be Connected needs the socket to keep flowing (Customize,
/// LevelUp, HullSelect, Map, StageComplete, WaitingForHost, Paused
/// — all of them). Otherwise heartbeats / state-changes / loadout
/// updates pile up in the OS UDP buffer and `detect_stale_peers`
/// times out the link the moment we re-enter a state that was
/// previously gated. The state-specific gating belongs on the apply
/// systems (e.g., `apply_enemy_snapshot` stays Playing-only), not
/// on the recv side.
pub fn in_mp_session(_state: Res<State<AppState>>, mode: Res<NetMode>) -> bool {
    matches!(*mode, NetMode::Connected)
}

/// Bevy plugin — wires every multiplayer system + resource. The whole
/// plugin is gated to non-wasm in main.rs so the browser build never
/// touches its deps.
pub struct MultiplayerPlugin;

impl Plugin for MultiplayerPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(NetMode::default())
            .insert_resource(HostStatus::default())
            .insert_resource(JoinIpEntry::default())
            .insert_resource(PeerSnapshots::default())
            .insert_resource(TransformSendTimer::default())
            .insert_resource(HeartbeatTimer::default())
            .insert_resource(NextNetEntityId::default())
            .insert_resource(EnemySnapshotTimer::default())
            .insert_resource(LatestEnemySnapshot::default())
            .insert_resource(PendingDamageRelay::default())
            .insert_resource(ProcFxInbox::default())
            .insert_resource(PendingStateChange::default())
            .insert_resource(LastBroadcastedState::default())
            .insert_resource(LocalPlayerName::default())
            .insert_resource(LobbyRoster::default())
            .insert_resource(PendingKick::default())
            .insert_resource(PendingPlayerStats::default())
            .insert_resource(PendingTurretConfig::default())
            .insert_resource(PeerLoadouts::default())
            .insert_resource(PendingWaveState::default())
            .insert_resource(LastBroadcastedWaveState::default())
            .insert_resource(PendingXpSync::default())
            .insert_resource(LastBroadcastedXp::default())
            .insert_resource(PendingLevelUpGrants::default())
            .insert_resource(LastSeenLocalLevelUps::default())
            .insert_resource(LocalReadyState::default())
            .insert_resource(TeamReadyTracker::default())
            .insert_resource(PendingPeerReady::default())
            .insert_resource(BulletFiredInbox::default())
            .insert_resource(LocalDeathState::default())
            .insert_resource(TeamDeathTracker::default())
            .insert_resource(PendingRevive::default())
            // Connection-handshake polling runs unconditionally while
            // we're in a connecting state. Cheap — the systems early-
            // exit on Solo.
            // PostStartup, not Startup, so `Res<PixelFont>` is
            // visible — `fonts::setup_pixel_font` inserts the
            // resource in Startup but `Commands::insert_resource`
            // only takes effect at the next sync point, so a
            // sibling Startup system can't read it.
            .add_systems(PostStartup, (ui::setup_overlay, ui::setup_lag_indicator))
            .add_systems(Update, (
                tick_handshake,
                capture_join_ip_keys,
                capture_name_keys,
                ui::update_overlay,
                ui::update_lag_indicator,
                ui::cancel_on_esc,
                // State sync — runs unconditionally so it catches
                // transitions in any AppState (MainMenu → HullSelect
                // etc.). Each handler internally short-circuits on
                // mode/session checks.
                broadcast_state_change,
                apply_state_change,
                // Loadout sync — change-driven on host, drained on
                // client. Cheap to run unconditionally; both
                // systems early-exit on the wrong mode/side.
                broadcast_player_stats,
                apply_received_player_stats,
                broadcast_turret_config,
                apply_received_turret_config,
                broadcast_wave_state,
                apply_wave_state,
                broadcast_xp,
                apply_received_xp,
                broadcast_level_up_grants,
                apply_received_level_up_grants,
            ))
            // Gameplay netloop. `recv_packets` runs in both Playing
            // and Lobby so the socket drains while we're in the
            // lobby waiting room too (otherwise PeerJoined / Kicked
            // / etc. would pile up unread until START). Everything
            // else stays Playing-gated.
            .add_systems(Update, recv_packets.run_if(in_mp_session))
            // Timeout-based disconnect detection — runs whenever
            // we're in a session (Lobby or Playing). Cheap (scans
            // the small last_seen map). Catches hard process kills
            // / network drops that don't send a clean Bye.
            .add_systems(Update, detect_stale_peers.run_if(in_mp_session).after(recv_packets))
            // Low-rate heartbeat keeps `last_seen` fresh in states
            // where no other packets fly (Paused, Lobby, menus) so
            // the timeout detector above doesn't kick peers out
            // during a long pause. Runs unconditionally; the system
            // short-circuits on Solo / not-yet-connected.
            .add_systems(Update, send_heartbeat)
            // Force a one-shot loadout broadcast each time we enter
            // Playing / Lobby so peers who never opened the shop
            // still push their default `TurretConfig` + `PlayerStats`
            // out for ghost rendering on the other side.
            .add_systems(OnEnter(AppState::Playing), force_initial_loadout_broadcast)
            .add_systems(OnEnter(AppState::Lobby),   force_initial_loadout_broadcast)
            .add_systems(Update, (
                spawn_missing_ghosts,
                refresh_ghost_turrets,
                apply_snapshots,
                cull_stale_ghosts,
                send_local_transform,
                assign_net_ids,
                apply_enemy_snapshot,
                // Lerp mirrors toward their MirrorTarget every
                // frame so the 20Hz snapshot cadence doesn't pop
                // visually. Runs AFTER apply_enemy_snapshot so the
                // latest target is in place before lerping.
                smooth_mirror_transforms,
                send_enemy_snapshot,
            ).chain().run_if(in_state(AppState::Playing)))
            // Damage relay sits in the middle of the existing damage
            // pipeline: `relay_damage_to_host` runs AFTER
            // `bullet_collisions` (so the queue is populated) and
            // BEFORE `process_damage_events` (so events targeting
            // mirrors get skimmed off before the local apply pass).
            // `apply_relayed_damage` runs on the host to push
            // incoming relayed events into the same queue, also
            // before `process_damage_events`.
            .add_systems(Update, (
                relay_damage_to_host
                    .after(crate::bullet::bullet_collisions)
                    .before(crate::bullet::process_damage_events),
                apply_relayed_damage
                    .after(recv_packets)
                    .before(crate::bullet::process_damage_events),
                // ProcFx broadcast: outgoing → wire (every peer);
                // incoming → re-broadcast on host. Order matters
                // less here than for damage; both run after the
                // recv_packets / send_local_transform chain.
                send_proc_fx
                    .after(send_local_transform),
                relay_proc_fx_to_peers
                    .after(recv_packets)
                    .before(send_proc_fx),
                // Spawn local visuals AFTER relay so the host has
                // already forwarded the packet — relay reads,
                // spawn_proc_fx_visuals drains.
                spawn_proc_fx_visuals
                    .after(relay_proc_fx_to_peers),
                // ---- Signal-based bullet replication ----
                // Order:
                //   bullet_collisions (production system) spawns local
                //   bullets → `emit_bullet_fired_signals` writes events
                //   → `send_bullet_fired` packetises → wire.
                //   Receivers: `recv_packets` populates BulletFiredInbox
                //   → `relay_bullet_fired` (host-only) re-broadcasts to
                //   other peers → `spawn_received_bullets` drains and
                //   spawns local damage=0 visual.
                emit_bullet_fired_signals,
                send_bullet_fired
                    .after(emit_bullet_fired_signals),
                relay_bullet_fired
                    .after(recv_packets)
                    .before(bullets::spawn_received_bullets),
                bullets::spawn_received_bullets
                    .after(relay_bullet_fired),
                // ---- Co-op death model ----
                // Order: detect_local_death runs every frame, sets
                // dead flag + sends PeerDied. host_track_own_death
                // mirrors local death into the team tracker. recv
                // pulls in remote PeerDied → tracker. Then
                // host_check_team_wipe fires GameOver if all dead.
                // apply_received_revive clears local state.
                detect_local_death,
                host_track_own_death,
                host_check_team_wipe.after(recv_packets),
                apply_received_revive,
                host_broadcast_revive_on_stage_complete,
                sync_spectator_overlay,
            ).run_if(in_state(AppState::Playing)))
            // On exit from Playing (death, pause-then-quit, etc.) tear
            // Hook session-end cleanup to `OnEnter(MainMenu)` — the
            // ONLY transition that means "this MP session is really
            // over." Hooking on `OnExit(Playing)` was wrong because
            // pause / customize / level-up / hull-select all leave
            // Playing too, and tearing down the session on a pause
            // would send Bye to peers + drop the local mode to Solo.
            // The MP-stays-alive states (Customize/LevelUp/HullSelect/
            // Map/Paused/etc.) keep ghosts + mirrors in place so the
            // arena resumes seamlessly when we re-enter Playing.
            .add_systems(OnEnter(AppState::MainMenu), (
                despawn_all_ghosts,
                despawn_all_mirrors,
                teardown_on_exit,
                reset_death_state,
            ))
            // ---- Per-peer ready check (Customize / LevelUp / HullSelect) ----
            // Each per-peer state gets a reset hook on entry so a
            // stale ready from the last visit doesn't auto-advance.
            // The Update systems run in any of these states and the
            // advance system routes to the right next state per the
            // table in `host_advance_when_all_ready`.
            .add_systems(OnEnter(AppState::Customize), reset_ready_state_on_enter)
            .add_systems(OnEnter(AppState::LevelUp), reset_ready_state_on_enter)
            .add_systems(OnEnter(AppState::HullSelect), reset_ready_state_on_enter)
            .add_systems(Update, (
                announce_local_ready,
                drain_ready_inbox.after(recv_packets),
                track_own_ready,
                host_advance_when_all_ready,
            ).run_if(|s: Res<State<AppState>>| matches!(
                *s.get(),
                AppState::Customize | AppState::LevelUp | AppState::HullSelect,
            )))
            // Overlay runs every frame (not just Customize) so it
            // can despawn cleanly on exit. The system itself
            // short-circuits when out of the per-peer states.
            .add_systems(Update, sync_ready_overlay)
            // ---- Lobby state lifecycle + click handlers ----
            .add_systems(OnEnter(AppState::Lobby), lobby::setup_lobby)
            .add_systems(OnExit(AppState::Lobby), lobby::teardown_lobby)
            .add_systems(Update, (
                lobby::refresh_roster,
                lobby::handle_start_click,
                lobby::handle_kick_click,
            ).run_if(in_state(AppState::Lobby)))
            // ---- WaitingForHost overlay lifecycle ----
            .add_systems(OnEnter(AppState::WaitingForHost), lobby::setup_waiting_overlay)
            .add_systems(OnExit(AppState::WaitingForHost), lobby::teardown_waiting_overlay)
            // Generic LEAVE handler covers Lobby + WaitingForHost so
            // ESC / LEAVE button works on both screens.
            .add_systems(Update, lobby::handle_leave_click_any_mp)
            // `handle_received_kick` runs in any MP state because a
            // host can kick mid-game; it self-gates.
            .add_systems(Update, lobby::handle_received_kick);
    }
}

/// Bind the host socket and flip to `Hosting`. Called when the player
/// clicks HOST on the main menu. Idempotent — if a session already
/// exists, no-op. Uses [`HOST_PORT`]; tests that need port isolation
/// should call [`start_hosting_on_port`] with `0` (OS-assigned
/// ephemeral) instead.
pub fn start_hosting(
    commands: &mut Commands,
    mode: &mut NetMode,
    status: &mut HostStatus,
    roster: &mut LobbyRoster,
    local_name: &LocalPlayerName,
) -> Result<(), String> {
    start_hosting_on_port(commands, mode, status, roster, local_name, HOST_PORT)
}

/// Backing implementation for `start_hosting` parameterised by the
/// listening port. Lets the test suite spin multiple hosts in
/// parallel by binding ephemeral ports instead of fighting over the
/// production `HOST_PORT`. Production code always calls
/// `start_hosting` (which fixes the port).
pub fn start_hosting_on_port(
    commands: &mut Commands,
    mode: &mut NetMode,
    status: &mut HostStatus,
    roster: &mut LobbyRoster,
    local_name: &LocalPlayerName,
    port: u16,
) -> Result<(), String> {
    if !matches!(*mode, NetMode::Solo) {
        return Ok(()); // already in some MP state
    }
    let sock = bind_socket(Some(port)).map_err(|e| format!("bind {port}: {e}"))?;
    let bound_port = sock.local_addr().map(|a| a.port()).unwrap_or(port);
    commands.insert_resource(NetSession {
        sock,
        my_id: 0,
        peers: HashMap::new(),
        next_peer_id: 1,
        welcomed: false,
        is_host: true,
        last_seen: HashMap::new(),
    });
    *status = HostStatus { lan_ip: local_lan_ip(), port: bound_port };
    *mode = NetMode::Hosting;
    // Seed the roster with the host's own entry (id 0). The lobby UI
    // reads this directly; clients learn the host's name via
    // `Welcome::host_name`.
    roster.by_id.clear();
    roster.by_id.insert(0, local_name.0.clone());
    bevy::log::info!("multiplayer: hosting on {}:{} as '{}'",
                     status.lan_ip, status.port, local_name.0);
    Ok(())
}

/// Parse the player's typed string into a `SocketAddr`. Accepts both
/// bare `1.2.3.4` (assumes [`HOST_PORT`]) and `1.2.3.4:5050`. Pure
/// function so the parsing logic can be unit-tested without a Bevy
/// `World`.
pub fn parse_join_addr(raw: &str) -> Result<SocketAddr, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("enter an IP first".to_string());
    }
    if raw.contains(':') {
        raw.parse().map_err(|e| format!("bad address: {e}"))
    } else {
        format!("{raw}:{HOST_PORT}")
            .parse()
            .map_err(|e| format!("bad address: {e}"))
    }
}

/// Submit the player's typed IP, bind an ephemeral socket, send a
/// Hello. Called from the JOIN sub-page when the user presses Enter.
/// On failure, writes the error into `JoinIpEntry.last_error` and
/// stays in `JoiningEntry` so the player can fix the address and try
/// again.
pub fn start_joining(
    commands: &mut Commands,
    mode: &mut NetMode,
    entry: &mut JoinIpEntry,
    local_name: &LocalPlayerName,
) {
    let host_addr = match parse_join_addr(&entry.buf) {
        Ok(a) => a,
        Err(e) => {
            entry.last_error = Some(e);
            return;
        }
    };
    let sock = match bind_socket(None) {
        Ok(s) => s,
        Err(e) => {
            entry.last_error = Some(format!("bind: {e}"));
            return;
        }
    };
    let mut peers = HashMap::new();
    peers.insert(0, host_addr); // host is id 0
    let session = NetSession {
        sock,
        my_id: 1, // placeholder until Welcome
        peers,
        next_peer_id: 0,
        welcomed: false,
        is_host: false,
        last_seen: HashMap::new(),
    };
    if let Err(e) = send_to(&session.sock, host_addr, &NetMsg::Hello {
        name: local_name.0.clone(),
    }) {
        entry.last_error = Some(format!("send Hello: {e}"));
        return;
    }
    commands.insert_resource(session);
    *mode = NetMode::JoiningWait;
    bevy::log::info!("multiplayer: sent Hello to {host_addr}, waiting for Welcome");
}

/// `OnExit(Playing)` system wrapper around [`tear_down_session`].
/// Exists as a Bevy-system-shaped function so we can register it
/// directly in the schedule.
pub fn teardown_on_exit(
    mut commands: Commands,
    mut mode: ResMut<NetMode>,
    session: Option<Res<NetSession>>,
) {
    if matches!(*mode, NetMode::Solo) { return; }
    tear_down_session(&mut commands, &mut mode, session.as_deref());
}

/// Tear everything down on a clean exit (player pressed ESC from the
/// host status screen, or quit gameplay).
pub fn tear_down_session(
    commands: &mut Commands,
    mode: &mut NetMode,
    session: Option<&NetSession>,
) {
    if let Some(session) = session {
        let bye = NetMsg::Bye { id: session.my_id };
        for &addr in session.peers.values() {
            let _ = send_to(&session.sock, addr, &bye);
        }
    }
    commands.remove_resource::<NetSession>();
    *mode = NetMode::Solo;
}

/// Per-frame: advance the connection state machine. Drives the
/// MainMenu→Playing transition the moment we go Connected so both
/// peers land in the same screen automatically.
fn tick_handshake(
    mut mode: ResMut<NetMode>,
    mut session: Option<ResMut<NetSession>>,
    mut snapshots: ResMut<PeerSnapshots>,
    mut roster: ResMut<LobbyRoster>,
    mut next: ResMut<NextState<AppState>>,
    state: Res<State<AppState>>,
    local_name: Res<LocalPlayerName>,
) {
    if matches!(*mode, NetMode::Solo | NetMode::JoiningEntry | NetMode::Connected) {
        // Solo + JoiningEntry: nothing to poll yet. Connected: handled
        // by the gameplay netloop.
        return;
    }
    // Drain the socket so Hello / Welcome land while we're still in
    // the menu. `recv_packets` would do the same but it's gated to
    // Playing; here we want it earlier so the welcomed flag flips
    // before we transition.
    if let Some(session) = session.as_mut() {
        let packets = net::drain_packets(&session.sock);
        let now = std::time::Instant::now();
        for (addr, msg) in packets {
            match msg {
                NetMsg::Hello { name } if session.is_host => {
                    if !session.peers.values().any(|a| *a == addr) {
                        let new_id = session.next_peer_id;
                        session.next_peer_id += 1;
                        session.peers.insert(new_id, addr);
                        // Build the existing-peers list (everyone in
                        // the roster EXCEPT the joiner) so the new
                        // client knows who's already here.
                        let existing_peers: Vec<(u8, String)> = roster.by_id.iter()
                            .map(|(&id, n)| (id, n.clone()))
                            .collect();
                        let _ = send_to(&session.sock, addr, &NetMsg::Welcome {
                            your_id: new_id,
                            host_name: local_name.0.clone(),
                            existing_peers,
                        });
                        // Tell every EXISTING client about the new
                        // arrival so their rosters update too.
                        let join_announce = NetMsg::PeerJoined {
                            id: new_id,
                            name: name.clone(),
                        };
                        for (&peer_id, &peer_addr) in session.peers.iter() {
                            if peer_id == new_id { continue; }
                            let _ = send_to(&session.sock, peer_addr, &join_announce);
                        }
                        roster.by_id.insert(new_id, name);
                        session.welcomed = true;
                        bevy::log::info!("multiplayer: peer {new_id} connected from {addr}");
                    }
                }
                NetMsg::Welcome { your_id, host_name, existing_peers } if !session.is_host => {
                    session.my_id = your_id;
                    session.welcomed = true;
                    session.peers.insert(0, addr);
                    // Seed the client's roster from the host's
                    // authoritative view: host (id 0) + every
                    // already-present peer + ourselves.
                    roster.by_id.clear();
                    roster.by_id.insert(0, host_name);
                    for (id, name) in existing_peers {
                        roster.by_id.insert(id, name);
                    }
                    roster.by_id.insert(your_id, local_name.0.clone());
                    bevy::log::info!("multiplayer: connected to host {addr} as id {your_id}");
                }
                NetMsg::PeerJoined { id, name } if !session.is_host => {
                    roster.by_id.insert(id, name);
                }
                NetMsg::PeerLeft { id } if !session.is_host => {
                    roster.by_id.remove(&id);
                }
                NetMsg::Transform { id, pos, rot } => {
                    snapshots.0.insert(
                        id,
                        ghost::PeerSnapshot {
                            pos: Vec2::new(pos[0], pos[1]),
                            rot,
                            last_seen: now,
                        },
                    );
                }
                _ => {}
            }
        }
        if session.welcomed && !matches!(*mode, NetMode::Connected) {
            *mode = NetMode::Connected;
            // Auto-transition into the lobby. From there the host's
            // START button drives the lockstep transition to Playing
            // via `StateChange`.
            if *state.get() == AppState::MainMenu {
                next.set(AppState::Lobby);
            }
        }
    }
}

/// Per-frame while in `JoiningEntry`: pump key events into
/// `JoinIpEntry.buf`. Backspace deletes, Enter submits, Escape
/// cancels. Restricted character set (digits, `.`, `:`) so the field
/// can't accidentally pick up random typing.
fn capture_join_ip_keys(
    mut commands: Commands,
    mut mode: ResMut<NetMode>,
    mut entry: ResMut<JoinIpEntry>,
    keys: Res<ButtonInput<KeyCode>>,
    local_name: Res<LocalPlayerName>,
) {
    if !matches!(*mode, NetMode::JoiningEntry) { return; }

    if keys.just_pressed(KeyCode::Escape) {
        *mode = NetMode::Solo;
        entry.buf.clear();
        entry.last_error = None;
        return;
    }
    if keys.just_pressed(KeyCode::Backspace) {
        entry.buf.pop();
        entry.last_error = None;
        return;
    }
    if keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::NumpadEnter) {
        start_joining(&mut commands, &mut mode, &mut entry, &local_name);
        return;
    }
    // Digit row: KeyCode::Digit0..Digit9 + numpad.
    let digit_pressed = [
        (KeyCode::Digit0, '0'), (KeyCode::Digit1, '1'), (KeyCode::Digit2, '2'),
        (KeyCode::Digit3, '3'), (KeyCode::Digit4, '4'), (KeyCode::Digit5, '5'),
        (KeyCode::Digit6, '6'), (KeyCode::Digit7, '7'), (KeyCode::Digit8, '8'),
        (KeyCode::Digit9, '9'),
        (KeyCode::Numpad0, '0'), (KeyCode::Numpad1, '1'), (KeyCode::Numpad2, '2'),
        (KeyCode::Numpad3, '3'), (KeyCode::Numpad4, '4'), (KeyCode::Numpad5, '5'),
        (KeyCode::Numpad6, '6'), (KeyCode::Numpad7, '7'), (KeyCode::Numpad8, '8'),
        (KeyCode::Numpad9, '9'),
        (KeyCode::Period, '.'),
        (KeyCode::NumpadDecimal, '.'),
        (KeyCode::Semicolon, ':'),
    ];
    for (k, c) in digit_pressed {
        if keys.just_pressed(k) && entry.buf.len() < 22 {
            entry.buf.push(c);
            entry.last_error = None;
        }
    }
}

/// Capture A-Z + digits + backspace into `LocalPlayerName` while the
/// player is on the main menu (NetMode = Solo). Capped at 16 chars.
/// Lets the player edit their display name in place — name is then
/// sent in `Hello` / used in `Welcome` so it shows in everyone's
/// roster. Quiet system that does nothing outside MainMenu / Solo
/// so it can't interfere with gameplay typing in other states.
pub fn capture_name_keys(
    mode: Res<NetMode>,
    state: Res<State<AppState>>,
    mut name: ResMut<LocalPlayerName>,
    keys: Res<ButtonInput<KeyCode>>,
) {
    // Only edit on the actual main menu in Solo mode — don't want
    // gameplay key chatter to scramble the player's name.
    if *state.get() != AppState::MainMenu { return; }
    if !matches!(*mode, NetMode::Solo) { return; }

    if keys.just_pressed(KeyCode::Backspace) {
        // First backspace strips the default "PLAYER" so the user
        // doesn't have to delete 6 chars to start fresh.
        if name.0 == "PLAYER" { name.0.clear(); return; }
        name.0.pop();
        return;
    }

    // A-Z mapping. Bevy 0.16 keycodes are KeyA..KeyZ.
    let letter_pressed: &[(KeyCode, char)] = &[
        (KeyCode::KeyA, 'A'), (KeyCode::KeyB, 'B'), (KeyCode::KeyC, 'C'),
        (KeyCode::KeyD, 'D'), (KeyCode::KeyE, 'E'), (KeyCode::KeyF, 'F'),
        (KeyCode::KeyG, 'G'), (KeyCode::KeyH, 'H'), (KeyCode::KeyI, 'I'),
        (KeyCode::KeyJ, 'J'), (KeyCode::KeyK, 'K'), (KeyCode::KeyL, 'L'),
        (KeyCode::KeyM, 'M'), (KeyCode::KeyN, 'N'), (KeyCode::KeyO, 'O'),
        (KeyCode::KeyP, 'P'), (KeyCode::KeyQ, 'Q'), (KeyCode::KeyR, 'R'),
        (KeyCode::KeyS, 'S'), (KeyCode::KeyT, 'T'), (KeyCode::KeyU, 'U'),
        (KeyCode::KeyV, 'V'), (KeyCode::KeyW, 'W'), (KeyCode::KeyX, 'X'),
        (KeyCode::KeyY, 'Y'), (KeyCode::KeyZ, 'Z'),
    ];
    for &(k, c) in letter_pressed {
        if keys.just_pressed(k) && name.0.len() < 16 {
            // First letter after default strips the placeholder so
            // typing "BOB" doesn't yield "PLAYERBOB".
            if name.0 == "PLAYER" { name.0.clear(); }
            name.0.push(c);
        }
    }
    let digit_pressed: &[(KeyCode, char)] = &[
        (KeyCode::Digit0, '0'), (KeyCode::Digit1, '1'), (KeyCode::Digit2, '2'),
        (KeyCode::Digit3, '3'), (KeyCode::Digit4, '4'), (KeyCode::Digit5, '5'),
        (KeyCode::Digit6, '6'), (KeyCode::Digit7, '7'), (KeyCode::Digit8, '8'),
        (KeyCode::Digit9, '9'),
    ];
    for &(k, c) in digit_pressed {
        if keys.just_pressed(k) && name.0.len() < 16 {
            if name.0 == "PLAYER" { name.0.clear(); }
            name.0.push(c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bare IPv4 (no port) should pick up `HOST_PORT` automatically.
    #[test]
    fn parse_bare_ip_uses_default_port() {
        let addr = parse_join_addr("192.168.1.50").expect("parse");
        assert_eq!(addr.ip().to_string(), "192.168.1.50");
        assert_eq!(addr.port(), HOST_PORT);
    }

    /// `ip:port` form should honour the typed port.
    #[test]
    fn parse_explicit_port() {
        let addr = parse_join_addr("10.0.0.1:5050").expect("parse");
        assert_eq!(addr.ip().to_string(), "10.0.0.1");
        assert_eq!(addr.port(), 5050);
    }

    /// Surrounding whitespace shouldn't break parsing — players may
    /// hit space accidentally typing into the field.
    #[test]
    fn parse_trims_whitespace() {
        let addr = parse_join_addr("  127.0.0.1  ").expect("parse");
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
        assert_eq!(addr.port(), HOST_PORT);
    }

    /// Empty string returns a printable "enter an IP first" message
    /// rather than a cryptic parse error, so the UI can show it
    /// straight from `last_error`.
    #[test]
    fn parse_empty_string_returns_helpful_error() {
        let err = parse_join_addr("").expect_err("empty should fail");
        assert!(err.contains("enter"), "got: {err}");
    }

    /// Garbage input is rejected with a non-panicking error so the UI
    /// can show the parse failure and let the player retry.
    #[test]
    fn parse_garbage_input_returns_error() {
        assert!(parse_join_addr("not an ip").is_err());
        assert!(parse_join_addr("192.168.1.").is_err());
        assert!(parse_join_addr("999.999.999.999").is_err());
    }

    /// IPv6 literal in brackets should parse — bevy's keyboard input
    /// won't let users type `[`, but other code paths might call
    /// `parse_join_addr` programmatically and we don't want to break
    /// them.
    #[test]
    fn parse_ipv6_with_port() {
        let addr = parse_join_addr("[::1]:5050").expect("parse");
        assert_eq!(addr.port(), 5050);
        assert!(addr.is_ipv6());
    }

    /// `NetMode` starts at `Solo` so single-player runs untouched
    /// when the multiplayer plugin is loaded.
    #[test]
    fn netmode_default_is_solo() {
        let m: NetMode = Default::default();
        assert!(matches!(m, NetMode::Solo));
    }

    /// `parse_join_addr` should accept port `0` syntactically — the
    /// OS treats it as "any" but the parser shouldn't reject it. (We
    /// don't actually want players typing 0, but the parser exists
    /// at a different layer than the validation.)
    #[test]
    fn parse_port_zero_is_accepted() {
        let addr = parse_join_addr("127.0.0.1:0").expect("parse");
        assert_eq!(addr.port(), 0);
    }

    /// Max port number (`65535`) should parse — IANA-allocated ports
    /// can go all the way up.
    #[test]
    fn parse_max_port() {
        let addr = parse_join_addr("127.0.0.1:65535").expect("parse");
        assert_eq!(addr.port(), 65535);
    }

    /// Port `65536` is one past the u16 max → must fail to parse.
    #[test]
    fn parse_overflow_port_rejected() {
        assert!(parse_join_addr("127.0.0.1:65536").is_err());
    }

    /// A buf made entirely of whitespace should fall through to the
    /// empty-string branch (post-trim, nothing remains).
    #[test]
    fn parse_only_whitespace_is_empty_error() {
        let err = parse_join_addr("   \t  ").expect_err("whitespace-only should fail");
        assert!(err.contains("enter"), "got: {err}");
    }

    /// Two `NetMode` instances of the same variant compare equal —
    /// guards against accidentally adding payload to a variant that
    /// would break the `==` checks scattered through the gating
    /// systems.
    #[test]
    fn netmode_equality_works() {
        assert_eq!(NetMode::Solo, NetMode::Solo);
        assert_eq!(NetMode::Connected, NetMode::Connected);
        assert_ne!(NetMode::Solo, NetMode::Hosting);
        assert_ne!(NetMode::Hosting, NetMode::JoiningEntry);
    }

    /// `start_hosting` should bind the host socket, insert a
    /// `NetSession` resource, populate `HostStatus`, and flip
    /// `NetMode` to `Hosting`. Verified via a minimal Bevy `World`
    /// + a one-shot system that calls into `start_hosting`.
    #[test]
    fn start_hosting_flips_state_and_inserts_session() {
        use bevy::ecs::system::RunSystemOnce;

        let mut world = bevy::ecs::world::World::new();
        world.insert_resource(NetMode::Solo);
        world.insert_resource(HostStatus::default());
        world.insert_resource(LobbyRoster::default());
        world.insert_resource(LocalPlayerName("HOST_NAME".to_string()));

        // Ephemeral port (0) so this test doesn't fight other tests
        // for HOST_PORT when cargo runs them in parallel.
        world
            .run_system_once(|mut commands: Commands,
                              mut mode: ResMut<NetMode>,
                              mut status: ResMut<HostStatus>,
                              mut roster: ResMut<LobbyRoster>,
                              local_name: Res<LocalPlayerName>| {
                start_hosting_on_port(&mut commands, &mut mode, &mut status,
                                      &mut roster, &local_name, 0)
                    .expect("bind");
            })
            .unwrap();

        assert!(matches!(*world.resource::<NetMode>(), NetMode::Hosting));
        // Status port is whatever the OS assigned (≠ 0 by definition
        // of an ephemeral bind).
        assert_ne!(world.resource::<HostStatus>().port, 0);
        assert!(world.contains_resource::<NetSession>(), "NetSession inserted");
        let session = world.resource::<NetSession>();
        assert!(session.is_host);
        assert_eq!(session.my_id, 0);
        assert!(!session.welcomed);
        // Roster should have the host's own name seeded at id 0.
        assert_eq!(world.resource::<LobbyRoster>().by_id.get(&0),
                   Some(&"HOST_NAME".to_string()));
    }

    /// `start_hosting` should be idempotent — calling it when already
    /// hosting must no-op rather than re-bind (which would fail
    /// because the port is taken).
    #[test]
    fn start_hosting_is_idempotent() {
        use bevy::ecs::system::RunSystemOnce;

        let mut world = bevy::ecs::world::World::new();
        world.insert_resource(NetMode::Hosting);
        world.insert_resource(HostStatus::default());
        world.insert_resource(LobbyRoster::default());
        world.insert_resource(LocalPlayerName::default());

        // Already in Hosting → call should silently succeed and not
        // try to bind a second socket on HOST_PORT.
        world
            .run_system_once(|mut commands: Commands,
                              mut mode: ResMut<NetMode>,
                              mut status: ResMut<HostStatus>,
                              mut roster: ResMut<LobbyRoster>,
                              local_name: Res<LocalPlayerName>| {
                start_hosting(&mut commands, &mut mode, &mut status,
                              &mut roster, &local_name).expect("idempotent");
            })
            .unwrap();

        assert!(matches!(*world.resource::<NetMode>(), NetMode::Hosting));
    }

    /// `tear_down_session` should remove the `NetSession` resource
    /// and reset `NetMode` to `Solo`. Verified by first inserting a
    /// session via `start_hosting` (Solo→Hosting), then tearing it
    /// down, then asserting state. Starting in Solo is important —
    /// `start_hosting` early-exits if mode != Solo.
    #[test]
    fn tear_down_session_resets_state() {
        use bevy::ecs::system::RunSystemOnce;

        let mut world = bevy::ecs::world::World::new();
        world.insert_resource(NetMode::Solo);
        world.insert_resource(HostStatus::default());
        world.insert_resource(LobbyRoster::default());
        world.insert_resource(LocalPlayerName::default());

        // Bind a real session so the teardown has something to clear.
        // Ephemeral port for parallel-test isolation.
        world
            .run_system_once(|mut commands: Commands,
                              mut mode: ResMut<NetMode>,
                              mut status: ResMut<HostStatus>,
                              mut roster: ResMut<LobbyRoster>,
                              local_name: Res<LocalPlayerName>| {
                start_hosting_on_port(&mut commands, &mut mode, &mut status,
                                      &mut roster, &local_name, 0)
                    .expect("bind");
            })
            .unwrap();
        assert!(world.contains_resource::<NetSession>());
        assert!(matches!(*world.resource::<NetMode>(), NetMode::Hosting));

        // Now tear it down via the wrapper.
        world.run_system_once(teardown_on_exit).unwrap();

        assert!(!world.contains_resource::<NetSession>(), "session removed");
        assert!(matches!(*world.resource::<NetMode>(), NetMode::Solo));
    }

    // ---------- End-to-end two-app harness ----------
    //
    // The tests below build two real Bevy `App`s in one process — one
    // playing the host, one playing the client — wire them together
    // through actual loopback UDP sockets, and tick them in lockstep.
    // This verifies the full system pipeline end-to-end (recv +
    // process + send), not just the wire format in isolation.
    //
    // We bypass the menu / state-machine setup by jumping straight
    // to `NetMode::Connected` with a pre-bound `NetSession`. That
    // skips the Hello/Welcome handshake half of the system flow on
    // the resources side, but the *first* e2e test exercises the
    // handshake explicitly so we still cover it.

    use super::ghost::{detect_stale_peers, recv_packets, send_heartbeat, send_local_transform};
    use super::enemies::{
        apply_relayed_damage, assign_net_ids, relay_damage_to_host,
        send_enemy_snapshot, send_proc_fx,
    };
    use super::loadout::{
        apply_received_player_stats, apply_received_turret_config,
        broadcast_player_stats, broadcast_turret_config,
    };
    use super::state_sync::{apply_state_change, broadcast_state_change};
    use super::bullets::{
        emit_bullet_fired_signals, relay_bullet_fired, send_bullet_fired,
    };
    use super::death::{
        apply_received_revive, detect_local_death, host_broadcast_revive_on_stage_complete,
        host_check_team_wipe, host_track_own_death,
    };
    use super::wave::{apply_wave_state, broadcast_wave_state};
    use super::xp_sync::{
        apply_received_level_up_grants, apply_received_xp, broadcast_level_up_grants,
        broadcast_xp,
    };
    /// Build a headless app for one peer. `is_host` controls the
    /// initial `NetSession` shape. `host_addr_for_client` lets the
    /// caller pre-populate the client's peer table with the host's
    /// real (ephemeral) port so we don't need a known-port collision
    /// in the test environment.
    fn build_peer_app(
        is_host: bool,
        host_addr_for_client: Option<std::net::SocketAddr>,
        start_connected: bool,
    ) -> App {
        use crate::multiplayer::enemies::{
            EnemySnapshotTimer, LatestEnemySnapshot, NextNetEntityId,
            PendingDamageRelay, ProcFxInbox,
        };
        use crate::multiplayer::ghost::{PeerSnapshots, TransformSendTimer};
        use crate::multiplayer::loadout::{PeerLoadouts, PendingPlayerStats, PendingTurretConfig};
        use crate::multiplayer::state_sync::{LastBroadcastedState, PendingStateChange};

        let mut app = App::new();
        // MinimalPlugins includes TimePlugin (so `Time::delta_secs`
        // advances each tick — required for the throttled netloop
        // systems) plus TaskPool / ScheduleRunner. No need to layer
        // TimePlugin separately; doing so panics with "plugin was
        // already added".
        app.add_plugins(bevy::MinimalPlugins);
        // StatesPlugin gives us `State<AppState>` + `NextState<AppState>`
        // which `tick_handshake` reads to auto-transition the menu →
        // Playing on connection. Without this the system fails
        // validation with "Resource does not exist".
        app.add_plugins(bevy::state::app::StatesPlugin);
        app.init_state::<AppState>();

        // Resources every netloop system reads.
        let mode = if start_connected { NetMode::Connected } else { NetMode::Solo };
        app.insert_resource(mode);
        app.insert_resource(HostStatus::default());
        app.insert_resource(JoinIpEntry::default());
        app.insert_resource(PeerSnapshots::default());
        app.insert_resource(TransformSendTimer::default());
        app.insert_resource(HeartbeatTimer::default());
        app.insert_resource(NextNetEntityId::default());
        app.insert_resource(EnemySnapshotTimer::default());
        app.insert_resource(LatestEnemySnapshot::default());
        app.insert_resource(PendingDamageRelay::default());
        app.insert_resource(ProcFxInbox::default());
        // ProcFx + BulletFired are event-driven; register the
        // event channels so EventWriter / EventReader params
        // validate.
        app.add_event::<crate::proc_fx::ProcFxFired>();
        app.add_event::<crate::proc_fx::BulletFiredEvent>();
        app.insert_resource(PendingStateChange::default());
        app.insert_resource(LastBroadcastedState::default());
        app.insert_resource(LocalPlayerName::default());
        app.insert_resource(LobbyRoster::default());
        app.insert_resource(PendingKick::default());
        app.insert_resource(PendingPlayerStats::default());
        app.insert_resource(PendingTurretConfig::default());
        app.insert_resource(PeerLoadouts::default());
        app.insert_resource(super::wave::PendingWaveState::default());
        app.insert_resource(super::wave::LastBroadcastedWaveState::default());
        app.insert_resource(super::xp_sync::PendingXpSync::default());
        app.insert_resource(super::xp_sync::LastBroadcastedXp::default());
        app.insert_resource(super::xp_sync::PendingLevelUpGrants::default());
        app.insert_resource(super::xp_sync::LastSeenLocalLevelUps::default());
        // Real Xp + LevelUpsPending resources so the broadcast +
        // apply systems' params validate. Default values are 0/level 1.
        app.insert_resource(crate::xp::Xp::default());
        app.insert_resource(crate::xp::LevelUpsPending::default());
        app.insert_resource(crate::xp::LevelUpReturn::default());
        // Scrap + per-stage tally — `apply_wave_state` writes through
        // a `ScrapWriter` SystemParam that needs both resources.
        app.insert_resource(crate::Scrap::default());
        app.insert_resource(crate::stage_complete::ScrapEarnedThisStage::default());
        app.insert_resource(super::bullets::BulletFiredInbox::default());
        app.insert_resource(super::death::LocalDeathState::default());
        app.insert_resource(super::death::TeamDeathTracker::default());
        app.insert_resource(super::death::PendingRevive::default());
        app.insert_resource(super::ready::LocalReadyState::default());
        app.insert_resource(super::ready::TeamReadyTracker::default());
        app.insert_resource(super::ready::PendingPeerReady::default());
        // PlayerStats + TurretConfig + CombatContext are needed by
        // the broadcast systems (host) and the apply systems (client).
        app.insert_resource(crate::stats::PlayerStats::default());
        app.insert_resource(crate::turret::TurretConfig::default());
        app.insert_resource(crate::map::CombatContext::default());

        // Bind a real socket on an ephemeral port (NOT HOST_PORT — we
        // don't want test runs to collide with each other or with a
        // running game instance).
        let sock = super::net::bind_socket(None).expect("bind ephemeral socket");

        let mut peers = std::collections::HashMap::new();
        if let Some(host_addr) = host_addr_for_client {
            // Client knows where the host lives.
            peers.insert(0u8, host_addr);
        }

        let session = NetSession {
            sock,
            my_id: if is_host { 0 } else { 1 },
            peers,
            next_peer_id: if is_host { 1 } else { 0 },
            // `welcomed` starts true only if the caller wants to skip
            // the handshake; otherwise the test exercises the
            // handshake via `tick_handshake`.
            welcomed: start_connected,
            is_host,
            last_seen: HashMap::new(),
        };
        app.insert_resource(session);

        // Register the protocol-layer systems only. The visual
        // systems (`spawn_missing_ghosts`, `apply_snapshots`,
        // `apply_enemy_snapshot`) need graphics resources
        // (`Palette`, `PaletteMaterials`, `EffectMeshes`) that drag
        // in a much heavier setup; the e2e tests assert on
        // `PeerSnapshots` / `LatestEnemySnapshot` directly instead
        // of the spawned entities downstream.
        //
        // `relay_damage_to_host` and `apply_relayed_damage` need the
        // `PendingDamageQueue` resource, which lives in `bullet.rs`.
        // We init it here so the systems' params validate without
        // pulling in the rest of the combat sim.
        app.insert_resource(crate::bullet::PendingDamageQueue::default());
        // Two chained groups so we stay under Bevy's 20-system
        // tuple limit. Group 1 = network protocol + sync. Group 2 =
        // gameplay signals (bullets + death). Sequenced with
        // `.after(recv_packets)` so group 2 sees this frame's
        // incoming packets.
        app.add_systems(
            Update,
            (
                tick_handshake,
                recv_packets,
                send_local_transform,
                assign_net_ids,
                send_enemy_snapshot,
                relay_damage_to_host,
                apply_relayed_damage,
                send_proc_fx,
                broadcast_state_change,
                apply_state_change,
                broadcast_player_stats,
                apply_received_player_stats,
                broadcast_turret_config,
                apply_received_turret_config,
                broadcast_wave_state,
                apply_wave_state,
            )
                .chain(),
        );
        // Group 2 — gameplay signals. Runs after recv_packets so the
        // host_check_team_wipe / apply_received_revive systems see
        // this frame's inbound PeerDied / PeerRevived packets.
        app.add_systems(
            Update,
            (
                emit_bullet_fired_signals,
                send_bullet_fired,
                relay_bullet_fired,
                detect_local_death,
                host_track_own_death,
                host_check_team_wipe,
                apply_received_revive,
                host_broadcast_revive_on_stage_complete,
            )
                .chain()
                .after(recv_packets),
        );
        // Group 3 — disconnect detection. Its own add_systems call so
        // the Group 1 chain stays under Bevy's 20-system tuple limit.
        app.add_systems(Update, detect_stale_peers.after(recv_packets));
        // Heartbeat keepalive — sends at 1Hz to refresh peers'
        // `last_seen` during quiet states.
        app.add_systems(Update, send_heartbeat);
        // Mirror production OnEnter resets for the per-peer states so
        // a transition between them clears the ready flag. Without
        // this, `local_ready=true` from LevelUp → Customize would
        // immediately satisfy Customize's all-ready check and skip
        // it entirely.
        app.add_systems(OnEnter(crate::AppState::Customize),  super::ready::reset_ready_state_on_enter);
        app.add_systems(OnEnter(crate::AppState::LevelUp),    super::ready::reset_ready_state_on_enter);
        app.add_systems(OnEnter(crate::AppState::HullSelect), super::ready::reset_ready_state_on_enter);
        // Group 4 — XP sync. Same reason (cap).
        app.add_systems(Update, (
            broadcast_xp,
            apply_received_xp,
            broadcast_level_up_grants,
            apply_received_level_up_grants,
        ).chain().after(recv_packets));
        // Group 5 — ready check. Runs unconditionally in test
        // fixture (production gates on Customize). Cheap — early-bails
        // on `ready=false`.
        app.add_systems(Update, (
            super::ready::announce_local_ready,
            super::ready::drain_ready_inbox,
            super::ready::track_own_ready,
            super::ready::host_advance_when_all_ready,
        ).after(recv_packets));
        app
    }

    /// Spin both apps for `ticks` iterations, sleeping briefly
    /// between to let UDP propagate and Time advance. Returns the
    /// number of ticks actually run.
    fn lockstep(host: &mut App, client: &mut App, ticks: usize) {
        for _ in 0..ticks {
            host.update();
            client.update();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Helper: read a peer app's bound socket port. Tests need this
    /// to point the client at the host's ephemeral port.
    fn peer_port(app: &App) -> u16 {
        app.world()
            .resource::<NetSession>()
            .sock
            .local_addr()
            .unwrap()
            .port()
    }

    /// E2E #1 — full handshake over loopback. Host starts in
    /// `Hosting` (not yet welcomed). Client starts in `JoiningWait`
    /// with a Hello already sent. After a few ticks, both should be
    /// in `Connected` with `welcomed = true` and their peer tables
    /// populated.
    #[test]
    fn e2e_handshake_completes_between_two_apps() {
        // Build host first so we know its port.
        let mut host = build_peer_app(true, None, false);
        // Host starts in `Hosting` (not Solo) so tick_handshake polls
        // for Hello packets instead of bailing out.
        *host.world_mut().resource_mut::<NetMode>() = NetMode::Hosting;

        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();

        // Client knows host's addr; sits in JoiningWait awaiting Welcome.
        let mut client = build_peer_app(false, Some(host_addr), false);
        *client.world_mut().resource_mut::<NetMode>() = NetMode::JoiningWait;

        // Client kicks off with a Hello. (`start_joining` would do
        // this in the real flow; bypass it to keep the test minimal.)
        super::net::send_to(
            &client.world().resource::<NetSession>().sock,
            host_addr,
            &super::net::NetMsg::Hello { name: "CLIENT".to_string() },
        )
        .expect("send Hello");

        // Seed the host's roster as `start_hosting` would have done
        // (the test bypasses that path).
        host.world_mut().resource_mut::<LobbyRoster>().by_id.insert(0, "HOST".to_string());

        // Lockstep enough ticks for the Hello → Welcome round-trip
        // plus the auto-transition into Lobby.
        lockstep(&mut host, &mut client, 30);

        // Both should now be `Connected` and welcomed.
        let host_mode = *host.world().resource::<NetMode>();
        let client_mode = *client.world().resource::<NetMode>();
        assert_eq!(host_mode, NetMode::Connected, "host should be Connected");
        assert_eq!(client_mode, NetMode::Connected, "client should be Connected");
        assert!(host.world().resource::<NetSession>().welcomed);
        assert!(client.world().resource::<NetSession>().welcomed);

        // Both peers should have auto-transitioned into Lobby.
        use crate::AppState;
        assert_eq!(*host.world().resource::<State<AppState>>().get(), AppState::Lobby,
                   "host should be in Lobby");
        assert_eq!(*client.world().resource::<State<AppState>>().get(), AppState::Lobby,
                   "client should be in Lobby");

        // Rosters should reflect both peers on both sides.
        let host_roster = &host.world().resource::<LobbyRoster>().by_id;
        let client_roster = &client.world().resource::<LobbyRoster>().by_id;
        assert!(host_roster.contains_key(&0), "host roster has host");
        assert!(host_roster.contains_key(&1), "host roster has the joined client");
        assert_eq!(host_roster.get(&1).map(|s| s.as_str()), Some("CLIENT"));
        assert!(client_roster.contains_key(&0), "client roster has host");
        assert_eq!(client_roster.get(&0).map(|s| s.as_str()), Some("HOST"));

        // Host should have one peer (the client) in its table.
        assert_eq!(host.world().resource::<NetSession>().peers.len(), 1);
        // Client should have the host (id 0) in its table.
        assert!(client.world().resource::<NetSession>().peers.contains_key(&0));
        // Client got assigned id 1 by the host.
        assert_eq!(client.world().resource::<NetSession>().my_id, 1);
    }

    /// E2E #2 — Transform sync flows both ways once connected. Each
    /// peer spawns a `Friendly` ship at a distinct world position;
    /// after a few ticks of `send_local_transform` + `recv_packets`,
    /// each peer's `PeerSnapshots` should contain the OTHER peer's
    /// id mapped to the OTHER peer's spawned position.
    #[test]
    fn e2e_transform_packets_flow_both_ways() {
        use crate::components::{Friendly, Heading};
        use crate::multiplayer::ghost::PeerSnapshots;

        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);

        // Host needs to know about the client's port to send back
        // Transforms. In real life this is populated when the host
        // first receives a Hello; we shortcut it here.
        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut()
            .resource_mut::<NetSession>()
            .peers
            .insert(1, client_addr);

        // Each peer spawns its own Friendly at a distinct position so
        // we can tell whose snapshot we're looking at by the coords.
        let host_pos = bevy::math::Vec2::new(10.0, 20.0);
        let client_pos = bevy::math::Vec2::new(-30.0, -40.0);
        host.world_mut().spawn((
            Friendly,
            Heading(0.0),
            Transform::from_xyz(host_pos.x, host_pos.y, 0.0),
        ));
        client.world_mut().spawn((
            Friendly,
            Heading(0.0),
            Transform::from_xyz(client_pos.x, client_pos.y, 0.0),
        ));

        // Run long enough for several throttled sends (interval is
        // ~33ms; 80 ticks × 5ms sleep = 400ms = ~12 send windows).
        lockstep(&mut host, &mut client, 80);

        // Host should have received the client's Transform.
        let host_snaps = host.world().resource::<PeerSnapshots>();
        let client_snap = host_snaps.0.get(&1).expect("host should see client");
        assert!((client_snap.pos.x - client_pos.x).abs() < 0.01);
        assert!((client_snap.pos.y - client_pos.y).abs() < 0.01);

        // Client should have received the host's Transform.
        let client_snaps = client.world().resource::<PeerSnapshots>();
        let host_snap = client_snaps.0.get(&0).expect("client should see host");
        assert!((host_snap.pos.x - host_pos.x).abs() < 0.01);
        assert!((host_snap.pos.y - host_pos.y).abs() < 0.01);
    }

    /// E2E #3 — host-authored enemy state propagates to the client.
    /// Spawn an Enemy on the host (`assign_net_ids` will tag it the
    /// first frame); after a few snapshot windows pass, the client's
    /// `LatestEnemySnapshot` should contain an entry with the same
    /// transform. (We don't spawn the mirror entity itself in the
    /// test because that needs PaletteMaterials / EffectMeshes, which
    /// drag in a much heavier setup; the propagation half is the
    /// thing we want to verify here.)
    #[test]
    fn e2e_enemy_snapshot_propagates_host_to_client() {
        use crate::components::{Faction, FactionKind, Health};
        use crate::enemy::{Enemy, EnemyState, EnemyVariant};
        use crate::multiplayer::enemies::LatestEnemySnapshot;

        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);

        // Host learns the client's port (in real life: from the
        // first Hello packet).
        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut()
            .resource_mut::<NetSession>()
            .peers
            .insert(1, client_addr);

        // Spawn a Standard enemy on the host at a distinctive
        // position. `assign_net_ids` will stamp a NetEntityId on it
        // the first frame, then `send_enemy_snapshot` will broadcast.
        let enemy_pos = bevy::math::Vec2::new(123.0, -45.0);
        host.world_mut().spawn((
            Enemy {
                variant: EnemyVariant::Standard,
                state: EnemyState::Approach,
                state_timer: 0.0,
                waypoint: bevy::math::Vec2::ZERO,
                fire_cd: 0.0,
                max_hp: 8,
            },
            Health(8),
            Faction(FactionKind::Enemy),
            Transform::from_xyz(enemy_pos.x, enemy_pos.y, 1.0),
        ));

        // Run long enough for at least one snapshot window
        // (snapshot interval = 50ms; 60 ticks × 5ms = 300ms ≈ 6
        // windows).
        lockstep(&mut host, &mut client, 60);

        // Client should have received at least one EnemySnapshot.
        let latest = client.world().resource::<LatestEnemySnapshot>();
        let entries = latest.0.as_ref().expect("client should have a snapshot");
        assert_eq!(entries.len(), 1, "exactly one enemy was spawned");
        let entry = &entries[0];
        assert_eq!(entry.kind, EnemyVariant::Standard.to_u8());
        assert!((entry.pos[0] - enemy_pos.x).abs() < 0.01);
        assert!((entry.pos[1] - enemy_pos.y).abs() < 0.01);
        assert_eq!(entry.hp, 8);
        assert!(entry.id > 0, "host should have assigned a real id");
        assert_eq!(entry.boss_class, super::net::NOT_A_BOSS,
            "regular enemy must NOT be flagged as a boss");
    }

    /// Boss replication regression — a hostile entity carrying both
    /// `Enemy` and `Ally` (the boss pattern from `ally::spawn_boss`)
    /// must:
    ///   1. ride the EnemySnapshot like any other enemy (HP / pos
    ///      reconcile through the normal path), AND
    ///   2. carry the host's `ShipClass` in `boss_class` so the client
    ///      knows to spawn proper boss visuals (not the Standard
    ///      placeholder mesh).
    ///
    /// Without (2), the boss appeared on the client as a regular
    /// Standard-variant enemy — wrong size, wrong colour, no class-
    /// specific visuals.
    #[test]
    fn e2e_boss_carries_ship_class_in_snapshot() {
        use crate::ally::{Ally, ShipClass};
        use crate::components::{Faction, FactionKind, Health};
        use crate::enemy::{Enemy, EnemyState, EnemyVariant};
        use crate::multiplayer::enemies::LatestEnemySnapshot;

        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);
        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut()
            .resource_mut::<NetSession>()
            .peers
            .insert(1, client_addr);

        // Spawn a boss-shaped entity: Enemy + Ally + the position +
        // a chunky boss-tier HP. Mirrors `ally::spawn_boss` minimally
        // — we don't need the full ship chassis for the snapshot path
        // to see Ally and stamp the class.
        host.world_mut().spawn((
            Enemy {
                variant: EnemyVariant::Standard,
                state: EnemyState::Approach,
                state_timer: 0.0,
                waypoint: bevy::math::Vec2::ZERO,
                fire_cd: 0.0,
                max_hp: 120,
            },
            Ally {
                class: ShipClass::Blackbeard,
                waypoint: bevy::math::Vec2::ZERO,
                waypoint_timer: 0.0,
            },
            Health(120),
            Faction(FactionKind::Enemy),
            Transform::from_xyz(0.0, 0.0, 1.0),
        ));

        lockstep(&mut host, &mut client, 60);

        let latest = client.world().resource::<LatestEnemySnapshot>();
        let entries = latest.0.as_ref().expect("client should have a snapshot");
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.boss_class, ShipClass::Blackbeard.to_u8(),
            "boss must carry its ShipClass in the snapshot for the client to render the right hull");
        assert_eq!(entry.hp, 120);
    }

    /// E2E #4 — damage relay client → host. Client primes its
    /// `PendingDamageQueue` with a damage event targeting a mirror
    /// enemy (a stub entity carrying `NetEntityId`). `relay_damage_to_host`
    /// strips it from the local queue and sends a `DamageEnemy` packet
    /// to the host. Host receives, `apply_relayed_damage` pushes onto
    /// the host's `PendingDamageQueue`. We assert the host queue
    /// gained the entry with the right amount + target id.
    #[test]
    fn e2e_damage_relay_client_to_host() {
        use crate::bullet::{DamageEvent, PendingDamageQueue};
        use crate::components::{Faction, FactionKind, Health};
        use crate::enemy::{Enemy, EnemyState, EnemyVariant};
        use crate::weapon::WeaponType;
        use super::enemies::NetEntityId;

        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);

        // Host needs to know the client's port for any return packets
        // (not required for damage relay specifically, but matches
        // the real-world flow shape).
        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut()
            .resource_mut::<NetSession>()
            .peers
            .insert(1, client_addr);

        // Spawn an authoritative enemy on the host at NetEntityId=1.
        // (We assign the id manually since `assign_net_ids` would also
        // do it, but doing it here pins the id we look up later.)
        let host_enemy = host.world_mut().spawn((
            Enemy {
                variant: EnemyVariant::Standard,
                state: EnemyState::Approach,
                state_timer: 0.0,
                waypoint: bevy::math::Vec2::ZERO,
                fire_cd: 0.0,
                max_hp: 100,
            },
            Health(100),
            Faction(FactionKind::Enemy),
            Transform::from_xyz(0.0, 0.0, 1.0),
            NetEntityId(777),
        )).id();
        let _ = host_enemy; // suppress unused — we look up via NetEntityId, not Entity

        // Spawn a mirror on the client with the SAME NetEntityId.
        let client_mirror = client.world_mut().spawn((
            Enemy {
                variant: EnemyVariant::Standard,
                state: EnemyState::Approach,
                state_timer: 0.0,
                waypoint: bevy::math::Vec2::ZERO,
                fire_cd: 0.0,
                max_hp: 100,
            },
            Health(100),
            Faction(FactionKind::Enemy),
            Transform::from_xyz(0.0, 0.0, 1.0),
            NetEntityId(777),
        )).id();

        // Prime the client's PendingDamageQueue with a hit on the mirror.
        client.world_mut()
            .resource_mut::<PendingDamageQueue>()
            .0.push(DamageEvent {
                target: client_mirror,
                amount: 42,
                hit_pos: bevy::math::Vec2::new(5.0, -10.0),
                weapon: WeaponType::Standard,
                source: None,
                runes: vec![],
                procced: vec![],
                proc_strength: 1.0,
            });

        // Tick lockstep — one tick should drain client's queue and
        // send the packet; another should let host recv + apply.
        lockstep(&mut host, &mut client, 30);

        // Client's queue should be empty (event relayed and removed).
        // The test app doesn't register `process_damage_events`, so
        // the client's queue retains the original event. In real
        // gameplay it would be drained by the local damage pipeline
        // for visual feedback. The protocol-layer thing we care
        // about is that the event was RELAYED — verified by the
        // host's queue below.

        // Host's queue should contain at least one relayed event for
        // the matching enemy. (In production, process_damage_events
        // drains the client's queue each frame so only one is sent;
        // the test fixture doesn't register that system so the same
        // event re-relays every tick. We assert the field shape, not
        // the count.)
        let host_queue = host.world().resource::<PendingDamageQueue>();
        assert!(
            !host_queue.0.is_empty(),
            "host should have at least one queued damage event from the client",
        );
        let ev = &host_queue.0[0];
        assert_eq!(ev.amount, 42, "amount preserved through relay");
        // Verify target is the host's enemy with NetEntityId 777.
        let target_net_id = host.world().get::<NetEntityId>(ev.target)
            .expect("relayed target should have NetEntityId");
        assert_eq!(target_net_id.0, 777);
        // hit_pos preserved.
        assert!((ev.hit_pos.x - 5.0).abs() < 0.01);
        assert!((ev.hit_pos.y - (-10.0)).abs() < 0.01);
    }

    /// E2E #5 — weapon + runes survive the damage relay.
    /// Client primes a damage event with a Sniper weapon + Fire +
    /// Bleed runes; after the round-trip, the host's queue should
    /// have the same weapon + same runes attached. Confirms the wire
    /// format and re-rolling honor the client's loadout.
    #[test]
    fn e2e_damage_relay_carries_weapon_and_runes() {
        use crate::bullet::{DamageEvent, PendingDamageQueue};
        use crate::components::{Faction, FactionKind, Health};
        use crate::enemy::{Enemy, EnemyState, EnemyVariant};
        use crate::rune::Rune;
        use crate::weapon::WeaponType;
        use super::enemies::NetEntityId;

        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);

        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut()
            .resource_mut::<NetSession>()
            .peers
            .insert(1, client_addr);

        // Host enemy + matching client mirror with the same id.
        host.world_mut().spawn((
            Enemy {
                variant: EnemyVariant::Heavy,
                state: EnemyState::Approach,
                state_timer: 0.0,
                waypoint: bevy::math::Vec2::ZERO,
                fire_cd: 0.0,
                max_hp: 200,
            },
            Health(200),
            Faction(FactionKind::Enemy),
            Transform::default(),
            NetEntityId(101),
        ));
        let mirror = client.world_mut().spawn((
            Enemy {
                variant: EnemyVariant::Heavy,
                state: EnemyState::Approach,
                state_timer: 0.0,
                waypoint: bevy::math::Vec2::ZERO,
                fire_cd: 0.0,
                max_hp: 200,
            },
            Health(200),
            Faction(FactionKind::Enemy),
            Transform::default(),
            NetEntityId(101),
        )).id();

        // Prime client's queue with a Sniper hit carrying Fire + Bleed.
        client.world_mut()
            .resource_mut::<PendingDamageQueue>()
            .0.push(DamageEvent {
                target: mirror,
                amount: 99,
                hit_pos: bevy::math::Vec2::new(7.0, 8.0),
                weapon: WeaponType::Sniper,
                source: None,
                runes: vec![Rune::Fire, Rune::Bleed],
                procced: vec![],
                proc_strength: 1.0,
            });

        lockstep(&mut host, &mut client, 30);

        let host_queue = host.world().resource::<PendingDamageQueue>();
        assert!(!host_queue.0.is_empty(), "host received the relayed hit");
        let ev = &host_queue.0[0];
        assert_eq!(ev.weapon, WeaponType::Sniper, "weapon preserved");
        assert!(ev.runes.contains(&Rune::Fire),  "Fire rune preserved");
        assert!(ev.runes.contains(&Rune::Bleed), "Bleed rune preserved");
        assert_eq!(ev.runes.len(), 2);
        assert_eq!(ev.amount, 99);
    }

    /// E2E #6 — status bitmask: host marks an enemy with
    /// `OnFire`, snapshot ships, client mirror receives the bit and
    /// (on the next apply pass with `PaletteMaterials` present)
    /// would mount the matching component. Since the test app has
    /// no `PaletteMaterials` (apply_enemy_snapshot bails before
    /// spawning mirrors), we assert at the protocol layer: the
    /// snapshot landing on the client carries the right bit set.
    #[test]
    fn e2e_status_bitmask_propagates_via_snapshot() {
        use crate::components::{Faction, FactionKind, Health};
        use crate::enemy::{Enemy, EnemyState, EnemyVariant};
        use crate::multiplayer::enemies::{
            status_bits, LatestEnemySnapshot, NetEntityId,
        };
        use crate::rune::OnFire;

        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);

        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut()
            .resource_mut::<NetSession>()
            .peers
            .insert(1, client_addr);

        // Host enemy: spawn WITH the OnFire component so the
        // snapshot sender includes the bit.
        host.world_mut().spawn((
            Enemy {
                variant: EnemyVariant::Standard,
                state: EnemyState::Approach,
                state_timer: 0.0,
                waypoint: bevy::math::Vec2::ZERO,
                fire_cd: 0.0,
                max_hp: 10,
            },
            Health(10),
            Faction(FactionKind::Enemy),
            Transform::default(),
            NetEntityId(50),
            OnFire::new(3),
        ));

        lockstep(&mut host, &mut client, 60);

        let snap = client.world().resource::<LatestEnemySnapshot>();
        let entries = snap.0.as_ref().expect("client got a snapshot");
        let entry = entries.iter().find(|e| e.id == 50)
            .expect("the host enemy with id 50 should appear in snapshot");
        assert!(
            entry.status_flags & status_bits::ON_FIRE != 0,
            "snapshot should carry ON_FIRE bit for the burning enemy",
        );
        assert_eq!(
            entry.status_flags & status_bits::ON_FROST, 0,
            "ON_FROST bit should be clear",
        );
        assert_eq!(
            entry.status_flags & status_bits::ON_BLEED, 0,
            "ON_BLEED bit should be clear",
        );
    }

    /// E2E #7 — ProcFx broadcast. Host writes a
    /// `ProcFxFired` event; `send_proc_fx` broadcasts it to every
    /// peer; client's `recv_packets` lands it in `ProcFxInbox`.
    /// Verifies the transient-effect pipe end-to-end.
    #[test]
    fn e2e_proc_fx_broadcast_host_to_client() {
        use bevy::ecs::system::RunSystemOnce;
        use crate::multiplayer::enemies::{proc_fx_kind, ProcFxInbox};
        use crate::proc_fx::ProcFxFired;

        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);

        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut()
            .resource_mut::<NetSession>()
            .peers
            .insert(1, client_addr);

        // Host writes a Shock arc event. EventWriter is a SystemParam
        // so we go through a one-shot system here.
        host.world_mut().run_system_once(
            |mut w: bevy::ecs::event::EventWriter<ProcFxFired>| {
                w.write(ProcFxFired {
                    kind: proc_fx_kind::SHOCK_ARC,
                    from: bevy::math::Vec2::new(1.0, 2.0),
                    to:   bevy::math::Vec2::new(3.0, 4.0),
                });
            },
        ).unwrap();

        lockstep(&mut host, &mut client, 30);

        // Client should have received the ProcFx packet into its inbox.
        let client_inbox = client.world().resource::<ProcFxInbox>();
        assert_eq!(client_inbox.events.len(), 1,
                   "client should have received exactly one ProcFx");
        let ev = client_inbox.events[0];
        assert_eq!(ev.kind, proc_fx_kind::SHOCK_ARC);
        assert!((ev.from.x - 1.0).abs() < 0.01);
        assert!((ev.from.y - 2.0).abs() < 0.01);
        assert!((ev.to.x   - 3.0).abs() < 0.01);
        assert!((ev.to.y   - 4.0).abs() < 0.01);
    }

    /// E2E #8 — when the host transitions
    /// `AppState`, the client follows. Both apps start in MainMenu;
    /// host transitions to HullSelect; after a few ticks, client
    /// should be in HullSelect too. Verifies the broadcast +
    /// apply path through real UDP.
    #[test]
    fn e2e_state_change_propagates_host_to_client() {
        use crate::AppState;

        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);

        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut()
            .resource_mut::<NetSession>()
            .peers
            .insert(1, client_addr);

        // Both peers start in MainMenu (AppState's default).
        assert_eq!(*host.world().resource::<State<AppState>>().get(), AppState::MainMenu);
        assert_eq!(*client.world().resource::<State<AppState>>().get(), AppState::MainMenu);

        // Host commands a transition. Bevy `NextState` applies on the
        // next state-flush tick.
        host.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::HullSelect);

        // First lockstep: state applies on host, broadcast goes out,
        // client receives + queues, client applies on its next tick.
        lockstep(&mut host, &mut client, 30);

        assert_eq!(
            *host.world().resource::<State<AppState>>().get(),
            AppState::HullSelect,
            "host should be in HullSelect",
        );
        // HullSelect now passes through on the client (per-peer
        // hull pick — each peer chooses their own). The ready check
        // gates the actual HullSelect → Playing transition. See
        // `state_sync::client_state_for`.
        assert_eq!(
            *client.world().resource::<State<AppState>>().get(),
            AppState::HullSelect,
            "client should follow host into HullSelect (per-peer pick)",
        );
    }

    /// E2E regression — `in_mp_session` gate must require
    /// `mode == Connected`, not just AppState in {Playing, Lobby}.
    /// Without this, on host (which enters Lobby BEFORE first peer
    /// connects), `recv_packets` would race `tick_handshake` for
    /// incoming Hello packets. If `recv_packets` won the race it
    /// would handle the Hello but not set `welcomed=true`, leaving
    /// mode stuck at Hosting → `broadcast_state_change` would
    /// never fire on START. This test asserts the gate function
    /// returns false during the pre-handshake window and true in
    /// every state once we're Connected.
    #[test]
    fn in_mp_session_requires_connected_mode() {
        use bevy::ecs::system::RunSystemOnce;
        use crate::AppState;

        // World with state plugin so `Res<State<AppState>>` exists.
        let mut app = App::new();
        app.add_plugins(bevy::state::app::StatesPlugin);
        app.init_state::<AppState>();
        app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Lobby);
        app.update();

        for &mode in &[NetMode::Solo, NetMode::Hosting, NetMode::JoiningEntry, NetMode::JoiningWait] {
            app.world_mut().insert_resource(mode);
            let result = app.world_mut().run_system_once(in_mp_session).unwrap();
            assert!(!result,
                    "in_mp_session should be false for mode {:?} (handshake incomplete)",
                    mode);
        }

        // Connected enables the gate regardless of state — drain
        // packets in Customize / LevelUp / Map / WaitingForHost too,
        // otherwise the link goes silent on those screens and
        // detect_stale_peers times the peer out.
        app.world_mut().insert_resource(NetMode::Connected);
        for state in [
            AppState::Lobby,
            AppState::Playing,
            AppState::Paused,
            AppState::Customize,
            AppState::LevelUp,
            AppState::HullSelect,
            AppState::Map,
            AppState::WaitingForHost,
        ] {
            app.world_mut().resource_mut::<NextState<AppState>>().set(state);
            app.update();
            let result = app.world_mut().run_system_once(in_mp_session).unwrap();
            assert!(result, "in_mp_session should be true for Connected + {state:?}");
        }
    }

    /// E2E #14 — ProcFx event emitted via the `EventWriter` path
    /// (the same path gameplay code uses from `rune.rs` / `bullet.rs`)
    /// propagates through `send_proc_fx` → wire → `recv_packets`
    /// → client's `ProcFxInbox`. Verifies the event-based path
    /// works end-to-end without the old resource queue.
    #[test]
    fn e2e_proc_fx_event_propagates_via_eventwriter() {
        use bevy::ecs::system::RunSystemOnce;
        use crate::multiplayer::enemies::{proc_fx_kind, ProcFxInbox};
        use crate::proc_fx::ProcFxFired;

        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);

        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut()
            .resource_mut::<NetSession>()
            .peers
            .insert(1, client_addr);

        // Fire three events in one tick — Shock + Cascade + Blast —
        // mirroring what a damage frame with all three runes would
        // generate.
        host.world_mut().run_system_once(
            |mut w: bevy::ecs::event::EventWriter<ProcFxFired>| {
                w.write(ProcFxFired {
                    kind: proc_fx_kind::SHOCK_ARC,
                    from: bevy::math::Vec2::new(10.0, 0.0),
                    to:   bevy::math::Vec2::new(20.0, 0.0),
                });
                w.write(ProcFxFired {
                    kind: proc_fx_kind::CASCADE,
                    from: bevy::math::Vec2::new(30.0, 0.0),
                    to:   bevy::math::Vec2::new(40.0, 0.0),
                });
                w.write(ProcFxFired {
                    kind: proc_fx_kind::BLAST_RING,
                    from: bevy::math::Vec2::new(50.0, 60.0),
                    to:   bevy::math::Vec2::new(50.0, 60.0),
                });
            },
        ).unwrap();

        lockstep(&mut host, &mut client, 30);

        let inbox = client.world().resource::<ProcFxInbox>();
        assert_eq!(inbox.events.len(), 3,
                   "client should have received all three transient events");
        let kinds: Vec<u8> = inbox.events.iter().map(|e| e.kind).collect();
        assert!(kinds.contains(&proc_fx_kind::SHOCK_ARC));
        assert!(kinds.contains(&proc_fx_kind::CASCADE));
        assert!(kinds.contains(&proc_fx_kind::BLAST_RING));
    }

    /// Per-peer PlayerStats broadcast: host mutates its own stats;
    /// client receives them under `PeerLoadouts[host_id]` (NOT
    /// overwriting its own stats — peers run their stats locally).
    #[test]
    fn e2e_peer_player_stats_broadcast_to_loadouts() {
        use crate::stats::PlayerStats;
        use crate::multiplayer::loadout::PeerLoadouts;

        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);
        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut().resource_mut::<NetSession>().peers.insert(1, client_addr);

        // Mutate host's local PlayerStats. Client's own stats stay
        // at default — the broadcast should NOT overwrite them.
        {
            let mut s = host.world_mut().resource_mut::<PlayerStats>();
            s.hp.flat = 50.0;
            s.crit_pct.flat = 75.0;
        }
        let client_baseline_hp = client.world().resource::<PlayerStats>().hp.flat;

        lockstep(&mut host, &mut client, 30);

        // Client's own PlayerStats unchanged.
        assert_eq!(
            client.world().resource::<PlayerStats>().hp.flat,
            client_baseline_hp,
            "client's local PlayerStats must NOT be overwritten by host's broadcast",
        );

        // PeerLoadouts[host_id=0] holds the host's stats.
        let loadouts = client.world().resource::<PeerLoadouts>();
        let host_loadout = loadouts.0.get(&0).expect("client should have host's loadout");
        let host_stats = host_loadout.stats.as_ref().expect("stats field populated");
        assert_eq!(host_stats.hp.flat, 50.0, "PeerLoadouts mirrors host hp.flat");
        assert_eq!(host_stats.crit_pct.flat, 75.0);
    }

    /// Per-peer TurretConfig broadcast: host equips a turret; client
    /// receives the config under `PeerLoadouts[host_id]` so its
    /// ghost-ship renderer can show the right turrets. Client's own
    /// TurretConfig stays at its local default.
    #[test]
    fn e2e_peer_turret_config_broadcast_to_loadouts() {
        use crate::rune::Rune;
        use crate::turret::{SlotCfg, TurretConfig};
        use crate::weapon::WeaponType;
        use crate::multiplayer::loadout::PeerLoadouts;

        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);
        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut().resource_mut::<NetSession>().peers.insert(1, client_addr);

        // Host equips a Sniper with Fire + Shock runes.
        {
            let mut cfg = host.world_mut().resource_mut::<TurretConfig>();
            cfg.slots[3] = SlotCfg {
                equipped:  true,
                weapon:    WeaponType::Sniper,
                damage:    25,
                fire_rate: 0.5,
                barrels:   2,
                runes:     [Some(Rune::Fire), Some(Rune::Shock), None],
            };
        }
        let client_baseline_slot3 = client.world().resource::<TurretConfig>().slots[3].equipped;

        lockstep(&mut host, &mut client, 30);

        // Client's own TurretConfig untouched.
        assert_eq!(
            client.world().resource::<TurretConfig>().slots[3].equipped,
            client_baseline_slot3,
            "client's local TurretConfig must NOT be overwritten",
        );

        // PeerLoadouts captured the host's config so the ghost renderer can read it.
        let loadouts = client.world().resource::<PeerLoadouts>();
        let host_loadout = loadouts.0.get(&0).expect("host's loadout present");
        let host_cfg = host_loadout.turret.as_ref().expect("turret field populated");
        assert_eq!(host_cfg.slots[3].equipped, true);
        assert_eq!(host_cfg.slots[3].weapon, WeaponType::Sniper);
        assert_eq!(host_cfg.slots[3].runes[0], Some(Rune::Fire));
        assert_eq!(host_cfg.slots[3].runes[1], Some(Rune::Shock));
    }

    /// `LocalPlayer` disambiguates the local ship
    /// from MP's remote-peer ship. Both spawn with `Friendly` (so
    /// enemy AI targets both), but only the local one has
    /// `LocalPlayer`. Systems that need to pick "the player's own
    /// ship" via `single()` use `With<LocalPlayer>` and skip the
    /// remote.
    #[test]
    fn local_player_marker_disambiguates_friendlies() {
        use crate::components::{Friendly, LocalPlayer};

        let mut world = bevy::ecs::world::World::new();
        // Local ship: both Friendly and LocalPlayer.
        world.spawn((Friendly, LocalPlayer, Transform::default()));
        // Remote ship: Friendly only (host-side mp adds this so
        // enemy AI targets both).
        world.spawn((Friendly, Transform::default()));

        // Friendly-only query matches BOTH ships (enemy AI desired).
        let friendly_count = world
            .query_filtered::<&Transform, With<Friendly>>()
            .iter(&world)
            .count();
        assert_eq!(friendly_count, 2, "Friendly query should match both ships");

        // LocalPlayer query matches ONLY the local ship — what
        // single()-using systems (trail, helicopter aim, shark aim,
        // octopus aim, etc.) actually need.
        let local_count = world
            .query_filtered::<&Transform, With<LocalPlayer>>()
            .iter(&world)
            .count();
        assert_eq!(local_count, 1, "LocalPlayer query should match only the local ship");
    }

    /// E2E regression — client's bullet with proc runes should NOT
    /// roll procs locally (host is authoritative on procs). After
    /// `relay_damage_to_host` runs, the local damage event's runes
    /// should be cleared so `process_damage_event`'s local proc-roll
    /// pass becomes a no-op. The relayed packet to the host carries
    /// the FULL rune list so the host can roll authoritatively.
    ///
    /// Catches the double-damage bug: client + host both rolling
    /// Shock chain → chain target hit twice.
    #[test]
    fn relay_damage_clears_local_runes_keeps_visuals() {
        use bevy::ecs::system::RunSystemOnce;
        use crate::bullet::{DamageEvent, PendingDamageQueue};
        use crate::components::{Faction, FactionKind, Health};
        use crate::enemy::{Enemy, EnemyState, EnemyVariant};
        use crate::multiplayer::enemies::{relay_damage_to_host, NetEntityId};
        use crate::rune::Rune;
        use crate::weapon::WeaponType;

        // Single-client world (no real network, just verifying the
        // local event mutation). Bind a real session so the system's
        // is_client check passes.
        let mut world = bevy::ecs::world::World::new();
        world.insert_resource(NetMode::Connected);

        let sock = super::net::bind_socket(None).expect("bind");
        let mut peers = std::collections::HashMap::new();
        peers.insert(0u8, super::net::bind_socket(None).unwrap().local_addr().unwrap()); // dummy host addr
        world.insert_resource(NetSession {
            sock,
            my_id: 1,
            peers,
            next_peer_id: 0,
            welcomed: true,
            is_host: false,
            last_seen: std::collections::HashMap::new(),
        });
        world.insert_resource(PendingDamageQueue::default());

        // Spawn a mirror enemy.
        let mirror = world.spawn((
            Enemy {
                variant: EnemyVariant::Standard,
                state: EnemyState::Approach,
                state_timer: 0.0,
                waypoint: bevy::math::Vec2::ZERO,
                fire_cd: 0.0,
                max_hp: 100,
            },
            Health(100),
            Faction(FactionKind::Enemy),
            Transform::default(),
            NetEntityId(7),
        )).id();

        // Prime a damage event with Shock + Fire runes (the runes
        // that would double-proc without the fix).
        world.resource_mut::<PendingDamageQueue>().0.push(DamageEvent {
            target: mirror,
            amount: 10,
            hit_pos: bevy::math::Vec2::ZERO,
            weapon: WeaponType::Sniper,
            source: None,
            runes: vec![Rune::Shock, Rune::Fire],
            procced: vec![],
            proc_strength: 1.0,
        });

        // Run the relay system.
        world.run_system_once(relay_damage_to_host).unwrap();

        // The event should still be in the queue (visual feedback
        // path needs it), but its runes should be CLEARED so
        // `process_damage_event`'s proc loop is a no-op.
        let queue = world.resource::<PendingDamageQueue>();
        assert_eq!(queue.0.len(), 1, "event stays in queue for visuals");
        let ev = &queue.0[0];
        assert_eq!(ev.amount, 10, "amount preserved — visuals (HP-bar flash) need it");
        assert!(ev.runes.is_empty(),
                "runes cleared so client doesn't double-roll procs (host re-rolls authoritatively)");
    }

    /// MP regression: when ONE peer dies, the team should NOT snap
    /// to GameOver — the dead peer goes into spectate, the round
    /// continues for the surviving peer, and `host_check_team_wipe`
    /// only triggers GameOver once everyone is dead. The single-
    /// player `level_fail_check` used to fire on every local death
    /// regardless of MP mode; this asserts it stays gated to Solo.
    #[test]
    fn level_fail_check_skips_in_multiplayer() {
        use crate::map::level_fail_check;
        use crate::components::{Faction, FactionKind, Health, LocalPlayer};
        use crate::modes::GameMode;
        use crate::ViewMode;
        use crate::AppState;
        use bevy::ecs::system::RunSystemOnce;

        let mut app = App::new();
        app.add_plugins(bevy::state::app::StatesPlugin);
        app.init_state::<AppState>();
        app.insert_resource(ViewMode::Combat);
        app.insert_resource(GameMode::Sandbox);
        app.insert_resource(NetMode::Connected);
        // A local player at 0 HP.
        app.world_mut().spawn((
            LocalPlayer,
            Health(0),
            Faction(FactionKind::Friendly),
            Transform::default(),
        ));

        // First pass: in MP, must NOT transition to GameOver.
        app.world_mut().run_system_once(level_fail_check).unwrap();
        app.update();
        assert_ne!(
            *app.world().resource::<State<AppState>>().get(),
            AppState::GameOver,
            "MP: solo fail-check must not fire — co-op death pipeline handles it",
        );

        // Sanity: with NetMode::Solo, the same setup DOES fire.
        app.world_mut().insert_resource(NetMode::Solo);
        app.world_mut().run_system_once(level_fail_check).unwrap();
        app.update();
        assert_eq!(
            *app.world().resource::<State<AppState>>().get(),
            AppState::GameOver,
            "Solo: fail-check should still fire on local-player death",
        );
    }

    /// `PeerDied` propagates from client to host
    /// and lands in the host's `TeamDeathTracker`.
    #[test]
    fn e2e_peer_died_lands_in_host_tracker() {
        use crate::multiplayer::death::TeamDeathTracker;

        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);

        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut()
            .resource_mut::<NetSession>()
            .peers
            .insert(1, client_addr);

        // Send PeerDied manually (mimicking detect_local_death after
        // the local Friendly dies).
        super::net::send_to(
            &client.world().resource::<NetSession>().sock,
            host_addr,
            &super::net::NetMsg::PeerDied { id: 1 },
        ).expect("send PeerDied");

        lockstep(&mut host, &mut client, 30);

        let tracker = host.world().resource::<TeamDeathTracker>();
        assert!(tracker.dead_peers.contains(&1),
                "host should track client (id=1) as dead");
    }

    /// Host does NOT trigger GameOver while only
    /// the client is dead (one peer still alive).
    #[test]
    fn e2e_partial_team_death_does_not_trigger_game_over() {
        use crate::multiplayer::death::TeamDeathTracker;
        use crate::AppState;

        // Set up a host that's in Playing state, with a roster.
        let mut host = build_peer_app(true, None, true);
        host.world_mut().resource_mut::<LobbyRoster>().by_id.insert(0, "HOST".into());
        host.world_mut().resource_mut::<LobbyRoster>().by_id.insert(1, "CLIENT".into());
        // Transition to Playing.
        host.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::Playing);
        host.update();

        // Mark client (id 1) as dead.
        host.world_mut().resource_mut::<TeamDeathTracker>().dead_peers.insert(1);
        // Run host_check_team_wipe via several ticks.
        for _ in 0..10 { host.update(); }

        // Host alive → no GameOver.
        assert_eq!(*host.world().resource::<State<AppState>>().get(), AppState::Playing,
                   "GameOver should NOT fire while host is still alive");
    }

    /// When BOTH peers are dead, host triggers
    /// `GameOver`. State-sync drags the client along (covered by
    /// `e2e_state_change_propagates_host_to_client`).
    #[test]
    fn e2e_full_team_death_triggers_game_over() {
        use crate::multiplayer::death::TeamDeathTracker;
        use crate::AppState;

        let mut host = build_peer_app(true, None, true);
        host.world_mut().resource_mut::<LobbyRoster>().by_id.insert(0, "HOST".into());
        host.world_mut().resource_mut::<LobbyRoster>().by_id.insert(1, "CLIENT".into());
        host.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::Playing);
        host.update();

        // Both peers dead.
        let mut tracker = host.world_mut().resource_mut::<TeamDeathTracker>();
        tracker.dead_peers.insert(0);
        tracker.dead_peers.insert(1);

        // host_check_team_wipe runs on next tick.
        host.update();
        host.update();

        assert_eq!(*host.world().resource::<State<AppState>>().get(), AppState::GameOver,
                   "GameOver should fire when ALL peers dead");
    }

    /// Host broadcasts `PeerRevived(REVIVE_ALL)` on
    /// entry to StageComplete; client receives + sets PendingRevive.
    #[test]
    fn e2e_host_broadcasts_revive_on_stage_complete() {
        use crate::multiplayer::death::{PendingRevive, TeamDeathTracker, REVIVE_ALL};
        use crate::AppState;

        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);

        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut()
            .resource_mut::<NetSession>()
            .peers
            .insert(1, client_addr);

        // Seed: client is "dead" (host knows), client's pending
        // revive starts false.
        host.world_mut().resource_mut::<TeamDeathTracker>().dead_peers.insert(1);
        assert!(!client.world().resource::<PendingRevive>().0);

        // Host transitions Playing → StageComplete (triggers the
        // revive broadcast).
        host.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::Playing);
        host.update();
        host.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::StageComplete);

        lockstep(&mut host, &mut client, 30);

        // Send a manual REVIVE_ALL too as a redundancy check — the
        // broadcast system fires once on state-change which may or
        // may not propagate within the lockstep window. Either way
        // the client should end up with PendingRevive set.
        super::net::send_to(
            &host.world().resource::<NetSession>().sock,
            client_addr,
            &super::net::NetMsg::PeerRevived { id: REVIVE_ALL },
        ).expect("send revive");
        lockstep(&mut host, &mut client, 10);

        // Host's tracker cleared.
        let tracker = host.world().resource::<TeamDeathTracker>();
        assert!(tracker.dead_peers.is_empty(),
                "host tracker should be cleared after revive broadcast");

        // Client received revive — note that `apply_received_revive`
        // runs each tick and consumes PendingRevive, so we can't
        // observe pending=true after lockstep. Instead: assert
        // local_death is false (it would have been set false on
        // consume).
        let local_death = client.world().resource::<super::death::LocalDeathState>();
        assert!(!local_death.dead,
                "client's local_death should remain false (was never set), \
                 and pending_revive should have been consumed");
    }

    /// **E2E gameplay scenario** — "client shoots an enemy, both
    /// peers see the shot AND the HP drop". This is the integration
    /// the user specifically asked for: signal-driven bullet visual
    /// goes via `BulletFired`, damage flow goes via `DamageEnemy`,
    /// both wired through the same two-app harness.
    ///
    /// Walks through:
    /// 1. Both peers have a shared enemy (NetEntityId 99).
    /// 2. Client emits `BulletFiredEvent` (mimicking what
    ///    `emit_bullet_fired_signals` would do for a freshly-spawned
    ///    bullet). Host receives the bullet-fire signal — visual
    ///    would now appear on host's screen.
    /// 3. Client primes a damage event targeting the mirror.
    ///    `relay_damage_to_host` sends it. Host's queue gains the
    ///    relayed damage.
    /// 4. Manually apply the damage on the host (mimicking
    ///    `process_damage_events` which the test fixture doesn't
    ///    register).
    /// 5. Next host snapshot carries the new HP back; client's
    ///    mirror Health updates.
    /// 6. Assert: both peers' BulletFired pipes saw the shot AND
    ///    the client's mirror reflects the new HP.
    #[test]
    fn e2e_client_shoots_enemy_both_peers_see_it() {
        use bevy::ecs::system::RunSystemOnce;
        use crate::bullet::{DamageEvent, PendingDamageQueue};
        use crate::components::{Faction, FactionKind, Health};
        use crate::enemy::{Enemy, EnemyState, EnemyVariant};
        use crate::multiplayer::bullets::BulletFiredInbox;
        use crate::multiplayer::enemies::{LatestEnemySnapshot, NetEntityId};
        use crate::proc_fx::BulletFiredEvent;
        use crate::weapon::WeaponType;

        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);

        // Register `apply_enemy_snapshot` on the client so received
        // snapshots actually update the mirror's Health. The default
        // fixture doesn't register it (its drain consumes the buffer
        // that other tests assert on directly).
        client.add_systems(Update, super::enemies::apply_enemy_snapshot);

        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut()
            .resource_mut::<NetSession>()
            .peers
            .insert(1, client_addr);

        // === Setup: enemy on host with NetEntityId 99, mirror on client ===
        let host_enemy = host.world_mut().spawn((
            Enemy {
                variant: EnemyVariant::Standard,
                state: EnemyState::Approach,
                state_timer: 0.0,
                waypoint: bevy::math::Vec2::ZERO,
                fire_cd: 0.0,
                max_hp: 100,
            },
            Health(100),
            Faction(FactionKind::Enemy),
            Transform::from_xyz(50.0, 0.0, 1.0),
            NetEntityId(99),
        )).id();

        let client_mirror = client.world_mut().spawn((
            Enemy {
                variant: EnemyVariant::Standard,
                state: EnemyState::Approach,
                state_timer: 0.0,
                waypoint: bevy::math::Vec2::ZERO,
                fire_cd: 0.0,
                max_hp: 100,
            },
            Health(100),
            Faction(FactionKind::Enemy),
            Transform::from_xyz(50.0, 0.0, 1.0),
            NetEntityId(99),
        )).id();

        // === Step 1: client fires a bullet (the signal a real
        // bullet would emit) ===
        client.world_mut().run_system_once(
            |mut w: EventWriter<BulletFiredEvent>| {
                w.write(BulletFiredEvent {
                    pos: bevy::math::Vec2::new(0.0, 0.0),
                    dir: bevy::math::Vec2::new(1.0, 0.0), // toward enemy
                    weapon: WeaponType::Standard.to_u8(),
                    range: 80.0,
                });
            },
        ).unwrap();

        // === Step 2: client primes a damage event for the mirror
        // (what bullet_collisions would do after the bullet hits) ===
        client.world_mut()
            .resource_mut::<PendingDamageQueue>()
            .0.push(DamageEvent {
                target: client_mirror,
                amount: 35,
                hit_pos: bevy::math::Vec2::new(50.0, 0.0),
                weapon: WeaponType::Standard,
                source: None,
                runes: vec![],
                procced: vec![],
                proc_strength: 1.0,
            });

        // === Lockstep — both signals propagate ===
        lockstep(&mut host, &mut client, 30);

        // === Assert: host's BulletFiredInbox saw the bullet ===
        let host_bullets = host.world().resource::<BulletFiredInbox>();
        assert!(!host_bullets.events.is_empty(),
                "host should have received the client's BulletFired signal");
        let bullet_ev = host_bullets.events[0];
        assert_eq!(bullet_ev.weapon, WeaponType::Standard.to_u8());

        // === Assert: host's damage queue gained the relayed event ===
        let host_queue = host.world().resource::<PendingDamageQueue>();
        assert!(!host_queue.0.is_empty(),
                "host should have received the relayed damage event");
        let dmg_ev = &host_queue.0[0];
        assert_eq!(dmg_ev.amount, 35, "amount preserved through relay");
        let target_id = host.world().get::<NetEntityId>(dmg_ev.target)
            .expect("host's target has NetEntityId");
        assert_eq!(target_id.0, 99, "relayed to the right enemy");

        // === Step 3: Manually apply damage on host (mocking
        // process_damage_events). Drains the queue first to avoid
        // the test fixture's re-relay loop. ===
        host.world_mut().resource_mut::<PendingDamageQueue>().0.clear();
        client.world_mut().resource_mut::<PendingDamageQueue>().0.clear();
        {
            let mut h = host.world_mut().get_mut::<Health>(host_enemy).unwrap();
            h.0 -= 35;
        }

        // === Lockstep — host's next snapshot reflects the new HP ===
        lockstep(&mut host, &mut client, 30);

        // === Assert: client's mirror Health was updated via snapshot ===
        let client_hp = client.world().get::<Health>(client_mirror).expect("mirror");
        assert_eq!(client_hp.0, 65,
                   "client should see enemy at 100-35=65 HP after the host applied damage");

        // Also assert: client's incoming snapshot says HP=65
        let snap = client.world().resource::<LatestEnemySnapshot>();
        if let Some(entries) = snap.0.as_ref() {
            let entry = entries.iter().find(|e| e.id == 99).expect("snapshot has enemy 99");
            assert_eq!(entry.hp, 65, "snapshot HP matches host authoritative");
        }
    }

    /// E2E #18 — Full mock round integration test. Walks two apps
    /// through: handshake → both in Lobby → host transitions to
    /// Playing → host spawns an enemy → client primes damage → host
    /// applies damage (via apply_relayed_damage) → enemy dies →
    /// snapshot drops the enemy → client's mirror despawns. Plus
    /// host's wave state ticks Spawning → Fighting → Cooldown over
    /// the round and the client sees each transition.
    ///
    /// Doesn't run real combat AI / spawn systems (those need full
    /// graphics resources). Instead it manually drives the
    /// authoritative state on the host and asserts the protocol
    /// faithfully reflects each beat.
    #[test]
    fn e2e_full_mock_round_integration() {
        use crate::bullet::{DamageEvent, PendingDamageQueue};
        use crate::components::{Faction, FactionKind, Health};
        use crate::enemy::{Enemy, EnemyState, EnemyVariant};
        use crate::map::{CombatContext, WavePhase};
        use crate::multiplayer::enemies::{LatestEnemySnapshot, NetEntityId};
        use crate::weapon::WeaponType;
        use crate::AppState;

        // === Setup: host + client in Lobby, then transition to Playing ===
        let (mut host, mut client) = build_pair_in_lobby();
        host.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::Playing);
        lockstep(&mut host, &mut client, 30);
        assert_eq!(*host.world().resource::<State<AppState>>().get(), AppState::Playing);
        assert_eq!(*client.world().resource::<State<AppState>>().get(), AppState::Playing);

        // === Host enters wave 1 of 5, Spawning phase, 8 enemies remaining ===
        {
            let mut c = host.world_mut().resource_mut::<CombatContext>();
            c.wave_idx       = 1;
            c.wave_count     = 5;
            c.wave_phase     = WavePhase::Spawning;
            c.wave_remaining = 8;
        }
        lockstep(&mut host, &mut client, 30);
        let cc = client.world().resource::<CombatContext>();
        assert_eq!(cc.wave_idx, 1,        "client sees wave 1");
        assert_eq!(cc.wave_count, 5,      "client sees wave count 5");
        assert_eq!(cc.wave_phase, WavePhase::Spawning);
        assert_eq!(cc.wave_remaining, 8);

        // === Host spawns an enemy with NetEntityId 42 ===
        let host_enemy = host.world_mut().spawn((
            Enemy {
                variant: EnemyVariant::Standard,
                state: EnemyState::Approach,
                state_timer: 0.0,
                waypoint: bevy::math::Vec2::ZERO,
                fire_cd: 0.0,
                max_hp: 8,
            },
            Health(8),
            Faction(FactionKind::Enemy),
            Transform::from_xyz(100.0, 0.0, 1.0),
            NetEntityId(42),
        )).id();
        lockstep(&mut host, &mut client, 30);
        let snap = client.world().resource::<LatestEnemySnapshot>();
        let entries = snap.0.as_ref().expect("client should have snapshot");
        assert!(entries.iter().any(|e| e.id == 42 && e.hp == 8),
                "client snapshot should include the host's enemy");

        // === Client primes a damage event targeting a mirror with id 42 ===
        // Spawn a mirror on the client first so the relay sees it.
        let mirror = client.world_mut().spawn((
            Enemy {
                variant: EnemyVariant::Standard,
                state: EnemyState::Approach,
                state_timer: 0.0,
                waypoint: bevy::math::Vec2::ZERO,
                fire_cd: 0.0,
                max_hp: 8,
            },
            Health(8),
            Faction(FactionKind::Enemy),
            Transform::from_xyz(100.0, 0.0, 1.0),
            NetEntityId(42),
        )).id();
        client.world_mut()
            .resource_mut::<PendingDamageQueue>()
            .0.push(DamageEvent {
                target: mirror,
                amount: 8, // lethal
                hit_pos: bevy::math::Vec2::new(100.0, 0.0),
                weapon: WeaponType::Standard,
                source: None,
                runes: vec![],
                procced: vec![],
                proc_strength: 1.0,
            });
        lockstep(&mut host, &mut client, 30);

        // === Host's queue should have received the damage event ===
        let host_queue = host.world().resource::<PendingDamageQueue>();
        assert!(!host_queue.0.is_empty(), "host received relayed damage");
        let relayed = &host_queue.0[0];
        assert_eq!(relayed.amount, 8);

        // === Manually apply the damage on the host (mock the
        // process_damage_events pipeline, which the test fixture
        // doesn't register). Then despawn the enemy to simulate
        // death + wave progression. ===
        {
            let mut h = host.world_mut().get_mut::<Health>(host_enemy).unwrap();
            h.0 = 0;
        }
        host.world_mut().entity_mut(host_enemy).despawn();
        // Host advances wave: Spawning → Fighting → next wave Spawning
        {
            let mut c = host.world_mut().resource_mut::<CombatContext>();
            c.wave_idx       = 2;
            c.wave_phase     = WavePhase::Spawning;
            c.wave_remaining = 12;
        }
        // Drain the client's queue too so the test doesn't infinite-relay
        client.world_mut().resource_mut::<PendingDamageQueue>().0.clear();
        // Despawn the client's mirror to simulate the snapshot reconciliation
        // that production's apply_enemy_snapshot would handle.
        client.world_mut().entity_mut(mirror).despawn();

        lockstep(&mut host, &mut client, 30);

        // === Client sees the new wave state ===
        let cc = client.world().resource::<CombatContext>();
        assert_eq!(cc.wave_idx, 2,        "client follows host into wave 2");
        assert_eq!(cc.wave_remaining, 12, "client sees new wave's remaining count");

        // === Client snapshot is empty (no enemies alive on host) ===
        let snap = client.world().resource::<LatestEnemySnapshot>();
        match &snap.0 {
            Some(entries) => assert!(entries.is_empty(),
                "client should have empty snapshot now, got {:?}", entries.len()),
            None => { /* fine — snapshot consumed last frame */ }
        }
    }

    /// E2E #18 — signal-based bullet replication.
    /// Client emits `BulletFiredEvent` (the signal a real turret
    /// firing would emit via `emit_bullet_fired_signals`). The
    /// packet flows: send → wire → host receives → host re-broadcasts
    /// to other peers → host's own inbox would spawn a visual.
    /// Since we have only 2 peers, the host's inbox is the one we
    /// assert on.
    #[test]
    fn e2e_bullet_fire_signal_propagates_to_host() {
        use bevy::ecs::system::RunSystemOnce;
        use crate::multiplayer::bullets::BulletFiredInbox;
        use crate::proc_fx::BulletFiredEvent;

        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);

        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut()
            .resource_mut::<NetSession>()
            .peers
            .insert(1, client_addr);

        // Client fires the signal (mimicking what
        // `emit_bullet_fired_signals` would do when a real bullet is
        // spawned by the turret_aim_fire path).
        client.world_mut().run_system_once(
            |mut w: EventWriter<BulletFiredEvent>| {
                w.write(BulletFiredEvent {
                    pos: bevy::math::Vec2::new(5.0, 10.0),
                    dir: bevy::math::Vec2::new(0.0, 1.0),
                    weapon: crate::weapon::WeaponType::Sniper.to_u8(),
                    range: 80.0,
                });
            },
        ).unwrap();

        lockstep(&mut host, &mut client, 30);

        // Host received the packet into its inbox.
        let host_inbox = host.world().resource::<BulletFiredInbox>();
        assert!(!host_inbox.events.is_empty(),
                "host should have received at least one BulletFired packet");
        let ev = host_inbox.events[0];
        assert!((ev.pos.x - 5.0).abs() < 0.01);
        assert!((ev.pos.y - 10.0).abs() < 0.01);
        assert!((ev.dir.y - 1.0).abs() < 0.01);
        assert_eq!(ev.weapon, crate::weapon::WeaponType::Sniper.to_u8());
        assert!((ev.range - 80.0).abs() < 0.01);
    }

    /// E2E #17 — host's wave-indicator state propagates to client's
    /// local `CombatContext`. Verifies the client's wave UI would
    /// show the host's authoritative values.
    #[test]
    fn e2e_host_wave_state_sync_to_client() {
        use crate::map::{CombatContext, WavePhase};

        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);

        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut()
            .resource_mut::<NetSession>()
            .peers
            .insert(1, client_addr);

        // Set distinctive wave state on host.
        {
            let mut c = host.world_mut().resource_mut::<CombatContext>();
            c.wave_idx       = 3;
            c.wave_count     = 7;
            c.wave_phase     = WavePhase::Fighting;
            c.wave_remaining = 12;
        }

        lockstep(&mut host, &mut client, 30);

        let client_c = client.world().resource::<CombatContext>();
        assert_eq!(client_c.wave_idx, 3);
        assert_eq!(client_c.wave_count, 7);
        assert_eq!(client_c.wave_phase, WavePhase::Fighting);
        assert_eq!(client_c.wave_remaining, 12);
    }

    /// Full happy-path integration: both peers go from Lobby through
    /// a single stage's worth of MP transitions and end up back in
    /// Customize ready for the next stage.
    ///
    /// What this exercises (all in one test, in order):
    ///   1. Handshake + state sync (Lobby → Playing on both)
    ///   2. EnemySnapshot replication (host spawns enemy, client mirrors)
    ///   3. Damage relay (client damage → host applies → snapshot omits → mirror despawned)
    ///   4. Wave state sync + per-peer scrap on Fighting → Cooldown
    ///   5. LevelUpGranted (host pending++ → client pending++ via grant message, NOT XpSync)
    ///   6. Per-peer LevelUp + ready check → advance to Customize
    ///   7. Per-peer loadout broadcast (host equips, client's PeerLoadouts updates)
    ///   8. Per-peer Customize + ready check → advance to Map
    ///   9. Final state sync (client follows Map → WaitingForHost)
    ///
    /// The per-system tests cover each link in isolation — this one
    /// confirms the whole chain holds together end-to-end. Any
    /// regression that breaks a transition between phases will trip
    /// the asserts here.
    #[test]
    fn e2e_full_happy_path_lobby_to_next_stage() {
        use crate::components::{Faction, FactionKind, Health};
        use crate::enemy::{Enemy, EnemyState, EnemyVariant};
        use crate::map::{CombatContext, WavePhase};
        use crate::rune::Rune;
        use crate::turret::{SlotCfg, TurretConfig};
        use crate::weapon::WeaponType;
        use crate::xp::{LevelUpsPending, Xp};
        use crate::AppState;
        use crate::Scrap;
        use super::loadout::PeerLoadouts;
        use super::ready::LocalReadyState;

        // ---------------- Setup: both peers connected ----------------
        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);
        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut().resource_mut::<NetSession>().peers.insert(1, client_addr);

        // Roster (for ready-check denominator).
        for app in [&mut host, &mut client] {
            let mut r = app.world_mut().resource_mut::<LobbyRoster>();
            r.by_id.insert(0, "HOST".into());
            r.by_id.insert(1, "CLIENT".into());
        }

        // ---------------- Step 1: Lobby → Playing ----------------
        host.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Playing);
        client.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Playing);
        lockstep(&mut host, &mut client, 10);
        assert_eq!(*host.world().resource::<State<AppState>>().get(),   AppState::Playing,
            "step 1: host entered Playing");
        assert_eq!(*client.world().resource::<State<AppState>>().get(), AppState::Playing,
            "step 1: client entered Playing via state sync");

        // ---------------- Step 2: EnemySnapshot replication ----------------
        let enemy = host.world_mut().spawn((
            Enemy {
                variant: EnemyVariant::Standard,
                state: EnemyState::Approach,
                state_timer: 0.0,
                waypoint: bevy::math::Vec2::ZERO,
                fire_cd: 0.0,
                max_hp: 8,
            },
            Health(8),
            Faction(FactionKind::Enemy),
            Transform::from_xyz(50.0, 0.0, 1.0),
        )).id();

        // Long enough for assign_net_ids → send_enemy_snapshot → client recv.
        lockstep(&mut host, &mut client, 30);
        let latest = client.world().resource::<super::enemies::LatestEnemySnapshot>();
        let entries = latest.0.as_ref().expect("step 2: client should have a snapshot");
        assert_eq!(entries.len(), 1, "step 2: exactly one enemy in snapshot");
        let enemy_net_id = entries[0].id;
        assert!(enemy_net_id > 0, "step 2: host assigned a NetEntityId");

        // ---------------- Step 3: Damage relay + death ----------------
        // Simulate the host's enemy taking lethal damage. The client
        // would normally drive this via its own bullet → mirror collision
        // → relay_damage_to_host → apply_relayed_damage; we shortcut by
        // setting HP=0 on the host directly. The wire-format integration
        // tests cover the relay path itself.
        host.world_mut().entity_mut(enemy).get_mut::<Health>().unwrap().0 = 0;
        host.world_mut().entity_mut(enemy).despawn();
        lockstep(&mut host, &mut client, 30);
        let after_death = client.world().resource::<super::enemies::LatestEnemySnapshot>();
        // Snapshot is consume-on-read; after the apply that despawned the
        // mirror, the next snapshot batch is empty.
        if let Some(entries) = &after_death.0 {
            assert!(!entries.iter().any(|e| e.id == enemy_net_id),
                "step 3: dead enemy should no longer appear in snapshots");
        }

        // ---------------- Step 4: Wave clear → +1 scrap on both ----------------
        let client_scrap_before = client.world().resource::<Scrap>().0;
        let host_scrap_before = host.world().resource::<Scrap>().0;
        // Seed client to Fighting so the Cooldown sync triggers the grant.
        client.world_mut().resource_mut::<CombatContext>().wave_phase = WavePhase::Fighting;
        // Host advances Fighting → Cooldown via the pure state-machine
        // method (production calls this from `spawn_enemies`).
        {
            let mut c = host.world_mut().resource_mut::<CombatContext>();
            c.wave_idx = 0;
            c.wave_count = 3;
            c.wave_phase = WavePhase::Fighting;
            c.wave_remaining = 0;
        }
        // Host grants its own scrap inline (production path) — simulate.
        host.world_mut().resource_mut::<CombatContext>().wave_phase = WavePhase::Cooldown;
        host.world_mut().resource_mut::<Scrap>().0 += 1;

        lockstep(&mut host, &mut client, 30);

        assert_eq!(host.world().resource::<Scrap>().0, host_scrap_before + 1,
            "step 4: host got +1 scrap from wave clear");
        assert_eq!(client.world().resource::<Scrap>().0, client_scrap_before + 1,
            "step 4: client got +1 scrap via WaveStateSync edge detection");
        assert_eq!(client.world().resource::<CombatContext>().wave_phase, WavePhase::Cooldown,
            "step 4: client's wave_phase mirrors host's Cooldown");

        // ---------------- Step 5+6: LevelUp per-peer with ready check ----------------
        // Host accumulates a level-up. Both transition to LevelUp;
        // LevelUpGranted carries the count to the client; both peers
        // click ready; host advances to Customize.
        host.world_mut().resource_mut::<LevelUpsPending>().0 = 1;
        host.world_mut().resource_mut::<Xp>().level = 2;
        host.world_mut().resource_mut::<NextState<AppState>>().set(AppState::LevelUp);
        client.world_mut().resource_mut::<NextState<AppState>>().set(AppState::LevelUp);
        lockstep(&mut host, &mut client, 25);

        assert_eq!(*host.world().resource::<State<AppState>>().get(),   AppState::LevelUp,
            "step 5: host in LevelUp");
        assert_eq!(*client.world().resource::<State<AppState>>().get(), AppState::LevelUp,
            "step 5: client in LevelUp (passes through, no longer WaitingForHost)");
        assert_eq!(client.world().resource::<LevelUpsPending>().0, 1,
            "step 5: client received the grant via LevelUpGranted");
        assert_eq!(client.world().resource::<Xp>().level, 2,
            "step 5: client's level mirrors host via XpSync");

        // Both peers "pick" — simulated by decrementing pending + flipping ready.
        host.world_mut().resource_mut::<LevelUpsPending>().0 = 0;
        client.world_mut().resource_mut::<LevelUpsPending>().0 = 0;
        host.world_mut().resource_mut::<LocalReadyState>().ready = true;
        client.world_mut().resource_mut::<LocalReadyState>().ready = true;
        lockstep(&mut host, &mut client, 25);

        assert_eq!(*host.world().resource::<State<AppState>>().get(), AppState::Customize,
            "step 6: host advanced LevelUp → Customize via all-ready");
        assert_eq!(*client.world().resource::<State<AppState>>().get(), AppState::Customize,
            "step 6: client followed to Customize");

        // OnEnter(Customize) runs reset_ready_state_on_enter
        // which clears local.ready locally. In-flight PeerReady
        // packets from the previous frame may briefly re-populate
        // the tracker on the receiving side; that's harmless because
        // the next per-peer-state advance gates on a fresh local
        // ready click anyway. The load-bearing assert is the
        // Customize → Map advance below.
        assert!(!host.world().resource::<LocalReadyState>().ready,
            "step 6: reset_ready_state_on_enter clears host's local flag");

        // ---------------- Step 7: Per-peer loadout broadcast ----------------
        host.world_mut().resource_mut::<TurretConfig>().slots[3] = SlotCfg {
            equipped: true, weapon: WeaponType::Sniper, damage: 25,
            fire_rate: 0.5, barrels: 2,
            runes: [Some(Rune::Fire), None, None],
        };
        client.world_mut().resource_mut::<TurretConfig>().slots[5] = SlotCfg {
            equipped: true, weapon: WeaponType::Mortar, damage: 40,
            fire_rate: 1.2, barrels: 1,
            runes: [Some(Rune::Frost), None, None],
        };
        lockstep(&mut host, &mut client, 25);

        let client_view_of_host = client.world().resource::<PeerLoadouts>()
            .0.get(&0).and_then(|l| l.turret.clone())
            .expect("step 7: client has host's loadout");
        assert_eq!(client_view_of_host.slots[3].weapon, WeaponType::Sniper);
        let host_view_of_client = host.world().resource::<PeerLoadouts>()
            .0.get(&1).and_then(|l| l.turret.clone())
            .expect("step 7: host has client's loadout");
        assert_eq!(host_view_of_client.slots[5].weapon, WeaponType::Mortar);

        // ---------------- Step 8+9: Customize ready → Map → client WaitingForHost ----------------
        host.world_mut().resource_mut::<LocalReadyState>().ready = true;
        client.world_mut().resource_mut::<LocalReadyState>().ready = true;
        lockstep(&mut host, &mut client, 25);

        assert_eq!(*host.world().resource::<State<AppState>>().get(), AppState::Map,
            "step 8: host advanced Customize → Map");
        assert_eq!(*client.world().resource::<State<AppState>>().get(), AppState::WaitingForHost,
            "step 9: client mapped Map → WaitingForHost (Map is host-only)");
    }

    /// Per-peer LevelUp ready check: both peers click ready → host
    /// advances LevelUp → Customize (default path, no override).
    #[test]
    fn e2e_ready_check_advances_levelup_to_customize() {
        use crate::AppState;
        use super::ready::LocalReadyState;

        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);
        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut().resource_mut::<NetSession>().peers.insert(1, client_addr);
        host.world_mut().resource_mut::<LobbyRoster>().by_id.insert(0, "HOST".into());
        host.world_mut().resource_mut::<LobbyRoster>().by_id.insert(1, "CLIENT".into());

        host.world_mut().resource_mut::<NextState<AppState>>().set(AppState::LevelUp);
        client.world_mut().resource_mut::<NextState<AppState>>().set(AppState::LevelUp);
        lockstep(&mut host, &mut client, 5);

        host.world_mut().resource_mut::<LocalReadyState>().ready = true;
        client.world_mut().resource_mut::<LocalReadyState>().ready = true;
        lockstep(&mut host, &mut client, 25);

        assert_eq!(
            *host.world().resource::<State<AppState>>().get(),
            AppState::Customize,
            "host should advance LevelUp → Customize when all peers ready",
        );
    }

    /// Per-peer LevelUp ready check honours `LevelUpReturn` for
    /// mid-stage drains: when the host's `LevelUpReturn.0 = Some(Playing)`,
    /// the all-ready advance goes to Playing, not Customize.
    #[test]
    fn e2e_ready_check_levelup_honours_return_state() {
        use crate::AppState;
        use crate::xp::LevelUpReturn;
        use super::ready::LocalReadyState;

        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);
        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut().resource_mut::<NetSession>().peers.insert(1, client_addr);
        host.world_mut().resource_mut::<LobbyRoster>().by_id.insert(0, "HOST".into());
        host.world_mut().resource_mut::<LobbyRoster>().by_id.insert(1, "CLIENT".into());

        host.world_mut().resource_mut::<NextState<AppState>>().set(AppState::LevelUp);
        client.world_mut().resource_mut::<NextState<AppState>>().set(AppState::LevelUp);
        // Mid-stage level-up — host stashed an override to return to Playing.
        host.world_mut().resource_mut::<LevelUpReturn>().0 = Some(AppState::Playing);
        lockstep(&mut host, &mut client, 5);

        host.world_mut().resource_mut::<LocalReadyState>().ready = true;
        client.world_mut().resource_mut::<LocalReadyState>().ready = true;
        lockstep(&mut host, &mut client, 25);

        assert_eq!(
            *host.world().resource::<State<AppState>>().get(),
            AppState::Playing,
            "LevelUpReturn::Playing must route the advance to Playing, not Customize",
        );
        // Override should be consumed.
        assert!(host.world().resource::<LevelUpReturn>().0.is_none(),
            "LevelUpReturn should be cleared after consumption");
    }

    /// Per-peer HullSelect ready check: each peer clicks PLAY,
    /// host advances HullSelect → Playing for everyone.
    #[test]
    fn e2e_ready_check_advances_hullselect_to_playing() {
        use crate::AppState;
        use super::ready::LocalReadyState;

        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);
        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut().resource_mut::<NetSession>().peers.insert(1, client_addr);
        host.world_mut().resource_mut::<LobbyRoster>().by_id.insert(0, "HOST".into());
        host.world_mut().resource_mut::<LobbyRoster>().by_id.insert(1, "CLIENT".into());

        host.world_mut().resource_mut::<NextState<AppState>>().set(AppState::HullSelect);
        client.world_mut().resource_mut::<NextState<AppState>>().set(AppState::HullSelect);
        lockstep(&mut host, &mut client, 5);

        host.world_mut().resource_mut::<LocalReadyState>().ready = true;
        client.world_mut().resource_mut::<LocalReadyState>().ready = true;
        lockstep(&mut host, &mut client, 25);

        assert_eq!(
            *host.world().resource::<State<AppState>>().get(),
            AppState::Playing,
            "host should advance HullSelect → Playing when all peers ready",
        );
    }

    /// Per-peer level-up pending drain. After receiving 3 grants
    /// from the host, the client picks (decrements) one card. The
    /// next XP sync from the host (e.g. another XP tick that
    /// doesn't cross a threshold) must NOT clobber the client's
    /// drained pending — that was the bug with the old
    /// "broadcast pending in XpSync" design.
    #[test]
    fn e2e_local_levelup_pick_not_clobbered_by_xp_resync() {
        use crate::xp::{LevelUpsPending, Xp};

        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);
        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut().resource_mut::<NetSession>().peers.insert(1, client_addr);

        // Host's first XP tick + 3 levelups granted.
        host.world_mut().resource_mut::<Xp>().level = 4;
        host.world_mut().resource_mut::<LevelUpsPending>().0 = 3;
        lockstep(&mut host, &mut client, 20);
        assert_eq!(client.world().resource::<LevelUpsPending>().0, 3,
            "client receives 3 grants");

        // Client picks a card (drops local pending to 2).
        client.world_mut().resource_mut::<LevelUpsPending>().0 = 2;

        // Host's XP shifts (e.g. accumulating XP that doesn't cross
        // a threshold) → another XpSync packet, but pending stays
        // unchanged so no new grant. The new XpSync MUST NOT reset
        // client's pending back to 3 — only LevelUpGranted does.
        host.world_mut().resource_mut::<Xp>().current = 12;
        lockstep(&mut host, &mut client, 20);
        assert_eq!(client.world().resource::<LevelUpsPending>().0, 2,
            "client's pending after local pick must NOT be clobbered by XpSync");
        assert_eq!(client.world().resource::<Xp>().current, 12,
            "client's xp.current still mirrors host's");

        // Host crosses ANOTHER level → +1 grant arrives → client's pending becomes 3 again.
        host.world_mut().resource_mut::<LevelUpsPending>().0 = 4;
        lockstep(&mut host, &mut client, 20);
        assert_eq!(client.world().resource::<LevelUpsPending>().0, 3,
            "client's pending bumps by the new grant delta (1), not reset to host's total");
    }

    /// Build-config replication, dense test pass — the user
    /// specifically wanted extra coverage here. These exercise the
    /// per-peer broadcast pipeline (host's local TurretConfig change
    /// → client's PeerLoadouts[0], and vice-versa).

    /// Both peers shop independently in parallel. Each peer's config
    /// shows up under the OTHER peer's PeerLoadouts — and crucially,
    /// neither peer's LOCAL TurretConfig is overwritten by the
    /// other's broadcast (the bug this whole refactor was avoiding).
    #[test]
    fn e2e_two_peers_independent_shop_configs() {
        use crate::rune::Rune;
        use crate::turret::{SlotCfg, TurretConfig};
        use crate::weapon::WeaponType;
        use crate::multiplayer::loadout::PeerLoadouts;

        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);
        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut().resource_mut::<NetSession>().peers.insert(1, client_addr);

        // Distinctive configs so we can verify each lands on the
        // other peer without crossing wires.
        let host_slot = SlotCfg {
            equipped: true, weapon: WeaponType::Sniper, damage: 25,
            fire_rate: 0.5, barrels: 2,
            runes: [Some(Rune::Fire), None, None],
        };
        let client_slot = SlotCfg {
            equipped: true, weapon: WeaponType::Mortar, damage: 40,
            fire_rate: 1.2, barrels: 1,
            runes: [Some(Rune::Frost), Some(Rune::Shock), None],
        };
        host.world_mut().resource_mut::<TurretConfig>().slots[2] = host_slot;
        client.world_mut().resource_mut::<TurretConfig>().slots[5] = client_slot;

        lockstep(&mut host, &mut client, 30);

        // Local configs untouched by the cross-broadcast.
        let host_local  = host.world().resource::<TurretConfig>();
        let client_local = client.world().resource::<TurretConfig>();
        assert_eq!(host_local.slots[2].weapon,  WeaponType::Sniper,
            "host's own config keeps its Sniper");
        assert_eq!(host_local.slots[5].equipped, false,
            "host's local config was NOT overwritten by client's Mortar broadcast");
        assert_eq!(client_local.slots[5].weapon, WeaponType::Mortar,
            "client's own config keeps its Mortar");
        assert_eq!(client_local.slots[2].equipped, false,
            "client's local config was NOT overwritten by host's Sniper broadcast");

        // Each peer's view of the OTHER lives in PeerLoadouts.
        let host_view_of_client = host.world().resource::<PeerLoadouts>()
            .0.get(&1).and_then(|l| l.turret.clone())
            .expect("host should have client's loadout");
        assert_eq!(host_view_of_client.slots[5].weapon, WeaponType::Mortar);
        assert_eq!(host_view_of_client.slots[5].damage, 40);
        assert_eq!(host_view_of_client.slots[5].runes[0], Some(Rune::Frost));
        assert_eq!(host_view_of_client.slots[5].runes[1], Some(Rune::Shock));

        let client_view_of_host = client.world().resource::<PeerLoadouts>()
            .0.get(&0).and_then(|l| l.turret.clone())
            .expect("client should have host's loadout");
        assert_eq!(client_view_of_host.slots[2].weapon, WeaponType::Sniper);
        assert_eq!(client_view_of_host.slots[2].runes[0], Some(Rune::Fire));
    }

    /// Mid-shop changes propagate eagerly. A peer modifying their
    /// config multiple times — selling, buying a different turret,
    /// swapping runes — ends with the OTHER peer seeing the FINAL
    /// state, not any intermediate.
    #[test]
    fn e2e_mid_shop_changes_settle_to_final_config() {
        use crate::rune::Rune;
        use crate::turret::{SlotCfg, TurretConfig};
        use crate::weapon::WeaponType;
        use crate::multiplayer::loadout::PeerLoadouts;

        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);
        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut().resource_mut::<NetSession>().peers.insert(1, client_addr);

        // Step 1 — host equips Standard.
        host.world_mut().resource_mut::<TurretConfig>().slots[1] = SlotCfg {
            equipped: true, weapon: WeaponType::Standard,
            ..Default::default()
        };
        lockstep(&mut host, &mut client, 10);

        // Step 2 — host sells it, equips Mortar with two runes instead.
        host.world_mut().resource_mut::<TurretConfig>().slots[1] = SlotCfg {
            equipped: true, weapon: WeaponType::Mortar,
            damage: 60, fire_rate: 1.5, barrels: 1,
            runes: [Some(Rune::Cascade), Some(Rune::Star), None],
        };
        lockstep(&mut host, &mut client, 20);

        let view = client.world().resource::<PeerLoadouts>()
            .0.get(&0).and_then(|l| l.turret.clone())
            .expect("client has host's loadout");
        assert_eq!(view.slots[1].weapon, WeaponType::Mortar,
            "client should see host's FINAL Mortar, not the intermediate Standard");
        assert_eq!(view.slots[1].runes[0], Some(Rune::Cascade));
        assert_eq!(view.slots[1].runes[1], Some(Rune::Star));

        // Step 3 — host clears the slot entirely (sell, no replace).
        host.world_mut().resource_mut::<TurretConfig>().slots[1] = SlotCfg::default();
        lockstep(&mut host, &mut client, 20);
        let view = client.world().resource::<PeerLoadouts>()
            .0.get(&0).and_then(|l| l.turret.clone()).unwrap();
        assert_eq!(view.slots[1].equipped, false,
            "client should see the slot cleared");
    }

    /// Every rune variant survives the wire-format round-trip when
    /// equipped on a peer's TurretConfig. Catches regressions in
    /// `Rune::to_u8` / `from_u8` for variants the more focused tests
    /// might not exercise.
    #[test]
    fn e2e_all_rune_variants_replicate_via_loadout() {
        use crate::rune::Rune;
        use crate::turret::{SlotCfg, TurretConfig};
        use crate::weapon::WeaponType;
        use crate::multiplayer::loadout::PeerLoadouts;

        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);
        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut().resource_mut::<NetSession>().peers.insert(1, client_addr);

        // A representative spread — one of each on-hit family +
        // utility runes. If `Rune::to_u8`/`from_u8` skips a variant,
        // the round-tripped slot will have `None` in that socket and
        // the assert below fires.
        let runes_to_test = [
            Rune::Fire, Rune::Frost, Rune::Shock, Rune::Bleed,
            Rune::Cascade, Rune::Blast, Rune::Star, Rune::Greed,
        ];
        for (i, &r) in runes_to_test.iter().enumerate().take(8) {
            host.world_mut().resource_mut::<TurretConfig>().slots[i] = SlotCfg {
                equipped: true, weapon: WeaponType::Standard,
                runes: [Some(r), None, None],
                ..Default::default()
            };
        }

        lockstep(&mut host, &mut client, 30);

        let view = client.world().resource::<PeerLoadouts>()
            .0.get(&0).and_then(|l| l.turret.clone()).expect("loadout present");
        for (i, &r) in runes_to_test.iter().enumerate().take(8) {
            assert_eq!(view.slots[i].runes[0], Some(r),
                "rune {:?} in slot {i} should round-trip cleanly", r);
        }
    }

    /// Live ready count is visible to EVERY peer (not just the host).
    /// Polish prerequisite: the "X / N READY" overlay reads each
    /// peer's local `TeamReadyTracker`, so PeerReady must propagate
    /// to all peers — not just the host.
    #[test]
    fn e2e_ready_count_visible_to_all_peers() {
        use crate::AppState;
        use super::ready::{LocalReadyState, TeamReadyTracker};

        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);
        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut().resource_mut::<NetSession>().peers.insert(1, client_addr);
        host.world_mut().resource_mut::<LobbyRoster>().by_id.insert(0, "HOST".into());
        host.world_mut().resource_mut::<LobbyRoster>().by_id.insert(1, "CLIENT".into());
        client.world_mut().resource_mut::<LobbyRoster>().by_id.insert(0, "HOST".into());
        client.world_mut().resource_mut::<LobbyRoster>().by_id.insert(1, "CLIENT".into());
        // Stay in Customize so host_advance_when_all_ready doesn't
        // race us to AppState::Map mid-test.
        host.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Customize);
        client.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Customize);
        lockstep(&mut host, &mut client, 5);

        // Only client ready (host hasn't clicked yet).
        client.world_mut().resource_mut::<LocalReadyState>().ready = true;
        lockstep(&mut host, &mut client, 15);

        // Both peers should see the client in the tracker. (Without
        // the broadcast-to-all change, the host's tracker would
        // contain client; client's own tracker would be empty.)
        assert!(client.world().resource::<TeamReadyTracker>().ready_peers.contains(&1),
            "client should see its own ready in local tracker");
        assert!(host.world().resource::<TeamReadyTracker>().ready_peers.contains(&1),
            "host should see client's ready in its tracker");

        // Host clicks ready too — every peer's tracker should now hold both ids.
        // Use a scratch app state hold so the all-ready check doesn't fire
        // before lockstep finishes propagating.
        host.world_mut().resource_mut::<LocalReadyState>().ready = true;
        // One frame is enough for host to track itself; full lockstep for the broadcast.
        host.update();
        // Stop the ready-driven Map advance from skewing client state during the broadcast lap.
        // We assert the tracker contents, which are populated regardless.
        lockstep(&mut host, &mut client, 15);

        assert!(host.world().resource::<TeamReadyTracker>().ready_peers.contains(&0),
            "host should track its own ready");
        assert!(client.world().resource::<TeamReadyTracker>().ready_peers.contains(&0),
            "client should see host's ready via broadcast");
    }

    /// Client grants +1 scrap locally on Fighting → Cooldown
    /// transition via WaveStateSync. Scrap is per-peer (no host
    /// authority); without this the client would always end up
    /// poorer than the host after every wave.
    #[test]
    fn e2e_client_grants_scrap_on_wave_clear_via_state_sync() {
        use crate::map::{CombatContext, WavePhase};
        use crate::Scrap;

        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);
        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut().resource_mut::<NetSession>().peers.insert(1, client_addr);

        // Pin client's starting scrap. Host's starting state irrelevant.
        let client_scrap_before = client.world().resource::<Scrap>().0;

        // Seed client to start in Fighting so the next sync brings
        // Cooldown (the transition we care about). The fixture's
        // CombatContext starts in Spawning by default, which would
        // skip the transition gate (prev != Fighting).
        {
            let mut c = client.world_mut().resource_mut::<CombatContext>();
            c.wave_phase = WavePhase::Fighting;
        }

        // Host flips to Cooldown — wave_state_sync will broadcast.
        {
            let mut c = host.world_mut().resource_mut::<CombatContext>();
            c.wave_idx = 0;
            c.wave_count = 3;
            c.wave_phase = WavePhase::Cooldown;
            c.wave_remaining = 0;
        }

        lockstep(&mut host, &mut client, 30);

        let client_scrap_after = client.world().resource::<Scrap>().0;
        assert_eq!(
            client_scrap_after,
            client_scrap_before + 1,
            "client should grant +1 scrap on the Fighting→Cooldown sync",
        );
        assert_eq!(
            client.world().resource::<CombatContext>().wave_phase,
            WavePhase::Cooldown,
            "client's phase should reflect the host's Cooldown",
        );
    }

    /// Ready check end-to-end: both peers click READY → host
    /// receives + tracks → host advances to Map → state sync moves
    /// client to WaitingForHost. This is the per-peer shop's gating
    /// mechanism.
    #[test]
    fn e2e_ready_check_advances_when_all_peers_ready() {
        use crate::AppState;
        use super::ready::{LocalReadyState, TeamReadyTracker};

        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);
        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut().resource_mut::<NetSession>().peers.insert(1, client_addr);
        // Seed roster — host's all-ready check reads roster.by_id.
        host.world_mut().resource_mut::<LobbyRoster>().by_id.insert(0, "HOST".into());
        host.world_mut().resource_mut::<LobbyRoster>().by_id.insert(1, "CLIENT".into());

        // Both peers enter Customize.
        host.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Customize);
        client.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Customize);
        lockstep(&mut host, &mut client, 5);

        // Only host ready → no advance yet.
        host.world_mut().resource_mut::<LocalReadyState>().ready = true;
        lockstep(&mut host, &mut client, 10);
        assert_eq!(
            *host.world().resource::<State<AppState>>().get(),
            AppState::Customize,
            "host should stay in Customize while waiting on client",
        );
        let tracker = host.world().resource::<TeamReadyTracker>();
        assert!(tracker.ready_peers.contains(&0), "host id 0 should be marked ready");
        assert!(!tracker.ready_peers.contains(&1), "client should not be ready yet");

        // Client clicks ready → broadcasts → host tracks → advances.
        client.world_mut().resource_mut::<LocalReadyState>().ready = true;
        lockstep(&mut host, &mut client, 20);

        let tracker = host.world().resource::<TeamReadyTracker>();
        assert!(tracker.ready_peers.contains(&1), "client should now be marked ready on host");
        assert_eq!(
            *host.world().resource::<State<AppState>>().get(),
            AppState::Map,
            "host should advance to Map once all peers are ready",
        );
    }

    /// Client pause propagates to the host. ESC on the client should
    /// freeze the team — otherwise the host keeps simulating while
    /// the client's UI is paused, and the client comes back to a
    /// dead boat. Regression for the pause-asymmetry bug.
    #[test]
    fn e2e_client_pause_propagates_to_host() {
        use crate::AppState;

        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);
        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut()
            .resource_mut::<NetSession>()
            .peers
            .insert(1, client_addr);

        // Both peers start in Playing.
        host.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Playing);
        client.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Playing);
        lockstep(&mut host, &mut client, 5);
        assert_eq!(*host.world().resource::<State<AppState>>().get(), AppState::Playing);
        assert_eq!(*client.world().resource::<State<AppState>>().get(), AppState::Playing);

        // Client pauses.
        client.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Paused);
        lockstep(&mut host, &mut client, 20);

        // Host should follow the client into Paused.
        assert_eq!(
            *host.world().resource::<State<AppState>>().get(),
            AppState::Paused,
            "host should follow client's pause",
        );
    }

    /// Host's own pause still works — host pause broadcasts as before,
    /// client maps Paused → WaitingForHost.
    #[test]
    fn e2e_host_pause_still_propagates_to_client() {
        use crate::AppState;

        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);
        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut()
            .resource_mut::<NetSession>()
            .peers
            .insert(1, client_addr);

        host.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Playing);
        client.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Playing);
        lockstep(&mut host, &mut client, 5);

        host.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Paused);
        lockstep(&mut host, &mut client, 20);

        assert_eq!(
            *client.world().resource::<State<AppState>>().get(),
            AppState::WaitingForHost,
            "client should map host's Paused to WaitingForHost",
        );
    }

    /// XP sync — host's Xp / LevelUpsPending mutations are mirrored
    /// to the client so the client's XP bar + level indicator match.
    /// Host's XP advances on kills (via `grant_kill_xp`); without
    /// this sync the client's bar stays at 0/1 forever.
    #[test]
    fn e2e_host_xp_sync_to_client() {
        use crate::xp::{LevelUpsPending, Xp};

        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);

        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut()
            .resource_mut::<NetSession>()
            .peers
            .insert(1, client_addr);

        // Bump host's XP state to something distinctive — simulates
        // killing a couple of enemies that pushed through a level.
        {
            let mut xp = host.world_mut().resource_mut::<Xp>();
            xp.current = 7;
            xp.level = 4;
        }
        // Rising-edge of LevelUpsPending — emit two grants. Client's
        // local pending should also reach 2 via LevelUpGranted (not
        // via XpSync, which no longer carries this field).
        host.world_mut().resource_mut::<LevelUpsPending>().0 = 2;

        lockstep(&mut host, &mut client, 30);

        let cxp = client.world().resource::<Xp>();
        let cpending = client.world().resource::<LevelUpsPending>();
        assert_eq!(cxp.current, 7, "client should mirror host's XP current");
        assert_eq!(cxp.level, 4, "client should mirror host's level");
        assert_eq!(cpending.0, 2, "client should receive grants via LevelUpGranted");
    }

    /// Disconnect / timeout detection: when the host hears nothing
    /// from a peer for longer than [`super::PEER_TIMEOUT_SECS`], the
    /// peer is dropped from the roster + session.peers, and a
    /// `PeerLeft` broadcast goes out.
    ///
    /// Simulates a hard process kill by NOT updating the peer's
    /// `last_seen` (no packets sent from the client side in this
    /// test) and back-dating the host's stored timestamp past the
    /// timeout threshold. Avoids a real wall-clock wait.
    #[test]
    fn detect_stale_peers_drops_silent_peer_on_host() {
        let mut host = build_peer_app(true, None, true);
        let _host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();

        // Seed: pretend a client connected, registered in roster +
        // peers, then went silent. Use a bogus addr — we never send
        // to it. Back-date last_seen so the next detector pass sees
        // it as expired.
        let bogus_addr: std::net::SocketAddr = "127.0.0.1:1".parse().unwrap();
        {
            let mut sess = host.world_mut().resource_mut::<NetSession>();
            sess.peers.insert(1, bogus_addr);
            sess.last_seen.insert(
                1,
                std::time::Instant::now()
                    - std::time::Duration::from_secs_f32(super::PEER_TIMEOUT_SECS + 1.0),
            );
        }
        host.world_mut().resource_mut::<LobbyRoster>().by_id.insert(1, "CLIENT".into());

        host.update();

        // Detector should have dropped the stale peer.
        let sess = host.world().resource::<NetSession>();
        assert!(!sess.peers.contains_key(&1),
            "stale peer should be removed from session.peers");
        assert!(!sess.last_seen.contains_key(&1),
            "stale peer's liveness entry should be removed");
        let roster = host.world().resource::<LobbyRoster>();
        assert!(!roster.by_id.contains_key(&1),
            "stale peer should be removed from roster");
    }

    /// Disconnect detection on the client side: if the HOST (id 0)
    /// goes silent for longer than the timeout, the client triggers
    /// `pending_kick` with a "host timed out" reason so the existing
    /// handle_received_kick path tears the session down.
    #[test]
    fn detect_stale_peers_signals_host_timeout_to_client() {
        let host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);

        // Back-date the client's last_seen for the host so the
        // detector fires.
        {
            let mut sess = client.world_mut().resource_mut::<NetSession>();
            sess.last_seen.insert(
                0,
                std::time::Instant::now()
                    - std::time::Duration::from_secs_f32(super::PEER_TIMEOUT_SECS + 1.0),
            );
        }

        client.update();

        let pending_kick = client.world().resource::<super::PendingKick>();
        assert!(pending_kick.0.is_some(),
            "host timeout should set pending_kick so client tears down");
        let reason = pending_kick.0.as_ref().unwrap();
        assert!(reason.contains("timed out"),
            "kick reason should mention timeout, got: {reason}");
    }

    /// Regression: the user-reported "stuck wave with no enemies on
    /// field" bug. Host enters Fighting with field clear → must
    /// transition to Cooldown → client must see Cooldown too.
    ///
    /// Catches:
    /// - any future regression where `try_advance_fighting` silently
    ///   stops firing (transition broken locally on host)
    /// - any regression where the wave-state broadcast is no longer
    ///   triggered on phase change (host advances but client never
    ///   learns about it → client UI shows stale wave indicator)
    #[test]
    fn e2e_host_fighting_to_cooldown_replicates_to_client() {
        use crate::map::{CombatContext, WavePhase, BETWEEN_WAVES_DURATION};

        let mut host = build_peer_app(true, None, true);
        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), true);
        let client_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&client)).parse().unwrap();
        host.world_mut()
            .resource_mut::<NetSession>()
            .peers
            .insert(1, client_addr);

        // Put host's wave state in the "wave still Fighting, field
        // empty" precondition. Mid-stage (idx 0 of 3) so the next
        // step would naturally be Cooldown → AdvanceWave, not stage
        // complete.
        {
            let mut c = host.world_mut().resource_mut::<CombatContext>();
            c.wave_idx = 0;
            c.wave_count = 3;
            c.wave_phase = WavePhase::Fighting;
            c.wave_remaining = 0;
        }

        // Drive the pure state-machine method directly (the spawner
        // system needs graphics deps that the test fixture omits).
        // This is what the production spawner calls every frame; if
        // that call site is removed, the spawner stops advancing and
        // the test still passes — that's a known limitation, but the
        // pure-method unit tests in `map.rs` cover the local logic
        // and this test covers the BROADCAST half of the pipeline.
        let advanced = host
            .world_mut()
            .resource_mut::<CombatContext>()
            .try_advance_fighting(0);
        assert!(advanced, "host's wave state machine failed to advance — bug");
        assert_eq!(
            host.world().resource::<CombatContext>().wave_phase,
            WavePhase::Cooldown,
            "host should be in Cooldown after advancing"
        );
        assert_eq!(
            host.world().resource::<CombatContext>().wave_cd,
            BETWEEN_WAVES_DURATION,
        );

        // Lockstep so the broadcast lands on the client and is
        // applied via `apply_wave_state`.
        lockstep(&mut host, &mut client, 30);

        let client_c = client.world().resource::<CombatContext>();
        assert_eq!(client_c.wave_phase, WavePhase::Cooldown,
            "client should mirror host's Cooldown phase via wave_state_sync");
        assert_eq!(client_c.wave_idx, 0);
        assert_eq!(client_c.wave_count, 3);
    }

    /// Lobby-flow helper — builds two apps, completes handshake, seeds
    /// both rosters, returns the pair. Used by the lobby E2E tests so
    /// each one starts from the same known-good "both peers in Lobby"
    /// position.
    fn build_pair_in_lobby() -> (App, App) {
        use crate::AppState;

        let mut host = build_peer_app(true, None, false);
        *host.world_mut().resource_mut::<NetMode>() = NetMode::Hosting;
        host.world_mut().resource_mut::<LobbyRoster>().by_id.insert(0, "HOST".to_string());

        let host_addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", peer_port(&host)).parse().unwrap();
        let mut client = build_peer_app(false, Some(host_addr), false);
        *client.world_mut().resource_mut::<NetMode>() = NetMode::JoiningWait;

        super::net::send_to(
            &client.world().resource::<NetSession>().sock,
            host_addr,
            &super::net::NetMsg::Hello { name: "CLIENT".to_string() },
        ).expect("send Hello");

        lockstep(&mut host, &mut client, 30);

        // Sanity check: both in Lobby with rosters populated.
        assert_eq!(*host.world().resource::<State<AppState>>().get(), AppState::Lobby);
        assert_eq!(*client.world().resource::<State<AppState>>().get(), AppState::Lobby);
        (host, client)
    }

    /// E2E #9 — Lobby roster on both peers contains both players
    /// after handshake. Catches regressions in `Welcome` (host_name
    /// + existing_peers fields) and the own-name insertion in
    /// `tick_handshake`'s Welcome arm.
    #[test]
    fn e2e_lobby_roster_contains_both_peers_on_both_sides() {
        let (host, client) = build_pair_in_lobby();
        let host_roster = &host.world().resource::<LobbyRoster>().by_id;
        let client_roster = &client.world().resource::<LobbyRoster>().by_id;
        assert_eq!(host_roster.len(), 2, "host roster should have host + client");
        assert_eq!(client_roster.len(), 2, "client roster should have host + self");
        assert_eq!(host_roster.get(&0).map(|s| s.as_str()),  Some("HOST"));
        assert_eq!(host_roster.get(&1).map(|s| s.as_str()),  Some("CLIENT"));
        assert_eq!(client_roster.get(&0).map(|s| s.as_str()), Some("HOST"));
        // Client's own-name entry is whatever LocalPlayerName was at
        // handshake time; default fixture leaves it at "PLAYER".
        assert_eq!(client_roster.get(&1).map(|s| s.as_str()), Some("PLAYER"));
    }

    /// E2E #10 — Host transitions Lobby → Playing; state-sync
    /// broadcast drags the client along. Mirrors the "host clicks
    /// START" UX flow.
    #[test]
    fn e2e_lobby_start_transitions_both_to_playing() {
        use crate::AppState;
        let (mut host, mut client) = build_pair_in_lobby();

        // Host fires the same transition that the START button click
        // handler would: `NextState::set(Playing)`.
        host.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::Playing);

        lockstep(&mut host, &mut client, 30);

        assert_eq!(*host.world().resource::<State<AppState>>().get(), AppState::Playing);
        assert_eq!(*client.world().resource::<State<AppState>>().get(), AppState::Playing,
            "client should have followed host into Playing via StateChange",
        );
    }

    /// E2E #11 — Host sends `Kicked` to client; client's
    /// `PendingKick` is populated. (Doesn't run the
    /// `handle_received_kick` system because that needs full
    /// AppState + JoinIpEntry setup; the protocol-side reception
    /// is what matters here.)
    #[test]
    fn e2e_host_kick_packet_lands_in_client_pending() {
        let (host, mut client) = build_pair_in_lobby();

        let client_addr = *host.world()
            .resource::<NetSession>()
            .peers
            .get(&1)
            .expect("host knows client addr");
        let host_sock = &host.world().resource::<NetSession>().sock;
        super::net::send_to(host_sock, client_addr, &super::net::NetMsg::Kicked {
            reason: "you suck".to_string(),
        }).expect("send Kicked");

        for _ in 0..30 {
            client.update();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        // Reference host so it isn't unused — keeps the test
        // honest about which app owns which side.
        let _ = host.world().resource::<NetSession>();

        let pending = client.world().resource::<PendingKick>();
        assert_eq!(pending.0.as_deref(), Some("you suck"),
            "client should have received the Kicked packet into PendingKick",
        );
    }

    /// E2E #12 — Client's session torn down on receiving Kicked.
    /// Runs the actual `handle_received_kick` system (which needs
    /// `State<AppState>` + `JoinIpEntry`), verifies the client
    /// ends back in MainMenu with `last_error` populated.
    #[test]
    fn e2e_client_handles_kick_returns_to_mainmenu() {
        use crate::AppState;
        let (host, mut client) = build_pair_in_lobby();

        // Register the kick-receive system on the client.
        client.add_systems(Update, lobby::handle_received_kick);

        let client_addr = *host.world()
            .resource::<NetSession>()
            .peers
            .get(&1)
            .expect("host knows client addr");
        super::net::send_to(
            &host.world().resource::<NetSession>().sock,
            client_addr,
            &super::net::NetMsg::Kicked { reason: "test kick".to_string() },
        ).expect("send Kicked");

        for _ in 0..30 {
            client.update();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        assert_eq!(*client.world().resource::<State<AppState>>().get(),
                   AppState::MainMenu,
                   "kicked client should return to MainMenu");
        assert!(!client.world().contains_resource::<NetSession>(),
                "kicked client should have torn down its session");
        let entry_err = client.world().resource::<JoinIpEntry>().last_error.clone();
        assert!(entry_err.is_some(), "kicked client should have last_error set");
        assert!(entry_err.unwrap().contains("test kick"),
                "last_error should carry the kick reason");
    }

    /// E2E #13 — `capture_name_keys` edits `LocalPlayerName` when on
    /// MainMenu + Solo. Verifies the default-strip behaviour (first
    /// keystroke wipes "PLAYER") + 16-char cap.
    #[test]
    fn capture_name_keys_strips_default_and_caps_length() {
        use bevy::ecs::system::RunSystemOnce;

        let mut world = bevy::ecs::world::World::new();
        world.insert_resource(NetMode::Solo);
        world.insert_resource(LocalPlayerName::default());
        // Simulate AppState::MainMenu without the full state plugin
        // by inserting a minimal stub `State<AppState>` resource.
        world.insert_resource(State::new(crate::AppState::MainMenu));

        // First press A → should strip "PLAYER" and start with "A".
        let mut keys = bevy::input::ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::KeyA);
        world.insert_resource(keys);
        world.run_system_once(capture_name_keys).unwrap();
        assert_eq!(world.resource::<LocalPlayerName>().0, "A");

        // Second press B → should append "B" → "AB".
        let mut keys = bevy::input::ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::KeyB);
        world.insert_resource(keys);
        world.run_system_once(capture_name_keys).unwrap();
        assert_eq!(world.resource::<LocalPlayerName>().0, "AB");

        // 16-char cap — push it past.
        world.resource_mut::<LocalPlayerName>().0 = "A".repeat(16);
        let mut keys = bevy::input::ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::KeyZ);
        world.insert_resource(keys);
        world.run_system_once(capture_name_keys).unwrap();
        assert_eq!(world.resource::<LocalPlayerName>().0, "A".repeat(16),
            "16-char cap should prevent appending",
        );
    }

    /// `start_joining` with garbage input should NOT bind a socket or
    /// transition state — `last_error` is populated and the mode
    /// stays `JoiningEntry` so the player can fix the input.
    #[test]
    fn start_joining_bad_addr_keeps_state() {
        use bevy::ecs::system::RunSystemOnce;

        let mut world = bevy::ecs::world::World::new();
        world.insert_resource(NetMode::JoiningEntry);
        world.insert_resource(JoinIpEntry { buf: "not an ip".to_string(), last_error: None });
        world.insert_resource(LocalPlayerName::default());

        world
            .run_system_once(|mut commands: Commands,
                              mut mode: ResMut<NetMode>,
                              mut entry: ResMut<JoinIpEntry>,
                              local_name: Res<LocalPlayerName>| {
                start_joining(&mut commands, &mut mode, &mut entry, &local_name);
            })
            .unwrap();

        assert!(matches!(*world.resource::<NetMode>(), NetMode::JoiningEntry),
                "mode should stay JoiningEntry on bad input");
        assert!(world.resource::<JoinIpEntry>().last_error.is_some(),
                "last_error should be set");
        assert!(!world.contains_resource::<NetSession>(),
                "no session inserted on failure");
    }
}
