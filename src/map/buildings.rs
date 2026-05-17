//! Per-stage combat resolution hooks.
//!
//! What's left after the building / economy system was removed:
//!   - `level_complete_check` — claim the section, bump campaign,
//!     transition into the StageComplete buffer (or Win on a 5★ kill).
//!   - `queue_next_stage_combat` — OnExit(StageComplete) hook that
//!     refills `CombatContext` for the next stage.
//!   - `level_fail_check` — single-player friendly-death → GameOver.
//!   - `clear_anims_on_view_change` — on a view flip, wipe the map
//!     animation timeline + despawn any live pulses/beams so we don't
//!     leak transient FX across view switches.

use bevy::prelude::*;

use crate::enemy::Enemy;

use super::{
    AnimBeam, AnimPulse, CombatContext, MapAnimTimeline, MapState, ViewMode,
};

/// On a view-mode flip, clear the map's transient animation state so
/// nothing leaks across views. Previously also closed the building
/// popup; that whole system is gone now but the anim-clear is still
/// useful as a clean break.
pub fn clear_anims_on_view_change(
    view: Res<ViewMode>,
    mut commands: Commands,
    mut timeline: ResMut<MapAnimTimeline>,
    anims: Query<Entity, Or<(With<AnimPulse>, With<AnimBeam>)>>,
) {
    if !view.is_changed() { return; }
    timeline.steps.clear();
    timeline.elapsed = 0.0;
    for e in &anims { commands.entity(e).despawn(); }
}

// ---------- Section combat resolution ----------

/// End of a level: when the per-section enemy budget is fully drained
/// AND no enemies are alive, claim the section, bump the campaign
/// counter, and transition into the StageComplete buffer. The buffer
/// hands off to Customize (shop) → Map → Playing, where the next
/// section's combat budget is queued as the boat crosses into it
/// (`map_boat_movement`).
///
/// Must only run in `Playing`, not in the `StageComplete` buffer,
/// because the precondition (`budget==0 && no enemies`) remains
/// satisfied throughout the buffer. If it re-fires there it would
/// over-increment `battles_cleared` and race `tick_stage_complete`'s
/// advance to LevelUp / Customize. Registered in main.rs with
/// `.run_if(in_state(AppState::Playing))` so this invariant is
/// enforced at the schedule level, not by defensive in-function
/// checks.
pub fn level_complete_check(
    view: Res<ViewMode>,
    mut state: ResMut<MapState>,
    mut campaign: ResMut<crate::CampaignProgress>,
    combat_ctx: Res<CombatContext>,
    mut boss_reward_pending: ResMut<crate::boss_reward::BossRewardPending>,
    mut next_state: ResMut<NextState<crate::AppState>>,
    mode: Res<crate::modes::GameMode>,
    enemies: Query<Entity, With<Enemy>>,
) {
    if !matches!(*view, ViewMode::Combat) { return; }
    if !matches!(*mode, crate::modes::GameMode::Sandbox) { return; }
    if combat_ctx.enemy_budget > 0 { return; }
    if enemies.iter().count() > 0 { return; }

    let id = state.current as usize;
    // Capture the boss class + star tier BEFORE flipping ownership so
    // the reward / win check has the section's data; collapse into
    // owned values so the immutable borrow on `state.sections` ends
    // before the mutable borrow on `state.owned`.
    let (boss_class, stars) = state
        .sections
        .get(id)
        .map(|s| (s.boss_class, s.stars))
        .unwrap_or((None, 0));
    if id < state.owned.len() && !state.owned[id] {
        state.owned[id] = true;
    }
    campaign.battles_cleared = campaign.battles_cleared.saturating_add(1);

    // Clearing a 5★ section ends the run with a win — skip the shop /
    // map cycle entirely. No boss reward is queued because the run is
    // over.
    if stars >= 5 {
        next_state.set(crate::AppState::Win);
        return;
    }

    if let Some(class) = boss_class {
        boss_reward_pending.0 = Some(class);
    }

    // Don't reset combat-context here — `CombatContext` still holds the
    // just-finished stage's wave_idx / wave_count so the wave readout
    // stays correct during the StageComplete buffer. The next stage's
    // budget is queued via the OnExit(StageComplete) hook
    // (`queue_next_stage_combat`), right before the shop opens.
    next_state.set(crate::AppState::StageComplete);
}

/// OnExit(StageComplete) hook — runs once between the stage-complete
/// overlay disappearing and the shop opening. Refills `CombatContext`
/// for the next stage so closing the shop drops the player into a
/// fresh combat with an enemy budget already queued.
pub fn queue_next_stage_combat(
    campaign: Res<crate::CampaignProgress>,
    mut combat_ctx: ResMut<CombatContext>,
) {
    let stars = (1 + (campaign.battles_cleared / 3)).min(5) as u8;
    combat_ctx.reset_for(stars, campaign.battles_cleared);
}

/// Failure path: when the friendly hull is destroyed during a Sandbox
/// level, transition to the `GameOver` state. The arena is left intact
/// deliberately so the dead ship + frozen enemies show through the
/// transparent end-screen overlay; cleanup runs from the overlay's
/// RESTART / MAIN MENU click handlers instead.
pub fn level_fail_check(
    view: Res<ViewMode>,
    mode: Res<crate::modes::GameMode>,
    // LocalPlayer (not just Friendly) so MP doesn't break — host
    // has two Friendlies (local + remote peer's ship) and `single()`
    // would bail.
    friendly: Query<&crate::components::Health, With<crate::components::LocalPlayer>>,
    net_mode: Res<crate::multiplayer::NetMode>,
    mut next: ResMut<NextState<crate::AppState>>,
) {
    if !matches!(*view, ViewMode::Combat) { return; }
    if !matches!(*mode, crate::modes::GameMode::Sandbox) { return; }

    // Multiplayer death goes through `detect_local_death` →
    // `PeerDied` → host's `TeamDeathTracker` → `host_check_team_wipe`
    // (only fires GameOver when EVERY peer is dead). The dead peer
    // sits in spectate until the next stage's `PeerRevived`. So
    // this single-player-only fail check must NOT fire in MP — else
    // it'd snap the whole team to GameOver the moment the first
    // player dies.
    if !matches!(*net_mode, crate::multiplayer::NetMode::Solo) { return; }

    let Ok(h) = friendly.single() else { return; };
    if h.0 > 0 { return; }

    next.set(crate::AppState::GameOver);
}
