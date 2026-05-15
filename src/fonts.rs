//! Shared font handle for tooltips + hint banners.
//!
//! Bevy's default font is an embedded fallback — fine for arbitrary
//! text, but every tooltip in the game wants the project's Pixel
//! Operator face for the chunky-pixel aesthetic. Rather than `load`
//! at each spawn site (which costs an AssetServer lookup per call
//! and would scatter the asset path string across the codebase),
//! we load once at Startup and stash the `Handle<Font>` in a
//! resource. Spawn sites take `Res<PixelFont>` and clone the handle
//! into their `TextFont`.

use bevy::prelude::*;
use bevy::text::FontSmoothing;

/// Shared `Handle<Font>` for Pixel Operator. Body copy: tooltips,
/// banners, settings labels — anywhere a line of running text needs
/// to read clearly at small sizes.
#[derive(Resource)]
pub struct PixelFont(pub Handle<Font>);

/// Shared `Handle<Font>` for Thaleah Fat — a chunkier, more
/// display-weight pixel face. Used for headlines: the main-menu
/// title, the wave-count banner, anywhere we want the text to
/// shout rather than read.
#[derive(Resource)]
pub struct ThaleahFont(pub Handle<Font>);

/// Load both fonts on the first frame. Asset paths are rooted at
/// the `assets/` folder (Bevy's default `AssetServer`); other
/// weights of Pixel Operator (Bold, Mono, etc.) live alongside the
/// regular cut for future use.
pub fn setup_pixel_font(asset_server: Res<AssetServer>, mut commands: Commands) {
    // Regular cut at sizes that are integer multiples of 8 — Pixel
    // Operator's design grid is 8px native, so 16 / 24 / 32 sample
    // 1:1 with the bitmap glyph shapes and stay crisp. At
    // non-multiples (14 / 18) the font resamples and edges blur.
    commands.insert_resource(PixelFont(
        asset_server.load("fonts/pixel_operator/PixelOperator.ttf"),
    ));
    commands.insert_resource(ThaleahFont(
        asset_server.load("fonts/ThaleahFat/ThaleahFat.ttf"),
    ));
}

/// Convenience builder for body text. Keeps `FontSmoothing::None`
/// across every call site so glyph edges sit on whole pixels —
/// matches the rest of the game's chunky aesthetic.
pub fn pixel_text_font(font: &PixelFont, size: f32) -> TextFont {
    TextFont {
        font: font.0.clone(),
        font_size: size,
        font_smoothing: FontSmoothing::None,
        ..default()
    }
}

/// Convenience builder for headline / display text using Thaleah
/// Fat. Same no-smoothing rule so the chunky letterforms stay
/// crisp on the pixel grid.
pub fn thaleah_text_font(font: &ThaleahFont, size: f32) -> TextFont {
    TextFont {
        font: font.0.clone(),
        font_size: size,
        font_smoothing: FontSmoothing::None,
        ..default()
    }
}
