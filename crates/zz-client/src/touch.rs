//! Mobile / forced-touch on-screen controls (M10 / NEXT item 3).
//!
//! Left-half virtual stick → move bools; right-half drag → aim delta;
//! FIRE / JUMP / GRENADE bevy_ui buttons. All paths write
//! [`TouchIntent`], which `game::fps_controller` ORs into the same 30 Hz
//! `PlayerInput` as keyboard/mouse. Mouse events also drive the stick/aim
//! in `?touch=1` mode so browser-automation panes can play with clicks.
//!
//! Pure vector math lives in free functions under `#[cfg(test)]` tests.

#![allow(clippy::type_complexity)] // Bevy Query filters
#![allow(clippy::too_many_arguments)] // Bevy system params / UI spawn helpers

use bevy::prelude::*;
use bevy::text::FontSize;
use bevy::ui::FocusPolicy;
use bevy::window::PrimaryWindow;

use crate::game::Session;
use crate::platform;
use crate::seams::SfxQueue;

// ── tuning (matches legacy Input.ts feel, task ~0.35 dead zone) ────────────

/// Stick radius in logical pixels (thumb travel limit).
pub const STICK_RADIUS: f32 = 64.0;
/// Displacement fraction of radius before a direction bool flips on.
pub const STICK_DEADZONE: f32 = 0.35;
/// Aim sensitivity: radians per pixel. Mouse desktop uses 0.0025; touch
/// multiplies so a short finger swipe covers more angle (legacy ~2.4×).
pub const TOUCH_AIM_SENS: f32 = 0.0025 * 2.0;
/// Movement (px) below which a right-side press is a tap (no aim applied
/// until this is exceeded — taps never fire; fire is button-only).
pub const TAP_MOVE_PX: f32 = 12.0;
/// Max duration (s) still counted as a tap when movement is under threshold.
#[allow(dead_code)] // used by pure-logic tests / future hold classification
pub const TAP_MAX_S: f32 = 0.22;
/// Minimum on-screen button size (logical px).
const BTN_MIN: f32 = 64.0;
const FIRE_SIZE: f32 = 88.0;
const SIDE_BTN: f32 = 68.0;

// Fixed layout anchors (logical px from window edges) — reported for automation.
/// Stick centre as fraction of window (x from left, y from top).
pub const STICK_ANCHOR: (f32, f32) = (0.18, 0.72);
/// FIRE button centre: (right inset, bottom inset) in px at reference 1280×720;
/// actual placement uses the same insets so centres stay predictable.
pub const FIRE_INSET: (f32, f32) = (56.0, 72.0);
pub const JUMP_INSET: (f32, f32) = (168.0, 150.0);
pub const GRENADE_INSET: (f32, f32) = (168.0, 72.0);
/// MELEE in the bottom-right cluster (does not overlap FIRE/JUMP/NADE).
/// Centre at 1280×720: (1280-250, 720-150) = (1030, 570).
pub const MELEE_INSET: (f32, f32) = (250.0, 150.0);
/// RELOAD chip near the ammo counter (bottom-right HUD), not the fire cluster.
/// Centre at 1280×720: (1280-100, 720-220) = (1180, 500).
pub const RELOAD_INSET: (f32, f32) = (100.0, 220.0);

// ── public intent ──────────────────────────────────────────────────────────

/// Per-frame touch contribution merged into `PlayerInput` by game.rs.
/// Cleared / rebuilt every Update; buttons hold while Interaction::Pressed.
#[derive(Resource, Debug, Clone, Default)]
pub struct TouchIntent {
    /// True when this client is in touch-control mode.
    pub enabled: bool,
    pub forward: bool,
    pub backward: bool,
    pub left: bool,
    pub right: bool,
    pub jump: bool,
    pub fire: bool,
    pub grenade: bool,
    pub melee: bool,
    pub reload: bool,
    /// Radians added to yaw/pitch this frame (already scaled; do not × dt).
    pub aim_yaw: f32,
    pub aim_pitch: f32,
}

// ── plugin ─────────────────────────────────────────────────────────────────

pub struct TouchPlugin;

impl Plugin for TouchPlugin {
    fn build(&self, app: &mut App) {
        let enabled = platform::is_touch_mode();
        platform::apply_touch_body_class(enabled);
        app.insert_resource(TouchIntent {
            enabled,
            ..Default::default()
        })
        .insert_resource(TouchState::default())
        .add_systems(Startup, setup_touch_ui)
        .add_systems(
            Update,
            (
                unlock_audio_on_gesture,
                gate_touch_chrome,
                process_pointers.run_if(touch_enabled),
                update_stick_visual.run_if(touch_enabled),
                sample_buttons.run_if(touch_enabled),
            )
                .chain(),
        );
    }
}

fn touch_enabled(intent: Res<TouchIntent>) -> bool {
    intent.enabled
}

// ── internal state ─────────────────────────────────────────────────────────

/// Synthetic pointer id for mouse-as-touch (desktop `?touch=1`).
const MOUSE_PTR: u64 = u64::MAX;

#[derive(Resource, Default)]
struct TouchState {
    stick: Option<StickTrack>,
    aim: Option<AimTrack>,
    audio_unlocked: bool,
    /// Last window size used to place fixed stick chrome.
    placed_for: Vec2,
}

struct StickTrack {
    id: u64,
    origin: Vec2,
    /// Normalised stick vector in [-1, 1] after radius clamp.
    vec: Vec2,
}

struct AimTrack {
    id: u64,
    last: Vec2,
    start: Vec2,
    /// Once movement exceeds [`TAP_MOVE_PX`], aim deltas apply.
    dragging: bool,
}

// ── UI markers ─────────────────────────────────────────────────────────────

#[derive(Component)]
struct TouchChrome;

#[derive(Component)]
struct StickBase;

#[derive(Component)]
struct StickKnob;

#[derive(Component, Clone, Copy, PartialEq, Eq)]
enum TouchBtn {
    Fire,
    Jump,
    Grenade,
    Melee,
    Reload,
}

// ── pure logic (unit-tested) ───────────────────────────────────────────────

/// Map a stick vector in [-1, 1] (x right, y down like screen) to move bools.
/// Dead zone is absolute displacement per axis (legacy Input.ts).
pub fn stick_to_move(vec: Vec2, deadzone: f32) -> (bool, bool, bool, bool) {
    // Screen y-down: stick up (negative y) = forward.
    let forward = vec.y < -deadzone;
    let backward = vec.y > deadzone;
    let left = vec.x < -deadzone;
    let right = vec.x > deadzone;
    (forward, backward, left, right)
}

/// Clamp a raw offset from stick origin into a unit disk of radius `r`.
pub fn clamp_stick_offset(delta: Vec2, radius: f32) -> Vec2 {
    let len = delta.length();
    if len > radius && len > 0.0 {
        delta * (radius / len)
    } else {
        delta
    }
}

/// Classify a completed press: tap if short time AND low movement.
/// Production aim path gates on movement alone (`TAP_MOVE_PX`); this helper
/// is the full time+distance rule used by unit tests and any future UI taps.
#[allow(dead_code)]
pub fn is_tap(duration_s: f32, move_px: f32, max_s: f32, max_px: f32) -> bool {
    duration_s <= max_s && move_px <= max_px
}

/// Aim yaw/pitch delta from a screen-space drag step (pixels).
/// Negative x drag → positive yaw (turn left looks left), matching mouse path
/// `yaw -= delta.x * sens`.
pub fn aim_delta_from_drag(pixel_delta: Vec2, sens: f32) -> (f32, f32) {
    let yaw = -pixel_delta.x * sens;
    let pitch = -pixel_delta.y * sens;
    (yaw, pitch)
}

// ── setup UI ───────────────────────────────────────────────────────────────

fn setup_touch_ui(mut commands: Commands, intent: Res<TouchIntent>) {
    // Always spawn chrome (cheap); gate_touch_chrome shows it only in-match
    // when touch mode is on. Intent is read so the resource is live at startup.
    let _ = intent.enabled;
    let root_vis = Visibility::Hidden;

    // Stick base (fixed anchor; knob moves inside).
    commands
        .spawn((
            TouchChrome,
            StickBase,
            Node {
                position_type: PositionType::Absolute,
                width: px(STICK_RADIUS * 2.0),
                height: px(STICK_RADIUS * 2.0),
                left: px(0.0),
                top: px(0.0),
                border: UiRect::all(px(2.0)),
                border_radius: BorderRadius::MAX,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(Color::srgba(0.08, 0.09, 0.12, 0.45)),
            BorderColor::all(Color::srgba(0.45, 0.5, 0.55, 0.7)),
            FocusPolicy::Pass, // stick is driven by global pointers, not UI hit
            root_vis,
            GlobalZIndex(50),
            Name::new("TouchStick"),
        ))
        .with_children(|p| {
            p.spawn((
                StickKnob,
                Node {
                    width: px(44.0),
                    height: px(44.0),
                    border_radius: BorderRadius::MAX,
                    margin: UiRect::all(px(0.0)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.18, 0.9, 0.84, 0.75)),
                FocusPolicy::Pass,
            ));
        });

    spawn_btn(
        &mut commands,
        TouchBtn::Fire,
        "FIRE",
        FIRE_SIZE,
        Color::srgba(1.0, 0.25, 0.4, 0.55),
        Color::srgba(1.0, 0.35, 0.45, 0.95),
        FIRE_INSET,
        root_vis,
    );
    spawn_btn(
        &mut commands,
        TouchBtn::Jump,
        "JUMP",
        SIDE_BTN.max(BTN_MIN),
        Color::srgba(0.12, 0.18, 0.22, 0.55),
        Color::srgba(0.18, 0.9, 0.84, 0.95),
        JUMP_INSET,
        root_vis,
    );
    spawn_btn(
        &mut commands,
        TouchBtn::Grenade,
        "NADE",
        SIDE_BTN.max(BTN_MIN),
        Color::srgba(0.18, 0.22, 0.10, 0.55),
        Color::srgba(0.55, 0.85, 0.30, 0.95),
        GRENADE_INSET,
        root_vis,
    );
    spawn_btn(
        &mut commands,
        TouchBtn::Melee,
        "MELEE",
        SIDE_BTN.max(BTN_MIN),
        Color::srgba(0.28, 0.14, 0.10, 0.55),
        Color::srgba(0.95, 0.55, 0.30, 0.95),
        MELEE_INSET,
        root_vis,
    );
    spawn_btn(
        &mut commands,
        TouchBtn::Reload,
        "RELOAD",
        SIDE_BTN.max(BTN_MIN),
        Color::srgba(0.12, 0.16, 0.28, 0.55),
        Color::srgba(0.45, 0.75, 0.95, 0.95),
        RELOAD_INSET,
        root_vis,
    );
}

fn spawn_btn(
    commands: &mut Commands,
    kind: TouchBtn,
    label: &str,
    size: f32,
    fill: Color,
    border: Color,
    inset_rb: (f32, f32),
    vis: Visibility,
) {
    let (right, bottom) = inset_rb;
    // Centre the circular button on the inset point: right/bottom are to the
    // *centre*, so offset by half size.
    let half = size * 0.5;
    commands
        .spawn((
            TouchChrome,
            kind,
            Button,
            Node {
                position_type: PositionType::Absolute,
                width: px(size),
                height: px(size),
                right: px(right - half),
                bottom: px(bottom - half),
                border: UiRect::all(px(3.0)),
                border_radius: BorderRadius::MAX,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(fill),
            BorderColor::all(border),
            vis,
            GlobalZIndex(50),
            Name::new(format!("TouchBtn_{label}")),
        ))
        .with_children(|p| {
            p.spawn((
                Text::new(label),
                TextFont {
                    font_size: FontSize::Px(if size >= 80.0 { 14.0 } else { 11.0 }),
                    ..default()
                },
                TextColor(border),
                // Don't let label steal hits.
                FocusPolicy::Pass,
            ));
        });
}

// ── systems ────────────────────────────────────────────────────────────────

fn gate_touch_chrome(
    intent: Res<TouchIntent>,
    session: Res<Session>,
    mut q: Query<&mut Visibility, With<TouchChrome>>,
) {
    let show = intent.enabled && matches!(*session, Session::Playing { .. });
    let target = if show {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    for mut v in &mut q {
        if *v != target {
            *v = target;
        }
    }
}

fn unlock_audio_on_gesture(
    mut state: ResMut<TouchState>,
    mouse: Res<ButtonInput<MouseButton>>,
    touches: Res<Touches>,
    keys: Res<ButtonInput<KeyCode>>,
    mut sfx: ResMut<SfxQueue>,
) {
    if state.audio_unlocked {
        return;
    }
    let gesture = mouse.any_just_pressed([MouseButton::Left, MouseButton::Right])
        || touches.any_just_pressed()
        || keys.get_just_pressed().next().is_some();
    if !gesture {
        return;
    }
    state.audio_unlocked = true;
    platform::unlock_audio();
    // A quiet click inside the gesture frame also primes Bevy's audio graph.
    sfx.0.push_back(crate::seams::Sfx::Click);
}

fn process_pointers(
    mut intent: ResMut<TouchIntent>,
    mut state: ResMut<TouchState>,
    mouse: Res<ButtonInput<MouseButton>>,
    touches: Res<Touches>,
    windows: Query<&Window, With<PrimaryWindow>>,
    btn_nodes: Query<(&TouchBtn, &Interaction), (With<Button>, With<TouchChrome>)>,
) {
    // Reset transient move/aim each frame; buttons re-sampled after.
    intent.forward = false;
    intent.backward = false;
    intent.left = false;
    intent.right = false;
    intent.aim_yaw = 0.0;
    intent.aim_pitch = 0.0;

    let Ok(window) = windows.single() else {
        return;
    };
    let win = Vec2::new(window.width(), window.height());
    if win.x <= 1.0 || win.y <= 1.0 {
        return;
    }

    // Place fixed stick chrome when window resizes.
    if state.placed_for != win {
        state.placed_for = win;
    }

    let stick_home = Vec2::new(win.x * STICK_ANCHOR.0, win.y * STICK_ANCHOR.1);

    // Helper: is this position over a touch button? (avoid steal for stick/aim)
    let over_button = |pos: Vec2| -> bool {
        for (kind, _) in &btn_nodes {
            let (inset_r, inset_b, size) = match kind {
                TouchBtn::Fire => (FIRE_INSET.0, FIRE_INSET.1, FIRE_SIZE),
                TouchBtn::Jump => (JUMP_INSET.0, JUMP_INSET.1, SIDE_BTN.max(BTN_MIN)),
                TouchBtn::Grenade => (GRENADE_INSET.0, GRENADE_INSET.1, SIDE_BTN.max(BTN_MIN)),
                TouchBtn::Melee => (MELEE_INSET.0, MELEE_INSET.1, SIDE_BTN.max(BTN_MIN)),
                TouchBtn::Reload => (RELOAD_INSET.0, RELOAD_INSET.1, SIDE_BTN.max(BTN_MIN)),
            };
            let centre = Vec2::new(win.x - inset_r, win.y - inset_b);
            if (pos - centre).length() <= size * 0.55 {
                return true;
            }
        }
        false
    };

    // ── real multi-touch ───────────────────────────────────────────────────
    for t in touches.iter_just_pressed() {
        let pos = t.position();
        // Bevy touch y is top-left origin already (logical).
        begin_pointer(
            &mut state,
            t.id(),
            pos,
            win,
            stick_home,
            over_button(pos),
        );
    }
    for t in touches.iter() {
        move_pointer(&mut state, &mut intent, t.id(), t.position());
    }
    for t in touches.iter_just_released() {
        end_pointer(&mut state, t.id());
    }
    for t in touches.iter_just_canceled() {
        end_pointer(&mut state, t.id());
    }

    // ── mouse as single pointer (desktop ?touch=1 / automation) ────────────
    if let Some(pos) = window.cursor_position() {
        if mouse.just_pressed(MouseButton::Left) {
            begin_pointer(
                &mut state,
                MOUSE_PTR,
                pos,
                win,
                stick_home,
                over_button(pos),
            );
        }
        if mouse.pressed(MouseButton::Left) {
            move_pointer(&mut state, &mut intent, MOUSE_PTR, pos);
        }
        if mouse.just_released(MouseButton::Left) {
            end_pointer(&mut state, MOUSE_PTR);
        }
    } else if mouse.just_released(MouseButton::Left) {
        end_pointer(&mut state, MOUSE_PTR);
    }

    // Stick bools from current stick vector.
    if let Some(ref s) = state.stick {
        let (f, b, l, r) = stick_to_move(s.vec, STICK_DEADZONE);
        intent.forward = f;
        intent.backward = b;
        intent.left = l;
        intent.right = r;
    }
}

fn begin_pointer(
    state: &mut TouchState,
    id: u64,
    pos: Vec2,
    win: Vec2,
    stick_home: Vec2,
    on_button: bool,
) {
    if on_button {
        return;
    }
    // Left ~45% of the screen → stick (legacy).
    if pos.x < win.x * 0.45 && state.stick.is_none() {
        // Dynamic origin at press, but snap near home if press is close so
        // automation can always hit the fixed stick chrome.
        let origin = if (pos - stick_home).length() < STICK_RADIUS * 1.4 {
            stick_home
        } else {
            pos
        };
        state.stick = Some(StickTrack {
            id,
            origin,
            vec: Vec2::ZERO,
        });
    } else if state.aim.is_none() {
        state.aim = Some(AimTrack {
            id,
            last: pos,
            start: pos,
            dragging: false,
        });
    }
}

fn move_pointer(state: &mut TouchState, intent: &mut TouchIntent, id: u64, pos: Vec2) {
    if let Some(ref mut s) = state.stick
        && s.id == id
    {
        let clamped = clamp_stick_offset(pos - s.origin, STICK_RADIUS);
        s.vec = clamped / STICK_RADIUS;
        return;
    }
    if let Some(ref mut a) = state.aim
        && a.id == id
    {
        let step = pos - a.last;
        a.last = pos;
        if !a.dragging {
            let total = (pos - a.start).length();
            if total >= TAP_MOVE_PX {
                a.dragging = true;
            } else {
                return; // still a potential tap — no aim
            }
        }
        let (yaw, pitch) = aim_delta_from_drag(step, TOUCH_AIM_SENS);
        intent.aim_yaw += yaw;
        intent.aim_pitch += pitch;
    }
}

fn end_pointer(state: &mut TouchState, id: u64) {
    if state.stick.as_ref().is_some_and(|s| s.id == id) {
        state.stick = None;
    }
    if state.aim.as_ref().is_some_and(|a| a.id == id) {
        state.aim = None;
    }
}

fn update_stick_visual(
    state: Res<TouchState>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut base_q: Query<&mut Node, (With<StickBase>, Without<StickKnob>)>,
    mut knob_q: Query<&mut Node, (With<StickKnob>, Without<StickBase>)>,
) {
    let Ok(window) = windows.single() else {
        return;
    };
    let win = Vec2::new(window.width(), window.height());
    let home = Vec2::new(win.x * STICK_ANCHOR.0, win.y * STICK_ANCHOR.1);
    let (origin, vec) = match state.stick.as_ref() {
        Some(s) => (s.origin, s.vec),
        None => (home, Vec2::ZERO),
    };

    if let Ok(mut node) = base_q.single_mut() {
        // Node left/top is top-left of the stick circle.
        node.left = px(origin.x - STICK_RADIUS);
        node.top = px(origin.y - STICK_RADIUS);
    }
    if let Ok(mut knob) = knob_q.single_mut() {
        // Knob is a child centred via flex; offset with translate via margin.
        let offset = vec * STICK_RADIUS;
        knob.left = px(offset.x);
        knob.top = px(offset.y);
        // With flex parent centering, left/top on the child aren't ideal.
        // Use margin instead relative to the natural centre position.
        knob.margin = UiRect {
            left: px(offset.x),
            top: px(offset.y),
            right: px(-offset.x),
            bottom: px(-offset.y),
        };
    }
}

fn sample_buttons(
    mut intent: ResMut<TouchIntent>,
    q: Query<(&TouchBtn, &Interaction), (With<Button>, With<TouchChrome>)>,
) {
    // Reset button bools then OR pressed states (held = true).
    intent.fire = false;
    intent.jump = false;
    intent.grenade = false;
    intent.melee = false;
    intent.reload = false;
    for (kind, interaction) in &q {
        if *interaction == Interaction::Pressed {
            match kind {
                TouchBtn::Fire => intent.fire = true,
                TouchBtn::Jump => intent.jump = true,
                TouchBtn::Grenade => intent.grenade = true,
                TouchBtn::Melee => intent.melee = true,
                TouchBtn::Reload => intent.reload = true,
            }
        }
    }
}

// ── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stick_deadzone_neutral() {
        let (f, b, l, r) = stick_to_move(Vec2::new(0.2, -0.2), 0.35);
        assert!(!f && !b && !l && !r);
    }

    #[test]
    fn stick_forward_and_right() {
        let (f, b, l, r) = stick_to_move(Vec2::new(0.8, -0.9), 0.35);
        assert!(f && !b && !l && r);
    }

    #[test]
    fn stick_backward_left() {
        let (f, b, l, r) = stick_to_move(Vec2::new(-0.5, 0.6), 0.35);
        assert!(!f && b && l && !r);
    }

    #[test]
    fn stick_diagonals_both_axes() {
        let (f, b, l, r) = stick_to_move(Vec2::new(0.4, -0.4), 0.35);
        assert!(f && !b && !l && r);
    }

    #[test]
    fn clamp_stick_inside_unchanged() {
        let v = Vec2::new(10.0, -20.0);
        let c = clamp_stick_offset(v, 64.0);
        assert!((c - v).length() < 1e-5);
    }

    #[test]
    fn clamp_stick_outside_on_circle() {
        let c = clamp_stick_offset(Vec2::new(100.0, 0.0), 64.0);
        assert!((c.length() - 64.0).abs() < 1e-4);
        assert!(c.x > 0.0 && c.y.abs() < 1e-5);
    }

    #[test]
    fn tap_vs_drag_classification() {
        assert!(is_tap(0.1, 5.0, TAP_MAX_S, TAP_MOVE_PX));
        assert!(!is_tap(0.5, 5.0, TAP_MAX_S, TAP_MOVE_PX)); // too long
        assert!(!is_tap(0.1, 40.0, TAP_MAX_S, TAP_MOVE_PX)); // too far
    }

    #[test]
    fn aim_delta_accumulation_matches_mouse_sign() {
        // Drag right → negative yaw (turn right), same as mouse path.
        let (yaw, pitch) = aim_delta_from_drag(Vec2::new(10.0, -4.0), 0.0025);
        assert!((yaw - (-10.0 * 0.0025)).abs() < 1e-6);
        assert!((pitch - (4.0 * 0.0025)).abs() < 1e-6);
    }

    #[test]
    fn aim_multi_step_accumulates() {
        let mut yaw = 0.0;
        let mut pitch = 0.0;
        for step in [Vec2::new(5.0, 0.0), Vec2::new(5.0, 2.0), Vec2::new(0.0, 2.0)] {
            let (dy, dp) = aim_delta_from_drag(step, TOUCH_AIM_SENS);
            yaw += dy;
            pitch += dp;
        }
        assert!((yaw - (-10.0 * TOUCH_AIM_SENS)).abs() < 1e-5);
        assert!((pitch - (-4.0 * TOUCH_AIM_SENS)).abs() < 1e-5);
    }
}
