//! The horde: flow-field navigation toward the players, local separation,
//! melee with a telegraphed windup. Zombies run the SAME step_body integrator
//! as players (per-kind speed) — stairs and step-up come for free.

use zz_core::constants::*;
use zz_core::map::WalkGrid;
use zz_core::types::{Aabb, Body, PlayerInput};

/// Wire states (snapshot `state` byte drives client anims).
/// Low 7 bits = anim state; high bit [`ZS_FRENZY_BIT`] = frenzy walker.
pub const ZS_WALK: u8 = 0;
pub const ZS_ATTACK: u8 = 1;
#[allow(dead_code)] // wire state reserved for hit-stagger (client anims)
pub const ZS_STAGGER: u8 = 2;

#[inline]
pub fn anim_state(state: u8) -> u8 {
    state & !ZS_FRENZY_BIT
}

#[inline]
pub fn with_frenzy(state: u8, frenzy: bool) -> u8 {
    if frenzy {
        (state & !ZS_FRENZY_BIT) | ZS_FRENZY_BIT
    } else {
        state & !ZS_FRENZY_BIT
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ZombieKind {
    Walker,
    Runner,
    Brute,
}

impl ZombieKind {
    pub fn stats(self) -> (f32, f32, f32, u32) {
        match self {
            ZombieKind::Walker => ZOMBIE_WALKER,
            ZombieKind::Runner => ZOMBIE_RUNNER,
            ZombieKind::Brute => ZOMBIE_BRUTE,
        }
    }
    pub fn wire(self) -> u8 {
        match self {
            ZombieKind::Walker => 0,
            ZombieKind::Runner => 1,
            ZombieKind::Brute => 2,
        }
    }
}

pub struct Zombie {
    pub id: u16,
    pub kind: ZombieKind,
    pub body: Body,
    pub yaw: f32,
    pub health: f32,
    pub state: u8,
    /// Ticks until the pending attack lands (while in ZS_ATTACK).
    pub windup_left: u32,
    /// Cooldown ticks until the next attack may start.
    pub cooldown_left: u32,
    /// Slot of the player being attacked when the windup ends.
    #[allow(dead_code)] // read by the M4 client half (attack telegraphs)
    pub target_slot: u8,
    /// Counts toward the current wave's clear condition.
    pub from_wave: bool,
    /// Runner flank waypoint (xz); cleared on arrival / close pursuit.
    pub flank_xz: Option<(f32, f32)>,
    /// Brute cover-smash cooldown.
    pub smash_cooldown: u32,
    /// Frenzy walker: +speed, glowing eyes (wire bit on `state`).
    pub frenzy: bool,
}

/// Flow field: per-cell unit direction toward the nearest alive player,
/// rebuilt every FLOWFIELD_REBUILD_TICKS from the shared walk grid.
pub struct FlowField {
    dirs: Vec<(f32, f32)>, // per cell; (0,0) = unreachable/no target
    n: usize,
}

impl FlowField {
    pub fn empty() -> Self {
        FlowField {
            dirs: Vec::new(),
            n: 0,
        }
    }

    /// True until the first rebuild against a live walk grid.
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    pub fn rebuild(grid: &WalkGrid, player_positions: &[(f32, f32)]) -> Self {
        let sources: Vec<(usize, usize)> = player_positions
            .iter()
            .filter_map(|&(x, z)| grid.cell_of(x, z))
            .collect();
        let dist = grid.distance_field(&sources);
        let n = grid.cells_per_side;
        let mut dirs = vec![(0.0f32, 0.0f32); n * n];
        for iz in 0..n {
            for ix in 0..n {
                let here = dist[iz * n + ix];
                if here == u16::MAX || here == 0 {
                    continue;
                }
                // steepest-descent neighbor (8-way)
                let mut best = here;
                let mut best_d = (0.0f32, 0.0f32);
                for (dx, dz) in NEIGHBORS8 {
                    let (nx, nz) = (ix as isize + dx, iz as isize + dz);
                    if nx < 0 || nz < 0 || nx as usize >= n || nz as usize >= n {
                        continue;
                    }
                    let (nx, nz) = (nx as usize, nz as usize);
                    if !grid.is_walkable(nx, nz) {
                        continue;
                    }
                    // diagonal moves must not cut wall corners
                    if dx != 0 && dz != 0 {
                        let side_a = grid.is_walkable((ix as isize + dx) as usize, iz);
                        let side_b = grid.is_walkable(ix, (iz as isize + dz) as usize);
                        if !side_a || !side_b {
                            continue;
                        }
                    }
                    let d = dist[nz * n + nx];
                    if d < best {
                        best = d;
                        let len = ((dx * dx + dz * dz) as f32).sqrt();
                        best_d = (dx as f32 / len, dz as f32 / len);
                    }
                }
                dirs[iz * n + ix] = best_d;
            }
        }
        FlowField { dirs, n }
    }

    pub fn dir_at(&self, grid: &WalkGrid, x: f32, z: f32) -> (f32, f32) {
        if self.n == 0 {
            return (0.0, 0.0);
        }
        grid.cell_of(x, z)
            .map_or((0.0, 0.0), |(ix, iz)| self.dirs[iz * self.n + ix])
    }
}

const NEIGHBORS8: [(isize, isize); 8] = [
    (-1, -1),
    (0, -1),
    (1, -1),
    (-1, 0),
    (1, 0),
    (-1, 1),
    (0, 1),
    (1, 1),
];

/// One zombie's steering + movement + attack state machine for one tick.
/// Returns Some((target_slot, damage)) when a windup completes ON a player
/// still in range — the room applies the damage.
#[allow(clippy::too_many_arguments)]
pub fn step_zombie(
    z: &mut Zombie,
    grid: &WalkGrid,
    flow: &FlowField,
    players: &[(u8, f32, f32, bool)], // (slot, x, z, alive)
    separation: (f32, f32),
    walls: &[Aabb],
    arena_half: f32,
) -> Option<(u8, f32)> {
    let (base_speed, _hp, dmg, _cost) = z.kind.stats();
    let speed = if z.frenzy {
        base_speed * FRENZY_SPEED_MUL
    } else {
        base_speed
    };

    if z.cooldown_left > 0 {
        z.cooldown_left -= 1;
    }
    if z.smash_cooldown > 0 {
        z.smash_cooldown -= 1;
    }

    // nearest alive player (squared distances — cheap)
    let mut nearest: Option<(u8, f32, f32, f32)> = None; // (slot, x, z, d2)
    for &(slot, px, pz, alive) in players {
        if !alive {
            continue;
        }
        let d2 = (px - z.body.x).powi(2) + (pz - z.body.z).powi(2);
        if nearest.is_none_or(|(_, _, _, bd2)| d2 < bd2) {
            nearest = Some((slot, px, pz, d2));
        }
    }
    let Some((tslot, tx, tz, d2)) = nearest else {
        z.state = with_frenzy(ZS_WALK, z.frenzy);
        return None; // nobody left to eat
    };

    // attack state machine (anim bits only)
    if anim_state(z.state) == ZS_ATTACK {
        if z.windup_left > 0 {
            z.windup_left -= 1;
            // face the victim through the windup; no movement (telegraph)
            z.yaw = yaw_toward(z.body.x, z.body.z, tx, tz);
            return None;
        }
        z.state = with_frenzy(ZS_WALK, z.frenzy);
        z.cooldown_left = ZOMBIE_ATTACK_COOLDOWN_TICKS;
        // the bite lands only if the victim is STILL in reach
        if d2 <= (ZOMBIE_ATTACK_RANGE * 1.25).powi(2) {
            return Some((tslot, dmg));
        }
        return None;
    }

    if d2 <= ZOMBIE_ATTACK_RANGE * ZOMBIE_ATTACK_RANGE && z.cooldown_left == 0 {
        z.state = with_frenzy(ZS_ATTACK, z.frenzy);
        z.windup_left = ZOMBIE_ATTACK_WINDUP_TICKS;
        z.yaw = yaw_toward(z.body.x, z.body.z, tx, tz);
        return None;
    }

    // Runner flank: bias pathing toward offset waypoint until close.
    if let Some((fx, fz)) = z.flank_xz {
        let fd2 = (fx - z.body.x).powi(2) + (fz - z.body.z).powi(2);
        if fd2 <= RUNNER_FLANK_ARRIVE_M * RUNNER_FLANK_ARRIVE_M
            || d2 <= ZOMBIE_PURSUE_RANGE * ZOMBIE_PURSUE_RANGE
        {
            z.flank_xz = None;
        }
    }

    // steering: direct pursuit in close range, flank target, or flow field
    let (mut dx, mut dz) = if d2 <= ZOMBIE_PURSUE_RANGE * ZOMBIE_PURSUE_RANGE {
        let len = d2.sqrt().max(1e-3);
        ((tx - z.body.x) / len, (tz - z.body.z) / len)
    } else if let Some((fx, fz)) = z.flank_xz {
        let fd2 = (fx - z.body.x).powi(2) + (fz - z.body.z).powi(2);
        let len = fd2.sqrt().max(1e-3);
        ((fx - z.body.x) / len, (fz - z.body.z) / len)
    } else {
        flow.dir_at(grid, z.body.x, z.body.z)
    };

    // separation nudge so the horde doesn't stack into one column
    dx += separation.0 * 0.6;
    dz += separation.1 * 0.6;

    let len = (dx * dx + dz * dz).sqrt();
    if len < 1e-3 {
        z.state = with_frenzy(ZS_WALK, z.frenzy);
        return None; // unreachable pocket — idle (director avoids these gates)
    }
    let (dx, dz) = (dx / len, dz / len);

    z.yaw = libm::atan2f(-dx, -dz); // forward = (-sin yaw, -cos yaw)
    let input = PlayerInput {
        forward: true,
        yaw: z.yaw,
        ..Default::default()
    };
    zz_core::movement::step_body(&mut z.body, &input, TICK_DT, speed, walls, arena_half);
    z.state = with_frenzy(ZS_WALK, z.frenzy);
    None
}

pub fn yaw_toward(from_x: f32, from_z: f32, to_x: f32, to_z: f32) -> f32 {
    // forward = (-sin yaw, -cos yaw); solve for yaw pointing at the target
    libm::atan2f(-(to_x - from_x), -(to_z - from_z))
}

/// 2 m-bucket spatial hash for neighbor separation. Rebuilt each tick into
/// scratch vecs (indices only, no per-tick allocation growth after warmup).
pub struct SpatialHash {
    buckets: std::collections::HashMap<(i32, i32), Vec<usize>>,
}

const BUCKET: f32 = 2.0;

impl SpatialHash {
    pub fn new() -> Self {
        SpatialHash {
            buckets: std::collections::HashMap::new(),
        }
    }

    pub fn rebuild(&mut self, positions: impl Iterator<Item = (f32, f32)>) {
        for v in self.buckets.values_mut() {
            v.clear();
        }
        for (i, (x, z)) in positions.enumerate() {
            let key = ((x / BUCKET).floor() as i32, (z / BUCKET).floor() as i32);
            self.buckets.entry(key).or_default().push(i);
        }
    }

    /// Repulsion vector away from up to 3 neighbors within 0.8 m.
    pub fn separation(&self, me: usize, x: f32, z: f32, all: &[(f32, f32)]) -> (f32, f32) {
        let (bx, bz) = ((x / BUCKET).floor() as i32, (z / BUCKET).floor() as i32);
        let mut sx = 0.0f32;
        let mut sz = 0.0f32;
        let mut found = 0;
        'scan: for kx in bx - 1..=bx + 1 {
            for kz in bz - 1..=bz + 1 {
                let Some(bucket) = self.buckets.get(&(kx, kz)) else {
                    continue;
                };
                for &j in bucket {
                    if j == me {
                        continue;
                    }
                    let (ox, oz) = all[j];
                    let d2 = (x - ox).powi(2) + (z - oz).powi(2);
                    if d2 < 0.8 * 0.8 && d2 > 1e-6 {
                        let d = d2.sqrt();
                        sx += (x - ox) / d * (1.0 - d / 0.8);
                        sz += (z - oz) / d * (1.0 - d / 0.8);
                        found += 1;
                        if found >= 3 {
                            break 'scan;
                        }
                    }
                }
            }
        }
        (sx, sz)
    }
}
