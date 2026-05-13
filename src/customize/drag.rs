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
    /// A press-and-hold candidate, waiting out the debounce window
    /// before becoming an active drag. A quick click → release inside
    /// `DRAG_HOLD_THRESHOLD` resolves as a click-buy with no ghost
    /// ever spawned, so the visual stays still for shop clicks.
    pub pending: Option<Pending>,
    pub ghost: Option<Entity>,
    /// Last known spec cursor — used by the ghost-follow system + hit-test.
    pub spec_cursor: Option<Vec2>,
}

#[derive(Clone)]
pub struct Picked {
    pub source: DragSourceKind,
    pub payload: Payload,
}

/// A draggable hit by the press but not yet promoted to an active
/// drag. `press_time` is `Time::elapsed_secs()` at the press; the
/// promoter checks the gap each frame and converts to `picked` once
/// the threshold passes.
#[derive(Clone)]
pub struct Pending {
    pub source: DragSourceKind,
    pub payload: Payload,
    pub press_time: f32,
}

/// How long a mouse-down must be held on a draggable before a drag
/// ghost actually appears. Below this window we treat the press as a
/// click-buy. Tuned so a deliberate click feels instant but a held
/// press starts the drag without noticeable latency.
pub const DRAG_HOLD_THRESHOLD: f32 = 0.15;

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
        StatKind::XpHarvest         => "XP GAIN",
        StatKind::RuneDamage        => "RUNE EFFECT",
        StatKind::TurretDamage      => "TURRET DAMAGE",
    }
}

/// Scrap cost to re-roll the shop. Refills every slot — sold or not.
pub const SHOP_REROLL_COST: u32 = 1;
/// Scrap cost for a turret purchase.
pub const SHOP_TURRET_COST: u32 = 2;
/// Scrap cost for a rune purchase.
pub const SHOP_RUNE_COST: u32 = 2;
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
        WeaponType::SpreadRockets,
        WeaponType::Flamethrower,
    ];
    let runes_pool = [
        Rune::Fire,
        Rune::Frost,
        Rune::Shock,
        Rune::Echo,
        Rune::Cascade,
        Rune::Conduit,
        Rune::Resonate,
        Rune::Vampire,
        Rune::Ward,
        Rune::Bleed,
        Rune::Blast,
        Rune::Hustle,
        Rune::Pierce,
        Rune::Greed,
        Rune::Executioner,
        Rune::Opener,
        Rune::TargetFurthest,
        Rune::TargetHighestHp,
        Rune::TargetLowestHp,
        Rune::TargetCarousel,
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
    open: Res<CustomizeOpen>,
    cfg: Res<TurretConfig>,
    shop_opt: Option<Res<CustomizeShop>>,
    mouse: Res<ButtonInput<MouseButton>>,
    time: Res<Time>,
    mut drag: ResMut<DragState>,
    sources: Query<(&Transform, &HitArea, &DragSourceMarker)>,
) {
    if !open.open || drag.picked.is_some() || drag.pending.is_some() {
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

    // Stash as a *pending* drag rather than spawning the ghost
    // immediately. `promote_pending_drag` upgrades it to an active
    // drag after `DRAG_HOLD_THRESHOLD`; if the player releases first
    // (a click rather than a hold), `complete_drag` resolves the
    // pending as a click-buy with no ghost ever visible.
    let Some(payload) = payload_for(source, &cfg, &shop) else { return };
    drag.pending = Some(Pending {
        source,
        payload,
        press_time: time.elapsed_secs(),
    });
}

/// Promote a held-down pending drag to an active drag once the
/// debounce window elapses. Spawns the ghost only at this point, so
/// a quick click → release before the threshold never causes the
/// drag animation to flash.
pub fn promote_pending_drag(
    mut commands: Commands,
    time: Res<Time>,
    mut drag: ResMut<DragState>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    if drag.picked.is_some() { return; }
    let Some(pending) = drag.pending.as_ref() else { return };
    if time.elapsed_secs() - pending.press_time < DRAG_HOLD_THRESHOLD {
        return;
    }
    let Some(cursor) = drag.spec_cursor else { return };
    let source = pending.source;
    let payload = pending.payload;
    drag.pending = None;
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
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    targets: Query<(&Transform, &HitArea, &DropTargetMarker)>,
    ship_slots: Query<(&Transform, &super::setup::ShipSlotBase)>,
    ghosts: Query<Entity, With<DragGhost>>,
) {
    if !mouse.just_released(MouseButton::Left) {
        return;
    }

    // Release-while-pending = the press never crossed the debounce
    // threshold, so the player meant to *click*, not drag. Skip the
    // drop-target resolution path entirely and run the click-buy
    // directly — no ghost ever spawned, no transient drag visual.
    //
    // Ship-sourced clicks released over the SELL strip should still
    // sell — the debounce shouldn't punish a quick press → release on
    // the sell panel. So we resolve targets here for ship sources too,
    // but only honour `DropTargetKind::Sell`; ship-to-ship rune/turret
    // moves still require an actual drag (gives the player a clear
    // visual contract: "to move, hold and drag").
    if drag.picked.is_none() {
        if let Some(pending) = drag.pending.take() {
            let from_shop = matches!(
                pending.source,
                DragSourceKind::ShopTurret(_) | DragSourceKind::ShopRune(_)
            );
            if from_shop {
                let burst_color = purchase_burst_color(
                    &pending.source,
                    shop.as_deref(),
                );
                if let (Some(mut shop_ref), Some(cursor)) = (shop.as_mut(), drag.spec_cursor) {
                    if click_buy_shop(pending.source, &mut cfg, &mut shop_ref, &mut scrap) {
                        if let Some(color) = burst_color {
                            spawn_purchase_burst(
                                &mut commands, &mut meshes, &mut materials, cursor, color,
                            );
                        }
                    }
                }
            } else if let Some(cursor) = drag.spec_cursor {
                // Ship-source click: only the SELL strip honours a
                // click. Everything else requires holding for a drag.
                let on_sell = targets.iter().any(|(tf, hit, marker)| {
                    matches!(marker.0, DropTargetKind::Sell)
                        && hit_test(cursor, tf.translation.truncate(), hit.size)
                });
                if on_sell {
                    let refund = sell_refund_for(&pending.source, &cfg);
                    if refund > 0 {
                        clear_source(&pending.source, &mut cfg);
                        scrap.0 = scrap.0.saturating_add(refund);
                    }
                }
            }
            return;
        }
        // Nothing in flight — clean up any stale ghost just in case.
        for e in &ghosts {
            commands.entity(e).despawn();
        }
        return;
    }

    let picked = drag.picked.take().expect("checked is_none above");
    drag.pending = None;
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
            let shop_ref = shop.as_deref();
            let burst_color = if from_shop {
                purchase_burst_color(&picked.source, shop_ref)
            } else {
                None
            };
            if resolve_drop(&picked, target, &mut cfg) {
                if from_shop {
                    scrap.0 = scrap.0.saturating_sub(shop_cost);
                    if let Some(shop) = shop.as_mut() {
                        consume_shop_slot(&picked.source, shop);
                    }
                    if let Some(color) = burst_color {
                        let pos = drop_burst_position(target, cursor, &ship_slots);
                        spawn_purchase_burst(
                            &mut commands, &mut meshes, &mut materials, pos, color,
                        );
                    }
                }
            }
        }
        None => {
            // Released off any drop target. A genuine click (press +
            // release inside `DRAG_HOLD_THRESHOLD`) is handled by the
            // pending branch at the top of this function — by the time
            // we reach the drop-resolution arm, the player has been
            // dragging a ghost. Releasing in empty space then means
            // "cancel this drag", not "auto-place". No purchase, no
            // refund, no movement.
            let _ = from_shop;
            let _ = cursor;
        }
    }
}

/// World-space spec coord to drop the particle burst on. Drag-drops
/// snap to the centre of the resolved slot (so the visual sits on
/// the freshly-equipped turret) rather than the cursor — looks
/// cleaner than a burst that lands wherever the player happened
/// to release.
fn drop_burst_position(
    target: DropTargetKind,
    cursor: Vec2,
    ship_slots: &Query<(&Transform, &super::setup::ShipSlotBase)>,
) -> Vec2 {
    let slot_idx = match target {
        DropTargetKind::ShipSlot(i) => Some(i),
        DropTargetKind::ShipRune { slot, .. } => Some(slot),
        DropTargetKind::Sell => None,
    };
    if let Some(idx) = slot_idx {
        for (tf, base) in ship_slots.iter() {
            if base.slot == idx {
                return tf.translation.truncate();
            }
        }
    }
    cursor
}

/// Resolve the burst colour for a shop-sourced drag — the turret's
/// or rune's own tint, so the particles read as "this thing arrived
/// here". Returns `None` for non-shop sources (ship-to-ship moves
/// don't get a burst).
fn purchase_burst_color(
    source: &DragSourceKind,
    shop: Option<&CustomizeShop>,
) -> Option<Color> {
    let shop = shop?;
    match *source {
        DragSourceKind::ShopTurret(idx) => {
            let offer = shop.turrets.get(idx).copied().flatten()?;
            Some(turret_color_for(offer.weapon))
        }
        DragSourceKind::ShopRune(idx) => {
            let rune = shop.runes.get(idx).copied().flatten()?;
            Some(rune_color_for(rune))
        }
        _ => None,
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
            let displaced = cfg.slots[slot].runes[rune_idx];
            let ship_src = if let DragSourceKind::ShipRune { slot: s, rune_idx: r } = picked.source {
                Some((s, r))
            } else {
                None
            };
            // Targeting-rune exclusivity on the destination slot. The
            // moving rune takes `rune_idx`, so exclude that socket. If
            // the drag started in the same slot, the rune still sits
            // in its source socket pre-write — exclude that too, or
            // we'd count it as a stale duplicate.
            if rune.is_targeting() {
                for (i, r) in cfg.slots[slot].runes.iter().enumerate() {
                    if i == rune_idx { continue; }
                    if let Some((src_slot, src_idx)) = ship_src {
                        if src_slot == slot && src_idx == i { continue; }
                    }
                    if let Some(other) = r {
                        if other.is_targeting() {
                            return false;
                        }
                    }
                }
            }
            // Cross-slot swap: the displaced rune is about to take the
            // source socket. Enforce the same exclusivity on the source
            // slot so a swap can't smuggle two targeting runes onto one
            // weapon.
            if let (Some(disp), Some((src_slot, src_idx))) = (displaced, ship_src) {
                if src_slot != slot && disp.is_targeting() {
                    for (i, r) in cfg.slots[src_slot].runes.iter().enumerate() {
                        if i == src_idx { continue; }
                        if let Some(other) = r {
                            if other.is_targeting() {
                                return false;
                            }
                        }
                    }
                }
            }
            cfg.slots[slot].runes[rune_idx] = Some(rune);
            if let Some((src_slot, src_idx)) = ship_src {
                // Send the displaced rune back to the source socket so
                // ship-to-ship rune drops behave as a swap, not an
                // overwrite. Empty destination => source clears.
                cfg.slots[src_slot].runes[src_idx] = displaced;
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

// ---------- Purchase-confirmation particles ----------

/// Short-lived sparkle spawned when a shop turret / rune is successfully
/// bought. Lives on `CUSTOMIZE_LAYER` so it renders inside the shop's
/// chunky-pixel render target alongside everything else.
#[derive(Component)]
pub struct PurchaseBurstParticle {
    pub life: f32,
    pub max_life: f32,
    pub velocity: Vec2,
}

/// Burst size + speed — tuned for a "subtle confirm" feel rather than
/// a kill explosion. Particles fly outward radially, fade as they
/// shrink, and despawn in well under half a second.
const BURST_COUNT: u32 = 10;
const BURST_LIFE_MIN: f32 = 0.18;
const BURST_LIFE_MAX: f32 = 0.36;
const BURST_SPEED_MIN: f32 = 10.0;
const BURST_SPEED_MAX: f32 = 22.0;
/// Z high enough to sit above the just-placed turret base but below
/// any popped-up tooltip text.
const BURST_Z: f32 = 6.0;

pub fn spawn_purchase_burst(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ColorMaterial>,
    pos: Vec2,
    color: Color,
) {
    let mesh = meshes.add(Rectangle::new(0.9, 0.9));
    let mat = materials.add(color);
    let mut rng = rand::thread_rng();
    for _ in 0..BURST_COUNT {
        let angle = rng.gen::<f32>() * std::f32::consts::TAU;
        let speed = rng.gen_range(BURST_SPEED_MIN..BURST_SPEED_MAX);
        let velocity = Vec2::new(angle.cos(), angle.sin()) * speed;
        let life = rng.gen_range(BURST_LIFE_MIN..BURST_LIFE_MAX);
        commands.spawn((
            Mesh2d(mesh.clone()),
            MeshMaterial2d(mat.clone()),
            Transform::from_translation(pos.extend(BURST_Z)),
            RenderLayers::layer(CUSTOMIZE_LAYER),
            PurchaseBurstParticle { life, max_life: life, velocity },
        ));
    }
}

/// Advance every `PurchaseBurstParticle`: drift outward, shrink by
/// life fraction, despawn when expired. Independent of `CustomizeOpen`
/// (particles already in flight when the panel closes finish their
/// short life rather than hanging in zombie state).
pub fn tick_purchase_particles(
    time: Res<Time>,
    mut commands: Commands,
    mut q: Query<(Entity, &mut Transform, &mut PurchaseBurstParticle)>,
) {
    let dt = time.delta_secs();
    for (e, mut tf, mut p) in &mut q {
        p.life -= dt;
        if p.life <= 0.0 {
            commands.entity(e).despawn();
            continue;
        }
        tf.translation.x += p.velocity.x * dt;
        tf.translation.y += p.velocity.y * dt;
        // Decelerate so the burst settles instead of flying linearly
        // to the edge of the panel.
        p.velocity *= (1.0 - dt * 4.0).max(0.0);
        let t = (p.life / p.max_life).clamp(0.0, 1.0);
        let s = t * 1.1;
        tf.scale = Vec3::new(s, s, 1.0);
    }
}
