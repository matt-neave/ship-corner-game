//! UI module — orchestrator + cross-surface types.
//!
//! Two submodules cover the two surfaces:
//! - `hud`   — always-on chrome: score banner, FPS, VSYNC/MAP/FOLLOW
//!             buttons, top-left HP bar (player + ally rows), draft
//!             prompt, desktop hint.
//! - `panel` — LHS turret-control panel. **Debug-only surface** now
//!             that the customize overlay owns the production loadout
//!             flow; kept for fast stat iteration.
//!
//! `ui_button_system` lives here because it routes both panel-side
//! actions (Equip / ±dmg / ±rate / ±brrl / ±rune) AND HUD-side toggles
//! (NightMode / CrtMode / WaveMode / Vsync / ReturnToMap / CameraFollow)
//! through a single `Changed<Interaction>` query, so duplicating it into
//! per-submodule handlers would be cost without benefit.

use bevy::prelude::*;

mod damage_panel;
mod hud;
mod panel;
mod wave_indicator;

pub use hud::{
    sync_ally_hp_bars, update_ally_hp_values, update_fps_text, update_hp_bar_pixel_scale,
    update_hp_subdividers, update_map_button, update_score_text, update_vsync_label,
    update_wave_ui, AllyHpRow, CameraFollowButton, FpsText, ReturnToMapButton, ScoreText,
    WaveHpUi,
};
pub use damage_panel::{
    reset_damage_stats, setup_damage_panel, sync_damage_panel_visibility,
    update_damage_panel, update_damage_row_icons,
};
pub use panel::{update_damage_bars, update_slot_labels, UiPanel};
pub use wave_indicator::{setup_wave_indicator, update_wave_indicator};

use crate::map::ViewMode;
use crate::modes::{CrtMode, NightMode, VsyncMode, WindowMode};
use crate::rune::{cycle_next, cycle_prev};
use crate::turret::TurretConfig;
use crate::weapon::WeaponType;

// ---------- Shared types ----------

/// Tag on every clickable button in the LHS panel + HUD corner buttons.
/// `slot` is the turret index for slot-specific buttons; ignored (use 0)
/// for header/HUD toggles.
#[derive(Component)]
pub struct SlotButton { pub slot: usize, pub kind: ButtonKind }

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    // Panel-side (turret slot mutations)
    Equip,
    DamageUp, DamageDown,
    RateUp, RateDown,
    BarrelsUp, BarrelsDown,
    RuneUp, RuneDown,
    // Panel-header toggles
    ToggleDesktopMode,
    ToggleNightMode,
    ToggleCrtMode,
    // HUD corner toggles
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

/// Top-level UI spawn. Calls into both submodules.
pub fn setup_ui(mut commands: Commands) {
    hud::setup_hud(&mut commands);
    panel::setup_panel(&mut commands);
}

/// Prototype-mode helper — keeps the LHS panel hidden every frame so
/// other systems (e.g. customize chrome toggle, window-mode switch)
/// can't sneak it back on. Lift the gate when the panel is wanted
/// again.
pub fn force_hide_ui_panel(
    mut q: Query<&mut Visibility, With<UiPanel>>,
) {
    for mut v in &mut q {
        if *v != Visibility::Hidden { *v = Visibility::Hidden; }
    }
}

// ---------- Click router ----------

/// Single click handler covering every `SlotButton` in the UI. Branches
/// per `ButtonKind`: HUD toggles update mode/view resources; panel
/// actions mutate `TurretConfig`.
pub fn ui_button_system(
    mut interactions: Query<(&Interaction, &SlotButton), Changed<Interaction>>,
    mut cfg: ResMut<TurretConfig>,
    mut window_mode: ResMut<WindowMode>,
    mut night: ResMut<NightMode>,
    mut crt: ResMut<CrtMode>,
    mut vsync: ResMut<VsyncMode>,
    mut view: ResMut<ViewMode>,
    mut camera_follow: ResMut<crate::modes::CameraFollow>,
) {
    for (interaction, btn) in &mut interactions {
        if !matches!(*interaction, Interaction::Pressed) { continue; }
        match btn.kind {
            ButtonKind::ToggleDesktopMode => {
                window_mode.desktop = !window_mode.desktop;
                continue;
            }
            ButtonKind::ToggleNightMode => {
                night.active = !night.active;
                continue;
            }
            ButtonKind::ToggleCrtMode => {
                crt.active = !crt.active;
                continue;
            }
            ButtonKind::ToggleVsync => {
                vsync.enabled = !vsync.enabled;
                continue;
            }
            ButtonKind::ReturnToMap => {
                *view = ViewMode::Map;
                continue;
            }
            ButtonKind::ToggleCameraFollow => {
                camera_follow.active = !camera_follow.active;
                continue;
            }
            _ => {}
        }
        let s = &mut cfg.slots[btn.slot];
        match btn.kind {
            ButtonKind::ToggleDesktopMode | ButtonKind::ToggleNightMode
            | ButtonKind::ToggleCrtMode
            | ButtonKind::ToggleVsync    | ButtonKind::ReturnToMap
            | ButtonKind::ToggleCameraFollow => unreachable!(),
            // Debug-panel rune controls cycle the FIRST rune socket only.
            // The customize overlay is the proper UI for the other two.
            ButtonKind::RuneUp   => { if s.equipped { s.runes[0] = cycle_next(s.runes[0]); } }
            ButtonKind::RuneDown => { if s.equipped { s.runes[0] = cycle_prev(s.runes[0]); } }
            ButtonKind::Equip => {
                // Cycle: unequipped → Standard → Sniper → MG → Shotgun →
                // Railgun → unequipped. On each step, snap stats to the
                // new weapon's defaults so the type's identity is felt
                // immediately. Player can still tweak with ±.
                if !s.equipped {
                    s.equipped = true;
                    s.weapon = WeaponType::Standard;
                    let (d, r) = s.weapon.defaults();
                    s.damage = d; s.fire_rate = r; s.barrels = 1;
                } else {
                    match s.weapon.next() {
                        Some(next) => {
                            s.weapon = next;
                            let (d, r) = next.defaults();
                            s.damage = d; s.fire_rate = r; s.barrels = 1;
                        }
                        None => {
                            s.equipped = false;
                            s.weapon = WeaponType::Standard;
                            let (d, r) = s.weapon.defaults();
                            s.damage = d; s.fire_rate = r; s.barrels = 1;
                            s.runes = [None; 3];
                        }
                    }
                }
            }
            ButtonKind::DamageUp    => { if s.equipped { s.damage += 1; } }
            ButtonKind::DamageDown  => { if s.equipped && s.damage > 1 { s.damage -= 1; } }
            ButtonKind::RateUp      => { if s.equipped { s.fire_rate += 0.1; } }
            ButtonKind::RateDown    => { if s.equipped && s.fire_rate > 0.2 { s.fire_rate -= 0.1; } }
            ButtonKind::BarrelsUp   => { if s.equipped && s.barrels < 3 { s.barrels += 1; } }
            ButtonKind::BarrelsDown => { if s.equipped && s.barrels > 1 { s.barrels -= 1; } }
        }
    }
}
