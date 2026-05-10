//! Full-screen ship-customization overlay.
//!
//! Rendered as primitive entities (sprites + text2d + meshes) on
//! `CUSTOMIZE_LAYER`, captured by `CustomizeCamera` into a low-res image,
//! then upscaled with nearest-neighbor sampling to the window. That low-
//! res rasterization is what gives the overlay the same chunky-pixel
//! look as the in-game ship — every primitive's edge stair-steps the
//! same way the ship's capsule hull does in combat.
//!
//! Layout
//! ------
//! - **Top-left**: scrap counter (text only).
//! - **Left**: shop with 3 random turret offerings + 2 random runes.
//!   Each slot is single-use; the REROLL button (cost in
//!   `drag::SHOP_REROLL_COST`) restocks every slot.
//! - **Centre**: the in-game `Capsule2d` hull scaled up, with all 8
//!   turret tiles pinned to their accurate `TURRET_POSITIONS`. Each
//!   turret carries 3 rune sockets on its natural side.
//! - **Top-right**: CLOSE button (rounded square + text).
//!
//! Drag-and-drop
//! -------------
//! Custom mouse picking on the customize layer (no `bevy_ui`
//! involvement). See `drag.rs` for the full state machine + merge rules.
//!
//! Module split
//! ------------
//! - `render`  — render target + camera + window↔spec coord mapping
//! - `setup`   — primitive spawning + container helpers
//! - `drag`    — cursor tracking + drag state + start/ghost/complete
//! - `update`  — per-frame visual sync from `TurretConfig` / `CustomizeShop`
//! - `tooltip` — hover description card

use bevy::prelude::*;

mod render;
mod setup;
mod drag;
mod stats_panel;
mod tooltip;
mod update;

pub use render::{
    resize_customize_display, setup_customize_render, toggle_customize_render,
};
pub use setup::{setup_customize_ui, sync_customize_text};
pub use drag::{
    complete_drag, init_customize_shop, start_drag, track_customize_cursor,
    update_drag_ghost, DragState,
};
pub use stats_panel::{handle_stat_debug_buttons, sync_stats_panel};
pub use tooltip::update_customize_tooltip;
pub use update::{
    handle_close_click, handle_reroll_button, update_customize_ship, update_customize_shop,
    update_customize_ui,
};

// ---------- Resources ----------

#[derive(Resource, Default)]
pub struct CustomizeOpen {
    pub open: bool,
}

// ---------- Marker components ----------

#[derive(Component)]
pub struct CustomizeRoot;

#[derive(Component, Clone, Copy)]
pub struct CustomizeCloseBtn;
