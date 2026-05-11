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
    /// `Some(tag)` only when the hovered source is a turret. The
    /// banner system uses this to look up the ladder + active tier.
    pub tag: Option<WeaponTag>,
}

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

/// Native-pixel font size for the synergy banner text.
const SYNERGY_FONT: f32 = 13.0;
/// Native-pixel padding around the banner text (left/right) — tight,
/// since the banner is a single line.
const SYNERGY_TEXT_PAD_X: f32 = 10.0;
/// Native-pixel padding above/below the banner text.
const SYNERGY_TEXT_PAD_Y: f32 = 6.0;
/// Spec-pixel gap between the main tooltip's top edge and the
/// banner's bottom edge.
const SYNERGY_GAP: f32 = 2.0;
/// Native-pixel cap on banner text width. Sized to fit a 2-line
/// full-sentence synergy description at body font (each line ≈ 50
/// characters wide), so the player gets the *why* of each tag
/// rather than the cryptic short-form descriptor.
const SYNERGY_TEXT_MAX_W: f32 = 380.0;
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

pub fn spawn_customize_tooltip(commands: &mut Commands) {
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
        TextFont {
            font_size: SYNERGY_FONT,
            font_smoothing: FontSmoothing::None,
            ..default()
        },
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
    data: TooltipDataCtx,
    viewport: Res<CustomizeViewport>,
    mut layout: ResMut<TooltipLayout>,
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
        layout.state = None;
        return;
    }
    let cursor = drag.spec_cursor.unwrap();
    let shop_ref = data.shop.as_deref();

    // `info` tracks the hovered target's (title, body, centre, half-extent);
    // `info_tag` tracks the WEAPON TAG when the hovered source is a turret
    // (shop slot or ship slot). Runes and stat rows leave it None, which
    // hides the synergy banner.
    let mut info: Option<(String, String, Vec2, Vec2)> = None;
    let mut info_tag: Option<WeaponTag> = None;
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
            marker.0, &data.cfg, shop_ref, &data.discovered, &data.stats,
        ) {
            info = Some((title, body, centre, half));
            info_tag = turret_tag_for_source(marker.0, &data.cfg, shop_ref);
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
        // Stat rows never get a synergy banner — they're stats, not turrets.
        info_tag = None;
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
    let fill_w_native = (text_w_native + 2.0 * TOOLTIP_TEXT_PAD).max(TOOLTIP_MIN_W * s);
    // Line-count estimate: how many `TOOLTIP_BODY_MAX_W` slabs the
    // unwrapped body needs. `ceil(body_w / max_w)`, min 1. Plus the
    // count of explicit `\n` in the body (e.g. the `[TAG]\n…` chip
    // line) — those force a break that the width-only calculation
    // would otherwise miss, leading to truncated short tooltips.
    let explicit_breaks = body.chars().filter(|&c| c == '\n').count() as f32;
    let body_lines = (body_unwrapped_w / TOOLTIP_BODY_MAX_W).ceil().max(1.0) + explicit_breaks;
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

    // Publish the layout so `update_synergy_banner` can stack
    // its banner above (or below) the main tooltip without
    // duplicating any of the sizing/positioning math.
    layout.state = Some(TooltipLayoutState {
        pos_spec: pos,
        size_spec: Vec2::new(tooltip_w_spec, tooltip_h_spec),
        tag: info_tag,
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
        (&mut Visibility, &mut Transform, &mut Text2d),
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
            (&mut Visibility, &mut Transform, &mut Text2d),
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
        if let Ok((mut v, _, _)) = text_q.single_mut() {
            if *v != Visibility::Hidden { *v = Visibility::Hidden; }
        }
    };

    let Some(state) = layout.state else {
        hide(&mut outline_q, &mut fill_q, &mut text_q);
        return;
    };
    let Some(tag) = state.tag else {
        hide(&mut outline_q, &mut fill_q, &mut text_q);
        return;
    };
    // Hide the banner entirely until the player has discovered
    // this synergy. The `[???]` chip in the main tooltip body is
    // the only cue at that point — once 2 of this tag are equipped
    // (`DiscoveredSynergies` flips the bit) the banner reveals
    // with full description + value ladder.
    if !discovered.has(tag) {
        hide(&mut outline_q, &mut fill_q, &mut text_q);
        return;
    }
    let (values, descriptor) = synergy_ladder(tag);
    let tier = active_tier(tag, &synergies);
    let description = synergy_description(tag);
    // Two visible sections, joined with a newline:
    //  1. Tag chip + full-sentence description (wraps freely).
    //  2. Value ladder (`V / V / V / V <unit>`) on its own line.
    // Width estimate uses each section's UNWRAPPED width so the box
    // accounts for the wider of the two when ladder + chip overflow
    // the cap by themselves.
    let header_plain = format!("[{}] {}", tag.label(), description);
    let ladder_plain = format!(
        "{} / {} / {} / {} {}",
        values[0], values[1], values[2], values[3], descriptor,
    );
    let banner_text_plain = format!("{}\n{}", header_plain, ladder_plain);
    let s = viewport.display_scale;
    let header_w_unwrapped = estimate_text_native_width(&header_plain, SYNERGY_FONT);
    let ladder_w_unwrapped = estimate_text_native_width(&ladder_plain, SYNERGY_FONT);
    // Wrapped box width caps at SYNERGY_TEXT_MAX_W but hugs tightly
    // when both sections fit under the cap.
    let widest_unwrapped = header_w_unwrapped.max(ladder_w_unwrapped);
    let banner_text_w = widest_unwrapped.min(SYNERGY_TEXT_MAX_W);
    let banner_fill_w_native = banner_text_w + 2.0 * SYNERGY_TEXT_PAD_X;
    // Line count: header wraps at the cap (could be 1-3 lines for a
    // long description); ladder is normally 1 line. Add them.
    let header_lines = (header_w_unwrapped / SYNERGY_TEXT_MAX_W).ceil().max(1.0);
    let ladder_lines = (ladder_w_unwrapped / SYNERGY_TEXT_MAX_W).ceil().max(1.0);
    let total_lines = header_lines + ladder_lines;
    let banner_fill_h_native =
        total_lines * SYNERGY_FONT * TOOLTIP_LINE_HEIGHT_MULT + 2.0 * SYNERGY_TEXT_PAD_Y;
    let banner_w_spec = banner_fill_w_native / s;
    let banner_h_spec = banner_fill_h_native / s;

    let canvas_half_w = CUSTOMIZE_INTERNAL_W as f32 * 0.5;
    let canvas_half_h = CUSTOMIZE_INTERNAL_H as f32 * 0.5;
    let mut banner_pos = Vec2::new(
        state.pos_spec.x,
        state.pos_spec.y + state.size_spec.y * 0.5 + SYNERGY_GAP + banner_h_spec * 0.5,
    );
    // Flip below if the top of the canvas would clip.
    if banner_pos.y + banner_h_spec * 0.5 > canvas_half_h {
        banner_pos.y = state.pos_spec.y - state.size_spec.y * 0.5 - SYNERGY_GAP - banner_h_spec * 0.5;
    }
    banner_pos.x = banner_pos.x.clamp(
        -canvas_half_w + banner_w_spec * 0.5,
        canvas_half_w - banner_w_spec * 0.5,
    );
    banner_pos.y = banner_pos.y.clamp(
        -canvas_half_h + banner_h_spec * 0.5,
        canvas_half_h - banner_h_spec * 0.5,
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
    if let Ok((mut v, mut tf, mut text)) = text_q.single_mut() {
        if *v != Visibility::Inherited { *v = Visibility::Inherited; }
        tf.translation.x = banner_native_centre.x;
        tf.translation.y = banner_native_centre.y;
        if !text.0.is_empty() { text.0 = String::new(); }
    }

    // Rebuild spans when tag, tier, or text changes. The
    // undiscovered case early-returns above, so we always render
    // the full discovered banner here.
    let banner_key = format!("{}|{}|{}", tag.label(), tier, banner_text_plain);
    if *banner_cache != banner_key {
        *banner_cache = banner_key;
        for span in existing_banner_spans.iter() {
            commands.entity(span).despawn();
        }
        if let Ok(banner_entity) = banner_entity_q.single() {
            commands.entity(banner_entity).with_children(|p| {
                // ---- Header line: tag chip + full description ----
                p.spawn((
                    TextSpan::new(format!("[{}] ", tag.label())),
                    TextFont { font_size: SYNERGY_FONT, font_smoothing: FontSmoothing::None, ..default() },
                    TextColor(tag.color()),
                    SynergyBannerSpan,
                ));
                p.spawn((
                    TextSpan::new(description.to_string()),
                    TextFont { font_size: SYNERGY_FONT, font_smoothing: FontSmoothing::None, ..default() },
                    TextColor(SYNERGY_DESC_COLOR),
                    SynergyBannerSpan,
                ));
                // ---- Newline forces the ladder onto its own line ----
                p.spawn((
                    TextSpan::new("\n".to_string()),
                    TextFont { font_size: SYNERGY_FONT, font_smoothing: FontSmoothing::None, ..default() },
                    TextColor(SYNERGY_DESC_COLOR),
                    SynergyBannerSpan,
                ));
                // ---- Ladder line: V / V / V / V <unit> ----
                for (i, v) in values.iter().enumerate() {
                    let active = (tier as usize) == i + 1;
                    let color = if active { SYNERGY_ACTIVE_COLOR } else { SYNERGY_INACTIVE_COLOR };
                    p.spawn((
                        TextSpan::new((*v).to_string()),
                        TextFont { font_size: SYNERGY_FONT, font_smoothing: FontSmoothing::None, ..default() },
                        TextColor(color),
                        SynergyBannerSpan,
                    ));
                    if i < values.len() - 1 {
                        p.spawn((
                            TextSpan::new(" / ".to_string()),
                            TextFont { font_size: SYNERGY_FONT, font_smoothing: FontSmoothing::None, ..default() },
                            TextColor(SYNERGY_INACTIVE_COLOR),
                            SynergyBannerSpan,
                        ));
                    }
                }
                p.spawn((
                    TextSpan::new(format!(" {}", descriptor)),
                    TextFont { font_size: SYNERGY_FONT, font_smoothing: FontSmoothing::None, ..default() },
                    TextColor(SYNERGY_DESC_COLOR),
                    SynergyBannerSpan,
                ));
            });
        }
    }
}

/// Resolve a drag-source to its weapon tag — Some only for turrets
/// (ship or shop). Runes and stat rows return None, so the synergy
/// banner stays hidden over them.
fn turret_tag_for_source(
    source: DragSourceKind,
    cfg: &TurretConfig,
    shop: Option<&CustomizeShop>,
) -> Option<WeaponTag> {
    match source {
        DragSourceKind::ShipSlot(slot) => {
            let s = cfg.slots[slot];
            if !s.equipped { return None; }
            Some(s.weapon.tag())
        }
        DragSourceKind::ShopTurret(idx) => shop
            .and_then(|s| s.turrets.get(idx))
            .and_then(|o| o.as_ref())
            .map(|o| o.weapon.tag()),
        DragSourceKind::ShipRune { .. } | DragSourceKind::ShopRune(_) => None,
    }
}

/// Default body color (matches the root `Text2d`'s `TextColor`).
const TOOLTIP_BODY_COLOR: Color = Color::srgb(0.85, 0.88, 0.94);
/// Tint for positive numeric tokens (`+30%`, `+1`).
const TOOLTIP_BUFF_COLOR: Color = Color::srgb(0.55, 0.95, 0.55);
/// Tint for negative numeric tokens (`-50`, `-70%`).
const TOOLTIP_NERF_COLOR: Color = Color::srgb(1.00, 0.55, 0.55);
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
fn tag_chip_color(name: &str) -> Option<Color> {
    for &tag in WeaponTag::all() {
        if tag.label() == name {
            return Some(tag.color());
        }
    }
    if name == "AOE" {
        return Some(TOOLTIP_AOE_TAG_COLOR);
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
        // body or right after whitespace, so a stray bracket inside
        // sentence text doesn't accidentally get coloured. The chip
        // text itself includes the brackets so the rendered output
        // reads e.g. `[NAVAL] Balanced cannon...`.
        let at_word_start = i == 0 || chars[i - 1].is_whitespace();
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
    discovered: &crate::onboarding::DiscoveredSynergies,
    stats: &crate::stats::PlayerStats,
) -> Option<(String, String)> {
    match source {
        DragSourceKind::ShipSlot(slot) => {
            let s = cfg.slots[slot];
            if !s.equipped {
                return None;
            }
            let tag_known = discovered.has(s.weapon.tag());
            Some(turret_tooltip(s.weapon, s.barrels.max(1), tag_known, stats))
        }
        DragSourceKind::ShipRune { slot, rune_idx } => {
            let s = cfg.slots[slot];
            if !s.equipped {
                return None;
            }
            s.runes[rune_idx].map(|r| rune_tooltip(r, stats))
        }
        DragSourceKind::ShopTurret(idx) => shop
            .and_then(|s| s.turrets.get(idx))
            .and_then(|o| o.as_ref())
            .map(|o| {
                let tag_known = discovered.has(o.weapon.tag());
                turret_tooltip(o.weapon, o.barrels.max(1), tag_known, stats)
            }),
        DragSourceKind::ShopRune(idx) => shop
            .and_then(|s| s.runes.get(idx))
            .and_then(|o| o.as_ref())
            .copied()
            .map(|r| rune_tooltip(r, stats)),
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
            "Melee kills heal your hull. Higher tiers, bigger heal.",
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
    tag_discovered: bool,
    stats: &crate::stats::PlayerStats,
) -> (String, String) {
    let title = weapon.label().to_string();
    let chip = if tag_discovered { weapon.tag().label() } else { "???" };
    let mut body = format!("[{}]\n", chip);
    if matches!(weapon, WeaponType::Mortar) {
        body.push_str(AOE_TAG);
    }
    body.push_str(weapon.description());
    body.push_str("\n\n");
    // Brotato-style dynamic stat lines below the flavour. Show base
    // damage × current TurretDamage modifier, plus weapon-specific
    // notes (knockback / splash / pierce) where relevant.
    body.push_str(&weapon_damage_line(weapon, stats));
    if let Some(rate_line) = weapon_rate_line(weapon, barrels) {
        body.push('\n');
        body.push_str(&rate_line);
    }
    if let Some(extra) = weapon_extra_line(weapon, stats) {
        body.push('\n');
        body.push_str(&extra);
    }
    (title, body)
}

fn rune_tooltip(rune: Rune, stats: &crate::stats::PlayerStats) -> (String, String) {
    let mut body = String::new();
    if matches!(rune, Rune::Splash) {
        body.push_str(AOE_TAG);
    }
    body.push_str(&rune_dynamic_description(rune, stats));
    (rune.label().to_string(), body)
}

/// "Damage X" line, expanded with a base + stat breakdown when the
/// player's Turret Damage stat isn't at default. Single value at
/// default keeps the tooltip lean; with a buff/nerf the player can
/// see what the weapon WOULD do raw and where the bonus came from
/// without doing the math themselves.
fn weapon_damage_line(weapon: WeaponType, stats: &crate::stats::PlayerStats) -> String {
    let (base_dmg, _rate) = weapon.defaults();
    let mult = stats.turret_damage_mult();
    let final_dmg = (base_dmg as f32 * mult).round() as i32;
    let pct_bonus = ((mult - 1.0) * 100.0).round() as i32;
    if pct_bonus == 0 {
        format!("Damage {}", final_dmg)
    } else {
        let sign = if pct_bonus > 0 { "+" } else { "" };
        format!(
            "Damage {} from {} base and {}{}% Turret Damage",
            final_dmg, base_dmg, sign, pct_bonus,
        )
    }
}

/// "Cooldown Xs" line. Twin / triple barrels alternate on the slot
/// cooldown so effective rate is `base x barrels`; cooldown is the
/// reciprocal. Returns None when the weapon doesn't fire from the
/// deck (Booster).
fn weapon_rate_line(weapon: WeaponType, barrels: u8) -> Option<String> {
    let (_base_dmg, base_rate) = weapon.defaults();
    if base_rate <= 0.0 { return None; }
    let effective_rate = base_rate * (barrels.max(1) as f32);
    let cooldown = 1.0 / effective_rate;
    Some(format!("Cooldown {:.2}s", cooldown))
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
    if final_pct == 100 {
        None
    } else {
        Some(format!("Range {}%", final_pct))
    }
}

/// Plain-English description for a rune with current `PlayerStats`
/// substituted in. Mirrors the static `Rune::description()` text
/// but with the dynamic numbers baked in (Fire's per-tick damage
/// after rune-damage scaling, Shock's chain count from rune-damage
/// rounded, etc.). Static-only runes (Frost, Echo, etc.) fall
/// through to `rune.description()`.
fn rune_dynamic_description(rune: Rune, stats: &crate::stats::PlayerStats) -> String {
    let rune_dmg = stats.rune_damage_mult();
    let chain_count = rune_dmg.round().max(1.0) as i32;
    match rune {
        Rune::Fire => {
            let per_tick = (1.0 * rune_dmg).max(0.1);
            let total = (8.0 * rune_dmg).max(0.0);
            format!(
                "Sets enemies ablaze. {:.1} damage every 0.5s for 4 seconds. {:.0} total. Stack to burn harder.",
                per_tick, total,
            )
        }
        Rune::Shock => format!(
            "Chains lightning to {} nearby enemies. 100% weapon damage each. Stack to add more arcs.",
            chain_count,
        ),
        Rune::Detonate => {
            "Pops Fire and Frost on the target for a damage burst. Bigger with stacks. Rune Damage scales the burst.".to_string()
        }
        _ => rune.description().to_string(),
    }
}
