# Multiplayer

Ship-game's multiplayer is **LAN-first peer-to-peer over UDP**, hand-rolled with `bincode` messages — no replication crate, no relay service. The whole module lives in `src/multiplayer/` and is gated `#[cfg(not(target_arch = "wasm32"))]` so the browser build stays single-player.

This document covers the design, the wire format, how to play, and what's planned next.

---

## Authority model at a glance

| What                     | Where it lives                          | How it propagates                                            |
| ------------------------ | --------------------------------------- | ------------------------------------------------------------ |
| Your boat                | Your laptop                             | You send `Transform` packets at 30 Hz; peer ghosts you       |
| Their boat               | Their laptop                            | They send `Transform`; you render a "ghost" hull             |
| Enemies                  | **Host only** runs spawn / AI / death   | Host sends `EnemySnapshot` at 20 Hz; client mirrors entities |
| Damage                   | **Host authoritative**                  | Client's bullets emit `DamageEnemy { weapon, runes }` to host; host runs the full damage pipeline including procs; HP + status bits propagate back in the next snapshot |
| Stateful procs (Fire/Frost/Bleed) | **Host authoritative**         | Snapshot carries 3-bit bitmask; client adds/removes the matching components on mirrors so local DOT systems tick |
| Transient procs (Shock arc, Cascade, Blast) | **Whoever rolls broadcasts**  | `OutgoingProcFxQueue` → `ProcFx` packet → host re-broadcasts to all peers → each renders local visual on receipt |
| AppState (menu/screens)  | **Host drives**                         | Host's `AppState` transitions emit `StateChange`; clients follow via `NextState`                          |
| Customize loadout        | **Phase 3 (in progress)** — currently divergent | Each peer has their own default hull and shop                |
| Map / waves              | **Phase 3 (later)** — currently divergent | Each peer has their own map; client sees no wave UI          |

We **trust the peers** — there's no validation that a peer isn't lying about their position or kill count. This is fine because the design target is "play with a friend you trust", not anti-cheat-grade competitive play.

---

## How to play (LAN)

### Set your name first

On the main menu, type A-Z / 0-9 to edit your display name (status banner shows `YOUR NAME: ___`). Default is `PLAYER`; the first keystroke wipes it. Capped at 16 chars. The name follows you into the lobby roster and rides every `Hello` packet.

### Host

1. Launch the game on laptop A.
2. (Optional) Set your name.
3. From the main menu, click **HOST**.
4. You drop straight into the **Lobby** screen. The status banner shows `HOSTING ON 192.168.x.x:49333` and your name appears in the roster with a `[HOST]` badge.
5. Tell your friend the IP.
6. As they join, their name appears in the roster.
7. Click **START** when ready — both peers transition to `Playing` simultaneously via state-sync broadcast.

### Client (joiner)

1. Launch the game on laptop B (same Wi-Fi / LAN as host).
2. (Optional) Set your name.
3. From the main menu, click **JOIN**.
4. Type the host IP. Allowed keys: digits, `.`, `:`. Backspace deletes; Enter submits; Esc cancels.
5. On a successful handshake, you land in the **Lobby** next to the host. Banner reads `WAITING FOR HOST TO START...`.
6. When host clicks START, you transition into `Playing` together.

### Leaving / kicking

- **LEAVE button** (or `Esc` in lobby) — tears down session, returns you to MainMenu. Notifies peers via `Bye` so their rosters shrink immediately.
- **KICK button** (host only, next to each non-host roster row) — sends `Kicked { reason }` to that peer. Kicked peer returns to MainMenu with the reason shown in the JOIN status banner's error field.

### Internet play

The same code path works over the public internet **the moment the host port-forwards UDP `49333` on their router**. The client then connects to the host's public IP. There is no relay service, no NAT-punching — if both peers are behind symmetric NATs without port forwarding, the handshake will never complete.

For a hobby use-case, port forwarding is fine. If you ever need NAT-free internet play, that's a future Phase X.

---

## Architecture

### Module layout

```
src/multiplayer/
├── mod.rs       # Plugin, NetMode state machine, NetSession resource,
│                # connect/join entry points, handshake polling.
├── net.rs       # UDP socket + NetMsg wire format + bincode serde.
├── ghost.rs     # RemoteGhost component, peer Transform sync, send/recv,
│                # ghost spawn/cull. Tags host-side ghost as Friendly so
│                # enemies engage it.
├── enemies.rs   # NetEntityId, host-side enemy snapshot send, client-side
│                # mirror spawn/update/despawn.
└── ui.rs        # bevy_ui status overlay shown during Hosting / JoiningEntry
                 # / JoiningWait states.
```

### `NetMode` state machine

```text
   ┌──────┐  click HOST                          ┌──────────────┐
   │ Solo │ ───────────────────────────────────► │   Hosting    │
   └──────┘                                       │ (socket bound│
      ▲                                           │  waiting for │
      │  ESC / disconnect                         │  Hello)      │
      │                                           └──────────────┘
      │                                                  │
      │                                  Hello received  │
      │                                                  ▼
      │                                       ┌────────────────────┐
      │                                       │  Connected (host)  │
      │                                       └────────────────────┘
      │
      │            click JOIN                  ┌─────────────────┐
      │      ─────────────────────────────►    │  JoiningEntry   │
      │                                        │ (typing IP)     │
      │                                        └─────────────────┘
      │                                                │
      │                                Enter pressed   │
      │                                                ▼
      │                                       ┌─────────────────┐
      │                                       │  JoiningWait    │
      │                                       │ (Hello sent,    │
      │                                       │  waiting for    │
      │                                       │  Welcome)       │
      │                                       └─────────────────┘
      │                                                │
      │                                Welcome rcvd    │
      │                                                ▼
      │                                       ┌─────────────────────┐
      └─────────────────────── OnExit(Playing) ──── Connected (client) ┐
                                              └─────────────────────┘
```

Every non-`Solo` state has the UDP socket bound. `Solo` is the resting state — single-player runs untouched.

### Connection lifecycle

1. **Bind.** `start_hosting` / `start_joining` create a `UdpSocket` (host on `49333`, client on an ephemeral port) and stash it in the `NetSession` resource. Both also seed `LobbyRoster` with the local name. All sockets are non-blocking.
2. **Handshake.** Client sends `NetMsg::Hello { name }` to the host. Host receives, allocates a peer id, replies with `NetMsg::Welcome { your_id, host_name, existing_peers }`. Host broadcasts `NetMsg::PeerJoined { id, name }` to every already-connected client so all rosters update. Both peers set `NetSession.welcomed = true`; the next frame, `tick_handshake` flips `NetMode` to `Connected` and (if still on the menu) transitions `AppState::MainMenu → Lobby`.
3. **Lobby (`AppState::Lobby`).** Both peers see the chunky-styled roster, host status banner, START + LEAVE + KICK buttons. Late joiners go through the same handshake; existing peers receive `PeerJoined` and update.
4. **START.** Host's START button triggers `Lobby → Playing`. `broadcast_state_change` sends `NetMsg::StateChange { state: Playing.to_u8() }`; clients receive and call `NextState.set(Playing)`. Both peers end up in `Playing` within ~one tick.
5. **Steady state (`Playing`).**
   - Each peer broadcasts `NetMsg::Transform` at `TRANSFORM_SEND_HZ = 30` (every ~33 ms).
   - Host broadcasts `NetMsg::EnemySnapshot` at `ENEMY_SNAPSHOT_HZ = 20` (every 50 ms).
   - Receivers drain the socket every frame.
6. **Leave / Kick / Teardown.**
   - **LEAVE** (or `Esc` in lobby) calls `tear_down_session` → sends `NetMsg::Bye` to each peer, drops `NetSession`, flips `NetMode → Solo`, returns to `MainMenu`.
   - **KICK** (host only) sends `NetMsg::Kicked { reason }` to one peer; removes them from `peers` + roster; broadcasts `PeerLeft { id }` to remaining peers so their rosters shrink.
   - Kicked peer's `recv_packets` sets `PendingKick`; `handle_received_kick` tears down their session + returns them to `MainMenu` with the reason in `JoinIpEntry.last_error`.

### Why hand-rolled instead of `bevy_replicon`?

We considered `bevy_replicon` (component-level replication via traits) and rejected it for this game's MVP:

- Two-player position sync is ~150 lines of code with raw UDP + `bincode`. Pulling a replication crate would add ~5 deps and a learning curve for one screen of payload.
- Replicon is server-authoritative by default. For per-player authoritative motion (each peer drives their own boat) we'd be working against the grain.
- The wire format here is small and stable. We control it directly; bytes-on-the-wire tests are easy to write.

If Phase 3 (shared customize state) grows enough that hand-rolling becomes unwieldy, we can revisit. For Phase 1 + 2 the raw approach is the right size.

---

## Wire format

Every packet is a single bincode-serialized `NetMsg`. Packets are tiny (under ~32 bytes for the connection-state messages, under ~720 bytes for a 30-enemy snapshot) so we don't bother with fragmentation, ACKs, or sequence numbers — lost packets are fine because every Transform / EnemySnapshot is a full state snapshot that supersedes the previous one.

```rust
enum NetMsg {
    Hello { name: String },                      // client → host on connect
    Welcome {                                    // host → client reply
        your_id: u8,
        host_name: String,
        existing_peers: Vec<(u8, String)>,       // other lobby members
    },
    PeerJoined { id: u8, name: String },         // host → existing peers
    PeerLeft { id: u8 },                         // host → other peers on drop/kick
    Kicked { reason: String },                   // host → kicked client
    Transform { id, pos: [f32; 2], rot: f32 },   // every 33 ms, every peer
    Bye { id },                                  // either → both on clean exit
    EnemySnapshot { entries: Vec<EnemyEntry> },  // host → client every 50 ms
    DamageEnemy {                                // client → host on hit
        enemy_id: u32,
        amount: i32,
        hit_pos: [f32; 2],
        weapon: u8,           // WeaponType::to_u8
        runes: Vec<u8>,       // Rune::to_u8 per element
    },
    ProcFx {                                     // transient effect broadcast
        kind: u8,             // proc_fx_kind discriminant
        from: [f32; 2],
        to: [f32; 2],
    },
    StateChange { state: u8 },                   // host → client AppState sync
}

struct EnemyEntry {
    id:           u32,    // NetEntityId (stable across snapshots)
    kind:         u8,     // EnemyVariant::to_u8 — append-only enum
    pos:          [f32; 2],
    rot:          f32,
    hp:           i32,
    status_flags: u8,     // bit 0=OnFire, 1=OnFrost, 2=OnBleed
}
```

### Append-only discriminants

Three enums carry stable wire-format discriminants, all with the same rule: **append only, never renumber**. A unit test (`*_discriminants_are_unique` / `*_u8_round_trip`) guards each one against accidental clashes in CI.

- `EnemyVariant::to_u8` / `from_u8` — 7 variants today
- `Rune::to_u8` / `from_u8` — 27 variants today
- `WeaponType::to_u8` / `from_u8` — 20 variants today
- `AppState::to_u8` / `from_u8` — 12 variants today
- `proc_fx_kind::{SHOCK_ARC, CASCADE, BLAST_RING}` — 3 kinds today

**Backwards compatibility.** The `EnemyVariant` discriminants are stable — adding a new variant means appending it with a fresh `u8` and updating `from_u8`. Renumbering existing variants will break peers running an older build. A unit test (`enemy_variant_discriminants_are_unique`) guards against accidental clashes.

**Forward compatibility.** Receivers handling `EnemyEntry` call `EnemyVariant::from_u8(kind).expect(...)` → `from_u8` returns `None` for unknown values, and the mirror system silently skips them. So a new client connecting to an old host doesn't crash if the old host sends a variant the client *would* know about — the issue is reversed (new host → old client could send unknown variants, which the client silently skips).

### Port

UDP `49333`. Picked from the IANA dynamic range to avoid clashing with anything well-known. The constant is in `src/multiplayer/net.rs::HOST_PORT` — change it once if you ever need to move.

---

## Testing

Tests live alongside their module via `#[cfg(test)] mod tests`. Run with:

```bash
cargo test --bin ship-game multiplayer
```

Coverage breaks down into:

- **Wire-format round-trips** — every `NetMsg` variant + `EnemyEntry` serialize → deserialize → assert equal. Catches accidental field reorders or type changes.
- **`EnemyVariant` discriminant stability** — round-trip every variant, assert no duplicates, assert `from_u8` returns `None` for unknown numbers.
- **UDP loopback** — bind two `127.0.0.1` sockets, send a `NetMsg`, drain on the other, verify the source address + decoded message. Also covers the malformed-packet drop path.
- **Socket sanity** — `bind_socket(None)` returns an ephemeral non-zero port; sockets are non-blocking (`recv_from` returns `WouldBlock` immediately).
- **IP parser** — `parse_join_addr` accepts bare IPv4, `ip:port`, IPv6, surrounding whitespace; rejects garbage; emits a friendly error on empty input.
- **State predicates** — `is_host_connected` / `is_client_connected` truth tables.
- **Ghost tint** — deterministic, clamps to `[0, 1]`, visibly differs from the source hull colour.
- **Snapshot buffer** — `LatestEnemySnapshot::take()` consume-once semantics.

If you add a new `NetMsg` variant, **add a round-trip test for it**. If you add a new `EnemyVariant`, **add an entry to `enemy_variant_u8_round_trip`** — both tests run in CI via `cargo test` and will fail loudly if you forget.

---

## What's not done yet

### Phase 2.6 (shipped): proc replication

Two-pronged design — **stateful** procs ride the snapshot, **transient** procs ride a dedicated broadcast event. See "Replication patterns" section below the wire-format spec.

**Damage relay now carries weapon + runes.** `DamageEnemy` packets serialise `WeaponType::to_u8` plus `Vec<u8>` rune discriminants. Host receives, deserialises back into typed values, and pushes to `PendingDamageQueue` with the full payload — so the existing `process_damage_events` pipeline rolls procs authoritatively on the host side. `OnFire`/`OnFrost`/`OnBleed`/`OnConduit`/`OnResonate` components are added on host enemies, and the next snapshot's `status_flags` bitmask carries the Fire/Frost/Bleed bits back to every client.

**Stateful procs reconcile via snapshot bitmask.** `EnemyEntry::status_flags` carries one bit per stateful proc kind. `send_enemy_snapshot` reads `Has<OnFire>` / `Has<OnFrost>` / `Has<OnBleed>` on each host enemy and packs the result. `apply_enemy_snapshot` on the client adds/removes the matching components on its mirrors based on the bits, so the client's existing `tick_on_fire` / `tick_on_frost` / `tick_on_bleed` systems light up the local DOT visuals + tick damage. The DOT damage that ticks on the client also goes through `relay_damage_to_host` (because the mirror is the target), so the host stays authoritative on HP.

**Transient procs broadcast via ProcFx.** New `ProcFx { kind, from, to }` packet for one-frame visuals (Shock arc, Cascade explosion, Blast ring). Gameplay code fires a `ProcFxFired` event (defined in `src/proc_fx.rs`); the `send_proc_fx` system reads events via `EventReader` and emits packets (clients send to host, host broadcasts to all). `recv_packets` lands them in `ProcFxInbox`. A host-side `relay_proc_fx_to_peers` re-broadcasts received events to peers other than the sender. **All three transient-proc call sites are wired**: Shock chain (`rune.rs::apply_proc`), Cascade (`bullet.rs::process_damage_event`), Blast ring (`bullet.rs::process_damage_event`). The `ProcFxFired` event is non-multiplayer-gated so single-player and wasm builds emit (and Bevy auto-drops) the events without coupling gameplay code to the multiplayer module.

### Phase 3 foundation (shipped): AppState sync

Host broadcasts every `AppState` transition via `NetMsg::StateChange { state: u8 }`; clients receive and trigger their own `NextState` to match. `broadcast_state_change` watches `Res<State<AppState>>` for changes and emits one packet per transition (not per frame). `apply_state_change` drains the inbox and calls `next.set(target)`.

This is the load-bearing primitive future Phase 3 work builds on. Without it, the host clicking PLAY (MainMenu → HullSelect) leaves the client stuck on MainMenu. With it, the lockstep menu / map / customize flow Just Works.

### Phase 2.5 (shipped): damage relay

Client bullets now damage host enemies via the relay path:

1. `relay_damage_to_host` runs **between** `bullet_collisions` and `process_damage_events`. It scans the local `PendingDamageQueue`; for every event whose target carries a `NetEntityId`, it serialises a `NetMsg::DamageEnemy { enemy_id, amount, hit_pos }` packet, sends to the host, and **removes the event from the local queue** so the client doesn't apply phantom damage to a mirror that the next snapshot is about to overwrite anyway.
2. Host's `recv_packets` routes incoming `DamageEnemy` into a `PendingDamageRelay` buffer.
3. `apply_relayed_damage` (host only) drains the buffer each frame, looks up the target enemy by `NetEntityId`, and pushes onto the host's own `PendingDamageQueue` via `push_initial`. The normal damage pipeline then runs as usual.
4. Host's next `EnemySnapshot` (50 ms cadence) carries the new HP back to the client; the mirror updates.

**Latency:** at LAN RTT (~5 ms), the player won't notice. On a 150 ms internet link, the HP drop visibly lags the hit particle by half a snapshot window — still playable.

**Phase 2.5 limitation — procs don't relay.** `DamageEnemy` carries only the raw amount; weapon + runes + source metadata aren't included. Consequences:

- The client's own bullet-fire path still rolls procs locally (Fire DOT, Shock chains, Cascade…) and shows them on the client's screen.
- The host applies the **base damage only** — host-side does not also proc the runes, so DOT/chain damage from a client's rune-loaded hit doesn't show up in subsequent snapshots.
- Net effect: client sees full local proc visuals; the underlying HP only decreases by the base damage. For "neat looking" but slightly underpowered runes, that's the current trade-off.

Fixing this means extending `DamageEnemy` with a serialised rune list + `WeaponType`, plus making the host's `apply_relayed_damage` use those instead of the `&[]` / `Standard` stubs. Add a `weapon: u8` discriminant for `WeaponType` and bincode the rune list — straightforward but it's its own task.

### Phase 3 (next chunks): shared loadout + map

The **foundation** (AppState sync) is in. Remaining chunks, in rough dependency order:

1. **Plumb `OutgoingProcFx` from rune code.** Push to the queue at each transient-proc spawn site in `rune.rs` (Shock chain, Cascade explosion, Blast ring). Small but invasive. Without it, the infrastructure exists but no actual proc visuals broadcast.
2. **Map seed sync.** Either send the seed at game start and have both peers run deterministic map gen, or replicate the full map state once on `OnEnter(Playing)`. Requires auditing map gen for non-determinism (random sources, frame-order-dependent decisions).
3. **Shared loadout.** Replicate `PlayerStats`, equipped weapons per slot, equipped runes per socket, and mods. Decide UX: lockstep (both see same shop, either can spend) vs mirror (each customizes independently). Lockstep is simpler to spec and matches the shared-run framing.
4. **Shared XP / scrap / level-up.** Pool resources. Either player's kill adds to the shared XP pool. LevelUp screen drives off shared XP.
5. **Wave / level progression sync.** Host's `CombatContext` drives waves; clients receive wave-state updates so the wave indicator UI agrees.

### Phase X (deferred, optional): internet without port forwarding

Either ship a relay service (free TURN, custom UDP relay, or Steam Sockets if we ever Steam-ify) or integrate a NAT-punch library (`matchbox` over WebRTC works in browsers too). Each has its own ops cost. Not blocking the local-multiplayer demo.

### Known cosmetic issues (Phase 2.6)

- Client sees their local wave indicator stuck at "wave 0/0" because the wave system runs only on host. Phase 3 wave sync will fix it.
- Client may see enemies appear at world positions outside their local map's section polygons because each peer's map is generated independently. Phase 3 map seed sync will fix it.
- Transient proc visuals (Shock arc, Cascade explosion, Blast ring) propagate end-to-end. Host's procs fire `ProcFxFired` events → `send_proc_fx` packetises them → peers receive into `ProcFxInbox` → `spawn_proc_fx_visuals` drains and spawns the local visual. Blast specifically still uses the lightning-arc visual as a placeholder on receive because `spawn_blast_ring` is private to `bullet.rs`; making it public is a one-line follow-up.
