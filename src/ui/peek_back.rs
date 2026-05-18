//! "BACK TO SHOP" button — surfaces inside the map view only
//! when [`MapPeek`] is active. The player came over from the
//! shop to inspect; clicking this button returns them straight
//! back without rerolling the shop (handled in
//! `init_customize_shop` by checking `MapPeek.active`).
//!
//! Visual style matches the chunky-CTA vocabulary used by the
//! main menu / lobby / hull-select PLAY button so the affordance
//! reads as "primary action right now."

use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::customize::MapPeek;
use crate::map::ViewMode;
use crate::modes::play_area_screen_rect;
use crate::ui_kit::{self, theme, ChunkyButtonStyle};
use crate::AppState;

/// Root wrapper for the button — owns the absolute top-right
/// anchor + visibility toggle.
#[derive(Component)]
pub struct PeekBackUi;

/// Tappable button child. Click → `AppState::Customize`.
#[derive(Component)]
pub struct PeekBackButton;

pub fn setup_peek_back(mut commands: Commands, font: Res<crate::fonts::PixelFont>) {
    let style = ChunkyButtonStyle::cta();
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(0.0),
                right: Val::Px(0.0),
                ..default()
            },
            ZIndex(45),
            Visibility::Hidden,
            PeekBackUi,
        ))
        .with_children(|p| {
            p.spawn((
                Button,
                Node {
                    padding: UiRect::axes(
                        Val::Px(theme::PAD_LG),
                        Val::Px(theme::PAD_MD),
                    ),
                    border: UiRect::all(Val::Px(theme::CHUNKY_BORDER_W)),
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::Center,
                    ..default()
                },
                BackgroundColor(style.idle_fill),
                BorderColor(style.idle_outline),
                BorderRadius::all(Val::Px(theme::CHUNKY_RADIUS)),
                style,
                PeekBackButton,
            ))
            .with_children(|b| {
                b.spawn(ui_kit::pixel_label(
                    &font,
                    "BACK TO SHOP",
                    theme::FONT_LG,
                    theme::ON_CTA,
                ));
            });
        });
}

/// Per-frame: pin to the play-area's top-right corner and toggle
/// visibility on `MapPeek.active` + `ViewMode::Map`. Mirrors the
/// `/ ui_scale` rule for the cursor-style anchor math.
pub fn update_peek_back(
    state: Res<State<AppState>>,
    view: Res<ViewMode>,
    peek: Res<MapPeek>,
    windows: Query<&Window, With<PrimaryWindow>>,
    ui_scale: Res<bevy::ui::UiScale>,
    mut q: Query<(&mut Visibility, &mut Node), With<PeekBackUi>>,
) {
    let want_vis = if peek.active
        && matches!(*state.get(), AppState::Map)
        && *view == ViewMode::Map
    {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };

    let ui_s = ui_scale.0.max(0.0001);
    let (anchor_top, anchor_right) = windows
        .single()
        .ok()
        .map(|w| {
            let (play_left, top, play_w, _) = play_area_screen_rect(w.width(), w.height());
            let play_right_screen = play_left + play_w;
            let right_from_edge = w.width() - play_right_screen;
            (
                (top / ui_s + 8.0).max(2.0),
                (right_from_edge / ui_s + 8.0).max(2.0),
            )
        })
        .unwrap_or((8.0, 8.0));

    for (mut v, mut node) in &mut q {
        if *v != want_vis { *v = want_vis; }
        let want_top = Val::Px(anchor_top);
        let want_right = Val::Px(anchor_right);
        if node.top != want_top { node.top = want_top; }
        if node.right != want_right { node.right = want_right; }
    }
}

/// Click handler: transition back to Customize. `MapPeek.active`
/// stays `true` across the transition — `init_customize_shop`
/// reads it on entry, skips the reroll, and clears the flag.
pub fn handle_peek_back_click(
    interactions: Query<&Interaction, (Changed<Interaction>, With<PeekBackButton>)>,
    mut next: ResMut<NextState<AppState>>,
) {
    for interaction in &interactions {
        if matches!(*interaction, Interaction::Pressed) {
            next.set(AppState::Customize);
            return;
        }
    }
}
