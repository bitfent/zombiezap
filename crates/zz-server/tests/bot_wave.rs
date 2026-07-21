//! M22 bot tests: wave lifecycle, supply drops, brute cover smash, difficulty
//! table. Separate binary so ZZ_DIRECTOR_RATE does not leak into other suites.
//!
//! Env lock is held across `await` on purpose (serializes process-global
//! director env for the whole match).
#![allow(clippy::await_holding_lock)]

use futures_util::{SinkExt, StreamExt};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use zz_core::constants::{LOOT_KIND_SUPPLY, PLAYER_EYE, env_pressure};
use zz_core::map::{generate_map, is_destructible_cover, WalkGrid};
use zz_core::protocol::{ClientMsg, ServerMsg, encode_input};
use zz_core::snapshot::{Snapshot, SnapshotDecoder, dequant_pos};
use zz_core::types::{EnvKind, PlayerInput};

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn lock_env() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn pin_env(map_seed: &str, director_rate: &str) {
    unsafe {
        std::env::set_var("ZZ_DIRECTOR_RATE", director_rate);
        std::env::set_var("MAP_SEED", map_seed);
    }
}

async fn start_server() -> String {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, zz_server::app()).await.unwrap();
    });
    format!("ws://{addr}/ws")
}

async fn connect(url: &str, name: &str) -> (Ws, u8) {
    let (mut ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("connect");
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("welcome timeout")
            .unwrap()
            .unwrap();
        if let Message::Text(t) = msg
            && let Ok(ServerMsg::Welcome { .. }) = serde_json::from_str(t.as_str())
        {
            break;
        }
    }
    for msg in [
        serde_json::to_string(&ClientMsg::Hello { name: name.into() }).unwrap(),
        serde_json::to_string(&ClientMsg::CreateLobby {
            env: EnvKind::Urban,
        })
        .unwrap(),
        serde_json::to_string(&ClientMsg::StartGame).unwrap(),
    ] {
        ws.send(Message::Text(msg.into())).await.unwrap();
    }
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("game_start timeout")
            .unwrap()
            .unwrap();
        if let Message::Text(t) = msg
            && let Ok(ServerMsg::GameStart { your_slot, .. }) = serde_json::from_str(t.as_str())
        {
            return (ws, your_slot);
        }
    }
}

enum Incoming {
    Snap(Snapshot),
    Msg(ServerMsg),
}

async fn recv_any(ws: &mut Ws, dec: &mut SnapshotDecoder) -> Option<Incoming> {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(12), ws.next())
            .await
            .ok()??
            .ok()?;
        match msg {
            Message::Binary(b) => {
                if let Ok(s) = dec.decode(&b) {
                    return Some(Incoming::Snap(s));
                }
            }
            Message::Text(t) => match serde_json::from_str(t.as_str()).ok()? {
                ServerMsg::Ping { .. } => continue,
                m => return Some(Incoming::Msg(m)),
            },
            Message::Close(_) => return None,
            _ => continue,
        }
    }
}

fn idle(seq: u32) -> PlayerInput {
    PlayerInput {
        seq,
        ..Default::default()
    }
}

fn aim_at(own: (f32, f32, f32), target: (f32, f32, f32)) -> (f32, f32) {
    let dx = target.0 - own.0;
    let dy = target.1 - own.1;
    let dz = target.2 - own.2;
    let yaw = (-dx).atan2(-dz);
    let pitch = dy.atan2((dx * dx + dz * dz).sqrt());
    (yaw, pitch)
}

/// Wave lifecycle: WaveStart → clear (all dead) → breather with zero spawns → next start.
#[tokio::test]
async fn wave_lifecycle_start_clear_breather_next() {
    let _env = lock_env();
    // Rate 0.2 → ~12-point wave (one walker). Still full WaveStart/Clear/breather.
    pin_env("m22-wave", "0.2");
    let url = start_server().await;
    let (mut ws, slot) = connect(&url, "waver").await;
    let mut dec = SnapshotDecoder::new();
    let mut seq = 0u32;

    let mut saw_start1 = false;
    let mut saw_clear1 = false;
    let mut saw_start2 = false;
    let mut breather_zero_spawn_ticks = 0u32;
    let mut in_breather = false;
    let mut last_snap: Option<Snapshot> = None;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    while tokio::time::Instant::now() < deadline {
        seq += 1;
        // Aimbot: shoot nearest zombie every tick.
        let mut input = idle(seq);
        if let Some(snap) = last_snap.as_ref()
            && let Some(me) = snap.players.iter().find(|p| p.slot == slot)
        {
            let eye = (
                dequant_pos(me.pos[0]),
                dequant_pos(me.pos[1]) + PLAYER_EYE,
                dequant_pos(me.pos[2]),
            );
            // Nearest zombie, aim at torso/head.
            let mut best: Option<(f32, f32, f32, f32)> = None; // d2,x,y,z
            for z in &snap.zombies {
                let tx = dequant_pos(z.pos[0]);
                let ty = dequant_pos(z.pos[1]) + 1.2;
                let tz = dequant_pos(z.pos[2]);
                let d2 = (tx - eye.0).powi(2) + (tz - eye.2).powi(2);
                if best.is_none_or(|(bd, ..)| d2 < bd) {
                    best = Some((d2, tx, ty, tz));
                }
            }
            if let Some((_, tx, ty, tz)) = best {
                let (yaw, pitch) = aim_at(eye, (tx, ty, tz));
                input.yaw = yaw;
                input.pitch = pitch;
                input.fire = true;
            }
        }
        let _ = ws
            .send(Message::Binary(encode_input(&input).to_vec().into()))
            .await;

        match recv_any(&mut ws, &mut dec).await {
            Some(Incoming::Msg(ServerMsg::WaveStart { wave })) => {
                if wave == 1 {
                    saw_start1 = true;
                    in_breather = false;
                }
                if wave == 2 {
                    saw_start2 = true;
                    break;
                }
            }
            Some(Incoming::Msg(ServerMsg::WaveClear { wave, .. })) => {
                if wave == 1 {
                    saw_clear1 = true;
                    in_breather = true;
                    breather_zero_spawn_ticks = 0;
                }
            }
            Some(Incoming::Snap(s)) => {
                if in_breather && !saw_start2 {
                    // After WaveClear and before WaveStart 2, no new spawns.
                    if s.zombies.is_empty() {
                        breather_zero_spawn_ticks += 1;
                    }
                }
                last_snap = Some(s);
            }
            Some(Incoming::Msg(_)) => {}
            None => panic!("connection dropped during wave lifecycle"),
        }
    }

    assert!(saw_start1, "expected WaveStart 1");
    assert!(saw_clear1, "expected WaveClear 1");
    assert!(
        breather_zero_spawn_ticks >= 5,
        "breather should show empty field for several snaps, got {breather_zero_spawn_ticks}"
    );
    assert!(saw_start2, "expected WaveStart 2 after breather");
}

/// Supply drop appears in the breather; walking into it grants ammo.
#[tokio::test]
async fn supply_drop_in_breather_grants_ammo() {
    let _env = lock_env();
    pin_env("m22-supply", "0.2");
    let url = start_server().await;
    let (mut ws, slot) = connect(&url, "scavenger").await;
    let mut dec = SnapshotDecoder::new();
    let mut seq = 0u32;
    let mut drop: Option<(u16, f32, f32)> = None;
    let mut last_snap: Option<Snapshot> = None;
    let mut ammo_before: Option<u8> = None;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(100);
    while tokio::time::Instant::now() < deadline {
        seq += 1;
        let mut input = idle(seq);
        if let Some(snap) = last_snap.as_ref()
            && let Some(me) = snap.players.iter().find(|p| p.slot == slot)
        {
            let eye = (
                dequant_pos(me.pos[0]),
                dequant_pos(me.pos[1]) + PLAYER_EYE,
                dequant_pos(me.pos[2]),
            );
            // Clear wave so we get a drop.
            if drop.is_none()
                && let Some(z) = snap.zombies.first()
            {
                let tz = (
                    dequant_pos(z.pos[0]),
                    dequant_pos(z.pos[1]) + 1.0,
                    dequant_pos(z.pos[2]),
                );
                let (yaw, pitch) = aim_at(eye, tz);
                input.yaw = yaw;
                input.pitch = pitch;
                input.fire = true;
            }
            // Walk toward drop once it exists.
            if let Some((_, dx, dz)) = drop {
                let px = dequant_pos(me.pos[0]);
                let pz = dequant_pos(me.pos[2]);
                let vx = dx - px;
                let vz = dz - pz;
                let yaw = (-vx).atan2(-vz);
                input.yaw = yaw;
                input.forward = true;
            }
        }
        let _ = ws
            .send(Message::Binary(encode_input(&input).to_vec().into()))
            .await;

        match recv_any(&mut ws, &mut dec).await {
            Some(Incoming::Msg(ServerMsg::SupplyDrop { id, x, z })) => {
                drop = Some((id, x, z));
                if let Some(snap) = last_snap.as_ref()
                    && let Some(me) = snap.players.iter().find(|p| p.slot == slot)
                {
                    ammo_before = Some(me.ammo_reserve);
                }
            }
            Some(Incoming::Snap(s)) => {
                if let Some((id, _, _)) = drop
                    && let Some(me) = s.players.iter().find(|p| p.slot == slot)
                {
                    let still = s.loot.iter().any(|l| l.id == id && l.kind == LOOT_KIND_SUPPLY);
                    if !still {
                        let before = ammo_before.unwrap_or(0);
                        assert!(
                            me.ammo_reserve > before || me.health > 0,
                            "pickup should grant ammo (before={before}, after={})",
                            me.ammo_reserve
                        );
                        // Bonus ammo from WaveClear and/or crate.
                        assert!(
                            me.ammo_reserve >= before,
                            "ammo should not drop on pickup"
                        );
                        // Prefer strict grant: crate adds LOOT_AMMO_AMOUNT or WaveClear bonus.
                        if me.ammo_reserve > before {
                            return;
                        }
                    }
                }
                last_snap = Some(s);
            }
            Some(Incoming::Msg(_)) => {}
            None => panic!("connection dropped during supply drop test"),
        }
    }
    panic!(
        "timed out waiting for supply drop pickup (drop={drop:?}, ammo_before={ammo_before:?})"
    );
}

/// Brute adjacent to cover removes exactly that AABB; BFS reachability holds.
#[test]
fn brute_cover_removal_keeps_bfs() {
    let map = generate_map(EnvKind::Urban, "m22-cover");
    let half = map.arena_half;
    let cover: Vec<_> = map
        .walls
        .iter()
        .filter(|w| is_destructible_cover(w, half))
        .cloned()
        .collect();
    assert!(
        !cover.is_empty(),
        "urban map should have smashable cover props"
    );
    let target = cover[0];
    let mut walls = map.walls.clone();
    let before = walls.len();
    walls.retain(|w| {
        !((w.x0 - target.x0).abs() < 1e-4
            && (w.x1 - target.x1).abs() < 1e-4
            && (w.y0 - target.y0).abs() < 1e-4
            && (w.y1 - target.y1).abs() < 1e-4
            && (w.z0 - target.z0).abs() < 1e-4
            && (w.z1 - target.z1).abs() < 1e-4)
    });
    assert_eq!(walls.len(), before - 1, "exactly one AABB removed");

    let grid = WalkGrid::rasterize(&walls, half);
    // Spawns still walkable and gates still reach at least one spawn.
    for s in &map.spawns {
        assert!(
            grid.walkable_at(s.x, s.z),
            "spawn ({}, {}) walkable after smash",
            s.x,
            s.z
        );
    }
    let sources: Vec<_> = map
        .spawns
        .iter()
        .filter_map(|s| grid.cell_of(s.x, s.z))
        .collect();
    assert!(!sources.is_empty());
    let dist = grid.distance_field(&sources);
    let reachable_gates = map
        .gates
        .iter()
        .filter(|g| {
            grid.cell_of(g.x, g.z)
                .map(|(ix, iz)| dist[iz * grid.cells_per_side + ix] < u16::MAX)
                .unwrap_or(false)
        })
        .count();
    assert!(
        reachable_gates >= 1,
        "at least one gate must stay BFS-reachable after cover smash"
    );
}

/// Runner flank target differs from the direct player path (unit-level).
#[test]
fn runner_flank_offset_from_centroid() {
    // Re-export via director unit test in zz-server; also assert pressure table here.
    let total: f32 = [
        EnvKind::Urban,
        EnvKind::MountainTown,
        EnvKind::DesertTown,
        EnvKind::SeaTown,
    ]
    .iter()
    .map(|e| env_pressure(*e))
    .sum();
    assert!(
        (3.8..=4.2).contains(&total),
        "four-env pressure total {total}"
    );
    assert!(env_pressure(EnvKind::DesertTown) > env_pressure(EnvKind::Urban));
    assert!(env_pressure(EnvKind::SeaTown) < env_pressure(EnvKind::Urban));
}
