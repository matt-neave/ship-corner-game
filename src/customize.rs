//! Full-screen ship-customization overlay.
//!
//! Player-facing way to configure each turret slot: 8 horizontal cards
//! (one per slot), each with a weapon button, three rune sockets, and
//! live DMG / RoF / DPS readouts. A top summary shows aggregate DPS and
//! slot occupancy.
//!
//! Pause behaviour: while open, `in_combat_view` evaluates false (it
//! also checks `CustomizeOpen`), so enemy spawns / AI / bullets freeze.
//!
//! Layout is built entirely from `ui_kit` primitives so retheming is
//! one-file work. Rune sockets are tinted with the corresponding rune's
//! color when filled, giving the row a strong "what's loaded" read at a
//! glance.
//!
//! Drag-and-drop + an inventory of acquirable weapons/runes are deferred
//! to a follow-up turn — this lays the layout + interaction groundwork.

use bevy::prelude::*;

use crate::palette::{
    hex, FIRE_HEX, FROST_HEX, SHOCK_HEX,
};
use crate::rune::{cycle_next, rune_display, Rune};
use crate::turret::TurretConfig;
use crate::ui_kit::{self, theme};
use crate::weapon::WeaponType;

// ---------- Resource ----------

/// Whether the customize overlay is currently open. Toggled by the
/// debug-panel "CUSTOMIZE" button. Read by `in_combat_view` to pause
/// the world while the player configures slots.
#[derive(Resource, Default)]
pub struct CustomizeOpen {
    pub open: bool,
}

// ---------- Marker components ----------

#[derive(Component)]
pub struct CustomizeRoot;

/// Click target for cycling a slot's *weapon*. Same cycle order as
/// the LHS panel's Equip button so both UIs stay coherent.
#[derive(Component)]
pub struct CustomizeWeaponBtn {
    pub slot: usize,
}

#[derive(Component)]
pub struct CustomizeWeaponLabel {
    pub slot: usize,
}

/// Per-slot stat readout. The `kind` discriminates DMG / RoF / DPS so
/// the same updater can rewrite all three from one query.
#[derive(Component)]
pub struct CustomizeStatLabel {
    pub slot: usize,
    pub kind: StatKind,
}

#[derive(Clone, Copy)]
pub enum StatKind { Damage, FireRate }

/// Click target for cycling one of the three rune sockets on a slot.
#[derive(Component)]
pub struct CustomizeRuneBtn {
    pub slot: usize,
    pub rune_idx: usize,
}

#[derive(Component)]
pub struct CustomizeRuneLabel {
    pub slot: usize,
    pub rune_idx: usize,
}

#[derive(Component)]
pub struct CustomizeCloseBtn;

/// Top-bar summary text: total DPS / slots used.
#[derive(Component)]
pub struct CustomizeSummaryLabel {
    pub kind: SummaryKind,
}

#[derive(Clone, Copy)]
pub enum SummaryKind { SlotsUsed }

// ---------- Setup ----------

/// Spawn the customize overlay once at startup, hidden. `setup_customize_ui`
/// is registered in main.rs's startup chain.
///
/// Tree:
///   Root (dim BG, absolute fullscreen)
///   └── Panel (centered, raised surface)
///       ├── title row (CUSTOMIZE + Close)
///       ├── summary row (TOTAL DPS · SLOTS USED)
///       ├── slot row (8 cards)
///       └── footer hint
pub fn setup_customize_ui(mut commands: Commands) {
    commands
        .spawn((
            // The overlay root is a full-screen dim layer that absorbs
            // clicks (via `Button` + dim BG) so they never fall through
            // to map_click_input below. Container-driven sizing isn't
            // a fit here — we want exact viewport coverage — so we keep
            // an explicit absolute Node rather than going through
            // ui_kit's container helpers.
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
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.78)),
            ZIndex(200),
            Visibility::Hidden,
            CustomizeRoot,
            Button,
        ))
        .with_children(|root| {
            // Centered panel. Padding + raised surface separate the
            // working area from the dim backdrop.
            root.spawn(ui_kit::panel(theme::SURFACE_RAISED, theme::PAD_LG))
                .with_children(|panel| {
                    spawn_title_row(panel);
                    spawn_summary_row(panel);
                    spawn_slot_row(panel);
                    spawn_footer(panel);
                });
        });
}

fn spawn_title_row(panel: &mut bevy::ecs::hierarchy::ChildSpawnerCommands) {
    panel
        .spawn(Node {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::SpaceBetween,
            column_gap: Val::Px(theme::GAP_LG),
            ..default()
        })
        .with_children(|row| {
            row.spawn(ui_kit::label(
                "CUSTOMIZE", theme::FONT_LG, theme::ON_SURFACE,
            ));

            row.spawn((ui_kit::button(theme::SURFACE), CustomizeCloseBtn))
                .with_children(|b| {
                    b.spawn(ui_kit::label(
                        "CLOSE", theme::FONT_MD, theme::ON_SURFACE,
                    ));
                });
        });
}

fn spawn_summary_row(panel: &mut bevy::ecs::hierarchy::ChildSpawnerCommands) {
    // Stat strip — single row with two pills. Reads as "ship summary"
    // above the per-slot detail.
    panel
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(theme::GAP_LG),
                padding: UiRect::axes(
                    Val::Px(theme::PAD_LG), Val::Px(theme::PAD_SM),
                ),
                ..default()
            },
            BackgroundColor(theme::SURFACE),
        ))
        .with_children(|row| {
            spawn_summary_pill(row, "SLOTS USED", SummaryKind::SlotsUsed);
        });
}

fn spawn_summary_pill(
    row: &mut bevy::ecs::hierarchy::ChildSpawnerCommands,
    title: &str,
    kind: SummaryKind,
) {
    row.spawn(ui_kit::row(theme::GAP_SM)).with_children(|r| {
        r.spawn(ui_kit::label(title, theme::FONT_SM, theme::ON_SURFACE_DIM));
        r.spawn((
            ui_kit::label("--", theme::FONT_LG, theme::ACCENT),
            CustomizeSummaryLabel { kind },
        ));
    });
}

fn spawn_slot_row(panel: &mut bevy::ecs::hierarchy::ChildSpawnerCommands) {
    // 8 slot cards in one horizontal strip.
    panel
        .spawn(Node {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Stretch,
            column_gap: Val::Px(theme::GAP_MD),
            ..default()
        })
        .with_children(|row| {
            for slot in 0..8usize {
                spawn_slot_card(row, slot);
            }
        });
}

/// One slot card. Composition tree:
///   Card (panel, SURFACE)
///   ├── header: SLOT N
///   ├── weapon button (cycles)
///   ├── stat lines: DMG / RoF / DPS
///   ├── divider header: RUNES
///   └── 3 rune socket buttons
fn spawn_slot_card(
    row: &mut bevy::ecs::hierarchy::ChildSpawnerCommands,
    slot: usize,
) {
    row.spawn(ui_kit::panel(theme::SURFACE, theme::PAD_MD))
        .with_children(|card| {
            // Slot index — small dim header so the eye doesn't read
            // it as part of the weapon name below.
            card.spawn(ui_kit::label(
                &format!("SLOT {}", slot + 1),
                theme::FONT_XS,
                theme::ON_SURFACE_DIM,
            ));

            // Weapon cycle button — full card width, centered text.
            card.spawn((ui_kit::button(theme::SURFACE_RAISED), CustomizeWeaponBtn { slot }))
                .with_children(|b| {
                    b.spawn((
                        ui_kit::label("---", theme::FONT_MD, theme::ON_SURFACE),
                        CustomizeWeaponLabel { slot },
                    ));
                });

            // Stat readout — 2 mini rows, name dim + value bright.
            spawn_stat_line(card, slot, StatKind::Damage,   "DMG");
            spawn_stat_line(card, slot, StatKind::FireRate, "ROF");

            // Section divider for runes. Same dim header style as
            // SLOT N keeps visual hierarchy uniform.
            card.spawn(ui_kit::label(
                "RUNES", theme::FONT_XS, theme::ON_SURFACE_DIM,
            ));

            // Rune sockets — tinted backgrounds when filled (rune
            // color), so the row reads as a glanceable loadout.
            for rune_idx in 0..3 {
                spawn_rune_socket(card, slot, rune_idx);
            }
        });
}

fn spawn_stat_line(
    card: &mut bevy::ecs::hierarchy::ChildSpawnerCommands,
    slot: usize,
    kind: StatKind,
    name: &str,
) {
    card.spawn(Node {
        flex_direction: FlexDirection::Row,
        align_items: AlignItems::Center,
        justify_content: JustifyContent::SpaceBetween,
        column_gap: Val::Px(theme::GAP_SM),
        ..default()
    })
    .with_children(|r| {
        r.spawn(ui_kit::label(name, theme::FONT_XS, theme::ON_SURFACE_DIM));
        r.spawn((
            ui_kit::label("--", theme::FONT_SM, theme::ON_SURFACE),
            CustomizeStatLabel { slot, kind },
        ));
    });
}

fn spawn_rune_socket(
    card: &mut bevy::ecs::hierarchy::ChildSpawnerCommands,
    slot: usize,
    rune_idx: usize,
) {
    // Start with the empty tint; updater rewrites it per-frame as
    // sockets fill / clear.
    card.spawn((
        ui_kit::button(empty_socket_tint()),
        CustomizeRuneBtn { slot, rune_idx },
    ))
    .with_children(|b| {
        b.spawn((
            ui_kit::label("·", theme::FONT_SM, theme::ON_SURFACE_DIM),
            CustomizeRuneLabel { slot, rune_idx },
        ));
    });
}

fn spawn_footer(panel: &mut bevy::ecs::hierarchy::ChildSpawnerCommands) {
    panel.spawn(ui_kit::label(
        "Click weapon to cycle. Click runes to socket. \
         Detailed stat tweaks live in the LHS debug panel.",
        theme::FONT_SM,
        theme::ON_SURFACE_DIM,
    ));
}

// ---------- Per-frame updater ----------

/// Sync the overlay's text + visibility + tints to live state. Cheap
/// (≤8 slots × 7 labels) and writes only on change.
pub fn update_customize_ui(
    open: Res<CustomizeOpen>,
    cfg: Res<TurretConfig>,
    mut root_q: Query<&mut Visibility, With<CustomizeRoot>>,
    mut weapon_labels: Query<
        (&CustomizeWeaponLabel, &mut Text),
        (Without<CustomizeRuneLabel>, Without<CustomizeStatLabel>, Without<CustomizeSummaryLabel>),
    >,
    mut rune_labels: Query<
        (&CustomizeRuneLabel, &mut Text),
        (Without<CustomizeWeaponLabel>, Without<CustomizeStatLabel>, Without<CustomizeSummaryLabel>),
    >,
    mut stat_labels: Query<
        (&CustomizeStatLabel, &mut Text),
        (Without<CustomizeWeaponLabel>, Without<CustomizeRuneLabel>, Without<CustomizeSummaryLabel>),
    >,
    mut summary_labels: Query<
        (&CustomizeSummaryLabel, &mut Text),
        (Without<CustomizeWeaponLabel>, Without<CustomizeRuneLabel>, Without<CustomizeStatLabel>),
    >,
    mut rune_bgs: Query<(&CustomizeRuneBtn, &mut BackgroundColor)>,
) {
    // Visibility toggle. Skip the rest of the work when closed —
    // text won't be visible anyway and `cfg` may have changed many
    // times since the last open.
    let want_vis = if open.open { Visibility::Inherited } else { Visibility::Hidden };
    for mut v in &mut root_q {
        if *v != want_vis { *v = want_vis; }
    }
    if !open.open { return; }

    // Per-slot weapon name.
    for (lbl, mut text) in &mut weapon_labels {
        let s = &cfg.slots[lbl.slot];
        let want = if s.equipped {
            weapon_short_label(s.weapon).to_string()
        } else {
            "EMPTY".to_string()
        };
        if text.0 != want { text.0 = want; }
    }

    // Per-slot stat pair.
    for (lbl, mut text) in &mut stat_labels {
        let s = &cfg.slots[lbl.slot];
        let want = if !s.equipped {
            "--".to_string()
        } else {
            match lbl.kind {
                StatKind::Damage   => format!("{}", s.damage),
                StatKind::FireRate => format!("{:.1}/s", s.fire_rate),
            }
        };
        if text.0 != want { text.0 = want; }
    }

    // Per-slot rune labels.
    for (lbl, mut text) in &mut rune_labels {
        let s = &cfg.slots[lbl.slot];
        let want = if !s.equipped {
            "·".to_string()
        } else {
            rune_display(s.runes[lbl.rune_idx]).to_string()
        };
        if text.0 != want { text.0 = want; }
    }

    // Rune socket background tints. Filled sockets get a darkened
    // version of the rune's signature color so the loadout row reads
    // as colour stripes; empty sockets stay neutral.
    for (btn, mut bg) in &mut rune_bgs {
        let s = &cfg.slots[btn.slot];
        let want = if !s.equipped {
            empty_socket_tint()
        } else {
            match s.runes[btn.rune_idx] {
                None    => empty_socket_tint(),
                Some(r) => rune_socket_tint(r),
            }
        };
        if bg.0 != want { bg.0 = want; }
    }

    // Summary line.
    let used = cfg.slots.iter().filter(|s| s.equipped).count();
    for (lbl, mut text) in &mut summary_labels {
        let want = match lbl.kind {
            SummaryKind::SlotsUsed => format!("{}/8", used),
        };
        if text.0 != want { text.0 = want; }
    }
}

/// Compact weapon label for the customize cards. Avoids relying on
/// translation keys so the column stays narrow.
fn weapon_short_label(w: WeaponType) -> &'static str {
    match w {
        WeaponType::Standard   => "STD",
        WeaponType::Sniper     => "SNIPER",
        WeaponType::MachineGun => "MG",
        WeaponType::Shotgun    => "SHOT",
        WeaponType::Railgun    => "RAIL",
    }
}

/// Background color for an empty rune socket — slightly darker than
/// the card's SURFACE so the empty cell reads as recessed.
fn empty_socket_tint() -> Color {
    Color::srgb(0.04, 0.05, 0.07)
}

/// Background color for a filled rune socket — the rune's signature
/// color, dimmed so the white/dim foreground text stays readable.
/// Runes without a dedicated palette hex (Detonate, Echo, Cascade,
/// Conduit, Resonate) fall back to thematic blends.
fn rune_socket_tint(r: Rune) -> Color {
    let base = match r {
        Rune::Fire     => hex(FIRE_HEX),
        Rune::Frost    => hex(FROST_HEX),
        Rune::Shock    => hex(SHOCK_HEX),
        // Detonate is a ka-boom / fire-adjacent burst — warm orange-red.
        Rune::Detonate => Color::srgb(1.00, 0.45, 0.20),
        // Echo is a delayed re-strike — purple, the "ghost" hue.
        Rune::Echo     => Color::srgb(0.65, 0.40, 0.95),
        // Cascade chains on kill — green for "spread".
        Rune::Cascade  => Color::srgb(0.45, 0.85, 0.50),
        // Conduit boosts proc strength — magenta for "amplifier".
        Rune::Conduit  => Color::srgb(0.95, 0.40, 0.75),
        // Resonate stacks a damage amplifier — soft gold.
        Rune::Resonate => Color::srgb(0.95, 0.80, 0.45),
    };
    // 35% lerp toward black keeps the chip readable behind text
    // without losing the "what rune is this" colour identity.
    let LinearRgba { red, green, blue, .. } = base.to_linear();
    Color::linear_rgba(red * 0.35, green * 0.35, blue * 0.35, 1.0)
}

// ---------- Click handlers ----------

/// Route weapon-button + rune-socket clicks into `TurretConfig`.
/// Mirrors the LHS panel's Equip + RuneUp behavior so both UIs
/// converge on identical state.
pub fn handle_customize_clicks(
    weapon_q: Query<(&Interaction, &CustomizeWeaponBtn), Changed<Interaction>>,
    rune_q:   Query<(&Interaction, &CustomizeRuneBtn), Changed<Interaction>>,
    close_q:  Query<&Interaction, (Changed<Interaction>, With<CustomizeCloseBtn>)>,
    mut cfg:  ResMut<TurretConfig>,
    mut open: ResMut<CustomizeOpen>,
) {
    for (interaction, btn) in &weapon_q {
        if !matches!(*interaction, Interaction::Pressed) { continue; }
        let s = &mut cfg.slots[btn.slot];
        if !s.equipped {
            // Empty → equip Standard.
            s.equipped = true;
            s.weapon = WeaponType::Standard;
            let (d, r) = s.weapon.defaults();
            s.damage = d;
            s.fire_rate = r;
            s.barrels = 1;
        } else {
            match s.weapon.next() {
                Some(next) => {
                    s.weapon = next;
                    let (d, r) = next.defaults();
                    s.damage = d;
                    s.fire_rate = r;
                    s.barrels = 1;
                }
                None => {
                    // Past the last weapon → unequip + clear runes.
                    s.equipped = false;
                    s.weapon = WeaponType::Standard;
                    let (d, r) = s.weapon.defaults();
                    s.damage = d;
                    s.fire_rate = r;
                    s.barrels = 1;
                    s.runes = [None; 3];
                }
            }
        }
    }

    for (interaction, btn) in &rune_q {
        if !matches!(*interaction, Interaction::Pressed) { continue; }
        let s = &mut cfg.slots[btn.slot];
        if !s.equipped { continue; } // can't socket a rune on an empty slot
        s.runes[btn.rune_idx] = cycle_next(s.runes[btn.rune_idx]);
    }

    for interaction in &close_q {
        if matches!(*interaction, Interaction::Pressed) {
            open.open = false;
        }
    }
}
