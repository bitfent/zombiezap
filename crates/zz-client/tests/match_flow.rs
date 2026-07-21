//! Headless match-flow integration tests (no winit / no GPU present).
//!
//! Reproduces the ECS-level conditions behind the browser regressions:
//! S1 HUD visibility on GameStart, S2 first-map build (MapRoot + no
//! Placeholders), S3 rematch wipe of remotes / LastStats / session state.
//! M20: prediction must not step without walls; GameStart+Snap queues inputs.

use std::time::Duration;

use bevy::prelude::*;
use bevy::time::TimeUpdateStrategy;
use zz_client::game::{GamePlugin, LodMetrics, Predicted, RemotePlayer, RemoteZombie, Session};
use zz_client::hud::{HudChrome, HudPlugin};
use zz_client::map_render::{CurrentMap, MapRenderPlugin, MapRoot, Placeholder};
use zz_client::net::{NetClient, NetEvent};
use zz_client::seams::{COMBO_RESET_SEC, LastStats, LatestSnapshot, Roster, WAVE_BANNER_SEC, WaveUi};
use zz_client::touch::TouchIntent;
use zz_client::voice::{VoiceRx, VoiceState};
use zz_core::constants::{
    MAG_SIZE, MAX_HEALTH, PLAYER_SPEED, START_GRENADES, START_RESERVE_AMMO, TICK_DT,
};
use zz_core::map::generate_map;
use zz_core::movement::step_body;
use zz_core::protocol::{MatchStats, PlayerStats, RosterPlayer, ServerMsg};
use zz_core::snapshot::{
    Snapshot, WirePlayer, WireShot, WireZombie, quant_pos3, quant_yaw8, quant_yaw16,
};
use zz_core::types::{EnvKind, PlayerInput};

/// Fixed sim step for headless Time — exactly one 30 Hz input cadence per
/// update after `send_accum` is primed. Avoids wall-clock / 16 ms races.
const FIXED_DT: Duration = Duration::from_nanos(33_333_333); // ≈ TICK_DT

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
        // Drive virtual Time with fixed deltas only — never wall-clock.
        .insert_resource(TimeUpdateStrategy::ManualDuration(FIXED_DT))
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

    // Drive a few frames so Startup (HUD spawn) + RigAssets bank land.
    // net_poll inserts RigAssets on first Update and returns early; second
    // frame is the first that can drain injects.
    for _ in 0..3 {
        app.update();
    }
    // Leave Boot so connect_on_start stops hammering a missing server.
    *app.world_mut().resource_mut::<Session>() = Session::Menu;
    app.update();
    app
}

/// Advance one fixed tick (ManualDuration already set; update applies it).
fn tick(app: &mut App) {
    app.update();
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
        waves_cleared: 0,
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

/// M14b / M20: after GameStart + first snapshot, prediction is synced and at
/// least one encoded input frame is queued. Deterministic: fixed Time steps
/// of ≈TICK_DT; GameStart and Snap applied on separate ticks so the harness
/// mirrors a real decoder frame boundary (not a wall-clock race on send_accum).
#[test]
fn game_start_snapshot_syncs_and_queues_input() {
    let mut app = headless_app();
    // Leave Menu so we're ready for a synthetic GameStart (not Boot reconnect).
    *app.world_mut().resource_mut::<Session>() = Session::InLobby;
    tick(&mut app);

    let slot = 0u8;
    let seed = "match-flow-input";
    let spawn = generate_map(EnvKind::Urban, seed).spawns[0];
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
            reload_ticks_left: 0,
        }],
        zombies: vec![],
        loot: vec![],
        grenades: vec![],
        shots: vec![],
        booms: vec![],
    };

    // Frame 1: GameStart only — Session→Playing, CurrentMap scheduled, Predicted reset.
    {
        let mut net = app.world_mut().resource_mut::<NetClient>();
        net.take_outbound_bin(); // clear any prior
        net.inject(NetEvent::Msg(ServerMsg::GameStart {
            map_seed: seed.into(),
            env: EnvKind::Urban,
            your_slot: slot,
            players: vec![RosterPlayer {
                slot,
                id: "c1".into(),
                name: "tester".into(),
            }],
        }));
    }
    tick(&mut app);
    assert!(
        matches!(
            *app.world().resource::<Session>(),
            Session::Playing { my_slot: 0 }
        ),
        "session Playing after GameStart tick"
    );
    assert!(
        !app.world().resource::<Predicted>().is_synced(),
        "must not be synced before the first snapshot"
    );

    // Frame 2: first Snap seeds body + send_accum=TICK_DT; same-frame
    // fps_controller (after net_poll in the chain) emits the idle input.
    {
        let mut net = app.world_mut().resource_mut::<NetClient>();
        net.inject(NetEvent::Snap(snap));
    }
    tick(&mut app);

    assert!(
        app.world().resource::<Predicted>().is_synced(),
        "predicted.synced must become true after GameStart + first snapshot"
    );
    let out = app.world_mut().resource_mut::<NetClient>().take_outbound_bin();
    assert!(
        !out.is_empty(),
        "at least one encoded input frame must be queued on the sync tick \
         (send_accum primed to TICK_DT; fixed Time step {FIXED_DT:?})"
    );
    // BIN_INPUT tag = 0
    assert_eq!(
        out[0].first().copied(),
        Some(0),
        "expected BIN_INPUT tag on first outbound frame"
    );
    assert!(
        app.world().resource::<Predicted>().pending_len() >= 1,
        "pending must retain the unacked idle input"
    );
}

/// M20: pre-map inputs are *sent* but must not move the predicted body;
/// post-map prediction with the same inputs matches server step_body within ε,
/// so reconciliation does not need a >0.25 m snap after walls load.
#[test]
fn prediction_waits_for_walls_then_matches_server() {
    let mut app = headless_app();
    *app.world_mut().resource_mut::<Session>() = Session::InLobby;
    tick(&mut app);

    let slot = 0u8;
    let seed = "m20-predict-walls";
    let map = generate_map(EnvKind::Urban, seed);
    let spawn = map.spawns[0];
    let y0 = 0.0f32;

    // GameStart without waiting for Commands→CurrentMap: inject Snap immediately
    // so we can exercise the synced-but-no-map send path by *removing* CurrentMap.
    {
        let mut net = app.world_mut().resource_mut::<NetClient>();
        net.inject(NetEvent::Msg(ServerMsg::GameStart {
            map_seed: seed.into(),
            env: EnvKind::Urban,
            your_slot: slot,
            players: vec![RosterPlayer {
                slot,
                id: "c1".into(),
                name: "pred".into(),
            }],
        }));
    }
    tick(&mut app);
    // Drop the map so the next inputs send without local step_body.
    app.world_mut().remove_resource::<CurrentMap>();

    let snap = Snapshot {
        tick: 1,
        game_time_ms: 33,
        difficulty: 0,
        paused: false,
        players: vec![WirePlayer {
            slot,
            pos: quant_pos3(spawn.x, y0, spawn.z),
            yaw: quant_yaw16(spawn.yaw),
            pitch: 0,
            health: MAX_HEALTH,
            ammo_mag: MAG_SIZE,
            ammo_reserve: START_RESERVE_AMMO,
            grenades: START_GRENADES,
            kills: 0,
            alive: true,
            last_acked_seq: 0,
            reload_ticks_left: 0,
        }],
        zombies: vec![],
        loot: vec![],
        grenades: vec![],
        shots: vec![],
        booms: vec![],
    };
    {
        let mut net = app.world_mut().resource_mut::<NetClient>();
        net.inject(NetEvent::Snap(snap));
    }
    // Hold W via TouchIntent so inputs are non-idle.
    {
        let mut touch = app.world_mut().resource_mut::<TouchIntent>();
        touch.forward = true;
    }
    tick(&mut app);

    assert!(
        app.world().resource::<Predicted>().is_synced(),
        "synced after first snap"
    );
    let body_pre = app.world().resource::<Predicted>().body;
    assert!(
        (body_pre.x - spawn.x).abs() < 1e-3 && (body_pre.z - spawn.z).abs() < 1e-3,
        "pre-map seed body at spawn, got ({}, {}) vs spawn ({}, {})",
        body_pre.x,
        body_pre.z,
        spawn.x,
        spawn.z
    );

    // Several fixed ticks with forward held and NO CurrentMap: body must not move.
    for _ in 0..5 {
        tick(&mut app);
    }
    let out_pre = app.world_mut().resource_mut::<NetClient>().take_outbound_bin();
    assert!(
        out_pre.len() >= 5,
        "must keep sending inputs pre-map (M14b), got {} frames",
        out_pre.len()
    );
    let body_still = app.world().resource::<Predicted>().body;
    let pre_map_move = ((body_still.x - body_pre.x).powi(2)
        + (body_still.z - body_pre.z).powi(2))
    .sqrt();
    assert!(
        pre_map_move < 1e-4,
        "pre-map prediction must not move body (got {pre_map_move} m); empty-wall predict is the rubber-band bug"
    );

    // Restore walls and continue predicting; compare to offline step_body.
    app.world_mut().insert_resource(CurrentMap(map.clone()));
    let mut server_body = body_still;
    let yaw = app.world().resource::<Predicted>().yaw;
    // Drain pending so we compare only post-map steps.
    // (pending still holds pre-map inputs; they were never stepped locally.)
    // Re-seed body to spawn-equivalent (still there) and step the same
    // forward inputs we will send for N ticks.
    let n_post = 10u32;
    {
        let mut touch = app.world_mut().resource_mut::<TouchIntent>();
        touch.forward = true;
    }
    for seq in 0..n_post {
        let input = PlayerInput {
            seq: 1000 + seq,
            forward: true,
            yaw,
            ..Default::default()
        };
        step_body(
            &mut server_body,
            &input,
            TICK_DT,
            PLAYER_SPEED,
            &map.walls,
            map.arena_half,
        );
        tick(&mut app);
    }
    let pred = app.world().resource::<Predicted>();
    let dx = pred.body.x - server_body.x;
    let dy = pred.body.y - server_body.y;
    let dz = pred.body.z - server_body.z;
    let err = (dx * dx + dy * dy + dz * dz).sqrt();
    // Allow pending-replay / seq offset noise under a quarter-metre; a wall-less
    // diverge would be metres after 10 forward ticks.
    assert!(
        err < 0.25,
        "post-map prediction must match server step_body within 0.25 m (err={err:.3} m); \
         pred=({:.3},{:.3},{:.3}) server=({:.3},{:.3},{:.3})",
        pred.body.x,
        pred.body.y,
        pred.body.z,
        server_body.x,
        server_body.y,
        server_body.z
    );
    // Reconciliation snap budget after map load: error_offset should stay small
    // when client and server share walls.
    assert!(
        pred.error_offset_len() < 0.5,
        "error_offset after map load should be < 0.5 m, got {}",
        pred.error_offset_len()
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
            reload_ticks_left: 0,
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

/// Seed Playing + first snap so fps_controller emits inputs.
fn enter_playing_with_snap(app: &mut App, seed: &str) -> u8 {
    let slot = 0u8;
    let spawn = generate_map(EnvKind::Urban, seed).spawns[0];
    {
        let mut net = app.world_mut().resource_mut::<NetClient>();
        net.inject(NetEvent::Msg(ServerMsg::GameStart {
            map_seed: seed.into(),
            env: EnvKind::Urban,
            your_slot: slot,
            players: vec![RosterPlayer {
                slot,
                id: "c1".into(),
                name: "m21".into(),
            }],
        }));
    }
    tick(app);
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
            reload_ticks_left: 0,
        }],
        zombies: vec![],
        loot: vec![],
        grenades: vec![],
        shots: vec![],
        booms: vec![],
    };
    {
        let mut net = app.world_mut().resource_mut::<NetClient>();
        net.inject(NetEvent::Snap(snap));
    }
    tick(app);
    // Clear any idle frames from the sync tick.
    let _ = app.world_mut().resource_mut::<NetClient>().take_outbound_bin();
    slot
}

/// M21: R / F keys set reload / melee bits on the 15-byte input frame.
#[test]
fn keyboard_r_and_f_set_reload_and_melee_bits() {
    use zz_core::protocol::{INPUT_FRAME_LEN, decode_input};

    let mut app = headless_app();
    *app.world_mut().resource_mut::<Session>() = Session::InLobby;
    tick(&mut app);
    enter_playing_with_snap(&mut app, "m21-keys");

    {
        let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        keys.press(KeyCode::KeyR);
        keys.press(KeyCode::KeyF);
    }
    for _ in 0..3 {
        tick(&mut app);
    }
    let out = app.world_mut().resource_mut::<NetClient>().take_outbound_bin();
    assert!(!out.is_empty(), "expected outbound input while R+F held");
    let frame = &out[0];
    assert_eq!(frame.len(), INPUT_FRAME_LEN, "M21 input frame is 15 bytes");
    let input = decode_input(frame).expect("decode");
    assert!(input.reload, "R must set reload bit");
    assert!(input.melee, "F must set melee bit");
}

/// M21: touch RELOAD / MELEE chips OR into the same input bits as keys.
#[test]
fn touch_reload_and_melee_chips_set_bits() {
    use zz_core::protocol::{INPUT_FRAME_LEN, decode_input};

    let mut app = headless_app();
    *app.world_mut().resource_mut::<Session>() = Session::InLobby;
    tick(&mut app);
    enter_playing_with_snap(&mut app, "m21-touch");

    {
        let mut touch = app.world_mut().resource_mut::<TouchIntent>();
        touch.enabled = true;
        touch.reload = true;
        touch.melee = true;
    }
    for _ in 0..3 {
        tick(&mut app);
    }
    let out = app.world_mut().resource_mut::<NetClient>().take_outbound_bin();
    assert!(!out.is_empty(), "expected outbound input with touch melee/reload");
    let frame = &out[0];
    assert_eq!(frame.len(), INPUT_FRAME_LEN);
    let input = decode_input(frame).expect("decode");
    assert!(input.reload, "touch.reload must set reload bit");
    assert!(input.melee, "touch.melee must set melee bit");
}

/// M22: WaveStart JSON sets WaveUi banner state.
#[test]
fn wave_start_sets_banner_state() {
    let mut app = headless_app();
    *app.world_mut().resource_mut::<Session>() = Session::InLobby;
    tick(&mut app);
    enter_playing_with_snap(&mut app, "m22-banner");

    {
        let mut net = app.world_mut().resource_mut::<NetClient>();
        net.inject(NetEvent::Msg(ServerMsg::WaveStart { wave: 3 }));
    }
    tick(&mut app);

    let wu = app.world().resource::<WaveUi>();
    assert_eq!(wu.wave, 3);
    assert_eq!(wu.banner, "WAVE 3");
    assert!(wu.banner_timer > 0.0, "banner timer armed");
}

/// M22b: banner fade is TIME-based (~WAVE_BANNER_SEC), not WaveClear-gated.
#[test]
fn wave_banner_fades_on_timer_without_clear() {
    let mut app = headless_app();
    *app.world_mut().resource_mut::<Session>() = Session::InLobby;
    tick(&mut app);
    enter_playing_with_snap(&mut app, "m22b-banner-fade");

    {
        let mut net = app.world_mut().resource_mut::<NetClient>();
        net.inject(NetEvent::Msg(ServerMsg::WaveStart { wave: 1 }));
    }
    tick(&mut app);
    {
        let wu = app.world().resource::<WaveUi>();
        assert_eq!(wu.banner, "WAVE 1");
        assert!(
            (wu.banner_timer - WAVE_BANNER_SEC).abs() < 0.05
                || wu.banner_timer > 0.0,
            "timer armed at ~{WAVE_BANNER_SEC}s, got {}",
            wu.banner_timer
        );
    }

    // Advance fixed dt past the banner duration with no WaveClear.
    let frames = ((WAVE_BANNER_SEC / TICK_DT).ceil() as u32) + 4;
    for _ in 0..frames {
        tick(&mut app);
    }

    let wu = app.world().resource::<WaveUi>();
    assert!(
        wu.banner.is_empty() && wu.banner_timer <= 0.0,
        "banner must clear on timer alone (banner={:?}, timer={})",
        wu.banner,
        wu.banner_timer
    );
}

/// M22: kill event increments combo; idle timeout resets it.
#[test]
fn kill_increments_combo_and_timeout_resets() {
    let mut app = headless_app();
    *app.world_mut().resource_mut::<Session>() = Session::InLobby;
    tick(&mut app);
    let slot = enter_playing_with_snap(&mut app, "m22-combo");

    // Own-shot kill (hit_kind 2) on the next snap.
    let mut snap = Snapshot {
        tick: 2,
        game_time_ms: 66,
        difficulty: 1,
        paused: false,
        players: vec![WirePlayer {
            slot,
            pos: quant_pos3(0.0, 0.0, 0.0),
            yaw: quant_yaw16(0.0),
            pitch: 0,
            health: MAX_HEALTH,
            ammo_mag: MAG_SIZE,
            ammo_reserve: START_RESERVE_AMMO,
            grenades: START_GRENADES,
            kills: 1,
            alive: true,
            last_acked_seq: 1,
            reload_ticks_left: 0,
        }],
        zombies: vec![],
        loot: vec![],
        grenades: vec![],
        shots: vec![WireShot {
            slot,
            end: quant_pos3(1.0, 1.0, 1.0),
            hit_kind: 2,
        }],
        booms: vec![],
    };
    {
        let mut net = app.world_mut().resource_mut::<NetClient>();
        net.inject(NetEvent::Snap(snap.clone()));
    }
    tick(&mut app);
    assert_eq!(app.world().resource::<WaveUi>().combo, 1);

    snap.tick = 3;
    snap.shots = vec![WireShot {
        slot,
        end: quant_pos3(2.0, 1.0, 1.0),
        hit_kind: 3, // headshot kill
    }];
    {
        let mut net = app.world_mut().resource_mut::<NetClient>();
        net.inject(NetEvent::Snap(snap));
    }
    tick(&mut app);
    assert_eq!(app.world().resource::<WaveUi>().combo, 2);

    // Advance virtual time past combo timeout via many fixed ticks.
    let steps = ((COMBO_RESET_SEC + 0.5) / TICK_DT).ceil() as u32 + 2;
    for _ in 0..steps {
        tick(&mut app);
    }
    assert_eq!(
        app.world().resource::<WaveUi>().combo,
        0,
        "combo should reset after idle timeout"
    );
}
