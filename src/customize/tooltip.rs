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
use bevy::text::TextBounds;

use bevy::ecs::system::SystemParam;

use crate::balance::{CUSTOMIZE_INTERNAL_H, CUSTOMIZE_INTERNAL_W, UPSCALE_LAYER};
use crate::rune::Rune;
use crate::synergy::Synergies;
use crate::turret::TurretConfig;
use crate::weapon::{WeaponTag, WeaponType};

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

/// Compact synergy banner — stacks ABOVE the main turret tooltip
/// when the hovered source has a `WeaponTag`. Shows the tag's
/// 4-tier value ladder (e.g. `10%/20%/30%/40%`) with the active
/// tier value brightened. Hidden whenever the main tooltip is
/// hidden OR the hovered source is a rune / stat row (no tag).
#[derive(Component, Clone, Copy)]
pub struct SynergyBannerFill;
#[derive(Component, Clone, Copy)]
pub struct SynergyBannerOutline;
#[derive(Component)]
pub struct SynergyBannerText;
/// Marker on each colored `TextSpan` child of the synergy banner —
/// the updater despawns these before rebuilding when the tag (or
/// active tier) changes.
#[derive(Component)]
pub struct SynergyBannerSpan;

/// Bundle of read-only resources the tooltip body builder reaches
/// into: the live `TurretConfig` (for ship slot lookups), the
/// optional `CustomizeShop` (for shop slot lookups), and the
/// per-run `DiscoveredSynergies` mask (so `[???]` swaps to the real
/// tag chip the moment the player discovers a synergy). Bundled
/// via `SystemParam` because `update_customize_tooltip` is at
/// Bevy's 16-arg cap for `IntoSystem` — one bundled arg unlocks
/// the discovery plumbing without splitting the system further.
#[derive(SystemParam)]
pub struct TooltipDataCtx<'w> {
    pub cfg: Res<'w, TurretConfig>,
    pub shop: Option<Res<'w, CustomizeShop>>,
    pub discovered: Res<'w, crate::onboarding::DiscoveredSynergies>,
    /// Live player stats — Brotato-style dynamic descriptions use this
    /// to substitute the current TurretDamage / RuneDamage / etc.
    /// modifiers into weapon and rune tooltip bodies.
    pub stats: Res<'w, crate::stats::PlayerStats>,
    /// Live synergies — the weapon-damage line folds Naval + Support
    /// multipliers in so the tooltip number matches the damage the
    /// shot actually deals.
    pub synergies: Res<'w, crate::synergy::Synergies>,
    /// Per-slot damage tallies from the last + current round. Surfaced
    /// in the turret tooltip as "Damage last round: X% (N)" so the
    /// player can tell which slots are pulling their weight.
    pub damage_stats: Res<'w, crate::ui::DamageStats>,
    /// Pixel Operator font handle used for every tooltip glyph.
    /// Bundled into this ctx so the parent system doesn't blow
    /// past Bevy's 16-element SystemParam tuple limit — adding
    /// `Res<PixelFont>` as a separate param pushed
    /// `update_customize_tooltip` to 17 args and broke the
    /// `IntoSystem` impl downstream.
    pub pixel_font: Res<'w, crate::fonts::PixelFont>,
}

/// Layout snapshot for the main tooltip. Written by
/// `update_customize_tooltip` whenever it positions the box;
/// read by `update_synergy_banner` to stack the banner above
/// (or below, when the canvas top would clip it). Cleared to
/// `None` whenever the tooltip is hidden so the banner system
/// hides in lockstep.
#[derive(Resource, Default)]
pub struct TooltipLayout {
    pub state: Option<TooltipLayoutState>,
}

#[derive(Clone, Copy)]
pub struct TooltipLayoutState {
    pub pos_spec: Vec2,
    pub size_spec: Vec2,
    /// `Some(tags)` only when the hovered source is a turret. The
    /// banner system iterates this and stacks one section per tag
    /// so multi-tag weapons (e.g. Harpoon = Pirate + Melee) show
    /// every synergy they participate in.
    pub tags: Option<&'static [WeaponTag]>,
}

/// Marker on the invisible hit area sitting under the SCRAP counter in
/// the customize overlay. Hovering it shows a fixed explainer tooltip
/// describing how scrap is earned. No dynamic state — the title and
/// body are constants.
#[derive(Component, Clone, Copy)]
pub struct ScrapTooltipHover;

/// Two queries the tooltip update reads to detect hovers over
/// non-drag-source UI: the stats column and the scrap counter. Bundled
/// as a `SystemParam` so the parent system stays inside Bevy's 16-arg
/// `IntoSystem` cap.
#[derive(SystemParam)]
pub struct HoverQueries<'w, 's> {
    pub stat_hovers: Query<'w, 's,
        (&'static Transform, &'static HitArea, &'static super::stats_panel::StatHover),
        Without<DragSourceMarker>,
    >,
    pub scrap_hovers: Query<'w, 's,
        (&'static Transform, &'static HitArea),
        (With<ScrapTooltipHover>, Without<DragSourceMarker>, Without<super::stats_panel::StatHover>),
    >,
}

/// Minimum tooltip box dims in spec pixels — the box grows beyond this
/// when the body/title text needs more space. Multiplied by
/// `display_scale` to get the native-pixel size each frame.
const TOOLTIP_MIN_W: f32 = 48.0;
const TOOLTIP_H: f32 = 22.0;
/// Spec-pixel gap between the hovered source and the tooltip edge.
const TOOLTIP_GAP: f32 = 2.0;
/// Native-pixel padding between the text bounds and the fill edge.
/// Split horizontal vs vertical so the box can stay short while
/// keeping the text comfortably away from the side outlines.
const TOOLTIP_TEXT_PAD_X: f32 = 14.0;
const TOOLTIP_TEXT_PAD_Y: f32 = 6.0;
/// Native-pixel thickness of the white outline ring around the fill.
const TOOLTIP_BORDER_PX: f32 = 2.0;
/// Title + body font sizes (native pixels). Sized to integer multiples
/// of 8 so PixelOperator8's bitmap glyph design samples cleanly — at
/// non-multiples (e.g. 14 or 18) the font resamples and the edges
/// blur. PixelOperator8 renders ~33% taller per nominal `font_size`
/// than the regular cut, so these are one grid step smaller than they
/// were when the regular cut was loaded.
const TOOLTIP_TITLE_FONT: f32 = 16.0;
const TOOLTIP_BODY_FONT: f32 = 12.0;
/// Native-pixel cap on body text width — body wraps at word boundaries
/// when it would exceed this. Generous: prefer a wide single-line box
/// over wrapping to multiple short lines.
const TOOLTIP_BODY_MAX_W: f32 = 380.0;
/// Approx char width (chars × font_size × this ≈ rendered native width).
/// Used both for the title's auto-fit and the body's line-count estimate.
/// The default font has variable glyph width — capitals like "M" and
/// "W" exceed this average — so an over-conservative value is better
/// than a tight one: it prevents body text overrunning the fill on
/// long descriptions (e.g. "Min HP", "Frost", "Furthest") at the cost
/// of slightly wider tooltips than strictly needed.
const TOOLTIP_CHAR_W: f32 = 0.72;
/// Vertical line-height multiplier for the wrapped body — turns
/// `body_font * lines` into the total body block height. Bevy text
/// adds a near-constant leading per line on top of the font size, so
/// at small font sizes (12) that fixed leading dominates more of the
/// per-line height than at the originally-tuned 24. 1.3 keeps the
/// footer line inside the box at body=12 without leaving towers of
/// whitespace at the larger sizes.
const TOOLTIP_LINE_HEIGHT_MULT: f32 = 1.3;

/// Native-pixel font size for the synergy banner text. Matched to
/// `TOOLTIP_BODY_FONT` so the banner reads as a natural sibling
/// of the main weapon tooltip rather than a smaller annotation.
const SYNERGY_FONT: f32 = TOOLTIP_BODY_FONT;
/// Native-pixel padding around the banner text (left/right) — tight,
/// since the banner is a single line.
const SYNERGY_TEXT_PAD_X: f32 = 10.0;
/// Native-pixel padding above/below the banner text.
const SYNERGY_TEXT_PAD_Y: f32 = 6.0;
/// Spec-pixel gap between the main tooltip's top edge and the
/// banner's bottom edge.
const SYNERGY_GAP: f32 = 2.0;
/// Native-pixel cap on banner text width. Wide enough that the
/// Support descriptor ("Boosts every neighbour that is not Support")
/// fits without overflowing the box, and the milestone ladder rows
/// stay on a single line each.
const SYNERGY_TEXT_MAX_W: f32 = 600.0;
/// Dim colour for inactive-tier values in the banner.
const SYNERGY_INACTIVE_COLOR: Color = Color::srgb(0.45, 0.48, 0.55);
/// Bright colour for the active-tier value.
const SYNERGY_ACTIVE_COLOR: Color = Color::srgb(1.00, 0.95, 0.55);
/// Colour for the trailing descriptor (`dmg`, `rate Future`, etc.).
const SYNERGY_DESC_COLOR: Color = Color::srgb(0.85, 0.88, 0.94);

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

pub fn spawn_customize_tooltip(commands: &mut Commands, font: &crate::fonts::PixelFont) {
    // ---------- Synergy banner (stacked above the main tooltip) ----------
    // Same outline+fill+text layout as the main tooltip, just compact
    // (single text line). Sizes are placeholders — `update_customize_tooltip`
    // rewrites them every frame from the active banner string.
    commands.spawn((
        Sprite {
            color: Color::WHITE,
            custom_size: Some(Vec2::new(TOOLTIP_MIN_W, TOOLTIP_H)),
            ..default()
        },
        Transform::from_xyz(0.0, 0.0, Z_TOOLTIP_OUTLINE),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        SynergyBannerOutline,
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
        SynergyBannerFill,
    ));
    commands.spawn((
        Text2d::new(""),
        crate::fonts::pixel_text_font(font, SYNERGY_FONT),
        TextColor(SYNERGY_DESC_COLOR),
        // Wrap-friendly: justify centred, cap horizontal width so
        // long descriptors break to a second line rather than
        // overflowing the box.
        TextLayout::new_with_justify(JustifyText::Center),
        TextBounds::new_horizontal(SYNERGY_TEXT_MAX_W),
        Anchor::Center,
        Transform::from_xyz(0.0, 0.0, Z_TOOLTIP_TEXT),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        SynergyBannerText,
    ));

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
        crate::fonts::pixel_text_font(font, TOOLTIP_TITLE_FONT),
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
        crate::fonts::pixel_text_font(font, TOOLTIP_BODY_FONT),
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
    data: TooltipDataCtx,
    viewport: Res<CustomizeViewport>,
    ui_scale: Res<bevy::ui::UiScale>,
    mut layout: ResMut<TooltipLayout>,
    mut body_cache: Local<String>,
    body_entity_q: Query<Entity, With<CustomizeTooltipBody>>,
    existing_body_spans: Query<Entity, With<CustomizeTooltipBodySpan>>,
    sources: Query<(&Transform, &HitArea, &DragSourceMarker)>,
    hovers: HoverQueries,
    mut outline_q: Query<
        (&mut Visibility, &mut Transform, &mut Sprite),
        (
            With<CustomizeTooltipOutline>,
            Without<CustomizeTooltipFill>,
            Without<CustomizeTooltipTitle>,
            Without<CustomizeTooltipBody>,
            Without<DragSourceMarker>,
            Without<super::stats_panel::StatHover>,
            Without<ScrapTooltipHover>,
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
            Without<ScrapTooltipHover>,
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
            Without<ScrapTooltipHover>,
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
            Without<ScrapTooltipHover>,
        ),
    >,
) {
    let hide = !open.open || drag.picked.is_some() || drag.spec_cursor.is_none();
    if hide {
        hide_all(&mut outline_q, &mut fill_q, &mut title_q, &mut body_q);
        layout.state = None;
        return;
    }
    let cursor = drag.spec_cursor.unwrap();
    let shop_ref = data.shop.as_deref();

    // `info` tracks the hovered target's (title, body, centre, half-extent);
    // `info_tags` tracks every WEAPON TAG when the hovered source is a turret
    // (shop slot or ship slot). Multi-tag weapons (e.g. Harpoon) carry
    // multiple tags so the banner can stack one section per tag. Runes
    // and stat rows leave it `None`, which hides the synergy banner.
    let mut info: Option<(String, String, Vec2, Vec2)> = None;
    let mut info_tags: Option<&'static [WeaponTag]> = None;
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
        if let Some((title, body)) = describe_source(
            marker.0, &data.cfg, shop_ref, &data.discovered, &data.stats, &data.synergies,
            &data.damage_stats,
        ) {
            info = Some((title, body, centre, half));
            info_tags = turret_tags_for_source(marker.0, &data.cfg, shop_ref);
            best_area = area;
        }
    }
    // Stat-row hovers — same smallest-hit-area selection, so a row only
    // wins over a turret/rune if the cursor is exclusively on the row.
    for (tf, hit, hover) in &hovers.stat_hovers {
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
        // Stat rows never get a synergy banner — they're stats, not turrets.
        info_tags = None;
        best_area = area;
    }
    // Scrap-counter hover — fixed explainer describing how scrap is earned.
    for (tf, hit) in &hovers.scrap_hovers {
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
            "SCRAP".to_string(),
            "\u{1F}+1 per wave cleared. +1 interest per 5 scrap held coming into a stage. Boss kills pay a bounty. Harvest stat rolls drops on kills (Pirate multiplies).".to_string(),
            centre,
            half,
        ));
        info_tags = None;
        best_area = area;
    }

    let Some((title, body, source_centre, source_half)) = info else {
        hide_all(&mut outline_q, &mut fill_q, &mut title_q, &mut body_q);
        layout.state = None;
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
    let fill_w_native = (text_w_native + 2.0 * TOOLTIP_TEXT_PAD_X).max(TOOLTIP_MIN_W * s);
    // Line-count estimate: how many `TOOLTIP_BODY_MAX_W` slabs the
    // unwrapped body needs. `ceil(body_w / max_w)`, min 1. Plus the
    // count of explicit `\n` in the body (e.g. the `[TAG]\n…` chip
    // line) — those force a break that the width-only calculation
    // would otherwise miss, leading to truncated short tooltips.
    let explicit_breaks = body.chars().filter(|&c| c == '\n').count() as f32;
    let body_lines = (body_unwrapped_w / TOOLTIP_BODY_MAX_W).ceil().max(1.0) + explicit_breaks;
    let body_block_h = body_lines * TOOLTIP_BODY_FONT * TOOLTIP_LINE_HEIGHT_MULT;
    let title_block_h = TOOLTIP_TITLE_FONT * TOOLTIP_LINE_HEIGHT_MULT;
    let fill_h_native = title_block_h + body_block_h + 2.0 * TOOLTIP_TEXT_PAD_Y;
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
    let fill_top_native = native_centre.y + fill_h_native * 0.5 - TOOLTIP_TEXT_PAD_Y;
    // Glyph scale follows `UiScale` (window-relative, matches bevy_ui
    // chrome). Positions are pre-multiplied by `display_scale`
    // (~4× at design) to land in the customize sprite's screen rect,
    // but glyphs use `UiScale` (1.0 at design) so they don't render
    // four times too big.
    let glyph_scale = ui_scale.0;
    let want_text_scale = Vec3::new(glyph_scale, glyph_scale, 1.0);
    if let Ok((mut v, mut tf, mut text)) = title_q.single_mut() {
        if *v != Visibility::Inherited {
            *v = Visibility::Inherited;
        }
        tf.translation.x = native_centre.x;
        tf.translation.y = fill_top_native;
        if tf.scale != want_text_scale { tf.scale = want_text_scale; }
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
        if tf.scale != want_text_scale { tf.scale = want_text_scale; }
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
                        crate::fonts::pixel_text_font(&data.pixel_font, TOOLTIP_BODY_FONT),
                        TextColor(color),
                        CustomizeTooltipBodySpan,
                    ));
                }
            });
        }
    }

    // Publish the layout so `update_synergy_banner` can stack
    // its banner above (or below) the main tooltip without
    // duplicating any of the sizing/positioning math.
    layout.state = Some(TooltipLayoutState {
        pos_spec: pos,
        size_spec: Vec2::new(tooltip_w_spec, tooltip_h_spec),
        tags: info_tags,
    });
}

/// Reads the latest main-tooltip layout from `TooltipLayout` and
/// renders the synergy banner stacked above it (or below, if there's
/// no room above). Hidden whenever `layout.state` is None or the
/// hovered source carries no `WeaponTag` (runes, stat rows). Split
/// out of `update_customize_tooltip` because the combined system
/// would have exceeded Bevy's max-param count for `IntoSystem`.
pub fn update_synergy_banner(
    mut commands: Commands,
    layout: Res<TooltipLayout>,
    viewport: Res<CustomizeViewport>,
    ui_scale: Res<bevy::ui::UiScale>,
    pixel_font: Res<crate::fonts::PixelFont>,
    synergies: Res<Synergies>,
    discovered: Res<crate::onboarding::DiscoveredSynergies>,
    mut banner_cache: Local<String>,
    banner_entity_q: Query<Entity, With<SynergyBannerText>>,
    existing_banner_spans: Query<Entity, With<SynergyBannerSpan>>,
    mut outline_q: Query<
        (&mut Visibility, &mut Transform, &mut Sprite),
        (
            With<SynergyBannerOutline>,
            Without<SynergyBannerFill>,
            Without<SynergyBannerText>,
        ),
    >,
    mut fill_q: Query<
        (&mut Visibility, &mut Transform, &mut Sprite),
        (
            With<SynergyBannerFill>,
            Without<SynergyBannerOutline>,
            Without<SynergyBannerText>,
        ),
    >,
    mut text_q: Query<
        (&mut Visibility, &mut Transform, &mut Text2d, &mut TextBounds),
        (
            With<SynergyBannerText>,
            Without<SynergyBannerOutline>,
            Without<SynergyBannerFill>,
        ),
    >,
) {
    let hide = |outline_q: &mut Query<
        (&mut Visibility, &mut Transform, &mut Sprite),
        (
            With<SynergyBannerOutline>,
            Without<SynergyBannerFill>,
            Without<SynergyBannerText>,
        ),
    >,
        fill_q: &mut Query<
            (&mut Visibility, &mut Transform, &mut Sprite),
            (
                With<SynergyBannerFill>,
                Without<SynergyBannerOutline>,
                Without<SynergyBannerText>,
            ),
        >,
        text_q: &mut Query<
            (&mut Visibility, &mut Transform, &mut Text2d, &mut TextBounds),
            (
                With<SynergyBannerText>,
                Without<SynergyBannerOutline>,
                Without<SynergyBannerFill>,
            ),
        >| {
        if let Ok((mut v, _, _)) = outline_q.single_mut() {
            if *v != Visibility::Hidden { *v = Visibility::Hidden; }
        }
        if let Ok((mut v, _, _)) = fill_q.single_mut() {
            if *v != Visibility::Hidden { *v = Visibility::Hidden; }
        }
        if let Ok((mut v, _, _, _)) = text_q.single_mut() {
            if *v != Visibility::Hidden { *v = Visibility::Hidden; }
        }
    };

    let Some(state) = layout.state else {
        hide(&mut outline_q, &mut fill_q, &mut text_q);
        return;
    };
    let Some(all_tags) = state.tags else {
        hide(&mut outline_q, &mut fill_q, &mut text_q);
        return;
    };
    // Multi-tag weapons stack one section per DISCOVERED tag. Hidden
    // chips (`[???]` in the main tooltip body) don't reveal their
    // ladder until 2 of that tag have been equipped. So filter down
    // to the discovered tags only; if none, hide entirely.
    let visible_tags: Vec<WeaponTag> = all_tags
        .iter()
        .copied()
        .filter(|t| discovered.has(*t))
        .collect();
    if visible_tags.is_empty() {
        hide(&mut outline_q, &mut fill_q, &mut text_q);
        return;
    }

    // Build a per-tag section descriptor — pre-computed so the
    // dimension math and the span-rendering both see the same data
    // without re-deriving from the tag enum twice.
    struct Section {
        tag: WeaponTag,
        description: &'static str,
        values: [&'static str; 4],
        descriptor: &'static str,
        tier: u8,
        header_plain: String,
        ladder_rows_plain: Vec<String>,
    }
    let sections: Vec<Section> = visible_tags
        .iter()
        .map(|&tag| {
            let (values, descriptor) = synergy_ladder(tag);
            let description = synergy_description(tag);
            let header_plain = format!("[{}] {}", tag.label(), description);
            let ladder_rows_plain = values
                .iter()
                .enumerate()
                .map(|(i, v)| format!("({}) {} {}", (i + 1) * 2, v, descriptor))
                .collect();
            Section {
                tag,
                description,
                values,
                descriptor,
                tier: active_tier(tag, &synergies),
                header_plain,
                ladder_rows_plain,
            }
        })
        .collect();

    // Plain-text mirror used by the change-detection cache key.
    let banner_text_plain = sections
        .iter()
        .map(|s| {
            let mut block = s.header_plain.clone();
            for row in &s.ladder_rows_plain {
                block.push('\n');
                block.push_str(row);
            }
            block
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let s = viewport.display_scale;
    let glyph_pre = ui_scale.0.max(0.0001);
    // `estimate_text_native_width` returns the text's LOCAL width
    // (pre Transform.scale). Multiply by glyph to get the actual
    // rendered width in screen pixels, which is what the box outline
    // is sized in.
    let widest_unwrapped_visual = sections
        .iter()
        .flat_map(|sec| {
            std::iter::once(estimate_text_native_width(&sec.header_plain, SYNERGY_FONT))
                .chain(
                    sec.ladder_rows_plain
                        .iter()
                        .map(|r| estimate_text_native_width(r, SYNERGY_FONT)),
                )
        })
        .fold(0.0_f32, f32::max) * glyph_pre;
    // Dynamic wrap cap — fit the box to content, capped at how much
    // of the customize canvas is actually available for it. The
    // `SYNERGY_TEXT_MAX_W` constant remains as an upper safety bound
    // so the box can never exceed it even on enormous windows.
    let canvas_half_w_pre = CUSTOMIZE_INTERNAL_W as f32 * 0.5;
    let canvas_margin_native = 12.0 * s;
    let canvas_available_native = (canvas_half_w_pre * 2.0 * s - canvas_margin_native).max(60.0);
    let max_text_w_native = (canvas_available_native - 2.0 * SYNERGY_TEXT_PAD_X)
        .min(SYNERGY_TEXT_MAX_W)
        .max(60.0);
    let banner_text_w = widest_unwrapped_visual.min(max_text_w_native);
    let banner_fill_w_native = banner_text_w + 2.0 * SYNERGY_TEXT_PAD_X;
    // Line count uses VISUAL widths against the visual wrap cap.
    let lines_per_section: Vec<f32> = sections
        .iter()
        .map(|sec| {
            let h_w_visual = estimate_text_native_width(&sec.header_plain, SYNERGY_FONT) * glyph_pre;
            let header_lines = (h_w_visual / banner_text_w.max(1.0)).ceil().max(1.0);
            header_lines + 4.0
        })
        .collect();
    let section_lines_total: f32 = lines_per_section.iter().sum();
    let dividers = sections.len().saturating_sub(1) as f32;
    let total_lines = section_lines_total + dividers;
    // Each rendered line is `SYNERGY_FONT * glyph * LINE_HEIGHT_MULT`
    // tall in screen pixels — multiply by glyph so the box height
    // grows / shrinks with `UiScale`.
    let banner_fill_h_native = total_lines * SYNERGY_FONT * glyph_pre * TOOLTIP_LINE_HEIGHT_MULT
        + 2.0 * SYNERGY_TEXT_PAD_Y;
    let banner_w_spec = banner_fill_w_native / s;
    let banner_h_spec = banner_fill_h_native / s;

    let canvas_half_w = CUSTOMIZE_INTERNAL_W as f32 * 0.5;
    let canvas_half_h = CUSTOMIZE_INTERNAL_H as f32 * 0.5;
    // Stack the banner BELOW the weapon tooltip by default (info
    // is supplementary, sits underneath). Auto-flip ABOVE if the
    // bottom edge would clip the canvas. Horizontally centred on
    // the weapon tooltip - never to the side, so it can never run
    // off the canvas's right edge on a tooltip already pushed to
    // the right of the source.
    let mut banner_pos = Vec2::new(
        state.pos_spec.x,
        state.pos_spec.y - state.size_spec.y * 0.5 - SYNERGY_GAP - banner_h_spec * 0.5,
    );
    // Margin so the banner outline doesn't kiss the canvas edge —
    // text-wrap estimates can undercount by a line at the boundary
    // case, which would otherwise punch a few pixels of the bottom
    // out of the customize sprite.
    let canvas_margin = 6.0;
    let safe_half_w = canvas_half_w - canvas_margin;
    let safe_half_h = canvas_half_h - canvas_margin;
    if banner_pos.y - banner_h_spec * 0.5 < -safe_half_h {
        banner_pos.y = state.pos_spec.y + state.size_spec.y * 0.5 + SYNERGY_GAP + banner_h_spec * 0.5;
    }
    banner_pos.x = banner_pos.x.clamp(
        -safe_half_w + banner_w_spec * 0.5,
        safe_half_w - banner_w_spec * 0.5,
    );
    banner_pos.y = banner_pos.y.clamp(
        -safe_half_h + banner_h_spec * 0.5,
        safe_half_h - banner_h_spec * 0.5,
    );
    let banner_native_centre = Vec2::new(banner_pos.x * s, banner_pos.y * s);
    let banner_fill_native = Vec2::new(banner_fill_w_native, banner_fill_h_native);
    let banner_outline_native = banner_fill_native + Vec2::splat(2.0 * TOOLTIP_BORDER_PX);

    if let Ok((mut v, mut tf, mut sprite)) = outline_q.single_mut() {
        if *v != Visibility::Inherited { *v = Visibility::Inherited; }
        tf.translation.x = banner_native_centre.x;
        tf.translation.y = banner_native_centre.y;
        if sprite.custom_size != Some(banner_outline_native) {
            sprite.custom_size = Some(banner_outline_native);
        }
    }
    if let Ok((mut v, mut tf, mut sprite)) = fill_q.single_mut() {
        if *v != Visibility::Inherited { *v = Visibility::Inherited; }
        tf.translation.x = banner_native_centre.x;
        tf.translation.y = banner_native_centre.y;
        if sprite.custom_size != Some(banner_fill_native) {
            sprite.custom_size = Some(banner_fill_native);
        }
    }
    if let Ok((mut v, mut tf, mut text, mut bounds)) = text_q.single_mut() {
        if *v != Visibility::Inherited { *v = Visibility::Inherited; }
        tf.translation.x = banner_native_centre.x;
        tf.translation.y = banner_native_centre.y;
        // Glyph scale follows `UiScale` (matches bevy_ui chrome) —
        // see comment in `sync_customize_text`.
        let glyph = ui_scale.0;
        let want_scale = Vec3::new(glyph, glyph, 1.0);
        if tf.scale != want_scale { tf.scale = want_scale; }
        if !text.0.is_empty() { text.0 = String::new(); }
        // TextBounds is in the text's LOCAL coords (pre-scale). To
        // wrap at `banner_text_w` screen pixels we have to divide by
        // the glyph scale, otherwise the visual wrap point sits at
        // `banner_text_w * glyph` and the rendered text spills out
        // past the box outline.
        let want_bounds_w = Some((banner_text_w / glyph.max(0.0001)).max(20.0));
        if bounds.width != want_bounds_w {
            bounds.width = want_bounds_w;
        }
    }

    // Rebuild spans when any section's tag/tier/text changes. Key
    // includes every section's (tag, tier) so a tier-up on either
    // half of a multi-tag banner re-renders without missing it.
    let banner_key = {
        let sigs: Vec<String> = sections
            .iter()
            .map(|s| format!("{}:{}", s.tag.label(), s.tier))
            .collect();
        format!("{}|{}", sigs.join(","), banner_text_plain)
    };
    if *banner_cache != banner_key {
        *banner_cache = banner_key;
        for span in existing_banner_spans.iter() {
            commands.entity(span).despawn();
        }
        if let Ok(banner_entity) = banner_entity_q.single() {
            commands.entity(banner_entity).with_children(|p| {
                for (sec_idx, sec) in sections.iter().enumerate() {
                    // Blank-line divider between sections — only
                    // emitted BEFORE second-and-later sections so
                    // the first header sits flush at the top of
                    // the banner.
                    if sec_idx > 0 {
                        p.spawn((
                            TextSpan::new("\n\n".to_string()),
                            crate::fonts::pixel_text_font(&pixel_font, SYNERGY_FONT),
                            TextColor(SYNERGY_DESC_COLOR),
                            SynergyBannerSpan,
                        ));
                    }
                    // ---- Header line: tag chip + full description ----
                    p.spawn((
                        TextSpan::new(format!("[{}] ", sec.tag.label())),
                        crate::fonts::pixel_text_font(&pixel_font, SYNERGY_FONT),
                        TextColor(sec.tag.color()),
                        SynergyBannerSpan,
                    ));
                    // Description can itself contain inline chips
                    // (e.g. "[MELEE] kills heal your hull"). Split
                    // into coloured segments so the chip renders in
                    // its tag colour rather than as plain bracket
                    // text in the dim desc colour.
                    for (segment, colour) in colorize_banner_description(sec.description) {
                        p.spawn((
                            TextSpan::new(segment),
                            crate::fonts::pixel_text_font(&pixel_font, SYNERGY_FONT),
                            TextColor(colour),
                            SynergyBannerSpan,
                        ));
                    }
                    p.spawn((
                        TextSpan::new("\n".to_string()),
                        crate::fonts::pixel_text_font(&pixel_font, SYNERGY_FONT),
                        TextColor(SYNERGY_DESC_COLOR),
                        SynergyBannerSpan,
                    ));
                    // ---- Vertical ladder: one row per tier ----
                    for (i, v) in sec.values.iter().enumerate() {
                        let active = (sec.tier as usize) == i + 1;
                        let count_color = if active { SYNERGY_ACTIVE_COLOR } else { SYNERGY_INACTIVE_COLOR };
                        let value_color = if active { SYNERGY_ACTIVE_COLOR } else { SYNERGY_INACTIVE_COLOR };
                        let desc_color = if active { SYNERGY_DESC_COLOR } else { SYNERGY_INACTIVE_COLOR };
                        let count = (i as u32 + 1) * 2;
                        p.spawn((
                            TextSpan::new(format!("({}) ", count)),
                            crate::fonts::pixel_text_font(&pixel_font, SYNERGY_FONT),
                            TextColor(count_color),
                            SynergyBannerSpan,
                        ));
                        p.spawn((
                            TextSpan::new(format!("{} ", v)),
                            crate::fonts::pixel_text_font(&pixel_font, SYNERGY_FONT),
                            TextColor(value_color),
                            SynergyBannerSpan,
                        ));
                        p.spawn((
                            TextSpan::new(sec.descriptor.to_string()),
                            crate::fonts::pixel_text_font(&pixel_font, SYNERGY_FONT),
                            TextColor(desc_color),
                            SynergyBannerSpan,
                        ));
                        if i < sec.values.len() - 1 {
                            p.spawn((
                                TextSpan::new("\n".to_string()),
                                crate::fonts::pixel_text_font(&pixel_font, SYNERGY_FONT),
                                TextColor(SYNERGY_DESC_COLOR),
                                SynergyBannerSpan,
                            ));
                        }
                    }
                }
            });
        }
    }
}

/// Resolve a drag-source to the weapon's tag list — `Some` only for
/// turrets (ship or shop). Returns the full slice so multi-tag
/// weapons (e.g. Harpoon) get every synergy banner stacked, not
/// just the primary tag's. Runes and stat rows return `None`, so
/// the synergy banner stays hidden over them.
fn turret_tags_for_source(
    source: DragSourceKind,
    cfg: &TurretConfig,
    shop: Option<&CustomizeShop>,
) -> Option<&'static [WeaponTag]> {
    match source {
        DragSourceKind::ShipSlot(slot) => {
            let s = cfg.slots[slot];
            if !s.equipped { return None; }
            Some(s.weapon.tags())
        }
        DragSourceKind::ShopTurret(idx) => shop
            .and_then(|s| s.turrets.get(idx))
            .and_then(|o| o.as_ref())
            .map(|o| o.weapon.tags()),
        DragSourceKind::ShipRune { .. } | DragSourceKind::ShopRune(_) => None,
    }
}

/// Default body color (matches the root `Text2d`'s `TextColor`).
const TOOLTIP_BODY_COLOR: Color = Color::srgb(0.85, 0.88, 0.94);
/// Tint for positive numeric tokens (`+30%`, `+1`).
const TOOLTIP_BUFF_COLOR: Color = crate::ui_kit::theme::BUFF_FG;
/// Tint for negative numeric tokens (`-50`, `-70%`).
const TOOLTIP_NERF_COLOR: Color = crate::ui_kit::theme::NERF_FG;
/// Tint for bare numeric tokens (`5`, `4.0/s`, `100%`) so stat values
/// pop against their labels in weapon / rune descriptions. Gold to
/// match the scrap accent the player already associates with
/// "important number".
const TOOLTIP_VALUE_COLOR: Color = Color::srgb(1.00, 0.85, 0.30);
/// Fallback chip colour for any `[XXX]` tag that isn't a known
/// `WeaponTag` — currently only `[AOE]` (used by mortar / Splash rune).
/// Picked to read as "informational tag" rather than buff/nerf.
const TOOLTIP_AOE_TAG_COLOR: Color = Color::srgb(1.00, 0.55, 0.20);

/// Look up the chip colour for a bracketed tag name (the text between
/// `[ ]`). Iterates `WeaponTag::all()` so adding a new tag in
/// `weapon.rs` automatically gets a coloured chip with no edits here.
/// Returns `None` for unknown tag names — caller falls back to the
/// default body color so unrecognised tags still render as plain text.
/// Split synergy-banner description text into coloured segments,
/// painting `[NAME]` tokens as tag chips and leaving everything
/// else in the banner's default desc colour. Lighter sibling of
/// `colorize_bonuses` — banner text only has chips, no `+X%` /
/// `-X%` tokens, and the body colour is `SYNERGY_DESC_COLOR` not
/// `TOOLTIP_BODY_COLOR`, so we don't share the implementation.
fn colorize_banner_description(text: &str) -> Vec<(String, Color)> {
    let mut segments: Vec<(String, Color)> = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let at_word_start = i == 0
            || chars[i - 1].is_whitespace()
            || chars[i - 1] == ']';
        if c == '[' && at_word_start {
            if let Some(close_off) = chars[i + 1..].iter().position(|&ch| ch == ']') {
                let close = i + 1 + close_off;
                let name: String = chars[i + 1..close].iter().collect();
                if let Some(color) = tag_chip_color(&name) {
                    if !current.is_empty() {
                        segments.push((std::mem::take(&mut current), SYNERGY_DESC_COLOR));
                    }
                    segments.push((format!("[{}]", name), color));
                    i = close + 1;
                    continue;
                }
            }
        }
        current.push(c);
        i += 1;
    }
    if !current.is_empty() {
        segments.push((current, SYNERGY_DESC_COLOR));
    }
    segments
}

fn tag_chip_color(name: &str) -> Option<Color> {
    for &tag in WeaponTag::all() {
        if tag.label() == name {
            return Some(tag.color());
        }
    }
    if name == "AOE" {
        return Some(TOOLTIP_AOE_TAG_COLOR);
    }
    // Targeting-rune chip — informational tag underneath the rune
    // name. Cool slate-grey so it reads as a meta-category, distinct
    // from elemental colours used by Fire/Frost/Shock.
    if name == "TARGET" {
        return Some(Color::srgb(0.70, 0.80, 0.95));
    }
    // `[???]` is rendered in dim grey — it's the "synergy not yet
    // discovered" placeholder used by `turret_tooltip` when the
    // player hasn't equipped 2 of this tag this run.
    if name == "???" {
        return Some(SYNERGY_INACTIVE_COLOR);
    }
    None
}

/// Split body text into colored segments. A run starting with `+`
/// or `-` immediately followed by a digit (optionally with a
/// trailing `%`) is a buff/nerf token and gets the corresponding
/// tint. A `[NAME]` chip (only at the start of a word) is coloured
/// per `tag_chip_color`. Everything else stays the default body color.
fn colorize_bonuses(text: &str) -> Vec<(String, Color)> {
    // `\x1F` (unit separator) acts as a "switch to gray footer" marker.
    // Everything before it goes through normal colorisation; everything
    // after is rendered as one flat-gray segment matching the `[???]`
    // tint, so the per-slot damage footer reads as auxiliary info
    // rather than competing with buff/nerf numbers above it.
    if let Some(split) = text.find('\u{1F}') {
        let (head, tail) = text.split_at(split);
        let mut segments = colorize_bonuses(head);
        let footer = &tail[1..];
        if !footer.is_empty() {
            segments.push((footer.to_string(), SYNERGY_INACTIVE_COLOR));
        }
        return segments;
    }
    let mut segments: Vec<(String, Color)> = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let is_sign_token = (c == '+' || c == '-')
            && i + 1 < chars.len()
            && chars[i + 1].is_ascii_digit();
        // Bare numeric tokens (no leading +/-) get the stat-value
        // colour so labels and their values render in different
        // tints on lines like "Damage 5" or "Fire rate 4.0/s".
        let is_bare_number = c.is_ascii_digit()
            && (i == 0 || chars[i - 1].is_whitespace());
        // `[XXX]` tag chip — only recognised at the very start of the
        // body, right after whitespace, or directly after a previous
        // chip's closing `]` (the multi-tag `[PIRATE][MELEE]` case),
        // so a stray bracket inside sentence text doesn't
        // accidentally get coloured. The chip text itself includes
        // the brackets so the rendered output reads e.g.
        // `[NAVAL] Balanced cannon...`.
        let at_word_start = i == 0
            || chars[i - 1].is_whitespace()
            || chars[i - 1] == ']';
        if c == '[' && at_word_start {
            if let Some(close_off) = chars[i + 1..].iter().position(|&ch| ch == ']') {
                let close = i + 1 + close_off;
                let name: String = chars[i + 1..close].iter().collect();
                if let Some(color) = tag_chip_color(&name) {
                    if !current.is_empty() {
                        segments.push((std::mem::take(&mut current), TOOLTIP_BODY_COLOR));
                    }
                    let mut chip = String::new();
                    chip.push('[');
                    chip.push_str(&name);
                    chip.push(']');
                    segments.push((chip, color));
                    i = close + 1;
                    continue;
                }
            }
        }
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
        } else if is_bare_number {
            if !current.is_empty() {
                segments.push((std::mem::take(&mut current), TOOLTIP_BODY_COLOR));
            }
            let mut tok = String::new();
            // Integer part.
            while i < chars.len() && chars[i].is_ascii_digit() {
                tok.push(chars[i]);
                i += 1;
            }
            // Optional decimal part.
            if i + 1 < chars.len()
                && chars[i] == '.'
                && chars[i + 1].is_ascii_digit()
            {
                tok.push('.');
                i += 1;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    tok.push(chars[i]);
                    i += 1;
                }
            }
            // Suffix family: %, /s, deg. Greedy match so "4.0/s"
            // and "100%" stay inside the value segment instead of
            // splitting across colour boundaries.
            if i < chars.len() && chars[i] == '%' {
                tok.push('%');
                i += 1;
            } else if i + 1 < chars.len() && chars[i] == '/' && chars[i + 1] == 's' {
                tok.push('/');
                tok.push('s');
                i += 2;
            }
            segments.push((tok, TOOLTIP_VALUE_COLOR));
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
            Without<ScrapTooltipHover>,
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
            Without<ScrapTooltipHover>,
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
            Without<ScrapTooltipHover>,
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
            Without<ScrapTooltipHover>,
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
    discovered: &crate::onboarding::DiscoveredSynergies,
    stats: &crate::stats::PlayerStats,
    synergies: &Synergies,
    damage_stats: &crate::ui::DamageStats,
) -> Option<(String, String)> {
    match source {
        DragSourceKind::ShipSlot(slot) => {
            let s = cfg.slots[slot];
            if !s.equipped {
                return None;
            }
            // `turret_tooltip` iterates each tag and renders a chip
            // for each one, gated by per-tag discovery. So a Harpoon
            // with only Pirate discovered shows `[PIRATE][???]`.
            let (title, mut body) = turret_tooltip(
                s.weapon, s.barrels.max(1), discovered, stats, synergies,
                Some((damage_stats.per_slot[slot], damage_stats.total)),
            );
            if let Some(extra) = weapon_slot_context_line(s.weapon, slot, cfg) {
                body.push('\n');
                body.push_str(&extra);
            }
            Some((title, body))
        }
        DragSourceKind::ShipRune { slot, rune_idx } => {
            let s = cfg.slots[slot];
            if !s.equipped {
                return None;
            }
            // Pass the slot's full rune array so Blast can fold in any
            // sibling Splash runes when displaying its splash radius.
            s.runes[rune_idx].map(|r| rune_tooltip(r, stats, Some(&s.runes)))
        }
        DragSourceKind::ShopTurret(idx) => shop
            .and_then(|s| s.turrets.get(idx))
            .and_then(|o| o.as_ref())
            .map(|o| turret_tooltip(
                o.weapon, o.barrels.max(1), discovered, stats, synergies, None,
            )),
        DragSourceKind::ShopRune(idx) => shop
            .and_then(|s| s.runes.get(idx))
            .and_then(|o| o.as_ref())
            .copied()
            // No slot context yet — the player hasn't placed it. Blast
            // will display its base radius.
            .map(|r| rune_tooltip(r, stats, None)),
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

/// Compact 4-tier ladder values + trailing descriptor for a tag.
/// Used by the synergy banner to render `V1/V2/V3/V4 desc` with
/// the active tier highlighted. Mirrors `synergy.rs`'s ladder
/// table — keep in sync when bonus numbers move.
fn synergy_ladder(tag: WeaponTag) -> ([&'static str; 4], &'static str) {
    match tag {
        WeaponTag::Naval      => (["10%", "20%", "30%", "40%"], "global damage"),
        WeaponTag::Future     => (["0.1s", "0.2s", "0.3s", "0.4s"], "stun on every hit"),
        WeaponTag::Autonomous => (["10%", "20%", "30%", "40%"], "fire rate and movement"),
        WeaponTag::Pirate     => (["+50%", "+100%", "+150%", "+200%"], "scrap drops from kills"),
        WeaponTag::Support    => (["10%", "20%", "30%", "40%"], "fire rate to non-Support turrets"),
        WeaponTag::Melee      => (["+1", "+2", "+3", "+4"], "HP healed per Melee kill"),
    }
}

/// Plain-English explanation of what this synergy does and why the
/// player wants to stack it. Renders above the value ladder in the
/// banner, wrapped at `SYNERGY_TEXT_MAX_W`. Intentionally a full
/// sentence — the prior shorthand ("Auto rate + speed") read as
/// notation rather than gameplay information.
fn synergy_description(tag: WeaponTag) -> &'static str {
    match tag {
        WeaponTag::Naval =>
            "Every Naval turret buffs the damage of every weapon you own. Higher tiers, bigger buff.",
        WeaponTag::Future =>
            "Future weapons stun enemies on hit. Higher tiers freeze them longer.",
        WeaponTag::Autonomous =>
            "Autonomous units fire faster and move faster. Higher tiers push both further.",
        WeaponTag::Pirate =>
            "Every kill drops more scrap. Higher tiers, fatter loot.",
        WeaponTag::Support =>
            "Boosts every neighbour that is not Support. Fire rate first, then damage from tier two onward.",
        WeaponTag::Melee =>
            "[MELEE] kills heal your hull. Higher tiers, bigger heal.",
    }
}

/// Per-tag active tier (0..=4) from the live `Synergies` resource.
fn active_tier(tag: WeaponTag, syn: &Synergies) -> u8 {
    match tag {
        WeaponTag::Naval      => syn.naval,
        WeaponTag::Future     => syn.future,
        WeaponTag::Autonomous => syn.autonomous,
        WeaponTag::Pirate     => syn.pirate,
        WeaponTag::Support    => syn.support,
        WeaponTag::Melee      => syn.melee,
    }
}

fn turret_tooltip(
    weapon: WeaponType,
    barrels: u8,
    discovered: &crate::onboarding::DiscoveredSynergies,
    stats: &crate::stats::PlayerStats,
    synergies: &Synergies,
    damage_share: Option<(u64, u64)>,
) -> (String, String) {
    let title = weapon.label().to_string();
    // Multi-tag weapons render one chip per tag, gated individually
    // by discovery. Undiscovered tags show `[???]`; discovered ones
    // show e.g. `[PIRATE]`. `colorize_bonuses` paints each bracket
    // token in its own colour, so a Harpoon with both tags known
    // reads `[PIRATE][MELEE]` in two distinct colours.
    let mut body = String::new();
    for &tag in weapon.tags() {
        let chip = if discovered.has(tag) { tag.label() } else { "???" };
        body.push_str(&format!("[{}]", chip));
    }
    body.push('\n');
    if matches!(weapon, WeaponType::Mortar) {
        body.push_str(AOE_TAG);
    }
    // Some weapons have tier-dependent descriptions — the static CSV
    // is replaced inline with a live phrasing that uses the slot's
    // current tier (`barrels`).
    match weapon {
        WeaponType::Amplifier => {
            let n = barrels.clamp(1, 3) as u32;
            let word = if n == 1 { "rune" } else { "runes" };
            body.push_str(&format!(
                "Adjacent weapons inherit {} of this Amplifier's {}.",
                n, word,
            ));
        }
        WeaponType::Booster => {
            let pct = (crate::booster::booster_mult_for_tier(barrels) - 1.0) * 100.0;
            body.push_str(&format!(
                "Adjacent turrets fire {:.0}% faster.",
                pct,
            ));
        }
        _ => {
            body.push_str(weapon.description());
        }
    }
    // Brotato-style dynamic stat lines below the flavour. Each line
    // is appended only if non-empty so non-firing weapons don't get
    // a stray "Damage 0.0" or orphan whitespace.
    let dmg_line = weapon_damage_line(weapon, stats, synergies);
    let rate_line = weapon_rate_line(weapon, barrels);
    let extra_line = weapon_extra_line(weapon, stats);
    let mut have_stats_block = false;
    let mut push_stat = |body: &mut String, line: &str| {
        if !have_stats_block {
            body.push_str("\n\n");
            have_stats_block = true;
        } else {
            body.push('\n');
        }
        body.push_str(line);
    };
    if !dmg_line.is_empty() {
        push_stat(&mut body, &dmg_line);
    }
    if let Some(rate) = rate_line.as_deref() {
        push_stat(&mut body, rate);
    }
    if let Some(extra) = extra_line.as_deref() {
        push_stat(&mut body, extra);
    }
    // Per-slot damage-last-round footer. Only present for equipped ship
    // slots (shop turrets have no history). The `\x1F` sentinel splits
    // the body in `colorize_bonuses` so the footer renders entirely in
    // the dim "[???]" gray instead of getting buff/value colorisation.
    if let Some((dealt, total)) = damage_share {
        if total > 0 {
            let pct = (dealt as f32 / total as f32 * 100.0).round() as u32;
            body.push_str(&format!(
                "\n\n\u{1F}Damage last round: {}% ({})",
                pct, dealt,
            ));
        } else {
            body.push_str("\n\n\u{1F}Damage last round: 0% (0)");
        }
    }
    (title, body)
}

fn rune_tooltip(
    rune: Rune,
    stats: &crate::stats::PlayerStats,
    slot_runes: Option<&[Option<Rune>; 3]>,
) -> (String, String) {
    // `slot_runes` still uses the 3-fixed-socket SlotCfg shape — the
    // tooltip caller has a `SlotCfg`, not a `TurretSlot`, so the
    // pre-flatten array stays here for now.
    let mut body = String::new();
    // Targeting runes carry a `[Targeting Mode]` chip on the line
    // beneath the name so the player can tell at a glance that
    // they're picking a target-selection rule, not an effect rune.
    // Colorised via the same `[chip]` token logic as weapon tag
    // chips — falls back to plain text when `colorize_bonuses` can't
    // resolve the chip name (which is fine here since the tag is
    // informational, not gameplay).
    if rune.target_priority().is_some() || matches!(rune, Rune::TargetCarousel) {
        body.push_str("[TARGET]\n");
    }
    // Neither Splash nor Blast gets a prefix `[AOE]` chip — the
    // chip appears inline inside each body sentence instead, so the
    // tag reads as the noun it modifies rather than a standalone
    // header.
    body.push_str(&rune_dynamic_description(rune, stats, slot_runes));
    (rune.label().to_string(), body)
}

/// "Damage X.X" line, expanded with a base + stat breakdown when
/// the player's Turret Damage stat isn't at default. One decimal
/// place so a fractional final (e.g. 4 base x 33% = 5.32 -> 5.3)
/// reads accurately instead of rounding to the same integer as a
/// neighbouring weapon. Base is integer-typed so it shows whole.
fn weapon_damage_line(
    weapon: WeaponType,
    stats: &crate::stats::PlayerStats,
    synergies: &Synergies,
) -> String {
    let (base_dmg, _rate) = weapon.defaults();
    // Non-firing weapons (Booster, Amplifier, SpikedPlate, CrowsNest)
    // have zero base damage — showing "Damage 0.0" is misleading.
    // Callers skip the preceding blank line when this is empty.
    if base_dmg <= 0 {
        return String::new();
    }
    // Fold every multiplier that actually applies in combat — the
    // player's Weapon Damage stat AND any active Naval / Support
    // synergies (Support opts out for Support-tagged weapons via
    // `damage_mult_for`, same logic as `sync_turret_config`). The
    // user's "I have 130% weapon damage but tooltip says 1 damage"
    // bug came from this line only multiplying by
    // `turret_damage_mult` while the panel's headline percentage
    // folded synergies in — the two numbers drifted apart.
    let mult = stats.turret_damage_mult()
        * synergies.damage_mult_for(weapon.tags());
    let final_dmg = base_dmg as f32 * mult;
    let pct_bonus = ((mult - 1.0) * 100.0).round() as i32;
    // Multi-shot weapons (currently SpreadRockets fires 4 rockets per
    // trigger pull) deserve a "DMG × COUNT" headline so the player
    // sees the volley total at a glance rather than the per-projectile
    // damage in isolation.
    let multishot = multishot_count(weapon);
    if multishot > 1 {
        let total = final_dmg * multishot as f32;
        if pct_bonus == 0 {
            return format!(
                "Damage {:.1} x {} ({:.0} total)",
                final_dmg, multishot, total,
            );
        }
        let sign = if pct_bonus > 0 { "+" } else { "" };
        return format!(
            "Damage {:.1} x {} ({:.0} total, {}{}% bonus)",
            final_dmg, multishot, total, sign, pct_bonus,
        );
    }
    if pct_bonus == 0 {
        format!("Damage {:.1}", final_dmg)
    } else {
        let sign = if pct_bonus > 0 { "+" } else { "" };
        format!(
            "Damage {:.1} ({} base, {}{}% bonus)",
            final_dmg, base_dmg, sign, pct_bonus,
        )
    }
}

/// How many projectiles a single trigger pull spawns. 1 for
/// single-shot weapons; matches the firing code's per-shot count
/// for multi-projectile weapons.
fn multishot_count(weapon: WeaponType) -> u8 {
    match weapon {
        WeaponType::SpreadRockets => 4,
        WeaponType::Shotgun => crate::balance::SHOTGUN_PELLETS.min(255) as u8,
        _ => 1,
    }
}

/// "Cooldown Xs" line. Twin / triple barrels alternate on the slot
/// cooldown so effective rate is `base x barrels`; cooldown is the
/// reciprocal. Returns None when the weapon doesn't fire from the
/// deck (Booster).
fn weapon_rate_line(weapon: WeaponType, barrels: u8) -> Option<String> {
    // Flamethrower's `fire_rate` is the internal damage-tick rate, not
    // a meaningful cooldown to the player — what reads as "cooldown"
    // is the reload phase between burns, which shrinks per tier and
    // disappears entirely at T3.
    if matches!(weapon, WeaponType::Flamethrower) {
        return match barrels.clamp(1, 3) {
            1 => Some("Cooldown 3.00s".to_string()),
            2 => Some("Cooldown 1.50s".to_string()),
            _ => Some("Always active".to_string()),
        };
    }
    let (_base_dmg, base_rate) = weapon.defaults();
    if base_rate <= 0.0 { return None; }
    let effective_rate = base_rate * (barrels.max(1) as f32);
    let cooldown = 1.0 / effective_rate;
    Some(format!("Cooldown {:.2}s", cooldown))
}

/// Live, slot-aware context line for weapons whose effect depends
/// on adjacency. Returns `None` for weapons without an adjacency
/// story (most of them). Only invoked for placed slots — shop
/// tooltips skip this since there's no neighbourhood yet.
fn weapon_slot_context_line(
    weapon: WeaponType,
    slot_idx: usize,
    cfg: &TurretConfig,
) -> Option<String> {
    match weapon {
        WeaponType::Amplifier => {
            let n = crate::balance::TURRET_ADJACENCY[slot_idx]
                .iter()
                .filter(|&&i| {
                    cfg.slots[i].equipped
                        && !matches!(cfg.slots[i].weapon, WeaponType::Amplifier)
                })
                .count();
            let word = if n == 1 { "turret" } else { "turrets" };
            Some(format!("Broadcasting to {} {}", n, word))
        }
        WeaponType::CrowsNest => {
            let s = cfg.slots[slot_idx];
            let tier = s.barrels.clamp(1, 3) as u32;
            let pct = tier * 15;
            Some(format!("Adjacent weapons gain +{}% range", pct))
        }
        _ => None,
    }
}

/// "Range X%" line. Shows the resolved final percentage only.
/// Hidden when at the 100% baseline (no point telling the player
/// nothing changed). The Range stat is exposed in the stats panel
/// so the player can verify the modifier independently.
fn weapon_extra_line(
    weapon: WeaponType,
    stats: &crate::stats::PlayerStats,
) -> Option<String> {
    let final_pct = (weapon.range_mult() * stats.range_mult() * 100.0).round() as i32;
    // Flamethrower's reach is part of its identity — surface the
    // line even at the 100% baseline so the player can see how the
    // Range stat will extend the cone.
    if matches!(weapon, WeaponType::Flamethrower) {
        return Some(format!("Range {}%", final_pct));
    }
    if final_pct == 100 {
        None
    } else {
        Some(format!("Range {}%", final_pct))
    }
}

/// Plain-English description for a rune with current `PlayerStats`
/// substituted in. Mirrors the static `Rune::description()` text but
/// with the live values baked in (per-tick damage, chain count, etc.).
/// Numbers reflect the current Rune Effect multiplier so the player
/// sees the exact value the rune will produce at fire time. Passive
/// targeting runes fall through to the static description.
fn rune_dynamic_description(
    rune: Rune,
    stats: &crate::stats::PlayerStats,
    slot_runes: Option<&[Option<Rune>; 3]>,
) -> String {
    let rune_dmg = stats.rune_damage_mult();
    let chain_count = rune_dmg.round().max(1.0) as i32;
    match rune {
        Rune::Fire => {
            let per_tick = (1.0 * rune_dmg).max(0.1);
            format!(
                "Sets enemies ablaze, dealing {:.1} every 0.5s for 4 seconds.",
                per_tick,
            )
        }
        Rune::Frost => {
            // Slow compounds across multiple Frost runes on the same
            // bullet; the displayed value is the per-application slow.
            let slow_pct = (1.0 - crate::balance::FROST_SPEED_MULT) * 100.0;
            format!(
                "Freezes enemies for {:.1}s. {:.0}% slow.",
                crate::balance::FROST_DURATION, slow_pct,
            )
        }
        Rune::Shock => format!(
            "Chains lightning to {} nearby enemies for 100% weapon damage each.",
            chain_count,
        ),
        Rune::Echo => format!(
            "Fires a second hit on the same target {:.1}s later.",
            crate::rune::ECHO_DELAY,
        ),
        Rune::Cascade => {
            "Killing blows leap to a nearby enemy at 70% proc strength.".to_string()
        }
        Rune::Conduit => {
            // Bonus mirrors `OnConduit::proc_mult`.
            let pct = (crate::balance::CONDUIT_PROC_MULT - 1.0) * rune_dmg * 100.0;
            format!(
                "Marks the target. Other runes are {:+.0}% more likely to trigger on marked enemies.",
                pct,
            )
        }
        Rune::Resonate => {
            let per_stack = crate::balance::RESONATE_DAMAGE_PER_STACK * 100.0;
            format!(
                "Hits weaken the target: each hit adds +{:.0}% damage taken (up to {} hits). Fades after {:.0}s without a hit.",
                per_stack,
                crate::balance::RESONATE_MAX_STACKS,
                crate::balance::RESONATE_DECAY,
            )
        }
        Rune::Vampire => {
            let hits_per_hp = (10.0 / rune_dmg.max(0.001)).max(1.0).round() as i32;
            format!(
                "Heal 1 HP every {} hits.",
                hits_per_hp,
            )
        }
        Rune::Ward => format!(
            "Killing blows grant {:.0} shield. Can overflow above Shield Max as a one-time buffer (no recharge above).",
            rune_dmg,
        ),
        Rune::Bleed => {
            let pct = crate::balance::BLEED_PCT_PER_TICK * 100.0 * rune_dmg;
            format!(
                "Anti-tank DoT. Each tick chips {:.1}% of the target's max HP for 4 seconds.",
                pct,
            )
        }
        Rune::Splash => {
            // Live value reflects the player's Rune Effect stat. The
            // `[AOE]` token is colourised inline by `colorize_bonuses`
            // so it reads as the same chip the rest of the tooltip
            // system uses for AoE tags.
            let per_stack = 0.5 * rune_dmg * 100.0;
            format!(
                "Widens [AOE] weapons' radius by {:+.0}%.",
                per_stack,
            )
        }
        Rune::Blast => {
            // "Attacks" not "bullets" — Blast fires inside the shared
            // damage-event pipeline, so it works for every weapon
            // type that pushes through `PendingDamageQueue` (melee
            // Blade, autonomous Helicopter / Octopus, beam Railgun,
            // mortar shells, regular bullets — all of them).
            // Deliberately no "per stack" language — Blast reads as
            // a weapon transformation, not a numeric upgrade.
            //
            // When slot context is available (hovering an equipped
            // Blast, not a shop card), the radius shown folds in any
            // Splash runes on the same slot — same `+50% × stacks ×
            // rune_effect` formula the runtime uses. Lets the player
            // see exactly how Splash + Blast combine without picking
            // them up off the ship.
            let splash_stacks = slot_runes
                .map(|rs| rs.iter().filter(|r| matches!(r, Some(Rune::Splash))).count())
                .unwrap_or(0) as f32;
            let splash_mult = 1.0 + 0.5 * splash_stacks * rune_dmg;
            let radius = crate::balance::BLAST_RADIUS * rune_dmg * splash_mult;
            let pct = crate::balance::BLAST_SPLASH_FRAC * 100.0;
            format!(
                "Attacks explode on impact, splashing {:.0}% damage to enemies within {:.1} px. This weapon is now considered [AOE].",
                pct, radius,
            )
        }
        Rune::Hustle => {
            // Speed bonus applied to the deployed unit of Autonomous-
            // tagged turrets. Live value reflects the player's Rune
            // Effect stat.
            let per_stack = rune_dmg * 100.0;
            format!(
                "Autonomous units get +{:.0}% move speed.",
                per_stack,
            )
        }
        Rune::Pierce => {
            let base = crate::balance::PIERCE_BASE_FALLOFF;
            let falloff = (base + (1.0 - base) * (rune_dmg - 1.0).max(0.0)).clamp(base, 1.0);
            let keep_pct = (falloff * 100.0).round() as i32;
            format!(
                "Bullets pierce one extra enemy per stack, keeping {}% damage on each subsequent hit.",
                keep_pct,
            )
        }
        Rune::Greed => {
            let raw = crate::balance::GREED_BASE_KILLS
                .saturating_sub(crate::balance::GREED_KILLS_PER_STACK);
            let needed = (raw as f32 / rune_dmg.max(0.01)).ceil().max(1.0) as i32;
            format!(
                "+1 scrap every {} kills landed by this weapon. Stacks lower the threshold.",
                needed,
            )
        }
        Rune::Executioner => {
            let pct = crate::balance::EXECUTIONER_BONUS_PER_STACK * rune_dmg * 100.0;
            let threshold = (crate::balance::EXECUTIONER_HP_THRESHOLD * 100.0).round() as i32;
            format!(
                "{:+.0}% damage to enemies below {}% HP.",
                pct, threshold,
            )
        }
        Rune::Opener => {
            let pct = crate::balance::OPENER_BONUS_PER_STACK * rune_dmg * 100.0;
            format!(
                "{:+.0}% damage to enemies at full HP.",
                pct,
            )
        }
        Rune::Leftovers => {
            let heal = (1.0 * rune_dmg).round().max(1.0) as i32;
            format!(
                "Killing blows drop a heal pickup worth {} HP.",
                heal,
            )
        }
        Rune::Star => {
            let pct = (25.0 * rune_dmg).round() as i32;
            format!(
                "+{}% XP from kills landed by this weapon.",
                pct,
            )
        }
        Rune::Thirst => {
            let pct = (50.0 * rune_dmg).round() as i32;
            format!(
                "After a kill, next shot from this slot deals +{}% damage.",
                pct,
            )
        }
        Rune::Medic => {
            let heal = (2.0 * rune_dmg).round().max(1.0) as i32;
            format!(
                "Equipped on a [SUPPORT], heal {} HP every 5s.",
                heal,
            )
        }
        Rune::Rally => {
            let pct = (1.0 * rune_dmg).max(0.1);
            format!(
                "Equipped on a [MELEE] weapon: kills grant +{:.1}% move speed for 5s (stacks).",
                pct,
            )
        }
        Rune::Thorns => {
            let bonus = (1.0 * rune_dmg).round().max(1.0) as i32;
            format!(
                "Contact damage on this slot's side: +{} per stack.",
                bonus,
            )
        }
        // Targeting runes have no value to show — pure aim modifiers.
        Rune::TargetFurthest
        | Rune::TargetHighestHp
        | Rune::TargetLowestHp
        | Rune::TargetCarousel => rune.description().to_string(),
    }
}
