Good — these images make the style concrete. Let me describe what I see, then deliver the final prompt.

What's actually in the SNKRX screenshot, in plain visual terms: a dark slate-grey background (not pure black) with a subtle checkerboard pattern of two near-identical greys. Everything in the play field is built from solid-fill primitive shapes — capsules, small rounded rectangles, circles — each in a single saturated flat color (red, orange, yellow, green, cyan, white) with **no outlines, no gradients, no shading**. The chunky pixel edges come from rendering at low resolution and upscaling with nearest-neighbor; you can count the pixels on every shape's edge. There's a soft additive glow around bright objects — the white circles in the center bleed slightly into the surroundings, and explosion debris has a faint halo. The play area is bordered by a darker frame that contains the action. Score text sits at top-right in a chunky pixel font, yellow, all-caps. The overall palette is high-contrast: muted dark backdrop, pure-saturation foreground.

And from your battleship mockup: the same chunky-pixel rasterization applied to your ship — visible stair-stepping on every curved edge, flat color fills, white motion trail behind the hull rendered as a sequence of small white pixel clusters that fade behind the ship rather than a smooth line. Bullets are tiny yellow capsules, also pixelated.

Here's the full revised prompt with everything integrated and SNKRX replaced by description:

```markdown
# Bevy Naval Prototype — Build Spec

## Project Setup
- Goal: Minimal playable prototype. No genre/loop yet — we are building the **core feature scaffolding**. Code should be modular and component-driven; prefer ECS systems over monoliths. Leave hooks where future features (damage reduction, costs, lose condition, etc.) will plug in.

## Tech Stack
- **Language**: Rust (latest stable).
- **Engine**: Bevy (latest stable release). Use Bevy's built-in 2D rendering, ECS, input, UI (`bevy_ui`), and audio. No third-party game frameworks.
- **Allowed crates**: standard Bevy ecosystem crates only where genuinely needed (e.g., `rand` for RNG). Use `bevy_ui` for the control panel — do not pull in `bevy_egui`. Ask before adding anything heavyweight.
- **No external assets of any kind.** No sprite sheets, no PNGs, no texture files, no fonts beyond Bevy's default, no audio files. The project must build and run from source alone with zero asset dependencies.

## Rendering Constraints — Read Carefully

### Everything is built from primitive shapes
This is a hard rule. No sprites, no textures, no images anywhere in the play area. Specifically:
- Ship hulls → capsule/ellipse meshes (Bevy's `Mesh2d` with `Capsule2d` / `Ellipse` / `Rectangle` primitives).
- Turret bases → small rectangle meshes.
- Turret barrels → long thin rectangle meshes, parented to the base.
- Bullets → small capsule meshes.
- Friendly ship's trail → a sequence of small white capsule or square primitives spawned behind the ship that fade out over ~0.5–1s. Not a continuous line, not a texture — discrete chunky white shapes that read as pixelated foam when rasterized.
- Ocean → solid blue clear color or a single colored quad. No water texture.

### Pixel-perfect post-processing (play area only)
- Render the play area to a **low-resolution offscreen render target**. Suggested internal resolution: **400×400** (tune for taste — the goal is that a capsule the size of an enemy ship is roughly 8–12 pixels long at internal resolution, so its edges visibly stair-step when upscaled).
- Upscale that render target to the on-screen play-area size using **nearest-neighbor sampling**. No bilinear filtering, no smoothing — confirm the texture's `ImageSampler` is set to `nearest()`.
- Implement via a `Camera` rendering to an `Image` render target, then displayed by a second camera or as a `bevy_ui` image node sized to the play area's on-screen rectangle.
- The result should look like: every shape's edge is a chunky stair-stepped silhouette, not a smooth curve. A circle becomes a recognizably-blocky cluster of pixels. This is the look — count-the-pixels chunky.

### Color and shape rules
- **Flat single-color fills only.** No gradients, no per-shape lighting, no drop shadows, no inner highlights.
- **No outlines on shapes.** Color contrast against the background does the work.
- High-saturation foreground colors against a muted backdrop. The ocean is a **medium-saturation blue** (think deep cobalt, not navy and not bright cyan); foreground ships and bullets pop against it.
- One color per shape part. The friendly hull is one tone; its turrets are a different tone; that's it.

### Optional bloom
A soft additive bloom on bright elements (bullets, the white trail, muzzle flashes if added later) is welcome and matches the target aesthetic. Apply bloom **after** the nearest-neighbor upscale so the glow itself is soft, not pixelated. If implementing bloom adds meaningful complexity, skip it for now and leave a hook to enable it later.

### Control UI rendering
- The control UI is rendered normally with `bevy_ui` at the screen's native resolution. **No post-process, no low-res target, no pixelation.** Crisp edges.
- The mini ship-schematic inside each control container is also drawn from primitives but **without** the pixel-perfect downscale — it should look clean and readable.

## Screen Layout
The window is split into two regions:
1. **Control UI** — left side, fixed width (suggest ~280px). Crisp `bevy_ui` rendering.
2. **Play Area** — right side, **square**, dynamically sized to fit the remaining space (height-constrained on most aspect ratios). Pixel-perfect post-processing applied here only.
3. **Score banner** — top-center, overlaid above the play area. See HUD section.

## Friendly Ship — Layout
Modelled on Cuniberti's "ideal battleship" — **8 single turrets**, arranged on a centerline-elongated capsule body:
- 1 turret at the **bow** (fore, centerline).
- 1 turret at the **stern** (aft, centerline).
- 6 wing turrets in **three pairs** (port/starboard) along the hull.
- The **middle pair sits further apart** (wider beam at midship) than the fore-pair and aft-pair of wing turrets.

Reference image `image.png` is authoritative for **turret positioning only** — the in-game ship is a flat capsule with primitive turrets, not a detailed illustration. Reference `image-1.png` shows general play-area composition and the target visual style (chunky pixels, flat colors, white pixelated trail) but its turret positions are wrong — ignore the ship's turret layout in that image.

## Friendly Ship — Behavior
- **Movement**: ship follows the mouse cursor when the cursor is inside the play area. Smooth turning that respects a turning circle — rotate toward the target heading at a fixed angular rate, do not snap. Tighter turning circle than enemies.
- **Mouse outside play area**: ship moves autonomously, **bouncing off the play-area walls** on edge contact (reflect the velocity vector). Maintain current speed.
- **Damage system**: ship is **invincible for now**, but build the damage pipeline as if it weren't. Implement a `Health` component, a `DamageEvent`, and a damage-resolution system that currently no-ops the final HP subtraction on the friendly ship. Leave a clean insertion point for future damage-reduction modifiers.
- Modelled as a **state machine** (simple for now: `Idle`, `Moving` — extend later).

## Friendly Turrets
- Each turret is an independent entity with its own state machine (`Searching`, `Tracking`, `Firing`).
- **Independent target selection**: each turret picks the **nearest enemy within its 90° firing arc** (±45° from the turret's mounted forward direction relative to the hull).
- **Pivot speed**: 90°/second.
- **Range**: 100px (in play-area world coordinates).
- **Fire rate**: 2 shots/second (default; modifiable via UI).
- **Damage**: 1 (default; modifiable via UI).
- A turret only fires when its barrel is aimed at the target (within a small tolerance), the target is in range, and the target is in arc.

## Enemies
- **Body**: smaller red capsule with a single black turret fixed forward — turret has **no rotation relative to hull**, only fires straight ahead.
- **Health**: 10 HP each.
- **Fire rate**: 1 shot/second.
- **Range**: 80px.
- **Movement**: autonomous, **wider turning circle** than the friendly ship. AI must **not** be naive direct-pursuit — mix in randomized waypoints, occasional strafing, off-angle approaches, so the player faces varied threats. Enemies should generally try to bring their nose to bear on the friendly ship to fire, since their gun is forward-fixed.
- Modelled as a **state machine** (e.g., `Wander`, `Approach`, `Attack`, `Reposition`).

## Spawning
- Endless. Spawn enemies from random points just outside the play-area edges; they enter the play area heading roughly inward.
- Reasonable prototype curve: start at ~1 enemy every 3 seconds; ramp linearly so spawn interval halves over the first 60 seconds, then floor at ~0.5s. Cap concurrent enemies at ~30 for readability. All values tunable.

## Bullets
- Both factions: small capsules.
- **On collision with a valid target**: deal damage, despawn.
- **Max range fallback**: each bullet despawns after travelling its turret's range (100px friendly, 80px enemy). No piercing.

## HUD — Score
- **Score**: +10 per enemy destroyed.
- Display: **top-center of the screen**, **yellow text**, large and clear, sitting above the play area. Use Bevy's default font.

## Control UI (Left Panel)
- **8 vertical containers**, one per turret slot, stacked top-to-bottom. Modular — render the same component 8 times, parameterized by slot index.
- Each container shows a **zoomed-in schematic of the friendly ship** with all turrets greyed out **except this slot's turret**, which is highlighted in its normal color. This visually identifies which physical turret the container controls.
- **Initial state**: only **slot 1** (bow turret) is equipped. The other 7 containers show an **"Equip gun"** button.
- **Equipping**: clicking "Equip gun" instantly attaches a default-stat turret to that slot (free, unlimited — pure prototype).
- **Equipped container** shows:
  - Current **damage** value with **▲ above / ▼ below** arrows. Steps of ±1.
  - Current **attack speed** value (shots/sec) with **▲ / ▼** arrows. Steps of ±0.1.
  - No caps, no costs (leave the modifier system extensible for future limits).
- Stat changes apply immediately to the live turret entity in the play area.

## Architecture Notes
- ECS-first: ships, turrets, bullets, enemies are all entities with composable components (`Health`, `Velocity`, `Faction`, `TurretSlot`, `FireCooldown`, etc.).
- Turrets are children of their ship entity; transforms cascade.
- State machines: implement as enum components with per-state systems, or use a small FSM helper — your call, keep it readable.
- Keep play-area rendering and UI rendering on separate camera/render-target pipelines so the post-process effect only touches the play area.
- All tunable values (speeds, ranges, fire rates, spawn curve, damage) live in a single config struct or `Resource` for easy iteration.

## Reference Images
- `image.png` — Cuniberti turret layout (authoritative for friendly ship turret positions).
- `image-1.png` — play-area composition and target visual style. **Turret positions in this image are wrong — ignore them.** Use it for: chunky-pixel rasterization look, flat color palette, white pixelated trail behind the ship, ocean color, general framing.
```