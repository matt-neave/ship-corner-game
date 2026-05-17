# Ship-game architecture notes

A Bevy 0.16 chunky-pixel naval shooter. Notes below are the load-bearing invariants — don't refactor past them without explicit alignment.

## Render pipeline: three layers, on purpose

1. **Play area** — game world renders into a `PLAY_INTERNAL_W × PLAY_INTERNAL_H` render-target image with nearest-neighbour sampling, then displayed as a sprite (`UpscaleSprite`) on the upscale camera. Chunky pixel look.
2. **Customize / shop** — separate `CUSTOMIZE_INTERNAL_W × CUSTOMIZE_INTERNAL_H` render target with its own camera + viewport (`CustomizeViewport`). Same chunky-pixel treatment.
3. **bevy_ui chrome** — level-up cards, pause/main menu, boss-reward overlay, hull-select panels, HP/XP bars, banners. Renders at the window's *native* resolution → crisp text + borders.

**Do not unify these.** Collapsing all UI into a single virtual viewport would force the bevy_ui chrome into chunky-pixel rendering and lose the crisp UI text. The split is the right architecture for this aesthetic.

## Resolution scaling

- `bevy_ui::UiScale` is the global multiplier for every `Val::Px` value in bevy_ui. Written by `sync_ui_scale` (in `src/rendering.rs`) each frame as `clamp(min(w / WINDOW_W, h / WINDOW_H), 0.5, 3.0)` — fit-mode.
- `play_area_screen_rect(logical_w, logical_h)` (in `src/modes.rs`) is the **single source of truth** for the play area's on-screen rect. Fractional fit-scale, capped by `PLAY_VERTICAL_PAD_RATIO`. Cursor mapping, upscale-sprite placement, HUD camera viewport, and overlay positions all read from it.
- `CustomizeViewport::display_scale` is the equivalent for the customize render target. `window_to_spec()` converts cursor → spec coords.

### The `/ ui_scale` rule

When you write a **screen-pixel coordinate** (output of `play_area_screen_rect`, cursor pos, monitor metric) into a `Val::Px(...)`, **divide by `UiScale`** so the bevy_ui layout pass multiplies back to the right screen pixel:

```rust
let s = ui_scale.0.max(0.0001);
node.left = Val::Px(play_left / s + margin_design);
```

When the value is **already in design pixels** (a width of `180.0` you chose for the bar), don't divide — `UiScale` handles scaling automatically.

### The two scales in the customize screen

- **Positions** of customize text use `viewport.display_scale` (~4× at design) so they land inside the upscaled customize render sprite's screen rect.
- **Glyph scale** of customize text (`Transform.scale`) uses `UiScale` (1.0 at design) so font sizes read consistent with bevy_ui chrome.

If you forget the second and use `display_scale` for both, text renders 4× too big.

## Settings pattern

Mode toggles (`NightMode`, `CrtMode`, `VsyncMode`, `WindowModeSetting`, `ResolutionSetting`):

1. Each is a `Resource` with a `last_applied: Option<T>` field for flip detection.
2. An `apply_*_mode` system watches the resource and only writes through when it changes.
3. `settings.rs` reads + writes the on-disk `settings.txt` (key=value). `apply_loaded_settings` runs once at startup, `persist_settings_on_change` writes on flip.
4. The main-menu and pause-menu settings panel share `SettingsItem` markers — `handle_settings_item_click` and `update_settings_labels` run unconditionally so both panels' buttons just work.

To add a new setting: define the resource → add its apply system → wire it into `Settings` (parse/serialize/from_modes) → add `SettingsItem` variant + spawn buttons in both panels.

## Damage event pipeline

Every damage source pushes a `DamageEvent` onto `PendingDamageQueue::push_initial(target, amount, hit_pos, weapon, source, runes)`. Sources include: regular bullets, mortar shells, blade melee, beam pierce, helicopter bullets, octopus tentacle slap. `process_damage_event` drains the queue, applies damage, then iterates the bullet's `runes` array for proc effects (Fire/Frost/Shock/Bleed/etc.). Chain events (Shock, Cascade) are pushed back onto the queue with `procced` accumulating to prevent infinite recursion.

**`hit_pos` is always the enemy's transform centre**, not the literal collision point. That's why Blast's AOE radiates from inside the target's body.

## Adding a new rune

Touch every spot or you'll get a non-exhaustive-match build error:

- `Rune` enum variant in `src/rune.rs`
- `label()` / `description()` / `proc_coefficient()` / `apply_rune_stacked()` arms
- `rune_color_for()` in `src/customize/setup.rs`
- `runes_pool` in `src/customize/drag.rs`
- `rune_dynamic_description()` in `src/customize/tooltip.rs` (for live numbers in the tooltip)
- CSV: `rune_<name>` + `rune_<name>_desc` in `data/translations.csv`
- If the rune has on-hit logic: match arm in `process_damage_event` in `src/bullet.rs`
- If the rune has tick logic (DoT, etc.): a `tick_on_<name>` system + register in `src/main.rs`

## Stats panel sharing

The level-up screen + boss-reward screen both render the same stats panel via `stats_panel_overlay::spawn_stats_panel`. The shop's stats panel is intentionally separate — it lives on the customize render target (chunky-pixel `Text2d`), a fundamentally different pipeline. Don't try to unify.

## Player damage central pipeline

Every source that damages the **local Friendly** goes through `bullet::apply_friendly_damage(h, fx, shield, stats, rng, incoming, is_local_player)`. It bakes dodge roll → armour reduction → shield absorb → HP write, in that order. Sites currently routed through it: enemy-bullet hits (`bullet.rs`), ram self-damage (`ship.rs`), bomber/rammer detonate (`enemy/mod.rs`), MP relayed `DamagePlayer` (`multiplayer/ghost.rs`).

**Don't damage the player via `apply_damage` or direct `h.0 -= n`** — you'll bypass dodge/armour/shield. The plain `apply_damage` is for enemy/ally targets only.

The `is_local_player` flag is set by reading `Has<LocalPlayer>` at the call site. The MP host-side ghost-of-peer (Friendly without LocalPlayer) takes full damage so the relay delta to the peer stays correct — the peer's own `apply_friendly_damage` then mitigates locally.

## Stat steps: debug vs upgrade

`StatKind` has two per-stat step methods:
- `debug_step()` — bigger. Used by the `+/-` glyph buttons in the customize stats panel. Tuned for dev-side range traversal.
- `upgrade_step()` — smaller, conservative. Used by `xp::buff_pool` (level-up cards) and as the authoring baseline for `MOD_LIBRARY` entries.

Adding a stat to a roll pool: use `upgrade_step`, never `debug_step`.

`StatKind::ALL` is the display list (stats panel readout). `StatKind::ROLLABLE` is the subset usable in shop mods + level-up rolls (currently `ALL` minus `TurretTurnSpeed` + `TurretArcBonus`). The two `Allowed` variants stay on `PlayerStats` but won't appear as random offers.

## Mod library

`customize::drag::MOD_LIBRARY` is the single source of truth for every shop mod. Each entry is a `ModSpec { name, rarity, changes: &[(StatKind, f32)] }`. The shop rolls 3 picks per stock via `weighted_pick_without_replacement` — per-pick weight comes from `ModRarity::weight()` (60/25/12/3 for Common/Uncommon/Rare/Legendary). Outline tint comes from `ModRarity::border_color()`.

Adding a new mod is one struct-literal append to `MOD_LIBRARY`. The roll, click-apply (iterates `spec().changes`), card label, and outline-tint all pick it up automatically.

## Multiplayer authority split

Host owns shared world (enemies/waves/boss/team-death/scrap-broadcast). Each peer owns their own boat / stats / loadout / scrap / shop / RNG / level-up picks / hull pick / autonomous units / bullet visuals / turret aims. Damage to enemies is relayed client→host via `DamageEnemy { weapon, runes }`; host re-rolls procs authoritatively. Damage to peers is detected on the host's ghost-of-peer and forwarded via `DamagePlayer`. Stateful procs ride the `EnemySnapshot.status_flags`; transient procs (Shock arc, Cascade, Blast, Conduit, Resonate) ride `ProcFx`.

**`recv_packets` is at Bevy's 16-SystemParam cap.** Bundle new inboxes into the existing `DeathRelayInboxes` / `LoadoutInboxes` / `XpInboxes` SystemParam structs.

Per-weapon visual sync (mortar / beam / flame / tentacle / harpoon-chain) rides dedicated signal events (`MortarFired`, `BeamFired`, `FlameTick`, `TentacleSlap`, `HarpoonAttached`). Each owner-side fire site writes an event; a per-fx bridge system broadcasts the `NetMsg`; receivers spawn a damage=0 visual via shared `spawn_*_visual` helpers (so mirror = real visual exactly).

## Tooltip + mod card UiScale rule

Customize-render-target overlays that need to track text size MUST scale their bounding box by `UiScale` if they also scale the text by `UiScale`. The tooltip (`customize/tooltip.rs`) does this: `fill_w_native = (text_w_native + pad) * glyph_scale`. The mod card sprite does NOT — instead it stays at `MOD_CARD_W * display_scale` to match its `HitArea` (which is spec-coord and doesn't know about `UiScale`), and keeps content sized via shorter labels. Pick one convention per overlay; mixing them desyncs text overflow vs hit-area at non-design resolutions.

## Pause as overlay state

Pause is a real `AppState::Paused` variant, but it's openable from `Playing` / `Map` / `StageComplete` / `BossReward`. The previous state is stashed in `PrePauseState: Option<AppState>` and restored on resume. Customize / LevelUp / HullSelect are excluded because their `OnEnter` hooks re-init the modal (rolling fresh shop items, losing drag state).

## Coding conventions

- **Comments describe what the code does NOW, not how it got there.** No "previously this was X" / "I changed this because Y" / "this used to floor() but now". Future readers see the current code; commit messages cover history.
- Prefer editing existing files over creating new ones.
- No emoji in code or comments unless the user asks.
- bevy_ui layout: `Val::Px` for fixed dims (scaled by `UiScale`), `Val::Percent` for proportional. Mix freely.
- For new mode toggles, follow the `last_applied: Option<T>` pattern so the apply system is cheap.
