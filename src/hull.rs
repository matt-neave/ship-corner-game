//! Hull-selection screen — sits between `MainMenu` and `Playing`.
//!
//! Two-pane layout:
//!   - **Left column** — vertical list of hull cards (one per
//!     `Hull` variant). Click highlights that hull as the active
//!     pick (writes `SelectedHull`); doesn't transition yet.
//!   - **Right panel** — larger detail card showing the currently
//!     selected hull's tagline, a bulleted BUFFS list (green), a
//!     bulleted NERFS list (red), and a single PLAY button at the
//!     bottom. Re-rendered on `SelectedHull` change.
//!
//! BACK button + Escape both return to `MainMenu` so a mis-PLAY is
//! cancellable. Death-RESTART re-applies the active hull without
//! re-showing the picker; only a full MainMenu round-trip prompts
//! again.

use bevy::prelude::*;

use crate::stats::PlayerStats;
use crate::ui_kit::{self, theme};

/// Which hull the player is running. Acts as both the highlighted
/// card on `HullSelect` and the locked-in pick after PLAY.
#[derive(Resource, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectedHull(pub Hull);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Hull {
    #[default]
    Default,
    GlassCannon,
    Rammer,
}

impl Hull {
    pub fn label(self) -> &'static str {
        match self {
            Hull::Default     => "STANDARD",
            Hull::GlassCannon => "GLASS CANNON",
            Hull::Rammer      => "RAMMER",
        }
    }

    /// One-line summary, shown both as the card subtitle and the
    /// detail-panel header.
    pub fn tagline(self) -> &'static str {
        match self {
            Hull::Default     => "Baseline hull. No modifiers.",
            Hull::GlassCannon => "Low HP, long range, harder-hitting runes.",
            Hull::Rammer      => "Triple HP, very short turret range.",
        }
    }

    /// Stat buffs (positive changes) — shown as green bullets in the
    /// detail panel. Empty for the baseline hull.
    pub fn buffs(self) -> &'static [&'static str] {
        match self {
            Hull::Default     => &[],
            Hull::GlassCannon => &[
                "+50% turret range",
                "+50% rune damage",
                "+10% crit chance",
            ],
            Hull::Rammer => &[
                "+200 HP",
                "+10 move speed",
            ],
        }
    }

    /// Stat nerfs (negative changes) — shown as red bullets in the
    /// detail panel.
    pub fn nerfs(self) -> &'static [&'static str] {
        match self {
            Hull::Default     => &[],
            Hull::GlassCannon => &[
                "-50 HP",
            ],
            Hull::Rammer => &[
                "-70% turret range",
            ],
        }
    }

    /// Apply this hull's modifiers to a fresh `PlayerStats`. Caller
    /// must reset stats to `default()` first — `apply` only writes
    /// the deltas, doesn't clear existing modifiers.
    pub fn apply(self, stats: &mut PlayerStats) {
        match self {
            Hull::Default => {}
            Hull::GlassCannon => {
                stats.hp.flat             = -50.0;
                stats.range_pct.flat      =  50.0;
                stats.rune_damage.percent =   0.5;
                stats.crit_pct.flat       =  10.0;
            }
            Hull::Rammer => {
                stats.hp.flat         = 200.0;
                stats.range_pct.flat  = -70.0;
                stats.move_speed.flat =  10.0;
            }
        }
    }
}

/// Iteration order of hulls in the left column.
const HULL_ORDER: [Hull; 3] = [Hull::Default, Hull::GlassCannon, Hull::Rammer];

// ---------- Font sizing ----------
//
// The hull-select overlay's text was reading as cramped; bumped
// across the board. `_FONT` constants kept local because they're
// only used by this overlay's two layout functions and should
// scale together if we want to retune.
const CARD_TITLE_FONT: f32 = theme::FONT_LG * 1.2;       // ~17 pt
const CARD_TAGLINE_FONT: f32 = theme::FONT_LG;           // 14 pt
const DETAIL_TITLE_FONT: f32 = theme::FONT_LG * 2.0;     // ~28 pt
const DETAIL_TAGLINE_FONT: f32 = theme::FONT_LG * 1.2;   // ~17 pt
const DETAIL_BULLET_FONT: f32 = theme::FONT_LG;          // 14 pt

// ---------- Markers ----------

#[derive(Component)]
pub struct HullSelectRoot;

#[derive(Component)]
pub struct HullDetailPanel;

#[derive(Component, Clone, Copy)]
pub struct HullCard(pub Hull);

#[derive(Component)]
pub struct HullPlayButton;

#[derive(Component)]
pub struct HullBackButton;

// ---------- Spawn ----------

pub fn enter_hull_select(
    commands: Commands,
    selected: Res<SelectedHull>,
) {
    spawn_overlay(commands, selected.0);
}

fn spawn_overlay(mut commands: Commands, selected: Hull) {
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
            BackgroundColor(Color::srgb(0.05, 0.05, 0.08)),
            ZIndex(150),
            Visibility::Inherited,
            HullSelectRoot,
            // Absorb clicks behind any future world overlay.
            Button,
        ))
        .with_children(|root| {
            root.spawn(ui_kit::label(
                "CHOOSE A HULL",
                theme::FONT_LG * 2.6,
                theme::ACCENT,
            ));

            // Two-column layout: left grid of hulls, right detail panel.
            root.spawn(Node {
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(theme::GAP_LG * 2.5),
                align_items: AlignItems::Stretch,
                ..default()
            })
            .with_children(|cols| {
                // ---- LEFT: hull list ----
                cols.spawn(Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(theme::GAP_LG),
                    width: Val::Px(300.0),
                    ..default()
                })
                .with_children(|list| {
                    for hull in HULL_ORDER {
                        spawn_card(list, hull, hull == selected);
                    }
                });

                // ---- RIGHT: detail panel ----
                cols.spawn((
                    Node {
                        width: Val::Px(480.0),
                        min_height: Val::Px(400.0),
                        padding: UiRect::all(Val::Px(theme::PAD_LG * 2.0)),
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::Stretch,
                        row_gap: Val::Px(theme::GAP_LG),
                        ..default()
                    },
                    BackgroundColor(theme::SURFACE_RAISED),
                    BorderColor(theme::BORDER_SUBTLE),
                    HullDetailPanel,
                ))
                .with_children(|panel| {
                    spawn_detail_content(panel, selected);
                });
            });

            // ---- BACK button under the columns ----
            root.spawn((ui_kit::button(theme::SURFACE_RAISED), HullBackButton))
                .with_children(|b| {
                    b.spawn(ui_kit::label("BACK", theme::FONT_MD, theme::ON_SURFACE_DIM));
                });
        });
}

/// Left-column card. Highlighted background when this hull is the
/// active pick; muted otherwise.
fn spawn_card(parent: &mut ChildSpawnerCommands, hull: Hull, selected: bool) {
    let bg = if selected { theme::SURFACE_HOVER } else { theme::SURFACE_RAISED };
    let border = if selected { theme::ACCENT } else { theme::BORDER_SUBTLE };
    parent
        .spawn((
            Button,
            Node {
                width: Val::Percent(100.0),
                padding: UiRect::all(Val::Px(theme::PAD_MD)),
                border: UiRect::all(Val::Px(theme::BORDER_W * 2.0)),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(theme::GAP_SM),
                ..default()
            },
            BackgroundColor(bg),
            BorderColor(border),
            HullCard(hull),
        ))
        .with_children(|card| {
            let title_color = if selected { theme::ACCENT } else { theme::ON_SURFACE };
            card.spawn(ui_kit::label(hull.label(), CARD_TITLE_FONT, title_color));
            card.spawn((
                Text::new(hull.tagline()),
                TextFont {
                    font_size: CARD_TAGLINE_FONT,
                    font_smoothing: bevy::text::FontSmoothing::None,
                    ..default()
                },
                TextColor(theme::ON_SURFACE_DIM),
            ));
        });
}

/// Right-panel content (title + tagline + buffs + nerfs + PLAY).
/// Lives inside the existing `HullDetailPanel` Node — caller is the
/// `with_children` closure of that node, so this fn spawns the
/// children directly.
fn spawn_detail_content(panel: &mut ChildSpawnerCommands, hull: Hull) {
    panel.spawn(ui_kit::label(hull.label(), DETAIL_TITLE_FONT, theme::ACCENT));
    panel.spawn((
        Text::new(hull.tagline()),
        TextFont {
            font_size: DETAIL_TAGLINE_FONT,
            font_smoothing: bevy::text::FontSmoothing::None,
            ..default()
        },
        TextColor(theme::ON_SURFACE),
    ));

    // Buffs — green, the leading `+` (already in the buff string)
    // is the only marker. No section header, no extra bullet glyph.
    for b in hull.buffs() {
        panel.spawn((
            Text::new(b.to_string()),
            TextFont {
                font_size: DETAIL_BULLET_FONT,
                font_smoothing: bevy::text::FontSmoothing::None,
                ..default()
            },
            TextColor(Color::srgb(0.55, 0.95, 0.55)),
        ));
    }

    // Nerfs — red, leading `-` from the source string.
    for n in hull.nerfs() {
        panel.spawn((
            Text::new(n.to_string()),
            TextFont {
                font_size: DETAIL_BULLET_FONT,
                font_smoothing: bevy::text::FontSmoothing::None,
                ..default()
            },
            TextColor(Color::srgb(1.00, 0.55, 0.55)),
        ));
    }

    // Spacer + PLAY button anchored bottom.
    panel.spawn(Node {
        flex_grow: 1.0,
        ..default()
    });
    panel
        .spawn((
            Button,
            Node {
                padding: UiRect::axes(Val::Px(theme::PAD_LG), Val::Px(theme::PAD_MD)),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(theme::ACCENT),
            HullPlayButton,
        ))
        .with_children(|b| {
            b.spawn(ui_kit::label(
                "PLAY",
                theme::FONT_LG,
                Color::srgb(0.05, 0.05, 0.08),
            ));
        });
}

pub fn exit_hull_select(
    mut commands: Commands,
    q: Query<Entity, With<HullSelectRoot>>,
    selected: Res<SelectedHull>,
    mut stats: ResMut<PlayerStats>,
    mut friendly: Query<&mut crate::components::Health, With<crate::components::Friendly>>,
) {
    for e in &q {
        commands.entity(e).despawn();
    }
    apply_selected_hull(&selected, &mut stats, &mut friendly);
}

/// Keep `PlayerStats` + friendly HP in sync with the active pick
/// every frame while the player is on the hull-select screen.
/// `exit_hull_select` would normally cover this on the state
/// transition, but running it every frame too makes the
/// pick-changes-immediately invariant rock-solid against any state-
/// transition timing quirks.
pub fn sync_hull_apply(
    selected: Res<SelectedHull>,
    mut stats: ResMut<PlayerStats>,
    mut friendly: Query<&mut crate::components::Health, With<crate::components::Friendly>>,
) {
    apply_selected_hull(&selected, &mut stats, &mut friendly);
}

/// Shared apply step: reset to baseline stats, layer on the chosen
/// hull's modifiers, then clamp the friendly's current HP to the
/// new max so the bar doesn't display a stale baseline (100/50 for
/// Glass Cannon, for instance).
fn apply_selected_hull(
    selected: &SelectedHull,
    stats: &mut PlayerStats,
    friendly: &mut Query<&mut crate::components::Health, With<crate::components::Friendly>>,
) {
    *stats = PlayerStats::default();
    selected.0.apply(stats);
    let new_max = stats.max_hp();
    for mut h in friendly.iter_mut() {
        h.0 = new_max;
    }
}

/// Belt-and-braces clamp: every frame, hold the friendly's `Health.0`
/// to ≤ `stats.max_hp()`. Catches any case where a stat downgrade
/// (hull pick, debug-panel HP-stat decrement) left the live HP
/// stale above the new cap, which would otherwise display as
/// `100/50`-style mismatches in the bar.
pub fn clamp_hp_to_max(
    stats: Res<PlayerStats>,
    mut friendly: Query<&mut crate::components::Health, With<crate::components::Friendly>>,
) {
    let max = stats.max_hp();
    for mut h in &mut friendly {
        if h.0 > max { h.0 = max; }
    }
}

// ---------- Click + input handlers ----------

/// Click a left-column card → set it as the active pick. Don't
/// transition yet; the player confirms with the PLAY button.
pub fn handle_card_click(
    interactions: Query<(&Interaction, &HullCard), Changed<Interaction>>,
    mut selected: ResMut<SelectedHull>,
) {
    for (interaction, card) in &interactions {
        if !matches!(*interaction, Interaction::Pressed) { continue; }
        if selected.0 != card.0 {
            selected.0 = card.0;
        }
    }
}

/// PLAY button → transition to Playing. Stat application happens in
/// `exit_hull_select` so PLAY / BACK / ESC paths all funnel through
/// one finaliser.
pub fn handle_play_click(
    interactions: Query<&Interaction, (Changed<Interaction>, With<HullPlayButton>)>,
    mut next: ResMut<NextState<crate::AppState>>,
) {
    for interaction in &interactions {
        if matches!(*interaction, Interaction::Pressed) {
            next.set(crate::AppState::Playing);
            return;
        }
    }
}

/// BACK button (or ESC) → bounce to MainMenu without committing.
pub fn handle_back_click(
    interactions: Query<&Interaction, (Changed<Interaction>, With<HullBackButton>)>,
    mut next: ResMut<NextState<crate::AppState>>,
) {
    for interaction in &interactions {
        if matches!(*interaction, Interaction::Pressed) {
            next.set(crate::AppState::MainMenu);
        }
    }
}

/// ESC = same as clicking BACK.
pub fn handle_back_on_esc(
    keys: Res<ButtonInput<KeyCode>>,
    mut next: ResMut<NextState<crate::AppState>>,
) {
    if keys.just_pressed(KeyCode::Escape) {
        next.set(crate::AppState::MainMenu);
    }
}

/// Rebuild the overlay whenever `SelectedHull` changes — keeps the
/// card highlights + right-panel content in sync without per-text
/// query plumbing. The overlay is small so the despawn/respawn cost
/// is fine.
pub fn sync_hull_select_on_change(
    selected: Res<SelectedHull>,
    commands: Commands,
    q: Query<Entity, With<HullSelectRoot>>,
    state: Res<State<crate::AppState>>,
) {
    if !selected.is_changed() { return; }
    if *state.get() != crate::AppState::HullSelect { return; }
    let mut commands = commands;
    for e in &q {
        commands.entity(e).despawn();
    }
    spawn_overlay(commands, selected.0);
}
