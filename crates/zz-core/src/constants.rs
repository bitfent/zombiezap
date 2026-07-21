//! Fixed gameplay rules. Tunable later; fixed now so client prediction and
//! server simulation can never disagree on the numbers. Values carried over
//! from ShotAnte (legacy/packages/shared/src/constants.ts) where applicable,
//! retuned for 5-player co-op survival where noted.

pub const PROTOCOL_VERSION: u32 = 1;

// ── ticks & netcode ────────────────────────────────────────────────────────
pub const TICK_RATE: u32 = 30;
pub const TICK_DT: f32 = 1.0 / TICK_RATE as f32;
/// Remote entities render this far in the past (interpolation buffer).
pub const INTERP_DELAY_MS: u32 = 100;
pub const PING_INTERVAL_MS: u64 = 2000;
/// No traffic for this long = the player left; the server closes the socket.
pub const PRESENCE_TIMEOUT_MS: u64 = 8000;
/// Full snapshot at least this often (~2 s) — bounds delta-chain error and
/// resyncs fresh/rejoining streams.
pub const KEYFRAME_EVERY: u32 = 60;
/// Zombie + loot snapshot sections ride every Nth tick (15 Hz at N=2).
pub const SNAPSHOT_ZOMBIE_EVERY: u32 = 2;
/// Anti-cheat: input flood cap.
pub const MAX_INPUTS_PER_SECOND: u32 = 90;
pub const MAX_PITCH: f32 = core::f32::consts::FRAC_PI_2 - 0.01;

// ── world & movement ───────────────────────────────────────────────────────
/// Co-op arena half-size in meters (ShotAnte was 30; hordes need room).
pub const ARENA_HALF: f64 = 40.0;
pub const PLAYER_SPEED: f32 = 6.0;
pub const PLAYER_RADIUS: f32 = 0.45;
pub const PLAYER_EYE: f32 = 1.55;
pub const GRAVITY: f32 = 20.0;
pub const JUMP_VELOCITY: f32 = 7.0;
/// Auto-climb height — stairs/curbs/crates you walk up.
pub const STEP_UP: f32 = 0.55;
pub const MAX_HEALTH: u8 = 100;

/// Hitbox = stacked spheres approximating the visible avatar (y offset, radius).
/// The hitbox must match what the player SEES (ShotAnte lesson).
pub const PLAYER_HITBOX: [(f32, f32); 3] = [(0.55, 0.5), (1.0, 0.55), (1.6, 0.4)];
/// Zombie hit spheres: body + head; head hits deal double damage. The head
/// sphere (1.58..1.98) sits entirely ABOVE the eye line (1.55) while the body
/// sphere (0.33..1.57) covers it — flat shots are body hits; headshots take
/// deliberate upward aim at the small head.
pub const ZOMBIE_BODY_SPHERE: (f32, f32) = (0.95, 0.62);
pub const ZOMBIE_HEAD_SPHERE: (f32, f32) = (1.78, 0.2);
pub const HEADSHOT_MULTIPLIER: f32 = 2.0;

// ── firearm ────────────────────────────────────────────────────────────────
pub const GUN_DAMAGE: f32 = 34.0; // 3 body shots for a walker
pub const FIRE_COOLDOWN_TICKS: u32 = 5; // ≈180 ms at 30 TPS (tick-counted, not wall clock)
pub const MAG_SIZE: u8 = 12;
pub const START_RESERVE_AMMO: u8 = 48;
/// Manual / auto-reload duration (~1.5 s at 30 TPS).
pub const RELOAD_TICKS: u32 = 45;
pub const SHOT_RANGE: f32 = 90.0;

// ── melee ──────────────────────────────────────────────────────────────────
/// Horizontal reach for rifle-butt melee (metres).
pub const MELEE_RANGE: f32 = 2.2;
/// Half-angle of the forward melee cone (radians, ≈51.5°).
pub const MELEE_HALF_ANGLE_RAD: f32 = 0.9;
/// Damage per swing. Walker health is 100 → dies in exactly 2 hits.
pub const MELEE_DAMAGE: u8 = 50;
/// Cooldown between melee swings (~0.7 s at 30 TPS).
pub const MELEE_COOLDOWN_TICKS: u32 = 21;

// ── grenades ───────────────────────────────────────────────────────────────
pub const START_GRENADES: u8 = 2;
pub const MAX_GRENADES: u8 = 4;
pub const GRENADE_FUSE_TICKS: u32 = 75; // 2.5 s
pub const GRENADE_RADIUS: f32 = 4.0;
pub const GRENADE_DMG_MAX: f32 = 120.0; // at center, linear falloff (barrel math)
pub const GRENADE_DMG_MIN: f32 = 30.0; // at edge of radius
pub const GRENADE_THROW_SPEED: f32 = 14.0;
/// Friendly fire is OFF; your own grenade still hurts you at this fraction.
pub const GRENADE_SELF_DAMAGE: f32 = 0.5;

// ── zombies ────────────────────────────────────────────────────────────────
pub const MAX_ZOMBIES: usize = 200;
pub const ZOMBIE_ATTACK_RANGE: f32 = 1.2;
/// Ticks between a zombie entering Attack state and the damage landing —
/// the telegraph window the client animates.
pub const ZOMBIE_ATTACK_WINDUP_TICKS: u32 = 12;
pub const ZOMBIE_ATTACK_COOLDOWN_TICKS: u32 = 30;
/// Flow field toward players is rebuilt every N ticks.
pub const FLOWFIELD_REBUILD_TICKS: u32 = 10;
/// Within this range a zombie pursues its target directly instead of
/// following the flow field.
pub const ZOMBIE_PURSUE_RANGE: f32 = 8.0;

/// Per-kind stats: (speed m/s, health, damage, director point cost).
pub const ZOMBIE_WALKER: (f32, f32, f32, u32) = (2.2, 100.0, 10.0, 10);
pub const ZOMBIE_RUNNER: (f32, f32, f32, u32) = (4.5, 60.0, 8.0, 15);
pub const ZOMBIE_BRUTE: (f32, f32, f32, u32) = (1.6, 400.0, 25.0, 40);

// ── spawn director (legacy continuous rate; still used as within-wave drip) ─
pub const DIRECTOR_BASE_POINTS_PER_SEC: f32 = 6.0;
/// Spawn budget multiplier grows by this per minute survived (endless ramp).
pub const DIRECTOR_RAMP_PER_MIN: f32 = 0.35;
/// Peak/lull rhythm: slow sine modulation of the budget, ±40%.
pub const DIRECTOR_PULSE_PERIOD_SEC: f32 = 45.0;
pub const DIRECTOR_PULSE_AMPLITUDE: f32 = 0.4;
/// Runners join the mix after this many minutes; brutes after twice this.
pub const RUNNERS_FROM_MIN: f32 = 2.0;
pub const BRUTES_FROM_MIN: f32 = 4.0;

// ── wave rhythm (M22 horde feel) ───────────────────────────────────────────
/// Points budget for wave 1 before player/env multipliers.
/// ~6 walkers (cost 10) so a rate=1 first push already feels like a pack.
pub const WAVE_BASE_POINTS: f32 = 60.0;
/// Extra points added per wave after the first.
pub const WAVE_POINTS_PER_WAVE: f32 = 28.0;
/// Each extra alive player multiplies wave budget by this (duo ≈ 1.4× solo).
pub const WAVE_PLAYER_BUDGET_STEP: f32 = 0.40;
/// Calm seconds before wave 1 (players settle / read the map).
pub const WAVE_INTRO_SEC: f32 = 2.5;
/// Breather between waves (seconds, inclusive range).
pub const WAVE_BREATHER_MIN_SEC: f32 = 8.0;
pub const WAVE_BREATHER_MAX_SEC: f32 = 12.0;
/// Within a wave, spend at least this fraction of remaining budget per second
/// so the horde appears as a push rather than a trickle (capped by MAX_ZOMBIES).
pub const WAVE_SPAWN_BURST_FRAC_PER_SEC: f32 = 0.55;
/// Runners appear from this wave number (1-indexed).
pub const RUNNERS_FROM_WAVE: u16 = 3;
/// Brutes appear from this wave number (1-indexed).
pub const BRUTES_FROM_WAVE: u16 = 5;
/// Frenzy walkers (speed buff + state bit) from this wave.
pub const FRENZY_FROM_WAVE: u16 = 4;
/// Chance a late-wave walker is frenzied.
pub const FRENZY_WALKER_CHANCE: f64 = 0.28;
/// Frenzy speed multiplier on walker base speed.
pub const FRENZY_SPEED_MUL: f32 = 1.40;
/// High bit on the zombie snapshot `state` byte: frenzy (anim uses low 7 bits).
pub const ZS_FRENZY_BIT: u8 = 0x80;
/// Supply-crate loot kind on the wire (grants ammo + health + grenade).
pub const LOOT_KIND_SUPPLY: u8 = 3;
/// How long a supply crate stays on the ground (ticks).
pub const SUPPLY_DESPAWN_TICKS: u32 = 900; // 30 s
/// Runner flank offset from the player-group centroid (metres).
pub const RUNNER_FLANK_OFFSET_M: f32 = 10.0;
/// Clear flank waypoint once the runner is this close.
pub const RUNNER_FLANK_ARRIVE_M: f32 = 3.5;
/// Brute smash: must be within this xz distance of a cover AABB edge.
pub const BRUTE_SMASH_RANGE: f32 = 1.35;
/// Cooldown between smash attempts per brute (ticks).
pub const BRUTE_SMASH_COOLDOWN_TICKS: u32 = 45;

/// Per-environment director pressure. Tuned for the four main maps; Rome is
/// left soft (experiment). Higher = more points per wave.
///
/// Targets (first-run feel): solo urban dies ~wave 2–3; duo reaches wave 4–6.
pub fn env_pressure(env: crate::types::EnvKind) -> f32 {
    use crate::types::EnvKind::*;
    match env {
        Urban => 1.00,
        // Terraces / stairs give natural chokepoints — slightly softer.
        MountainTown => 0.92,
        // Open sightlines and sparse cover — denser waves to force movement.
        DesertTown => 1.18,
        // Dock edge + warehouse chokes — defender-favoured.
        SeaTown => 0.90,
        // Big map experiment: do not over-pressure (M15 approach already helps).
        RomeEur => 0.80,
    }
}

/// Wave-N point budget for `alive_players` on `env` (before ZZ_DIRECTOR_RATE).
pub fn wave_budget_points(wave: u16, alive_players: usize, env: crate::types::EnvKind) -> f32 {
    let w = wave.max(1) as f32;
    let base = WAVE_BASE_POINTS + (w - 1.0) * WAVE_POINTS_PER_WAVE;
    let n = alive_players.max(1) as f32;
    let player_scale = 1.0 + (n - 1.0) * WAVE_PLAYER_BUDGET_STEP;
    base * player_scale * env_pressure(env)
}

// ── director pacing vs map size (M15) ───────────────────────────────────────
// Urban/Mountain/Desert/Sea use arena_half ≈ 30. Rome EUR is 250. Without
// scaling, walkers take minutes to cross Rome. These formulas keep first
// contact ~20 s on every map while leaving goldens untouched (no sim math).
//
// Reference half-extent for "normal" maps (mapgen ARENA_HALF, not co-op 40).
pub const DIRECTOR_REF_ARENA_HALF: f32 = 30.0;
/// Max distance (m) from nearest alive player at which a zombie may spawn.
/// Walkers at 2.2 m/s cover ~45 m in ~20 s.
pub const DIRECTOR_APPROACH_DIST_M: f32 = 45.0;

/// Budget rate multiplier for a map of half-extent `arena_half`.
/// Urban (30) → 1.0; Rome (250) → ~2.9 (sqrt scale, clamped).
pub fn director_rate_scale(arena_half: f32) -> f32 {
    let t = (arena_half / DIRECTOR_REF_ARENA_HALF).max(1.0);
    t.sqrt().clamp(1.0, 3.5)
}

/// Pull-in distance for spawn placement on large maps (metres).
/// Never farther than this from the nearest alive player when possible.
pub fn director_approach_dist(arena_half: f32) -> f32 {
    DIRECTOR_APPROACH_DIST_M
        .min(arena_half * 0.9)
        .max(18.0)
}

// ── loot ───────────────────────────────────────────────────────────────────
pub const PICKUP_RADIUS: f32 = 0.9;
pub const LOOT_DESPAWN_TICKS: u32 = 600; // 20 s
pub const DROP_CHANCE_AMMO: f64 = 0.15;
pub const DROP_CHANCE_HEALTH: f64 = 0.10;
pub const DROP_CHANCE_GRENADE: f64 = 0.05;
pub const LOOT_AMMO_AMOUNT: u8 = 24;
pub const LOOT_HEAL_AMOUNT: u8 = 25;

// ── voice chat ─────────────────────────────────────────────────────────────
/// 3D euclidean metres: BIN_VOICE frames relay only to teammates inside this
/// radius of the speaker (authoritative positions).
pub const CHAT_PROXIMITY_RADIUS: f32 = 25.0;

// ── lobby ──────────────────────────────────────────────────────────────────
pub const MAX_PLAYERS: usize = 5;
/// Lobby codes use this unambiguous alphabet (no 0/O/1/I) — ShotAnte's.
pub const LOBBY_CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
pub const LOBBY_CODE_LEN: usize = 5;

#[cfg(test)]
mod director_pacing_tests {
    use super::*;
    use crate::types::EnvKind;

    #[test]
    fn rate_scale_vs_arena_half() {
        let urban = director_rate_scale(30.0);
        assert!((urban - 1.0).abs() < 1e-4, "urban baseline scale=1, got {urban}");
        let small = director_rate_scale(20.0);
        assert!((small - 1.0).abs() < 1e-4, "sub-ref maps clamp to 1");
        let rome = director_rate_scale(250.0);
        assert!(rome > urban, "rome needs more budget than urban");
        assert!(rome <= 3.5 + 1e-4, "rome scale clamped");
        // Monotonic in half
        assert!(director_rate_scale(60.0) > director_rate_scale(30.0));
        assert!(director_rate_scale(120.0) > director_rate_scale(60.0));
    }

    #[test]
    fn approach_dist_caps_first_contact() {
        let urban = director_approach_dist(30.0);
        // urban half 30 → 45.min(27).max(18) = 27
        assert!((18.0..=45.0).contains(&urban));
        let rome = director_approach_dist(250.0);
        assert!((rome - DIRECTOR_APPROACH_DIST_M).abs() < 1e-4);
        // Walkers at 2.2 m/s: approach/speed < 25 s
        let t = rome / 2.2;
        assert!(t < 25.0, "approach walk time {t}s should be < 25s");
    }

    #[test]
    fn env_pressure_table_four_main_envs() {
        let urban = env_pressure(EnvKind::Urban);
        let mountain = env_pressure(EnvKind::MountainTown);
        let desert = env_pressure(EnvKind::DesertTown);
        let sea = env_pressure(EnvKind::SeaTown);
        let rome = env_pressure(EnvKind::RomeEur);
        assert!((urban - 1.0).abs() < 1e-4);
        assert!(desert > urban, "desert open sightlines → higher pressure");
        assert!(sea < urban, "sea chokes → lower pressure");
        assert!(mountain < urban);
        assert!(rome < urban, "rome experiment stays soft");
        // Sum over the four main envs stays in a sane band for tuning audits.
        let total = urban + mountain + desert + sea;
        assert!(
            (3.8..=4.2).contains(&total),
            "four-env pressure total {total} expected ~4.0"
        );
    }

    #[test]
    fn wave_budget_scales_with_wave_and_players() {
        let solo_w1 = wave_budget_points(1, 1, EnvKind::Urban);
        let solo_w3 = wave_budget_points(3, 1, EnvKind::Urban);
        let duo_w1 = wave_budget_points(1, 2, EnvKind::Urban);
        assert!(solo_w3 > solo_w1);
        assert!(duo_w1 > solo_w1);
        // Duo wave 1 should still be approachable (not 3× solo).
        assert!(duo_w1 < solo_w1 * 2.0);
    }
}
