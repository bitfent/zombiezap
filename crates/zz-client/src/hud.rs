//! In-match HUD + combat visuals + stats overlay.
//!
//! Reads seam resources only (`LatestSnapshot`, `FxQueue`, `LastStats`,
//! `Roster`, `UiQueue`). Never touches networking. Gated by
//! `game::in_match` / `Session::Ended`.

#![allow(clippy::type_complexity)] // Bevy Query filters
#![allow(clippy::too_many_arguments)] // Bevy system params

use std::collections::{HashMap, HashSet};
use std::f32::consts::TAU;

use bevy::prelude::*;
use bevy::text::FontSize;
use zz_core::constants::{MAX_HEALTH, PLAYER_EYE};
use zz_core::snapshot::{Snapshot, WireLoot, WirePlayer, dequant_pos};

use crate::game::{self, Predicted, Session};
use crate::seams::{FxQueue, LastStats, LatestSnapshot, Roster, UiIntent, UiQueue, VisualEvent};
use crate::touch::TouchIntent;
use crate::voice::VoiceState;

// ── timing / layout constants ──────────────────────────────────────────────

const CROSSHAIR_FLASH_S: f32 = 0.120;
const VIGNETTE_PEAK: f32 = 0.35;
const VIGNETTE_DECAY_S: f32 = 0.5;
const TRACER_LIFE_S: f32 = 0.080;
const SPARK_LIFE_S: f32 = 0.150;
const BOOM_LIFE_S: f32 = 0.350;
const BOOM_R0: f32 = 0.5;
const BOOM_R1: f32 = 4.0;
const EYE: f32 = PLAYER_EYE;
/// Slight right+down offset from eye for own muzzle (metres, view space).
const MUZZLE_RIGHT: f32 = 0.18;
const MUZZLE_DOWN: f32 = 0.08;
const HEALTH_BAR_W: f32 = 200.0;
const HEALTH_BAR_H: f32 = 14.0;
const TEAMMATE_BAR_W: f32 = 120.0;

// ── plugin ─────────────────────────────────────────────────────────────────

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<HudLocal>().add_systems(
            Startup,
            (setup_fx_assets, setup_hud_ui, setup_fx_root).chain(),
        );
        app.add_systems(
            Update,
            (
                // After session so a same-frame GameStart → Playing makes chrome
                // Visible immediately (no one-frame blank HUD).
                gate_visibility.after(game::GameSessionSet),
                reset_hud_on_match_start.after(game::GameSessionSet),
                lift_hud_for_thumbs,
                pause_key.run_if(game::playing),
                (
                    update_crosshair,
                    update_vitals,
                    update_top_left,
                    update_teammates,
                    update_vignette,
                    update_paused_overlay,
                    update_death_banner,
                    update_mic_chip,
                )
                    .run_if(playing),
                (update_stats_overlay, stats_back_button).run_if(ended),
                // FX only while playing — Ended freezes the world (S7).
                (drain_fx, tick_fx, sync_loot, sync_flight_grenades).run_if(playing),
            ),
        );
    }
}

fn playing(session: Res<Session>) -> bool {
    matches!(*session, Session::Playing { .. })
}

fn ended(session: Res<Session>) -> bool {
    matches!(*session, Session::Ended { .. })
}

// ── local HUD state ────────────────────────────────────────────────────────

#[derive(Resource, Default)]
struct HudLocal {
    prev_health: Option<u8>,
    vignette: f32,
    crosshair_flash: f32,
    /// True when last from_me hit was a kill (hit_kind ≥ 2).
    crosshair_kill: bool,
}

// ── shared FX assets ───────────────────────────────────────────────────────

#[derive(Resource)]
struct FxAssets {
    unit_cube: Handle<Mesh>,
    unit_sphere: Handle<Mesh>,
    spark_mat: Handle<StandardMaterial>,
    loot_ammo: Handle<StandardMaterial>,
    loot_health: Handle<StandardMaterial>,
    loot_health_band: Handle<StandardMaterial>,
    loot_grenade: Handle<StandardMaterial>,
    flight_grenade: Handle<StandardMaterial>,
}

fn unlit(color: Color, alpha: AlphaMode) -> StandardMaterial {
    StandardMaterial {
        base_color: color,
        unlit: true,
        alpha_mode: alpha,
        perceptual_roughness: 1.0,
        metallic: 0.0,
        ..default()
    }
}

fn setup_fx_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(FxAssets {
        unit_cube: meshes.add(Cuboid::new(1.0, 1.0, 1.0)),
        unit_sphere: meshes.add(Sphere::new(0.5).mesh().uv(16, 8)),
        spark_mat: materials.add(unlit(Color::srgba(1.0, 0.95, 0.55, 1.0), AlphaMode::Blend)),
        loot_ammo: materials.add(unlit(Color::srgb(0.95, 0.85, 0.15), AlphaMode::Opaque)),
        loot_health: materials.add(unlit(Color::srgb(0.85, 0.12, 0.12), AlphaMode::Opaque)),
        loot_health_band: materials.add(unlit(Color::srgb(0.95, 0.95, 0.95), AlphaMode::Opaque)),
        loot_grenade: materials.add(unlit(Color::srgb(0.45, 0.55, 0.18), AlphaMode::Opaque)),
        flight_grenade: materials.add(unlit(Color::srgb(0.12, 0.12, 0.10), AlphaMode::Opaque)),
    });
}

// ── world-space FX root ────────────────────────────────────────────────────

#[derive(Component)]
struct FxRoot;

#[derive(Resource)]
struct FxRootEntity(Entity);

fn setup_fx_root(mut commands: Commands) {
    let e = commands
        .spawn((FxRoot, Transform::IDENTITY, Visibility::Hidden))
        .id();
    commands.insert_resource(FxRootEntity(e));
}

// ── UI markers ─────────────────────────────────────────────────────────────

/// Bottom-corner HUD panels that must clear thumbs in touch mode.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
enum HudCorner {
    BottomLeft,
    BottomRight,
}

/// Raise bottom vitals/ammo so the virtual stick (left) and FIRE cluster
/// (right) do not cover them. Desktop keeps the original inset.
fn lift_hud_for_thumbs(
    touch: Res<TouchIntent>,
    mut q: Query<(&HudCorner, &mut Node)>,
) {
    // Touch: park health above the stick (~28% from bottom), ammo above the
    // FIRE button cluster. Non-touch: original 28 px bottom inset.
    let (bl_bottom, br_bottom) = if touch.enabled {
        (px(200.0), px(200.0))
    } else {
        (px(28.0), px(28.0))
    };
    for (corner, mut node) in &mut q {
        match corner {
            HudCorner::BottomLeft => {
                if node.bottom != bl_bottom {
                    node.bottom = bl_bottom;
                }
            }
            HudCorner::BottomRight => {
                if node.bottom != br_bottom {
                    node.bottom = br_bottom;
                }
            }
        }
    }
}

/// Standard in-match HUD chrome (hidden unless Session::Playing).
/// Public so headless match-flow tests can assert visibility.
#[derive(Component)]
pub struct HudChrome;

#[derive(Component)]
struct CrosshairArm;

#[derive(Component)]
struct HealthFill;

#[derive(Component)]
struct HealthNum;

/// Proximity-voice mic chip (muted / live).
#[derive(Component)]
struct MicChip;

#[derive(Component)]
struct MicChipText;

#[derive(Component)]
struct AmmoText;

#[derive(Component)]
struct GrenadePips;

#[derive(Component)]
struct KillsText;

#[derive(Component)]
struct ZombiesText;

#[derive(Component)]
struct WaveText;

#[derive(Component)]
struct TeammateList;

#[derive(Component)]
struct TeammateRow {
    slot: u8,
}

#[derive(Component)]
struct TeammateName;

#[derive(Component)]
struct TeammateHealthFill;

#[derive(Component)]
struct TeammateDeadMark;

#[derive(Component)]
struct VignetteNode;

#[derive(Component)]
struct PausedOverlay;

#[derive(Component)]
struct DeathBanner;

#[derive(Component)]
struct StatsRoot;

#[derive(Component)]
struct StatsMatchLine;

#[derive(Component)]
struct StatsPlayerList;

#[derive(Component)]
struct StatsPlayerRow {
    slot: u8,
}

#[derive(Component)]
struct StatsBackButton;

#[derive(Component)]
struct StatsBackWasPressed(bool);

// ── FX markers ─────────────────────────────────────────────────────────────

#[derive(Component)]
struct TracerFx {
    age: f32,
    mat: Handle<StandardMaterial>,
}

#[derive(Component)]
struct SparkFx {
    age: f32,
}

#[derive(Component)]
struct BoomFx {
    age: f32,
    mat: Handle<StandardMaterial>,
}

#[derive(Component)]
struct LootFx {
    id: u16,
}

#[derive(Component)]
struct FlightGrenadeFx {
    id: u8,
}

// ── UI helpers ─────────────────────────────────────────────────────────────

fn mono(size: f32) -> TextFont {
    TextFont {
        font_size: FontSize::Px(size),
        ..default()
    }
}

fn panel_bg() -> BackgroundColor {
    BackgroundColor(Color::srgba(0.04, 0.05, 0.07, 0.72))
}

fn session_slot(session: &Session) -> Option<u8> {
    match *session {
        Session::Playing { my_slot } | Session::Ended { my_slot } => Some(my_slot),
        _ => None,
    }
}

fn my_player(snap: &Snapshot, my_slot: u8) -> Option<&WirePlayer> {
    snap.players.iter().find(|p| p.slot == my_slot)
}

// ── UI setup ───────────────────────────────────────────────────────────────

fn setup_hud_ui(mut commands: Commands) {
    // Crosshair: four small arms around the screen center.
    let arm = |w: f32, h: f32, ox: f32, oy: f32| {
        (
            CrosshairArm,
            Node {
                position_type: PositionType::Absolute,
                width: px(w),
                height: px(h),
                left: Val::Percent(50.0),
                top: Val::Percent(50.0),
                margin: UiRect {
                    left: px(ox - w * 0.5),
                    top: px(oy - h * 0.5),
                    ..default()
                },
                ..default()
            },
            BackgroundColor(Color::srgba(0.92, 0.95, 0.88, 0.9)),
            Visibility::Hidden,
        )
    };
    commands.spawn(arm(10.0, 2.0, -11.0, 0.0));
    commands.spawn(arm(10.0, 2.0, 11.0, 0.0));
    commands.spawn(arm(2.0, 10.0, 0.0, -11.0));
    commands.spawn(arm(2.0, 10.0, 0.0, 11.0));

    // Bottom-left: mic chip (above) + health bar + numeric.
    // Markers used by `lift_hud_for_thumbs` so touch mode can clear the stick zone.
    // Mic chip sits at the top of this column (left: 16, bottom: 28 desktop /
    // 200 touch) — muted by default, live when open-mic is transmitting.
    commands
        .spawn((
            HudChrome,
            HudCorner::BottomLeft,
            Node {
                position_type: PositionType::Absolute,
                bottom: px(28),
                left: px(16),
                flex_direction: FlexDirection::Column,
                row_gap: px(4),
                padding: UiRect::all(px(8)),
                ..default()
            },
            panel_bg(),
            Visibility::Hidden,
        ))
        .with_children(|p| {
            // Tiny mic chip: "MIC · MUTED" / "MIC · LIVE" / "MIC · …"
            p.spawn((
                MicChip,
                Node {
                    padding: UiRect::axes(px(6), px(3)),
                    border: UiRect::all(px(1)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.12, 0.12, 0.14, 0.9)),
                BorderColor::all(Color::srgba(0.45, 0.45, 0.5, 0.85)),
            ))
            .with_children(|chip| {
                chip.spawn((
                    MicChipText,
                    Text::new("MIC · MUTED"),
                    mono(12.0),
                    TextColor(Color::srgb(0.7, 0.72, 0.75)),
                ));
            });
            p.spawn((
                Node {
                    width: px(HEALTH_BAR_W),
                    height: px(HEALTH_BAR_H),
                    border: UiRect::all(px(1)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.15, 0.08, 0.08, 0.9)),
                BorderColor::all(Color::srgba(0.4, 0.2, 0.2, 0.9)),
            ))
            .with_children(|bar| {
                bar.spawn((
                    HealthFill,
                    Node {
                        width: px(HEALTH_BAR_W),
                        height: px(HEALTH_BAR_H - 2.0),
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.85, 0.12, 0.12)),
                ));
            });
            p.spawn((
                HealthNum,
                Text::new("100"),
                mono(16.0),
                TextColor(Color::srgb(0.95, 0.85, 0.8)),
            ));
        });

    // Bottom-right: ammo + grenade pips.
    // Lifted in touch mode so FIRE/JUMP/NADE thumbs don't cover ammo.
    commands
        .spawn((
            HudChrome,
            HudCorner::BottomRight,
            Node {
                position_type: PositionType::Absolute,
                bottom: px(28),
                right: px(16),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::FlexEnd,
                row_gap: px(4),
                padding: UiRect::all(px(8)),
                ..default()
            },
            panel_bg(),
            Visibility::Hidden,
        ))
        .with_children(|p| {
            p.spawn((
                AmmoText,
                Text::new("0 / 0"),
                mono(20.0),
                TextColor(Color::srgb(0.95, 0.9, 0.55)),
            ));
            p.spawn((
                GrenadePips,
                Text::new(""),
                mono(18.0),
                TextColor(Color::srgb(0.55, 0.75, 0.35)),
            ));
        });

    // Top-left: kills / live zombies / wave (below FPS counter).
    commands
        .spawn((
            HudChrome,
            Node {
                position_type: PositionType::Absolute,
                top: px(40),
                left: px(12),
                flex_direction: FlexDirection::Column,
                row_gap: px(2),
                padding: UiRect::all(px(8)),
                ..default()
            },
            panel_bg(),
            Visibility::Hidden,
        ))
        .with_children(|p| {
            p.spawn((
                KillsText,
                Text::new("KILLS 0"),
                mono(15.0),
                TextColor(Color::srgb(0.9, 0.92, 0.85)),
            ));
            p.spawn((
                ZombiesText,
                Text::new("ZEDS 0"),
                mono(15.0),
                TextColor(Color::srgb(0.85, 0.7, 0.55)),
            ));
            p.spawn((
                WaveText,
                Text::new("WAVE 1"),
                mono(15.0),
                TextColor(Color::srgb(0.7, 0.9, 0.95)),
            ));
        });

    // Top-right: teammate rows.
    commands.spawn((
        HudChrome,
        TeammateList,
        Node {
            position_type: PositionType::Absolute,
            top: px(40),
            right: px(12),
            flex_direction: FlexDirection::Column,
            row_gap: px(4),
            padding: UiRect::all(px(8)),
            min_width: px(160),
            ..default()
        },
        panel_bg(),
        Visibility::Hidden,
    ));

    // Damage vignette — full-screen, non-interactive.
    commands.spawn((
        VignetteNode,
        Node {
            position_type: PositionType::Absolute,
            width: percent(100),
            height: percent(100),
            left: px(0),
            top: px(0),
            ..default()
        },
        BackgroundColor(Color::srgba(0.85, 0.05, 0.05, 0.0)),
        bevy::ui::FocusPolicy::Pass,
        GlobalZIndex(10),
        Visibility::Hidden,
    ));

    // Paused overlay.
    commands
        .spawn((
            PausedOverlay,
            Node {
                position_type: PositionType::Absolute,
                width: percent(100),
                height: percent(100),
                left: px(0),
                top: px(0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.55)),
            bevy::ui::FocusPolicy::Pass,
            GlobalZIndex(20),
            Visibility::Hidden,
        ))
        .with_children(|p| {
            p.spawn((
                Text::new("PAUSED — [P] RESUME"),
                mono(28.0),
                TextColor(Color::srgb(0.95, 0.95, 0.85)),
            ));
        });

    // Own-death banner while team fights on.
    commands
        .spawn((
            DeathBanner,
            Node {
                position_type: PositionType::Absolute,
                width: percent(100),
                top: percent(35),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            bevy::ui::FocusPolicy::Pass,
            GlobalZIndex(15),
            Visibility::Hidden,
        ))
        .with_children(|p| {
            p.spawn((
                Text::new("YOU DIED — SPECTATING"),
                mono(32.0),
                TextColor(Color::srgb(0.95, 0.25, 0.2)),
                BackgroundColor(Color::srgba(0.05, 0.0, 0.0, 0.55)),
            ));
        });

    // Stats overlay (Session::Ended).
    commands
        .spawn((
            StatsRoot,
            Node {
                position_type: PositionType::Absolute,
                width: percent(100),
                height: percent(100),
                left: px(0),
                top: px(0),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                row_gap: px(12),
                padding: UiRect::all(px(24)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.02, 0.02, 0.04, 0.92)),
            GlobalZIndex(50),
            Visibility::Hidden,
        ))
        .with_children(|p| {
            p.spawn((
                Text::new("OVERRUN"),
                mono(42.0),
                TextColor(Color::srgb(0.95, 0.3, 0.22)),
            ));
            p.spawn((
                StatsMatchLine,
                Text::new(""),
                mono(16.0),
                TextColor(Color::srgb(0.8, 0.82, 0.78)),
            ));
            p.spawn((
                StatsPlayerList,
                Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: px(6),
                    padding: UiRect::all(px(12)),
                    min_width: px(420),
                    ..default()
                },
                panel_bg(),
            ));
            p.spawn((
                Button,
                StatsBackButton,
                StatsBackWasPressed(false),
                Node {
                    margin: UiRect::top(px(16)),
                    padding: UiRect::axes(px(20), px(12)),
                    border: UiRect::all(px(2)),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    ..default()
                },
                BorderColor::all(Color::srgb(0.7, 0.75, 0.55)),
                BackgroundColor(Color::srgb(0.12, 0.14, 0.10)),
            ))
            .with_children(|b| {
                b.spawn((
                    Text::new("[ BACK TO LOBBY ]"),
                    mono(18.0),
                    TextColor(Color::srgb(0.9, 0.95, 0.7)),
                ));
            });
        });
}

// ── visibility gate ────────────────────────────────────────────────────────

/// Clear vignette / crosshair flash when a new match begins so rematch never
/// inherits a full-screen red wash from the previous wipe.
fn reset_hud_on_match_start(session: Res<Session>, mut local: ResMut<HudLocal>) {
    if !session.is_changed() {
        return;
    }
    if matches!(*session, Session::Playing { .. }) {
        *local = HudLocal::default();
    }
}

fn gate_visibility(
    session: Res<Session>,
    mut sets: ParamSet<(
        Query<&mut Visibility, With<HudChrome>>,
        Query<&mut Visibility, With<CrosshairArm>>,
        Query<&mut Visibility, With<StatsRoot>>,
        Query<&mut Visibility, With<FxRoot>>,
        Query<&mut Visibility, With<VignetteNode>>,
        Query<&mut Visibility, With<PausedOverlay>>,
        Query<&mut Visibility, With<DeathBanner>>,
    )>,
) {
    let is_playing = matches!(*session, Session::Playing { .. });
    let is_ended = matches!(*session, Session::Ended { .. });
    // FX root only while actively playing — freeze combat visuals on OVERRUN.
    let fx_live = is_playing;

    let chrome_vis = if is_playing {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    for mut vis in sets.p0().iter_mut() {
        *vis = chrome_vis;
    }
    for mut vis in sets.p1().iter_mut() {
        *vis = chrome_vis;
    }
    let stats_vis = if is_ended {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    for mut vis in sets.p2().iter_mut() {
        *vis = stats_vis;
    }
    let fx_vis = if fx_live {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    for mut vis in sets.p3().iter_mut() {
        *vis = fx_vis;
    }
    // Force-hide overlays that only apply in Playing when we leave the match.
    if !is_playing {
        for mut vis in sets.p4().iter_mut() {
            *vis = Visibility::Hidden;
        }
        for mut vis in sets.p5().iter_mut() {
            *vis = Visibility::Hidden;
        }
        for mut vis in sets.p6().iter_mut() {
            *vis = Visibility::Hidden;
        }
    }
}

// ── pause key ──────────────────────────────────────────────────────────────

fn pause_key(keys: Res<ButtonInput<KeyCode>>, mut ui: ResMut<UiQueue>, session: Res<Session>) {
    if !matches!(*session, Session::Playing { .. }) {
        return;
    }
    if keys.just_pressed(KeyCode::KeyP) {
        ui.0.push_back(UiIntent::PauseToggle);
    }
}

// ── HUD updates ────────────────────────────────────────────────────────────

fn update_crosshair(
    mut local: ResMut<HudLocal>,
    time: Res<Time>,
    mut arms: Query<&mut BackgroundColor, With<CrosshairArm>>,
) {
    if local.crosshair_flash > 0.0 {
        local.crosshair_flash = (local.crosshair_flash - time.delta_secs()).max(0.0);
    }
    let color = if local.crosshair_flash > 0.0 {
        if local.crosshair_kill {
            Color::srgba(1.0, 0.25, 0.2, 1.0)
        } else {
            Color::srgba(1.0, 1.0, 0.95, 1.0)
        }
    } else {
        Color::srgba(0.92, 0.95, 0.88, 0.9)
    };
    for mut bg in &mut arms {
        *bg = BackgroundColor(color);
    }
}

fn update_vitals(
    session: Res<Session>,
    latest: Res<LatestSnapshot>,
    mut fill: Query<&mut Node, With<HealthFill>>,
    mut num: Query<&mut Text, (With<HealthNum>, Without<AmmoText>, Without<GrenadePips>)>,
    mut ammo: Query<&mut Text, (With<AmmoText>, Without<HealthNum>, Without<GrenadePips>)>,
    mut pips: Query<&mut Text, (With<GrenadePips>, Without<HealthNum>, Without<AmmoText>)>,
) {
    let Some(my_slot) = session_slot(&session) else {
        return;
    };
    let Some(snap) = latest.0.as_ref() else {
        return;
    };
    let Some(me) = my_player(snap, my_slot) else {
        return;
    };

    let frac = (me.health as f32 / MAX_HEALTH as f32).clamp(0.0, 1.0);
    for mut node in &mut fill {
        node.width = px(HEALTH_BAR_W * frac);
    }
    for mut t in &mut num {
        **t = format!("{}", me.health);
    }
    for mut t in &mut ammo {
        **t = format!("{} / {}", me.ammo_mag, me.ammo_reserve);
    }
    for mut t in &mut pips {
        **t = "●".repeat(me.grenades as usize);
    }
}

/// Mic chip bottom-left (above health): MUTED (default) / LIVE / DENIED / ….
fn update_mic_chip(
    voice: Res<VoiceState>,
    mut text_q: Query<&mut Text, With<MicChipText>>,
    mut chip_q: Query<(&mut BackgroundColor, &mut BorderColor), With<MicChip>>,
    mut color_q: Query<&mut TextColor, With<MicChipText>>,
) {
    let (label, bg, border, fg) = if voice.mic_denied {
        (
            "MIC · DENIED",
            Color::srgba(0.25, 0.08, 0.08, 0.92),
            Color::srgba(0.7, 0.25, 0.2, 0.9),
            Color::srgb(0.95, 0.55, 0.5),
        )
    } else if voice.muted {
        (
            "MIC · MUTED",
            Color::srgba(0.12, 0.12, 0.14, 0.9),
            Color::srgba(0.45, 0.45, 0.5, 0.85),
            Color::srgb(0.7, 0.72, 0.75),
        )
    } else if voice.self_live {
        (
            "MIC · LIVE",
            Color::srgba(0.08, 0.22, 0.12, 0.92),
            Color::srgba(0.35, 0.85, 0.45, 0.95),
            Color::srgb(0.55, 0.98, 0.65),
        )
    } else if !voice.mic_ready {
        (
            "MIC · …",
            Color::srgba(0.18, 0.16, 0.08, 0.92),
            Color::srgba(0.75, 0.65, 0.3, 0.9),
            Color::srgb(0.95, 0.88, 0.55),
        )
    } else {
        (
            "MIC · ON",
            Color::srgba(0.08, 0.16, 0.12, 0.9),
            Color::srgba(0.35, 0.65, 0.45, 0.85),
            Color::srgb(0.65, 0.9, 0.7),
        )
    };

    for mut t in &mut text_q {
        **t = label.to_string();
    }
    for mut c in &mut color_q {
        c.0 = fg;
    }
    for (mut bg_c, mut border_c) in &mut chip_q {
        *bg_c = BackgroundColor(bg);
        *border_c = BorderColor::all(border);
    }
}

fn update_top_left(
    session: Res<Session>,
    latest: Res<LatestSnapshot>,
    mut kills: Query<&mut Text, (With<KillsText>, Without<ZombiesText>, Without<WaveText>)>,
    mut zeds: Query<&mut Text, (With<ZombiesText>, Without<KillsText>, Without<WaveText>)>,
    mut wave: Query<&mut Text, (With<WaveText>, Without<KillsText>, Without<ZombiesText>)>,
) {
    let Some(my_slot) = session_slot(&session) else {
        return;
    };
    let Some(snap) = latest.0.as_ref() else {
        return;
    };
    let kills_n = my_player(snap, my_slot).map(|p| p.kills).unwrap_or(0);
    let zed_n = snap.zombies.len();
    let wave_n = u16::from(snap.difficulty) + 1;

    for mut t in &mut kills {
        **t = format!("KILLS {kills_n}");
    }
    for mut t in &mut zeds {
        **t = format!("ZEDS {zed_n}");
    }
    for mut t in &mut wave {
        **t = format!("WAVE {wave_n}");
    }
}

fn update_teammates(
    mut commands: Commands,
    session: Res<Session>,
    latest: Res<LatestSnapshot>,
    roster: Res<Roster>,
    list_q: Query<Entity, With<TeammateList>>,
    rows: Query<(Entity, &TeammateRow)>,
    children_q: Query<&Children>,
    mut names: Query<&mut Text, (With<TeammateName>, Without<TeammateDeadMark>)>,
    mut fills: Query<&mut Node, With<TeammateHealthFill>>,
    mut dead_marks: Query<
        (&mut Text, &mut Visibility),
        (With<TeammateDeadMark>, Without<TeammateName>),
    >,
) {
    let Ok(list) = list_q.single() else {
        return;
    };
    let Some(my_slot) = session_slot(&session) else {
        return;
    };
    let Some(snap) = latest.0.as_ref() else {
        return;
    };

    let wanted: Vec<(u8, String)> = roster
        .0
        .iter()
        .filter(|(slot, _, is_me)| !*is_me && *slot != my_slot)
        .map(|(slot, name, _)| (*slot, name.clone()))
        .collect();

    let existing: HashMap<u8, Entity> = rows.iter().map(|(e, r)| (r.slot, e)).collect();
    let wanted_slots: HashSet<u8> = wanted.iter().map(|(s, _)| *s).collect();

    for (e, row) in rows.iter() {
        if !wanted_slots.contains(&row.slot) {
            commands.entity(e).despawn();
        }
    }

    for (slot, name) in &wanted {
        if existing.contains_key(slot) {
            continue;
        }
        commands.entity(list).with_children(|p| {
            p.spawn((
                TeammateRow { slot: *slot },
                Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: px(2),
                    ..default()
                },
            ))
            .with_children(|row| {
                row.spawn((
                    TeammateName,
                    Text::new(name.clone()),
                    mono(13.0),
                    TextColor(Color::srgb(0.88, 0.9, 0.85)),
                ));
                row.spawn((
                    Node {
                        width: px(TEAMMATE_BAR_W),
                        height: px(8),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.15, 0.08, 0.08, 0.9)),
                ))
                .with_children(|bar| {
                    bar.spawn((
                        TeammateHealthFill,
                        Node {
                            width: px(TEAMMATE_BAR_W),
                            height: px(8),
                            ..default()
                        },
                        BackgroundColor(Color::srgb(0.7, 0.15, 0.15)),
                    ));
                });
                row.spawn((
                    TeammateDeadMark,
                    Text::new(""),
                    mono(12.0),
                    TextColor(Color::srgb(0.95, 0.35, 0.3)),
                    Visibility::Hidden,
                ));
            });
        });
    }

    // Refresh row contents for entities still alive this frame.
    for (e, row) in rows.iter() {
        if !wanted_slots.contains(&row.slot) {
            continue;
        }
        let p = snap.players.iter().find(|p| p.slot == row.slot);
        let (health, alive) = match p {
            Some(pl) => (pl.health, pl.alive),
            None => (0, false),
        };
        let display_name = roster
            .0
            .iter()
            .find(|(s, _, _)| *s == row.slot)
            .map(|(_, n, _)| n.as_str())
            .unwrap_or("?");
        let frac = (health as f32 / MAX_HEALTH as f32).clamp(0.0, 1.0);

        let Ok(kids) = children_q.get(e) else {
            continue;
        };
        for kid in kids.iter() {
            if let Ok(mut t) = names.get_mut(kid) {
                **t = display_name.to_string();
            }
            if let Ok((mut t, mut vis)) = dead_marks.get_mut(kid) {
                if alive {
                    **t = String::new();
                    *vis = Visibility::Hidden;
                } else {
                    **t = "DEAD".into();
                    *vis = Visibility::Visible;
                }
            }
            // Health fill is a grandchild under the bar node.
            if let Ok(grand) = children_q.get(kid) {
                for g in grand.iter() {
                    if let Ok(mut node) = fills.get_mut(g) {
                        node.width = px(TEAMMATE_BAR_W * frac);
                    }
                }
            }
            if let Ok(mut node) = fills.get_mut(kid) {
                node.width = px(TEAMMATE_BAR_W * frac);
            }
        }
    }
}

fn update_vignette(
    mut local: ResMut<HudLocal>,
    session: Res<Session>,
    latest: Res<LatestSnapshot>,
    time: Res<Time>,
    mut q: Query<(&mut BackgroundColor, &mut Visibility), With<VignetteNode>>,
) {
    let Some(my_slot) = session_slot(&session) else {
        return;
    };
    if let Some(snap) = latest.0.as_ref()
        && let Some(me) = my_player(snap, my_slot)
    {
        if let Some(prev) = local.prev_health
            && me.health < prev
        {
            local.vignette = VIGNETTE_PEAK;
        }
        local.prev_health = Some(me.health);
    }
    local.vignette = vignette_decay(local.vignette, time.delta_secs(), VIGNETTE_DECAY_S);

    for (mut bg, mut vis) in &mut q {
        *vis = if local.vignette > 0.001 {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        *bg = BackgroundColor(Color::srgba(0.85, 0.05, 0.05, local.vignette));
    }
}

fn update_paused_overlay(
    latest: Res<LatestSnapshot>,
    mut q: Query<&mut Visibility, With<PausedOverlay>>,
) {
    let paused = latest.0.as_ref().is_some_and(|s| s.paused);
    for mut vis in &mut q {
        *vis = if paused {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
}

fn update_death_banner(
    session: Res<Session>,
    latest: Res<LatestSnapshot>,
    mut q: Query<&mut Visibility, With<DeathBanner>>,
) {
    let Some(my_slot) = session_slot(&session) else {
        return;
    };
    let dead = latest
        .0
        .as_ref()
        .and_then(|s| my_player(s, my_slot))
        .is_some_and(|p| !p.alive);
    for mut vis in &mut q {
        *vis = if dead {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
}

// ── stats overlay ──────────────────────────────────────────────────────────

fn update_stats_overlay(
    mut commands: Commands,
    last: Res<LastStats>,
    roster: Res<Roster>,
    mut match_line: Query<&mut Text, With<StatsMatchLine>>,
    list_q: Query<Entity, With<StatsPlayerList>>,
    mut rows: Query<(Entity, &StatsPlayerRow, &mut Text), Without<StatsMatchLine>>,
) {
    let Some(stats) = last.0.as_ref() else {
        for mut t in &mut match_line {
            **t = "…".into();
        }
        // Drop stale rows when last_stats is cleared (GameStart / BackToLobby).
        for (e, _, _) in rows.iter() {
            commands.entity(e).despawn();
        }
        return;
    };

    let dur_s = stats.duration_ms as f32 / 1000.0;
    let wave = u16::from(stats.difficulty_reached) + 1;
    for mut t in &mut match_line {
        **t = format!(
            "{dur_s:.0}s  ·  {zk} zombies killed  ·  peak horde {peak}  ·  wave {wave}",
            zk = stats.zombies_killed,
            peak = stats.peak_zombies,
        );
    }

    let Ok(list) = list_q.single() else {
        return;
    };

    let wanted_slots: HashSet<u8> = stats.players.iter().map(|p| p.slot).collect();
    let to_despawn: Vec<Entity> = rows
        .iter()
        .filter(|(_, row, _)| !wanted_slots.contains(&row.slot))
        .map(|(e, _, _)| e)
        .collect();
    for e in to_despawn {
        commands.entity(e).despawn();
    }

    // Snapshot existing slots first (no simultaneous mut borrow).
    let existing: HashMap<u8, Entity> = rows.iter().map(|(e, r, _)| (r.slot, e)).collect();

    // Per-player lines — always rewrite text so rematch never keeps the
    // previous match's survivor numbers under a fresh header.
    let mut spawn_lines: Vec<(u8, String)> = Vec::new();
    for p in &stats.players {
        let name = if p.name.is_empty() {
            roster
                .0
                .iter()
                .find(|(s, _, _)| *s == p.slot)
                .map(|(_, n, _)| n.clone())
                .unwrap_or_else(|| format!("P{}", p.slot))
        } else {
            p.name.clone()
        };
        let acc = accuracy_pct(p.hits, p.shots_fired);
        let alive_s = p.time_alive_ms as f32 / 1000.0;
        let line = format!(
            "{name}  ·  K {k}  ·  DMG {d}  ·  ACC {acc:.0}%  ·  {alive_s:.0}s",
            k = p.kills,
            d = p.damage_dealt,
        );
        if let Some(&e) = existing.get(&p.slot) {
            if let Ok((_, _, mut text)) = rows.get_mut(e) {
                **text = line;
            }
        } else {
            spawn_lines.push((p.slot, line));
        }
    }
    for (slot, line) in spawn_lines {
        commands.entity(list).with_children(|c| {
            c.spawn((
                StatsPlayerRow { slot },
                Text::new(line),
                mono(14.0),
                TextColor(Color::srgb(0.88, 0.9, 0.82)),
            ));
        });
    }
}

fn stats_back_button(
    // Do not require `Changed<Interaction>`: same-frame press+release (egui /
    // trackpad / automation) can leave Interaction never observed as Pressed
    // across two frames. Edge-detect with our own was-pressed flag instead.
    mut q: Query<
        (&Interaction, &mut StatsBackWasPressed, &mut BackgroundColor),
        With<StatsBackButton>,
    >,
    mut ui: ResMut<UiQueue>,
) {
    for (interaction, mut was, mut bg) in &mut q {
        match *interaction {
            Interaction::Pressed => {
                *bg = BackgroundColor(Color::srgb(0.25, 0.3, 0.15));
                if !was.0 {
                    was.0 = true;
                    ui.0.push_back(UiIntent::BackToLobby);
                }
            }
            Interaction::Hovered => {
                was.0 = false;
                *bg = BackgroundColor(Color::srgb(0.18, 0.22, 0.12));
            }
            Interaction::None => {
                was.0 = false;
                *bg = BackgroundColor(Color::srgb(0.12, 0.14, 0.10));
            }
        }
    }
}

// ── combat FX ──────────────────────────────────────────────────────────────

fn drain_fx(
    mut commands: Commands,
    mut fx: ResMut<FxQueue>,
    mut local: ResMut<HudLocal>,
    assets: Res<FxAssets>,
    root: Res<FxRootEntity>,
    latest: Res<LatestSnapshot>,
    session: Res<Session>,
    predicted: Res<Predicted>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let my_slot = session_slot(&session);
    while let Some(ev) = fx.0.pop_front() {
        match ev {
            VisualEvent::Shot {
                slot,
                end,
                hit_kind,
                from_me,
            } => {
                if from_me && hit_kind >= 1 {
                    local.crosshair_flash = CROSSHAIR_FLASH_S;
                    local.crosshair_kill = hit_kind >= 2;
                }
                let muzzle = muzzle_for_slot(slot, from_me, my_slot, &predicted, latest.0.as_ref());
                let xform = tracer_transform(muzzle, end);
                let mat =
                    materials.add(unlit(Color::srgba(1.0, 0.92, 0.45, 0.95), AlphaMode::Blend));
                commands.entity(root.0).with_children(|c| {
                    c.spawn((
                        TracerFx {
                            age: 0.0,
                            mat: mat.clone(),
                        },
                        Mesh3d(assets.unit_cube.clone()),
                        MeshMaterial3d(mat),
                        xform,
                    ));
                    c.spawn((
                        SparkFx { age: 0.0 },
                        Mesh3d(assets.unit_cube.clone()),
                        MeshMaterial3d(assets.spark_mat.clone()),
                        Transform::from_translation(end).with_scale(Vec3::splat(0.12)),
                    ));
                });
            }
            VisualEvent::Boom { pos } => {
                let mat =
                    materials.add(unlit(Color::srgba(1.0, 0.45, 0.08, 0.75), AlphaMode::Blend));
                commands.entity(root.0).with_children(|c| {
                    c.spawn((
                        BoomFx {
                            age: 0.0,
                            mat: mat.clone(),
                        },
                        Mesh3d(assets.unit_sphere.clone()),
                        MeshMaterial3d(mat),
                        Transform::from_translation(pos).with_scale(Vec3::splat(BOOM_R0 * 2.0)),
                    ));
                });
            }
        }
    }
}

fn muzzle_for_slot(
    slot: u8,
    from_me: bool,
    my_slot: Option<u8>,
    predicted: &Predicted,
    snap: Option<&Snapshot>,
) -> Vec3 {
    if from_me || my_slot == Some(slot) {
        let rot = Quat::from_euler(EulerRot::YXZ, predicted.yaw, predicted.pitch, 0.0);
        let eye = Vec3::new(predicted.body.x, predicted.body.y + EYE, predicted.body.z);
        return eye + rot * Vec3::X * MUZZLE_RIGHT + rot * Vec3::NEG_Y * MUZZLE_DOWN;
    }
    if let Some(snap) = snap
        && let Some(p) = snap.players.iter().find(|p| p.slot == slot)
    {
        return Vec3::new(
            dequant_pos(p.pos[0]),
            dequant_pos(p.pos[1]) + EYE,
            dequant_pos(p.pos[2]),
        );
    }
    Vec3::ZERO
}

fn tick_fx(
    mut commands: Commands,
    time: Res<Time>,
    mut tracers: Query<(Entity, &mut TracerFx, &mut Transform)>,
    mut sparks: Query<(Entity, &mut SparkFx, &mut Transform), Without<TracerFx>>,
    mut booms: Query<(Entity, &mut BoomFx, &mut Transform), (Without<TracerFx>, Without<SparkFx>)>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let dt = time.delta_secs();

    for (e, mut fx, mut tf) in &mut tracers {
        fx.age += dt;
        let t = (fx.age / TRACER_LIFE_S).clamp(0.0, 1.0);
        let alpha = 1.0 - t;
        if let Some(mut mat) = materials.get_mut(&fx.mat) {
            let mut c = mat.base_color.to_srgba();
            c.alpha = 0.95 * alpha;
            mat.base_color = Color::Srgba(c);
        }
        tf.scale.x = 0.025 * alpha.max(0.05);
        tf.scale.y = 0.025 * alpha.max(0.05);
        if fx.age >= TRACER_LIFE_S {
            commands.entity(e).despawn();
        }
    }

    for (e, mut fx, mut tf) in &mut sparks {
        fx.age += dt;
        let t = (fx.age / SPARK_LIFE_S).clamp(0.0, 1.0);
        tf.scale = Vec3::splat((0.12 * (1.0 - t)).max(0.01));
        if fx.age >= SPARK_LIFE_S {
            commands.entity(e).despawn();
        }
    }

    for (e, mut fx, mut tf) in &mut booms {
        fx.age += dt;
        let t = (fx.age / BOOM_LIFE_S).clamp(0.0, 1.0);
        let r = BOOM_R0 + (BOOM_R1 - BOOM_R0) * t;
        // unit sphere radius 0.5 → diameter scale = 2r
        tf.scale = Vec3::splat(r * 2.0);
        if let Some(mut mat) = materials.get_mut(&fx.mat) {
            let mut c = mat.base_color.to_srgba();
            c.alpha = 0.75 * (1.0 - t);
            mat.base_color = Color::Srgba(c);
        }
        if fx.age >= BOOM_LIFE_S {
            commands.entity(e).despawn();
        }
    }
}

fn sync_loot(
    mut commands: Commands,
    latest: Res<LatestSnapshot>,
    assets: Res<FxAssets>,
    root: Res<FxRootEntity>,
    time: Res<Time>,
    mut existing: Query<(Entity, &LootFx, &mut Transform)>,
) {
    let Some(snap) = latest.0.as_ref() else {
        for (e, _, _) in existing.iter() {
            commands.entity(e).despawn();
        }
        return;
    };
    let live: HashMap<u16, &WireLoot> = snap.loot.iter().map(|l| (l.id, l)).collect();
    let mut seen = HashSet::new();

    for (e, loot, mut tf) in &mut existing {
        if let Some(l) = live.get(&loot.id) {
            seen.insert(loot.id);
            let pos = Vec3::new(
                dequant_pos(l.pos[0]),
                dequant_pos(l.pos[1]),
                dequant_pos(l.pos[2]),
            );
            let bob = (time.elapsed_secs() * 2.2 + loot.id as f32 * 0.7).sin() * 0.08;
            let spin = time.elapsed_secs() * 1.4 + loot.id as f32;
            let scale = tf.scale;
            tf.translation = pos + Vec3::Y * (0.25 + bob);
            tf.rotation = Quat::from_rotation_y(spin.rem_euclid(TAU));
            tf.scale = scale;
        } else {
            commands.entity(e).despawn();
        }
    }

    for (id, l) in live {
        if seen.contains(&id) {
            continue;
        }
        let pos = Vec3::new(
            dequant_pos(l.pos[0]),
            dequant_pos(l.pos[1]) + 0.25,
            dequant_pos(l.pos[2]),
        );
        commands.entity(root.0).with_children(|c| match l.kind {
            0 => {
                c.spawn((
                    LootFx { id },
                    Mesh3d(assets.unit_cube.clone()),
                    MeshMaterial3d(assets.loot_ammo.clone()),
                    Transform::from_translation(pos).with_scale(Vec3::new(0.28, 0.18, 0.22)),
                ));
            }
            1 => {
                c.spawn((
                    LootFx { id },
                    Mesh3d(assets.unit_cube.clone()),
                    MeshMaterial3d(assets.loot_health.clone()),
                    Transform::from_translation(pos).with_scale(Vec3::new(0.26, 0.26, 0.26)),
                ))
                .with_children(|box_| {
                    box_.spawn((
                        Mesh3d(assets.unit_cube.clone()),
                        MeshMaterial3d(assets.loot_health_band.clone()),
                        Transform::from_scale(Vec3::new(1.05, 0.22, 1.05)),
                    ));
                });
            }
            _ => {
                c.spawn((
                    LootFx { id },
                    Mesh3d(assets.unit_sphere.clone()),
                    MeshMaterial3d(assets.loot_grenade.clone()),
                    Transform::from_translation(pos).with_scale(Vec3::splat(0.28)),
                ));
            }
        });
    }
}

fn sync_flight_grenades(
    mut commands: Commands,
    latest: Res<LatestSnapshot>,
    assets: Res<FxAssets>,
    root: Res<FxRootEntity>,
    mut existing: Query<(Entity, &FlightGrenadeFx, &mut Transform)>,
) {
    let Some(snap) = latest.0.as_ref() else {
        for (e, _, _) in existing.iter() {
            commands.entity(e).despawn();
        }
        return;
    };
    let live: HashMap<u8, Vec3> = snap
        .grenades
        .iter()
        .map(|g| {
            (
                g.id,
                Vec3::new(
                    dequant_pos(g.pos[0]),
                    dequant_pos(g.pos[1]),
                    dequant_pos(g.pos[2]),
                ),
            )
        })
        .collect();
    let mut seen = HashSet::new();

    for (e, g, mut tf) in &mut existing {
        if let Some(pos) = live.get(&g.id) {
            seen.insert(g.id);
            tf.translation = *pos;
        } else {
            commands.entity(e).despawn();
        }
    }

    for (id, pos) in live {
        if seen.contains(&id) {
            continue;
        }
        commands.entity(root.0).with_children(|c| {
            c.spawn((
                FlightGrenadeFx { id },
                Mesh3d(assets.unit_sphere.clone()),
                MeshMaterial3d(assets.flight_grenade.clone()),
                Transform::from_translation(pos).with_scale(Vec3::splat(0.18)),
            ));
        });
    }
}

// ── pure helpers (unit-tested) ─────────────────────────────────────────────

/// Accuracy percentage; 0 when no shots fired.
pub(crate) fn accuracy_pct(hits: u32, shots_fired: u32) -> f32 {
    if shots_fired == 0 {
        0.0
    } else {
        (hits as f32 / shots_fired as f32) * 100.0
    }
}

/// Linear decay of vignette alpha toward 0 over `decay_s` seconds from peak.
pub(crate) fn vignette_decay(current: f32, dt: f32, decay_s: f32) -> f32 {
    if decay_s <= 0.0 || current <= 0.0 {
        return 0.0;
    }
    (current - (VIGNETTE_PEAK / decay_s) * dt).max(0.0)
}

/// Thin cuboid from `from` to `to`: Z-axis aligned with the segment, unit cube scaled.
pub(crate) fn tracer_transform(from: Vec3, to: Vec3) -> Transform {
    let dir = to - from;
    let len = dir.length().max(1e-4);
    let mid = from + dir * 0.5;
    let rot = Quat::from_rotation_arc(Vec3::Z, dir / len);
    Transform {
        translation: mid,
        rotation: rot,
        scale: Vec3::new(0.025, 0.025, len),
    }
}

// ── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accuracy_zero_shots() {
        assert_eq!(accuracy_pct(0, 0), 0.0);
        assert_eq!(accuracy_pct(5, 0), 0.0);
    }

    #[test]
    fn accuracy_full_and_half() {
        assert!((accuracy_pct(10, 10) - 100.0).abs() < 1e-4);
        assert!((accuracy_pct(1, 2) - 50.0).abs() < 1e-4);
        assert!((accuracy_pct(0, 8) - 0.0).abs() < 1e-4);
    }

    #[test]
    fn vignette_decays_linearly() {
        let v0 = VIGNETTE_PEAK;
        let half = vignette_decay(v0, VIGNETTE_DECAY_S * 0.5, VIGNETTE_DECAY_S);
        assert!((half - VIGNETTE_PEAK * 0.5).abs() < 1e-4);
        let done = vignette_decay(v0, VIGNETTE_DECAY_S, VIGNETTE_DECAY_S);
        assert!(done <= 1e-5);
        assert_eq!(vignette_decay(0.0, 0.1, VIGNETTE_DECAY_S), 0.0);
    }

    #[test]
    fn tracer_midpoint_and_length() {
        let a = Vec3::new(0.0, 0.0, 0.0);
        let b = Vec3::new(0.0, 0.0, 10.0);
        let t = tracer_transform(a, b);
        assert!((t.translation - Vec3::new(0.0, 0.0, 5.0)).length() < 1e-4);
        assert!((t.scale.z - 10.0).abs() < 1e-4);
        let forward = t.rotation * Vec3::Z;
        assert!((forward - Vec3::Z).length() < 1e-3);
    }

    #[test]
    fn tracer_diagonal() {
        let a = Vec3::ZERO;
        let b = Vec3::new(3.0, 4.0, 0.0);
        let t = tracer_transform(a, b);
        assert!((t.scale.z - 5.0).abs() < 1e-3);
        assert!((t.translation - Vec3::new(1.5, 2.0, 0.0)).length() < 1e-3);
        let dir = (t.rotation * Vec3::Z).normalize();
        let expected = (b - a).normalize();
        assert!((dir - expected).length() < 1e-3);
    }
}
