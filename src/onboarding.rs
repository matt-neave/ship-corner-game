//! First-encounter onboarding banners + synergy discovery popups.
//!
//! Two banner kinds, same shape, sharing the same bottom-left stack
//! + lifecycle:
//!  1. **NEW!** — a new enemy variant spawns this run.
//!  2. **Discovered!** — the player equips 2 of a weapon tag for
//!     the first time, unlocking that tag's synergy.
//!
//! Both carry `NotificationLifetime` so a single tick system counts
//! them down + despawns at zero, and the stacking offset uses that
//! shared marker so the two kinds slot in together rather than
//! piling at the same position.
//!
//! Discovery and "seen" state both reset on run-start (RESTART or
//! returning to MainMenu) so each fresh run re-teaches threats and
//! re-locks the synergies.

use bevy::prelude::*;

use crate::enemy::{EnemyVariant, ALL_VARIANTS};
use crate::ui_kit::theme;
use crate::weapon::WeaponTag;

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

/// Per-tag bitmask of synergies the player has unlocked this run.
/// A tag is "discovered" the first time its `Synergies` tier reaches
/// 1 (i.e. the player has 2 of that tag equipped). Once discovered
/// stays discovered for the rest of the run — the synergy tooltip
/// reveals its description + value ladder, and the player saw the
/// pop-up. Resets on MainMenu enter alongside `SeenVariants`.
#[derive(Resource, Default, Clone, Copy, Debug)]
pub struct DiscoveredSynergies(pub u8);

impl DiscoveredSynergies {
    pub fn has(&self, t: WeaponTag) -> bool {
        self.0 & tag_bit(t) != 0
    }
    pub fn mark(&mut self, t: WeaponTag) {
        self.0 |= tag_bit(t);
    }
    pub fn reset(&mut self) {
        self.0 = 0;
    }
}

fn tag_bit(t: WeaponTag) -> u8 {
    let idx = WeaponTag::all().iter().position(|&x| x == t).unwrap_or(0);
    1u8 << idx
}

/// Shared lifecycle component on every banner. `tick_notifications`
/// decrements `remaining` and despawns the entity at zero. The
/// banner-type markers (`NewEnemyBanner` / `SynergyDiscoveredBanner`)
/// are unit structs used only for type-specific queries; the timer
/// itself lives here so a single tick system covers both kinds.
#[derive(Component)]
pub struct NotificationLifetime {
    pub remaining: f32,
}

/// Marker for the "NEW! <enemy variant>" pop-up.
#[derive(Component)]
pub struct NewEnemyBanner;

/// Marker for the "Discovered! <tag synergy>" pop-up.
#[derive(Component)]
pub struct SynergyDiscoveredBanner;

pub struct OnboardingPlugin;

impl Plugin for OnboardingPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(SeenVariants::default())
            .insert_resource(DiscoveredSynergies::default())
            .add_systems(Update, tick_notifications)
            // Reset both on returning to main menu so the next run
            // starts with a fresh slate of unseen variants AND
            // re-locked synergies.
            .add_systems(OnEnter(crate::AppState::MainMenu), reset_on_main_menu);
    }
}

/// Per-frame: count down each notification's remaining time and
/// despawn when it hits 0. Generic across NEW! and Discovered!
/// pop-ups via the shared `NotificationLifetime` component.
pub fn tick_notifications(
    time: Res<Time>,
    mut commands: Commands,
    mut q: Query<(Entity, &mut NotificationLifetime)>,
) {
    let dt = time.delta_secs();
    for (e, mut b) in &mut q {
        b.remaining -= dt;
        if b.remaining <= 0.0 {
            commands.entity(e).despawn();
        }
    }
}

/// Spec for the bottom-left panel — shared sizing so both banner
/// kinds line up in the stack at identical bounds. Tall enough to
/// fit a header + a wrapped two-line description without clipping;
/// `min_height` on the Node lets it grow further on the rare long
/// string, with `stack_bottom_px` using the conservative `PANEL_H`
/// so even an over-grown banner doesn't crash into its neighbour.
const PANEL_W: f32 = 260.0;
const PANEL_H: f32 = 92.0;
const PANEL_INSET: f32 = 12.0;
const PANEL_GAP: f32 = 6.0;

/// `existing_banners` is any query that captures BOTH banner kinds
/// (via `With<NotificationLifetime>`); the count drives the
/// per-banner vertical offset so a synergy popup that lands during
/// an active NEW! banner stacks above it instead of overlapping.
fn stack_bottom_px(count: usize) -> f32 {
    PANEL_INSET + (count as f32) * (PANEL_H + PANEL_GAP)
}

/// Spawn the bottom-left "New!" panel for `variant`. Three lines:
/// accent-yellow "New!" header, the variant's name in body white,
/// then a short plain-language behaviour cue underneath so the
/// player learns the threat in one read.
pub fn spawn_new_enemy_banner(
    commands: &mut Commands,
    existing_banners: &Query<Entity, With<NotificationLifetime>>,
    variant: EnemyVariant,
) {
    let bottom_px = stack_bottom_px(existing_banners.iter().count());
    spawn_text_banner(
        commands,
        bottom_px,
        theme::ACCENT,
        "New!",
        Some(variant.label()),
        enemy_short_desc(variant),
        theme::ON_SURFACE,
        NewEnemyBanner,
    );
}

/// Spawn the bottom-left "Discovered!" panel announcing a freshly
/// unlocked synergy. Same three-line layout as the New! enemy
/// panel — the two share `NotificationLifetime` so they stack via
/// `stack_bottom_px`. Border + header take the tag's accent color
/// so each unlock reads distinct at a glance.
pub fn spawn_synergy_discovered_banner(
    commands: &mut Commands,
    existing_banners: &Query<Entity, With<NotificationLifetime>>,
    tag: WeaponTag,
) {
    let bottom_px = stack_bottom_px(existing_banners.iter().count());
    spawn_text_banner(
        commands,
        bottom_px,
        tag.color(),
        "Discovered!",
        Some(tag.label()),
        "New weapon synergy.",
        theme::ON_SURFACE,
        SynergyDiscoveredBanner,
    );
}

/// Shared three-line notification panel used by both
/// `spawn_new_enemy_banner` and `spawn_synergy_discovered_banner`.
/// They differ in the accent colour, the strings, and the marker
/// component. Layout is a column with `FlexStart` so the header
/// sits on top, optional subtitle (e.g. variant or tag name) sits
/// below it, and the description wraps inside the panel width
/// underneath.
fn spawn_text_banner<M: Component>(
    commands: &mut Commands,
    bottom_px: f32,
    accent: Color,
    header: &str,
    subtitle: Option<&str>,
    description: impl Into<String>,
    description_color: Color,
    marker: M,
) {
    let description = description.into();
    let subtitle = subtitle.map(|s| s.to_string());
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                bottom: Val::Px(bottom_px),
                left: Val::Px(PANEL_INSET),
                width: Val::Px(PANEL_W),
                // `min_height` instead of `height` so a longer
                // wrapped description grows the panel vertically
                // instead of getting clipped at the bottom edge.
                min_height: Val::Px(PANEL_H),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::FlexStart,
                justify_content: JustifyContent::FlexStart,
                padding: UiRect::axes(Val::Px(12.0), Val::Px(10.0)),
                row_gap: Val::Px(6.0),
                border: UiRect::all(Val::Px(2.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.07, 0.08, 0.11, 0.92)),
            BorderColor(accent),
            ZIndex(190),
            NotificationLifetime { remaining: BANNER_DURATION },
            marker,
        ))
        .with_children(|root| {
            // Header: "New!" / "Discovered!" — bold accent line.
            root.spawn((
                Text::new(header.to_string()),
                TextFont { font_size: 18.0, ..default() },
                TextColor(accent),
            ));
            // Optional subtitle: the thing being announced
            // (variant name for enemies, tag name for synergies).
            // White/body colour so the accent header still
            // dominates the colour hierarchy.
            if let Some(sub) = subtitle {
                root.spawn((
                    Text::new(sub),
                    TextFont { font_size: 15.0, ..default() },
                    TextColor(description_color),
                ));
            }
            // Description — wraps inside the panel width.
            root.spawn((
                Text::new(description),
                TextFont { font_size: 14.0, ..default() },
                TextColor(description_color),
            ));
        });
}

/// One-sentence plain-language summary of each enemy variant. Goes
/// under the "New!" header on first encounter so the player reads
/// a behaviour cue rather than just a name + silhouette.
fn enemy_short_desc(v: EnemyVariant) -> &'static str {
    // Punchy, game-flavoured one-liners. No separator dashes
    // (Bevy's default font has no em-dash glyph and the spaced
    // hyphen reads as LLM filler) and no parenthetical asides.
    match v {
        EnemyVariant::Standard  => "Common contact ship. Closes to ram.",
        EnemyVariant::Scout     => "Fast skirmisher. Circles and harasses.",
        EnemyVariant::Heavy     => "Slow armoured brute. Soaks damage.",
        EnemyVariant::Bomber    => "Sprints in. Explodes on contact.",
        EnemyVariant::Rammer    => "Charges hull first at full speed.",
        EnemyVariant::Sniper    => "Stops to aim. Fires heavy rounds.",
        EnemyVariant::Artillery => "Lobs shells from afar.",
    }
}

fn reset_on_main_menu(
    mut seen: ResMut<SeenVariants>,
    mut discovered: ResMut<DiscoveredSynergies>,
) {
    seen.reset();
    discovered.reset();
}
