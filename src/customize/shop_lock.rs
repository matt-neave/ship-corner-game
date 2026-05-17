//! Per-slot lock-state visual indicator for the shop.
//!
//! Right-click on a turret / rune / mod slot toggles the matching
//! lock flag in `CustomizeShop` (handled by `update::handle_right_click_lock`).
//! This module renders the visceral feedback: a bright gold frame
//! around the slot + a small padlock icon in the corner.
//!
//! Per slot we spawn five sprites (4 frame edges + 1 corner badge),
//! all hidden by default. `sync_lock_badges` flips visibility each
//! frame from the lock arrays. Everything lives on `UPSCALE_LAYER`
//! so it draws above the slot contents.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;

use crate::balance::UPSCALE_LAYER;

use super::render::CustomizeViewport;
use super::CustomizeOpen;

/// Which slot family a lock badge belongs to. Drives which lock
/// array `sync_lock_badges` reads to decide if this badge shows.
#[derive(Component, Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShopSlotKind {
    Turret,
    Rune,
    Mod,
}

/// One lock-badge entity attached to a specific shop slot.
/// `spec_pos` + `spec_size` capture the slot's spec-coord geometry
/// so the badge can size + position itself each frame relative to
/// the live `display_scale`.
#[derive(Component, Clone, Copy)]
pub struct ShopLockBadge {
    pub kind: ShopSlotKind,
    pub idx: usize,
    pub spec_pos: Vec2,
    pub spec_size: Vec2,
    /// Which piece of the badge composition this entity is.
    /// Drives sizing in `sync_lock_badges` without per-piece
    /// marker types.
    pub piece: LockBadgePiece,
}

/// Sub-pieces of one lock badge. `Frame*` are the four border
/// rectangles; `Padlock` is the corner icon.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LockBadgePiece {
    FrameTop,
    FrameBottom,
    FrameLeft,
    FrameRight,
    Padlock,
}

/// Bright accent gold for the lock frame + padlock icon. Matches
/// the theme accent so the badge feels of-a-piece with the shop
/// chrome but stands out clearly against the dark slot backgrounds.
const LOCK_COLOR: Color = Color::srgb(1.0, 0.85, 0.30);
/// Frame thickness in spec px. Thick enough to read as "border"
/// rather than "outline" from any zoom.
const FRAME_THICK: f32 = 1.5;
/// Padlock-icon size in spec px (square).
const PADLOCK_SIZE: f32 = 4.0;

/// Z above the slot content so the gold frame paints on top.
const Z_LOCK_FRAME: f32 = 110.0;
const Z_LOCK_ICON: f32 = 110.5;

/// Spawn the full 5-piece lock badge for one slot. Hidden by
/// default; `sync_lock_badges` flips visibility each frame.
pub fn spawn_lock_badge(
    commands: &mut Commands,
    kind: ShopSlotKind,
    idx: usize,
    spec_pos: Vec2,
    spec_size: Vec2,
) {
    use LockBadgePiece::*;
    for piece in [FrameTop, FrameBottom, FrameLeft, FrameRight, Padlock] {
        commands.spawn((
            Sprite {
                color: LOCK_COLOR,
                custom_size: Some(Vec2::ONE), // sized each frame
                ..default()
            },
            Transform::from_xyz(0.0, 0.0, if matches!(piece, Padlock) { Z_LOCK_ICON } else { Z_LOCK_FRAME }),
            Visibility::Hidden,
            RenderLayers::layer(UPSCALE_LAYER),
            ShopLockBadge { kind, idx, spec_pos, spec_size, piece },
        ));
    }
}

/// Per-frame: size + position each badge piece from its slot's
/// spec geometry × the live `display_scale`, and toggle visibility
/// from the matching lock array. Hides everything if the customize
/// overlay is closed.
pub fn sync_lock_badges(
    open: Res<CustomizeOpen>,
    shop: Option<Res<super::drag::CustomizeShop>>,
    viewport: Res<CustomizeViewport>,
    mut q: Query<(&ShopLockBadge, &mut Visibility, &mut Transform, &mut Sprite)>,
) {
    let s = viewport.display_scale;
    let shop_open = open.open && shop.is_some();
    for (badge, mut vis, mut tf, mut sprite) in &mut q {
        let locked = if shop_open {
            let shop = shop.as_deref().unwrap();
            match badge.kind {
                ShopSlotKind::Turret => shop
                    .turrets_locked
                    .get(badge.idx)
                    .copied()
                    .unwrap_or(false)
                    && shop.turrets.get(badge.idx).copied().flatten().is_some(),
                ShopSlotKind::Rune => shop
                    .runes_locked
                    .get(badge.idx)
                    .copied()
                    .unwrap_or(false)
                    && shop.runes.get(badge.idx).copied().flatten().is_some(),
                ShopSlotKind::Mod => shop
                    .mods_locked
                    .get(badge.idx)
                    .copied()
                    .unwrap_or(false)
                    && shop.mods.get(badge.idx).copied().flatten().is_some(),
            }
        } else {
            false
        };
        let want_vis = if locked { Visibility::Inherited } else { Visibility::Hidden };
        if *vis != want_vis { *vis = want_vis; }
        if !locked { continue; }

        // Native-pixel layout: spec_pos + spec_size scaled by
        // display_scale. Frame edges hug the slot bounds; the
        // padlock icon sits in the top-right corner.
        let centre = badge.spec_pos * s;
        let half = badge.spec_size * 0.5 * s;
        let thick_native = FRAME_THICK * s;
        let frame_w = badge.spec_size.x * s + 2.0 * thick_native;
        let frame_h = badge.spec_size.y * s + 2.0 * thick_native;
        match badge.piece {
            LockBadgePiece::FrameTop => {
                tf.translation.x = centre.x;
                tf.translation.y = centre.y + half.y + thick_native * 0.5;
                let size = Vec2::new(frame_w, thick_native);
                if sprite.custom_size != Some(size) { sprite.custom_size = Some(size); }
            }
            LockBadgePiece::FrameBottom => {
                tf.translation.x = centre.x;
                tf.translation.y = centre.y - half.y - thick_native * 0.5;
                let size = Vec2::new(frame_w, thick_native);
                if sprite.custom_size != Some(size) { sprite.custom_size = Some(size); }
            }
            LockBadgePiece::FrameLeft => {
                tf.translation.x = centre.x - half.x - thick_native * 0.5;
                tf.translation.y = centre.y;
                let size = Vec2::new(thick_native, frame_h);
                if sprite.custom_size != Some(size) { sprite.custom_size = Some(size); }
            }
            LockBadgePiece::FrameRight => {
                tf.translation.x = centre.x + half.x + thick_native * 0.5;
                tf.translation.y = centre.y;
                let size = Vec2::new(thick_native, frame_h);
                if sprite.custom_size != Some(size) { sprite.custom_size = Some(size); }
            }
            LockBadgePiece::Padlock => {
                let icon_native = PADLOCK_SIZE * s;
                tf.translation.x = centre.x + half.x - icon_native * 0.4;
                tf.translation.y = centre.y + half.y - icon_native * 0.4;
                let size = Vec2::splat(icon_native);
                if sprite.custom_size != Some(size) { sprite.custom_size = Some(size); }
            }
        }
    }
}
