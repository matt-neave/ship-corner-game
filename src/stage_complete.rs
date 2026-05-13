//! 5-second "STAGE COMPLETE" buffer between clearing a level and the
//! shop opening.
//!
//! Architected as its own `AppState` variant so combat sim freezes for
//! the duration (gameplay-affecting systems are already gated on
//! `state == Playing`, so they idle automatically). The screen is a
//! transparent overlay with centred accent text — no dark backdrop, so
//! the player can still see their ship sitting in the cleared arena.
//!
//! Lifecycle:
//! - `OnEnter(StageComplete)` spawns the UI + resets the timer.
//! - `tick_stage_complete` increments while the state is active.
//! - At `DURATION` seconds the system queues `NextState(Customize)`.
//! - `OnExit(StageComplete)` despawns the UI; the next-round combat
//!   budget was already queued by `level_complete_check` so the shop
//!   has work to do as soon as it closes.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;
use bevy::text::FontSmoothing;
use bevy::window::PrimaryWindow;

use crate::balance::UPSCALE_LAYER;
use crate::ui_kit::theme;
use crate::AppState;

/// Owns the "STAGE COMPLETE" buffer: the elapsed-time + scrap-earned
/// resources, the per-stage scrap reset on `OnEnter(Playing)`, the
/// overlay spawn/despawn on the state itself, and the gated tick
/// systems that animate the title + advance to the next screen.
pub struct StageCompletePlugin;

impl Plugin for StageCompletePlugin {
    fn build(&self, app: &mut App) {
        app
            .insert_resource(StageCompleteTimer::default())
            .insert_resource(ScrapEarnedThisStage::default())
            .add_systems(OnEnter(AppState::StageComplete), enter_stage_complete)
            // Stage advances on EXIT so the in-buffer readout still
            // shows the just-finished stage, not the next stage's
            // "WAVE 1/N". `queue_next_stage_combat` lives in `map`,
            // not here, so it's registered alongside in main. We
            // also reset the per-stage scrap tally here — once per
            // stage end, after the payout has been displayed. The
            // earlier hook on `OnEnter(Playing)` wiped the tally
            // whenever a mid-stage level-up returned to Playing.
            .add_systems(
                OnExit(AppState::StageComplete),
                (exit_stage_complete, reset_scrap_earned_on_play),
            )
            .add_systems(
                Update,
                (tick_stage_complete, tick_stage_complete_wave, tick_payout_reveal)
                    .run_if(in_state(AppState::StageComplete)),
            )
            // Transition wipe runs unconditionally so it survives the
            // state swap it triggers — the entity outlives the
            // StageComplete state, paints the new screen white, then
            // collapses to reveal it.
            .add_systems(Update, tick_transition);
    }
}

/// Total buffer length in seconds.
pub const DURATION: f32 = 5.0;
/// Wavey title — vertical bob amplitude per character (px).
const WAVE_AMP: f32 = 8.0;
/// Wavey title — angular frequency of the bob (rad/s).
const WAVE_SPEED: f32 = 5.0;
/// Wavey title — phase offset between adjacent characters (rad).
/// Bigger value = tighter ripple, smaller = the whole word moves
/// closer to in-sync.
const WAVE_PHASE_PER_CHAR: f32 = 0.45;

/// Radial-wipe transition timings (modelled on SNKRX's
/// `TransitionEffect` in `shared.lua`). Expand grows a white circle
/// from the screen centre until it covers the window; the state
/// swap fires at the moment of full coverage; hold gives a beat of
/// pure white; collapse shrinks the circle back to reveal the new
/// state underneath.
const TRANSITION_EXPAND: f32   = 0.30;
const TRANSITION_HOLD: f32     = 0.18;
const TRANSITION_COLLAPSE: f32 = 0.30;
const TRANSITION_TOTAL: f32    =
    TRANSITION_EXPAND + TRANSITION_HOLD + TRANSITION_COLLAPSE;

/// Delay before the first payout line reveals.
const PAYOUT_FIRST_DELAY: f32 = 0.20;
/// Gap between successive payout lines popping in.
const PAYOUT_LINE_GAP: f32 = 0.22;
/// Duration of the white-flash punch on each line as it reveals,
/// before easing back to the line's base colour.
const PAYOUT_FLASH_DURATION: f32 = 0.18;
const PAYOUT_FLASH_COLOR: Color = Color::WHITE;

/// Time elapsed since `OnEnter(StageComplete)` fired. Reset on entry,
/// ticked during the state, ignored otherwise.
#[derive(Resource, Default)]
pub struct StageCompleteTimer(pub f32);

/// Running tally of scrap earned during the current combat stage.
/// `enemy_death_check` adds every kill drop to this resource as well
/// as the live `Scrap` total; `enter_stage_complete` reads it to
/// render the "+N SCRAP" payout line. `OnEnter(Playing)` resets it
/// so each fresh stage counts from zero.
#[derive(Resource, Default)]
pub struct ScrapEarnedThisStage(pub u32);

/// Bundled mutable access to both scrap counters — used by systems
/// that already maxed Bevy's 16-param SystemParam limit. Grants and
/// the per-stage tally always move in lockstep so wrapping them is
/// a net simplification at every callsite.
#[derive(bevy::ecs::system::SystemParam)]
pub struct ScrapWriter<'w> {
    pub total: ResMut<'w, crate::Scrap>,
    pub earned: ResMut<'w, ScrapEarnedThisStage>,
}

impl ScrapWriter<'_> {
    pub fn grant(&mut self, amount: u32) {
        self.total.0 = self.total.0.saturating_add(amount);
        self.earned.0 = self.earned.0.saturating_add(amount);
    }
}

/// Reset the per-stage scrap tally. Registered on `OnExit(StageComplete)`
/// so it fires exactly once per finished stage — after the payout has
/// been rendered and just before the shop opens. Not on
/// `OnEnter(Playing)` because mid-stage level-ups bounce the state
/// Playing → LevelUp → Playing, which would otherwise wipe the
/// running tally.
pub fn reset_scrap_earned_on_play(mut s: ResMut<ScrapEarnedThisStage>) {
    s.0 = 0;
}

#[derive(Component)]
pub struct StageCompleteUi;

/// Per-character marker on each glyph in the wavey title. `idx` drives
/// the per-char phase offset so the bob ripples left-to-right.
#[derive(Component)]
pub struct StageCompleteWaveChar { pub idx: usize }

/// One staggered payout line under the title. `idx` drives the reveal
/// order (0, 1, 2…). `base_color` is the colour the line settles on
/// after the flash punch fades.
#[derive(Component)]
pub struct StagePayoutLine {
    pub idx: u8,
    pub base_color: Color,
}

/// Radial white-wipe transition entity. Lives on `UPSCALE_LAYER` so
/// it survives state changes and the `UpscaleCamera` keeps drawing
/// it. `tick_transition` animates `Transform.scale`, fires the
/// queued `next.set(target_state)` at the apex, then despawns when
/// the collapse finishes.
#[derive(Component)]
pub struct TransitionEffect {
    pub elapsed: f32,
    pub target_state: AppState,
    pub state_swapped: bool,
}

pub fn enter_stage_complete(
    mut commands: Commands,
    mut timer: ResMut<StageCompleteTimer>,
    mut scrap_earned: ResMut<ScrapEarnedThisStage>,
    mut scrap: ResMut<crate::Scrap>,
) {
    timer.0 = 0.0;
    // Interest: +1 scrap per 5 held going INTO the stage, before this
    // round's wave-clear earnings stack onto the pile. Subtracting
    // `scrap_earned.0` from the current total gives the principal the
    // player walked into the stage with.
    let earned_pre_interest = scrap_earned.0;
    let pre_round_principal = scrap.0.saturating_sub(scrap_earned.0);
    let interest = pre_round_principal / 5;
    if interest > 0 {
        scrap.0 = scrap.0.saturating_add(interest);
        scrap_earned.0 = scrap_earned.0.saturating_add(interest);
    }
    let total = earned_pre_interest + interest;

    let payout_color = Color::srgb(1.0, 0.85, 0.30);
    let total_color  = theme::ACCENT;
    let line_specs: [(String, Color); 3] = [
        (format!("EARNED: +{}", earned_pre_interest),  payout_color),
        (format!("INTEREST: +{}", interest),           payout_color),
        (format!("TOTAL: +{}", total),                 total_color),
    ];

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
                ..default()
            },
            BackgroundColor(Color::NONE),
            ZIndex(180),
            Visibility::Inherited,
            StageCompleteUi,
        ))
        .with_children(|root| {
            // Per-character glyphs in a flex row so each one can bob
            // independently. `tick_stage_complete_wave` updates each
            // glyph's `Node.top` from its `idx` each frame, producing
            // a left-to-right ripple. Splitting the title into N
            // entities forfeits cross-glyph kerning, which is fine
            // for the chunky pixel font.
            root.spawn(Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                ..default()
            })
            .with_children(|row| {
                for (i, ch) in "STAGE COMPLETE".chars().enumerate() {
                    // Use a non-breaking space for the gap so the
                    // glyph node doesn't collapse / get trimmed.
                    let s = if ch == ' ' { "\u{00A0}".to_string() } else { ch.to_string() };
                    row.spawn((
                        Text::new(s),
                        TextFont {
                            font_size: 48.0,
                            font_smoothing: FontSmoothing::None,
                            ..default()
                        },
                        TextColor(theme::ACCENT),
                        Node {
                            position_type: PositionType::Relative,
                            ..default()
                        },
                        StageCompleteWaveChar { idx: i },
                    ));
                }
            });
            // Three payout lines stagger-revealed by `tick_payout_reveal`.
            // Spawned `Visibility::Hidden`; each becomes visible when
            // `StageCompleteTimer` crosses its reveal threshold and
            // pulses white → its base colour as a "punch" cue.
            root.spawn(Node {
                margin: UiRect { top: Val::Px(24.0), ..default() },
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(6.0),
                ..default()
            })
            .with_children(|wrap| {
                for (idx, (text, base_color)) in line_specs.iter().enumerate() {
                    wrap.spawn((
                        Text::new(text.clone()),
                        TextFont {
                            font_size: if idx == 2 { 32.0 } else { 26.0 },
                            font_smoothing: FontSmoothing::None,
                            ..default()
                        },
                        TextColor(*base_color),
                        Visibility::Hidden,
                        StagePayoutLine {
                            idx: idx as u8,
                            base_color: *base_color,
                        },
                    ));
                }
            });
        });
}

/// Stagger-reveal the payout lines under the title. Each line's
/// reveal threshold is `PAYOUT_FIRST_DELAY + idx × PAYOUT_LINE_GAP`;
/// after crossing it the line goes visible and its colour pulses from
/// `PAYOUT_FLASH_COLOR` → its stored base over `PAYOUT_FLASH_DURATION`
/// seconds. The pulse is what the eye lands on, mimicking the
/// "[nudge_down]" effect SNKRX uses on its end-of-round screen.
pub fn tick_payout_reveal(
    timer: Res<StageCompleteTimer>,
    mut q: Query<(&StagePayoutLine, &mut Visibility, &mut TextColor)>,
) {
    let t = timer.0;
    for (line, mut vis, mut color) in &mut q {
        let reveal_at = PAYOUT_FIRST_DELAY + line.idx as f32 * PAYOUT_LINE_GAP;
        if t < reveal_at {
            if *vis != Visibility::Hidden { *vis = Visibility::Hidden; }
            continue;
        }
        if *vis != Visibility::Inherited { *vis = Visibility::Inherited; }
        let since = t - reveal_at;
        let want = if since >= PAYOUT_FLASH_DURATION {
            line.base_color
        } else {
            // Smooth-step the flash → base mix so the pulse settles
            // softly instead of snapping.
            let k = since / PAYOUT_FLASH_DURATION;
            let k = k * k * (3.0 - 2.0 * k);
            lerp_color(PAYOUT_FLASH_COLOR, line.base_color, k)
        };
        if color.0 != want { color.0 = want; }
    }
}

fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let a: bevy::color::Srgba = a.into();
    let b: bevy::color::Srgba = b.into();
    Color::srgba(
        a.red   + (b.red   - a.red)   * t,
        a.green + (b.green - a.green) * t,
        a.blue  + (b.blue  - a.blue)  * t,
        a.alpha + (b.alpha - a.alpha) * t,
    )
}

/// Bob each glyph along Y based on `(time × WAVE_SPEED + idx ×
/// WAVE_PHASE_PER_CHAR)`. Negative `top` lifts the glyph above its
/// natural flex position; positive drops it below.
pub fn tick_stage_complete_wave(
    time: Res<Time>,
    mut q: Query<(&StageCompleteWaveChar, &mut Node)>,
) {
    let t = time.elapsed_secs();
    for (c, mut node) in &mut q {
        let phase = c.idx as f32 * WAVE_PHASE_PER_CHAR;
        let bob = -(t * WAVE_SPEED + phase).sin() * WAVE_AMP;
        let want = Val::Px(bob);
        if node.top != want { node.top = want; }
    }
}

pub fn exit_stage_complete(
    mut commands: Commands,
    q: Query<Entity, With<StageCompleteUi>>,
) {
    for e in &q {
        commands.entity(e).despawn();
    }
}

pub fn tick_stage_complete(
    time: Res<Time>,
    mut commands: Commands,
    mut timer: ResMut<StageCompleteTimer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    pending: Res<crate::xp::LevelUpsPending>,
    boss_reward: Res<crate::boss_reward::BossRewardPending>,
    mut next: ResMut<NextState<crate::AppState>>,
) {
    timer.0 += time.delta_secs();
    if timer.0 < DURATION { return; }
    // Pick order: boss reward → level-up cards → shop. The wipe only
    // fires on the shop hop (last step before Customize); the other
    // hops are interstitial overlays that don't need a screen wipe.
    if boss_reward.0.is_some() {
        next.set(crate::AppState::BossReward);
    } else if pending.0 > 0 {
        next.set(crate::AppState::LevelUp);
    } else {
        spawn_transition(
            &mut commands, &mut meshes, &mut materials,
            crate::AppState::Customize,
        );
    }
}

/// Spawn a radial white wipe targeting `target_state`. Public so other
/// screens can invoke the same effect on their own state changes.
pub fn spawn_transition(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ColorMaterial>,
    target_state: AppState,
) {
    let mesh = meshes.add(Circle::new(0.5));
    // Match the customize/shop backdrop + camera clear so the wipe
    // hand-off into the shop is invisible — the circle fills the
    // screen with the same colour the shop is already painted in,
    // and the collapse phase reveals the shop without a colour cut.
    let mat  = materials.add(Color::srgb(0.13, 0.14, 0.17));
    commands.spawn((
        Mesh2d(mesh),
        MeshMaterial2d(mat),
        Transform {
            // High Z so the wipe sits above every other UPSCALE_LAYER
            // sprite (customize backdrop is at z=1.5, display sprite
            // at z=2.0, customize text up to z≈100). Far higher
            // keeps it on top regardless of which screen is active
            // when the wipe is mid-flight.
            translation: Vec3::new(0.0, 0.0, 500.0),
            scale: Vec3::splat(0.0),
            ..default()
        },
        RenderLayers::layer(UPSCALE_LAYER),
        TransitionEffect {
            elapsed: 0.0,
            target_state,
            state_swapped: false,
        },
    ));
}

/// Drive every live transition wipe: grow → state-swap at apex →
/// hold → shrink → despawn. Runs unconditionally because the entity
/// outlives the state it was spawned in.
pub fn tick_transition(
    time: Res<Time>,
    mut commands: Commands,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut next: ResMut<NextState<AppState>>,
    mut q: Query<(Entity, &mut Transform, &mut TransitionEffect)>,
) {
    if q.is_empty() { return; }
    // Target diameter: cover the window with margin, regardless of
    // aspect. `UpscaleCamera` is `WindowSize`-projected (1 world unit
    // = 1 screen pixel), so feeding the window's max dimension × 2
    // guarantees full coverage.
    let target_diameter = windows
        .single()
        .ok()
        .map(|w| (w.width().max(w.height()) * 2.0).max(2400.0))
        .unwrap_or(2400.0);
    let dt = time.delta_secs();
    for (e, mut tf, mut fx) in &mut q {
        fx.elapsed += dt;
        let scale = if fx.elapsed < TRANSITION_EXPAND {
            // Smooth-step ease so the wipe accelerates into full
            // coverage rather than landing as an abrupt cut.
            let k = fx.elapsed / TRANSITION_EXPAND;
            let k = k * k * (3.0 - 2.0 * k);
            k * target_diameter
        } else if fx.elapsed < TRANSITION_EXPAND + TRANSITION_HOLD {
            target_diameter
        } else {
            let into_collapse = fx.elapsed - TRANSITION_EXPAND - TRANSITION_HOLD;
            let k = (1.0 - (into_collapse / TRANSITION_COLLAPSE)).max(0.0);
            let k = k * k * (3.0 - 2.0 * k);
            k * target_diameter
        };
        tf.scale = Vec3::new(scale, scale, 1.0);
        // Fire the state swap exactly once, at the moment of full
        // coverage. The new screen spawns under the still-fullscreen
        // white circle; the collapse phase wipes it away to reveal
        // the new state.
        if !fx.state_swapped && fx.elapsed >= TRANSITION_EXPAND {
            next.set(fx.target_state);
            fx.state_swapped = true;
        }
        if fx.elapsed >= TRANSITION_TOTAL {
            commands.entity(e).despawn();
        }
    }
}
