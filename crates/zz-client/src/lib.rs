//! ZombieZap Bevy client library (binary entry is `main.rs` → [`run`]).

//! ZombieZap Bevy 0.19 client skeleton.
//!
//! Native: `cargo run -p zz-client`
//! Browser: `cd web && trunk serve` (see README).

pub mod audio;
pub mod game;
pub mod hud;
pub mod lobby_ui;
pub mod map_render;
pub mod models;
pub mod net;
pub mod platform;
pub mod retro;
pub mod seams;
pub mod touch;
pub mod voice;

use std::f32::consts::{FRAC_PI_2, PI};

use bevy::{
    camera::RenderTarget,
    core_pipeline::tonemapping::Tonemapping,
    diagnostic::{Diagnostic, DiagnosticsStore, FrameTimeDiagnosticsPlugin},
    input::mouse::AccumulatedMouseMotion,
    pbr::{DistanceFog, FogFalloff},
    prelude::*,
    render::view::Msaa,
    text::FontSize,
    window::{CursorGrabMode, CursorOptions, WindowResolution},
};

/// LUT-free tonemap for the 3D camera (and present camera).
///
/// Bevy's default `TonyMcMapface` needs `tonemapping_luts` + ktx2 + zstd, which
/// cost multi-MB of ship wasm. `SomewhatBoringDisplayTransform` is the same
/// author's non-LUT cousin — neutral, mild hue shift in brights, closest
/// match to Tony without the LUT stack (see crates/zz-client/README.md).
const SHIP_TONEMAPPING: Tonemapping = Tonemapping::SomewhatBoringDisplayTransform;

/// Marker for the fly camera entity.
#[derive(Component)]
struct FlyCamera;

/// Marker for the FPS text node.
#[derive(Component)]
struct FpsText;

/// Mouse-look sensitivity (radians per pixel).
const LOOK_SENSITIVITY: f32 = 0.002;
/// Walk speed in m/s.
const MOVE_SPEED: f32 = 12.0;
/// Sprint multiplier while Shift is held.
const SPRINT_MULT: f32 = 2.5;

pub fn run() {
    App::new()
        .add_plugins((
            DefaultPlugins.set(WindowPlugin {
                primary_window: Some(Window {
                    title: "ZombieZap".into(),
                    resolution: WindowResolution::new(1280, 720),
                    // Wasm: bind to the page canvas and size to its parent.
                    canvas: Some("#zz-canvas".into()),
                    fit_canvas_to_parent: true,
                    // Capture pointer events on the canvas (WASD / mouse look).
                    prevent_default_event_handling: true,
                    ..default()
                }),
                ..default()
            }),
            FrameTimeDiagnosticsPlugin::default(),
            retro::RetroRenderPlugin,
            // Game before map_render/hud so GameSessionSet exists for `.after`.
            game::GamePlugin,
            map_render::MapRenderPlugin,
            lobby_ui::LobbyUiPlugin,
            hud::HudPlugin,
            audio::AudioPlugin,
            touch::TouchPlugin,
            voice::VoicePlugin,
        ))
        .insert_resource(net::NetClient::disconnected())
        .add_systems(Startup, (setup_scene, setup_ui))
        .add_systems(
            Update,
            (
                // click-to-grab only applies in a match; menus keep the cursor
                toggle_cursor_grab.run_if(game::in_match),
                // the fly camera only flies before a match starts; in-match
                // the FPS controller in game.rs owns the camera transform
                fly_camera_look.run_if(game::menu_active),
                fly_camera_move.run_if(game::menu_active),
                update_fps,
            ),
        )
        .run();
}

/// Ground plane + ~30 colored AABB cubes + fly camera + light.
///
/// Everything except the camera is tagged [`map_render::Placeholder`]: the
/// first real map build despawns the whole backdrop, lights included — the
/// map owns its lighting (per-env sun/ambient/fog, shadows baked at build).
fn setup_scene(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    retro_target: Res<retro::RetroTarget>,
) {
    // ~80×80 m ground (dark procedural-ish slate).
    let ground = meshes.add(Plane3d::new(Vec3::Y, Vec2::splat(40.0)));
    let ground_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.12, 0.14, 0.16),
        perceptual_roughness: 0.95,
        metallic: 0.0,
        ..default()
    });
    commands.spawn((
        map_render::Placeholder,
        Mesh3d(ground),
        MeshMaterial3d(ground_mat),
        Transform::IDENTITY,
    ));

    // Placeholder "AABB map": grid of cubes with varied heights/colors.
    let unit_cube = meshes.add(Cuboid::new(1.0, 1.0, 1.0));
    let palette = [
        Color::srgb(0.55, 0.22, 0.18),
        Color::srgb(0.25, 0.45, 0.30),
        Color::srgb(0.20, 0.35, 0.55),
        Color::srgb(0.55, 0.45, 0.20),
        Color::srgb(0.40, 0.25, 0.50),
        Color::srgb(0.30, 0.50, 0.50),
    ];

    let mut i = 0usize;
    // 5×6 = 30 cubes across a ~60 m patch centered on origin.
    for gx in 0..5 {
        for gz in 0..6 {
            let x = -24.0 + gx as f32 * 12.0;
            let z = -30.0 + gz as f32 * 12.0;
            // Deterministic pseudo-height in [1.5, 8.0] m.
            let h = 1.5 + ((i * 17 + gx * 3 + gz * 7) % 14) as f32 * 0.5;
            let color = palette[i % palette.len()];
            let mat = materials.add(StandardMaterial {
                base_color: color,
                perceptual_roughness: 0.85,
                metallic: 0.05,
                ..default()
            });
            commands.spawn((
                map_render::Placeholder,
                Mesh3d(unit_cube.clone()),
                MeshMaterial3d(mat),
                Transform::from_xyz(x, h * 0.5, z).with_scale(Vec3::new(3.0, h, 3.0)),
            ));
            i += 1;
        }
    }

    // Backdrop sun. Real-time shadow maps stay OFF everywhere: map shadows
    // are baked once per map build (ShotAnte's frame-budget lesson).
    commands.spawn((
        map_render::Placeholder,
        DirectionalLight {
            illuminance: 12_000.0,
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::from_rotation(Quat::from_euler(EulerRot::XYZ, -PI * 0.35, PI * 0.2, 0.0)),
    ));

    // The one 3D camera: renders the world at RETRO_W×RETRO_H into the retro
    // target (nearest-upscaled to the window by RetroRenderPlugin). MSAA off —
    // chunky pixels are the point. Fog is retuned per env on every map build.
    //
    // Bevy 0.19: `RenderTarget` is a required *component* on the camera entity
    // (not a field of `Camera`). Default is the primary window; override with
    // `RenderTarget::Image(ImageRenderTarget)` for offscreen passes.
    commands.spawn((
        FlyCamera,
        Camera3d::default(),
        Camera {
            // Present camera (order 1) blits the retro frame on top of letterbox bars.
            order: 0,
            // Always clear the full 480×270 target every frame. Leaving this
            // on `Default` with a partial viewport / stale image can produce
            // horizontal garbage bands (top ~20%) and duplicated sky stripes
            // after match start — the retro buffer must never partially scan.
            clear_color: ClearColorConfig::Custom(Color::srgb_u8(20, 24, 32)),
            ..default()
        },
        // Override Camera3d's required default (TonyMcMapface / LUT-backed).
        SHIP_TONEMAPPING,
        RenderTarget::Image(retro_target.image.clone().into()),
        Msaa::Off,
        DistanceFog {
            color: Color::srgb_u8(158, 201, 239),
            falloff: FogFalloff::Linear {
                start: 60.0,
                end: 220.0,
            },
            ..default()
        },
        Transform::from_xyz(0.0, 4.0, 18.0).looking_at(Vec3::new(0.0, 2.0, 0.0), Vec3::Y),
    ));
}

fn setup_ui(mut commands: Commands) {
    commands.spawn((
        FpsText,
        Text::new("FPS: --"),
        TextFont {
            font_size: FontSize::Px(18.0),
            ..default()
        },
        TextColor(Color::srgb(0.85, 0.95, 0.55)),
        Node {
            position_type: PositionType::Absolute,
            top: px(10),
            left: px(12),
            ..default()
        },
    ));

    commands.spawn((
        Text::new("Click to lock cursor · WASD move · Shift sprint · Esc release"),
        TextFont {
            font_size: FontSize::Px(14.0),
            ..default()
        },
        TextColor(Color::srgba(0.75, 0.78, 0.82, 0.85)),
        Node {
            position_type: PositionType::Absolute,
            bottom: px(12),
            left: px(12),
            ..default()
        },
    ));
}

/// Left-click locks/hides the cursor; Esc releases it.
///
/// Bevy 0.19: cursor grab lives on the `CursorOptions` component (no longer
/// `Window::cursor.grab_mode`). Disabled entirely in touch mode — on-screen
/// stick/aim need a free cursor (and `?touch=1` desktop automation uses mouse
/// as a pointer, not pointer-lock deltas).
fn toggle_cursor_grab(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    touch: Res<touch::TouchIntent>,
    mut cursor_options: Single<&mut CursorOptions>,
) {
    if touch.enabled {
        // Keep free + visible so stick/buttons receive cursor positions.
        if cursor_options.grab_mode != CursorGrabMode::None {
            cursor_options.visible = true;
            cursor_options.grab_mode = CursorGrabMode::None;
        }
        return;
    }
    if mouse.just_pressed(MouseButton::Left) {
        cursor_options.visible = false;
        cursor_options.grab_mode = CursorGrabMode::Locked;
    }
    if keys.just_pressed(KeyCode::Escape) {
        cursor_options.visible = true;
        cursor_options.grab_mode = CursorGrabMode::None;
    }
}

/// Mouse-look only while the pointer is locked.
fn fly_camera_look(
    mouse_motion: Res<AccumulatedMouseMotion>,
    cursor_options: Single<&CursorOptions>,
    mut camera: Single<&mut Transform, With<FlyCamera>>,
) {
    if cursor_options.grab_mode == CursorGrabMode::None {
        return;
    }
    let delta = mouse_motion.delta;
    if delta == Vec2::ZERO {
        return;
    }

    let delta_yaw = -delta.x * LOOK_SENSITIVITY;
    let delta_pitch = -delta.y * LOOK_SENSITIVITY;

    let (yaw, pitch, roll) = camera.rotation.to_euler(EulerRot::YXZ);
    let yaw = yaw + delta_yaw;
    // Avoid gimbal lock / flipped yaw at ±90°.
    let pitch = (pitch + delta_pitch).clamp(-FRAC_PI_2 + 0.01, FRAC_PI_2 - 0.01);
    camera.rotation = Quat::from_euler(EulerRot::YXZ, yaw, pitch, roll);
}

/// WASD horizontal (camera-relative), Space/Ctrl vertical world-up.
fn fly_camera_move(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut camera: Single<&mut Transform, With<FlyCamera>>,
) {
    let mut wish = Vec3::ZERO;
    if keys.pressed(KeyCode::KeyW) {
        wish.z -= 1.0;
    }
    if keys.pressed(KeyCode::KeyS) {
        wish.z += 1.0;
    }
    if keys.pressed(KeyCode::KeyA) {
        wish.x -= 1.0;
    }
    if keys.pressed(KeyCode::KeyD) {
        wish.x += 1.0;
    }

    let speed = if keys.pressed(KeyCode::ShiftLeft) {
        MOVE_SPEED * SPRINT_MULT
    } else {
        MOVE_SPEED
    };

    let mut delta = Vec3::ZERO;
    if wish != Vec3::ZERO {
        // Flatten forward/right onto the XZ plane so pitch does not dive the fly cam.
        let forward = camera.forward();
        let flat_forward = Vec3::new(forward.x, 0.0, forward.z).normalize_or_zero();
        let right = camera.right();
        let flat_right = Vec3::new(right.x, 0.0, right.z).normalize_or_zero();
        delta += flat_forward * -wish.z + flat_right * wish.x;
    }

    if keys.pressed(KeyCode::Space) {
        delta.y += 1.0;
    }
    if keys.pressed(KeyCode::ControlLeft) {
        delta.y -= 1.0;
    }

    if delta != Vec3::ZERO {
        camera.translation += delta.normalize_or_zero() * speed * time.delta_secs();
    }
}

/// Corner FPS text + window title.
fn update_fps(
    diagnostics: Res<DiagnosticsStore>,
    mut text_q: Query<&mut Text, With<FpsText>>,
    mut window: Single<&mut Window>,
) {
    let Some(fps) = diagnostics
        .get(&FrameTimeDiagnosticsPlugin::FPS)
        .and_then(Diagnostic::smoothed)
    else {
        return;
    };

    let label = format!("FPS: {fps:.0}");
    if let Ok(mut text) = text_q.single_mut() {
        **text = label.clone();
    }
    window.title = format!("ZombieZap — {fps:.0} FPS");
}
