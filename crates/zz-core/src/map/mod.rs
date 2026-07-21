//! Seeded procedural urban arenas — port of
//! `legacy/packages/shared/src/mapgen.ts` (+ `arena.ts` constants).
//!
//! All mapgen internals are **f64** so generated maps are bit-identical to the
//! TypeScript original. Only [`Arena64::to_f32_walls`] narrows to `f32` AABBs
//! for the movement / collision layer.
//!
//! Note: `ARENA_HALF` here is the **legacy** 30.0 playfield half-size. Do not
//! use `crate::constants::ARENA_HALF` (the co-op 40.0 value).

mod coop;
mod desert;
mod grid;
mod mountain;
mod rome_eur;
mod sea;
mod urban;

pub use coop::{Billboard, GameMap, Gate, generate_map, is_destructible_cover};
pub use grid::WalkGrid;
pub use urban::{find_open_spot, generate_arena};

use crate::types::Aabb;

/// Axis-aligned box in mapgen space (f64, matches TS `Box`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Box64 {
    pub x0: f64,
    pub x1: f64,
    pub y0: f64,
    pub y1: f64,
    pub z0: f64,
    pub z1: f64,
}

/// Spawn point (f64, matches TS `Spawn`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spawn64 {
    pub x: f64,
    pub z: f64,
    pub yaw: f64,
}

/// Health-pickup ground position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pickup64 {
    pub x: f64,
    pub z: f64,
}

/// Explosive barrel prop — `wall_index` indexes into [`Arena64::walls`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Barrel64 {
    pub x: f64,
    pub z: f64,
    pub wall_index: usize,
}

/// Full arena produced by [`generate_arena`].
#[derive(Clone, Debug, PartialEq)]
pub struct Arena64 {
    pub seed: String,
    pub walls: Vec<Box64>,
    pub spawns: Vec<Spawn64>,
    pub pickups: Vec<Pickup64>,
    pub accent: u32,
    pub barrels: Vec<Barrel64>,
}

impl Arena64 {
    /// Narrow wall boxes to f32 AABBs for movement / collision.
    pub fn to_f32_walls(&self) -> Vec<Aabb> {
        self.walls
            .iter()
            .map(|b| Aabb {
                x0: b.x0 as f32,
                x1: b.x1 as f32,
                y0: b.y0 as f32,
                y1: b.y1 as f32,
                z0: b.z0 as f32,
                z1: b.z1 as f32,
            })
            .collect()
    }
}

// ── Legacy mapgen constants (f64; copied from mapgen.ts / arena.ts / constants.ts) ──

/// Legacy urban half-size (60×60 playfield). Private to this module.
pub(crate) const ARENA_HALF: f64 = 30.0;

pub(crate) const ACCENTS: [u32; 6] = [
    0x2e_e6_d6, // 0x2ee6d6
    0xff_33_55, 0xff_d2_3f, 0xb4_78_ff, 0x6c_ff_8a, 0xff_8a_3d,
];

pub(crate) const WALL_H: f64 = 6.0;
pub(crate) const T: f64 = 0.6;
pub(crate) const EDGE_MARGIN: f64 = 1.5;
pub(crate) const GAP: f64 = 1.6;
pub(crate) const SPAWN_CLEAR: f64 = 3.5;
pub(crate) const DOOR: f64 = 1.8;
pub(crate) const BWALL_T: f64 = 0.4;
pub(crate) const BUILD_H: f64 = 3.2;
pub(crate) const BUILD_H_TALL: f64 = 4.0;
pub(crate) const SILL_H: f64 = 1.3;
pub(crate) const HEAD_Y: f64 = 1.9;
pub(crate) const ROOF_T: f64 = 0.35;
pub(crate) const PARAPET_H: f64 = 0.45;
pub(crate) const STEP_RISE: f64 = 0.5;
pub(crate) const STEP_RUN: f64 = 0.8;
pub(crate) const PLATFORM_H: f64 = 2.0;
pub(crate) const TOWER_H: f64 = 3.0;
pub(crate) const DECK_TOP: f64 = PLATFORM_H;
pub(crate) const DECK_BOTTOM: f64 = 1.85;
pub(crate) const BARREL_SPAWN_CLEAR: f64 = 6.0;
pub(crate) const PLAYER_EYE: f64 = 1.55;

/// JS `Math.round` semantic: half toward +∞. Rust `f64::round` is half away
/// from zero and would break `snap` on negative .25 boundaries.
#[inline]
pub(crate) fn js_round(v: f64) -> f64 {
    (v + 0.5).floor()
}

#[inline]
pub(crate) fn snap(v: f64) -> f64 {
    js_round(v * 2.0) / 2.0
}
