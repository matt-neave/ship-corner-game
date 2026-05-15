//! Customize-overlay primitive spawning.
//!
//! Two layers in play:
//! - **`CUSTOMIZE_LAYER`** (low-res, chunky pixels) — ship hull, turret
//!   bases, barrels, rune sockets, shop tile bodies, level-badge bodies,
//!   tooltip background. Anything where the chunky stair-stepping is
//!   the look we want.
//! - **`UPSCALE_LAYER`** (native res, sharp) — every text label. The
//!   user wants text immune to pixelation, so labels render through the
//!   same camera as the in-game HUD and stay crisp.
//!
//! Component decomposition (your "core shapes" guidance):
//! - **Ship turret slot** = `Circle` base + 1–3 thin barrel `Rectangle`s,
//!   matching the in-game `ship.rs` rendering at 2× scale.
//! - **Rune socket** = small rounded square (h-rect + v-rect + 4 corner
//!   circles). Same compositional pattern as the shop tiles.
//! - **Shop turret tile** = chunky-rounded square (large corner radius)
//!   with the in-game-style turret silhouette inside.
//! - **Hull** = the in-game `Capsule2d` mesh, scaled 2× and rotated -90°
//!   so bow faces +X.
//! - **Tooltip** = container background (chunky pixel) + native-res title
//!   + native-res body text.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;
use bevy::sprite::MeshMaterial2d;
use bevy::text::FontSmoothing;

use crate::balance::{
    CUSTOMIZE_INTERNAL_H, CUSTOMIZE_INTERNAL_W, CUSTOMIZE_LAYER, HULL_LEN, HULL_WIDTH,
    TURRET_POSITIONS, UPSCALE_LAYER,
};
use crate::palette::{
    hex, Palette, MG_HEX, MORTAR_BRIGHT_HEX, MORTAR_HEX, RAILGUN_HEX, SHOTGUN_HEX, SNIPER_HEX,
};
use crate::rune::Rune;
use crate::weapon::WeaponType;
use crate::Scrap;

use super::drag::{DragSourceKind, DropTargetKind};
use super::CustomizeRoot;

// ---------- Marker components ----------

/// Tag on every text entity in the customize overlay. Toggled visible
/// by the visibility-sync system based on `CustomizeOpen`. Lives on the
/// upscale layer so text stays sharp.
#[derive(Component)]
pub struct CustomizeText;

/// Spec-coord position of a customize text entity. The sync system
/// multiplies by `viewport.display_scale` each frame to derive the
/// upscale-camera world position (1 world unit = 1 window pixel).
#[derive(Component, Clone, Copy)]
pub struct CustomizeTextSpec(pub Vec2);

/// Live scrap counter — updated each frame from the resource.
#[derive(Component)]
pub struct CustomizeScrapText;

/// Tag on the click target for a ship turret slot. The `DragSourceMarker`
/// + `DropTargetMarker` carry the slot index for resolution.
#[derive(Component, Clone, Copy)]
pub struct ShipSlotButton;

/// Tag on the round turret base for a ship slot. The base's material
/// colour is sync'd to the equipped weapon by the updater.
#[derive(Component, Clone, Copy)]
pub struct ShipSlotBase {
    pub slot: usize,
}

/// Centred number on a turret base showing barrel level (1-3). Sync'd
/// from `cfg.barrels` each frame; blank when the slot is empty or the
/// turret is being dragged.
#[derive(Component)]
pub struct ShipSlotBadgeText {
    pub slot: usize,
}

/// Tag on the click target for a rune socket. Slot + rune index live on
/// the `DragSourceMarker` / `DropTargetMarker`.
#[derive(Component, Clone, Copy)]
pub struct ShipRuneSocket;

/// Tag on every shape that's part of a rune socket — used by the
/// updater to recolour the whole socket when its contents change.
#[derive(Component, Clone, Copy)]
pub struct ShipRuneSocketPart {
    pub slot: usize,
    pub rune_idx: usize,
}

/// Tag on the diagonal-hash stripe overlay entities for a rune socket.
/// Hidden by default; toggled visible by `update_customize_ui` when the
/// socket is locked by a targeting rune in a sibling socket. Each
/// stripe is its own entity (alternating light- and dark-red materials)
/// so the result reads as a hatched "no-entry" pattern rather than a
/// flat red fill.
#[derive(Component, Clone, Copy)]
pub struct ShipRuneSocketLockHash {
    pub slot: usize,
    pub rune_idx: usize,
}

#[derive(Component)]
pub struct ShopTurretSlot;

#[derive(Component)]
pub struct ShopRuneSlot;

/// Tag on the gray-hash sell panel at the bottom of the shop column.
/// `DropTargetMarker(Sell)` is attached alongside; dropping a
/// ship-sourced item here refunds `SHOP_SELL_FRACTION` of its
/// original cost via `complete_drag`.
#[derive(Component)]
pub struct ShopSellPanel;

/// Tag on every shape that's part of a shop turret tile body.
#[derive(Component, Clone, Copy)]
pub struct ShopTurretVisual {
    pub idx: usize,
}

/// Tag on the inner darker circle of a shop turret tile. Re-coloured
/// uniformly each frame; the tile colour identifies the weapon, not this
/// inner disc.
#[derive(Component, Clone, Copy)]
pub struct ShopTurretBase;

/// Centred number on a shop turret tile showing barrel level (1-3).
#[derive(Component)]
pub struct ShopTurretBadgeText {
    pub idx: usize,
}

#[derive(Component)]
pub struct ShopTurretNameText {
    pub idx: usize,
}

/// Cost label below a shop turret tile. Cleared when the slot is sold
/// or being dragged out — same lifecycle as `ShopTurretNameText`.
#[derive(Component)]
pub struct ShopTurretCostText {
    pub idx: usize,
}

#[derive(Component, Clone, Copy)]
pub struct ShopRuneVisual {
    pub idx: usize,
}

#[derive(Component)]
pub struct ShopRuneNameText {
    pub idx: usize,
}

/// Cost label below a shop rune socket. Same clear-on-sold/dragged
/// behaviour as `ShopRuneNameText`.
#[derive(Component)]
pub struct ShopRuneCostText {
    pub idx: usize,
}

/// AOE badge on a shop turret card. Currently force-hidden — the
/// AOE tag was moved into the tooltip description. Entities are kept
/// around for a cheap revert by reactivating the per-card sync.
#[derive(Component, Clone, Copy)]
#[allow(dead_code)]
pub struct ShopTurretAoeTag {
    pub idx: usize,
    pub spec_pos: Vec2,
}

/// Sibling text label "AOE" tied to a `ShopTurretAoeTag`. Currently
/// force-hidden alongside its parent.
#[derive(Component, Clone, Copy)]
#[allow(dead_code)]
pub struct ShopTurretAoeTagText {
    pub idx: usize,
    pub spec_pos: Vec2,
}

/// AOE badge on a shop rune card. Currently force-hidden — see
/// `ShopTurretAoeTag`.
#[derive(Component, Clone, Copy)]
#[allow(dead_code)]
pub struct ShopRuneAoeTag {
    pub idx: usize,
    pub spec_pos: Vec2,
}

/// Sibling text label "AOE" tied to a `ShopRuneAoeTag`. Currently
/// force-hidden alongside its parent.
#[derive(Component, Clone, Copy)]
#[allow(dead_code)]
pub struct ShopRuneAoeTagText {
    pub idx: usize,
    pub spec_pos: Vec2,
}

// ---------- Hit areas ----------

#[derive(Component, Clone, Copy)]
pub struct HitArea {
    pub size: Vec2,
}

#[derive(Component, Clone, Copy)]
pub struct DragSourceMarker(pub DragSourceKind);

#[derive(Component, Clone, Copy)]
pub struct DropTargetMarker(pub DropTargetKind);

// ---------- Layout (spec units = internal pixels) ----------

const Z_HULL: f32 = 1.0;
const Z_TILE_BG: f32 = 2.0;
const Z_TILE_FG: f32 = 3.0;

// AOE badge — bright orange, sized to read as a tag without dominating
// the card. Both turret + rune cards use the same colour so the player
// links "Mortar (AOE weapon) ↔ Splash rune (AOE buff)".
pub const AOE_TAG_COLOR: Color = Color::srgb(1.0, 0.55, 0.15);
pub const AOE_TAG_SIZE: Vec2 = Vec2::new(14.0, 7.0);
/// Z above tile body (Z_TILE_FG) so the badge overlays the card.
const Z_AOE_TAG: f32 = 100.5;
const Z_AOE_TAG_TEXT: f32 = 101.0;

/// Pixels per in-game-hull-unit. Comfortable hull footprint inside the
/// 320×200 spec canvas without dominating it.
const SHIP_SCALE: f32 = 3.0;

// ---- Ship turret (in-game style: Circle base, barrel level shown as a
//      centred number — no rectangle barrels) ----
const TURRET_BASE_R: f32 = 6.0; // ~3× the in-game Circle::new(2.0)

// ---- Shop tiles (chunky rounded squares; sized so three fit in the LHS
//      column with margin) ----
const SHOP_TILE: f32 = 16.0;
const SHOP_TILE_RADIUS: f32 = 5.0;
const SHOP_TURRET_BASE_R: f32 = 4.0; // inner turret-from-above circle

// ---- Rune sockets ----
const SOCKET: f32 = 8.0;
const SOCKET_RADIUS: f32 = 3.0;
const SOCKET_GAP: f32 = 2.0;
/// Distance from a turret centre to the NEAREST socket. The triplet
/// stacks outward from there (column above/below wing turrets, row
/// right/left of bow/stern) — that way three sockets per turret never
/// collide with adjacent turrets' triplets along the ship's x-axis.
const SOCKET_OFFSET: f32 = 12.0;

// ---------- Setup ----------

pub fn setup_customize_ui(
    mut commands: Commands,
    scrap: Res<Scrap>,
    palette: Res<Palette>,
    pixel_font: Res<crate::fonts::PixelFont>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    let hull_capsule = meshes.add(Capsule2d::new(
        HULL_WIDTH * SHIP_SCALE * 0.5,
        (HULL_LEN - HULL_WIDTH) * SHIP_SCALE,
    ));
    let hull_mat = materials.add(palette.hull);

    commands.spawn((
        Transform::default(),
        Visibility::Inherited,
        CustomizeRoot,
    ));

    // ---------- Top-left scrap counter (text on UPSCALE_LAYER) ----------
    let scrap_pos = Vec2::new(
        -(CUSTOMIZE_INTERNAL_W as f32) * 0.5 + 30.0,
         (CUSTOMIZE_INTERNAL_H as f32) * 0.5 - 12.0,
    );
    spawn_text(
        &mut commands,
        scrap_pos,
        format!("SCRAP {}", scrap.0),
        Color::srgb(1.0, 0.85, 0.30),
        20.0,
        CustomizeScrapText,
    );
    // Invisible hit area sitting under the SCRAP counter — hovering
    // it routes through the same tooltip pipeline as turret / stat
    // hovers and shows a fixed explainer about how scrap is earned.
    commands.spawn((
        Transform::from_translation(scrap_pos.extend(2.0)),
        HitArea { size: Vec2::new(60.0, 14.0) },
        super::tooltip::ScrapTooltipHover,
        RenderLayers::layer(CUSTOMIZE_LAYER),
    ));

    // ---------- Top-right CLOSE button ----------
    let close_pos = Vec2::new(
         (CUSTOMIZE_INTERNAL_W as f32) * 0.5 - 22.0,
         (CUSTOMIZE_INTERNAL_H as f32) * 0.5 - 12.0,
    );
    spawn_container(
        &mut commands,
        &mut meshes,
        &mut materials,
        close_pos,
        Vec2::new(34.0, 12.0),
        SHOP_TILE_RADIUS.min(5.0),
        Color::srgb(0.50, 0.20, 0.22),
        Z_TILE_BG,
        super::CustomizeCloseBtn,
    );
    spawn_text(&mut commands, close_pos, "CLOSE", Color::WHITE, 14.0, CloseLabelTag);
    commands.spawn((
        Transform::from_translation(close_pos.extend(Z_TILE_BG)),
        HitArea { size: Vec2::new(34.0, 12.0) },
        super::CustomizeCloseBtn,
        CloseHitTag,
        RenderLayers::layer(CUSTOMIZE_LAYER),
    ));

    // ---------- LHS shop ----------
    // Anchor the shop column far enough from the canvas left edge
    // that every row fits. The mod-card row (3 × MOD_CARD_W + 2
    // gaps) is the widest, so `shop_x` is computed from the
    // half-width of that row plus a small outer margin. Tightened
    // mod-card width pushes the whole column further left, which
    // gives the sell strip below the ship room to clear the reroll
    // button without overlap.
    let canvas_half_w = CUSTOMIZE_INTERNAL_W as f32 * 0.5;
    let tile_gap = 4.0;
    let shop_x = -canvas_half_w + 4.0
        + (super::shop_mods::MOD_CARD_W * 1.5
            + super::shop_mods::MOD_CARD_GAP);
    // Drop the shop column further from the top edge so the SHOP header
    // sits clearly below the SCRAP counter (both top-left). The previous
    // y=76 placed SHOP at y=90 vs SCRAP at y=88 — they overlapped.
    let shop_top_y = (CUSTOMIZE_INTERNAL_H as f32) * 0.5 - 40.0;
    spawn_text(&mut commands, Vec2::new(shop_x, shop_top_y + 14.0), "SHOP", Color::srgb(1.0, 0.85, 0.30), 18.0, ShopHeaderTag);
    spawn_text(&mut commands, Vec2::new(shop_x, shop_top_y), "TURRETS", Color::srgb(0.55, 0.60, 0.70), 12.0, ShopHeaderTag);
    for idx in 0..3usize {
        let x = shop_x + (idx as f32 - 1.0) * (SHOP_TILE + tile_gap);
        let y = shop_top_y - 16.0;
        spawn_shop_turret_tile(&mut commands, &mut meshes, &mut materials, idx, Vec2::new(x, y));
    }
    // Vertical layout — each row leaves room for its tile body PLUS
    // its name + cost label below before the next section header. The
    // turret-cost label hangs at -38 from `shop_top_y`, so RUNES starts
    // 14 below that, etc. Bumping any of these requires checking the
    // labels (`spawn_shop_*_tile` apply offsets relative to the tile
    // pos) for overlap with the next row's header.
    spawn_text(&mut commands, Vec2::new(shop_x, shop_top_y - 52.0), "RUNES", Color::srgb(0.55, 0.60, 0.70), 12.0, ShopHeaderTag);
    for idx in 0..2usize {
        // Wider gap between the two rune sockets so the larger
        // name labels under them have room to breathe.
        let x = shop_x + (idx as f32 - 0.5) * (SOCKET + 22.0);
        let y = shop_top_y - 68.0;
        spawn_shop_rune_tile(&mut commands, &mut meshes, &mut materials, idx, Vec2::new(x, y));
    }

    // Stat-modifier cards — 3 click-to-buy options below the runes.
    // Header sits well above the card-row top so the taller mod
    // cards (now 36 spec px tall to fit trade-off labels) don't
    // creep up over the "MODS" text.
    spawn_text(&mut commands, Vec2::new(shop_x, shop_top_y - 92.0), "MODS", Color::srgb(0.55, 0.60, 0.70), 12.0, ShopHeaderTag);
    super::shop_mods::spawn_mod_cards(&mut commands, shop_x, shop_top_y - 122.0);

    // Reroll button — sits at the bottom of the shop column. Costs
    // `SHOP_REROLL_COST` scrap (`drag::SHOP_REROLL_COST`); refills every
    // sold slot with fresh offerings. Pushed further down so the
    // taller mod cards (centre at -122, half-height 18 → bottom at
    // -140) clear the reroll container.
    let reroll_pos = Vec2::new(shop_x, shop_top_y - 156.0);
    spawn_container(
        &mut commands,
        &mut meshes,
        &mut materials,
        reroll_pos,
        Vec2::new(48.0, 13.0),
        3.0,
        Color::srgb(0.22, 0.40, 0.26),
        Z_TILE_BG,
        ShopRerollBg,
    );
    spawn_text(
        &mut commands,
        reroll_pos,
        format!("REROLL {}", super::drag::SHOP_REROLL_COST),
        Color::WHITE,
        12.0,
        ShopRerollCostText,
    );
    commands.spawn((
        Transform::from_translation(reroll_pos.extend(Z_TILE_BG)),
        HitArea { size: Vec2::new(48.0, 13.0) },
        ShopRerollBtn,
        RenderLayers::layer(CUSTOMIZE_LAYER),
    ));

    // (Sell panel is spawned BELOW the ship — see further down, after
    // the ship + slot spawn block. The shop column no longer hosts it.)

    // ---------- Centre ship + slots + sockets ----------
    // Ship sits left of canvas centre so the RHS stats column has room
    // to flow down the right edge.
    let ship_centre = Vec2::new(-10.0, 0.0);
    spawn_hull(&mut commands, ship_centre, hull_capsule, hull_mat);

    for (slot, &(gx, gy)) in TURRET_POSITIONS.iter().enumerate() {
        // Rotate game (+Y bow) → spec (+X bow). 2D CW 90°: (x,y) → (y, -x).
        // Game port (-X) → spec +Y (top).
        let spec = Vec2::new(gy * SHIP_SCALE, -gx * SHIP_SCALE);
        let pos = ship_centre + spec;
        spawn_ship_slot(&mut commands, &mut meshes, &mut materials, slot, pos);
        spawn_rune_triplet_for_slot(&mut commands, &mut meshes, &mut materials, slot, pos);
    }

    // ---------- RHS live stats readout ----------
    // Right edge of the panel sits a few px in from the canvas edge;
    // top row begins below the CLOSE button.
    let stats_right_edge = canvas_half_w - 6.0;
    let stats_top_y = (CUSTOMIZE_INTERNAL_H as f32) * 0.5 - 28.0;
    super::stats_panel::spawn_stats_panel(&mut commands, stats_right_edge, stats_top_y);

    // ---------- Sell strip ----------
    // Stacked two-line block below the ship: bold "SELL" header on
    // top, gold "+N SCRAP" preview underneath. Taller than the
    // earlier one-line strip so both lines get readable padding;
    // both texts sit centred on the panel's X so the block reads as
    // a vertical pair.
    const SELL_PANEL_W: f32 = 90.0;
    const SELL_PANEL_H: f32 = 13.0;
    let sell_pos = Vec2::new(ship_centre.x, -75.0);
    let sell_hash_img = images.add(crate::rendering::make_hash_image_with_tile(
        Color::srgb(0.30, 0.32, 0.36), // light gray
        Color::srgb(0.16, 0.17, 0.20), // dark gray
        8,                             // 8-px tile → small diagonal stripes
    ));
    commands.spawn((
        Sprite {
            image: sell_hash_img,
            custom_size: Some(Vec2::new(SELL_PANEL_W, SELL_PANEL_H)),
            image_mode: bevy::sprite::SpriteImageMode::Tiled {
                tile_x: true,
                tile_y: true,
                stretch_value: 1.0,
            },
            ..default()
        },
        Transform::from_translation(sell_pos.extend(Z_TILE_BG)),
        // HitArea on the sprite entity itself, mirroring the drop
        // target footprint, so `update_sell_label` can locate the
        // strip via `Query<(&Transform, &HitArea), With<ShopSellPanel>>`
        // and detect cursor-over-strip for the preview swap. (The
        // drop target below carries its own HitArea for the drop
        // resolution path in `complete_drag` — they intentionally
        // duplicate so each system queries its own marker.)
        HitArea { size: Vec2::new(SELL_PANEL_W, SELL_PANEL_H) },
        RenderLayers::layer(CUSTOMIZE_LAYER),
        ShopSellPanel,
    ));
    // Static "SELL" label - centred on the strip. The "+N SCRAP"
    // preview spawns at the SAME position; `update_sell_label`
    // toggles their visibilities so only one is showing at a time
    // (SELL when idle, +N SCRAP while dragging a sellable over
    // the strip).
    spawn_text(
        &mut commands,
        sell_pos,
        "SELL",
        Color::WHITE,
        11.0,
        SellPanelLabel,
    );
    spawn_text(
        &mut commands,
        sell_pos,
        "",
        Color::srgb(1.00, 0.85, 0.30),
        11.0,
        SellPricePreview,
    );
    // Drop target hit area — DropTargetKind::Sell triggers the refund
    // path in `complete_drag`. Matches the strip footprint exactly.
    commands.spawn((
        Transform::from_translation(sell_pos.extend(Z_TILE_BG)),
        HitArea { size: Vec2::new(SELL_PANEL_W, SELL_PANEL_H) },
        DropTargetMarker(DropTargetKind::Sell),
        RenderLayers::layer(CUSTOMIZE_LAYER),
    ));

    super::tooltip::spawn_customize_tooltip(&mut commands, &pixel_font);
}

// ---------- Ancillary tags for misc text ----------

#[derive(Component)]
pub struct CloseLabelTag;
#[derive(Component)]
pub struct CloseHitTag;
#[derive(Component)]
pub struct ShopHeaderTag;

/// Marker on the centred static "SELL" text inside the sell panel.
/// Currently no consumer — kept as a marker so a future system can
/// re-style/animate the label without re-grepping.
#[derive(Component)]
pub struct SellPanelLabel;

/// Marker on the gold "+N" refund-preview text that sits just below
/// the sell panel. `update_sell_label` toggles it visible + sets the
/// number while the player drags a ship-sourced sellable; hidden in
/// every other state.
#[derive(Component)]
pub struct SellPricePreview;

/// Click target for the SHOP "REROLL" button.
#[derive(Component, Clone, Copy)]
pub struct ShopRerollBtn;
/// Container background for the reroll button (so the updater can dim
/// it when the player can't afford the cost).
#[derive(Component, Clone, Copy)]
pub struct ShopRerollBg;
/// Tag on the reroll cost label so the updater can colour it red when
/// the player is short on scrap.
#[derive(Component)]
pub struct ShopRerollCostText;

// ---------- Container helper (rounded square at low res) ----------

pub fn spawn_container<M: Component + Copy>(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ColorMaterial>,
    center: Vec2,
    size: Vec2,
    radius: f32,
    color: Color,
    z: f32,
    marker: M,
) -> Entity {
    let mat = materials.add(color);
    let circle = meshes.add(Circle::new(radius));
    let h_rect = meshes.add(Rectangle::new(size.x, (size.y - 2.0 * radius).max(0.0)));
    let v_rect = meshes.add(Rectangle::new((size.x - 2.0 * radius).max(0.0), size.y));

    let entity = commands.spawn((
        Mesh2d(h_rect),
        MeshMaterial2d(mat.clone()),
        Transform::from_translation(center.extend(z)),
        RenderLayers::layer(CUSTOMIZE_LAYER),
        marker,
    )).id();
    commands.spawn((
        Mesh2d(v_rect),
        MeshMaterial2d(mat.clone()),
        Transform::from_translation(center.extend(z)),
        RenderLayers::layer(CUSTOMIZE_LAYER),
        marker,
    ));
    let half = (size - Vec2::splat(2.0 * radius)).max(Vec2::ZERO) * 0.5;
    for offset in [
        Vec2::new(-half.x, -half.y),
        Vec2::new( half.x, -half.y),
        Vec2::new(-half.x,  half.y),
        Vec2::new( half.x,  half.y),
    ] {
        commands.spawn((
            Mesh2d(circle.clone()),
            MeshMaterial2d(mat.clone()),
            Transform::from_translation((center + offset).extend(z)),
            RenderLayers::layer(CUSTOMIZE_LAYER),
            marker,
        ));
    }
    entity
}

// ---------- Native-res text helper ----------

fn spawn_text<M: Component>(
    commands: &mut Commands,
    spec_pos: Vec2,
    text: impl Into<String>,
    color: Color,
    font_size: f32,
    marker: M,
) -> Entity {
    commands.spawn((
        Text2d::new(text),
        TextFont {
            font_size,
            font_smoothing: FontSmoothing::None,
            ..default()
        },
        TextColor(color),
        // Initial position; the per-frame sync system rewrites this from
        // `CustomizeTextSpec * viewport.display_scale`.
        Transform::from_xyz(0.0, 0.0, 100.0),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        CustomizeText,
        CustomizeTextSpec(spec_pos),
        marker,
    )).id()
}

// ---------- Hull ----------

fn spawn_hull(
    commands: &mut Commands,
    centre: Vec2,
    hull_capsule: Handle<Mesh>,
    hull_mat: Handle<ColorMaterial>,
) {
    commands.spawn((
        Mesh2d(hull_capsule),
        MeshMaterial2d(hull_mat),
        Transform::from_translation(centre.extend(Z_HULL))
            .with_rotation(Quat::from_rotation_z(-std::f32::consts::FRAC_PI_2)),
        RenderLayers::layer(CUSTOMIZE_LAYER),
    ));
}

// ---------- Ship slots (in-game style: Circle base + thin barrels) ----------

fn spawn_ship_slot(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ColorMaterial>,
    slot: usize,
    pos: Vec2,
) {
    // Base — a Circle, just like the in-game turret base.
    let base_mesh = meshes.add(Circle::new(TURRET_BASE_R));
    let base_mat = materials.add(empty_slot_color());
    commands.spawn((
        Mesh2d(base_mesh),
        MeshMaterial2d(base_mat),
        Transform::from_translation(pos.extend(Z_TILE_BG)),
        RenderLayers::layer(CUSTOMIZE_LAYER),
        ShipSlotBase { slot },
    ));

    // Barrel level — single number centred on the turret. Native-res
    // text so it stays sharp; updater rewrites the digit from `cfg.barrels`.
    spawn_text(
        commands,
        pos,
        "1",
        Color::WHITE,
        14.0,
        ShipSlotBadgeText { slot },
    );

    // Hit area + drag/drop markers.
    commands.spawn((
        Transform::from_translation(pos.extend(Z_TILE_BG)),
        HitArea { size: Vec2::splat(TURRET_BASE_R * 2.0 + 4.0) },
        ShipSlotButton,
        DragSourceMarker(DragSourceKind::ShipSlot(slot)),
        DropTargetMarker(DropTargetKind::ShipSlot(slot)),
        RenderLayers::layer(CUSTOMIZE_LAYER),
    ));
}

// ---------- Rune sockets (around the ship, not on it) ----------

#[derive(Clone, Copy)]
enum SocketSide {
    Above,
    Below,
    Left,
    Right,
}

fn socket_side_for(slot: usize) -> SocketSide {
    match slot {
        0 => SocketSide::Right, // bow
        7 => SocketSide::Left,  // stern
        1 | 3 | 5 => SocketSide::Above,
        2 | 4 | 6 => SocketSide::Below,
        _ => SocketSide::Above,
    }
}

fn spawn_rune_triplet_for_slot(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ColorMaterial>,
    slot: usize,
    pos: Vec2,
) {
    let side = socket_side_for(slot);
    for rune_idx in 0..3usize {
        let (sx, sy) = socket_offset(side, rune_idx);
        let p = pos + Vec2::new(sx, sy);
        spawn_socket_container(commands, meshes, materials, slot, rune_idx, p, empty_socket_color());
        commands.spawn((
            Transform::from_translation(p.extend(Z_TILE_BG)),
            HitArea { size: Vec2::splat(SOCKET + 2.0) },
            ShipRuneSocket,
            DragSourceMarker(DragSourceKind::ShipRune { slot, rune_idx }),
            DropTargetMarker(DropTargetKind::ShipRune { slot, rune_idx }),
            RenderLayers::layer(CUSTOMIZE_LAYER),
        ));
    }
}

/// Stack the triplet *outward* from the turret instead of along its
/// neighbouring turret's axis: column going further up/down for wing
/// turrets, row extending further out for bow/stern. Index 0 is the
/// nearest socket; 2 is the furthest.
fn socket_offset(side: SocketSide, rune_idx: usize) -> (f32, f32) {
    let perp = SOCKET_OFFSET + SOCKET * 0.5;
    let stack = rune_idx as f32 * (SOCKET + SOCKET_GAP); // 0, +1, +2 outward
    let dist = perp + stack;
    match side {
        SocketSide::Above => (0.0, dist),
        SocketSide::Below => (0.0, -dist),
        SocketSide::Left => (-dist, 0.0),
        SocketSide::Right => (dist, 0.0),
    }
}

fn spawn_socket_container(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ColorMaterial>,
    slot: usize,
    rune_idx: usize,
    pos: Vec2,
    color: Color,
) {
    let mat = materials.add(color);
    let circle = meshes.add(Circle::new(SOCKET_RADIUS));
    let h_rect = meshes.add(Rectangle::new(SOCKET, SOCKET - 2.0 * SOCKET_RADIUS));
    let v_rect = meshes.add(Rectangle::new(SOCKET - 2.0 * SOCKET_RADIUS, SOCKET));

    for mesh in [Mesh2d(h_rect), Mesh2d(v_rect)] {
        commands.spawn((
            mesh,
            MeshMaterial2d(mat.clone()),
            Transform::from_translation(pos.extend(Z_TILE_BG)),
            RenderLayers::layer(CUSTOMIZE_LAYER),
            ShipRuneSocketPart { slot, rune_idx },
        ));
    }
    let half = (SOCKET - 2.0 * SOCKET_RADIUS) * 0.5;
    for offset in [
        Vec2::new(-half, -half),
        Vec2::new( half, -half),
        Vec2::new(-half,  half),
        Vec2::new( half,  half),
    ] {
        commands.spawn((
            Mesh2d(circle.clone()),
            MeshMaterial2d(mat.clone()),
            Transform::from_translation((pos + offset).extend(Z_TILE_BG)),
            RenderLayers::layer(CUSTOMIZE_LAYER),
            ShipRuneSocketPart { slot, rune_idx },
        ));
    }

    // Red X overlay marking sockets blocked by targeting-rune
    // exclusivity. Two diagonal bars (45° and -45°) form the cross.
    // Initially Hidden — toggled visible by `update_customize_ui`
    // when a sibling socket holds a targeting rune.
    //
    // Bar geometry kept INSIDE the rounded 8×8 socket: length 6,
    // thickness 1.2. At 45° the endpoints sit ~2.1 from the centre
    // along each axis, well within the socket's 4-half-extent.
    let bar_mesh = meshes.add(Rectangle::new(6.0, 1.2));
    let cross_mat = materials.add(Color::srgb(0.90, 0.30, 0.30));
    for angle in [std::f32::consts::FRAC_PI_4, -std::f32::consts::FRAC_PI_4] {
        commands.spawn((
            Mesh2d(bar_mesh.clone()),
            MeshMaterial2d(cross_mat.clone()),
            Transform {
                translation: pos.extend(Z_TILE_FG),
                rotation: Quat::from_rotation_z(angle),
                ..default()
            },
            Visibility::Hidden,
            RenderLayers::layer(CUSTOMIZE_LAYER),
            ShipRuneSocketLockHash { slot, rune_idx },
        ));
    }
}

// ---------- Shop turret + rune tiles ----------

fn spawn_shop_turret_tile(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ColorMaterial>,
    idx: usize,
    pos: Vec2,
) {
    // Chunky rounded-square card body.
    spawn_shop_tile_container(commands, meshes, materials, idx, pos, empty_slot_color());

    // Inner turret circle (no barrels — barrel level is shown as a
    // number on the turret instead).
    spawn_shop_turret_silhouette(commands, meshes, materials, pos);

    // Centred number on the inner turret = barrel level.
    spawn_text(
        commands,
        pos,
        "1",
        Color::WHITE,
        14.0,
        ShopTurretBadgeText { idx },
    );

    // Cost label sits IMMEDIATELY below the tile so the player's
    // eye doesn't have to walk past the name to find the price.
    // Updater clears it when the slot is sold or being dragged out.
    spawn_text(
        commands,
        pos + Vec2::new(0.0, -SHOP_TILE * 0.5 - 6.0),
        format!("{}", super::drag::SHOP_TURRET_COST),
        Color::srgb(1.0, 0.85, 0.30),
        13.0,
        ShopTurretCostText { idx },
    );
    // Name label below the cost (still beneath the tile, just one
    // line further down).
    spawn_text(
        commands,
        pos + Vec2::new(0.0, -SHOP_TILE * 0.5 - 16.0),
        "---",
        Color::WHITE,
        13.0,
        ShopTurretNameText { idx },
    );

    // AOE badge — top-right corner of the card. Hidden by default;
    // updater flips visibility based on stocked weapon (= Mortar).
    let tag_spec = pos + Vec2::new(SHOP_TILE * 0.5 - 7.0, SHOP_TILE * 0.5 - 4.0);
    commands.spawn((
        Sprite {
            color: AOE_TAG_COLOR,
            custom_size: Some(AOE_TAG_SIZE),
            ..default()
        },
        Transform::from_xyz(0.0, 0.0, Z_AOE_TAG),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        ShopTurretAoeTag { idx, spec_pos: tag_spec },
    ));
    commands.spawn((
        Text2d::new("AOE"),
        TextFont {
            font_size: 7.0,
            font_smoothing: FontSmoothing::None,
            ..default()
        },
        TextColor(Color::srgb(0.10, 0.05, 0.02)),
        Transform::from_xyz(0.0, 0.0, Z_AOE_TAG_TEXT),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        ShopTurretAoeTagText { idx, spec_pos: tag_spec },
    ));

    commands.spawn((
        Transform::from_translation(pos.extend(Z_TILE_BG)),
        HitArea { size: Vec2::splat(SHOP_TILE) },
        ShopTurretSlot,
        DragSourceMarker(DragSourceKind::ShopTurret(idx)),
        RenderLayers::layer(CUSTOMIZE_LAYER),
    ));
}

fn spawn_shop_tile_container(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ColorMaterial>,
    idx: usize,
    pos: Vec2,
    color: Color,
) {
    let mat = materials.add(color);
    let r = SHOP_TILE_RADIUS;
    let circle = meshes.add(Circle::new(r));
    let h_rect = meshes.add(Rectangle::new(SHOP_TILE, SHOP_TILE - 2.0 * r));
    let v_rect = meshes.add(Rectangle::new(SHOP_TILE - 2.0 * r, SHOP_TILE));
    for mesh in [Mesh2d(h_rect), Mesh2d(v_rect)] {
        commands.spawn((
            mesh,
            MeshMaterial2d(mat.clone()),
            Transform::from_translation(pos.extend(Z_TILE_BG)),
            RenderLayers::layer(CUSTOMIZE_LAYER),
            ShopTurretVisual { idx },
        ));
    }
    let half = (SHOP_TILE - 2.0 * r) * 0.5;
    for offset in [
        Vec2::new(-half, -half),
        Vec2::new( half, -half),
        Vec2::new(-half,  half),
        Vec2::new( half,  half),
    ] {
        commands.spawn((
            Mesh2d(circle.clone()),
            MeshMaterial2d(mat.clone()),
            Transform::from_translation((pos + offset).extend(Z_TILE_BG)),
            RenderLayers::layer(CUSTOMIZE_LAYER),
            ShopTurretVisual { idx },
        ));
    }
}

/// Spawn the inner turret circle inside a shop card. Barrel level is
/// rendered as a number centred on this circle (in `spawn_shop_turret_tile`),
/// so no barrel rectangles here.
fn spawn_shop_turret_silhouette(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ColorMaterial>,
    pos: Vec2,
) {
    let base_mesh = meshes.add(Circle::new(SHOP_TURRET_BASE_R));
    let base_mat = materials.add(empty_slot_color());
    commands.spawn((
        Mesh2d(base_mesh),
        MeshMaterial2d(base_mat),
        Transform::from_translation((pos).extend(Z_TILE_FG - 0.05)),
        RenderLayers::layer(CUSTOMIZE_LAYER),
        ShopTurretBase,
    ));
}

fn spawn_shop_rune_tile(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ColorMaterial>,
    idx: usize,
    pos: Vec2,
) {
    let mat = materials.add(empty_socket_color());
    let r = SOCKET_RADIUS;
    let circle = meshes.add(Circle::new(r));
    let h_rect = meshes.add(Rectangle::new(SOCKET, SOCKET - 2.0 * r));
    let v_rect = meshes.add(Rectangle::new(SOCKET - 2.0 * r, SOCKET));
    for mesh in [Mesh2d(h_rect), Mesh2d(v_rect)] {
        commands.spawn((
            mesh,
            MeshMaterial2d(mat.clone()),
            Transform::from_translation(pos.extend(Z_TILE_BG)),
            RenderLayers::layer(CUSTOMIZE_LAYER),
            ShopRuneVisual { idx },
        ));
    }
    let half = (SOCKET - 2.0 * r) * 0.5;
    for offset in [
        Vec2::new(-half, -half),
        Vec2::new( half, -half),
        Vec2::new(-half,  half),
        Vec2::new( half,  half),
    ] {
        commands.spawn((
            Mesh2d(circle.clone()),
            MeshMaterial2d(mat.clone()),
            Transform::from_translation((pos + offset).extend(Z_TILE_BG)),
            RenderLayers::layer(CUSTOMIZE_LAYER),
            ShopRuneVisual { idx },
        ));
    }
    // Rune NAME sits immediately below the socket - the player
    // reads the rune kind first, the cost second. Bigger font so
    // the name reads at a glance now that there's more horizontal
    // breathing room between sockets.
    spawn_text(
        commands,
        pos + Vec2::new(0.0, -SOCKET * 0.5 - 7.0),
        "---",
        Color::WHITE,
        15.0,
        ShopRuneNameText { idx },
    );
    // Cost label below the name. Gold accent.
    spawn_text(
        commands,
        pos + Vec2::new(0.0, -SOCKET * 0.5 - 18.0),
        format!("{}", super::drag::SHOP_ITEM_COST),
        Color::srgb(1.0, 0.85, 0.30),
        12.0,
        ShopRuneCostText { idx },
    );

    // AOE badge — perched above the (smaller) rune socket and skewed
    // to its right edge to mirror the turret card's top-right placement.
    // Sits clear of both the socket body and the neighbouring socket
    // (sockets are spaced `SOCKET + 6.0` apart, so `+1` overlap from the
    // tag's right edge at the next socket's left edge is acceptable —
    // the badge is well above the socket vertically). Hidden by default;
    // updater flips visibility based on stocked rune (= Splash).
    let tag_spec = pos + Vec2::new(SOCKET * 0.5 - 3.0, SOCKET * 0.5 + 5.0);
    commands.spawn((
        Sprite {
            color: AOE_TAG_COLOR,
            custom_size: Some(AOE_TAG_SIZE),
            ..default()
        },
        Transform::from_xyz(0.0, 0.0, Z_AOE_TAG),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        ShopRuneAoeTag { idx, spec_pos: tag_spec },
    ));
    commands.spawn((
        Text2d::new("AOE"),
        TextFont {
            font_size: 7.0,
            font_smoothing: FontSmoothing::None,
            ..default()
        },
        TextColor(Color::srgb(0.10, 0.05, 0.02)),
        Transform::from_xyz(0.0, 0.0, Z_AOE_TAG_TEXT),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        ShopRuneAoeTagText { idx, spec_pos: tag_spec },
    ));

    commands.spawn((
        Transform::from_translation(pos.extend(Z_TILE_BG)),
        HitArea { size: Vec2::splat(SOCKET + 4.0) },
        ShopRuneSlot,
        DragSourceMarker(DragSourceKind::ShopRune(idx)),
        RenderLayers::layer(CUSTOMIZE_LAYER),
    ));
}

// ---------- Per-frame text positioning ----------

/// Sync each customize text entity's world position AND visual scale
/// each frame, and toggle visibility based on `CustomizeOpen`. Cheap
/// (~50 entities) and unconditional.
///
/// Two scaling factors are at play here and they're *different*:
///
/// - **Position** uses `viewport.display_scale` (~4 at 1280×800). The
///   text's `CustomizeTextSpec` is authored in customize-internal
///   pixels (320×200 canvas); multiplying by `display_scale` puts the
///   text inside the on-screen rect of the customize sprite.
/// - **Glyph size** uses `UiScale` (1.0 at 1280×800). Customize text
///   lives on `UPSCALE_LAYER`, whose camera is `WindowSize` (1 world
///   unit = 1 screen pixel), so `font_size: 16.0` would render at 16
///   *screen* pixels regardless of window — which doesn't scale at
///   all. Using `display_scale` for the visual scale instead (the
///   first version of this fix) blew text up to 64 screen pixels on
///   the design window, way too big. `UiScale` matches the bevy_ui
///   chrome's scaling — 16 design pixels read consistently across
///   resolutions.
pub fn sync_customize_text(
    open: Res<super::CustomizeOpen>,
    viewport: Res<super::render::CustomizeViewport>,
    ui_scale: Res<bevy::ui::UiScale>,
    mut q: Query<(&CustomizeTextSpec, &mut Transform, &mut Visibility), With<CustomizeText>>,
) {
    let want = if open.open { Visibility::Inherited } else { Visibility::Hidden };
    for (spec, mut tf, mut vis) in &mut q {
        if *vis != want {
            *vis = want;
        }
        if open.open {
            let s = viewport.display_scale;
            tf.translation.x = spec.0.x * s;
            tf.translation.y = spec.0.y * s;
            let scale = ui_scale.0;
            let want_scale = Vec3::new(scale, scale, 1.0);
            if tf.scale != want_scale { tf.scale = want_scale; }
        }
    }
}

// ---------- Colour helpers (shared) ----------

pub fn empty_slot_color() -> Color {
    Color::srgb(0.20, 0.23, 0.30)
}

pub fn empty_socket_color() -> Color {
    // Brighter than the empty slot — empty sockets need to read as
    // "deliberate slot here, drag a rune onto me" rather than blending
    // into the dark backdrop. Picks out as a warmer steel against the
    // hull's cool blue-grey.
    Color::srgb(0.42, 0.45, 0.52)
}

pub fn turret_color_for(weapon: WeaponType) -> Color {
    match weapon {
        WeaponType::Standard => Color::srgb(0.34, 0.42, 0.52),
        WeaponType::Sniper => hex(SNIPER_HEX),
        WeaponType::MachineGun => hex(MG_HEX),
        WeaponType::Shotgun => hex(SHOTGUN_HEX),
        WeaponType::Railgun => hex(RAILGUN_HEX),
        WeaponType::Mortar => hex(MORTAR_HEX),
        // HeliPad reads as army-green deck pad — same hue as the
        // helicopter that launches from it.
        WeaponType::HeliPad => hex(crate::palette::HELIPAD_DECK_HEX),
        WeaponType::Cannon => hex(crate::palette::CANNON_HEX),
        WeaponType::Booster => hex(crate::palette::BOOSTER_HEX),
        WeaponType::Blade => hex(crate::palette::BLADE_HEX),
        WeaponType::Cage => hex(crate::palette::CAGE_HEX),
        WeaponType::Harpoon => hex(crate::palette::HARPOON_HEX),
        WeaponType::SpreadRockets => hex(crate::palette::SPREAD_ROCKETS_HEX),
        WeaponType::Flamethrower => hex(crate::palette::FLAMETHROWER_HEX),
        WeaponType::SpikedPlate => hex(crate::palette::SPIKED_PLATE_HEX),
        WeaponType::Amplifier => Color::srgb(0.45, 0.78, 0.82),
        WeaponType::SharkNet => Color::srgb(0.18, 0.32, 0.55),
        WeaponType::AnchorFlail => Color::srgb(0.32, 0.34, 0.38),
        WeaponType::PlasmaTorpedo => Color::srgb(0.34, 0.18, 0.55),
        WeaponType::CrowsNest => Color::srgb(0.36, 0.24, 0.16),
    }
}

pub fn turret_barrel_color_for(weapon: WeaponType) -> Color {
    match weapon {
        WeaponType::Standard => Color::srgb(0.78, 0.84, 0.90),
        WeaponType::Sniper => hex("#ff70d4"),
        WeaponType::MachineGun => hex("#6bd5ff"),
        WeaponType::Shotgun => hex("#ff7080"),
        WeaponType::Railgun => hex("#5cf2e8"),
        WeaponType::Mortar => hex(MORTAR_BRIGHT_HEX),
        WeaponType::HeliPad => hex(crate::palette::HELIPAD_DECK_HEX),
        // Brass — readable on both the dark shop background AND the
        // wood-brown cannon base. The bullet's true cannonball colour
        // (`CANNON_BRIGHT_HEX`, near-black iron) stays for the
        // projectile itself; this brass colour is purely for the
        // customize-screen barrel/label rendering.
        WeaponType::Cannon => hex("#e8b060"),
        WeaponType::Booster => hex(crate::palette::BOOSTER_BRIGHT_HEX),
        WeaponType::Blade => hex(crate::palette::BLADE_BRIGHT_HEX),
        WeaponType::Cage => hex(crate::palette::OCTOPUS_BODY_HEX),
        WeaponType::Harpoon => hex(crate::palette::HARPOON_BRIGHT_HEX),
        // Bright steel barrel highlight for the rocket rack.
        WeaponType::SpreadRockets => Color::srgb(0.92, 0.96, 1.0),
        // Hot-orange glow on the nozzle tip — same hue family as
        // the Fire rune so the weapon reads as a burner at a glance.
        WeaponType::Flamethrower => hex(crate::palette::FIRE_HEX),
        // Bright steel for the spike tips on top of the plate.
        WeaponType::SpikedPlate => hex(crate::palette::SPIKED_PLATE_TIP_HEX),
        // Brighter teal — pops on the pad colour without bleeding
        // into Booster's amber-gold pulse ring.
        WeaponType::Amplifier => Color::srgb(0.70, 0.95, 0.98),
        // Pale steel — the visible shark barrel against the
        // deep-ocean base.
        WeaponType::SharkNet => Color::srgb(0.85, 0.92, 1.00),
        // Polished steel highlight for the anchor's iron.
        WeaponType::AnchorFlail => Color::srgb(0.78, 0.82, 0.86),
        // Bright cyan-violet plasma glow.
        WeaponType::PlasmaTorpedo => Color::srgb(0.85, 0.95, 1.00),
        // Sun-faded plank highlight for the lookout platform.
        WeaponType::CrowsNest => Color::srgb(0.72, 0.54, 0.32),
    }
}

pub fn rune_color_for(rune: Rune) -> Color {
    use crate::palette::{BLEED_HEX, FIRE_HEX, FROST_HEX, SHOCK_HEX};
    match rune {
        Rune::Fire             => hex(FIRE_HEX),
        Rune::Frost            => hex(FROST_HEX),
        Rune::Shock            => hex(SHOCK_HEX),
        Rune::Echo             => Color::srgb(0.65, 0.40, 0.95),
        Rune::Cascade          => Color::srgb(0.45, 0.85, 0.50),
        Rune::Conduit          => Color::srgb(0.95, 0.40, 0.75),
        Rune::Resonate         => Color::srgb(0.95, 0.80, 0.45),
        Rune::Vampire          => Color::srgb(0.70, 0.10, 0.20), // crimson lifesteal
        Rune::Ward             => Color::srgb(0.65, 0.90, 0.98), // pale cyan shield
        Rune::Bleed            => hex(BLEED_HEX),                // dark blood red
        // Targeting runes: cool/neutral palette so they read as
        // "modifier" rather than "elemental".
        Rune::TargetFurthest   => Color::srgb(0.50, 0.30, 0.80), // long-range purple
        Rune::TargetHighestHp  => Color::srgb(0.85, 0.30, 0.30), // big-target red
        Rune::TargetLowestHp   => Color::srgb(0.30, 0.85, 0.40), // execute green
        Rune::TargetCarousel   => Color::srgb(0.95, 0.85, 0.35), // rotating gold
        Rune::Splash           => Color::srgb(0.95, 0.55, 0.20), // explosive orange
        Rune::Blast            => Color::srgb(1.00, 0.42, 0.10), // hotter blast orange
        Rune::Hustle           => Color::srgb(0.45, 0.85, 0.40), // autonomous-speed green
        Rune::Pierce           => Color::srgb(0.85, 0.85, 0.92), // bullet-trail silver
        Rune::Greed            => Color::srgb(0.95, 0.78, 0.20), // gold-scrap
        Rune::Executioner      => Color::srgb(0.55, 0.10, 0.10), // dark blood red
        Rune::Opener           => Color::srgb(0.95, 0.95, 0.55), // first-strike yellow
        Rune::Leftovers        => Color::srgb(0.40, 0.85, 0.55), // medkit green
        Rune::Star             => Color::srgb(0.95, 0.90, 0.40), // sparkle gold
        Rune::Thirst           => Color::srgb(0.75, 0.20, 0.45), // hungry magenta
        Rune::Medic            => Color::srgb(0.95, 0.85, 0.90), // medical pink
        Rune::Rally            => Color::srgb(1.00, 0.55, 0.20), // war-cry orange
        Rune::Thorns           => Color::srgb(0.40, 0.55, 0.35), // briar green
    }
}

