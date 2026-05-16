// `ui_kit` is a launchpad of UI primitives — some land in callers right
// away, others sit ready for the next feature. Unused-warnings here would
// just be noise that incentivizes deleting future-proofing.
#![allow(dead_code)]

//! Components launchpad — minimal, container-driven UI primitives with
//! self-contained theming. The kit owns its theme (colors + sizing); call
//! sites pull from `theme::*` only, so retheming is one-file work.
//!
//! Deliberately *not* imported from `palette::UI_*` — that lets the kit
//! evolve a fresh look without being anchored to the existing in-game
//! panel's choices. Modules migrating to the kit drop their old
//! constants and use `theme` instead.
//!
//! Design notes
//! ------------
//! - **Borders are opt-in.** A minimal aesthetic leans on surface-color
//!   contrast (a popup is "raised" because it's lighter, not because it's
//!   outlined). Use `panel` for the default; `bordered_panel` when you
//!   genuinely need an outline.
//! - **No fixed widths/heights.** Everything sizes to content via flex,
//!   so localized strings get the room they need. If a call site needs a
//!   bound, set it there; don't bake it into the kit.
//! - **Containers, not absolutes.** Use `column`/`row` for layout.
//!   Reserve `position_type: Absolute` for the small set of overlays
//!   (popups anchored to a cursor, e.g.) where flex can't reach.
//!
//! Adding a primitive
//! ------------------
//! 1. Pick a name describing the *role*, not the look (`button`, not
//!    `dark_btn_24`).
//! 2. Return a concrete component tuple (a `Bundle`); callers spawn it.
//! 3. Source colors and sizing from `theme`. No literals at the call site.

use bevy::prelude::*;
use bevy::text::FontSmoothing;

/// Theme — single source of truth for colors and sizing across the kit.
/// Tweak here, every kit-built node updates. If we ever need runtime
/// retheming (e.g., per palette / accessibility), promote the constants
/// to fields on a `Resource` and have helpers read from it; for now
/// constants keep the call site `theme::SURFACE` syntax noise-free.
pub mod theme {
    use super::Color;

    // ---------- Surfaces (layered backgrounds) ----------
    //
    // Three ground levels, all drawn from the project's 16-colour
    // palette. The map view's popup is "raised" above the map by
    // being SURFACE_RAISED on top of the camera clear; option
    // buttons sit at SURFACE so they read as recessed within the
    // popup. A minimal UI conveys hierarchy through these contrasts
    // rather than outlines.
    pub const SURFACE:        Color = Color::srgb(0.102, 0.110, 0.173); // #1a1c2c
    pub const SURFACE_RAISED: Color = Color::srgb(0.200, 0.235, 0.341); // #333c57
    pub const SURFACE_HOVER:  Color = Color::srgb(0.337, 0.424, 0.525); // #566c86

    // ---------- Foreground (text/icons on a surface) ----------
    pub const ON_SURFACE:     Color = Color::srgb(0.957, 0.957, 0.957); // #f4f4f4
    pub const ON_SURFACE_DIM: Color = Color::srgb(0.580, 0.690, 0.761); // #94b0c2
    pub const ACCENT:         Color = Color::srgb(1.000, 0.804, 0.459); // #ffcd75

    // ---------- Stat-delta hues (buff / nerf bullet text) ----------
    /// Bright lime — palette's saturated green; reads cleanly as a
    /// stat buff against any of the surface tones.
    pub const BUFF_FG: Color = Color::srgb(0.655, 0.941, 0.439); // #a7f070
    /// Warm orange — chosen over palette-red (#b13e53) because the
    /// red's luminance is too close to SURFACE_RAISED to read at
    /// small font sizes. Orange is the next palette warm slot down
    /// and stays distinct from ACCENT yellow.
    pub const NERF_FG: Color = Color::srgb(0.937, 0.490, 0.341); // #ef7d57

    // ---------- Borders (when explicit outlines are wanted) ----------
    pub const BORDER_SUBTLE: Color = Color::srgb(0.200, 0.235, 0.341); // #333c57
    /// Near-black outline. Use for chrome that needs to read as a
    /// strong frame against a saturated fill (e.g., the HP bar's fill
    /// is bright red — a soft border would dissolve into it).
    pub const BORDER_DARK:   Color = Color::srgb(0.102, 0.110, 0.173); // #1a1c2c

    // ---------- Sizing ----------
    pub const FONT_XS: f32 = 7.0;
    pub const FONT_SM: f32 = 9.0;
    pub const FONT_MD: f32 = 11.0;
    pub const FONT_LG: f32 = 14.0;

    pub const PAD_SM: f32 = 3.0;
    pub const PAD_MD: f32 = 6.0;
    pub const PAD_LG: f32 = 12.0;

    pub const GAP_SM: f32 = 3.0;
    pub const GAP_MD: f32 = 6.0;
    pub const GAP_LG: f32 = 12.0;

    /// Width of an explicit border when one is used. 1px keeps the
    /// pixel-grid aesthetic; lift to 2 only for emphasis states.
    pub const BORDER_W: f32 = 1.0;

    // ---------- Chunky-pixel vocabulary ----------
    //
    // For bevy_ui screens that want to LOOK like the chunky-pixel
    // mesh chrome (main menu, shop) without giving up bevy_ui's
    // pixel-perfect text. Same colour grammar as `main_menu::*_color`:
    // dark-navy outline at rest, lifted fill + mid-blue outline on
    // hover, slightly lifted fill + restful outline on press.
    pub const CHUNKY_FILL:           Color = Color::srgb(0.20,  0.22,  0.28);
    pub const CHUNKY_FILL_HOVER:     Color = Color::srgb(0.28,  0.31,  0.40);
    pub const CHUNKY_FILL_PRESS:     Color = Color::srgb(0.35,  0.40,  0.52);
    pub const CHUNKY_OUTLINE:        Color = Color::srgb(0.102, 0.110, 0.173);
    pub const CHUNKY_OUTLINE_HOVER:  Color = Color::srgb(0.45,  0.55,  0.70);
    pub const CHUNKY_OUTLINE_PRESS:  Color = Color::srgb(0.18,  0.22,  0.30);

    /// Primary-CTA palette (PLAY / CONFIRM / committed selection).
    /// Sea-green — reads as "go" without competing with `BUFF_FG`'s
    /// brighter lime (which we reserve for stat-buff text). Pairs
    /// cleanly with the navy chrome and stays distinct from gold
    /// `ACCENT` so the two roles don't bleed together.
    pub const CTA_FILL:           Color = Color::srgb(0.30, 0.75, 0.50);
    pub const CTA_FILL_HOVER:     Color = Color::srgb(0.42, 0.85, 0.60);
    pub const CTA_FILL_PRESS:     Color = Color::srgb(0.22, 0.62, 0.40);
    /// Text colour on a `CTA_FILL` button — near-white for readability
    /// over the mid-luminance green (black gets muddy at this hue).
    pub const ON_CTA:             Color = Color::srgb(0.97, 0.98, 0.95);

    /// Border width for chunky-style panels — thick enough to read
    /// as a frame at native window resolution, matching the visual
    /// weight of the menu's outline ring (1 spec px upscaled 4×).
    pub const CHUNKY_BORDER_W: f32 = 3.0;
    /// Corner radius for chunky panels / buttons. Bevy 0.16's
    /// `BorderRadius` rasterises at native pixels, so 4 reads as a
    /// gentle rounding rather than a pill at the menu's design size.
    pub const CHUNKY_RADIUS:   f32 = 4.0;
}

// ---------- Containers ----------

/// Vertical flex container. Children stack top-to-bottom with `gap`
/// between them; container size is auto so children determine the bounds.
pub fn column(gap: f32) -> Node {
    Node {
        flex_direction: FlexDirection::Column,
        align_items: AlignItems::Stretch,
        row_gap: Val::Px(gap),
        ..default()
    }
}

/// Horizontal flex container. Children flow left-to-right, vertically
/// centered, with `gap` between them.
pub fn row(gap: f32) -> Node {
    Node {
        flex_direction: FlexDirection::Row,
        align_items: AlignItems::Center,
        column_gap: Val::Px(gap),
        ..default()
    }
}

// ---------- Panels ----------

/// Minimal panel — padded surface with a background, *no border*. The
/// default for layered UI: rely on the bg contrast against whatever sits
/// behind to convey the boundary.
pub fn panel(bg: Color, padding: f32) -> (Node, BackgroundColor) {
    (
        Node {
            padding: UiRect::all(Val::Px(padding)),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Stretch,
            row_gap: Val::Px(theme::GAP_SM),
            ..default()
        },
        BackgroundColor(bg),
    )
}

/// Bordered panel — same as `panel` plus an explicit 1px outline. Use
/// when bg contrast alone isn't enough (e.g., a popup over a busy world
/// background where the edges would otherwise blur into noise).
pub fn bordered_panel(bg: Color, padding: f32, border: Color)
    -> (Node, BackgroundColor, BorderColor)
{
    (
        Node {
            padding: UiRect::all(Val::Px(padding)),
            border: UiRect::all(Val::Px(theme::BORDER_W)),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Stretch,
            row_gap: Val::Px(theme::GAP_SM),
            ..default()
        },
        BackgroundColor(bg),
        BorderColor(border),
    )
}

// ---------- Buttons ----------

/// Minimal button — padded tappable surface, *no outline*. Conveys
/// affordance through bg contrast against the parent panel.
///
/// Caller adds the marker component identifying *which* button this is,
/// plus a `label` child for the text. For hover/pressed feedback, swap
/// the `BackgroundColor` in a system using `theme::SURFACE_HOVER` etc.
///
/// **Don't** spawn this alongside an extra `Node` in the same bundle —
/// the kit already provides one, and Bevy bundles can't have duplicate
/// components. If you need a non-default Node (different padding,
/// `width: 100%`, FlexStart alignment, etc.), inline `Button +
/// custom Node + BackgroundColor` directly at the call site rather
/// than calling this helper.
pub fn button(bg: Color) -> (Button, Node, BackgroundColor) {
    (
        Button,
        Node {
            padding: UiRect::axes(Val::Px(theme::PAD_MD), Val::Px(theme::PAD_SM)),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            ..default()
        },
        BackgroundColor(bg),
    )
}

// ---------- Text ----------

/// Auto-sized text. Use `theme::FONT_*` for sizing so all labels stay
/// consistent across the UI. The container should not impose a fixed
/// width on this — translated strings get the room they need.
///
/// `FontSmoothing::None` keeps glyph edges on whole-pixel boundaries so
/// kit text reads as part of the same pixel grid as the game world,
/// rather than the soft anti-aliased edges that would otherwise draw the
/// eye to the UI as "different from the art".
pub fn label(text: impl Into<String>, size: f32, color: Color)
    -> (Text, TextFont, TextColor)
{
    (
        Text::new(text),
        TextFont {
            font_size: size,
            font_smoothing: FontSmoothing::None,
            ..default()
        },
        TextColor(color),
    )
}

/// PixelOperator8-faced label. Same shape as `label`, but routes
/// through the shared `PixelFont` resource so the text matches the
/// main menu + shop typography. Use this everywhere in bevy_ui that
/// wants to LOOK like part of the chunky-pixel game UI rather than
/// "default Bevy demo text" (which `label` renders in Fira).
pub fn pixel_label(
    font: &crate::fonts::PixelFont,
    text: impl Into<String>,
    size: f32,
    color: Color,
) -> (Text, TextFont, TextColor) {
    (
        Text::new(text),
        crate::fonts::pixel_text_font(font, size),
        TextColor(color),
    )
}

// ---------- Chunky-styled bevy_ui ----------
//
// Bevy_ui that mimics the look of the mesh-based menu chrome —
// rounded corners, thick dark outline, fill/outline brightening on
// hover, slightly different tint on press. Keeps bevy_ui's pixel-
// perfect text rendering while sharing the visual vocabulary of the
// rest of the game UI.

/// Per-state fill + outline pair for a chunky button. Attach this
/// component to any Bevy `Button` you want the generic tint system
/// to manage — same component instance covers idle, hover, and press
/// so all three colours stay in lock-step.
///
/// For "selected" buttons that shouldn't react to hover (e.g. the
/// currently-picked hull tile in hull-select), set `idle`, `hover`,
/// and `press` to the same pair so the button reads as locked.
#[derive(Component, Clone, Copy)]
pub struct ChunkyButtonStyle {
    pub idle_fill:     Color,
    pub idle_outline:  Color,
    pub hover_fill:    Color,
    pub hover_outline: Color,
    pub press_fill:    Color,
    pub press_outline: Color,
}

impl ChunkyButtonStyle {
    /// Default chunky palette — dark slab + dark-navy outline at rest,
    /// brighter fill + mid-blue outline on hover. Use for most
    /// neutral buttons (BACK, settings rows, list items).
    pub fn neutral() -> Self {
        Self {
            idle_fill:     theme::CHUNKY_FILL,
            idle_outline:  theme::CHUNKY_OUTLINE,
            hover_fill:    theme::CHUNKY_FILL_HOVER,
            hover_outline: theme::CHUNKY_OUTLINE_HOVER,
            press_fill:    theme::CHUNKY_FILL_PRESS,
            press_outline: theme::CHUNKY_OUTLINE_PRESS,
        }
    }
    /// Accent-coloured palette for headline/value text-backed
    /// buttons. Gold at rest. Reserve for cases where the gold
    /// vocabulary is semantically the right colour (a "rare reward"
    /// confirm, say); for plain CTAs prefer [`Self::cta`].
    pub fn accent() -> Self {
        Self {
            idle_fill:     theme::ACCENT,
            idle_outline:  theme::CHUNKY_OUTLINE,
            hover_fill:    Color::srgb(1.00, 0.90, 0.55),
            hover_outline: theme::CHUNKY_OUTLINE,
            press_fill:    Color::srgb(0.85, 0.70, 0.25),
            press_outline: theme::CHUNKY_OUTLINE,
        }
    }
    /// Primary action palette (PLAY, CONFIRM, committed-selection
    /// tile). Sea-green at rest, lifts toward fresh lime on hover,
    /// dims on press. Use this — not [`Self::accent`] — when the
    /// button is the screen's primary "go" affordance.
    pub fn cta() -> Self {
        Self {
            idle_fill:     theme::CTA_FILL,
            idle_outline:  theme::CHUNKY_OUTLINE,
            hover_fill:    theme::CTA_FILL_HOVER,
            hover_outline: theme::CHUNKY_OUTLINE,
            press_fill:    theme::CTA_FILL_PRESS,
            press_outline: theme::CHUNKY_OUTLINE,
        }
    }
    /// Locked-in / selected style: identical across idle/hover/press
    /// so the button looks pinned. Use for the currently-active tile
    /// in a picker grid where hover should NOT preview a colour
    /// change.
    pub fn locked(fill: Color, outline: Color) -> Self {
        Self {
            idle_fill:     fill,
            idle_outline:  outline,
            hover_fill:    fill,
            hover_outline: outline,
            press_fill:    fill,
            press_outline: outline,
        }
    }
}

/// Node + BackgroundColor + BorderColor + BorderRadius bundle for a
/// chunky-styled panel. Use for any container that wants the dark
/// rounded slab look (left-column ship preview card, detail panel,
/// pill list backdrop, etc.). `extra_padding` is added to a default
/// `PAD_MD` baseline so callers pass `0.0` for the common case.
pub fn chunky_panel_bundle(
    fill:          Color,
    outline:       Color,
    extra_padding: f32,
) -> (Node, BackgroundColor, BorderColor, BorderRadius) {
    (
        Node {
            padding: UiRect::all(Val::Px(theme::PAD_MD + extra_padding)),
            border: UiRect::all(Val::Px(theme::CHUNKY_BORDER_W)),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Stretch,
            row_gap: Val::Px(theme::GAP_SM),
            ..default()
        },
        BackgroundColor(fill),
        BorderColor(outline),
        BorderRadius::all(Val::Px(theme::CHUNKY_RADIUS)),
    )
}

/// Bundle for a chunky-styled tappable button. Returns Button + the
/// same Node/BackgroundColor/BorderColor/BorderRadius shape as
/// `chunky_panel_bundle`. Caller attaches a `ChunkyButtonStyle` so
/// the tint system swaps colours on hover / press.
pub fn chunky_button_bundle(
    style: ChunkyButtonStyle,
) -> (Button, Node, BackgroundColor, BorderColor, BorderRadius, ChunkyButtonStyle) {
    (
        Button,
        Node {
            padding: UiRect::axes(Val::Px(theme::PAD_LG), Val::Px(theme::PAD_MD)),
            border: UiRect::all(Val::Px(theme::CHUNKY_BORDER_W)),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            ..default()
        },
        BackgroundColor(style.idle_fill),
        BorderColor(style.idle_outline),
        BorderRadius::all(Val::Px(theme::CHUNKY_RADIUS)),
        style,
    )
}

/// Per-frame: read each chunky button's `Interaction` and swap its
/// `BackgroundColor` + `BorderColor` to match. Only writes when the
/// colour actually changes (cheap to run unconditionally). One
/// generic system covers every chunky button in the codebase — no
/// per-screen plumbing.
pub fn update_chunky_button_visuals(
    mut q: Query<
        (&Interaction, &ChunkyButtonStyle, &mut BackgroundColor, &mut BorderColor),
        Changed<Interaction>,
    >,
) {
    for (interaction, style, mut bg, mut border) in &mut q {
        let (want_fill, want_outline) = match interaction {
            Interaction::Pressed => (style.press_fill,  style.press_outline),
            Interaction::Hovered => (style.hover_fill,  style.hover_outline),
            Interaction::None    => (style.idle_fill,   style.idle_outline),
        };
        if bg.0 != want_fill   { bg.0 = want_fill; }
        if border.0 != want_outline { border.0 = want_outline; }
    }
}
