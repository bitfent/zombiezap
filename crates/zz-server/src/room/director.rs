//! Spawn director: an endless, escalating points budget spent on zombies at
//! gates the players can't see. Tick-counted (pause-safe), deterministic per
//! room seed.

use super::zombies::{Zombie, ZombieKind, yaw_toward};
use zz_core::constants::*;
use zz_core::map::{GameMap, Gate};
use zz_core::math::{Vec3, dir_from_angles, nearest_wall_t};
use zz_core::rng::Mulberry32;
use zz_core::types::Body;

pub struct Director {
    points: f32,
    rng: Mulberry32,
    next_id: u16,
    /// Test/tuning override: multiplies the budget rate (ZZ_DIRECTOR_RATE).
    rate_mul: f32,
}

impl Director {
    pub fn new(seed: &str) -> Self {
        let rate_mul = std::env::var("ZZ_DIRECTOR_RATE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1.0);
        Director {
            points: 0.0,
            rng: Mulberry32::from_seed(&format!("{seed}|director")),
            next_id: 1,
            rate_mul,
        }
    }

    /// Difficulty stage byte for the HUD/music (0..=255, one per minute-ish).
    pub fn difficulty(tick: u32) -> u8 {
        (tick / (60 * TICK_RATE)).min(255) as u8
    }

    /// Accrue budget and spawn while it lasts. Called once per (unpaused) tick.
    pub fn step(
        &mut self,
        tick: u32,
        map: &GameMap,
        zombies: &mut Vec<Zombie>,
        players: &[(f32, f32, f32, f32, bool)], // (x, eye_y, z, yaw, alive)
    ) {
        let minutes = tick as f32 / (60.0 * TICK_RATE as f32);
        // slow sine pulse for horde/lull rhythm, tick-derived (pause-safe)
        let phase =
            (tick as f32 / TICK_RATE as f32) / DIRECTOR_PULSE_PERIOD_SEC * core::f32::consts::TAU;
        let pulse = 1.0 + DIRECTOR_PULSE_AMPLITUDE * libm::sinf(phase);
        // Map-size scale: big arenas (Rome) get a higher budget so the horde
        // actually forms; approach-distance clamp below does first-contact.
        let size_scale = director_rate_scale(map.arena_half);
        let rate = DIRECTOR_BASE_POINTS_PER_SEC
            * (1.0 + minutes * DIRECTOR_RAMP_PER_MIN)
            * pulse
            * size_scale;
        self.points += rate * self.rate_mul * TICK_DT;

        let approach = director_approach_dist(map.arena_half);

        while zombies.len() < MAX_ZOMBIES {
            let kind = self.pick_kind(minutes);
            let cost = kind.stats().3 as f32;
            if self.points < cost {
                break;
            }
            let Some(gate) = self.pick_gate(map, players) else {
                break;
            };
            self.points -= cost;
            let (jx, jz) = (
                (self.rng.next() as f32 - 0.5) * 1.5,
                (self.rng.next() as f32 - 0.5) * 1.5,
            );
            let (gx, gz) = (gate.x + jx, gate.z + jz);
            // Pull spawn toward nearest alive player so walkers threaten in
            // ~20 s even on 500 m arenas (gates alone can be 200 m away).
            let (x, z) = Self::approach_spawn(gx, gz, players, approach);
            let (cx, cz) = Self::alive_centroid(players);
            zombies.push(Zombie {
                id: self.next_id,
                kind,
                body: Body::at(
                    x.clamp(-map.arena_half + 1.0, map.arena_half - 1.0),
                    z.clamp(-map.arena_half + 1.0, map.arena_half - 1.0),
                ),
                yaw: yaw_toward(x, z, cx, cz),
                health: kind.stats().1,
                state: 0,
                windup_left: 0,
                cooldown_left: 0,
                target_slot: 0,
            });
            self.next_id = self.next_id.wrapping_add(1).max(1);
        }
    }

    fn alive_centroid(players: &[(f32, f32, f32, f32, bool)]) -> (f32, f32) {
        let mut cx = 0.0f32;
        let mut cz = 0.0f32;
        let mut n = 0u32;
        for p in players.iter().filter(|p| p.4) {
            cx += p.0;
            cz += p.2;
            n += 1;
        }
        if n == 0 {
            (0.0, 0.0)
        } else {
            (cx / n as f32, cz / n as f32)
        }
    }

    /// If the gate is farther than `approach` from every alive player, place
    /// the spawn on the segment gate→nearest-player at distance `approach`.
    fn approach_spawn(
        gx: f32,
        gz: f32,
        players: &[(f32, f32, f32, f32, bool)],
        approach: f32,
    ) -> (f32, f32) {
        let mut best: Option<(f32, f32, f32)> = None; // (px, pz, d2)
        for p in players.iter().filter(|p| p.4) {
            let dx = gx - p.0;
            let dz = gz - p.2;
            let d2 = dx * dx + dz * dz;
            let better = match best {
                None => true,
                Some((_, _, bd2)) => d2 < bd2,
            };
            if better {
                best = Some((p.0, p.2, d2));
            }
        }
        let Some((px, pz, d2)) = best else {
            return (gx, gz);
        };
        let dist = d2.sqrt();
        if dist <= approach || dist < 1e-3 {
            return (gx, gz);
        }
        // Point on gate→player segment at `approach` metres from the player.
        let t = approach / dist;
        let x = px + (gx - px) * t;
        let z = pz + (gz - pz) * t;
        (x, z)
    }

    fn pick_kind(&mut self, minutes: f32) -> ZombieKind {
        let roll = self.rng.next();
        if minutes >= BRUTES_FROM_MIN && roll < 0.12 {
            ZombieKind::Brute
        } else if minutes >= RUNNERS_FROM_MIN && roll < 0.40 {
            ZombieKind::Runner
        } else {
            ZombieKind::Walker
        }
    }

    /// Prefer a gate no alive player currently sees (one occlusion ray per
    /// candidate); among those pick the one nearest the player centroid.
    /// Falls back to any gate when all are watched — the horde keeps coming.
    fn pick_gate<'m>(
        &mut self,
        map: &'m GameMap,
        players: &[(f32, f32, f32, f32, bool)],
    ) -> Option<&'m Gate> {
        let alive: Vec<_> = players.iter().filter(|p| p.4).collect();
        if map.gates.is_empty() {
            return None;
        }
        let (mut cx, mut cz) = (0.0f32, 0.0f32);
        for p in &alive {
            cx += p.0;
            cz += p.2;
        }
        if !alive.is_empty() {
            cx /= alive.len() as f32;
            cz /= alive.len() as f32;
        }

        let visible = |g: &Gate| -> bool {
            for &&(px, eye, pz, yaw, _) in alive.iter() {
                let dx = g.x - px;
                let dz = g.z - pz;
                let dist = (dx * dx + dz * dz).sqrt().max(1e-3);
                // facing check first (cheap): gate must be roughly in front
                let fwd = dir_from_angles(yaw, 0.0);
                let dot = (dx / dist) * fwd.x + (dz / dist) * fwd.z;
                if dot < 0.2 {
                    continue;
                }
                let o = Vec3::new(px, eye, pz);
                let d = Vec3::new(dx / dist, 0.0, dz / dist);
                match nearest_wall_t(o, d, &map.walls) {
                    Some(t) if t < dist - 0.5 => continue, // a wall hides it
                    _ => return true,                      // clear line of sight
                }
            }
            false
        };

        let mut best: Option<(&Gate, f32, bool)> = None; // (gate, d2 to centroid, hidden)
        for g in &map.gates {
            let hidden = !visible(g);
            let d2 = (g.x - cx).powi(2) + (g.z - cz).powi(2);
            let better = match &best {
                None => true,
                Some((_, bd2, bhidden)) => match (hidden, bhidden) {
                    (true, false) => true,
                    (false, true) => false,
                    _ => d2 < *bd2,
                },
            };
            if better {
                best = Some((g, d2, hidden));
            }
        }
        best.map(|(g, _, _)| g)
    }
}
