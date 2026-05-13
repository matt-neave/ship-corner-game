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
- `label()` / `description()` / `proc_coefficient()` / `cycle_next()` / `cycle_prev()` / `apply_rune_stacked()` arms
- `rune_color_for()` in `src/customize/setup.rs`
- `runes_pool` in `src/customize/drag.rs`
- `rune_dynamic_description()` in `src/customize/tooltip.rs` (for live numbers in the tooltip)
- CSV: `rune_<name>` + `rune_<name>_desc` in `data/translations.csv`
- If the rune has on-hit logic: match arm in `process_damage_event` in `src/bullet.rs`
- If the rune has tick logic (DoT, etc.): a `tick_on_<name>` system + register in `src/main.rs`

## Stats panel sharing

The level-up screen + boss-reward screen both render the same stats panel via `stats_panel_overlay::spawn_stats_panel`. The shop's stats panel is intentionally separate — it lives on the customize render target (chunky-pixel `Text2d`), a fundamentally different pipeline. Don't try to unify.

## Coding conventions

- **Comments describe what the code does NOW, not how it got there.** No "previously this was X" / "I changed this because Y" / "this used to floor() but now". Future readers see the current code; commit messages cover history.
- Prefer editing existing files over creating new ones.
- No emoji in code or comments unless the user asks.
- bevy_ui layout: `Val::Px` for fixed dims (scaled by `UiScale`), `Val::Percent` for proportional. Mix freely.
- For new mode toggles, follow the `last_applied: Option<T>` pattern so the apply system is cheap.
