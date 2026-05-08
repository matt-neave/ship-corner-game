//! Friendly-ship setup (hull + 8 turrets + dock + pier grid + trail), and
//! the per-frame movement system (mouse-follow in Sandbox, auto-engage in
//! Wave). Also hosts `apply_velocity` since it's a tiny shared integrator and
//! `approach_angle` since it's used by both ship + enemy heading code.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;
use bevy::window::PrimaryWindow;

use crate::balance::{
    BEAM_LENGTH, ENEMY_LEN, ENEMY_WIDTH, FRIENDLY_SPEED, FRIENDLY_TURN_RATE, FROST_SPEED_MULT,
    HULL_HALF_LEN, HULL_LEN, HULL_WIDTH, PIER_CELL_W, PIER_CELL_X, PIER_Y_START, PIER_Y_STEP,
    PLAY_LAYER, PLAY_WORLD, TURRET_MOUNTS, TURRET_POSITIONS, TURRET_RANGE,
};
use crate::components::{Faction, FactionKind, Friendly, Health, Heading, Velocity};
use crate::effects::{EffectMeshes, HitFx};
use crate::enemy::Enemy;
use crate::modes::{effective_ui_width, play_area_screen_rect, GameMode, WindowMode};
use crate::palette::{Palette, PaletteMaterials};
use crate::pier::PierVisual;
use crate::rune::{FireExtent, OnFrost};
use crate::trails::{empty_dynamic_mesh, Trail};
use crate::turret::{BarrelIndex, TurretBarrel, TurretConfig, TurretSlot};

// ---------- Setup ----------

/// Spawn everything that belongs in the play world: borders, friendly trail,
/// hull, dock + pier grid, 8 turrets, and the cached `EffectMeshes` /
/// `PaletteMaterials` resources that downstream systems pull from.
pub fn setup_world(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    cfg: Res<TurretConfig>,
    palette: Res<Palette>,
) {
    // Build palette-material handles once. Every entity in the play world
    // references one of these — runtime palette swaps update them all.
    let pm = PaletteMaterials::build(&palette, &mut materials);

    // 1px play-area border, drawn inside the play world (z=6 so it always
    // frames the action).
    let border_h = meshes.add(Rectangle::new(PLAY_WORLD, 1.0));
    let border_v = meshes.add(Rectangle::new(1.0, PLAY_WORLD));
    let half_w = PLAY_WORLD / 2.0 - 0.5;
    for (m, x, y) in [
        (border_h.clone(), 0.0,  half_w),
        (border_h.clone(), 0.0, -half_w),
        (border_v.clone(),  half_w, 0.0),
        (border_v.clone(), -half_w, 0.0),
    ] {
        commands.spawn((
            Mesh2d(m),
            MeshMaterial2d(pm.border.clone()),
            Transform::from_xyz(x, y, 6.0),
            RenderLayers::layer(PLAY_LAYER),
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
        Faction(FactionKind::Friendly),
        Health(100),
        Velocity(Vec2::new(0.0, FRIENDLY_SPEED)),
        Heading(0.0),
        HitFx::new(pm.hull.clone()),
        FireExtent(Vec2::new(HULL_WIDTH * 0.5, HULL_LEN * 0.5)),
        RenderLayers::layer(PLAY_LAYER),
    )).id();

    // Pier grid — 8 stacked cells along the LHS wall, drawn as thin grid
    // lines. Doubles as the dock visual and the placement surface for upgrade
    // buildings. Hidden until Wave mode.
    let pier_top    = PIER_Y_START - PIER_Y_STEP / 2.0;
    let pier_bottom = PIER_Y_START + (8.0 - 1.0) * PIER_Y_STEP + PIER_Y_STEP / 2.0;
    let pier_height = pier_bottom - pier_top;
    let h_line_mesh = meshes.add(Rectangle::new(PIER_CELL_W, 1.0));
    let v_line_mesh = meshes.add(Rectangle::new(1.0, pier_height));

    // 9 horizontal lines (top + bottom + 7 separators) + 2 vertical edges.
    for i in 0..=8 {
        let y = pier_top + i as f32 * PIER_Y_STEP;
        commands.spawn((
            Mesh2d(h_line_mesh.clone()),
            MeshMaterial2d(pm.border.clone()),
            Transform::from_xyz(PIER_CELL_X, y, 0.4),
            Visibility::Hidden,
            PierVisual,
            RenderLayers::layer(PLAY_LAYER),
        ));
    }
    for x_off in [-PIER_CELL_W / 2.0, PIER_CELL_W / 2.0] {
        commands.spawn((
            Mesh2d(v_line_mesh.clone()),
            MeshMaterial2d(pm.border.clone()),
            Transform::from_xyz(PIER_CELL_X + x_off, (pier_top + pier_bottom) / 2.0, 0.4),
            Visibility::Hidden,
            PierVisual,
            RenderLayers::layer(PLAY_LAYER),
        ));
    }

    // Friendly turrets. Barrel kept ≥1.5 wide so it doesn't alias to zero
    // pixels at off-axis rotations now that MSAA is off — sub-pixel rects
    // were vanishing entirely between integer-grid angles.
    let base_mesh = meshes.add(Circle::new(2.0));
    let barrel_mesh = meshes.add(Rectangle::new(1.5, 4.0));

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
                rune: slot.rune,
            },
            RenderLayers::layer(PLAY_LAYER),
        ));
        ec.insert(ChildOf(ship));
        let turret_id = ec.id();

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
    }

    // Cache effect meshes once so muzzle flashes / hit particles don't
    // allocate. Bullet + enemy primitives are also cached here so every bullet
    // / enemy can share the same mesh handle and benefit from Bevy's
    // draw-call batching. Built locally so we can pass `&em` to the ally
    // spawn below before handing the resource off to the ECS.
    let em = EffectMeshes {
        muzzle_flash:          meshes.add(Capsule2d::new(1.6, 4.0)),
        particle:              meshes.add(Capsule2d::new(0.7, 1.6)),
        bullet_friendly_outer: meshes.add(Capsule2d::new(2.0, 1.5)),
        bullet_friendly_inner: meshes.add(Capsule2d::new(1.3, 1.5)),
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
        // Mine: dark sphere ~3 wide; red dot at the center is half that.
        mine_outer:            meshes.add(Circle::new(1.5)),
        mine_inner:            meshes.add(Circle::new(0.6)),
        beam:                  meshes.add(Rectangle::new(1.0, BEAM_LENGTH)),
    };

    // Seed allied fleet — one Pirate Ship + one Carrier + one Submarine.
    // Carrier launches its own air wing on startup; planes are spawned
    // inside `spawn_ally` when class is Carrier.
    crate::ally::spawn_ally(
        &mut commands,
        &pm,
        &em,
        &mut meshes,
        Vec2::new(-30.0, 30.0),
        std::f32::consts::FRAC_PI_2,
        crate::ally::ShipClass::PirateShip,
    );
    crate::ally::spawn_ally(
        &mut commands,
        &pm,
        &em,
        &mut meshes,
        Vec2::new(30.0, -30.0),
        std::f32::consts::FRAC_PI_2,
        crate::ally::ShipClass::Carrier,
    );
    crate::ally::spawn_ally(
        &mut commands,
        &pm,
        &em,
        &mut meshes,
        Vec2::new(0.0, -50.0),
        std::f32::consts::FRAC_PI_2,
        crate::ally::ShipClass::Submarine,
    );
    crate::ally::spawn_ally(
        &mut commands,
        &pm,
        &em,
        &mut meshes,
        Vec2::new(50.0, 30.0),
        std::f32::consts::FRAC_PI_2,
        crate::ally::ShipClass::Minelayer,
    );
    crate::ally::spawn_ally(
        &mut commands,
        &pm,
        &em,
        &mut meshes,
        Vec2::new(-15.0, 0.0),
        std::f32::consts::FRAC_PI_2,
        crate::ally::ShipClass::Tender,
    );

    // Hand both off to the ECS so other systems can pick them up.
    commands.insert_resource(pm);
    commands.insert_resource(em);
}

// ---------- Movement ----------

/// Sandbox: the friendly ship follows the cursor when it's over the play area;
/// otherwise it auto-engages the nearest enemy at a comfortable range.
/// Wave mode: cursor is ignored (true auto-battle).
pub fn friendly_movement(
    time: Res<Time>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mode: Res<WindowMode>,
    game_mode: Res<GameMode>,
    enemies: Query<&Transform, (With<Enemy>, Without<Friendly>)>,
    mut q: Query<(&mut Transform, &mut Velocity, &mut Heading), With<Friendly>>,
) {
    let dt = time.delta_secs();
    let Ok(win) = windows.single() else { return; };
    let cursor = win.cursor_position();

    let (play_left, play_top, play_screen) =
        play_area_screen_rect(win.width(), win.height(), effective_ui_width(&mode));

    // Wave mode is fully auto-battle — ignore the cursor entirely so the
    // enemy-seeking branch below takes over regardless of mouse position.
    let target_world: Option<Vec2> = if matches!(*game_mode, GameMode::Wave) {
        None
    } else {
        cursor.and_then(|c| {
            if c.x >= play_left && c.x <= play_left + play_screen
                && c.y >= play_top && c.y <= play_top + play_screen
            {
                let nx = (c.x - play_left) / play_screen;
                let ny = (c.y - play_top) / play_screen;
                Some(Vec2::new((nx - 0.5) * PLAY_WORLD, (0.5 - ny) * PLAY_WORLD))
            } else {
                None
            }
        })
    };

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
        let margin = HULL_HALF_LEN + 2.0;
        let bound = PLAY_WORLD / 2.0 - margin;
        let target = Vec2::new(target.x.clamp(-bound, bound), target.y.clamp(-bound, bound));

        let to = target - pos;
        if to.length_squared() > 1.0 {
            let desired = to.y.atan2(to.x) - std::f32::consts::FRAC_PI_2;
            heading.0 = approach_angle(heading.0, desired, FRIENDLY_TURN_RATE * dt);
        }
        let dir = Vec2::new(-heading.0.sin(), heading.0.cos());
        vel.0 = dir * FRIENDLY_SPEED;
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

/// Tiny shared integrator: `position += velocity * dt` for everything that
/// has both components. Runs once per frame. Entities with the `OnFrost`
/// status are slowed by `FROST_SPEED_MULT` — the integrator is the right
/// place since it's the single point where Velocity becomes movement.
pub fn apply_velocity(
    time: Res<Time>,
    mut q: Query<(&mut Transform, &Velocity, Option<&OnFrost>)>,
) {
    let dt = time.delta_secs();
    for (mut tf, v, frost) in &mut q {
        let mult = if frost.is_some() { FROST_SPEED_MULT } else { 1.0 };
        tf.translation.x += v.0.x * mult * dt;
        tf.translation.y += v.0.y * mult * dt;
    }
}
