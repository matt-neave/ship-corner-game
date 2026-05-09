//! Always-on HUD chrome: top score/wave banner, FPS counter, VSYNC /
//! MAP / FOLLOW corner buttons, top-left HP bar (player + ally rows),
//! desktop-mode hint, and the bottom-of-screen draft prompt root.
//!
//! All entities here are bevy_ui Nodes rendered through the upscale
//! camera (native resolution). The customize overlay's
//! `toggle_customize_render` hides this whole layer while the player
//! is in the loadout screen.

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::ecs::hierarchy::ChildSpawnerCommands;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::balance::{FRIENDLY_HP_WAVE, PLAY_INTERNAL, UI_WIDTH};
use crate::ally::Ally;
use crate::components::{Friendly, Health};
use crate::i18n::tr;
use crate::modes::{
    effective_ui_width, play_area_screen_rect,
    DesktopHint, GameMode, VsyncMode, WindowMode,
};
use crate::map::ViewMode;
use crate::palette::{UI_TEXT, UI_TEXT_DIM, UI_VALUE};
use crate::pier::{spawn_draft_card, DraftPanel, PierVisual};
use crate::ui_kit::theme;
use crate::wave::WaveState;
use crate::Score;

use super::{ButtonKind, SlotButton};

// ---------- Marker components ----------

/// Top-center "SCORE N" / "WAVE N" text.
#[derive(Component)]
pub struct ScoreText;

/// Top-right FPS counter, driven by `FrameTimeDiagnosticsPlugin`.
#[derive(Component)]
pub struct FpsText;

/// Label inside the VSYNC toggle button. Updated whenever
/// `VsyncMode.enabled` flips.
#[derive(Component)]
pub struct VsyncLabel;

/// Marker for the "MAP" button — visibility toggled so it only appears
/// while in Combat view.
#[derive(Component)]
pub struct ReturnToMapButton;

/// Marker on the FOLLOW button + its label.
#[derive(Component)]
pub struct CameraFollowButton;

#[derive(Component)]
pub struct CameraFollowLabel;

/// Top-left HP bar container — outer Node holding the "HP" label + track.
#[derive(Component)]
pub struct WaveHpUi;
/// The bar track itself.
#[derive(Component)]
pub struct WaveHpTrack;
/// Coloured fill inside the track — width animated by `update_wave_ui`.
#[derive(Component)]
pub struct WaveHpFill;
/// Numeric readout overlaid centered inside the track.
#[derive(Component)]
pub struct WaveHpText;
/// Vertical tick line inside the track, one per 50-HP mark.
#[derive(Component)]
pub struct HpBarSubdivider;

/// Container below the main HP bar that holds one bar per live ally.
#[derive(Component)]
pub struct AllyHpRow;

/// One ally's HP bar.
#[derive(Component)]
pub struct AllyHpBar { pub ally: Entity }

/// Coloured fill child of an `AllyHpBar`.
#[derive(Component)]
pub struct AllyHpFill { pub ally: Entity }

// ---------- Setup ----------

pub fn setup_hud(commands: &mut Commands) {
    // Score / wave banner — pixel-game compact, top-center over the play area.
    commands.spawn((
        Text::new(format!("{} 0", tr("score_label"))),
        TextFont { font_size: 22.0, ..default() },
        TextColor(UI_VALUE),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(6.0),
            left: Val::Px(UI_WIDTH),
            right: Val::Px(0.0),
            justify_content: JustifyContent::Center,
            ..default()
        },
        ScoreText,
    ));

    // FPS counter — small, dim, top-right corner.
    commands.spawn((
        Text::new("FPS --"),
        TextFont { font_size: 9.0, ..default() },
        TextColor(UI_TEXT_DIM),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(4.0),
            right: Val::Px(6.0),
            ..default()
        },
        FpsText,
    ));

    // VSYNC toggle — directly below the FPS counter.
    commands.spawn((
        Button,
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(18.0),
            right: Val::Px(6.0),
            padding: UiRect::axes(Val::Px(4.0), Val::Px(2.0)),
            border: UiRect::all(Val::Px(1.0)),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            ..default()
        },
        BackgroundColor(Color::NONE),
        BorderColor(UI_TEXT_DIM),
        SlotButton { slot: 0, kind: ButtonKind::ToggleVsync },
    ))
    .with_children(|b| {
        b.spawn((
            Text::new("VSYNC OFF"),
            TextFont { font_size: 8.0, ..default() },
            TextColor(UI_TEXT),
            VsyncLabel,
        ));
    });

    // MAP button — combat-only; gated by `update_map_button`.
    commands.spawn((
        Button,
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(36.0),
            right: Val::Px(6.0),
            padding: UiRect::axes(Val::Px(6.0), Val::Px(2.0)),
            border: UiRect::all(Val::Px(1.0)),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            ..default()
        },
        BackgroundColor(Color::NONE),
        BorderColor(UI_VALUE),
        Visibility::Hidden,
        SlotButton { slot: 0, kind: ButtonKind::ReturnToMap },
        ReturnToMapButton,
    ))
    .with_children(|b| {
        b.spawn((
            Text::new("MAP"),
            TextFont { font_size: 9.0, ..default() },
            TextColor(UI_VALUE),
        ));
    });

    // FOLLOW toggle — sits below MAP.
    commands.spawn((
        Button,
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(60.0),
            right: Val::Px(6.0),
            padding: UiRect::axes(Val::Px(6.0), Val::Px(2.0)),
            border: UiRect::all(Val::Px(1.0)),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            ..default()
        },
        BackgroundColor(Color::NONE),
        BorderColor(UI_VALUE),
        SlotButton { slot: 0, kind: ButtonKind::ToggleCameraFollow },
        CameraFollowButton,
    ))
    .with_children(|b| {
        b.spawn((
            Text::new("FOLLOW"),
            TextFont { font_size: 9.0, ..default() },
            TextColor(UI_VALUE),
            CameraFollowLabel,
        ));
    });

    // Wave-mode draft panel — bottom of screen, hidden unless WavePhase::Drafting.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            bottom: Val::Px(14.0),
            left: Val::Px(UI_WIDTH),
            right: Val::Px(0.0),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            row_gap: Val::Px(4.0),
            ..default()
        },
        Visibility::Hidden,
        DraftPanel,
    ))
    .with_children(|p| {
        p.spawn((
            Text::new(tr("draft_instruction")),
            TextFont { font_size: 9.0, ..default() },
            TextColor(UI_TEXT_DIM),
        ));
        p.spawn(Node {
            flex_direction: FlexDirection::Row,
            column_gap: Val::Px(8.0),
            ..default()
        })
        .with_children(|row| {
            for i in 0..3u8 {
                spawn_draft_card(row, i);
            }
        });
    });

    // Player HP bar + ally bars — anchored inside the play square's top-left.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(0.0),
            left: Val::Px(0.0),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::FlexStart,
            row_gap: Val::Px(3.0),
            ..default()
        },
        Visibility::Hidden,
        WaveHpUi,
    ))
    .with_children(|p| {
        // Main bar — 180×22 track with fill, ticks, right-aligned readout.
        p.spawn((
            Node {
                width: Val::Px(180.0),
                height: Val::Px(22.0),
                border: UiRect::all(Val::Px(1.0)),
                position_type: PositionType::Relative,
                ..default()
            },
            BackgroundColor(theme::BORDER_SUBTLE),
            BorderColor(theme::BORDER_DARK),
            WaveHpTrack,
        ))
        .with_children(|track| {
            track.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Px(0.0),
                    left: Val::Px(0.0),
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.25, 0.85, 0.30)),
                WaveHpFill,
            ));
            // Numeric overlay.
            track.spawn(Node {
                position_type: PositionType::Absolute,
                top: Val::Px(0.0),
                left: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::FlexEnd,
                align_items: AlignItems::Center,
                padding: UiRect::right(Val::Px(6.0)),
                ..default()
            })
            .with_children(|over| {
                over.spawn((
                    Text::new(format!("{}", FRIENDLY_HP_WAVE)),
                    TextFont { font_size: 13.0, ..default() },
                    TextColor(theme::ON_SURFACE),
                    WaveHpText,
                ));
            });
        });

        // Ally bar container — populated dynamically by `sync_ally_hp_bars`.
        p.spawn((
            Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(2.0),
                ..default()
            },
            AllyHpRow,
        ));
    });

    // Desktop-mode hint — small grey text at top-center, hidden by default.
    commands.spawn((
        Text::new(tr("btn_press_esc")),
        TextFont { font_size: 8.0, ..default() },
        TextColor(Color::srgb(0.7, 0.72, 0.78)),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(3.0),
            left: Val::Px(0.0),
            right: Val::Px(0.0),
            justify_content: JustifyContent::Center,
            ..default()
        },
        Visibility::Hidden,
        DesktopHint,
    ));
}

// ---------- Update systems ----------

pub fn update_score_text(
    score: Res<Score>,
    mode: Res<GameMode>,
    wave: Res<WaveState>,
    mut q: Query<&mut Text, With<ScoreText>>,
) {
    if !score.is_changed() && !mode.is_changed() && !wave.is_changed() { return; }
    for mut t in &mut q {
        **t = match *mode {
            GameMode::Sandbox => format!("{} {}", tr("score_label"), score.0),
            GameMode::Wave    => format!("{} {}", tr("wave_label"),  wave.wave.max(1)),
        };
    }
}

/// FPS counter. Computes a true arithmetic mean over Bevy's diagnostic
/// history and writes it to the top-right text node at 4 Hz.
///
/// Why not `Diagnostic::smoothed()`? That's an EMA with a per-frame
/// smoothing factor — at 240 fps it fully converges in ~0.25 s, so it
/// tracks recent noise rather than a stable average. A simple mean over
/// the full history (120 samples by default → 0.5–2 s of frames) damps
/// that out cleanly.
///
/// **1% low**: 1st-percentile of the FPS history. Catches stutters the
/// average hides.
pub fn update_fps_text(
    diagnostics: Res<DiagnosticsStore>,
    time: Res<Time>,
    mut refresh_timer: Local<f32>,
    mut q: Query<&mut Text, With<FpsText>>,
) {
    *refresh_timer -= time.delta_secs();
    if *refresh_timer > 0.0 { return; }
    *refresh_timer = 0.25;

    let Some(diag) = diagnostics.get(&FrameTimeDiagnosticsPlugin::FPS) else { return; };
    let history: Vec<f64> = diag.values().copied().collect();
    if history.is_empty() { return; }

    let avg = history.iter().sum::<f64>() / history.len() as f64;

    let mut sorted = history;
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((sorted.len() as f64 * 0.01) as usize).min(sorted.len() - 1);
    let one_pct_low = sorted[idx];

    for mut t in &mut q {
        **t = format!("FPS {:.0}\n1%LOW {:.0}", avg, one_pct_low);
    }
}

pub fn update_vsync_label(
    vsync: Res<VsyncMode>,
    mut q: Query<&mut Text, With<VsyncLabel>>,
) {
    if !vsync.is_changed() { return; }
    for mut t in &mut q {
        **t = if vsync.enabled { "VSYNC ON".into() } else { "VSYNC OFF".into() };
    }
}

/// Show the MAP button only in Combat view — Map view exits via clicking
/// sections, not via this button.
pub fn update_map_button(
    view: Res<ViewMode>,
    mut q: Query<&mut Visibility, With<ReturnToMapButton>>,
) {
    if !view.is_changed() { return; }
    let target = match *view {
        ViewMode::Combat => Visibility::Inherited,
        ViewMode::Map    => Visibility::Hidden,
    };
    for mut v in &mut q {
        if *v != target { *v = target; }
    }
}

/// Toggle pier + HP-bar visibility and drive the bar's fill width +
/// numeric readout from the player's current `Health`.
pub fn update_wave_ui(
    mode: Res<GameMode>,
    view: Res<ViewMode>,
    friendly: Query<&Health, With<Friendly>>,
    mut pier_q: Query<&mut Visibility, (With<PierVisual>, Without<WaveHpUi>)>,
    mut hp_root_q: Query<&mut Visibility, (With<WaveHpUi>, Without<PierVisual>)>,
    mut hp_fill_q: Query<&mut Node, With<WaveHpFill>>,
    mut hp_text_q: Query<&mut Text, With<WaveHpText>>,
) {
    if mode.is_changed() {
        let want_pier = matches!(*mode, GameMode::Wave);
        let target = if want_pier { Visibility::Inherited } else { Visibility::Hidden };
        for mut v in &mut pier_q { if *v != target { *v = target; } }
    }

    if view.is_changed() {
        let want = if matches!(*view, ViewMode::Combat) {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        for mut v in &mut hp_root_q {
            if *v != want { *v = want; }
        }
    }

    let Ok(h) = friendly.single() else { return; };
    let max_hp = current_max_hp(&mode);
    let pct = (h.0 as f32 / max_hp as f32).clamp(0.0, 1.0);

    for mut node in &mut hp_fill_q {
        node.width = Val::Percent(pct * 100.0);
    }
    for mut t in &mut hp_text_q {
        **t = format!("{}", h.0.max(0));
    }
}

/// Despawn + respawn the bar's vertical tick lines whenever max HP
/// changes (currently mode flips: Sandbox 100 ↔ Wave 50).
pub fn update_hp_subdividers(
    mode: Res<GameMode>,
    mut commands: Commands,
    track_q: Query<Entity, With<WaveHpTrack>>,
    subdivider_q: Query<Entity, With<HpBarSubdivider>>,
) {
    if !mode.is_changed() { return; }
    for e in &subdivider_q { commands.entity(e).despawn(); }
    let Ok(track) = track_q.single() else { return; };

    let max_hp = current_max_hp(&mode);
    commands.entity(track).with_children(|t| {
        let step = 50;
        let mut hp = step;
        while hp < max_hp {
            let pct = hp as f32 / max_hp as f32 * 100.0;
            t.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Percent(pct),
                    top: Val::Px(0.0),
                    width: Val::Px(1.0),
                    height: Val::Percent(100.0),
                    ..default()
                },
                BackgroundColor(theme::BORDER_DARK),
                HpBarSubdivider,
            ));
            hp += step;
        }
    });
}

fn current_max_hp(mode: &GameMode) -> i32 {
    if matches!(mode, GameMode::Wave) { FRIENDLY_HP_WAVE } else { 100 }
}

fn spawn_ally_hp_bar(parent: &mut ChildSpawnerCommands, ally: Entity) {
    parent
        .spawn((
            Node {
                width: Val::Px(120.0),
                height: Val::Px(14.0),
                border: UiRect::all(Val::Px(1.0)),
                position_type: PositionType::Relative,
                ..default()
            },
            BackgroundColor(theme::BORDER_SUBTLE),
            BorderColor(theme::BORDER_DARK),
            AllyHpBar { ally },
        ))
        .with_children(|b| {
            b.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Px(0.0),
                    left: Val::Px(0.0),
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.25, 0.85, 0.30)),
                AllyHpFill { ally },
            ));
        });
}

/// Reconcile the ally HP-bar set with the live `Ally` entities each
/// frame: spawn for new allies, despawn bars whose ally is gone.
pub fn sync_ally_hp_bars(
    mut commands: Commands,
    container_q: Query<Entity, With<AllyHpRow>>,
    allies: Query<Entity, With<Ally>>,
    bars: Query<(Entity, &AllyHpBar)>,
) {
    use std::collections::HashSet;
    let live: HashSet<Entity> = allies.iter().collect();
    let bar_targets: HashSet<Entity> = bars.iter().map(|(_, b)| b.ally).collect();

    for (bar_e, bar) in &bars {
        if !live.contains(&bar.ally) {
            commands.entity(bar_e).despawn();
        }
    }

    let Ok(container) = container_q.single() else { return; };
    let new_allies: Vec<Entity> = allies
        .iter()
        .filter(|e| !bar_targets.contains(e))
        .collect();
    if new_allies.is_empty() { return; }
    commands.entity(container).with_children(|c| {
        for ally in new_allies {
            spawn_ally_hp_bar(c, ally);
        }
    });
}

pub fn update_ally_hp_values(
    allies: Query<(&Ally, &Health)>,
    mut fills: Query<(&AllyHpFill, &mut Node)>,
) {
    for (marker, mut node) in &mut fills {
        let Ok((ally, h)) = allies.get(marker.ally) else { continue; };
        let max = ally.class.hp().max(1);
        let pct = (h.0 as f32 / max as f32).clamp(0.0, 1.0);
        let want = Val::Percent(pct * 100.0);
        if node.width != want { node.width = want; }
    }
}

/// Anchor the HP bar inside the play square's top-left and match its
/// chrome (border + tick widths) to one game pixel so it lives on the
/// same grid as the upscaled play sprite.
pub fn update_hp_bar_pixel_scale(
    windows: Query<&Window, With<PrimaryWindow>>,
    window_mode: Res<WindowMode>,
    mut root_q: Query<
        &mut Node,
        (
            With<WaveHpUi>,
            Without<WaveHpTrack>,
            Without<HpBarSubdivider>,
            Without<AllyHpBar>,
        ),
    >,
    mut track_q: Query<
        &mut Node,
        (
            With<WaveHpTrack>,
            Without<WaveHpUi>,
            Without<HpBarSubdivider>,
            Without<AllyHpBar>,
        ),
    >,
    mut subdivider_q: Query<
        &mut Node,
        (
            With<HpBarSubdivider>,
            Without<WaveHpUi>,
            Without<WaveHpTrack>,
            Without<AllyHpBar>,
        ),
    >,
    mut ally_bar_q: Query<
        &mut Node,
        (
            With<AllyHpBar>,
            Without<WaveHpUi>,
            Without<WaveHpTrack>,
            Without<HpBarSubdivider>,
        ),
    >,
) {
    let Ok(win) = windows.single() else { return; };
    let (play_left, play_top, size) = play_area_screen_rect(
        win.width(), win.height(), effective_ui_width(&window_mode),
    );
    let upscale = (size / PLAY_INTERNAL as f32).max(1.0);
    let px = Val::Px(upscale);
    let border = UiRect::all(px);

    let margin = upscale * 4.0;
    let want_left = Val::Px(play_left + margin);
    let want_top  = Val::Px(play_top  + margin);
    for mut node in &mut root_q {
        if node.left != want_left { node.left = want_left; }
        if node.top  != want_top  { node.top  = want_top;  }
    }

    for mut node in &mut track_q {
        if node.border != border { node.border = border; }
    }
    for mut node in &mut subdivider_q {
        if node.width != px { node.width = px; }
    }
    for mut node in &mut ally_bar_q {
        if node.border != border { node.border = border; }
    }
}
