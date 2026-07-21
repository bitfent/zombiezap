//! Procedural articulated rigs for zombies, survivors, and the first-person
//! rifle viewmodel. Zero asset files: shared cuboid meshes + a small material
//! bank so hundreds of rigged zombies stay cheap (one mesh handle per part
//! family, kind/slot materials shared across the horde).
//!
//! Hierarchy (Minecraft-style pivots): root → body → {torso, head, eyes?,
//! limb pivots → hanging cuboids}. Joint rotation swings limbs; the root
//! carries world translation/yaw from interpolation.

use std::f32::consts::{FRAC_PI_2, TAU};

use bevy::prelude::*;

// ── shared geometry proportions (metres, upright rest pose) ────────────────
// Zombie blocks intentionally read larger / more menacing than survivors:
// bigger head+jaw, hunched torso, longer arms (stranger-test at mid-range).

const TORSO_W: f32 = 0.42;
const TORSO_H: f32 = 0.62;
const TORSO_D: f32 = 0.26;
const TORSO_CY: f32 = 1.14; // torso centre Y

/// Survivor head (players stay human-proportioned).
const HEAD_W: f32 = 0.34;
const HEAD_H: f32 = 0.34;
const HEAD_D: f32 = 0.32;
const HEAD_CY: f32 = 1.62;

/// Zombie head: larger block + deeper jaw for silhouette at 15–30 m.
const Z_HEAD_W: f32 = 0.42;
const Z_HEAD_H: f32 = 0.40;
const Z_HEAD_D: f32 = 0.40;
const Z_HEAD_CY: f32 = 1.58;
const Z_JAW_W: f32 = 0.36;
const Z_JAW_H: f32 = 0.14;
const Z_JAW_D: f32 = 0.28;
const Z_JAW_CY: f32 = 1.36;
/// Slight forward pitch on the body pivot for a permanent hunch.
pub const ZOMBIE_HUNCH_PITCH: f32 = 0.28;

const LEG_W: f32 = 0.17;
const LEG_LEN: f32 = 0.84;
const LEG_JOINT_Y: f32 = 0.86;
const LEG_X: f32 = 0.12;

const ARM_W: f32 = 0.14;
const ARM_LEN: f32 = 0.62;
const ARM_JOINT_Y: f32 = 1.42;
const ARM_X: f32 = 0.27;
/// Zombie arms hang longer than human (reach + silhouette).
const Z_ARM_LEN: f32 = 0.78;
const Z_ARM_W: f32 = 0.15;

/// Zombie eyes: larger + further forward so emissive reads at 30 m+.
const Z_EYE_S: f32 = 0.09;
const Z_EYE_Y: f32 = 1.64;
const Z_EYE_Z: f32 = -0.22;
const Z_EYE_X: f32 = 0.10;

/// Max limb swing |angle| (radians) at any speed — tests assert against this.
pub const MAX_LIMB_SWING: f32 = 0.85;

/// Death crumple duration in seconds.
pub const CRUMPLE_DURATION_S: f32 = 0.55;

/// Hit-flash lifetime (unique material instances — README note 23).
pub const HIT_FLASH_S: f32 = 0.09;

// ── animation LOD distances (metres from camera) ───────────────────────────
// Hysteresis: enter and exit bands differ so zombies near a threshold don't
// flap Full↔Bob↔Static every frame (M15 thrash: per-frame sort + visibility
// writes invalidated batches and tanked FPS).
/// Full limb walk cycle — enter when closer than this.
pub const LOD_FULL_M: f32 = 40.0;
/// Leave Full for Bob when farther than this.
pub const LOD_FULL_EXIT_M: f32 = 48.0;
/// Bob band outer enter (Static → Bob when closer than this).
pub const LOD_BOB_M: f32 = 80.0;
/// Leave Bob for Static when farther than this.
pub const LOD_BOB_EXIT_M: f32 = 92.0;
/// Soft cap: only this many nearest zombies keep full limb animation; the rest
/// drop to bob/static even if inside FULL range (horde frame budget).
pub const LOD_FULL_CAP: usize = 40;
/// Recompute camera distance / rank at most every N frames (staggered by id).
pub const LOD_DIST_PERIOD: u32 = 10;

// ── pure anim math (unit-tested) ───────────────────────────────────────────

/// Deterministic walk-cycle phase offset from a zombie/player id.
/// Same id → same phase; different ids → different phases (no lockstep horde).
pub fn phase_offset_for_id(id: u16) -> f32 {
    // Multiplicative hash into [0, TAU).
    let h = (id as u32).wrapping_mul(0x9E37_79B9).wrapping_add(0x85EB_CA6B);
    (h as f32 / u32::MAX as f32) * TAU
}

/// Kind → root scale. Walker 1×, runner gaunt, brute 1.5× bulk.
pub fn kind_scale(kind: u8) -> Vec3 {
    match kind {
        1 => Vec3::new(0.72, 0.95, 0.72), // runner: lean
        2 => Vec3::new(1.5, 1.35, 1.5),    // brute: bulk
        _ => Vec3::ONE,                   // walker
    }
}

/// Kind → high-contrast body colour (walker / runner / brute silhouettes).
/// Boosted separation so kinds read at mid-range under sun-bleached fog.
pub fn kind_color(kind: u8) -> Color {
    match kind {
        1 => Color::srgb(0.72, 0.78, 0.18), // runner: sickly chartreuse
        2 => Color::srgb(0.14, 0.22, 0.12), // brute: near-black bulk
        _ => Color::srgb(0.22, 0.55, 0.20), // walker: saturated rotten green
    }
}

/// Per-kind silhouette scale factors (head bulk × arm length) for tests /
/// stranger-test documentation. Totals must stay kind-distinct.
pub fn silhouette_scale_table(kind: u8) -> (f32, f32) {
    match kind {
        1 => (0.95, 1.15), // runner: slightly smaller head, long arms
        2 => (1.35, 1.20), // brute: massive head block
        _ => (1.15, 1.25), // walker: enlarged head + long arms vs human
    }
}

/// Attack telegraph root scale pulse (1.0 = rest). Peaks early in windup.
pub fn telegraph_scale_pulse(attacking: bool, phase: f32) -> f32 {
    if !attacking {
        return 1.0;
    }
    // Gentle breathe 1.00 → 1.08 so arms-up pose reads at 15 m.
    1.0 + 0.08 * (0.5 + 0.5 * (phase * 6.0).sin())
}

/// Animation LOD band from distance (metres).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimLod {
    /// Full limb walk cycle.
    Full,
    /// Vertical bob only (no limb joint writes).
    Bob,
    /// Frozen pose (impostor / far tail).
    Static,
}

/// Distance → LOD band (before the near-cap is applied). Cold start / no prior.
pub fn anim_lod_for_distance(dist: f32) -> AnimLod {
    if dist <= LOD_FULL_M {
        AnimLod::Full
    } else if dist <= LOD_BOB_M {
        AnimLod::Bob
    } else {
        AnimLod::Static
    }
}

/// Hysteretic LOD transition so boundary zombies don't flap every frame.
///
/// Enter Full ≤ [`LOD_FULL_M`], exit Full > [`LOD_FULL_EXIT_M`];
/// enter Bob from Static ≤ [`LOD_BOB_M`], exit Bob → Static > [`LOD_BOB_EXIT_M`].
pub fn anim_lod_hysteresis(prev: AnimLod, dist: f32) -> AnimLod {
    match prev {
        AnimLod::Full => {
            if dist > LOD_FULL_EXIT_M {
                AnimLod::Bob
            } else {
                AnimLod::Full
            }
        }
        AnimLod::Bob => {
            if dist <= LOD_FULL_M {
                AnimLod::Full
            } else if dist > LOD_BOB_EXIT_M {
                AnimLod::Static
            } else {
                AnimLod::Bob
            }
        }
        AnimLod::Static => {
            if dist <= LOD_BOB_M {
                AnimLod::Bob
            } else {
                AnimLod::Static
            }
        }
    }
}

/// Cadence multiplier (phase advance rate) per kind — runners faster, brutes slow.
pub fn kind_cadence(kind: u8) -> f32 {
    match kind {
        1 => 1.55, // runner: fast cadence
        2 => 0.65, // brute: heavy sway
        _ => 1.0,
    }
}

/// Limb swing angle (radians) from walk phase + speed.
/// Legs and opposite arms alternate via `sign` (±1). Bounded by [`MAX_LIMB_SWING`].
pub fn limb_swing_angle(phase: f32, speed: f32, kind: u8, sign: f32) -> f32 {
    let speed_amp = (speed * 0.14).min(0.75);
    let kind_amp = match kind {
        1 => 0.95, // runner: larger stride feel via speed already high
        2 => 0.70, // brute: heavy but not floppy
        _ => 0.85,
    };
    let amp = (0.12 + speed_amp * kind_amp).min(MAX_LIMB_SWING);
    let angle = phase.sin() * amp * sign;
    angle.clamp(-MAX_LIMB_SWING, MAX_LIMB_SWING)
}

/// Attack telegraph: arms raise toward this local X rotation (radians).
/// Stronger raise so the windup silhouette reads at ~15 m.
pub fn attack_arm_raise() -> f32 {
    -1.45 // arms up/forward toward the player
}

/// Crumple pose at normalized time `t ∈ [0, 1]`.
/// Returns (body pitch, body roll, body y drop). At `t ≥ 1` the body is at rest
/// on the ground (fully collapsed).
pub fn crumple_pose(t: f32) -> (f32, f32, f32) {
    let t = t.clamp(0.0, 1.0);
    // Ease-out collapse: quick drop then settle.
    let e = 1.0 - (1.0 - t).powi(2);
    let pitch = e * (FRAC_PI_2 * 0.92); // face-plant-ish
    let roll = e * 0.55;
    let y_drop = e * 0.55;
    (pitch, roll, y_drop)
}

/// Survivor slot palette (matches the existing remote-player colours).
pub fn slot_color(slot: u8) -> Color {
    const PALETTE: [Color; 5] = [
        Color::srgb(0.95, 0.35, 0.35),
        Color::srgb(0.35, 0.55, 0.95),
        Color::srgb(0.95, 0.85, 0.30),
        Color::srgb(0.65, 0.40, 0.90),
        Color::srgb(0.35, 0.90, 0.75),
    ];
    PALETTE[slot as usize % PALETTE.len()]
}

// ── shared GPU assets ──────────────────────────────────────────────────────

/// Shared meshes + materials for every rigged entity and the viewmodel.
///
/// **Batching contract (horde perf):** every zombie of kind K reuses the same
/// `zombie_mats[K]` + `unit_cube` + `eye_mat` handles. Only hit-flash FX clones
/// a material (README note 23). Do not add per-zombie mesh/material at spawn.
#[derive(Resource)]
pub struct RigAssets {
    pub unit_cube: Handle<Mesh>,
    /// Walker / runner / brute body materials (shared across the horde).
    pub zombie_mats: [Handle<StandardMaterial>; 3],
    /// Glowing eyes — one handle for all zombies (high emissive for 30 m+).
    pub eye_mat: Handle<StandardMaterial>,
    pub player_mats: Vec<Handle<StandardMaterial>>,
    pub gun_mat: Handle<StandardMaterial>,
    /// Base colour for muzzle-flash clones (each flash gets a unique handle).
    pub muzzle_flash_color: Color,
}

impl RigAssets {
    pub fn build(
        meshes: &mut Assets<Mesh>,
        materials: &mut Assets<StandardMaterial>,
    ) -> Self {
        let entity_mat = |c: Color, emissive_scale: f32| StandardMaterial {
            base_color: c,
            emissive: c.to_linear() * emissive_scale,
            perceptual_roughness: 0.78,
            metallic: 0.0,
            ..default()
        };

        let zombie_mats = [
            materials.add(entity_mat(kind_color(0), 0.14)),
            materials.add(entity_mat(kind_color(1), 0.18)),
            materials.add(entity_mat(kind_color(2), 0.10)),
        ];

        // Hot emissive eyes — readable through fog at 30 m+ without bloom.
        let eye_mat = materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 0.12, 0.05),
            emissive: LinearRgba::rgb(18.0, 1.2, 0.25),
            perceptual_roughness: 1.0,
            metallic: 0.0,
            ..default()
        });

        let player_mats: Vec<_> = (0..5u8)
            .map(|s| materials.add(entity_mat(slot_color(s), 0.10)))
            .collect();

        let gun_mat = materials.add(StandardMaterial {
            base_color: Color::srgb(0.14, 0.15, 0.18),
            emissive: LinearRgba::rgb(0.02, 0.02, 0.025),
            perceptual_roughness: 0.55,
            metallic: 0.35,
            ..default()
        });

        Self {
            unit_cube: meshes.add(Cuboid::new(1.0, 1.0, 1.0)),
            zombie_mats,
            eye_mat,
            player_mats,
            gun_mat,
            muzzle_flash_color: Color::srgba(1.0, 0.85, 0.35, 0.95),
        }
    }

    pub fn body_mat(&self, is_zombie: bool, kind_or_slot: u8) -> Handle<StandardMaterial> {
        if is_zombie {
            self.zombie_mats[kind_or_slot.min(2) as usize].clone()
        } else {
            self.player_mats[kind_or_slot as usize % self.player_mats.len()].clone()
        }
    }
}

// ── components ─────────────────────────────────────────────────────────────

/// Joint pivots + walk state driven each frame from interpolated motion.
#[derive(Component)]
pub struct HumanoidRig {
    pub body: Entity,
    pub left_arm: Entity,
    pub right_arm: Entity,
    pub left_leg: Entity,
    pub right_leg: Entity,
    /// Full articulated group (hidden at Static impostor LOD).
    pub detailed: Entity,
    /// Single shared-mesh cuboid shown at Static LOD (far tail).
    pub impostor: Entity,
    /// Mesh entities that use the body material (for hit-flash restore).
    pub body_parts: Vec<Entity>,
    pub phase: f32,
    pub phase_offset: f32,
    pub speed: f32,
    pub last_pos: Option<Vec3>,
    /// Zombie kind 0/1/2, or player slot for survivors.
    pub kind: u8,
    pub is_zombie: bool,
    /// Snapshot `state == 1` attack telegraph.
    pub attacking: bool,
    /// Current animation LOD (hysteretic; refreshed every [`LOD_DIST_PERIOD`]).
    pub lod: AnimLod,
    /// Cached camera distance used between LOD refresh frames.
    pub lod_dist: f32,
    /// Frames until next distance / LOD recompute (0 = do it now).
    pub lod_refresh_in: u8,
}

/// Brief white flash on a hit zombie — unique material handles (note 23).
#[derive(Component)]
pub struct HitFlash {
    pub age: f32,
    /// (entity, original material) to restore when the flash ends.
    pub restore: Vec<(Entity, Handle<StandardMaterial>)>,
}

/// Short-lived death tumble — does not participate in net bookkeeping.
#[derive(Component)]
pub struct CrumpleFx {
    pub age: f32,
    pub body: Entity,
}

/// First-person rifle parented to the 3D camera.
#[derive(Component)]
pub struct Viewmodel {
    pub recoil: f32,
    pub flash_age: f32,
    pub flash_entity: Entity,
    pub flash_mat: Handle<StandardMaterial>,
    pub rest_translation: Vec3,
}

// ── spawning ───────────────────────────────────────────────────────────────

fn part(
    parent: &mut ChildSpawnerCommands,
    mesh: Handle<Mesh>,
    mat: Handle<StandardMaterial>,
    translation: Vec3,
    scale: Vec3,
) {
    parent.spawn((
        Mesh3d(mesh),
        MeshMaterial3d(mat),
        Transform::from_translation(translation).with_scale(scale),
    ));
}

/// Spawn a hanging limb: pivot at `joint`, cuboid of height `len` hanging -Y.
fn limb_pivot(
    parent: &mut ChildSpawnerCommands,
    mesh: Handle<Mesh>,
    mat: Handle<StandardMaterial>,
    joint: Vec3,
    width: f32,
    len: f32,
) -> Entity {
    parent
        .spawn((Transform::from_translation(joint), Visibility::default()))
        .with_children(|p| {
            part(
                p,
                mesh,
                mat,
                Vec3::new(0.0, -len * 0.5, 0.0),
                Vec3::new(width, len, width),
            );
        })
        .id()
}

/// Build a full humanoid under `root` (already spawned). Returns the rig component.
///
/// Hierarchy: root → detailed{ body{torso,head,jaw?,eyes?,limbs} } + impostor.
/// Shared mesh/material handles only — see [`RigAssets`] batching contract.
pub fn attach_humanoid(
    root: &mut EntityCommands,
    assets: &RigAssets,
    is_zombie: bool,
    kind_or_slot: u8,
    id_for_phase: u16,
) -> HumanoidRig {
    let mat = assets.body_mat(is_zombie, kind_or_slot);
    let cube = assets.unit_cube.clone();
    let eye_mat = assets.eye_mat.clone();
    let (sil_head, _sil_arm) = silhouette_scale_table(if is_zombie {
        kind_or_slot.min(2)
    } else {
        0
    });

    let mut body_e = Entity::PLACEHOLDER;
    let mut left_arm = Entity::PLACEHOLDER;
    let mut right_arm = Entity::PLACEHOLDER;
    let mut left_leg = Entity::PLACEHOLDER;
    let mut right_leg = Entity::PLACEHOLDER;
    let mut detailed_e = Entity::PLACEHOLDER;
    let mut impostor_e = Entity::PLACEHOLDER;
    let mut body_parts: Vec<Entity> = Vec::new();

    let arm_len = if is_zombie { Z_ARM_LEN } else { ARM_LEN };
    let arm_w = if is_zombie { Z_ARM_W } else { ARM_W };
    let (head_w, head_h, head_d, head_cy) = if is_zombie {
        (
            Z_HEAD_W * sil_head,
            Z_HEAD_H * sil_head,
            Z_HEAD_D * sil_head,
            Z_HEAD_CY,
        )
    } else {
        (HEAD_W, HEAD_H, HEAD_D, HEAD_CY)
    };

    root.with_children(|root_c| {
        // Far-LOD impostor: one shared cuboid (hidden until Static band).
        impostor_e = root_c
            .spawn((
                Mesh3d(cube.clone()),
                MeshMaterial3d(mat.clone()),
                Transform::from_translation(Vec3::new(0.0, 0.95, 0.0))
                    .with_scale(Vec3::new(0.55, 1.75, 0.40)),
                Visibility::Hidden,
            ))
            .id();

        let detailed_id = root_c
            .spawn((Transform::IDENTITY, Visibility::default()))
            .with_children(|det| {
                let body_id = det
                    .spawn((
                        // Permanent hunch on zombies for mid-range menace.
                        Transform::from_rotation(if is_zombie {
                            Quat::from_rotation_x(ZOMBIE_HUNCH_PITCH)
                        } else {
                            Quat::IDENTITY
                        }),
                        Visibility::default(),
                    ))
                    .with_children(|body| {
                        // Torso
                        body_parts.push(
                            body.spawn((
                                Mesh3d(cube.clone()),
                                MeshMaterial3d(mat.clone()),
                                Transform::from_translation(Vec3::new(0.0, TORSO_CY, 0.0))
                                    .with_scale(Vec3::new(TORSO_W, TORSO_H, TORSO_D)),
                            ))
                            .id(),
                        );
                        // Head
                        body_parts.push(
                            body.spawn((
                                Mesh3d(cube.clone()),
                                MeshMaterial3d(mat.clone()),
                                Transform::from_translation(Vec3::new(0.0, head_cy, 0.0))
                                    .with_scale(Vec3::new(head_w, head_h, head_d)),
                            ))
                            .id(),
                        );
                        if is_zombie {
                            // Jaw block — deep silhouette under the head.
                            body_parts.push(
                                body.spawn((
                                    Mesh3d(cube.clone()),
                                    MeshMaterial3d(mat.clone()),
                                    Transform::from_translation(Vec3::new(0.0, Z_JAW_CY, -0.04))
                                        .with_scale(Vec3::new(Z_JAW_W, Z_JAW_H, Z_JAW_D)),
                                ))
                                .id(),
                            );
                            // Emissive eyes — large, forward, shared eye mat.
                            body.spawn((
                                Mesh3d(cube.clone()),
                                MeshMaterial3d(eye_mat.clone()),
                                Transform::from_translation(Vec3::new(-Z_EYE_X, Z_EYE_Y, Z_EYE_Z))
                                    .with_scale(Vec3::splat(Z_EYE_S)),
                            ));
                            body.spawn((
                                Mesh3d(cube.clone()),
                                MeshMaterial3d(eye_mat),
                                Transform::from_translation(Vec3::new(Z_EYE_X, Z_EYE_Y, Z_EYE_Z))
                                    .with_scale(Vec3::splat(Z_EYE_S)),
                            ));
                        }

                        left_leg = limb_pivot(
                            body,
                            cube.clone(),
                            mat.clone(),
                            Vec3::new(-LEG_X, LEG_JOINT_Y, 0.0),
                            LEG_W,
                            LEG_LEN,
                        );
                        right_leg = limb_pivot(
                            body,
                            cube.clone(),
                            mat.clone(),
                            Vec3::new(LEG_X, LEG_JOINT_Y, 0.0),
                            LEG_W,
                            LEG_LEN,
                        );
                        left_arm = limb_pivot(
                            body,
                            cube.clone(),
                            mat.clone(),
                            Vec3::new(-ARM_X, ARM_JOINT_Y, 0.0),
                            arm_w,
                            arm_len,
                        );
                        right_arm = limb_pivot(
                            body,
                            cube.clone(),
                            mat.clone(),
                            Vec3::new(ARM_X, ARM_JOINT_Y, 0.0),
                            arm_w,
                            arm_len,
                        );
                    })
                    .id();
                body_e = body_id;
            })
            .id();
        detailed_e = detailed_id;
    });

    HumanoidRig {
        body: body_e,
        left_arm,
        right_arm,
        left_leg,
        right_leg,
        detailed: detailed_e,
        impostor: impostor_e,
        body_parts,
        phase: 0.0,
        phase_offset: phase_offset_for_id(id_for_phase),
        speed: 0.0,
        last_pos: None,
        kind: kind_or_slot,
        is_zombie,
        attacking: false,
        lod: AnimLod::Full,
        lod_dist: 0.0,
        // Stagger first refresh by phase hash so the horde doesn't all recompute
        // on the same frame after a mass spawn.
        lod_refresh_in: (id_for_phase % LOD_DIST_PERIOD as u16) as u8,
    }
}

/// Spawn a short-lived crumple copy at the dead zombie's last pose.
/// Caller has already despawned (or will despawn) the live net entity.
pub fn spawn_crumple(
    commands: &mut Commands,
    assets: &RigAssets,
    pos: Vec3,
    yaw: f32,
    scale: Vec3,
    kind: u8,
) {
    let mut root = commands.spawn((
        Transform::from_translation(pos)
            .with_rotation(Quat::from_rotation_y(yaw))
            .with_scale(scale),
        Visibility::default(),
    ));
    let rig = attach_humanoid(&mut root, assets, true, kind, 0);
    let body = rig.body;
    root.insert(CrumpleFx { age: 0.0, body });
    // Leave limbs in a slightly splayed rest so the tumble reads as a body.
    let _ = rig;
}

/// Joint angles/offsets for one frame of walk/attack animation.
#[derive(Clone, Copy, Debug)]
pub struct RigPose {
    pub body_y: f32,
    pub left_arm_x: f32,
    pub right_arm_x: f32,
    pub left_leg_x: f32,
    pub right_leg_x: f32,
    /// Root uniform scale multiplier (telegraph pulse).
    pub root_scale: f32,
    /// Whether limb joint rotations should be written this frame.
    pub write_limbs: bool,
}

/// Advance walk-cycle state from interpolated root position; return joint pose.
///
/// `lod` selects how much work to do (Full / Bob / Static). Bob still advances
/// phase for continuity when the zombie re-enters Full range.
pub fn compute_rig_pose(rig: &mut HumanoidRig, pos: Vec3, dt: f32, lod: AnimLod) -> RigPose {
    rig.lod = lod;

    if lod != AnimLod::Static {
        if let Some(prev) = rig.last_pos {
            let raw = Vec3::new(pos.x - prev.x, 0.0, pos.z - prev.z).length() / dt.max(1e-4);
            let target = raw.min(9.0);
            // Low-pass so interp jitter doesn't stutter the gait.
            let ease = (dt * 12.0).min(1.0);
            rig.speed += (target - rig.speed) * ease;
        }
        rig.last_pos = Some(pos);
    }

    let kind = if rig.is_zombie { rig.kind } else { 0 };
    let cadence = if rig.is_zombie {
        kind_cadence(kind)
    } else {
        1.1 // survivors: subtle, slightly snappy gait
    };

    if lod != AnimLod::Static && rig.speed > 0.25 {
        rig.phase += rig.speed * dt * 2.4 * cadence;
    }

    let walk_phase = rig.phase + rig.phase_offset;
    let root_scale = if rig.is_zombie {
        telegraph_scale_pulse(rig.attacking, walk_phase)
    } else {
        1.0
    };

    if lod == AnimLod::Static {
        return RigPose {
            body_y: 0.0,
            left_arm_x: 0.0,
            right_arm_x: 0.0,
            left_leg_x: 0.0,
            right_leg_x: 0.0,
            root_scale,
            write_limbs: false,
        };
    }

    let swing = if rig.speed > 0.35 {
        1.0
    } else {
        (rig.speed / 0.35).clamp(0.0, 1.0)
    };

    // Survivor walks are subtler.
    let survivor_scale = if rig.is_zombie { 1.0 } else { 0.55 };

    let bob = if rig.speed > 0.35 {
        walk_phase.sin().abs() * 0.04 * swing * survivor_scale
    } else {
        0.0
    };

    if lod == AnimLod::Bob {
        return RigPose {
            body_y: bob,
            left_arm_x: 0.0,
            right_arm_x: 0.0,
            left_leg_x: 0.0,
            right_leg_x: 0.0,
            root_scale,
            write_limbs: false,
        };
    }

    let leg_l = limb_swing_angle(walk_phase, rig.speed, kind, 1.0) * swing * survivor_scale;
    let leg_r = limb_swing_angle(walk_phase, rig.speed, kind, -1.0) * swing * survivor_scale;
    let (arm_l, arm_r) = if rig.attacking && rig.is_zombie {
        let raise = attack_arm_raise();
        (raise, raise)
    } else {
        (
            limb_swing_angle(walk_phase, rig.speed, kind, -1.0) * swing * survivor_scale * 0.85,
            limb_swing_angle(walk_phase, rig.speed, kind, 1.0) * swing * survivor_scale * 0.85,
        )
    };

    RigPose {
        body_y: bob,
        left_arm_x: arm_l,
        right_arm_x: arm_r,
        left_leg_x: leg_l,
        right_leg_x: leg_r,
        root_scale,
        write_limbs: true,
    }
}

/// Apply unique flash materials; returns restore list for [`HitFlash`].
pub fn apply_hit_flash_materials(
    materials: &mut Assets<StandardMaterial>,
    body_parts: &[Entity],
    mesh_mats: &mut Query<&mut MeshMaterial3d<StandardMaterial>>,
) -> Vec<(Entity, Handle<StandardMaterial>)> {
    let mut restore = Vec::with_capacity(body_parts.len());
    for &e in body_parts {
        let Ok(mut mesh_mat) = mesh_mats.get_mut(e) else {
            continue;
        };
        let original = mesh_mat.0.clone();
        let flash = materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 1.0, 1.0),
            emissive: LinearRgba::rgb(5.0, 5.0, 5.0),
            perceptual_roughness: 0.55,
            metallic: 0.0,
            ..default()
        });
        mesh_mat.0 = flash;
        restore.push((e, original));
    }
    restore
}

/// Restore shared materials after a hit flash; remove unique handles.
pub fn end_hit_flash(
    materials: &mut Assets<StandardMaterial>,
    flash: &HitFlash,
    mesh_mats: &mut Query<&mut MeshMaterial3d<StandardMaterial>>,
) {
    for (e, original) in &flash.restore {
        if let Ok(mut mesh_mat) = mesh_mats.get_mut(*e) {
            // Drop the unique flash handle by swapping back to shared.
            let old = std::mem::replace(&mut mesh_mat.0, original.clone());
            materials.remove(old.id());
        }
    }
}

/// Advance a crumple entity; returns true when it should despawn.
pub fn tick_crumple(fx: &mut CrumpleFx, body_tf: &mut Transform, dt: f32) -> bool {
    fx.age += dt;
    let t = (fx.age / CRUMPLE_DURATION_S).clamp(0.0, 1.0);
    let (pitch, roll, y_drop) = crumple_pose(t);
    body_tf.rotation = Quat::from_euler(EulerRot::XYZ, pitch, 0.0, roll);
    body_tf.translation.y = -y_drop;
    fx.age >= CRUMPLE_DURATION_S
}

// ── first-person viewmodel ─────────────────────────────────────────────────

/// Local camera-space rest pose: bottom-right boxy rifle.
pub const VM_REST: Vec3 = Vec3::new(0.28, -0.28, -0.55);

/// Small rest yaw on the viewmodel root (matches spawn).
pub const VM_REST_YAW: f32 = 0.05;

/// Muzzle tip in viewmodel-local space — barrel end / flash quad translation.
///
/// Barrel cuboid centre z=-0.48, half-depth 0.19 → tip ≈ z=-0.67; flash sits
/// just past the tip at z=-0.72 so tracers and the flash share one origin.
pub const VIEWMODEL_MUZZLE_LOCAL: Vec3 = Vec3::new(0.0, 0.02, -0.72);

/// Legacy remote-player muzzle drop below eye (metres) when the TP rig has no
/// carried gun — matches ShotAnte `addTracer` `from.y - 0.12`.
pub const REMOTE_MUZZLE_DROP: f32 = 0.12;

/// Camera-space offset of the muzzle tip at rest (no recoil kick).
///
/// Viewmodel is a camera child: `VM_REST` + rest-yaw × local muzzle tip.
pub fn viewmodel_muzzle_camera_offset() -> Vec3 {
    let rest_rot = Quat::from_rotation_y(VM_REST_YAW);
    VM_REST + rest_rot * VIEWMODEL_MUZZLE_LOCAL
}

/// World-space muzzle tip from a camera world transform and a camera-space
/// muzzle offset (from [`viewmodel_muzzle_camera_offset`]).
///
/// Pure: identity camera → offset as-is; rotated camera → offset spun with
/// the camera. Visual origin only — server hitscan stays eye-based.
pub fn muzzle_world_from_camera(camera: &Transform, camera_space_muzzle: Vec3) -> Vec3 {
    camera.translation + camera.rotation * camera_space_muzzle
}

/// World-space origin for a remote player's tracer when the survivor rig does
/// not carry a gun: feet + (eye height − [`REMOTE_MUZZLE_DROP`]).
pub fn remote_muzzle_from_feet(feet: Vec3, eye_height: f32) -> Vec3 {
    feet + Vec3::Y * (eye_height - REMOTE_MUZZLE_DROP)
}

/// Spawn the rifle as a child of the camera entity. Returns the viewmodel entity.
pub fn spawn_viewmodel(
    commands: &mut Commands,
    camera: Entity,
    assets: &RigAssets,
    materials: &mut Assets<StandardMaterial>,
) -> Entity {
    let cube = assets.unit_cube.clone();
    let gun = assets.gun_mat.clone();

    // Unique flash material (alpha animated independently — README item 23).
    let flash_mat = materials.add(StandardMaterial {
        base_color: assets.muzzle_flash_color,
        emissive: LinearRgba::rgb(6.0, 4.0, 0.8),
        perceptual_roughness: 1.0,
        metallic: 0.0,
        alpha_mode: AlphaMode::Blend,
        ..default()
    });
    let flash_mat_h = flash_mat.clone();

    let mut flash_entity = Entity::PLACEHOLDER;

    let vm = commands
        .spawn((
            Transform::from_translation(VM_REST)
                .with_rotation(Quat::from_rotation_y(VM_REST_YAW)),
            Visibility::default(),
            ChildOf(camera),
        ))
        .with_children(|c| {
            // Receiver
            part(
                c,
                cube.clone(),
                gun.clone(),
                Vec3::new(0.0, 0.0, -0.12),
                Vec3::new(0.07, 0.11, 0.42),
            );
            // Barrel (forward = -Z)
            part(
                c,
                cube.clone(),
                gun.clone(),
                Vec3::new(0.0, 0.02, -0.48),
                Vec3::new(0.04, 0.04, 0.38),
            );
            // Magazine
            part(
                c,
                cube.clone(),
                gun.clone(),
                Vec3::new(0.0, -0.10, -0.08),
                Vec3::new(0.05, 0.14, 0.08),
            );
            // Stock
            part(
                c,
                cube.clone(),
                gun.clone(),
                Vec3::new(0.0, -0.02, 0.18),
                Vec3::new(0.06, 0.10, 0.18),
            );
            // Front sight
            part(
                c,
                cube.clone(),
                gun.clone(),
                Vec3::new(0.0, 0.07, -0.55),
                Vec3::new(0.02, 0.05, 0.02),
            );
            // Muzzle flash at the barrel tip (same local as VIEWMODEL_MUZZLE_LOCAL)
            flash_entity = c
                .spawn((
                    Mesh3d(cube),
                    MeshMaterial3d(flash_mat),
                    Transform::from_translation(VIEWMODEL_MUZZLE_LOCAL)
                        .with_scale(Vec3::new(0.12, 0.12, 0.04)),
                    Visibility::Hidden,
                ))
                .id();
        })
        .id();

    commands.entity(vm).insert(Viewmodel {
        recoil: 0.0,
        flash_age: 99.0, // start hidden
        flash_entity,
        flash_mat: flash_mat_h,
        rest_translation: VM_REST,
    });

    vm
}

/// Kick recoil + muzzle flash (own Shot).
pub fn viewmodel_on_shot(vm: &mut Viewmodel) {
    vm.recoil = 1.0;
    vm.flash_age = 0.0;
}

/// Decay recoil and flash; update transforms/visibility/alpha.
pub fn tick_viewmodel(
    vm: &mut Viewmodel,
    vm_tf: &mut Transform,
    flash_vis: Option<&mut Visibility>,
    flash_mat: Option<&mut StandardMaterial>,
    dt: f32,
) {
    vm.recoil = (vm.recoil - dt * 10.0).max(0.0);
    vm.flash_age += dt;

    // Recoil: kick back (+Z toward camera) and up slightly.
    let kick = vm.recoil;
    vm_tf.translation = vm.rest_translation + Vec3::new(0.0, kick * 0.04, kick * 0.08);
    vm_tf.rotation = Quat::from_euler(EulerRot::YXZ, 0.05, -kick * 0.12, kick * 0.04);

    const FLASH_LIFE: f32 = 0.06;
    let flashing = vm.flash_age < FLASH_LIFE;
    if let Some(vis) = flash_vis {
        *vis = if flashing {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    if flashing && let Some(mat) = flash_mat {
        let a = 1.0 - (vm.flash_age / FLASH_LIFE);
        mat.base_color = Color::srgba(1.0, 0.85, 0.35, 0.95 * a);
        mat.emissive = LinearRgba::rgb(6.0 * a, 4.0 * a, 0.8 * a);
    }
}

// ── proximity growl trigger (pure) ─────────────────────────────────────────

/// Whether zombie `id` should growl in this half-second time bucket.
/// Deterministic (no rand); ~5% of nearby zombies per bucket.
pub fn should_growl(id: u16, time_secs: f64) -> bool {
    let bucket = (time_secs * 2.0).floor() as u32;
    let h = (id as u32)
        .wrapping_mul(0x045D_9F3B)
        .wrapping_add(bucket.wrapping_mul(0x27D4_EB2D));
    // Sparse: about 1 in 18 buckets for a given id ≈ occasional.
    h.is_multiple_of(18)
}

/// Max distance for proximity growls (metres).
pub const GROWL_RANGE: f32 = 16.0;

// ── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_offset_deterministic_and_varied() {
        let a = phase_offset_for_id(7);
        let b = phase_offset_for_id(7);
        assert!((a - b).abs() < 1e-6, "same id → same phase");
        let c = phase_offset_for_id(8);
        assert!((a - c).abs() > 0.05, "different ids → different phase");
        // Many ids should spread across the circle.
        let mut phases: Vec<f32> = (0u16..32).map(phase_offset_for_id).collect();
        phases.sort_by(|x, y| x.partial_cmp(y).unwrap());
        let span = phases.last().unwrap() - phases.first().unwrap();
        assert!(
            span > std::f32::consts::PI,
            "phase offsets should span a wide range, span={span}"
        );
    }

    #[test]
    fn limb_swing_bounded_at_any_speed() {
        for kind in [0u8, 1, 2] {
            for speed in [0.0_f32, 0.5, 2.0, 6.0, 20.0, 100.0] {
                for phase in [0.0_f32, 0.5, 1.0, 2.0, std::f32::consts::PI, TAU * 0.75] {
                    for sign in [1.0_f32, -1.0] {
                        let a = limb_swing_angle(phase, speed, kind, sign);
                        assert!(
                            a.abs() <= MAX_LIMB_SWING + 1e-5,
                            "angle {a} exceeds max at speed={speed} kind={kind}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn muzzle_world_identity_camera() {
        let offset = viewmodel_muzzle_camera_offset();
        let cam = Transform::IDENTITY;
        let world = muzzle_world_from_camera(&cam, offset);
        assert!(
            (world - offset).length() < 1e-5,
            "identity camera must leave camera-space muzzle as world: got {world:?} expected {offset:?}"
        );
        // Offset must sit bottom-right-forward of the eye (camera origin).
        assert!(offset.x > 0.2, "muzzle right of eye, x={}", offset.x);
        assert!(offset.y < -0.15, "muzzle below eye, y={}", offset.y);
        assert!(offset.z < -1.0, "muzzle forward of eye, z={}", offset.z);
        // Flash / tip local matches exported constant.
        assert_eq!(VIEWMODEL_MUZZLE_LOCAL, Vec3::new(0.0, 0.02, -0.72));
    }

    #[test]
    fn muzzle_world_rotated_camera() {
        let offset = viewmodel_muzzle_camera_offset();
        // +90° yaw (CCW about Y): camera forward (−Z) maps to world −X.
        let cam = Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::FRAC_PI_2))
            .with_translation(Vec3::new(10.0, 2.0, -5.0));
        let world = muzzle_world_from_camera(&cam, offset);
        let expected = cam.translation + cam.rotation * offset;
        assert!(
            (world - expected).length() < 1e-5,
            "rotated: got {world:?} expected {expected:?}"
        );
        // Forward (−Z cam) contributes to world −X after +90° yaw.
        assert!(
            world.x < cam.translation.x,
            "after +90° yaw muzzle (forward of eye) should sit at smaller world X: {world:?}"
        );
        // Y is unchanged by pure yaw (offset.y stays below eye).
        assert!(
            (world.y - (cam.translation.y + offset.y)).abs() < 1e-4,
            "yaw must not tilt the vertical muzzle offset"
        );
    }

    #[test]
    fn remote_muzzle_drops_below_eye() {
        let feet = Vec3::new(1.0, 0.0, 3.0);
        let eye_h = 1.6;
        let m = remote_muzzle_from_feet(feet, eye_h);
        assert!((m.x - 1.0).abs() < 1e-6);
        assert!((m.z - 3.0).abs() < 1e-6);
        assert!((m.y - (eye_h - REMOTE_MUZZLE_DROP)).abs() < 1e-6);
    }

    #[test]
    fn kind_scale_and_color_table() {
        // Walker
        assert_eq!(kind_scale(0), Vec3::ONE);
        // Runner gaunt
        let r = kind_scale(1);
        assert!(r.x < 1.0 && r.z < 1.0);
        // Brute bulk ~1.5x
        let b = kind_scale(2);
        assert!((b.x - 1.5).abs() < 1e-5);
        assert!(b.y > 1.0);

        // Colors differ across kinds (boosted contrast)
        let c0 = kind_color(0).to_srgba();
        let c1 = kind_color(1).to_srgba();
        let c2 = kind_color(2).to_srgba();
        assert!(
            (c0.red - c1.red).abs() + (c0.green - c1.green).abs() > 0.12,
            "walker vs runner colours should differ strongly"
        );
        assert!(
            (c0.red - c2.red).abs() + (c0.green - c2.green).abs() > 0.12,
            "walker vs brute colours should differ strongly"
        );

        // Cadence: runner > walker > brute
        assert!(kind_cadence(1) > kind_cadence(0));
        assert!(kind_cadence(0) > kind_cadence(2));

        // Silhouette tables total per kind and stay distinct
        let mut head_sum = 0.0f32;
        let mut arm_sum = 0.0f32;
        for k in 0u8..3 {
            let _ = kind_scale(k);
            let _ = kind_color(k);
            let _ = kind_cadence(k);
            let (h, a) = silhouette_scale_table(k);
            assert!(h > 0.5 && a > 0.5, "silhouette scales positive");
            head_sum += h;
            arm_sum += a;
        }
        assert!(head_sum > 3.0 && arm_sum > 3.0);
        assert!((silhouette_scale_table(2).0 - silhouette_scale_table(0).0).abs() > 0.1);
    }

    #[test]
    fn telegraph_and_lod_tables() {
        assert!((telegraph_scale_pulse(false, 0.0) - 1.0).abs() < 1e-5);
        let p = telegraph_scale_pulse(true, 0.5);
        assert!((1.0..=1.12).contains(&p), "telegraph pulse in range, got {p}");

        assert_eq!(anim_lod_for_distance(10.0), AnimLod::Full);
        assert_eq!(anim_lod_for_distance(LOD_FULL_M), AnimLod::Full);
        assert_eq!(anim_lod_for_distance(LOD_FULL_M + 1.0), AnimLod::Bob);
        assert_eq!(anim_lod_for_distance(LOD_BOB_M + 1.0), AnimLod::Static);

        // Hysteresis: stay Full in the exit gap, don't flap at 40 m.
        assert_eq!(
            anim_lod_hysteresis(AnimLod::Full, LOD_FULL_M + 4.0),
            AnimLod::Full
        );
        assert_eq!(
            anim_lod_hysteresis(AnimLod::Full, LOD_FULL_EXIT_M + 0.1),
            AnimLod::Bob
        );
        assert_eq!(
            anim_lod_hysteresis(AnimLod::Bob, LOD_FULL_M),
            AnimLod::Full
        );
        assert_eq!(
            anim_lod_hysteresis(AnimLod::Static, LOD_BOB_M),
            AnimLod::Bob
        );
        assert_eq!(
            anim_lod_hysteresis(AnimLod::Bob, LOD_BOB_EXIT_M + 0.1),
            AnimLod::Static
        );
    }

    #[test]
    fn crumple_reaches_rest_within_duration() {
        let (p0, r0, y0) = crumple_pose(0.0);
        assert!(p0.abs() < 1e-5 && r0.abs() < 1e-5 && y0.abs() < 1e-5);

        let (p1, r1, y1) = crumple_pose(1.0);
        assert!(p1 > 1.0, "full crumple pitch should be substantial");
        assert!(y1 > 0.4, "body should drop toward ground");

        // At t>=1 pose is stable (rest of the collapse).
        let (p2, r2, y2) = crumple_pose(1.5);
        assert!((p1 - p2).abs() < 1e-5);
        assert!((r1 - r2).abs() < 1e-5);
        assert!((y1 - y2).abs() < 1e-5);

        // Midway is between start and end.
        let (pm, _, ym) = crumple_pose(0.5);
        assert!(pm > p0 && pm < p1);
        assert!(ym > y0 && ym < y1);
    }

    #[test]
    fn growl_trigger_deterministic() {
        assert_eq!(should_growl(42, 10.0), should_growl(42, 10.0));
        // Different buckets can differ; same bucket same result.
        let a = should_growl(1, 0.0);
        let b = should_growl(1, 0.4); // same half-second bucket
        assert_eq!(a, b);
    }
}
