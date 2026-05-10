//! Death screen — semi-transparent overlay with RESTART / MAIN MENU /
//! QUIT. Spawned on `OnEnter(GameOver)` so the frozen play scene reads
//! through the backdrop; despawned on `OnExit(GameOver)`.
//!
//! `level_fail_check` (in `map::buildings`) is the only path into this
//! state: when the friendly hull hits 0 HP during a Sandbox combat, it
//! sets `NextState(GameOver)` and the combat-sim run-conditions idle the
//! same way they do for `Paused` / `Customize`. The arena is left intact
//! deliberately — the dead ship sitting behind the overlay sells the
//! "you lost" beat better than a wiped stage would.
//!
//! RESTART runs `reset_run` (full fresh-run state) and flips back to
//! `Playing`. MAIN MENU just transitions to `MainMenu`; that screen's
//! own `clear_arena_on_main_menu` hook handles the cleanup.

use bevy::app::AppExit;
use bevy::prelude::*;

use crate::map::{CombatContext, MapBoat, MapState};
use crate::ui_kit::{self, theme};

#[derive(Component)]
pub struct GameOverRoot;

#[derive(Component)]
pub struct RestartButton;

#[derive(Component)]
pub struct GameOverMainMenuButton;

#[derive(Component)]
pub struct GameOverQuitButton;

pub fn enter_game_over(mut commands: Commands) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(0.0),
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                bottom: Val::Px(0.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                row_gap: Val::Px(theme::GAP_LG),
                ..default()
            },
            // Semi-transparent so the frozen play scene reads through.
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.55)),
            // Sits below the customize overlay (200) and the pause menu
            // (180) — neither can be open during GameOver, but keeping
            // it under both leaves the layering invariant clean.
            ZIndex(170),
            Visibility::Inherited,
            GameOverRoot,
            // Absorb clicks so they don't fall through to gameplay UI.
            Button,
        ))
        .with_children(|root| {
            root.spawn(ui_kit::label(
                "GAME OVER",
                theme::FONT_LG * 1.8,
                Color::srgb(0.95, 0.30, 0.30),
            ));

            root.spawn((ui_kit::button(theme::SURFACE_RAISED), RestartButton))
                .with_children(|b| {
                    b.spawn(ui_kit::label("RESTART", theme::FONT_LG, theme::ON_SURFACE));
                });

            root.spawn((ui_kit::button(theme::SURFACE_RAISED), GameOverMainMenuButton))
                .with_children(|b| {
                    b.spawn(ui_kit::label("MAIN MENU", theme::FONT_LG, theme::ON_SURFACE));
                });

            root.spawn((ui_kit::button(theme::SURFACE_RAISED), GameOverQuitButton))
                .with_children(|b| {
                    b.spawn(ui_kit::label("QUIT", theme::FONT_LG, theme::ON_SURFACE));
                });
        });
}

pub fn exit_game_over(mut commands: Commands, q: Query<Entity, With<GameOverRoot>>) {
    for e in &q {
        commands.entity(e).despawn();
    }
}

/// Wipe everything to a fresh-run baseline: stats, scrap, campaign,
/// turret config, friendly HP, map state + boat position, combat
/// context, damage tallies, and the arena. Hooked from RESTART click.
pub fn reset_run_for_restart(
    mut stats: ResMut<crate::stats::PlayerStats>,
    mut scrap: ResMut<crate::Scrap>,
    mut campaign: ResMut<crate::CampaignProgress>,
    mut cfg: ResMut<crate::turret::TurretConfig>,
    mut combat_ctx: ResMut<CombatContext>,
    mut map_state: ResMut<MapState>,
    mut damage_stats: ResMut<crate::ui::DamageStats>,
    mut xp: ResMut<crate::xp::Xp>,
    mut pending: ResMut<crate::xp::LevelUpsPending>,
    selected_hull: Res<crate::hull::SelectedHull>,
    mut friendly: Query<&mut crate::components::Health, With<crate::components::Friendly>>,
    arena: Query<Entity, crate::wave::ArenaDisposeFilter>,
    mut boat: Query<&mut Transform, With<MapBoat>>,
    mut commands: Commands,
) {
    *stats = crate::stats::PlayerStats::default();
    // Re-apply the active hull's stat modifiers so a RESTART picks
    // up where the original PLAY left off — the player picked Glass
    // Cannon once, they keep Glass Cannon after dying. Returning to
    // MainMenu and re-PLAY-ing routes through HullSelect to repick.
    selected_hull.0.apply(&mut stats);
    scrap.0 = 15;
    *campaign = crate::CampaignProgress::default();
    *cfg = crate::turret::TurretConfig::default();
    *damage_stats = crate::ui::DamageStats::default();
    xp.reset();
    pending.0 = 0;

    if let Ok(mut h) = friendly.single_mut() {
        h.0 = stats.max_hp();
    }

    for e in &arena {
        commands.entity(e).despawn();
    }

    // Reset campaign progress to stage 1 — same shape as the boot-time
    // `CombatContext::default()` so the post-restart combat starts at
    // the smallest tier rather than continuing the run that just
    // failed.
    combat_ctx.reset_for(1, 0);
    combat_ctx.boss_pending = None;
    map_state.boat_target = None;
    map_state.current = 0;
    // Drop every section back to "unclaimed" except the starting one so
    // the run truly starts over. Section layout is preserved — only the
    // ownership bits + current pointer reset.
    let n = map_state.owned.len();
    map_state.owned = vec![false; n];
    if !map_state.owned.is_empty() {
        map_state.owned[0] = true;
    }
    if let Ok(mut tf) = boat.single_mut() {
        let s0 = map_state
            .sections
            .first()
            .map(|s| s.center)
            .unwrap_or(Vec2::ZERO);
        tf.translation.x = s0.x;
        tf.translation.y = s0.y;
    }
}

pub fn handle_restart_click(
    interactions: Query<&Interaction, (Changed<Interaction>, With<RestartButton>)>,
    mut next: ResMut<NextState<crate::AppState>>,
) {
    for interaction in &interactions {
        if matches!(*interaction, Interaction::Pressed) {
            next.set(crate::AppState::Playing);
        }
    }
}

pub fn handle_main_menu_click(
    interactions: Query<&Interaction, (Changed<Interaction>, With<GameOverMainMenuButton>)>,
    mut next: ResMut<NextState<crate::AppState>>,
) {
    for interaction in &interactions {
        if matches!(*interaction, Interaction::Pressed) {
            next.set(crate::AppState::MainMenu);
        }
    }
}

pub fn handle_quit_click(
    interactions: Query<&Interaction, (Changed<Interaction>, With<GameOverQuitButton>)>,
    mut exit: EventWriter<AppExit>,
) {
    for interaction in &interactions {
        if matches!(*interaction, Interaction::Pressed) {
            exit.write(AppExit::Success);
        }
    }
}
