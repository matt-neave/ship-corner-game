# Multiplayer

Ship-game's multiplayer is **LAN-first peer-to-peer over UDP**, hand-rolled with `bincode` messages — no replication crate, no relay service. The whole module lives in `src/multiplayer/` and is gated `#[cfg(not(target_arch = "wasm32"))]` so the browser build stays single-player.

This document covers the design, the wire format, how to play, and what's missing.

---

## Authority model

Per-peer where it makes sense, host-authoritative for shared world state. The split is deliberate — see [[project-mp-authority-split]] memory for the why.

| What | Where it lives | How it propagates |
| ---- | -------------- | ----------------- |
| Your boat (position, heading, turret aims) | Your laptop | `Transform` packets at 30Hz; receivers ghost you |
| Your local stats, turret loadout, scrap | Your laptop | Broadcast on change; receivers store per-peer (no overwrite of own) |
| Your shop / loot rolls / RNG / customize | Your laptop | Not synced — runs independently per peer |
| Your level-up card picks | Your laptop | Local pending counter, ready-check before advance |
| Your hull pick at run start | Your laptop | Per-peer HullSelect, ready-check |
| Your autonomous units (heli, shark, octopus, flail) | Your laptop | Position snapshot at 15Hz; receivers spawn visual mirrors |
| Your bullets fired | Your laptop | `BulletFired` signal; receivers spawn damage=0 visuals |
| Enemies | **Host** runs spawn / AI / death | `EnemySnapshot` at 20Hz; clients mirror entities by `NetEntityId` |
| Damage to enemies | **Host authoritative** | Client bullets queue `DamageEnemy` with weapon + runes; host applies authoritatively |
| Damage to peers from enemy bullets | **Host detects on ghost** | Host's enemy bullets hit ghost-of-peer (Health=sentinel); `relay_ghost_damage` sends `DamagePlayer { amount, hit_pos }` to that peer |
| Stateful procs (Fire/Frost/Bleed) | **Host authoritative** | Snapshot status_flags bitmask; clients reconcile components |
| Transient procs (Shock arc, Cascade, Blast ring) | **Whoever rolls broadcasts** | `ProcFx` packet → host re-broadcasts → each peer spawns local visual |
| Wave clock | **Host authoritative** | `WaveStateSync`; client grants own scrap on Fighting→Cooldown edge |
| XP / level | **Host authoritative** | `XpSync` mirrors current+level; bar is shared |
| LevelUpsPending | **Edge-triggered from host** | `LevelUpGranted { count }` — additive, never clobbers local picks |
| AppState transitions | **Host drives** (mostly) | `StateChange`; per-peer states (Customize/LevelUp/HullSelect/GameOver/Win) pass through to clients; everything else maps to `WaitingForHost`. Pause is bidirectional. |
| Team death | **Host aggregates** | `PeerDied` → `TeamDeathTracker`; GameOver fires only when EVERY peer is dead |
| Revive on stage transition | **Host broadcasts** | `PeerRevived { id: REVIVE_ALL }` on entry to StageComplete |

**Trust model:** we trust the peers. No validation that a peer isn't lying about position or kill count. Design target is "play with a friend you trust", not anti-cheat-grade competitive play.

---

## How to play (LAN)

### Set your name first

Main menu shows a `YOUR NAME` card. Type A-Z / 0-9 to edit. Default is `PLAYER`; first keystroke wipes it. Capped at 16 chars. Name follows you into the lobby roster and rides every `Hello` packet.

### Host

1. Launch the game on laptop A.
2. (Optional) set your name.
3. Click **HOST**. You drop straight into the **Lobby**. Status banner shows `HOSTING ON 192.168.x.x:49333`.
4. Tell your friend the IP.
5. As they join, their name appears in the roster.
6. Click **START** when ready — both peers transition to **HullSelect** simultaneously. Each peer picks their own hull, clicks READY, then transitions to Playing once everyone is ready.

### Client (joiner)

1. Launch the game on laptop B (same LAN as host).
2. (Optional) set your name.
3. Click **JOIN**. Default IP is the dev-LAN address; first keystroke clears it (or backspace).
4. Type the host IP, press Enter.
5. On handshake, you land in the **Lobby** next to the host.
6. When host clicks START, both peers transition through HullSelect → ready check → Playing.

### Per-peer screens (Customize / LevelUp / HullSelect)

Both peers enter these states together (via state sync). Each peer interacts independently — own scrap, own loot, own picks. A small "X / N READY" overlay shows live ready count. The host advances to the next state only when EVERY peer has clicked READY; until then a "WAITING FOR PARTNER..." subtitle appears under your local ready button.

### Pause

Either peer can pause. The pause broadcasts so both peers freeze together. ESC on the WaitingForHost overlay is **inert** (was previously kicking peers out; don't accidentally LEAVE).

### Leaving / kicking

- **LEAVE button** — tears down session, returns to MainMenu. Notifies peers via `Bye`.
- **KICK button** (host only) — sends `Kicked { reason }`. Kicked peer returns to MainMenu with the reason shown.

### Internet play

LAN code-path works over the internet the moment the host port-forwards UDP `49333`. No relay service, no NAT-punching — symmetric NATs without forwarding won't handshake. Easiest workaround for cross-internet test: Tailscale or similar VPN — the LAN IPs work transparently.

---

## Architecture

### Module layout

```
src/multiplayer/
├── mod.rs         # MultiplayerPlugin, NetMode, NetSession, handshake,
│                  # join-IP entry, host/client setup, lobby state
├── net.rs         # UDP socket + NetMsg wire format + bincode
├── ghost.rs       # PeerSnapshots, peer Transform sync (incl. turret rotations),
│                  # ghost spawn, ghost damage relay (DamagePlayer),
│                  # autonomous unit snapshot, signal-fx (mortar/beam/flame),
│                  # heartbeat keepalive
├── enemies.rs     # NetEntityId, EnemySnapshot send/apply, mirror spawn,
│                  # damage relay (DamageEnemy), proc fx broadcast
├── bullets.rs     # BulletFired signal, send/recv/relay, homing rocket sync
├── state_sync.rs  # AppState broadcast + apply, bidirectional pause
├── loadout.rs     # Per-peer PlayerStats + TurretConfig broadcast,
│                  # PeerLoadouts for ghost visual rendering
├── wave.rs        # WaveStateSync, client-side scrap on Fighting→Cooldown
├── xp_sync.rs     # XpSync (current + level) + LevelUpGranted (additive)
├── death.rs       # LocalDeathState, TeamDeathTracker, spectator overlay,
│                  # revive on stage transition
├── ready.rs       # LocalReadyState, TeamReadyTracker, per-peer ready check
│                  # (Customize / LevelUp / HullSelect), live X/N overlay
├── lobby.rs       # Lobby + WaitingForHost UI, START/KICK/LEAVE handlers
└── ui.rs          # Main-menu status card (name editor / hosting / joining),
                   # lag indicator
```

### `NetMode` state machine

```
   ┌──────┐ click HOST                ┌──────────────┐  Hello rcvd  ┌──────────────┐
   │ Solo │ ─────────────────────────►│   Hosting    │ ───────────► │ Connected    │
   └──────┘                           │ (socket bound│              │ (host=true)  │
      ▲                               │  waiting)    │              └──────────────┘
      │                               └──────────────┘
      │
      │ click JOIN     ┌──────────────┐ Enter   ┌──────────────┐  Welcome rcvd  ┌──────────────┐
      │ ──────────────►│ JoiningEntry │ ──────► │ JoiningWait  │ ─────────────► │ Connected    │
      │                │ (typing IP)  │         │ (Hello sent) │                │ (host=false) │
      │                └──────────────┘         └──────────────┘                └──────────────┘
      │
      └──── OnEnter(MainMenu): teardown_on_exit drops session, mode→Solo
```

Every non-`Solo` state has the UDP socket bound. `Solo` is the resting state — single-player runs untouched.

### Connection lifecycle

1. **Bind.** `start_hosting` / `start_joining` create a `UdpSocket` (host on `49333`, client on ephemeral) and stash in `NetSession`. Sockets are non-blocking.
2. **Handshake.** Client sends `Hello { name }`. Host allocates peer id, replies `Welcome { your_id, host_name, existing_peers }`. Host broadcasts `PeerJoined` to other clients. Both peers set `welcomed=true`; `tick_handshake` flips `NetMode → Connected` next frame.
3. **Lobby.** Both peers see the chunky-styled roster + START / LEAVE / KICK buttons.
4. **START.** Host transitions `Lobby → HullSelect`. State sync carries the client.
5. **HullSelect (per-peer).** Each peer picks their hull, clicks READY. Host advances to `Playing` when all peers ready.
6. **Playing.** Each peer broadcasts `Transform` at 30Hz (including per-turret rotations). Host broadcasts `EnemySnapshot` at 20Hz. Each peer broadcasts `FriendlyUnitsSnapshot` at 15Hz. `Heartbeat` at 1Hz keeps idle links alive. Bullets / mortars / beams / flames fire signal packets on creation.
7. **Wave clear.** Host advances Fighting → Cooldown. Wave state syncs. Clients grant their own +1 scrap on the edge.
8. **Stage clear.** Host enters StageComplete → optional BossReward → optional LevelUp → Customize → Map. Per-peer states (LevelUp / Customize) gate exit on team ready-check.
9. **Death.** Local death sets `LocalDeathState.dead = true`, despawns local Friendly + autonomous units, sends `PeerDied`. Host aggregates; GameOver fires only when EVERY peer dead. Survivor sees a "YOU DIED — WAITING FOR PARTNER" overlay; dead peer respawns on next stage transition (`PeerRevived { REVIVE_ALL }`).
10. **Disconnect.** `Bye` on clean exit. `detect_stale_peers` removes peers silent for > 5s (with heartbeat keeping liveness fresh). Client receiving `Bye` from host triggers `PendingKick("host disconnected")`.

### Tear-down on `OnEnter(MainMenu)` (NOT `OnExit(Playing)`)

`teardown_on_exit` + `despawn_all_ghosts` + `despawn_all_mirrors` + `reset_death_state` are hooked to `OnEnter(MainMenu)`. Hooking on `OnExit(Playing)` was a bug — pause / customize / levelup / hullselect all leave Playing too, and tearing the session down on those transitions would send Bye to peers and drop the local mode to Solo.

### Why hand-rolled instead of `bevy_replicon`?

Two-player position sync is ~150 lines with raw UDP + `bincode`. The wire format is small, stable, and bytes-on-the-wire testable. Replicon is server-authoritative by default; per-peer-authoritative motion would fight the grain. For the design target (2-4 players, LAN, "trust your friend"), the raw approach is the right size. Reconsider if the wire format grows past a couple hundred LOC.

---

## Wire format

Every packet is a single bincode-serialized `NetMsg`. Packets are tiny (under ~50 bytes for most; the largest is `EnemySnapshot` at ~700 bytes for 30 enemies). No fragmentation, no ACKs, no sequence numbers — every snapshot is full state that supersedes the previous one. Lost packets are fine.

```rust
enum NetMsg {
    // Connection / lobby
    Hello { name: String },
    Welcome { your_id: u8, host_name: String, existing_peers: Vec<(u8, String)> },
    PeerJoined { id: u8, name: String },
    PeerLeft { id: u8 },
    Kicked { reason: String },
    Bye { id: u8 },
    Heartbeat,                                            // 1Hz keepalive

    // Per-peer motion + per-turret aims (30Hz)
    Transform { id: u8, pos: [f32; 2], rot: f32, turret_rots: [f32; 8] },

    // Host → all: enemy state (20Hz)
    EnemySnapshot { entries: Vec<EnemyEntry> },

    // Client → host: damage relay
    DamageEnemy { enemy_id: u32, amount: i32, hit_pos: [f32; 2],
                  weapon: u8, runes: Vec<u8> },

    // Host → specific peer: damage to that peer's ghost (host-side enemy bullet
    // hit ghost; relay the damage to the peer's local Friendly)
    DamagePlayer { amount: i32, hit_pos: [f32; 2] },

    // Either → others: transient procs (Shock arc, Cascade, Blast ring)
    ProcFx { kind: u8, from: [f32; 2], to: [f32; 2] },

    // State sync (host broadcasts most; client may broadcast Paused/Playing)
    StateChange { state: u8 },                            // AppState::to_u8

    // Per-peer broadcasts (every peer; receivers store keyed by sender id)
    PlayerStatsSync   { from_peer: u8, stats: SerializedPlayerStats },
    TurretConfigSync  { from_peer: u8, slots: [SerializedSlotCfg; 8] },

    // Wave state (host → all, on change)
    WaveStateSync { wave_idx: u32, wave_count: u32, phase: u8, remaining: u32 },

    // XP (host → all, on change)
    XpSync           { current: u32, level: u32 },        // shared bar
    LevelUpGranted   { count: u8 },                       // additive, edge

    // Ready check (every peer; sender_state stamps so receivers drop stale
    // packets from the previous state — phantom-ready prevention)
    PeerReady { id: u8, sender_state: u8 },

    // Co-op death
    PeerDied { id: u8 },
    PeerRevived { id: u8 },                               // REVIVE_ALL = 0xFF

    // Bullets fired (signal-driven, replaces "spawn AI on remote")
    BulletFired { pos: [f32; 2], dir: [f32; 2], weapon: u8, range: f32,
                  target_net_id: u32 },                   // 0 = no target

    // Per-peer autonomous units (heli, shark, octopus, flail head) — 15Hz
    FriendlyUnitsSnapshot { from_peer: u8, units: Vec<FriendlyUnitEntry> },

    // Signal-driven FX (don't fire Bullet entities, can't ride BulletFired)
    MortarFired { pos: [f32; 2], target: [f32; 2], weapon: u8, splash_radius: f32 },
    BeamFired   { origin: [f32; 2], dir: [f32; 2], length: f32, weapon: u8 },
    FlameTick   { pos: [f32; 2], dir: [f32; 2] },
}

struct EnemyEntry {
    id: u32,                  // NetEntityId, stable across snapshots
    kind: u8,                 // EnemyVariant::to_u8 — append-only
    pos: [f32; 2],
    rot: f32,
    hp: i32,
    status_flags: u8,         // bit 0=OnFire, 1=OnFrost, 2=OnBleed
    boss_class: u8,           // ShipClass::to_u8 for bosses; NOT_A_BOSS otherwise
}

struct FriendlyUnitEntry {
    kind: u8,                 // FriendlyUnitKind::to_u8
    pos: [f32; 2],
    rot: f32,
}
enum FriendlyUnitKind { Helicopter, Shark, Octopus, FlailHead }
```

### Append-only discriminants

Stable wire-format enums (NEVER renumber existing variants — append only):

- `EnemyVariant::to_u8` / `from_u8`
- `Rune::to_u8` / `from_u8`
- `WeaponType::to_u8` / `from_u8`
- `AppState::to_u8` / `from_u8`
- `ShipClass::to_u8` / `from_u8` (for boss replication)
- `FriendlyUnitKind::to_u8` / `from_u8`
- `proc_fx_kind` (SHOCK_ARC, CASCADE, BLAST_RING)

Each is guarded by a `*_round_trip` / `*_discriminants_are_unique` unit test in CI. `from_u8` returns `None` for unknown numbers — forward-compat for older clients receiving a future variant.

### Port

UDP `49333`. Constant in `src/multiplayer/net.rs::HOST_PORT`.

---

## Visual-spawn refactor (mirrors look identical)

Autonomous units (helicopter, shark, octopus, flail head) and ghost ships use **shared visual-spawn helpers** so peers see pixel-identical visuals without code duplication:

```rust
// pure visual — no AI / gameplay components
pub fn spawn_<unit>_visual(commands, pm, meshes, pos, ...) -> Entity;

// production sync: visual + gameplay
let e = spawn_helicopter_visual(...);
commands.entity(e).insert((Helicopter { ... }, /* AI, fire */));

// MP mirror apply: visual + marker only
let e = spawn_helicopter_visual(...);
commands.entity(e).insert(PeerUnitMirror { peer_id });
```

Helpers live in `turret/heli.rs::spawn_helicopter_visual`, `turret/sharknet.rs::spawn_shark_visual`, `octopus::spawn_octopus_visual`, `anchor_flail::spawn_flail_head_visual`. Any future visual change propagates to mirrors automatically.

Ghost ships use the same `pm.hull` material as the local ship (no cyan tint). Turret children spawn from `PeerLoadouts[peer_id].turret`. Per-turret rotations apply each frame from `PeerSnapshot.turret_rots`. Weapon-specific decorations (Blade arms, Booster ring) layer on top.

---

## Testing

```bash
cargo test --bin ship-game multiplayer
```

Current coverage (161+ tests):

- **Wire-format round-trips** for every `NetMsg` variant + nested structs
- **Discriminant stability** for `EnemyVariant`, `Rune`, `WeaponType`, `AppState`, `ShipClass`, `FriendlyUnitKind`
- **UDP loopback** — bind two `127.0.0.1` sockets, send a message, verify decode + sender address
- **Socket sanity** — `bind_socket(None)` returns ephemeral non-zero port; non-blocking
- **IP parser** — bare IPv4, `ip:port`, IPv6, whitespace, garbage rejection
- **State predicates** — `is_host_connected`, `is_client`, `in_mp_session`
- **State sync mapping** — `client_state_for` per-peer pass-through (Playing/Lobby/MainMenu/Customize/LevelUp/HullSelect/GameOver/Win); host-only menus → WaitingForHost
- **Per-peer loadout** — host + client mutate independently; broadcast deposits in `PeerLoadouts`, doesn't overwrite own resources; mid-shop changes settle to final config; every rune variant survives the round-trip
- **Ready check** — host stays until all peers ready; LevelUp honours `LevelUpReturn` override; HullSelect → Playing; ready count visible to all peers
- **Pause sync** — bidirectional; either peer pausing freezes the team; session survives pause
- **State-transition safety** — session survives all per-peer state transitions; OnEnter(MainMenu) tears down
- **Heartbeat keepalive** — idle window doesn't kick peers
- **Disconnect detection** — `detect_stale_peers` removes peers silent > timeout; host timeout signals client kick path
- **XP sync** — host's current+level mirrors to client; `LevelUpGranted` additive, not clobbered by `XpSync` resync
- **Wave clear scrap** — client grants own scrap on Fighting→Cooldown edge
- **Damage relay** — client → host via `DamageEnemy`; full rune + weapon payload; host re-applies authoritatively
- **Boss replication** — boss carries `ShipClass` in snapshot; client renders correct hull
- **Co-op death** — `PeerDied` lands in host tracker; team wipe triggers GameOver; revive on stage transition; `level_fail_check` skips in MP
- **Full happy-path integration** — `e2e_full_happy_path_lobby_to_next_stage` walks both peers through Lobby → Playing → enemy → damage → wave clear → LevelUp + ready → Customize + ready → Map
- **Full-plugin integration tests** — `multiplayer::full_plugin_tests` uses the REAL `MultiplayerPlugin::build` (not the cherry-picked test fixture) to catch system-registration bugs the cherry-picked fixture misses

If you add a new `NetMsg` variant, **add a round-trip test**. If you change a discriminant enum, **the existing tests will catch any clash or unknown-variant regression** automatically.

---

## What's NOT done

### Known gaps (visible)

- **No NAT-punching.** Internet play needs port-forwarding or a VPN like Tailscale.

### Deferred (not blocking gameplay)

- **Reconnection.** A peer that drops mid-game can't rejoin into the live state. They have to reconnect through the lobby, which won't be in `Lobby` state if the rest are mid-game.
- **Mid-game join.** Joining a host already in Playing/Customize lands the new peer with no enemy backfill, no current loadout state, etc. Probably should reject in the `Hello` arm if state != Lobby.
- **Boarders (Blackbeard boss).** When a host-side Blackbeard boss launches a boarding party at the host's ship, peers don't see the boarder dots or the rope. Damage still relays via the standard `relay_ghost_damage` path if the boss happens to target a peer's ghost. Visual-only gap.

### Future, larger work

- **Public-internet play.** Either ship a STUN/TURN relay or integrate `matchbox` / Steam Sockets. Each has ops cost.
- **More than 2 peers.** The wire format supports it (peer_id is u8); the actual gameplay UI has been built + tested with 2. Some assumptions (e.g. `roster.by_id.len()` for the ready denominator) work for N, but the lobby UI layout is two-row.
