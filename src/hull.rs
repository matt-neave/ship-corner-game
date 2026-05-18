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
use crate::ui_kit::{self, theme, ChunkyButtonStyle};
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
            // Render-target + persistent preview entities built once
            // at startup; `toggle_dockyard_render` flips the camera
            // active only while HullSelect is up so the GPU doesn't
            // pay to render it otherwise.
            .add_systems(Startup, crate::dockyard_view::setup_hull_preview_render)
            .add_systems(
                OnEnter(AppState::HullSelect),
                (enter_hull_select, crate::reset_run_timer),
            )
            // OnExit(HullSelect): tear down the overlay, regenerate
            // the map with the player's chosen `MapSize`, then
            // re-run the map-view setup so the new topology has its
            // visuals. Chained so the new `MapState` is in place
            // before `setup_map` reads it.
            .add_systems(
                OnExit(AppState::HullSelect),
                (
                    exit_hull_select,
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
                    handle_difficulty_click,
                    handle_play_click,
                    handle_back_click,
                    handle_back_on_esc,
                    sync_hull_select_on_change,
                    sync_hull_apply,
                ).run_if(in_state(AppState::HullSelect)),
            )
            // Always-on: clamp HP to max each frame so a stat change
            // never leaves a "100/50" readout. Lives with hull-select
            // because the hull is what defines max HP, but applies
            // everywhere.
            .add_systems(Update, clamp_hp_to_max)
            // Toggle the gameplay HUD chrome on entering/leaving
            // HullSelect so the menu reads clean. Also drives the
            // hull-preview re-tint when the player hovers a
            // different tile.
            .add_systems(
                Update,
                (
                    crate::dockyard_view::toggle_dockyard_render,
                    crate::dockyard_view::update_hull_preview,
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
    /// Stripped-down chassis — only 4 turret slots out of the usual 8.
    /// Trade-off compensates with huge damage / fire-rate buffs and
    /// extra HP so each remaining slot punches above its weight.
    /// Cuts decision overhead by half: fewer slots, sharper build.
    Cutter,
    /// Pirate-only chassis. Refuses every weapon that doesn't carry
    /// the [`WeaponTag::Pirate`] tag (Cannon / Harpoon / Anchor
    /// Flail / Crow's Nest). Pirate-tag synergy stays maxed by
    /// construction, plus a flat scrap + crit boost from the
    /// `Privateer` lineage.
    Marauder,
}

impl Hull {
    pub fn label(self) -> &'static str {
        match self {
            Hull::Default     => "GUNBOAT-8",
            Hull::GlassCannon => "GLASS CANNON",
            Hull::Rammer      => "RAMMER",
            Hull::Dreadnought => "DREADNOUGHT",
            Hull::Privateer   => "PRIVATEER",
            Hull::Corsair     => "CORSAIR",
            Hull::Harpooner   => "HARPOONER",
            Hull::Revenant    => "REVENANT",
            Hull::Cutter      => "CUTTER-4",
            Hull::Marauder    => "MARAUDER",
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
            Hull::Cutter      => "Only 4 turret slots. Each one punches twice as hard.",
            Hull::Marauder    => "Pirate-only chassis. Tag synergy locked in.",
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
                "+2/s shield recharge",
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
                "+5/s shield recharge",
                "-1.5s shield recharge delay",
            ],
            Hull::Cutter => &[
                "+100% turret damage",
                "+30% fire rate (cooldown reduction)",
                "+100 HP",
            ],
            Hull::Marauder => &[
                "+100% scrap harvest",
                "+15% crit chance",
                "+15% luck",
                "+10% move speed",
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
            Hull::Cutter => &[
                "Only 4 turret slots (vs 8)",
            ],
            Hull::Marauder => &[
                "Pirate-tag weapons only",
                "-40 HP",
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
                stats.shield_recharge_rate.flat     =    2.0;
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
                stats.shield_max.flat               =  120.0;
                stats.shield_recharge_rate.flat     =    5.0;
                stats.shield_recharge_delay.flat    =   -1.5;
                stats.hp.flat                       =  -50.0;
                stats.range_pct.flat                =  -10.0;
            }
            Hull::Cutter => {
                // Stat-side compensation for losing 4 slots.
                // turret_damage_pct + cooldown_pct are the two
                // levers that affect every weapon equally, so the
                // remaining 4 slots scale up cleanly.
                stats.turret_damage_pct.flat = 100.0;
                stats.cooldown_pct.flat      =  30.0;
                stats.hp.flat                = 100.0;
            }
            Hull::Marauder => {
                stats.harvest_pct.flat = 100.0;
                stats.crit_pct.flat    =  15.0;
                stats.luck_pct.flat    =  15.0;
                stats.move_speed.flat  =  10.0;
                stats.hp.flat          = -40.0;
            }
        }
    }

    /// How many of the 8 turret slots this hull can actually use.
    /// Slots beyond this index are visually marked locked and the
    /// equip path refuses drops onto them. Default is 8 — only
    /// hulls that explicitly trade slot count for stat compensation
    /// override.
    pub fn turret_slot_cap(self) -> usize {
        match self {
            Hull::Cutter => 4,
            _ => 8,
        }
    }

    /// True if this hull restricts weapon equipping to a specific
    /// `WeaponTag`. Returns `None` if any weapon is allowed.
    /// Enforced by the customize drag/drop path — drops of a
    /// disallowed weapon are rejected silently (same UX as
    /// invalid-slot drops).
    pub fn weapon_tag_lock(self) -> Option<crate::weapon::WeaponTag> {
        match self {
            Hull::Marauder => Some(crate::weapon::WeaponTag::Pirate),
            _ => None,
        }
    }

    /// True if `weapon` is allowed by this hull's tag lock. Always
    /// true for hulls without a lock. The shop's mod cards + the
    /// drag-and-drop resolver both call this before committing an
    /// equip / merge.
    pub fn allows_weapon(self, weapon: crate::weapon::WeaponType) -> bool {
        match self.weapon_tag_lock() {
            Some(lock) => weapon.tags().iter().any(|t| *t == lock),
            None => true,
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

/// Marker on each difficulty button (0/1/2). Read by
/// `handle_difficulty_click` to write the `Difficulty` resource; the
/// overlay rebuilds on the resource change to re-tint the active pill.
#[derive(Component, Clone, Copy)]
pub struct DifficultyButton(pub u8);

/// Marker on a difficulty / voyage-length pill that should not
/// accept clicks — drawn in the disabled palette and skipped by
/// the click handlers. Used to gate harder difficulties + non-
/// medium voyage lengths until the debug overlay (`#`) is open.
#[derive(Component)]
pub struct LockedSelector;

#[derive(Component, Clone, Copy)]
pub struct HullCard(pub Hull);

#[derive(Component)]
pub struct HullPlayButton;

#[derive(Component)]
pub struct HullBackButton;

pub fn enter_hull_select(
    commands: Commands,
    selected: Res<SelectedHull>,
    preview: Res<PreviewHull>,
    map_size: Res<crate::map::MapSize>,
    difficulty: Res<crate::Difficulty>,
    hull_preview: Res<crate::dockyard_view::HullPreviewImage>,
    pixel_font: Res<crate::fonts::PixelFont>,
    mode: Res<crate::multiplayer::NetMode>,
    debug_visible: Res<crate::map::DebugUiVisible>,
) {
    // Detail panel reflects the hover preview when present, else the
    // committed selection. The hull-tile grid below reads
    // `SelectedHull` directly for its highlight state.
    let panel_hull = preview.0.unwrap_or(selected.0);
    spawn_overlay(commands, &pixel_font, panel_hull, selected.0, *map_size, *difficulty, &hull_preview.0, &mode, debug_visible.0);
}

fn spawn_overlay(
    mut commands: Commands,
    font: &crate::fonts::PixelFont,
    panel_hull: Hull,
    selected_hull: Hull,
    map_size: crate::map::MapSize,
    difficulty: crate::Difficulty,
    hull_preview_image: &Handle<Image>,
    mode: &crate::multiplayer::NetMode,
    debug_unlock: bool,
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
                align_items: AlignItems::Stretch,
                ..default()
            },
            BackgroundColor(theme::SURFACE),
            ZIndex(150),
            Visibility::Inherited,
            HullSelectRoot,
        ))
        .with_children(|root| {
            // ============ TOP HALF — 3 column panels ============
            root.spawn((
                Node {
                    width: Val::Percent(100.0),
                    height: Val::Percent(50.0),
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Stretch,
                    padding: UiRect::all(Val::Px(theme::PAD_LG)),
                    column_gap: Val::Px(theme::GAP_LG),
                    ..default()
                },
                BackgroundColor(Color::NONE),
            ))
            .with_children(|top| {
                // --- LHS: ship preview ---
                top.spawn((
                    Node {
                        flex_basis: Val::Percent(33.0),
                        flex_grow: 1.0,
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::Center,
                        justify_content: JustifyContent::Center,
                        padding: UiRect::all(Val::Px(theme::PAD_LG)),
                        border: UiRect::all(Val::Px(theme::CHUNKY_BORDER_W)),
                        ..default()
                    },
                    BackgroundColor(theme::SURFACE_RAISED),
                    BorderColor(theme::CHUNKY_OUTLINE),
                    BorderRadius::all(Val::Px(theme::CHUNKY_RADIUS)),
                ))
                .with_children(|card| {
                    spawn_ship_preview(card, hull_preview_image);
                });

                // --- Middle: name + stats ---
                top.spawn((
                    Node {
                        flex_basis: Val::Percent(34.0),
                        flex_grow: 1.0,
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::Stretch,
                        justify_content: JustifyContent::FlexStart,
                        padding: UiRect::all(Val::Px(theme::PAD_LG * 1.5)),
                        row_gap: Val::Px(theme::GAP_MD),
                        border: UiRect::all(Val::Px(theme::CHUNKY_BORDER_W)),
                        ..default()
                    },
                    BackgroundColor(theme::SURFACE_RAISED),
                    BorderColor(theme::CHUNKY_OUTLINE),
                    BorderRadius::all(Val::Px(theme::CHUNKY_RADIUS)),
                ))
                .with_children(|info| {
                    spawn_detail_content(info, font, panel_hull);
                });

                // --- RHS: voyage + PLAY + BACK ---
                top.spawn((
                    Node {
                        flex_basis: Val::Percent(33.0),
                        flex_grow: 1.0,
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::Stretch,
                        justify_content: JustifyContent::FlexStart,
                        padding: UiRect::all(Val::Px(theme::PAD_LG * 1.5)),
                        row_gap: Val::Px(theme::GAP_MD),
                        border: UiRect::all(Val::Px(theme::CHUNKY_BORDER_W)),
                        ..default()
                    },
                    BackgroundColor(theme::SURFACE_RAISED),
                    BorderColor(theme::CHUNKY_OUTLINE),
                    BorderRadius::all(Val::Px(theme::CHUNKY_RADIUS)),
                ))
                .with_children(|run| {
                    run.spawn(ui_kit::pixel_label(
                        font,
                        "VOYAGE LENGTH",
                        theme::FONT_LG,
                        theme::ACCENT,
                    ));
                    for &size in crate::map::MapSize::ALL {
                        // Only Medium voyage is unlocked outside
                        // debug mode. Press `#` (DebugUiVisible) to
                        // open the others up for testing.
                        let locked = !debug_unlock
                            && !matches!(size, crate::map::MapSize::Medium);
                        spawn_map_size_pill(run, font, size, size == map_size, locked);
                    }
                    run.spawn((
                        Node {
                            margin: UiRect::top(Val::Px(theme::GAP_MD)),
                            ..default()
                        },
                        ui_kit::pixel_label(
                            font,
                            "DIFFICULTY",
                            theme::FONT_LG,
                            theme::ACCENT,
                        ),
                    ));
                    // Single row of 7 difficulty pills. Tier 0
                    // (leftmost) is the baseline; each step right
                    // gets harder. SNKRX / Brotato convention —
                    // progress through tiers as you get better,
                    // not "pick a difficulty band centred on
                    // normal". Avoids the numpad look the 3×3
                    // grid produced.
                    run.spawn(Node {
                        flex_direction: FlexDirection::Row,
                        align_items: AlignItems::Center,
                        justify_content: JustifyContent::Center,
                        column_gap: Val::Px(theme::GAP_SM),
                        ..default()
                    })
                    .with_children(|row| {
                        for &v in crate::Difficulty::VALUES {
                            // Only difficulty 0 is unlocked outside
                            // debug mode. Press `#` (DebugUiVisible)
                            // to open the harder tiers for testing.
                            let locked = !debug_unlock && v != 0;
                            spawn_difficulty_pill(row, font, v, v == difficulty.0, locked);
                        }
                    });
                    // Spacer pushes PLAY + BACK to the card bottom.
                    run.spawn(Node {
                        flex_grow: 1.0,
                        ..default()
                    });
                    // In MP the PLAY button doubles as the per-peer
                    // READY trigger — host advances HullSelect →
                    // Playing once both peers click. Label matches
                    // the shop's READY CTA so the affordance reads
                    // consistently across the per-peer states.
                    let cta_label = if matches!(*mode, crate::multiplayer::NetMode::Solo) {
                        "PLAY"
                    } else {
                        "READY"
                    };
                    spawn_play_button(run, font, cta_label);
                    spawn_back_button(run, font);
                });
            });

            // ============ BOTTOM HALF — hull-tile grid ============
            // Outer container is just the layout shell that inherits
            // the top-half's screen-edge gap; the frame styling lives
            // on the inner node so the chunky border sits inset from
            // the screen, matching the inset of the three top-half
            // panels.
            root.spawn((
                Node {
                    width: Val::Percent(100.0),
                    height: Val::Percent(50.0),
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Stretch,
                    padding: UiRect::all(Val::Px(theme::PAD_LG)),
                    ..default()
                },
                BackgroundColor(Color::NONE),
            ))
            .with_children(|outer| {
                outer.spawn((
                    Node {
                        flex_grow: 1.0,
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::Stretch,
                        padding: UiRect::all(Val::Px(theme::PAD_LG * 1.5)),
                        row_gap: Val::Px(theme::GAP_MD),
                        border: UiRect::all(Val::Px(theme::CHUNKY_BORDER_W)),
                        ..default()
                    },
                    BackgroundColor(theme::SURFACE_RAISED),
                    BorderColor(theme::CHUNKY_OUTLINE),
                    BorderRadius::all(Val::Px(theme::CHUNKY_RADIUS)),
                ))
                .with_children(|bottom| {
                    bottom.spawn(ui_kit::pixel_label(
                        font, "SELECT VESSEL", theme::FONT_LG, theme::ON_SURFACE_DIM,
                    ));
                    bottom.spawn((
                        Node {
                            flex_direction: FlexDirection::Row,
                            flex_wrap: FlexWrap::Wrap,
                            align_content: AlignContent::FlexStart,
                            column_gap: Val::Px(theme::GAP_SM),
                            row_gap: Val::Px(theme::GAP_SM),
                            ..default()
                        },
                        BackgroundColor(Color::NONE),
                    ))
                    .with_children(|grid| {
                        for &hull in HULL_ORDER {
                            spawn_hull_tile(grid, font, hull, hull == selected_hull);
                        }
                    });
                });
            });
        });
}

/// Hull declaration order — drives the tile grid layout (top-left to
/// bottom-right). Add a new hull to this list and it shows up in the
/// next free slot automatically.
const HULL_ORDER: &[Hull] = &[
    Hull::Default,
    Hull::GlassCannon,
    Hull::Rammer,
    Hull::Dreadnought,
    Hull::Privateer,
    Hull::Corsair,
    Hull::Harpooner,
    Hull::Revenant,
    Hull::Cutter,
    Hull::Marauder,
];

/// Ship-preview dimensions in UI pixels. Matches the internal
/// render-target aspect (32×48 spec px) at a 6× upscale — so the
/// chunky pixels read 6×6 screen px each, lining up with the
/// in-game ship's chunky-pixel look.
const PREVIEW_W: f32 = 192.0;
const PREVIEW_H: f32 = 288.0;

/// Build the ship preview inside the LHS top-card. Displays the
/// hull-preview render-target Image (managed by `dockyard_view`)
/// via an `ImageNode` — same pixel-art rasterisation as the in-game
/// ship hull. The render target itself is updated whenever the
/// player hovers a different hull tile, so this Node just needs to
/// hold the image.
fn spawn_ship_preview(parent: &mut ChildSpawnerCommands, image: &Handle<Image>) {
    parent.spawn((
        Node {
            width: Val::Px(PREVIEW_W),
            height: Val::Px(PREVIEW_H),
            ..default()
        },
        ImageNode {
            image: image.clone(),
            ..default()
        },
    ));
}

/// Per-hull silhouette tint for the preview. Hulls are otherwise
/// shape-identical, so a different tint per pick keeps the eye
/// anchored on the active selection. (Unused after the render-target
/// rework but kept available for fallback / future bevy_ui synthetic
/// previews.)
#[allow(dead_code)]
fn preview_hull_color(hull: Hull) -> Color {
    match hull {
        Hull::Default     => Color::srgb(0.78, 0.80, 0.86),
        Hull::GlassCannon => Color::srgb(0.55, 0.85, 0.90),
        Hull::Rammer      => Color::srgb(0.78, 0.50, 0.38),
        Hull::Dreadnought => Color::srgb(0.50, 0.55, 0.62),
        Hull::Privateer   => Color::srgb(0.95, 0.55, 0.30),
        Hull::Corsair     => Color::srgb(0.88, 0.78, 0.42),
        Hull::Harpooner   => Color::srgb(0.50, 0.70, 0.95),
        Hull::Revenant    => Color::srgb(0.70, 0.78, 0.88),
        Hull::Cutter      => Color::srgb(0.85, 0.85, 0.78),
        Hull::Marauder    => Color::srgb(0.85, 0.55, 0.25),
    }
}

/// Large PLAY action button — primary CTA. Sea-green chunky button;
/// hover lifts toward fresh lime so the eye lands on it.
fn spawn_play_button(parent: &mut ChildSpawnerCommands, font: &crate::fonts::PixelFont, label: &str) {
    let style = ChunkyButtonStyle::cta();
    parent
        .spawn((
            Button,
            Node {
                padding: UiRect::axes(Val::Px(theme::PAD_LG), Val::Px(theme::PAD_MD)),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                border: UiRect::all(Val::Px(theme::CHUNKY_BORDER_W)),
                ..default()
            },
            BackgroundColor(style.idle_fill),
            BorderColor(style.idle_outline),
            BorderRadius::all(Val::Px(theme::CHUNKY_RADIUS)),
            style,
            HullPlayButton,
        ))
        .with_children(|b| {
            b.spawn(ui_kit::pixel_label(font, label, theme::FONT_LG, theme::ON_CTA));
        });
}

/// BACK action button — neutral chunky button, smaller padding so it
/// reads as the secondary affordance next to PLAY.
fn spawn_back_button(parent: &mut ChildSpawnerCommands, font: &crate::fonts::PixelFont) {
    let style = ChunkyButtonStyle::neutral();
    parent
        .spawn((
            Button,
            Node {
                padding: UiRect::axes(Val::Px(theme::PAD_LG), Val::Px(theme::PAD_SM)),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                align_self: AlignSelf::Center,
                border: UiRect::all(Val::Px(theme::CHUNKY_BORDER_W)),
                ..default()
            },
            BackgroundColor(style.idle_fill),
            BorderColor(style.idle_outline),
            BorderRadius::all(Val::Px(theme::CHUNKY_RADIUS)),
            style,
            HullBackButton,
        ))
        .with_children(|b| {
            b.spawn(ui_kit::pixel_label(font, "BACK", theme::FONT_MD, theme::ON_SURFACE));
        });
}

/// One tile in the bottom-grid hull picker. Selected tile gets the
/// accent fill *locked* across all interaction states so hovering it
/// doesn't preview an unselected look; unselected tiles use the
/// neutral chunky palette so hover lifts the fill + outline.
fn spawn_hull_tile(parent: &mut ChildSpawnerCommands, font: &crate::fonts::PixelFont, hull: Hull, selected: bool) {
    let (style, text_color) = if selected {
        (ChunkyButtonStyle::locked(theme::CTA_FILL, theme::CHUNKY_OUTLINE), theme::ON_CTA)
    } else {
        (ChunkyButtonStyle::neutral(), theme::ON_SURFACE)
    };
    parent
        .spawn((
            Button,
            Node {
                width: Val::Px(110.0),
                height: Val::Px(64.0),
                padding: UiRect::all(Val::Px(theme::PAD_SM)),
                border: UiRect::all(Val::Px(theme::CHUNKY_BORDER_W)),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(style.idle_fill),
            BorderColor(style.idle_outline),
            BorderRadius::all(Val::Px(theme::CHUNKY_RADIUS)),
            style,
            HullCard(hull),
        ))
        .with_children(|t| {
            t.spawn(ui_kit::pixel_label(font, hull.label(), theme::FONT_MD, text_color));
        });
}

/// One difficulty pill (0 / 1 / 2). Active pill uses the locked
/// accent style (no hover shift); inactive pills use the neutral
/// chunky style.
fn spawn_difficulty_pill(
    parent: &mut ChildSpawnerCommands,
    font: &crate::fonts::PixelFont,
    value: u8,
    active: bool,
    locked: bool,
) {
    let (style, label_color) = if active {
        (ChunkyButtonStyle::locked(theme::CTA_FILL, theme::CHUNKY_OUTLINE), theme::ON_CTA)
    } else if locked {
        // Disabled palette: dim fill + muted outline + grey label.
        // Same `locked` chunky style as the active CTA but tinted
        // to read as "not available" — no hover-lift either.
        (
            ChunkyButtonStyle::locked(
                Color::srgb(0.18, 0.20, 0.24),
                Color::srgb(0.30, 0.32, 0.38),
            ),
            Color::srgb(0.45, 0.48, 0.55),
        )
    } else {
        (ChunkyButtonStyle::neutral(), theme::ON_SURFACE)
    };
    let label = crate::Difficulty(value).label();
    let mut entity = parent.spawn((
        Button,
        Node {
            // 2× the previous chip-style padding so the pill
            // reads as a proper button rather than a numpad key.
            padding: UiRect::axes(Val::Px(theme::PAD_LG * 1.5), Val::Px(theme::PAD_MD)),
            min_width: Val::Px(48.0),
            border: UiRect::all(Val::Px(theme::CHUNKY_BORDER_W)),
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            ..default()
        },
        BackgroundColor(style.idle_fill),
        BorderColor(style.idle_outline),
        BorderRadius::all(Val::Px(theme::CHUNKY_RADIUS)),
        style,
        DifficultyButton(value),
    ));
    if locked {
        entity.insert(LockedSelector);
    }
    entity.with_children(|pill| {
        pill.spawn(ui_kit::pixel_label(font, label, theme::FONT_LG * 1.6, label_color));
    });
}

/// One map-size pill. Active pill uses the locked accent style; the
/// rest fall back to the neutral chunky palette that hover-lifts.
fn spawn_map_size_pill(
    parent: &mut ChildSpawnerCommands,
    font: &crate::fonts::PixelFont,
    size: crate::map::MapSize,
    active: bool,
    locked: bool,
) {
    let (style, label_color) = if active {
        (ChunkyButtonStyle::locked(theme::CTA_FILL, theme::CHUNKY_OUTLINE), theme::ON_CTA)
    } else if locked {
        (
            ChunkyButtonStyle::locked(
                Color::srgb(0.18, 0.20, 0.24),
                Color::srgb(0.30, 0.32, 0.38),
            ),
            Color::srgb(0.45, 0.48, 0.55),
        )
    } else {
        (ChunkyButtonStyle::neutral(), theme::ON_SURFACE)
    };
    let mut entity = parent.spawn((
        Button,
        Node {
            padding: UiRect::axes(Val::Px(theme::PAD_MD), Val::Px(theme::PAD_SM)),
            border: UiRect::all(Val::Px(theme::CHUNKY_BORDER_W)),
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            ..default()
        },
        BackgroundColor(style.idle_fill),
        BorderColor(style.idle_outline),
        BorderRadius::all(Val::Px(theme::CHUNKY_RADIUS)),
        style,
        MapSizeButton(size),
    ));
    if locked {
        entity.insert(LockedSelector);
    }
    entity.with_children(|pill| {
        pill.spawn(ui_kit::pixel_label(font, size.label(), theme::FONT_MD, label_color));
    });
}

/// Right-panel content (title + tagline + buffs + nerfs + PLAY).
/// Lives inside the existing detail panel Node — caller is the
/// `with_children` closure of that node, so this fn spawns the
/// children directly.
fn spawn_detail_content(panel: &mut ChildSpawnerCommands, font: &crate::fonts::PixelFont, hull: Hull) {
    panel.spawn(ui_kit::pixel_label(
        font,
        hull.label(),
        DETAIL_TITLE_FONT,
        theme::ACCENT,
    ));
    panel.spawn((
        ui_kit::pixel_label(font, hull.tagline(), DETAIL_TAGLINE_FONT, theme::ON_SURFACE_DIM),
        Node {
            margin: UiRect::bottom(Val::Px(theme::GAP_SM)),
            ..default()
        },
    ));

    // Buffs — palette lime so they pop against the dark card.
    for b in hull.buffs() {
        panel.spawn(ui_kit::pixel_label(font, b.to_string(), DETAIL_BULLET_FONT, theme::BUFF_FG));
    }

    // Nerfs — palette orange (red would dissolve into the dark
    // surface at this font size).
    for n in hull.nerfs() {
        panel.spawn(ui_kit::pixel_label(font, n.to_string(), DETAIL_BULLET_FONT, theme::NERF_FG));
    }
}

pub fn exit_hull_select(
    mut commands: Commands,
    q: Query<Entity, With<HullSelectRoot>>,
    selected: Res<SelectedHull>,
    mut stats: ResMut<PlayerStats>,
    mut friendly: Query<
        &mut crate::components::Health,
        (With<crate::components::LocalPlayer>, Without<crate::multiplayer::ghost::RemoteGhost>),
    >,
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
    mut friendly: Query<
        &mut crate::components::Health,
        (With<crate::components::LocalPlayer>, Without<crate::multiplayer::ghost::RemoteGhost>),
    >,
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
    // LocalPlayer + Without<RemoteGhost> so a hull pick in MP
    // doesn't crush the host's ghost-of-peer Health back to the
    // host's max_hp (which triggers the `relay_ghost_damage`
    // insta-kill chain).
    friendly: &mut Query<
        &mut crate::components::Health,
        (With<crate::components::LocalPlayer>, Without<crate::multiplayer::ghost::RemoteGhost>),
    >,
) {
    *stats = PlayerStats::default();
    selected.0.apply(stats);
    let new_max = stats.max_hp();
    for mut h in friendly.iter_mut() {
        h.0 = new_max;
    }
}

/// Belt-and-braces clamp: every frame, hold the local friendly's
/// `Health.0` to ≤ `stats.max_hp()`. Catches any case where a stat
/// downgrade (hull pick, debug-panel HP-stat decrement) left the
/// live HP stale above the new cap.
///
/// `Without<RemoteGhost>` is critical in MP: the host-side ghost
/// has `Health(GHOST_HP_SENTINEL = 1_000_000)` as a damage absorber
/// for `relay_ghost_damage`. Without this exclusion, the clamp
/// would crush the ghost's HP from 1_000_000 down to the player's
/// `max_hp` every frame, the relay would interpret that as a
/// 999_925-damage hit, cap it at max_hp, and one-shot the peer.
pub fn clamp_hp_to_max(
    stats: Res<PlayerStats>,
    mut friendly: Query<
        &mut crate::components::Health,
        (With<crate::components::Friendly>, Without<crate::multiplayer::ghost::RemoteGhost>),
    >,
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
///
/// Multiplayer: PLAY flips `LocalReadyState.ready` instead of
/// advancing directly. Host's `host_advance_when_all_ready` watches
/// the team tracker and transitions `HullSelect → Playing` for
/// everyone once each peer has clicked.
pub fn handle_play_click(
    interactions: Query<&Interaction, (Changed<Interaction>, With<HullPlayButton>)>,
    mode: Res<crate::multiplayer::NetMode>,
    mut local_ready: ResMut<crate::multiplayer::ready::LocalReadyState>,
    mut next: ResMut<NextState<crate::AppState>>,
) {
    for interaction in &interactions {
        if matches!(*interaction, Interaction::Pressed) {
            if matches!(*mode, crate::multiplayer::NetMode::Solo) {
                next.set(crate::AppState::Playing);
            } else {
                local_ready.ready = true;
            }
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
    difficulty: Res<crate::Difficulty>,
    debug_visible: Res<crate::map::DebugUiVisible>,
    commands: Commands,
    q: Query<Entity, With<HullSelectRoot>>,
    state: Res<State<crate::AppState>>,
    hull_preview: Res<crate::dockyard_view::HullPreviewImage>,
    pixel_font: Res<crate::fonts::PixelFont>,
    mode: Res<crate::multiplayer::NetMode>,
) {
    if !selected.is_changed()
        && !preview.is_changed()
        && !map_size.is_changed()
        && !difficulty.is_changed()
        && !debug_visible.is_changed()
    {
        return;
    }
    if *state.get() != crate::AppState::HullSelect { return; }
    let mut commands = commands;
    for e in &q {
        commands.entity(e).despawn();
    }
    let panel_hull = preview.0.unwrap_or(selected.0);
    spawn_overlay(commands, &pixel_font, panel_hull, selected.0, *map_size, *difficulty, &hull_preview.0, &mode, debug_visible.0);
}

/// Click handler — commit the player's pick to the `MapSize` resource.
/// The rebuild system above picks up the change and re-tints the pills.
pub fn handle_map_size_click(
    interactions: Query<
        (&Interaction, &MapSizeButton, bevy::ecs::query::Has<LockedSelector>),
        Changed<Interaction>,
    >,
    mut map_size: ResMut<crate::map::MapSize>,
) {
    for (interaction, btn, locked) in &interactions {
        if locked { continue; }
        if matches!(*interaction, Interaction::Pressed) {
            if *map_size != btn.0 {
                *map_size = btn.0;
            }
            return;
        }
    }
}

/// Click handler — commit the player's difficulty pick. The rebuild
/// system picks up the change and re-tints the active pill.
pub fn handle_difficulty_click(
    interactions: Query<
        (&Interaction, &DifficultyButton, bevy::ecs::query::Has<LockedSelector>),
        Changed<Interaction>,
    >,
    mut difficulty: ResMut<crate::Difficulty>,
) {
    for (interaction, btn, locked) in &interactions {
        if locked { continue; }
        if matches!(*interaction, Interaction::Pressed) {
            if difficulty.0 != btn.0 {
                difficulty.0 = btn.0;
            }
            return;
        }
    }
}
