//! Orphan-rune visuals: when a stack-merge moves a turret off its
//! slot, the runes that were socketed on the source stay behind as
//! "orphans" instead of vanishing. The player gets a chance to
//! re-equip them; anything still orphaned when the shop closes is
//! wiped.
//!
//! Detection is derived state, not stored — a slot is an orphan
//! source iff `!equipped && runes.iter().any(|r| r.is_some())`. The
//! merge path in `drag::resolve_drop` is the only place that
//! produces this combination; sell / sell-strip / drag-pickup all
//! clear runes alongside `equipped`.
//!
//! Visuals: one `!` Text2d per (slot, socket) pair, spawned once at
//! `setup_customize_ui` and kept hidden unless the matching socket
//! is currently displaying an orphan rune. Per-frame jitter shakes
//! the visible markers so the eye lands on them.
//!
//! Cleanup: `clear_orphan_runes_on_exit` wipes every unequipped
//! slot's rune array on `OnExit(AppState::Customize)`.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;
use bevy::sprite::Anchor;

use crate::balance::UPSCALE_LAYER;
use crate::turret::TurretConfig;

use super::render::CustomizeViewport;
use super::CustomizeOpen;

/// Marker for the `!` text floating above a single ship rune socket.
/// One entity per (slot, rune_idx); the spec position is the
/// socket's exact world-space spec coordinate plus a small upward
/// nudge so the glyph reads as "ALERT — orphan here."
#[derive(Component, Clone, Copy)]
pub struct OrphanWarningMark {
    pub slot: usize,
    pub rune_idx: usize,
    /// Resting spec-pixel position of the marker. The per-frame
    /// shake system applies a jitter on top of this; visibility +
    /// position lookups read from here so the geometry stays a
    /// pure function of (slot, rune_idx).
    pub spec_pos: Vec2,
}

/// Spec-pixel vertical offset above the socket centre so the `!`
/// sits clearly outside the socket geometry. Adjusted alongside
/// `setup::SOCKET` if the socket size ever changes.
pub const MARK_VERTICAL_OFFSET: f32 = 7.0;
/// Native-pixel font size for the `!` glyph. Bigger than the
/// surrounding socket labels so it shouts.
const MARK_FONT: f32 = 14.0;
/// Native-pixel shake amplitude on each axis. Stays small —
/// the goal is "wiggle" not "earthquake".
const SHAKE_AMP_PX: f32 = 2.0;
/// Angular frequency of the shake (rad/s). Two distinct
/// frequencies on x vs y so the jitter doesn't read as a
/// straight-line oscillation.
const SHAKE_FREQ_X: f32 = 18.0;
const SHAKE_FREQ_Y: f32 = 13.5;

/// Spawn the 24 (8 × 3) `!` markers. Called once from
/// `setup_customize_ui` after the rune sockets are placed.
///
/// Markers are spawned hidden; the per-frame syncer toggles
/// visibility based on `TurretConfig` state.
pub fn spawn_orphan_marks(
    commands: &mut Commands,
    font: &crate::fonts::PixelFont,
    socket_positions: [[Vec2; 3]; 8],
) {
    for (slot, sockets) in socket_positions.iter().enumerate() {
        for (rune_idx, &socket_pos) in sockets.iter().enumerate() {
            let spec_pos = socket_pos + Vec2::new(0.0, MARK_VERTICAL_OFFSET);
            commands.spawn((
                Text2d::new("!"),
                crate::fonts::pixel_text_font(font, MARK_FONT),
                TextColor(Color::srgb(1.0, 0.85, 0.30)),
                Anchor::Center,
                Transform::from_xyz(0.0, 0.0, 105.0),
                Visibility::Hidden,
                RenderLayers::layer(UPSCALE_LAYER),
                OrphanWarningMark { slot, rune_idx, spec_pos },
            ));
        }
    }
}

/// Per-frame: derive each marker's "should be visible" state from
/// `TurretConfig`, position from the spec coord × display scale,
/// and a sine-jitter on top. Cheap (24 entities); runs
/// unconditionally and self-gates on `CustomizeOpen` to mirror
/// the rest of the customize sync chain.
pub fn update_orphan_marks(
    open: Res<CustomizeOpen>,
    viewport: Res<CustomizeViewport>,
    ui_scale: Res<bevy::ui::UiScale>,
    time: Res<bevy::time::Time<bevy::time::Real>>,
    cfg: Res<TurretConfig>,
    mut q: Query<(&OrphanWarningMark, &mut Transform, &mut Visibility)>,
) {
    if !open.open {
        for (_, _, mut v) in &mut q {
            if *v != Visibility::Hidden {
                *v = Visibility::Hidden;
            }
        }
        return;
    }
    let s = viewport.display_scale;
    let glyph = ui_scale.0.max(0.0001);
    let t = time.elapsed_secs();
    for (mark, mut tf, mut vis) in &mut q {
        let orphan_here = is_orphan_socket(&cfg, mark.slot, mark.rune_idx);
        let want_vis = if orphan_here {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *vis != want_vis {
            *vis = want_vis;
        }
        if !orphan_here {
            continue;
        }
        // Phase per (slot, rune_idx) so adjacent markers don't
        // jitter in lock-step — the eye reads chaotic motion as
        // alarm, synchronised motion as decoration.
        let phase = mark.slot as f32 * 0.7 + mark.rune_idx as f32 * 1.3;
        let jx = ((t * SHAKE_FREQ_X) + phase).sin() * SHAKE_AMP_PX;
        let jy = ((t * SHAKE_FREQ_Y) + phase * 1.7).cos() * SHAKE_AMP_PX;
        tf.translation.x = mark.spec_pos.x * s + jx;
        tf.translation.y = mark.spec_pos.y * s + jy;
        let want_scale = Vec3::new(glyph, glyph, 1.0);
        if tf.scale != want_scale {
            tf.scale = want_scale;
        }
    }
}

/// Predicate: is this (slot, rune_idx) currently displaying an
/// orphan rune? True iff the slot is unequipped (turret moved
/// out via merge) but the socket still holds a rune.
pub fn is_orphan_socket(cfg: &TurretConfig, slot: usize, rune_idx: usize) -> bool {
    let Some(s) = cfg.slots.get(slot) else { return false };
    !s.equipped && s.runes.get(rune_idx).map_or(false, |r| r.is_some())
}

/// `OnExit(AppState::Customize)` hook: wipe every unequipped slot's
/// rune array so the orphans don't carry over into combat. Equipped
/// slots are untouched. Runs alongside the other customize teardown
/// (sell-strip, drag state, etc.).
pub fn clear_orphan_runes_on_exit(mut cfg: ResMut<TurretConfig>) {
    for slot in cfg.slots.iter_mut() {
        if !slot.equipped {
            slot.runes = [None; 3];
        }
    }
}
