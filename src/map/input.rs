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
use bevy::render::view::RenderLayers;
use bevy::window::PrimaryWindow;

use crate::balance::PLAY_WORLD;
use crate::components::Heading;
use crate::effects::{spawn_particles_on_layer, EffectMeshes};
use crate::modes::play_area_screen_rect;
use crate::palette::PaletteMaterials;
use crate::ship::approach_angle;

use super::buildings::spawn_building_popup;
use super::{
    point_in_polygon, BuildingPopup, CombatContext, DebugClaimMode, MapBoat, MapBuilding,
    MapState, ViewMode, MAP_LAYER, SLOT_HALF,
};

pub fn map_click_input(
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    view: Res<ViewMode>,
    claim_mode: Res<DebugClaimMode>,
    mut state: ResMut<MapState>,
    mut commands: Commands,
    interactions: Query<&Interaction, With<Button>>,
    popups: Query<Entity, With<BuildingPopup>>,
    em: Option<Res<EffectMeshes>>,
    pm: Option<Res<PaletteMaterials>>,
) {
    if *view != ViewMode::Map { return; }
    if !mouse.just_pressed(MouseButton::Left) { return; }

    if interactions.iter().any(|i| matches!(i, Interaction::Pressed)) {
        return;
    }

    let Ok(win) = windows.single() else { return; };
    let Some(c) = win.cursor_position() else { return; };

    let (left, top, play_w, play_h) = play_area_screen_rect(win.width(), win.height());
    // Map sits in a square centered in the play screen rect — pad
    // horizontally in wide_play mode so cursor math stays correct.
    let map_size = play_w.min(play_h);
    let map_left = left + (play_w - map_size) * 0.5;
    let map_top  = top  + (play_h - map_size) * 0.5;
    if c.x < map_left || c.x > map_left + map_size || c.y < map_top || c.y > map_top + map_size { return; }
    let nx = (c.x - map_left) / map_size;
    let ny = (c.y - map_top) / map_size;
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

    // Cosmetic splash burst at the click position. Spawned on `MAP_LAYER`
    // so the map camera sees it (the combat play camera is on a different
    // layer). Reuses `HitParticle` + `update_hit_particles` — no extra
    // tick system. Skipped if asset caches aren't ready yet.
    if let (Some(em), Some(pm)) = (em, pm) {
        let mut rng = rand::thread_rng();
        let count = rand::Rng::gen_range(&mut rng, 6..=10);
        spawn_particles_on_layer(
            &mut commands,
            &em,
            &pm.splash,
            world,
            count,
            60.0,
            RenderLayers::layer(MAP_LAYER),
            &mut rng,
        );
    }
}

/// Steer the boat toward `state.boat_target` using the same turn-then-
/// advance pattern as the in-combat ship. Click sets the target; the
/// boat sails there *only* — it doesn't continuously chase the cursor.
pub fn map_boat_movement(
    time: Res<Time>,
    mut state: ResMut<MapState>,
    view: Res<ViewMode>,
    mut combat_ctx: ResMut<CombatContext>,
    campaign: Res<crate::CampaignProgress>,
    stats: Res<crate::stats::PlayerStats>,
    mut next_state: ResMut<NextState<crate::AppState>>,
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
            // Map navigation uses the unmodded base speeds so high-end
            // builds (or hobbled ones) don't change how long it takes to
            // cross a section — keeps overworld pacing consistent.
            let new_h = approach_angle(heading.0, desired, stats.turn_speed.base * dt);
            heading.0 = new_h;
            let dir = Vec2::new(-new_h.sin(), new_h.cos());
            let new_pos = pos + dir * stats.move_speed.base * dt;
            // Map's bounds are the authored 200×200 square regardless
            // of the combat play-area shape — keep clamp on PLAY_WORLD
            // (which aliases PLAY_WORLD_H = 200).
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
                combat_ctx.reset_for(stars, campaign.battles_cleared);
                // Carry the section's pre-rolled boss class into the
                // combat. `spawn_enemies` consumes it on the first
                // frame of the final wave; `None` here means a normal
                // section with no boss kicker.
                combat_ctx.boss_pending = state.sections[id as usize].boss_class;
                // Hand off to the state machine — `OnEnter(Playing)`
                // flips ViewMode to Combat and `OnExit(Map)` runs the
                // stage-start refill + arena cleanup. ViewMode is
                // derived from AppState now, no longer set here.
                next_state.set(crate::AppState::Playing);
            }
        }
    }
}
