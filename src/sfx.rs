//! Central SFX pipeline.
//!
//! Three pieces live here:
//! - `Sfx` — the enum of named effects. Every play site refers to a
//!   variant; the variant → file mapping lives in one place (`load_sfx`),
//!   so swapping the underlying audio asset is one-line work.
//! - `SfxLibrary` — `Handle<AudioSource>` table populated at Startup.
//! - `SfxPlayer` — a bundled `SystemParam` callers use to actually fire
//!   sounds. Wraps `Commands` + `SfxLibrary` + `SfxVolume` + a small
//!   `SfxRepeatState` so call sites just write `sfx.play(Sfx::Hit)`.
//!
//! Volume + pitch
//! --------------
//! `SfxVolume(0.0..=1.0)` is the master volume. Persists through
//! `settings.rs`. Values of `0.0` short-circuit playback (no entity
//! spawned at all), so muting is genuinely silent rather than a
//! near-inaudible sample.
//!
//! Rapid repeats of the same `Sfx` get a cumulative pitch bump so a
//! string of identical plays (machine-gun, mortar volley) doesn't read
//! as a flat metronome. Every play also gets a small random jitter so
//! even isolated one-shots feel less clinical.

use bevy::audio::{AudioPlayer, PlaybackSettings, Volume};
use bevy::prelude::*;
use rand::Rng;
use std::collections::HashMap;

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum Sfx {
    /// Generic small-arms shot (turret muzzle flash).
    Shoot,
    /// Heavy cannon-style boom (mortar / cannon class weapons).
    Cannon,
    /// Bullet impact on enemy hull.
    Hit,
    /// Bomber detonation, artillery splash, mine cook-off.
    Explosion,
    /// Friendly hull takes damage.
    PlayerHit,
    /// Scrap drop pickup.
    Coin,
    /// Level-up screen reveal.
    LevelUp,
    /// Generic UI button press.
    UiClick,
    /// Settings toggle / pill click.
    Switch,
    /// Win screen reveal.
    Victory,
    /// Game-over screen reveal.
    GameOver,
    /// Enemy entity drops in.
    EnemySpawn,
}

/// Asset table. Filled once at Startup; read on every play.
#[derive(Resource, Default)]
pub struct SfxLibrary {
    handles: HashMap<Sfx, Handle<AudioSource>>,
}

/// Master SFX volume (`0.0..=1.0` linear). Persisted via `settings.rs`.
/// Default sits below max so a fresh install doesn't blast the user on
/// the first menu click.
#[derive(Resource, Clone, Copy, PartialEq, Debug)]
pub struct SfxVolume(pub f32);

impl Default for SfxVolume {
    fn default() -> Self { Self(0.6) }
}

impl SfxVolume {
    /// Steps the settings UI cycles through. Five rungs is enough
    /// granularity for a click-to-cycle setting; players who want
    /// finer control can edit `settings.txt` directly.
    pub const STEPS: &'static [f32] = &[0.0, 0.25, 0.5, 0.75, 1.0];

    /// Cycle to the next step. Snaps to nearest existing step first so
    /// a hand-edited `settings.txt` value rejoins the rung sequence.
    pub fn cycle(self) -> Self {
        let nearest_idx = Self::STEPS
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                (*a - self.0).abs().partial_cmp(&(*b - self.0).abs()).unwrap()
            })
            .map(|(i, _)| i)
            .unwrap_or(0);
        let next = (nearest_idx + 1) % Self::STEPS.len();
        Self(Self::STEPS[next])
    }

    pub fn label(self) -> String {
        format!("{}%", (self.0 * 100.0).round() as i32)
    }
}

/// Per-`Sfx` last-played + streak counter for the repeat-pitch logic.
/// Resource (not a Component) so cross-frame state survives the
/// one-shot AudioPlayer entities which despawn on completion.
#[derive(Resource, Default)]
pub struct SfxRepeatState {
    last_played_at: HashMap<Sfx, f32>,
    streak: HashMap<Sfx, u32>,
}

/// Two plays of the same `Sfx` within this many seconds are treated as
/// a continued burst; otherwise the streak resets to zero.
const REPEAT_WINDOW: f32 = 0.18;
/// Cumulative pitch bump per consecutive repeat hit.
const PITCH_BUMP_PER_REPEAT: f32 = 0.04;
/// Hard cap on the pitch bump so a long sustained burst doesn't drift
/// into a chipmunk register.
const PITCH_MAX_BUMP: f32 = 0.30;
/// Random per-shot jitter, applied even to non-repeat plays. Small —
/// enough to round off the edges, not enough to read as instability.
const PITCH_JITTER: f32 = 0.04;

pub struct SfxPlugin;

impl Plugin for SfxPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SfxLibrary>()
            .init_resource::<SfxVolume>()
            .init_resource::<SfxRepeatState>()
            .add_systems(Startup, load_sfx)
            // Universal button-click feedback. Any bevy_ui `Button`
            // that just transitioned to `Pressed` plays the Switch
            // sample — same crisp click the player heard on the
            // main menu. Per-button handlers can still fire their
            // own Sfx on top (e.g. settings cycle) but they no
            // longer need to remember the click sound at all.
            .add_systems(Update, play_button_click_sound);
    }
}

/// Fire `Sfx::Switch` whenever any `bevy_ui::Button` transitions to
/// `Interaction::Pressed`. `Changed<Interaction>` keeps this idle
/// most frames — only fires on press enter, not hold. Single-source
/// of truth so adding a new button anywhere gets the click sound
/// for free, with no per-handler plumbing.
fn play_button_click_sound(
    interactions: Query<&Interaction, (Changed<Interaction>, With<Button>)>,
    mut sfx: SfxPlayer,
) {
    for interaction in &interactions {
        if matches!(*interaction, Interaction::Pressed) {
            sfx.play(Sfx::Switch);
        }
    }
}

fn load_sfx(asset_server: Res<AssetServer>, mut lib: ResMut<SfxLibrary>) {
    // Variant → file mapping. To swap a sample, just change the path.
    // To add a variant: add it here + to the `Sfx` enum + (optionally)
    // at one or more play sites.
    let map: &[(Sfx, &str)] = &[
        (Sfx::Shoot,      "sounds/shoot.ogg"),
        (Sfx::Cannon,     "sounds/cannon.ogg"),
        (Sfx::Hit,        "sounds/hit.ogg"),
        (Sfx::Explosion,  "sounds/explosion.ogg"),
        (Sfx::PlayerHit,  "sounds/player_hit.ogg"),
        (Sfx::Coin,       "sounds/coin.ogg"),
        (Sfx::LevelUp,    "sounds/level_up.ogg"),
        (Sfx::UiClick,    "sounds/ui_click.ogg"),
        (Sfx::Switch,     "sounds/switch.ogg"),
        (Sfx::Victory,    "sounds/victory.ogg"),
        (Sfx::GameOver,   "sounds/game_over.ogg"),
        (Sfx::EnemySpawn, "sounds/enemy_spawn.ogg"),
    ];
    for &(sfx, path) in map {
        lib.handles.insert(sfx, asset_server.load(path));
    }
}

/// Bundled SystemParam — drop this into any system that wants to fire
/// SFX and call `.play(Sfx::Foo)`. Bundles `Commands` + all the
/// resources play needs so callers don't have to plumb six params.
#[derive(bevy::ecs::system::SystemParam)]
pub struct SfxPlayer<'w, 's> {
    pub commands: Commands<'w, 's>,
    pub lib: Res<'w, SfxLibrary>,
    pub volume: Res<'w, SfxVolume>,
    pub state: ResMut<'w, SfxRepeatState>,
    pub time: Res<'w, Time>,
}

impl SfxPlayer<'_, '_> {
    /// Single mapping from a player-weapon variant to the SFX
    /// fired on each shot. `None` means the weapon doesn't make a
    /// muzzle sound (autonomous units fire from their own bodies;
    /// support / passive weapons don't fire at all; Flamethrower
    /// runs at per-frame puff cadence and would spam the repeat-
    /// pitch buffer).
    ///
    /// Living here keeps "what does each weapon sound like" a
    /// one-spot edit. Adding a new `Sfx::SniperShot` later is one
    /// match-arm change — every firing site already calls through
    /// this lookup.
    pub fn weapon_fire_sfx(weapon: crate::weapon::WeaponType) -> Option<Sfx> {
        use crate::weapon::WeaponType;
        match weapon {
            // Crisp small-arms beat.
            WeaponType::Standard
            | WeaponType::Sniper
            | WeaponType::MachineGun
            | WeaponType::Shotgun
            | WeaponType::Harpoon => Some(Sfx::Shoot),
            // Heavy boom — anything with real mass behind the shot.
            WeaponType::Cannon
            | WeaponType::Railgun
            | WeaponType::Mortar
            | WeaponType::SpreadRockets
            | WeaponType::PlasmaTorpedo => Some(Sfx::Cannon),
            // Autonomous + passive weapons make no muzzle sound at
            // the deck level. (Their own units may play sounds
            // later — that's a separate hook.)
            WeaponType::HeliPad
            | WeaponType::Booster
            | WeaponType::Blade
            | WeaponType::Cage
            | WeaponType::Flamethrower
            | WeaponType::SpikedPlate
            | WeaponType::Amplifier
            | WeaponType::SharkNet
            | WeaponType::AnchorFlail
            | WeaponType::CrowsNest => None,
        }
    }

    /// Convenience: play the per-weapon fire SFX, silently no-op
    /// if the weapon has no mapped sound. Call sites just write
    /// `sfx.play_fire(weapon)` next to the muzzle-flash spawn so
    /// adding sound to a new weapon is one match-arm edit (in
    /// `weapon_fire_sfx`) instead of touching every firing site.
    pub fn play_fire(&mut self, weapon: crate::weapon::WeaponType) {
        if let Some(s) = Self::weapon_fire_sfx(weapon) {
            self.play(s);
        }
    }

    /// Spawn a one-shot audio entity for `sfx`. Silent when volume is
    /// 0.0 (no entity spawned at all). Pitch picks up the per-shot
    /// jitter and a cumulative bump if the same `Sfx` fired again
    /// inside `REPEAT_WINDOW` seconds.
    pub fn play(&mut self, sfx: Sfx) {
        let v = self.volume.0;
        if v <= 0.0 { return; }
        let Some(handle) = self.lib.handles.get(&sfx).cloned() else { return };

        let now = self.time.elapsed_secs();
        let last = self
            .state
            .last_played_at
            .get(&sfx)
            .copied()
            .unwrap_or(f32::NEG_INFINITY);
        let streak = self.state.streak.get(&sfx).copied().unwrap_or(0);
        let new_streak = if now - last < REPEAT_WINDOW { streak + 1 } else { 0 };
        self.state.last_played_at.insert(sfx, now);
        self.state.streak.insert(sfx, new_streak);

        let mut rng = rand::thread_rng();
        let jitter = rng.gen_range(-PITCH_JITTER..PITCH_JITTER);
        let bump = (new_streak as f32 * PITCH_BUMP_PER_REPEAT).min(PITCH_MAX_BUMP);
        let pitch = (1.0 + bump + jitter).max(0.1);

        self.commands.spawn((
            AudioPlayer::new(handle),
            PlaybackSettings::DESPAWN
                .with_volume(Volume::Linear(v))
                .with_speed(pitch),
        ));
    }
}
