//! Shop mod cards — 3 click-to-buy stat-modifier offerings rendered
//! below the rune row.
//!
//! Visual: thin white outline + dark fill on `UPSCALE_LAYER` (sharp
//! edges, like the tooltip), with a single text line inside showing
//! the signed delta + stat name (e.g. `+25 CRIT`). Clicking applies
//! the delta to the targeted stat's `flat` field and consumes the
//! slot.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;

use crate::balance::{CUSTOMIZE_LAYER, UPSCALE_LAYER};
use crate::stats::PlayerStats;

use super::drag::{CustomizeShop, DragState};
use super::render::CustomizeViewport;
use super::setup::HitArea;
use super::CustomizeOpen;

// Spec-pixel layout. Three cards in a row; the spawn helper centres
// the row on its `centre_x` argument. Sized to fit a TWO-line label
// (signed value on top, short stat name below — see `ShopMod::label`)
// at the bumped 14pt font, with the row staying narrow enough to
// keep the shop column anchored far enough left that the sell strip
// fits cleanly under the ship.
pub const MOD_CARD_W: f32 = 32.0;
// Tightened so the mod row + cost label + reroll button all clear
// the bottom of the canvas without scrolling. Pure mods only show
// 2 lines anyway; trade-off cards still fit their 3 (name + buff
// + nerf) at the smaller font, with margin.
pub const MOD_CARD_H: f32 = 22.0;
pub const MOD_CARD_GAP: f32 = 4.0;
const Z_OUTLINE: f32 = 99.0;
const Z_FILL: f32 = 99.5;
const Z_TEXT: f32 = 100.0;
/// Native-pixel border thickness on the white outline.
const BORDER_PX: f32 = 2.0;

/// Marker on the click target. The `idx` indexes into `CustomizeShop.mods`.
#[derive(Component, Clone, Copy)]
pub struct ShopModSlot { pub idx: usize }

/// White outline sprite. Carries its own spec position so the layout
/// updater doesn't need to recompute the row geometry.
#[derive(Component, Clone, Copy)]
pub struct ShopModOutline { pub idx: usize, pub spec_pos: Vec2 }

/// Dark fill sprite (rendered above the outline).
#[derive(Component, Clone, Copy)]
pub struct ShopModFill { pub idx: usize, pub spec_pos: Vec2 }

/// Card label. Owned visibility — *not* a `CustomizeText`, since the
/// shared sync would override our "hide when slot empty" state.
#[derive(Component, Clone, Copy)]
pub struct ShopModText { pub idx: usize, pub spec_pos: Vec2 }

/// Marker on each per-line `TextSpan` child of a card's `ShopModText`
/// root. The update system despawns these whenever the card's label
/// content changes and respawns a fresh set with per-line colours
/// (green for `+...` lines, red for `-...`, neutral for names).
#[derive(Component)]
pub struct ShopModTextSpan { pub idx: usize }

/// Cost label below the card. Same owned-visibility lifecycle as
/// `ShopModText`: shown while the slot is occupied, hidden when empty.
#[derive(Component, Clone, Copy)]
pub struct ShopModCostText { pub idx: usize, pub spec_pos: Vec2 }

/// Spawn three card slots centred on `centre_x` at the given `y`.
pub fn spawn_mod_cards(commands: &mut Commands, font: &crate::fonts::PixelFont, centre_x: f32, y: f32) {
    let step = MOD_CARD_W + MOD_CARD_GAP;
    for idx in 0..3usize {
        let x = centre_x + (idx as f32 - 1.0) * step;
        let pos = Vec2::new(x, y);
        spawn_card(commands, font, idx, pos);
    }
}

fn spawn_card(commands: &mut Commands, font: &crate::fonts::PixelFont, idx: usize, spec_pos: Vec2) {
    commands.spawn((
        Sprite {
            color: Color::WHITE,
            custom_size: Some(Vec2::new(MOD_CARD_W, MOD_CARD_H)),
            ..default()
        },
        Transform::from_xyz(0.0, 0.0, Z_OUTLINE),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        ShopModOutline { idx, spec_pos },
    ));
    commands.spawn((
        Sprite {
            color: Color::srgb(0.13, 0.14, 0.17),
            custom_size: Some(Vec2::new(MOD_CARD_W, MOD_CARD_H)),
            ..default()
        },
        Transform::from_xyz(0.0, 0.0, Z_FILL),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        ShopModFill { idx, spec_pos },
    ));
    commands.spawn((
        Text2d::new(""),
        // Smaller body font (10 vs the earlier 14) — keeps every
        // "+10% LABEL" line well inside the now-narrower card and
        // leaves headroom for trade-off mods' 3 stacked lines
        // without overflowing into the neighbour card.
        crate::fonts::pixel_text_font(font, 10.0),
        TextColor(Color::srgb(1.0, 0.85, 0.30)),
        // Centre the two-line value/name pair so each line stacks
        // on its own row inside the card.
        TextLayout::new_with_justify(JustifyText::Center),
        Transform::from_xyz(0.0, 0.0, Z_TEXT),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        ShopModText { idx, spec_pos },
    ));
    // Cost label, positioned below the card. Same gold accent as the
    // other shop labels; per-frame sync rewrites position + visibility.
    let cost_spec = spec_pos + Vec2::new(0.0, -MOD_CARD_H * 0.5 - 6.0);
    commands.spawn((
        Text2d::new(""),
        crate::fonts::pixel_text_font(font, 10.0),
        TextColor(Color::srgb(1.0, 0.85, 0.30)),
        Transform::from_xyz(0.0, 0.0, Z_TEXT),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        ShopModCostText { idx, spec_pos: cost_spec },
    ));
    // Hit area on the customize layer. Used for cursor-in-rect tests.
    commands.spawn((
        Transform::from_translation(spec_pos.extend(2.0)),
        HitArea { size: Vec2::new(MOD_CARD_W, MOD_CARD_H) },
        ShopModSlot { idx },
        RenderLayers::layer(CUSTOMIZE_LAYER),
    ));
}

/// Per-frame layout + visibility + content sync.
pub fn update_shop_mod_cards(
    mut commands: Commands,
    open: Res<CustomizeOpen>,
    viewport: Res<CustomizeViewport>,
    ui_scale: Res<bevy::ui::UiScale>,
    pixel_font: Res<crate::fonts::PixelFont>,
    shop: Option<Res<CustomizeShop>>,
    mut text_cache: Local<[Option<String>; 3]>,
    existing_spans: Query<(Entity, &ShopModTextSpan)>,
    text_entities: Query<(Entity, &ShopModText)>,
    mut outlines: Query<(&ShopModOutline, &mut Visibility, &mut Transform, &mut Sprite),
        (Without<ShopModFill>, Without<ShopModText>, Without<ShopModCostText>)>,
    mut fills: Query<(&ShopModFill, &mut Visibility, &mut Transform, &mut Sprite),
        (Without<ShopModOutline>, Without<ShopModText>, Without<ShopModCostText>)>,
    mut texts: Query<(&ShopModText, &mut Visibility, &mut Transform, &mut Text2d, &mut TextColor),
        (Without<ShopModOutline>, Without<ShopModFill>, Without<ShopModCostText>)>,
    mut cost_texts: Query<(&ShopModCostText, &mut Visibility, &mut Transform, &mut Text2d),
        (Without<ShopModOutline>, Without<ShopModFill>, Without<ShopModText>)>,
) {
    let panel_visible = open.open && shop.is_some();
    if !panel_visible {
        for (_, mut v, _, _) in &mut outlines   { hide_one(&mut v); }
        for (_, mut v, _, _) in &mut fills      { hide_one(&mut v); }
        for (_, mut v, _, _, _) in &mut texts   { hide_one(&mut v); }
        for (_, mut v, _, _) in &mut cost_texts { hide_one(&mut v); }
        return;
    }
    let shop = shop.unwrap();
    let s = viewport.display_scale;
    // Card size stays in spec×display_scale so it lines up with
    // the `HitArea` (which is spec-coord) at every resolution.
    // The per-line text scales with `UiScale` separately; we keep
    // glyph counts under control via shorter `short_stat_label`
    // names instead of resizing the card itself.
    let fill_size = Vec2::new(MOD_CARD_W * s, MOD_CARD_H * s);
    let outline_size = fill_size + Vec2::splat(2.0 * BORDER_PX);

    for (slot, mut vis, mut tf, mut sprite) in &mut outlines {
        let occupied = shop.mods.get(slot.idx).map_or(false, |m| m.is_some());
        set_vis(&mut vis, if occupied { Visibility::Inherited } else { Visibility::Hidden });
        if !occupied { continue; }
        if sprite.custom_size != Some(outline_size) { sprite.custom_size = Some(outline_size); }
        tf.translation.x = slot.spec_pos.x * s;
        tf.translation.y = slot.spec_pos.y * s;
    }
    for (slot, mut vis, mut tf, mut sprite) in &mut fills {
        let occupied = shop.mods.get(slot.idx).map_or(false, |m| m.is_some());
        set_vis(&mut vis, if occupied { Visibility::Inherited } else { Visibility::Hidden });
        if !occupied { continue; }
        if sprite.custom_size != Some(fill_size) { sprite.custom_size = Some(fill_size); }
        tf.translation.x = slot.spec_pos.x * s;
        tf.translation.y = slot.spec_pos.y * s;
    }
    for (slot, mut vis, mut tf, mut text, mut color) in &mut texts {
        let m = shop.mods.get(slot.idx).and_then(|m| *m);
        let occupied = m.is_some();
        set_vis(&mut vis, if occupied { Visibility::Inherited } else { Visibility::Hidden });
        if let Some(m) = m {
            let label = m.label();
            tf.translation.x = slot.spec_pos.x * s;
            tf.translation.y = slot.spec_pos.y * s;
            // Visual scale follows `UiScale` (window-relative,
            // matches bevy_ui chrome) — same fix as
            // `sync_customize_text`. Using `display_scale` here
            // produced 64-screen-pixel text on the design window.
            let glyph = ui_scale.0;
            let want_scale = Vec3::new(glyph, glyph, 1.0);
            if tf.scale != want_scale { tf.scale = want_scale; }
            // Root text stays empty - per-line colour comes from
            // the TextSpan children rebuilt below.
            if !text.0.is_empty() { text.0 = String::new(); }
            // Root colour is irrelevant since the text is empty,
            // but reset to neutral for consistency.
            let neutral = Color::srgb(0.85, 0.88, 0.94);
            if color.0 != neutral { color.0 = neutral; }
            // Rebuild span children only when the label string
            // changes (cheap cache key per slot).
            let cached = &mut text_cache[slot.idx];
            if cached.as_deref() != Some(label.as_str()) {
                *cached = Some(label.clone());
                // Despawn this slot's existing spans.
                for (e, span) in &existing_spans {
                    if span.idx == slot.idx { commands.entity(e).despawn(); }
                }
                // Find the parent text entity for this slot.
                let parent = text_entities
                    .iter()
                    .find(|(_, t)| t.idx == slot.idx)
                    .map(|(e, _)| e);
                if let Some(parent) = parent {
                    commands.entity(parent).with_children(|p| {
                        let lines: Vec<&str> = label.split('\n').collect();
                        for (li, line) in lines.iter().enumerate() {
                            // Per-line tint: green for `+`, red
                            // for `-`, neutral for everything
                            // else (the stat-name lines).
                            let line_color = if line.starts_with('+') {
                                crate::ui_kit::theme::BUFF_FG
                            } else if line.starts_with('-') {
                                crate::ui_kit::theme::NERF_FG
                            } else {
                                Color::srgb(0.85, 0.88, 0.94)
                            };
                            // Bevy's Text2d treats a `\n` inside a
                            // span as a hard break, so we glue
                            // the newline to the preceding span
                            // and don't need separator-only spans.
                            let txt = if li + 1 < lines.len() {
                                format!("{}\n", line)
                            } else {
                                line.to_string()
                            };
                            p.spawn((
                                TextSpan::new(txt),
                                crate::fonts::pixel_text_font(&pixel_font, 10.0),
                                TextColor(line_color),
                                ShopModTextSpan { idx: slot.idx },
                            ));
                        }
                    });
                }
            }
        } else {
            // Slot empty: clear the cache + drop any leftover spans.
            if text_cache[slot.idx].is_some() {
                text_cache[slot.idx] = None;
                for (e, span) in &existing_spans {
                    if span.idx == slot.idx { commands.entity(e).despawn(); }
                }
                if !text.0.is_empty() { text.0 = String::new(); }
            }
        }
    }
    let cost_label = super::drag::SHOP_ITEM_COST.to_string();
    for (slot, mut vis, mut tf, mut text) in &mut cost_texts {
        let occupied = shop.mods.get(slot.idx).map_or(false, |m| m.is_some());
        set_vis(&mut vis, if occupied { Visibility::Inherited } else { Visibility::Hidden });
        if !occupied { continue; }
        if text.0 != cost_label { text.0 = cost_label.clone(); }
        tf.translation.x = slot.spec_pos.x * s;
        tf.translation.y = slot.spec_pos.y * s;
        let glyph = ui_scale.0;
        let want_scale = Vec3::new(glyph, glyph, 1.0);
        if tf.scale != want_scale { tf.scale = want_scale; }
    }
}

fn set_vis(vis: &mut Visibility, want: Visibility) {
    if *vis != want { *vis = want; }
}
fn hide_one(vis: &mut Visibility) { set_vis(vis, Visibility::Hidden); }

/// Click handler — applies the mod and consumes the slot. Costs
/// `SHOP_ITEM_COST` scrap; does nothing when the player can't afford
/// it (slot and scrap both untouched).
pub fn handle_shop_mod_click(
    open: Res<CustomizeOpen>,
    mouse: Res<ButtonInput<MouseButton>>,
    drag: Res<DragState>,
    shop: Option<ResMut<CustomizeShop>>,
    mut stats: ResMut<PlayerStats>,
    mut scrap: ResMut<crate::Scrap>,
    btn_q: Query<(&Transform, &HitArea, &ShopModSlot)>,
) {
    if !open.open { return; }
    if !mouse.just_pressed(MouseButton::Left) { return; }
    if drag.picked.is_some() { return; }
    let Some(cursor) = drag.spec_cursor else { return };
    let Some(mut shop) = shop else { return };
    if scrap.0 < super::drag::SHOP_ITEM_COST { return; }
    for (tf, hit, slot) in &btn_q {
        let centre = tf.translation.truncate();
        let half = hit.size * 0.5;
        if cursor.x < centre.x - half.x
            || cursor.x > centre.x + half.x
            || cursor.y < centre.y - half.y
            || cursor.y > centre.y + half.y
        { continue; }
        let Some(slot_entry) = shop.mods.get_mut(slot.idx) else { return };
        let Some(m) = *slot_entry else { return };
        // Apply every stat change defined by this mod's spec.
        // Pure-buff mods carry one change; trade-off mods carry
        // two (or more) — the apply loop is identical either way.
        for &(kind, delta) in m.spec().changes {
            let stat = kind.stat_mut(&mut stats);
            stat.flat += delta;
        }
        *slot_entry = None;
        scrap.0 = scrap.0.saturating_sub(super::drag::SHOP_ITEM_COST);
        return;
    }
}
