//! Transient proc effect events — the moment-in-time "this happened"
//! that gameplay code emits when a Shock chain arcs, a Cascade fires,
//! or a Blast ring expands. Decoupled from networking so the damage
//! pipeline can fire these without depending on the multiplayer
//! module (which is `#[cfg(not(wasm))]`).
//!
//! On native multiplayer, `multiplayer::enemies::send_proc_fx` reads
//! these events via `EventReader` and broadcasts them as
//! `NetMsg::ProcFx` packets so other peers can render the visual.
//! On wasm or single-player, no system reads them; Bevy auto-drops
//! them after the default 2-frame retention window.

use bevy::prelude::*;

/// One transient visual that just spawned locally. Kind matches the
/// `proc_fx_kind` discriminants used by the multiplayer wire format.
/// `from` and `to` are world positions — for one-point effects
/// (Blast ring) `to == from`.
#[derive(Event, Clone, Copy, Debug)]
pub struct ProcFxFired {
    pub kind: u8,
    pub from: Vec2,
    pub to:   Vec2,
}

/// Discriminant constants. Mirrored from
/// `multiplayer::enemies::proc_fx_kind` so call sites here don't
/// have to depend on the multiplayer module — the numbers are the
/// load-bearing wire-format contract, not the module path.
#[allow(dead_code)]
pub mod kind {
    pub const SHOCK_ARC:  u8 = 0;
    pub const CASCADE:    u8 = 1;
    pub const BLAST_RING: u8 = 2;
}

/// Signal: the local player just fired a bullet from a turret. Emit
/// this event from the firing code path so multiplayer can replay
/// the moment on other peers without trusting their RNG / timing.
/// Receivers spawn a *visual-only* replica (damage = 0) so the
/// authoritative damage flow stays via `DamageEnemy` relay.
///
/// Marked non-multiplayer so single-player builds emit + auto-drop
/// the events with no coupling to networking code.
#[derive(Event, Clone, Copy, Debug)]
pub struct BulletFiredEvent {
    pub pos:    Vec2,
    pub dir:    Vec2,
    /// `WeaponType::to_u8` for stable wire format.
    pub weapon: u8,
    /// Effective range. Receivers use it to pick a matching lifetime
    /// so the visual fades at the same time as the firing peer's.
    pub range:  f32,
}
