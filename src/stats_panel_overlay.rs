//! Shared current-stats panel used by every fullscreen bevy_ui overlay
//! that needs to show the player's current numbers (level-up screen,
//! boss-reward screen). Identical look + behaviour across both:
//! `CURRENT STATS` header, one row per `StatKind`, value tinted
//! green/red vs. baseline, hover surfaces a stat description in the
//! tooltip slot at the bottom of the panel.
//!
//! NOT used by the customize/shop screen — that panel lives on the
//! `CUSTOMIZE_LAYER` render target (chunky-pixel `Text2d`, world-space
//! positioning), a fundamentally different rendering path. If both
//! flavours need to stay visually consistent, the shop side has to be
//! retuned at the spec-pixel level — they can't share bevy_ui entities.

use bevy::prelude::*;
use bevy::text::FontSmoothing;

use crate::stats::{PlayerStats, StatKind};
use crate::ui_kit::{self, theme};

/// Plugin registration — owns just the hover-driven tooltip syncer.
/// Runs unconditionally so any screen that spawns this panel gets
/// hover behaviour without needing to gate on its own state.
pub struct StatsPanelOverlayPlugin;

impl Plugin for StatsPanelOverlayPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<HighlightedStats>()
            // Clear the highlight set first thing each frame; the
            // hover producers (level-up cards, shop mod cards)
            // refill it later in Update. No-hover state = empty set.
            .add_systems(First, |mut h: bevy::prelude::ResMut<HighlightedStats>| {
                h.kinds.clear();
            })
            // Tooltip is hover-driven via `Changed<Interaction>`.
            // The row-tint consumer reads `HighlightedStats` so it
            // ordered AFTER the customize producer + the level-up
            // producer — without these `.after`s, the producer
            // sometimes ran in the same frame AFTER this consumer,
            // and the next-frame interleaving flicker showed up
            // as "highlight blinks on/off every other frame".
            .add_systems(
                Update,
                (
                    update_stat_panel_tooltip,
                    apply_stat_panel_highlight
                        .after(crate::customize::shop_mods::update_mod_hover_highlight)
                        .after(crate::xp::update_level_up_tooltip),
                ),
            );
    }
}

/// Marker on each stat-row Button. The tooltip syncer reads
/// `Interaction` + this kind to populate the description text.
#[derive(Component, Clone, Copy)]
pub struct StatPanelRow(pub StatKind);

/// Sign of a hover-highlight entry. Producers tag each affected
/// stat with `Buff` (delta positive, paint green) or `Nerf`
/// (delta negative, paint red). When two producers conflict on
/// the same stat the later writer wins — fine in practice since
/// only one card is hovered at a time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HighlightSign { Buff, Nerf }

/// One affected stat in a hover-highlight set. `delta` + `to_flat`
/// describe how the producer would apply the change to a `Stat`:
/// `to_flat=true` adds to `.flat`, `to_flat=false` adds to `.percent`.
/// The consumer (customize stats panel) clones `PlayerStats`,
/// re-applies the delta into a probe copy, and renders both the
/// current and the would-be value in the row.
#[derive(Clone, Copy, Debug)]
pub struct HighlightEntry {
    pub sign: HighlightSign,
    pub delta: f32,
    pub to_flat: bool,
}

/// Shared cross-screen hover-highlight signal. Producers (level-up
/// buff-card hover, shop mod-card hover) write each affected
/// `StatKind` along with the sign of the change + the delta the
/// producer would apply; consumers tint rows (sign-based) AND
/// render before / after numbers (delta-based).
///
/// Cleared every frame by `First` — no entry means no hover.
#[derive(bevy::prelude::Resource, Default, Clone)]
pub struct HighlightedStats {
    pub kinds: std::collections::HashMap<StatKind, HighlightEntry>,
}

/// Marker on the tooltip text node. One per spawned panel.
#[derive(Component)]
pub struct StatPanelTooltip;

/// Spawn the panel as a child of `parent`. Renders a fixed-width
/// column with a header, one row per stat, and a tooltip slot.
/// `tooltip_min_h` is the reserved vertical space for the tooltip
/// so the panel doesn't reflow when hover toggles visibility.
pub fn spawn_stats_panel(parent: &mut ChildSpawnerCommands, stats: &PlayerStats) {
    let baseline = PlayerStats::default();
    parent
        .spawn((
            Node {
                width: Val::Px(280.0),
                padding: UiRect::all(Val::Px(theme::PAD_LG)),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(theme::GAP_SM + 2.0),
                ..default()
            },
            BackgroundColor(theme::SURFACE_RAISED),
            BorderColor(theme::BORDER_SUBTLE),
        ))
        .with_children(|panel| {
            panel.spawn(ui_kit::label(
                "CURRENT STATS",
                theme::FONT_LG,
                theme::ACCENT,
            ));

            for &kind in StatKind::ALL {
                let cur = kind.stat(stats).effective();
                let base = kind.stat(&baseline).effective();
                let value_color = if cur > base + 0.001 {
                    theme::BUFF_FG
                } else if cur < base - 0.001 {
                    theme::NERF_FG
                } else {
                    theme::ON_SURFACE
                };
                panel
                    .spawn((
                        // `Button` is required for `Interaction` events
                        // — the tooltip syncer reads them.
                        Button,
                        Node {
                            flex_direction: FlexDirection::Row,
                            justify_content: JustifyContent::SpaceBetween,
                            column_gap: Val::Px(theme::GAP_MD),
                            padding: UiRect::vertical(Val::Px(1.0)),
                            ..default()
                        },
                        BackgroundColor(Color::NONE),
                        StatPanelRow(kind),
                    ))
                    .with_children(|row| {
                        row.spawn(ui_kit::label(
                            kind.label(),
                            theme::FONT_MD,
                            theme::ON_SURFACE_DIM,
                        ));
                        row.spawn((
                            Text::new(kind.format_value(stats, None)),
                            TextFont {
                                font_size: theme::FONT_MD,
                                font_smoothing: FontSmoothing::None,
                                ..default()
                            },
                            TextColor(value_color),
                        ));
                    });
            }

            // Tooltip slot. Fixed min-height so hover toggling
            // doesn't reflow the panel above it.
            panel
                .spawn((
                    Node {
                        width: Val::Percent(100.0),
                        min_height: Val::Px(40.0),
                        padding: UiRect::top(Val::Px(theme::GAP_MD)),
                        ..default()
                    },
                    BackgroundColor(Color::NONE),
                ))
                .with_children(|hint| {
                    hint.spawn((
                        Text::new(""),
                        TextFont {
                            font_size: theme::FONT_SM,
                            font_smoothing: FontSmoothing::None,
                            ..default()
                        },
                        TextColor(theme::ON_SURFACE_DIM),
                        TextLayout::new_with_justify(JustifyText::Left),
                        Node {
                            max_width: Val::Px(260.0),
                            ..default()
                        },
                        Visibility::Hidden,
                        StatPanelTooltip,
                    ));
                });
        });
}

/// Per-frame: any hovered `StatPanelRow` writes its description into
/// every `StatPanelTooltip` and reveals it; leaving the row hides it.
/// `Changed<Interaction>` keeps the system idle most frames — it
/// only fires on hover enter/exit, not while held.
///
/// Works across multiple screens because we tag rows + tooltip with
/// the same markers regardless of which screen spawned them. When
/// neither screen is up there are no entities and the system is a
/// no-op.
/// Tint matching rows in the shared bevy_ui stats panel based on
/// `HighlightedStats`. Writes a translucent accent backdrop on
/// rows whose `StatKind` is currently being targeted by a hovered
/// mod/buff card; reverts to transparent when not.
pub fn apply_stat_panel_highlight(
    highlight: bevy::prelude::Res<HighlightedStats>,
    mut rows: bevy::prelude::Query<(&StatPanelRow, &mut bevy::prelude::BackgroundColor)>,
) {
    // Translucent green / red wash sized to match the row text's
    // BUFF_FG / NERF_FG so the backdrop colour-codes the change
    // sign at a glance. Alpha low enough that text stays legible.
    let on_buff = bevy::prelude::Color::srgba(0.45, 0.92, 0.55, 0.20);
    let on_nerf = bevy::prelude::Color::srgba(0.95, 0.45, 0.45, 0.20);
    let off = bevy::prelude::Color::NONE;
    for (row, mut bg) in &mut rows {
        let want = match highlight.kinds.get(&row.0).map(|e| e.sign) {
            Some(HighlightSign::Buff) => on_buff,
            Some(HighlightSign::Nerf) => on_nerf,
            None => off,
        };
        if bg.0 != want { bg.0 = want; }
    }
}

pub fn update_stat_panel_tooltip(
    rows: Query<(&Interaction, &StatPanelRow), Changed<Interaction>>,
    stats: Res<crate::stats::PlayerStats>,
    mut tooltips: Query<(&mut Text, &mut Visibility), With<StatPanelTooltip>>,
) {
    for (interaction, row) in &rows {
        let (text_value, vis_value) = match *interaction {
            Interaction::Hovered | Interaction::Pressed => (
                format!("{}: {}", row.0.label(), row.0.dynamic_description(&stats)),
                Visibility::Inherited,
            ),
            Interaction::None => (String::new(), Visibility::Hidden),
        };
        for (mut text, mut vis) in &mut tooltips {
            if text.0 != text_value {
                text.0 = text_value.clone();
            }
            if *vis != vis_value {
                *vis = vis_value;
            }
        }
    }
}
