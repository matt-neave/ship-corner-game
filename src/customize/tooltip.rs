//! Hover tooltip for the customize overlay.
//!
//! Composition:
//! - **Background container** (chunky pixel, on `CUSTOMIZE_LAYER`) —
//!   built via `setup::spawn_container` so the rounded square pixelates
//!   the same way the rest of the UI does.
//! - **Title + body text** (sharp, native res, on `UPSCALE_LAYER`) — the
//!   user wants text immune to pixelation, so labels live next to the
//!   in-game HUD.
//!
//! Each frame the title/body positions are derived from
//! `viewport.display_scale × spec_pos`. The container's spec position
//! tracks the cursor too.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;
use bevy::text::FontSmoothing;

use crate::balance::{CUSTOMIZE_INTERNAL_H, CUSTOMIZE_INTERNAL_W, UPSCALE_LAYER};
use crate::rune::Rune;
use crate::turret::TurretConfig;
use crate::weapon::WeaponType;

use super::drag::{
    CustomizeShop, DragSourceKind, DragState,
};
use super::render::CustomizeViewport;
use super::setup::{spawn_container, DragSourceMarker, HitArea};
use super::CustomizeOpen;

#[derive(Component, Clone, Copy)]
pub struct CustomizeTooltipPart;

#[derive(Component)]
pub struct CustomizeTooltipTitle;

#[derive(Component)]
pub struct CustomizeTooltipBody;

const TOOLTIP_W: f32 = 110.0;
const TOOLTIP_H: f32 = 30.0;

pub fn spawn_customize_tooltip(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ColorMaterial>,
) {
    let pos = Vec2::new(0.0, 0.0);
    spawn_container(
        commands,
        meshes,
        materials,
        pos,
        Vec2::new(TOOLTIP_W, TOOLTIP_H),
        4.0,
        Color::srgb(0.13, 0.14, 0.17),
        7.0,
        CustomizeTooltipPart,
    );
    // Title — bright accent, native res.
    commands.spawn((
        Text2d::new(""),
        TextFont {
            font_size: 14.0,
            font_smoothing: FontSmoothing::None,
            ..default()
        },
        TextColor(Color::srgb(1.0, 0.85, 0.30)),
        Transform::from_xyz(0.0, 0.0, 100.0),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        CustomizeTooltipTitle,
    ));
    // Body — softer.
    commands.spawn((
        Text2d::new(""),
        TextFont {
            font_size: 11.0,
            font_smoothing: FontSmoothing::None,
            ..default()
        },
        TextColor(Color::srgb(0.85, 0.88, 0.94)),
        Transform::from_xyz(0.0, 0.0, 100.0),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        CustomizeTooltipBody,
    ));
}

pub fn update_customize_tooltip(
    open: Res<CustomizeOpen>,
    drag: Res<DragState>,
    cfg: Res<TurretConfig>,
    shop: Option<Res<CustomizeShop>>,
    viewport: Res<CustomizeViewport>,
    sources: Query<(&Transform, &HitArea, &DragSourceMarker)>,
    mut parts: Query<
        (&mut Visibility, &mut Transform),
        (
            With<CustomizeTooltipPart>,
            Without<CustomizeTooltipTitle>,
            Without<CustomizeTooltipBody>,
            Without<DragSourceMarker>,
        ),
    >,
    mut title_q: Query<
        (&mut Visibility, &mut Transform, &mut Text2d),
        (
            With<CustomizeTooltipTitle>,
            Without<CustomizeTooltipPart>,
            Without<CustomizeTooltipBody>,
            Without<DragSourceMarker>,
        ),
    >,
    mut body_q: Query<
        (&mut Visibility, &mut Transform, &mut Text2d),
        (
            With<CustomizeTooltipBody>,
            Without<CustomizeTooltipPart>,
            Without<CustomizeTooltipTitle>,
            Without<DragSourceMarker>,
        ),
    >,
) {
    let hide = !open.open || drag.picked.is_some() || drag.spec_cursor.is_none();
    if hide {
        hide_all(&mut parts, &mut title_q, &mut body_q);
        return;
    }
    let cursor = drag.spec_cursor.unwrap();
    let shop_ref = shop.as_deref();

    let mut info: Option<(String, String)> = None;
    let mut best_area = f32::INFINITY;
    for (tf, hit, marker) in &sources {
        let centre = tf.translation.truncate();
        let half = hit.size * 0.5;
        if cursor.x < centre.x - half.x
            || cursor.x > centre.x + half.x
            || cursor.y < centre.y - half.y
            || cursor.y > centre.y + half.y
        {
            continue;
        }
        let area = hit.size.x * hit.size.y;
        if area >= best_area {
            continue;
        }
        if let Some(pair) = describe_source(marker.0, &cfg, shop_ref) {
            info = Some(pair);
            best_area = area;
        }
    }

    let Some((title, body)) = info else {
        hide_all(&mut parts, &mut title_q, &mut body_q);
        return;
    };

    // Position tooltip near the cursor in spec coords; clamp inside canvas.
    let canvas_half_w = CUSTOMIZE_INTERNAL_W as f32 * 0.5;
    let canvas_half_h = CUSTOMIZE_INTERNAL_H as f32 * 0.5;
    let mut pos = cursor + Vec2::new(TOOLTIP_W * 0.5 + 6.0, TOOLTIP_H * 0.5 + 6.0);
    if pos.x + TOOLTIP_W * 0.5 > canvas_half_w {
        pos.x = cursor.x - TOOLTIP_W * 0.5 - 6.0;
    }
    if pos.y + TOOLTIP_H * 0.5 > canvas_half_h {
        pos.y = cursor.y - TOOLTIP_H * 0.5 - 6.0;
    }
    pos.x = pos.x.clamp(-canvas_half_w + TOOLTIP_W * 0.5, canvas_half_w - TOOLTIP_W * 0.5);
    pos.y = pos.y.clamp(-canvas_half_h + TOOLTIP_H * 0.5, canvas_half_h - TOOLTIP_H * 0.5);

    // Container parts (on customize layer, in spec coords).
    for (mut v, mut tf) in &mut parts {
        if *v != Visibility::Inherited {
            *v = Visibility::Inherited;
        }
        tf.translation.x = pos.x;
        tf.translation.y = pos.y;
    }

    let s = viewport.display_scale;
    if let Ok((mut v, mut tf, mut text)) = title_q.single_mut() {
        if *v != Visibility::Inherited {
            *v = Visibility::Inherited;
        }
        tf.translation.x = pos.x * s;
        tf.translation.y = (pos.y + TOOLTIP_H * 0.25) * s;
        if text.0 != title {
            text.0 = title;
        }
    }
    if let Ok((mut v, mut tf, mut text)) = body_q.single_mut() {
        if *v != Visibility::Inherited {
            *v = Visibility::Inherited;
        }
        tf.translation.x = pos.x * s;
        tf.translation.y = (pos.y - TOOLTIP_H * 0.25) * s;
        if text.0 != body {
            text.0 = body;
        }
    }
}

fn hide_all(
    parts: &mut Query<
        (&mut Visibility, &mut Transform),
        (
            With<CustomizeTooltipPart>,
            Without<CustomizeTooltipTitle>,
            Without<CustomizeTooltipBody>,
            Without<DragSourceMarker>,
        ),
    >,
    title_q: &mut Query<
        (&mut Visibility, &mut Transform, &mut Text2d),
        (
            With<CustomizeTooltipTitle>,
            Without<CustomizeTooltipPart>,
            Without<CustomizeTooltipBody>,
            Without<DragSourceMarker>,
        ),
    >,
    body_q: &mut Query<
        (&mut Visibility, &mut Transform, &mut Text2d),
        (
            With<CustomizeTooltipBody>,
            Without<CustomizeTooltipPart>,
            Without<CustomizeTooltipTitle>,
            Without<DragSourceMarker>,
        ),
    >,
) {
    for (mut v, _) in parts {
        if *v != Visibility::Hidden {
            *v = Visibility::Hidden;
        }
    }
    if let Ok((mut v, _, _)) = title_q.single_mut() {
        if *v != Visibility::Hidden {
            *v = Visibility::Hidden;
        }
    }
    if let Ok((mut v, _, _)) = body_q.single_mut() {
        if *v != Visibility::Hidden {
            *v = Visibility::Hidden;
        }
    }
}

fn describe_source(
    source: DragSourceKind,
    cfg: &TurretConfig,
    shop: Option<&CustomizeShop>,
) -> Option<(String, String)> {
    match source {
        DragSourceKind::ShipSlot(slot) => {
            let s = cfg.slots[slot];
            if !s.equipped {
                return None;
            }
            Some(turret_tooltip(s.weapon, s.barrels.max(1)))
        }
        DragSourceKind::ShipRune { slot, rune_idx } => {
            let s = cfg.slots[slot];
            if !s.equipped {
                return None;
            }
            s.runes[rune_idx].map(rune_tooltip)
        }
        DragSourceKind::ShopTurret(idx) => shop
            .and_then(|s| s.turrets.get(idx))
            .and_then(|o| o.as_ref())
            .map(|o| turret_tooltip(o.weapon, o.barrels.max(1))),
        DragSourceKind::ShopRune(idx) => shop
            .and_then(|s| s.runes.get(idx))
            .and_then(|o| o.as_ref())
            .copied()
            .map(rune_tooltip),
    }
}

fn turret_tooltip(weapon: WeaponType, barrels: u8) -> (String, String) {
    let title = format!("{} {}B", weapon.label(), barrels);
    (title, weapon.description().to_string())
}

fn rune_tooltip(rune: Rune) -> (String, String) {
    (rune.label().to_string(), rune.description().to_string())
}
