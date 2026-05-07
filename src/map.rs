//! Map view — a zoomed-out second view where the player picks where to
//! sail next. The same square play area is reused; we just swap what the
//! play camera renders by flipping its `RenderLayers` between
//! `PLAY_LAYER` (combat) and `MAP_LAYER` (map). One camera, two views.
//!
//! Layout: 10 hand-authored irregular sections — varied sizes and shapes
//! (small corner wedges, large L-shaped flanks, central pentagons),
//! deliberately *not* arranged in straight rows so no continuous horizontal
//! or vertical line crosses the map. Adjacent sections share their boundary
//! corners exactly + use a deterministic `wobble_for_edge` curve so the
//! dividers look hand-drawn but match across regions (no slivers or gaps).
//! Outer-edge segments stay straight so the map fills the square cleanly.
//!
//! Movement reuses the in-game pattern (`approach_angle` toward a desired
//! heading, fixed forward speed) — but the destination is set by clicking
//! an adjacent section instead of following the cursor continuously.
//!
//! Currently, entering an unowned section just flips view to combat;
//! "winning" or "capturing" isn't wired yet (per design discussion).

use bevy::image::ImageSampler;
use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::render::render_asset::RenderAssetUsages;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::render::view::RenderLayers;
use bevy::window::PrimaryWindow;

use crate::balance::{
    FRIENDLY_SPEED, FRIENDLY_TURN_RATE, HULL_LEN, HULL_WIDTH, PLAY_INTERNAL, PLAY_WORLD,
    TURRET_MOUNTS, TURRET_POSITIONS,
};
use crate::components::Heading;
use crate::i18n::tr;
use crate::modes::{effective_ui_width, play_area_screen_rect, WindowMode};
use crate::palette::{MapCamera, Palette, PaletteMaterials, PlayCamera};
use crate::ship::approach_angle;
use crate::ui_kit::{self, theme};

/// Render layer for everything visible only in map view. `apply_view_mode`
/// flips the play camera between `PLAY_LAYER` and this.
pub const MAP_LAYER: usize = 3;

/// Z-band used by map entities so they layer cleanly:
///   0.5 = section fills,    0.7  = boundary segments,
///   0.85 = slot box,         0.90 = star marks,
///   1.0  = phase animations (pulses/beams),
///   1.5  = boat token.
///
/// Slot labels are *not* in this band — they're Bevy UI nodes drawn
/// in screen space (native res) so AA text isn't blurred by the
/// nearest-neighbor upscale.
const Z_FILL:      f32 = 0.5;
const Z_OUTLINE:   f32 = 0.7;
const Z_SLOT_BOX:  f32 = 0.85;
const Z_SLOT_STAR: f32 = 0.90;
const Z_ANIM:      f32 = 1.0;
const Z_BOAT:      f32 = 1.5;

/// Visual scale of the map boat token relative to its in-combat size.
/// Same hull mesh, half the size — implies a zoomed-out world view.
const MAP_BOAT_SCALE: f32 = 0.5;

/// Slot box geometry. World-space units; the play area is `PLAY_WORLD`
/// (=200) wide so a 10-unit box reads as a small but clickable tile.
const SLOT_SIZE: f32      = 10.0;
const SLOT_HALF: f32      = SLOT_SIZE / 2.0;
/// Star-mark geometry — small filled squares stacked horizontally above
/// the slot, leaving room for up to 5. Sizes are integer world units
/// so they rasterize to exact internal pixels (no AA, no MSAA in this
/// pipeline) at any section.center.x. With `STAR_SIZE = 2` and
/// `STAR_GAP = 2`, stars render as 2-px filled squares with 2-px
/// gaps — clearly distinguishable as separate pips, not a merged bar.
const STAR_SIZE: f32      = 2.0;
const STAR_GAP:  f32      = 2.0;
/// Distance from slot center up to the row of stars.
const STAR_Y_OFFSET: f32  = 9.0;

// ---------- Resources ----------

#[derive(Resource, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Map,
    Combat,
}
impl Default for ViewMode {
    fn default() -> Self { ViewMode::Map }
}

/// Snapshot of the section that triggered the *current* combat. Written
/// by `map_boat_movement` when the boat crosses into an unowned zone;
/// `spawn_enemies` reads it to scale enemy density by star rating
/// (1★ → light skirmish, 5★ → swarm). Default `stars = 1` covers any
/// combat reached without going through map flow (e.g., a fresh game
/// where the player jumps straight into Wave mode).
#[derive(Resource)]
pub struct CombatContext {
    pub stars: u8,
}

impl Default for CombatContext {
    fn default() -> Self { Self { stars: 1 } }
}

impl CombatContext {
    /// On-screen enemy cap for sandbox-style drip spawning. Linear in
    /// stars at 6 per tier so 5★ = 30 (the previous fixed cap, now the
    /// upper-bound) and 1★ = 6 (a noticeably calmer skirmish).
    pub fn enemy_cap(&self) -> usize {
        (6 * self.stars.max(1) as usize).min(30)
    }
}

/// Buildings that can be placed in a section's upgrade slot. Each is gated
/// by the section's star rating (`MapBuilding::options_for_stars`).
///
/// Adding a new building is a four-place edit — variant here, plus an arm
/// in each of `label` / `description` / `options_for_stars`, plus the two
/// translation rows. Effects (combat / adjacency / begin-phase) plug in
/// where they belong (e.g. `map_begin_phase`); they don't have to clutter
/// this enum.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MapBuilding {
    /// Placeholder for the weapon-customization system (label-only for now).
    Weaponry,
    /// Demo building exercising the adjacency graph: at the start of every
    /// map phase, prints the buildings in its neighboring sections. Keeps
    /// the modularity story honest — adjacency-effect code uses
    /// `MapState::neighbor_buildings` instead of reaching into the section
    /// vec directly.
    Dockyard,
}

impl MapBuilding {
    /// Localized label rendered in the slot when the building is placed,
    /// and on the popup option button.
    pub fn label(self) -> &'static str {
        match self {
            MapBuilding::Weaponry => tr("map_building_weaponry"),
            MapBuilding::Dockyard => tr("map_building_dockyard"),
        }
    }

    /// One-sentence localized description shown in the popup's footer
    /// while the player hovers the option. Keep it short — the footer is
    /// soft-bounded; long descriptions wrap.
    pub fn description(self) -> &'static str {
        match self {
            MapBuilding::Weaponry => tr("map_building_weaponry_desc"),
            MapBuilding::Dockyard => tr("map_building_dockyard_desc"),
        }
    }

    /// Buildings the player may place in a slot at this star tier. Single
    /// source of truth so the popup, slot click handler, and any future
    /// "what unlocks here" UI all agree.
    pub fn options_for_stars(stars: u8) -> Vec<MapBuilding> {
        let mut opts = Vec::new();
        if stars >= 1 { opts.push(MapBuilding::Weaponry); }
        if stars >= 1 { opts.push(MapBuilding::Dockyard); }
        opts
    }
}

pub struct MapSection {
    pub id: u32,
    /// Original CCW corner points (no wobble). Used to enumerate distinct
    /// boundary edges for ribbon-divider rendering and adjacency-deriving.
    pub corners: Vec<Vec2>,
    /// CCW polygon vertices, including curved-boundary intermediate points.
    /// This is the mesh-fill polygon (corners + per-edge wobble baked in).
    pub polygon: Vec<Vec2>,
    /// Center point — both visual (fan-tri pivot) and the boat's start
    /// position when the section is first owned.
    pub center: Vec2,
    /// Adjacency list — section ids this one shares a boundary with.
    /// Together with the rest of `MapState.sections`, this *is* the map
    /// graph (vertices = sections, edges = shared boundaries). Used by
    /// `compute_stars` (BFS distance from S0) and by adjacency-effect
    /// queries via `MapState::neighbors` / `MapState::neighbor_buildings`,
    /// which are the public surface for "this building affects adjacent
    /// tiles" logic.
    pub adjacencies: Vec<u32>,
    /// 1..=5 — combat difficulty / building-unlock tier. Computed in
    /// `MapState::new` as `BFS_distance_from_S0 + 1`, capped at 5.
    pub stars: u8,
    /// Per-section upgrade slots. `None` = empty, `Some(b)` = built. Slot
    /// count is always 1 today; the `Vec` is here so we can scale it with
    /// stars (or any other rule) without restructuring callers.
    pub slots: Vec<Option<MapBuilding>>,
}

#[derive(Resource)]
pub struct MapState {
    pub sections: Vec<MapSection>,
    /// Section the boat is *currently inside*. Updated each frame by
    /// `map_boat_movement` based on point-in-polygon containment.
    pub current: u32,
    /// Indexed by section id.
    pub owned: Vec<bool>,
    /// World-space click target the boat is sailing toward, if any. Cleared
    /// on arrival or when the boat enters an unowned (red) zone.
    pub boat_target: Option<Vec2>,
}

impl MapState {
    pub fn new() -> Self {
        let mut sections = build_default_map();
        // Star rating = BFS hops from the starting section + 1, capped at 5.
        // Why BFS-from-start: stars gate which buildings are available and
        // (later) combat difficulty, so the further from the start, the
        // tougher / richer the area — a natural difficulty gradient without
        // needing to hand-author per-section ratings.
        let stars = compute_stars(&sections, 0);
        for (i, s) in sections.iter_mut().enumerate() {
            s.stars = stars[i];
            // One slot everywhere for now; star gating only affects which
            // *buildings* can be placed (see `MapBuilding::options_for_stars`).
            s.slots = vec![None; 1];
        }
        let mut owned: Vec<bool> = vec![false; sections.len()];
        owned[0] = true; // start owning the top-left section
        Self { sections, current: 0, owned, boat_target: None }
    }

    pub fn section(&self, id: u32) -> &MapSection {
        &self.sections[id as usize]
    }

    // ----- Graph queries -----
    //
    // Adjacency-list storage already lives on `MapSection.adjacencies`;
    // these helpers exist so adjacency-effect code (e.g. "Foundry gives
    // +1 damage to neighbors with a Weaponry") doesn't reach into the
    // section vec directly. Kept here as scaffolding for the first
    // building that uses them — `dead_code` until then is expected, not
    // a smell.

    /// Section ids that share a boundary with `section_id`.
    #[allow(dead_code)]
    pub fn neighbors(&self, section_id: u32) -> &[u32] {
        &self.sections[section_id as usize].adjacencies
    }

    /// Iterator over `(neighbor_id, building)` for every built building
    /// in any neighbor of `section_id`. Useful for "this building does
    /// X per adjacent Y" effects without hand-rolling the same nested
    /// flat_map at every call site.
    #[allow(dead_code)]
    pub fn neighbor_buildings(
        &self,
        section_id: u32,
    ) -> impl Iterator<Item = (u32, MapBuilding)> + '_ {
        self.neighbors(section_id).iter().flat_map(move |&nid| {
            self.sections[nid as usize].slots.iter()
                .filter_map(move |slot| slot.map(|b| (nid, b)))
        })
    }
}

/// BFS distance from the starting section, then `+1` and clamped to 5,
/// produces a 1..=5 star rating per section. Standard breadth-first walk
/// over `adjacencies`; a section unreachable from `start` would return 5
/// (saturating the hop count) but that shouldn't happen with our connected
/// hand-authored map.
fn compute_stars(sections: &[MapSection], start: usize) -> Vec<u8> {
    let n = sections.len();
    let mut dist = vec![u8::MAX; n];
    if start >= n { return vec![1; n]; }
    dist[start] = 0;
    let mut q: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    q.push_back(start);
    while let Some(i) = q.pop_front() {
        let d = dist[i];
        for &nbr in &sections[i].adjacencies {
            let nbr = nbr as usize;
            if dist[nbr] == u8::MAX {
                dist[nbr] = d.saturating_add(1);
                q.push_back(nbr);
            }
        }
    }
    dist.iter().map(|&d| d.saturating_add(1).min(5)).collect()
}

// ---------- Marker components ----------

#[derive(Component)]
pub struct MapBoat;

/// Marker on the single sprite that displays the pre-rasterized section
/// fill image. We render the entire map fill as one sprite (one quad,
/// one draw call) instead of per-section meshes — that way alpha
/// rendering can't produce hairline seams between fan-triangle edges,
/// which is what was causing visible "rays" through the tints.
#[derive(Component)]
pub struct MapFillSprite;

#[derive(Component)]
pub struct MapSectionBoundary;

/// Grey square at a section's center where a building can be placed.
/// `section_id`/`slot_index` let a future hit-test path look up which slot
/// was hit without scanning all sections geometrically; today the click
/// handler uses point-in-box math directly, so the fields are dormant
/// scaffolding (kept so adding entity-based picking later doesn't churn
/// the spawn site).
#[derive(Component)]
#[allow(dead_code)]
pub struct MapSlotBox {
    pub section_id: u32,
    pub slot_index: usize,
}

/// Tag for an individual star mark above a slot. Despawned + respawned
/// only on capture (which doesn't exist yet); for now they're spawned
/// once at setup_map and never change.
#[derive(Component)]
pub struct MapSlotStar;

/// Text2d label inside a slot box, showing the placed building's name
/// (empty when the slot is unbuilt). Updated by `update_map_slot_labels`.
#[derive(Component)]
pub struct MapSlotLabel {
    pub section_id: u32,
    pub slot_index: usize,
}

/// Root entity of a building-choice popup. One at a time. Spawned by the
/// click handler when the player clicks an unbuilt slot; despawned by the
/// choice button, by clicking outside, or by switching to combat view.
#[derive(Component)]
pub struct BuildingPopup;

/// Marker on each option button inside a popup — carries which slot the
/// click should write to and which building the option represents.
#[derive(Component)]
pub struct BuildingChoiceButton {
    pub section_id: u32,
    pub slot_index: usize,
    pub building: MapBuilding,
}

/// The description text element at the bottom of a building popup.
/// `update_building_description` writes into it whenever the player
/// hovers an option; cleared when the cursor leaves all options.
#[derive(Component)]
pub struct BuildingPopupDescription;

// ---------- Debug overlay (claim land + trigger phase) ----------

/// Toggled by the CLAIM debug button. While `active`, left-clicks on
/// the map flip the clicked section's `owned` flag instead of sailing.
/// Pure dev affordance — keeps capture mechanics out of normal play
/// until they're actually designed.
#[derive(Resource, Default)]
pub struct DebugClaimMode {
    pub active: bool,
}

/// Fired by the PHASE debug button. `map_begin_phase` reacts to it the
/// same way it reacts to a Map-bound view change, re-running the
/// per-building begin-phase sequence on whatever current state exists.
#[derive(Event)]
pub struct TriggerMapPhase;

/// Root of the bottom-right debug panel (CLAIM + PHASE buttons).
#[derive(Component)]
pub struct DebugPanel;

/// Identifies which debug button was clicked. Marker carries the kind
/// rather than separate component types so one query handles both.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
pub enum DebugButton {
    ClaimMode,
    Phase,
}

/// Tag on the CLAIM button's text label so `update_claim_label` can
/// flip it between "CLAIM" and "CLAIMING…" without rebuilding the node.
#[derive(Component)]
pub struct DebugClaimLabel;

// ---------- Phase animations (timeline + primitives) ----------

/// Sequenced animation queue for the map's phase effects (e.g., a
/// Dockyard's "begin phase" pulse → beam → neighbor pulse). Steps are
/// pushed by `map_begin_phase` and fired by `advance_map_anim_timeline`
/// when their `at` time is reached. Per-Dockyard sequences are appended
/// back-to-back so multiple Dockyards play in order, not all at once.
///
/// Cleared on view change so leaving and re-entering the map starts
/// fresh — we never want a stale half-finished sequence to resume.
#[derive(Resource, Default)]
pub struct MapAnimTimeline {
    pub elapsed: f32,
    pub steps: std::collections::VecDeque<TimelineStep>,
}

pub struct TimelineStep {
    /// Seconds since this sequence started. The driver compares against
    /// `MapAnimTimeline.elapsed`; once `at <= elapsed` the step fires.
    pub at: f32,
    pub action: TimelineAction,
}

/// What a timeline step does when it fires. Adding a new animation
/// shape (e.g., shockwave ring, particle burst) is one variant here +
/// one match arm in the driver.
pub enum TimelineAction {
    /// Pulse a slot tile at world `pos`: an overlay sprite that
    /// fades-in / fades-out while gently scaling.
    Pulse { pos: Vec2, color: Color, duration: f32 },
    /// Draw a thin beam from `from` to `to` that fades in then out.
    /// The visual link "between buildings" in adjacency effects.
    Beam { from: Vec2, to: Vec2, color: Color, duration: f32 },
}

/// Transient overlay above a slot: alpha + scale bell-curve over its
/// `Timer`'s lifetime, then despawned by `update_anim_pulses`.
#[derive(Component)]
pub struct AnimPulse {
    pub timer: Timer,
    pub peak_alpha: f32,
}

/// Thin sprite stretched between two points. Alpha bell-curve driven by
/// `update_anim_beams`; despawned at finish.
#[derive(Component)]
pub struct AnimBeam {
    pub timer: Timer,
    pub peak_alpha: f32,
}

// Animation tuning — short, snappy. Tweak here.
const ANIM_PULSE_DUR: f32   = 0.45;
const ANIM_BEAM_DUR:  f32   = 0.40;
const ANIM_PULSE_PEAK_ALPHA: f32 = 0.55;
const ANIM_BEAM_PEAK_ALPHA:  f32 = 0.85;
/// How much the pulse sprite scales up at its peak (1.0 = no scale).
const ANIM_PULSE_PEAK_SCALE: f32 = 1.30;
/// Pulse base size — slightly larger than the slot box so it reads as a
/// glow over it rather than coverage.
const ANIM_PULSE_SIZE: f32 = SLOT_SIZE + 4.0;
/// Beam thickness in world units.
const ANIM_BEAM_THICKNESS: f32 = 1.4;
/// Overlap between sequence steps so the sequence flows instead of
/// stopping between each pulse and beam.
const ANIM_STEP_OVERLAP: f32 = 0.5;

// ---------- Map authoring ----------

/// Hand-authored 10-section layout. Topology is intentionally irregular:
/// sections vary in size (small corner wedges, large L-shaped flanks,
/// irregular central pentagons) and the trijunctions zig-zag in y so no
/// continuous horizontal or vertical line crosses the map. Each interior
/// edge then gets the deterministic `wobble_for_edge` curve on top,
/// finishing the hand-drawn feel. Trijunction names like `j_134` mean
/// "shared by sections 1, 3, 4" — handy for tracing adjacencies.
fn build_default_map() -> Vec<MapSection> {
    let m = PLAY_WORLD / 2.0; // 100

    // Edge-boundary points — interior boundaries hitting the square edges.
    // Asymmetric distribution (2 left, 2 top, 2 right, 2 bottom, all at
    // different distances from corners) so the perimeter doesn't read as
    // a regular grid either.
    let p_left_t  = v(-m,    33.0);  // left edge,   S0|S3
    let p_left_b  = v(-m,   -22.0);  // left edge,   S3|S7
    let p_top_l   = v(-32.0,  m);    // top edge,    S0|S1
    let p_top_r   = v( 44.0,  m);    // top edge,    S1|S2
    let p_right_t = v( m,    32.0);  // right edge,  S2|S6
    let p_right_b = v( m,   -42.0);  // right edge,  S6|S9
    let p_bot_l   = v(-28.0, -m);    // bottom edge, S7|S8
    let p_bot_r   = v( 38.0, -m);    // bottom edge, S8|S9

    // Interior trijunctions — each shared by exactly 3 sections. Named by
    // those section ids so polygons can reference the same point and
    // shared edges line up exactly. y-values are deliberately scattered:
    // there is no horizontal or vertical line that all junctions sit on.
    let j_013 = v(-48.0,  56.0);   // S0/S1/S3
    let j_134 = v( 12.0,  62.0);   // S1/S3/S4
    let j_124 = v( 54.0,  64.0);   // S1/S2/S4
    let j_345 = v(-26.0,  24.0);   // S3/S4/S5
    let j_246 = v( 62.0,  26.0);   // S2/S4/S6
    let j_357 = v(-54.0,  -8.0);   // S3/S5/S7
    let j_456 = v( 16.0,   4.0);   // S4/S5/S6
    let j_578 = v(-14.0, -56.0);   // S5/S7/S8
    let j_568 = v( 44.0, -26.0);   // S5/S6/S8
    let j_689 = v( 76.0, -54.0);   // S6/S8/S9

    // Square corners.
    let sq_tl = v(-m,  m);
    let sq_tr = v( m,  m);
    let sq_br = v( m, -m);
    let sq_bl = v(-m, -m);

    // Cell corner lists (CCW). Each polygon refers to the trijunctions
    // above, so shared edges have *exactly* matching endpoints.
    let cells: [(u32, Vec<Vec2>, Vec2, &[u32]); 10] = [
        // S0 — small top-left wedge.
        (0,
         vec![sq_tl, p_left_t, j_013, p_top_l],
         v(-72.0, 71.0),
         &[1, 3]),
        // S1 — top-middle, fans across the upper interior.
        (1,
         vec![p_top_l, j_013, j_134, j_124, p_top_r],
         v(  4.0, 76.0),
         &[0, 2, 3, 4]),
        // S2 — top-right corner.
        (2,
         vec![p_top_r, j_124, j_246, p_right_t, sq_tr],
         v( 72.0, 64.0),
         &[1, 4, 6]),
        // S3 — tall left flank, runs the middle third of the left edge.
        (3,
         vec![p_left_t, p_left_b, j_357, j_345, j_134, j_013],
         v(-54.0, 22.0),
         &[0, 1, 4, 5, 7]),
        // S4 — central-upper irregular pentagon. No edges with the
        // outside square; entirely interior.
        (4,
         vec![j_345, j_456, j_246, j_124, j_134],
         v( 22.0, 36.0),
         &[1, 2, 3, 5, 6]),
        // S5 — central-lower irregular pentagon. Also fully interior.
        (5,
         vec![j_357, j_578, j_568, j_456, j_345],
         v( -7.0, -12.0),
         &[3, 4, 6, 7, 8]),
        // S6 — large L-shaped right flank wrapping around S4 and S5.
        (6,
         vec![p_right_t, j_246, j_456, j_568, j_689, p_right_b],
         v( 67.0, -11.0),
         &[2, 4, 5, 8, 9]),
        // S7 — bottom-left chunk including the SW square corner.
        (7,
         vec![p_left_b, sq_bl, p_bot_l, j_578, j_357],
         v(-60.0, -58.0),
         &[3, 5, 8]),
        // S8 — bottom-middle.
        (8,
         vec![p_bot_l, p_bot_r, j_689, j_568, j_578],
         v( 22.0, -67.0),
         &[5, 6, 7, 9]),
        // S9 — bottom-right wedge including the SE square corner.
        (9,
         vec![p_bot_r, sq_br, p_right_b, j_689],
         v( 78.0, -75.0),
         &[6, 8]),
    ];

    cells
        .into_iter()
        .map(|(id, corners, center, adj)| {
            let polygon = build_section_polygon(&corners);
            MapSection {
                id,
                corners,
                polygon,
                center,
                adjacencies: adj.to_vec(),
                // Stars + slots are filled in by `MapState::new`. We can't
                // compute stars here because BFS needs every section's
                // adjacencies to exist first.
                stars: 1,
                slots: Vec::new(),
            }
        })
        .collect()
}

#[inline]
fn v(x: f32, y: f32) -> Vec2 { Vec2::new(x, y) }

/// Build the full polygon vertex list from corner points by inserting the
/// shared deterministic wobble between any two corners that lie on an
/// interior boundary. Outer-square edges stay straight so the map fills
/// the play area cleanly.
fn build_section_polygon(corners: &[Vec2]) -> Vec<Vec2> {
    let n = corners.len();
    let mut pts = Vec::with_capacity(n * 5);
    for i in 0..n {
        let a = corners[i];
        let b = corners[(i + 1) % n];
        pts.push(a);
        if !is_outer_edge(a, b) {
            pts.extend(wobble_for_edge(a, b));
        }
    }
    pts
}

fn is_outer_edge(a: Vec2, b: Vec2) -> bool {
    let m = PLAY_WORLD / 2.0;
    let on_left  = (a.x - (-m)).abs() < 0.01 && (b.x - (-m)).abs() < 0.01;
    let on_right = (a.x -  m  ).abs() < 0.01 && (b.x -  m  ).abs() < 0.01;
    let on_bot   = (a.y - (-m)).abs() < 0.01 && (b.y - (-m)).abs() < 0.01;
    let on_top   = (a.y -  m  ).abs() < 0.01 && (b.y -  m  ).abs() < 0.01;
    on_left || on_right || on_bot || on_top
}

/// Deterministic curve points along an interior edge. Both polygons sharing
/// the edge call this with their own (a, b) order — endpoints are sorted
/// canonically and the result is reversed if needed, so the resulting
/// curve is *identical* on both sides of the boundary.
fn wobble_for_edge(a: Vec2, b: Vec2) -> Vec<Vec2> {
    // Canonical order — lex-smaller endpoint goes first.
    let (p, q, reversed) = if (a.x, a.y) <= (b.x, b.y) {
        (a, b, false)
    } else {
        (b, a, true)
    };

    // Phase derived from the canonical endpoints — same edge always gets
    // the same wobble shape regardless of polygon iteration order.
    let phase = p.x * 0.131 + p.y * 0.317 + q.x * 0.713 + q.y * 1.103;
    // Larger amplitude so boundaries read as natural map dividers, not
    // grid lines. Windowed by sin(πt) at the endpoints so corners stay
    // exact (no kinks), and capped relative to the edge length so short
    // edges don't get over-wiggled.
    let amp = 8.0_f32.min(((q - p).length() * 0.18).max(2.0));
    const STEPS: u32 = 8;

    let dir = q - p;
    let len = dir.length();
    let unit = if len > 0.001 { dir / len } else { Vec2::X };
    let perp = Vec2::new(-unit.y, unit.x);

    let mut pts: Vec<Vec2> = (1..STEPS)
        .map(|i| {
            let t = i as f32 / STEPS as f32;
            let along = p + dir * t;
            // Two superimposed sines for an irregular hand-drawn feel.
            let s = (t * std::f32::consts::PI * 2.5 + phase).sin() * 0.65
                  + (t * std::f32::consts::PI * 1.2 + phase * 1.7).cos() * 0.35;
            // Window the wobble so endpoints are exact (no kink at corners).
            let window = (t * std::f32::consts::PI).sin();
            along + perp * (s * amp * window)
        })
        .collect();

    if reversed { pts.reverse(); }
    pts
}

// ---------- Mesh builders ----------

/// Build a single mitered triangle-strip "ribbon" tracing `points` with
/// uniform `width`. Each interior vertex uses the average of incoming and
/// outgoing segment perpendiculars, so the ribbon bends smoothly along the
/// curve instead of reading as rotated rectangles meeting at sharp corners.
fn build_ribbon_mesh(points: &[Vec2], width: f32) -> Mesh {
    let n = points.len();
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    if n < 2 { return mesh; }

    let half_w = width * 0.5;
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(n * 2);

    for i in 0..n {
        let p = points[i];
        // Perpendicular at this point. Endpoints use the single adjacent
        // segment's perpendicular; interior points average incoming +
        // outgoing — that's a simple miter join, smooth for shallow curves.
        let perp = if i == 0 {
            let d = (points[1] - points[0]).normalize_or_zero();
            Vec2::new(-d.y, d.x)
        } else if i == n - 1 {
            let d = (points[n - 1] - points[n - 2]).normalize_or_zero();
            Vec2::new(-d.y, d.x)
        } else {
            let d_in  = (points[i] - points[i - 1]).normalize_or_zero();
            let d_out = (points[i + 1] - points[i]).normalize_or_zero();
            let d_avg = (d_in + d_out).normalize_or_zero();
            Vec2::new(-d_avg.y, d_avg.x)
        };

        positions.push([p.x + perp.x * half_w, p.y + perp.y * half_w, 0.0]);
        positions.push([p.x - perp.x * half_w, p.y - perp.y * half_w, 0.0]);
    }

    let mut indices: Vec<u32> = Vec::with_capacity((n - 1) * 6);
    for i in 0..(n - 1) as u32 {
        let i0 = 2 * i;
        let i1 = 2 * i + 1;
        let i2 = 2 * i + 2;
        let i3 = 2 * i + 3;
        // Two triangles per quad, both CCW.
        indices.extend_from_slice(&[i0, i1, i2, i1, i3, i2]);
    }

    let normals: Vec<[f32; 3]> = vec![[0.0, 0.0, 1.0]; positions.len()];
    let uvs:     Vec<[f32; 2]> = vec![[0.0, 0.0];      positions.len()];
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL,   normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0,     uvs);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// Pre-rasterize the section fills into a single `PLAY_INTERNAL × PLAY_INTERNAL`
/// image. One pixel per internal-resolution play pixel, color picked by
/// point-in-polygon against the section list.
///
/// Tints are *baked against the current ocean color*: instead of writing a
/// translucent green/red and letting the GPU alpha-blend it over the ocean
/// clear, we pre-mix the tint with `palette.ocean` here and emit opaque
/// pixels. Reason: the GPU blend is hue-shifted by the ocean (e.g. red over
/// daytime light-blue ocean reads as purple), so tints looked
/// palette-dependent. Pre-mixing at a high tint weight keeps the tint
/// dominant and consistent across day/night ocean colors. This is why
/// `refresh_map_fill_on_palette_change` re-rasterizes whenever the palette
/// (and therefore ocean) changes.
///
/// `regenerate_map_fill_image` is the hook to call from a state-change
/// handler later (e.g. capturing a section).
fn build_map_fill_image(state: &MapState, palette: &Palette) -> Image {
    let w = PLAY_INTERNAL;
    let h = PLAY_INTERNAL;
    let mut data = vec![0u8; (w * h * 4) as usize];

    // Vivid green/red tints baked against the *actual* ocean color. The
    // 0.70 weight keeps the tint dominant in both day and night so a
    // section reads as "owned green" or "enemy red" regardless of palette;
    // the remaining 0.30 ocean lets sections feel anchored to the water.
    let owned = blend_to_bgra(palette.ocean, Color::srgb(0.18, 0.98, 0.40), 0.70);
    let enemy = blend_to_bgra(palette.ocean, Color::srgb(1.00, 0.05, 0.15), 0.70);
    let transparent: [u8; 4] = [0, 0, 0, 0]; // outside any section — let ocean show

    for py in 0..h {
        for px in 0..w {
            // Pixel center → world coords. Image y=0 is the top row, world y
            // is up, so flip y.
            let world_x = (px as f32 + 0.5) / w as f32 * PLAY_WORLD - PLAY_WORLD / 2.0;
            let world_y = ((h - py) as f32 - 0.5) / h as f32 * PLAY_WORLD - PLAY_WORLD / 2.0;
            let pos = Vec2::new(world_x, world_y);

            let mut color = transparent;
            for section in &state.sections {
                if point_in_polygon(pos, &section.polygon) {
                    color = if state.owned[section.id as usize] { owned } else { enemy };
                    break;
                }
            }

            let i = ((py * w + px) * 4) as usize;
            data[i + 0] = color[0]; // B
            data[i + 1] = color[1]; // G
            data[i + 2] = color[2]; // R
            data[i + 3] = color[3]; // A
        }
    }

    let mut img = Image::new(
        Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::Bgra8UnormSrgb,
        RenderAssetUsages::default(),
    );
    img.sampler = ImageSampler::nearest();
    img
}

/// Mix `tint` into `base` at weight `t` (0=base, 1=tint) and return BGRA
/// bytes for the `Bgra8UnormSrgb` format. Mixing is in sRGB space — close
/// enough to perceptual for the broad "green over blue" / "red over blue"
/// blends we use, and matches the texture format so no extra gamma math.
fn blend_to_bgra(base: Color, tint: Color, t: f32) -> [u8; 4] {
    let b: bevy::color::Srgba = base.into();
    let n: bevy::color::Srgba = tint.into();
    let r  = (b.red   * (1.0 - t) + n.red   * t).clamp(0.0, 1.0);
    let g  = (b.green * (1.0 - t) + n.green * t).clamp(0.0, 1.0);
    let bl = (b.blue  * (1.0 - t) + n.blue  * t).clamp(0.0, 1.0);
    [
        (bl * 255.0).round() as u8, // B
        (g  * 255.0).round() as u8, // G
        (r  * 255.0).round() as u8, // R
        255,                        // A (opaque — tint already mixed with ocean)
    ]
}

// ---------- Setup ----------

pub fn setup_map(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    pm: Option<Res<PaletteMaterials>>,
    palette: Res<Palette>,
    state: Res<MapState>,
) {
    let Some(pm) = pm else { return; };

    // Section fills — one pre-rasterized sprite for the entire map. Single
    // quad rendering = no per-triangle seams in the alpha blend, no matter
    // the alpha value or section shape.
    let fill_handle = images.add(build_map_fill_image(&state, &palette));
    commands.spawn((
        Sprite {
            image: fill_handle,
            custom_size: Some(Vec2::splat(PLAY_WORLD)),
            ..default()
        },
        Transform::from_xyz(0.0, 0.0, Z_FILL),
        RenderLayers::layer(MAP_LAYER),
        MapFillSprite,
    ));

    // Boundary dividers — one continuous mitered ribbon per *unique*
    // interior edge. Deduped across polygons (each interior edge is
    // shared by exactly two sections) so the divider draws once and the
    // wobble curve looks like a single hand-drawn line instead of a
    // staircase of rotated rectangles. Quantizes corner coordinates to
    // sidestep floating-point key drift in the dedupe set.
    let q = |v: Vec2| (
        (v.x * 1000.0).round() as i32,
        (v.y * 1000.0).round() as i32,
    );
    let canonical_key = |a: Vec2, b: Vec2| {
        let (p, r) = if (a.x, a.y) <= (b.x, b.y) { (a, b) } else { (b, a) };
        (q(p), q(r))
    };
    let mut seen: std::collections::HashSet<((i32, i32), (i32, i32))>
        = std::collections::HashSet::new();
    for section in &state.sections {
        let n = section.corners.len();
        for i in 0..n {
            let a = section.corners[i];
            let b = section.corners[(i + 1) % n];
            if is_outer_edge(a, b) { continue; }
            if !seen.insert(canonical_key(a, b)) { continue; }

            // Path through the wobble: [a, w0..wN, b].
            let mut path = Vec::with_capacity(10);
            path.push(a);
            path.extend(wobble_for_edge(a, b));
            path.push(b);

            let ribbon = build_ribbon_mesh(&path, 1.4);
            commands.spawn((
                Mesh2d(meshes.add(ribbon)),
                MeshMaterial2d(pm.map_divider.clone()),
                Transform::from_xyz(0.0, 0.0, Z_OUTLINE),
                RenderLayers::layer(MAP_LAYER),
                MapSectionBoundary,
            ));
        }
    }

    // Two layers, two passes:
    //   - Info (stars): on every section so red-zone ratings are
    //     visible too — that's what tells the player which territory
    //     is worth pushing into.
    //   - Build (slot box + label): only on owned sections, since you
    //     can't place a building you don't control.
    let slot_box_mesh = meshes.add(Rectangle::new(SLOT_SIZE, SLOT_SIZE));
    let star_mesh     = meshes.add(Rectangle::new(STAR_SIZE, STAR_SIZE));
    for section in &state.sections {
        spawn_section_stars(&mut commands, section, &star_mesh, &pm);
    }
    for (i, section) in state.sections.iter().enumerate() {
        if !state.owned[i] { continue; }
        spawn_slot_box_and_label(&mut commands, section, &slot_box_mesh, &pm);
    }

    // Map boat — same hull + 8-turret rig as the in-combat ship, just
    // shrunk uniformly by `MAP_BOAT_SCALE` (parent scale propagates to all
    // children). Turrets each get a single forward-pointing barrel —
    // simpler than the in-combat 3-barrel rig because the map view
    // doesn't model weapon configs or aiming, it only needs to *read* as
    // a warship at a glance. All on `MAP_LAYER`.
    let hull_radius      = HULL_WIDTH / 2.0;
    let hull_inner       = HULL_LEN - HULL_WIDTH;
    let hull_mesh        = meshes.add(Capsule2d::new(hull_radius, hull_inner));
    let turret_base_mesh = meshes.add(Circle::new(2.0));
    let barrel_mesh      = meshes.add(Rectangle::new(1.5, 4.0));

    let start = state.section(state.current).center;
    let boat = commands.spawn((
        Mesh2d(hull_mesh),
        MeshMaterial2d(pm.hull.clone()),
        Transform::from_xyz(start.x, start.y, Z_BOAT)
            .with_scale(Vec3::splat(MAP_BOAT_SCALE)),
        Heading(0.0),
        MapBoat,
        RenderLayers::layer(MAP_LAYER),
    )).id();

    // Turret bases + a single barrel each, mirroring `setup_world` but
    // without the BarrelIndex / TurretSlot components since nothing on the
    // map view reads them. Local Z values stack above the hull through
    // child-of-parent composition.
    for (i, (lx, ly)) in TURRET_POSITIONS.iter().enumerate() {
        let mount = TURRET_MOUNTS[i];
        let turret = commands.spawn((
            Mesh2d(turret_base_mesh.clone()),
            MeshMaterial2d(pm.turret.clone()),
            Transform::from_xyz(*lx, *ly, 0.5)
                .with_rotation(Quat::from_rotation_z(mount)),
            RenderLayers::layer(MAP_LAYER),
        )).id();
        commands.entity(turret).insert(ChildOf(boat));

        let barrel = commands.spawn((
            Mesh2d(barrel_mesh.clone()),
            MeshMaterial2d(pm.turret.clone()),
            Transform::from_xyz(0.0, 3.0, 0.1),
            RenderLayers::layer(MAP_LAYER),
        )).id();
        commands.entity(barrel).insert(ChildOf(turret));
    }
}

/// Spawn the row of star marks above a section's center. Always-on
/// info-layer visual — every section shows its rating regardless of
/// ownership, so the player can read difficulty / unlock tier of red
/// zones before deciding where to push.
fn spawn_section_stars(
    commands: &mut Commands,
    section: &MapSection,
    star_mesh: &Handle<Mesh>,
    pm: &PaletteMaterials,
) {
    let stars = section.stars as usize;
    if stars == 0 { return; }
    let pitch = STAR_SIZE + STAR_GAP;
    let row_left = section.center.x - (stars as f32 - 1.0) * 0.5 * pitch;
    let star_y = section.center.y + STAR_Y_OFFSET;
    for s in 0..stars {
        commands.spawn((
            Mesh2d(star_mesh.clone()),
            MeshMaterial2d(pm.map_slot_star.clone()),
            Transform::from_xyz(row_left + s as f32 * pitch, star_y, Z_SLOT_STAR),
            RenderLayers::layer(MAP_LAYER),
            MapSlotStar,
        ));
    }
}

/// Spawn the slot tile + UI label for each slot of an *owned* section.
/// The build layer — only appears once you control the territory.
/// Stars are handled separately by `spawn_section_stars` so they
/// remain visible on enemy zones too.
fn spawn_slot_box_and_label(
    commands: &mut Commands,
    section: &MapSection,
    slot_box_mesh: &Handle<Mesh>,
    pm: &PaletteMaterials,
) {
    let n_slots = section.slots.len();
    if n_slots == 0 { return; }

    for slot_index in 0..n_slots {
        // Single-slot today, so the slot sits at the section center.
        // Multi-slot would offset on x: `center.x + (slot_index as f32
        // - (n - 1) / 2) * SLOT_GAP`.
        let pos = section.center;

        // Slot tile (world-space mesh).
        commands.spawn((
            Mesh2d(slot_box_mesh.clone()),
            MeshMaterial2d(pm.map_slot.clone()),
            Transform::from_xyz(pos.x, pos.y, Z_SLOT_BOX),
            RenderLayers::layer(MAP_LAYER),
            MapSlotBox { section_id: section.id, slot_index },
        ));

        // Label: Bevy UI text node, *not* `Text2d`. The whole map
        // world is rendered to a 200×200 internal buffer that's then
        // nearest-neighbor upscaled to screen — fine for blocky art,
        // but blurry for AA glyphs (their soft edges get chunked into
        // 3×3 blocks). UI nodes bypass the upscale and render at
        // native resolution. Position is set each frame by
        // `update_map_slot_labels` from section world coords.
        commands.spawn((
            Text::new(""),
            TextFont { font_size: theme::FONT_SM, ..default() },
            TextColor(theme::ON_SURFACE),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                ..default()
            },
            Visibility::Hidden,
            MapSlotLabel { section_id: section.id, slot_index },
        ));
    }
}

// ---------- Debug overlay ----------

/// Spawn the bottom-right debug panel: a small column of dev buttons
/// (CLAIM toggle + PHASE re-trigger). Built from `ui_kit` primitives so
/// it inherits theme + pixel-perfect text without local style choices.
pub fn setup_debug_ui(mut commands: Commands) {
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            bottom: Val::Px(8.0),
            right: Val::Px(8.0),
            padding: UiRect::all(Val::Px(theme::PAD_MD)),
            border: UiRect::all(Val::Px(theme::BORDER_W)),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Stretch,
            row_gap: Val::Px(theme::GAP_SM),
            ..default()
        },
        BackgroundColor(theme::SURFACE_RAISED),
        BorderColor(theme::BORDER_SUBTLE),
        ZIndex(50),
        DebugPanel,
    ))
    .with_children(|p| {
        p.spawn(ui_kit::label("DEBUG", theme::FONT_SM, theme::ON_SURFACE_DIM));

        p.spawn((ui_kit::button(theme::SURFACE), DebugButton::ClaimMode))
            .with_children(|b| {
                b.spawn((
                    ui_kit::label("CLAIM", theme::FONT_MD, theme::ON_SURFACE),
                    DebugClaimLabel,
                ));
            });

        p.spawn((ui_kit::button(theme::SURFACE), DebugButton::Phase))
            .with_children(|b| {
                b.spawn(ui_kit::label("PHASE", theme::FONT_MD, theme::ON_SURFACE));
            });
    });
}

/// Click router for the debug buttons. CLAIM toggles the mode flag;
/// PHASE writes a `TriggerMapPhase` event so `map_begin_phase` reruns
/// the same sequence it would on a view change.
pub fn handle_debug_buttons(
    interactions: Query<(&Interaction, &DebugButton), Changed<Interaction>>,
    mut claim_mode: ResMut<DebugClaimMode>,
    mut phase_evt: EventWriter<TriggerMapPhase>,
) {
    for (interaction, button) in &interactions {
        if !matches!(*interaction, Interaction::Pressed) { continue; }
        match *button {
            DebugButton::ClaimMode => claim_mode.active = !claim_mode.active,
            DebugButton::Phase => { phase_evt.write(TriggerMapPhase); }
        }
    }
}

/// Background color for each debug button. CLAIM is special: when the
/// mode is active, its bg sticks at `ACCENT` regardless of hover, so
/// it's obvious you're in claim mode. Hover/press still tint it
/// brighter via the matching short-circuits in the match.
pub fn update_debug_button_tints(
    claim_mode: Res<DebugClaimMode>,
    mut q: Query<(&Interaction, &DebugButton, &mut BackgroundColor)>,
) {
    for (interaction, button, mut bg) in &mut q {
        let claim_locked = matches!(button, DebugButton::ClaimMode) && claim_mode.active;
        bg.0 = match (*interaction, claim_locked) {
            (Interaction::Pressed, _) => theme::ACCENT,
            (Interaction::Hovered, _) => theme::SURFACE_HOVER,
            (Interaction::None, true) => theme::ACCENT,
            (Interaction::None, false) => theme::SURFACE,
        };
    }
}

/// Mirror `DebugClaimMode` state into the CLAIM button's label so the
/// player can see at a glance whether further clicks will claim land.
pub fn update_claim_label(
    claim_mode: Res<DebugClaimMode>,
    mut q: Query<&mut Text, With<DebugClaimLabel>>,
) {
    if !claim_mode.is_changed() { return; }
    let target = if claim_mode.active { "CLAIMING…" } else { "CLAIM" };
    for mut text in &mut q {
        if text.0 != target { text.0 = target.to_string(); }
    }
}

// ---------- Per-frame systems ----------

/// `run_if` predicate for systems that should only tick during combat.
/// Pauses enemy spawning, AI, bullets, fire/frost ticks, etc. while the
/// player is on the map — keeps the world frozen until they re-enter.
pub fn in_combat_view(view: Res<ViewMode>) -> bool {
    *view == ViewMode::Combat
}

/// Toggle which of the two play-target cameras is active. PlayCamera owns
/// `PLAY_LAYER`, MapCamera owns `MAP_LAYER`; both target the same render
/// image. Only the active one renders, so the inactive layer's entities
/// can't bleed through (any seeming bleed-through under `RenderLayers`
/// swap was traced to using a single camera + change-detection).
pub fn apply_view_mode(
    view: Res<ViewMode>,
    mut play_q: Query<&mut Camera, (With<PlayCamera>, Without<MapCamera>)>,
    mut map_q:  Query<&mut Camera, (With<MapCamera>, Without<PlayCamera>)>,
) {
    let want_combat = matches!(*view, ViewMode::Combat);
    if let Ok(mut cam) = play_q.single_mut() {
        if cam.is_active != want_combat { cam.is_active = want_combat; }
    }
    if let Ok(mut cam) = map_q.single_mut() {
        let want_map = !want_combat;
        if cam.is_active != want_map { cam.is_active = want_map; }
    }
}

/// Rebuild the map fill sprite's image. Call this when `MapState.owned`
/// changes (no capture mechanic exists yet, so it isn't wired into a
/// system, but it's the single hook to use later — re-rasterize and swap
/// the texture handle on the existing `MapFillSprite` entity).
#[allow(dead_code)]
pub fn regenerate_map_fill_image(
    state: &MapState,
    palette: &Palette,
    images: &mut Assets<Image>,
    sprite: &mut Sprite,
) {
    let img = build_map_fill_image(state, palette);
    sprite.image = images.add(img);
}

/// Re-rasterize the map fill image whenever the palette changes (the
/// night-mode toggle swaps `palette.ocean`) or when section ownership
/// flips (claim mode / future capture). Necessary because tints are
/// pre-mixed against the ocean color in `build_map_fill_image`, so a
/// stale image would show the old blend after either of those updates.
///
/// Owned-state diffing uses a `Local<Vec<bool>>` snapshot rather than
/// `state.is_changed()` so we don't rebuild the 200×200 image on every
/// frame the boat moves (which mutates `MapState` continuously).
pub fn refresh_map_fill(
    palette: Res<Palette>,
    state: Res<MapState>,
    mut images: ResMut<Assets<Image>>,
    mut q: Query<&mut Sprite, With<MapFillSprite>>,
    mut owned_snapshot: Local<Vec<bool>>,
) {
    let palette_changed = palette.is_changed();
    let owned_changed = if owned_snapshot.len() != state.owned.len() {
        // First-frame init — sync the snapshot, but don't rebuild;
        // setup_map already produced the initial image.
        *owned_snapshot = state.owned.clone();
        false
    } else if owned_snapshot.as_slice() != state.owned.as_slice() {
        *owned_snapshot = state.owned.clone();
        true
    } else {
        false
    };
    if !palette_changed && !owned_changed { return; }

    let Ok(mut sprite) = q.single_mut() else { return; };
    let img = build_map_fill_image(&state, &palette);
    sprite.image = images.add(img);
}

/// React to ownership flips by spawning slot visuals for newly-owned
/// sections. The initial owned section's slot is spawned by `setup_map`;
/// this picks up everything after (debug claim mode today, real capture
/// later). Despawn-on-loss isn't wired because we never lose ownership
/// today — when capture arrives, add the symmetric branch here.
pub fn sync_owned_slot_visuals(
    mut commands: Commands,
    state: Res<MapState>,
    pm: Option<Res<PaletteMaterials>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut owned_snapshot: Local<Vec<bool>>,
) {
    // Same diff-via-snapshot pattern as `refresh_map_fill` — boat moves
    // mutate state every frame and we only care about owned flips.
    if owned_snapshot.len() != state.owned.len() {
        *owned_snapshot = state.owned.clone();
        return;
    }
    if owned_snapshot.as_slice() == state.owned.as_slice() { return; }

    let mut newly_owned: Vec<usize> = Vec::new();
    for (i, &now) in state.owned.iter().enumerate() {
        if now && !owned_snapshot[i] { newly_owned.push(i); }
    }
    *owned_snapshot = state.owned.clone();

    if newly_owned.is_empty() { return; }
    let Some(pm) = pm else { return; };
    // Only the build-layer visuals (slot box + label) here — stars
    // were spawned for every section at startup and stay put across
    // ownership flips.
    let slot_box_mesh = meshes.add(Rectangle::new(SLOT_SIZE, SLOT_SIZE));
    for i in newly_owned {
        spawn_slot_box_and_label(
            &mut commands,
            &state.sections[i],
            &slot_box_mesh,
            &pm,
        );
    }
}

/// Drive the slot labels each frame: write the building name, snap the
/// UI node to the slot's screen position, and gate visibility on map
/// view. Runs every frame because labels are screen-space — window
/// resizes, view switches, and slot updates all need to flow through
/// the same path. Cheap (≤10 sections × 1 slot today).
///
/// Position math mirrors the cursor→world conversion in
/// `map_click_input` but inverted: world center → normalized [0,1] →
/// screen pixels via `play_area_screen_rect`. The label sits just
/// below the slot tile in *screen* units (upscale factor handles the
/// physical offset).
pub fn update_map_slot_labels(
    state: Res<MapState>,
    view: Res<ViewMode>,
    windows: Query<&Window, With<PrimaryWindow>>,
    window_mode: Res<WindowMode>,
    mut q: Query<(&MapSlotLabel, &mut Node, &mut Text, &mut Visibility)>,
) {
    let Ok(win) = windows.single() else { return; };
    let (left, top, size) = play_area_screen_rect(
        win.width(), win.height(), effective_ui_width(&window_mode),
    );
    let upscale = size / PLAY_WORLD;
    let in_map = *view == ViewMode::Map;

    for (tag, mut node, mut text, mut vis) in &mut q {
        let section = &state.sections[tag.section_id as usize];

        // Text content from current slot state.
        let label = section.slots
            .get(tag.slot_index)
            .copied()
            .flatten()
            .map(|b| b.label())
            .unwrap_or("");
        if text.0 != label { text.0 = label.to_string(); }

        // Visibility: hidden in combat view; also hidden when empty so
        // an empty zero-size node doesn't quietly catch hover events.
        let want_vis = if in_map && !label.is_empty() {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *vis != want_vis { *vis = want_vis; }

        if !in_map || label.is_empty() { continue; }

        // World center → screen pixel position of the slot tile.
        let nx = (section.center.x + PLAY_WORLD / 2.0) / PLAY_WORLD;
        let ny = (PLAY_WORLD / 2.0 - section.center.y) / PLAY_WORLD;
        let slot_x = left + nx * size;
        let slot_y = top  + ny * size;

        // Anchor below the slot tile. Approximate horizontal centering
        // — we don't have the rasterized text width, so estimate from
        // char count × font size × constant. Off by a few pixels for
        // odd-width strings; close enough that the label reads as
        // belonging to the tile.
        let approx_w = label.chars().count() as f32 * theme::FONT_SM * 0.55;
        let label_x = slot_x - approx_w * 0.5;
        let label_y = slot_y + (SLOT_HALF + 2.0) * upscale;
        node.left = Val::Px(label_x);
        node.top  = Val::Px(label_y);
    }
}

/// Map-view click handler — five modes, in priority order:
///   1. **UI button absorbed it** — any `Button` Pressed → bail. UI
///      handlers (popup options, debug buttons) do their own work; we
///      don't touch world state.
///   2. **Debug claim mode active** — point-in-polygon resolves which
///      section was clicked; flip its `owned` flag and consume the
///      click. (Reactive systems pick up the change to spawn slots
///      and re-rasterize the map fill.)
///   3. **Popup is open and click lands outside any UI button** —
///      treat as dismiss-intent: despawn the popup and return *without*
///      sailing the boat. Single click closes; second click sails.
///   4. **Click on a slot tile in an owned section** — open a building
///      picker popup at the cursor.
///   5. Otherwise — set the boat's world target (existing behavior).
///
/// Crossing into a red section while sailing will still trigger combat
/// (handled in `map_boat_movement`).
pub fn map_click_input(
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    window_mode: Res<WindowMode>,
    view: Res<ViewMode>,
    claim_mode: Res<DebugClaimMode>,
    mut state: ResMut<MapState>,
    mut commands: Commands,
    interactions: Query<&Interaction, With<Button>>,
    popups: Query<Entity, With<BuildingPopup>>,
) {
    if *view != ViewMode::Map { return; }
    if !mouse.just_pressed(MouseButton::Left) { return; }

    // (1) UI button took the click — let its handler do the work.
    if interactions.iter().any(|i| matches!(i, Interaction::Pressed)) {
        return;
    }

    let Ok(win) = windows.single() else { return; };
    let Some(c) = win.cursor_position() else { return; };

    let (left, top, size) =
        play_area_screen_rect(win.width(), win.height(), effective_ui_width(&window_mode));
    if c.x < left || c.x > left + size || c.y < top || c.y > top + size { return; }
    let nx = (c.x - left) / size;
    let ny = (c.y - top) / size;
    let world = Vec2::new((nx - 0.5) * PLAY_WORLD, (0.5 - ny) * PLAY_WORLD);

    // (2) Claim mode — point-in-polygon decides which section was clicked.
    // We claim regardless of current owner state (a no-op flip on already-
    // owned sections is harmless and keeps the handler trivial).
    if claim_mode.active {
        for i in 0..state.sections.len() {
            if point_in_polygon(world, &state.sections[i].polygon) {
                if !state.owned[i] { state.owned[i] = true; }
                break;
            }
        }
        return;
    }

    // (3) Outside-popup click → close popup, no boat target.
    if let Ok(popup) = popups.single() {
        commands.entity(popup).despawn();
        return;
    }

    // (4) Slot click? Only owned sections render slots, so only check those.
    for i in 0..state.sections.len() {
        if !state.owned[i] { continue; }
        let section = &state.sections[i];
        for slot_index in 0..section.slots.len() {
            // Mirror the layout in `spawn_slot_visuals` — single slot at
            // section center for now. Multi-slot would offset on x.
            let slot_pos = section.center;
            if (world.x - slot_pos.x).abs() <= SLOT_HALF
                && (world.y - slot_pos.y).abs() <= SLOT_HALF
            {
                // Built slots are inert for now — clicking does nothing.
                // Future: open a details/upgrade panel.
                if section.slots[slot_index].is_some() { return; }
                let options = MapBuilding::options_for_stars(section.stars);
                if options.is_empty() { return; }
                spawn_building_popup(&mut commands, c, section.id, slot_index, &options);
                return;
            }
        }
    }

    // (5) Default — sail there.
    state.boat_target = Some(world);
}

/// Spawn the building-picker popup anchored to the cursor. Layout:
///   - "BUILD" header (dim, small).
///   - Vertical list of option buttons, each stretched to popup width
///     so they read as list rows rather than chips.
///   - Description footer that `update_building_description` fills
///     in while the cursor sits over an option, and clears otherwise.
///
/// Colors + sizing all flow through `ui_kit::theme`. Only the popup root
/// is hand-built (absolute positioning + `ZIndex` + click-absorbing
/// `Button`); children compose from kit primitives.
fn spawn_building_popup(
    commands: &mut Commands,
    cursor_screen: Vec2,
    section_id: u32,
    slot_index: usize,
    options: &[MapBuilding],
) {
    let popup = commands.spawn((
        Node {
            // Anchor to the cursor; tiny offset so the pointer doesn't
            // immediately overlap the first option.
            position_type: PositionType::Absolute,
            left: Val::Px(cursor_screen.x + 6.0),
            top:  Val::Px(cursor_screen.y + 6.0),
            padding: UiRect::all(Val::Px(theme::PAD_MD)),
            border: UiRect::all(Val::Px(theme::BORDER_W)),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Stretch,
            // Soft minimum so a single short option doesn't produce a
            // tiny popup, and a soft cap so a long localized label
            // doesn't grow the popup off-screen — within the cap, text
            // wraps. Auto-grow within these bounds = localization-safe.
            min_width: Val::Px(140.0),
            max_width: Val::Px(260.0),
            row_gap: Val::Px(theme::GAP_SM),
            ..default()
        },
        BackgroundColor(theme::SURFACE_RAISED),
        BorderColor(theme::BORDER_SUBTLE),
        ZIndex(100),
        // Mark the root with `Button` so a click on the popup's chrome
        // (not on an option) still registers `Interaction::Pressed`.
        // `map_click_input` then bails on rule (1), keeping the popup
        // open instead of accidentally sailing the boat.
        Button,
        BuildingPopup,
    )).id();

    commands.entity(popup).with_children(|p| {
        // Header
        p.spawn(ui_kit::label(
            tr("map_popup_build"), theme::FONT_SM, theme::ON_SURFACE_DIM,
        ));

        // Options list — each button stretches to popup width so they
        // line up as list rows. Built inline (rather than via
        // `ui_kit::button`) because the kit's button bakes its own
        // Node, and Bevy bundles can't have two `Node` components in
        // the same spawn — list-row layout is custom enough that
        // re-using the kit's default Node and overriding it would
        // collide. Visuals (Button + BackgroundColor + the kit's
        // `label` child) still flow through the theme.
        for &opt in options {
            p.spawn((
                Button,
                Node {
                    padding: UiRect::axes(
                        Val::Px(theme::PAD_MD), Val::Px(theme::PAD_SM),
                    ),
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::FlexStart,
                    width: Val::Percent(100.0),
                    ..default()
                },
                BackgroundColor(theme::SURFACE),
                BuildingChoiceButton { section_id, slot_index, building: opt },
            ))
            .with_children(|b| {
                b.spawn(ui_kit::label(
                    opt.label(), theme::FONT_MD, theme::ON_SURFACE,
                ));
            });
        }

        // Description footer — empty until hovered. Sits flush at the
        // bottom of the popup; lighter color so it reads as supporting
        // text, not another option.
        p.spawn((
            ui_kit::label("", theme::FONT_SM, theme::ON_SURFACE_DIM),
            BuildingPopupDescription,
        ));
    });
}

/// Reactive bg tint for popup option buttons: idle → `SURFACE`, hovered
/// → `SURFACE_HOVER`, pressed → `ACCENT` so the click feels acknowledged
/// even before the resolver fires. Filtering by `Changed<Interaction>`
/// keeps this cheap — we only write when the state actually flips.
pub fn update_building_button_tints(
    mut q: Query<
        (&Interaction, &mut BackgroundColor),
        (With<BuildingChoiceButton>, Changed<Interaction>),
    >,
) {
    for (interaction, mut bg) in &mut q {
        bg.0 = match *interaction {
            Interaction::None    => theme::SURFACE,
            Interaction::Hovered => theme::SURFACE_HOVER,
            Interaction::Pressed => theme::ACCENT,
        };
    }
}

/// Mirror the currently-hovered option's description into the popup's
/// description footer. When the cursor leaves a button, that button
/// fires `Interaction::None` and we clear; if the cursor moved straight
/// to another option, that one's `Hovered` event runs in the same frame
/// and overwrites the cleared text — so the footer reads the *current*
/// hover regardless of source order.
pub fn update_building_description(
    interactions: Query<
        (&Interaction, &BuildingChoiceButton),
        Changed<Interaction>,
    >,
    mut text_q: Query<&mut Text, With<BuildingPopupDescription>>,
) {
    if interactions.is_empty() { return; }
    let Ok(mut text) = text_q.single_mut() else { return; };
    for (interaction, choice) in &interactions {
        match *interaction {
            Interaction::Hovered | Interaction::Pressed => {
                let new = choice.building.description();
                if text.0 != new { text.0 = new.to_string(); }
            }
            Interaction::None => {
                if !text.0.is_empty() { text.0.clear(); }
            }
        }
    }
}

// ---------- Animation drivers ----------

/// Spawn a transient pulse overlay at `pos` using a `Mesh2d` quad +
/// per-entity `ColorMaterial`. Sprite-without-image silently doesn't
/// render in Bevy 0.16 (the renderer skips invalid `image` handles),
/// so we go through the mesh+material path the rest of the map uses.
/// One material per pulse so alpha animates independently.
fn spawn_pulse(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ColorMaterial>,
    pos: Vec2, color: Color, duration: f32,
) {
    let mesh = meshes.add(Rectangle::new(ANIM_PULSE_SIZE, ANIM_PULSE_SIZE));
    let material = materials.add(ColorMaterial {
        color: color.with_alpha(0.0), // fade-in handled by the update system
        alpha_mode: bevy::sprite::AlphaMode2d::Blend,
        ..default()
    });
    commands.spawn((
        Mesh2d(mesh),
        MeshMaterial2d(material),
        Transform::from_xyz(pos.x, pos.y, Z_ANIM),
        RenderLayers::layer(MAP_LAYER),
        AnimPulse {
            timer: Timer::from_seconds(duration, TimerMode::Once),
            peak_alpha: ANIM_PULSE_PEAK_ALPHA,
        },
    ));
}

/// Spawn a transient beam between `from` and `to` as a Mesh2d
/// rectangle, sized to the segment length and rotated to span the
/// endpoints. Each beam owns its own `ColorMaterial` for independent
/// alpha animation.
fn spawn_beam(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ColorMaterial>,
    from: Vec2, to: Vec2, color: Color, duration: f32,
) {
    let dir = to - from;
    let len = dir.length();
    if len < 0.001 { return; } // degenerate — skip
    let angle = dir.y.atan2(dir.x);
    let mid = (from + to) * 0.5;
    let mesh = meshes.add(Rectangle::new(len, ANIM_BEAM_THICKNESS));
    let material = materials.add(ColorMaterial {
        color: color.with_alpha(0.0),
        alpha_mode: bevy::sprite::AlphaMode2d::Blend,
        ..default()
    });
    commands.spawn((
        Mesh2d(mesh),
        MeshMaterial2d(material),
        Transform::from_xyz(mid.x, mid.y, Z_ANIM)
            .with_rotation(Quat::from_rotation_z(angle)),
        RenderLayers::layer(MAP_LAYER),
        AnimBeam {
            timer: Timer::from_seconds(duration, TimerMode::Once),
            peak_alpha: ANIM_BEAM_PEAK_ALPHA,
        },
    ));
}

/// Walk the timeline: any step whose `at` has been reached fires (we
/// spawn the corresponding visual). When the queue drains, reset
/// `elapsed` so the next phase can start at t=0 without leaking time.
pub fn advance_map_anim_timeline(
    time: Res<Time>,
    mut timeline: ResMut<MapAnimTimeline>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
) {
    if timeline.steps.is_empty() {
        if timeline.elapsed != 0.0 { timeline.elapsed = 0.0; }
        return;
    }
    timeline.elapsed += time.delta_secs();
    while let Some(front) = timeline.steps.front() {
        if front.at > timeline.elapsed { break; }
        let step = timeline.steps.pop_front().unwrap();
        match step.action {
            TimelineAction::Pulse { pos, color, duration } => {
                spawn_pulse(&mut commands, &mut meshes, &mut materials, pos, color, duration);
            }
            TimelineAction::Beam { from, to, color, duration } => {
                spawn_beam(&mut commands, &mut meshes, &mut materials, from, to, color, duration);
            }
        }
    }
}

/// Animate every live `AnimPulse`: alpha + scale follow `sin(πt)` so
/// they ease in and out around the midpoint of the timer's life.
/// Color is mutated through `Assets<ColorMaterial>` since the material
/// is per-entity (looked up via the `MeshMaterial2d` handle).
pub fn update_anim_pulses(
    time: Res<Time>,
    mut commands: Commands,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut q: Query<(Entity, &mut AnimPulse, &mut Transform, &MeshMaterial2d<ColorMaterial>)>,
) {
    for (entity, mut anim, mut tf, mat_handle) in &mut q {
        anim.timer.tick(time.delta());
        let t = anim.timer.fraction();
        let bell = (std::f32::consts::PI * t).sin();      // 0 → 1 → 0
        let scale = 1.0 + (ANIM_PULSE_PEAK_SCALE - 1.0) * bell;
        tf.scale = Vec3::new(scale, scale, 1.0);
        if let Some(mat) = materials.get_mut(&mat_handle.0) {
            mat.color = mat.color.with_alpha(anim.peak_alpha * bell);
        }
        if anim.timer.finished() {
            commands.entity(entity).despawn();
        }
    }
}

/// Animate every live `AnimBeam`: alpha bell curve only — the mesh's
/// length is fixed at spawn so we don't touch the transform's scale.
pub fn update_anim_beams(
    time: Res<Time>,
    mut commands: Commands,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut q: Query<(Entity, &mut AnimBeam, &MeshMaterial2d<ColorMaterial>)>,
) {
    for (entity, mut anim, mat_handle) in &mut q {
        anim.timer.tick(time.delta());
        let t = anim.timer.fraction();
        let bell = (std::f32::consts::PI * t).sin();
        if let Some(mat) = materials.get_mut(&mat_handle.0) {
            mat.color = mat.color.with_alpha(anim.peak_alpha * bell);
        }
        if anim.timer.finished() {
            commands.entity(entity).despawn();
        }
    }
}

/// Resolve a click on one of the popup's option buttons: write the chosen
/// building into `MapState`, then despawn the popup. `Changed<Interaction>`
/// gives us a one-shot trigger on press without latching while held.
pub fn handle_building_choice_clicks(
    mut commands: Commands,
    interactions: Query<(&Interaction, &BuildingChoiceButton), Changed<Interaction>>,
    popups: Query<Entity, With<BuildingPopup>>,
    mut state: ResMut<MapState>,
) {
    for (interaction, choice) in &interactions {
        if !matches!(*interaction, Interaction::Pressed) { continue; }
        if let Some(section) = state.sections.get_mut(choice.section_id as usize) {
            if let Some(slot) = section.slots.get_mut(choice.slot_index) {
                *slot = Some(choice.building);
            }
        }
        for popup in &popups { commands.entity(popup).despawn(); }
    }
}

/// "Begin phase" — fires when:
///   - the player enters map view (`view.is_changed()` while `*view == Map`,
///     which also fires on the first frame — a fresh game is map phase 0), or
///   - the PHASE debug button writes a `TriggerMapPhase` event (rerun
///     the same sequence on whatever current state exists).
///
/// Today only `Dockyard` is wired: it pushes a sequence of timeline
/// steps that pulse the source, beam to each neighbor, and pulse each
/// neighbor in turn. Multiple Dockyards play in order (we keep advancing
/// a shared `t` cursor) so each building reads distinctly, not as one
/// chaotic simultaneous burst.
pub fn map_begin_phase(
    view: Res<ViewMode>,
    state: Res<MapState>,
    mut timeline: ResMut<MapAnimTimeline>,
    mut phase_evt: EventReader<TriggerMapPhase>,
    mut commands: Commands,
    anims: Query<Entity, Or<(With<AnimPulse>, With<AnimBeam>)>>,
) {
    let view_to_map = view.is_changed() && *view == ViewMode::Map;
    let manual = !phase_evt.is_empty();
    phase_evt.clear();
    if !view_to_map && !manual { return; }
    // Phase only plays in map view — manual triggers fired during combat
    // are dropped (the button does nothing if you're not on the map).
    if *view != ViewMode::Map { return; }

    // Reset any in-flight sequence — manual re-triggers must not pile up
    // on top of a sequence already animating, and the view-change path
    // is also covered by `close_popup_on_view_change`, but keeping the
    // reset here makes the system self-contained.
    timeline.steps.clear();
    timeline.elapsed = 0.0;
    for e in &anims { commands.entity(e).despawn(); }

    let color = ui_kit::theme::ACCENT;
    // `t` is the running cursor along the queued sequence; each scheduled
    // step bumps it forward. Overlap (`* ANIM_STEP_OVERLAP`) keeps the
    // sequence flowing visually instead of stop-starting between steps.
    let mut t = 0.0_f32;

    for section in &state.sections {
        for slot in &section.slots {
            let Some(building) = *slot else { continue; };
            match building {
                MapBuilding::Weaponry => {
                    // No begin-phase effect today; weapon-customization
                    // is triggered in the slot click flow, not here.
                }
                MapBuilding::Dockyard => {
                    let pos = section.center;
                    let neighbors: Vec<(u32, MapBuilding)> =
                        state.neighbor_buildings(section.id).collect();

                    // Source pulse — kicks off this Dockyard's sequence.
                    timeline.steps.push_back(TimelineStep {
                        at: t,
                        action: TimelineAction::Pulse {
                            pos, color, duration: ANIM_PULSE_DUR,
                        },
                    });

                    // All adjacency effects fire *simultaneously* from
                    // the same source: every beam shares one start
                    // time, every neighbor pulse shares another. This
                    // reads as "the Dockyard pushes out to all
                    // neighbors at once" rather than a slow round-robin.
                    // Different *source* buildings still play
                    // sequentially via the outer `t` cursor below.
                    let beam_start = t + ANIM_PULSE_DUR * ANIM_STEP_OVERLAP;
                    let nbr_pulse_start = beam_start + ANIM_BEAM_DUR * 0.6;
                    for (nbr_id, _) in &neighbors {
                        let nbr_pos = state.sections[*nbr_id as usize].center;
                        timeline.steps.push_back(TimelineStep {
                            at: beam_start,
                            action: TimelineAction::Beam {
                                from: pos, to: nbr_pos,
                                color, duration: ANIM_BEAM_DUR,
                            },
                        });
                        timeline.steps.push_back(TimelineStep {
                            at: nbr_pulse_start,
                            action: TimelineAction::Pulse {
                                pos: nbr_pos, color, duration: ANIM_PULSE_DUR,
                            },
                        });
                    }

                    // Advance the outer cursor past the whole burst —
                    // longest-running step ends at `nbr_pulse_start +
                    // ANIM_PULSE_DUR` (when neighbors exist), else just
                    // the source pulse. Plus a small breath between
                    // buildings so distinct sources don't visually merge.
                    let burst_end = if neighbors.is_empty() {
                        t + ANIM_PULSE_DUR
                    } else {
                        nbr_pulse_start + ANIM_PULSE_DUR
                    };
                    t = burst_end + 0.2;

                    // Keep the debug log so the data side is auditable
                    // even when a sequence is too quick to read on screen.
                    let names: Vec<&str> = neighbors.iter()
                        .map(|(_, b)| b.label())
                        .collect();
                    if names.is_empty() {
                        info!("Dockyard@S{}: no adjacent buildings", section.id);
                    } else {
                        info!(
                            "Dockyard@S{}: adjacent buildings = {:?}",
                            section.id, names,
                        );
                    }
                }
            }
        }
    }
}

/// Reset transient map UI on a view-mode flip:
///   - Despawn any open building popup so we don't return to a stale
///     popup overlaying the map / combat.
///   - Clear the animation timeline + despawn live pulses/beams so a
///     half-finished sequence doesn't keep playing in the background or
///     resume after a combat detour.
///
/// `map_begin_phase` runs the same frame on a Map-bound transition
/// (system order in the schedule puts cleanup before begin) so it
/// repopulates the timeline immediately after we drained it.
pub fn close_popup_on_view_change(
    view: Res<ViewMode>,
    mut commands: Commands,
    popups: Query<Entity, With<BuildingPopup>>,
    mut timeline: ResMut<MapAnimTimeline>,
    anims: Query<Entity, Or<(With<AnimPulse>, With<AnimBeam>)>>,
) {
    if !view.is_changed() { return; }
    for popup in &popups { commands.entity(popup).despawn(); }
    timeline.steps.clear();
    timeline.elapsed = 0.0;
    for e in &anims { commands.entity(e).despawn(); }
}

/// Steer the boat toward `state.boat_target` using the same turn-then-
/// advance pattern as the in-combat ship. Click sets the target; the boat
/// sails there *only* — it doesn't continuously chase the cursor.
///
/// Each frame, after moving, point-in-polygon-test the boat against the
/// section list. On a *transition* (boat crossed into a different section
/// than `state.current`), update `state.current`. If the new section is
/// unowned (red), drop into combat immediately and clear the target so
/// the boat doesn't auto-resume sailing when the player returns to map.
pub fn map_boat_movement(
    time: Res<Time>,
    mut state: ResMut<MapState>,
    mut view: ResMut<ViewMode>,
    mut combat_ctx: ResMut<CombatContext>,
    mut q: Query<(&mut Transform, &mut Heading), With<MapBoat>>,
) {
    if *view != ViewMode::Map { return; }
    let Ok((mut tf, mut heading)) = q.single_mut() else { return; };
    let dt = time.delta_secs();

    if let Some(tgt) = state.boat_target {
        let pos = tf.translation.truncate();
        let to = tgt - pos;
        if to.length() < 1.0 {
            // Arrived — stop, clear target.
            state.boat_target = None;
        } else {
            let desired = (-to.x).atan2(to.y);
            let new_h = approach_angle(heading.0, desired, FRIENDLY_TURN_RATE * dt);
            heading.0 = new_h;
            let dir = Vec2::new(-new_h.sin(), new_h.cos());
            let new_pos = pos + dir * FRIENDLY_SPEED * dt;
            let half = PLAY_WORLD / 2.0;
            tf.translation.x = new_pos.x.clamp(-half, half);
            tf.translation.y = new_pos.y.clamp(-half, half);
            tf.rotation = Quat::from_rotation_z(new_h);
        }
    }

    // Section-transition check. Crossing into an unowned section flips
    // the view to combat — the placeholder for "entered enemy territory".
    // Will be replaced by real capture mechanics later.
    let now_pos = tf.translation.truncate();
    let containing = state
        .sections
        .iter()
        .find(|s| point_in_polygon(now_pos, &s.polygon))
        .map(|s| s.id);
    if let Some(id) = containing {
        if id != state.current {
            state.current = id;
            if !state.owned[id as usize] {
                state.boat_target = None;
                // Capture the section's star rating into combat
                // context *before* the view flip — `spawn_enemies` may
                // fire later in the same frame and pick up the new cap.
                combat_ctx.stars = state.sections[id as usize].stars;
                *view = ViewMode::Combat;
            }
        }
    }
}

// ---------- Geometry helpers ----------

/// Standard ray-casting point-in-polygon. Works for the wobbled (but still
/// non-self-intersecting) polygons we hand-author.
fn point_in_polygon(p: Vec2, poly: &[Vec2]) -> bool {
    let n = poly.len();
    if n < 3 { return false; }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let pi = poly[i];
        let pj = poly[j];
        let crosses = (pi.y > p.y) != (pj.y > p.y);
        if crosses {
            let x_at = (pj.x - pi.x) * (p.y - pi.y) / (pj.y - pi.y) + pi.x;
            if p.x < x_at { inside = !inside; }
        }
        j = i;
    }
    inside
}
