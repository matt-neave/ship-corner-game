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

/// Iteration order of hulls in the dockyard grid. Tier-1 (vanilla,
/// pure stat-swap variants) come first; the more-flavoured options
/// come after. Layout wraps to a 2-column grid in `spawn_overlay`.
const HULL_ORDER: [Hull; 8] = [
    Hull::Default,
    Hull::GlassCannon,
    Hull::Rammer,
    Hull::Dreadnought,
    Hull::Privateer,
    Hull::Corsair,
    Hull::Harpooner,
    Hull::Revenant,
];

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
/// Harbour water behind / around the dock — desaturated dusk teal so
/// the planks pop without fighting the in-game ocean colour.
const HARBOUR_WATER: Color = Color::srgb(0.10, 0.18, 0.26);
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
    images: ResMut<Assets<Image>>,
) {
    spawn_overlay(commands, selected.0, images);
}

fn spawn_overlay(
    mut commands: Commands,
    selected: Hull,
    mut images: ResMut<Assets<Image>>,
) {
    // Plank tile — narrow vertical extent (3 planks × 12 px) repeats
    // up/down to fill the dock floor without obvious seams.
    let plank_tile = images.add(crate::rendering::make_plank_image(
        96, 12, 3, WOOD_LIGHT, WOOD_DARK, WOOD_GAP,
    ));
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
            BackgroundColor(HARBOUR_WATER),
            ZIndex(150),
            Visibility::Inherited,
            HullSelectRoot,
            // Absorb clicks behind any future world overlay.
            Button,
        ))
        .with_children(|root| {
            root.spawn(ui_kit::label(
                "DOCKYARD",
                theme::FONT_LG * 2.6,
                ROPE,
            ));
            root.spawn((
                Text::new("Pick a vessel from the berths"),
                TextFont {
                    font_size: theme::FONT_LG,
                    font_smoothing: bevy::text::FontSmoothing::None,
                    ..default()
                },
                TextColor(PARCHMENT),
            ));

            // The "dock floor" — a single big wooden plank rectangle
            // hosting the berth grid + detail manifest. Rope-coloured
            // border reads as the timber edge of the wharf.
            root.spawn((
                Node {
                    flex_direction: FlexDirection::Row,
                    column_gap: Val::Px(theme::GAP_LG * 2.0),
                    align_items: AlignItems::Stretch,
                    padding: UiRect::all(Val::Px(theme::PAD_LG * 1.5)),
                    border: UiRect::all(Val::Px(theme::BORDER_W * 3.0)),
                    ..default()
                },
                ImageNode {
                    image: plank_tile.clone(),
                    image_mode: bevy::ui::widget::NodeImageMode::Tiled {
                        tile_x: true,
                        tile_y: true,
                        stretch_value: 2.0,
                    },
                    ..default()
                },
                BorderColor(ROPE),
            ))
            .with_children(|cols| {
                // ---- LEFT: berth grid ----
                // Row + flex_wrap so each row holds two cards, wrapping
                // to the next on overflow. Width fits exactly 2 × CARD_W
                // plus the inter-card gap.
                const CARD_W: f32 = 200.0;
                let grid_w = CARD_W * 2.0 + theme::GAP_LG;
                cols.spawn(Node {
                    flex_direction: FlexDirection::Row,
                    flex_wrap: FlexWrap::Wrap,
                    column_gap: Val::Px(theme::GAP_LG),
                    row_gap: Val::Px(theme::GAP_LG),
                    width: Val::Px(grid_w),
                    ..default()
                })
                .with_children(|list| {
                    for hull in HULL_ORDER {
                        spawn_card(list, hull, hull == selected, CARD_W);
                    }
                });

                // ---- RIGHT: ship manifest (parchment on planks) ----
                cols.spawn((
                    Node {
                        width: Val::Px(420.0),
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
                    spawn_detail_content(panel, selected);
                });
            });

            // ---- BACK button under the dock ----
            root.spawn((ui_kit::button(WOOD_DARK), HullBackButton))
                .with_children(|b| {
                    b.spawn(ui_kit::label("BACK", theme::FONT_MD, PARCHMENT));
                });
        });
}

/// One "berth" card — a vessel docked at a slip. The card itself
/// shows the hull's silhouette (capsule, hull-tinted) floating in a
/// water trough, with the name + tagline pinned underneath. Border
/// flips to rope-yellow on the active pick.
fn spawn_card(
    parent: &mut ChildSpawnerCommands,
    hull: Hull,
    selected: bool,
    card_w: f32,
) {
    let bg = if selected { WOOD_LIGHT } else { WOOD_DARK };
    let border = if selected { ROPE } else { WOOD_GAP };
    parent
        .spawn((
            Button,
            Node {
                width: Val::Px(card_w),
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
            // "Water trough" — a thin band of harbour blue with the
            // vessel's silhouette floating in it. Reads as the slip
            // each ship is moored in.
            card.spawn((
                Node {
                    width: Val::Percent(100.0),
                    height: Val::Px(34.0),
                    padding: UiRect::axes(Val::Px(theme::PAD_MD), Val::Px(theme::PAD_SM)),
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::Center,
                    border: UiRect::all(Val::Px(theme::BORDER_W)),
                    ..default()
                },
                BackgroundColor(HARBOUR_WATER),
                BorderColor(WOOD_GAP),
            ))
            .with_children(|slip| {
                let (fill, length, height) = hull_silhouette(hull);
                slip.spawn((
                    Node {
                        width: Val::Px(length),
                        height: Val::Px(height),
                        ..default()
                    },
                    BackgroundColor(fill),
                    BorderRadius::all(Val::Px(height * 0.5)),
                ));
            });
            let title_color = if selected { ROPE } else { PARCHMENT };
            card.spawn(ui_kit::label(hull.label(), CARD_TITLE_FONT, title_color));
            card.spawn((
                Text::new(hull.tagline()),
                TextFont {
                    font_size: CARD_TAGLINE_FONT,
                    font_smoothing: bevy::text::FontSmoothing::None,
                    ..default()
                },
                TextColor(Color::srgb(0.85, 0.78, 0.62)),
            ));
        });
}

/// Per-hull silhouette: (fill colour, length px, height px). Wider /
/// taller capsules for the tankier hulls, slimmer for the fast scout
/// flavours — gives the berth visual variety without needing
/// per-hull sprites.
fn hull_silhouette(hull: Hull) -> (Color, f32, f32) {
    match hull {
        Hull::Default     => (Color::srgb(0.75, 0.78, 0.84), 70.0, 16.0),
        Hull::GlassCannon => (Color::srgb(0.55, 0.85, 0.90), 80.0, 12.0),
        Hull::Rammer      => (Color::srgb(0.65, 0.55, 0.45), 78.0, 22.0),
        Hull::Dreadnought => (Color::srgb(0.40, 0.45, 0.50), 92.0, 26.0),
        Hull::Privateer   => (Color::srgb(0.90, 0.55, 0.30), 74.0, 16.0),
        Hull::Corsair     => (Color::srgb(0.85, 0.80, 0.40), 84.0, 11.0),
        Hull::Harpooner   => (Color::srgb(0.50, 0.70, 0.95), 88.0, 13.0),
        Hull::Revenant    => (Color::srgb(0.60, 0.75, 0.85), 78.0, 14.0),
    }
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

/// Hover a left-column card → set it as the active pick (preview in
/// the right detail panel). The PLAY button commits whichever hull
/// was last hovered. `Interaction::Hovered` fires on cursor-enter,
/// which also covers click — clicking implies hovering first, so we
/// don't need a separate `Pressed` branch.
pub fn handle_card_click(
    interactions: Query<(&Interaction, &HullCard), Changed<Interaction>>,
    mut selected: ResMut<SelectedHull>,
) {
    for (interaction, card) in &interactions {
        if !matches!(*interaction, Interaction::Hovered | Interaction::Pressed) {
            continue;
        }
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
    images: ResMut<Assets<Image>>,
) {
    if !selected.is_changed() { return; }
    if *state.get() != crate::AppState::HullSelect { return; }
    let mut commands = commands;
    for e in &q {
        commands.entity(e).despawn();
    }
    spawn_overlay(commands, selected.0, images);
}
