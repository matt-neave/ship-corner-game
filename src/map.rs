//! Map view — a zoomed-out second view where the player picks where to
//! sail next. The same square play area is reused; we just swap what the
//! play camera renders by flipping its `RenderLayers` between
//! `PLAY_LAYER` (combat) and `MAP_LAYER` (map). One camera, two views.
//!
//! Layout: 10 hand-authored irregular sections. Adjacent sections share
//! their boundary corners exactly + use a deterministic `wobble_for_edge`
//! curve so dividers look hand-drawn but match across regions. Outer
//! edges stay straight so the map fills the square cleanly.
//!
//! Movement reuses the in-game pattern (`approach_angle` toward a desired
//! heading, fixed forward speed) — the destination is set by clicking an
//! adjacent section instead of following the cursor continuously.
//!
//! Module split
//! ------------
//! - `build`     — section authoring + polygon/wobble + meshes + fill image
//!                  + view-mode camera toggle + map-fill refresh.
//! - `setup`     — initial spawn (fill sprite, dividers, slot tiles, boat)
//!                  + slot-visual reconciliation + slot-label syncing.
//! - `buildings` — popup + progress bars + tooltip + click handler +
//!                  per-frame economy tick + level resolution.
//! - `hud`       — currency + level status banner + debug panel.
//! - `anim`      — phase-animation timeline + pulse/beam drivers.
//! - `input`     — map click handling + boat steering.

use bevy::prelude::*;

mod anim;
mod build;
mod buildings;
mod hud;
mod input;
mod setup;

pub use anim::{advance_map_anim_timeline, map_begin_phase, update_anim_beams, update_anim_pulses};
pub use build::{apply_view_mode, refresh_map_fill};
pub use buildings::{
    close_popup_on_view_change, handle_building_choice_clicks, level_complete_check,
    level_fail_check, setup_progress_assets, tick_buildings, update_building_button_tints,
    update_building_description, update_building_hover_tooltip, update_building_progress_bars,
};
pub use hud::{
    handle_debug_buttons, setup_currency_ui, setup_debug_ui, setup_level_status_ui,
    update_claim_label, update_currency_ui, update_debug_button_tints,
    update_level_status_ui, update_refined_steel_text, update_scrap_text, update_steel_text,
};
pub use input::{map_boat_movement, map_click_input};
pub use setup::{setup_map, sync_owned_slot_visuals, update_map_slot_labels};

use crate::i18n::tr;
use std::collections::HashMap;

// ---------- Layer + Z constants ----------

/// Render layer for everything visible only in map view. `apply_view_mode`
/// flips the play camera between `PLAY_LAYER` and this.
pub const MAP_LAYER: usize = 3;

/// Z-band used by map entities so they layer cleanly:
///   0.5 = section fills,    0.7  = boundary segments,
///   0.85 = slot box,         0.90 = star marks,
///   1.0  = phase animations (pulses/beams),
///   1.5  = boat token.
pub(crate) const Z_FILL:      f32 = 0.5;
pub(crate) const Z_OUTLINE:   f32 = 0.7;
pub(crate) const Z_SLOT_BOX:  f32 = 0.85;
pub(crate) const Z_SLOT_STAR: f32 = 0.90;
pub(crate) const Z_ANIM:      f32 = 1.0;
pub(crate) const Z_BOAT:      f32 = 1.5;

/// Visual scale of the map boat token relative to its in-combat size.
pub(crate) const MAP_BOAT_SCALE: f32 = 0.5;

/// Slot box geometry. World-space units; the play area is `PLAY_WORLD`
/// (=200) wide so a 10-unit box reads as a small but clickable tile.
pub(crate) const SLOT_SIZE: f32 = 10.0;
pub(crate) const SLOT_HALF: f32 = SLOT_SIZE / 2.0;
/// Star-mark geometry — small filled squares stacked horizontally above
/// the slot. With `STAR_SIZE = 2` and `STAR_GAP = 2`, stars render as
/// 2-px filled squares with 2-px gaps.
pub(crate) const STAR_SIZE: f32 = 2.0;
pub(crate) const STAR_GAP:  f32 = 2.0;
pub(crate) const STAR_Y_OFFSET: f32 = 9.0;

// Animation tuning — short, snappy. Tweak here.
pub(crate) const ANIM_PULSE_DUR: f32 = 0.45;
pub(crate) const ANIM_BEAM_DUR:  f32 = 0.40;
pub(crate) const ANIM_PULSE_PEAK_ALPHA: f32 = 0.55;
pub(crate) const ANIM_BEAM_PEAK_ALPHA:  f32 = 0.85;
pub(crate) const ANIM_PULSE_PEAK_SCALE: f32 = 1.30;
pub(crate) const ANIM_PULSE_SIZE: f32 = SLOT_SIZE + 4.0;
pub(crate) const ANIM_BEAM_THICKNESS: f32 = 1.4;
pub(crate) const ANIM_STEP_OVERLAP: f32 = 0.5;

// ---------- Resources ----------

#[derive(Resource, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Map,
    Combat,
}
impl Default for ViewMode {
    /// Game starts in combat — the player drops straight into level 1.
    fn default() -> Self { ViewMode::Combat }
}

/// Snapshot of the section that triggered the current combat. Written
/// by `map_boat_movement` when the boat crosses into an unowned zone;
/// `spawn_enemies` reads it to scale enemy density by star rating.
#[derive(Resource)]
pub struct CombatContext {
    pub stars: u8,
    /// Total enemies still to spawn this combat.
    pub enemy_budget: u32,
    /// Snapshot of `enemy_budget` at level start, used as the depletion
    /// progress bar's denominator.
    pub enemy_total: u32,
}

impl Default for CombatContext {
    fn default() -> Self {
        let starter = crate::balance::level_enemy_budget(1, 0);
        Self {
            stars: 1,
            enemy_budget: starter,
            enemy_total:  starter,
        }
    }
}

impl CombatContext {
    /// On-screen enemy cap for sandbox-style drip spawning. Linear in stars
    /// at 6 per tier so 5★ = 30, 1★ = 6.
    pub fn enemy_cap(&self) -> usize {
        (6 * self.stars.max(1) as usize).min(30)
    }
}

/// Buildings that can be placed in a section's upgrade slot. Adding a
/// new variant is a four-place edit (variant + label + description +
/// options_for_stars + 2 translation rows).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MapBuilding {
    Weaponry,
    Dockyard,
    Foundry,
    Crane,
    Refinery,
}

impl MapBuilding {
    pub fn label(self) -> &'static str {
        match self {
            MapBuilding::Weaponry => tr("map_building_weaponry"),
            MapBuilding::Dockyard => tr("map_building_dockyard"),
            MapBuilding::Foundry  => tr("map_building_foundry"),
            MapBuilding::Crane    => tr("map_building_crane"),
            MapBuilding::Refinery => tr("map_building_refinery"),
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            MapBuilding::Weaponry => tr("map_building_weaponry_desc"),
            MapBuilding::Dockyard => tr("map_building_dockyard_desc"),
            MapBuilding::Foundry  => tr("map_building_foundry_desc"),
            MapBuilding::Crane    => tr("map_building_crane_desc"),
            MapBuilding::Refinery => tr("map_building_refinery_desc"),
        }
    }

    pub fn options_for_stars(stars: u8) -> Vec<MapBuilding> {
        let mut opts = Vec::new();
        if stars >= 1 { opts.push(MapBuilding::Weaponry); }
        if stars >= 1 { opts.push(MapBuilding::Dockyard); }
        if stars >= 1 { opts.push(MapBuilding::Foundry);  }
        if stars >= 2 { opts.push(MapBuilding::Crane);    }
        if stars >= 3 { opts.push(MapBuilding::Refinery); }
        opts
    }

    pub fn cost_scrap(self) -> u32 {
        match self {
            MapBuilding::Weaponry => 10,
            MapBuilding::Dockyard => 10,
            MapBuilding::Foundry  => 10,
            MapBuilding::Crane    => 20,
            MapBuilding::Refinery => 30,
        }
    }
}

pub struct MapSection {
    pub id: u32,
    pub corners: Vec<Vec2>,
    pub polygon: Vec<Vec2>,
    pub center: Vec2,
    pub adjacencies: Vec<u32>,
    pub stars: u8,
    pub slots: Vec<Option<MapBuilding>>,
}

#[derive(Default)]
pub struct BuildingTickState {
    pub cooldown: f32,
    pub fueled: bool,
}

#[derive(Resource, Default)]
pub struct BuildingTimers {
    pub state: HashMap<(u32, usize), BuildingTickState>,
}

#[derive(Resource)]
pub struct MapState {
    pub sections: Vec<MapSection>,
    pub current: u32,
    pub owned: Vec<bool>,
    pub boat_target: Option<Vec2>,
}

impl MapState {
    pub fn new() -> Self {
        let mut sections = build::build_default_map();
        let stars = compute_stars(&sections, 0);
        for (i, s) in sections.iter_mut().enumerate() {
            s.stars = stars[i];
            s.slots = vec![None; 1];
        }
        let mut owned: Vec<bool> = vec![false; sections.len()];
        owned[0] = true;
        Self { sections, current: 0, owned, boat_target: None }
    }

    pub fn section(&self, id: u32) -> &MapSection {
        &self.sections[id as usize]
    }

    /// Section ids that share a boundary with `section_id`.
    #[allow(dead_code)]
    pub fn neighbors(&self, section_id: u32) -> &[u32] {
        &self.sections[section_id as usize].adjacencies
    }

    /// `(neighbor_id, building)` for every built building in any neighbor
    /// of `section_id`.
    pub fn neighbor_buildings(
        &self,
        section_id: u32,
    ) -> impl Iterator<Item = (u32, MapBuilding)> + '_ {
        self.neighbors(section_id).iter().flat_map(move |&nid| {
            self.sections[nid as usize].slots.iter()
                .filter_map(move |slot| slot.map(|b| (nid, b)))
        })
    }
}

/// BFS distance from the starting section, then `+1` and clamped to 5,
/// produces a 1..=5 star rating per section.
fn compute_stars(sections: &[MapSection], start: usize) -> Vec<u8> {
    let n = sections.len();
    let mut dist = vec![u8::MAX; n];
    if start >= n { return vec![1; n]; }
    dist[start] = 0;
    let mut q: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    q.push_back(start);
    while let Some(i) = q.pop_front() {
        let d = dist[i];
        for &nbr in &sections[i].adjacencies {
            let nbr = nbr as usize;
            if dist[nbr] == u8::MAX {
                dist[nbr] = d.saturating_add(1);
                q.push_back(nbr);
            }
        }
    }
    dist.iter().map(|&d| d.saturating_add(1).min(5)).collect()
}

// ---------- Animation timeline ----------

#[derive(Resource, Default)]
pub struct MapAnimTimeline {
    pub elapsed: f32,
    pub steps: std::collections::VecDeque<TimelineStep>,
}

pub struct TimelineStep {
    pub at: f32,
    pub action: TimelineAction,
}

pub enum TimelineAction {
    Pulse { pos: Vec2, color: Color, duration: f32 },
    Beam { from: Vec2, to: Vec2, color: Color, duration: f32 },
}

// ---------- Debug overlay state ----------

#[derive(Resource, Default)]
pub struct DebugClaimMode {
    pub active: bool,
}

#[derive(Event)]
pub struct TriggerMapPhase;

// ---------- Marker components ----------

#[derive(Component)]
pub struct MapBoat;

/// Marker on the single sprite that displays the pre-rasterized section
/// fill image. We render the entire map fill as one sprite to avoid
/// hairline seams between fan-triangle edges.
#[derive(Component)]
pub struct MapFillSprite;

#[derive(Component)]
pub struct MapSectionBoundary;

/// Grey square at a section's center where a building can be placed.
#[derive(Component)]
#[allow(dead_code)]
pub struct MapSlotBox {
    pub section_id: u32,
    pub slot_index: usize,
}

#[derive(Component)]
pub struct MapSlotStar;

#[derive(Component)]
pub struct MapSlotLabel {
    pub section_id: u32,
    pub slot_index: usize,
}

/// Root entity of a building-choice popup.
#[derive(Component)]
pub struct BuildingPopup;

#[derive(Component)]
pub struct BuildingChoiceButton {
    pub section_id: u32,
    pub slot_index: usize,
    pub building: MapBuilding,
}

/// Description text element at the bottom of a building popup.
#[derive(Component)]
pub struct BuildingPopupDescription;

#[derive(Component)]
#[allow(dead_code)]
pub struct BuildingCostLabel {
    pub cost: u32,
}

/// Hover tooltip that appears next to the cursor over a placed
/// building's slot.
#[derive(Component)]
pub struct BuildingTooltip {
    pub building: MapBuilding,
}

// ---------- Currency UI markers ----------

#[derive(Component)]
pub struct CurrencyUi;

#[derive(Component)]
pub struct ScrapText;

#[derive(Component)]
pub struct SteelText;

#[derive(Component)]
pub struct RefinedSteelText;

// ---------- Level status markers ----------

#[derive(Component)]
pub struct LevelStatusUi;

#[derive(Component)]
pub struct LevelStatusText;

#[derive(Component)]
pub struct LevelEnemyBar;

// ---------- Converter progress bar markers ----------

#[derive(Component)]
pub struct BuildingProgressBg {
    #[allow(dead_code)]
    pub section_id: u32,
    #[allow(dead_code)]
    pub slot_index: usize,
}

#[derive(Component)]
pub struct BuildingProgressBar {
    pub section_id: u32,
    pub slot_index: usize,
    pub interval: f32,
    pub left_x: f32,
    pub y: f32,
    pub max_w: f32,
    pub z: f32,
}

#[derive(Resource)]
pub struct ProgressBarAssets {
    pub bg_mesh: Handle<Mesh>,
    pub fill_mesh: Handle<Mesh>,
    pub bg_material: Handle<ColorMaterial>,
    pub fill_material: Handle<ColorMaterial>,
}

// ---------- Debug panel markers ----------

#[derive(Component)]
pub struct DebugPanel;

#[derive(Component, Clone, Copy, PartialEq, Eq)]
pub enum DebugButton {
    ClaimMode,
    Phase,
    SpawnAlly(crate::ally::ShipClass),
    SpawnBoss(crate::ally::ShipClass),
    OpenCustomize,
}

#[derive(Component)]
pub struct DebugClaimLabel;

// ---------- Animation primitive markers ----------

#[derive(Component)]
pub struct AnimPulse {
    pub timer: Timer,
    pub peak_alpha: f32,
}

#[derive(Component)]
pub struct AnimBeam {
    pub timer: Timer,
    pub peak_alpha: f32,
}

// ---------- Cross-cutting helpers ----------

/// `run_if` predicate for systems that should only tick during combat.
/// Pauses combat-side systems while the player is on the map, has the
/// customize overlay open, or has the ESC pause menu up.
pub fn in_combat_view(
    view: Res<ViewMode>,
    customize: Res<crate::customize::CustomizeOpen>,
    paused: Res<crate::pause::Paused>,
) -> bool {
    *view == ViewMode::Combat && !customize.open && !paused.0
}

/// Standard ray-casting point-in-polygon. Works for the wobbled (but
/// still non-self-intersecting) polygons we hand-author.
pub(crate) fn point_in_polygon(p: Vec2, poly: &[Vec2]) -> bool {
    let n = poly.len();
    if n < 3 { return false; }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let pi = poly[i];
        let pj = poly[j];
        let crosses = (pi.y > p.y) != (pj.y > p.y);
        if crosses {
            let x_at = (pj.x - pi.x) * (p.y - pi.y) / (pj.y - pi.y) + pi.x;
            if p.x < x_at { inside = !inside; }
        }
        j = i;
    }
    inside
}
