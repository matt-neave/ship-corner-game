//! Borderlands-style boss intro: a dramatic ~3-second beat between the
//! final regular spawn and the boss actually arriving. Combat freezes
//! because `BossIntro` isn't in `in_combat_view`'s allow-list, so every
//! gameplay-affecting system idles for the duration.
//!
//! Flow:
//! - `spawn_enemies` would have called `spawn_boss` on the final wave.
//!   Instead it stashes the queued spawn into `BossIntroPending` and
//!   asks the state machine to enter `BossIntro`.
//! - `OnEnter(BossIntro)` spawns the overlay (backdrop + two sweeping
//!   white bars + big class label) and resets the timer.
//! - `tick_boss_intro` animates the bars + advances the timer.
//! - At `DURATION` the timer queues `NextState(Playing)`.
//! - `OnExit(BossIntro)` despawns the overlay AND drops the real boss
//!   into the arena from the pending data, then clears the resource.

use bevy::prelude::*;
use bevy::text::FontSmoothing;

use crate::ally::ShipClass;
use crate::effects::EffectMeshes;
use crate::palette::PaletteMaterials;
use crate::ui_kit::theme;
use crate::AppState;

/// Owns everything the boss-intro screen needs: its two resources, the
/// OnEnter/OnExit spawn-and-teardown hooks, and the tick system that
/// drives the streak animation + state hand-off back to `Playing`.
pub struct BossIntroPlugin;

impl Plugin for BossIntroPlugin {
    fn build(&self, app: &mut App) {
        app
            .insert_resource(BossIntroTimer::default())
            .insert_resource(BossIntroPending::default())
            .add_systems(OnEnter(AppState::BossIntro), enter_boss_intro)
            .add_systems(OnExit(AppState::BossIntro), exit_boss_intro)
            .add_systems(
                Update,
                tick_boss_intro.run_if(in_state(AppState::BossIntro)),
            );
    }
}

/// Total intro length, in seconds. Long enough for the player to read
/// the class name; short enough to keep the campaign moving.
pub const DURATION: f32 = 2.8;

/// Window during which the streaks slide in from off-screen. After this
/// they hold their resting positions for the rest of the intro.
const SWEEP_IN_TIME: f32 = 0.45;

/// Vertical % of the viewport each accent streak occupies. Thin — they
/// frame the text band rather than dominate it. Bigger numbers turn
/// the screen into white-on-white and swallow the class label.
const STREAK_HEIGHT_PCT: f32 = 1.6;

/// Vertical positions (in viewport %) of the top and bottom accent
/// streaks. Chosen so the BOSS subtitle + big class label sit
/// comfortably in the gap between them.
const TOP_STREAK_Y_PCT: f32    = 36.0;
const BOTTOM_STREAK_Y_PCT: f32 = 60.0;

/// Held data for the boss that's about to spawn. `spawn_enemies` writes
/// it when it would have called `spawn_boss`; `exit_boss_intro` consumes
/// it and finally drops the ship into the arena.
#[derive(Resource, Default, Clone, Copy)]
pub struct BossIntroPending {
    pub class: Option<ShipClass>,
    pub pos: Vec2,
    pub heading: f32,
    /// Star tier of the section the boss is spawning in. `spawn_boss`
    /// multiplies the class's base `boss_hp()` by this, so a 5★ boss
    /// is ~5× as durable as a 3★ boss of the same class.
    pub stars: u8,
    /// Stage progression at spawn time (campaign.battles_cleared). Folded
    /// into the boss HP multiplier so each successive boss tanks harder.
    pub battles_cleared: u32,
}

/// Elapsed time since `OnEnter(BossIntro)` fired. Drives the streak
/// sweep + transition back to `Playing`.
#[derive(Resource, Default)]
pub struct BossIntroTimer(pub f32);

/// Marker on every UI entity owned by the intro overlay. `OnExit` walks
/// this and despawns the tree.
#[derive(Component)]
pub struct BossIntroUi;

/// Per-bar marker so the tick system can drive their slide-in. `from_left`
/// flips the entry side; `target_left_pct` is the resting Left value in
/// percent of viewport width.
#[derive(Component)]
pub struct BossIntroStreak {
    pub from_left: bool,
    pub target_left_pct: f32,
}

pub fn enter_boss_intro(
    mut commands: Commands,
    mut timer: ResMut<BossIntroTimer>,
    pending: Res<BossIntroPending>,
) {
    timer.0 = 0.0;
    let class_label = pending.class.map(|c| c.label()).unwrap_or("BOSS");

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
                row_gap: Val::Px(6.0),
                ..default()
            },
            // Heavier dim so the bars + text dominate. The previous
            // 0.55 alpha let the arena bleed through enough to make
            // the class label hard to read on busy backgrounds.
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.78)),
            ZIndex(220),
            Visibility::Inherited,
            BossIntroUi,
        ))
        .with_children(|root| {
            // Two thin horizontal accent streaks that FRAME the text
            // band — top streak above the BOSS subtitle, bottom streak
            // below the class label. Sweep in from opposite sides
            // (Borderlands-style swipe). Previously they were thick
            // white bars sitting ON TOP of the text, which made the
            // white class label invisible.
            root.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Percent(TOP_STREAK_Y_PCT),
                    left: Val::Percent(-110.0),
                    width: Val::Percent(110.0),
                    height: Val::Percent(STREAK_HEIGHT_PCT),
                    ..default()
                },
                BackgroundColor(theme::ACCENT),
                BossIntroStreak { from_left: true, target_left_pct: -5.0 },
                BossIntroUi,
            ));
            root.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Percent(BOTTOM_STREAK_Y_PCT),
                    left: Val::Percent(100.0),
                    width: Val::Percent(110.0),
                    height: Val::Percent(STREAK_HEIGHT_PCT),
                    ..default()
                },
                BackgroundColor(theme::ACCENT),
                BossIntroStreak { from_left: false, target_left_pct: -5.0 },
                BossIntroUi,
            ));

            // Text block in the middle — flex children of the root so
            // their vertical positions auto-centre and stay stable
            // regardless of font size or window dimensions. Previously
            // each was absolute-positioned via `top: %`, which let the
            // class label drift into the bars.
            root.spawn((
                Text::new("BOSS"),
                TextFont {
                    font_size: 26.0,
                    font_smoothing: FontSmoothing::None,
                    ..default()
                },
                TextColor(theme::NERF_FG),
                BossIntroUi,
            ));
            root.spawn((
                Text::new(class_label),
                TextFont {
                    font_size: 60.0,
                    font_smoothing: FontSmoothing::None,
                    ..default()
                },
                TextColor(theme::ON_SURFACE),
                BossIntroUi,
            ));
        });
}

pub fn tick_boss_intro(
    time: Res<Time>,
    mut timer: ResMut<BossIntroTimer>,
    mut next: ResMut<NextState<crate::AppState>>,
    mut streaks: Query<(&BossIntroStreak, &mut Node)>,
) {
    timer.0 += time.delta_secs();

    // Streak sweep: ease the bars from their off-screen start to the
    // resting target over `SWEEP_IN_TIME`, then hold. Linear is fine
    // here — the bars are travelling fast enough that the eye reads
    // them as a swipe regardless of the curve.
    let p = (timer.0 / SWEEP_IN_TIME).clamp(0.0, 1.0);
    for (streak, mut node) in &mut streaks {
        let start = if streak.from_left { -110.0 } else { 100.0 };
        let lerped = start + (streak.target_left_pct - start) * p;
        let want = Val::Percent(lerped);
        if node.left != want { node.left = want; }
    }

    if timer.0 >= DURATION {
        next.set(crate::AppState::Playing);
    }
}

pub fn exit_boss_intro(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    pm: Option<Res<PaletteMaterials>>,
    em: Option<Res<EffectMeshes>>,
    mut pending: ResMut<BossIntroPending>,
    ui: Query<Entity, With<BossIntroUi>>,
) {
    for e in &ui {
        commands.entity(e).despawn();
    }
    // Now drop the actual boss into the arena. Caches must be ready
    // — they were when `spawn_enemies` queued the intro — so we
    // shouldn't see the `None` branches in practice. The bail-out is
    // defensive.
    let (Some(pm), Some(em)) = (pm, em) else { return; };
    if let Some(class) = pending.class.take() {
        crate::ally::spawn_boss(
            &mut commands, &pm, &em, &mut meshes,
            pending.pos, pending.heading, class, pending.stars,
            pending.battles_cleared,
        );
    }
}
