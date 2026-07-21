//! Headless match-flow integration tests (no winit / no GPU present).
//!
//! Reproduces the ECS-level conditions behind the browser regressions:
//! S1 HUD visibility on GameStart, S2 first-map build (MapRoot + no
//! Placeholders), S3 rematch wipe of remotes / LastStats / session state.

use std::time::Duration;

use bevy::prelude::*;
use bevy::time::TimeUpdateStrategy;
use zz_client::game::{GamePlugin, Predicted, RemotePlayer, RemoteZombie, Session};
use zz_client::hud::{HudChrome, HudPlugin};
use zz_client::map_render::{CurrentMap, MapRenderPlugin, MapRoot, Placeholder};
use zz_client::net::{NetClient, NetEvent};
use zz_client::seams::{LastStats, LatestSnapshot, Roster};
use zz_client::touch::TouchIntent;
use zz_client::voice::{VoiceRx, VoiceState};
use zz_core::constants::{MAG_SIZE, MAX_HEALTH, START_GRENADES, START_RESERVE_AMMO};
use zz_core::map::generate_map;
use zz_core::protocol::{MatchStats, PlayerStats, RosterPlayer, ServerMsg};
use zz_core::snapshot::{Snapshot, WirePlayer, quant_pos3, quant_yaw16};
use zz_core::types::EnvKind;

/// Minimal plugin set: assets + session + map build + HUD gate, no window/GPU.
fn headless_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .add_plugins(AssetPlugin::default())
        .init_asset::<Mesh>()
        .init_asset::<Image>()
        .init_asset::<StandardMaterial>()
        .insert_resource(GlobalAmbientLight::default())
        .insert_resource(ClearColor::default())
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            16,
        )))
        .insert_resource(NetClient::disconnected())
        // Input resources game/hud systems require (no full InputPlugin).
        .insert_resource(ButtonInput::<KeyCode>::default())
        .insert_resource(ButtonInput::<MouseButton>::default())
        .insert_resource(bevy::input::mouse::AccumulatedMouseMotion::default())
        // Seam resources HUD/voice systems need — skip full Touch/Voice plugins
        // (they pull audio / pointer graph that MinimalPlugins omits).
        .insert_resource(TouchIntent::default())
        .insert_resource(VoiceState::default())
        .insert_resource(VoiceRx::default())
        .add_plugins((GamePlugin, MapRenderPlugin, HudPlugin));

    // 3D camera so rebuild can attach DistanceFog / clear colour.
    app.world_mut().spawn((
        Camera3d::default(),
        Camera::default(),
        Transform::default(),
    ));

    // Skeleton placeholders — first map build must clear these.
    for i in 0..3 {
        app.world_mut().spawn((
            Placeholder,
            Name::new(format!("ph-{i}")),
            Transform::default(),
            Visibility::default(),
        ));
    }

    // Drive a few frames so Startup (HUD spawn) runs.
    for _ in 0..2 {
        app.update();
    }
    // Leave Boot so connect_on_start stops hammering a missing server.
    *app.world_mut().resource_mut::<Session>() = Session::Menu;
    app.update();
    app
}

fn count_with<T: Component>(world: &mut World) -> usize {
    let mut q = world.query_filtered::<Entity, With<T>>();
    q.iter(world).count()
}

fn any_hud_visible(world: &mut World) -> bool {
    let mut q = world.query_filtered::<&Visibility, With<HudChrome>>();
    q.iter(world)
        .any(|v| matches!(*v, Visibility::Visible))
}

fn simulate_game_start(app: &mut App, seed: &str, slot: u8) {
    let map = generate_map(EnvKind::Urban, seed);
    app.world_mut().insert_resource(CurrentMap(map));
    *app.world_mut().resource_mut::<Session>() = Session::Playing { my_slot: slot };
    app.world_mut().resource_mut::<LastStats>().0 = None;
    app.world_mut().resource_mut::<LatestSnapshot>().0 = None;
    app.world_mut().resource_mut::<Roster>().0 = vec![(slot, "tester".into(), true)];
}

#[test]
fn game_start_builds_map_and_shows_hud_within_two_ticks() {
    let mut app = headless_app();
    assert!(
        count_with::<Placeholder>(app.world_mut()) >= 3,
        "fixture placeholders present"
    );
    assert_eq!(count_with::<MapRoot>(app.world_mut()), 0);

    simulate_game_start(&mut app, "match-flow-s1", 0);

    // Two ticks: command-apply of CurrentMap + rebuild (and HUD gate).
    app.update();
    app.update();

    assert_eq!(
        count_with::<Placeholder>(app.world_mut()),
        0,
        "S2: placeholders must be gone after first map build"
    );
    assert!(
        count_with::<MapRoot>(app.world_mut()) >= 1,
        "S2: MapRoot must exist within 2 ticks of GameStart"
    );
    assert!(
        matches!(
            *app.world().resource::<Session>(),
            Session::Playing { my_slot: 0 }
        ),
        "session Playing"
    );
    assert!(
        any_hud_visible(app.world_mut()),
        "S1: HudChrome must be Visible while Playing"
    );
    // Atmosphere applied: ClearColor no longer pure default black/menu.
    let clear = app.world().resource::<ClearColor>().0;
    let s = clear.to_srgba();
    assert!(
        s.red + s.green + s.blue > 0.4,
        "S5: sky clear colour should be bright urban blue-ish, got {s:?}"
    );
}

#[test]
fn rematch_wipes_stale_horde_stats_and_rebuilds_map() {
    let mut app = headless_app();

    // First match.
    simulate_game_start(&mut app, "match-flow-a", 0);
    app.update();
    app.update();
    assert!(count_with::<MapRoot>(app.world_mut()) >= 1);

    // Stale remote zombies + player + OVERRUN stats as if match 1 just ended.
    for id in 0..12u16 {
        app.world_mut().spawn((
            RemoteZombie::new(id, 0),
            Transform::from_xyz(id as f32, 0.0, 0.0),
            Visibility::default(),
        ));
    }
    app.world_mut().spawn((
        RemotePlayer::new(1),
        Transform::default(),
        Visibility::default(),
    ));
    app.world_mut().resource_mut::<LastStats>().0 = Some(MatchStats {
        duration_ms: 37_000,
        zombies_killed: 25,
        peak_zombies: 25,
        difficulty_reached: 0,
        players: vec![PlayerStats {
            slot: 0,
            name: "tester".into(),
            kills: 0,
            damage_dealt: 0,
            shots_fired: 0,
            hits: 0,
            grenades_thrown: 0,
            time_alive_ms: 53_000, // deliberately > duration (pre-fix shape)
        }],
    });
    *app.world_mut().resource_mut::<Session>() = Session::Ended { my_slot: 0 };
    app.update();

    assert_eq!(count_with::<RemoteZombie>(app.world_mut()), 12);
    assert!(app.world().resource::<LastStats>().0.is_some());

    // BackToLobby intent path: leave match → cleanup_match_visuals.
    *app.world_mut().resource_mut::<Session>() = Session::InLobby;
    app.update();
    app.update();

    assert_eq!(
        count_with::<RemoteZombie>(app.world_mut()),
        0,
        "S3: remotes despawned on leave"
    );
    assert_eq!(
        count_with::<RemotePlayer>(app.world_mut()),
        0,
        "S3: remote players despawned on leave"
    );
    assert!(
        app.world().resource::<LastStats>().0.is_none(),
        "S3: LastStats cleared on leave"
    );

    // Second GameStart — fresh map + Playing, no stale OVERRUN.
    simulate_game_start(&mut app, "match-flow-b", 0);
    app.update();
    app.update();

    assert!(
        matches!(
            *app.world().resource::<Session>(),
            Session::Playing { my_slot: 0 }
        ),
        "S3: session Playing after second GameStart"
    );
    assert_eq!(
        count_with::<RemoteZombie>(app.world_mut()),
        0,
        "S3: zero RemoteZombie from old match after rematch"
    );
    assert!(
        count_with::<MapRoot>(app.world_mut()) >= 1,
        "S3: fresh MapRoot after rematch"
    );
    assert!(
        any_hud_visible(app.world_mut()),
        "S3/S1: HUD visible in rematch"
    );
    assert!(
        app.world().resource::<LastStats>().0.is_none(),
        "S3: LastStats still clear while Playing"
    );
}

/// M14b: after GameStart + first snapshot, prediction is synced and at least
/// one encoded input frame is queued within a few ticks (even with no keys).
#[test]
fn game_start_snapshot_syncs_and_queues_input() {
    let mut app = headless_app();
    // Leave Menu so we're ready for a synthetic GameStart (not Boot reconnect).
    *app.world_mut().resource_mut::<Session>() = Session::InLobby;
    app.update();

    let slot = 0u8;
    let spawn = generate_map(EnvKind::Urban, "match-flow-input").spawns[0];
    let snap = Snapshot {
        tick: 1,
        game_time_ms: 33,
        difficulty: 0,
        paused: false,
        players: vec![WirePlayer {
            slot,
            pos: quant_pos3(spawn.x, 0.0, spawn.z),
            yaw: quant_yaw16(spawn.yaw),
            pitch: 0,
            health: MAX_HEALTH,
            ammo_mag: MAG_SIZE,
            ammo_reserve: START_RESERVE_AMMO,
            grenades: START_GRENADES,
            kills: 0,
            alive: true,
            last_acked_seq: 0,
        }],
        zombies: vec![],
        loot: vec![],
        grenades: vec![],
        shots: vec![],
        booms: vec![],
    };

    {
        let mut net = app.world_mut().resource_mut::<NetClient>();
        net.inject(NetEvent::Msg(ServerMsg::GameStart {
            map_seed: "match-flow-input".into(),
            env: EnvKind::Urban,
            your_slot: slot,
            players: vec![RosterPlayer {
                slot,
                id: "c1".into(),
                name: "tester".into(),
            }],
        }));
        net.inject(NetEvent::Snap(snap));
        net.take_outbound_bin(); // clear any prior
    }

    // N ticks of fixed 16 ms: GameStart+Snap → synced + idle input send.
    let mut saw_synced = false;
    let mut saw_input = false;
    for i in 0..8 {
        app.update();
        let synced = app.world().resource::<Predicted>().is_synced();
        if synced {
            saw_synced = true;
        }
        let out = app.world_mut().resource_mut::<NetClient>().take_outbound_bin();
        if !out.is_empty() {
            // BIN_INPUT tag = 0
            assert_eq!(out[0].first().copied(), Some(0), "frame {i}: expected BIN_INPUT");
            saw_input = true;
            break;
        }
    }

    assert!(
        saw_synced,
        "predicted.synced must become true after GameStart + first snapshot"
    );
    assert!(
        saw_input,
        "at least one encoded input frame must be queued within N ticks after sync"
    );
    assert!(
        matches!(
            *app.world().resource::<Session>(),
            Session::Playing { my_slot: 0 }
        ),
        "session Playing"
    );
}
