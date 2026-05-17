//! Hitstop / time-ramp resource for juice.
//!
//! Bevy's `Time<Virtual>` already supports a global relative-speed
//! multiplier — every system that reads `Res<Time>` from the
//! Update schedule (which uses `Time<()>` aliased to virtual time)
//! transparently sees the slowed delta. We layer a small stack of
//! "speed requests" on top so different juice triggers compose
//! without fighting each other:
//!
//! * Player takes damage → ~60 ms freeze (relative_speed 0.0)
//! * Level-up entered → 200 ms slow-mo at 0.3× speed
//! * (Future) crit kill → 50 ms freeze
//!
//! The resource always honours the SLOWEST active request; when
//! all requests expire, virtual speed resets to 1.0. Real time
//! (`Time<Real>`) keeps ticking so animations driven by it (the
//! existing camera shake, etc.) don't freeze visibly.

use bevy::prelude::*;
use bevy::time::{Real, Time, Virtual};

pub struct HitStopPlugin;

impl Plugin for HitStopPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<HitStopController>()
            // Tick first so the rest of Update sees today's
            // effective speed. `Time<Virtual>` reads from this
            // resource's combined multiplier.
            .add_systems(First, tick_hit_stop_controller);
    }
}

/// Single active time-warp request. The controller picks the
/// MIN `speed_mult` across all live requests each frame.
#[derive(Clone, Copy)]
struct ActiveRamp {
    /// Remaining real-time seconds.
    remaining: f32,
    /// Multiplier this request wants on `Time<Virtual>`. 0.0 =
    /// freeze, 0.3 = slow-mo, 1.0 = no-op.
    speed_mult: f32,
}

/// Stack of live time-warp requests. Drain happens in
/// `tick_hit_stop_controller`; pushers call [`HitStopController::push`]
/// from any system that wants to slow time.
#[derive(Resource, Default)]
pub struct HitStopController {
    ramps: Vec<ActiveRamp>,
}

impl HitStopController {
    /// Push a new ramp. Multiple pushes layer — the controller
    /// honours the smallest active `speed_mult` so a freeze on
    /// top of a slow-mo still freezes for its duration.
    pub fn push(&mut self, duration_secs: f32, speed_mult: f32) {
        if duration_secs <= 0.0 { return; }
        self.ramps.push(ActiveRamp {
            remaining: duration_secs.max(0.0),
            speed_mult: speed_mult.clamp(0.0, 1.0),
        });
    }

    /// Convenience: brief freeze for ~60 ms. Use on damage / kill
    /// triggers that want the classic hitstop feel. Currently no
    /// callers (previous damage triggers were removed for reading
    /// as FPS hitches) — kept so a re-enable is a one-line change.
    #[allow(dead_code)]
    pub fn freeze(&mut self, duration_secs: f32) {
        self.push(duration_secs, 0.0);
    }

    /// Convenience: a longer slow-mo (default 0.3× for `dur` s).
    #[allow(dead_code)]
    pub fn slow(&mut self, dur_secs: f32, speed_mult: f32) {
        self.push(dur_secs, speed_mult);
    }
}

fn tick_hit_stop_controller(
    real: Res<Time<Real>>,
    mut ctrl: ResMut<HitStopController>,
    mut virt: ResMut<Time<Virtual>>,
) {
    let dt = real.delta_secs();
    // Decay every active ramp by REAL time so the freeze ends on
    // a real-world schedule (otherwise a 0× ramp would never expire).
    for r in ctrl.ramps.iter_mut() {
        r.remaining -= dt;
    }
    ctrl.ramps.retain(|r| r.remaining > 0.0);
    let mult = ctrl
        .ramps
        .iter()
        .map(|r| r.speed_mult)
        .fold(f32::INFINITY, f32::min);
    let want = if mult.is_finite() { mult } else { 1.0 };
    // `relative_speed` doesn't have a `set_if_neq` helper; cheap
    // float-eq guard avoids change-detection spam.
    if (virt.relative_speed() - want).abs() > 0.0001 {
        virt.set_relative_speed(want);
    }
}
