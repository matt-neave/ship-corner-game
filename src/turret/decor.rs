//! Per-weapon deck visuals for slots that don't fire bullets:
//! - `SpikedPlate` — two triangle spikes pointing in the slot's mount
//!   direction, reading as "armoured forward edge".
//! - `Amplifier` — three small accent dots arranged in a triangle on
//!   top of the deck, hinting at "three rune sockets broadcasting".
//!
//! Both visuals follow the same parent-child pattern as `blade.rs`:
//! the decor entities are children of the turret-slot entity, so they
//! inherit the slot's `mount_angle` rotation for free. Swap the slot
//! to a different weapon and `sync_*_decor` tears the decor down.
//!
//! Cheap to despawn/respawn — `TurretConfig` only changes on shop
//! interactions, not per frame.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;

use crate::balance::PLAY_LAYER;
use crate::palette::PaletteMaterials;
use crate::turret::{TurretConfig, TurretSlot};
use crate::weapon::WeaponType;

// ---------- Spike plate decor ----------

#[derive(Component)]
pub struct SpikedDecor;

/// Side-by-side spacing between adjacent spikes in a multi-spike
/// unit. Tuned so the row reads as distinct teeth rather than a
/// single chunky shape.
const SPIKE_LATERAL_GAP: f32 = 1.6;
/// Base width of each triangle (in world units).
const SPIKE_BASE_W: f32 = 1.8;
/// Height of each triangle from its base to the tip.
const SPIKE_HEIGHT: f32 = 2.6;
/// Distance from the slot centre to the BASE of each spike. Pushes
/// the spikes outward so the tip clears the deck pad.
const SPIKE_FORWARD_OFFSET: f32 = 1.6;

pub fn sync_spiked_decor(
    mut commands: Commands,
    cfg: Res<TurretConfig>,
    pm: Option<Res<PaletteMaterials>>,
    mut meshes: ResMut<Assets<Mesh>>,
    slots: Query<(Entity, &TurretSlot, Option<&Children>)>,
    existing: Query<Entity, With<SpikedDecor>>,
) {
    if !cfg.is_changed() { return; }
    let Some(pm) = pm else { return; };

    let mut spike_mesh: Option<Handle<Mesh>> = None;

    for (slot_entity, slot, children) in &slots {
        let s = cfg.slots[slot.index];
        let want = s.equipped && matches!(s.weapon, WeaponType::SpikedPlate);

        // Tear down any existing spike children regardless — the
        // decor is rebuilt fresh on every cfg change, no diffing
        // needed (the slot only owns at most `barrels` spikes).
        let existing_children: Vec<Entity> = children
            .into_iter()
            .flat_map(|c| c.iter())
            .filter(|c| existing.get(*c).is_ok())
            .collect();
        for e in existing_children {
            commands.entity(e).despawn();
        }

        if !want { continue; }

        // Triangle pointing along local +Y (the slot's mount-forward
        // direction since the slot entity is already rotated to its
        // mount angle). Built once and shared across every spike +
        // every SpikedPlate slot in the same frame.
        let mesh_h = spike_mesh
            .get_or_insert_with(|| meshes.add(Triangle2d::new(
                Vec2::new(-SPIKE_BASE_W * 0.5, 0.0),
                Vec2::new( SPIKE_BASE_W * 0.5, 0.0),
                Vec2::new( 0.0, SPIKE_HEIGHT),
            )))
            .clone();

        // Spike count scales with `barrels` — 1/2/3 tiers map to
        // 1/2/3 teeth on the plate. Layout mirrors the standard
        // multi-barrel turret: centre when n=1, port+stbd when n=2,
        // port+centre+stbd when n=3.
        let n = s.barrels.clamp(1, 3);
        for i in 0..n {
            let lateral = match (n, i) {
                (1, _) => 0.0,
                (2, 0) => -SPIKE_LATERAL_GAP * 0.5,
                (2, _) =>  SPIKE_LATERAL_GAP * 0.5,
                (3, 0) => -SPIKE_LATERAL_GAP,
                (3, 1) =>  0.0,
                (3, _) =>  SPIKE_LATERAL_GAP,
                _ => 0.0,
            };
            let spike = commands.spawn((
                Mesh2d(mesh_h.clone()),
                MeshMaterial2d(pm.bullet_friendly.clone()),
                // Tip colour comes from the existing "bright steel"
                // material — same one the bullet_friendly inner uses,
                // reads as polished metal on the deck base.
                Transform::from_xyz(lateral, SPIKE_FORWARD_OFFSET, 0.05),
                SpikedDecor,
                RenderLayers::layer(PLAY_LAYER),
            )).id();
            commands.entity(spike).insert(ChildOf(slot_entity));
        }
    }
}

// ---------- Crow's Nest decor ----------

#[derive(Component)]
pub struct CrowsNestDecor;

/// Height of the mast pole rising from the deck pad. Reads as a
/// vertical post even at the chunky-pixel scale.
const NEST_MAST_HEIGHT: f32 = 4.0;
const NEST_MAST_WIDTH: f32 = 0.9;
/// Outer width of the lookout bucket — wider than the mast so the
/// silhouette reads as "the deck post with a perched barrel."
const NEST_BUCKET_W: f32 = 4.2;
const NEST_BUCKET_H: f32 = 2.4;
/// Inner cavity of the bucket (where the lookout stands) — drawn
/// in the lighter "platform" tone over the darker bucket rim to
/// fake a rim/interior split.
const NEST_BUCKET_INNER_W: f32 = 2.8;
const NEST_BUCKET_INNER_H: f32 = 1.4;
/// Lookout figure — a small dark dot sitting in the bucket.
const NEST_LOOKOUT_R: f32 = 0.55;
/// Pennant flag flying off the bucket's top — narrow rectangle
/// canted slightly so the silhouette reads as a banner.
const NEST_FLAG_W: f32 = 2.6;
const NEST_FLAG_H: f32 = 0.8;

pub fn sync_crows_nest_decor(
    mut commands: Commands,
    cfg: Res<TurretConfig>,
    pm: Option<Res<PaletteMaterials>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    slots: Query<(Entity, &TurretSlot, Option<&Children>)>,
    existing: Query<Entity, With<CrowsNestDecor>>,
) {
    if !cfg.is_changed() { return; }
    let Some(pm) = pm else { return; };

    let mut mast_mesh:  Option<Handle<Mesh>> = None;
    let mut bucket_mesh: Option<Handle<Mesh>> = None;
    let mut bucket_inner_mesh: Option<Handle<Mesh>> = None;
    let mut lookout_mesh: Option<Handle<Mesh>> = None;
    let mut flag_mesh: Option<Handle<Mesh>> = None;
    let mut lookout_mat: Option<Handle<ColorMaterial>> = None;
    let mut flag_mat: Option<Handle<ColorMaterial>> = None;

    for (slot_entity, slot, children) in &slots {
        let s = cfg.slots[slot.index];
        let want = s.equipped && matches!(s.weapon, WeaponType::CrowsNest);

        let existing_children: Vec<Entity> = children
            .into_iter()
            .flat_map(|c| c.iter())
            .filter(|c| existing.get(*c).is_ok())
            .collect();
        for e in existing_children {
            commands.entity(e).despawn();
        }

        if !want { continue; }

        let mast_h = mast_mesh
            .get_or_insert_with(|| meshes.add(Rectangle::new(NEST_MAST_WIDTH, NEST_MAST_HEIGHT)))
            .clone();
        let bucket_h = bucket_mesh
            .get_or_insert_with(|| meshes.add(Rectangle::new(NEST_BUCKET_W, NEST_BUCKET_H)))
            .clone();
        let bucket_inner_h = bucket_inner_mesh
            .get_or_insert_with(|| meshes.add(Rectangle::new(NEST_BUCKET_INNER_W, NEST_BUCKET_INNER_H)))
            .clone();
        let lookout_h = lookout_mesh
            .get_or_insert_with(|| meshes.add(Circle::new(NEST_LOOKOUT_R)))
            .clone();
        let flag_h = flag_mesh
            .get_or_insert_with(|| meshes.add(Rectangle::new(NEST_FLAG_W, NEST_FLAG_H)))
            .clone();
        let lookout_m = lookout_mat
            .get_or_insert_with(|| materials.add(Color::srgb(0.10, 0.10, 0.12)))
            .clone();
        let flag_m = flag_mat
            .get_or_insert_with(|| materials.add(Color::srgb(0.92, 0.22, 0.18)))
            .clone();

        // Mast — dark wood post running up from the deck pad.
        let mast = commands.spawn((
            Mesh2d(mast_h),
            MeshMaterial2d(pm.turret_crows_nest.clone()),
            Transform::from_xyz(0.0, NEST_MAST_HEIGHT * 0.5, 0.05),
            CrowsNestDecor,
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(mast).insert(ChildOf(slot_entity));

        // Bucket — wider barrel sitting on the mast tip. Painted in
        // the dark mast tone so its outer rim reads as the same
        // weathered wood, with the lighter interior painted on top.
        let bucket_y = NEST_MAST_HEIGHT;
        let bucket = commands.spawn((
            Mesh2d(bucket_h),
            MeshMaterial2d(pm.turret_crows_nest.clone()),
            Transform::from_xyz(0.0, bucket_y, 0.08),
            CrowsNestDecor,
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(bucket).insert(ChildOf(slot_entity));

        // Bucket interior — lighter plank tone, slightly inset, so
        // the bucket reads as "rim + interior" rather than one slab.
        let inner = commands.spawn((
            Mesh2d(bucket_inner_h),
            MeshMaterial2d(pm.crows_nest_top.clone()),
            Transform::from_xyz(0.0, bucket_y - 0.15, 0.11),
            CrowsNestDecor,
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(inner).insert(ChildOf(slot_entity));

        // Lookout figure — small dark dot sitting in the bucket.
        // Gives the silhouette a human cue without needing pixel
        // art.
        let lookout = commands.spawn((
            Mesh2d(lookout_h),
            MeshMaterial2d(lookout_m),
            Transform::from_xyz(0.0, bucket_y + 0.25, 0.14),
            CrowsNestDecor,
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(lookout).insert(ChildOf(slot_entity));

        // Red pennant flag, planted slightly off-centre and tilted
        // a few degrees so it reads as "fluttering in the wind"
        // even without animation. Anchored at the bucket's right
        // rim, extending outward.
        let flag = commands.spawn((
            Mesh2d(flag_h),
            MeshMaterial2d(flag_m),
            Transform {
                translation: Vec3::new(NEST_BUCKET_W * 0.5 + NEST_FLAG_W * 0.4, bucket_y + 1.6, 0.12),
                rotation: Quat::from_rotation_z(0.18),
                scale: Vec3::ONE,
            },
            CrowsNestDecor,
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(flag).insert(ChildOf(slot_entity));
    }
}

// ---------- Flamethrower nozzle decor ----------

#[derive(Component)]
pub struct FlamethrowerNozzle;

/// Short nozzle length pointing forward from the deck pad. Reads as
/// "this is where the flame comes out" — without it the slot just
/// looks like a coloured circle.
const NOZZLE_LENGTH: f32 = 3.0;
/// Nozzle width — narrow so it looks like a focused tube, matching
/// the new tight 20° cone.
const NOZZLE_WIDTH: f32 = 1.2;
/// Distance from slot centre to the BASE of the nozzle. Pushes it
/// off the deck-pad sprite so it visibly extends outward.
const NOZZLE_FORWARD_OFFSET: f32 = 1.4;

pub fn sync_flamethrower_decor(
    mut commands: Commands,
    cfg: Res<TurretConfig>,
    pm: Option<Res<PaletteMaterials>>,
    mut meshes: ResMut<Assets<Mesh>>,
    slots: Query<(Entity, &TurretSlot, Option<&Children>)>,
    existing: Query<Entity, With<FlamethrowerNozzle>>,
) {
    if !cfg.is_changed() { return; }
    let Some(pm) = pm else { return; };

    let mut nozzle_mesh: Option<Handle<Mesh>> = None;

    for (slot_entity, slot, children) in &slots {
        let s = cfg.slots[slot.index];
        let want = s.equipped && matches!(s.weapon, WeaponType::Flamethrower);

        let existing_children: Vec<Entity> = children
            .into_iter()
            .flat_map(|c| c.iter())
            .filter(|c| existing.get(*c).is_ok())
            .collect();
        for e in existing_children {
            commands.entity(e).despawn();
        }

        if !want { continue; }

        // Rectangle centred at +NOZZLE_LENGTH/2 so the BASE sits on
        // the deck pad and the tip projects forward by NOZZLE_LENGTH.
        // Local +Y is the slot's mount-forward direction (the slot
        // entity is already rotated by `mount_angle`).
        let mesh_h = nozzle_mesh
            .get_or_insert_with(|| meshes.add(Rectangle::new(NOZZLE_WIDTH, NOZZLE_LENGTH)))
            .clone();
        let nozzle = commands.spawn((
            Mesh2d(mesh_h),
            // Reuse the flamethrower deck tint so the nozzle reads
            // as part of the same machinery, not a foreign piece.
            MeshMaterial2d(pm.turret_flamethrower.clone()),
            Transform::from_xyz(0.0, NOZZLE_FORWARD_OFFSET + NOZZLE_LENGTH * 0.5, 0.05),
            FlamethrowerNozzle,
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(nozzle).insert(ChildOf(slot_entity));
    }
}

// ---------- Amplifier decor ----------

#[derive(Component)]
pub struct AmplifierDecor;

/// Radius of each accent dot.
const AMP_DOT_RADIUS: f32 = 0.55;
/// Distance from slot centre to each dot's centre, traced around a
/// regular triangle so the three dots evoke "three rune sockets
/// broadcasting" without overlapping the deck pad.
const AMP_DOT_RING: f32 = 1.5;

pub fn sync_amplifier_decor(
    mut commands: Commands,
    cfg: Res<TurretConfig>,
    pm: Option<Res<PaletteMaterials>>,
    mut meshes: ResMut<Assets<Mesh>>,
    slots: Query<(Entity, &TurretSlot, Option<&Children>)>,
    existing: Query<Entity, With<AmplifierDecor>>,
) {
    if !cfg.is_changed() { return; }
    let Some(pm) = pm else { return; };

    let mut dot_mesh: Option<Handle<Mesh>> = None;

    for (slot_entity, slot, children) in &slots {
        let s = cfg.slots[slot.index];
        let want = s.equipped && matches!(s.weapon, WeaponType::Amplifier);

        let existing_children: Vec<Entity> = children
            .into_iter()
            .flat_map(|c| c.iter())
            .filter(|c| existing.get(*c).is_ok())
            .collect();
        for e in existing_children {
            commands.entity(e).despawn();
        }

        if !want { continue; }

        let mesh_h = dot_mesh
            .get_or_insert_with(|| meshes.add(Circle::new(AMP_DOT_RADIUS)))
            .clone();

        // Dot count tracks the Amplifier's tier — each barrel level
        // represents one additional rune the Amplifier can broadcast,
        // so the visible dots mirror its current sharing capacity.
        let dot_count = s.barrels.clamp(1, 3) as usize;
        // Dots arranged on the upper arc of the regular triangle,
        // starting at the mount-forward (+Y) direction. With one dot
        // we just place it forward; with two or three we splay across
        // the triangle's vertices.
        for i in 0..dot_count {
            let theta = std::f32::consts::FRAC_PI_2
                + (i as f32) * std::f32::consts::TAU / 3.0;
            let x = AMP_DOT_RING * theta.cos();
            let y = AMP_DOT_RING * theta.sin();
            let dot = commands.spawn((
                Mesh2d(mesh_h.clone()),
                MeshMaterial2d(pm.bullet_friendly.clone()),
                Transform::from_xyz(x, y, 0.05),
                AmplifierDecor,
                RenderLayers::layer(PLAY_LAYER),
            )).id();
            commands.entity(dot).insert(ChildOf(slot_entity));
        }
    }
}

// ---------- SharkNet deck decor ----------

#[derive(Component)]
pub struct SharkNetDeck;

/// Outer dark square — fully obscures the base disc behind it.
const NET_OUTER: f32 = 5.0;
/// Inner cell square — slightly lighter, recessed pen feel.
const NET_INNER: f32 = 3.4;
/// Cross-strand thickness for the `+` net pattern.
const NET_STRAND_THICK: f32 = 0.6;

/// SharkNet slot visual: a square "net" that fully covers the base
/// turret disc so the slot reads as a holding pen, not a cannon.
/// Uses the dark `turret_sharknet` material for the outer plate and
/// the light `bullet_friendly` cyan for the inner cell + cross
/// strands.
pub fn sync_sharknet_decor(
    mut commands: Commands,
    cfg: Res<TurretConfig>,
    pm: Option<Res<PaletteMaterials>>,
    mut meshes: ResMut<Assets<Mesh>>,
    slots: Query<(Entity, &TurretSlot, Option<&Children>)>,
    existing: Query<Entity, With<SharkNetDeck>>,
) {
    if !cfg.is_changed() { return; }
    let Some(pm) = pm else { return; };

    let outer_mesh = meshes.add(Rectangle::new(NET_OUTER, NET_OUTER));
    let inner_mesh = meshes.add(Rectangle::new(NET_INNER, NET_INNER));
    let strand_h_mesh = meshes.add(Rectangle::new(NET_INNER, NET_STRAND_THICK));
    let strand_v_mesh = meshes.add(Rectangle::new(NET_STRAND_THICK, NET_INNER));

    for (slot_entity, slot, children) in &slots {
        let s = cfg.slots[slot.index];
        let want = s.equipped && matches!(s.weapon, WeaponType::SharkNet);

        let existing_children: Vec<Entity> = children
            .into_iter()
            .flat_map(|c| c.iter())
            .filter(|c| existing.get(*c).is_ok())
            .collect();
        for e in existing_children {
            commands.entity(e).despawn();
        }

        if !want { continue; }

        // Outer dark plate — sits above the base disc and fully
        // covers it. z bumped well above the disc's z=2.0 so the
        // 2D sort puts the net on top no matter the depth-buffer
        // behaviour for transparent quads.
        let outer = commands.spawn((
            Mesh2d(outer_mesh.clone()),
            MeshMaterial2d(pm.turret_sharknet.clone()),
            Transform::from_xyz(0.0, 0.0, 1.0),
            SharkNetDeck,
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(outer).insert(ChildOf(slot_entity));

        // Inner light cell — pen interior.
        let inner = commands.spawn((
            Mesh2d(inner_mesh.clone()),
            MeshMaterial2d(pm.bullet_friendly.clone()),
            Transform::from_xyz(0.0, 0.0, 1.05),
            SharkNetDeck,
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(inner).insert(ChildOf(slot_entity));

        // Cross strands — net pattern over the cell.
        let strand_h = commands.spawn((
            Mesh2d(strand_h_mesh.clone()),
            MeshMaterial2d(pm.turret_sharknet.clone()),
            Transform::from_xyz(0.0, 0.0, 1.10),
            SharkNetDeck,
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(strand_h).insert(ChildOf(slot_entity));

        let strand_v = commands.spawn((
            Mesh2d(strand_v_mesh.clone()),
            MeshMaterial2d(pm.turret_sharknet.clone()),
            Transform::from_xyz(0.0, 0.0, 1.10),
            SharkNetDeck,
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(strand_v).insert(ChildOf(slot_entity));
    }
}
