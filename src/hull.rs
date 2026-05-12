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
use crate::AppState;

/// Owns the hull-select / dockyard screen: the two pick-state
/// resources, the persistent dockyard render-target plumbing (set up
/// once at startup), the spawn/despawn lifecycle for the pixel scene
/// + UI overlay, every state-gated input handler for both the cards
/// and the pixel-scene berths, and the always-on `clamp_hp_to_max`
/// guard (which runs everywhere — a stat downgrade outside this
/// screen still shouldn't leave a stale HP readout).
pub struct HullSelectPlugin;

impl Plugin for HullSelectPlugin {
    fn build(&self, app: &mut App) {
        app
            .insert_resource(SelectedHull::default())
            .insert_resource(PreviewHull::default())
            // Render-target plumbing for the dockyard pixel scene
            // (camera + image + backdrop + display sprite) is created
            // once at startup and kept warm. The scene contents are
            // spawned/despawned in the OnEnter/OnExit hooks below.
            .add_systems(Startup, crate::dockyard_view::setup_dockyard_render)
            .add_systems(
                OnEnter(AppState::HullSelect),
                (
                    enter_hull_select,
                    crate::reset_run_timer,
                    crate::dockyard_view::spawn_dockyard_scene,
                    crate::dockyard_view::spawn_dockyard_labels,
                ),
            )
            // OnExit(HullSelect) chain: tear down the dockyard UI,
            // regenerate the map with the player's chosen `MapSize`,
            // then re-run the map-view setup so the new topology has
            // its visuals. Chained so the new `MapState` is in place
            // before `setup_map` reads it.
            .add_systems(
                OnExit(AppState::HullSelect),
                (
                    exit_hull_select,
                    crate::dockyard_view::despawn_dockyard_scene,
                    crate::map::regenerate_map,
                    crate::map::setup_map,
                    crate::map::spawn_boss_patrols,
                ).chain(),
            )
            .add_systems(
                Update,
                (
                    handle_card_click,
                    handle_map_size_click,
                    handle_play_click,
                    handle_back_click,
                    handle_back_on_esc,
                    sync_hull_select_on_change,
                    sync_hull_apply,
                    // Dockyard pixel-scene driving — hover preview,
                    // click commit, per-frame highlight + label
                    // positioning.
                    crate::dockyard_view::handle_dockyard_hover,
                    crate::dockyard_view::handle_dockyard_click,
                    crate::dockyard_view::update_dockyard_highlight,
                    crate::dockyard_view::update_dockyard_labels,
                ).run_if(in_state(AppState::HullSelect)),
            )
            // Always-on: clamp HP to max each frame so a stat change
            // never leaves a "100/50" readout. Lives with hull-select
            // because the hull is what defines max HP, but applies
            // everywhere.
            .add_systems(Update, clamp_hp_to_max)
            // Dockyard render-target activation + display-sprite
            // sizing — both self-gate, so cheap to leave always-on.
            .add_systems(
                Update,
                (
                    crate::dockyard_view::toggle_dockyard_render,
                    crate::dockyard_view::resize_dockyard_display,
                ),
            );
    }
}

/// Which hull the player is running. Acts as both the highlighted
/// card on `HullSelect` and the locked-in pick after PLAY. Committed
/// by clicking a berth card; hovering only updates `PreviewHull`.
#[derive(Resource, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectedHull(pub Hull);

/// Transient hover preview — the hull currently under the cursor on
/// the dockyard, or `None` if the cursor isn't over any card. Drives
/// the right-side detail panel ONLY; the actual selection
/// (`SelectedHull`) only changes on click. Defaulting to `None`
/// keeps the panel showing `SelectedHull` until the player hovers
/// something new.
#[derive(Resource, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreviewHull(pub Option<Hull>);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Hull {
    #[default]
    Default,
    GlassCannon,
    Rammer,
    /// Slow heavy with massive HP + shield. Wide turret arcs, glacial
    /// movement, short range.
    Dreadnought,
    /// Pirate raider — scrap-magnet glass cannon. High crit + harvest,
    /// thin hull.
    Privateer,
    /// Fast/lucky scout. Move + luck + crit, low HP + range.
    Corsair,
    /// Rune + proc specialist. Long range, big proc strength + rune
    /// damage, narrow turret arc.
    Harpooner,
    /// Shield-tank ghost ship. Big shield + fast recharge, low HP.
    Revenant,
}

impl Hull {
    pub fn label(self) -> &'static str {
        match self {
            Hull::Default     => "STANDARD",
            Hull::GlassCannon => "GLASS CANNON",
            Hull::Rammer      => "RAMMER",
            Hull::Dreadnought => "DREADNOUGHT",
            Hull::Privateer   => "PRIVATEER",
            Hull::Corsair     => "CORSAIR",
            Hull::Harpooner   => "HARPOONER",
            Hull::Revenant    => "REVENANT",
        }
    }

    /// One-line summary, shown both as the card subtitle and the
    /// detail-panel header.
    pub fn tagline(self) -> &'static str {
        match self {
            Hull::Default     => "Baseline hull. No modifiers.",
            Hull::GlassCannon => "Low HP, long range, harder-hitting runes.",
            Hull::Rammer      => "Triple HP, very short turret range.",
            Hull::Dreadnought => "Slow heavy. Massive HP and shield, wide turret arcs.",
            Hull::Privateer   => "Pirate raider. Crit + scrap drops, thin hull.",
            Hull::Corsair     => "Fast scout. Move + luck + crit, low HP.",
            Hull::Harpooner   => "Rune specialist. Long range, big procs, narrow arc.",
            Hull::Revenant    => "Ghost ship. Big shield, fast recharge, low HP.",
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
            Hull::Dreadnought => &[
                "+200 HP",
                "+80 shield",
                "+20 turret arc (deg)",
                "+25% shield recharge",
            ],
            Hull::Privateer => &[
                "+75% scrap harvest",
                "+15% crit chance",
                "+10% luck",
            ],
            Hull::Corsair => &[
                "+12 move speed",
                "+30% luck",
                "+8% crit chance",
            ],
            Hull::Harpooner => &[
                "+25% turret range",
                "+30% rune damage",
                "+30% proc strength",
            ],
            Hull::Revenant => &[
                "+120 shield",
                "+30% shield recharge",
                "-1.5s shield recharge delay",
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
            Hull::Dreadnought => &[
                "-12 move speed",
                "-25% turret range",
            ],
            Hull::Privateer => &[
                "-60 HP",
                "-15% turret range",
            ],
            Hull::Corsair => &[
                "-50 HP",
                "-10% turret range",
            ],
            Hull::Harpooner => &[
                "-70 HP",
                "-15 turret arc (deg)",
            ],
            Hull::Revenant => &[
                "-50 HP",
                "-10% turret range",
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
            Hull::Dreadnought => {
                stats.hp.flat                       =  200.0;
                stats.shield_max.flat               =   80.0;
                stats.turret_arc_bonus_deg.flat     =   20.0;
                stats.shield_recharge_rate_pct.flat =   25.0;
                stats.move_speed.flat               =  -12.0;
                stats.range_pct.flat                =  -25.0;
            }
            Hull::Privateer => {
                stats.harvest_pct.flat = 75.0;
                stats.crit_pct.flat    = 15.0;
                stats.luck_pct.flat    = 10.0;
                stats.hp.flat          = -60.0;
                stats.range_pct.flat   = -15.0;
            }
            Hull::Corsair => {
                stats.move_speed.flat =  12.0;
                stats.luck_pct.flat   =  30.0;
                stats.crit_pct.flat   =   8.0;
                stats.hp.flat         = -50.0;
                stats.range_pct.flat  = -10.0;
            }
            Hull::Harpooner => {
                stats.range_pct.flat            =  25.0;
                stats.rune_damage.percent       =   0.30;
                stats.proc_strength_pct.flat    =  30.0;
                stats.hp.flat                   = -70.0;
                stats.turret_arc_bonus_deg.flat = -15.0;
            }
            Hull::Revenant => {
                stats.shield_max.flat                =  120.0;
                stats.shield_recharge_rate_pct.flat  =   30.0;
                stats.shield_recharge_delay.flat     =   -1.5;
                stats.hp.flat                        =  -50.0;
                stats.range_pct.flat                 =  -10.0;
            }
        }
    }
}

// ---------- Font sizing ----------
//
// The hull-select overlay's text was reading as cramped; bumped
// across the board. `_FONT` constants kept local because they're
// only used by this overlay's layout functions and should scale
// together if we want to retune.
const DETAIL_TITLE_FONT: f32 = theme::FONT_LG * 2.0;     // ~28 pt
const DETAIL_TAGLINE_FONT: f32 = theme::FONT_LG * 1.2;   // ~17 pt
const DETAIL_BULLET_FONT: f32 = theme::FONT_LG;          // 14 pt

// ---------- Markers ----------

#[derive(Component)]
pub struct HullSelectRoot;

/// Marker on each map-size button — `MapSize` is part of the dockyard
/// pick alongside the hull. Read by `handle_map_size_click` to update
/// the `MapSize` resource; the overlay rebuilds on the resource
/// change to re-tint the selected pill.
#[derive(Component, Clone, Copy)]
pub struct MapSizeButton(pub crate::map::MapSize);

#[derive(Component)]
pub struct HullDetailPanel;

#[derive(Component, Clone, Copy)]
pub struct HullCard(pub Hull);

#[derive(Component)]
pub struct HullPlayButton;

#[derive(Component)]
pub struct HullBackButton;

// ---------- Spawn ----------

// ---------- Dockyard palette ----------
//
// Hand-tuned warm-wood + harbour palette so the overlay reads as an
// actual berth, not a UI window. Plank base and gap are the only two
// colours used to bake the tile-able plank image; the rest tint
// borders and accents.

/// Lighter weathered plank.
const WOOD_LIGHT: Color = Color::srgb(0.55, 0.36, 0.20);
/// Darker plank stripe (alternates with WOOD_LIGHT in the tile).
const WOOD_DARK: Color = Color::srgb(0.40, 0.24, 0.13);
/// Plank-seam line / wood vein colour.
const WOOD_GAP: Color = Color::srgb(0.16, 0.09, 0.05);
/// Mooring rope — used for the selected card's border and the dock-
/// edge trim.
const ROPE: Color = Color::srgb(0.86, 0.70, 0.42);
/// Pinned-paper / parchment for the detail panel body — reads as a
/// ship manifest nailed to a wood frame.
const PARCHMENT: Color = Color::srgb(0.92, 0.85, 0.66);
/// Dark inked text on the parchment.
const INK: Color = Color::srgb(0.18, 0.13, 0.08);

pub fn enter_hull_select(
    commands: Commands,
    selected: Res<SelectedHull>,
    preview: Res<PreviewHull>,
    map_size: Res<crate::map::MapSize>,
) {
    // Manifest panel reflects the hover preview when present, else the
    // committed selection. Berth ship tinting (in the pixel scene) is
    // driven separately by `dockyard_view::update_dockyard_highlight`
    // and reads `SelectedHull` directly.
    let panel_hull = preview.0.unwrap_or(selected.0);
    spawn_overlay(commands, panel_hull, *map_size);
}

fn spawn_overlay(
    mut commands: Commands,
    panel_hull: Hull,
    map_size: crate::map::MapSize,
) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(0.0),
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                bottom: Val::Px(0.0),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Stretch,
                ..default()
            },
            // Transparent — the pixel-art dockyard scene rendered by
            // `dockyard_view` shows through behind this overlay.
            BackgroundColor(Color::NONE),
            ZIndex(150),
            Visibility::Inherited,
            HullSelectRoot,
        ))
        .with_children(|root| {
            // ---- LEFT: flex spacer that lets the pixel dockyard
            //      render show through. Holds the big DOCKYARD
            //      header pinned to the top-left so it overlays the
            //      water + ships behind it.
            root.spawn((
                Node {
                    flex_grow: 1.0,
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::FlexStart,
                    justify_content: JustifyContent::FlexStart,
                    padding: UiRect::all(Val::Px(theme::PAD_LG * 2.0)),
                    row_gap: Val::Px(theme::GAP_SM),
                    ..default()
                },
                BackgroundColor(Color::NONE),
            ))
            .with_children(|left| {
                left.spawn(ui_kit::label(
                    "DOCKYARD",
                    theme::FONT_LG * 2.4,
                    ROPE,
                ));
                left.spawn((
                    Text::new("Click a vessel to inspect, then PLAY."),
                    TextFont {
                        font_size: theme::FONT_MD,
                        font_smoothing: bevy::text::FontSmoothing::None,
                        ..default()
                    },
                    TextColor(PARCHMENT),
                ));
            });

            // ---- RIGHT: minimal column — parchment manifest +
            //      voyage selector + BACK, no decking background.
            //      Sits transparent so the pixel dockyard scene
            //      shows through behind it.
            root.spawn((
                Node {
                    width: Val::Px(440.0),
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Stretch,
                    justify_content: JustifyContent::Center,
                    padding: UiRect::all(Val::Px(theme::PAD_LG)),
                    row_gap: Val::Px(theme::GAP_LG),
                    ..default()
                },
                BackgroundColor(Color::NONE),
            ))
            .with_children(|panel_col| {
                panel_col.spawn((
                    Node {
                        min_height: Val::Px(420.0),
                        padding: UiRect::all(Val::Px(theme::PAD_LG * 2.0)),
                        border: UiRect::all(Val::Px(theme::BORDER_W * 2.5)),
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::Stretch,
                        row_gap: Val::Px(theme::GAP_LG),
                        ..default()
                    },
                    BackgroundColor(PARCHMENT),
                    BorderColor(WOOD_GAP),
                    HullDetailPanel,
                ))
                .with_children(|panel| {
                    spawn_detail_content(panel, panel_hull);
                });

                // ---- Voyage size selector ----
                // Stacked vertically (one pill per row) so the names +
                // section counts fit comfortably without overflowing
                // the column, even at smaller window sizes.
                panel_col.spawn((
                    Node {
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::Stretch,
                        row_gap: Val::Px(theme::GAP_SM),
                        ..default()
                    },
                    BackgroundColor(Color::NONE),
                ))
                .with_children(|col| {
                    col.spawn((
                        Text::new("VOYAGE LENGTH"),
                        TextFont {
                            font_size: theme::FONT_MD,
                            font_smoothing: bevy::text::FontSmoothing::None,
                            ..default()
                        },
                        TextColor(ROPE),
                        Node {
                            align_self: AlignSelf::Center,
                            ..default()
                        },
                    ));
                    for &size in crate::map::MapSize::ALL {
                        spawn_map_size_pill(col, size, size == map_size);
                    }
                });

                // ---- BACK button at the bottom of the right column.
                panel_col.spawn((
                    Node {
                        align_self: AlignSelf::Center,
                        ..default()
                    },
                    BackgroundColor(Color::NONE),
                ))
                .with_children(|wrap| {
                    wrap.spawn((ui_kit::button(WOOD_DARK), HullBackButton))
                        .with_children(|b| {
                            b.spawn(ui_kit::label("BACK", theme::FONT_MD, PARCHMENT));
                        });
                });
            });
        });
}

/// One map-size pill. Stacked vertically with its siblings, laid out
/// as a single horizontal row: label on the left, detail on the
/// right. Active pill gets a rope border + lighter wood fill.
fn spawn_map_size_pill(
    parent: &mut ChildSpawnerCommands,
    size: crate::map::MapSize,
    active: bool,
) {
    let bg = if active { WOOD_LIGHT } else { Color::srgba(0.0, 0.0, 0.0, 0.35) };
    let border = if active { ROPE } else { WOOD_DARK };
    let label_color = if active { Color::WHITE } else { ROPE };
    let detail_color = if active {
        Color::srgb(0.95, 0.90, 0.74)
    } else {
        Color::srgb(0.80, 0.72, 0.55)
    };
    parent
        .spawn((
            Button,
            Node {
                padding: UiRect::axes(Val::Px(theme::PAD_MD), Val::Px(theme::PAD_SM)),
                border: UiRect::all(Val::Px(theme::BORDER_W)),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::SpaceBetween,
                column_gap: Val::Px(theme::GAP_MD),
                ..default()
            },
            BackgroundColor(bg),
            BorderColor(border),
            MapSizeButton(size),
        ))
        .with_children(|pill| {
            pill.spawn(ui_kit::label(size.label(), theme::FONT_MD, label_color));
            pill.spawn((
                Text::new(size.detail()),
                TextFont {
                    font_size: theme::FONT_SM,
                    font_smoothing: bevy::text::FontSmoothing::None,
                    ..default()
                },
                TextColor(detail_color),
            ));
        });
}

/// Right-panel content (title + tagline + buffs + nerfs + PLAY).
/// Lives inside the existing `HullDetailPanel` Node — caller is the
/// `with_children` closure of that node, so this fn spawns the
/// children directly.
fn spawn_detail_content(panel: &mut ChildSpawnerCommands, hull: Hull) {
    // Title is INKED on the parchment, not the gold accent — keeps
    // the manifest reading as "writing on paper" rather than UI.
    panel.spawn(ui_kit::label(hull.label(), DETAIL_TITLE_FONT, INK));
    panel.spawn((
        Text::new(hull.tagline()),
        TextFont {
            font_size: DETAIL_TAGLINE_FONT,
            font_smoothing: bevy::text::FontSmoothing::None,
            ..default()
        },
        TextColor(Color::srgb(0.32, 0.24, 0.16)),
    ));

    // Buffs — deep green ink (matches the parchment palette).
    for b in hull.buffs() {
        panel.spawn((
            Text::new(b.to_string()),
            TextFont {
                font_size: DETAIL_BULLET_FONT,
                font_smoothing: bevy::text::FontSmoothing::None,
                ..default()
            },
            TextColor(Color::srgb(0.15, 0.45, 0.18)),
        ));
    }

    // Nerfs — wax-seal red.
    for n in hull.nerfs() {
        panel.spawn((
            Text::new(n.to_string()),
            TextFont {
                font_size: DETAIL_BULLET_FONT,
                font_smoothing: bevy::text::FontSmoothing::None,
                ..default()
            },
            TextColor(Color::srgb(0.65, 0.18, 0.15)),
        ));
    }

    // Spacer + PLAY button anchored bottom — rope-tinted button so
    // it reads as "the captain's stamp" against the parchment.
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
                border: UiRect::all(Val::Px(theme::BORDER_W * 2.0)),
                ..default()
            },
            BackgroundColor(ROPE),
            BorderColor(WOOD_GAP),
            HullPlayButton,
        ))
        .with_children(|b| {
            b.spawn(ui_kit::label("PLAY", theme::FONT_LG, INK));
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

/// Card interactions, split between hover-preview and click-commit:
/// - `Interaction::Hovered` → update `PreviewHull` so the right
///   panel previews that card. Doesn't change `SelectedHull`, so
///   the player can scan options without losing their committed
///   pick.
/// - `Interaction::Pressed` → update `SelectedHull` (the committed
///   choice); also keep `PreviewHull` in sync so the panel doesn't
///   flicker on the click frame.
/// - Cursor leaving every card (no `Hovered` or `Pressed` matches
///   this frame) clears `PreviewHull`, falling the detail panel
///   back to whichever hull is `SelectedHull`.
pub fn handle_card_click(
    interactions: Query<(&Interaction, &HullCard), Changed<Interaction>>,
    all_cards: Query<&Interaction, With<HullCard>>,
    mut selected: ResMut<SelectedHull>,
    mut preview: ResMut<PreviewHull>,
) {
    for (interaction, card) in &interactions {
        match *interaction {
            Interaction::Pressed => {
                if selected.0 != card.0 { selected.0 = card.0; }
                if preview.0 != Some(card.0) { preview.0 = Some(card.0); }
            }
            Interaction::Hovered => {
                if preview.0 != Some(card.0) { preview.0 = Some(card.0); }
            }
            Interaction::None => {
                // Clearing handled below by scanning all cards — a
                // single card going to `None` doesn't necessarily
                // mean the cursor left the whole strip (it might
                // have moved straight to a neighbour).
            }
        }
    }
    // If NO card is currently hovered or pressed, clear the preview.
    let any_active = all_cards
        .iter()
        .any(|i| matches!(i, Interaction::Hovered | Interaction::Pressed));
    if !any_active && preview.0.is_some() {
        preview.0 = None;
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

/// Rebuild the overlay whenever the player's dockyard pick changes —
/// `SelectedHull`, `PreviewHull`, or `MapSize`. Keeps card highlights
/// + right-panel content + size pill tints in sync without per-text
/// query plumbing. The overlay is small so despawn/respawn is fine.
pub fn sync_hull_select_on_change(
    selected: Res<SelectedHull>,
    preview: Res<PreviewHull>,
    map_size: Res<crate::map::MapSize>,
    commands: Commands,
    q: Query<Entity, With<HullSelectRoot>>,
    state: Res<State<crate::AppState>>,
) {
    if !selected.is_changed() && !preview.is_changed() && !map_size.is_changed() { return; }
    if *state.get() != crate::AppState::HullSelect { return; }
    let mut commands = commands;
    for e in &q {
        commands.entity(e).despawn();
    }
    let panel_hull = preview.0.unwrap_or(selected.0);
    spawn_overlay(commands, panel_hull, *map_size);
}

/// Click handler — commit the player's pick to the `MapSize` resource.
/// The rebuild system above picks up the change and re-tints the pills.
pub fn handle_map_size_click(
    interactions: Query<(&Interaction, &MapSizeButton), Changed<Interaction>>,
    mut map_size: ResMut<crate::map::MapSize>,
) {
    for (interaction, btn) in &interactions {
        if matches!(*interaction, Interaction::Pressed) {
            if *map_size != btn.0 {
                *map_size = btn.0;
            }
            return;
        }
    }
}
