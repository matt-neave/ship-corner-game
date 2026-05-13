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
/// Shield bar overlaid on the HP track. Sits between `WaveHpFill`
/// and the numeric overlay so shield depletion reveals HP underneath.
/// Width is set per-frame from `Shield::current / stats.shield_max`.
#[derive(Component)]
pub struct ShieldFill;
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
        crate::xp::spawn_xp_track(p);
        // HP track — 180×22 with fill, ticks, right-aligned readout.
        // `overflow: clip` keeps the shield overlay bounded inside
        // the track — when shield_max + HP would overflow the bar,
        // the cyan portion gets cropped at the border rather than
        // spilling onto the world.
        p.spawn((
            Node {
                width: Val::Px(180.0),
                height: Val::Px(22.0),
                border: UiRect::all(Val::Px(2.0)),
                position_type: PositionType::Relative,
                overflow: Overflow::clip(),
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
            // Shield overlay — drawn on top of the green HP fill at
            // the leading edge of the bar. `update_wave_ui` sizes it
            // as `min(shield_cur, max_hp)` so it can never extend
            // past the track's right edge (the `overflow: clip` on
            // the track is a belt-and-braces gate).
            track.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Px(0.0),
                    left: Val::Px(0.0),
                    width: Val::Percent(0.0),
                    height: Val::Percent(100.0),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.35, 0.85, 0.95)),
                Visibility::Hidden,
                ShieldFill,
            ));
            // Numeric overlay — high ZIndex so the readout renders
            // above the per-50-HP subdivider lines (which are
            // dynamically respawned by `update_hp_subdividers` and
            // would otherwise occlude the digits).
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
            ))
            .with_children(|over| {
                over.spawn((
                    // Placeholder text — `update_wave_ui` rewrites
                    // this every frame from the live HP+shield pool.
                    Text::new("0/0"),
                    TextFont { font_size: 13.0, ..default() },
                    TextColor(theme::ON_SURFACE),
                    // Black drop shadow so the white digits read
                    // clearly over BOTH the green HP fill and the
                    // dark empty-bar background (without the shadow
                    // they vanish into the green at high HP).
                    TextShadow {
                        offset: Vec2::splat(1.0),
                        color: Color::srgba(0.0, 0.0, 0.0, 0.85),
                    },
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
/// numeric readout from the player's current `Health`. Also drives the
/// shield overlay from `Shield::current / stats.shield_max`.
pub fn update_wave_ui(
    view: Res<ViewMode>,
    stats: Res<crate::stats::PlayerStats>,
    friendly: Query<(&Health, Option<&crate::stats::Shield>), With<Friendly>>,
    mut hp_root_q: Query<&mut Visibility, With<WaveHpUi>>,
    mut hp_fill_q: Query<&mut Node, (With<WaveHpFill>, Without<ShieldFill>)>,
    mut hp_text_q: Query<&mut Text, With<WaveHpText>>,
    mut shield_q: Query<(&mut Node, &mut Visibility), (With<ShieldFill>, Without<WaveHpFill>, Without<WaveHpUi>)>,
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
    let shield_max = stats.shield_max.effective().max(0.0).round() as i32;
    let shield_cur = shield.map(|s| s.current).unwrap_or(0.0).max(0.0);

    // Bar pool = HP + shield. The bar's full width represents this
    // combined pool. HP fills from the left; shield stacks ON TOP
    // of the HP fill (anchored to its right edge), so the cyan
    // always butts up against the green and slides left as HP
    // drops. No gap between the two segments at any HP level.
    let total_pool = (max_hp + shield_max).max(1) as f32;

    let hp_fill_pct = (h.0 as f32 / total_pool).clamp(0.0, 1.0);
    // Shield width is its own contribution to the pool, capped so
    // the combined fill can never exceed 100%.
    let shield_fill_pct = (shield_cur / total_pool)
        .clamp(0.0, (1.0 - hp_fill_pct).max(0.0));

    // Readout: "HP+shield / max_hp". With 100 HP + 25 shield this
    // reads "125/100" — the slash is "current effective pool / hp
    // baseline", so the player can see total survivability at a
    // glance.
    let total_cur = h.0.max(0) + shield_cur.round() as i32;
    for mut t in &mut hp_text_q {
        **t = format!("{}/{}", total_cur, max_hp);
    }

    for mut node in &mut hp_fill_q {
        node.width = Val::Percent(hp_fill_pct * 100.0);
    }

    // Shield fill: anchored to the right edge of the HP fill so the
    // cyan always touches the green. Hidden entirely when
    // `shield_max == 0` (no shield bought).
    let want_vis = if shield_max > 0 { Visibility::Inherited } else { Visibility::Hidden };
    for (mut node, mut vis) in &mut shield_q {
        let want_left = Val::Percent(hp_fill_pct * 100.0);
        let want_w = Val::Percent(shield_fill_pct * 100.0);
        if node.left != want_left { node.left = want_left; }
        if node.width != want_w { node.width = want_w; }
        if *vis != want_vis { *vis = want_vis; }
    }
}

/// Despawn + respawn the bar's vertical tick lines whenever max HP
/// changes (PlayerStats.hp upgrades).
pub fn update_hp_subdividers(
    stats: Res<crate::stats::PlayerStats>,
    mut commands: Commands,
    track_q: Query<Entity, With<WaveHpTrack>>,
    subdivider_q: Query<Entity, With<HpBarSubdivider>>,
) {
    if !stats.is_changed() { return; }
    for e in &subdivider_q { commands.entity(e).despawn(); }
    let Ok(track) = track_q.single() else { return; };

    let max_hp = stats.max_hp();
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
