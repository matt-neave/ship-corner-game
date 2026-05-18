//! UI module — orchestrator + cross-surface types.
//!
//! `hud`   — always-on chrome: score banner, FPS, VSYNC/MAP/FOLLOW
//!           buttons, top-left HP bar (player + ally rows), draft
//!           prompt.
//!
//! Customisation of turret loadouts lives entirely in the customize
//! overlay (shop). `ui_button_system` here routes only the HUD-corner
//! toggles (VSYNC / RETURN-TO-MAP / CAMERA FOLLOW) through a single
//! `Changed<Interaction>` query.

use bevy::prelude::*;

mod difficulty_meter;
mod hud;
mod map_hint;
mod peek_back;
mod wave_indicator;

pub use hud::{
    sync_ally_hp_bars, update_ally_hp_values, update_fps_text, update_hp_bar_pixel_scale,
    update_map_button, update_score_text,
    update_vsync_label, sync_hud_dev_buttons_visibility,
    update_wave_ui, AllyHpRow, CameraFollowButton, FpsText, ReturnToMapButton, ScoreText,
    VsyncButton, WaveHpUi,
};
pub use wave_indicator::{setup_wave_indicator, update_wave_indicator};
pub use map_hint::{setup_map_hint, update_map_hint};
pub use difficulty_meter::{setup_difficulty_meter, update_difficulty_meter};
pub use peek_back::{setup_peek_back, update_peek_back, handle_peek_back_click};

use crate::map::ViewMode;
use crate::modes::VsyncMode;

// ---------- Shared types ----------

/// Tag on every clickable HUD-corner button.
#[derive(Component)]
pub struct SlotButton { pub kind: ButtonKind }

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    ToggleVsync,
    /// Click → switch to `ViewMode::Map`. Visible only in Combat view.
    ReturnToMap,
    /// Toggles the play camera between fixed (default) and follow-the-player.
    ToggleCameraFollow,
}

// ---------- Resources ----------

/// Per-slot tally of damage actually dealt to enemies (overkill is clamped to
/// the enemy's remaining HP). Bullets / beams write to it; the LHS panel's
/// share bars read it.
#[derive(Resource, Default)]
pub struct DamageStats {
    pub per_slot: [u64; 8],
    /// Per-ally-class damage, indexed by `ShipClass::to_index()`.
    /// Sized to `ShipClass::COUNT` (currently 6).
    pub per_ally: [u64; crate::ally::ShipClass::COUNT],
    pub total: u64,
}

/// Reset the per-run damage tally. Run on screen transitions
/// (`OnExit(Customize)` and `OnExit(MainMenu)`) so each combat
/// stage starts with fresh stats. Lives here now that the in-game
/// damage-panel UI has been removed — the stats themselves are
/// still useful for the post-game customize-shop "damage share"
/// rollups attached to weapon / rune tooltips.
pub fn reset_damage_stats(mut stats: ResMut<DamageStats>) {
    stats.per_slot = [0; 8];
    stats.per_ally = [0; crate::ally::ShipClass::COUNT];
    stats.total = 0;
}

// ---------- Setup ----------

pub fn setup_ui(
    mut commands: Commands,
    thaleah: Res<crate::fonts::ThaleahFont>,
) {
    hud::setup_hud(&mut commands, &thaleah);
}

// ---------- Click router ----------

/// Click handler for the HUD-corner toggle buttons.
pub fn ui_button_system(
    mut interactions: Query<(&Interaction, &SlotButton), Changed<Interaction>>,
    mut vsync: ResMut<VsyncMode>,
    mut view: ResMut<ViewMode>,
    mut camera_follow: ResMut<crate::modes::CameraFollow>,
) {
    for (interaction, btn) in &mut interactions {
        if !matches!(*interaction, Interaction::Pressed) { continue; }
        match btn.kind {
            ButtonKind::ToggleVsync       => vsync.enabled = !vsync.enabled,
            ButtonKind::ReturnToMap       => *view = ViewMode::Map,
            ButtonKind::ToggleCameraFollow => camera_follow.active = !camera_follow.active,
        }
    }
}
