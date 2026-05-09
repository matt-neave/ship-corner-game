//! Building economy + popup UI + per-cycle progress bars.
//!
//! Covers everything that happens after the player clicks an owned slot
//! tile:
//! - Open the build picker popup (main panel + description sidecar).
//! - Resolve a click on an option → deduct scrap, write the building.
//! - Spawn a Foundry/Refinery progress bar above the slot.
//! - Tick converters every frame (Foundry, Crane boost, Refinery).
//! - Hover tooltip on placed buildings.
//! - Combat-side level resolution (`level_complete_check`,
//!   `level_fail_check`) lives here too — the "section economy" slice
//!   covers what happens when a section fight ends.

use bevy::ecs::hierarchy::ChildSpawnerCommands;
use bevy::prelude::*;
use bevy::render::view::RenderLayers;
use bevy::window::PrimaryWindow;

use crate::balance::{
    CRANE_INTERVAL, CRANE_SPEED_MULT, FOUNDRY_INTERVAL, PLAY_WORLD,
    REFINERY_INPUT, REFINERY_INTERVAL,
};
use crate::enemy::Enemy;
use crate::i18n::tr;
use crate::modes::{effective_ui_width, play_area_screen_rect, WindowMode};
use crate::ui_kit::{self, theme};
use crate::{RefinedSteel, Scrap, Steel};

use super::{
    AnimBeam, AnimPulse, BuildingChoiceButton, BuildingCostLabel, BuildingPopup,
    BuildingPopupDescription, BuildingProgressBar, BuildingProgressBg, BuildingTickState,
    BuildingTimers, BuildingTooltip, CombatContext, MapAnimTimeline, MapBoat, MapBuilding,
    MapState, ProgressBarAssets, ViewMode, MAP_LAYER, SLOT_HALF, Z_OUTLINE,
};

// ---------- Converter progress bars ----------

const PROGRESS_BAR_W: f32 = 8.0;
const PROGRESS_BAR_H: f32 = 1.4;
const PROGRESS_BAR_Y_OFFSET: f32 = 6.5;

pub fn setup_progress_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
) {
    let bg_mesh   = meshes.add(Rectangle::new(PROGRESS_BAR_W, PROGRESS_BAR_H));
    let fill_mesh = meshes.add(Rectangle::new(PROGRESS_BAR_W, PROGRESS_BAR_H));
    let bg_material   = materials.add(Color::srgb(0.10, 0.12, 0.18));
    let fill_material = materials.add(Color::WHITE);
    commands.insert_resource(ProgressBarAssets {
        bg_mesh,
        fill_mesh,
        bg_material,
        fill_material,
    });
}

/// Spawn a progress bar over a slot. No-op for non-converter buildings.
fn spawn_building_progress_bar(
    commands: &mut Commands,
    assets: &ProgressBarAssets,
    section: &super::MapSection,
    slot_index: usize,
    building: MapBuilding,
) {
    let interval = match building {
        MapBuilding::Foundry  => FOUNDRY_INTERVAL,
        MapBuilding::Refinery => REFINERY_INTERVAL,
        _ => return,
    };
    let fill_mat = assets.fill_material.clone();

    let center_x = section.center.x;
    let bar_y = section.center.y + PROGRESS_BAR_Y_OFFSET;
    let bg_z = Z_OUTLINE + 0.05;
    let fill_z = bg_z + 0.01;
    let bar_left = center_x - PROGRESS_BAR_W / 2.0;

    commands.spawn((
        Mesh2d(assets.bg_mesh.clone()),
        MeshMaterial2d(assets.bg_material.clone()),
        Transform::from_xyz(center_x, bar_y, bg_z),
        RenderLayers::layer(MAP_LAYER),
        BuildingProgressBg { section_id: section.id, slot_index },
    ));

    commands.spawn((
        Mesh2d(assets.fill_mesh.clone()),
        MeshMaterial2d(fill_mat),
        Transform::from_xyz(bar_left, bar_y, fill_z)
            .with_scale(Vec3::new(0.0, 1.0, 1.0)),
        RenderLayers::layer(MAP_LAYER),
        BuildingProgressBar {
            section_id: section.id,
            slot_index,
            interval,
            left_x: bar_left,
            y: bar_y,
            max_w: PROGRESS_BAR_W,
            z: fill_z,
        },
    ));
}

pub fn update_building_progress_bars(
    timers: Res<BuildingTimers>,
    state: Res<MapState>,
    mut q: Query<(&BuildingProgressBar, &mut Transform)>,
) {
    for (bar, mut tf) in &mut q {
        let still_converter = state.sections
            .get(bar.section_id as usize)
            .and_then(|s| s.slots.get(bar.slot_index))
            .and_then(|s| *s)
            .is_some_and(|b| matches!(b, MapBuilding::Foundry | MapBuilding::Refinery));

        let progress = if !still_converter {
            0.0
        } else {
            let key = (bar.section_id, bar.slot_index);
            let cd = timers.state.get(&key).map(|s| s.cooldown).unwrap_or(bar.interval);
            (1.0 - cd / bar.interval).clamp(0.0, 1.0)
        };

        tf.translation.x = bar.left_x + bar.max_w * 0.5 * progress;
        tf.translation.y = bar.y;
        tf.translation.z = bar.z;
        tf.scale.x = progress;
    }
}

// ---------- Hover tooltip ----------

pub fn update_building_hover_tooltip(
    mut commands: Commands,
    view: Res<ViewMode>,
    state: Res<MapState>,
    windows: Query<&Window, With<PrimaryWindow>>,
    window_mode: Res<WindowMode>,
    popups: Query<&BuildingPopup>,
    existing: Query<(Entity, &BuildingTooltip, &mut Node)>,
) {
    let desired: Option<(MapBuilding, f32, f32)> = (|| {
        if !matches!(*view, ViewMode::Map) { return None; }
        if !popups.is_empty() { return None; }
        let win = windows.single().ok()?;
        let c = win.cursor_position()?;
        let (left, top, size) = play_area_screen_rect(
            win.width(), win.height(), effective_ui_width(&window_mode),
        );
        if c.x < left || c.x > left + size || c.y < top || c.y > top + size {
            return None;
        }
        let nx = (c.x - left) / size;
        let ny = (c.y - top) / size;
        let world = Vec2::new((nx - 0.5) * PLAY_WORLD, (0.5 - ny) * PLAY_WORLD);

        for section in &state.sections {
            if !state.owned[section.id as usize] { continue; }
            for slot in &section.slots {
                let slot_pos = section.center;
                if (world.x - slot_pos.x).abs() <= SLOT_HALF
                    && (world.y - slot_pos.y).abs() <= SLOT_HALF
                {
                    if let Some(building) = *slot {
                        return Some((building, c.x, c.y));
                    }
                    return None;
                }
            }
        }
        None
    })();

    let mut existing = existing;
    match (desired, existing.iter_mut().next()) {
        (None, Some((e, _, _))) => {
            commands.entity(e).despawn();
        }
        (Some((building, cx, cy)), Some((e, tip, mut node))) => {
            if tip.building == building {
                node.left = Val::Px(cx + 12.0);
                node.top  = Val::Px(cy + 12.0);
            } else {
                commands.entity(e).despawn();
                spawn_building_tooltip(&mut commands, building, cx, cy);
            }
        }
        (Some((building, cx, cy)), None) => {
            spawn_building_tooltip(&mut commands, building, cx, cy);
        }
        (None, None) => {}
    }
}

fn spawn_building_tooltip(
    commands: &mut Commands,
    building: MapBuilding,
    cx: f32,
    cy: f32,
) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(cx + 12.0),
                top:  Val::Px(cy + 12.0),
                padding: UiRect::all(Val::Px(theme::PAD_MD)),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Stretch,
                max_width: Val::Px(200.0),
                row_gap: Val::Px(theme::GAP_SM),
                ..default()
            },
            BackgroundColor(theme::SURFACE_RAISED),
            ZIndex(95),
            BuildingTooltip { building },
        ))
        .with_children(|p| {
            p.spawn(ui_kit::label(building.label(), theme::FONT_MD, theme::ON_SURFACE));
            p.spawn(ui_kit::label(building.description(), theme::FONT_SM, theme::ON_SURFACE_DIM));
        });
}

// ---------- Build picker popup ----------

pub fn spawn_building_popup(
    commands: &mut Commands,
    cursor_screen: Vec2,
    window_w: f32,
    section_id: u32,
    slot_index: usize,
    options: &[MapBuilding],
) {
    let on_right_half = cursor_screen.x > window_w * 0.5;

    let root = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: if on_right_half {
                    Val::Auto
                } else {
                    Val::Px(cursor_screen.x + 6.0)
                },
                right: if on_right_half {
                    Val::Px(window_w - cursor_screen.x + 6.0)
                } else {
                    Val::Auto
                },
                top: Val::Px(cursor_screen.y + 6.0),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::FlexStart,
                column_gap: Val::Px(theme::GAP_MD),
                ..default()
            },
            ZIndex(100),
            BuildingPopup,
        ))
        .id();

    commands.entity(root).with_children(|p| {
        if on_right_half {
            spawn_popup_description_panel(p);
            spawn_popup_main_panel(p, section_id, slot_index, options);
        } else {
            spawn_popup_main_panel(p, section_id, slot_index, options);
            spawn_popup_description_panel(p);
        }
    });
}

fn spawn_popup_main_panel(
    parent: &mut ChildSpawnerCommands,
    section_id: u32,
    slot_index: usize,
    options: &[MapBuilding],
) {
    parent
        .spawn((
            Node {
                padding: UiRect::all(Val::Px(theme::PAD_MD)),
                border: UiRect::all(Val::Px(theme::BORDER_W)),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Stretch,
                min_width: Val::Px(140.0),
                max_width: Val::Px(260.0),
                row_gap: Val::Px(theme::GAP_SM),
                ..default()
            },
            BackgroundColor(theme::SURFACE_RAISED),
            BorderColor(theme::BORDER_SUBTLE),
            Button,
        ))
        .with_children(|p| {
            p.spawn(ui_kit::label(
                tr("map_popup_build"), theme::FONT_SM, theme::ON_SURFACE_DIM,
            ));
            for &opt in options {
                p.spawn((
                    Button,
                    Node {
                        padding: UiRect::axes(
                            Val::Px(theme::PAD_MD), Val::Px(theme::PAD_SM),
                        ),
                        align_items: AlignItems::Center,
                        justify_content: JustifyContent::SpaceBetween,
                        column_gap: Val::Px(theme::GAP_MD),
                        width: Val::Percent(100.0),
                        ..default()
                    },
                    BackgroundColor(theme::SURFACE),
                    BuildingChoiceButton { section_id, slot_index, building: opt },
                ))
                .with_children(|b| {
                    b.spawn(ui_kit::label(
                        opt.label(), theme::FONT_MD, theme::ON_SURFACE,
                    ));
                    let cost = opt.cost_scrap();
                    if cost > 0 {
                        b.spawn((
                            ui_kit::label(
                                &format!("{} ⛯", cost),
                                theme::FONT_SM,
                                theme::ACCENT,
                            ),
                            BuildingCostLabel { cost },
                        ));
                    }
                });
            }
        });
}

fn spawn_popup_description_panel(parent: &mut ChildSpawnerCommands) {
    parent
        .spawn((
            Node {
                padding: UiRect::all(Val::Px(theme::PAD_MD)),
                border: UiRect::all(Val::Px(theme::BORDER_W)),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Stretch,
                width: Val::Px(180.0),
                min_height: Val::Px(64.0),
                ..default()
            },
            BackgroundColor(theme::SURFACE_RAISED),
            BorderColor(theme::BORDER_SUBTLE),
            Button,
        ))
        .with_children(|p| {
            p.spawn((
                ui_kit::label("", theme::FONT_SM, theme::ON_SURFACE_DIM),
                BuildingPopupDescription,
            ));
        });
}

pub fn update_building_button_tints(
    mut q: Query<
        (&Interaction, &mut BackgroundColor),
        (With<BuildingChoiceButton>, Changed<Interaction>),
    >,
) {
    for (interaction, mut bg) in &mut q {
        bg.0 = match *interaction {
            Interaction::None    => theme::SURFACE,
            Interaction::Hovered => theme::SURFACE_HOVER,
            Interaction::Pressed => theme::ACCENT,
        };
    }
}

pub fn update_building_description(
    interactions: Query<
        (&Interaction, &BuildingChoiceButton),
        Changed<Interaction>,
    >,
    mut text_q: Query<&mut Text, With<BuildingPopupDescription>>,
) {
    if interactions.is_empty() { return; }
    let Ok(mut text) = text_q.single_mut() else { return; };
    for (interaction, choice) in &interactions {
        match *interaction {
            Interaction::Hovered | Interaction::Pressed => {
                let new = choice.building.description();
                if text.0 != new { text.0 = new.to_string(); }
            }
            Interaction::None => {
                if !text.0.is_empty() { text.0.clear(); }
            }
        }
    }
}

pub fn handle_building_choice_clicks(
    mut commands: Commands,
    interactions: Query<(&Interaction, &BuildingChoiceButton), Changed<Interaction>>,
    popups: Query<Entity, With<BuildingPopup>>,
    mut state: ResMut<MapState>,
    mut scrap: ResMut<Scrap>,
    progress_assets: Option<Res<ProgressBarAssets>>,
) {
    for (interaction, choice) in &interactions {
        if !matches!(*interaction, Interaction::Pressed) { continue; }
        let cost = choice.building.cost_scrap();
        if scrap.0 < cost { continue; }
        scrap.0 -= cost;
        if let Some(section) = state.sections.get_mut(choice.section_id as usize) {
            if let Some(slot) = section.slots.get_mut(choice.slot_index) {
                *slot = Some(choice.building);
            }
        }
        if matches!(
            choice.building,
            MapBuilding::Foundry | MapBuilding::Refinery
        ) {
            if let (Some(assets), Some(section)) = (
                progress_assets.as_deref(),
                state.sections.get(choice.section_id as usize),
            ) {
                spawn_building_progress_bar(
                    &mut commands, assets, section,
                    choice.slot_index, choice.building,
                );
            }
        }
        for popup in &popups { commands.entity(popup).despawn(); }
    }
}

/// Reset transient map UI on a view-mode flip:
///   - Despawn any open building popup so we don't return to a stale popup.
///   - Clear the animation timeline + despawn live pulses/beams.
pub fn close_popup_on_view_change(
    view: Res<ViewMode>,
    mut commands: Commands,
    popups: Query<Entity, With<BuildingPopup>>,
    mut timeline: ResMut<MapAnimTimeline>,
    anims: Query<Entity, Or<(With<AnimPulse>, With<AnimBeam>)>>,
) {
    if !view.is_changed() { return; }
    for popup in &popups { commands.entity(popup).despawn(); }
    timeline.steps.clear();
    timeline.elapsed = 0.0;
    for e in &anims { commands.entity(e).despawn(); }
}

// ---------- Per-frame economy tick ----------

/// Tick every Foundry / Crane / Refinery each frame. Runs in *both*
/// views so the economy keeps working while the player is in combat.
pub fn tick_buildings(
    time: Res<Time>,
    state: Res<MapState>,
    mut timers: ResMut<BuildingTimers>,
    mut scrap: ResMut<Scrap>,
    mut steel: ResMut<Steel>,
    mut refined: ResMut<RefinedSteel>,
) {
    let dt = time.delta_secs();
    if dt <= 0.0 { return; }

    // Pass 1: which sections currently have a fueled Crane?
    let mut fueled_crane_sections: std::collections::HashSet<u32> =
        std::collections::HashSet::new();
    for section in &state.sections {
        for (idx, slot) in section.slots.iter().enumerate() {
            if !matches!(slot, Some(MapBuilding::Crane)) { continue; }
            let key = (section.id, idx);
            let s = timers.state.entry(key).or_insert(BuildingTickState {
                cooldown: CRANE_INTERVAL,
                fueled: true,
            });
            if s.fueled {
                fueled_crane_sections.insert(section.id);
            }
        }
    }

    // Pass 2: tick every Foundry / Crane / Refinery.
    for section in &state.sections {
        for (idx, slot) in section.slots.iter().enumerate() {
            let Some(building) = *slot else { continue; };
            let key = (section.id, idx);

            match building {
                MapBuilding::Foundry => {
                    let boosted = section.adjacencies.iter()
                        .any(|nbr| fueled_crane_sections.contains(nbr));
                    let speed_mult = if boosted { CRANE_SPEED_MULT } else { 1.0 };

                    let s = timers.state.entry(key).or_insert(BuildingTickState {
                        cooldown: FOUNDRY_INTERVAL,
                        fueled: false,
                    });
                    s.cooldown -= dt * speed_mult;
                    if s.cooldown > 0.0 { continue; }
                    if scrap.0 >= 1 {
                        scrap.0 -= 1;
                        steel.0 = steel.0.saturating_add(1);
                        s.fueled = true;
                        s.cooldown = FOUNDRY_INTERVAL;
                    } else {
                        s.fueled = false;
                        s.cooldown = 0.0;
                    }
                }
                MapBuilding::Crane => {
                    let s = timers.state.entry(key).or_insert(BuildingTickState {
                        cooldown: CRANE_INTERVAL,
                        fueled: true,
                    });
                    s.cooldown -= dt;
                    if s.cooldown > 0.0 { continue; }
                    if steel.0 >= 1 {
                        steel.0 -= 1;
                        s.fueled = true;
                        s.cooldown = CRANE_INTERVAL;
                    } else {
                        s.fueled = false;
                        s.cooldown = 0.0;
                    }
                }
                MapBuilding::Refinery => {
                    let boosted = section.adjacencies.iter()
                        .any(|nbr| fueled_crane_sections.contains(nbr));
                    let speed_mult = if boosted { CRANE_SPEED_MULT } else { 1.0 };

                    let s = timers.state.entry(key).or_insert(BuildingTickState {
                        cooldown: REFINERY_INTERVAL,
                        fueled: false,
                    });
                    s.cooldown -= dt * speed_mult;
                    if s.cooldown > 0.0 { continue; }
                    if steel.0 >= REFINERY_INPUT {
                        steel.0 -= REFINERY_INPUT;
                        refined.0 = refined.0.saturating_add(1);
                        s.fueled = true;
                        s.cooldown = REFINERY_INTERVAL;
                    } else {
                        s.fueled = false;
                        s.cooldown = 0.0;
                    }
                }
                _ => {}
            }
        }
    }
}

// ---------- Section combat resolution ----------

/// End of a level: when the per-section enemy budget is fully drained
/// AND no enemies are alive, claim the current section, bump the
/// campaign-progress counter, and flip the view back to the map.
pub fn level_complete_check(
    mut view: ResMut<ViewMode>,
    mut state: ResMut<MapState>,
    mut campaign: ResMut<crate::CampaignProgress>,
    mode: Res<crate::modes::GameMode>,
    combat_ctx: Res<CombatContext>,
    enemies: Query<Entity, With<Enemy>>,
) {
    if !matches!(*view, ViewMode::Combat) { return; }
    if !matches!(*mode, crate::modes::GameMode::Sandbox) { return; }
    if combat_ctx.enemy_budget > 0 { return; }
    if enemies.iter().count() > 0 { return; }

    let id = state.current as usize;
    if id < state.owned.len() && !state.owned[id] {
        state.owned[id] = true;
    }
    campaign.battles_cleared = campaign.battles_cleared.saturating_add(1);
    *view = ViewMode::Map;
}

/// Failure path: when the friendly hull is destroyed during a Sandbox
/// level, wipe the arena, restore the player ship to full HP, send the
/// map boat back to the starting section, and flip the view back to the
/// map.
pub fn level_fail_check(
    mut view: ResMut<ViewMode>,
    mut state: ResMut<MapState>,
    mut combat_ctx: ResMut<CombatContext>,
    mode: Res<crate::modes::GameMode>,
    mut commands: Commands,
    mut friendly: Query<&mut crate::components::Health, With<crate::components::Friendly>>,
    arena: Query<Entity, crate::wave::ArenaDisposeFilter>,
    mut boat: Query<&mut Transform, With<MapBoat>>,
) {
    if !matches!(*view, ViewMode::Combat) { return; }
    if !matches!(*mode, crate::modes::GameMode::Sandbox) { return; }

    let Ok(mut h) = friendly.single_mut() else { return; };
    if h.0 > 0 { return; }

    for e in &arena { commands.entity(e).despawn(); }

    h.0 = 100;
    combat_ctx.enemy_budget = 0;
    state.boat_target = None;
    state.current = 0;
    if let Ok(mut tf) = boat.single_mut() {
        let s0 = state.sections.first().map(|s| s.center).unwrap_or(Vec2::ZERO);
        tf.translation.x = s0.x;
        tf.translation.y = s0.y;
    }
    *view = ViewMode::Map;
}
