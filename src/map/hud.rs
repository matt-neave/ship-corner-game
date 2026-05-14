//! Map-view + combat-view HUD overlays:
//! - **Currency HUD** (top-left of map view): live scrap / steel /
//!   refined steel readouts, anchored inside the play square's corner.
//! - **Level status banner** (top-center of combat view, Sandbox only):
//!   shows `LEVEL N - X ENEMIES LEFT` with a depleting fill bar.
//! - **Debug panel** (bottom-right of map view): CLAIM toggle, PHASE
//!   re-trigger, OPEN CUSTOMIZE, plus rows of SPAWN ALLY / SPAWN BOSS
//!   buttons driven by `ShipClass::ALL`.

use bevy::ecs::hierarchy::ChildSpawnerCommands;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::enemy::Enemy;
use crate::modes::play_area_screen_rect;
use crate::palette::PaletteMaterials;
use crate::ui_kit::{self, theme};
use crate::{RefinedSteel, Scrap, Steel};

use super::{
    CombatContext, CurrencyUi, DebugButton, DebugClaimLabel, DebugClaimMode, DebugPanel,
    LevelEnemyBar, LevelStatusText, LevelStatusUi, RefinedSteelText, ScrapText, SteelText,
    TriggerMapPhase, ViewMode,
};

// ---------- Currency HUD ----------

pub fn setup_currency_ui(mut commands: Commands) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(8.0),
                left: Val::Px(8.0),
                padding: UiRect::all(Val::Px(theme::PAD_MD)),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::FlexStart,
                row_gap: Val::Px(theme::GAP_SM),
                ..default()
            },
            BackgroundColor(theme::SURFACE_RAISED),
            ZIndex(50),
            CurrencyUi,
        ))
        .with_children(|p| {
            spawn_currency_row(
                p,
                Color::srgb(0.62, 0.66, 0.72),
                Some(Color::srgb(0.42, 0.46, 0.54)),
                ScrapText,
            );
            spawn_currency_row(
                p,
                Color::srgb(0.55, 0.62, 0.78),
                None,
                SteelText,
            );
            spawn_currency_row(
                p,
                Color::srgb(0.92, 0.78, 0.40),
                None,
                RefinedSteelText,
            );
        });
}

fn spawn_currency_row<M: Component>(
    parent: &mut ChildSpawnerCommands,
    icon_color: Color,
    inner: Option<Color>,
    marker: M,
) {
    parent
        .spawn(Node {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: Val::Px(theme::GAP_SM),
            ..default()
        })
        .with_children(|row| {
            row.spawn((
                Node {
                    width: Val::Px(12.0),
                    height: Val::Px(12.0),
                    ..default()
                },
                BackgroundColor(icon_color),
            ))
            .with_children(|icon| {
                if let Some(inner_color) = inner {
                    icon.spawn((
                        Node {
                            position_type: PositionType::Absolute,
                            top: Val::Px(4.0),
                            left: Val::Px(4.0),
                            width: Val::Px(6.0),
                            height: Val::Px(6.0),
                            ..default()
                        },
                        BackgroundColor(inner_color),
                    ));
                }
            });
            row.spawn((
                ui_kit::label("0", theme::FONT_MD, theme::ON_SURFACE),
                marker,
            ));
        });
}

pub fn update_scrap_text(
    scrap: Res<Scrap>,
    mut q: Query<&mut Text, With<ScrapText>>,
) {
    if !scrap.is_changed() { return; }
    let s = scrap.0.to_string();
    for mut text in &mut q {
        if text.0 != s { text.0 = s.clone(); }
    }
}

pub fn update_steel_text(
    steel: Res<Steel>,
    mut q: Query<&mut Text, With<SteelText>>,
) {
    if !steel.is_changed() { return; }
    let s = steel.0.to_string();
    for mut text in &mut q {
        if text.0 != s { text.0 = s.clone(); }
    }
}

pub fn update_refined_steel_text(
    refined: Res<RefinedSteel>,
    mut q: Query<&mut Text, With<RefinedSteelText>>,
) {
    if !refined.is_changed() { return; }
    let s = refined.0.to_string();
    for mut text in &mut q {
        if text.0 != s { text.0 = s.clone(); }
    }
}

/// Per-frame upkeep on the currency HUD: anchor its top-left corner to
/// the *play area's* top-left and toggle visibility based on `ViewMode`.
pub fn update_currency_ui(
    view: Res<ViewMode>,
    windows: Query<&Window, With<PrimaryWindow>>,
    ui_scale: Res<bevy::ui::UiScale>,
    mut q: Query<(&mut Visibility, &mut Node), With<CurrencyUi>>,
) {
    let Ok(win) = windows.single() else { return; };
    let (play_left, play_top, _play_w, _play_h) = play_area_screen_rect(win.width(), win.height());
    // Position is a screen-pixel coord; divide by `UiScale` so the
    // bevy_ui layout pass multiplies it back to the right place.
    // Margin authored in design pixels so it scales with UiScale.
    let s = ui_scale.0.max(0.0001);
    let margin_design = 4.0;
    let want_left = Val::Px(play_left / s + margin_design);
    let want_top  = Val::Px(play_top  / s + margin_design);
    let want_vis = if matches!(*view, ViewMode::Map) {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    for (mut vis, mut node) in &mut q {
        if node.left != want_left { node.left = want_left; }
        if node.top  != want_top  { node.top  = want_top;  }
        if *vis != want_vis { *vis = want_vis; }
    }
}

// ---------- Level status banner ----------

pub fn setup_level_status_ui(mut commands: Commands) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(0.0),
                left: Val::Px(0.0),
                width: Val::Px(0.0),
                justify_content: JustifyContent::Center,
                flex_direction: FlexDirection::Row,
                ..default()
            },
            ZIndex(50),
            Visibility::Hidden,
            LevelStatusUi,
        ))
        .with_children(|p| {
            p.spawn((
                Node {
                    padding: UiRect::axes(
                        Val::Px(theme::PAD_MD),
                        Val::Px(theme::PAD_SM),
                    ),
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Stretch,
                    row_gap: Val::Px(theme::GAP_SM),
                    min_width: Val::Px(180.0),
                    ..default()
                },
                BackgroundColor(theme::SURFACE_RAISED),
            ))
            .with_children(|inner| {
                inner.spawn((
                    ui_kit::label("", theme::FONT_MD, theme::ON_SURFACE),
                    LevelStatusText,
                ));
                inner.spawn((
                    Node {
                        width: Val::Percent(100.0),
                        height: Val::Px(5.0),
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.10, 0.12, 0.18)),
                ))
                .with_children(|bar| {
                    bar.spawn((
                        Node {
                            width:  Val::Percent(100.0),
                            height: Val::Percent(100.0),
                            ..default()
                        },
                        BackgroundColor(Color::srgb(0.85, 0.20, 0.20)),
                        LevelEnemyBar,
                    ));
                });
            });
        });
}

pub fn update_level_status_ui(
    view: Res<ViewMode>,
    mode: Res<crate::modes::GameMode>,
    combat_ctx: Res<CombatContext>,
    windows: Query<&Window, With<PrimaryWindow>>,
    ui_scale: Res<bevy::ui::UiScale>,
    enemies: Query<&Enemy>,
    mut root_q: Query<(&mut Visibility, &mut Node), (With<LevelStatusUi>, Without<LevelEnemyBar>)>,
    mut text_q: Query<&mut Text, With<LevelStatusText>>,
    mut bar_q:  Query<&mut Node, (With<LevelEnemyBar>, Without<LevelStatusUi>)>,
) {
    // Banner intentionally hidden — the wave indicator (top-right)
    // already covers wave-progress feedback; the centred enemy bar
    // doubled up. Flip back by setting `visible` to its old value
    // (`Combat && Sandbox`) if the banner is wanted again.
    let _ = (&*view, &*mode);
    let visible = false;
    let want_vis = if visible { Visibility::Inherited } else { Visibility::Hidden };

    let Ok(win) = windows.single() else { return; };
    let (play_left, play_top, play_w, _play_h) = play_area_screen_rect(win.width(), win.height());
    // Same UiScale-compensated layout as `update_hp_bar_pixel_scale`.
    let s = ui_scale.0.max(0.0001);
    let margin_design = 4.0;
    let want_top   = Val::Px(play_top / s + margin_design);
    let want_left  = Val::Px(play_left / s);
    let want_width = Val::Px(play_w / s);

    for (mut vis, mut node) in &mut root_q {
        if *vis != want_vis      { *vis = want_vis; }
        if node.left  != want_left  { node.left  = want_left;  }
        if node.top   != want_top   { node.top   = want_top;   }
        if node.width != want_width { node.width = want_width; }
    }

    if !visible { return; }

    let alive = enemies.iter().count() as u32;
    let total_left = combat_ctx.enemy_budget + alive;
    let s = format!(
        "LEVEL {} - {} ENEMIES LEFT",
        combat_ctx.stars, total_left,
    );
    for mut t in &mut text_q {
        if t.0 != s { t.0 = s.clone(); }
    }

    let denom = combat_ctx.enemy_total.max(1) as f32;
    let pct = (total_left as f32 / denom).clamp(0.0, 1.0) * 100.0;
    let want_fill = Val::Percent(pct);
    for mut node in &mut bar_q {
        if node.width != want_fill { node.width = want_fill; }
    }
}

// ---------- Debug panel ----------
//
// All six functions below are registered in `main.rs` only when the
// `demo` feature is OFF. `#[cfg_attr]` mirrors that gating so demo
// builds don't emit dead-code warnings for symbols the linker will
// strip.

#[cfg_attr(feature = "demo", allow(dead_code))]
pub fn setup_debug_ui(mut commands: Commands) {
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            bottom: Val::Px(8.0),
            right: Val::Px(8.0),
            padding: UiRect::all(Val::Px(theme::PAD_MD)),
            border: UiRect::all(Val::Px(theme::BORDER_W)),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Stretch,
            row_gap: Val::Px(theme::GAP_SM),
            ..default()
        },
        BackgroundColor(theme::SURFACE_RAISED),
        BorderColor(theme::BORDER_SUBTLE),
        ZIndex(50),
        DebugPanel,
    ))
    .with_children(|p| {
        p.spawn(ui_kit::label("DEBUG", theme::FONT_SM, theme::ON_SURFACE_DIM));

        p.spawn((ui_kit::button(theme::SURFACE), DebugButton::ClaimMode))
            .with_children(|b| {
                b.spawn((
                    ui_kit::label("CLAIM", theme::FONT_MD, theme::ON_SURFACE),
                    DebugClaimLabel,
                ));
            });

        p.spawn((ui_kit::button(theme::SURFACE), DebugButton::Phase))
            .with_children(|b| {
                b.spawn(ui_kit::label("PHASE", theme::FONT_MD, theme::ON_SURFACE));
            });

        p.spawn((ui_kit::button(theme::SURFACE), DebugButton::OpenCustomize))
            .with_children(|b| {
                b.spawn(ui_kit::label("CUSTOMIZE", theme::FONT_MD, theme::ON_SURFACE));
            });

        p.spawn((ui_kit::button(theme::SURFACE), DebugButton::AddScrap))
            .with_children(|b| {
                b.spawn(ui_kit::label("+10 SCRAP", theme::FONT_MD, theme::ON_SURFACE));
            });

        p.spawn(ui_kit::label("SPAWN ALLY", theme::FONT_SM, theme::ON_SURFACE_DIM));
        for &class in crate::ally::ShipClass::ALL {
            p.spawn((
                ui_kit::button(theme::SURFACE),
                DebugButton::SpawnAlly(class),
            ))
            .with_children(|b| {
                b.spawn(ui_kit::label(
                    class.label(),
                    theme::FONT_SM,
                    theme::ON_SURFACE,
                ));
            });
        }

        p.spawn(ui_kit::label("SPAWN BOSS", theme::FONT_SM, theme::ON_SURFACE_DIM));
        for &class in crate::ally::ShipClass::ALL {
            p.spawn((
                ui_kit::button(theme::SURFACE),
                DebugButton::SpawnBoss(class),
            ))
            .with_children(|b| {
                b.spawn(ui_kit::label(
                    class.label(),
                    theme::FONT_SM,
                    theme::ON_SURFACE,
                ));
            });
        }

        p.spawn(ui_kit::label("SPAWN ENEMY", theme::FONT_SM, theme::ON_SURFACE_DIM));
        for &variant in crate::enemy::ALL_VARIANTS {
            p.spawn((
                ui_kit::button(theme::SURFACE),
                DebugButton::SpawnEnemy(variant),
            ))
            .with_children(|b| {
                b.spawn(ui_kit::label(
                    variant.label(),
                    theme::FONT_SM,
                    theme::ON_SURFACE,
                ));
            });
        }
    });
}

#[cfg_attr(feature = "demo", allow(dead_code))]
pub fn handle_debug_buttons(
    interactions: Query<(&Interaction, &DebugButton), Changed<Interaction>>,
    mut claim_mode: ResMut<DebugClaimMode>,
    mut phase_evt: EventWriter<TriggerMapPhase>,
    mut next_state: ResMut<NextState<crate::AppState>>,
    mut commands: Commands,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<crate::effects::EffectMeshes>>,
    mut meshes: ResMut<Assets<Mesh>>,
    difficulty: Res<crate::Difficulty>,
    friendly: Query<&Transform, With<crate::components::Friendly>>,
    mut scrap: ResMut<crate::Scrap>,
) {
    for (interaction, button) in &interactions {
        if !matches!(*interaction, Interaction::Pressed) { continue; }
        match *button {
            DebugButton::ClaimMode => claim_mode.active = !claim_mode.active,
            DebugButton::Phase => { phase_evt.write(TriggerMapPhase); }
            DebugButton::OpenCustomize => { next_state.set(crate::AppState::Customize); }
            DebugButton::AddScrap => { scrap.0 = scrap.0.saturating_add(10); }
            DebugButton::SpawnAlly(class) => {
                let Some(pm_ref) = pm.as_deref() else { continue; };
                let Some(em_ref) = em.as_deref() else { continue; };
                use rand::Rng;
                let mut rng = rand::thread_rng();
                let player_pos = friendly.single()
                    .map(|t| t.translation.truncate())
                    .unwrap_or(Vec2::ZERO);
                let offset = Vec2::new(
                    rng.gen_range(-15.0..15.0),
                    rng.gen_range(-15.0..15.0),
                );
                crate::ally::spawn_ally(
                    &mut commands, pm_ref, em_ref, &mut meshes,
                    player_pos + offset,
                    std::f32::consts::FRAC_PI_2,
                    class,
                );
            }
            DebugButton::SpawnBoss(class) => {
                let Some(pm_ref) = pm.as_deref() else { continue; };
                let Some(em_ref) = em.as_deref() else { continue; };
                use rand::Rng;
                let mut rng = rand::thread_rng();
                let player_pos = friendly.single()
                    .map(|t| t.translation.truncate())
                    .unwrap_or(Vec2::ZERO);
                let angle = rng.gen_range(0.0..std::f32::consts::TAU);
                let dist  = rng.gen_range(35.0..55.0);
                let offset = Vec2::new(angle.cos() * dist, angle.sin() * dist);
                crate::ally::spawn_boss(
                    &mut commands, pm_ref, em_ref, &mut meshes,
                    player_pos + offset,
                    std::f32::consts::FRAC_PI_2,
                    class,
                    // Debug spawner — drop in at 3★ baseline.
                    3,
                    // No campaign context in the debug path; treat as a
                    // fresh-run boss so the multiplier is the baseline 1.0×.
                    0,
                    *difficulty,
                );
            }
            DebugButton::SpawnEnemy(variant) => {
                let Some(pm_ref) = pm.as_deref() else { continue; };
                let Some(em_ref) = em.as_deref() else { continue; };
                use rand::Rng;
                let mut rng = rand::thread_rng();
                let player_pos = friendly.single()
                    .map(|t| t.translation.truncate())
                    .unwrap_or(Vec2::ZERO);
                // Spawn at the player so the enemy is immediately
                // visible and engages right away — debug-only, so
                // standing on the player isn't a concern.
                let angle = rng.gen_range(0.0..std::f32::consts::TAU);
                let dist  = rng.gen_range(40.0..70.0);
                let pos = player_pos + Vec2::new(angle.cos() * dist, angle.sin() * dist);
                let inward = (player_pos - pos).normalize_or(Vec2::Y);
                let heading = (-inward.x).atan2(inward.y);
                crate::enemy::spawn_enemy(
                    &mut commands, pm_ref, em_ref, &mut meshes,
                    pos, heading, variant,
                    *difficulty,
                );
            }
        }
    }
}

#[cfg_attr(feature = "demo", allow(dead_code))]
pub fn update_debug_button_tints(
    claim_mode: Res<DebugClaimMode>,
    mut q: Query<(&Interaction, &DebugButton, &mut BackgroundColor)>,
) {
    for (interaction, button, mut bg) in &mut q {
        let claim_locked = matches!(button, DebugButton::ClaimMode) && claim_mode.active;
        bg.0 = match (*interaction, claim_locked) {
            (Interaction::Pressed, _) => theme::ACCENT,
            (Interaction::Hovered, _) => theme::SURFACE_HOVER,
            (Interaction::None, true) => theme::ACCENT,
            (Interaction::None, false) => theme::SURFACE,
        };
    }
}

#[cfg_attr(feature = "demo", allow(dead_code))]
pub fn update_claim_label(
    claim_mode: Res<DebugClaimMode>,
    mut q: Query<&mut Text, With<DebugClaimLabel>>,
) {
    if !claim_mode.is_changed() { return; }
    let target = if claim_mode.active { "CLAIMING…" } else { "CLAIM" };
    for mut text in &mut q {
        if text.0 != target { text.0 = target.to_string(); }
    }
}

// ---------- Debug panel show/hide ----------

/// Toggle for the bottom-right debug panel. Default visible. The `#`
/// key flips this; `sync_debug_panel_visibility` writes through to the
/// panel's `Visibility`.
#[derive(Resource)]
pub struct DebugUiVisible(pub bool);
impl Default for DebugUiVisible {
    /// Visible by default on dev builds, off in demo builds. The demo
    /// build never spawns the debug panel anyway (its setup system is
    /// stripped in `main.rs`), but flipping the default to off here
    /// also disables the customize-screen `+/-` stat-debug glyphs,
    /// which self-gate on this resource.
    #[cfg(not(feature = "demo"))]
    fn default() -> Self { Self(true) }
    #[cfg(feature = "demo")]
    fn default() -> Self { Self(false) }
}

/// Toggle the debug panel on `#` (any keyboard layout — reads the
/// logical character, not a physical KeyCode). Layered in addition to
/// the customize-overlay's auto-hide, which still wins via
/// `sync_debug_panel_visibility` below.
#[cfg_attr(feature = "demo", allow(dead_code))]
pub fn toggle_debug_ui_on_hash(
    mut events: EventReader<KeyboardInput>,
    mut visible: ResMut<DebugUiVisible>,
) {
    for ev in events.read() {
        if !ev.state.is_pressed() { continue; }
        if let Key::Character(s) = &ev.logical_key {
            if s.as_str() == "#" {
                visible.0 = !visible.0;
            }
        }
    }
}

/// Sole writer of `DebugPanel` visibility. Combines the customize-open
/// auto-hide and the `#` toggle, so the two never fight: the panel is
/// visible only when customize is closed AND the toggle is on.
#[cfg_attr(feature = "demo", allow(dead_code))]
pub fn sync_debug_panel_visibility(
    visible: Res<DebugUiVisible>,
    customize_open: Res<crate::customize::CustomizeOpen>,
    main_menu: Res<crate::main_menu::MainMenuOpen>,
    paused: Res<crate::pause::Paused>,
    mut q: Query<&mut Visibility, With<DebugPanel>>,
) {
    let want = if customize_open.open || main_menu.0 || paused.0 || !visible.0 {
        Visibility::Hidden
    } else {
        Visibility::Inherited
    };
    for mut v in &mut q {
        if *v != want { *v = want; }
    }
}
