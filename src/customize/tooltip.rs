//! Hover tooltip for the customize overlay.
//!
//! Composition: every part — background fill, white outline, title, body —
//! lives on `UPSCALE_LAYER` (native res). That keeps the panel as a clean
//! rectangle (no chunky-pixel rounding) AND lets us push the z above the
//! customize UI's native-res text (z=100) so other labels can't clip
//! through the tooltip body.

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
use super::setup::{DragSourceMarker, HitArea};
use super::CustomizeOpen;

#[derive(Component, Clone, Copy)]
pub struct CustomizeTooltipFill;

#[derive(Component, Clone, Copy)]
pub struct CustomizeTooltipOutline;

#[derive(Component)]
pub struct CustomizeTooltipTitle;

#[derive(Component)]
pub struct CustomizeTooltipBody;

/// Minimum tooltip box dims in spec pixels — the box grows beyond this
/// when the body/title text is wider. Multiplied by `display_scale` to
/// get the native-pixel size each frame.
const TOOLTIP_MIN_W: f32 = 48.0;
const TOOLTIP_H: f32 = 22.0;
/// Spec-pixel gap between the hovered source and the tooltip edge.
const TOOLTIP_GAP: f32 = 2.0;
/// Native-pixel padding between the text bounds and the fill edge.
const TOOLTIP_TEXT_PAD: f32 = 10.0;
/// Native-pixel thickness of the white outline ring around the fill.
const TOOLTIP_BORDER_PX: f32 = 2.0;

// Z layering. Other customize UI text sits at z=100 on UPSCALE_LAYER, so
// the tooltip needs to be above that to avoid being clipped.
const Z_TOOLTIP_OUTLINE: f32 = 110.0;
const Z_TOOLTIP_FILL: f32 = 110.5;
const Z_TOOLTIP_TEXT: f32 = 111.0;

/// Match the customize camera's clear color so the tooltip fill reads as
/// "the menu's background extended". The 1px white outline does the work
/// of separating it from the canvas.
fn tooltip_bg_color() -> Color {
    Color::srgb(0.13, 0.14, 0.17)
}

pub fn spawn_customize_tooltip(commands: &mut Commands) {
    // Initial sizes are placeholders — the update system rewrites both
    // each frame from the title/body text width.
    commands.spawn((
        Sprite {
            color: Color::WHITE,
            custom_size: Some(Vec2::new(TOOLTIP_MIN_W, TOOLTIP_H)),
            ..default()
        },
        Transform::from_xyz(0.0, 0.0, Z_TOOLTIP_OUTLINE),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        CustomizeTooltipOutline,
    ));
    commands.spawn((
        Sprite {
            color: tooltip_bg_color(),
            custom_size: Some(Vec2::new(TOOLTIP_MIN_W, TOOLTIP_H)),
            ..default()
        },
        Transform::from_xyz(0.0, 0.0, Z_TOOLTIP_FILL),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        CustomizeTooltipFill,
    ));
    // Title — bright accent.
    commands.spawn((
        Text2d::new(""),
        TextFont {
            font_size: 14.0,
            font_smoothing: FontSmoothing::None,
            ..default()
        },
        TextColor(Color::srgb(1.0, 0.85, 0.30)),
        Transform::from_xyz(0.0, 0.0, Z_TOOLTIP_TEXT),
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
        Transform::from_xyz(0.0, 0.0, Z_TOOLTIP_TEXT),
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
    mut outline_q: Query<
        (&mut Visibility, &mut Transform, &mut Sprite),
        (
            With<CustomizeTooltipOutline>,
            Without<CustomizeTooltipFill>,
            Without<CustomizeTooltipTitle>,
            Without<CustomizeTooltipBody>,
            Without<DragSourceMarker>,
        ),
    >,
    mut fill_q: Query<
        (&mut Visibility, &mut Transform, &mut Sprite),
        (
            With<CustomizeTooltipFill>,
            Without<CustomizeTooltipOutline>,
            Without<CustomizeTooltipTitle>,
            Without<CustomizeTooltipBody>,
            Without<DragSourceMarker>,
        ),
    >,
    mut title_q: Query<
        (&mut Visibility, &mut Transform, &mut Text2d),
        (
            With<CustomizeTooltipTitle>,
            Without<CustomizeTooltipOutline>,
            Without<CustomizeTooltipFill>,
            Without<CustomizeTooltipBody>,
            Without<DragSourceMarker>,
        ),
    >,
    mut body_q: Query<
        (&mut Visibility, &mut Transform, &mut Text2d),
        (
            With<CustomizeTooltipBody>,
            Without<CustomizeTooltipOutline>,
            Without<CustomizeTooltipFill>,
            Without<CustomizeTooltipTitle>,
            Without<DragSourceMarker>,
        ),
    >,
) {
    let hide = !open.open || drag.picked.is_some() || drag.spec_cursor.is_none();
    if hide {
        hide_all(&mut outline_q, &mut fill_q, &mut title_q, &mut body_q);
        return;
    }
    let cursor = drag.spec_cursor.unwrap();
    let shop_ref = shop.as_deref();

    let mut info: Option<(String, String, Vec2, Vec2)> = None;
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
        if let Some((title, body)) = describe_source(marker.0, &cfg, shop_ref) {
            info = Some((title, body, centre, half));
            best_area = area;
        }
    }

    let Some((title, body, source_centre, source_half)) = info else {
        hide_all(&mut outline_q, &mut fill_q, &mut title_q, &mut body_q);
        return;
    };

    // Size the box to fit the wider of (title, body). Dynamic so short
    // descriptions get a tight box and long ones don't overspill.
    let s = viewport.display_scale;
    let title_w_native = estimate_text_native_width(&title, 14.0);
    let body_w_native = estimate_text_native_width(&body, 11.0);
    let text_w_native = title_w_native.max(body_w_native);
    let fill_w_native = (text_w_native + 2.0 * TOOLTIP_TEXT_PAD).max(TOOLTIP_MIN_W * s);
    let fill_h_native = TOOLTIP_H * s;
    let tooltip_w_spec = fill_w_native / s;

    // Anchor to the hovered source. Right of the source by default; flip
    // left if that would clip the canvas edge.
    let canvas_half_w = CUSTOMIZE_INTERNAL_W as f32 * 0.5;
    let canvas_half_h = CUSTOMIZE_INTERNAL_H as f32 * 0.5;
    let right_x = source_centre.x + source_half.x + TOOLTIP_GAP + tooltip_w_spec * 0.5;
    let left_x = source_centre.x - source_half.x - TOOLTIP_GAP - tooltip_w_spec * 0.5;
    let mut pos = Vec2::new(right_x, source_centre.y);
    if pos.x + tooltip_w_spec * 0.5 > canvas_half_w {
        pos.x = left_x;
    }
    pos.x = pos.x.clamp(-canvas_half_w + tooltip_w_spec * 0.5, canvas_half_w - tooltip_w_spec * 0.5);
    pos.y = pos.y.clamp(-canvas_half_h + TOOLTIP_H * 0.5, canvas_half_h - TOOLTIP_H * 0.5);

    let native_centre = Vec2::new(pos.x * s, pos.y * s);
    let fill_size_native = Vec2::new(fill_w_native, fill_h_native);
    let outline_size_native = fill_size_native + Vec2::splat(2.0 * TOOLTIP_BORDER_PX);

    if let Ok((mut v, mut tf, mut sprite)) = outline_q.single_mut() {
        if *v != Visibility::Inherited {
            *v = Visibility::Inherited;
        }
        tf.translation.x = native_centre.x;
        tf.translation.y = native_centre.y;
        if sprite.custom_size != Some(outline_size_native) {
            sprite.custom_size = Some(outline_size_native);
        }
    }
    if let Ok((mut v, mut tf, mut sprite)) = fill_q.single_mut() {
        if *v != Visibility::Inherited {
            *v = Visibility::Inherited;
        }
        tf.translation.x = native_centre.x;
        tf.translation.y = native_centre.y;
        if sprite.custom_size != Some(fill_size_native) {
            sprite.custom_size = Some(fill_size_native);
        }
    }
    if let Ok((mut v, mut tf, mut text)) = title_q.single_mut() {
        if *v != Visibility::Inherited {
            *v = Visibility::Inherited;
        }
        tf.translation.x = native_centre.x;
        tf.translation.y = (pos.y + TOOLTIP_H * 0.25) * s;
        if text.0 != title {
            text.0 = title;
        }
    }
    if let Ok((mut v, mut tf, mut text)) = body_q.single_mut() {
        if *v != Visibility::Inherited {
            *v = Visibility::Inherited;
        }
        tf.translation.x = native_centre.x;
        tf.translation.y = (pos.y - TOOLTIP_H * 0.25) * s;
        if text.0 != body {
            text.0 = body;
        }
    }
}

fn hide_all(
    outline_q: &mut Query<
        (&mut Visibility, &mut Transform, &mut Sprite),
        (
            With<CustomizeTooltipOutline>,
            Without<CustomizeTooltipFill>,
            Without<CustomizeTooltipTitle>,
            Without<CustomizeTooltipBody>,
            Without<DragSourceMarker>,
        ),
    >,
    fill_q: &mut Query<
        (&mut Visibility, &mut Transform, &mut Sprite),
        (
            With<CustomizeTooltipFill>,
            Without<CustomizeTooltipOutline>,
            Without<CustomizeTooltipTitle>,
            Without<CustomizeTooltipBody>,
            Without<DragSourceMarker>,
        ),
    >,
    title_q: &mut Query<
        (&mut Visibility, &mut Transform, &mut Text2d),
        (
            With<CustomizeTooltipTitle>,
            Without<CustomizeTooltipOutline>,
            Without<CustomizeTooltipFill>,
            Without<CustomizeTooltipBody>,
            Without<DragSourceMarker>,
        ),
    >,
    body_q: &mut Query<
        (&mut Visibility, &mut Transform, &mut Text2d),
        (
            With<CustomizeTooltipBody>,
            Without<CustomizeTooltipOutline>,
            Without<CustomizeTooltipFill>,
            Without<CustomizeTooltipTitle>,
            Without<DragSourceMarker>,
        ),
    >,
) {
    if let Ok((mut v, _, _)) = outline_q.single_mut() {
        if *v != Visibility::Hidden {
            *v = Visibility::Hidden;
        }
    }
    if let Ok((mut v, _, _)) = fill_q.single_mut() {
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

/// Generous estimate of rendered text width in native pixels. Bevy's
/// default font (Fira) averages ~0.55× font_size per glyph; we round up
/// so the box never under-sizes its content.
fn estimate_text_native_width(text: &str, font_size: f32) -> f32 {
    text.chars().count() as f32 * font_size * 0.6
}

fn turret_tooltip(weapon: WeaponType, barrels: u8) -> (String, String) {
    let title = format!("{} {}B", weapon.label(), barrels);
    (title, weapon.description().to_string())
}

fn rune_tooltip(rune: Rune) -> (String, String) {
    (rune.label().to_string(), rune.description().to_string())
}
