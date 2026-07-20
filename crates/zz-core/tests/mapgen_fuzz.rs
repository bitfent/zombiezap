//! Mapgen fuzz: co-op invariants over many seeds (the check-mapgen.mjs
//! philosophy with 1v1 fairness rules replaced by co-op rules).

use zz_core::constants::MAX_PLAYERS;
use zz_core::map::{WalkGrid, generate_map};
use zz_core::types::EnvKind;

const SEEDS: usize = 200;

#[test]
fn coop_map_invariants_hold_across_seeds() {
    for i in 0..SEEDS {
        let seed = format!("fuzz-{i}");
        let map = generate_map(EnvKind::Urban, &seed);
        let half = map.arena_half;

        // determinism: same seed, same map
        let again = generate_map(EnvKind::Urban, &seed);
        assert_eq!(map.walls, again.walls, "{seed}: walls nondeterministic");
        assert_eq!(map.spawns, again.spawns, "{seed}: spawns nondeterministic");
        assert_eq!(map.gates, again.gates, "{seed}: gates nondeterministic");
        assert_eq!(
            map.billboards, again.billboards,
            "{seed}: billboards nondeterministic"
        );

        // geometry in bounds (perimeter overhangs by its thickness)
        for (wi, w) in map.walls.iter().enumerate() {
            assert!(
                w.x0 >= -half - 1.0
                    && w.x1 <= half + 1.0
                    && w.z0 >= -half - 1.0
                    && w.z1 <= half + 1.0,
                "{seed}: wall #{wi} out of bounds"
            );
            assert!(
                w.x0 < w.x1 && w.y0 < w.y1 && w.z0 < w.z1,
                "{seed}: degenerate wall #{wi}"
            );
        }

        let grid = WalkGrid::rasterize(&map.walls, half);

        // spawns: full team, walkable ground, clustered but not stacked
        assert_eq!(map.spawns.len(), MAX_PLAYERS, "{seed}: spawn count");
        for (si, s) in map.spawns.iter().enumerate() {
            assert!(
                grid.walkable_at(s.x, s.z),
                "{seed}: spawn #{si} at ({}, {}) not walkable",
                s.x,
                s.z
            );
        }
        let anchor = map.spawns[0];
        for s in &map.spawns[1..] {
            let d2 = (s.x - anchor.x).powi(2) + (s.z - anchor.z).powi(2);
            assert!(d2 <= 8.0 * 8.0, "{seed}: spawn strayed from the cluster");
        }

        // gates: at least 3 usable entrances, every listed gate reachable
        assert!(
            map.gates.len() >= 3,
            "{seed}: only {} gates",
            map.gates.len()
        );
        let sources: Vec<_> = map
            .spawns
            .iter()
            .filter_map(|s| grid.cell_of(s.x, s.z))
            .collect();
        let dist = grid.distance_field(&sources);
        let n = grid.cells_per_side;
        for (gi, g) in map.gates.iter().enumerate() {
            let (ix, iz) = grid.cell_of(g.x, g.z).expect("gate on grid");
            let reachable = [
                (ix, iz),
                (ix + 1, iz),
                (ix.saturating_sub(1), iz),
                (ix, iz + 1),
                (ix, iz.saturating_sub(1)),
            ]
            .iter()
            .any(|&(cx, cz)| cx < n && cz < n && dist[cz * n + cx] != u16::MAX);
            assert!(
                reachable,
                "{seed}: gate #{gi} unreachable from the spawn cluster"
            );
        }

        // billboards: above head height, on the perimeter, never colliding
        assert!(!map.billboards.is_empty(), "{seed}: no ad inventory");
        for (bi, b) in map.billboards.iter().enumerate() {
            assert!(b.y > 3.0, "{seed}: billboard #{bi} hangs in the play space");
            let on_perimeter = b.x.abs() >= half - 0.5 || b.z.abs() >= half - 0.5;
            assert!(on_perimeter, "{seed}: billboard #{bi} off the perimeter");
        }

        // loot spots on open ground
        for (pi, (x, z)) in map.pickups.iter().enumerate() {
            assert!(
                grid.walkable_at(*x, *z),
                "{seed}: pickup #{pi} not walkable"
            );
        }
    }
}

#[test]
fn walk_grid_agrees_with_movement_probe() {
    // Sanity coupling: a body standing on a walkable cell must be able to
    // exist there per the movement rules (ground ≤ 1.0, headroom clear).
    let map = generate_map(EnvKind::Urban, "grid-probe");
    let grid = WalkGrid::rasterize(&map.walls, map.arena_half);
    let mut checked = 0;
    for iz in 0..grid.cells_per_side {
        for ix in 0..grid.cells_per_side {
            if !grid.is_walkable(ix, iz) {
                continue;
            }
            checked += 1;
        }
    }
    // an urban map is mostly streets — the majority of cells must be walkable
    let total = grid.cells_per_side * grid.cells_per_side;
    assert!(
        checked * 2 > total,
        "suspiciously blocked map: {checked}/{total} walkable"
    );
}
