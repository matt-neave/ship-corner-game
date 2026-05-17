//! Chests — Brotato-style supply drops.
//!
//! Flow:
//! 1. Enemy dies → [`enemy_death_check`] rolls `CHEST_DROP_CHANCE` (1%);
//!    on a hit, [`spawn_chest_pickup`] drops a small chest at the
//!    enemy's position.
//! 2. Player walks over the chest → [`tick_chest_pickups`] rolls one
//!    mod via [`drag::roll_one_mod`] and pushes it onto
//!    [`PendingChests`].
//! 3. At the next [`StageComplete`] tick, if `PendingChests` is
//!    non-empty the chain routes through [`AppState::ChestOpen`]
//!    instead of going straight to BossReward / LevelUp / Customize.
//! 4. The modal shows the front chest's offer: a single card with
//!    TAKE (free, applies the mod) and SELL (refund 50% of the
//!    mod's shop cost, rounded down — min 1).
//! 5. After every claim the queue advances; once empty the chain
//!    continues to BossReward / LevelUp / Customize as if the
//!    chest detour hadn't happened.
//!
//! Chests carry over between rounds via the `PendingChests`
//! resource (cleared on run reset by `game_over::reset_run_for_restart`).

use bevy::prelude::*;
use bevy::render::view::RenderLayers;

use crate::balance::PLAY_LAYER;
use crate::components::Friendly;
use crate::customize::drag::{roll_one_mod, ShopMod};
use crate::rune::Magnetic;
use crate::stats::PlayerStats;
use crate::ui_kit::{self, theme};
use crate::AppState;

pub struct ChestPlugin;

impl Plugin for ChestPlugin {
    fn build(&self, app: &mut App) {
        app
            .insert_resource(PendingChests::default())
            .add_systems(Update, tick_chest_pickups)
            .add_systems(OnEnter(AppState::ChestOpen), enter_chest_open)
            .add_systems(OnExit(AppState::ChestOpen), exit_chest_open)
            .add_systems(
                Update,
                handle_chest_click.run_if(in_state(AppState::ChestOpen)),
            );
    }
}

// ---------- Tuning ----------

/// Default per-kill chest drop chance. 0.01 = 1%; on a 50-kill wave
/// you'd expect ~0.5 chests on average, so the player sees one every
/// other wave on a typical run.
pub const CHEST_DROP_CHANCE: f32 = 0.01;
/// Pickup collision radius. Slightly larger than scrap/HP so a chest
/// is harder to miss in a chaotic clear — it's a rare event, missing
/// one stings.
pub const CHEST_PICKUP_RADIUS: f32 = 5.5;
/// Lifetime on the ground before despawn. Longer than scrap (16s)
/// because chests are a much rarer drop.
pub const CHEST_PICKUP_LIFETIME: f32 = 40.0;
/// Sell refund fraction. 0.5 means a 4-scrap mod sells for 2 scrap.
/// Min refund is 1 scrap so even a 2-cost mod gives something.
const CHEST_SELL_FRACTION: f32 = 0.5;

// ---------- Resources ----------

/// FIFO queue of mods the player has picked up but not yet claimed
/// or sold. Drained one-at-a-time by the [`AppState::ChestOpen`]
/// modal. Cleared on run reset.
#[derive(Resource, Default)]
pub struct PendingChests(pub Vec<ShopMod>);

// ---------- World pickup ----------

#[derive(Component)]
pub struct ChestPickup {
    pub lifetime: f32,
}

/// Spawn a chest at `pos`. Two-tone rectangle so the silhouette reads
/// as a wooden crate with a gold band — visually distinct from the
/// round scrap coin and round HP pickup.
pub fn spawn_chest_pickup(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ColorMaterial>,
    pos: Vec2,
) {
    let body_mesh = meshes.add(Rectangle::new(4.0, 3.2));
    let band_mesh = meshes.add(Rectangle::new(4.0, 0.8));
    let body_mat = materials.add(Color::srgb(0.42, 0.26, 0.14));
    let band_mat = materials.add(Color::srgb(1.0, 0.85, 0.30));

    let id = commands.spawn((
        Mesh2d(body_mesh),
        MeshMaterial2d(body_mat),
        Transform::from_xyz(pos.x, pos.y, 4.5),
        ChestPickup { lifetime: CHEST_PICKUP_LIFETIME },
        Magnetic::default_pull(),
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    // Gold band as a child so the magnet pull only moves the parent.
    let band = commands.spawn((
        Mesh2d(band_mesh),
        MeshMaterial2d(band_mat),
        Transform::from_xyz(0.0, 0.0, 0.1),
        RenderLayers::layer(PLAY_LAYER),
    )).id();
    commands.entity(band).insert(ChildOf(id));
}

/// Per-frame: decay lifetime, despawn expired, claim on player
/// contact. Claim rolls one mod via [`roll_one_mod`] and pushes
/// onto [`PendingChests`].
pub fn tick_chest_pickups(
    time: Res<Time>,
    mut commands: Commands,
    mut pending: ResMut<PendingChests>,
    mut sfx: crate::sfx::SfxPlayer,
    mut chests: Query<(Entity, &Transform, &mut ChestPickup)>,
    friendly: Query<&Transform, (With<Friendly>, Without<ChestPickup>)>,
) {
    let dt = time.delta_secs();
    let Ok(ftf) = friendly.single() else { return };
    let fp = ftf.translation.truncate();
    let r2 = CHEST_PICKUP_RADIUS * CHEST_PICKUP_RADIUS;
    for (e, tf, mut chest) in &mut chests {
        chest.lifetime -= dt;
        if chest.lifetime <= 0.0 {
            commands.entity(e).despawn();
            continue;
        }
        let pp = tf.translation.truncate();
        if pp.distance_squared(fp) < r2 {
            pending.0.push(roll_one_mod());
            sfx.play(crate::sfx::Sfx::Coin);
            commands.entity(e).despawn();
        }
    }
}

// ---------- Routing ----------

/// Next state the chest modal should transition to once the queue
/// is empty. Mirrors `stage_complete::tick_stage_complete`'s pick:
/// BossReward → LevelUp → Customize. Pulled out so both the
/// stage-complete tick AND the chest modal's click handler use the
/// same chain.
pub fn next_state_after_chests(
    boss_reward: &crate::boss_reward::BossRewardPending,
    pending_levels: &crate::xp::LevelUpsPending,
) -> AppState {
    if boss_reward.0.is_some() {
        AppState::BossReward
    } else if pending_levels.0 > 0 {
        AppState::LevelUp
    } else {
        AppState::Customize
    }
}

// ---------- Modal ----------

#[derive(Component)]
pub struct ChestOpenRoot;

#[derive(Component, Clone, Copy, PartialEq, Eq)]
pub enum ChestOpenButton {
    Take,
    Sell,
}

pub fn enter_chest_open(
    mut commands: Commands,
    pending: Res<PendingChests>,
    stats: Res<PlayerStats>,
) {
    let Some(offer) = pending.0.first().copied() else {
        // Shouldn't normally happen — `tick_stage_complete` only
        // routes here when the queue is non-empty. If we landed
        // here empty (e.g. dev jumped state manually), spawn a
        // minimal panel with no card to avoid an empty modal.
        return;
    };
    spawn_overlay(&mut commands, offer, &stats, pending.0.len() as u32);
}

pub fn exit_chest_open(
    mut commands: Commands,
    q: Query<Entity, With<ChestOpenRoot>>,
) {
    for e in &q {
        commands.entity(e).despawn();
    }
}

pub fn handle_chest_click(
    interactions: Query<(&Interaction, &ChestOpenButton), Changed<Interaction>>,
    mut pending: ResMut<PendingChests>,
    mut stats: ResMut<PlayerStats>,
    mut scrap: ResMut<crate::Scrap>,
    mut scrap_earned: ResMut<crate::stage_complete::ScrapEarnedThisStage>,
    mut active: ResMut<crate::customize::drag::ActiveLegendaries>,
    boss_reward: Res<crate::boss_reward::BossRewardPending>,
    pending_levels: Res<crate::xp::LevelUpsPending>,
    mut next: ResMut<NextState<AppState>>,
) {
    for (interaction, btn) in &interactions {
        if !matches!(*interaction, Interaction::Pressed) { continue; }
        let Some(offer) = pending.0.first().copied() else { return };
        let spec = offer.spec();
        match *btn {
            ChestOpenButton::Take => {
                for &(kind, delta) in spec.changes {
                    let stat = kind.stat_mut(&mut stats);
                    stat.flat += delta;
                }
                if let Some(eff) = spec.effect {
                    apply_chest_effect(eff, &mut stats, &mut active);
                }
            }
            ChestOpenButton::Sell => {
                let refund = ((spec.rarity.cost() as f32 * CHEST_SELL_FRACTION)
                    .floor() as u32).max(1);
                scrap.0 = scrap.0.saturating_add(refund);
                scrap_earned.0 = scrap_earned.0.saturating_add(refund);
            }
        }
        pending.0.remove(0);
        // Re-enter ChestOpen if more chests remain, else route
        // through the normal post-stage chain.
        if !pending.0.is_empty() {
            // OnExit despawns this modal, OnEnter spawns the next.
            next.set(AppState::ChestOpen);
        } else {
            next.set(next_state_after_chests(&boss_reward, &pending_levels));
        }
        return;
    }
}

fn apply_chest_effect(
    eff: crate::customize::drag::ModEffect,
    stats: &mut PlayerStats,
    active: &mut crate::customize::drag::ActiveLegendaries,
) {
    use crate::customize::drag::ModEffect;
    match eff {
        ModEffect::Monomaniac => active.monomaniac = true,
        ModEffect::Duelist    => active.duelist    = true,
        ModEffect::Harmony    => active.harmony    = true,
        ModEffect::Purist     => active.purist     = true,
        ModEffect::Specialist => active.specialist = true,
        ModEffect::Turtle => {
            let s = stats.shield_max.effective().max(0.0);
            stats.hp.flat += s;
            stats.shield_max.flat -= s;
        }
    }
}

fn spawn_overlay(
    commands: &mut Commands,
    offer: ShopMod,
    stats: &PlayerStats,
    queue_len: u32,
) {
    let spec = offer.spec();
    let rarity_color = spec.rarity.border_color();
    let sell_value = ((spec.rarity.cost() as f32 * CHEST_SELL_FRACTION)
        .floor() as u32).max(1);
    let title = if queue_len > 1 {
        format!("CHEST 1/{}", queue_len)
    } else {
        "CHEST".to_string()
    };
    let mod_name = spec.name.to_string();
    let rarity_label = spec.rarity.label().to_string();
    let changes_lines: Vec<(String, Color)> = spec.changes.iter().map(|&(kind, delta)| {
        let line = format!("{} {}", kind.format_delta(delta), kind.label());
        let color = if delta >= 0.0 { theme::BUFF_FG } else { theme::NERF_FG };
        (line, color)
    }).collect();
    let effect_body = spec.effect.map(|e| e.tooltip_body().to_string());

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
            ChestOpenRoot,
        ))
        .with_children(|root| {
            root.spawn(ui_kit::label(
                &title,
                theme::FONT_LG * 1.6,
                theme::ACCENT,
            ));

            // Two columns: card on the left, stats panel on the
            // right — same shape as BossReward so the player can
            // read their build state while deciding.
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
                cols.spawn((
                    Node {
                        width: Val::Px(280.0),
                        min_height: Val::Px(220.0),
                        border: UiRect::all(Val::Px(theme::BORDER_W)),
                        padding: UiRect::all(Val::Px(theme::PAD_LG)),
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::Center,
                        justify_content: JustifyContent::FlexStart,
                        row_gap: Val::Px(theme::GAP_SM),
                        ..default()
                    },
                    BorderColor(rarity_color),
                    BackgroundColor(theme::SURFACE_RAISED),
                ))
                .with_children(|card| {
                    card.spawn(ui_kit::label(
                        &mod_name,
                        theme::FONT_LG,
                        theme::ON_SURFACE,
                    ));
                    card.spawn(ui_kit::label(
                        &rarity_label,
                        theme::FONT_MD,
                        rarity_color,
                    ));
                    // Stat-change lines.
                    for (line, color) in &changes_lines {
                        card.spawn(ui_kit::label(line, theme::FONT_MD, *color));
                    }
                    // Build-warping rule body, if any.
                    if let Some(body) = &effect_body {
                        card.spawn((
                            Node { margin: UiRect::top(Val::Px(theme::GAP_SM)), ..default() },
                            BackgroundColor(Color::NONE),
                        )).with_children(|w| {
                            w.spawn(ui_kit::label(
                                body,
                                theme::FONT_SM,
                                theme::ON_SURFACE_DIM,
                            ));
                        });
                    }
                    // Spacer.
                    card.spawn((
                        Node { height: Val::Px(theme::GAP_MD), ..default() },
                        BackgroundColor(Color::NONE),
                    ));
                    // TAKE + SELL buttons side by side.
                    card.spawn((
                        Node {
                            flex_direction: FlexDirection::Row,
                            column_gap: Val::Px(theme::GAP_MD),
                            ..default()
                        },
                        BackgroundColor(Color::NONE),
                    ))
                    .with_children(|row| {
                        row.spawn((
                            ui_kit::button(theme::SURFACE_RAISED),
                            ChestOpenButton::Take,
                        ))
                        .with_children(|b| {
                            b.spawn(ui_kit::label(
                                "TAKE",
                                theme::FONT_LG,
                                theme::ACCENT,
                            ));
                        });
                        row.spawn((
                            ui_kit::button(theme::SURFACE_RAISED),
                            ChestOpenButton::Sell,
                        ))
                        .with_children(|b| {
                            b.spawn(ui_kit::label(
                                &format!("SELL +{}", sell_value),
                                theme::FONT_LG,
                                theme::ON_SURFACE,
                            ));
                        });
                    });
                });

                crate::stats_panel_overlay::spawn_stats_panel(cols, stats);
            });
        });
}
