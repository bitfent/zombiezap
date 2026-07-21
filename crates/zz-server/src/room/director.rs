//! Wave-based spawn director: CoD-style rounds with L4D/KF breathers.
//!
//! Lifecycle: intro breather → WaveStart → spend wave budget (no new spawns
//! after budget is empty) → all wave zombies dead → WaveClear + supply drop →
//! 8–12 s breather → next wave. Tick-counted (pause-safe), deterministic per
//! room seed.

use super::zombies::{Zombie, ZombieKind, yaw_toward};
use zz_core::constants::*;
use zz_core::map::{GameMap, Gate};
use zz_core::math::{Vec3, dir_from_angles, nearest_wall_t};
use zz_core::movement::body_blocked_at;
use zz_core::rng::Mulberry32;
use zz_core::types::{Aabb, Body, EnvKind};

/// Side effects the room applies after each director step.
#[derive(Default)]
pub struct DirectorEvents {
    pub wave_start: Option<u16>,
    pub wave_clear: Option<(u16, u8)>, // (wave, bonus_ammo)
    pub supply_drop: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// No spawns. `remaining` ticks until the next WaveStart.
    Breather { remaining: u32 },
    /// Spending budget and/or waiting for live wave zombies to die.
    Fighting,
}

pub struct Director {
    /// Accrued within-wave spend points (not the wave budget itself).
    points: f32,
    /// Remaining points for the current wave (Fighting only).
    wave_budget_left: f32,
    rng: Mulberry32,
    next_id: u16,
    /// Test/tuning override: multiplies budgets + within-wave spend rate.
    rate_mul: f32,
    phase: Phase,
    /// Current wave number (1-indexed once fighting has begun; 0 pre-wave-1).
    wave: u16,
    /// Live zombies that count toward the current wave clear.
    live_from_wave: u32,
    /// Waves that have fully cleared (MatchStats).
    waves_cleared: u16,
    env: EnvKind,
}

impl Director {
    pub fn new(seed: &str, env: EnvKind) -> Self {
        let rate_mul = std::env::var("ZZ_DIRECTOR_RATE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1.0);
        let intro = (WAVE_INTRO_SEC * TICK_RATE as f32).round() as u32;
        // High director rates (bot tests) skip almost all calm so growth
        // assertions still land inside their short windows.
        let intro = if rate_mul > 1.01 {
            intro.clamp(1, TICK_RATE / 3)
        } else {
            intro
        };
        Director {
            points: 0.0,
            wave_budget_left: 0.0,
            rng: Mulberry32::from_seed(&format!("{seed}|director")),
            next_id: 1,
            rate_mul,
            phase: Phase::Breather { remaining: intro },
            wave: 0,
            live_from_wave: 0,
            waves_cleared: 0,
            env,
        }
    }

    pub fn waves_cleared(&self) -> u16 {
        self.waves_cleared
    }

    /// Snapshot difficulty byte: wave number (clamped), not wall-clock minutes.
    pub fn difficulty(&self) -> u8 {
        self.wave.min(255) as u8
    }

    /// Call when a wave-spawned zombie dies (or is removed).
    pub fn on_wave_zombie_killed(&mut self) {
        self.live_from_wave = self.live_from_wave.saturating_sub(1);
    }

    /// Advance one sim tick. Spawns into `zombies`; returns broadcast events.
    pub fn step(
        &mut self,
        map: &GameMap,
        grid: &zz_core::map::WalkGrid,
        zombies: &mut Vec<Zombie>,
        players: &[(f32, f32, f32, f32, bool)], // (x, eye_y, z, yaw, alive)
    ) -> DirectorEvents {
        let mut events = DirectorEvents::default();
        let alive_n = players.iter().filter(|p| p.4).count();
        if alive_n == 0 {
            return events;
        }

        match self.phase {
            Phase::Breather { remaining } => {
                if remaining > 0 {
                    self.phase = Phase::Breather {
                        remaining: remaining - 1,
                    };
                    return events;
                }
                // Start next wave.
                self.wave = self.wave.saturating_add(1);
                let budget = wave_budget_points(self.wave, alive_n, self.env) * self.rate_mul;
                // points = remaining spendable budget for this wave.
                self.points = budget;
                self.wave_budget_left = budget;
                self.live_from_wave = 0;
                self.phase = Phase::Fighting;
                events.wave_start = Some(self.wave);
                // Fall through to spawn on the same tick.
            }
            Phase::Fighting => {}
        }

        if matches!(self.phase, Phase::Fighting) {
            self.spawn_from_budget(map, grid, zombies, players);
            // Clear is also checked after kills (room) — spawn path covers the
            // case where budget ends with zero live zombies already.
            if let Some(clear) = self.try_clear(alive_n) {
                events.wave_clear = Some(clear);
                events.supply_drop = true;
            }
        }

        events
    }

    /// After kills: if fighting, budget spent, and no live wave zombies → clear.
    pub fn check_clear_after_kills(&mut self, alive_players: usize) -> DirectorEvents {
        let mut events = DirectorEvents::default();
        if let Some(clear) = self.try_clear(alive_players) {
            events.wave_clear = Some(clear);
            events.supply_drop = true;
        }
        events
    }

    fn try_clear(&mut self, alive_n: usize) -> Option<(u16, u8)> {
        if !matches!(self.phase, Phase::Fighting) {
            return None;
        }
        let cheapest = ZOMBIE_WALKER.3 as f32;
        // Budget exhausted when we can't afford even a walker.
        if self.points >= cheapest {
            return None;
        }
        if self.live_from_wave > 0 {
            return None;
        }
        let mut events = DirectorEvents::default();
        self.finish_wave(&mut events, alive_n);
        events.wave_clear
    }

    fn finish_wave(&mut self, events: &mut DirectorEvents, alive_n: usize) {
        let cleared = self.wave;
        self.waves_cleared = self.waves_cleared.saturating_add(1);
        // Bonus ammo is 0 on the wire — the supply crate is the breather reward
        // (free mag top-ups fought ammo accounting tests and felt unearned).
        events.wave_clear = Some((cleared, 0));
        events.supply_drop = true;

        let lo = (WAVE_BREATHER_MIN_SEC * TICK_RATE as f32) as u32;
        let hi = (WAVE_BREATHER_MAX_SEC * TICK_RATE as f32) as u32;
        let span = hi.saturating_sub(lo).max(1);
        let mut remaining = lo + (self.rng.next() * span as f64).floor() as u32;
        if self.rate_mul > 1.01 {
            // Elevated rates (bot / stress): short calm so pressure stays on.
            remaining = remaining.clamp(TICK_RATE / 2, TICK_RATE * 2);
        } else if self.rate_mul < 0.5 {
            // Tiny-budget bot tests: still observe a multi-tick empty field.
            remaining = remaining.clamp(TICK_RATE, TICK_RATE * 3);
        }
        // Soft scale: more players → slightly shorter calm (keep pressure).
        if alive_n >= 3 {
            remaining = (remaining as f32 * 0.85) as u32;
        }
        self.phase = Phase::Breather { remaining };
        self.wave_budget_left = 0.0;
        self.points = 0.0;
    }

    fn spawn_from_budget(
        &mut self,
        map: &GameMap,
        grid: &zz_core::map::WalkGrid,
        zombies: &mut Vec<Zombie>,
        players: &[(f32, f32, f32, f32, bool)],
    ) {
        // Cap spawns per tick so a huge rate doesn't block the room tick.
        // Turbo rates (bot tests) may dump many; normal play still fronts
        // the wave as a push (not a one-per-second trickle).
        let max_spawns = if self.rate_mul >= 10.0 {
            64
        } else if self.rate_mul > 1.01 {
            24
        } else {
            10
        };
        let mut spawned = 0usize;

        let approach = director_approach_dist(map.arena_half);
        let (cx, cz) = Self::alive_centroid(players);
        // Group facing: average player forward, for runner flank perpendicular.
        let (fx, fz) = Self::alive_forward(players);

        while zombies.len() < MAX_ZOMBIES && spawned < max_spawns {
            let kind = self.pick_kind();
            let cost = kind.stats().3 as f32;
            if self.points < cost {
                break;
            }
            let Some(gate) = self.pick_gate(map, players) else {
                break;
            };
            self.points -= cost;
            self.wave_budget_left = self.points;

            let (jx, jz) = (
                (self.rng.next() as f32 - 0.5) * 1.5,
                (self.rng.next() as f32 - 0.5) * 1.5,
            );
            let (gx, gz) = (gate.x + jx, gate.z + jz);
            // Per-spawn approach jitter so a wave dump does not stack every
            // walker on the same (possibly stuck) pull-in cell.
            let approach_i = (approach * (0.82 + self.rng.next() as f32 * 0.30)).clamp(14.0, approach);
            let (x, z) = Self::approach_spawn(gx, gz, players, approach_i, grid, &map.walls);
            let (x, z) = (
                x.clamp(-map.arena_half + 1.0, map.arena_half - 1.0),
                z.clamp(-map.arena_half + 1.0, map.arena_half - 1.0),
            );

            let frenzy = kind == ZombieKind::Walker
                && self.wave >= FRENZY_FROM_WAVE
                && self.rng.next() < FRENZY_WALKER_CHANCE;

            let flank = if kind == ZombieKind::Runner {
                // Perpendicular to group facing, random sign, offset from centroid.
                let sign = if self.rng.next() < 0.5 { 1.0 } else { -1.0 };
                let px = -fz * sign; // rotate facing 90°
                let pz = fx * sign;
                let plen = (px * px + pz * pz).sqrt().max(1e-3);
                let (px, pz) = (px / plen, pz / plen);
                let fx_t = cx + px * RUNNER_FLANK_OFFSET_M;
                let fz_t = cz + pz * RUNNER_FLANK_OFFSET_M;
                let (fx_t, fz_t) = Self::snap_walkable(fx_t, fz_t, grid, &map.walls);
                Some((fx_t, fz_t))
            } else {
                None
            };

            let mut state = 0u8;
            if frenzy {
                state |= ZS_FRENZY_BIT;
            }

            zombies.push(Zombie {
                id: self.next_id,
                kind,
                body: Body::at(x, z),
                yaw: yaw_toward(x, z, cx, cz),
                health: kind.stats().1,
                state,
                windup_left: 0,
                cooldown_left: 0,
                target_slot: 0,
                from_wave: true,
                flank_xz: flank,
                smash_cooldown: 0,
                frenzy,
            });
            self.next_id = self.next_id.wrapping_add(1).max(1);
            self.live_from_wave = self.live_from_wave.saturating_add(1);
            spawned += 1;
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

    fn alive_forward(players: &[(f32, f32, f32, f32, bool)]) -> (f32, f32) {
        let mut fx = 0.0f32;
        let mut fz = 0.0f32;
        let mut n = 0u32;
        for p in players.iter().filter(|p| p.4) {
            let d = dir_from_angles(p.3, 0.0);
            fx += d.x;
            fz += d.z;
            n += 1;
        }
        if n == 0 {
            return (0.0, -1.0);
        }
        let len = (fx * fx + fz * fz).sqrt();
        if len < 1e-3 {
            (0.0, -1.0)
        } else {
            (fx / len, fz / len)
        }
    }

    /// If the gate is farther than `approach` from every alive player, place
    /// the spawn on the segment gate→nearest-player at distance `approach`,
    /// then snap to a walkable, body-clear cell so the flow field can route
    /// and continuous collision can actually step (M22b).
    fn approach_spawn(
        gx: f32,
        gz: f32,
        players: &[(f32, f32, f32, f32, bool)],
        approach: f32,
        grid: &zz_core::map::WalkGrid,
        walls: &[Aabb],
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
            return Self::snap_walkable(gx, gz, grid, walls);
        };
        let dist = d2.sqrt();
        if dist <= approach || dist < 1e-3 {
            return Self::snap_walkable(gx, gz, grid, walls);
        }
        for k in 0..24 {
            let along = (approach + k as f32 * 2.0).min(dist);
            let t = along / dist;
            let x = px + (gx - px) * t;
            let z = pz + (gz - pz) * t;
            if let Some(c) = Self::try_spawn_point(x, z, grid, walls) {
                return c;
            }
        }
        Self::snap_walkable(gx, gz, grid, walls)
    }

    /// Accept only positions whose cell is walkable AND whose continuous body
    /// footprint is free. Always return the cell centre so partial wall
    /// overlaps inside a walkable cell cannot freeze the body (M22b).
    fn try_spawn_point(
        x: f32,
        z: f32,
        grid: &zz_core::map::WalkGrid,
        walls: &[Aabb],
    ) -> Option<(f32, f32)> {
        let (ix, iz) = grid.cell_of(x, z)?;
        if !grid.is_walkable(ix, iz) {
            return None;
        }
        let (cx, cz) = Self::cell_center(grid, ix, iz);
        if body_blocked_at(cx, cz, walls) {
            return None;
        }
        Some((cx, cz))
    }

    fn cell_center(grid: &zz_core::map::WalkGrid, ix: usize, iz: usize) -> (f32, f32) {
        const CELL: f32 = 1.0;
        (
            -grid.half + (ix as f32 + 0.5) * CELL,
            -grid.half + (iz as f32 + 0.5) * CELL,
        )
    }

    fn snap_walkable(
        x: f32,
        z: f32,
        grid: &zz_core::map::WalkGrid,
        walls: &[Aabb],
    ) -> (f32, f32) {
        if let Some(c) = Self::try_spawn_point(x, z, grid, walls) {
            return c;
        }
        // Spiral search in cell units (centre samples) — farther than the old
        // 8 m ring so a thick block does not leave the spawn inside solid.
        if let Some((ox, oz)) = grid.cell_of(x, z) {
            for r in 1..=14 {
                let ri = r as isize;
                for dz in -ri..=ri {
                    for dx in -ri..=ri {
                        if dx.abs() != ri && dz.abs() != ri {
                            continue; // ring only
                        }
                        let nx = ox as isize + dx;
                        let nz = oz as isize + dz;
                        if nx < 0 || nz < 0 {
                            continue;
                        }
                        let (nx, nz) = (nx as usize, nz as usize);
                        if !grid.is_walkable(nx, nz) {
                            continue;
                        }
                        let (cx, cz) = Self::cell_center(grid, nx, nz);
                        if !body_blocked_at(cx, cz, walls) {
                            return (cx, cz);
                        }
                    }
                }
            }
        }
        // Last resort: original coords (may still be stuck — step_zombie unsticks).
        (x, z)
    }

    fn pick_kind(&mut self) -> ZombieKind {
        let roll = self.rng.next();
        if self.wave >= BRUTES_FROM_WAVE && roll < 0.12 {
            ZombieKind::Brute
        } else if self.wave >= RUNNERS_FROM_WAVE && roll < 0.40 {
            ZombieKind::Runner
        } else {
            ZombieKind::Walker
        }
    }

    /// Prefer a gate no alive player currently sees; among those, sample near
    /// the player centroid. A wave dumps many spawns in one tick — always
    /// picking the single nearest gate stacked every walker on one pull-in
    /// cell (M22b). We pick uniformly among competitive gates instead.
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
                let fwd = dir_from_angles(yaw, 0.0);
                let dot = (dx / dist) * fwd.x + (dz / dist) * fwd.z;
                if dot < 0.2 {
                    continue;
                }
                let o = Vec3::new(px, eye, pz);
                let d = Vec3::new(dx / dist, 0.0, dz / dist);
                match nearest_wall_t(o, d, &map.walls) {
                    Some(t) if t < dist - 0.5 => continue,
                    _ => return true,
                }
            }
            false
        };

        let mut scored: Vec<(&Gate, f32, bool)> = map
            .gates
            .iter()
            .map(|g| {
                let hidden = !visible(g);
                let d2 = (g.x - cx).powi(2) + (g.z - cz).powi(2);
                (g, d2, hidden)
            })
            .collect();
        if scored.is_empty() {
            return None;
        }
        // Prefer hidden tier; among that tier keep gates within 1.6× best d².
        let any_hidden = scored.iter().any(|(_, _, h)| *h);
        if any_hidden {
            scored.retain(|(_, _, h)| *h);
        }
        let best_d2 = scored
            .iter()
            .map(|(_, d2, _)| *d2)
            .fold(f32::MAX, f32::min);
        let cutoff = (best_d2 * 1.6).max(best_d2 + 1.0);
        scored.retain(|(_, d2, _)| *d2 <= cutoff);
        let i = (self.rng.next() * scored.len() as f64).floor() as usize;
        scored.get(i.min(scored.len() - 1)).map(|(g, _, _)| *g)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zz_core::map::generate_map;
    use zz_core::types::EnvKind;

    #[test]
    fn runner_flank_differs_from_centroid() {
        let map = generate_map(EnvKind::Urban, "flank-test");
        let grid = zz_core::map::WalkGrid::rasterize(&map.walls, map.arena_half);
        let mut dir = Director::new("flank-test", EnvKind::Urban);
        // Force fighting wave 3+ for runners.
        dir.wave = 3;
        dir.phase = Phase::Fighting;
        dir.wave_budget_left = 500.0;
        dir.points = 500.0;
        let players = vec![(0.0, 1.55, 0.0, 0.0, true)];
        let mut zombies = Vec::new();
        // Spend until we get a runner.
        for _ in 0..40 {
            let _ = dir.step(&map, &grid, &mut zombies, &players);
            if zombies.iter().any(|z| z.kind == ZombieKind::Runner) {
                break;
            }
        }
        let runners: Vec<_> = zombies
            .iter()
            .filter(|z| z.kind == ZombieKind::Runner)
            .collect();
        assert!(!runners.is_empty(), "expected a runner by wave 3");
        for r in runners {
            let flank = r.flank_xz.expect("runner should have flank target");
            let d2 = flank.0 * flank.0 + flank.1 * flank.1;
            assert!(
                d2 > 1.0,
                "flank should be offset from origin centroid, got {flank:?}"
            );
        }
    }
}



