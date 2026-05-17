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

use crate::AppState;

mod render;
mod setup;
pub mod drag;
mod shop_lock;
mod shop_mods;
mod stats_panel;
mod tooltip;
mod update;

/// Owns the customize / shop overlay: its three resources, the
/// one-time startup spawn (render target + ship + shop UI), the
/// enter/exit cleanup hooks, and the dense per-frame Update block
/// that drives every customize sub-system. Most of these self-gate
/// on `CustomizeOpen` so they're cheap to leave always-on.
pub struct CustomizePlugin;

impl Plugin for CustomizePlugin {
    fn build(&self, app: &mut App) {
        app
            .insert_resource(CustomizeOpen::default())
            .insert_resource(DragState::default())
            .insert_resource(TooltipLayout::default())
            .insert_resource(drag::ActiveLegendaries::default())
            .add_systems(
                Startup,
                (init_customize_shop, setup_customize_render, setup_customize_ui).chain(),
            )
            .add_systems(
                OnEnter(AppState::Customize),
                (init_customize_shop, crate::enemy::clear_spawn_indicators),
            )
            // Wipe legendary build-warpers when returning to the
            // main menu so a fresh run starts clean.
            .add_systems(
                OnEnter(AppState::MainMenu),
                |mut active: ResMut<drag::ActiveLegendaries>| {
                    *active = drag::ActiveLegendaries::default();
                },
            )
            .add_systems(OnExit(AppState::Customize), crate::ui::reset_damage_stats)
            // Cursor tracker is registered FIRST and on its own so
            // every downstream click / drag system can `.after()` it.
            // Without this explicit ordering, Bevy is free to run
            // click handlers (close, reroll, mod purchase, drag
            // start) BEFORE the cursor is refreshed for the frame
            // — they'd read the previous frame's `spec_cursor` and
            // fire on stale positions, producing "random" closes
            // and ghost purchases.
            .add_systems(Update, track_customize_cursor)
            // Split into two `.add_systems` blocks because a single
            // 15-element tuple with `.after(track_customize_cursor)`
            // overflows Bevy's `IntoSystemConfigs` trait-impl limit.
            // Both halves still order behind the cursor tracker.
            .add_systems(
                Update,
                (
                    // Every customize system self-gates on `CustomizeOpen`.
                    toggle_customize_render,
                    resize_customize_display,
                    sync_customize_text,
                    update_customize_ui,
                    update_customize_ship,
                    update_customize_shop,
                    update_customize_tooltip,
                    update_synergy_banner,
                ).after(track_customize_cursor),
            )
            .add_systems(
                Update,
                (
                    sync_stats_panel,
                    // After sync_customize_text so the debug-only Hidden
                    // write isn't overwritten by the generic Inherited.
                    sync_stat_debug_visibility.after(sync_customize_text),
                    handle_stat_debug_buttons,
                    update_shop_mod_cards,
                    handle_shop_mod_click,
                    handle_close_click,
                    handle_reroll_button,
                    update::handle_right_click_lock,
                    shop_lock::sync_lock_badges,
                ).after(track_customize_cursor),
            )
            // Drag chain in its own add_systems — chained tuples
            // nested inside the block above hit a Bevy trait-impl
            // limit. `.after(sync_customize_text)` makes
            // `update_sell_label` the final writer for the preview's
            // visibility on the strip.
            .add_systems(
                Update,
                (start_drag, promote_pending_drag, update_drag_ghost, complete_drag, update_sell_label)
                    .chain()
                    .after(sync_customize_text)
                    .after(track_customize_cursor),
            )
            // Unconditional synergy chain — discovery must fire while
            // customize is open so equipping a 2nd tagged turret pops
            // the banner immediately.
            .add_systems(
                Update,
                (crate::synergy::compute_synergies, crate::synergy::discover_synergies).chain(),
            )
            // Purchase confirmation particles. Unconditional so any
            // in-flight burst finishes its short life even if the
            // player closes the panel mid-fade.
            .add_systems(Update, tick_purchase_particles);
    }
}

pub use render::{
    resize_customize_display, setup_customize_render, toggle_customize_render,
};
pub use setup::{setup_customize_ui, sync_customize_text};
pub use drag::{
    complete_drag, init_customize_shop, promote_pending_drag, start_drag,
    tick_purchase_particles, track_customize_cursor, update_drag_ghost, DragState,
};
pub use shop_mods::{handle_shop_mod_click, update_shop_mod_cards};
pub use stats_panel::{handle_stat_debug_buttons, sync_stat_debug_visibility, sync_stats_panel};
pub use tooltip::{update_customize_tooltip, update_synergy_banner, TooltipLayout};
pub use update::{
    handle_close_click, handle_reroll_button, update_customize_ship, update_customize_shop,
    update_customize_ui, update_sell_label,
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
