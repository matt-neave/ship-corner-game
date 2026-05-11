//! First-encounter onboarding banner.
//!
//! When a new enemy variant spawns for the first time IN THE CURRENT
//! RUN, a small square panel pops up at the bottom-left of the
//! screen for `BANNER_DURATION` seconds: a "NEW!" header, a
//! body-color sprite stand-in, and the variant's name. Resets on
//! run-start (RESTART or returning to MainMenu) so each fresh run
//! re-introduces the threats.
//!
//! Pairs with `EnemyVariant::unlock_battles` — together they ensure
//! the player meets new threats one at a time, with a clear visual
//! call-out the moment each one shows up.

use bevy::prelude::*;

use crate::enemy::{EnemyVariant, ALL_VARIANTS};
use crate::palette::{
    hex, ENEMY_ARTILLERY_HEX, ENEMY_RAMMER_HEX, ENEMY_SNIPER_HEX,
};
use crate::ui_kit::theme;

/// How long the panel stays on screen after a new-variant first-spawn.
pub const BANNER_DURATION: f32 = 10.0;

/// Bitmask resource — one bit per variant. Set on the variant's
/// first spawn this run; checked in `spawn_enemies` to decide
/// whether to fire the banner. Reset on run-start so a fresh PLAY /
/// RESTART re-introduces every variant.
#[derive(Resource, Default, Clone, Copy, Debug)]
pub struct SeenVariants(pub u8);

impl SeenVariants {
    pub fn has(&self, v: EnemyVariant) -> bool {
        self.0 & bit_for(v) != 0
    }
    pub fn mark(&mut self, v: EnemyVariant) {
        self.0 |= bit_for(v);
    }
    pub fn reset(&mut self) {
        self.0 = 0;
    }
}

fn bit_for(v: EnemyVariant) -> u8 {
    let idx = ALL_VARIANTS.iter().position(|&x| x == v).unwrap_or(0);
    1u8 << idx
}

/// Marker on the banner root Node. Carries the seconds remaining;
/// `tick_new_enemy_banner` decrements and despawns at 0.
#[derive(Component)]
pub struct NewEnemyBanner {
    pub remaining: f32,
}

pub struct OnboardingPlugin;

impl Plugin for OnboardingPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(SeenVariants::default())
            .add_systems(Update, tick_new_enemy_banner)
            // Reset on returning to main menu so the next run
            // starts with a fresh slate of unseen variants.
            .add_systems(OnEnter(crate::AppState::MainMenu), reset_on_main_menu);
    }
}

/// Per-frame: count down each banner's remaining time and despawn
/// when it hits 0.
pub fn tick_new_enemy_banner(
    time: Res<Time>,
    mut commands: Commands,
    mut q: Query<(Entity, &mut NewEnemyBanner)>,
) {
    let dt = time.delta_secs();
    for (e, mut b) in &mut q {
        b.remaining -= dt;
        if b.remaining <= 0.0 {
            commands.entity(e).despawn();
        }
    }
}

/// Spawn the bottom-left "NEW!" panel for `variant`. Called from
/// `enemy::spawn_enemies` the first time a variant appears in a run.
/// Stacks ABOVE existing banners — they don't replace each other,
/// so a wave that introduces multiple new variants in quick
/// succession shows them all in a column.
pub fn spawn_new_enemy_banner(
    commands: &mut Commands,
    existing_banners: &Query<Entity, With<NewEnemyBanner>>,
    variant: EnemyVariant,
) {
    let body_color = display_color_for(variant);
    let panel_size: f32 = 110.0;
    // Stack vertically: each new banner sits one panel-height
    // (+ small gap) above the next-newest one.
    let stack_index = existing_banners.iter().count() as f32;
    let bottom_px = 12.0 + stack_index * (panel_size + 6.0);

    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                bottom: Val::Px(bottom_px),
                left: Val::Px(12.0),
                width: Val::Px(panel_size),
                height: Val::Px(panel_size),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::SpaceBetween,
                padding: UiRect::all(Val::Px(8.0)),
                border: UiRect::all(Val::Px(2.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.07, 0.08, 0.11, 0.92)),
            BorderColor(theme::ACCENT),
            ZIndex(190),
            NewEnemyBanner { remaining: BANNER_DURATION },
        ))
        .with_children(|root| {
            // "NEW!" header — bright accent, stands out against the
            // dark panel.
            root.spawn((
                Text::new("NEW!"),
                TextFont { font_size: 14.0, ..default() },
                TextColor(theme::ACCENT),
            ));

            // Sprite stand-in — a colored capsule-ish block in the
            // variant's body color. Rounded corners approximate the
            // capsule silhouette of the actual enemy mesh.
            root.spawn((
                Node {
                    width: Val::Px(28.0),
                    height: Val::Px(46.0),
                    border: UiRect::all(Val::Px(1.0)),
                    ..default()
                },
                BackgroundColor(body_color),
                BorderColor(Color::srgba(0.0, 0.0, 0.0, 0.5)),
                BorderRadius::all(Val::Px(14.0)),
            ));

            // Variant name — white, bottom-aligned by SpaceBetween.
            root.spawn((
                Text::new(variant.label()),
                TextFont { font_size: 12.0, ..default() },
                TextColor(theme::ON_SURFACE),
            ));
        });
}

/// Sprite-stand-in colour for each variant — matches the body
/// material the enemy actually renders with so the player learns the
/// "X colour means Y threat" association.
fn display_color_for(v: EnemyVariant) -> Color {
    match v {
        // Standard / Scout / Heavy / Bomber use the active palette's
        // enemy hue (palette-driven, not a fixed hex). Hardcoded to
        // the AAP-64 naval defaults from `Palette::aap64_naval` so
        // the banner reads correctly even if the palette swaps later
        // (we'd update this match in the same pass).
        EnemyVariant::Standard  => hex("#b13e53"),
        EnemyVariant::Scout     => hex("#c87a8e"),
        EnemyVariant::Heavy     => hex("#5e2230"),
        EnemyVariant::Bomber    => hex("#571c27"),
        EnemyVariant::Rammer    => hex(ENEMY_RAMMER_HEX),
        EnemyVariant::Sniper    => hex(ENEMY_SNIPER_HEX),
        EnemyVariant::Artillery => hex(ENEMY_ARTILLERY_HEX),
    }
}

fn reset_on_main_menu(mut seen: ResMut<SeenVariants>) {
    seen.reset();
}
