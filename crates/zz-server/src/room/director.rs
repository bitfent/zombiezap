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

/// No progress toward players for this long (fighting, live horde) → respawn
/// stuck bodies on a flow-connected cell (M26 belt-and-braces).
const STALL_RESPAWN_SEC: f32 = 8.0;
/// Horde min-distance must improve by at least this many metres to count as
/// progress (filters jitter / separation noise).
const STALL_PROGRESS_M: f32 = 0.5;

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
    /// Best (smallest) horde→player distance seen this fighting phase.
    stall_best_min_dist: f32,
    /// Consecutive fighting ticks with no progress toward players.
    stall_ticks: u32,
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
            stall_best_min_dist: f32::MAX,
            stall_ticks: 0,
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
                self.stall_best_min_dist = f32::MAX;
                self.stall_ticks = 0;
                events.wave_start = Some(self.wave);
                // Fall through to spawn on the same tick.
            }
            Phase::Fighting => {}
        }

        if matches!(self.phase, Phase::Fighting) {
            self.spawn_from_budget(map, grid, zombies, players);
            // M26: if the whole horde is frozen in a disconnected pocket,
            // teleport-respawn at flow-connected cells so the room cannot soft-lock.
            self.unstall_if_needed(map, grid, zombies, players);
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
        // Multi-source BFS from alive players — same connectivity the flow
        // field uses. Walkable-but-disconnected pockets (courtyards, cover
        // rings) must not be accepted as spawn cells (M26).
        let reach = Self::player_reach(grid, players);

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
            let (x, z) =
                Self::approach_spawn(gx, gz, players, approach_i, grid, &map.walls, reach.as_deref());
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
                let (fx_t, fz_t) =
                    Self::snap_walkable(fx_t, fz_t, grid, &map.walls, reach.as_deref());
                // Flank is only useful if the flow field can route through it.
                if Self::is_flow_reachable(grid, reach.as_deref(), fx_t, fz_t) {
                    Some((fx_t, fz_t))
                } else {
                    None
                }
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

    /// Distance field from all alive player cells (`u16::MAX` = unreachable).
    fn player_reach(
        grid: &zz_core::map::WalkGrid,
        players: &[(f32, f32, f32, f32, bool)],
    ) -> Option<Vec<u16>> {
        let sources: Vec<(usize, usize)> = players
            .iter()
            .filter(|p| p.4)
            .filter_map(|p| grid.cell_of(p.0, p.2))
            .filter(|&(ix, iz)| grid.is_walkable(ix, iz))
            .collect();
        if sources.is_empty() {
            None
        } else {
            Some(grid.distance_field(&sources))
        }
    }

    fn is_flow_reachable(
        grid: &zz_core::map::WalkGrid,
        reach: Option<&[u16]>,
        x: f32,
        z: f32,
    ) -> bool {
        let Some(dist) = reach else {
            // No player cells on the grid — don't block placement.
            return true;
        };
        let Some((ix, iz)) = grid.cell_of(x, z) else {
            return false;
        };
        dist[iz * grid.cells_per_side + ix] < u16::MAX
    }

    /// If fighting zombies make zero progress toward players for
    /// [`STALL_RESPAWN_SEC`], re-seat stuck bodies on a flow-connected cell.
    ///
    /// Far / unroutable bodies go to a normal approach spawn. Bodies that are
    /// already mid-map but frozen against geometry are **pulled closer** (never
    /// shoved back out to the approach ring — that looped forever on maze-y
    /// seeds like ZZ27-0).
    fn unstall_if_needed(
        &mut self,
        map: &GameMap,
        grid: &zz_core::map::WalkGrid,
        zombies: &mut [Zombie],
        players: &[(f32, f32, f32, f32, bool)],
    ) {
        if !matches!(self.phase, Phase::Fighting) || zombies.is_empty() {
            self.stall_ticks = 0;
            return;
        }
        let alive: Vec<(f32, f32)> = players
            .iter()
            .filter(|p| p.4)
            .map(|p| (p.0, p.2))
            .collect();
        if alive.is_empty() {
            return;
        }

        let mut min_dist = f32::MAX;
        for z in zombies.iter() {
            for &(px, pz) in &alive {
                let d = ((z.body.x - px).powi(2) + (z.body.z - pz).powi(2)).sqrt();
                min_dist = min_dist.min(d);
            }
        }
        if min_dist < self.stall_best_min_dist - STALL_PROGRESS_M {
            self.stall_best_min_dist = min_dist;
            self.stall_ticks = 0;
            return;
        }
        // Only treat as "engaged" when a bite is imminent. Pursue-range alone
        // is not enough: ZZ42-0 freezes a walker at ~6 m forever while the
        // rest of the pack idles mid-map (watchdog used to early-out).
        if min_dist <= ZOMBIE_ATTACK_RANGE * 1.5 {
            self.stall_ticks = 0;
            return;
        }
        self.stall_ticks = self.stall_ticks.saturating_add(1);
        let limit = (STALL_RESPAWN_SEC * TICK_RATE as f32).round() as u32;
        if self.stall_ticks < limit {
            return;
        }

        let reach = Self::player_reach(grid, players);
        let approach = director_approach_dist(map.arena_half);
        let (cx, cz) = Self::alive_centroid(players);
        let mut moved = 0u32;
        for z in zombies.iter_mut() {
            let (px, pz, d) = alive
                .iter()
                .map(|&(ax, az)| {
                    let dd = ((z.body.x - ax).powi(2) + (z.body.z - az).powi(2)).sqrt();
                    (ax, az, dd)
                })
                .min_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
                .unwrap_or((cx, cz, f32::MAX));
            let routable =
                Self::is_flow_reachable(grid, reach.as_deref(), z.body.x, z.body.z);
            if d <= ZOMBIE_ATTACK_RANGE * 1.5 && routable {
                continue;
            }

            let candidate = if !routable || d >= approach * 0.55 {
                // Soft-lock class: disconnected pocket or still out at the
                // approach ring — re-seat on a connected approach cell.
                let gate = if map.gates.is_empty() {
                    (cx + approach, cz)
                } else {
                    let i = (self.rng.next() * map.gates.len() as f64).floor() as usize;
                    let g = &map.gates[i.min(map.gates.len() - 1)];
                    (g.x, g.z)
                };
                Self::approach_spawn(
                    gate.0,
                    gate.1,
                    players,
                    approach,
                    grid,
                    &map.walls,
                    reach.as_deref(),
                )
            } else {
                // Mid / close freeze (maze wall, cul-de-sac, or jammed in
                // pursue range): pull inward toward the player, never farther
                // than current d. Prefer a bite-range ring when already close.
                let pull = if d <= ZOMBIE_PURSUE_RANGE {
                    (ZOMBIE_ATTACK_RANGE * 0.85).min(d * 0.5).max(0.6)
                } else {
                    (d * 0.45).clamp(ZOMBIE_PURSUE_RANGE * 0.75, (d - 1.0).max(4.0))
                };
                let (tx, tz) = if d < 1e-3 {
                    (px + pull, pz)
                } else {
                    let t = pull / d;
                    (px + (z.body.x - px) * t, pz + (z.body.z - pz) * t)
                };
                let ring_r = if d <= ZOMBIE_PURSUE_RANGE {
                    ZOMBIE_ATTACK_RANGE * 1.1
                } else {
                    (d * 0.5).clamp(ZOMBIE_PURSUE_RANGE, 14.0)
                };
                Self::snap_walkable_opt(tx, tz, grid, &map.walls, reach.as_deref())
                    .or_else(|| {
                        Self::fallback_connected_ring(
                            px,
                            pz,
                            ring_r,
                            grid,
                            &map.walls,
                            reach.as_deref(),
                        )
                    })
                    .unwrap_or_else(|| {
                        Self::snap_walkable(tx, tz, grid, &map.walls, reach.as_deref())
                    })
            };
            let (x, zpos) = (
                candidate.0.clamp(-map.arena_half + 1.0, map.arena_half - 1.0),
                candidate.1.clamp(-map.arena_half + 1.0, map.arena_half - 1.0),
            );
            // Refuse a "rescue" that puts a routable body farther away.
            let new_d = ((x - px).powi(2) + (zpos - pz).powi(2)).sqrt();
            if new_d > d + 0.5 && routable {
                let ring_r = (d * 0.5).clamp(ZOMBIE_PURSUE_RANGE, 14.0);
                let Some((nx, nz)) =
                    Self::fallback_connected_ring(px, pz, ring_r, grid, &map.walls, reach.as_deref())
                else {
                    continue;
                };
                z.body.x = nx.clamp(-map.arena_half + 1.0, map.arena_half - 1.0);
                z.body.z = nz.clamp(-map.arena_half + 1.0, map.arena_half - 1.0);
            } else {
                z.body.x = x;
                z.body.z = zpos;
            }
            z.body.y = 0.0;
            z.yaw = yaw_toward(z.body.x, z.body.z, cx, cz);
            z.flank_xz = None;
            z.state = if z.frenzy { ZS_FRENZY_BIT } else { 0 };
            z.windup_left = 0;
            moved += 1;
        }
        if moved > 0 {
            self.stall_ticks = 0;
            self.stall_best_min_dist = f32::MAX;
            for z in zombies.iter() {
                for &(ax, az) in &alive {
                    let d = ((z.body.x - ax).powi(2) + (z.body.z - az).powi(2)).sqrt();
                    self.stall_best_min_dist = self.stall_best_min_dist.min(d);
                }
            }
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
    /// then snap to a walkable, body-clear, **flow-reachable** cell so the
    /// flow field can route and continuous collision can actually step
    /// (M22b + M26).
    fn approach_spawn(
        gx: f32,
        gz: f32,
        players: &[(f32, f32, f32, f32, bool)],
        approach: f32,
        grid: &zz_core::map::WalkGrid,
        walls: &[Aabb],
        reach: Option<&[u16]>,
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
            return Self::snap_walkable(gx, gz, grid, walls, reach);
        };
        let dist = d2.sqrt();
        if dist <= approach || dist < 1e-3 {
            return Self::snap_walkable(gx, gz, grid, walls, reach);
        }
        for k in 0..24 {
            let along = (approach + k as f32 * 2.0).min(dist);
            let t = along / dist;
            let x = px + (gx - px) * t;
            let z = pz + (gz - pz) * t;
            if let Some(c) = Self::try_spawn_point(x, z, grid, walls, reach) {
                return c;
            }
        }
        // Segment samples missed (solid / pocket). Prefer a connected cell
        // near the intended approach point, else a known player-ring cell.
        if let Some(c) = Self::snap_walkable_opt(gx, gz, grid, walls, reach)
            .or_else(|| {
                let t = (approach / dist).min(1.0);
                let ax = px + (gx - px) * t;
                let az = pz + (gz - pz) * t;
                Self::snap_walkable_opt(ax, az, grid, walls, reach)
            })
            .or_else(|| Self::fallback_connected_ring(px, pz, approach, grid, walls, reach))
        {
            return c;
        }
        Self::snap_walkable(gx, gz, grid, walls, reach)
    }

    /// Accept only positions whose cell is walkable, body-clear, and in the
    /// players' flow-field region. Always return the cell centre so partial
    /// wall overlaps inside a walkable cell cannot freeze the body (M22b).
    fn try_spawn_point(
        x: f32,
        z: f32,
        grid: &zz_core::map::WalkGrid,
        walls: &[Aabb],
        reach: Option<&[u16]>,
    ) -> Option<(f32, f32)> {
        let (ix, iz) = grid.cell_of(x, z)?;
        if !grid.is_walkable(ix, iz) {
            return None;
        }
        if let Some(dist) = reach
            && dist[iz * grid.cells_per_side + ix] == u16::MAX
        {
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

    /// Spiral search for a walkable + body-clear + flow-reachable cell.
    fn snap_walkable_opt(
        x: f32,
        z: f32,
        grid: &zz_core::map::WalkGrid,
        walls: &[Aabb],
        reach: Option<&[u16]>,
    ) -> Option<(f32, f32)> {
        if let Some(c) = Self::try_spawn_point(x, z, grid, walls, reach) {
            return Some(c);
        }
        // Spiral search in cell units (centre samples) — farther than the old
        // 8 m ring so a thick block does not leave the spawn inside solid.
        let (ox, oz) = grid.cell_of(x, z)?;
        for r in 1..=18 {
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
                    if nx >= grid.cells_per_side || nz >= grid.cells_per_side {
                        continue;
                    }
                    if !grid.is_walkable(nx, nz) {
                        continue;
                    }
                    if let Some(dist) = reach
                        && dist[nz * grid.cells_per_side + nx] == u16::MAX
                    {
                        continue;
                    }
                    let (cx, cz) = Self::cell_center(grid, nx, nz);
                    if !body_blocked_at(cx, cz, walls) {
                        return Some((cx, cz));
                    }
                }
            }
        }
        None
    }

    fn snap_walkable(
        x: f32,
        z: f32,
        grid: &zz_core::map::WalkGrid,
        walls: &[Aabb],
        reach: Option<&[u16]>,
    ) -> (f32, f32) {
        if let Some(c) = Self::snap_walkable_opt(x, z, grid, walls, reach) {
            return c;
        }
        // Prefer any connected ring around the nearest approach, then give up.
        if let Some(c) = Self::fallback_connected_ring(x, z, 12.0, grid, walls, reach) {
            return c;
        }
        // Last resort without reach filter (body may still unstick). Prefer a
        // walkable centre over raw coords if one exists nearby.
        if let Some(c) = Self::snap_walkable_opt(x, z, grid, walls, None) {
            return c;
        }
        (x, z)
    }

    /// Sample a ring of radii around `(ox, oz)` for a flow-connected spawn.
    /// Used when the gate→player segment only hits solid or disconnected cells.
    fn fallback_connected_ring(
        ox: f32,
        oz: f32,
        radius: f32,
        grid: &zz_core::map::WalkGrid,
        walls: &[Aabb],
        reach: Option<&[u16]>,
    ) -> Option<(f32, f32)> {
        // 16 headings × a few radii around the intended distance.
        const N_DIR: i32 = 16;
        for r_i in 0..6 {
            let r = (radius * (0.55 + r_i as f32 * 0.18)).max(6.0);
            for d in 0..N_DIR {
                let ang = (d as f32) * (std::f32::consts::TAU / N_DIR as f32);
                let x = ox + ang.cos() * r;
                let z = oz + ang.sin() * r;
                if let Some(c) = Self::try_spawn_point(x, z, grid, walls, reach) {
                    return Some(c);
                }
                if let Some(c) = Self::snap_walkable_opt(x, z, grid, walls, reach) {
                    return Some(c);
                }
            }
        }
        // Any reachable body-clear cell near the target radius. Full scan is
        // fine on normal maps (60²); skip on huge grids (Rome) — ring samples
        // above already cover headings, and the stall watchdog is the safety net.
        let dist = reach?;
        let n = grid.cells_per_side;
        if n > 120 {
            return None;
        }
        let mut best: Option<(u16, f32, f32)> = None; // (dist_from_player, x, z)
        let target = radius.round().clamp(8.0, 60.0) as i32;
        for iz in 0..n {
            for ix in 0..n {
                let d = dist[iz * n + ix];
                // Prefer ~approach range (avoid spawning on the player's toes).
                if !(8..60).contains(&d) {
                    continue;
                }
                if !grid.is_walkable(ix, iz) {
                    continue;
                }
                let (cx, cz) = Self::cell_center(grid, ix, iz);
                if body_blocked_at(cx, cz, walls) {
                    continue;
                }
                let better = match best {
                    None => true,
                    // Prefer closer to the requested radius.
                    Some((bd, _, _)) => {
                        (d as i32 - target).unsigned_abs() < (bd as i32 - target).unsigned_abs()
                    }
                };
                if better {
                    best = Some((d, cx, cz));
                }
            }
        }
        best.map(|(_, x, z)| (x, z))
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
    use zz_core::map::{generate_map, WalkGrid};
    use zz_core::types::EnvKind;

    #[test]
    fn runner_flank_differs_from_centroid() {
        let map = generate_map(EnvKind::Urban, "flank-test");
        let grid = WalkGrid::rasterize(&map.walls, map.arena_half);
        let mut dir = Director::new("flank-test", EnvKind::Urban);
        // Force fighting wave 3+ for runners.
        dir.wave = 3;
        dir.phase = Phase::Fighting;
        dir.wave_budget_left = 500.0;
        dir.points = 500.0;
        // Player on a real walkable spawn so flow-reach is defined.
        let s = map.spawns[0];
        let players = vec![(s.x, 1.55, s.z, 0.0, true)];
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
            // Flank may be cleared if the offset landed outside the routable
            // set; when present it must be offset from the player.
            if let Some(flank) = r.flank_xz {
                let d2 = (flank.0 - s.x).powi(2) + (flank.1 - s.z).powi(2);
                assert!(
                    d2 > 1.0,
                    "flank should be offset from player, got {flank:?}"
                );
            }
        }
    }

    /// M26 bad-seed class: disconnected walkable pockets near the approach
    /// ring. Placement must still land every zombie on a flow-reachable cell.
    #[test]
    fn wave1_spawns_flow_reachable_on_pocket_seeds() {
        for seed in ["m26-125", "code053-0", "code098-0", "NXW7H-453"] {
            let map = generate_map(EnvKind::Urban, seed);
            let grid = WalkGrid::rasterize(&map.walls, map.arena_half);
            let s = map.spawns[0];
            let players = vec![(s.x, 1.55, s.z, 0.0, true)];
            let reach = Director::player_reach(&grid, &players).expect("player cell");
            let mut dir = Director::new(seed, EnvKind::Urban);
            dir.wave = 1;
            dir.phase = Phase::Fighting;
            dir.points = 500.0;
            dir.wave_budget_left = 500.0;
            let mut zombies = Vec::new();
            for _ in 0..20 {
                let _ = dir.step(&map, &grid, &mut zombies, &players);
            }
            assert!(
                !zombies.is_empty(),
                "{seed}: expected wave-1 spawns"
            );
            for z in &zombies {
                assert!(
                    Director::is_flow_reachable(&grid, Some(&reach), z.body.x, z.body.z),
                    "{seed}: zombie at ({:.1},{:.1}) not flow-reachable from player ({:.1},{:.1})",
                    z.body.x,
                    z.body.z,
                    s.x,
                    s.z
                );
            }
        }
    }


    /// Headless sim: wave-1 pack must close to pursue range within 25 s.
    #[test]
    fn wave1_closes_on_known_bot_fail_seeds() {
        use super::super::zombies::{step_zombie, FlowField, SpatialHash};
        for seed in ["ZZ27-0", "ZZ42-0", "ZZ13-0", "m26-125"] {
            let map = generate_map(EnvKind::Urban, seed);
            let grid = WalkGrid::rasterize(&map.walls, map.arena_half);
            let s = map.spawns[0];
            let players_dir = vec![(s.x, 1.55, s.z, 0.0, true)];
            let mut dir = Director::new(seed, EnvKind::Urban);
            // Real intro then fighting
            let mut zombies = Vec::new();
            let intro = (WAVE_INTRO_SEC * TICK_RATE as f32).round() as u32 + 5;
            for _ in 0..intro {
                let _ = dir.step(&map, &grid, &mut zombies, &players_dir);
            }
            // force drain budget quickly if still breather leftover
            for _ in 0..30 {
                let _ = dir.step(&map, &grid, &mut zombies, &players_dir);
                if !zombies.is_empty() { break; }
            }
            assert!(!zombies.is_empty(), "{seed}: no spawns");
            let mut flow = FlowField::empty();
            let mut hash = SpatialHash::new();
            let ticks_25s = TICK_RATE * 25;
            let mut min_dist = f32::MAX;
            let mut engaged = false;
            for t in 0..ticks_25s {
                let _ = dir.step(&map, &grid, &mut zombies, &players_dir);
                if t % FLOWFIELD_REBUILD_TICKS == 0 || flow.is_empty() {
                    flow = FlowField::rebuild(&grid, &[(s.x, s.z)]);
                }
                let positions: Vec<(f32, f32)> =
                    zombies.iter().map(|z| (z.body.x, z.body.z)).collect();
                hash.rebuild(positions.iter().copied());
                let player_flat = vec![(0u8, s.x, s.z, true)];
                for (zi, z) in zombies.iter_mut().enumerate() {
                    let sep = hash.separation(zi, z.body.x, z.body.z, &positions);
                    let _ = step_zombie(
                        z, &grid, &flow, &player_flat, sep, &map.walls, map.arena_half,
                    );
                }
                for z in &zombies {
                    let d = ((z.body.x - s.x).powi(2) + (z.body.z - s.z).powi(2)).sqrt();
                    min_dist = min_dist.min(d);
                    if d <= ZOMBIE_PURSUE_RANGE {
                        engaged = true;
                    }
                }
                if engaged { break; }
            }
            // Report positions
            let poss: Vec<_> = zombies.iter().map(|z| {
                let d = ((z.body.x - s.x).powi(2) + (z.body.z - s.z).powi(2)).sqrt();
                format!("({:.1},{:.1}) d={d:.1}", z.body.x, z.body.z)
            }).collect();
            eprintln!("{seed}: min_dist={min_dist:.1} engaged={engaged} zeds={}", zombies.len());
            eprintln!("  positions: {}", poss.join(" | "));
            // flow sample at first zed
            if let Some(z) = zombies.first() {
                let fd = flow.dir_at(&grid, z.body.x, z.body.z);
                eprintln!("  flow0={fd:?} body=({:.2},{:.2})", z.body.x, z.body.z);
            }
            assert!(
                engaged || min_dist < 12.0,
                "{seed}: horde did not close (min_dist={min_dist:.1})"
            );
        }
    }

    /// Stall watchdog: zombies frozen far from the player for STALL_RESPAWN_SEC
    /// are teleported onto a connected cell.
    #[test]
    fn stall_watchdog_teleports_frozen_horde() {
        let seed = "m26-125";
        let map = generate_map(EnvKind::Urban, seed);
        let grid = WalkGrid::rasterize(&map.walls, map.arena_half);
        let s = map.spawns[0];
        let players = vec![(s.x, 1.55, s.z, 0.0, true)];
        let reach = Director::player_reach(&grid, &players).unwrap();

        // Plant a zombie deep in a disconnected pocket if one exists; otherwise
        // plant far outside pursue range on an unroutable coord.
        let n = grid.cells_per_side;
        let mut stuck_pos = None;
        for iz in 0..n {
            for ix in 0..n {
                if grid.is_walkable(ix, iz) && reach[iz * n + ix] == u16::MAX {
                    let (cx, cz) = Director::cell_center(&grid, ix, iz);
                    if !body_blocked_at(cx, cz, &map.walls) {
                        stuck_pos = Some((cx, cz));
                        break;
                    }
                }
            }
            if stuck_pos.is_some() {
                break;
            }
        }
        let (sx, sz) = stuck_pos.unwrap_or((s.x + 40.0, s.z + 40.0));

        let mut dir = Director::new(seed, EnvKind::Urban);
        dir.wave = 1;
        dir.phase = Phase::Fighting;
        dir.points = 0.0; // no new spawns
        dir.wave_budget_left = 0.0;
        dir.live_from_wave = 1;
        dir.stall_best_min_dist = 100.0;
        let mut zombies = vec![Zombie {
            id: 1,
            kind: ZombieKind::Walker,
            body: Body::at(sx, sz),
            yaw: 0.0,
            health: 100.0,
            state: 0,
            windup_left: 0,
            cooldown_left: 0,
            target_slot: 0,
            from_wave: true,
            flank_xz: None,
            smash_cooldown: 0,
            frenzy: false,
        }];

        let ticks = (STALL_RESPAWN_SEC * TICK_RATE as f32).round() as u32 + 2;
        for _ in 0..ticks {
            let _ = dir.step(&map, &grid, &mut zombies, &players);
        }
        let z = &zombies[0];
        assert!(
            Director::is_flow_reachable(&grid, Some(&reach), z.body.x, z.body.z),
            "watchdog should teleport to flow-reachable cell, still at ({:.1},{:.1})",
            z.body.x,
            z.body.z
        );
    }
}



