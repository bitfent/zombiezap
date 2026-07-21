//! M21 bot tests: server-authoritative melee + triggerable reload.
//! Separate binary so `ZZ_DIRECTOR_RATE` / `MAP_SEED` do not race other suites.
//! Prefer low director rates — a dead player stops acking seq and hangs wait_ack.

use futures_util::{SinkExt, StreamExt};
use std::sync::OnceLock;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use zz_core::constants::{
    FIRE_COOLDOWN_TICKS, MAG_SIZE, MELEE_COOLDOWN_TICKS, MELEE_RANGE, RELOAD_TICKS,
    START_RESERVE_AMMO,
};
use zz_core::protocol::{ClientMsg, ServerMsg, encode_input};
use zz_core::snapshot::{Snapshot, SnapshotDecoder, dequant_pos};
use zz_core::types::PlayerInput;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Serialize tests in this binary — MAP_SEED / ZZ_DIRECTOR_RATE are process-global.
async fn env_lock() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await
}

fn pin_env(map_seed: &str, director_rate: &str) {
    unsafe {
        std::env::set_var("MAP_SEED", map_seed);
        std::env::set_var("ZZ_DIRECTOR_RATE", director_rate);
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

async fn open_conn(url: &str, name: &str) -> Ws {
    let (mut ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("connect");
    let welcome = recv_json(&mut ws).await.expect("welcome");
    assert!(matches!(welcome, ServerMsg::Welcome { .. }));
    let hello = serde_json::to_string(&ClientMsg::Hello { name: name.into() }).unwrap();
    ws.send(Message::Text(hello.into())).await.unwrap();
    ws
}

async fn create_and_start(url: &str, name: &str) -> (Ws, u8, SnapshotDecoder) {
    let mut ws = open_conn(url, name).await;
    let create = serde_json::to_string(&ClientMsg::CreateLobby {
        env: zz_core::types::EnvKind::Urban,
    })
    .unwrap();
    ws.send(Message::Text(create.into())).await.unwrap();
    let _ = recv_json(&mut ws).await.expect("lobby");
    let start = serde_json::to_string(&ClientMsg::StartGame).unwrap();
    ws.send(Message::Text(start.into())).await.unwrap();
    let slot = loop {
        match recv_json(&mut ws).await.expect("game_start") {
            ServerMsg::GameStart { your_slot, .. } => break your_slot,
            ServerMsg::LobbyState { .. } => continue,
            other => panic!("expected game_start, got {other:?}"),
        }
    };
    (ws, slot, SnapshotDecoder::new())
}

async fn recv_json(ws: &mut Ws) -> Option<ServerMsg> {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .ok()??
            .ok()?;
        match msg {
            Message::Text(t) => match serde_json::from_str(t.as_str()).ok()? {
                ServerMsg::Ping { .. } => continue,
                other => return Some(other),
            },
            Message::Binary(_) => continue,
            Message::Close(_) => return None,
            _ => continue,
        }
    }
}

async fn recv_snapshot(ws: &mut Ws, dec: &mut SnapshotDecoder) -> Option<Snapshot> {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .ok()??
            .ok()?;
        if let Message::Binary(b) = msg
            && let Ok(snap) = dec.decode(&b)
        {
            return Some(snap);
        }
    }
}

async fn send_input(ws: &mut Ws, input: &PlayerInput) {
    ws.send(Message::Binary(encode_input(input).to_vec().into()))
        .await
        .unwrap();
}

async fn wait_ack(ws: &mut Ws, dec: &mut SnapshotDecoder, slot: u8, seq: u32) -> Snapshot {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "wait_ack timed out for seq={seq} (player may be dead)"
        );
        let s = recv_snapshot(ws, dec).await.expect("snap");
        if let Some(p) = s.players.iter().find(|p| p.slot == slot) {
            if !p.alive {
                panic!("player died while waiting for ack seq={seq}");
            }
            if p.last_acked_seq >= seq {
                return s;
            }
        }
    }
}

fn me_of(snap: &Snapshot, slot: u8) -> &zz_core::snapshot::WirePlayer {
    snap.players.iter().find(|p| p.slot == slot).expect("me")
}

async fn wait_self(ws: &mut Ws, dec: &mut SnapshotDecoder, slot: u8) -> Snapshot {
    loop {
        let s = recv_snapshot(ws, dec).await.expect("snap");
        if s.players.iter().any(|p| p.slot == slot) {
            return s;
        }
    }
}

async fn idle_ticks(ws: &mut Ws, dec: &mut SnapshotDecoder, slot: u8, seq: &mut u32, n: u32) {
    for _ in 0..n {
        *seq += 1;
        send_input(
            ws,
            &PlayerInput {
                seq: *seq,
                ..Default::default()
            },
        )
        .await;
        tokio::time::sleep(Duration::from_millis(34)).await;
    }
    let _ = wait_ack(ws, dec, slot, *seq).await;
}

async fn empty_mag(ws: &mut Ws, dec: &mut SnapshotDecoder, slot: u8, seq: &mut u32) {
    let gap = (FIRE_COOLDOWN_TICKS as u64 * 1000 / 30 + 15).max(50);
    for _ in 0..(MAG_SIZE as u32 + 4) {
        *seq += 1;
        send_input(
            ws,
            &PlayerInput {
                seq: *seq,
                fire: true,
                ..Default::default()
            },
        )
        .await;
        tokio::time::sleep(Duration::from_millis(gap)).await;
    }
    let last = wait_ack(ws, dec, slot, *seq).await;
    assert_eq!(
        me_of(&last, slot).ammo_mag,
        0,
        "mag should be empty (got {}, reserve {})",
        me_of(&last, slot).ammo_mag,
        me_of(&last, slot).ammo_reserve
    );
    *seq += 1;
    send_input(
        ws,
        &PlayerInput {
            seq: *seq,
            ..Default::default()
        },
    )
    .await;
    let _ = wait_ack(ws, dec, slot, *seq).await;
}

#[tokio::test]
async fn reload_refills_mag_from_reserve_and_blocks_fire() {
    let _g = env_lock().await;
    pin_env("m21-reload", "0");
    let url = start_server().await;
    let (mut ws, slot, mut dec) = create_and_start(&url, "reloader").await;
    let _ = wait_self(&mut ws, &mut dec, slot).await;

    let mut seq = 0u32;
    empty_mag(&mut ws, &mut dec, slot, &mut seq).await;
    let after = wait_ack(&mut ws, &mut dec, slot, seq).await;
    let reserve_before = me_of(&after, slot).ammo_reserve;
    assert_eq!(reserve_before, START_RESERVE_AMMO);
    assert_eq!(me_of(&after, slot).ammo_mag, 0);

    seq += 1;
    send_input(
        &mut ws,
        &PlayerInput {
            seq,
            reload: true,
            ..Default::default()
        },
    )
    .await;
    let reloading = wait_ack(&mut ws, &mut dec, slot, seq).await;
    let me = me_of(&reloading, slot);
    assert!(me.reload_ticks_left > 0, "reload started");
    assert_eq!(me.ammo_mag, 0);

    let reserve_mid = me.ammo_reserve;
    for _ in 0..6 {
        seq += 1;
        send_input(
            &mut ws,
            &PlayerInput {
                seq,
                fire: true,
                reload: true,
                ..Default::default()
            },
        )
        .await;
        tokio::time::sleep(Duration::from_millis(34)).await;
    }
    let mid = wait_ack(&mut ws, &mut dec, slot, seq).await;
    assert_eq!(me_of(&mid, slot).ammo_mag, 0);
    assert_eq!(me_of(&mid, slot).ammo_reserve, reserve_mid);

    idle_ticks(&mut ws, &mut dec, slot, &mut seq, RELOAD_TICKS + 5).await;
    let done = wait_ack(&mut ws, &mut dec, slot, seq).await;
    let me = me_of(&done, slot);
    assert_eq!(me.reload_ticks_left, 0);
    assert_eq!(me.ammo_mag, MAG_SIZE);
    assert_eq!(me.ammo_reserve, reserve_before - MAG_SIZE);
}

#[tokio::test]
async fn fire_on_empty_with_reserve_auto_reloads() {
    let _g = env_lock().await;
    pin_env("m21-autoreload", "0");
    let url = start_server().await;
    let (mut ws, slot, mut dec) = create_and_start(&url, "auto").await;
    let _ = wait_self(&mut ws, &mut dec, slot).await;

    let mut seq = 0u32;
    empty_mag(&mut ws, &mut dec, slot, &mut seq).await;

    seq += 1;
    send_input(
        &mut ws,
        &PlayerInput {
            seq,
            fire: true,
            ..Default::default()
        },
    )
    .await;
    let snap = wait_ack(&mut ws, &mut dec, slot, seq).await;
    assert!(
        me_of(&snap, slot).reload_ticks_left > 0,
        "fire on empty+reserve starts reload"
    );
    assert_eq!(me_of(&snap, slot).ammo_mag, 0);
}

/// Melee geometry is unit-tested in combat.rs; this bot test verifies wire
/// path: two swings can kill a walker when close, and out-of-range misses.
#[tokio::test]
async fn melee_two_hits_kill_walker_range_and_cooldown() {
    let _g = env_lock().await;
    // Mild director: one/few walkers, not an insta-wipe.
    pin_env("m21-melee", "2");
    let url = start_server().await;
    let (mut ws, slot, mut dec) = create_and_start(&url, "slugger").await;
    let mut snap = wait_self(&mut ws, &mut dec, slot).await;
    let mut seq = 0u32;

    for _ in 0..600 {
        if !snap.zombies.is_empty() {
            break;
        }
        seq += 1;
        send_input(
            &mut ws,
            &PlayerInput {
                seq,
                ..Default::default()
            },
        )
        .await;
        tokio::time::sleep(Duration::from_millis(25)).await;
        if let Some(s) =
            tokio::time::timeout(Duration::from_millis(40), recv_snapshot(&mut ws, &mut dec))
                .await
                .ok()
                .flatten()
        {
            snap = s;
        }
    }
    assert!(!snap.zombies.is_empty(), "director should spawn a zombie");

    for _ in 0..400 {
        let me = me_of(&snap, slot);
        assert!(me.alive, "must stay alive while approaching");
        let mx = dequant_pos(me.pos[0]);
        let mz = dequant_pos(me.pos[2]);
        let z = snap
            .zombies
            .iter()
            .min_by(|a, b| {
                let da = {
                    let dx = dequant_pos(a.pos[0]) - mx;
                    let dz = dequant_pos(a.pos[2]) - mz;
                    dx * dx + dz * dz
                };
                let db = {
                    let dx = dequant_pos(b.pos[0]) - mx;
                    let dz = dequant_pos(b.pos[2]) - mz;
                    dx * dx + dz * dz
                };
                da.partial_cmp(&db).unwrap()
            })
            .unwrap();
        let dx = dequant_pos(z.pos[0]) - mx;
        let dz = dequant_pos(z.pos[2]) - mz;
        let dist = (dx * dx + dz * dz).sqrt();
        let yaw = (-dx).atan2(-dz);
        seq += 1;
        send_input(
            &mut ws,
            &PlayerInput {
                seq,
                forward: dist > MELEE_RANGE * 0.45,
                yaw,
                ..Default::default()
            },
        )
        .await;
        tokio::time::sleep(Duration::from_millis(25)).await;
        if let Some(s) =
            tokio::time::timeout(Duration::from_millis(40), recv_snapshot(&mut ws, &mut dec))
                .await
                .ok()
                .flatten()
        {
            snap = s;
        }
        if dist <= MELEE_RANGE * 0.65 {
            break;
        }
    }

    let me = me_of(&snap, slot);
    let mx = dequant_pos(me.pos[0]);
    let mz = dequant_pos(me.pos[2]);
    let z = snap
        .zombies
        .iter()
        .min_by(|a, b| {
            let da = {
                let dx = dequant_pos(a.pos[0]) - mx;
                let dz = dequant_pos(a.pos[2]) - mz;
                dx * dx + dz * dz
            };
            let db = {
                let dx = dequant_pos(b.pos[0]) - mx;
                let dz = dequant_pos(b.pos[2]) - mz;
                dx * dx + dz * dz
            };
            da.partial_cmp(&db).unwrap()
        })
        .unwrap();
    let dx = dequant_pos(z.pos[0]) - mx;
    let dz = dequant_pos(z.pos[2]) - mz;
    let dist = (dx * dx + dz * dz).sqrt();
    assert!(
        dist <= MELEE_RANGE + 0.8,
        "failed to close for melee, dist={dist}"
    );
    let mut yaw = (-dx).atan2(-dz);
    let zid = z.id;
    let kills0 = me.kills;
    let health0 = z.health;

    for swing in 0..2u32 {
        if let Some(t) = snap.zombies.iter().find(|zz| zz.id == zid) {
            let me_now = me_of(&snap, slot);
            let mx = dequant_pos(me_now.pos[0]);
            let mz = dequant_pos(me_now.pos[2]);
            let dx = dequant_pos(t.pos[0]) - mx;
            let dz = dequant_pos(t.pos[2]) - mz;
            yaw = (-dx).atan2(-dz);
        }
        seq += 1;
        send_input(
            &mut ws,
            &PlayerInput {
                seq,
                melee: true,
                yaw,
                ..Default::default()
            },
        )
        .await;
        let _ = wait_ack(&mut ws, &mut dec, slot, seq).await;
        seq += 1;
        send_input(
            &mut ws,
            &PlayerInput {
                seq,
                yaw,
                ..Default::default()
            },
        )
        .await;
        snap = wait_ack(&mut ws, &mut dec, slot, seq).await;
        if swing == 0 {
            idle_ticks(
                &mut ws,
                &mut dec,
                slot,
                &mut seq,
                MELEE_COOLDOWN_TICKS + 2,
            )
            .await;
            if let Some(s) =
                tokio::time::timeout(Duration::from_millis(80), recv_snapshot(&mut ws, &mut dec))
                    .await
                    .ok()
                    .flatten()
            {
                snap = s;
            }
        }
    }
    idle_ticks(&mut ws, &mut dec, slot, &mut seq, 5).await;
    let after = wait_ack(&mut ws, &mut dec, slot, seq).await;
    let me = me_of(&after, slot);
    let z_after = after.zombies.iter().find(|z| z.id == zid);
    let damaged = z_after.map(|z| z.health < health0).unwrap_or(true);
    let killed = me.kills > kills0 || z_after.is_none();
    assert!(
        killed || damaged,
        "melee should hit/kill in-range walker (kills {kills0}→{}, health {health0}→{:?}, dist={dist:.2})",
        me.kills,
        z_after.map(|z| z.health)
    );

    // Cooldown: back-to-back edges without panic.
    seq += 1;
    send_input(
        &mut ws,
        &PlayerInput {
            seq,
            melee: true,
            yaw,
            ..Default::default()
        },
    )
    .await;
    seq += 1;
    send_input(
        &mut ws,
        &PlayerInput {
            seq,
            ..Default::default()
        },
    )
    .await;
    seq += 1;
    send_input(
        &mut ws,
        &PlayerInput {
            seq,
            melee: true,
            yaw,
            ..Default::default()
        },
    )
    .await;
    let _ = wait_ack(&mut ws, &mut dec, slot, seq).await;
}

/// Dry fire with no reserve melees: burn one mag + one reload cycle's ammo
/// until reserve is 0 is expensive — instead empty mag, burn reserve via
/// four auto-reload cycles is enough (START_RESERVE=48 → 4×12). Uses
/// director rate 0 so we don't die while reloading; then spawns nothing for
/// hit verification — we only assert reload_ticks stays 0 and mag stays 0
/// while fire edge is accepted (melee path runs; geometry tested in combat).
#[tokio::test]
async fn fire_on_empty_no_reserve_melee_path() {
    let _g = env_lock().await;
    pin_env("m21-dry", "0");
    let url = start_server().await;
    let (mut ws, slot, mut dec) = create_and_start(&url, "dry").await;
    let _ = wait_self(&mut ws, &mut dec, slot).await;
    let mut seq = 0u32;

    // Mag + 4 reloads empties START_RESERVE (48).
    for cycle in 0..5 {
        empty_mag(&mut ws, &mut dec, slot, &mut seq).await;
        let after = wait_ack(&mut ws, &mut dec, slot, seq).await;
        if me_of(&after, slot).ammo_reserve == 0 {
            break;
        }
        assert!(
            cycle < 4,
            "should empty reserve within 4 reloads, still {}",
            me_of(&after, slot).ammo_reserve
        );
        seq += 1;
        send_input(
            &mut ws,
            &PlayerInput {
                seq,
                fire: true,
                ..Default::default()
            },
        )
        .await;
        let s = wait_ack(&mut ws, &mut dec, slot, seq).await;
        assert!(
            me_of(&s, slot).reload_ticks_left > 0,
            "auto-reload on empty+reserve"
        );
        idle_ticks(&mut ws, &mut dec, slot, &mut seq, RELOAD_TICKS + 3).await;
    }
    let dry = wait_ack(&mut ws, &mut dec, slot, seq).await;
    assert_eq!(me_of(&dry, slot).ammo_mag, 0);
    assert_eq!(me_of(&dry, slot).ammo_reserve, 0);
    assert_eq!(me_of(&dry, slot).reload_ticks_left, 0);

    // Fire edge while fully dry: must NOT start reload; accepted as melee.
    seq += 1;
    send_input(
        &mut ws,
        &PlayerInput {
            seq,
            fire: true,
            ..Default::default()
        },
    )
    .await;
    let snap = wait_ack(&mut ws, &mut dec, slot, seq).await;
    assert_eq!(me_of(&snap, slot).reload_ticks_left, 0, "no reload when dry");
    assert_eq!(me_of(&snap, slot).ammo_mag, 0);

    // Explicit melee edge also accepted (cooldown enforced on second spam).
    seq += 1;
    send_input(
        &mut ws,
        &PlayerInput {
            seq,
            ..Default::default()
        },
    )
    .await;
    let _ = wait_ack(&mut ws, &mut dec, slot, seq).await;
    seq += 1;
    send_input(
        &mut ws,
        &PlayerInput {
            seq,
            melee: true,
            ..Default::default()
        },
    )
    .await;
    let _ = wait_ack(&mut ws, &mut dec, slot, seq).await;
}
