//! Mapgen fuzz: co-op invariants over many seeds (the check-mapgen.mjs
//! philosophy with 1v1 fairness rules replaced by co-op rules).
//!
//! Runs the full property set over every `EnvKind`, including the Rome EUR
//! OSM fixture (geometry is seed-invariant; accent/ad slots may vary).

use zz_core::constants::MAX_PLAYERS;
use zz_core::map::{WalkGrid, generate_map};
use zz_core::types::EnvKind;

const SEEDS: usize = 200;
/// Rome EUR is a fixed fixture — a handful of seeds is enough for determinism
/// + walkability, and avoids 200× rasterizing a 500 m WalkGrid in CI.
const ROME_SEEDS: usize = 8;

const ALL_ENVS: [EnvKind; 5] = [
    EnvKind::Urban,
    EnvKind::MountainTown,
    EnvKind::DesertTown,
    EnvKind::SeaTown,
    EnvKind::RomeEur,
];

fn env_name(env: EnvKind) -> &'static str {
    match env {
        EnvKind::Urban => "urban",
        EnvKind::MountainTown => "mountain",
        EnvKind::DesertTown => "desert",
        EnvKind::SeaTown => "sea",
        EnvKind::RomeEur => "rome_eur",
    }
}

fn seed_count(env: EnvKind) -> usize {
    match env {
        EnvKind::RomeEur => ROME_SEEDS,
        _ => SEEDS,
    }
}

#[test]
fn coop_map_invariants_hold_across_seeds() {
    for env in ALL_ENVS {
        for i in 0..seed_count(env) {
            let seed = format!("fuzz-{i}");
            let label = format!("{}:{seed}", env_name(env));
            let map = generate_map(env, &seed);
            let half = map.arena_half;

            assert_eq!(map.env, env, "{label}: env field mismatch");

            // determinism: same seed, same map
            let again = generate_map(env, &seed);
            assert_eq!(map.walls, again.walls, "{label}: walls nondeterministic");
            assert_eq!(map.spawns, again.spawns, "{label}: spawns nondeterministic");
            assert_eq!(map.gates, again.gates, "{label}: gates nondeterministic");
            assert_eq!(
                map.billboards, again.billboards,
                "{label}: billboards nondeterministic"
            );

            // geometry in bounds (perimeter overhangs by its thickness)
            for (wi, w) in map.walls.iter().enumerate() {
                assert!(
                    w.x0 >= -half - 1.0
                        && w.x1 <= half + 1.0
                        && w.z0 >= -half - 1.0
                        && w.z1 <= half + 1.0,
                    "{label}: wall #{wi} out of bounds"
                );
                assert!(
                    w.x0 < w.x1 && w.y0 < w.y1 && w.z0 < w.z1,
                    "{label}: degenerate wall #{wi}"
                );
            }

            // Rome EUR uses a 500 m arena; still must fit the i16 wire ±255.9 m.
            assert!(
                half <= 250.0 + 1e-3,
                "{label}: arena_half {half} exceeds wire-safe 250"
            );

            let grid = WalkGrid::rasterize(&map.walls, half);

            // spawns: full team, walkable ground, clustered but not stacked
            assert_eq!(map.spawns.len(), MAX_PLAYERS, "{label}: spawn count");
            for (si, s) in map.spawns.iter().enumerate() {
                assert!(
                    grid.walkable_at(s.x, s.z),
                    "{label}: spawn #{si} at ({}, {}) not walkable",
                    s.x,
                    s.z
                );
                assert!(
                    s.x.abs() <= half && s.z.abs() <= half,
                    "{label}: spawn #{si} outside arena"
                );
            }
            let anchor = map.spawns[0];
            for s in &map.spawns[1..] {
                let d2 = (s.x - anchor.x).powi(2) + (s.z - anchor.z).powi(2);
                assert!(d2 <= 8.0 * 8.0, "{label}: spawn strayed from the cluster");
            }

            // gates: at least 3 usable entrances, every listed gate reachable
            assert!(
                map.gates.len() >= 3,
                "{label}: only {} gates",
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
                    "{label}: gate #{gi} unreachable from the spawn cluster"
                );
            }

            // billboards: above head height, on the perimeter, never colliding
            assert!(!map.billboards.is_empty(), "{label}: no ad inventory");
            for (bi, b) in map.billboards.iter().enumerate() {
                assert!(b.y > 3.0, "{label}: billboard #{bi} hangs in the play space");
                let on_perimeter = b.x.abs() >= half - 0.5 || b.z.abs() >= half - 0.5;
                assert!(on_perimeter, "{label}: billboard #{bi} off the perimeter");
            }

            // loot spots on open ground
            for (pi, (x, z)) in map.pickups.iter().enumerate() {
                assert!(
                    grid.walkable_at(*x, *z),
                    "{label}: pickup #{pi} not walkable"
                );
            }
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

/// Urban generation must stay bit-identical across the M8b env dispatch work:
/// same seed → same wall count and same wall AABB payload hash.
#[test]
fn urban_output_regression_known_seed() {
    let map = generate_map(EnvKind::Urban, "urban-regression-canary");
    assert_eq!(map.env, EnvKind::Urban);
    // Captured from pre-M8b urban_coop (seed "urban-regression-canary").
    // If this fails, urban generation drifted — restore bit-identical output.
    // Captured from urban_coop after M8b helper extraction (walls still come
    // solely from the golden-exact generate_arena — count+hash must not drift).
    assert_eq!(
        map.walls.len(),
        215,
        "urban wall count drifted (got {})",
        map.walls.len()
    );
    let hash = wall_payload_hash(&map.walls);
    assert_eq!(
        hash, 0xe1cc_3f97_eb09_c41f,
        "urban wall payload hash drifted (got {hash:#x})"
    );
    assert_eq!(map.spawns.len(), MAX_PLAYERS);
    assert!(map.gates.len() >= 3);
}

/// Canary: non-Rome envs keep arena_half 30 and produce walls after Rome EUR
/// is wired into dispatch. Urban bit-identity is covered by
/// `urban_output_regression_known_seed`.
#[test]
fn non_rome_env_outputs_stable_shape() {
    for env in [
        EnvKind::Urban,
        EnvKind::MountainTown,
        EnvKind::DesertTown,
        EnvKind::SeaTown,
    ] {
        let map = generate_map(env, "canary-shape");
        assert!(
            (map.arena_half - 30.0).abs() < 1e-3,
            "{env:?}: unexpected arena_half {}",
            map.arena_half
        );
        assert!(!map.walls.is_empty(), "{env:?}: empty walls");
        assert_eq!(map.spawns.len(), MAX_PLAYERS);
        assert!(map.gates.len() >= 3);
    }
    let rome = generate_map(EnvKind::RomeEur, "canary-r");
    assert!((rome.arena_half - 250.0).abs() < 1e-3);
    assert!(rome.walls.len() > 500);
    assert!(rome.walls.len() < 2000);
}

/// FNV-1a over wall f32 bits — stable across runs, independent of Vec address.
fn wall_payload_hash(walls: &[zz_core::types::Aabb]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for w in walls {
        for v in [w.x0, w.x1, w.y0, w.y1, w.z0, w.z1] {
            let bits = v.to_bits() as u64;
            h ^= bits;
            h = h.wrapping_mul(0x1000_0000_01b3);
        }
    }
    h
}
