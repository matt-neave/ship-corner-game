//! Port-side upgrade column for Wave mode. 8 vertical cells along the LHS
//! coast; between waves the player drafts 1-of-3 building cards and places
//! one in an empty cell. Adjacency drives synergies.
//!
//! Adding a new building type:
//! 1. Add a variant to `BuildingType`.
//! 2. Add rows in `label`, `description`, `hex`, `pool`.
//! 3. Add a glyph in `rebuild_pier_buildings`.
//! 4. Add an effect — a new helper here (used by `wave.rs` / `turret.rs`),
//!    or a new field on `Pier` if state needs to persist.
//!
//! The grid lines themselves are spawned in `setup_world` (main.rs) and
//! tagged `PierVisual`; this file owns the resource state, the drafting UI,
//! the per-frame rebuild of the in-cell text labels, and the buff math.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;
use bevy::window::PrimaryWindow;
use rand::Rng;

use crate::balance::{
    PIER_CELL_W, PIER_CELL_X, PIER_Y_START, PIER_Y_STEP, PLAY_LAYER, PLAY_WORLD,
};
use crate::i18n::tr;
use crate::modes::{
    effective_ui_width, play_area_screen_rect, GameMode, WindowMode,
};
use crate::palette::{
    hex, BUILDING_DRYDOCK_HEX, BUILDING_MUNITIONS_HEX, BUILDING_WATCHTOWER_HEX,
    UI_TEXT, UI_TEXT_DIM, UI_VALUE,
};
use crate::wave::{WavePhase, WaveState};

// ---------- Building catalogue ----------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BuildingType {
    /// +1 damage to every turret within ±1 cells.
    MunitionsDepot,
    /// +25% range to every turret within ±1 cells.
    Watchtower,
    /// Heals the ship +5 HP after the wave clears, plus +5 per adjacent Drydock.
    Drydock,
}

impl BuildingType {
    pub fn label(self) -> &'static str {
        match self {
            BuildingType::MunitionsDepot => tr("building_munitions"),
            BuildingType::Watchtower     => tr("building_watchtower"),
            BuildingType::Drydock        => tr("building_drydock"),
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            BuildingType::MunitionsDepot => tr("building_munitions_desc"),
            BuildingType::Watchtower     => tr("building_watchtower_desc"),
            BuildingType::Drydock        => tr("building_drydock_desc"),
        }
    }
    pub fn hex(self) -> &'static str {
        match self {
            BuildingType::MunitionsDepot => BUILDING_MUNITIONS_HEX,
            BuildingType::Watchtower     => BUILDING_WATCHTOWER_HEX,
            BuildingType::Drydock        => BUILDING_DRYDOCK_HEX,
        }
    }
    /// One-character glyph rendered in the pier cell when this building is
    /// placed. Uses Text2d in the play world so it picks up the pixel-perfect
    /// upscale.
    pub fn glyph(self) -> &'static str {
        match self {
            BuildingType::MunitionsDepot => "M",
            BuildingType::Watchtower     => "W",
            BuildingType::Drydock        => "D",
        }
    }
    /// Pool used by `generate_draft` — duplicates allowed for variety bias.
    pub fn pool() -> [BuildingType; 3] {
        [
            BuildingType::MunitionsDepot,
            BuildingType::Watchtower,
            BuildingType::Drydock,
        ]
    }
}

// ---------- Resources ----------

#[derive(Resource, Default)]
pub struct Pier {
    pub cells: [Option<BuildingType>; 8],
}

#[derive(Resource, Default)]
pub struct WaveDraft {
    /// Three options offered to the player. None = no active draft.
    pub options: Option<[BuildingType; 3]>,
    /// Card index (0..3) the player has clicked, ready to place.
    pub selected: Option<usize>,
}

// ---------- Markers ----------

/// Tag for any pier-related visual (grid lines + placed-building text).
/// Toggled visible only in Wave mode.
#[derive(Component)]
pub struct PierVisual;

/// Marker for the on-pier building text sprite. The whole set is rebuilt
/// from `Pier` whenever it changes.
#[derive(Component)]
pub struct PierBuildingMarker;

#[derive(Component)]
pub struct DraftPanel;
#[derive(Component)]
pub struct DraftCardButton { pub index: u8 }
#[derive(Component)]
pub struct DraftCardTitle { pub index: u8 }
#[derive(Component)]
pub struct DraftCardDesc { pub index: u8 }

// ---------- Layout helpers ----------

/// Pier cell center in world coords. Single source of truth for cell geometry.
pub fn pier_cell_world(index: u8) -> Vec2 {
    Vec2::new(PIER_CELL_X, PIER_Y_START + index as f32 * PIER_Y_STEP)
}

/// Cursor-position → cell index, or None if not over the pier column.
pub fn pier_cell_at(world: Vec2) -> Option<u8> {
    if (world.x - PIER_CELL_X).abs() > PIER_CELL_W / 2.0 { return None; }
    let rel = (world.y - PIER_Y_START) / PIER_Y_STEP + 0.5;
    if rel < 0.0 || rel >= 8.0 { return None; }
    Some(rel as u8)
}

// ---------- Adjacency math ----------

/// Sum the +1 damage bonuses contributed by Munitions Depots within ±1 cells
/// of `idx`. Includes the Depot in cell `idx` itself.
pub fn pier_damage_bonus(pier: &Pier, idx: usize) -> i32 {
    let mut bonus = 0;
    for off in -1..=1i32 {
        let n = idx as i32 + off;
        if !(0..8).contains(&n) { continue; }
        if matches!(pier.cells[n as usize], Some(BuildingType::MunitionsDepot)) {
            bonus += 1;
        }
    }
    bonus
}

pub fn pier_range_mult(pier: &Pier, idx: usize) -> f32 {
    let mut mult = 1.0;
    for off in -1..=1i32 {
        let n = idx as i32 + off;
        if !(0..8).contains(&n) { continue; }
        if matches!(pier.cells[n as usize], Some(BuildingType::Watchtower)) {
            mult += 0.25;
        }
    }
    mult
}

/// Total HP healed by the current Drydock layout per wave clear. Each Drydock
/// gives 5; each pair of adjacent Drydocks gives an extra 5 to BOTH cells.
pub fn pier_drydock_heal(pier: &Pier) -> i32 {
    let mut total = 0;
    for i in 0..8 {
        if !matches!(pier.cells[i], Some(BuildingType::Drydock)) { continue; }
        total += 5;
        for off in [-1i32, 1] {
            let n = i as i32 + off;
            if !(0..8).contains(&n) { continue; }
            if matches!(pier.cells[n as usize], Some(BuildingType::Drydock)) {
                total += 5;
            }
        }
    }
    total
}

/// Roll three random building options for the next draft. Uniform over the
/// pool — duplicates allowed so a useful card can show up twice.
pub fn generate_draft(rng: &mut rand::rngs::ThreadRng) -> [BuildingType; 3] {
    let pool = BuildingType::pool();
    [
        pool[rng.gen_range(0..pool.len())],
        pool[rng.gen_range(0..pool.len())],
        pool[rng.gen_range(0..pool.len())],
    ]
}

// ---------- Draft-card UI helper ----------

/// Minimal draft card: thin 1px border, two stacked text labels. Background
/// stays transparent so the card reads as text-on-screen — matches the
/// "minimal graphics" direction. Spawned by `setup_ui` (main.rs).
pub fn spawn_draft_card(parent: &mut ChildSpawnerCommands, index: u8) {
    parent.spawn((
        Button,
        Node {
            width: Val::Px(160.0),
            height: Val::Px(64.0),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            border: UiRect::all(Val::Px(1.0)),
            padding: UiRect::axes(Val::Px(8.0), Val::Px(6.0)),
            row_gap: Val::Px(4.0),
            ..default()
        },
        BackgroundColor(Color::NONE),
        BorderColor(UI_TEXT_DIM),
        DraftCardButton { index },
    ))
    .with_children(|c| {
        c.spawn((
            Text::new("---"),
            TextFont { font_size: 13.0, ..default() },
            TextColor(UI_TEXT),
            DraftCardTitle { index },
        ));
        c.spawn((
            Text::new(""),
            TextFont { font_size: 10.0, ..default() },
            TextColor(UI_TEXT_DIM),
            DraftCardDesc { index },
        ));
    });
}

// ---------- Visual sync ----------

/// Despawn any existing PierBuildingMarker entities and respawn one Text2d
/// per occupied cell, reflecting the current Pier resource. Called whenever
/// the Pier changes so the on-pier labels stay in sync.
fn rebuild_pier_buildings(
    commands: &mut Commands,
    pier: &Pier,
    existing: &Query<Entity, With<PierBuildingMarker>>,
    visible: bool,
) {
    for e in existing.iter() { commands.entity(e).despawn(); }
    for (i, cell) in pier.cells.iter().enumerate() {
        let Some(b) = cell else { continue; };
        let pos = pier_cell_world(i as u8);
        commands.spawn((
            Text2d::new(b.glyph().to_string()),
            TextFont { font_size: 11.0, ..default() },
            TextColor(hex(b.hex())),
            Transform::from_xyz(pos.x, pos.y, 0.6),
            if visible { Visibility::Inherited } else { Visibility::Hidden },
            PierVisual,
            PierBuildingMarker,
            RenderLayers::layer(PLAY_LAYER),
        ));
    }
}

/// Rebuild the on-pier text labels whenever `Pier` or `GameMode` changes.
/// Cheap (≤8 entities) so we don't bother with diffing.
pub fn sync_pier_visuals(
    mut commands: Commands,
    pier: Res<Pier>,
    mode: Res<GameMode>,
    existing: Query<Entity, With<PierBuildingMarker>>,
) {
    if !pier.is_changed() && !mode.is_changed() { return; }
    let visible = matches!(*mode, GameMode::Wave);
    rebuild_pier_buildings(&mut commands, &pier, &existing, visible);
}

// ---------- Drafting input + UI ----------

/// Handle clicks during the Drafting phase: card click selects an option,
/// then a click on an empty pier cell places it. Clearing `draft.options`
/// signals the wave orchestrator that the draft is resolved.
pub fn draft_input(
    state: Res<WaveState>,
    mut pier: ResMut<Pier>,
    mut draft: ResMut<WaveDraft>,
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    window_mode: Res<WindowMode>,
    card_interactions: Query<(&Interaction, &DraftCardButton), Changed<Interaction>>,
) {
    if state.phase != WavePhase::Drafting { return; }
    let Some(opts) = draft.options else { return; };

    // Card click → select that option.
    for (interaction, btn) in &card_interactions {
        if matches!(*interaction, Interaction::Pressed) {
            draft.selected = Some(btn.index as usize);
        }
    }

    // Placement click on a pier cell. We only fire on the press edge so the
    // same click that selected the card doesn't immediately place.
    if !mouse.just_pressed(MouseButton::Left) { return; }
    let Some(card_idx) = draft.selected else { return; };
    let Ok(win) = windows.single() else { return; };
    let Some(cursor) = win.cursor_position() else { return; };

    let (left, top, size) =
        play_area_screen_rect(win.width(), win.height(), effective_ui_width(&window_mode));
    if cursor.x < left || cursor.x > left + size || cursor.y < top || cursor.y > top + size {
        return;
    }
    let nx = (cursor.x - left) / size;
    let ny = (cursor.y - top) / size;
    let world = Vec2::new((nx - 0.5) * PLAY_WORLD, (0.5 - ny) * PLAY_WORLD);

    let Some(cell) = pier_cell_at(world) else { return; };
    if pier.cells[cell as usize].is_some() { return; }   // occupied
    pier.cells[cell as usize] = Some(opts[card_idx]);
    draft.options = None;
    draft.selected = None;
}

/// Show / hide the draft panel + populate card text from `WaveDraft.options`.
/// Selected card gets a yellow border so the player can see what they picked
/// before clicking a cell.
pub fn update_draft_ui(
    state: Res<WaveState>,
    draft: Res<WaveDraft>,
    mut panel_q: Query<&mut Visibility, With<DraftPanel>>,
    mut card_q: Query<(&DraftCardButton, &mut BorderColor)>,
    mut title_q: Query<(&DraftCardTitle, &mut Text), Without<DraftCardDesc>>,
    mut desc_q: Query<(&DraftCardDesc, &mut Text), Without<DraftCardTitle>>,
) {
    let visible = state.phase == WavePhase::Drafting && draft.options.is_some();
    let target = if visible { Visibility::Inherited } else { Visibility::Hidden };
    for mut v in &mut panel_q {
        if *v != target { *v = target; }
    }
    if !visible { return; }
    let opts = draft.options.unwrap();
    for (btn, mut border) in &mut card_q {
        let is_selected = draft.selected == Some(btn.index as usize);
        border.0 = if is_selected { UI_VALUE } else { UI_TEXT_DIM };
    }
    for (title, mut text) in &mut title_q {
        let b = opts[title.index as usize];
        **text = b.label().to_string();
    }
    for (desc, mut text) in &mut desc_q {
        let b = opts[desc.index as usize];
        **text = b.description().to_string();
    }
}
