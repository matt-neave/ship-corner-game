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
use rand::Rng;

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
    /// Sell panel — drop a ship-sourced turret/rune here to refund a
    /// fraction of its original shop cost. Source ship-slot/socket
    /// is cleared; shop-sourced drops on this panel are no-ops.
    Sell,
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
    /// Optional trade-off side-effect: a SECOND stat that ALSO
    /// changes when this mod is picked, typically a nerf paired
    /// with the primary buff. None for plain mods. The roll table
    /// in `roll_fresh_stock` mixes pure buffs with these trade-off
    /// cards so the shop offers genuine choices, not just always-
    /// good upgrades.
    pub side: Option<(StatKind, f32)>,
}

impl ShopMod {
    /// Card text. Pure mods render two lines (value + short name).
    /// Trade-off mods render FOUR — primary buff on top, side
    /// nerf below — so the player sees both halves of the deal at
    /// a glance. The colour pass on the card paints buffs green /
    /// nerfs red automatically (it sees the sign on each segment).
    pub fn label(self) -> String {
        let main = format!(
            "{}\n{}",
            self.kind.format_delta(self.delta),
            short_stat_label(self.kind),
        );
        match self.side {
            Some((k, d)) => format!(
                "{}\n{}\n{}",
                main,
                k.format_delta(d),
                short_stat_label(k),
            ),
            None => main,
        }
    }
}

/// Compact card-friendly label for a stat. The stats panel uses
/// `StatKind::label` for the full form; this trims the longer
/// names down so the mod card text stays one line.
fn short_stat_label(kind: StatKind) -> &'static str {
    match kind {
        StatKind::Hp                => "HEALTH",
        StatKind::ShieldMax         => "SHIELD",
        StatKind::MoveSpeed         => "SPEED",
        StatKind::TurnSpeed         => "TURN",
        StatKind::TurretTurnSpeed   => "TURRET TURN",
        StatKind::TurretArcBonus    => "TURRET ARC",
        StatKind::Range             => "RANGE",
        StatKind::Crit              => "CRIT",
        StatKind::Luck              => "LUCK",
        StatKind::ProcStrength      => "PROC STRENGTH",
        StatKind::Harvest           => "HARVEST",
        StatKind::RuneDamage        => "RUNE DAMAGE",
        StatKind::TurretDamage      => "TURRET DAMAGE",
    }
}

/// Scrap cost to re-roll the shop. Refills every slot — sold or not.
pub const SHOP_REROLL_COST: u32 = 5;
/// Scrap cost for a turret purchase.
pub const SHOP_TURRET_COST: u32 = 15;
/// Scrap cost for a rune purchase.
pub const SHOP_RUNE_COST: u32 = 10;
/// Sell refund fraction — selling returns this share of the
/// original purchase cost (rounded down). `0.33` → 33%: a 15-scrap
/// turret refunds 4 (15 × 0.33 = 4.95 → 4); a 10-scrap rune refunds
/// 3 (10 × 0.33 = 3.3 → 3).
pub const SHOP_SELL_FRACTION: f32 = 0.33;
/// Backwards-compatibility alias: existing callers that don't yet
/// distinguish turret/rune/mod still reference `SHOP_ITEM_COST`.
/// Pointed at `SHOP_TURRET_COST` so the most expensive baseline wins
/// when used as a guardrail (e.g. cost-display defaults).
pub const SHOP_ITEM_COST: u32 = SHOP_TURRET_COST;

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
        WeaponType::Cannon,
        WeaponType::Booster,
        WeaponType::Blade,
        WeaponType::Cage,
        WeaponType::Harpoon,
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
        // Roughly one in three rolls is a trade-off card: bigger
        // buff on the primary stat plus a nerf on a DIFFERENT
        // stat. Forces the player to weigh each mod against their
        // build instead of clicking everything. Pure-buff mods use
        // the standard `debug_step` value; trade-off cards bump
        // the buff to 1.5x and apply a -0.75x nerf to a random
        // other stat, so the upside outweighs the downside but the
        // pick still costs you something.
        let trade_off = rng.gen_bool(0.33);
        if trade_off {
            // Pick a side-effect stat that isn't the primary.
            let side_kind = loop {
                let k = *StatKind::ALL.choose(&mut rng).unwrap();
                if k != kind { break k; }
            };
            let buff = kind.debug_step() * 1.5;
            let nerf = -side_kind.debug_step() * 0.75;
            mods.push(Some(ShopMod {
                kind,
                delta: buff,
                side: Some((side_kind, nerf)),
            }));
        } else {
            mods.push(Some(ShopMod {
                kind,
                delta: kind.debug_step(),
                side: None,
            }));
        }
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
    cfg: Res<TurretConfig>,
    shop_opt: Option<Res<CustomizeShop>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut drag: ResMut<DragState>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
    sources: Query<(&Transform, &HitArea, &DragSourceMarker)>,
) {
    if !open.open || drag.picked.is_some() {
        return;
    }
    if !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Some(cursor) = drag.spec_cursor else { return };
    let Some(shop) = shop_opt else { return };

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

    // Shop AND ship sources both enter the drag flow now. Shop drops
    // on a valid slot/socket consume the shop offering + deduct cost
    // in `complete_drag`. Releasing without hitting a drop target
    // falls back to click-buy (auto-place to the first empty slot)
    // so a quick click still buys without requiring precise aim.
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
    // Cost depends on what the player is buying.
    let cost = match source {
        DragSourceKind::ShopTurret(_) => SHOP_TURRET_COST,
        DragSourceKind::ShopRune(_)   => SHOP_RUNE_COST,
        _ => return false,
    };
    if scrap.0 < cost { return false; }
    let placed = try_place_shop_item(source, cfg, shop);
    if placed {
        scrap.0 = scrap.0.saturating_sub(cost);
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
    // Mirror the in-socket rune visual — a rounded-square built from
    // 4 corner circles + a horizontal rect + a vertical rect, all the
    // same colour. The previous ghost drew a flat 8×8 square, which
    // looked nothing like the actual rune mid-drag. We spawn an empty
    // root entity at the cursor, then parent each shape part under it
    // with local offsets — `update_drag_ghost` moves only the root,
    // and the children follow via Bevy's transform propagation.
    const SOCKET: f32 = 8.0;
    const SOCKET_RADIUS: f32 = 3.0;
    let mat = materials.add(rune_color_for(rune));
    let circle = meshes.add(Circle::new(SOCKET_RADIUS));
    let h_rect = meshes.add(Rectangle::new(SOCKET, SOCKET - 2.0 * SOCKET_RADIUS));
    let v_rect = meshes.add(Rectangle::new(SOCKET - 2.0 * SOCKET_RADIUS, SOCKET));
    let root = commands.spawn((
        Transform::from_translation(cursor.extend(9.0)),
        Visibility::default(),
        RenderLayers::layer(CUSTOMIZE_LAYER),
        DragGhost,
    )).id();
    // Body rects (cross shape) — local (0,0) relative to root.
    for mesh in [Mesh2d(h_rect), Mesh2d(v_rect)] {
        let part = commands.spawn((
            mesh,
            MeshMaterial2d(mat.clone()),
            Transform::from_xyz(0.0, 0.0, 0.0),
            RenderLayers::layer(CUSTOMIZE_LAYER),
        )).id();
        commands.entity(part).insert(ChildOf(root));
    }
    // Corner circles — offset diagonally to round the cross's corners.
    let half = (SOCKET - 2.0 * SOCKET_RADIUS) * 0.5;
    for offset in [
        Vec2::new(-half, -half),
        Vec2::new( half, -half),
        Vec2::new(-half,  half),
        Vec2::new( half,  half),
    ] {
        let part = commands.spawn((
            Mesh2d(circle.clone()),
            MeshMaterial2d(mat.clone()),
            Transform::from_xyz(offset.x, offset.y, 0.0),
            RenderLayers::layer(CUSTOMIZE_LAYER),
        )).id();
        commands.entity(part).insert(ChildOf(root));
    }
    root
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
    mut scrap: ResMut<crate::Scrap>,
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

    let from_shop = matches!(
        picked.source,
        DragSourceKind::ShopTurret(_) | DragSourceKind::ShopRune(_)
    );
    let shop_cost = match picked.source {
        DragSourceKind::ShopTurret(_) => SHOP_TURRET_COST,
        DragSourceKind::ShopRune(_) => SHOP_RUNE_COST,
        _ => 0,
    };

    match best {
        Some((_, DropTargetKind::Sell)) => {
            // Sell path — refund scrap proportional to the original
            // purchase cost. Shop-sourced drops here are no-ops
            // (nothing to "sell" — you don't own them yet).
            if from_shop { return; }
            let refund = sell_refund_for(&picked.source, &cfg);
            if refund == 0 { return; }
            clear_source(&picked.source, &mut cfg);
            scrap.0 = scrap.0.saturating_add(refund);
        }
        Some((_, target)) => {
            // Drag-drop path. Shop-sourced drops cost scrap; ship-
            // sourced drops are free (just moving turrets around).
            if from_shop && scrap.0 < shop_cost {
                return; // can't afford — leave shop + cfg untouched
            }
            if resolve_drop(&picked, target, &mut cfg) {
                if from_shop {
                    scrap.0 = scrap.0.saturating_sub(shop_cost);
                    if let Some(shop) = shop.as_mut() {
                        consume_shop_slot(&picked.source, shop);
                    }
                }
            }
        }
        None => {
            // Released without aim on a slot/socket — for shop
            // sources, behave like a click-buy: auto-place into the
            // first empty target. Keeps the quick-click UX even
            // though we always spawn a ghost on mouse-down now.
            if from_shop {
                if let Some(mut shop_ref) = shop {
                    click_buy_shop(picked.source, &mut cfg, &mut shop_ref, &mut scrap);
                }
            }
        }
    }
}

/// Refund a ship-sourced drag (turret or rune) at `SHOP_SELL_FRACTION`
/// of its original buy cost. Multi-barrel turrets refund per-barrel,
/// and socketed runes' cost is included in the turret-sell payout so
/// the player isn't taxed twice for losing them.
///
/// Shop-sourced drags return `0` — can't sell something you don't
/// own yet.
pub fn sell_refund_for(source: &DragSourceKind, cfg: &TurretConfig) -> u32 {
    match *source {
        DragSourceKind::ShipSlot(slot) => {
            let s = cfg.slots[slot];
            if !s.equipped { return 0; }
            let mut total_cost = SHOP_TURRET_COST * s.barrels.max(1) as u32;
            for _ in s.runes.iter().flatten() {
                total_cost += SHOP_RUNE_COST;
            }
            (total_cost as f32 * SHOP_SELL_FRACTION).floor() as u32
        }
        DragSourceKind::ShipRune { slot, rune_idx } => {
            if cfg.slots[slot].runes[rune_idx].is_none() { return 0; }
            (SHOP_RUNE_COST as f32 * SHOP_SELL_FRACTION).floor() as u32
        }
        _ => 0,
    }
}

/// Clear a ship-sourced slot (or socket) after a successful sell.
fn clear_source(source: &DragSourceKind, cfg: &mut TurretConfig) {
    match *source {
        DragSourceKind::ShipSlot(s) => {
            cfg.slots[s] = SlotCfg::default();
        }
        DragSourceKind::ShipRune { slot, rune_idx } => {
            cfg.slots[slot].runes[rune_idx] = None;
        }
        _ => {}
    }
}

/// Helper for `complete_drag`: blank the shop slot a successful
/// drag-buy came from so the player can't pick the same offering
/// twice. Cost deduction is the caller's responsibility.
fn consume_shop_slot(source: &DragSourceKind, shop: &mut CustomizeShop) {
    match source {
        DragSourceKind::ShopTurret(idx) => {
            if let Some(slot) = shop.turrets.get_mut(*idx) {
                *slot = None;
            }
        }
        DragSourceKind::ShopRune(idx) => {
            if let Some(slot) = shop.runes.get_mut(*idx) {
                *slot = None;
            }
        }
        _ => {}
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
            // Runes carry with the turret: when picking up a ship-
            // slot turret and dropping it on a fresh slot, the
            // socketed runes travel with it. Shop-sourced turrets
            // start with empty sockets.
            let carried_runes = match picked.source {
                DragSourceKind::ShipSlot(src) => cfg.slots[src].runes,
                _ => [None; 3],
            };
            let target_state = cfg.slots[target_slot];
            if !target_state.equipped {
                cfg.slots[target_slot] = SlotCfg {
                    equipped: true,
                    weapon,
                    damage: weapon.defaults().0,
                    fire_rate: weapon.defaults().1,
                    barrels,
                    runes: carried_runes,
                };
                clear_source_if_ship(picked, cfg);
                true
            } else if target_state.weapon == weapon
                && target_state.barrels == barrels
                && target_state.barrels < 3
            {
                // Stack-merge: bump barrels and slot any carried
                // runes into the target's empty sockets so they
                // don't vanish into the void.
                cfg.slots[target_slot].barrels = (target_state.barrels + 1).min(3);
                let mut merged = cfg.slots[target_slot].runes;
                for r in carried_runes.iter().flatten() {
                    if let Some(slot) = merged.iter_mut().find(|s| s.is_none()) {
                        *slot = Some(*r);
                    }
                }
                cfg.slots[target_slot].runes = merged;
                clear_source_if_ship(picked, cfg);
                true
            } else if let DragSourceKind::ShipSlot(src) = picked.source {
                // Ship-to-ship swap: target is occupied with a
                // different weapon (or already-maxed barrels of the
                // same weapon). Exchange the two SlotCfgs wholesale —
                // stats, barrels, and runes all travel with their
                // weapon. Both slots stay equipped post-swap.
                cfg.slots.swap(src, target_slot);
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
