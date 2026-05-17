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

use crate::ally::Ally;
use crate::components::{Friendly, Health};
use crate::i18n::tr;
use crate::modes::{play_area_screen_rect, VsyncMode};
use crate::map::ViewMode;
use crate::palette::{UI_TEXT, UI_TEXT_DIM, UI_VALUE};
use crate::ui_kit::theme;
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

/// Marker on the VSYNC toggle button root — lets the main-menu chrome
/// hider include it alongside FPS / MAP / FOLLOW.
#[derive(Component)]
pub struct VsyncButton;

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
/// Wrapper Node for the stacked HP readout (1 main Thaleah glyph
/// + 8 black stroke twins at diagonal/cardinal offsets). Children
/// are tagged with `WaveHpTextPart` so the per-frame label sync
/// updates every twin in lockstep.
#[derive(Component)]
pub struct WaveHpText;

/// One Text child of the `WaveHpText` wrapper. The main child paints
/// in the readout colour; stroke children paint black behind it so
/// the visible glyph picks up an 8-direction outline.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
pub enum WaveHpTextPart {
    Stroke,
    Main,
}
/// Top cyan stripe inside `WaveHpTrack`. Hidden when the build
/// has no shield (`shield_max == 0 && cur == 0`); when hidden the
/// HP zone expands to fill the whole bounding box so the player
/// isn't left staring at empty dark space at the top.
#[derive(Component)]
pub struct ShieldBarUi;
#[derive(Component)]
pub struct ShieldBarTrack;
#[derive(Component)]
pub struct ShieldBarFill;
/// Marker on the inner HP zone Node — `update_shield_bar` mutates
/// its `top` + `height` to collapse or expand depending on whether
/// the shield stripe is active.
#[derive(Component)]
pub struct WaveHpZone;

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

pub fn setup_hud(
    commands: &mut Commands,
    thaleah: &crate::fonts::ThaleahFont,
) {
    // Score banner removed for the prototype — re-enable by restoring
    // the spawn with `ScoreText` + `Text::new(format!("{} 0", tr("score_label")))`.
    // `update_score_text` is harmless when the entity doesn't exist
    // (the query is just empty).

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
        SlotButton { kind: ButtonKind::ToggleVsync },
        VsyncButton,
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
        SlotButton { kind: ButtonKind::ReturnToMap },
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
        SlotButton { kind: ButtonKind::ToggleCameraFollow },
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


    // Player HUD column — XP track on top, HP track below, ally
    // bars below that. Each bar owns its own border so
    // `update_hp_bar_pixel_scale` can scale BOTH to match the
    // play-area's grey frame at every window size.
    // `update_hp_bar_pixel_scale` snaps `WaveHpUi` to the play
    // area's top-left every frame.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(0.0),
            left: Val::Px(0.0),
            width: Val::Px(180.0),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::FlexStart,
            row_gap: Val::Px(3.0),
            ..default()
        },
        Visibility::Hidden,
        WaveHpUi,
    ))
    .with_children(|p| {
        // XP track — its own bordered bar, sits on top of the HP
        // bar in the column stack.
        crate::xp::spawn_xp_track(p, thaleah);
        // Unified HP+Shield track — one chunky-bordered container
        // with two stacked horizontal stripes inside: cyan shield
        // on top (7 px), green HP on bottom (15 px). Independent
        // fills + readouts, hidden shield stripe is just empty +
        // text-blank so the bounding box geometry stays constant.
        p.spawn((
            Node {
                width: Val::Px(150.0),
                height: Val::Px(22.0),
                border: UiRect::all(Val::Px(theme::CHUNKY_BORDER_W)),
                position_type: PositionType::Relative,
                overflow: Overflow::clip(),
                ..default()
            },
            BackgroundColor(theme::BORDER_SUBTLE),
            BorderColor(theme::BORDER_DARK),
            WaveHpTrack,
        ))
        .with_children(|track| {
            // ---- Shield stripe (top 7 px) ----
            // Hidden by default; `update_shield_bar` flips it on
            // when the build has any shield and simultaneously
            // resizes the HP zone so the bar geometry adapts.
            track.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Px(0.0),
                    left: Val::Px(0.0),
                    width: Val::Percent(100.0),
                    height: Val::Px(7.0),
                    ..default()
                },
                Visibility::Hidden,
                ShieldBarUi,
                ShieldBarTrack,
            ))
            .with_children(|zone| {
                zone.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        top: Val::Px(0.0),
                        left: Val::Px(0.0),
                        width: Val::Percent(0.0),
                        height: Val::Percent(100.0),
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.18, 0.38, 0.78)),
                    ShieldBarFill,
                ));
            });

            // ---- HP stripe ----
            // Default geometry assumes no shield (fills the whole
            // bounding box). `update_shield_bar` shrinks it down to
            // the bottom 15 px when the shield stripe becomes visible.
            track.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Px(0.0),
                    left: Val::Px(0.0),
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    ..default()
                },
                WaveHpZone,
            ))
            .with_children(|zone| {
                zone.spawn((
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
            });

            // ---- HP / shield readout ----
            // Wrapper sits at the WaveHpTrack root (not inside the
            // shrinking HP zone) so the digits stay the same size
            // and centred regardless of whether the shield stripe is
            // visible. Same 8-direction Thaleah stroke pattern as the
            // WAVE indicator — 8 black twins + 1 coloured main glyph.
            track.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Px(0.0),
                    left: Val::Px(0.0),
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    justify_content: JustifyContent::FlexEnd,
                    align_items: AlignItems::Center,
                    padding: UiRect::right(Val::Px(6.0)),
                    ..default()
                },
                ZIndex(10),
                WaveHpText,
            ))
            .with_children(|over| {
                const STROKE_OFFSETS: &[(f32, f32)] = &[
                    (-2.0, -2.0), ( 2.0, -2.0), (-2.0,  2.0), ( 2.0,  2.0),
                    (-2.0,  0.0), ( 2.0,  0.0), ( 0.0, -2.0), ( 0.0,  2.0),
                ];
                // Wrapping inner node holds the stack — sits inside
                // the right-aligned flex column at the wrapper level
                // so the stroke twins overlay the main glyph cleanly.
                over.spawn(Node {
                    position_type: PositionType::Relative,
                    width: Val::Auto,
                    height: Val::Auto,
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::Center,
                    ..default()
                })
                .with_children(|stack| {
                    for &(dx, dy) in STROKE_OFFSETS {
                        stack.spawn((
                            Node {
                                position_type: PositionType::Absolute,
                                top: Val::Px(dy),
                                left: Val::Px(dx),
                                ..default()
                            },
                            Text::new("0/0"),
                            crate::fonts::thaleah_text_font(thaleah, 18.0),
                            TextColor(Color::srgba(0.0, 0.0, 0.0, 0.95)),
                            WaveHpTextPart::Stroke,
                        ));
                    }
                    stack.spawn((
                        Text::new("0/0"),
                        crate::fonts::thaleah_text_font(thaleah, 18.0),
                        TextColor(theme::ON_SURFACE),
                        WaveHpTextPart::Main,
                    ));
                });
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

}

// ---------- Update systems ----------

pub fn update_score_text(
    score: Res<Score>,
    mut q: Query<&mut Text, With<ScoreText>>,
) {
    if !score.is_changed() { return; }
    for mut t in &mut q {
        **t = format!("{} {}", tr("score_label"), score.0);
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
    run_timer: Res<crate::RunTimer>,
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

    let total = run_timer.secs.max(0.0) as u32;
    let mins = total / 60;
    let secs = total % 60;

    for mut t in &mut q {
        **t = format!(
            "FPS {:.0}\n1%LOW {:.0}\n{:02}:{:02}",
            avg, one_pct_low, mins, secs,
        );
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

/// Show the MAP button only in Combat view AND only when the debug
/// UI toggle is on (`#` key) — Map view exits via clicking sections,
/// and the debug toggle hides this HUD chrome alongside the FPS /
/// VSYNC / FOLLOW buttons so a polished play view stays clean.
pub fn update_map_button(
    view: Res<ViewMode>,
    debug_visible: Res<crate::map::DebugUiVisible>,
    customize_open: Res<crate::customize::CustomizeOpen>,
    mut q: Query<&mut Visibility, With<ReturnToMapButton>>,
) {
    // Runs every frame (no `is_changed` early-bail) because the
    // customize-render toggle would otherwise force-show this
    // button on customize close without re-firing our gate.
    let want_show = matches!(*view, ViewMode::Combat)
        && debug_visible.0
        && !customize_open.open;
    let target = if want_show { Visibility::Inherited } else { Visibility::Hidden };
    for mut v in &mut q {
        if *v != target { *v = target; }
    }
}

/// Gate the FPS / VSYNC / FOLLOW top-right HUD buttons on the
/// `DebugUiVisible` toggle (`#` key) AND hide them while customize
/// is open so the shop view stays clean. These are dev-quality
/// readouts — hidden by default so the play view stays clean for
/// screenshots / players.
pub fn sync_hud_dev_buttons_visibility(
    debug_visible: Res<crate::map::DebugUiVisible>,
    customize_open: Res<crate::customize::CustomizeOpen>,
    mut fps_q: Query<
        &mut Visibility,
        (With<FpsText>, Without<VsyncButton>, Without<CameraFollowButton>),
    >,
    mut vsync_q: Query<
        &mut Visibility,
        (With<VsyncButton>, Without<FpsText>, Without<CameraFollowButton>),
    >,
    mut follow_q: Query<
        &mut Visibility,
        (With<CameraFollowButton>, Without<FpsText>, Without<VsyncButton>),
    >,
) {
    let want_show = debug_visible.0 && !customize_open.open;
    let want = if want_show { Visibility::Inherited } else { Visibility::Hidden };
    for mut v in &mut fps_q    { if *v != want { *v = want; } }
    for mut v in &mut vsync_q  { if *v != want { *v = want; } }
    for mut v in &mut follow_q { if *v != want { *v = want; } }
}

/// Toggle pier + HP-bar visibility and drive the bar's fill width +
/// numeric readout from the player's current `Health`. Also drives the
/// shield overlay from `Shield::current / stats.shield_max`.
pub fn update_wave_ui(
    view: Res<ViewMode>,
    stats: Res<crate::stats::PlayerStats>,
    friendly: Query<(&Health, Option<&crate::stats::Shield>), With<Friendly>>,
    mut hp_root_q: Query<&mut Visibility, With<WaveHpUi>>,
    mut hp_fill_q: Query<&mut Node, With<WaveHpFill>>,
    mut hp_text_q: Query<&mut Text, With<WaveHpTextPart>>,
) {
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

    let Ok((h, shield)) = friendly.single() else { return; };
    let max_hp = stats.max_hp();
    let hp_fill_pct = (h.0 as f32 / max_hp.max(1) as f32).clamp(0.0, 1.0);

    // Combined effective-health readout: shield_cur + hp_cur over
    // hp_max. Ward / Conduit stacking naturally pushes the numerator
    // past the denominator ("175/100") so the player sees the bonus
    // pool without a separate label. When shield is 0 the readout
    // collapses to the plain HP form.
    let shield_max = stats.shield_max.effective().max(0.0).round() as i32;
    let shield_cur = shield.map(|s| s.current).unwrap_or(0.0).max(0.0).round() as i32;
    let label = if shield_max > 0 || shield_cur > 0 {
        format!("{}/{}", (h.0.max(0) + shield_cur), max_hp)
    } else {
        format!("{}/{}", h.0.max(0), max_hp)
    };
    for mut t in &mut hp_text_q {
        if t.0 != label { **t = label.clone(); }
    }
    for mut node in &mut hp_fill_q {
        node.width = Val::Percent(hp_fill_pct * 100.0);
    }
}

/// Drive the dedicated shield bar (`ShieldBarUi`) below the HP bar.
///
/// Hidden when both `shield_max` and `shield_cur` are zero — most
/// builds without Barrier / Ward see no shield bar at all and the
/// HP / XP rails sit alone in the top-left column.
///
/// Fill width: `shield_cur / max(shield_max, shield_cur)`. The
/// `max` denominator means an overflowed pool (Ward stacking past
/// nominal max) saturates at 100% rather than clipping. The numeric
/// readout flips gold + drops the "/max" suffix to flag overflow,
/// so the player sees the actual `shield_cur` value even after the
/// bar caps.
pub fn update_shield_bar(
    stats: Res<crate::stats::PlayerStats>,
    friendly: Query<Option<&crate::stats::Shield>, With<Friendly>>,
    mut shield_root_q: Query<&mut Visibility, With<ShieldBarUi>>,
    mut fill_q: Query<&mut Node, (With<ShieldBarFill>, Without<WaveHpZone>)>,
    mut hp_zone_q: Query<&mut Node, (With<WaveHpZone>, Without<ShieldBarFill>)>,
) {
    let Ok(shield) = friendly.single() else { return };
    let shield_max = stats.shield_max.effective().max(0.0).round() as i32;
    let shield_cur = shield.map(|s| s.current).unwrap_or(0.0).max(0.0);
    let shield_cur_int = shield_cur.round() as i32;
    let active = shield_max > 0 || shield_cur_int > 0;

    // Toggle the cyan stripe + resize the HP zone in the same pass.
    // Inactive: shield hidden, HP zone fills the whole 22-px box.
    // Active: shield visible (top 7 px), HP zone shrinks to the
    // bottom 15 px.
    let want_vis = if active { Visibility::Inherited } else { Visibility::Hidden };
    for mut v in &mut shield_root_q {
        if *v != want_vis { *v = want_vis; }
    }
    let (hp_top, hp_h) = if active {
        (Val::Px(7.0), Val::Px(15.0))
    } else {
        (Val::Px(0.0), Val::Percent(100.0))
    };
    for mut node in &mut hp_zone_q {
        if node.top != hp_top { node.top = hp_top; }
        if node.height != hp_h { node.height = hp_h; }
    }

    // Cyan fill width. Overflow (Ward-stacked shield > max) saturates
    // at 100% — the actual `shield_cur` value lands in the combined
    // HP readout written by `update_wave_ui` so the player still sees
    // the bonus pool numerically.
    if active {
        let denom = (shield_max as f32).max(shield_cur).max(1.0);
        let fill_pct = (shield_cur / denom).clamp(0.0, 1.0) * 100.0;
        for mut node in &mut fill_q {
            let want = Val::Percent(fill_pct);
            if node.width != want { node.width = want; }
        }
    }
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
/// frame: spawn for new allies, despawn bars whose ally is gone OR
/// whose HP has hit zero (so the bar disappears at the same frame the
/// ally falls, not a frame later after `ally_death_check` despawns
/// the entity).
pub fn sync_ally_hp_bars(
    mut commands: Commands,
    container_q: Query<Entity, With<AllyHpRow>>,
    // `Without<Enemy>` excludes bosses — they carry both `Ally` (for
    // class-aware AI) and `Enemy` (for routing). The ally HP row is
    // the *player's fleet* readout, not "every Ally-tagged entity".
    allies: Query<
        (Entity, &crate::components::Health),
        (With<Ally>, Without<crate::enemy::Enemy>),
    >,
    bars: Query<(Entity, &AllyHpBar)>,
) {
    use std::collections::HashSet;
    // Treat 0-HP allies as already-defeated for visibility purposes —
    // the entity may still exist for one more frame until
    // `ally_death_check` runs, but the bar should be gone immediately.
    let live: HashSet<Entity> = allies
        .iter()
        .filter(|(_, h)| h.0 > 0)
        .map(|(e, _)| e)
        .collect();
    let bar_targets: HashSet<Entity> = bars.iter().map(|(_, b)| b.ally).collect();

    for (bar_e, bar) in &bars {
        if !live.contains(&bar.ally) {
            commands.entity(bar_e).despawn();
        }
    }

    let Ok(container) = container_q.single() else { return; };
    let new_allies: Vec<Entity> = allies
        .iter()
        .filter(|(e, h)| h.0 > 0 && !bar_targets.contains(e))
        .map(|(e, _)| e)
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

/// Anchor the HP bar to the play area's top-left every frame. The bar's
/// own dimensions (width, height, border thickness, subdivider widths)
/// are now governed entirely by `UiScale` — they're authored as fixed
/// `Val::Px` values at spawn time and multiplied by the scale-with-
/// window factor in `sync_ui_scale`. Previously this system wrote a
/// per-frame `upscale` value into every border / subdivider, which
/// stacked with `UiScale`'s own multiplication and produced
/// double-scaled chrome (huge borders on big windows, invisible-thin
/// borders on small ones).
///
/// Positions are screen-pixel coordinates (from `play_area_screen_rect`,
/// which already returns the right values for the live window). We
/// divide by `UiScale` so that when the bevy_ui layout pass multiplies
/// the `Val::Px` back up, we end up at the exact screen pixel we wanted.
pub fn update_hp_bar_pixel_scale(
    windows: Query<&Window, With<PrimaryWindow>>,
    ui_scale: Res<UiScale>,
    mut root_q: Query<&mut Node, With<WaveHpUi>>,
) {
    let Ok(win) = windows.single() else { return; };
    let (play_left, play_top, _play_w, _play_h) =
        play_area_screen_rect(win.width(), win.height());
    let s = ui_scale.0.max(0.0001);
    // 4 design pixels of inset from the play-area corner — matches the
    // pre-UiScale `upscale * 4.0` intent (4 game-pixels of margin).
    // Authored in design pixels so UiScale handles the scaling.
    let margin_design = 4.0;
    let want_left = Val::Px(play_left / s + margin_design);
    let want_top  = Val::Px(play_top  / s + margin_design);
    for mut node in &mut root_q {
        if node.left != want_left { node.left = want_left; }
        if node.top  != want_top  { node.top  = want_top;  }
    }
}
