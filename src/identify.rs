//! "Easy identify" mode — paints a soft halo under every ship so
//! enemies (red) read as distinct from friendlies / allies (green)
//! at a glance.
//!
//! Toggled by [`IdentifyMode`] (settings panel). When on, a halo
//! sibling is spawned for every `Enemy` and every `Friendly`/`Ally`;
//! the sibling carries [`IdentifyHalo`] and is positioned via
//! the existing `shadow::sync_shadows` system (which also follows
//! shadow entities). Halos are scaled 1.4× the source's body so the
//! coloured ring shows around the hull's silhouette without
//! covering the centre.
//!
//! When toggled off, every `IdentifyHalo` entity is despawned.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;

use crate::balance::PLAY_LAYER;
use crate::ally::Ally;
use crate::components::Friendly;
use crate::enemy::Enemy;

pub struct IdentifyPlugin;

impl Plugin for IdentifyPlugin {
    fn build(&self, app: &mut App) {
        app
            .insert_resource(crate::modes::IdentifyMode::default())
            .add_systems(Startup, setup_identify_assets)
            .add_systems(Update, sync_identify_halos);
    }
}

/// Shared halo mesh + the two tinted translucent materials. One
/// allocation at startup, reused for every spawned halo.
#[derive(Resource)]
pub struct IdentifyHaloAssets {
    pub mesh: Handle<Mesh>,
    pub red: Handle<ColorMaterial>,
    pub green: Handle<ColorMaterial>,
}

/// Halo sibling — one per identified entity. `source` tracks the
/// hull so the sync system can despawn the halo when the source
/// disappears AND follow it each frame via `shadow::sync_shadows`.
#[derive(Component)]
pub struct IdentifyHalo {
    pub source: Entity,
}

fn setup_identify_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
) {
    // Generic capsule sized to roughly cover any ship-class hull —
    // scaled per-source at spawn time to match the source's
    // Transform.scale. The base capsule matches the standard
    // friendly hull dimensions; the per-spawn scale factor + 1.4×
    // halo padding produces a visible ring on each ship without
    // tight chrome around the silhouette.
    let mesh = meshes.add(Capsule2d::new(
        crate::balance::HULL_WIDTH * 0.5,
        crate::balance::HULL_LEN,
    ));
    // Translucent saturated tints — bright enough to read against
    // both day + night ocean, soft enough that the body's colour
    // still dominates the silhouette read.
    let red = materials.add(Color::srgba(1.0, 0.18, 0.18, 0.45));
    let green = materials.add(Color::srgba(0.30, 0.95, 0.40, 0.45));
    commands.insert_resource(IdentifyHaloAssets { mesh, red, green });
}

/// Halo padding multiplier relative to the source's body scale.
/// 1.4× = the halo extends ~40% past the hull silhouette so the
/// coloured ring is visible all the way around.
const HALO_SCALE_MULT: f32 = 1.4;
/// Z offset on the halo relative to the source so the halo renders
/// just BELOW the hull. The body covers the centre and only the
/// outer ring shows.
const HALO_Z: f32 = 0.95;

/// Per-frame: spawn / despawn halos to match `IdentifyMode` +
/// the current entity set. Halos carry `crate::shadow::ShadowOf`
/// so the existing `shadow::sync_shadows` system keeps them
/// glued to the source's position + rotation every frame — no
/// duplicate follow logic needed.
#[allow(clippy::type_complexity)]
pub fn sync_identify_halos(
    mut commands: Commands,
    mode: Res<crate::modes::IdentifyMode>,
    assets: Option<Res<IdentifyHaloAssets>>,
    // Sources, partitioned by side:
    enemies: Query<(Entity, &Transform), With<Enemy>>,
    friendlies: Query<
        (Entity, &Transform),
        (Or<(With<Friendly>, With<Ally>)>, Without<Enemy>),
    >,
    // Existing halos so we can diff against the live source set.
    halos: Query<(Entity, &IdentifyHalo)>,
) {
    let Some(assets) = assets else { return };

    // Mode off: wipe every halo and bail.
    if !mode.active {
        for (e, _) in &halos {
            commands.entity(e).try_despawn();
        }
        return;
    }

    // Mode on: index existing halos by source so we can skip
    // entities that already have one + despawn halos whose source
    // is gone.
    use std::collections::HashSet;
    let mut covered: HashSet<Entity> = HashSet::with_capacity(64);
    for (halo_e, halo) in &halos {
        // Source-gone check: if the source isn't in either query
        // anymore, drop the halo.
        let alive = enemies.get(halo.source).is_ok()
            || friendlies.get(halo.source).is_ok();
        if !alive {
            commands.entity(halo_e).try_despawn();
            continue;
        }
        covered.insert(halo.source);
    }

    // Spawn missing halos. Per-side colour pick.
    for (source, tf) in &enemies {
        if covered.contains(&source) { continue; }
        spawn_halo(&mut commands, &assets, source, tf, true);
    }
    for (source, tf) in &friendlies {
        if covered.contains(&source) { continue; }
        spawn_halo(&mut commands, &assets, source, tf, false);
    }
}

fn spawn_halo(
    commands: &mut Commands,
    assets: &IdentifyHaloAssets,
    source: Entity,
    source_tf: &Transform,
    is_enemy: bool,
) {
    let material = if is_enemy { assets.red.clone() } else { assets.green.clone() };
    // Match the source's world position + rotation so the halo
    // appears under the hull on the spawn frame. `shadow::sync_shadows`
    // takes over from there each subsequent frame.
    let scale = source_tf.scale.x * HALO_SCALE_MULT;
    let pos = source_tf.translation.truncate();
    let halo = commands.spawn((
        Mesh2d(assets.mesh.clone()),
        MeshMaterial2d(material),
        Transform {
            translation: Vec3::new(pos.x, pos.y, HALO_Z),
            rotation: source_tf.rotation,
            scale: Vec3::new(scale, scale, 1.0),
        },
        RenderLayers::layer(PLAY_LAYER),
        IdentifyHalo { source },
        crate::shadow::ShadowOf {
            source,
            offset: Vec2::ZERO,
        },
    )).id();
    let _ = halo;
}
