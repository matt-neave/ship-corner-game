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

/// Spawn the bottom-left "NEW!" panel for `variant`. Bevy UI Node
/// based so the text renders at NATIVE screen resolution and stays
/// readable — the PLAY_LAYER experiment looked like-in-game but
/// text was illegible. Sprite is a capsule-shaped colour block
/// (rounded corners on the short axis) standing in for the actual
/// enemy silhouette. Stacks ABOVE existing banners.
pub fn spawn_new_enemy_banner(
    commands: &mut Commands,
    existing_banners: &Query<Entity, With<NewEnemyBanner>>,
    variant: EnemyVariant,
) {
    let body_color = display_color_for(variant);
    let panel_w: f32 = 120.0;
    let panel_h: f32 = 120.0;
    let stack_index = existing_banners.iter().count() as f32;
    let bottom_px = 12.0 + stack_index * (panel_h + 6.0);

    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                bottom: Val::Px(bottom_px),
                left: Val::Px(12.0),
                width: Val::Px(panel_w),
                height: Val::Px(panel_h),
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
            // "NEW!" header — accent yellow, large enough to read.
            root.spawn((
                Text::new("NEW!"),
                TextFont { font_size: 16.0, ..default() },
                TextColor(theme::ACCENT),
            ));

            // Sprite stand-in — capsule-shaped colour block in the
            // variant's body hue. Rounded ends on the short axis
            // approximate the in-game capsule silhouette. Black
            // outline frames it against the dark panel; a small
            // dark chip near the top reads as the bow / warhead
            // marker the real enemy mesh would have.
            root.spawn((
                Node {
                    width: Val::Px(36.0),
                    height: Val::Px(56.0),
                    border: UiRect::all(Val::Px(2.0)),
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::FlexStart,
                    padding: UiRect::all(Val::Px(0.0)),
                    ..default()
                },
                BackgroundColor(body_color),
                BorderColor(Color::srgb(0.0, 0.0, 0.0)),
                BorderRadius::all(Val::Px(18.0)),
            ))
            .with_children(|sprite| {
                sprite.spawn((
                    Node {
                        width: Val::Px(14.0),
                        height: Val::Px(8.0),
                        margin: UiRect { top: Val::Px(6.0), ..default() },
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.55)),
                    BorderRadius::all(Val::Px(3.0)),
                ));
            });

            // Variant name — white, full-size.
            root.spawn((
                Text::new(variant.label()),
                TextFont { font_size: 14.0, ..default() },
                TextColor(theme::ON_SURFACE),
            ));
        });
}

/// Sprite-stand-in colour for each variant — matches the body
/// material the enemy actually renders with so the player learns
/// the colour-to-threat association from the banner.
fn display_color_for(v: EnemyVariant) -> Color {
    match v {
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
