//! Walkability grid: a 1 m rasterization of the box world, shared by the
//! server's zombie flow-field AI and the mapgen fuzz tests (one definition of
//! "walkable" — the AI and the reachability invariants can't disagree).

use crate::types::Aabb;

/// Ground-level walkability. A cell is walkable when its standable surface is
/// no higher than `MAX_FLOOR` and there is `CLEARANCE` of open space above it
/// (fits the 1.7 m body + headroom). Roofs/perches above 1 m are deliberately
/// NOT in the grid — the accepted v1 limit; stairs still work because each
/// step is ≤ 1 m and bodies use step-up physics, while the flow field routes
/// along the ground.
pub struct WalkGrid {
    pub half: f32,
    pub cells_per_side: usize,
    walkable: Vec<bool>,
}

const CELL: f32 = 1.0;
const MAX_FLOOR: f32 = 1.0;
const CLEARANCE: f32 = 1.8;

impl WalkGrid {
    pub fn rasterize(walls: &[Aabb], half: f32) -> Self {
        let cells_per_side = ((half * 2.0) / CELL).ceil() as usize;
        let mut walkable = vec![false; cells_per_side * cells_per_side];
        for iz in 0..cells_per_side {
            for ix in 0..cells_per_side {
                let x = -half + (ix as f32 + 0.5) * CELL;
                let z = -half + (iz as f32 + 0.5) * CELL;
                walkable[iz * cells_per_side + ix] = point_walkable(walls, x, z);
            }
        }
        WalkGrid {
            half,
            cells_per_side,
            walkable,
        }
    }

    pub fn cell_of(&self, x: f32, z: f32) -> Option<(usize, usize)> {
        let ix = ((x + self.half) / CELL).floor();
        let iz = ((z + self.half) / CELL).floor();
        if ix < 0.0 || iz < 0.0 {
            return None;
        }
        let (ix, iz) = (ix as usize, iz as usize);
        (ix < self.cells_per_side && iz < self.cells_per_side).then_some((ix, iz))
    }

    pub fn is_walkable(&self, ix: usize, iz: usize) -> bool {
        ix < self.cells_per_side
            && iz < self.cells_per_side
            && self.walkable[iz * self.cells_per_side + ix]
    }

    pub fn walkable_at(&self, x: f32, z: f32) -> bool {
        self.cell_of(x, z)
            .is_some_and(|(ix, iz)| self.is_walkable(ix, iz))
    }

    /// Multi-source BFS distance field (in cells, 4-connected). `u16::MAX`
    /// means unreachable. This is the exact structure the zombie flow field
    /// uses; the fuzz tests use it for reachability invariants.
    pub fn distance_field(&self, sources: &[(usize, usize)]) -> Vec<u16> {
        let n = self.cells_per_side;
        let mut dist = vec![u16::MAX; n * n];
        let mut queue = std::collections::VecDeque::new();
        for &(ix, iz) in sources {
            if self.is_walkable(ix, iz) {
                dist[iz * n + ix] = 0;
                queue.push_back((ix, iz));
            }
        }
        while let Some((ix, iz)) = queue.pop_front() {
            let d = dist[iz * n + ix];
            for (nx, nz) in neighbors4(ix, iz, n) {
                if self.is_walkable(nx, nz) && dist[nz * n + nx] == u16::MAX {
                    dist[nz * n + nx] = d + 1;
                    queue.push_back((nx, nz));
                }
            }
        }
        dist
    }
}

fn neighbors4(ix: usize, iz: usize, n: usize) -> impl Iterator<Item = (usize, usize)> {
    [
        (ix.wrapping_sub(1), iz),
        (ix + 1, iz),
        (ix, iz.wrapping_sub(1)),
        (ix, iz + 1),
    ]
    .into_iter()
    .filter(move |&(x, z)| x < n && z < n)
}

fn point_walkable(walls: &[Aabb], x: f32, z: f32) -> bool {
    // standable floor = highest box top ≤ MAX_FLOOR covering the point
    let mut floor = 0.0f32;
    for b in walls {
        if x > b.x0 && x < b.x1 && z > b.z0 && z < b.z1 && b.y1 <= MAX_FLOOR && b.y1 > floor {
            floor = b.y1;
        }
    }
    // blocked if anything intrudes into the body space above the floor
    for b in walls {
        if x > b.x0 && x < b.x1 && z > b.z0 && z < b.z1 {
            let intrudes = b.y1 > floor + 0.05 && b.y0 < floor + CLEARANCE;
            if intrudes {
                return false;
            }
        }
    }
    true
}
