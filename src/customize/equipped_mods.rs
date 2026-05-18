//! "Equipped mods" grid — small read-only panel below the stats
//! column showing every mod the player has bought this run with a
//! per-mod count. Hovering a cell opens the same tooltip pipeline
//! the shop mod cards use, so the player can re-read what a stack
//! is doing without having to remember each card's prose.
//!
//! Layout pattern mirrors `shop_mods` (sprite outline + dark fill +
//! short text label, all on `UPSCALE_LAYER`). A fixed pool of cell
//! entities is spawned once at setup; the per-frame syncer fills
//! them from [`PurchasedMods`] and hides any unused slot.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;

use crate::balance::{CUSTOMIZE_LAYER, UPSCALE_LAYER};

use super::drag::{PurchasedMods, MOD_LIBRARY};
use super::render::CustomizeViewport;
use super::setup::HitArea;
use super::CustomizeOpen;

/// Grid columns × rows. The band-below-sell layout uses two
/// rows of seven cells = 14 slots; any mods past the 14th
/// unique pick are silently skipped (their stat effects still
/// apply — this panel is purely a read-out).
const COLS: usize = 7;
const ROWS: usize = 2;
/// Spec-pixel size of one cell. Width tuned so a seven-tile row
/// fits between roughly the sell-strip's left edge and the right
/// edge of the stats panel; cells stay wide enough for the
/// longest mod names ("GLASS CANNON" / "MONOMANIAC") at the
/// chunky 10pt font.
const CELL_W: f32 = 28.0;
const CELL_H: f32 = 7.0;
const GAP_X: f32 = 2.0;
const GAP_Y: f32 = 1.0;
/// Native-pixel border thickness on the rarity-tinted outline.
/// Matched to `shop_mods::BORDER_PX` so the equipped-mod cells
/// read as a row-of-cards visual sibling to the shop cards.
const BORDER_PX: f32 = 2.0;
const CELL_FONT: f32 = 9.0;
/// Native-pixel font for the corner count badge. Sits one step
/// ABOVE the cell-name font so the stack count reads at a glance
/// without crowding the name — small enough still to feel like a
/// tag, large enough to be legible at the design resolution.
const COUNT_FONT: f32 = 11.0;

const Z_OUTLINE: f32 = 99.0;
const Z_FILL: f32 = 99.5;
const Z_TEXT: f32 = 100.0;

/// Marker on the outline sprite for one grid cell. `cell_idx` is
/// the slot's position in the grid (`row * COLS + col`); the live
/// `spec_idx` (into `MOD_LIBRARY`) is written by the sync system
/// onto the matching [`EquippedModHover`] each frame.
#[derive(Component, Clone, Copy)]
pub struct EquippedModOutline {
    pub cell_idx: usize,
    pub spec_pos: Vec2,
}

/// Dark fill sprite (rendered above the outline).
#[derive(Component, Clone, Copy)]
pub struct EquippedModFill {
    pub cell_idx: usize,
    pub spec_pos: Vec2,
}

/// Cell label text — the mod's name, centred in the cell.
#[derive(Component, Clone, Copy)]
pub struct EquippedModText {
    pub cell_idx: usize,
    pub spec_pos: Vec2,
}

/// Small "xN" count badge perched in the top-right corner of the
/// cell. Hidden when the stack count is 1.
#[derive(Component, Clone, Copy)]
pub struct EquippedModCountText {
    pub cell_idx: usize,
    pub spec_pos: Vec2,
}

/// Click-test hit area for the cell. `spec_idx = None` means the
/// cell is empty; the tooltip system skips it. Filled in each frame
/// by [`update_equipped_mods_grid`].
#[derive(Component, Clone, Copy)]
pub struct EquippedModHover {
    pub cell_idx: usize,
    pub spec_idx: Option<usize>,
}

/// Spawn the 12 cell entities. `right_edge_x` is the spec
/// x-coordinate the rightmost column right-aligns to; `top_row_y`
/// is the centre y of the first (top) row. Cells flow leftward +
/// downward from there.
pub fn spawn_equipped_mods_grid(
    commands: &mut Commands,
    font: &crate::fonts::PixelFont,
    right_edge_x: f32,
    top_row_y: f32,
) {
    let total_w = CELL_W * COLS as f32 + GAP_X * (COLS as f32 - 1.0);
    let leftmost_centre = right_edge_x - total_w + CELL_W * 0.5;

    let row_step = CELL_H + GAP_Y;
    let col_step = CELL_W + GAP_X;
    let first_row_y = top_row_y;
    for row in 0..ROWS {
        for col in 0..COLS {
            let cell_idx = row * COLS + col;
            let x = leftmost_centre + col as f32 * col_step;
            let y = first_row_y - row as f32 * row_step;
            let spec_pos = Vec2::new(x, y);
            commands.spawn((
                Sprite {
                    color: Color::WHITE,
                    custom_size: Some(Vec2::splat(1.0)),
                    ..default()
                },
                Transform::from_xyz(0.0, 0.0, Z_OUTLINE),
                Visibility::Hidden,
                RenderLayers::layer(UPSCALE_LAYER),
                EquippedModOutline { cell_idx, spec_pos },
            ));
            commands.spawn((
                Sprite {
                    color: Color::srgb(0.13, 0.14, 0.17),
                    custom_size: Some(Vec2::splat(1.0)),
                    ..default()
                },
                Transform::from_xyz(0.0, 0.0, Z_FILL),
                Visibility::Hidden,
                RenderLayers::layer(UPSCALE_LAYER),
                EquippedModFill { cell_idx, spec_pos },
            ));
            commands.spawn((
                Text2d::new(""),
                crate::fonts::pixel_text_font(font, CELL_FONT),
                TextColor(Color::srgb(0.92, 0.94, 0.97)),
                TextLayout::new_with_justify(JustifyText::Center),
                Transform::from_xyz(0.0, 0.0, Z_TEXT),
                Visibility::Hidden,
                RenderLayers::layer(UPSCALE_LAYER),
                EquippedModText { cell_idx, spec_pos },
            ));
            // No count-badge entity at setup time — the per-frame
            // syncer spawns one for each cell whose stack count
            // crosses to >1, and despawns it when the count drops
            // back to 1. Keeps the entity churn proportional to
            // actual stacks.
            //
            // Hit area lives on the customize layer (spec coords) so
            // the tooltip's cursor-in-rect test picks it up.
            commands.spawn((
                Transform::from_translation(spec_pos.extend(2.0)),
                HitArea { size: Vec2::new(CELL_W, CELL_H) },
                EquippedModHover { cell_idx, spec_idx: None },
                RenderLayers::layer(CUSTOMIZE_LAYER),
            ));
        }
    }
}

/// Per-frame: fill each cell with its assigned mod (or hide it if
/// the player hasn't bought that many unique mods yet). Pulls the
/// rarity border tint + name straight from `MOD_LIBRARY` so adding
/// a new mod requires zero changes here.
pub fn update_equipped_mods_grid(
    mut commands: Commands,
    open: Res<CustomizeOpen>,
    viewport: Res<CustomizeViewport>,
    ui_scale: Res<bevy::ui::UiScale>,
    purchased: Res<PurchasedMods>,
    pixel_font: Res<crate::fonts::PixelFont>,
    mut outlines: Query<(&EquippedModOutline, &mut Visibility, &mut Transform, &mut Sprite),
        (Without<EquippedModFill>, Without<EquippedModText>,
         Without<EquippedModCountText>)>,
    mut fills: Query<(&EquippedModFill, &mut Visibility, &mut Transform, &mut Sprite),
        (Without<EquippedModOutline>, Without<EquippedModText>,
         Without<EquippedModCountText>)>,
    mut texts: Query<(&EquippedModText, &mut Visibility, &mut Transform, &mut Text2d),
        (Without<EquippedModOutline>, Without<EquippedModFill>,
         Without<EquippedModCountText>)>,
    mut counts: Query<(Entity, &EquippedModCountText, &mut Transform, &mut Text2d),
        (Without<EquippedModOutline>, Without<EquippedModFill>,
         Without<EquippedModText>)>,
    mut hovers: Query<&mut EquippedModHover>,
) {
    let visible = open.open;
    if !visible {
        for (_, mut v, _, _) in &mut outlines { set_vis(&mut v, Visibility::Hidden); }
        for (_, mut v, _, _) in &mut fills    { set_vis(&mut v, Visibility::Hidden); }
        for (_, mut v, _, _) in &mut texts    { set_vis(&mut v, Visibility::Hidden); }
        // Drop every count badge when the panel is hidden — they're
        // re-spawned on re-open if any stack still has count > 1.
        for (e, _, _, _) in &counts { commands.entity(e).despawn(); }
        return;
    }
    let s = viewport.display_scale;
    let glyph = ui_scale.0.max(0.0001);
    let fill_size = Vec2::new(CELL_W * s, CELL_H * s);
    let outline_size = fill_size + Vec2::splat(2.0 * BORDER_PX);

    // Look up each cell's assignment by `cell_idx`. Cells beyond
    // the purchased list get hidden.
    let slot_for = |cell_idx: usize| -> Option<(usize, u32)> {
        purchased.entries.get(cell_idx).copied()
    };

    for (slot, mut vis, mut tf, mut sprite) in &mut outlines {
        let occupied = slot_for(slot.cell_idx);
        set_vis(&mut vis, if occupied.is_some() { Visibility::Inherited } else { Visibility::Hidden });
        let Some((spec_idx, _)) = occupied else { continue };
        if sprite.custom_size != Some(outline_size) { sprite.custom_size = Some(outline_size); }
        tf.translation.x = slot.spec_pos.x * s;
        tf.translation.y = slot.spec_pos.y * s;
        let want = MOD_LIBRARY
            .get(spec_idx)
            .map(|spec| spec.rarity.border_color())
            .unwrap_or(Color::WHITE);
        if sprite.color != want { sprite.color = want; }
    }
    for (slot, mut vis, mut tf, mut sprite) in &mut fills {
        let occupied = slot_for(slot.cell_idx);
        set_vis(&mut vis, if occupied.is_some() { Visibility::Inherited } else { Visibility::Hidden });
        if occupied.is_none() { continue; }
        if sprite.custom_size != Some(fill_size) { sprite.custom_size = Some(fill_size); }
        tf.translation.x = slot.spec_pos.x * s;
        tf.translation.y = slot.spec_pos.y * s;
    }
    for (slot, mut vis, mut tf, mut text) in &mut texts {
        let occupied = slot_for(slot.cell_idx);
        set_vis(&mut vis, if occupied.is_some() { Visibility::Inherited } else { Visibility::Hidden });
        let Some((spec_idx, _)) = occupied else { continue };
        tf.translation.x = slot.spec_pos.x * s;
        tf.translation.y = slot.spec_pos.y * s;
        let want_scale = Vec3::new(glyph, glyph, 1.0);
        if tf.scale != want_scale { tf.scale = want_scale; }
        let name = MOD_LIBRARY
            .get(spec_idx)
            .map(|spec| spec.name)
            .unwrap_or("---");
        if text.0 != name { text.0 = name.to_string(); }
    }

    // Reconcile count badges. One entity per cell whose stack
    // count is > 1; cells with count == 1 (or empty) carry no
    // badge entity at all.
    let want_scale = Vec3::new(glyph, glyph, 1.0);
    // First pass: walk existing badges, update or schedule despawn.
    let mut covered = [false; COLS * ROWS];
    for (e, cnt, mut tf, mut text) in &mut counts {
        let want = slot_for(cnt.cell_idx).filter(|(_, c)| *c > 1).map(|(_, c)| c);
        let Some(c) = want else {
            commands.entity(e).despawn();
            continue;
        };
        if cnt.cell_idx < covered.len() { covered[cnt.cell_idx] = true; }
        tf.translation.x = cnt.spec_pos.x * s;
        tf.translation.y = cnt.spec_pos.y * s;
        if tf.scale != want_scale { tf.scale = want_scale; }
        let label = format!("x{}", c);
        if text.0 != label { text.0 = label; }
    }
    // Second pass: spawn fresh badges for cells that need one and
    // didn't already have an entity. The cell's spec_pos is
    // recovered from the matching `EquippedModText` (the outline,
    // fill, text, and count badge all share the same anchor).
    for (text_slot, _, _, _) in &texts {
        let cell_idx = text_slot.cell_idx;
        if covered.get(cell_idx).copied().unwrap_or(false) { continue; }
        let Some((_, c)) = slot_for(cell_idx) else { continue };
        if c <= 1 { continue; }
        // Anchor the badge to the cell's top-right corner so a
        // bigger glyph fits entirely inside the cell instead of
        // spilling past the border.
        let count_spec = text_slot.spec_pos
            + Vec2::new(CELL_W * 0.5 - 1.0, CELL_H * 0.5 - 0.5);
        commands.spawn((
            Text2d::new(format!("x{}", c)),
            crate::fonts::pixel_text_font(&pixel_font, COUNT_FONT),
            TextColor(Color::srgb(1.0, 0.85, 0.30)),
            bevy::sprite::Anchor::TopRight,
            Transform {
                translation: Vec3::new(count_spec.x * s, count_spec.y * s, Z_TEXT + 0.5),
                scale: want_scale,
                ..default()
            },
            Visibility::Inherited,
            RenderLayers::layer(UPSCALE_LAYER),
            EquippedModCountText { cell_idx, spec_pos: count_spec },
        ));
    }

    // Write the per-cell spec_idx onto the hover marker so the
    // tooltip system can fish it out without re-deriving from
    // `PurchasedMods` (and without taking an extra system arg).
    for mut hover in &mut hovers {
        let new = slot_for(hover.cell_idx).map(|(idx, _)| idx);
        if hover.spec_idx != new { hover.spec_idx = new; }
    }
}

fn set_vis(vis: &mut Visibility, want: Visibility) {
    if *vis != want { *vis = want; }
}
