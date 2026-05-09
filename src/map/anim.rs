//! Phase animation pipeline.
//!
//! `MapAnimTimeline` is a queue of `(at, action)` steps. `map_begin_phase`
//! pushes a sequence whenever the player enters map view (or when the
//! debug PHASE button fires). `advance_map_anim_timeline` drains the
//! queue based on a running `elapsed` counter and spawns the actual
//! pulse / beam visuals. `update_anim_*` animate them through their
//! lifetime and despawn at finish.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;

use crate::ui_kit;

use super::{
    AnimBeam, AnimPulse, MapAnimTimeline, MapBuilding, MapState, TimelineAction,
    TimelineStep, TriggerMapPhase, ViewMode, ANIM_BEAM_DUR, ANIM_BEAM_PEAK_ALPHA,
    ANIM_BEAM_THICKNESS, ANIM_PULSE_DUR, ANIM_PULSE_PEAK_ALPHA, ANIM_PULSE_PEAK_SCALE,
    ANIM_PULSE_SIZE, ANIM_STEP_OVERLAP, MAP_LAYER, Z_ANIM,
};

fn spawn_pulse(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ColorMaterial>,
    pos: Vec2, color: Color, duration: f32,
) {
    let mesh = meshes.add(Rectangle::new(ANIM_PULSE_SIZE, ANIM_PULSE_SIZE));
    let material = materials.add(ColorMaterial {
        color: color.with_alpha(0.0),
        alpha_mode: bevy::sprite::AlphaMode2d::Blend,
        ..default()
    });
    commands.spawn((
        Mesh2d(mesh),
        MeshMaterial2d(material),
        Transform::from_xyz(pos.x, pos.y, Z_ANIM),
        RenderLayers::layer(MAP_LAYER),
        AnimPulse {
            timer: Timer::from_seconds(duration, TimerMode::Once),
            peak_alpha: ANIM_PULSE_PEAK_ALPHA,
        },
    ));
}

fn spawn_beam(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ColorMaterial>,
    from: Vec2, to: Vec2, color: Color, duration: f32,
) {
    let dir = to - from;
    let len = dir.length();
    if len < 0.001 { return; }
    let angle = dir.y.atan2(dir.x);
    let mid = (from + to) * 0.5;
    let mesh = meshes.add(Rectangle::new(len, ANIM_BEAM_THICKNESS));
    let material = materials.add(ColorMaterial {
        color: color.with_alpha(0.0),
        alpha_mode: bevy::sprite::AlphaMode2d::Blend,
        ..default()
    });
    commands.spawn((
        Mesh2d(mesh),
        MeshMaterial2d(material),
        Transform::from_xyz(mid.x, mid.y, Z_ANIM)
            .with_rotation(Quat::from_rotation_z(angle)),
        RenderLayers::layer(MAP_LAYER),
        AnimBeam {
            timer: Timer::from_seconds(duration, TimerMode::Once),
            peak_alpha: ANIM_BEAM_PEAK_ALPHA,
        },
    ));
}

/// Walk the timeline: any step whose `at` has been reached fires.
pub fn advance_map_anim_timeline(
    time: Res<Time>,
    mut timeline: ResMut<MapAnimTimeline>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
) {
    if timeline.steps.is_empty() {
        if timeline.elapsed != 0.0 { timeline.elapsed = 0.0; }
        return;
    }
    timeline.elapsed += time.delta_secs();
    while let Some(front) = timeline.steps.front() {
        if front.at > timeline.elapsed { break; }
        let step = timeline.steps.pop_front().unwrap();
        match step.action {
            TimelineAction::Pulse { pos, color, duration } => {
                spawn_pulse(&mut commands, &mut meshes, &mut materials, pos, color, duration);
            }
            TimelineAction::Beam { from, to, color, duration } => {
                spawn_beam(&mut commands, &mut meshes, &mut materials, from, to, color, duration);
            }
        }
    }
}

pub fn update_anim_pulses(
    time: Res<Time>,
    mut commands: Commands,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut q: Query<(Entity, &mut AnimPulse, &mut Transform, &MeshMaterial2d<ColorMaterial>)>,
) {
    for (entity, mut anim, mut tf, mat_handle) in &mut q {
        anim.timer.tick(time.delta());
        let t = anim.timer.fraction();
        let bell = (std::f32::consts::PI * t).sin();
        let scale = 1.0 + (ANIM_PULSE_PEAK_SCALE - 1.0) * bell;
        tf.scale = Vec3::new(scale, scale, 1.0);
        if let Some(mat) = materials.get_mut(&mat_handle.0) {
            mat.color = mat.color.with_alpha(anim.peak_alpha * bell);
        }
        if anim.timer.finished() {
            commands.entity(entity).despawn();
        }
    }
}

pub fn update_anim_beams(
    time: Res<Time>,
    mut commands: Commands,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut q: Query<(Entity, &mut AnimBeam, &MeshMaterial2d<ColorMaterial>)>,
) {
    for (entity, mut anim, mat_handle) in &mut q {
        anim.timer.tick(time.delta());
        let t = anim.timer.fraction();
        let bell = (std::f32::consts::PI * t).sin();
        if let Some(mat) = materials.get_mut(&mat_handle.0) {
            mat.color = mat.color.with_alpha(anim.peak_alpha * bell);
        }
        if anim.timer.finished() {
            commands.entity(entity).despawn();
        }
    }
}

/// "Begin phase" — fires when the player enters map view OR when the
/// PHASE debug button writes a `TriggerMapPhase` event. Today only
/// `Dockyard` is wired: pulses the source, beams to each neighbor,
/// pulses each neighbor. Multiple Dockyards play in order along a
/// shared `t` cursor so each building reads distinctly.
pub fn map_begin_phase(
    view: Res<ViewMode>,
    state: Res<MapState>,
    mut timeline: ResMut<MapAnimTimeline>,
    mut phase_evt: EventReader<TriggerMapPhase>,
    mut commands: Commands,
    anims: Query<Entity, Or<(With<AnimPulse>, With<AnimBeam>)>>,
) {
    let view_to_map = view.is_changed() && *view == ViewMode::Map;
    let manual = !phase_evt.is_empty();
    phase_evt.clear();
    if !view_to_map && !manual { return; }
    if *view != ViewMode::Map { return; }

    timeline.steps.clear();
    timeline.elapsed = 0.0;
    for e in &anims { commands.entity(e).despawn(); }

    let color = ui_kit::theme::ACCENT;
    let mut t = 0.0_f32;

    for section in &state.sections {
        for slot in &section.slots {
            let Some(building) = *slot else { continue; };
            match building {
                MapBuilding::Weaponry
                | MapBuilding::Foundry
                | MapBuilding::Crane
                | MapBuilding::Refinery => {}
                MapBuilding::Dockyard => {
                    let pos = section.center;
                    let neighbors: Vec<(u32, MapBuilding)> =
                        state.neighbor_buildings(section.id).collect();

                    timeline.steps.push_back(TimelineStep {
                        at: t,
                        action: TimelineAction::Pulse {
                            pos, color, duration: ANIM_PULSE_DUR,
                        },
                    });

                    let beam_start = t + ANIM_PULSE_DUR * ANIM_STEP_OVERLAP;
                    let nbr_pulse_start = beam_start + ANIM_BEAM_DUR * 0.6;
                    for (nbr_id, _) in &neighbors {
                        let nbr_pos = state.sections[*nbr_id as usize].center;
                        timeline.steps.push_back(TimelineStep {
                            at: beam_start,
                            action: TimelineAction::Beam {
                                from: pos, to: nbr_pos,
                                color, duration: ANIM_BEAM_DUR,
                            },
                        });
                        timeline.steps.push_back(TimelineStep {
                            at: nbr_pulse_start,
                            action: TimelineAction::Pulse {
                                pos: nbr_pos, color, duration: ANIM_PULSE_DUR,
                            },
                        });
                    }

                    let burst_end = if neighbors.is_empty() {
                        t + ANIM_PULSE_DUR
                    } else {
                        nbr_pulse_start + ANIM_PULSE_DUR
                    };
                    t = burst_end + 0.2;

                    let names: Vec<&str> = neighbors.iter()
                        .map(|(_, b)| b.label())
                        .collect();
                    if names.is_empty() {
                        info!("Dockyard@S{}: no adjacent buildings", section.id);
                    } else {
                        info!(
                            "Dockyard@S{}: adjacent buildings = {:?}",
                            section.id, names,
                        );
                    }
                }
            }
        }
    }
}
