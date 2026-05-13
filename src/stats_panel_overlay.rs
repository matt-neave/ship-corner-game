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
        app.add_systems(Update, update_stat_panel_tooltip);
    }
}

/// Marker on each stat-row Button. The tooltip syncer reads
/// `Interaction` + this kind to populate the description text.
#[derive(Component, Clone, Copy)]
pub struct StatPanelRow(pub StatKind);

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
pub fn update_stat_panel_tooltip(
    rows: Query<(&Interaction, &StatPanelRow), Changed<Interaction>>,
    mut tooltips: Query<(&mut Text, &mut Visibility), With<StatPanelTooltip>>,
) {
    for (interaction, row) in &rows {
        let (text_value, vis_value) = match *interaction {
            Interaction::Hovered | Interaction::Pressed => (
                format!("{}: {}", row.0.label(), row.0.description()),
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
