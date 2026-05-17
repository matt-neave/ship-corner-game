//! Friendly-ship setup (hull + 8 turrets + dock + pier grid + trail), and
//! the per-frame movement system (mouse-follow in Sandbox, auto-engage in
//! Wave). Also hosts `apply_velocity` since it's a tiny shared integrator and
//! `approach_angle` since it's used by both ship + enemy heading code.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;
use bevy::window::PrimaryWindow;

use crate::balance::{
    ARENA_H, ARENA_W, BEAM_LENGTH, ENEMY_LEN, ENEMY_WIDTH,
    HULL_HALF_LEN, HULL_LEN, HULL_WIDTH,
    PLAY_LAYER, PLAY_WORLD_H, PLAY_WORLD_W, TURRET_MOUNTS, TURRET_POSITIONS, TURRET_RANGE,
};
use crate::components::{Faction, FactionKind, Friendly, Health, Heading, LocalPlayer, Velocity};
use crate::effects::{EffectMeshes, HitFx};
use crate::enemy::{Enemy, EnemyVariant};
use crate::modes::play_area_screen_rect;
use crate::palette::{Palette, PaletteMaterials};
use crate::rune::{FireExtent, OnFrost};
use crate::trails::{empty_dynamic_mesh, Trail};
use crate::turret::{BarrelIndex, HeliPadDecal, TurretBarrel, TurretConfig, TurretSlot};

// ---------- Setup ----------

/// Marker on each of the four arena-border line entities so
/// `despawn_player_world` can sweep them up alongside the friendly
/// hull when the player returns to MainMenu.
#[derive(Component)]
pub struct PlayBorder;

/// Build the cached `PaletteMaterials` + `EffectMeshes` resources that
/// every play-world entity references. Runs once at Startup so the
/// resources exist before any spawn site needs them.
///
/// The play-world ENTITIES (friendly hull, trail, arena border) are
/// spawned on demand by `spawn_player_world` at `OnEnter(Playing)`,
/// not here. Keeping the play world empty during MainMenu / HullSelect
/// means those screens don't have to firefight a stale player ship +
/// border rendering behind their own chrome.
pub fn setup_world(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    palette: Res<Palette>,
) {
    let pm = PaletteMaterials::build(&palette, &mut materials);
    let em = build_effect_meshes(&mut meshes);
    commands.insert_resource(pm);
    commands.insert_resource(em);
}

/// Spawn the play-world entities the player needs in combat: arena
/// border, friendly hull, 8 turrets + barrels + HeliPad decals, and
/// the wake trail. Idempotent — subsequent `OnEnter(Playing)` ticks
/// (after Pause / Map / Customize) see the Friendly already alive and
/// short-circuit. Only fresh-start paths (MainMenu → HullSelect →
/// Playing) hit the actual spawn code.
pub fn spawn_player_world(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    // Materials only consumed by the experimental sprite-stacked
    // turret (gated on `stacked_standard_turret`) to allocate the
    // black bore-dot material. Stays unused on the default build —
    // accept the dead-code warning rather than cfg-gating the param
    // signature (which makes the call sites diverge).
    #[allow(unused_mut)]
    mut materials: ResMut<Assets<ColorMaterial>>,
    pm: Res<PaletteMaterials>,
    cfg: Res<TurretConfig>,
    stats: Res<crate::stats::PlayerStats>,
    // In MP, `LocalDeathState.dead = true` means we've been killed and
    // are in spectate mode. We must NOT respawn the local Friendly
    // on every `OnEnter(Playing)` — that would revive the dead peer
    // mid-stage on the very first between-wave LevelUp bounce
    // (Playing → LevelUp → Playing). Revive only happens on stage
    // transition via `host_broadcast_revive_on_stage_complete`,
    // which clears `LocalDeathState.dead` before the next
    // `OnEnter(Playing)`.
    local_death: Res<crate::multiplayer::death::LocalDeathState>,
    existing: Query<Entity, With<Friendly>>,
) {
    // Discard the unused-binding warning on the default (no-feature)
    // build path. Cleaner than cfg-gating the parameter list.
    #[cfg(not(feature = "stacked_standard_turret"))]
    let _ = &mut materials;
    if !existing.is_empty() { return; }
    if local_death.dead { return; }

    // 1px play-area border drawn around the *arena* (z=6 so it always
    // frames the action). With `big_arena` the border tracks the
    // larger bounds rather than the viewport — the camera reveals
    // the walls as the player approaches them.
    let border_h = meshes.add(Rectangle::new(ARENA_W, 1.0));
    let border_v = meshes.add(Rectangle::new(1.0, ARENA_H));
    let half_x = ARENA_W * 0.5 - 0.5;
    let half_y = ARENA_H * 0.5 - 0.5;
    for (m, x, y) in [
        (border_h.clone(), 0.0,  half_y),
        (border_h.clone(), 0.0, -half_y),
        (border_v.clone(),  half_x, 0.0),
        (border_v.clone(), -half_x, 0.0),
    ] {
        commands.spawn((
            Mesh2d(m),
            MeshMaterial2d(pm.border.clone()),
            Transform::from_xyz(x, y, 6.0),
            RenderLayers::layer(PLAY_LAYER),
            PlayBorder,
        ));
    }

    // Friendly trail — a ribbon mesh rebuilt every frame from path history.
    // Mesh positions live in world space, so the entity transform stays at origin.
    let trail_mesh = meshes.add(empty_dynamic_mesh());
    commands.spawn((
        Mesh2d(trail_mesh),
        MeshMaterial2d(pm.trail.clone()),
        Transform::from_xyz(0.0, 0.0, 0.5),
        Trail,
        RenderLayers::layer(PLAY_LAYER),
    ));

    // Friendly ship hull (rounded capsule).
    let hull_radius = HULL_WIDTH / 2.0;
    let hull_inner = HULL_LEN - HULL_WIDTH;
    let hull_mesh = meshes.add(Capsule2d::new(hull_radius, hull_inner));
    let ship = commands.spawn((
        Mesh2d(hull_mesh),
        MeshMaterial2d(pm.hull.clone()),
        Transform::from_xyz(0.0, 0.0, 1.0),
        Visibility::Inherited,
        Friendly,
        // LocalPlayer disambiguates from multiplayer's remote-peer
        // ship (which is also Friendly so enemies target it, but
        // shouldn't be driven by local input or counted in trail /
        // HUD single() queries).
        LocalPlayer,
        Faction(FactionKind::Friendly),
        Health(stats.max_hp()),
        Velocity(Vec2::new(0.0, stats.move_speed.effective())),
        crate::stats::Shield::default(),
        Heading(0.0),
        HitFx::new(pm.hull.clone()),
        FireExtent(Vec2::new(HULL_WIDTH * 0.5, HULL_LEN * 0.5)),
        RenderLayers::layer(PLAY_LAYER),
    )).id();

    // (Pier grid + drafting visuals were Wave-mode only — both gone.)

    // Friendly turrets. Barrel ≥1.5 wide so it doesn't alias to zero
    // pixels at off-axis rotations — without MSAA, sub-pixel rects vanish
    // entirely between integer-grid angles.
    let base_mesh = meshes.add(Circle::new(2.0));
    let barrel_mesh = meshes.add(Rectangle::new(1.5, 4.0));
    // Shared H-decal meshes — three rectangles forming a chunky `H`,
    // painted yellow on the HeliPad deck. Sized to fill most of the
    // turret-base disc (`Circle::new(2.0)`, diameter 4) so the letter
    // reads from gameplay distance.
    let h_post_mesh = meshes.add(Rectangle::new(0.8, 3.0));
    let h_bar_mesh  = meshes.add(Rectangle::new(2.2, 0.8));

    for (i, (lx, ly)) in TURRET_POSITIONS.iter().enumerate() {
        let slot = cfg.slots[i];
        let visible = slot.equipped;
        let mount = TURRET_MOUNTS[i];
        let turret_mat = pm.turret_for(slot.weapon).clone();
        let mut ec = commands.spawn((
            Mesh2d(base_mesh.clone()),
            MeshMaterial2d(turret_mat.clone()),
            Transform::from_xyz(*lx, *ly, 2.0).with_rotation(Quat::from_rotation_z(mount)),
            if visible { Visibility::Inherited } else { Visibility::Hidden },
            TurretSlot {
                index: i,
                barrel_angle: mount,
                mount_angle: mount,
                fire_cd: 0.0,
                damage: slot.damage,
                fire_rate: slot.fire_rate,
                weapon: slot.weapon,
                range_mult: 1.0,
                barrels: slot.barrels.max(1),
                next_barrel: 0,
                // SlotCfg's `[Option<Rune>; 3]` flattens to a Vec
                // here. `sync_turret_config` rebuilds this every
                // frame with the proper Amplifier merge, so the
                // initial seed just needs the right shape.
                runes: slot.runes.iter().copied().flatten().collect(),
                cycle_idx: 0,
            },
            RenderLayers::layer(PLAY_LAYER),
        ));
        ec.insert(ChildOf(ship));
        let turret_id = ec.id();

        // ---- Experimental: 3-layer sprite-stacked Standard turret ----
        // Two identical-radius discs on top of the base (layer 0),
        // each offset by exactly 1 internal pixel (1.0 world unit) along
        // the turret's local +Y. The 1-px-per-slice rule is what makes
        // sprite stacking read — sub-pixel offsets get snapped away by
        // the chunky-pixel render target's nearest-neighbour sampling,
        // and tapering radii would just look like a cone. Same
        // silhouette + integer offset = horizontal slices of a 3D mesh.
        //
        // Plus a single black pixel at the centre of the top layer —
        // sells the "hollow cylinder seen from above" illusion (the
        // bore of the gun barrel). Without it the stack reads as a
        // solid puck instead of a tube.
        //
        // Parented to the turret so the whole assembly rotates with the
        // aim direction.
        #[cfg(feature = "stacked_standard_turret")]
        if matches!(slot.weapon, crate::weapon::WeaponType::Standard) {
            // (local_y_offset, z_offset_from_base)
            let layers: [(f32, f32); 2] = [
                (1.0, 0.05),
                (2.0, 0.10),
            ];
            for (y, dz) in layers {
                let layer = commands.spawn((
                    Mesh2d(base_mesh.clone()),
                    MeshMaterial2d(turret_mat.clone()),
                    Transform::from_xyz(0.0, y, dz),
                    if visible { Visibility::Inherited } else { Visibility::Hidden },
                    RenderLayers::layer(PLAY_LAYER),
                )).id();
                commands.entity(layer).insert(ChildOf(turret_id));
            }

            // Barrel-bore dot: 1×1 internal-pixel black square on top
            // of layer 2 (z = 0.15 to sit above both stacked discs).
            // Rectangle rather than Circle so the chunky-pixel sampler
            // lands exactly one pixel regardless of camera offset.
            let bore_mesh = meshes.add(Rectangle::new(1.0, 1.0));
            let bore_mat = materials.add(Color::BLACK);
            let bore = commands.spawn((
                Mesh2d(bore_mesh),
                MeshMaterial2d(bore_mat),
                Transform::from_xyz(0.0, 2.0, 0.15),
                if visible { Visibility::Inherited } else { Visibility::Hidden },
                RenderLayers::layer(PLAY_LAYER),
            )).id();
            commands.entity(bore).insert(ChildOf(turret_id));
        }

        // Spawn THREE barrel children, indexed port / middle / starboard.
        // Single-barrel mode shows just the middle; twin shows port + stbd
        // (skipping the middle); triple shows all three. `sync_turret_config`
        // owns visibility, lateral offset, and the middle-barrel scale-up
        // that gives the triple upgrade its distinguishing look.
        for barrel_i in 0..3u8 {
            let barrel = commands.spawn((
                Mesh2d(barrel_mesh.clone()),
                MeshMaterial2d(turret_mat.clone()),
                Transform::from_xyz(0.0, 3.0, 0.1),
                Visibility::Hidden,
                TurretBarrel,
                BarrelIndex(barrel_i),
                RenderLayers::layer(PLAY_LAYER),
            )).id();
            commands.entity(barrel).insert(ChildOf(turret_id));
        }

        // Yellow painted `H` for the HeliPad deck — three thin
        // rectangles forming an H shape, all hidden by default.
        // `sync_turret_config` toggles them visible iff the slot's
        // weapon is `HeliPad`.
        let h_mat = pm.helipad_h.clone();
        for offset in [
            Vec3::new(-0.7, 0.0, 0.05), // left post
            Vec3::new( 0.7, 0.0, 0.05), // right post
        ] {
            let seg = commands.spawn((
                Mesh2d(h_post_mesh.clone()),
                MeshMaterial2d(h_mat.clone()),
                Transform::from_translation(offset),
                Visibility::Hidden,
                HeliPadDecal,
                RenderLayers::layer(PLAY_LAYER),
            )).id();
            commands.entity(seg).insert(ChildOf(turret_id));
        }
        let bar = commands.spawn((
            Mesh2d(h_bar_mesh.clone()),
            MeshMaterial2d(h_mat.clone()),
            Transform::from_xyz(0.0, 0.0, 0.05),
            Visibility::Hidden,
            HeliPadDecal,
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(bar).insert(ChildOf(turret_id));
    }

}

/// Sweep the friendly hull (+ children: turrets, barrels, decals),
/// the wake trail, and the arena border on the way out of the play
/// world. Runs at `OnEnter(MainMenu)` so the menu screen never has to
/// fight a still-alive player ship rendering behind its chrome.
///
/// `Commands::despawn` is recursive in Bevy 0.15+, so despawning the
/// `Friendly` root takes its descendants along — no per-child
/// bookkeeping required.
pub fn despawn_player_world(
    mut commands: Commands,
    ships: Query<Entity, With<Friendly>>,
    trails: Query<Entity, With<Trail>>,
    borders: Query<Entity, With<PlayBorder>>,
) {
    for e in &ships { commands.entity(e).despawn(); }
    for e in &trails { commands.entity(e).despawn(); }
    for e in &borders { commands.entity(e).despawn(); }
}

/// Build the shared `EffectMeshes` resource — one mesh handle per
/// short-lived FX / per-bullet / per-enemy primitive so spawns
/// reference the same handle and Bevy can batch their draw calls.
fn build_effect_meshes(meshes: &mut Assets<Mesh>) -> EffectMeshes {
    EffectMeshes {
        muzzle_flash:          meshes.add(Capsule2d::new(1.6, 4.0)),
        particle:              meshes.add(Capsule2d::new(0.7, 1.6)),
        bullet_friendly_outer: meshes.add(Capsule2d::new(2.0, 1.5)),
        bullet_friendly_inner: meshes.add(Capsule2d::new(1.3, 1.5)),
        // Round bullet — radii match the friendly capsule's wide
        // dimension so the cannonball can swap meshes without
        // re-tuning the per-weapon scale used in `cannon.rs`.
        bullet_round_outer:    meshes.add(Circle::new(2.0)),
        bullet_round_inner:    meshes.add(Circle::new(1.3)),
        bullet_enemy_outer:    meshes.add(Capsule2d::new(1.5, 1.5)),
        bullet_enemy_inner:    meshes.add(Capsule2d::new(0.8, 1.5)),
        enemy_body:            meshes.add(Capsule2d::new(ENEMY_WIDTH / 2.0, ENEMY_LEN - ENEMY_WIDTH)),
        enemy_turret_base:     meshes.add(Circle::new(1.0)),
        enemy_turret_barrel:   meshes.add(Rectangle::new(0.9, 3.5)),
        bomber_warhead:        meshes.add(Circle::new(1.4)),
        ally_turret_base:      meshes.add(Circle::new(1.4)),
        ally_turret_barrel:    meshes.add(Rectangle::new(1.1, 3.0)),
        bullet_plane_outer:    meshes.add(Capsule2d::new(0.7, 0.8)),
        bullet_plane_inner:    meshes.add(Capsule2d::new(0.4, 0.8)),
        // Missile: longer + thinner than a friendly cannonball so the
        // silhouette reads as a guided projectile in flight.
        bullet_missile_outer:  meshes.add(Capsule2d::new(1.0, 4.0)),
        bullet_missile_inner:  meshes.add(Capsule2d::new(0.6, 4.0)),
        // Mine: dark sphere ~3 wide; red dot at the centre is about
        // a third of that. The dot pulses and the whole mine bobs
        // each frame for the "floating in water" feel.
        mine_outer:            meshes.add(Circle::new(1.5)),
        mine_inner:            meshes.add(Circle::new(0.55)),
        // Boarder: small disc that reads as a tiny crew silhouette
        // when traveling and clustered around the target. Bumped to
        // 0.8 so the boarders read clearly as "people on the rope"
        // rather than being mistaken for bullet pellets.
        boarder_dot:           meshes.add(Circle::new(0.8)),
        beam:                  meshes.add(Rectangle::new(1.0, BEAM_LENGTH)),
    }
}

// ---------- Movement ----------

/// The friendly ship follows the cursor when it's over the play area;
/// otherwise it auto-engages the nearest enemy at a comfortable range.
/// (Wave mode is gone, so the auto-battle short-circuit went with it.)
pub fn friendly_movement(
    time: Res<Time>,
    windows: Query<&Window, With<PrimaryWindow>>,
    stats: Res<crate::stats::PlayerStats>,
    buffs: Res<crate::rune::BuffStacks>,
    enemies: Query<&Transform, (With<Enemy>, Without<Friendly>)>,
    play_camera: Query<&Transform, (With<crate::palette::PlayCamera>, Without<Friendly>, Without<Enemy>)>,
    // LocalPlayer only — the ghost (other peer's ship) is moved by
    // snapshots, not by local input. Iterating all Friendlies here
    // would yank the ghost around on every cursor move.
    // `Without<Enemy>` + `Without<PlayCamera>` make this statically
    // disjoint from the read-only Transform queries above for
    // Bevy's parameter-conflict checker.
    mut q: Query<
        (&mut Transform, &mut Velocity, &mut Heading),
        (
            With<crate::components::LocalPlayer>,
            Without<crate::multiplayer::ghost::RemoteGhost>,
            Without<Enemy>,
            Without<crate::palette::PlayCamera>,
        ),
    >,
) {
    let dt = time.delta_secs();
    let Ok(win) = windows.single() else { return; };
    let cursor = win.cursor_position();

    let (play_left, play_top, play_screen_w, play_screen_h) =
        play_area_screen_rect(win.width(), win.height());

    // Camera offset — when follow-mode shifts the play camera off the
    // origin, the cursor → world conversion needs to slide with it so
    // a click at "the centre of the screen" maps to the centre of the
    // *currently visible* world (the player's location), not to world
    // origin. Zero in fixed-camera mode, so the math is a no-op there.
    let cam_off = play_camera.single()
        .map(|t| t.translation.truncate())
        .unwrap_or(Vec2::ZERO);

    // Cursor over the play area pulls the ship toward it; outside
    // the play area falls through to the auto-engage branch below.
    let target_world: Option<Vec2> = cursor.and_then(|c| {
        if c.x >= play_left && c.x <= play_left + play_screen_w
            && c.y >= play_top && c.y <= play_top + play_screen_h
        {
            let nx = (c.x - play_left) / play_screen_w;
            let ny = (c.y - play_top) / play_screen_h;
            Some(Vec2::new(
                (nx - 0.5) * PLAY_WORLD_W + cam_off.x,
                (0.5 - ny) * PLAY_WORLD_H + cam_off.y,
            ))
        } else {
            None
        }
    });

    for (mut tf, mut vel, mut heading) in &mut q {
        let pos = tf.translation.truncate();

        // Pick a steering target. Cursor over play area → follow it. Otherwise
        // compute a "tactical" target that engages the nearest enemy at a
        // comfortable range, biased toward the centroid when multiple are around.
        let target = if let Some(t) = target_world {
            t
        } else {
            let mut nearest: Option<(f32, Vec2)> = None;
            let mut centroid_sum = Vec2::ZERO;
            let mut centroid_count = 0u32;
            for etf in &enemies {
                let ep = etf.translation.truncate();
                let d = ep.distance(pos);
                if nearest.map_or(true, |(bd, _)| d < bd) { nearest = Some((d, ep)); }
                if d < TURRET_RANGE * 1.5 {
                    centroid_sum += ep;
                    centroid_count += 1;
                }
            }
            if let Some((d, ep)) = nearest {
                let to = ep - pos;
                let unit = to.normalize_or_zero();
                let desired_range = TURRET_RANGE * 0.7;
                if d > desired_range + 8.0 {
                    ep // approach
                } else if d < desired_range - 8.0 {
                    pos - unit * 30.0 // back away
                } else {
                    // Hold range: orbit perpendicularly so multiple turrets bear.
                    // Bias the orbit direction toward the enemy centroid so we
                    // sweep toward where the action is.
                    let perp = Vec2::new(-unit.y, unit.x);
                    let bias = if centroid_count > 0 {
                        let c = centroid_sum / centroid_count as f32;
                        if perp.dot(c - pos) >= 0.0 { 1.0 } else { -1.0 }
                    } else { 1.0 };
                    pos + perp * (bias * 30.0)
                }
            } else {
                Vec2::ZERO // no enemies — drift toward play-area center
            }
        };

        // Keep target inside the playable area so we don't crash the wall.
        // Uses ARENA bounds (which equals viewport unless `big_arena`).
        let margin = HULL_HALF_LEN + 2.0;
        let bound_x = ARENA_W * 0.5 - margin;
        let bound_y = ARENA_H * 0.5 - margin;
        let target = Vec2::new(target.x.clamp(-bound_x, bound_x), target.y.clamp(-bound_y, bound_y));

        let to = target - pos;
        if to.length_squared() > 1.0 {
            let desired = to.y.atan2(to.x) - std::f32::consts::FRAC_PI_2;
            heading.0 = approach_angle(heading.0, desired, stats.turn_speed.effective() * dt);
        }
        let dir = Vec2::new(-heading.0.sin(), heading.0.cos());
        // Rally rune folds in on top of the base move speed via
        // the shared `BuffStacks` engine. Each live stack adds
        // `+1% × rune_effect`. Stacks decay independently in
        // `tick_buff_stacks` so the buff naturally ramps up during
        // a melee killstreak and fades when the kills stop.
        let rally_mult = buffs.linear_mult(
            crate::rune::BuffId::Rally,
            crate::rune::RALLY_PER_STACK,
            stats.rune_damage_mult(),
        );
        vel.0 = dir * stats.move_speed.effective() * rally_mult;
        tf.rotation = Quat::from_rotation_z(heading.0);
    }
}

/// Steer `cur` toward `tgt` by at most `max` radians, taking the shorter way
/// around the circle. Used by friendly + enemy heading systems.
pub fn approach_angle(cur: f32, tgt: f32, max: f32) -> f32 {
    let mut d = (tgt - cur + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
        - std::f32::consts::PI;
    if d > max { d = max; }
    if d < -max { d = -max; }
    cur + d
}

/// Shared movement integrator — composes every modifier that ends up
/// translating an entity each frame:
///   1. **Natural movement** — `Velocity * frost_mult * dt`. AI / control
///      systems set `Velocity` each frame; `OnFrost` (if present) scales
///      it down (each stack compounds via `speed_mult`).
///   2. **Knockback impulse** — `Knockedback.velocity * dt`, NOT scaled by
///      frost. A frost-slowed enemy still gets fully shoved by a
///      cannonball; otherwise frost trivialises crowd-control. The
///      impulse decays per `decay_per_sec` each frame and is removed
///      once it drops below a noticeable threshold.
///
/// Adding a new movement effect (drag, pull, gust, …) is one new
/// `Option<&Mut>` in this query plus an addition to the per-entity
/// translation here — keeps the composition rules centralised.
pub fn apply_velocity(
    time: Res<Time>,
    mut commands: Commands,
    mut q: Query<(
        Entity,
        &mut Transform,
        &Velocity,
        Option<&OnFrost>,
        Option<&crate::components::Stunned>,
        Option<&mut crate::components::Knockedback>,
    )>,
) {
    let dt = time.delta_secs();
    for (entity, mut tf, v, frost, stunned, knock) in &mut q {
        let mult = frost.map(|f| f.speed_mult()).unwrap_or(1.0);
        // Stunned entities don't translate under their own steam, but
        // knockback impulses still apply (an impulse landed while
        // frozen should still shove you — same philosophy as Frost
        // not eating knockback).
        if stunned.is_none() {
            tf.translation.x += v.0.x * mult * dt;
            tf.translation.y += v.0.y * mult * dt;
        }

        if let Some(mut k) = knock {
            tf.translation.x += k.velocity.x * dt;
            tf.translation.y += k.velocity.y * dt;
            // Multiplicative decay each frame. Clamp the multiplier to
            // 0 so a high `decay_per_sec` × low frame-rate doesn't flip
            // the velocity sign.
            let m = (1.0 - k.decay_per_sec * dt).max(0.0);
            k.velocity *= m;
            // Below ~1 unit/sec the impulse is invisible; remove the
            // component so the rest of the schedule stops paying for
            // it (and so future impulses get a clean state to insert
            // into rather than stacking on a dying remnant).
            if k.velocity.length_squared() < 1.0 {
                commands.entity(entity).remove::<crate::components::Knockedback>();
            }
        }
    }
}

/// Decrement every `Stunned`'s remaining time and remove the
/// component once it goes non-positive. Pairs with the
/// `apply_velocity` early-out and the `Without<Stunned>` filters
/// on enemy AI/firing so the effect lifts cleanly the moment the
/// timer runs out.
pub fn tick_stunned(
    time: Res<Time>,
    mut commands: Commands,
    mut q: Query<(Entity, &mut crate::components::Stunned)>,
) {
    let dt = time.delta_secs();
    for (entity, mut s) in &mut q {
        s.remaining -= dt;
        if s.remaining <= 0.0 {
            commands.entity(entity).remove::<crate::components::Stunned>();
        }
    }
}

// ---------- Ram damage ----------

/// Per-enemy cooldown after being rammed. Prevents the player ship
/// dealing 5 dmg/frame for the duration of an overlap — adds a brief
/// grace window that feels like a discrete impact.
#[derive(Component)]
pub struct RamGrace {
    pub remaining: f32,
}

const RAM_DAMAGE_TO_ENEMY: i32 = 5;
/// Self-damage taken by the player ship per collision. Tuned at the
/// same magnitude as the damage dealt out so ramming feels reciprocal
/// — kill an enemy, take a hit yourself.
const RAM_DAMAGE_TO_SELF: i32 = 5;
const RAM_GRACE: f32 = 0.5;
/// Camera trauma added per ram impact. Bumped well above 0.5 so the
/// quadratic `trauma²` shake actually registers — at 0.75 we get
/// roughly 0.56 × `SHAKE_MAX_OFFSET` of peak displacement, which
/// reads clearly without dragging the camera off-target.
const RAM_TRAUMA: f32 = 0.75;

/// Detect overlaps between the friendly ship and any enemy and apply
/// ram damage + a screen-shake kick to *both* sides — ramming costs
/// the player too. Per-enemy `RamGrace` keeps the damage discrete
/// (one tick per physical collision) rather than per-frame while
/// overlapping; `RamSelfGrace` does the same for the ship so each
/// collision only chunks the player once.
pub fn friendly_ram_damage(
    time: Res<Time>,
    mut commands: Commands,
    mut shake: ResMut<crate::modes::ScreenShake>,
    cfg: Res<crate::turret::TurretConfig>,
    stats: Res<crate::stats::PlayerStats>,
    difficulty: Res<crate::Difficulty>,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    mut sfx: crate::sfx::SfxPlayer,
    mut friendly: Query<
        (
            Entity,
            &Transform,
            &Heading,
            &mut Health,
            &mut HitFx,
            Option<&mut crate::stats::Shield>,
            Option<&mut RamSelfGrace>,
            bevy::ecs::query::Has<crate::components::LocalPlayer>,
        ),
        (With<Friendly>, Without<Enemy>),
    >,
    mut enemies: Query<
        (Entity, &Transform, &Enemy, &mut Health, &mut HitFx, Option<&mut RamGrace>),
        Without<Friendly>,
    >,
) {
    let dt = time.delta_secs();
    // MP: host has TWO Friendlies (local + remote-peer ghost). The
    // old `single_mut()` Err'd and skipped contact damage entirely
    // — that's why "bullets work but rams don't" on host. Iterate
    // every friendly so both ships take their own contact hits;
    // `relay_ghost_damage` forwards the ghost's damage to the peer.
    for (fe, f_tf, f_heading, mut fh, mut ffx, f_shield, f_self_grace, f_is_local) in &mut friendly {
        let f_pos = f_tf.translation.truncate();
        let hull_yaw = f_heading.0;

        let mut self_grace_remaining = f_self_grace.as_ref().map(|g| g.remaining).unwrap_or(0.0);
        if let Some(mut g) = f_self_grace {
            g.remaining -= dt;
            if g.remaining <= 0.0 {
                commands.entity(fe).remove::<RamSelfGrace>();
                self_grace_remaining = 0.0;
            } else {
                self_grace_remaining = g.remaining;
            }
        }
        let mut shield_opt = f_shield;

    for (e, etf, en, mut h, mut fx, grace) in &mut enemies {
        // Kamikaze variants (Bomber, Rammer) take the same contact
        // path as everyone else so the impact "feels the same" as a
        // standard collision — what differs is the payload + the
        // explosion. They one-shot themselves on impact and deal
        // their full detonation damage to the player.
        let kamikaze_payload = match en.variant {
            EnemyVariant::Bomber => Some(15),
            EnemyVariant::Rammer => Some(5),
            _                    => None,
        };
        // Tick down any active enemy grace and skip damage while it's hot.
        if let Some(mut g) = grace {
            g.remaining -= dt;
            if g.remaining > 0.0 { continue; }
            commands.entity(e).remove::<RamGrace>();
        }
        if h.0 <= 0 { continue; }

        let ep = etf.translation.truncate();
        let enemy_r = 3.5 * en.variant.scale();
        let r = HULL_HALF_LEN + enemy_r;
        if f_pos.distance_squared(ep) < r * r {
            // Ram damage is base + Spike Plate bonus *only if* the
            // contact lands on the slot side carrying a Spike Plate.
            // Same per-slot mapping the bullet-damage reduction uses,
            // so plate placement matters in both directions. Thorns
            // follows the same side-mapping rule — only the runes on
            // the impacted slot fire, so rune placement matters as
            // much as weapon placement.
            let contact_slot = slot_for_contact(f_pos, ep, hull_yaw);
            let thorns_bonus = contact_slot
                .map(|idx| crate::rune::thorns_contact_bonus_for_slot(
                    &cfg, idx, stats.rune_damage_mult(),
                ))
                .unwrap_or(0);
            let ram_damage = RAM_DAMAGE_TO_ENEMY
                + spiked_plate_contact_bonus(&cfg, f_pos, ep, hull_yaw)
                + thorns_bonus;
            // Damage the enemy + flash. Kamikazes get bulldozed to 0
            // HP regardless of the ram-damage value so they always
            // detonate on first contact, even at the lowest difficulty
            // tier where 5 ram damage wouldn't cleanly kill a 2-HP
            // Bomber if its scaled HP rounded up.
            crate::bullet::apply_damage(&mut h, &mut fx, ram_damage);
            if kamikaze_payload.is_some() {
                h.0 = 0;
            }
            shake.add_trauma(RAM_TRAUMA);
            commands.entity(e).insert(RamGrace { remaining: RAM_GRACE });

            // Self-damage: chip the ship through its shield first,
            // gated by the global self-grace so simultaneous contacts
            // don't stack in one frame. Kamikazes deal their
            // explosion payload (difficulty-scaled) instead of the
            // flat ram self-damage.
            if self_grace_remaining <= 0.0 {
                let mut rng = rand::thread_rng();
                let payload = match kamikaze_payload {
                    Some(base) => difficulty.scale_damage(base),
                    None       => RAM_DAMAGE_TO_SELF,
                };
                crate::bullet::apply_friendly_damage(
                    &mut fh, &mut ffx,
                    shield_opt.as_deref_mut(),
                    &stats, &mut rng,
                    payload, f_is_local,
                );
                commands.entity(fe).insert(RamSelfGrace { remaining: RAM_GRACE });
                self_grace_remaining = RAM_GRACE;
            }

            // Explosion VFX + SFX for kamikaze contact. Mirrors what
            // the old `bomber_detonate` system painted: a two-tone
            // particle burst sized per-variant, plus the explosion
            // SFX. Spawned even if the enemy was already at 0 HP
            // from `apply_damage` above so the particles always read.
            if let (Some(payload), Some(pm), Some(em)) =
                (kamikaze_payload, pm.as_deref(), em.as_deref())
            {
                let _ = payload;
                sfx.play(crate::sfx::Sfx::Explosion);
                let (n1, n2, sp1, sp2) = match en.variant {
                    EnemyVariant::Bomber => (14, 8, 80.0, 100.0),
                    EnemyVariant::Rammer => (8,  4, 60.0, 80.0),
                    _ => (0, 0, 0.0, 0.0),
                };
                if n1 > 0 {
                    let mut rng = rand::thread_rng();
                    crate::effects::spawn_hit_particles(
                        &mut commands, em, &pm.enemy,        ep, n1, sp1, &mut rng,
                    );
                    crate::effects::spawn_hit_particles(
                        &mut commands, em, &pm.bullet_enemy, ep, n2, sp2, &mut rng,
                    );
                }
            }
        }
    }
    }  // end per-friendly loop
}

/// Mirror of `RamGrace` but on the friendly ship. Throttles
/// self-damage so a multi-enemy mash doesn't delete the player in one
/// frame.
#[derive(Component)]
pub struct RamSelfGrace {
    pub remaining: f32,
}

/// Map a contact-direction (enemy relative to ship) to the turret
/// slot index whose mount angle is closest. Returns `None` if the
/// enemy is sitting exactly on top of the ship (no direction to
/// resolve). Pulled out so `spiked_plate_contact_bonus` and
/// `thorns_contact_bonus_for_slot` share one slot-mapping rule
/// — Spike Plate and Thorns both read as "the slot on the side
/// the enemy hit you on" from the player's POV.
pub fn slot_for_contact(
    ship_pos: Vec2,
    enemy_pos: Vec2,
    hull_yaw: f32,
) -> Option<usize> {
    let dir = enemy_pos - ship_pos;
    if dir.length_squared() < 0.001 { return None; }
    let world_angle = (-dir.x).atan2(dir.y);
    let mut local_angle = world_angle - hull_yaw;
    local_angle = (local_angle + std::f32::consts::PI)
        .rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
    let mut best: Option<(usize, f32)> = None;
    for (i, &mount) in crate::balance::TURRET_MOUNTS.iter().enumerate() {
        let mut delta = local_angle - mount;
        delta = (delta + std::f32::consts::PI)
            .rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
        let abs = delta.abs();
        if best.map_or(true, |(_, b)| abs < b) {
            best = Some((i, abs));
        }
    }
    best.map(|(idx, _)| idx)
}

/// Bonus damage added to a ram hit per equipped Spike Plate slot,
/// regardless of which side of the hull the rammed enemy contacted.
/// Stacks linearly — 3 plates = +15 ram damage on every contact.
/// Used to be per-side (matching `spiked_plate_reduction`) but the
/// hidden mapping made stacked plates feel like nothing was
/// happening; going global makes each plate a legible +5 to ram.
fn spiked_plate_contact_bonus(
    cfg: &crate::turret::TurretConfig,
    _ship_pos: Vec2,
    _enemy_pos: Vec2,
    _hull_yaw: f32,
) -> i32 {
    let plate_count = cfg.slots.iter().filter(|s| {
        s.equipped && matches!(s.weapon, crate::weapon::WeaponType::SpikedPlate)
    }).count() as i32;
    plate_count * crate::balance::SPIKED_PLATE_DAMAGE_BONUS
}
