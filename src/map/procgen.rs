//! Procedural map generation via Voronoi tessellation.
//!
//! N seed points are sampled inside the play area with a minimum
//! spacing, nudged toward their cell centroids over two Lloyd's-
//! relaxation passes to round off slivers, then sorted so the seed
//! closest to a corner becomes section 0 — that way the BFS-based
//! star rating spans the full 1→5 range with the easy zone near the
//! start.
//!
//! Each cell is computed by Sutherland–Hodgman clipping of the play-
//! area square against the perpendicular bisector with every other
//! seed (closer-to-self half-plane wins). After all cells are built,
//! corner positions are snapped to a shared canonical set so
//! neighbouring cells agree exactly on their trijunctions — that
//! agreement is what makes `build::wobble_for_edge` produce the same
//! curve on both sides of a shared boundary.
//!
//! Adjacency falls out from shared corners: two cells are adjacent
//! iff their corner lists share at least two canonical points (an
//! edge). Voronoi tessellations are always fully connected, so the
//! star-rating BFS will reach every section.

use bevy::math::Vec2;
use rand::Rng;

use crate::balance::PLAY_WORLD;

use super::MapSection;

/// Build a fresh map with `target_sections` cells. Caller picks the
/// count — typically rolled at run-start from {10, 15, 20} so each
/// run shows a different topology.
pub fn build_random_map(rng: &mut impl Rng, target_sections: usize) -> Vec<MapSection> {
    let m = PLAY_WORLD * 0.5;
    let bounds_min = Vec2::new(-m, -m);
    let bounds_max = Vec2::new( m,  m);

    // 1. Seed sampling + 2 passes of Lloyd's relaxation.
    let mut seeds = sample_seeds(rng, target_sections, bounds_min, bounds_max);
    for _ in 0..2 {
        let cells = compute_voronoi(&seeds, bounds_min, bounds_max);
        for (i, cell) in cells.iter().enumerate() {
            if !cell.is_empty() {
                seeds[i] = polygon_centroid(cell);
            }
        }
    }

    // 2. Re-order so section 0 sits closest to the top-left corner.
    //    BFS distance from there gives a diagonal star gradient
    //    across the map.
    let anchor = Vec2::new(-m * 0.85, m * 0.85);
    seeds.sort_by(|a, b| {
        a.distance_squared(anchor)
            .partial_cmp(&b.distance_squared(anchor))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // 3. Final Voronoi pass.
    let mut cells = compute_voronoi(&seeds, bounds_min, bounds_max);

    // 4. Snap corners to a canonical set so neighbouring cells share
    //    exact trijunction coordinates (otherwise `wobble_for_edge`
    //    sees slightly-different endpoints and emits diverging curves).
    snap_to_canonical(&mut cells, 0.6);

    // 5. Adjacency from shared corners.
    let adjacency = adjacency_from_corners(&cells);

    cells
        .into_iter()
        .enumerate()
        .map(|(i, corners)| {
            let polygon = super::build::build_section_polygon(&corners);
            MapSection {
                id: i as u32,
                corners,
                polygon,
                center: seeds[i],
                adjacencies: adjacency[i].clone(),
                stars: 1,
                slots: Vec::new(),
                boss_class: None,
            }
        })
        .collect()
}

// ---------- Seed sampling ----------

/// Random sampling with a minimum-distance constraint, derived from
/// the target count so cells stay roughly uniform. Falls back to
/// fewer points if the rejection-sampling loop bails out — guarantees
/// progress on pathological RNGs.
fn sample_seeds(rng: &mut impl Rng, n: usize, lo: Vec2, hi: Vec2) -> Vec<Vec2> {
    // Square-root spacing keeps cells visually balanced: bigger N →
    // tighter min distance.
    let area = (hi.x - lo.x) * (hi.y - lo.y);
    let min_dist = (area / n as f32).sqrt() * 0.65;
    // 8-unit inset so seeds don't sit right on the wall — keeps cells
    // off the boundary corners which would otherwise produce slivers.
    let inset = 8.0;
    let mut seeds = Vec::with_capacity(n);
    let mut attempts = 0;
    let max_attempts = n * 200;
    while seeds.len() < n && attempts < max_attempts {
        let p = Vec2::new(
            rng.gen_range(lo.x + inset .. hi.x - inset),
            rng.gen_range(lo.y + inset .. hi.y - inset),
        );
        if seeds.iter().all(|&s: &Vec2| s.distance(p) > min_dist) {
            seeds.push(p);
        }
        attempts += 1;
    }
    seeds
}

// ---------- Voronoi clipping ----------

/// Clip `poly` to the half-plane defined by `(plane_point, plane_normal)`,
/// keeping points where `(p - plane_point) · plane_normal <= 0`. Standard
/// Sutherland–Hodgman: iterate edges, emit kept points + intersection
/// points where edges cross the plane.
fn clip_polygon_by_halfplane(
    poly: &[Vec2],
    plane_point: Vec2,
    plane_normal: Vec2,
) -> Vec<Vec2> {
    let n = poly.len();
    if n == 0 { return Vec::new(); }
    let mut out = Vec::with_capacity(n + 2);
    for i in 0..n {
        let curr = poly[i];
        let next = poly[(i + 1) % n];
        let d_curr = (curr - plane_point).dot(plane_normal);
        let d_next = (next - plane_point).dot(plane_normal);
        let curr_in = d_curr <= 0.0;
        let next_in = d_next <= 0.0;
        if curr_in {
            out.push(curr);
        }
        if curr_in != next_in {
            // Edge crosses the plane — emit the intersection.
            let denom = d_curr - d_next;
            if denom.abs() > 1e-6 {
                let t = d_curr / denom;
                out.push(curr + (next - curr) * t);
            }
        }
    }
    out
}

/// Build one Voronoi cell per seed: starts as the bounding rectangle,
/// then clipped against every other seed's perpendicular bisector
/// (the half-plane closer to `seed`). Cells are returned in seed
/// order; empty cells are possible for degenerate inputs but the
/// caller ignores them.
fn compute_voronoi(seeds: &[Vec2], lo: Vec2, hi: Vec2) -> Vec<Vec<Vec2>> {
    let mut cells = Vec::with_capacity(seeds.len());
    let bounds = vec![
        Vec2::new(lo.x, lo.y),
        Vec2::new(hi.x, lo.y),
        Vec2::new(hi.x, hi.y),
        Vec2::new(lo.x, hi.y),
    ];
    for (i, &seed) in seeds.iter().enumerate() {
        let mut poly = bounds.clone();
        for (j, &other) in seeds.iter().enumerate() {
            if i == j { continue; }
            let mid = (seed + other) * 0.5;
            // Normal points from `seed` toward `other`; keep the side
            // where `(p - mid) · normal <= 0` (closer to `seed`).
            let normal = (other - seed).normalize_or_zero();
            if normal == Vec2::ZERO { continue; }
            poly = clip_polygon_by_halfplane(&poly, mid, normal);
            if poly.is_empty() { break; }
        }
        cells.push(poly);
    }
    cells
}

// ---------- Geometry helpers ----------

/// Area-weighted centroid of a simple polygon (CCW or CW; sign is
/// absorbed). Falls back to the arithmetic mean for degenerate
/// (near-zero area) polygons so a Lloyd iteration on a slim cell
/// doesn't NaN out.
fn polygon_centroid(poly: &[Vec2]) -> Vec2 {
    let n = poly.len();
    if n == 0 { return Vec2::ZERO; }
    if n < 3 {
        let sum: Vec2 = poly.iter().copied().sum();
        return sum / n as f32;
    }
    let mut area2 = 0.0_f32;
    let mut cx = 0.0_f32;
    let mut cy = 0.0_f32;
    for i in 0..n {
        let p0 = poly[i];
        let p1 = poly[(i + 1) % n];
        let cross = p0.x * p1.y - p1.x * p0.y;
        area2 += cross;
        cx += (p0.x + p1.x) * cross;
        cy += (p0.y + p1.y) * cross;
    }
    let a = area2 * 0.5;
    if a.abs() < 1e-3 {
        let sum: Vec2 = poly.iter().copied().sum();
        return sum / n as f32;
    }
    Vec2::new(cx / (6.0 * a), cy / (6.0 * a))
}

/// Snap every cell's corners to a canonical set so cells that share a
/// trijunction use the EXACT same `Vec2` for that point. Without this,
/// floating-point clip-intersection differences leave 0.01-unit
/// mismatches between neighbours, which break shared-edge wobble and
/// adjacency detection.
///
/// Algorithm: walk every corner; if any canonical point is within
/// `eps`, use it; otherwise add this corner as the new canonical.
fn snap_to_canonical(cells: &mut Vec<Vec<Vec2>>, eps: f32) {
    let mut canonical: Vec<Vec2> = Vec::new();
    let eps_sq = eps * eps;
    for cell in cells.iter_mut() {
        for corner in cell.iter_mut() {
            let mut found: Option<Vec2> = None;
            for &c in &canonical {
                if c.distance_squared(*corner) < eps_sq {
                    found = Some(c);
                    break;
                }
            }
            match found {
                Some(c) => *corner = c,
                None => canonical.push(*corner),
            }
        }
    }
}

/// Build per-cell adjacency lists from shared corners. Two cells are
/// adjacent iff they share ≥2 canonical corners (= an edge). After
/// `snap_to_canonical`, "shared" is bitwise equality.
fn adjacency_from_corners(cells: &[Vec<Vec2>]) -> Vec<Vec<u32>> {
    let n = cells.len();
    let mut out = vec![Vec::new(); n];
    for i in 0..n {
        for j in (i + 1)..n {
            let mut shared = 0;
            'outer: for a in &cells[i] {
                for b in &cells[j] {
                    if a == b {
                        shared += 1;
                        if shared >= 2 { break 'outer; }
                        break;
                    }
                }
            }
            if shared >= 2 {
                out[i].push(j as u32);
                out[j].push(i as u32);
            }
        }
    }
    out
}

// ---------- Tests ----------

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;
    use std::collections::{HashSet, VecDeque};

    /// Deterministic RNG for reproducible test runs.
    fn seeded(seed: u64) -> StdRng {
        StdRng::seed_from_u64(seed)
    }

    /// Seeds the test suite exercises. Spread across distinct integers
    /// so a Voronoi bug that only triggers on a narrow RNG path is
    /// likely to land in at least one of them.
    const SEEDS: &[u64] = &[1, 2, 3, 7, 42, 99, 1234, 999_999];

    // ----- polygon_centroid -----

    #[test]
    fn centroid_of_unit_square_is_origin() {
        let sq = [
            Vec2::new(-1.0, -1.0),
            Vec2::new( 1.0, -1.0),
            Vec2::new( 1.0,  1.0),
            Vec2::new(-1.0,  1.0),
        ];
        let c = polygon_centroid(&sq);
        assert!(c.length() < 1e-3, "expected origin, got {c:?}");
    }

    #[test]
    fn centroid_of_offset_square() {
        let sq = [
            Vec2::new(4.0, 2.0),
            Vec2::new(6.0, 2.0),
            Vec2::new(6.0, 4.0),
            Vec2::new(4.0, 4.0),
        ];
        let c = polygon_centroid(&sq);
        assert!((c - Vec2::new(5.0, 3.0)).length() < 1e-3, "got {c:?}");
    }

    #[test]
    fn centroid_of_collinear_polygon_falls_back_to_mean() {
        // Zero-area input → arithmetic mean of points.
        let line = [Vec2::ZERO, Vec2::new(1.0, 0.0), Vec2::new(2.0, 0.0)];
        let c = polygon_centroid(&line);
        assert!((c - Vec2::new(1.0, 0.0)).length() < 1e-3, "got {c:?}");
    }

    #[test]
    fn centroid_of_empty_polygon_is_origin() {
        assert_eq!(polygon_centroid(&[]), Vec2::ZERO);
    }

    // ----- clip_polygon_by_halfplane -----

    fn square(side: f32) -> Vec<Vec2> {
        let h = side * 0.5;
        vec![
            Vec2::new(-h, -h),
            Vec2::new( h, -h),
            Vec2::new( h,  h),
            Vec2::new(-h,  h),
        ]
    }

    #[test]
    fn clip_keeps_polygon_entirely_on_keep_side() {
        // Plane at x=10, normal +X → keep is x <= 10. Square in [-1,1] is fully inside.
        let kept = clip_polygon_by_halfplane(&square(2.0), Vec2::new(10.0, 0.0), Vec2::X);
        assert_eq!(kept.len(), 4);
    }

    #[test]
    fn clip_returns_empty_when_polygon_entirely_on_cut_side() {
        // Same square, plane at x=-10 → keep is x <= -10. Nothing survives.
        let kept = clip_polygon_by_halfplane(&square(2.0), Vec2::new(-10.0, 0.0), Vec2::X);
        assert!(kept.is_empty());
    }

    #[test]
    fn clip_halves_square_with_vertical_plane() {
        // Plane at origin, normal +X → keep is x <= 0. Left half (4 vertices).
        let kept = clip_polygon_by_halfplane(&square(4.0), Vec2::ZERO, Vec2::X);
        assert_eq!(kept.len(), 4, "got polygon {kept:?}");
        for p in &kept {
            assert!(p.x <= 1e-3, "{p:?} should be on x ≤ 0 side");
        }
        // Area of left half should be 4 (half of 4×4 = 8 → wait that's half of 16=8).
        // The full square is 4×4 = 16, half = 8.
        let area = polygon_signed_area(&kept).abs();
        assert!((area - 8.0).abs() < 1e-3, "expected area 8.0, got {area}");
    }

    /// Local helper — area for assertions. Same shoelace formula used
    /// elsewhere; we don't expose it in the production module.
    fn polygon_signed_area(poly: &[Vec2]) -> f32 {
        let n = poly.len();
        let mut acc = 0.0;
        for i in 0..n {
            let p0 = poly[i];
            let p1 = poly[(i + 1) % n];
            acc += p0.x * p1.y - p1.x * p0.y;
        }
        acc * 0.5
    }

    // ----- compute_voronoi -----

    #[test]
    fn single_seed_fills_bounding_box() {
        let lo = Vec2::new(-10.0, -10.0);
        let hi = Vec2::new( 10.0,  10.0);
        let cells = compute_voronoi(&[Vec2::ZERO], lo, hi);
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].len(), 4, "got {:?}", cells[0]);
    }

    #[test]
    fn two_seeds_split_along_bisector() {
        let lo = Vec2::new(-10.0, -10.0);
        let hi = Vec2::new( 10.0,  10.0);
        let seeds = [Vec2::new(-5.0, 0.0), Vec2::new(5.0, 0.0)];
        let cells = compute_voronoi(&seeds, lo, hi);
        assert_eq!(cells.len(), 2);
        // Left cell entirely x ≤ 0; right cell entirely x ≥ 0.
        for p in &cells[0] {
            assert!(p.x <= 1e-3, "left cell vertex {p:?} on wrong side");
        }
        for p in &cells[1] {
            assert!(p.x >= -1e-3, "right cell vertex {p:?} on wrong side");
        }
    }

    #[test]
    fn voronoi_cells_partition_the_bounds() {
        // 4 seeds → 4 cells; total area should equal bounds area.
        let lo = Vec2::new(-10.0, -10.0);
        let hi = Vec2::new( 10.0,  10.0);
        let seeds = [
            Vec2::new(-5.0, -5.0),
            Vec2::new( 5.0, -5.0),
            Vec2::new( 5.0,  5.0),
            Vec2::new(-5.0,  5.0),
        ];
        let cells = compute_voronoi(&seeds, lo, hi);
        let total_area: f32 = cells.iter().map(|c| polygon_signed_area(c).abs()).sum();
        let bounds_area = 20.0 * 20.0;
        assert!(
            (total_area - bounds_area).abs() < 1e-2,
            "cells cover {total_area}, bounds area {bounds_area}",
        );
    }

    // ----- snap_to_canonical -----

    #[test]
    fn snap_merges_near_equal_corners() {
        let mut cells = vec![
            vec![Vec2::new(0.0, 0.0), Vec2::new(1.0, 0.0)],
            vec![Vec2::new(0.0001, 0.0001), Vec2::new(1.0, 0.0001)],
        ];
        snap_to_canonical(&mut cells, 0.5);
        // Both cells should share the EXACT Vec2 for their two corners.
        assert_eq!(cells[0][0], cells[1][0]);
        assert_eq!(cells[0][1], cells[1][1]);
    }

    #[test]
    fn snap_preserves_distant_corners() {
        let cells_before = vec![
            vec![Vec2::ZERO, Vec2::new(50.0, 0.0)],
            vec![Vec2::new(0.0, 50.0), Vec2::new(50.0, 50.0)],
        ];
        let mut cells = cells_before.clone();
        snap_to_canonical(&mut cells, 0.5);
        // 50-unit separation is far above the 0.5 epsilon → no merges.
        assert_eq!(cells, cells_before);
    }

    // ----- adjacency_from_corners -----

    #[test]
    fn cells_sharing_two_corners_are_adjacent() {
        let p1 = Vec2::new(0.0, 0.0);
        let p2 = Vec2::new(1.0, 0.0);
        let cells = vec![
            vec![p1, p2, Vec2::new(0.5, -1.0)],
            vec![p1, p2, Vec2::new(0.5,  1.0)],
        ];
        let adj = adjacency_from_corners(&cells);
        assert_eq!(adj[0], vec![1u32]);
        assert_eq!(adj[1], vec![0u32]);
    }

    #[test]
    fn cells_sharing_one_corner_are_not_adjacent() {
        // A single shared vertex doesn't form an edge, just a touch.
        let shared = Vec2::ZERO;
        let cells = vec![
            vec![shared, Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0)],
            vec![shared, Vec2::new(-1.0, 0.0), Vec2::new(0.0, -1.0)],
        ];
        let adj = adjacency_from_corners(&cells);
        assert!(adj[0].is_empty());
        assert!(adj[1].is_empty());
    }

    #[test]
    fn cells_sharing_no_corners_are_not_adjacent() {
        let cells = vec![
            vec![Vec2::new(0.0, 0.0), Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0)],
            vec![Vec2::new(10.0, 10.0), Vec2::new(11.0, 10.0), Vec2::new(10.0, 11.0)],
        ];
        let adj = adjacency_from_corners(&cells);
        assert!(adj[0].is_empty());
        assert!(adj[1].is_empty());
    }

    // ----- build_random_map: integration invariants -----

    #[test]
    fn map_has_requested_section_count() {
        for n in [10usize, 15, 20] {
            for &seed in SEEDS {
                let mut rng = seeded(seed);
                let sections = build_random_map(&mut rng, n);
                assert_eq!(
                    sections.len(), n,
                    "seed {seed}: expected {n} sections, got {}",
                    sections.len(),
                );
            }
        }
    }

    #[test]
    fn section_ids_are_consecutive_and_zero_indexed() {
        let mut rng = seeded(42);
        let sections = build_random_map(&mut rng, 15);
        for (i, s) in sections.iter().enumerate() {
            assert_eq!(s.id, i as u32);
        }
    }

    #[test]
    fn every_section_has_a_valid_polygon() {
        for &seed in SEEDS {
            let mut rng = seeded(seed);
            let sections = build_random_map(&mut rng, 20);
            for s in &sections {
                assert!(
                    s.corners.len() >= 3,
                    "seed {seed}: section {} has only {} corners",
                    s.id, s.corners.len(),
                );
                assert!(
                    s.polygon.len() >= 3,
                    "seed {seed}: section {} has only {} polygon vertices",
                    s.id, s.polygon.len(),
                );
            }
        }
    }

    #[test]
    fn corners_stay_inside_play_bounds() {
        let m = PLAY_WORLD * 0.5;
        // 0.1 slack — Voronoi clipping should hit ±m exactly, but the
        // canonical-snap pass can nudge corners by < eps (0.6) and
        // floating-point clipping can leave sub-pixel drift.
        let slack = 0.1;
        for &seed in SEEDS {
            let mut rng = seeded(seed);
            let sections = build_random_map(&mut rng, 20);
            for s in &sections {
                for corner in &s.corners {
                    assert!(
                        corner.x >= -m - slack && corner.x <= m + slack
                        && corner.y >= -m - slack && corner.y <= m + slack,
                        "seed {seed}: section {} corner {corner:?} outside ±{m}",
                        s.id,
                    );
                }
            }
        }
    }

    #[test]
    fn adjacency_is_symmetric() {
        for &seed in SEEDS {
            let mut rng = seeded(seed);
            let sections = build_random_map(&mut rng, 15);
            for s in &sections {
                for &nbr in &s.adjacencies {
                    let n = &sections[nbr as usize];
                    assert!(
                        n.adjacencies.contains(&s.id),
                        "seed {seed}: {} lists {} but {} doesn't list {} back",
                        s.id, nbr, nbr, s.id,
                    );
                }
            }
        }
    }

    #[test]
    fn no_self_or_duplicate_adjacencies() {
        for &seed in SEEDS {
            let mut rng = seeded(seed);
            let sections = build_random_map(&mut rng, 20);
            for s in &sections {
                let mut seen = HashSet::new();
                for &nbr in &s.adjacencies {
                    assert_ne!(nbr, s.id, "seed {seed}: section {} lists itself", s.id);
                    assert!(
                        seen.insert(nbr),
                        "seed {seed}: section {} has duplicate adjacency to {}",
                        s.id, nbr,
                    );
                }
            }
        }
    }

    #[test]
    fn map_is_fully_connected() {
        // Voronoi tessellations of a connected domain are themselves
        // connected. BFS from section 0 should visit every section.
        // If this fails, the corner-snap eps is probably too tight to
        // bridge floating-point drift between shared trijunctions.
        for &seed in SEEDS {
            let mut rng = seeded(seed);
            let sections = build_random_map(&mut rng, 20);
            let n = sections.len();
            let mut visited = vec![false; n];
            let mut q = VecDeque::new();
            q.push_back(0usize);
            visited[0] = true;
            while let Some(i) = q.pop_front() {
                for &nbr in &sections[i].adjacencies {
                    let nbr = nbr as usize;
                    if !visited[nbr] {
                        visited[nbr] = true;
                        q.push_back(nbr);
                    }
                }
            }
            let unreached: Vec<usize> = visited.iter().enumerate()
                .filter(|(_, &v)| !v).map(|(i, _)| i).collect();
            assert!(
                unreached.is_empty(),
                "seed {seed}: {} sections unreachable from section 0: {:?}",
                unreached.len(), unreached,
            );
        }
    }

    #[test]
    fn section_0_anchors_in_top_left_quadrant() {
        // The post-relaxation sort places seed 0 closest to the
        // top-left anchor — its center should land with x < 0 and
        // y > 0. Tested with a slack of 0 because the anchor is
        // strictly inside the top-left quadrant.
        for &seed in SEEDS {
            let mut rng = seeded(seed);
            let sections = build_random_map(&mut rng, 15);
            let c = sections[0].center;
            assert!(
                c.x < 0.0 && c.y > 0.0,
                "seed {seed}: section 0 center {c:?} not in top-left quadrant",
            );
        }
    }

    #[test]
    fn cell_centers_are_inside_their_own_cells() {
        // Lloyd-relaxed seed should still be inside its own cell — if
        // it weren't, the relaxation step would have nudged it back.
        for &seed in SEEDS {
            let mut rng = seeded(seed);
            let sections = build_random_map(&mut rng, 15);
            for s in &sections {
                assert!(
                    crate::map::point_in_polygon(s.center, &s.corners),
                    "seed {seed}: section {} center {:?} outside its own polygon",
                    s.id, s.center,
                );
            }
        }
    }
}

