//! Synergies readout for the customize overlay.
//!
//! Six rows, one per `WeaponTag`. Each row shows:
//!   `<TAG>  T<n>  <bonus text>`
//! …with the tag-name in the tag's accent color when the tier is
//! active (tier ≥ 1) and dimmed grey when inactive.
//!
//! The panel lives at the bottom-centre of the customize canvas so the
//! player can scan their active set bonuses at a glance while they
//! drag turrets in/out. Updates every frame `Synergies` changes.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;
use bevy::sprite::Anchor;
use bevy::text::FontSmoothing;

use crate::balance::UPSCALE_LAYER;
use crate::synergy::Synergies;
use crate::weapon::WeaponTag;

use super::setup::{CustomizeText, CustomizeTextSpec};

/// Marker carrying the `WeaponTag` whose row this text node displays.
/// The row's tier + bonus text is rewritten by `update_synergy_panel`
/// each time `Synergies` mutates.
#[derive(Component, Clone, Copy)]
pub struct SynergyRow(pub WeaponTag);

/// Spec-pixel anchor for the panel's top-left. Centre-bottom of the
/// canvas, just below the ship.
const PANEL_LEFT_X: f32 = -55.0;
const PANEL_TOP_Y: f32 = -42.0;
/// Vertical spacing between rows.
const ROW_STEP: f32 = 8.0;
/// Font sizes — header slightly larger, rows compact.
const HEADER_FONT: f32 = 11.0;
const ROW_FONT: f32 = 10.0;

/// Dim grey used for inactive synergy rows. Distinct from the active
/// tag color so the eye sorts active ↔ inactive at a glance.
const INACTIVE_COLOR: Color = Color::srgb(0.40, 0.42, 0.48);

pub fn spawn_synergy_panel(commands: &mut Commands) {
    spawn_left_text(
        commands,
        Vec2::new(PANEL_LEFT_X, PANEL_TOP_Y),
        "SYNERGIES".to_string(),
        Color::srgb(0.85, 0.88, 0.94),
        HEADER_FONT,
        SynergyHeader,
    );
    for (i, &tag) in WeaponTag::all().iter().enumerate() {
        let y = PANEL_TOP_Y - HEADER_FONT * 0.6 - (i as f32 + 1.0) * ROW_STEP;
        spawn_left_text(
            commands,
            Vec2::new(PANEL_LEFT_X, y),
            tag.label().to_string(),
            INACTIVE_COLOR,
            ROW_FONT,
            SynergyRow(tag),
        );
    }
}

#[derive(Component, Clone, Copy)]
struct SynergyHeader;

/// Left-anchored variant of `setup::spawn_text` — the rows want their
/// LEFT edges to line up, not their centres, so we ask for
/// `Anchor::CenterLeft` rather than the default centre anchor used by
/// the rest of the customize text.
fn spawn_left_text<M: Component>(
    commands: &mut Commands,
    spec_pos: Vec2,
    text: String,
    color: Color,
    font_size: f32,
    marker: M,
) {
    commands.spawn((
        Text2d::new(text),
        TextFont {
            font_size,
            font_smoothing: FontSmoothing::None,
            ..default()
        },
        TextColor(color),
        Anchor::CenterLeft,
        Transform::from_xyz(0.0, 0.0, 100.0),
        Visibility::Hidden,
        RenderLayers::layer(UPSCALE_LAYER),
        CustomizeText,
        CustomizeTextSpec(spec_pos),
        marker,
    ));
}

/// Per-frame: rewrite each synergy row's text + color from the live
/// `Synergies`. Idempotent — gated on `Synergies::is_changed()` so
/// idle frames are cheap.
pub fn update_synergy_panel(
    syn: Res<Synergies>,
    mut q: Query<(&SynergyRow, &mut Text2d, &mut TextColor)>,
) {
    if !syn.is_changed() { return; }
    for (row, mut text, mut color) in &mut q {
        let (tier, bonus) = bonus_text_for(row.0, &syn);
        let label = if tier == 0 {
            row.0.label().to_string()
        } else {
            format!("{}  T{}  {}", row.0.label(), tier, bonus)
        };
        if text.0 != label { text.0 = label; }
        let want = if tier > 0 { row.0.color() } else { INACTIVE_COLOR };
        if color.0 != want { color.0 = want; }
    }
}

/// Return `(active_tier, short_bonus_text)` for a tag, given the
/// current `Synergies`. The bonus text is an at-a-glance cue
/// ("+20% dmg" / "+2 HP/kill" / etc.) — full numbers per tier are
/// in `synergy.rs` doc comments.
fn bonus_text_for(tag: WeaponTag, syn: &Synergies) -> (u8, String) {
    match tag {
        WeaponTag::Naval => {
            let t = syn.naval;
            (t, format!("+{}% dmg global", (t as u32) * 10))
        }
        WeaponTag::Future => {
            let t = syn.future;
            (t, format!("+{}% rate Future", (t as u32) * 15))
        }
        WeaponTag::Autonomous => {
            let t = syn.autonomous;
            (t, format!("+{}% rate Auto", (t as u32) * 20))
        }
        WeaponTag::Pirate => {
            let t = syn.pirate;
            (t, format!("+{}% scrap", (t as u32) * 50))
        }
        WeaponTag::Support => {
            let t = syn.support;
            let dmg = match t { 0 | 1 => 0, 2 => 10, 3 => 20, _ => 25 };
            let rate = (t as u32) * 10;
            if dmg == 0 {
                (t, format!("+{}% rate others", rate))
            } else {
                (t, format!("+{}% rate, +{}% dmg others", rate, dmg))
            }
        }
        WeaponTag::Melee => {
            let t = syn.melee;
            (t, format!("+{} HP / Melee kill", t))
        }
    }
}
