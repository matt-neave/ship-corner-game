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

mod damage_panel;
mod hud;
mod wave_indicator;

pub use hud::{
    sync_ally_hp_bars, update_ally_hp_values, update_fps_text, update_hp_bar_pixel_scale,
    update_hp_subdividers, update_map_button, update_score_text, update_vsync_label,
    update_wave_ui, AllyHpRow, CameraFollowButton, FpsText, ReturnToMapButton, ScoreText,
    VsyncButton, WaveHpUi,
};
pub use damage_panel::{
    reset_damage_stats, setup_damage_panel, sync_damage_panel_visibility,
    update_damage_panel, update_damage_row_icons,
};
pub use wave_indicator::{setup_wave_indicator, update_wave_indicator};

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

// ---------- Setup ----------

pub fn setup_ui(mut commands: Commands) {
    hud::setup_hud(&mut commands);
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
