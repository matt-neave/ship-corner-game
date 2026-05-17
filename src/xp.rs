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
                (
                    handle_level_up_click,
                    reveal_level_up_after_layout,
                    update_level_up_button_focus,
                    update_level_up_tooltip,
                )
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

/// Bundled SystemParam for the two level-up resources `spawn_enemies`
/// needs to mutate when a mid-stage threshold crossing should bounce
/// the player to the LevelUp overlay. Wrapping them into one
/// `SystemParam` keeps `spawn_enemies` under Bevy's 16-param cap.
#[derive(bevy::ecs::system::SystemParam)]
pub struct LevelUpQueue<'w> {
    pub pending: Res<'w, LevelUpsPending>,
    pub return_state: ResMut<'w, LevelUpReturn>,
}

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

/// Master list of buffs the level-up screen can roll. Deltas use
/// `StatKind::upgrade_step` — the conservative player-facing step,
/// not the larger `debug_step` (which is for the dev `+/-`
/// buttons). Pulls from `StatKind::ROLLABLE` (not `ALL`) so
/// `TurretArcBonus` / `TurretTurnSpeed` never come up as picks
/// while still being configurable from the stats panel.
fn buff_pool() -> Vec<Buff> {
    StatKind::ROLLABLE
        .iter()
        .copied()
        .map(|kind| Buff { kind, delta: kind.upgrade_step(), flat: true })
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
    let mult = stats.xp_harvest_mult();
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
pub const XP_BAR_WIDTH: f32 = 150.0;
pub const XP_BAR_HEIGHT: f32 = 16.0;
const XP_BAR_FILL_COLOR: Color = Color::srgb(1.0, 0.78, 0.20);

/// Spawn the XP track as a child of `WaveHpUi`. Mirrors the HP
/// track's chrome exactly — same 180×22 dimensions, 2 px border at
/// `BORDER_DARK`, `BORDER_SUBTLE` surface — so both rails read as
/// one UI family. `update_hp_bar_pixel_scale` re-derives the border
/// width every frame from the play-area upscale, keeping it
/// pixel-aligned with the play-area's grey frame.
pub fn spawn_xp_track(parent: &mut ChildSpawnerCommands, thaleah: &crate::fonts::ThaleahFont) {
    parent
        .spawn((
            Node {
                width: Val::Px(XP_BAR_WIDTH),
                height: Val::Px(XP_BAR_HEIGHT),
                border: UiRect::all(Val::Px(theme::CHUNKY_BORDER_W)),
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
                    // Thaleah Fat + drop shadow — same voice as the
                    // WAVE / LEVEL N! / GAME OVER chrome, sized to
                    // fit inside the bar without dominating it.
                    Text::new("LV 1"),
                    crate::fonts::thaleah_text_font(thaleah, 14.0),
                    TextColor(theme::ON_SURFACE),
                    TextShadow {
                        offset: Vec2::splat(1.0),
                        color: Color::srgba(0.0, 0.0, 0.0, 0.95),
                    },
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
    thaleah: Res<crate::fonts::ThaleahFont>,
    mut sfx: crate::sfx::SfxPlayer,
) {
    choices.buffs = pick_buffs(4);
    spawn_level_up_overlay(&mut commands, &choices, &xp, &stats, &thaleah);
    sfx.play(crate::sfx::Sfx::LevelUp);
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
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    xp: Res<Xp>,
    thaleah: Res<crate::fonts::ThaleahFont>,
    mode: Res<crate::multiplayer::NetMode>,
    mut local_ready: ResMut<crate::multiplayer::ready::LocalReadyState>,
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
            spawn_level_up_overlay(&mut commands, &choices, &xp, &stats, &thaleah);
        } else {
            // Honour the override set by `spawn_enemies` for mid-stage
            // (between-wave) drains — return the player to combat
            // instead of routing through the shop. Falls back to
            // Customize for the post-stage path. Cleared so a stale
            // override can't leak into the next level-up.
            //
            // Multiplayer: don't consume the override or transition
            // ourselves. Each peer flips `LocalReadyState.ready`;
            // host's `host_advance_when_all_ready` consumes the
            // override (or falls back to Customize) and transitions
            // everyone together via the state-sync pipeline.
            //
            // The overlay is despawned in both paths so the cards
            // vanish immediately on the local pick.
            for e in &overlay {
                commands.entity(e).despawn();
            }
            if matches!(*mode, crate::multiplayer::NetMode::Solo) {
                let dest = return_state.0.take().unwrap_or(crate::AppState::Customize);
                if matches!(dest, crate::AppState::Customize) {
                    crate::stage_complete::spawn_transition(
                        &mut commands, &mut meshes, &mut materials, dest,
                    );
                } else {
                    next.set(dest);
                }
            } else {
                local_ready.ready = true;
            }
        }
        return;
    }
}

/// Hover / press focus tinting for the level-up cards. Mirrors the
/// main-menu button feel: idle fill = `SURFACE_RAISED` + gold
/// `ACCENT` outline; hover lifts the fill to `CHUNKY_FILL_HOVER` and
/// brightens the outline to a pale gold; press deepens the fill to
/// `CHUNKY_FILL_PRESS`. Watches `Changed<Interaction>` so it only
/// runs when a card's state actually flips.
pub fn update_level_up_button_focus(
    mut cards: Query<
        (&Interaction, &mut BackgroundColor, &mut BorderColor),
        (Changed<Interaction>, With<LevelUpButton>),
    >,
) {
    // Brighter gold for the hover ring — pale enough to read as a
    // wake-up over the resting accent gold.
    const ACCENT_HOVER: Color = Color::srgb(1.0, 0.92, 0.65);
    for (interaction, mut bg, mut border) in &mut cards {
        let (want_bg, want_border) = match *interaction {
            Interaction::Hovered => (theme::CHUNKY_FILL_HOVER, ACCENT_HOVER),
            Interaction::Pressed => (theme::CHUNKY_FILL_PRESS, ACCENT_HOVER),
            Interaction::None    => (theme::SURFACE_RAISED, theme::ACCENT),
        };
        if bg.0 != want_bg { bg.0 = want_bg; }
        if border.0 != want_border { border.0 = want_border; }
    }
}

/// Build the overlay tree. Shared between `enter_level_up` and the
/// click-handler reroll path so the layout stays in one place.
fn spawn_level_up_overlay(
    commands: &mut Commands,
    choices: &LevelUpChoices,
    xp: &Xp,
    stats: &PlayerStats,
    thaleah: &crate::fonts::ThaleahFont,
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
            // Title in the same voice as the GAME OVER overlay —
            // Thaleah Fat + drop shadow — so the level-up beat sits
            // in the same typographic family as the rest of the
            // big-moment overlays. Gold tint matches the XP bar.
            root.spawn((
                Text::new(format!("LEVEL {}!", xp.level)),
                crate::fonts::thaleah_text_font(thaleah, 56.0),
                TextColor(theme::ACCENT),
                TextShadow {
                    offset: Vec2::splat(2.0),
                    color: Color::srgba(0.0, 0.0, 0.0, 0.85),
                },
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
                // ---- LEFT: buff cards + tooltip strip stacked
                // vertically. The tooltip sits IMMEDIATELY below
                // the card row, width-matched to the row so it
                // never extends across into the stats panel.
                cols.spawn((
                    Node {
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::Center,
                        row_gap: Val::Px(theme::GAP_LG),
                        ..default()
                    },
                    BackgroundColor(Color::NONE),
                ))
                .with_children(|left| {
                    // Buff-card row.
                    left.spawn((
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
                                    border: UiRect::all(Val::Px(theme::CHUNKY_BORDER_W)),
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

                    // Tooltip strip — width matched to the card row
                    // (4 × 120 + 3 × GAP_LG). Absolute-positioned BELOW
                    // the card row so multi-line stat descriptions
                    // can grow downward without re-flowing the
                    // centered column (which would push the cards
                    // up every time a longer description shows up).
                    // `top: 100%` anchors to the bottom edge of the
                    // parent left-column, which auto-sizes to just
                    // the card row's height since this strip no
                    // longer participates in the column layout.
                    let row_w = 120.0 * choices.buffs.len() as f32
                        + theme::GAP_LG * (choices.buffs.len().saturating_sub(1) as f32);
                    left.spawn((
                        Node {
                            position_type: PositionType::Absolute,
                            top: Val::Percent(100.0),
                            left: Val::Px(0.0),
                            margin: UiRect::top(Val::Px(theme::GAP_LG)),
                            width: Val::Px(row_w),
                            padding: UiRect::all(Val::Px(theme::PAD_MD)),
                            border: UiRect::all(Val::Px(theme::CHUNKY_BORDER_W)),
                            align_items: AlignItems::Center,
                            justify_content: JustifyContent::Center,
                            ..default()
                        },
                        BackgroundColor(theme::SURFACE_RAISED),
                        BorderColor(theme::CHUNKY_OUTLINE),
                        LevelUpTooltipRoot,
                    ))
                    .with_children(|tip| {
                        tip.spawn((
                            Text::new(""),
                            TextFont { font_size: theme::FONT_MD, ..default() },
                            TextColor(theme::ON_SURFACE),
                            LevelUpTooltipText,
                        ));
                    });
                });

                // ---- RIGHT: current-stats panel ----
                // Shared helper — same panel as the boss-defeat screen
                // (buff/nerf tinting, hover tooltips, fixed width).
                crate::stats_panel_overlay::spawn_stats_panel(cols, stats);
            });
        });
}

/// Marker on the bordered tooltip strip below the buff cards.
#[derive(Component)]
pub struct LevelUpTooltipRoot;

/// Marker on the Text inside `LevelUpTooltipRoot`.
#[derive(Component)]
pub struct LevelUpTooltipText;

/// Per-frame: scan the buff cards for `Interaction::Hovered`, look
/// up the buff's `StatKind`, and write its dynamic description to
/// the tooltip strip. Falls back to a blank string when no card is
/// hovered, so the line vanishes the instant the cursor leaves.
pub fn update_level_up_tooltip(
    choices: Res<LevelUpChoices>,
    stats: Res<PlayerStats>,
    cards: Query<(&Interaction, &LevelUpButton)>,
    mut text_q: Query<&mut Text, With<LevelUpTooltipText>>,
    mut root_q: Query<&mut Visibility, With<LevelUpTooltipRoot>>,
) {
    let hovered = cards.iter().find_map(|(i, btn)| {
        matches!(*i, Interaction::Hovered | Interaction::Pressed).then_some(btn.idx)
    });
    let want = hovered
        .and_then(|idx| choices.buffs.get(idx))
        .map(|b| b.kind.dynamic_description(&stats))
        .unwrap_or_default();
    for mut t in &mut text_q {
        if t.0 != want { **t = want.clone(); }
    }
    // Hide the whole bordered strip when nothing's hovered so the
    // empty box doesn't sit there demanding attention.
    let want_vis = if hovered.is_some() {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    for mut v in &mut root_q {
        if *v != want_vis { *v = want_vis; }
    }
}
