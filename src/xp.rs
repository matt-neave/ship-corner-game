//! Player XP / level-up system.
//!
//! Three pieces:
//! - `Xp { current, level }` resource — accumulates XP from kills.
//! - `LevelUpsPending(u32)` resource — queue of level-ups owed to the
//!   player but not yet "spent" via the level-up screen.
//! - XP bar HUD across the top of the play area + a `LevelUp` overlay
//!   showing 4 buff cards.
//!
//! Granting flow: `enemy_death_check` calls `Xp::grant_kill` (1 / 5 XP
//! depending on whether the slain entity reads as a normal enemy or a
//! boss). Threshold crossings increment `LevelUpsPending`. The state
//! machine drains the queue between StageComplete → Customize.

use bevy::prelude::*;
use bevy::text::FontSmoothing;
use bevy::window::PrimaryWindow;
use rand::seq::SliceRandom;

use crate::balance::PLAY_INTERNAL;
use crate::map::ViewMode;
use crate::modes::{effective_ui_width, play_area_screen_rect, WindowMode};
use crate::stats::{PlayerStats, StatKind};
use crate::ui_kit::{self, theme};

// ---------- Resources ----------

/// Single source of truth for the player's XP + level. `current` is XP
/// accumulated toward the next level (always `< xp_to_next(level)`);
/// `level` starts at 1 and goes up indefinitely.
#[derive(Resource, Debug)]
pub struct Xp {
    pub current: u32,
    pub level: u32,
}

impl Default for Xp {
    fn default() -> Self {
        Self { current: 0, level: 1 }
    }
}

impl Xp {
    /// Reset to fresh-run baseline. Used by main-menu / restart resets.
    pub fn reset(&mut self) {
        self.current = 0;
        self.level = 1;
    }

    /// Grant `amount` XP, queuing one level-up per threshold crossed.
    /// Multiple crossings in one call all queue (e.g., a boss kill that
    /// pushes through two thresholds increments `pending` twice).
    pub fn grant(&mut self, amount: u32, pending: &mut LevelUpsPending) {
        self.current = self.current.saturating_add(amount);
        loop {
            let need = xp_to_next(self.level);
            if self.current < need { break; }
            self.current -= need;
            self.level = self.level.saturating_add(1);
            pending.0 = pending.0.saturating_add(1);
        }
    }
}

/// XP threshold to advance from `level` to `level + 1`. Linear ramp:
/// level 1→2 = 15 XP, 2→3 = 20 XP, etc.
pub fn xp_to_next(level: u32) -> u32 {
    10 + level * 5
}

/// Number of level-ups the player has earned but not yet spent on a
/// buff. Decremented each time the level-up screen finishes.
#[derive(Resource, Default, Debug)]
pub struct LevelUpsPending(pub u32);

// ---------- Buff catalog ----------

/// One offerable buff. Mutates a single `Stat` field on `PlayerStats`.
#[derive(Clone, Copy, Debug)]
pub struct Buff {
    pub kind: StatKind,
    pub delta: f32,
    /// Apply to `flat` (true) or `percent` (false).
    pub flat: bool,
}

impl Buff {
    /// Card label, e.g. `+20 HP`, `+10% RANGE`.
    pub fn label(&self) -> String {
        let unit_pct = !self.flat;
        // Pick a sensible decimal precision per stat.
        let int_like = matches!(
            self.kind,
            StatKind::Hp
                | StatKind::MoveSpeed
                | StatKind::TurnSpeed
                | StatKind::ShieldMax
                | StatKind::Crit
                | StatKind::Luck
                | StatKind::ProcStrength
                | StatKind::Range
        );
        let value = if int_like {
            format!("{:+.0}", self.delta)
        } else {
            format!("{:+.1}", self.delta)
        };
        if unit_pct {
            format!("{}% {}", value, self.kind.label())
        } else {
            format!("{} {}", value, self.kind.label())
        }
    }

    pub fn apply(&self, stats: &mut PlayerStats) {
        let s = self.kind.stat_mut(stats);
        if self.flat {
            s.flat += self.delta;
        } else {
            s.percent += self.delta / 100.0;
        }
    }
}

/// Master list of buffs the level-up screen can roll. Mix of flat +
/// percent on stats that meaningfully scale.
fn buff_pool() -> Vec<Buff> {
    vec![
        Buff { kind: StatKind::Hp,                delta: 20.0, flat: true },
        Buff { kind: StatKind::MoveSpeed,         delta: 5.0,  flat: true },
        Buff { kind: StatKind::TurnSpeed,         delta: 1.0,  flat: true },
        Buff { kind: StatKind::Range,             delta: 10.0, flat: false },
        Buff { kind: StatKind::ShieldMax,         delta: 25.0, flat: true },
        Buff { kind: StatKind::Crit,              delta: 5.0,  flat: true },
        Buff { kind: StatKind::RuneDamage,        delta: 25.0, flat: false },
        Buff { kind: StatKind::Luck,              delta: 10.0, flat: true },
        Buff { kind: StatKind::ProcStrength,      delta: 20.0, flat: true },
    ]
}

/// Pick `n` distinct buffs from the master pool. Order randomized.
fn pick_buffs(n: usize) -> Vec<Buff> {
    let mut pool = buff_pool();
    let mut rng = rand::thread_rng();
    pool.shuffle(&mut rng);
    pool.into_iter().take(n).collect()
}

// ---------- XP grant ----------

/// Per-kill XP value. Call from `enemy_death_check`. `is_boss` triggers
/// the 5x boss bonus.
pub fn grant_kill_xp(xp: &mut Xp, pending: &mut LevelUpsPending, is_boss: bool) {
    let amount = if is_boss { 5 } else { 1 };
    xp.grant(amount, pending);
}

// ---------- HUD: XP bar across the top of the play area ----------

#[derive(Component)]
pub struct XpBarRoot;

#[derive(Component)]
pub struct XpBarFill;

#[derive(Component)]
pub struct XpBarLabel;

/// XP bar dimensions — chosen to match the HP bar (`WaveHpTrack`)
/// exactly so both rails read as the same UI family. Positioned in
/// the play-area's top-LEFT corner, stacked above the HP bar.
pub const XP_BAR_WIDTH: f32 = 180.0;
pub const XP_BAR_HEIGHT: f32 = 22.0;
/// Pixels from the play-area top edge to the XP bar's top. Leaves a
/// tiny clearance off the 1-game-pixel frame border.
pub const XP_BAR_TOP_INSET: f32 = 6.0;
const XP_BAR_FILL_COLOR: Color = Color::srgb(1.0, 0.78, 0.20);

pub fn setup_xp_bar(mut commands: Commands) {
    // Same dimensions + chrome as the HP bar (`WaveHpTrack`): 180×22,
    // `theme::BORDER_SUBTLE` surface, `theme::BORDER_DARK` border at
    // 2 px. HUD-camera-relative positioning so `top: 6, left: 0`
    // lands inside the play area's top-left without any screen-coord
    // math.
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(XP_BAR_TOP_INSET),
                left: Val::Px(0.0),
                width: Val::Px(XP_BAR_WIDTH),
                height: Val::Px(XP_BAR_HEIGHT),
                border: UiRect::all(Val::Px(2.0)),
                overflow: Overflow::clip(),
                ..default()
            },
            BackgroundColor(theme::BORDER_SUBTLE),
            BorderColor(theme::BORDER_DARK),
            ZIndex(40),
            Visibility::Hidden,
            XpBarRoot,
        ))
        .with_children(|root| {
            root.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Px(0.0),
                    left: Val::Px(0.0),
                    bottom: Val::Px(0.0),
                    width: Val::Percent(0.0),
                    ..default()
                },
                BackgroundColor(XP_BAR_FILL_COLOR),
                XpBarFill,
            ));
            // "LV N" text inset inside the bar, left-aligned. Mirror
            // of the HP bar's right-aligned numeric overlay. High
            // ZIndex so the gold fill behind it doesn't drown it
            // out as the bar fills up.
            root.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Px(0.0),
                    left: Val::Px(0.0),
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    justify_content: JustifyContent::FlexStart,
                    align_items: AlignItems::Center,
                    padding: UiRect::left(Val::Px(6.0)),
                    ..default()
                },
                ZIndex(10),
            ))
            .with_children(|over| {
                over.spawn((
                    Text::new("LV 1"),
                    TextFont {
                        font_size: 10.0,
                        font_smoothing: FontSmoothing::None,
                        ..default()
                    },
                    TextColor(theme::ON_SURFACE),
                    XpBarLabel,
                ));
            });
        });
}

pub fn update_xp_bar(
    state: Res<State<crate::AppState>>,
    view: Res<ViewMode>,
    xp: Res<Xp>,
    windows: Query<&Window, With<PrimaryWindow>>,
    window_mode: Res<WindowMode>,
    mut roots: Query<(&mut Visibility, &mut Node), With<XpBarRoot>>,
    mut fills: Query<&mut Node, (With<XpBarFill>, Without<XpBarRoot>)>,
    mut labels: Query<&mut Text, With<XpBarLabel>>,
) {
    let s = *state.get();
    let want_vis = if matches!(s, crate::AppState::Playing | crate::AppState::StageComplete)
        && *view == ViewMode::Combat
    {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };

    let need = xp_to_next(xp.level).max(1);
    let pct = (xp.current as f32 / need as f32).clamp(0.0, 1.0) * 100.0;

    // Anchor the root to the play area's top-left every frame, the
    // same way `update_hp_bar_pixel_scale` does — bevy_ui's default
    // UI camera covers the whole window, so without this the bar
    // would render in the letterbox above the play area instead of
    // inside it.
    let (anchor_left, anchor_top) = windows
        .single()
        .ok()
        .map(|w| {
            let (left, top, size) =
                play_area_screen_rect(w.width(), w.height(), effective_ui_width(&window_mode));
            let upscale = (size / PLAY_INTERNAL as f32).max(1.0);
            let margin = upscale * 4.0;
            (left + margin, top + margin)
        })
        .unwrap_or((0.0, 0.0));

    for (mut v, mut node) in &mut roots {
        if *v != want_vis { *v = want_vis; }
        let want_top = Val::Px(anchor_top);
        let want_left = Val::Px(anchor_left);
        if node.top != want_top { node.top = want_top; }
        if node.left != want_left { node.left = want_left; }
    }
    for mut node in &mut fills {
        let want = Val::Percent(pct);
        if node.width != want { node.width = want; }
    }
    let label_text = format!("LV {}", xp.level);
    for mut t in &mut labels {
        if t.0 != label_text { t.0 = label_text.clone(); }
    }
}

// ---------- Level-up screen ----------

/// Root marker for the level-up overlay. Despawned wholesale on
/// `OnExit(LevelUp)`.
#[derive(Component)]
pub struct LevelUpRoot;

/// Marker on each buff card button. Index maps to `LevelUpChoices.buffs`.
#[derive(Component, Clone, Copy)]
pub struct LevelUpButton {
    pub idx: usize,
}

/// The current set of 4 buffs offered for this level-up. Re-rolled on
/// each `OnEnter(LevelUp)`.
#[derive(Resource, Default)]
pub struct LevelUpChoices {
    pub buffs: Vec<Buff>,
}

pub fn enter_level_up(
    mut commands: Commands,
    mut choices: ResMut<LevelUpChoices>,
    xp: Res<Xp>,
    stats: Res<PlayerStats>,
) {
    choices.buffs = pick_buffs(4);
    spawn_level_up_overlay(&mut commands, &choices, &xp, &stats);
}

pub fn exit_level_up(mut commands: Commands, q: Query<Entity, With<LevelUpRoot>>) {
    for e in &q {
        commands.entity(e).despawn();
    }
}

/// Click handler. Applies the chosen buff to `PlayerStats`, decrements
/// the queue, and either:
/// - More pending: tears down the overlay and spawns a fresh one
///   in-place with rerolled buffs. We can't `set(LevelUp)` to re-fire
///   `OnEnter` since Bevy treats same-state set as a no-op.
/// - Queue empty: transitions to `Customize`.
pub fn handle_level_up_click(
    mut commands: Commands,
    interactions: Query<(&Interaction, &LevelUpButton), Changed<Interaction>>,
    mut choices: ResMut<LevelUpChoices>,
    mut stats: ResMut<PlayerStats>,
    mut pending: ResMut<LevelUpsPending>,
    xp: Res<Xp>,
    mut next: ResMut<NextState<crate::AppState>>,
    overlay: Query<Entity, With<LevelUpRoot>>,
) {
    for (interaction, btn) in &interactions {
        if !matches!(*interaction, Interaction::Pressed) { continue; }
        let Some(buff) = choices.buffs.get(btn.idx).copied() else { continue };
        buff.apply(&mut stats);
        pending.0 = pending.0.saturating_sub(1);
        if pending.0 > 0 {
            // Reroll in-place: despawn current overlay + rebuild.
            for e in &overlay {
                commands.entity(e).despawn();
            }
            choices.buffs = pick_buffs(4);
            spawn_level_up_overlay(&mut commands, &choices, &xp, &stats);
        } else {
            next.set(crate::AppState::Customize);
        }
        return;
    }
}

/// Build the overlay tree. Shared between `enter_level_up` and the
/// click-handler reroll path so the layout stays in one place.
fn spawn_level_up_overlay(
    commands: &mut Commands,
    choices: &LevelUpChoices,
    xp: &Xp,
    stats: &PlayerStats,
) {
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
            // Semi-transparent so the play area shows through.
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.55)),
            // Below customize (200) and pause (180); above gameplay HUD.
            ZIndex(160),
            Visibility::Inherited,
            // Absorb clicks so they don't fall through to gameplay UI.
            Button,
            LevelUpRoot,
        ))
        .with_children(|root| {
            root.spawn(ui_kit::label(
                format!("LEVEL {}!", xp.level),
                theme::FONT_LG * 1.8,
                theme::ACCENT,
            ));

            // Main row: buff cards on the left, current-stats panel
            // on the right. Two-column layout so the player can see
            // their current numbers while choosing the buff.
            root.spawn((
                Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::FlexStart,
                    column_gap: Val::Px(theme::GAP_LG * 1.5),
                    ..default()
                },
                BackgroundColor(Color::NONE),
            ))
            .with_children(|cols| {
                // ---- LEFT: buff cards ----
                cols.spawn((
                    Node {
                        flex_direction: FlexDirection::Row,
                        align_items: AlignItems::Stretch,
                        column_gap: Val::Px(theme::GAP_LG),
                        ..default()
                    },
                    BackgroundColor(Color::NONE),
                ))
                .with_children(|row| {
                    for (i, buff) in choices.buffs.iter().enumerate() {
                        row.spawn((
                            Button,
                            Node {
                                width: Val::Px(120.0),
                                height: Val::Px(80.0),
                                border: UiRect::all(Val::Px(theme::BORDER_W)),
                                padding: UiRect::all(Val::Px(theme::PAD_MD)),
                                flex_direction: FlexDirection::Column,
                                align_items: AlignItems::Center,
                                justify_content: JustifyContent::Center,
                                ..default()
                            },
                            BackgroundColor(theme::SURFACE_RAISED),
                            BorderColor(theme::ACCENT),
                            LevelUpButton { idx: i },
                        ))
                        .with_children(|card| {
                            card.spawn(ui_kit::label(
                                buff.label(),
                                theme::FONT_LG,
                                theme::ON_SURFACE,
                            ));
                        });
                    }
                });

                // ---- RIGHT: current-stats panel ----
                cols.spawn((
                    Node {
                        width: Val::Px(180.0),
                        padding: UiRect::all(Val::Px(theme::PAD_MD)),
                        flex_direction: FlexDirection::Column,
                        row_gap: Val::Px(theme::GAP_SM),
                        ..default()
                    },
                    BackgroundColor(theme::SURFACE_RAISED),
                    BorderColor(theme::BORDER_SUBTLE),
                ))
                .with_children(|panel| {
                    panel.spawn(ui_kit::label(
                        "CURRENT STATS",
                        theme::FONT_MD,
                        theme::ACCENT,
                    ));
                    for kind in crate::stats::StatKind::ALL {
                        panel
                            .spawn((
                                Node {
                                    flex_direction: FlexDirection::Row,
                                    justify_content: JustifyContent::SpaceBetween,
                                    column_gap: Val::Px(theme::GAP_MD),
                                    ..default()
                                },
                                BackgroundColor(Color::NONE),
                            ))
                            .with_children(|stat_row| {
                                stat_row.spawn(ui_kit::label(
                                    kind.label(),
                                    theme::FONT_SM,
                                    theme::ON_SURFACE_DIM,
                                ));
                                stat_row.spawn(ui_kit::label(
                                    kind.format_value(stats),
                                    theme::FONT_SM,
                                    theme::ON_SURFACE,
                                ));
                            });
                    }
                });
            });
        });
}
