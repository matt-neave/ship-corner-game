//! Top-of-screen hint banner shown only on the map view: tells the
//! player what to do here. Single bevy_ui Text2d node, anchored
//! above the play sprite, hidden whenever `ViewMode != Map`.
//!
//! Mirrors the wave-indicator pattern (sibling module) — own setup
//! + per-frame visibility & positioning system.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::map::ViewMode;
use crate::modes::play_area_screen_rect;

/// Marker on the map-hint Text node.
#[derive(Component)]
pub struct MapHint;

const HINT_TEXT: &str = "Capture all regions to win.   Left-click to move.";

pub fn setup_map_hint(mut commands: Commands, thaleah: Res<crate::fonts::ThaleahFont>) {
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(8.0),
            left: Val::Px(0.0),
            right: Val::Px(0.0),
            justify_content: JustifyContent::Center,
            ..default()
        },
        ZIndex(40),
        Visibility::Hidden,
        MapHint,
    ))
    .with_children(|p| {
        p.spawn((
            Text::new(HINT_TEXT),
            crate::fonts::thaleah_text_font(&thaleah, 18.0),
            TextColor(Color::WHITE),
            TextShadow {
                offset: Vec2::splat(1.0),
                color: Color::srgba(0.0, 0.0, 0.0, 0.95),
            },
        ));
    });
}

/// Hide unless `ViewMode::Map`. Anchored just above the play-area
/// top edge so the line sits in the letterbox region, not on top
/// of the map content. Same UiScale-compensated math as the wave
/// indicator.
pub fn update_map_hint(
    state: Res<State<crate::AppState>>,
    view: Res<ViewMode>,
    windows: Query<&Window, With<PrimaryWindow>>,
    ui_scale: Res<bevy::ui::UiScale>,
    mut q: Query<(&mut Visibility, &mut Node), With<MapHint>>,
) {
    let s = *state.get();
    // Only show on the actual Map state (not MainMenu, not Playing
    // map-mode glances) — the hint is a teaching prompt for the
    // strategic-map screen.
    let want_vis = if matches!(s, crate::AppState::Map) && *view == ViewMode::Map {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };

    // Anchor the hint just above the play-area top edge — sits in
    // the letterbox region between the window edge and the play
    // sprite. With wide_play, that band is wider so the hint has
    // more breathing room.
    let ui_s = ui_scale.0.max(0.0001);
    let anchor_top = windows
        .single()
        .ok()
        .map(|w| {
            let (_left, top, _play_w, _play_h) = play_area_screen_rect(w.width(), w.height());
            // 8 design px above the play area's top edge — the hint
            // ends up in the letterbox band rather than on the map.
            (top / ui_s - 28.0).max(4.0)
        })
        .unwrap_or(4.0);

    for (mut v, mut node) in &mut q {
        if *v != want_vis { *v = want_vis; }
        let top_val = Val::Px(anchor_top);
        if node.top != top_val { node.top = top_val; }
    }
}
