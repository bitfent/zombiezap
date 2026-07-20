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

const TORSO_W: f32 = 0.42;
const TORSO_H: f32 = 0.62;
const TORSO_D: f32 = 0.26;
const TORSO_CY: f32 = 1.14; // torso centre Y

const HEAD_W: f32 = 0.34;
const HEAD_H: f32 = 0.34;
const HEAD_D: f32 = 0.32;
const HEAD_CY: f32 = 1.62;

const LEG_W: f32 = 0.17;
const LEG_LEN: f32 = 0.84;
const LEG_JOINT_Y: f32 = 0.86;
const LEG_X: f32 = 0.12;

const ARM_W: f32 = 0.14;
const ARM_LEN: f32 = 0.62;
const ARM_JOINT_Y: f32 = 1.42;
const ARM_X: f32 = 0.27;

const EYE_S: f32 = 0.06;
const EYE_Y: f32 = 1.66;
const EYE_Z: f32 = -0.17; // face forward = -Z
const EYE_X: f32 = 0.08;

/// Max limb swing |angle| (radians) at any speed — tests assert against this.
pub const MAX_LIMB_SWING: f32 = 0.85;

/// Death crumple duration in seconds.
pub const CRUMPLE_DURATION_S: f32 = 0.55;

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

/// Kind → rotten-green body colour (walker / runner / brute silhouettes).
pub fn kind_color(kind: u8) -> Color {
    match kind {
        1 => Color::srgb(0.50, 0.58, 0.22), // runner: sickly yellow-green
        2 => Color::srgb(0.22, 0.32, 0.20), // brute: dark bulk
        _ => Color::srgb(0.30, 0.48, 0.26), // walker: rotten green
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
pub fn attack_arm_raise() -> f32 {
    -1.15 // arms up/forward toward the player
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
#[derive(Resource)]
pub struct RigAssets {
    pub unit_cube: Handle<Mesh>,
    /// Walker / runner / brute body materials (shared across the horde).
    pub zombie_mats: [Handle<StandardMaterial>; 3],
    /// Glowing eyes — one handle for all zombies.
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
            materials.add(entity_mat(kind_color(0), 0.10)),
            materials.add(entity_mat(kind_color(1), 0.12)),
            materials.add(entity_mat(kind_color(2), 0.08)),
        ];

        // Bright emissive eyes for dark-street readability (no bloom required).
        let eye_mat = materials.add(StandardMaterial {
            base_color: Color::srgb(0.95, 0.15, 0.08),
            emissive: LinearRgba::rgb(8.0, 0.6, 0.15),
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

    fn body_mat(&self, is_zombie: bool, kind_or_slot: u8) -> Handle<StandardMaterial> {
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
    pub phase: f32,
    pub phase_offset: f32,
    pub speed: f32,
    pub last_pos: Option<Vec3>,
    /// Zombie kind 0/1/2, or player slot for survivors.
    pub kind: u8,
    pub is_zombie: bool,
    /// Snapshot `state == 1` attack telegraph.
    pub attacking: bool,
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

    let mut body_e = Entity::PLACEHOLDER;
    let mut left_arm = Entity::PLACEHOLDER;
    let mut right_arm = Entity::PLACEHOLDER;
    let mut left_leg = Entity::PLACEHOLDER;
    let mut right_leg = Entity::PLACEHOLDER;

    root.with_children(|root_c| {
        let body_id = root_c
            .spawn((Transform::IDENTITY, Visibility::default()))
            .with_children(|body| {
                // Torso
                part(
                    body,
                    cube.clone(),
                    mat.clone(),
                    Vec3::new(0.0, TORSO_CY, 0.0),
                    Vec3::new(TORSO_W, TORSO_H, TORSO_D),
                );
                // Head
                part(
                    body,
                    cube.clone(),
                    mat.clone(),
                    Vec3::new(0.0, HEAD_CY, 0.0),
                    Vec3::new(HEAD_W, HEAD_H, HEAD_D),
                );
                // Emissive eyes (zombies only) for dark readability
                if is_zombie {
                    part(
                        body,
                        cube.clone(),
                        eye_mat.clone(),
                        Vec3::new(-EYE_X, EYE_Y, EYE_Z),
                        Vec3::splat(EYE_S),
                    );
                    part(
                        body,
                        cube.clone(),
                        eye_mat,
                        Vec3::new(EYE_X, EYE_Y, EYE_Z),
                        Vec3::splat(EYE_S),
                    );
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
                    ARM_W,
                    ARM_LEN,
                );
                right_arm = limb_pivot(
                    body,
                    cube.clone(),
                    mat.clone(),
                    Vec3::new(ARM_X, ARM_JOINT_Y, 0.0),
                    ARM_W,
                    ARM_LEN,
                );
            })
            .id();
        body_e = body_id;
    });

    HumanoidRig {
        body: body_e,
        left_arm,
        right_arm,
        left_leg,
        right_leg,
        phase: 0.0,
        phase_offset: phase_offset_for_id(id_for_phase),
        speed: 0.0,
        last_pos: None,
        kind: kind_or_slot,
        is_zombie,
        attacking: false,
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
}

/// Advance walk-cycle state from interpolated root position; return joint pose.
pub fn compute_rig_pose(rig: &mut HumanoidRig, pos: Vec3, dt: f32) -> RigPose {
    if let Some(prev) = rig.last_pos {
        let raw = Vec3::new(pos.x - prev.x, 0.0, pos.z - prev.z).length() / dt.max(1e-4);
        let target = raw.min(9.0);
        // Low-pass so interp jitter doesn't stutter the gait.
        let ease = (dt * 12.0).min(1.0);
        rig.speed += (target - rig.speed) * ease;
    }
    rig.last_pos = Some(pos);

    let kind = if rig.is_zombie { rig.kind } else { 0 };
    let cadence = if rig.is_zombie {
        kind_cadence(kind)
    } else {
        1.1 // survivors: subtle, slightly snappy gait
    };

    if rig.speed > 0.25 {
        rig.phase += rig.speed * dt * 2.4 * cadence;
    }

    let walk_phase = rig.phase + rig.phase_offset;
    let swing = if rig.speed > 0.35 {
        1.0
    } else {
        (rig.speed / 0.35).clamp(0.0, 1.0)
    };

    // Survivor walks are subtler.
    let survivor_scale = if rig.is_zombie { 1.0 } else { 0.55 };

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

    let bob = if rig.speed > 0.35 {
        walk_phase.sin().abs() * 0.04 * swing * survivor_scale
    } else {
        0.0
    };

    RigPose {
        body_y: bob,
        left_arm_x: arm_l,
        right_arm_x: arm_r,
        left_leg_x: leg_l,
        right_leg_x: leg_r,
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
const VM_REST: Vec3 = Vec3::new(0.28, -0.28, -0.55);

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
            Transform::from_translation(VM_REST).with_rotation(Quat::from_rotation_y(0.05)),
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
            // Muzzle flash (emissive cuboid; visibility + alpha driven per shot)
            flash_entity = c
                .spawn((
                    Mesh3d(cube),
                    MeshMaterial3d(flash_mat),
                    Transform::from_translation(Vec3::new(0.0, 0.02, -0.72))
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

        // Colors differ across kinds
        let c0 = kind_color(0).to_srgba();
        let c1 = kind_color(1).to_srgba();
        let c2 = kind_color(2).to_srgba();
        assert!(
            (c0.red - c1.red).abs() + (c0.green - c1.green).abs() > 0.05,
            "walker vs runner colours should differ"
        );
        assert!(
            (c0.red - c2.red).abs() + (c0.green - c2.green).abs() > 0.05,
            "walker vs brute colours should differ"
        );

        // Cadence: runner > walker > brute
        assert!(kind_cadence(1) > kind_cadence(0));
        assert!(kind_cadence(0) > kind_cadence(2));

        // Total over kinds 0/1/2: scales and colours all defined (no panic)
        for k in 0u8..3 {
            let _ = kind_scale(k);
            let _ = kind_color(k);
            let _ = kind_cadence(k);
        }
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
