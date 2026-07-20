//! Combat: server-authoritative hitscan against the horde (wall occlusion,
//! head/body spheres, headshot bonus) and grenades (ballistic arc, fuse,
//! linear-falloff radius damage — the ShotAnte barrel math, thrown).

use super::zombies::Zombie;
use zz_core::constants::*;
use zz_core::math::{Vec3, dir_from_angles, nearest_wall_t, ray_sphere};
use zz_core::types::Aabb;

pub struct HitResult {
    /// Index into the zombies vec, if a zombie was struck.
    pub zombie_index: Option<usize>,
    pub damage: f32,
    pub headshot: bool,
    /// Tracer endpoint (wall stop, hit point, or max range).
    pub end: (f32, f32, f32),
}

/// Fire one hitscan ray from a player's eye. Zombies are tested as two
/// spheres (body + head); the nearest sphere hit in front of the first wall
/// wins; headshots deal double.
pub fn fire_hitscan(
    origin: (f32, f32, f32),
    yaw: f32,
    pitch: f32,
    zombies: &[Zombie],
    walls: &[Aabb],
) -> HitResult {
    let o = Vec3::new(origin.0, origin.1, origin.2);
    let d = dir_from_angles(yaw, pitch);
    let wall_t = nearest_wall_t(o, d, walls)
        .unwrap_or(SHOT_RANGE)
        .min(SHOT_RANGE);

    let mut best: Option<(usize, f32, bool)> = None; // (index, t, headshot)
    for (i, z) in zombies.iter().enumerate() {
        let (by, br) = ZOMBIE_BODY_SPHERE;
        let (hy, hr) = ZOMBIE_HEAD_SPHERE;
        let body_c = Vec3::new(z.body.x, z.body.y + by, z.body.z);
        let head_c = Vec3::new(z.body.x, z.body.y + hy, z.body.z);
        for (c, r, is_head) in [(head_c, hr, true), (body_c, br, false)] {
            if let Some(t) = ray_sphere(o, d, c, r)
                && t < wall_t
                && best.is_none_or(|(_, bt, _)| t < bt)
            {
                best = Some((i, t, is_head));
            }
        }
    }

    match best {
        Some((i, t, headshot)) => HitResult {
            zombie_index: Some(i),
            damage: GUN_DAMAGE * if headshot { HEADSHOT_MULTIPLIER } else { 1.0 },
            headshot,
            end: (o.x + d.x * t, o.y + d.y * t, o.z + d.z * t),
        },
        None => HitResult {
            zombie_index: None,
            damage: 0.0,
            headshot: false,
            end: (o.x + d.x * wall_t, o.y + d.y * wall_t, o.z + d.z * wall_t),
        },
    }
}

pub struct Grenade {
    pub id: u8,
    pub owner_slot: u8,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub vx: f32,
    pub vy: f32,
    pub vz: f32,
    pub fuse_left: u32,
    pub resting: bool,
}

impl Grenade {
    pub fn thrown(id: u8, owner_slot: u8, eye: (f32, f32, f32), yaw: f32, pitch: f32) -> Self {
        // slight upward bias so a flat throw still arcs
        let d = dir_from_angles(yaw, pitch);
        Grenade {
            id,
            owner_slot,
            x: eye.0 + d.x * 0.4,
            y: eye.1 + d.y * 0.4,
            z: eye.2 + d.z * 0.4,
            vx: d.x * GRENADE_THROW_SPEED,
            vy: d.y * GRENADE_THROW_SPEED + 3.0,
            vz: d.z * GRENADE_THROW_SPEED,
            fuse_left: GRENADE_FUSE_TICKS,
            resting: false,
        }
    }

    /// Ballistic step with a segment sweep against the walls; on impact the
    /// grenade stops (no bounce — retro-simple and predictable). Returns true
    /// when the fuse has run out (explode now).
    pub fn step(&mut self, walls: &[Aabb], arena_half: f32) -> bool {
        if self.fuse_left > 0 {
            self.fuse_left -= 1;
        }
        if !self.resting {
            let (sx, sy, sz) = (self.x, self.y, self.z);
            let (nx, ny, nz) = (
                sx + self.vx * TICK_DT,
                sy + self.vy * TICK_DT,
                sz + self.vz * TICK_DT,
            );
            let seg_len = ((nx - sx).powi(2) + (ny - sy).powi(2) + (nz - sz).powi(2))
                .sqrt()
                .max(1e-6);
            let d = Vec3::new(
                (nx - sx) / seg_len,
                (ny - sy) / seg_len,
                (nz - sz) / seg_len,
            );
            let hit_t = nearest_wall_t(Vec3::new(sx, sy, sz), d, walls);
            match hit_t {
                Some(t) if t <= seg_len => {
                    // stop just short of the surface
                    self.x = sx + d.x * (t - 0.05).max(0.0);
                    self.y = sy + d.y * (t - 0.05).max(0.0);
                    self.z = sz + d.z * (t - 0.05).max(0.0);
                    self.resting = true;
                }
                _ => {
                    self.x = nx;
                    self.y = ny;
                    self.z = nz;
                    self.vy -= GRAVITY * TICK_DT;
                    if self.y <= 0.12 {
                        self.y = 0.12;
                        self.resting = true;
                    }
                }
            }
            let lim = arena_half - 0.2;
            self.x = self.x.clamp(-lim, lim);
            self.z = self.z.clamp(-lim, lim);
        }
        self.fuse_left == 0
    }
}

/// Linear-falloff explosion damage at distance `d` (the barrel math).
pub fn explosion_damage(d: f32) -> f32 {
    if d > GRENADE_RADIUS {
        return 0.0;
    }
    GRENADE_DMG_MAX - (GRENADE_DMG_MAX - GRENADE_DMG_MIN) * (d / GRENADE_RADIUS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zz_core::types::Body;

    fn zombie_at(x: f32, z: f32) -> Zombie {
        Zombie {
            id: 1,
            kind: super::super::zombies::ZombieKind::Walker,
            body: Body::at(x, z),
            yaw: 0.0,
            health: 100.0,
            state: 0,
            windup_left: 0,
            cooldown_left: 0,
            target_slot: 0,
        }
    }

    #[test]
    fn hitscan_hits_zombie_in_front() {
        // shooter at origin looking -z; zombie 10 m ahead
        let z = zombie_at(0.0, -10.0);
        let r = fire_hitscan((0.0, PLAYER_EYE, 0.0), 0.0, 0.0, &[z], &[]);
        assert_eq!(r.zombie_index, Some(0));
        // flat eye-height shots meet the (larger) body sphere before the head
        assert!(!r.headshot);
        assert_eq!(r.damage, GUN_DAMAGE);
    }

    #[test]
    fn wall_blocks_hitscan() {
        let z = zombie_at(0.0, -10.0);
        let wall = Aabb {
            x0: -2.0,
            x1: 2.0,
            y0: 0.0,
            y1: 3.0,
            z0: -5.5,
            z1: -5.0,
        };
        let r = fire_hitscan((0.0, PLAYER_EYE, 0.0), 0.0, 0.0, &[z], &[wall]);
        assert_eq!(r.zombie_index, None);
        assert!((r.end.2 - -5.0).abs() < 0.01, "tracer stops on the wall");
    }

    #[test]
    fn headshot_when_aiming_high() {
        let z = zombie_at(0.0, -8.0);
        // aim up so the ray passes through the head sphere's center
        let pitch = libm::atan2f(ZOMBIE_HEAD_SPHERE.0 - PLAYER_EYE, 8.0);
        let r = fire_hitscan((0.0, PLAYER_EYE, 0.0), 0.0, pitch, &[z], &[]);
        assert_eq!(r.zombie_index, Some(0));
        assert!(r.headshot);
        assert_eq!(r.damage, GUN_DAMAGE * HEADSHOT_MULTIPLIER);
    }

    #[test]
    fn grenade_arcs_lands_and_fuses() {
        let mut g = Grenade::thrown(1, 0, (0.0, PLAYER_EYE, 0.0), 0.0, 0.3);
        let mut exploded_at = None;
        for i in 0..GRENADE_FUSE_TICKS + 5 {
            if g.step(&[], 30.0) {
                exploded_at = Some(i);
                break;
            }
        }
        assert_eq!(exploded_at, Some(GRENADE_FUSE_TICKS - 1));
        assert!(
            g.resting,
            "grenade should have landed before the fuse ran out"
        );
        assert!(
            g.z < -3.0,
            "grenade should have traveled forward, z={}",
            g.z
        );
    }

    #[test]
    fn explosion_falloff_is_linear() {
        assert_eq!(explosion_damage(0.0), GRENADE_DMG_MAX);
        assert_eq!(explosion_damage(GRENADE_RADIUS), GRENADE_DMG_MIN);
        assert_eq!(explosion_damage(GRENADE_RADIUS + 0.1), 0.0);
        let mid = explosion_damage(GRENADE_RADIUS / 2.0);
        assert!((mid - (GRENADE_DMG_MAX + GRENADE_DMG_MIN) / 2.0).abs() < 1e-3);
    }
}
