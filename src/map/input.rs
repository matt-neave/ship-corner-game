//! Map-view click input + boat steering.
//!
//! Click handling has five modes (priority order):
//! 1. UI button absorbed it — bail.
//! 2. Debug claim mode — point-in-polygon flips the section's `owned`.
//! 3. Popup is open + click outside — dismiss popup.
//! 4. Click on an owned slot tile — open the build picker.
//! 5. Otherwise — set a sail target.
//!
//! `map_boat_movement` steers toward the target; crossing into an
//! unowned section flips the view to combat (with budget snapshot).

use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::balance::{FRIENDLY_SPEED, FRIENDLY_TURN_RATE, PLAY_WORLD};
use crate::components::Heading;
use crate::modes::{effective_ui_width, play_area_screen_rect, WindowMode};
use crate::ship::approach_angle;

use super::buildings::spawn_building_popup;
use super::{
    point_in_polygon, BuildingPopup, CombatContext, DebugClaimMode, MapBoat, MapBuilding,
    MapState, ViewMode, SLOT_HALF,
};

pub fn map_click_input(
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    window_mode: Res<WindowMode>,
    view: Res<ViewMode>,
    claim_mode: Res<DebugClaimMode>,
    mut state: ResMut<MapState>,
    mut commands: Commands,
    interactions: Query<&Interaction, With<Button>>,
    popups: Query<Entity, With<BuildingPopup>>,
) {
    if *view != ViewMode::Map { return; }
    if !mouse.just_pressed(MouseButton::Left) { return; }

    if interactions.iter().any(|i| matches!(i, Interaction::Pressed)) {
        return;
    }

    let Ok(win) = windows.single() else { return; };
    let Some(c) = win.cursor_position() else { return; };

    let (left, top, size) =
        play_area_screen_rect(win.width(), win.height(), effective_ui_width(&window_mode));
    if c.x < left || c.x > left + size || c.y < top || c.y > top + size { return; }
    let nx = (c.x - left) / size;
    let ny = (c.y - top) / size;
    let world = Vec2::new((nx - 0.5) * PLAY_WORLD, (0.5 - ny) * PLAY_WORLD);

    if claim_mode.active {
        for i in 0..state.sections.len() {
            if point_in_polygon(world, &state.sections[i].polygon) {
                if !state.owned[i] { state.owned[i] = true; }
                break;
            }
        }
        return;
    }

    if let Ok(popup) = popups.single() {
        commands.entity(popup).despawn();
        return;
    }

    for i in 0..state.sections.len() {
        if !state.owned[i] { continue; }
        let section = &state.sections[i];
        for slot_index in 0..section.slots.len() {
            let slot_pos = section.center;
            if (world.x - slot_pos.x).abs() <= SLOT_HALF
                && (world.y - slot_pos.y).abs() <= SLOT_HALF
            {
                if section.slots[slot_index].is_some() { return; }
                let options = MapBuilding::options_for_stars(section.stars);
                if options.is_empty() { return; }
                spawn_building_popup(
                    &mut commands, c, win.width(),
                    section.id, slot_index, &options,
                );
                return;
            }
        }
    }

    state.boat_target = Some(world);
}

/// Steer the boat toward `state.boat_target` using the same turn-then-
/// advance pattern as the in-combat ship. Click sets the target; the
/// boat sails there *only* — it doesn't continuously chase the cursor.
pub fn map_boat_movement(
    time: Res<Time>,
    mut state: ResMut<MapState>,
    mut view: ResMut<ViewMode>,
    mut combat_ctx: ResMut<CombatContext>,
    campaign: Res<crate::CampaignProgress>,
    mut q: Query<(&mut Transform, &mut Heading), With<MapBoat>>,
) {
    if *view != ViewMode::Map { return; }
    let Ok((mut tf, mut heading)) = q.single_mut() else { return; };
    let dt = time.delta_secs();

    if let Some(tgt) = state.boat_target {
        let pos = tf.translation.truncate();
        let to = tgt - pos;
        if to.length() < 1.0 {
            state.boat_target = None;
        } else {
            let desired = (-to.x).atan2(to.y);
            let new_h = approach_angle(heading.0, desired, FRIENDLY_TURN_RATE * dt);
            heading.0 = new_h;
            let dir = Vec2::new(-new_h.sin(), new_h.cos());
            let new_pos = pos + dir * FRIENDLY_SPEED * dt;
            let half = PLAY_WORLD / 2.0;
            tf.translation.x = new_pos.x.clamp(-half, half);
            tf.translation.y = new_pos.y.clamp(-half, half);
            tf.rotation = Quat::from_rotation_z(new_h);
        }
    }

    // Section transition — crossing into an unowned section flips to combat.
    let now_pos = tf.translation.truncate();
    let containing = state
        .sections
        .iter()
        .find(|s| point_in_polygon(now_pos, &s.polygon))
        .map(|s| s.id);
    if let Some(id) = containing {
        if id != state.current {
            state.current = id;
            if !state.owned[id as usize] {
                state.boat_target = None;
                let stars = state.sections[id as usize].stars;
                let budget = crate::balance::level_enemy_budget(
                    stars,
                    campaign.battles_cleared,
                );
                combat_ctx.stars        = stars;
                combat_ctx.enemy_budget = budget;
                combat_ctx.enemy_total  = budget;
                *view = ViewMode::Combat;
            }
        }
    }
}
