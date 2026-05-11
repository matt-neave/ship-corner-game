//! Melee `Blade` turret — extends a rotating arm from the slot with
//! a spinning blade at the tip. Damages enemies inside the blade's
//! reach on a tick rather than firing a projectile.
//!
//! Two pieces:
//!   - `sync_blade_decor` keeps the deck visual in sync with `TurretConfig`:
//!     each Blade slot gets exactly one `BladeArm` + one `BladeEdge`
//!     child entity; non-Blade slots have neither (so swapping the slot's
//!     weapon cleans up the decoration). Mirrors the booster pattern.
//!   - `blade_tick` rotates the spinning edge each frame and, every
//!     `1.0 / fire_rate` seconds, deals `slot.damage` to every enemy
//!     within `BLADE_REACH` of the blade's WORLD position. The blade
//!     entity carries a `GlobalTransform`, so we get its world-space
//!     position straight from there — no manual ship/turret rotation
//!     compose needed.
//!
//! `WeaponType::Blade::has_barrels()` already returns false so the
//! standard cannon barrels stay hidden for these slots, and
//! `fires_from_base()` returns false so `turret_aim_fire` skips them.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;

use crate::balance::PLAY_LAYER;
use crate::bullet::{DamageSource, PendingDamageQueue};
use crate::components::Health;
use crate::enemy::Enemy;
use crate::palette::PaletteMaterials;
use crate::turret::{TurretConfig, TurretSlot};
use crate::weapon::WeaponType;

/// Length (world units) of the rectangular arm extending outward from
/// the turret base in the slot's mount direction. The arm's local +Y
/// runs along the mount axis (the turret base entity is already rotated
/// to `mount_angle`), so the arm is centered at +ARM_LENGTH/2 and the
/// blade pivot lands at +ARM_LENGTH — this is the "distance from ship".
const BLADE_ARM_LENGTH: f32 = 8.0;

/// Width (thickness) of the arm rectangle.
const BLADE_ARM_WIDTH: f32 = 0.8;

/// Cross-arm length for the spinning blade rectangle. Reads as a
/// flickering bar rather than a sharp edge — fast rotation does the
/// rest.
const BLADE_EDGE_LENGTH: f32 = 5.0;
const BLADE_EDGE_WIDTH: f32 = 1.0;

/// Angular velocity (rad/s) of the spinning blade. Pure visual — has
/// no bearing on damage cadence (which is driven by `slot.fire_rate`).
const BLADE_SPIN_RATE: f32 = 12.0;

/// Damage radius around the blade's WORLD position. Tuned to ~the
/// blade's visual half-length plus a little slack so enemies that
/// brush the spinning edge get hit even though the rotating sprite
/// is mostly empty space.
const BLADE_REACH: f32 = 4.5;

/// Marker for the rectangular arm child of a Blade-equipped turret slot.
/// `sync_blade_decor` ensures exactly one of these exists per equipped
/// Blade slot.
#[derive(Component)]
pub struct BladeArm;

/// Marker for the spinning blade rectangle at the end of the arm.
/// Carries its own per-slot damage cooldown so multiple Blade slots
/// tick independently.
#[derive(Component)]
pub struct BladeEdge {
    /// Owning turret slot entity — used by `blade_tick` to look up the
    /// slot's current damage / fire-rate / index for damage crediting.
    pub slot: Entity,
    /// Seconds remaining until the next damage tick. Counted down each
    /// frame; on reach 0 we apply damage and reset to `1.0 / fire_rate`.
    pub cooldown: f32,
}

pub struct BladePlugin;

impl Plugin for BladePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (sync_blade_decor, blade_tick));
    }
}

/// Maintain blade decor on every config change. Tears down all existing
/// arm + edge children for every slot, then if the slot is an equipped
/// Blade, respawns:
///   - one `BladeArm` extending outward in the mount direction
///   - **`barrels` × `BladeEdge`** rectangles at the arm tip, evenly
///     spaced around 180° (so T1 = single bar, T2 = `+` cross, T3 =
///     three-pointed star). Each blade ticks its damage cooldown
///     independently, so total dps = `damage × fire_rate × barrels`.
///
/// Cheap to despawn-and-respawn — `TurretConfig` only changes on shop
/// interactions, not per frame.
pub fn sync_blade_decor(
    mut commands: Commands,
    cfg: Res<TurretConfig>,
    pm: Option<Res<PaletteMaterials>>,
    mut meshes: ResMut<Assets<Mesh>>,
    slots: Query<(Entity, &TurretSlot, Option<&Children>)>,
    arms: Query<Entity, With<BladeArm>>,
    edges: Query<Entity, With<BladeEdge>>,
) {
    if !cfg.is_changed() { return; }
    let Some(pm) = pm else { return; };

    // Lazily build the meshes — only when we actually need to spawn
    // decor. Reused across every Blade slot in the same frame.
    let mut arm_mesh: Option<Handle<Mesh>> = None;
    let mut edge_mesh: Option<Handle<Mesh>> = None;

    for (slot_entity, slot, children) in &slots {
        let s = cfg.slots[slot.index];
        let want_decor = s.equipped && matches!(s.weapon, WeaponType::Blade);

        // Always tear down existing decor first — easier than diffing
        // (T1 → T2 changes blade count, requires respawn anyway).
        let existing_arm = children
            .into_iter()
            .flat_map(|c| c.iter())
            .find(|c| arms.get(*c).is_ok());
        let existing_edges: Vec<Entity> = children
            .into_iter()
            .flat_map(|c| c.iter())
            .filter(|c| edges.get(*c).is_ok())
            .collect();

        if let Some(arm) = existing_arm {
            commands.entity(arm).despawn();
        }
        for edge in &existing_edges {
            commands.entity(*edge).despawn();
        }

        if !want_decor { continue; }

        let blade_count = s.barrels.clamp(1, 3) as usize;

        // Arm — rotated by the slot's `mount_angle` already (the slot
        // entity carries the rotation). Local +Y is OUTWARD from the
        // ship; centre the rect at +ARM_LENGTH/2 so its base sits on
        // the deck pad and its tip lands at +ARM_LENGTH.
        let arm_h = arm_mesh
            .get_or_insert_with(|| meshes.add(Rectangle::new(BLADE_ARM_WIDTH, BLADE_ARM_LENGTH)))
            .clone();
        let arm = commands.spawn((
            Mesh2d(arm_h),
            MeshMaterial2d(pm.turret_blade.clone()),
            Transform::from_xyz(0.0, BLADE_ARM_LENGTH * 0.5, 0.05),
            BladeArm,
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(arm).insert(ChildOf(slot_entity));

        // N blades at the arm tip. All blades share the same world
        // position (so they damage the same enemies) but start at
        // staggered local rotations — `i × π/N` evenly tiles 180°
        // (the rectangle is symmetric under 180° rotation, so 0 + π
        // == one blade visually). T1 = single bar; T2 = `+`; T3 =
        // three-pointed star. Cooldowns are also staggered so the
        // damage ticks spread out across the period instead of all
        // firing on the same frame.
        let edge_h = edge_mesh
            .get_or_insert_with(|| meshes.add(Rectangle::new(BLADE_EDGE_LENGTH, BLADE_EDGE_WIDTH)))
            .clone();
        let initial_period = if s.fire_rate > 0.0 { 1.0 / s.fire_rate } else { 0.0 };
        for i in 0..blade_count {
            let offset = (i as f32) * std::f32::consts::PI / (blade_count as f32);
            let cd = (i as f32 / blade_count as f32) * initial_period;
            let edge = commands.spawn((
                Mesh2d(edge_h.clone()),
                MeshMaterial2d(pm.blade_edge.clone()),
                Transform::from_xyz(0.0, BLADE_ARM_LENGTH, 0.10)
                    .with_rotation(Quat::from_rotation_z(offset)),
                BladeEdge { slot: slot_entity, cooldown: cd },
                RenderLayers::layer(PLAY_LAYER),
            )).id();
            commands.entity(edge).insert(ChildOf(slot_entity));
        }
    }
}

/// Per-frame: spin every BladeEdge around its local Z, count down its
/// damage cooldown, and on tick apply `slot.damage` to every enemy
/// within `BLADE_REACH` of the blade's WORLD position.
///
/// World position comes from the blade's `GlobalTransform`, which Bevy
/// already composes from the ship → turret-base → blade parent chain.
/// No manual ship-rotation maths needed.
///
/// Heal-on-kill: when a blade tick brings an enemy to 0 HP, heals the
/// player by `Synergies::melee_heal_per_kill()` (1/2/3/4 at the four
/// Melee tiers). Multiple kills in the same tick stack.
pub fn blade_tick(
    time: Res<Time>,
    synergies: Res<crate::synergy::Synergies>,
    player_stats: Res<crate::stats::PlayerStats>,
    mut queue: ResMut<PendingDamageQueue>,
    mut blades: Query<(&mut Transform, &GlobalTransform, &mut BladeEdge, &Visibility)>,
    slot_q: Query<&TurretSlot>,
    enemies: Query<(Entity, &Transform, &Enemy, &Health), (With<Enemy>, Without<BladeEdge>, Without<crate::components::Friendly>)>,
    mut friendly: Query<&mut crate::components::Health, (With<crate::components::Friendly>, Without<Enemy>, Without<BladeEdge>)>,
) {
    let dt = time.delta_secs();
    let heal_per_kill = synergies.melee_heal_per_kill();
    let max_hp = player_stats.max_hp();
    for (mut tf, gtf, mut edge, vis) in &mut blades {
        // Inherited visibility — when the parent slot is hidden
        // (unequipped), we still update transform/cooldown? Skip the
        // damage tick if the slot isn't actually visible. We check
        // local Visibility here; the parent toggles to Hidden in
        // `sync_turret_config` so an unequipped slot's children render
        // as hidden via inheritance regardless, but we should still
        // gate damage so an invisible blade doesn't carve up enemies.
        if matches!(vis, Visibility::Hidden) {
            // Still spin so it looks alive when re-equipped, but cheap.
            tf.rotate_z(BLADE_SPIN_RATE * dt);
            continue;
        }

        // Visual spin.
        tf.rotate_z(BLADE_SPIN_RATE * dt);

        // Damage cadence. `slot.fire_rate` is repurposed as
        // "damage ticks per second" for Blade weapons (see weapon.rs
        // defaults comment).
        edge.cooldown -= dt;
        if edge.cooldown > 0.0 { continue; }

        let Ok(slot) = slot_q.get(edge.slot) else { continue; };
        if !matches!(slot.weapon, WeaponType::Blade) { continue; }
        let rate = slot.fire_rate.max(0.1);
        edge.cooldown = 1.0 / rate;

        let damage = slot.damage;
        if damage <= 0 { continue; }

        // Blade's world position — straight from GlobalTransform, which
        // already accounts for ship translation + rotation and the
        // turret base's local mount-angle rotation.
        let blade_world = gtf.translation().truncate();
        let source = Some(DamageSource::PlayerSlot(slot.index as u8));

        // Push DamageEvents into the shared queue so the runes on
        // this slot get a turn through `process_damage_events` (Fire
        // / Frost / Shock / Detonate / Echo / Cascade / Conduit /
        // Resonate). Pre-tick HP < threshold check is informational
        // only — the actual kill detection (and Melee heal) happens
        // post-drain via the lethal branch in process_damage_event,
        // so we approximate here by counting enemies whose CURRENT
        // HP is ≤ damage. This is a 1-frame-stale heuristic but
        // good enough for the heal counter; perfect kill detection
        // would require a separate "DamageDealt" event from the
        // drain back to systems.
        // Multi-target: blade hits EVERY enemy in reach on a tick
        // (preserved from the original direct-damage version).
        // Heal counter is a 1-frame-stale heuristic — we count
        // enemies whose pre-event HP is ≤ damage, which doesn't
        // perfectly match what `process_damage_event` will actually
        // kill (Resonate amp / Detonate burst can overkill, Echo
        // delays a hit), but is close enough for the on-kill heal.
        let mut kills_this_tick: i32 = 0;
        for (e, etf, en, h) in &enemies {
            if h.0 <= 0 { continue; }
            let ep = etf.translation.truncate();
            let er = 3.5 * en.variant.scale();
            let reach = BLADE_REACH + er;
            if ep.distance_squared(blade_world) > reach * reach { continue; }
            queue.push_initial(e, damage, ep, WeaponType::Blade, source, &slot.runes);
            if h.0 <= damage { kills_this_tick += 1; }
        }
        if heal_per_kill > 0 && kills_this_tick > 0 {
            if let Ok(mut hp) = friendly.single_mut() {
                hp.0 = (hp.0 + heal_per_kill * kills_this_tick).min(max_hp);
            }
        }
    }
}
