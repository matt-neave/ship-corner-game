//! Melee `AnchorFlail` weapon — throws an iron anchor on a chain
//! along the slot's mount direction, then retracts it. Damages enemies
//! along the chain path on BOTH the outward swing and the retraction,
//! so each cycle has two damage windows.
//!
//! Structure mirrors `blade.rs`:
//! - `sync_anchor_flail_decor` keeps deck visuals in sync with
//!   `TurretConfig`: each AnchorFlail slot gets an `AnchorHead` +
//!   `AnchorChain` pair as children. The head's shape escalates per
//!   tier (T1 hook → T2 + crossbar → T3 + decorative spike).
//! - `anchor_flail_tick` runs the state machine each frame: Idle (cd)
//!   → Out (extending) → In (retracting) → Idle. Damages enemies near
//!   the chain segment between slot world position and the anchor
//!   head's world position.
//!
//! Per-tier tuning (driven by `barrels`):
//! - T1: reach 14u, swing 1.0s per direction, idle 0.8s
//! - T2: reach 18u, swing 0.8s per direction, idle 0.6s
//! - T3: reach 22u, swing 0.6s per direction, idle 0.4s
//!
//! `WeaponType::AnchorFlail::fires_from_base()` returns false and
//! `has_barrels()` returns false so the standard cannon barrels +
//! aim/fire path skip these slots entirely.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;

use crate::balance::PLAY_LAYER;
use crate::bullet::{DamageSource, PendingDamageQueue};
use crate::components::Health;
use crate::enemy::Enemy;
use crate::palette::PaletteMaterials;
use crate::turret::{TurretConfig, TurretSlot};
use crate::weapon::WeaponType;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AnchorPhase {
    /// Cooldown between swings — anchor sits at the slot, no damage.
    Idle,
    /// Anchor extended at full reach, orbiting around the slot.
    Swing,
}

/// State attached to every equipped AnchorFlail slot. Drives the
/// per-frame orbit of the anchor head around the slot and the
/// per-swing damage grace list.
#[derive(Component)]
pub struct AnchorFlail {
    pub phase: AnchorPhase,
    pub phase_timer: f32,
    /// Orbit radius for this slot's tier — set at decor-sync time
    /// from `slot.barrels`.
    pub reach: f32,
    /// Seconds for a single swing phase (one cycle of orbital motion).
    pub swing_duration: f32,
    /// Idle-phase length between swings.
    pub idle_duration: f32,
    /// Number of full revolutions the head completes per swing — set
    /// at decor-sync time from tier.
    pub revolutions: f32,
    /// Current orbit angle in radians (0 = slot-local +Y, growing
    /// clockwise). Wraps within a swing; reset to 0 at Swing entry.
    pub angle: f32,
    /// Enemies already damaged on the CURRENT swing. Cleared on
    /// every Idle → Swing transition.
    pub hit_this_swing: Vec<Entity>,
}

/// Marker on the anchor's head entity (the visible iron hook +
/// crossbar). Child of the slot — inherits the slot's mount-angle
/// rotation, so its local +Y is the swing's outward direction.
#[derive(Component)]
pub struct AnchorHead {
    pub slot: Entity,
}

/// Marker on the chain rectangle stretched between slot and anchor
/// head. The mesh is centred at the slot origin; the tick system
/// rescales it each frame so its top edge tracks the head's local Y.
#[derive(Component)]
pub struct AnchorChain {
    pub slot: Entity,
}

/// Per-tier reach (world units). Index = barrels - 1.
const REACH_BY_TIER: [f32; 3] = [14.0, 18.0, 22.0];
/// Per-tier swing duration (seconds). Total length of one orbital
/// swing window before returning to idle.
const SWING_BY_TIER: [f32; 3] = [1.4, 1.2, 1.0];
/// Per-tier idle pause between swings (seconds).
const IDLE_BY_TIER: [f32; 3] = [0.6, 0.5, 0.4];
/// Per-tier full revolutions completed per swing. Higher tier = more
/// rotations within roughly the same window, so the flail visibly
/// whips faster as the player upgrades it.
const REVS_BY_TIER: [f32; 3] = [1.0, 1.5, 2.0];
/// Anchor hit radius around the head's world position.
const HIT_RADIUS: f32 = 4.0;
/// Visible chain thickness.
const CHAIN_WIDTH: f32 = 0.5;

pub struct AnchorFlailPlugin;

impl Plugin for AnchorFlailPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (sync_anchor_flail_decor, anchor_flail_tick));
    }
}

/// Maintain the anchor decor invariant on every cfg change:
///   - Equipped AnchorFlail slots have exactly one `AnchorHead` +
///     one `AnchorChain` child, sized per tier.
///   - Non-AnchorFlail slots (or unequipped) have neither.
///
/// Rebuilds from scratch on every cfg change — cheaper than diffing
/// tier-state (T1 → T2 changes the head's child mesh tree anyway),
/// and config changes happen only on shop interactions.
pub fn sync_anchor_flail_decor(
    mut commands: Commands,
    cfg: Res<TurretConfig>,
    pm: Option<Res<PaletteMaterials>>,
    mut meshes: ResMut<Assets<Mesh>>,
    slots: Query<(Entity, &TurretSlot, Option<&Children>)>,
    heads: Query<Entity, With<AnchorHead>>,
    chains: Query<Entity, With<AnchorChain>>,
) {
    if !cfg.is_changed() { return; }
    let Some(pm) = pm else { return; };

    for (slot_entity, slot, children) in &slots {
        let s = cfg.slots[slot.index];
        let want = s.equipped && matches!(s.weapon, WeaponType::AnchorFlail);

        // Tear down existing decor + AnchorFlail state component
        // — they'll be re-attached fresh below if `want`.
        let existing_head = children
            .into_iter()
            .flat_map(|c| c.iter())
            .find(|c| heads.get(*c).is_ok());
        let existing_chain = children
            .into_iter()
            .flat_map(|c| c.iter())
            .find(|c| chains.get(*c).is_ok());
        if let Some(h) = existing_head { commands.entity(h).despawn(); }
        if let Some(ch) = existing_chain { commands.entity(ch).despawn(); }
        commands.entity(slot_entity).remove::<AnchorFlail>();

        if !want { continue; }

        let tier = s.barrels.clamp(1, 3) as usize;
        let tier_idx = tier - 1;
        let reach = REACH_BY_TIER[tier_idx];
        let swing = SWING_BY_TIER[tier_idx];
        let idle = IDLE_BY_TIER[tier_idx];
        let revs = REVS_BY_TIER[tier_idx];

        commands.entity(slot_entity).insert(AnchorFlail {
            phase: AnchorPhase::Idle,
            phase_timer: idle,
            reach,
            swing_duration: swing,
            idle_duration: idle,
            revolutions: revs,
            angle: 0.0,
            hit_this_swing: Vec::new(),
        });

        // Chain — dark thin rectangle. Centred at the slot (local
        // y=0) with a unit length; tick scales it to match the
        // anchor head's current extension each frame. Z below the
        // anchor head so the head renders on top.
        let chain_mesh = meshes.add(Rectangle::new(CHAIN_WIDTH, 1.0));
        let chain = commands.spawn((
            Mesh2d(chain_mesh),
            MeshMaterial2d(pm.harpoon_chain.clone()),
            Transform::from_xyz(0.0, 0.0, 0.05),
            AnchorChain { slot: slot_entity },
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(chain).insert(ChildOf(slot_entity));

        // Anchor head — composed of multiple sub-meshes per tier.
        // Spawn a parent at local +0 (will be repositioned by the
        // tick system); attach the per-tier sub-meshes as children
        // of the head so they all move together.
        let head = commands.spawn((
            // Pivot transform — has no mesh of its own; the children
            // below carry the visible silhouette. Visibility::Inherited
            // is required for child-transform propagation.
            Transform::from_xyz(0.0, 0.0, 0.10),
            Visibility::Inherited,
            AnchorHead { slot: slot_entity },
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(head).insert(ChildOf(slot_entity));

        // Tier-specific sub-meshes. Local space: +Y is "away from
        // the ship", since the head is a child of the slot which
        // is already rotated to mount_angle. The head pivot sits
        // at the chain tip — sub-meshes extend BACKWARD (-Y) from
        // there so the visible head fans out behind the impact
        // point, the way a real anchor's flukes hang below the ring.
        spawn_anchor_visual(&mut commands, &mut meshes, &pm, head, tier);
    }
}

/// Spawn a standalone anchor-head visual at world position `pos` —
/// used by the MP mirror path. Owner side uses `spawn_anchor_visual`
/// directly with a parent slot entity; mirrors don't have the slot
/// hierarchy so this wrapper creates a pivot at world coords and
/// composes the tier-1 silhouette on top.
///
/// Tier defaults to 1 (basic hook) because the FriendlyUnitsSnapshot
/// carries pos+rot only — barrels/tier isn't synced. Visually a tier
/// mismatch reads as "they have a fancier anchor than I'm rendering"
/// which is acceptable.
pub fn spawn_flail_head_visual(
    commands: &mut Commands,
    pm: &PaletteMaterials,
    meshes: &mut Assets<Mesh>,
    pos: Vec2,
) -> Entity {
    let head = commands.spawn((
        Transform::from_xyz(pos.x, pos.y, 1.6),
        Visibility::Inherited,
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    spawn_anchor_visual(commands, meshes, pm, head, 1);
    head
}

/// Build the per-tier anchor silhouette. Composition of small
/// rectangles + triangles around the head pivot.
///
/// - T1: simple triangular hook (flukes only).
/// - T2: hook + horizontal crossbar (the stock).
/// - T3: hook + stock + a small back-spike beneath the stock.
pub fn spawn_anchor_visual(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    pm: &PaletteMaterials,
    head: Entity,
    tier: usize,
) {
    // Crown — 5 thin line-segment rectangles forming an anchor head
    // that extends outward from the ring. +Y points away from the
    // ship, so the crown sits at the FAR end of the head with the
    // hook tips curving back toward the chain. Classic Admiralty
    // silhouette: shank → crown shoulder → arms sweep back-and-out
    // → fluke tips facing the ring.
    let crown_points = [
        Vec2::new(-3.0, 0.5),
        Vec2::new(-2.0, 1.8),
        Vec2::new(-0.5, 2.5),
        Vec2::new( 0.5, 2.5),
        Vec2::new( 2.0, 1.8),
        Vec2::new( 3.0, 0.5),
    ];
    for w in crown_points.windows(2) {
        let a = w[0];
        let b = w[1];
        let delta = b - a;
        let length = delta.length();
        let angle = delta.y.atan2(delta.x);
        let mid = (a + b) * 0.5;
        let seg = meshes.add(Rectangle::new(length, 0.9));
        let seg_e = commands.spawn((
            Mesh2d(seg),
            MeshMaterial2d(pm.anchor_iron.clone()),
            Transform::from_xyz(mid.x, mid.y, 0.0)
                .with_rotation(Quat::from_rotation_z(angle)),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(seg_e).insert(ChildOf(head));
    }

    if tier >= 2 {
        // Stock — horizontal crossbar above the flukes, sitting at
        // the chain attachment point. Reads as the iconic anchor
        // silhouette ("T" topped with a bar).
        let stock = meshes.add(Rectangle::new(4.6, 0.8));
        let stock_e = commands.spawn((
            Mesh2d(stock),
            MeshMaterial2d(pm.anchor_iron.clone()),
            Transform::from_xyz(0.0, 0.35, 0.01),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(stock_e).insert(ChildOf(head));
    }

    if tier >= 3 {
        // Back-spike — small needle protruding past the crown,
        // T3-exclusive "extra menace" upgrade. Reads as a
        // tournament-grade anchor vs the journeyman T1/T2.
        let spike = meshes.add(Triangle2d::new(
            Vec2::new(-0.6, 2.5),
            Vec2::new( 0.6, 2.5),
            Vec2::new( 0.0, 4.0),
        ));
        let spike_e = commands.spawn((
            Mesh2d(spike),
            MeshMaterial2d(pm.anchor_iron.clone()),
            Transform::from_xyz(0.0, 0.0, 0.02),
            RenderLayers::layer(PLAY_LAYER),
        )).id();
        commands.entity(spike_e).insert(ChildOf(head));
    }
}

/// Per-frame state machine + damage. Orbits the anchor head around
/// the slot at full reach during the Swing phase, dragging the chain
/// behind it. Idle parks the head at the slot. Damage applies to any
/// enemy within HIT_RADIUS of the head's world position, with a
/// per-swing grace list so each enemy is only chunked once per cycle.
pub fn anchor_flail_tick(
    time: Res<Time>,
    cfg: Res<TurretConfig>,
    mut queue: ResMut<PendingDamageQueue>,
    mut slots: Query<(Entity, &TurretSlot, &mut AnchorFlail)>,
    mut heads: Query<(&mut Transform, &AnchorHead), (Without<AnchorChain>, Without<Enemy>)>,
    mut chains: Query<
        (&mut Transform, &AnchorChain),
        (Without<AnchorHead>, Without<Enemy>),
    >,
    slot_world: Query<&GlobalTransform, With<TurretSlot>>,
    mut enemies: Query<(Entity, &Transform, &Health), (With<Enemy>, Without<AnchorHead>, Without<TurretSlot>)>,
) {
    let dt = time.delta_secs();
    let r2 = HIT_RADIUS * HIT_RADIUS;

    for (slot_entity, slot, mut flail) in &mut slots {
        let s = cfg.slots[slot.index];
        if !s.equipped || !matches!(s.weapon, WeaponType::AnchorFlail) {
            continue;
        }

        // ---- Phase tick ----
        flail.phase_timer -= dt;
        if flail.phase_timer <= 0.0 {
            flail.phase = match flail.phase {
                AnchorPhase::Idle => AnchorPhase::Swing,
                AnchorPhase::Swing => AnchorPhase::Idle,
            };
            flail.phase_timer = match flail.phase {
                AnchorPhase::Idle => flail.idle_duration,
                AnchorPhase::Swing => flail.swing_duration,
            };
            if matches!(flail.phase, AnchorPhase::Swing) {
                flail.hit_this_swing.clear();
                flail.angle = 0.0;
            }
        }

        // ---- Orbit angle + head position ----
        // During Swing the head orbits the slot at full reach. The
        // angle is measured from slot-local +Y (the mount direction),
        // sweeping clockwise so the local position is
        // (sin a, cos a) * reach.
        let local_pos = match flail.phase {
            AnchorPhase::Idle => Vec2::ZERO,
            AnchorPhase::Swing => {
                let omega = flail.revolutions
                    * std::f32::consts::TAU
                    / flail.swing_duration.max(0.0001);
                flail.angle += omega * dt;
                Vec2::new(flail.angle.sin(), flail.angle.cos()) * flail.reach
            }
        };

        for (mut tf, head) in &mut heads {
            if head.slot != slot_entity { continue; }
            tf.translation.x = local_pos.x;
            tf.translation.y = local_pos.y;
            // Rotate the head so its local +Y axis stays aligned with
            // the radial (outward) direction — the crown always
            // points away from the slot as it whips around.
            tf.rotation = Quat::from_rotation_z(-flail.angle);
        }
        for (mut tf, chain) in &mut chains {
            if chain.slot != slot_entity { continue; }
            let len = local_pos.length().max(0.01);
            let mid = local_pos * 0.5;
            tf.translation.x = mid.x;
            tf.translation.y = mid.y;
            tf.rotation = Quat::from_rotation_z(-flail.angle);
            tf.scale.x = 1.0;
            tf.scale.y = len;
            tf.scale.z = 1.0;
        }

        // ---- Damage during Swing ----
        if matches!(flail.phase, AnchorPhase::Idle) { continue; }
        let Ok(slot_g) = slot_world.get(slot_entity) else { continue; };
        let m = slot_g.compute_transform();
        let head_world_pos = (m.translation
            + m.rotation.mul_vec3(Vec3::new(local_pos.x, local_pos.y, 0.0)))
            .truncate();

        let damage = slot.damage.max(1);
        let source = Some(DamageSource::PlayerSlot(slot.index as u8));
        let mut new_hits: Vec<Entity> = Vec::new();
        for (e, etf, h) in &mut enemies {
            if h.0 <= 0 { continue; }
            if flail.hit_this_swing.contains(&e) { continue; }
            let ep = etf.translation.truncate();
            if ep.distance_squared(head_world_pos) >= r2 { continue; }
            queue.push_initial(
                e, damage, ep,
                WeaponType::AnchorFlail,
                source,
                &slot.runes,
            );
            new_hits.push(e);
        }
        for e in new_hits {
            flail.hit_this_swing.push(e);
        }
    }
}
