//! Headless match-flow integration tests (no winit / no GPU present).
//!
//! Reproduces the ECS-level conditions behind the browser regressions:
//! S1 HUD visibility on GameStart, S2 first-map build (MapRoot + no
//! Placeholders), S3 rematch wipe of remotes / LastStats / session state.

use std::time::Duration;

use bevy::prelude::*;
use bevy::time::TimeUpdateStrategy;
use zz_client::game::{GamePlugin, LodMetrics, Predicted, RemotePlayer, RemoteZombie, Session};
use zz_client::hud::{HudChrome, HudPlugin};
use zz_client::map_render::{CurrentMap, MapRenderPlugin, MapRoot, Placeholder};
use zz_client::net::{NetClient, NetEvent};
use zz_client::seams::{LastStats, LatestSnapshot, Roster};
use zz_client::touch::TouchIntent;
use zz_client::voice::{VoiceRx, VoiceState};
use zz_core::constants::{MAG_SIZE, MAX_HEALTH, START_GRENADES, START_RESERVE_AMMO};
use zz_core::map::generate_map;
use zz_core::protocol::{MatchStats, PlayerStats, RosterPlayer, ServerMsg};
use zz_core::snapshot::{Snapshot, WirePlayer, WireZombie, quant_pos3, quant_yaw8, quant_yaw16};
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

/// M15b: horde LOD must not thrash. 150 simulated zombies, 300 frames after
/// steady spawn — transitions/frame < 5% of horde, entity root count stable
/// (impostors are pooled children; zero spawn/despawn in steady state).
#[test]
fn horde_lod_transitions_bounded_and_entity_count_stable() {
    let mut app = headless_app();
    *app.world_mut().resource_mut::<Session>() = Session::InLobby;
    app.update();

    let slot = 0u8;
    let map = generate_map(EnvKind::Urban, "lod-bench");
    let spawn = map.spawns[0];
    let n_zeds = 150u16;
    // Ring of zombies at mixed distances so all three LOD bands are occupied.
    let zombies: Vec<WireZombie> = (0..n_zeds)
        .map(|i| {
            let t = i as f32 / n_zeds as f32 * std::f32::consts::TAU;
            // 15 m … 120 m so Full / Bob / Static are all represented.
            let r = 15.0 + (i as f32 % 50.0) * 2.1;
            let x = spawn.x + t.cos() * r;
            let z = spawn.z + t.sin() * r;
            WireZombie {
                id: i + 1,
                kind: (i % 3) as u8,
                state: 0,
                pos: quant_pos3(x, 0.0, z),
                yaw: quant_yaw8(t),
                health: 100,
            }
        })
        .collect();

    let make_snap = |tick: u32, zeds: &[WireZombie]| Snapshot {
        tick,
        game_time_ms: tick.saturating_mul(33),
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
        zombies: zeds.to_vec(),
        loot: vec![],
        grenades: vec![],
        shots: vec![],
        booms: vec![],
    };

    {
        let mut net = app.world_mut().resource_mut::<NetClient>();
        net.inject(NetEvent::Msg(ServerMsg::GameStart {
            map_seed: "lod-bench".into(),
            env: EnvKind::Urban,
            your_slot: slot,
            players: vec![RosterPlayer {
                slot,
                id: "c1".into(),
                name: "lodder".into(),
            }],
        }));
    }
    // Apply GameStart so CurrentMap / Session land before the horde snap.
    app.update();
    {
        let mut net = app.world_mut().resource_mut::<NetClient>();
        net.inject(NetEvent::Snap(make_snap(1, &zombies)));
    }
    app.update(); // one snap only — two snaps in one drain double-spawn (Commands-deferred)

    // Warm-up past the first staggered distance refresh for every rig
    // (LOD_DIST_PERIOD = 10) so initial Full→band settles before we measure.
    for f in 0..20 {
        {
            let mut net = app.world_mut().resource_mut::<NetClient>();
            net.inject(NetEvent::Snap(make_snap(2 + f, &zombies)));
        }
        app.update();
    }
    let roots_after_spawn = count_with::<RemoteZombie>(app.world_mut());
    assert_eq!(
        roots_after_spawn, n_zeds as usize,
        "expected {n_zeds} RemoteZombie roots after inject, got {roots_after_spawn}"
    );

    // Steady state: re-inject the same horde (tiny motion) for 300 frames.
    app.world_mut().resource_mut::<LodMetrics>().reset_counters();
    let measure_frames = 300u32;
    for f in 0..measure_frames {
        // Nudge a few cm so pose systems run; ids stable → no despawn.
        let moved: Vec<WireZombie> = zombies
            .iter()
            .map(|z| {
                let mut z = *z;
                let x = dequant_approx(z.pos[0]);
                let zz = dequant_approx(z.pos[2]);
                let phase = f as f32 * 0.02 + z.id as f32 * 0.1;
                z.pos = quant_pos3(x + phase.cos() * 0.05, 0.0, zz + phase.sin() * 0.05);
                z
            })
            .collect();
        {
            let mut net = app.world_mut().resource_mut::<NetClient>();
            net.inject(NetEvent::Snap(make_snap(100 + f, &moved)));
        }
        app.update();
    }

    let roots_end = count_with::<RemoteZombie>(app.world_mut());
    assert_eq!(
        roots_end, roots_after_spawn,
        "RemoteZombie root count must be stable (no spawn/despawn thrash): start={roots_after_spawn} end={roots_end}"
    );

    let m = app.world().resource::<LodMetrics>();
    let horde = n_zeds as u32;
    // Peak and average both under 5% of horde (M15 thrash was ~every zombie every frame).
    let budget = ((horde as f32) * 0.05).ceil() as u32;
    let avg = m.total_transitions as f32 / measure_frames.max(1) as f32;
    assert!(
        m.peak_transitions <= budget.max(2),
        "LOD transitions/frame peak {} exceeds 5% budget {} (horde={horde}, total_trans={}, avg={avg:.2})",
        m.peak_transitions,
        budget,
        m.total_transitions
    );
    assert!(
        avg <= horde as f32 * 0.05,
        "LOD transitions/frame avg {avg:.2} exceeds 5% of horde ({})",
        horde as f32 * 0.05
    );
    assert!(m.frame >= measure_frames, "expected ~{measure_frames} metric frames, got {}", m.frame);
    assert_eq!(m.zombie_roots, horde);
}

fn dequant_approx(q: i16) -> f32 {
    zz_core::snapshot::dequant_pos(q)
}
