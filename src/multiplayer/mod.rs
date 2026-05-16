//! Phase 1 LAN multiplayer: two players, each running their own
//! single-player simulation, exchanging position updates so they see
//! each other's boats moving in real time.
//!
//! Scope explicitly does NOT include:
//! - Shared enemies (each player has their own enemy sim)
//! - Shared XP / scrap / customize state
//! - Anything beyond Transform sync for the player ship
//!
//! Connection flow:
//! 1. Player A clicks HOST on the main menu → `start_hosting` binds a
//!    UDP socket on `HOST_PORT` and writes the LAN IP into
//!    [`HostStatus`] so the menu can show it.
//! 2. Player B clicks JOIN, types the host's IP, presses Enter →
//!    `start_joining` binds an ephemeral UDP socket, sends
//!    [`NetMsg::Hello`] to the host, and waits for a Welcome.
//! 3. On either side, the moment `welcomed` becomes true we transition
//!    `AppState::Playing` so both peers end up in the same screen.
//! 4. In Playing, each peer broadcasts [`NetMsg::Transform`] at
//!    `TRANSFORM_SEND_HZ` and spawns / updates ghost entities for
//!    the other side's reported position.
//!
//! Native-only: the module is gated off on `wasm32` (browsers can't
//! open UDP sockets). The WASM build stays single-player.

use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};

use bevy::prelude::*;

pub mod enemies;
pub mod ghost;
pub mod lobby;
pub mod net;
pub mod state_sync;
pub mod ui;

use crate::AppState;
use enemies::{
    apply_enemy_snapshot, apply_relayed_damage, assign_net_ids, despawn_all_mirrors,
    relay_damage_to_host, relay_proc_fx_to_peers, send_enemy_snapshot, send_proc_fx,
    spawn_proc_fx_visuals, EnemySnapshotTimer, LatestEnemySnapshot, NextNetEntityId,
    PendingDamageRelay, ProcFxInbox,
};
use state_sync::{
    apply_state_change, broadcast_state_change, LastBroadcastedState, PendingStateChange,
};
use ghost::{
    apply_snapshots, cull_stale_ghosts, despawn_all_ghosts, recv_packets, send_local_transform,
    spawn_missing_ghosts, PeerSnapshots, TransformSendTimer,
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
}

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

/// Run condition: true when the local AppState is one of the
/// active-network states (Playing or Lobby) AND the connection
/// handshake has completed (mode is Connected). Used to gate
/// `recv_packets` so it doesn't race `tick_handshake` for incoming
/// packets during the handshake window:
/// - Pre-handshake (mode = Hosting / JoiningWait): only
///   `tick_handshake` drains. It sets `welcomed=true` and flips
///   mode to Connected on first packet.
/// - Post-handshake (mode = Connected): only `recv_packets` drains.
///   `tick_handshake` bails early on Connected so the two never
///   race.
pub fn in_mp_session(state: Res<State<AppState>>, mode: Res<NetMode>) -> bool {
    matches!(*state.get(), AppState::Playing | AppState::Lobby)
        && matches!(*mode, NetMode::Connected)
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
            // Connection-handshake polling runs unconditionally while
            // we're in a connecting state. Cheap — the systems early-
            // exit on Solo.
            // PostStartup, not Startup, so `Res<PixelFont>` is
            // visible — `fonts::setup_pixel_font` inserts the
            // resource in Startup but `Commands::insert_resource`
            // only takes effect at the next sync point, so a
            // sibling Startup system can't read it.
            .add_systems(PostStartup, ui::setup_overlay)
            .add_systems(Update, (
                tick_handshake,
                capture_join_ip_keys,
                capture_name_keys,
                ui::update_overlay,
                ui::cancel_on_esc,
                // State sync — runs unconditionally so it catches
                // transitions in any AppState (MainMenu → HullSelect
                // etc.). Each handler internally short-circuits on
                // mode/session checks.
                broadcast_state_change,
                apply_state_change,
            ))
            // Gameplay netloop. `recv_packets` runs in both Playing
            // and Lobby so the socket drains while we're in the
            // lobby waiting room too (otherwise PeerJoined / Kicked
            // / etc. would pile up unread until START). Everything
            // else stays Playing-gated.
            .add_systems(Update, recv_packets.run_if(in_mp_session))
            .add_systems(Update, (
                spawn_missing_ghosts,
                apply_snapshots,
                cull_stale_ghosts,
                send_local_transform,
                assign_net_ids,
                apply_enemy_snapshot,
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
            ).run_if(in_state(AppState::Playing)))
            // On exit from Playing (death, pause-then-quit, etc.) tear
            // down ghosts + mirrors AND notify peers of clean
            // disconnect so the next session starts fresh on both
            // sides.
            .add_systems(OnExit(AppState::Playing), (
                despawn_all_ghosts,
                despawn_all_mirrors,
                teardown_on_exit,
            ))
            // ---- Lobby state lifecycle + click handlers ----
            .add_systems(OnEnter(AppState::Lobby), lobby::setup_lobby)
            .add_systems(OnExit(AppState::Lobby), lobby::teardown_lobby)
            .add_systems(Update, (
                lobby::refresh_roster,
                lobby::handle_start_click,
                lobby::handle_leave_click,
                lobby::handle_kick_click,
            ).run_if(in_state(AppState::Lobby)))
            // `handle_received_kick` runs in both Lobby and Playing
            // because a host can kick mid-game; it self-gates.
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

    use super::ghost::{recv_packets, send_local_transform};
    use super::enemies::{
        apply_relayed_damage, assign_net_ids, relay_damage_to_host,
        send_enemy_snapshot, send_proc_fx,
    };
    use super::state_sync::{apply_state_change, broadcast_state_change};
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
        app.insert_resource(NextNetEntityId::default());
        app.insert_resource(EnemySnapshotTimer::default());
        app.insert_resource(LatestEnemySnapshot::default());
        app.insert_resource(PendingDamageRelay::default());
        app.insert_resource(ProcFxInbox::default());
        // ProcFx is event-driven now; register the event channel so
        // EventWriter / EventReader params validate.
        app.add_event::<crate::proc_fx::ProcFxFired>();
        app.insert_resource(PendingStateChange::default());
        app.insert_resource(LastBroadcastedState::default());
        app.insert_resource(LocalPlayerName::default());
        app.insert_resource(LobbyRoster::default());
        app.insert_resource(PendingKick::default());

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
                // Intentionally NOT registering `relay_proc_fx_to_peers`
                // — it drains the inbox after re-broadcasting, which
                // would clobber the assertion in
                // `e2e_proc_fx_broadcast_host_to_client`. The 2-peer
                // test doesn't need re-broadcast anyway.
                broadcast_state_change,
                apply_state_change,
            )
                .chain(),
        );
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
    }

    /// E2E #4 — Phase 2.5 damage relay. Client primes its
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
        assert!(
            client.world().resource::<PendingDamageQueue>().0.is_empty(),
            "client should have relayed and removed the damage event",
        );

        // Host's queue should contain the relayed event for the
        // matching enemy. We check by amount + target lookup.
        let host_queue = host.world().resource::<PendingDamageQueue>();
        assert_eq!(
            host_queue.0.len(), 1,
            "host should have one queued damage event from the client",
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

    /// E2E #5 — Phase 2.6 weapon + runes survive the damage relay.
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
        assert_eq!(host_queue.0.len(), 1, "host received the relayed hit");
        let ev = &host_queue.0[0];
        assert_eq!(ev.weapon, WeaponType::Sniper, "weapon preserved");
        assert!(ev.runes.contains(&Rune::Fire),  "Fire rune preserved");
        assert!(ev.runes.contains(&Rune::Bleed), "Bleed rune preserved");
        assert_eq!(ev.runes.len(), 2);
        assert_eq!(ev.amount, 99);
    }

    /// E2E #6 — Phase 2.6 status bitmask: host marks an enemy with
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

    /// E2E #7 — Phase 2.6 ProcFx broadcast. Host writes a
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

    /// E2E #8 — Phase 3 foundation: when the host transitions
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
        assert_eq!(
            *client.world().resource::<State<AppState>>().get(),
            AppState::HullSelect,
            "client should have followed host into HullSelect",
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
    /// returns false during the pre-handshake window.
    #[test]
    fn in_mp_session_requires_connected_mode() {
        use bevy::ecs::system::RunSystemOnce;
        use crate::AppState;

        // World with state plugin so `Res<State<AppState>>` exists.
        let mut app = App::new();
        app.add_plugins(bevy::state::app::StatesPlugin);
        app.init_state::<AppState>();
        // Host's AppState is Lobby immediately after clicking HOST,
        // but mode is still Hosting until first client connects.
        app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Lobby);
        app.update(); // apply NextState

        for &mode in &[NetMode::Solo, NetMode::Hosting, NetMode::JoiningEntry, NetMode::JoiningWait] {
            app.world_mut().insert_resource(mode);
            let result = app.world_mut().run_system_once(in_mp_session).unwrap();
            assert!(!result,
                    "in_mp_session should be false for mode {:?} (handshake incomplete)",
                    mode);
        }

        // Only Connected enables the gate.
        app.world_mut().insert_resource(NetMode::Connected);
        let result = app.world_mut().run_system_once(in_mp_session).unwrap();
        assert!(result, "in_mp_session should be true for Connected + Lobby");

        // And not in MainMenu even if Connected.
        app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::MainMenu);
        app.update();
        let result = app.world_mut().run_system_once(in_mp_session).unwrap();
        assert!(!result, "in_mp_session should be false in MainMenu");
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
