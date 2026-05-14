//! Per-frame visual sync for the customize overlay.
//!
//! Walks each tagged primitive and rewrites its `MeshMaterial2d` /
//! `Visibility` / `Transform` from the live `TurretConfig` /
//! `CustomizeShop` / `Scrap` resources. Cheap; gated on `CustomizeOpen`.

use bevy::input::mouse::MouseButton;
use bevy::prelude::*;
use bevy::sprite::MeshMaterial2d;

use crate::turret::TurretConfig;
use crate::weapon::WeaponType;
use crate::Scrap;

use super::drag::{
    roll_fresh_stock, CustomizeShop, DragSourceKind, DragState, SHOP_ITEM_COST,
    SHOP_REROLL_COST,
};
use super::setup::{
    empty_slot_color, empty_socket_color, rune_color_for, turret_barrel_color_for,
    turret_color_for, CustomizeScrapText, DragSourceMarker, SellPricePreview,
    ShipRuneSocketLockHash, ShipRuneSocketPart, ShipSlotBadgeText,
    ShipSlotBase, ShopRerollBg, ShopRerollBtn, ShopRerollCostText, ShopRuneAoeTag,
    ShopRuneAoeTagText, ShopRuneCostText, ShopRuneNameText, ShopRuneVisual,
    ShopTurretAoeTag, ShopTurretAoeTagText, ShopTurretBadgeText, ShopTurretBase,
    ShopTurretCostText, ShopTurretNameText, ShopTurretVisual,
};
use crate::rune::Rune;
use super::CustomizeOpen;

pub fn update_customize_ui(
    open: Res<CustomizeOpen>,
    scrap: Res<Scrap>,
    mut scrap_q: Query<&mut Text2d, With<CustomizeScrapText>>,
) {
    if !open.open {
        return;
    }
    let s = format!("SCRAP {}", scrap.0);
    for mut text in &mut scrap_q {
        if text.0 != s {
            text.0 = s.clone();
        }
    }
}

pub fn update_customize_ship(
    open: Res<CustomizeOpen>,
    cfg: Res<TurretConfig>,
    drag: Res<DragState>,
    shop: Option<Res<CustomizeShop>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    bases: Query<(&ShipSlotBase, &MeshMaterial2d<ColorMaterial>)>,
    socket_parts: Query<(&ShipRuneSocketPart, &MeshMaterial2d<ColorMaterial>)>,
    mut hash_overlays: Query<(&ShipRuneSocketLockHash, &mut Visibility)>,
    mut badge_texts: Query<(&ShipSlotBadgeText, &mut Text2d)>,
    drag_sources: Query<(&Transform, &super::setup::HitArea, &DragSourceMarker)>,
) {
    if !open.open {
        return;
    }

    // While the player drags a ship turret/rune, render its source as
    // empty — the ghost is what represents the payload. Snaps back when
    // the drag completes (invalid drop = no state change → next frame
    // re-reads cfg and the source visual reappears).
    let dragged_slot = match drag.picked.as_ref().map(|p| p.source) {
        Some(DragSourceKind::ShipSlot(s)) => Some(s),
        _ => None,
    };
    let dragged_socket = match drag.picked.as_ref().map(|p| p.source) {
        Some(DragSourceKind::ShipRune { slot, rune_idx }) => Some((slot, rune_idx)),
        _ => None,
    };

    // Ship turret base colours.
    for (base, mat_handle) in &bases {
        let s = cfg.slots[base.slot];
        let dragged = dragged_slot == Some(base.slot);
        let want = if !dragged && s.equipped {
            turret_color_for(s.weapon)
        } else {
            empty_slot_color()
        };
        if let Some(mat) = materials.get_mut(&mat_handle.0) {
            if mat.color != want {
                mat.color = want;
            }
        }
    }

    // Rune sockets — colour by the equipped rune (or empty tint). A
    // socket whose rune is being dragged renders as empty; sockets
    // on a turret-being-dragged also empty out (the whole slot is
    // "in transit"). Empty sockets get a diagonal hash overlay
    // (light + dark red stripes, spawned in `spawn_socket_container`)
    // when the slot already holds a targeting rune in another socket
    // — targeting-rune exclusivity: one per weapon. The base material
    // stays the regular empty-socket tint so the hash reads cleanly.
    //
    // The hash is gated on the player *currently interacting with a
    // targeting rune*: either actively dragging one, or hovering one
    // in the shop. Otherwise the warning is irrelevant noise on every
    // slot that already holds a targeting rune.
    let active_rune = active_targeting_rune(
        &drag,
        shop.as_deref(),
        &cfg,
        &drag_sources,
    );
    let lock_state = |slot_idx: usize, rune_idx: usize| -> bool {
        if active_rune.is_none() { return false; }
        let s = cfg.slots[slot_idx];
        if !s.equipped { return false; }
        if dragged_slot == Some(slot_idx) { return false; }
        if dragged_socket == Some((slot_idx, rune_idx)) { return false; }
        if s.runes[rune_idx].is_some() { return false; }
        s.runes.iter().enumerate().any(|(i, r)| {
            i != rune_idx && r.map_or(false, |rune| rune.is_targeting())
        })
    };
    for (part, mat_handle) in &socket_parts {
        let s = cfg.slots[part.slot];
        let slot_dragged = dragged_slot == Some(part.slot);
        let socket_dragged = dragged_socket == Some((part.slot, part.rune_idx));
        let want = if !s.equipped || slot_dragged || socket_dragged {
            empty_socket_color()
        } else {
            match s.runes[part.rune_idx] {
                None => empty_socket_color(),
                Some(r) => rune_color_for(r),
            }
        };
        if let Some(mat) = materials.get_mut(&mat_handle.0) {
            if mat.color != want {
                mat.color = want;
            }
        }
    }
    for (hash, mut vis) in &mut hash_overlays {
        let want = if lock_state(hash.slot, hash.rune_idx) {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *vis != want { *vis = want; }
    }

    // Barrel-level number on each slot — single digit centred on the
    // turret circle. Blank when the slot is empty or being dragged so
    // the turret reads as "not present".
    for (text_marker, mut text) in &mut badge_texts {
        let s = cfg.slots[text_marker.slot];
        let dragged = dragged_slot == Some(text_marker.slot);
        let want = if s.equipped && !dragged {
            s.barrels.max(1).to_string()
        } else {
            String::new()
        };
        if text.0 != want {
            text.0 = want;
        }
    }

}

pub fn update_customize_shop(
    open: Res<CustomizeOpen>,
    shop: Option<Res<CustomizeShop>>,
    drag: Res<DragState>,
    viewport: Res<super::render::CustomizeViewport>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    shop_turrets: Query<(&ShopTurretVisual, &MeshMaterial2d<ColorMaterial>)>,
    shop_bases: Query<(&ShopTurretBase, &MeshMaterial2d<ColorMaterial>)>,
    shop_runes: Query<(&ShopRuneVisual, &MeshMaterial2d<ColorMaterial>)>,
    mut shop_badge_texts: Query<
        (&ShopTurretBadgeText, &mut Text2d),
        (
            Without<ShopTurretNameText>,
            Without<ShopRuneNameText>,
            Without<ShopTurretCostText>,
            Without<ShopRuneCostText>,
            Without<ShopTurretAoeTagText>,
            Without<ShopRuneAoeTagText>,
        ),
    >,
    mut shop_name_texts: Query<
        (&ShopTurretNameText, &mut Text2d, &mut TextColor),
        (
            Without<ShopTurretBadgeText>,
            Without<ShopRuneNameText>,
            Without<ShopTurretCostText>,
            Without<ShopRuneCostText>,
            Without<ShopTurretAoeTagText>,
            Without<ShopRuneAoeTagText>,
        ),
    >,
    mut shop_rune_name_texts: Query<
        (&ShopRuneNameText, &mut Text2d, &mut TextColor),
        (
            Without<ShopTurretBadgeText>,
            Without<ShopTurretNameText>,
            Without<ShopTurretCostText>,
            Without<ShopRuneCostText>,
            Without<ShopTurretAoeTagText>,
            Without<ShopRuneAoeTagText>,
        ),
    >,
    mut shop_turret_cost_texts: Query<
        (&ShopTurretCostText, &mut Text2d),
        (
            Without<ShopTurretBadgeText>,
            Without<ShopTurretNameText>,
            Without<ShopRuneNameText>,
            Without<ShopRuneCostText>,
            Without<ShopTurretAoeTagText>,
            Without<ShopRuneAoeTagText>,
        ),
    >,
    mut shop_rune_cost_texts: Query<
        (&ShopRuneCostText, &mut Text2d),
        (
            Without<ShopTurretBadgeText>,
            Without<ShopTurretNameText>,
            Without<ShopRuneNameText>,
            Without<ShopTurretCostText>,
            Without<ShopTurretAoeTagText>,
            Without<ShopRuneAoeTagText>,
        ),
    >,
    // AOE badge sprites (parents) — combined turret + rune via
    // `Or` filter to keep the system within Bevy's tuple-arg limit.
    // Each entity has exactly one of the two markers, so the
    // `Option<...>` reads cleanly fork on which branch we're in.
    mut shop_aoe_tag_sprites: Query<
        (
            Option<&ShopTurretAoeTag>,
            Option<&ShopRuneAoeTag>,
            &mut Sprite,
            &mut Transform,
            &mut Visibility,
        ),
        (
            Or<(With<ShopTurretAoeTag>, With<ShopRuneAoeTag>)>,
            Without<ShopTurretAoeTagText>,
            Without<ShopRuneAoeTagText>,
        ),
    >,
    // AOE badge "AOE" text labels (siblings) — owned visibility, not
    // `CustomizeText`, since the shared sync would force-show them on
    // every card whenever the shop is open. Combined with `Or`
    // for the same arg-budget reason.
    mut shop_aoe_tag_texts: Query<
        (
            Option<&ShopTurretAoeTagText>,
            Option<&ShopRuneAoeTagText>,
            &mut Text2d,
            &mut Transform,
            &mut Visibility,
        ),
        (
            Or<(With<ShopTurretAoeTagText>, With<ShopRuneAoeTagText>)>,
            Without<ShopTurretBadgeText>,
            Without<ShopTurretNameText>,
            Without<ShopRuneNameText>,
            Without<ShopTurretCostText>,
            Without<ShopRuneCostText>,
            Without<ShopTurretAoeTag>,
            Without<ShopRuneAoeTag>,
        ),
    >,
) {
    // When the shop closes (or hasn't been initialised yet) hide every
    // AOE tag entity. Other shop visuals also "go stale" but they're
    // owned by the customize root's `Visibility::Inherited` cascade —
    // the AOE tags live on UPSCALE_LAYER without a parent in that tree
    // so we have to flip their visibility ourselves.
    if !open.open || shop.is_none() {
        for (_, _, _, _, mut v) in &mut shop_aoe_tag_sprites {
            if *v != Visibility::Hidden { *v = Visibility::Hidden; }
        }
        for (_, _, _, _, mut v) in &mut shop_aoe_tag_texts {
            if *v != Visibility::Hidden { *v = Visibility::Hidden; }
        }
        if !open.open {
            return;
        }
    }
    let Some(shop) = shop.as_deref() else { return };

    // Treat the source as empty while it's being dragged out.
    let dragged_shop_turret = match drag.picked.as_ref().map(|p| p.source) {
        Some(DragSourceKind::ShopTurret(idx)) => Some(idx),
        _ => None,
    };
    let dragged_shop_rune = match drag.picked.as_ref().map(|p| p.source) {
        Some(DragSourceKind::ShopRune(idx)) => Some(idx),
        _ => None,
    };

    // Card body colour (matches weapon hue so the SNKRX-style card reads
    // as the weapon's identity, not a neutral container). Empty/sold/
    // dragged slots fall back to the dim slot colour.
    for (vis_marker, mat_handle) in &shop_turrets {
        let dragged = dragged_shop_turret == Some(vis_marker.idx);
        let want = if dragged {
            empty_slot_color()
        } else {
            shop
                .turrets
                .get(vis_marker.idx)
                .and_then(|o| o.as_ref())
                .map(|o| turret_color_for(o.weapon))
                .unwrap_or(empty_slot_color())
        };
        if let Some(mat) = materials.get_mut(&mat_handle.0) {
            if mat.color != want {
                mat.color = want;
            }
        }
    }

    // Inner turret-base disc — keep it darker than the card so the
    // silhouette reads. Use the slot's empty colour as a neutral.
    for (_base, mat_handle) in &shop_bases {
        let want = empty_slot_color();
        if let Some(mat) = materials.get_mut(&mat_handle.0) {
            if mat.color != want {
                mat.color = want;
            }
        }
    }

    for (vis_marker, mat_handle) in &shop_runes {
        let dragged = dragged_shop_rune == Some(vis_marker.idx);
        let want = if dragged {
            empty_socket_color()
        } else {
            shop
                .runes
                .get(vis_marker.idx)
                .and_then(|o| o.as_ref())
                .map(|r| rune_color_for(*r))
                .unwrap_or(empty_socket_color())
        };
        if let Some(mat) = materials.get_mut(&mat_handle.0) {
            if mat.color != want {
                mat.color = want;
            }
        }
    }

    // Badge text — show level while stock is available, blank when sold
    // OR while being dragged.
    for (badge_text, mut text) in &mut shop_badge_texts {
        let dragged = dragged_shop_turret == Some(badge_text.idx);
        let n = if dragged {
            String::new()
        } else {
            shop
                .turrets
                .get(badge_text.idx)
                .and_then(|o| o.as_ref())
                .map(|o| o.barrels.max(1).to_string())
                .unwrap_or_else(String::new)
        };
        if text.0 != n {
            text.0 = n;
        }
    }

    for (name_marker, mut text, mut color) in &mut shop_name_texts {
        let dragged = dragged_shop_turret == Some(name_marker.idx);
        let offer = if dragged { None } else { shop.turrets.get(name_marker.idx).and_then(|o| o.as_ref()) };
        match offer {
            Some(o) => {
                let label = weapon_short_label(o.weapon).to_string();
                if text.0 != label {
                    text.0 = label;
                }
                let want = turret_barrel_color_for(o.weapon);
                if color.0 != want {
                    color.0 = want;
                }
            }
            None => {
                if !text.0.is_empty() {
                    text.0.clear();
                }
            }
        }
    }
    for (name_marker, mut text, mut color) in &mut shop_rune_name_texts {
        let dragged = dragged_shop_rune == Some(name_marker.idx);
        let rune = if dragged {
            None
        } else {
            shop.runes.get(name_marker.idx).and_then(|o| o.as_ref()).copied()
        };
        match rune {
            Some(r) => {
                let label = r.label().to_string();
                if text.0 != label {
                    text.0 = label;
                }
                let want = rune_color_for(r);
                if color.0 != want {
                    color.0 = want;
                }
            }
            None => {
                if !text.0.is_empty() {
                    text.0.clear();
                }
            }
        }
    }

    // Cost labels — show the scrap price while a tile is stocked,
    // blank when sold or being dragged. Cost is the same on every
    // card today (`SHOP_ITEM_COST`); no per-tile math.
    let cost_str = SHOP_ITEM_COST.to_string();
    for (cost_marker, mut text) in &mut shop_turret_cost_texts {
        let dragged = dragged_shop_turret == Some(cost_marker.idx);
        let stocked = !dragged
            && shop
                .turrets
                .get(cost_marker.idx)
                .and_then(|o| o.as_ref())
                .is_some();
        let want: &str = if stocked { cost_str.as_str() } else { "" };
        if text.0 != want {
            text.0 = want.to_string();
        }
    }
    for (cost_marker, mut text) in &mut shop_rune_cost_texts {
        let dragged = dragged_shop_rune == Some(cost_marker.idx);
        let stocked = !dragged
            && shop
                .runes
                .get(cost_marker.idx)
                .and_then(|o| o.as_ref())
                .is_some();
        let want: &str = if stocked { cost_str.as_str() } else { "" };
        if text.0 != want {
            text.0 = want.to_string();
        }
    }

    // ---------- AOE badges ----------
    // Force-hide every AOE badge entity. The AOE tag was moved into
    // the tooltip description (see `customize::tooltip::AOE_TAG`),
    // so the on-card overlay is redundant. Spawn entities are kept
    // for an easy revert by re-introducing the per-card sync.
    let _ = viewport;
    let _ = (dragged_shop_turret, dragged_shop_rune);
    for (_, _, _, _, mut vis) in &mut shop_aoe_tag_sprites {
        if *vis != Visibility::Hidden { *vis = Visibility::Hidden; }
    }
    for (_, _, _, _, mut vis) in &mut shop_aoe_tag_texts {
        if *vis != Visibility::Hidden { *vis = Visibility::Hidden; }
    }
}

/// Drive the sell-strip refund preview. The static "SELL" label is
/// always visible on the strip's left; this updater fills in the
/// gold "+N SCRAP" on the right while the player is BOTH dragging
/// a ship-sourced sellable AND hovering the strip. Hover-gating
/// makes the preview feel "live" — it pops on as the cursor crosses
/// the strip and vanishes the instant the cursor leaves OR the
/// drag releases. Shop-sourced drags refund 0 so the preview stays
/// hidden for those (you can't sell what you don't own yet).
pub fn update_sell_label(
    open: Res<CustomizeOpen>,
    drag: Res<DragState>,
    cfg: Res<TurretConfig>,
    sell_panel: Query<(&Transform, &super::setup::HitArea), With<super::setup::ShopSellPanel>>,
    mut preview_q: Query<
        (&mut Text2d, &mut Visibility),
        (With<SellPricePreview>, Without<super::setup::SellPanelLabel>),
    >,
    mut sell_q: Query<
        &mut Visibility,
        (With<super::setup::SellPanelLabel>, Without<SellPricePreview>),
    >,
) {
    if !open.open { return; }
    let hovering = drag.spec_cursor.and_then(|cursor| {
        sell_panel.iter().find_map(|(tf, hit)| {
            let centre = tf.translation.truncate();
            let half = hit.size * 0.5;
            let inside = cursor.x >= centre.x - half.x
                && cursor.x <= centre.x + half.x
                && cursor.y >= centre.y - half.y
                && cursor.y <= centre.y + half.y;
            if inside { Some(()) } else { None }
        })
    }).is_some();
    let preview = drag
        .picked
        .as_ref()
        .filter(|_| hovering)
        .map(|p| super::drag::sell_refund_for(&p.source, &cfg))
        .filter(|&r| r > 0);

    // Both texts share the same centred position; we swap which one
    // is visible based on whether a preview is active.
    let (preview_vis, sell_vis) = match preview {
        Some(_) => (Visibility::Inherited, Visibility::Hidden),
        None    => (Visibility::Hidden, Visibility::Inherited),
    };
    for (mut text, mut vis) in &mut preview_q {
        if let Some(refund) = preview {
            let s = format!("+{} SCRAP", refund);
            if text.0 != s { text.0 = s; }
        }
        if *vis != preview_vis { *vis = preview_vis; }
    }
    for mut vis in &mut sell_q {
        if *vis != sell_vis { *vis = sell_vis; }
    }
}

fn weapon_short_label(w: WeaponType) -> &'static str {
    // Full names everywhere, no shorthand. Shop tiles are sized to
    // accommodate the longer names; if a future weapon name pushes
    // the layout, widen the shop tile rather than reverting to
    // abbreviations.
    match w {
        WeaponType::Standard   => "STANDARD",
        WeaponType::Sniper     => "SNIPER",
        WeaponType::MachineGun => "MACHINE GUN",
        WeaponType::Shotgun    => "SHOTGUN",
        WeaponType::Railgun    => "RAILGUN",
        WeaponType::Mortar     => "MORTAR",
        WeaponType::HeliPad    => "HELIPAD",
        WeaponType::Cannon     => "CANNON",
        WeaponType::Booster    => "BOOSTER",
        WeaponType::Blade      => "BLADE",
        WeaponType::Cage       => "CAGE",
        WeaponType::Harpoon    => "HARPOON",
        WeaponType::SpreadRockets => "ROCKETS",
        WeaponType::Flamethrower => "FLAMER",
        WeaponType::SpikedPlate => "SPIKES",
        WeaponType::Amplifier => "AMPLIFIER",
        WeaponType::SharkNet => "SHARK NET",
        WeaponType::AnchorFlail => "ANCHOR FLAIL",
        WeaponType::PlasmaTorpedo => "PLASMA",
        WeaponType::CrowsNest => "CROW'S NEST",
    }
}

/// Reroll click + per-frame "can afford?" tint. The `can_afford` check
/// drives both:
/// - **Tint**: green container when affordable, dim grey when not.
/// - **Cost text colour**: white when affordable, red when not.
pub fn handle_reroll_button(
    open: Res<CustomizeOpen>,
    mouse: Res<ButtonInput<bevy::input::mouse::MouseButton>>,
    drag: Res<DragState>,
    mut scrap: ResMut<crate::Scrap>,
    mut commands: Commands,
    mut materials: ResMut<Assets<ColorMaterial>>,
    btn_q: Query<(&Transform, &super::setup::HitArea), With<ShopRerollBtn>>,
    bg_q: Query<&MeshMaterial2d<ColorMaterial>, With<ShopRerollBg>>,
    mut cost_q: Query<&mut TextColor, With<ShopRerollCostText>>,
) {
    if !open.open {
        return;
    }

    let can_afford = scrap.0 >= SHOP_REROLL_COST;

    // Tint the container green-on-affordable / muddy-grey-on-broke. The
    // container has 6 mesh entities (h-rect + v-rect + 4 corner circles)
    // all sharing one ColorMaterial handle, so a single mat write covers
    // the whole pill.
    let want_bg = if can_afford {
        Color::srgb(0.22, 0.40, 0.26)
    } else {
        Color::srgb(0.20, 0.22, 0.26)
    };
    for mat_handle in &bg_q {
        if let Some(mat) = materials.get_mut(&mat_handle.0) {
            if mat.color != want_bg {
                mat.color = want_bg;
            }
        }
    }

    let want_text = if can_afford {
        Color::WHITE
    } else {
        Color::srgb(0.85, 0.42, 0.42)
    };
    for mut color in &mut cost_q {
        if color.0 != want_text {
            color.0 = want_text;
        }
    }

    // Click resolution.
    if !mouse.just_pressed(bevy::input::mouse::MouseButton::Left) {
        return;
    }
    if drag.picked.is_some() {
        return;
    }
    if !can_afford {
        return;
    }
    let Some(cursor) = drag.spec_cursor else { return };
    for (tf, hit) in &btn_q {
        let centre = tf.translation.truncate();
        let half = hit.size * 0.5;
        if cursor.x >= centre.x - half.x
            && cursor.x <= centre.x + half.x
            && cursor.y >= centre.y - half.y
            && cursor.y <= centre.y + half.y
        {
            scrap.0 -= SHOP_REROLL_COST;
            commands.insert_resource(roll_fresh_stock());
            return;
        }
    }
}

pub fn handle_close_click(
    mouse: Res<ButtonInput<MouseButton>>,
    drag: Res<super::drag::DragState>,
    open: Res<CustomizeOpen>,
    mut next: ResMut<NextState<crate::AppState>>,
    close_q: Query<(&Transform, &super::setup::HitArea), With<super::CustomizeCloseBtn>>,
) {
    if !open.open {
        return;
    }
    if !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    if drag.picked.is_some() {
        return;
    }
    let Some(cursor) = drag.spec_cursor else { return };
    for (tf, hit) in &close_q {
        let centre = tf.translation.truncate();
        let half = hit.size * 0.5;
        if cursor.x >= centre.x - half.x
            && cursor.x <= centre.x + half.x
            && cursor.y >= centre.y - half.y
            && cursor.y <= centre.y + half.y
        {
            // Closing the shop drops the player onto the map between
            // stages — they pick the next section to attack rather
            // than getting yanked straight into combat. The `OnExit(Map)`
            // hook refills HP + clears the arena when they enter a
            // section.
            next.set(crate::AppState::Map);
            return;
        }
    }
}

/// Which targeting rune (if any) the player is currently *interacting
/// with* — defined as either dragging it (from a ship socket or a shop
/// card) or hovering it on a shop card. Returns `None` if the
/// interaction is not on a targeting rune; that's the signal used by
/// the socket-lockout hash overlay to stay hidden. Without this gate
/// the hash would render perpetually on every slot that owns a
/// targeting rune, which is just noise — the warning only matters
/// when the player could plausibly try to slot a second one.
fn active_targeting_rune(
    drag: &DragState,
    shop: Option<&CustomizeShop>,
    cfg: &TurretConfig,
    drag_sources: &Query<(&Transform, &super::setup::HitArea, &DragSourceMarker)>,
) -> Option<Rune> {
    // Drag path — the picked source resolves to a specific rune.
    if let Some(picked) = drag.picked.as_ref() {
        return resolve_rune(picked.source, cfg, shop);
    }
    // Hover path — only matters when nothing is being dragged. Walk
    // every drag source under the cursor and pick the smallest hit
    // area (sockets win over slot bases, matching the click-resolver
    // behaviour in `complete_drag`). Only shop runes contribute here:
    // hovering an already-equipped ship rune doesn't suggest the
    // player is about to add a second one.
    let cursor = drag.spec_cursor?;
    let mut best: Option<(f32, Rune)> = None;
    for (tf, hit, marker) in drag_sources {
        if !matches!(marker.0, DragSourceKind::ShopRune(_)) { continue; }
        let centre = tf.translation.truncate();
        let half = hit.size * 0.5;
        if cursor.x < centre.x - half.x || cursor.x > centre.x + half.x { continue; }
        if cursor.y < centre.y - half.y || cursor.y > centre.y + half.y { continue; }
        let Some(rune) = resolve_rune(marker.0, cfg, shop) else { continue; };
        let area = hit.size.x * hit.size.y;
        if best.map_or(true, |(a, _)| area < a) {
            best = Some((area, rune));
        }
    }
    let (_, rune) = best?;
    if !rune.is_targeting() { return None; }
    Some(rune)
}

fn resolve_rune(
    source: DragSourceKind,
    cfg: &TurretConfig,
    shop: Option<&CustomizeShop>,
) -> Option<Rune> {
    match source {
        DragSourceKind::ShipRune { slot, rune_idx } => {
            let rune = cfg.slots.get(slot)?.runes.get(rune_idx).copied().flatten()?;
            if rune.is_targeting() { Some(rune) } else { None }
        }
        DragSourceKind::ShopRune(idx) => {
            let rune = shop?.runes.get(idx).copied().flatten()?;
            if rune.is_targeting() { Some(rune) } else { None }
        }
        _ => None,
    }
}
