//! Drag-and-drop for the customize overlay.
//!
//! Custom mouse picking — the customize UI lives on a low-res render
//! target (see `render.rs`), so the standard `bevy_ui` cursor →
//! interaction pipeline doesn't apply. Instead we:
//!
//! 1. Convert the window cursor → spec coords each frame via
//!    `CustomizeViewport::window_to_spec`.
//! 2. Test the spec cursor against every entity carrying a `HitArea` +
//!    `DragSourceMarker` / `DropTargetMarker`.
//!
//! State machine
//! -------------
//! - **Press**: hit-test → if the cursor is over a draggable, snapshot
//!   what's being dragged and spawn a `DragGhost` sprite parented under
//!   the customize render (so it's pixelated like everything else).
//! - **Hold**: every frame, move the ghost to the cursor's spec coord.
//! - **Release**: hit-test against drop targets, resolve via merge rules.
//!
//! Merge rules: identical to the previous bevy_ui-based version.

use bevy::input::mouse::MouseButton;
use bevy::prelude::*;
use bevy::render::view::RenderLayers;
use bevy::sprite::MeshMaterial2d;
use bevy::window::PrimaryWindow;
use rand::seq::SliceRandom;

use crate::balance::CUSTOMIZE_LAYER;
use crate::rune::Rune;
use crate::stats::StatKind;
use crate::turret::{SlotCfg, TurretConfig};
use crate::weapon::WeaponType;

use super::render::CustomizeViewport;
use super::setup::{
    rune_color_for, turret_color_for, DragSourceMarker, DropTargetMarker, HitArea,
};
use super::CustomizeOpen;

// ---------- Drag state + payload ----------

#[derive(Resource, Default)]
pub struct DragState {
    pub picked: Option<Picked>,
    pub ghost: Option<Entity>,
    /// Last known spec cursor — used by the ghost-follow system + hit-test.
    pub spec_cursor: Option<Vec2>,
}

#[derive(Clone)]
pub struct Picked {
    pub source: DragSourceKind,
    pub payload: Payload,
}

#[derive(Clone, Copy)]
pub enum Payload {
    Turret { weapon: WeaponType, barrels: u8 },
    Rune(Rune),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragSourceKind {
    ShipSlot(usize),
    ShipRune { slot: usize, rune_idx: usize },
    ShopTurret(usize),
    ShopRune(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropTargetKind {
    ShipSlot(usize),
    ShipRune { slot: usize, rune_idx: usize },
}

#[derive(Component)]
pub struct DragGhost;

// ---------- Shop offerings ----------

/// Shop stock. Each slot is `Some(offer)` until the player drags it
/// out — at that point we set it to `None` so it can't be picked again.
/// Keeps slot indices stable across drags so the spawned UI tiles
/// (which carry hard-coded indices 0/1/2) keep pointing at the right
/// stock entry; visual updates render `None` slots as empty.
#[derive(Resource, Default, Clone)]
pub struct CustomizeShop {
    pub turrets: Vec<Option<ShopTurretOffer>>,
    pub runes: Vec<Option<Rune>>,
    /// Stat-modifier cards. Click-to-apply: the delta is added to the
    /// stat's `flat` field and the slot is consumed.
    pub mods: Vec<Option<ShopMod>>,
}

#[derive(Clone, Copy)]
pub struct ShopTurretOffer {
    pub weapon: WeaponType,
    pub barrels: u8,
}

#[derive(Clone, Copy)]
pub struct ShopMod {
    pub kind: StatKind,
    pub delta: f32,
}

impl ShopMod {
    /// Card text, e.g. "+25 CRIT" or "+0.5 RUNE". Sign is always
    /// included so stacking direction reads at a glance.
    pub fn label(self) -> String {
        if self.delta.fract().abs() < 0.01 {
            format!("{:+.0} {}", self.delta, self.kind.label())
        } else {
            format!("{:+.1} {}", self.delta, self.kind.label())
        }
    }
}

/// Scrap cost to re-roll the shop. Refills every slot — sold or not.
pub const SHOP_REROLL_COST: u32 = 5;
/// Scrap cost for any single shop purchase (turret / rune / mod).
/// Same flat number for now; per-item tier pricing is a future
/// iteration.
pub const SHOP_ITEM_COST: u32 = 5;

/// Roll a fresh set of offerings. Used by both the startup init and the
/// runtime reroll button. Always returns a fully-stocked shop (every
/// slot Some(...)), so a reroll restocks anything the player bought.
pub fn roll_fresh_stock() -> CustomizeShop {
    let mut rng = rand::thread_rng();
    let weapons = [
        WeaponType::Standard,
        WeaponType::Sniper,
        WeaponType::MachineGun,
        WeaponType::Shotgun,
        WeaponType::Railgun,
        WeaponType::Mortar,
        WeaponType::HeliPad,
    ];
    let runes_pool = [
        Rune::Fire,
        Rune::Frost,
        Rune::Shock,
        Rune::Detonate,
        Rune::Echo,
        Rune::Cascade,
        Rune::Conduit,
        Rune::Resonate,
        Rune::TargetFurthest,
        Rune::TargetHighestHp,
        Rune::TargetLowestHp,
        Rune::Splash,
    ];
    let mut turrets = Vec::with_capacity(3);
    for _ in 0..3 {
        let w = *weapons.choose(&mut rng).unwrap();
        turrets.push(Some(ShopTurretOffer { weapon: w, barrels: 1 }));
    }
    let mut runes_owned: Vec<_> = runes_pool.to_vec();
    runes_owned.shuffle(&mut rng);
    let runes = runes_owned.into_iter().take(2).map(Some).collect();
    let mut mods = Vec::with_capacity(3);
    for _ in 0..3 {
        let kind = *StatKind::ALL.choose(&mut rng).unwrap();
        // Reuse the debug step as the shop delta — already tuned per
        // stat to be a meaningful nudge in that stat's natural unit.
        mods.push(Some(ShopMod { kind, delta: kind.debug_step() }));
    }
    CustomizeShop { turrets, runes, mods }
}

pub fn init_customize_shop(mut commands: Commands) {
    commands.insert_resource(roll_fresh_stock());
}

// ---------- Cursor tracking ----------

/// Refresh `DragState.spec_cursor` from the window cursor + the live
/// viewport mapping. Runs first in the drag chain so every other system
/// reads the same value within a frame.
pub fn track_customize_cursor(
    open: Res<CustomizeOpen>,
    viewport: Res<CustomizeViewport>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut drag: ResMut<DragState>,
) {
    if !open.open {
        drag.spec_cursor = None;
        return;
    }
    let cursor = windows.single().ok().and_then(|w| w.cursor_position());
    drag.spec_cursor = cursor.and_then(|c| viewport.window_to_spec(c));
}

// ---------- Press → start drag ----------

pub fn start_drag(
    mut commands: Commands,
    open: Res<CustomizeOpen>,
    mut cfg: ResMut<TurretConfig>,
    shop_opt: Option<ResMut<CustomizeShop>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut drag: ResMut<DragState>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut scrap: ResMut<crate::Scrap>,
    sources: Query<(&Transform, &HitArea, &DragSourceMarker)>,
) {
    if !open.open || drag.picked.is_some() {
        return;
    }
    if !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Some(cursor) = drag.spec_cursor else { return };
    let Some(mut shop) = shop_opt else { return };

    // Find the smallest hit-area containing the cursor — when sockets
    // overlap larger panels visually, the more-specific source wins.
    let mut best: Option<(f32, DragSourceKind)> = None;
    for (tf, hit, marker) in &sources {
        if !hit_test(cursor, tf.translation.truncate(), hit.size) {
            continue;
        }
        let area = hit.size.x * hit.size.y;
        if best.map_or(true, |(a, _)| area < a) {
            best = Some((area, marker.0));
        }
    }
    let Some((_, source)) = best else { return };

    // Shop sources are click-to-equip — auto-place into the first empty
    // slot/socket and consume the shop offering. No drag, no ghost.
    // Only places into EMPTY targets so click-buy never accidentally
    // upgrades an existing turret's barrel level.
    if matches!(source, DragSourceKind::ShopTurret(_) | DragSourceKind::ShopRune(_)) {
        click_buy_shop(source, &mut cfg, &mut shop, &mut scrap);
        return;
    }

    let Some(payload) = payload_for(source, &cfg, &shop) else { return };
    drag.picked = Some(Picked { source, payload });
    let ghost = spawn_ghost(&mut commands, &mut meshes, &mut materials, payload, cursor);
    drag.ghost = Some(ghost);
}

/// Place a shop offering into the first empty target slot/socket.
/// Returns false silently when there's no room or the player can't
/// afford it — clicks that don't apply leave both the shop slot and
/// the scrap counter untouched. Cost is deducted only on a successful
/// placement.
fn click_buy_shop(
    source: DragSourceKind,
    cfg: &mut TurretConfig,
    shop: &mut CustomizeShop,
    scrap: &mut crate::Scrap,
) -> bool {
    if scrap.0 < SHOP_ITEM_COST { return false; }
    let placed = try_place_shop_item(source, cfg, shop);
    if placed {
        scrap.0 = scrap.0.saturating_sub(SHOP_ITEM_COST);
    }
    placed
}

fn try_place_shop_item(
    source: DragSourceKind,
    cfg: &mut TurretConfig,
    shop: &mut CustomizeShop,
) -> bool {
    match source {
        DragSourceKind::ShopTurret(idx) => {
            let Some(offering) = shop.turrets.get(idx).and_then(|o| *o) else { return false };
            let Some(slot_i) = (0..cfg.slots.len()).find(|&i| !cfg.slots[i].equipped) else {
                return false;
            };
            cfg.slots[slot_i] = SlotCfg {
                equipped: true,
                weapon: offering.weapon,
                damage: offering.weapon.defaults().0,
                fire_rate: offering.weapon.defaults().1,
                barrels: offering.barrels,
                runes: [None; 3],
            };
            if let Some(slot) = shop.turrets.get_mut(idx) { *slot = None; }
            true
        }
        DragSourceKind::ShopRune(idx) => {
            let Some(rune) = shop.runes.get(idx).and_then(|o| *o) else { return false };
            for slot_i in 0..cfg.slots.len() {
                if !cfg.slots[slot_i].equipped { continue; }
                for r in 0..3 {
                    if cfg.slots[slot_i].runes[r].is_none() {
                        cfg.slots[slot_i].runes[r] = Some(rune);
                        if let Some(slot) = shop.runes.get_mut(idx) { *slot = None; }
                        return true;
                    }
                }
            }
            false
        }
        _ => false,
    }
}

fn hit_test(cursor: Vec2, centre: Vec2, size: Vec2) -> bool {
    let half = size * 0.5;
    let min = centre - half;
    let max = centre + half;
    cursor.x >= min.x && cursor.x <= max.x && cursor.y >= min.y && cursor.y <= max.y
}

fn payload_for(
    source: DragSourceKind,
    cfg: &TurretConfig,
    shop: &CustomizeShop,
) -> Option<Payload> {
    match source {
        DragSourceKind::ShipSlot(slot) => {
            let s = cfg.slots[slot];
            if !s.equipped {
                return None;
            }
            Some(Payload::Turret {
                weapon: s.weapon,
                barrels: s.barrels.max(1),
            })
        }
        DragSourceKind::ShipRune { slot, rune_idx } => {
            let s = cfg.slots[slot];
            if !s.equipped {
                return None;
            }
            s.runes[rune_idx].map(Payload::Rune)
        }
        DragSourceKind::ShopTurret(idx) => {
            shop.turrets.get(idx).and_then(|o| o.as_ref()).map(|o| Payload::Turret {
                weapon: o.weapon,
                barrels: o.barrels,
            })
        }
        DragSourceKind::ShopRune(idx) => shop
            .runes
            .get(idx)
            .and_then(|o| o.as_ref())
            .copied()
            .map(Payload::Rune),
    }
}

// ---------- Ghost (follows cursor in spec coords) ----------

fn spawn_ghost(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ColorMaterial>,
    payload: Payload,
    cursor: Vec2,
) -> Entity {
    match payload {
        Payload::Turret { weapon, barrels } => spawn_ghost_turret(commands, meshes, materials, weapon, barrels, cursor),
        Payload::Rune(r) => spawn_ghost_rune(commands, meshes, materials, r, cursor),
    }
}

fn spawn_ghost_turret(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ColorMaterial>,
    weapon: WeaponType,
    barrels: u8,
    cursor: Vec2,
) -> Entity {
    // Mirror the in-game turret: a single coloured Circle. Barrel level
    // is conveyed by a number on the actual slot tiles (and the source
    // visual hides during drag), so the ghost stays minimal.
    let body_mat = materials.add(turret_color_for(weapon));
    let body = meshes.add(Circle::new(6.0));
    let _ = barrels; // level isn't drawn on the ghost itself
    commands.spawn((
        Mesh2d(body),
        MeshMaterial2d(body_mat),
        Transform::from_translation(cursor.extend(9.0)),
        RenderLayers::layer(CUSTOMIZE_LAYER),
        DragGhost,
    )).id()
}

fn spawn_ghost_rune(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ColorMaterial>,
    rune: Rune,
    cursor: Vec2,
) -> Entity {
    let mat = materials.add(rune_color_for(rune));
    let m = meshes.add(Rectangle::new(8.0, 8.0));
    commands.spawn((
        Mesh2d(m),
        MeshMaterial2d(mat),
        Transform::from_translation(cursor.extend(9.0)),
        RenderLayers::layer(CUSTOMIZE_LAYER),
        DragGhost,
    )).id()
}

pub fn update_drag_ghost(
    drag: Res<DragState>,
    mut ghosts: Query<&mut Transform, With<DragGhost>>,
) {
    if drag.picked.is_none() {
        return;
    }
    let Some(cursor) = drag.spec_cursor else { return };
    for mut tf in &mut ghosts {
        tf.translation.x = cursor.x;
        tf.translation.y = cursor.y;
    }
}

// ---------- Release → resolve drop ----------

pub fn complete_drag(
    mut commands: Commands,
    mouse: Res<ButtonInput<MouseButton>>,
    mut drag: ResMut<DragState>,
    mut cfg: ResMut<TurretConfig>,
    mut shop: Option<ResMut<CustomizeShop>>,
    targets: Query<(&Transform, &HitArea, &DropTargetMarker)>,
    ghosts: Query<Entity, With<DragGhost>>,
) {
    if !mouse.just_released(MouseButton::Left) {
        return;
    }
    let Some(picked) = drag.picked.take() else {
        for e in &ghosts {
            commands.entity(e).despawn();
        }
        return;
    };
    drag.ghost = None;
    for e in &ghosts {
        commands.entity(e).despawn();
    }

    let Some(cursor) = drag.spec_cursor else { return };

    // Smallest-area target wins (sockets > slots).
    let mut best: Option<(f32, DropTargetKind)> = None;
    for (tf, hit, marker) in &targets {
        if !hit_test(cursor, tf.translation.truncate(), hit.size) {
            continue;
        }
        let area = hit.size.x * hit.size.y;
        if best.map_or(true, |(a, _)| area < a) {
            best = Some((area, marker.0));
        }
    }
    let Some((_, target)) = best else { return };

    // Successful drop on a shop-sourced item consumes the shop slot
    // so the player can't pick the same offering twice.
    if resolve_drop(&picked, target, &mut cfg) {
        if let Some(shop) = shop.as_mut() {
            match picked.source {
                DragSourceKind::ShopTurret(idx) => {
                    if let Some(slot) = shop.turrets.get_mut(idx) {
                        *slot = None;
                    }
                }
                DragSourceKind::ShopRune(idx) => {
                    if let Some(slot) = shop.runes.get_mut(idx) {
                        *slot = None;
                    }
                }
                _ => {}
            }
        }
    }
}

/// Returns `true` if the drop changed game state (move / merge / equip).
/// Invalid drops (type mismatch, self-drop, mismatch on occupied target)
/// return `false` so the caller can leave the source untouched and the
/// shop unchanged.
fn resolve_drop(picked: &Picked, target: DropTargetKind, cfg: &mut TurretConfig) -> bool {
    match (picked.payload, target) {
        (Payload::Turret { weapon, barrels }, DropTargetKind::ShipSlot(target_slot)) => {
            if let DragSourceKind::ShipSlot(src) = picked.source {
                if src == target_slot {
                    return false;
                }
            }
            let target_state = cfg.slots[target_slot];
            if !target_state.equipped {
                cfg.slots[target_slot] = SlotCfg {
                    equipped: true,
                    weapon,
                    damage: weapon.defaults().0,
                    fire_rate: weapon.defaults().1,
                    barrels,
                    runes: [None; 3],
                };
                clear_source_if_ship(picked, cfg);
                true
            } else if target_state.weapon == weapon
                && target_state.barrels == barrels
                && target_state.barrels < 3
            {
                cfg.slots[target_slot].barrels = (target_state.barrels + 1).min(3);
                clear_source_if_ship(picked, cfg);
                true
            } else {
                false
            }
        }
        (Payload::Rune(rune), DropTargetKind::ShipRune { slot, rune_idx }) => {
            if !cfg.slots[slot].equipped {
                return false;
            }
            if let DragSourceKind::ShipRune { slot: s, rune_idx: r } = picked.source {
                if s == slot && r == rune_idx {
                    return false;
                }
            }
            cfg.slots[slot].runes[rune_idx] = Some(rune);
            if let DragSourceKind::ShipRune { slot: s, rune_idx: r } = picked.source {
                cfg.slots[s].runes[r] = None;
            }
            true
        }
        _ => false,
    }
}

fn clear_source_if_ship(picked: &Picked, cfg: &mut TurretConfig) {
    if let DragSourceKind::ShipSlot(s) = picked.source {
        cfg.slots[s] = SlotCfg::default();
    }
}
