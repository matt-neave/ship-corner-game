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
use rand::seq::SliceRandom;

use crate::stats::{PlayerStats, StatKind};
use crate::ui_kit::{self, theme};
use crate::AppState;

/// Owns the XP + level-up overlay: its four resources, the
/// state-gated click handler, and the spawn/despawn pair on
/// `LevelUp` enter/exit. The persistent XP bar update
/// (`update_xp_bar`) lives in the map UI bucket and is left to
/// main's wiring because it shares the same Update tuple as other
/// map-frame HUD systems.
pub struct LevelUpPlugin;

impl Plugin for LevelUpPlugin {
    fn build(&self, app: &mut App) {
        app
            .insert_resource(Xp::default())
            .insert_resource(LevelUpsPending::default())
            .insert_resource(LevelUpReturn::default())
            .insert_resource(LevelUpChoices::default())
            .add_systems(OnEnter(AppState::LevelUp), enter_level_up)
            .add_systems(OnExit(AppState::LevelUp), exit_level_up)
            .add_systems(
                Update,
                (handle_level_up_click, reveal_level_up_after_layout)
                    .run_if(in_state(AppState::LevelUp)),
            );
    }
}

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

/// Where to send the player when the LevelUp screen finishes draining
/// its queue. `None` = default → `Customize` (the post-stage flow used
/// by `tick_stage_complete`). `Some(state)` = override, set by
/// `spawn_enemies` when level-ups are drained mid-stage between waves;
/// the player rejoins combat instead of being yanked into the shop.
///
/// Cleared back to `None` by `handle_level_up_click` once consumed so a
/// stale override can't leak into the next stage.
#[derive(Resource, Default, Debug)]
pub struct LevelUpReturn(pub Option<crate::AppState>);

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
    /// Card label, e.g. `+25 HP`, `+10% RANGE`. Routes through
    /// `StatKind::format_delta` so the value renders in the same
    /// units as the shop mod cards (and the stats panel readout)
    /// without a second per-stat formatter.
    pub fn label(&self) -> String {
        format!("{} {}", self.kind.format_delta(self.delta), self.kind.label())
    }

    pub fn apply(&self, stats: &mut PlayerStats) {
        // Buffs apply to `stat.flat` so a delta from a level-up means
        // the same thing as the same delta from a shop mod card.
        let s = self.kind.stat_mut(stats);
        s.flat += self.delta;
        let _ = self.flat;
    }
}

/// Master list of buffs the level-up screen can roll. Deltas mirror
/// `StatKind::debug_step` so a level-up pick is exactly equivalent
/// to a shop mod purchase — no surprise that "+10 RANGE" from a
/// level-up means the same as "+10 RANGE" from a mod card.
fn buff_pool() -> Vec<Buff> {
    StatKind::ALL
        .iter()
        .copied()
        .map(|kind| Buff { kind, delta: kind.debug_step(), flat: true })
        .collect()
}

/// Pick `n` distinct buffs from the master pool. Order randomized.
fn pick_buffs(n: usize) -> Vec<Buff> {
    let mut pool = buff_pool();
    let mut rng = rand::thread_rng();
    pool.shuffle(&mut rng);
    pool.into_iter().take(n).collect()
}

// ---------- XP grant ----------

/// Per-kill XP value, scaled by the player's `XpHarvest` stat.
/// Call from `enemy_death_check`. `is_boss` triggers the 5× boss
/// bonus baseline. Effective amount = base × (1 + xp_harvest/100),
/// rounded; minimum 1 so a kill always nets at least one point.
pub fn grant_kill_xp(
    xp: &mut Xp,
    pending: &mut LevelUpsPending,
    stats: &crate::stats::PlayerStats,
    is_boss: bool,
) {
    let base = if is_boss { 5 } else { 1 } as f32;
    let mult = 1.0 + stats.xp_harvest_pct.effective() / 100.0;
    let amount = (base * mult).round().max(1.0) as u32;
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

/// Spawn the XP track as a child of `WaveHpUi`. Mirrors the HP
/// track's chrome exactly — same 180×22 dimensions, 2 px border at
/// `BORDER_DARK`, `BORDER_SUBTLE` surface — so both rails read as
/// one UI family. `update_hp_bar_pixel_scale` re-derives the border
/// width every frame from the play-area upscale, keeping it
/// pixel-aligned with the play-area's grey frame.
pub fn spawn_xp_track(parent: &mut ChildSpawnerCommands) {
    parent
        .spawn((
            Node {
                width: Val::Px(XP_BAR_WIDTH),
                height: Val::Px(XP_BAR_HEIGHT),
                border: UiRect::all(Val::Px(2.0)),
                position_type: PositionType::Relative,
                overflow: Overflow::clip(),
                ..default()
            },
            BackgroundColor(theme::BORDER_SUBTLE),
            BorderColor(theme::BORDER_DARK),
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
    xp: Res<Xp>,
    mut fills: Query<&mut Node, With<XpBarFill>>,
    mut labels: Query<&mut Text, With<XpBarLabel>>,
) {
    // The XP track now lives inside the HP bar's outer frame
    // (see `setup_hud`), so visibility + positioning are inherited
    // from the parent `WaveHpUi`. This system only drives the
    // gold-fill width + "LV N" text — everything else is layout.
    let need = xp_to_next(xp.level).max(1);
    let pct = (xp.current as f32 / need as f32).clamp(0.0, 1.0) * 100.0;

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

/// Reveal the overlay only once Bevy UI has actually finished computing
/// positions for it. Spawned `Hidden` + `HideUntilLayout`. Each Update:
///   - If the root's `ComputedNode.size` is still zero → layout hasn't
///     run yet, keep waiting.
///   - Once `size > 0` we record it and require one additional frame
///     before revealing, so text-glyph metrics get baked in and the
///     final layout pass settles. (Otherwise we reveal on the frame
///     layout first runs with default text sizes, then the next frame
///     the text re-measures and rows shift — the visible "jump".)
///
/// More robust than a fixed frame counter: large overlays with lots of
/// text can take >1 frame to stabilise on slow machines, and a counter
/// can't tell whether layout actually converged.
pub fn reveal_level_up_after_layout(
    mut commands: Commands,
    mut q: Query<(Entity, &mut Visibility, &mut HideUntilLayout, &ComputedNode)>,
) {
    for (e, mut vis, mut marker, computed) in &mut q {
        let size = computed.size();
        if size.x <= 0.0 || size.y <= 0.0 {
            // Layout hasn't run yet — keep hidden, no countdown.
            continue;
        }
        if marker.0 > 0 {
            // Layout ran. Burn one extra frame for text-glyph
            // metrics to settle before flipping visible.
            marker.0 -= 1;
            continue;
        }
        if *vis != Visibility::Inherited { *vis = Visibility::Inherited; }
        commands.entity(e).remove::<HideUntilLayout>();
    }
}

/// Stability counter consumed *after* the root's `ComputedNode.size`
/// becomes non-zero. Spawn with `frames=1` so we wait one full Update
/// past the first valid layout pass before revealing. See
/// `reveal_level_up_after_layout`.
#[derive(Component)]
pub struct HideUntilLayout(pub u8);

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
    mut return_state: ResMut<LevelUpReturn>,
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
            // Honour the override set by `spawn_enemies` for mid-stage
            // (between-wave) drains — return the player to combat
            // instead of routing through the shop. Falls back to
            // Customize for the post-stage path. Cleared so a stale
            // override can't leak into the next level-up.
            let dest = return_state.0.take().unwrap_or(crate::AppState::Customize);
            next.set(dest);
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
            // Hidden until bevy_ui has actually laid out the tree
            // (otherwise the stats panel + buff cards flash at
            // default/uncomputed positions, then snap into place).
            // `reveal_level_up_after_layout` watches `ComputedNode`
            // and reveals after one stable layout pass.
            Visibility::Hidden,
            HideUntilLayout(1),
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
            // their current numbers while choosing the buff. Centre
            // alignment so the 3 buff cards float vertically in the
            // taller stats panel's height instead of pinning to its
            // top edge.
            root.spawn((
                Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
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
                // Wider + bigger font than before so the 13-row list
                // doesn't compress into an unreadable column. Panel
                // height grows naturally to fit the rows.
                cols.spawn((
                    Node {
                        width: Val::Px(260.0),
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
                                    theme::FONT_MD,
                                    theme::ON_SURFACE_DIM,
                                ));
                                stat_row.spawn(ui_kit::label(
                                    kind.format_value(stats, None),
                                    theme::FONT_MD,
                                    theme::ON_SURFACE,
                                ));
                            });
                    }
                });
            });
        });
}
