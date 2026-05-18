//! Win screen — end-of-run summary shown when the player defeats a
//! 5★ section boss. Mirrors the stage-complete payout treatment:
//! translucent dark wash over the play world, a wave-bobbing
//! "VICTORY" title, a summary card with LEVEL / SCRAP / TIME rows,
//! staggered row reveal with a white-flash punch on each value,
//! and drop shadows on every glyph. MAIN MENU / QUIT buttons sit
//! beneath the card.
//!
//! `level_complete_check` (in `map::buildings`) is the only path
//! into this state — when the cleared section's `stars == 5`, the
//! transition routes here instead of `StageComplete`, ending the
//! run before the shop/map cycle would otherwise resume.

use bevy::app::AppExit;
use bevy::prelude::*;
use bevy::text::FontSmoothing;

use crate::ui_kit::{self, theme};
use crate::AppState;

pub struct WinScreenPlugin;

impl Plugin for WinScreenPlugin {
    fn build(&self, app: &mut App) {
        app
            .insert_resource(WinTimer::default())
            .add_systems(OnEnter(AppState::Win), enter_win)
            .add_systems(
                OnExit(AppState::Win),
                (exit_win, crate::game_over::reset_run_for_restart),
            )
            .add_systems(
                Update,
                (
                    tick_win_timer,
                    tick_win_wave,
                    tick_win_payout_reveal,
                    handle_main_menu_click,
                    handle_quit_click,
                )
                    .run_if(in_state(AppState::Win)),
            );
    }
}

/// Wavey title — per-character bob amplitude (px).
const WAVE_AMP: f32 = 10.0;
/// Wavey title — angular frequency of the bob (rad/s).
const WAVE_SPEED: f32 = 5.0;
/// Wavey title — phase offset between adjacent characters (rad).
const WAVE_PHASE_PER_CHAR: f32 = 0.45;

/// Delay before the first summary row reveals.
const PAYOUT_FIRST_DELAY: f32 = 0.25;
/// Gap between successive rows popping in.
const PAYOUT_LINE_GAP: f32 = 0.22;
/// Duration of the white-flash punch on each row as it reveals.
const PAYOUT_FLASH_DURATION: f32 = 0.18;
const PAYOUT_FLASH_COLOR: Color = Color::WHITE;

/// Elapsed time since the win screen opened. Drives the per-char
/// wave + the staggered row reveal.
#[derive(Resource, Default)]
pub struct WinTimer(pub f32);

#[derive(Component)]
pub struct WinRoot;

#[derive(Component)]
pub struct WinMainMenuButton;

#[derive(Component)]
pub struct WinQuitButton;

#[derive(Component)]
pub struct WinWaveChar { pub idx: usize }

#[derive(Component)]
pub struct WinPayoutLine { pub idx: u8 }

#[derive(Component)]
pub struct WinPayoutValue { pub idx: u8, pub base_color: Color }

pub fn enter_win(
    mut commands: Commands,
    mut timer: ResMut<WinTimer>,
    mut sfx: crate::sfx::SfxPlayer,
    scrap: Res<crate::Scrap>,
    xp: Res<crate::xp::Xp>,
    run_timer: Res<crate::RunTimer>,
    pixel: Option<Res<crate::fonts::PixelFont>>,
    thaleah: Option<Res<crate::fonts::ThaleahFont>>,
    cfg: Res<crate::turret::TurretConfig>,
    purchased: Res<crate::customize::drag::PurchasedMods>,
) {
    sfx.play(crate::sfx::Sfx::Victory);
    timer.0 = 0.0;

    let value_color = Color::srgb(1.0, 0.88, 0.40);
    let accent      = theme::ACCENT;
    let label_color = Color::srgb(0.92, 0.93, 0.96);

    // Three summary rows. TIME formatted as M:SS.
    let total_secs = run_timer.secs.max(0.0) as u32;
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    let row_specs: [(String, String, Color); 3] = [
        ("LEVEL".to_string(), format!("Lv {}", xp.level), value_color),
        ("SCRAP".to_string(), format!("{}", scrap.0),    value_color),
        ("TIME".to_string(),  format!("{}:{:02}", mins, secs), accent),
    ];

    let drop_shadow = TextShadow {
        offset: Vec2::splat(2.0),
        color: Color::srgba(0.0, 0.0, 0.0, 0.85),
    };

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
            // Translucent dark wash so the underlying play world
            // shows through, same treatment as the stage-complete
            // overlay (vs. the previous opaque shop-backdrop fill).
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.55)),
            ZIndex(190),
            Visibility::Inherited,
            WinRoot,
            Button,
        ))
        .with_children(|root| {
            // Per-character bobbing "VICTORY" title. Each glyph
            // lives on its own Node so `tick_win_wave` can write
            // an independent `top` offset.
            root.spawn(Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                ..default()
            })
            .with_children(|row| {
                for (i, ch) in "VICTORY".chars().enumerate() {
                    let s = if ch == ' ' { "\u{00A0}".to_string() } else { ch.to_string() };
                    let title_font = if let Some(t) = thaleah.as_deref() {
                        crate::fonts::thaleah_text_font(t, 48.0)
                    } else {
                        TextFont {
                            font_size: 48.0,
                            font_smoothing: FontSmoothing::None,
                            ..default()
                        }
                    };
                    row.spawn((
                        Text::new(s),
                        title_font,
                        TextColor(accent),
                        drop_shadow,
                        Node {
                            position_type: PositionType::Relative,
                            ..default()
                        },
                        WinWaveChar { idx: i },
                    ));
                }
            });

            // Summary card — same shape as the stage-complete
            // payout card: solid surface tone, accent border,
            // chunky rounded corners, header row above the three
            // staggered-reveal stat rows.
            root.spawn((
                Node {
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Stretch,
                    justify_content: JustifyContent::Center,
                    padding: UiRect::axes(
                        Val::Px(theme::PAD_LG),
                        Val::Px(theme::PAD_MD),
                    ),
                    border: UiRect::all(Val::Px(theme::CHUNKY_BORDER_W)),
                    row_gap: Val::Px(4.0),
                    min_width: Val::Px(240.0),
                    ..default()
                },
                BackgroundColor(theme::SURFACE_RAISED),
                BorderColor(theme::ACCENT),
                BorderRadius::all(Val::Px(theme::CHUNKY_RADIUS)),
            ))
            .with_children(|card| {
                card.spawn(Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Baseline,
                    justify_content: JustifyContent::SpaceBetween,
                    column_gap: Val::Px(theme::GAP_LG),
                    margin: UiRect::bottom(Val::Px(4.0)),
                    ..default()
                })
                .with_children(|h| {
                    let header_font = if let Some(p) = pixel.as_deref() {
                        crate::fonts::pixel_text_font(p, 11.0)
                    } else {
                        TextFont { font_size: 11.0, font_smoothing: FontSmoothing::None, ..default() }
                    };
                    h.spawn((
                        Text::new("RUN SUMMARY"),
                        header_font,
                        TextColor(theme::ON_SURFACE_DIM),
                    ));
                });
                for (idx, (label, value, value_base)) in row_specs.iter().enumerate() {
                    let label_font_size = 14.0;
                    let value_font_size = 20.0;
                    let label_text_font = if let Some(p) = pixel.as_deref() {
                        crate::fonts::pixel_text_font(p, label_font_size)
                    } else {
                        TextFont { font_size: label_font_size, font_smoothing: FontSmoothing::None, ..default() }
                    };
                    let value_text_font = if let Some(t) = thaleah.as_deref() {
                        crate::fonts::thaleah_text_font(t, value_font_size)
                    } else {
                        TextFont { font_size: value_font_size, font_smoothing: FontSmoothing::None, ..default() }
                    };
                    card.spawn((
                        Node {
                            flex_direction: FlexDirection::Row,
                            align_items: AlignItems::Baseline,
                            justify_content: JustifyContent::SpaceBetween,
                            column_gap: Val::Px(theme::GAP_LG),
                            ..default()
                        },
                        BackgroundColor(Color::NONE),
                        Visibility::Hidden,
                        WinPayoutLine { idx: idx as u8 },
                    ))
                    .with_children(|row| {
                        row.spawn((
                            Text::new(label.clone()),
                            label_text_font,
                            TextColor(label_color),
                            drop_shadow,
                        ));
                        row.spawn((
                            Text::new(value.clone()),
                            value_text_font,
                            TextColor(*value_base),
                            drop_shadow,
                            WinPayoutValue {
                                idx: idx as u8,
                                base_color: *value_base,
                            },
                        ));
                    });
                }
            });

            // Build summary — slots between the staggered-reveal
            // summary card and the navigation buttons so the player
            // sees their numbers AND what they built.
            crate::build_summary::spawn_build_summary(
                root,
                cfg.as_ref(),
                purchased.as_ref(),
                pixel.as_deref(),
                thaleah.as_deref(),
            );

            root.spawn((ui_kit::button(theme::SURFACE_RAISED), WinMainMenuButton))
                .with_children(|b| {
                    b.spawn(ui_kit::label("MAIN MENU", theme::FONT_LG, theme::ON_SURFACE));
                });

            root.spawn((ui_kit::button(theme::SURFACE_RAISED), WinQuitButton))
                .with_children(|b| {
                    b.spawn(ui_kit::label("QUIT", theme::FONT_LG, theme::ON_SURFACE));
                });
        });
}

pub fn tick_win_timer(time: Res<Time>, mut timer: ResMut<WinTimer>) {
    timer.0 += time.delta_secs();
}

pub fn tick_win_wave(timer: Res<WinTimer>, mut q: Query<(&WinWaveChar, &mut Node)>) {
    let t = timer.0;
    for (c, mut node) in &mut q {
        let phase = c.idx as f32 * WAVE_PHASE_PER_CHAR;
        let bob = -(t * WAVE_SPEED + phase).sin() * WAVE_AMP;
        let want = Val::Px(bob);
        if node.top != want { node.top = want; }
    }
}

pub fn tick_win_payout_reveal(
    timer: Res<WinTimer>,
    mut rows: Query<(&WinPayoutLine, &mut Visibility)>,
    mut values: Query<(&WinPayoutValue, &mut TextColor)>,
) {
    let t = timer.0;
    for (line, mut vis) in &mut rows {
        let reveal_at = PAYOUT_FIRST_DELAY + line.idx as f32 * PAYOUT_LINE_GAP;
        let want_vis = if t < reveal_at {
            Visibility::Hidden
        } else {
            Visibility::Inherited
        };
        if *vis != want_vis { *vis = want_vis; }
    }
    for (val, mut color) in &mut values {
        let reveal_at = PAYOUT_FIRST_DELAY + val.idx as f32 * PAYOUT_LINE_GAP;
        if t < reveal_at { continue; }
        let since = t - reveal_at;
        let want = if since >= PAYOUT_FLASH_DURATION {
            val.base_color
        } else {
            let k = since / PAYOUT_FLASH_DURATION;
            let k = k * k * (3.0 - 2.0 * k);
            lerp_color(PAYOUT_FLASH_COLOR, val.base_color, k)
        };
        if color.0 != want { color.0 = want; }
    }
}

fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let a: bevy::color::Srgba = a.into();
    let b: bevy::color::Srgba = b.into();
    Color::srgba(
        a.red   + (b.red   - a.red)   * t,
        a.green + (b.green - a.green) * t,
        a.blue  + (b.blue  - a.blue)  * t,
        a.alpha + (b.alpha - a.alpha) * t,
    )
}

pub fn exit_win(mut commands: Commands, q: Query<Entity, With<WinRoot>>) {
    for e in &q {
        commands.entity(e).despawn();
    }
}

pub fn handle_main_menu_click(
    interactions: Query<&Interaction, (Changed<Interaction>, With<WinMainMenuButton>)>,
    mut next: ResMut<NextState<AppState>>,
) {
    for interaction in &interactions {
        if matches!(*interaction, Interaction::Pressed) {
            next.set(AppState::MainMenu);
        }
    }
}

pub fn handle_quit_click(
    interactions: Query<&Interaction, (Changed<Interaction>, With<WinQuitButton>)>,
    mut exit: EventWriter<AppExit>,
) {
    for interaction in &interactions {
        if matches!(*interaction, Interaction::Pressed) {
            exit.write(AppExit::Success);
        }
    }
}
