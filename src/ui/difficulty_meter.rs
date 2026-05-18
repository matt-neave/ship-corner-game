//! Risk-of-Rain-style difficulty meter shown on the map view.
//!
//! Coefficient = `1 + 0.22 * min(battles_cleared, 12)`. That is
//! exactly the wave-size multiplier in [`balance::wave_size`], so
//! the displayed `×N.NN` is the literal scalar applied to wave
//! counts (and, indirectly, to the on-screen enemy cap + spawn
//! interval, which both also ramp with `battles_cleared`).
//!
//! The player's chosen [`Difficulty`] tier is intentionally NOT
//! folded in — that's a static run-wide setting that shifts enemy
//! HP / damage but not the run-progression curve. The meter is
//! about how far the current run has climbed, not which tier was
//! picked at the dockyard.
//!
//! Visual: chunky-bordered track in the same style as the player
//! HP bar (`WaveHpTrack` in `ui::hud`). Inside the track, seven flat
//! tier-coloured segments lay out left-to-right with NO interpolation
//! between them — the harsh boundaries are the look. A dim overlay
//! is anchored to the right and shrinks as `battles_cleared` climbs,
//! uncovering the segments one tier at a time.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::map::ViewMode;
use crate::modes::play_area_screen_rect;
use crate::ui_kit::theme;
use crate::CampaignProgress;

/// Wrapper Node — owns the absolute top anchor + cascading
/// Visibility for the children.
#[derive(Component)]
pub struct DifficultyMeter;

/// Tier-name text ("DRIZZLE" / "RAIN" / "HAHAHA"), painted with
/// the active tier's colour.
#[derive(Component)]
pub struct DifficultyMeterTier;

/// Dim overlay anchored to the RIGHT of the track. Its width is
/// rewritten each frame: `(1 - fill_pct) * 100` so the colored
/// segments behind it are revealed left-to-right.
#[derive(Component)]
pub struct DifficultyMeterDim;

/// Numeric `×N.NN` readout — right of the bar.
#[derive(Component)]
pub struct DifficultyMeterCoeff;

/// Stages-cleared cap for the bar's fill axis — matches the
/// internal cap in `balance::wave_size`. The HAHAHA segment fully
/// reveals when the player hits this many cleared stages.
const BATTLES_CAP: u32 = 12;
/// Bar dimensions. Same height as the player HP track for visual
/// kinship; width chosen so the segments are wide enough to read
/// as distinct bands without crowding the surrounding labels.
const BAR_W: f32 = 168.0;
const BAR_H: f32 = 22.0;

/// Seven tier colours, painted as flat horizontal bands inside
/// the track. Order matches [`tier_for`].
const TIER_COLORS: [Color; 7] = [
    Color::srgb(0.45, 0.85, 0.65), // DRIZZLE   — calm teal-green
    Color::srgb(0.55, 0.90, 0.40), // SHOWERS   — fresh lime
    Color::srgb(0.95, 0.88, 0.30), // RAIN      — caution yellow
    Color::srgb(0.98, 0.65, 0.25), // STORM     — amber
    Color::srgb(0.98, 0.40, 0.20), // HURRICANE — hot orange
    Color::srgb(0.98, 0.25, 0.25), // MONSOON   — danger red
    Color::srgb(0.95, 0.15, 0.45), // HAHAHA    — final magenta
];

pub fn setup_difficulty_meter(
    mut commands: Commands,
    thaleah: Res<crate::fonts::ThaleahFont>,
) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(0.0),
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                column_gap: Val::Px(theme::GAP_MD),
                ..default()
            },
            ZIndex(41),
            Visibility::Hidden,
            DifficultyMeter,
        ))
        .with_children(|p| {
            // ----- Tier name (left) -----
            p.spawn((
                Node {
                    min_width: Val::Px(92.0),
                    justify_content: JustifyContent::FlexEnd,
                    ..default()
                },
            ))
            .with_children(|cell| {
                cell.spawn((
                    Text::new("DRIZZLE"),
                    crate::fonts::thaleah_text_font(&thaleah, 16.0),
                    TextColor(TIER_COLORS[0]),
                    TextShadow {
                        offset: Vec2::splat(1.0),
                        color: Color::srgba(0.0, 0.0, 0.0, 0.95),
                    },
                    DifficultyMeterTier,
                ));
            });

            // ----- Track (chunky-bordered bar, same style as HP) -----
            p.spawn((
                Node {
                    width: Val::Px(BAR_W),
                    height: Val::Px(BAR_H),
                    border: UiRect::all(Val::Px(theme::CHUNKY_BORDER_W)),
                    position_type: PositionType::Relative,
                    flex_direction: FlexDirection::Row,
                    overflow: Overflow::clip(),
                    ..default()
                },
                BackgroundColor(theme::BORDER_SUBTLE),
                BorderColor(theme::BORDER_DARK),
            ))
            .with_children(|track| {
                // Seven flat colour bands — equal share of the
                // track width. No gradient between them: the hard
                // transitions ARE the visual.
                let share = 100.0 / TIER_COLORS.len() as f32;
                for tier_color in TIER_COLORS {
                    track.spawn((
                        Node {
                            width: Val::Percent(share),
                            height: Val::Percent(100.0),
                            ..default()
                        },
                        BackgroundColor(tier_color),
                    ));
                }
                // Dim overlay — anchored right, width recedes as
                // the coefficient climbs. Spawned LAST so bevy_ui
                // paints it on top of the colour bands.
                track.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        top: Val::Px(0.0),
                        right: Val::Px(0.0),
                        width: Val::Percent(100.0),
                        height: Val::Percent(100.0),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.04, 0.05, 0.08, 0.88)),
                    DifficultyMeterDim,
                ));
            });

            // ----- Coefficient (right) -----
            p.spawn((
                Node {
                    min_width: Val::Px(54.0),
                    ..default()
                },
            ))
            .with_children(|cell| {
                cell.spawn((
                    Text::new("\u{00D7}1.00"),
                    crate::fonts::thaleah_text_font(&thaleah, 16.0),
                    TextColor(Color::srgb(1.0, 0.85, 0.30)),
                    TextShadow {
                        offset: Vec2::splat(1.0),
                        color: Color::srgba(0.0, 0.0, 0.0, 0.95),
                    },
                    DifficultyMeterCoeff,
                ));
            });
        });
}

/// Per-frame: anchor the meter above the play area in the
/// letterbox band, toggle visibility on Map state + Map view, and
/// rewrite tier label / dim-overlay width / coefficient text.
pub fn update_difficulty_meter(
    state: Res<State<crate::AppState>>,
    view: Res<ViewMode>,
    progress: Res<CampaignProgress>,
    windows: Query<&Window, With<PrimaryWindow>>,
    ui_scale: Res<bevy::ui::UiScale>,
    mut root_q: Query<(&mut Visibility, &mut Node), With<DifficultyMeter>>,
    mut tier_q: Query<&mut Text,
        (With<DifficultyMeterTier>, Without<DifficultyMeterCoeff>)>,
    mut tier_color_q: Query<&mut TextColor,
        (With<DifficultyMeterTier>, Without<DifficultyMeterCoeff>)>,
    mut coeff_q: Query<&mut Text,
        (With<DifficultyMeterCoeff>, Without<DifficultyMeterTier>)>,
    mut dim_q: Query<&mut Node,
        (With<DifficultyMeterDim>, Without<DifficultyMeter>)>,
) {
    let s = *state.get();
    let want_vis = if matches!(s, crate::AppState::Map) && *view == ViewMode::Map {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };

    // Anchor above the play area's top edge. `MapHint` sits at
    // `top / ui_s - 28.0`; the chunky bar needs more clearance
    // than the old 14-px strip, so pull it higher.
    let ui_s = ui_scale.0.max(0.0001);
    let anchor_top = windows
        .single()
        .ok()
        .map(|w| {
            let (_l, top, _pw, _ph) = play_area_screen_rect(w.width(), w.height());
            (top / ui_s - 60.0).max(2.0)
        })
        .unwrap_or(2.0);

    for (mut v, mut node) in &mut root_q {
        if *v != want_vis { *v = want_vis; }
        let want_top = Val::Px(anchor_top);
        if node.top != want_top { node.top = want_top; }
    }

    if !matches!(want_vis, Visibility::Inherited) { return; }

    let cleared = progress.battles_cleared;
    let coeff = difficulty_coefficient(cleared);
    let (tier_name, tier_idx) = tier_for(cleared);
    let tier_color = TIER_COLORS[tier_idx];
    let pct = (cleared.min(BATTLES_CAP) as f32 / BATTLES_CAP as f32) * 100.0;
    let dim_pct = (100.0 - pct).clamp(0.0, 100.0);

    for mut t in &mut tier_q {
        if t.0 != tier_name { t.0 = tier_name.to_string(); }
    }
    for mut c in &mut tier_color_q {
        if c.0 != tier_color { c.0 = tier_color; }
    }
    let coeff_label = format!("\u{00D7}{:.2}", coeff);
    for mut t in &mut coeff_q {
        if t.0 != coeff_label { t.0 = coeff_label.clone(); }
    }
    let want_dim_w = Val::Percent(dim_pct);
    for mut node in &mut dim_q {
        if node.width != want_dim_w { node.width = want_dim_w; }
    }
}

/// Wave-size multiplier from `balance::wave_size`. Pure function
/// of stages cleared so the meter and the spawn code share one
/// source of truth — flipping the rate here would mean changing
/// `balance::wave_size` to match.
pub fn difficulty_coefficient(battles_cleared: u32) -> f32 {
    1.0 + 0.22 * (battles_cleared.min(BATTLES_CAP) as f32)
}

/// Map stages-cleared to a tier name + an index into [`TIER_COLORS`].
/// Twelve stages spread evenly across seven bands (≈1.7 stages per
/// tier): 0=DRIZZLE, 1-2=SHOWERS, 3-4=RAIN, 5-6=STORM, 7-8=HURRICANE,
/// 9-10=MONSOON, 11+=HAHAHA.
fn tier_for(battles_cleared: u32) -> (&'static str, usize) {
    match battles_cleared {
        0       => ("DRIZZLE",   0),
        1..=2   => ("SHOWERS",   1),
        3..=4   => ("RAIN",      2),
        5..=6   => ("STORM",     3),
        7..=8   => ("HURRICANE", 4),
        9..=10  => ("MONSOON",   5),
        _       => ("HAHAHA",    6),
    }
}
