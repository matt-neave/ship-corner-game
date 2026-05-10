//! Hover tooltip for the customize overlay.
//!
//! Composition: every part — background fill, white outline, title, body —
//! lives on `UPSCALE_LAYER` (native res). That keeps the panel as a clean
//! rectangle (no chunky-pixel rounding) AND lets us push the z above the
//! customize UI's native-res text (z=100) so other labels can't clip
//! through the tooltip body.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;
use bevy::sprite::Anchor;
use bevy::text::{FontSmoothing, TextBounds};

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

/// Marker on each `TextSpan` child of the body entity. Used by the
/// per-frame body-text updater to find + despawn the old segment
/// spans before respawning a fresh set for the new description.
#[derive(Component)]
pub struct CustomizeTooltipBodySpan;

/// Minimum tooltip box dims in spec pixels — the box grows beyond this
/// when the body/title text needs more space. Multiplied by
/// `display_scale` to get the native-pixel size each frame.
const TOOLTIP_MIN_W: f32 = 48.0;
const TOOLTIP_H: f32 = 22.0;
/// Spec-pixel gap between the hovered source and the tooltip edge.
const TOOLTIP_GAP: f32 = 2.0;
/// Native-pixel padding between the text bounds and the fill edge.
const TOOLTIP_TEXT_PAD: f32 = 12.0;
/// Native-pixel thickness of the white outline ring around the fill.
const TOOLTIP_BORDER_PX: f32 = 2.0;
/// Title + body font sizes (native pixels). Both bumped so labels read
/// clearly without zooming in.
const TOOLTIP_TITLE_FONT: f32 = 18.0;
const TOOLTIP_BODY_FONT: f32 = 14.0;
/// Native-pixel cap on body text width — body wraps at word boundaries
/// when it would exceed this. Big enough that short descriptions fit on
/// one line; small enough that long descriptions stack neatly without
/// dominating the canvas.
const TOOLTIP_BODY_MAX_W: f32 = 280.0;
/// Approx char width (chars × font_size × this ≈ rendered native width).
/// Used both for the title's auto-fit and the body's line-count estimate.
const TOOLTIP_CHAR_W: f32 = 0.55;
/// Vertical line-height multiplier for the wrapped body — turns
/// `body_font * lines` into the total body block height.
const TOOLTIP_LINE_HEIGHT_MULT: f32 = 1.25;

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
    // Title — bright accent. Top-centre anchor so the title sits at
    // the top of the box; the update system positions the y so the
    // top edge lines up with the fill rectangle minus padding.
    commands.spawn((
        Text2d::new(""),
        TextFont {
            font_size: TOOLTIP_TITLE_FONT,
            font_smoothing: FontSmoothing::None,
            ..default()
        },
        TextColor(Color::srgb(1.0, 0.85, 0.30)),
        Anchor::TopCenter,
        Transform::from_xyz(0.0, 0.0, Z_TOOLTIP_TEXT),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        CustomizeTooltipTitle,
    ));
    // Body — softer. Word-wrapped at `TOOLTIP_BODY_MAX_W` via
    // `TextBounds`. Top-centre anchor so we can stack it under the
    // title cleanly.
    commands.spawn((
        Text2d::new(""),
        TextFont {
            font_size: TOOLTIP_BODY_FONT,
            font_smoothing: FontSmoothing::None,
            ..default()
        },
        TextColor(Color::srgb(0.85, 0.88, 0.94)),
        TextLayout::new_with_justify(JustifyText::Center),
        TextBounds::new_horizontal(TOOLTIP_BODY_MAX_W),
        Anchor::TopCenter,
        Transform::from_xyz(0.0, 0.0, Z_TOOLTIP_TEXT),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        CustomizeTooltipBody,
    ));
}

pub fn update_customize_tooltip(
    mut commands: Commands,
    open: Res<CustomizeOpen>,
    drag: Res<DragState>,
    cfg: Res<TurretConfig>,
    shop: Option<Res<CustomizeShop>>,
    viewport: Res<CustomizeViewport>,
    mut body_cache: Local<String>,
    body_entity_q: Query<Entity, With<CustomizeTooltipBody>>,
    existing_body_spans: Query<Entity, With<CustomizeTooltipBodySpan>>,
    sources: Query<(&Transform, &HitArea, &DragSourceMarker)>,
    stat_hovers: Query<(&Transform, &HitArea, &super::stats_panel::StatHover), Without<DragSourceMarker>>,
    mut outline_q: Query<
        (&mut Visibility, &mut Transform, &mut Sprite),
        (
            With<CustomizeTooltipOutline>,
            Without<CustomizeTooltipFill>,
            Without<CustomizeTooltipTitle>,
            Without<CustomizeTooltipBody>,
            Without<DragSourceMarker>,
            Without<super::stats_panel::StatHover>,
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
            Without<super::stats_panel::StatHover>,
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
            Without<super::stats_panel::StatHover>,
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
            Without<super::stats_panel::StatHover>,
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
    // Stat-row hovers — same smallest-hit-area selection, so a row only
    // wins over a turret/rune if the cursor is exclusively on the row.
    for (tf, hit, hover) in &stat_hovers {
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
        info = Some((
            hover.0.label().to_string(),
            hover.0.description().to_string(),
            centre,
            half,
        ));
        best_area = area;
    }

    let Some((title, body, source_centre, source_half)) = info else {
        hide_all(&mut outline_q, &mut fill_q, &mut title_q, &mut body_q);
        return;
    };

    // Size the box to fit (title width vs wrapped-body width) and a
    // height proportional to the wrapped body's line count + title.
    let s = viewport.display_scale;
    let title_w_native = estimate_text_native_width(&title, TOOLTIP_TITLE_FONT);
    let body_unwrapped_w = estimate_text_native_width(&body, TOOLTIP_BODY_FONT);
    // Wrapped body width never exceeds the cap; if the unwrapped body
    // is narrower than the cap, use the actual width so the box is
    // tight on short descriptions.
    let body_wrapped_w = body_unwrapped_w.min(TOOLTIP_BODY_MAX_W);
    let text_w_native = title_w_native.max(body_wrapped_w);
    let fill_w_native = (text_w_native + 2.0 * TOOLTIP_TEXT_PAD).max(TOOLTIP_MIN_W * s);
    // Line-count estimate: how many `TOOLTIP_BODY_MAX_W` slabs the
    // unwrapped body needs. `ceil(body_w / max_w)`, min 1.
    let body_lines = (body_unwrapped_w / TOOLTIP_BODY_MAX_W).ceil().max(1.0);
    let body_block_h = body_lines * TOOLTIP_BODY_FONT * TOOLTIP_LINE_HEIGHT_MULT;
    let title_block_h = TOOLTIP_TITLE_FONT * TOOLTIP_LINE_HEIGHT_MULT;
    let fill_h_native = title_block_h + body_block_h + 2.0 * TOOLTIP_TEXT_PAD;
    let tooltip_w_spec = fill_w_native / s;
    let tooltip_h_spec = fill_h_native / s;

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
    pos.y = pos.y.clamp(-canvas_half_h + tooltip_h_spec * 0.5, canvas_half_h - tooltip_h_spec * 0.5);

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
    // Title pinned to the top of the fill (anchor TopCenter); body
    // sits directly below it. Top-of-fill y = native_centre.y + h/2 -
    // pad; body starts at top_y - title_block_h.
    let fill_top_native = native_centre.y + fill_h_native * 0.5 - TOOLTIP_TEXT_PAD;
    if let Ok((mut v, mut tf, mut text)) = title_q.single_mut() {
        if *v != Visibility::Inherited {
            *v = Visibility::Inherited;
        }
        tf.translation.x = native_centre.x;
        tf.translation.y = fill_top_native;
        if text.0 != title {
            text.0 = title;
        }
    }
    if let Ok((mut v, mut tf, mut text)) = body_q.single_mut() {
        if *v != Visibility::Inherited {
            *v = Visibility::Inherited;
        }
        tf.translation.x = native_centre.x;
        tf.translation.y = fill_top_native - title_block_h;
        // Clear the root section text — all visible text lives in
        // colored `TextSpan` children spawned below. The root stays
        // as the layout/anchor host.
        if !text.0.is_empty() { text.0 = String::new(); }
    }

    // Rebuild colored body spans when the description text changes.
    // `+30%` / `+1` get a green tint, `-50` / `-70%` get red; the
    // rest stays the default body color. Despawning + respawning is
    // cheap here — bodies change only on hover-target switches, not
    // every frame, gated by the `body_cache` compare.
    if *body_cache != body {
        *body_cache = body.clone();
        for span in existing_body_spans.iter() {
            commands.entity(span).despawn();
        }
        if let Ok(body_entity) = body_entity_q.single() {
            let segments = colorize_bonuses(&body);
            commands.entity(body_entity).with_children(|p| {
                for (segment, color) in segments {
                    p.spawn((
                        TextSpan::new(segment),
                        TextFont {
                            font_size: TOOLTIP_BODY_FONT,
                            font_smoothing: FontSmoothing::None,
                            ..default()
                        },
                        TextColor(color),
                        CustomizeTooltipBodySpan,
                    ));
                }
            });
        }
    }
}

/// Default body color (matches the root `Text2d`'s `TextColor`).
const TOOLTIP_BODY_COLOR: Color = Color::srgb(0.85, 0.88, 0.94);
/// Tint for positive numeric tokens (`+30%`, `+1`).
const TOOLTIP_BUFF_COLOR: Color = Color::srgb(0.55, 0.95, 0.55);
/// Tint for negative numeric tokens (`-50`, `-70%`).
const TOOLTIP_NERF_COLOR: Color = Color::srgb(1.00, 0.55, 0.55);

/// Split body text into colored segments. A run starting with `+`
/// or `-` immediately followed by a digit (optionally with a
/// trailing `%`) is a buff/nerf token and gets the corresponding
/// tint; everything else stays the default body color.
fn colorize_bonuses(text: &str) -> Vec<(String, Color)> {
    let mut segments: Vec<(String, Color)> = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let is_sign_token = (c == '+' || c == '-')
            && i + 1 < chars.len()
            && chars[i + 1].is_ascii_digit();
        if is_sign_token {
            if !current.is_empty() {
                segments.push((std::mem::take(&mut current), TOOLTIP_BODY_COLOR));
            }
            let mut tok = String::new();
            tok.push(c);
            i += 1;
            while i < chars.len() && chars[i].is_ascii_digit() {
                tok.push(chars[i]);
                i += 1;
            }
            if i < chars.len() && chars[i] == '%' {
                tok.push('%');
                i += 1;
            }
            let color = if c == '+' { TOOLTIP_BUFF_COLOR } else { TOOLTIP_NERF_COLOR };
            segments.push((tok, color));
        } else {
            current.push(c);
            i += 1;
        }
    }
    if !current.is_empty() {
        segments.push((current, TOOLTIP_BODY_COLOR));
    }
    segments
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
            Without<super::stats_panel::StatHover>,
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
            Without<super::stats_panel::StatHover>,
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
            Without<super::stats_panel::StatHover>,
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
            Without<super::stats_panel::StatHover>,
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
    text.chars().count() as f32 * font_size * TOOLTIP_CHAR_W
}

/// Prefix the description with an `[AOE]` tag for weapons / runes
/// that participate in the splash-radius family. Surfaces the tag in
/// the tooltip body rather than as a card badge — same color cue as
/// the in-game splash particles.
const AOE_TAG: &str = "[AOE] ";

fn turret_tooltip(weapon: WeaponType, barrels: u8) -> (String, String) {
    // 1-barrel suffix omitted (it's the default — redundant noise on
    // every popover). 2/3-barrel still surfaces the upgrade.
    let title = if barrels <= 1 {
        weapon.label().to_string()
    } else {
        format!("{} {}B", weapon.label(), barrels)
    };
    let mut body = String::new();
    if matches!(weapon, WeaponType::Mortar) {
        body.push_str(AOE_TAG);
    }
    body.push_str(weapon.description());
    (title, body)
}

fn rune_tooltip(rune: Rune) -> (String, String) {
    let mut body = String::new();
    if matches!(rune, Rune::Splash) {
        body.push_str(AOE_TAG);
    }
    body.push_str(rune.description());
    (rune.label().to_string(), body)
}
