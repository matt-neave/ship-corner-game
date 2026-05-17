//! Drop-shadow primitive for above-ground hulls.
//!
//! Mirrors the SNKRX `shadow_canvas` effect on a per-entity basis:
//! every shadowable hull gets a sibling silhouette in translucent
//! black, offset down-right in WORLD space (always the same screen
//! direction, regardless of hull rotation). A single per-frame
//! sync system follows the source entity's `GlobalTransform` —
//! 1-frame lag for shadows is invisible to the player.
//!
//! Usage:
//! ```ignore
//! let id = commands.spawn((Mesh2d(body), ...)).id();
//! shadow::spawn_for(&mut commands, &mut materials, body_mesh.clone(), id, scale);
//! ```
//!
//! The shadow despawns automatically when its source is gone (sync
//! system checks via `Query::get`).

use bevy::prelude::*;
use bevy::render::view::RenderLayers;

use crate::balance::PLAY_LAYER;

/// Sibling shadow entity. `source` points back at the hull whose
/// `GlobalTransform` drives this shadow each frame. `offset` is
/// the world-space displacement from the source — sea-level
/// objects use [`SHADOW_OFFSET`], airborne objects (helicopter)
/// pass a bigger offset to fake altitude.
#[derive(Component)]
pub struct ShadowOf {
    pub source: Entity,
    pub offset: Vec2,
}

/// World-space offset (always the same screen direction) from a
/// surface-level source to its shadow. Matches
/// `ship::SHIP_SHADOW_OFFSET` so every ground-level shadow lights
/// from the same imaginary top-left source.
pub const SHADOW_OFFSET: Vec2 = Vec2::new(1.5, -1.5);

/// Bigger offset for airborne objects (helicopter). A larger
/// distance between the body and its shadow reads as "this is
/// above the water" — the same trick top-down arcade games use to
/// fake altitude without an actual third axis.
pub const SHADOW_OFFSET_AIR: Vec2 = Vec2::new(5.0, -5.0);

/// Z below typical hull z (~1.0) but above the water trail (~0.5)
/// so shadows paint between them.
pub const SHADOW_Z: f32 = 0.75;

/// Default shadow material colour. Same dark translucent black the
/// hull shadow uses so every silhouette reads as the same shading.
pub fn material_color() -> Color {
    Color::srgba(0.0, 0.0, 0.0, 0.42)
}

/// Spawn a sibling shadow at the default sea-level offset.
/// `initial_pos` + `initial_rot` are the source's spawn-frame
/// world transform — passing them seeds the shadow at the correct
/// place so the first frame doesn't render it at world origin
/// before `sync_shadows` updates it next tick.
pub fn spawn_for(
    commands: &mut Commands,
    material: Handle<ColorMaterial>,
    mesh: Handle<Mesh>,
    source: Entity,
    scale: f32,
    initial_pos: Vec2,
    initial_rot: Quat,
) -> Entity {
    spawn_for_with_offset(
        commands, material, mesh, source, scale,
        SHADOW_OFFSET, initial_pos, initial_rot,
    )
}

/// Spawn a sibling shadow with an explicit world-space offset.
/// Use [`SHADOW_OFFSET_AIR`] (or any custom Vec2) for airborne
/// objects that should read as being above the water.
pub fn spawn_for_with_offset(
    commands: &mut Commands,
    material: Handle<ColorMaterial>,
    mesh: Handle<Mesh>,
    source: Entity,
    scale: f32,
    offset: Vec2,
    initial_pos: Vec2,
    initial_rot: Quat,
) -> Entity {
    let translation = Vec3::new(
        initial_pos.x + offset.x,
        initial_pos.y + offset.y,
        SHADOW_Z,
    );
    commands.spawn((
        Mesh2d(mesh),
        MeshMaterial2d(material),
        Transform {
            translation,
            rotation: initial_rot,
            scale: Vec3::new(scale, scale, 1.0),
        },
        RenderLayers::layer(PLAY_LAYER),
        ShadowOf { source, offset },
    )).id()
}

/// Per-frame: follow source GlobalTransform + apply SE offset.
/// Despawn the shadow if the source is gone (entity despawned, or
/// query no longer matches because it lost a component we'd need).
pub fn sync_shadows(
    mut commands: Commands,
    sources: Query<&GlobalTransform, Without<ShadowOf>>,
    mut shadows: Query<(Entity, &ShadowOf, &mut Transform)>,
) {
    for (e, shadow, mut tf) in &mut shadows {
        let Ok(gt) = sources.get(shadow.source) else {
            // Source entity is gone — clean ourselves up.
            commands.entity(e).despawn();
            continue;
        };
        let (_scale, rot, translation) = gt.to_scale_rotation_translation();
        let want_xy = Vec3::new(
            translation.x + shadow.offset.x,
            translation.y + shadow.offset.y,
            SHADOW_Z,
        );
        if tf.translation != want_xy { tf.translation = want_xy; }
        if tf.rotation != rot { tf.rotation = rot; }
    }
}
