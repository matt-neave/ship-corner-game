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
                handle_boss_reward_click.run_if(in_state(AppState::BossReward)),
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
    mut commands: Commands,
    interactions: Query<(&Interaction, &BossRewardButton), Changed<Interaction>>,
    offer: Res<BossRewardOffer>,
    mut recruits: ResMut<RecruitedAllies>,
    mut scrap: ResMut<crate::Scrap>,
    mut scrap_earned: ResMut<crate::stage_complete::ScrapEarnedThisStage>,
    mut stats: ResMut<PlayerStats>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
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
        if pending_levels.0 > 0 {
            next.set(crate::AppState::LevelUp);
        } else {
            // White-wipe transition on the hop to the shop.
            crate::stage_complete::spawn_transition(
                &mut commands, &mut meshes, &mut materials,
                crate::AppState::Customize,
            );
        }
        return;
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
                    spawn_value_card(
                        row, BossRewardButton::Recruit, "RECRUIT", boss_name,
                    );
                    spawn_value_card(
                        row, BossRewardButton::Bounty, "BOUNTY",
                        &format!("+{} SCRAP", bounty),
                    );
                    spawn_super_mod_card(
                        row, BossRewardButton::SuperMod, super_mod.as_ref(),
                    );
                });

                // ---- RIGHT: stats panel with hover tooltips ----
                crate::stats_panel_overlay::spawn_stats_panel(cols, stats);
            });
        });
}

/// Shared card chrome — fixed dimensions, gold border, dark surface.
/// The two reward-card flavours (`value` and `super_mod`) build their
/// content with the closure.
fn spawn_card_frame(
    parent: &mut ChildSpawnerCommands,
    button: BossRewardButton,
    build: impl FnOnce(&mut ChildSpawnerCommands),
) {
    parent
        .spawn((
            Button,
            Node {
                width: Val::Px(240.0),
                min_height: Val::Px(180.0),
                border: UiRect::all(Val::Px(theme::BORDER_W)),
                padding: UiRect::all(Val::Px(theme::PAD_LG)),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                row_gap: Val::Px(theme::GAP_MD),
                ..default()
            },
            BackgroundColor(theme::SURFACE_RAISED),
            BorderColor(theme::ACCENT),
            button,
        ))
        .with_children(build);
}

/// Recruit + Bounty cards — single big value under the header. No
/// explanatory body line; the card is the choice.
fn spawn_value_card(
    parent: &mut ChildSpawnerCommands,
    button: BossRewardButton,
    header: &str,
    value: &str,
) {
    spawn_card_frame(parent, button, |card| {
        card.spawn(ui_kit::label(header, theme::FONT_LG, theme::ACCENT));
        card.spawn((
            Text::new(value.to_string()),
            TextFont { font_size: theme::FONT_LG, ..default() },
            TextColor(theme::ON_SURFACE),
            TextLayout::new_with_justify(JustifyText::Center),
            Node {
                max_width: Val::Px(220.0),
                ..default()
            },
        ));
    });
}

/// Super-mod card — header, mod name, then an itemised list of every
/// effect (buff in green, nerf in red), matching the colour family
/// of shop mod cards + the level-up overlay buffs/nerfs.
fn spawn_super_mod_card(
    parent: &mut ChildSpawnerCommands,
    button: BossRewardButton,
    mod_data: Option<&SuperMod>,
) {
    spawn_card_frame(parent, button, |card| {
        card.spawn(ui_kit::label("SUPER MOD", theme::FONT_LG, theme::ACCENT));
        let name = mod_data.map(|m| m.name).unwrap_or("???");
        card.spawn((
            Text::new(name.to_string()),
            TextFont { font_size: theme::FONT_LG, ..default() },
            TextColor(theme::ON_SURFACE),
            TextLayout::new_with_justify(JustifyText::Center),
        ));
        if let Some(m) = mod_data {
            for buff in &m.effects {
                let color = if buff.delta >= 0.0 {
                    theme::BUFF_FG // green: positive
                } else {
                    theme::NERF_FG // red: negative
                };
                card.spawn((
                    Text::new(buff.label()),
                    TextFont { font_size: theme::FONT_MD, ..default() },
                    TextColor(color),
                    TextLayout::new_with_justify(JustifyText::Center),
                ));
            }
        }
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
