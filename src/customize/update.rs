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
    roll_fresh_stock, CustomizeShop, DragSourceKind, DragState, SHOP_REROLL_COST,
};
use super::setup::{
    empty_slot_color, empty_socket_color, rune_color_for, turret_barrel_color_for,
    turret_color_for, CustomizeScrapText, ShipRuneSocketPart, ShipSlotBadgeText,
    ShipSlotBase, ShopRerollBg, ShopRerollBtn, ShopRerollCostText, ShopRuneNameText,
    ShopRuneVisual, ShopTurretBadgeText, ShopTurretBase, ShopTurretNameText,
    ShopTurretVisual,
};
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
    mut materials: ResMut<Assets<ColorMaterial>>,
    bases: Query<(&ShipSlotBase, &MeshMaterial2d<ColorMaterial>)>,
    socket_parts: Query<(&ShipRuneSocketPart, &MeshMaterial2d<ColorMaterial>)>,
    mut badge_texts: Query<(&ShipSlotBadgeText, &mut Text2d)>,
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
    // socket whose rune is being dragged renders as empty; sockets on a
    // turret-being-dragged also empty out (the whole slot is "in transit").
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
    mut materials: ResMut<Assets<ColorMaterial>>,
    shop_turrets: Query<(&ShopTurretVisual, &MeshMaterial2d<ColorMaterial>)>,
    shop_bases: Query<(&ShopTurretBase, &MeshMaterial2d<ColorMaterial>)>,
    shop_runes: Query<(&ShopRuneVisual, &MeshMaterial2d<ColorMaterial>)>,
    mut shop_badge_texts: Query<
        (&ShopTurretBadgeText, &mut Text2d),
        (Without<ShopTurretNameText>, Without<ShopRuneNameText>),
    >,
    mut shop_name_texts: Query<
        (&ShopTurretNameText, &mut Text2d, &mut TextColor),
        (Without<ShopTurretBadgeText>, Without<ShopRuneNameText>),
    >,
    mut shop_rune_name_texts: Query<
        (&ShopRuneNameText, &mut Text2d, &mut TextColor),
        (Without<ShopTurretBadgeText>, Without<ShopTurretNameText>),
    >,
) {
    if !open.open {
        return;
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
}

fn weapon_short_label(w: WeaponType) -> &'static str {
    match w {
        WeaponType::Standard => "STD",
        WeaponType::Sniper => "SNIPER",
        WeaponType::MachineGun => "MG",
        WeaponType::Shotgun => "SHOT",
        WeaponType::Railgun => "RAIL",
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
            next.set(crate::AppState::Playing);
            return;
        }
    }
}
