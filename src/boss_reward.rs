//! Boss-defeat reward screen — sits between StageComplete and LevelUp
//! when the just-cleared section had a `boss_class`.
//!
//! Three mutually-exclusive options, level-up-style cards:
//! 1. **Recruit** the defeated boss as a permanent ally.
//! 2. **Bounty** — a fat scrap drop scaled by section star tier.
//! 3. **Super mod** — a single random game-changing stat trade
//!    (e.g. +60% damage / -30 HP). Big numbers both ways so the pick
//!    actually reshapes the build instead of just nudging it.
//!
//! `BossRewardPending` is the gate: populated by `level_complete_check`
//! when the cleared section's `boss_class` was `Some`, drained by the
//! BossReward state on entry.
//!
//! Recruited allies are kept in `RecruitedAllies` as a *permanent
//! roster*. `respawn_allies_for_stage` runs on `OnEnter(Playing)` and
//! despawns every live `Ally` then respawns one from each class in
//! the roster — so the fleet shows up at full HP every stage, with
//! no carry-over damage from the previous run.

use bevy::prelude::*;
use rand::seq::SliceRandom;

use crate::ally::{Ally, ShipClass};
use crate::stats::{PlayerStats, StatKind};
use crate::ui_kit::{self, theme};
use crate::xp::Buff;
use crate::AppState;

/// Owns the boss-reward screen + the supporting persistent roster.
/// The cross-state hooks (`respawn_allies_for_stage` on `OnExit(Map)`,
/// `reset_boss_reward_state` on `OnEnter(MainMenu)` /
/// `OnExit(GameOver)`) live here too because they read or clear
/// resources owned by this plugin.
pub struct BossRewardPlugin;

impl Plugin for BossRewardPlugin {
    fn build(&self, app: &mut App) {
        app
            .insert_resource(BossRewardPending::default())
            .insert_resource(RecruitedAllies::default())
            .insert_resource(BossRewardOffer::default())
            // Per-section roster refresh — respawn the recruited fleet
            // at full HP whenever the player commits to a new combat.
            .add_systems(OnExit(AppState::Map), respawn_allies_for_stage)
            // Wipe the roster + clear any in-flight reward offer when
            // the run resets, so a fresh MainMenu / restart starts
            // clean.
            .add_systems(OnEnter(AppState::MainMenu), reset_boss_reward_state)
            .add_systems(OnExit(AppState::GameOver), reset_boss_reward_state)
            // The screen itself.
            .add_systems(OnEnter(AppState::BossReward), enter_boss_reward)
            .add_systems(OnExit(AppState::BossReward), exit_boss_reward)
            .add_systems(
                Update,
                (handle_boss_reward_click, update_boss_reward_stat_tooltip)
                    .run_if(in_state(AppState::BossReward)),
            );
    }
}

// ---------- Resources ----------

/// `Some(class)` when the player just cleared a boss section and the
/// reward screen hasn't run yet. Set by `level_complete_check`,
/// drained by `enter_boss_reward`. `None` otherwise.
#[derive(Resource, Default)]
pub struct BossRewardPending(pub Option<ShipClass>);

/// Permanent roster of boss ships the player has recruited. Persists
/// across stages; `respawn_allies_for_stage` rebuilds the live fleet
/// from this list on every `OnEnter(Playing)` so the recruits show
/// up at full HP each combat.
#[derive(Resource, Default)]
pub struct RecruitedAllies(pub Vec<ShipClass>);

/// The 3 cards currently on offer. Re-rolled each `OnEnter(BossReward)`
/// so the SuperMod and bounty values are fresh per encounter.
#[derive(Resource, Default)]
pub struct BossRewardOffer {
    pub boss: Option<ShipClass>,
    pub bounty_scrap: u32,
    pub super_mod: Option<SuperMod>,
}

// ---------- Super mod catalog ----------

/// One super-mod card. Composed from one positive `Buff` and one (or
/// more) negative — picking it applies every entry to PlayerStats.
#[derive(Clone)]
pub struct SuperMod {
    pub name: &'static str,
    pub effects: Vec<Buff>,
}

impl SuperMod {
    /// Subtitle line that summarises the trade — "+60% TURRET DMG  /
    /// -30 HP". Reads off the `Buff` deltas directly so a future
    /// change to the catalog auto-updates the UI.
    pub fn summary(&self) -> String {
        self.effects
            .iter()
            .map(|b| b.label())
            .collect::<Vec<_>>()
            .join("   ")
    }
}

fn super_mod_catalog() -> Vec<SuperMod> {
    vec![
        SuperMod {
            name: "GLASS CANNON",
            effects: vec![
                Buff { kind: StatKind::TurretDamage, delta: 60.0, flat: true },
                Buff { kind: StatKind::Hp,           delta: -30.0, flat: true },
            ],
        },
        SuperMod {
            name: "HEAVY PLATING",
            effects: vec![
                Buff { kind: StatKind::Hp,        delta:  60.0, flat: true },
                Buff { kind: StatKind::MoveSpeed, delta: -10.0, flat: true },
            ],
        },
        SuperMod {
            name: "LONG LENS",
            effects: vec![
                Buff { kind: StatKind::Range,        delta:  40.0, flat: true },
                Buff { kind: StatKind::TurretDamage, delta: -25.0, flat: true },
            ],
        },
        SuperMod {
            name: "RAZOR EDGE",
            effects: vec![
                Buff { kind: StatKind::Crit,         delta:  40.0, flat: true },
                Buff { kind: StatKind::TurretDamage, delta: -20.0, flat: true },
            ],
        },
        SuperMod {
            name: "GAMBLER",
            effects: vec![
                Buff { kind: StatKind::Luck,    delta:  50.0, flat: true },
                Buff { kind: StatKind::Harvest, delta:  50.0, flat: true },
                Buff { kind: StatKind::Hp,      delta: -30.0, flat: true },
            ],
        },
        SuperMod {
            name: "BERSERKER",
            effects: vec![
                Buff { kind: StatKind::TurretDamage, delta:  50.0, flat: true },
                Buff { kind: StatKind::TurnSpeed,    delta:   1.0, flat: true },
                Buff { kind: StatKind::Range,        delta: -30.0, flat: true },
            ],
        },
        SuperMod {
            name: "ARCANIST",
            effects: vec![
                Buff { kind: StatKind::RuneDamage,   delta:  0.50, flat: true },
                Buff { kind: StatKind::ProcStrength, delta: 25.0, flat: true },
                Buff { kind: StatKind::TurretDamage, delta: -20.0, flat: true },
            ],
        },
    ]
}

/// Pick one super mod at random.
fn pick_super_mod() -> SuperMod {
    let mut cat = super_mod_catalog();
    let mut rng = rand::thread_rng();
    cat.shuffle(&mut rng);
    cat.pop().expect("catalog must not be empty")
}

/// Bounty scrap scales with the cleared section's star tier so big
/// boss fights pay out more than easy ones.
pub fn bounty_for_stars(stars: u8) -> u32 {
    match stars {
        5 => 100,
        4 => 70,
        3 => 50,
        _ => 30,
    }
}

// ---------- UI ----------

/// Root marker on the overlay so we can despawn it wholesale on exit.
#[derive(Component)]
pub struct BossRewardRoot;

/// Which card the player clicked. Mapped from a stable index for the
/// click handler dispatch.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
pub enum BossRewardButton {
    Recruit,
    Bounty,
    SuperMod,
}

/// Marker on each stat row — driven by `update_boss_reward_stat_tooltip`
/// to show the row's description in the tooltip area on hover.
#[derive(Component, Clone, Copy)]
pub struct BossRewardStatRow(pub StatKind);

/// Single tooltip text node at the bottom of the stats panel. Picks
/// up the hovered row's description; hidden when nothing is hovered.
#[derive(Component)]
pub struct BossRewardStatTooltip;

pub fn enter_boss_reward(
    mut commands: Commands,
    mut pending: ResMut<BossRewardPending>,
    mut offer: ResMut<BossRewardOffer>,
    map_state: Res<crate::map::MapState>,
    stats: Res<PlayerStats>,
) {
    let boss = pending.0.take();
    let stars = map_state
        .sections
        .get(map_state.current as usize)
        .map(|s| s.stars)
        .unwrap_or(3);
    offer.boss = boss;
    offer.bounty_scrap = bounty_for_stars(stars);
    offer.super_mod = Some(pick_super_mod());

    spawn_overlay(&mut commands, &offer, &stats);
}

pub fn exit_boss_reward(
    mut commands: Commands,
    q: Query<Entity, With<BossRewardRoot>>,
) {
    for e in &q {
        commands.entity(e).despawn();
    }
}

/// Apply the chosen reward, queue the next state, and let `OnExit`
/// tear down the overlay.
pub fn handle_boss_reward_click(
    interactions: Query<(&Interaction, &BossRewardButton), Changed<Interaction>>,
    offer: Res<BossRewardOffer>,
    mut recruits: ResMut<RecruitedAllies>,
    mut scrap: ResMut<crate::Scrap>,
    mut scrap_earned: ResMut<crate::stage_complete::ScrapEarnedThisStage>,
    mut stats: ResMut<PlayerStats>,
    pending_levels: Res<crate::xp::LevelUpsPending>,
    mut next: ResMut<NextState<crate::AppState>>,
) {
    for (interaction, btn) in &interactions {
        if !matches!(*interaction, Interaction::Pressed) { continue; }
        match *btn {
            BossRewardButton::Recruit => {
                if let Some(class) = offer.boss {
                    recruits.0.push(class);
                }
            }
            BossRewardButton::Bounty => {
                scrap.0 = scrap.0.saturating_add(offer.bounty_scrap);
                scrap_earned.0 = scrap_earned.0.saturating_add(offer.bounty_scrap);
            }
            BossRewardButton::SuperMod => {
                if let Some(m) = &offer.super_mod {
                    for buff in &m.effects {
                        buff.apply(&mut stats);
                    }
                }
            }
        }
        let dest = if pending_levels.0 > 0 {
            crate::AppState::LevelUp
        } else {
            crate::AppState::Customize
        };
        next.set(dest);
        return;
    }
}

/// Watch stat-row hovers and write the hovered row's description into
/// the tooltip text. `Changed<Interaction>` keeps the system idle most
/// frames — only fires on entry/exit, not while held.
pub fn update_boss_reward_stat_tooltip(
    rows: Query<(&Interaction, &BossRewardStatRow), Changed<Interaction>>,
    mut tooltip: Query<(&mut Text, &mut Visibility), With<BossRewardStatTooltip>>,
) {
    let Ok((mut text, mut vis)) = tooltip.single_mut() else { return; };
    for (interaction, row) in &rows {
        match *interaction {
            Interaction::Hovered | Interaction::Pressed => {
                let new_text = format!("{}: {}", row.0.label(), row.0.description());
                if text.0 != new_text { text.0 = new_text; }
                if *vis != Visibility::Inherited { *vis = Visibility::Inherited; }
            }
            Interaction::None => {
                if !text.0.is_empty() { text.0.clear(); }
                if *vis != Visibility::Hidden { *vis = Visibility::Hidden; }
            }
        }
    }
}

fn spawn_overlay(commands: &mut Commands, offer: &BossRewardOffer, stats: &PlayerStats) {
    let boss_name = offer.boss.map(|c| c.label()).unwrap_or("");
    let bounty = offer.bounty_scrap;
    let super_mod = offer.super_mod.clone();

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
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.62)),
            ZIndex(170),
            Visibility::Inherited,
            Button,
            BossRewardRoot,
        ))
        .with_children(|root| {
            root.spawn(ui_kit::label(
                "BOSS DEFEATED",
                theme::FONT_LG * 1.6,
                theme::ACCENT,
            ));
            root.spawn(ui_kit::label(
                boss_name,
                theme::FONT_LG,
                theme::ON_SURFACE_DIM,
            ));
            root.spawn(ui_kit::label(
                "CHOOSE!",
                theme::FONT_LG * 1.2,
                theme::ACCENT,
            ));

            // Main row: 3 reward cards left, current-stats panel right.
            // Mirrors the level-up overlay's two-column shape so the
            // player can read their build state while choosing.
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
                // ---- LEFT: reward cards ----
                cols.spawn((
                    Node {
                        flex_direction: FlexDirection::Row,
                        column_gap: Val::Px(theme::GAP_LG),
                        align_items: AlignItems::Stretch,
                        ..default()
                    },
                    BackgroundColor(Color::NONE),
                ))
                .with_children(|row| {
                    spawn_card(row, BossRewardButton::Recruit,
                        "RECRUIT",
                        boss_name,
                        "Joins your fleet permanently. Respawns at full HP every stage.",
                    );
                    spawn_card(row, BossRewardButton::Bounty,
                        "BOUNTY",
                        &format!("+{} SCRAP", bounty),
                        "Currency for the shop — turrets, runes, and mods.",
                    );
                    let (mod_name, mod_summary) = match &super_mod {
                        Some(m) => (m.name, m.summary()),
                        None    => ("???", String::new()),
                    };
                    spawn_card(row, BossRewardButton::SuperMod,
                        "SUPER MOD",
                        mod_name,
                        &mod_summary,
                    );
                });

                // ---- RIGHT: stats panel with hover tooltips ----
                spawn_stats_panel(cols, stats);
            });
        });
}

/// One reward card — wider than the original 180px so the SuperMod's
/// multi-stat summary doesn't wrap to a tiny column. Headers, the
/// thing on offer, and the description each get their own line with
/// room to breathe.
fn spawn_card(
    parent: &mut ChildSpawnerCommands,
    button: BossRewardButton,
    header: &str,
    subtitle: &str,
    body: &str,
) {
    parent.spawn((
        Button,
        Node {
            width: Val::Px(240.0),
            min_height: Val::Px(180.0),
            border: UiRect::all(Val::Px(theme::BORDER_W)),
            padding: UiRect::all(Val::Px(theme::PAD_LG)),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::FlexStart,
            row_gap: Val::Px(theme::GAP_SM + 2.0),
            ..default()
        },
        BackgroundColor(theme::SURFACE_RAISED),
        BorderColor(theme::ACCENT),
        button,
    ))
    .with_children(|card| {
        card.spawn(ui_kit::label(header, theme::FONT_LG, theme::ACCENT));
        if !subtitle.is_empty() {
            // Centred subtitle inside the card; allowed to wrap if the
            // SuperMod summary is wide. Bevy UI wraps Text nodes
            // automatically when the parent has a constrained width.
            card.spawn((
                Text::new(subtitle.to_string()),
                TextFont { font_size: theme::FONT_MD, ..default() },
                TextColor(theme::ON_SURFACE),
                TextLayout::new_with_justify(JustifyText::Center),
                Node {
                    max_width: Val::Px(220.0),
                    ..default()
                },
            ));
        }
        if !body.is_empty() {
            card.spawn((
                Text::new(body.to_string()),
                TextFont { font_size: theme::FONT_SM, ..default() },
                TextColor(theme::ON_SURFACE_DIM),
                TextLayout::new_with_justify(JustifyText::Center),
                Node {
                    max_width: Val::Px(220.0),
                    ..default()
                },
            ));
        }
    });
}

/// Stats panel mirroring the shop's RHS readout — each row labels the
/// stat, shows the current value tinted by buff/nerf vs baseline, and
/// drives the bottom-of-panel tooltip on hover.
fn spawn_stats_panel(parent: &mut ChildSpawnerCommands, stats: &PlayerStats) {
    let baseline = PlayerStats::default();
    parent.spawn((
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
        panel.spawn(ui_kit::label("CURRENT STATS", theme::FONT_LG, theme::ACCENT));

        for &kind in StatKind::ALL {
            let cur = kind.stat(stats).effective();
            let base = kind.stat(&baseline).effective();
            let value_color = if cur > base + 0.001 {
                Color::srgb(0.55, 0.95, 0.55) // buffed
            } else if cur < base - 0.001 {
                Color::srgb(1.00, 0.55, 0.55) // nerfed
            } else {
                theme::ON_SURFACE
            };
            panel.spawn((
                // `Button` so this row receives `Interaction::Hovered`
                // events — used by `update_boss_reward_stat_tooltip`.
                Button,
                Node {
                    flex_direction: FlexDirection::Row,
                    justify_content: JustifyContent::SpaceBetween,
                    column_gap: Val::Px(theme::GAP_MD),
                    padding: UiRect::vertical(Val::Px(1.0)),
                    ..default()
                },
                BackgroundColor(Color::NONE),
                BossRewardStatRow(kind),
            ))
            .with_children(|stat_row| {
                stat_row.spawn(ui_kit::label(
                    kind.label(),
                    theme::FONT_MD,
                    theme::ON_SURFACE_DIM,
                ));
                stat_row.spawn((
                    Text::new(kind.format_value(stats)),
                    TextFont { font_size: theme::FONT_MD, ..default() },
                    TextColor(value_color),
                ));
            });
        }

        // Tooltip slot — sits at the bottom of the panel. Hidden until
        // the player hovers a stat row.
        panel.spawn((
            Node {
                width: Val::Percent(100.0),
                min_height: Val::Px(36.0),
                padding: UiRect::top(Val::Px(theme::GAP_MD)),
                ..default()
            },
            BackgroundColor(Color::NONE),
        ))
        .with_children(|hint| {
            hint.spawn((
                Text::new(""),
                TextFont { font_size: theme::FONT_SM, ..default() },
                TextColor(theme::ON_SURFACE_DIM),
                TextLayout::new_with_justify(JustifyText::Left),
                Node { max_width: Val::Px(260.0), ..default() },
                Visibility::Hidden,
                BossRewardStatTooltip,
            ));
        });
    });
}

// ---------- Reset on restart ----------

/// Clear pending boss reward + queued recruits. Runs alongside
/// `game_over::reset_run_for_restart` on every fresh-run path
/// (RESTART, quit-to-menu, hull BACK) so a stale recruit can't
/// leak into the next run.
pub fn reset_boss_reward_state(
    mut pending: ResMut<BossRewardPending>,
    mut recruits: ResMut<RecruitedAllies>,
) {
    pending.0 = None;
    recruits.0.clear();
}

// ---------- Recruited-ally spawn hook ----------

/// Despawn any leftover ally entities, then respawn one of each class
/// in the permanent `RecruitedAllies` roster at full HP. Runs on
/// `OnEnter(Playing)` so each stage starts with the player's full
/// recruited fleet, not whatever was left limping after last stage.
pub fn respawn_allies_for_stage(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    recruits: Res<RecruitedAllies>,
    pm: Option<Res<crate::palette::PaletteMaterials>>,
    em: Option<Res<crate::effects::EffectMeshes>>,
    existing_allies: Query<Entity, With<Ally>>,
    friendly: Query<&Transform, With<crate::components::Friendly>>,
) {
    let Some(pm) = pm else { return; };
    let Some(em) = em else { return; };

    // Wipe last stage's allies first — any survivor would otherwise
    // double up with the fresh respawn below.
    for e in &existing_allies {
        commands.entity(e).despawn();
    }

    if recruits.0.is_empty() { return; }

    let player_pos = friendly.single()
        .map(|t| t.translation.truncate())
        .unwrap_or(Vec2::ZERO);
    use rand::Rng;
    let mut rng = rand::thread_rng();
    for &class in &recruits.0 {
        let offset = Vec2::new(
            rng.gen_range(-15.0..15.0),
            rng.gen_range(-15.0..15.0),
        );
        crate::ally::spawn_ally(
            &mut commands, &pm, &em, &mut meshes,
            player_pos + offset,
            std::f32::consts::FRAC_PI_2,
            class,
        );
    }
}
