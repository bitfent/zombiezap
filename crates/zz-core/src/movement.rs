//! Deterministic movement with vertical collision + step-up, shared by server
//! simulation AND client prediction — the same function body runs on both
//! sides, so the predicted camera and the authoritative position agree.
//! Port of legacy/packages/shared/src/movement.ts, generalized so one function
//! drives players (PLAYER_SPEED) and zombies (per-kind speed).
//!
//! Box rules (everything is an AABB):
//!   * a box BLOCKS horizontal movement only if its top is more than STEP_UP
//!     above your feet (a real wall) AND its bottom is below your head — so you
//!     pass UNDER decks/window-headers and step OVER low curbs/stairs
//!   * the surface you stand on is the highest box-top under your feet within
//!     STEP_UP of your current feet height — walk up stairs, fall off ledges

use crate::constants::{GRAVITY, JUMP_VELOCITY, PLAYER_RADIUS, STEP_UP};
use crate::types::{Aabb, Body, PlayerInput};

/// Body clearance above feet (eye is at 1.55).
const HEAD: f32 = 1.7;
const EPS: f32 = 1e-3;

fn footprint(x: f32, z: f32, b: &Aabb) -> bool {
    x > b.x0 - PLAYER_RADIUS
        && x < b.x1 + PLAYER_RADIUS
        && z > b.z0 - PLAYER_RADIUS
        && z < b.z1 + PLAYER_RADIUS
}

/// Highest standable surface at (x,z), given current feet height.
fn ground_at(x: f32, z: f32, feet_y: f32, walls: &[Aabb]) -> f32 {
    let mut g = 0.0f32;
    for b in walls {
        if b.y1 <= feet_y + STEP_UP + EPS && b.y1 > g && footprint(x, z, b) {
            g = b.y1;
        }
    }
    g
}

/// Does a wall block horizontal movement into (x,z) at this feet height?
fn blocked_xz(x: f32, z: f32, feet_y: f32, walls: &[Aabb]) -> bool {
    let head = feet_y + HEAD;
    for b in walls {
        if b.y1 <= feet_y + STEP_UP + EPS {
            continue; // low — step over/onto it
        }
        if b.y0 >= head {
            continue; // high — pass under it
        }
        if footprint(x, z, b) {
            return true;
        }
    }
    false
}

/// True when a body at `(x, z)` with feet on the ground would be inside solid
/// geometry (cannot take any step). Used by the director spawn snap so wave
/// zombies are never born inside a wall that the 1 m walk-grid still marks
/// walkable at the cell centre.
pub fn body_blocked_at(x: f32, z: f32, walls: &[Aabb]) -> bool {
    blocked_xz(x, z, 0.0, walls)
}

/// Advance one body by one input over dt seconds. `speed` is the body's move
/// speed (players and zombies share this integrator), `arena_half` the hard
/// clamp. Mutates `b`.
pub fn step_body(
    b: &mut Body,
    input: &PlayerInput,
    dt: f32,
    speed: f32,
    walls: &[Aabb],
    arena_half: f32,
) {
    let mut dx = 0.0f32;
    let mut dz = 0.0f32;
    let sin = libm::sinf(input.yaw);
    let cos = libm::cosf(input.yaw);
    if input.forward {
        dx -= sin;
        dz -= cos;
    }
    if input.backward {
        dx += sin;
        dz += cos;
    }
    if input.left {
        dx -= cos;
        dz += sin;
    }
    if input.right {
        dx += cos;
        dz -= sin;
    }
    let len = libm::sqrtf(dx * dx + dz * dz);
    if len > 0.0 {
        dx = (dx / len) * speed * dt;
        dz = (dz / len) * speed * dt;
        if !blocked_xz(b.x + dx, b.z, b.y, walls) {
            b.x += dx;
        }
        if !blocked_xz(b.x, b.z + dz, b.y, walls) {
            b.z += dz;
        }
    }

    let ground = ground_at(b.x, b.z, b.y, walls);

    // step up onto a higher surface we just walked into (stairs, curbs, crates)
    if b.on_ground && ground > b.y && ground - b.y <= STEP_UP + EPS {
        b.y = ground;
    }

    if input.jump && b.on_ground {
        b.vy = JUMP_VELOCITY;
        b.on_ground = false;
    }

    if !b.on_ground || b.vy != 0.0 {
        b.y += b.vy * dt;
        b.vy -= GRAVITY * dt;
        if b.y <= ground {
            b.y = ground;
            b.vy = 0.0;
            b.on_ground = true;
        }
    } else if ground < b.y - EPS {
        // walked off a ledge — start falling
        b.on_ground = false;
    } else {
        b.y = ground;
    }

    // hard clamp inside the arena
    let lim = arena_half - PLAYER_RADIUS;
    b.x = b.x.clamp(-lim, lim);
    b.z = b.z.clamp(-lim, lim);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{PLAYER_SPEED, TICK_DT};

    const HALF: f32 = 40.0;

    fn fwd(yaw: f32) -> PlayerInput {
        PlayerInput {
            forward: true,
            yaw,
            ..Default::default()
        }
    }

    /// Walk toward -z into a box spanning z in [-3.5, -3.0].
    fn wall(h: f32) -> Aabb {
        Aabb {
            x0: -5.0,
            x1: 5.0,
            y0: 0.0,
            y1: h,
            z0: -3.5,
            z1: -3.0,
        }
    }

    fn walk(b: &mut Body, input: &PlayerInput, steps: usize, walls: &[Aabb]) {
        for _ in 0..steps {
            step_body(b, input, TICK_DT, PLAYER_SPEED, walls, HALF);
        }
    }

    #[test]
    fn steps_onto_low_box() {
        // 16 steps × 0.2 m puts the body at z ≈ -3.2 — mid-box, on top of it.
        let mut b = Body::at(0.0, 0.0);
        walk(&mut b, &fwd(0.0), 16, &[wall(0.5)]);
        assert!(
            b.y == 0.5,
            "expected on top of the box, got y={} z={}",
            b.y,
            b.z
        );
    }

    #[test]
    fn blocked_by_tall_wall() {
        let mut b = Body::at(0.0, 0.0);
        walk(&mut b, &fwd(0.0), 60, &[wall(1.2)]);
        assert!(b.z > -3.5 + -0.0 - PLAYER_RADIUS - 0.05 && b.y == 0.0);
        assert!(
            b.z >= -3.0 + PLAYER_RADIUS - 1e-3,
            "walked through the wall: z={}",
            b.z
        );
    }

    #[test]
    fn passes_under_high_deck() {
        // deck bottom at 1.85 > HEAD? no: HEAD=1.7, deck y0=1.85 → passes under.
        let deck = Aabb {
            x0: -5.0,
            x1: 5.0,
            y0: 1.85,
            y1: 2.0,
            z0: -3.5,
            z1: -3.0,
        };
        let mut b = Body::at(0.0, 0.0);
        walk(&mut b, &fwd(0.0), 60, &[deck]);
        assert!(b.z < -3.5, "should have walked under the deck, z={}", b.z);
        assert_eq!(b.y, 0.0);
    }

    #[test]
    fn window_slit_blocks_body() {
        // sill 0..1.3 + header 1.9..3.2 with a 1.3–1.9 slit: capsule can't pass.
        let sill = Aabb {
            x0: -5.0,
            x1: 5.0,
            y0: 0.0,
            y1: 1.3,
            z0: -3.5,
            z1: -3.0,
        };
        let header = Aabb {
            x0: -5.0,
            x1: 5.0,
            y0: 1.9,
            y1: 3.2,
            z0: -3.5,
            z1: -3.0,
        };
        let mut b = Body::at(0.0, 0.0);
        walk(&mut b, &fwd(0.0), 60, &[sill, header]);
        assert!(
            b.z >= -3.0 + PLAYER_RADIUS - 1e-3,
            "squeezed through the slit: z={}",
            b.z
        );
    }

    #[test]
    fn falls_off_ledge() {
        let ledge = Aabb {
            x0: -5.0,
            x1: 5.0,
            y0: 0.0,
            y1: 0.5,
            z0: -0.5,
            z1: 0.5,
        };
        let mut b = Body {
            x: 0.0,
            y: 0.5,
            z: 0.0,
            vy: 0.0,
            on_ground: true,
        };
        walk(&mut b, &fwd(0.0), 40, &[ledge]);
        assert_eq!(b.y, 0.0, "should have landed on the floor");
    }

    #[test]
    fn jump_apex_matches_physics() {
        let mut b = Body::at(0.0, 0.0);
        let jump = PlayerInput {
            jump: true,
            ..Default::default()
        };
        step_body(&mut b, &jump, TICK_DT, PLAYER_SPEED, &[], HALF);
        let mut apex = 0.0f32;
        for _ in 0..60 {
            step_body(
                &mut b,
                &PlayerInput::default(),
                TICK_DT,
                PLAYER_SPEED,
                &[],
                HALF,
            );
            apex = apex.max(b.y);
        }
        let ideal = JUMP_VELOCITY * JUMP_VELOCITY / (2.0 * GRAVITY); // 1.225
        assert!((apex - ideal).abs() < 0.15, "apex {apex} vs ideal {ideal}");
        assert!(b.on_ground && b.y == 0.0);
    }

    #[test]
    fn clamped_inside_arena() {
        let mut b = Body::at(0.0, 0.0);
        walk(&mut b, &fwd(core::f32::consts::PI), 1000, &[]); // toward +z forever
        assert!(b.z <= HALF - PLAYER_RADIUS + 1e-3);
    }
}
