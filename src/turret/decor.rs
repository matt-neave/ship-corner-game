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

/// Side-by-side spacing between the two spike triangles. Tuned so the
/// pair reads as a "row of spikes" rather than a single chunky shape.
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
        // needed (the slot only owns 2 spikes max).
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
        // mount angle). Built once and shared across both spikes +
        // every SpikedPlate slot in the same frame.
        let mesh_h = spike_mesh
            .get_or_insert_with(|| meshes.add(Triangle2d::new(
                Vec2::new(-SPIKE_BASE_W * 0.5, 0.0),
                Vec2::new( SPIKE_BASE_W * 0.5, 0.0),
                Vec2::new( 0.0, SPIKE_HEIGHT),
            )))
            .clone();

        for side in [-1.0_f32, 1.0_f32] {
            let lateral = side * SPIKE_LATERAL_GAP * 0.5;
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

        // Three dots arranged on an equilateral triangle. Starting
        // angle of `PI/2` puts the top dot pointing along the
        // slot's mount-forward (+Y) direction, with the other two
        // splayed back-left and back-right.
        for i in 0..3 {
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
