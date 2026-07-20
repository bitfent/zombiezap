//! M4 bot tests: the horde grows, gunfire thins it, an idle team gets
//! overrun to a MatchEnd with coherent stats, and pause freezes the sim.
//! Separate test binary from bot_match so ZZ_DIRECTOR_RATE (process-global)
//! never leaks into the M2 netcode tests.

use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use zz_core::constants::PLAYER_EYE;
use zz_core::protocol::{ClientMsg, ServerMsg, encode_input};
use zz_core::snapshot::{Snapshot, SnapshotDecoder, dequant_pos};
use zz_core::types::PlayerInput;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn fast_director(rate: &str) {
    // safe here: every test in THIS binary wants an aggressive director, and
    // the room reads the vars once at creation
    unsafe {
        std::env::set_var("ZZ_DIRECTOR_RATE", rate);
        std::env::set_var("MAP_SEED", "m4-dev");
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
            env: zz_core::types::EnvKind::Urban,
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
        let msg = tokio::time::timeout(Duration::from_secs(10), ws.next())
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

fn aim_at(own: (f32, f32, f32), target: (f32, f32, f32)) -> (f32, f32) {
    let dx = target.0 - own.0;
    let dy = target.1 - own.1;
    let dz = target.2 - own.2;
    let yaw = (-dx).atan2(-dz);
    let pitch = dy.atan2((dx * dx + dz * dz).sqrt());
    (yaw, pitch)
}

#[tokio::test]
async fn horde_grows_and_gunfire_thins_it() {
    fast_director("40");
    let url = start_server().await;
    let (mut ws, slot) = connect(&url, "gunner").await;
    let mut dec = SnapshotDecoder::new();

    // phase 1: watch the horde grow
    let mut peak = 0usize;
    let mut last: Option<Snapshot> = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
    while tokio::time::Instant::now() < deadline {
        // keep presence alive with idle inputs
        static mut SEQ: u32 = 0;
        let seq = unsafe {
            SEQ += 1;
            SEQ
        };
        let idle = PlayerInput {
            seq,
            ..Default::default()
        };
        let _ = ws
            .send(Message::Binary(encode_input(&idle).to_vec().into()))
            .await;
        match recv_any(&mut ws, &mut dec).await {
            Some(Incoming::Snap(s)) => {
                peak = peak.max(s.zombies.len());
                last = Some(s);
            }
            Some(Incoming::Msg(_)) => {}
            None => panic!("connection dropped during growth phase"),
        }
    }
    assert!(peak >= 5, "horde should have grown, peak was {peak}");

    // phase 2: aimbot — shoot the nearest zombie until kills register
    let mut seq = 100_000u32;
    let mut kills = 0u16;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
    let mut snap = last.expect("have a snapshot");
    while tokio::time::Instant::now() < deadline && kills == 0 {
        let me = snap.players.iter().find(|p| p.slot == slot).expect("self");
        kills = me.kills;
        let own = (
            dequant_pos(me.pos[0]),
            dequant_pos(me.pos[1]) + PLAYER_EYE,
            dequant_pos(me.pos[2]),
        );
        if let Some(z) = snap.zombies.iter().min_by(|a, b| {
            let da =
                (dequant_pos(a.pos[0]) - own.0).powi(2) + (dequant_pos(a.pos[2]) - own.2).powi(2);
            let db =
                (dequant_pos(b.pos[0]) - own.0).powi(2) + (dequant_pos(b.pos[2]) - own.2).powi(2);
            da.partial_cmp(&db).unwrap()
        }) {
            let target = (
                dequant_pos(z.pos[0]),
                dequant_pos(z.pos[1]) + 0.95, // body sphere center
                dequant_pos(z.pos[2]),
            );
            let (yaw, pitch) = aim_at(own, target);
            seq += 1;
            let input = PlayerInput {
                seq,
                fire: true,
                yaw,
                pitch,
                ..Default::default()
            };
            let _ = ws
                .send(Message::Binary(encode_input(&input).to_vec().into()))
                .await;
        }
        match recv_any(&mut ws, &mut dec).await {
            Some(Incoming::Snap(s)) => snap = s,
            Some(Incoming::Msg(_)) => {}
            None => panic!("connection dropped during combat phase"),
        }
    }
    assert!(kills > 0, "aimbot should have registered at least one kill");
}

#[tokio::test]
async fn idle_team_gets_overrun_to_match_end() {
    fast_director("300");
    let url = start_server().await;
    let (mut ws, _slot) = connect(&url, "victim").await;
    let mut dec = SnapshotDecoder::new();

    let mut seq = 0u32;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    let stats = loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no MatchEnd within 90 s"
        );
        // stand still but stay present
        seq += 1;
        let idle = PlayerInput {
            seq,
            ..Default::default()
        };
        let _ = ws
            .send(Message::Binary(encode_input(&idle).to_vec().into()))
            .await;
        match recv_any(&mut ws, &mut dec).await {
            Some(Incoming::Msg(ServerMsg::MatchEnd { stats })) => break stats,
            Some(_) => {}
            None => panic!("connection dropped before MatchEnd"),
        }
    };
    assert_eq!(stats.players.len(), 1);
    assert_eq!(stats.players[0].kills, 0, "victim never fought back");
    assert!(stats.duration_ms > 0);
    assert!(stats.peak_zombies > 0);
    assert!(stats.players[0].time_alive_ms <= stats.duration_ms);
}

#[tokio::test]
async fn pause_freezes_game_time_and_zombies() {
    fast_director("40");
    let url = start_server().await;
    let (mut ws, _slot) = connect(&url, "pauser").await;
    let mut dec = SnapshotDecoder::new();

    // wait until zombies exist
    let mut seq = 0u32;
    let mut snap;
    loop {
        seq += 1;
        let idle = PlayerInput {
            seq,
            ..Default::default()
        };
        let _ = ws
            .send(Message::Binary(encode_input(&idle).to_vec().into()))
            .await;
        match recv_any(&mut ws, &mut dec).await {
            Some(Incoming::Snap(s)) if !s.zombies.is_empty() => {
                snap = s;
                break;
            }
            Some(_) => {}
            None => panic!("dropped"),
        }
    }

    let pause = serde_json::to_string(&ClientMsg::Pause).unwrap();
    ws.send(Message::Text(pause.into())).await.unwrap();

    // drain until the paused flag shows up, then sample frozen state
    let (frozen_time, frozen_zombies) = loop {
        match recv_any(&mut ws, &mut dec).await {
            Some(Incoming::Snap(s)) => {
                if s.paused {
                    break (s.game_time_ms, s.zombies.clone());
                }
                snap = s;
            }
            Some(Incoming::Msg(ServerMsg::Paused { .. })) => {}
            Some(_) => {}
            None => panic!("dropped"),
        }
    };
    let _ = snap;

    // ~1 s of paused snapshots: time and zombies must not move
    for _ in 0..20 {
        match recv_any(&mut ws, &mut dec).await {
            Some(Incoming::Snap(s)) => {
                assert!(s.paused, "must stay paused");
                assert_eq!(
                    s.game_time_ms, frozen_time,
                    "game clock advanced while paused"
                );
                assert_eq!(s.zombies, frozen_zombies, "zombies moved while paused");
            }
            Some(_) => {}
            None => panic!("dropped"),
        }
    }

    // resume: game time advances again
    let resume = serde_json::to_string(&ClientMsg::Resume).unwrap();
    ws.send(Message::Text(resume.into())).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(tokio::time::Instant::now() < deadline, "sim never resumed");
        match recv_any(&mut ws, &mut dec).await {
            Some(Incoming::Snap(s)) => {
                if !s.paused && s.game_time_ms > frozen_time {
                    break;
                }
            }
            Some(_) => {}
            None => panic!("dropped"),
        }
    }
}
