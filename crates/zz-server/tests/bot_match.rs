//! Headless bot tests against the REAL server over the real wire protocol —
//! the ShotAnte bot-match.mjs philosophy, in Rust. The server runs in-process
//! on an ephemeral port; bots are plain tokio-tungstenite WebSocket clients
//! speaking zz-core's protocol + codec.

use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use zz_core::protocol::{ClientMsg, ServerMsg, encode_input};
use zz_core::snapshot::{Snapshot, SnapshotDecoder, dequant_pos};
use zz_core::types::PlayerInput;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

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

fn pin_map() {
    // room seeds derive from random lobby codes; pin the map for determinism
    unsafe { std::env::set_var("MAP_SEED", "m4-dev") };
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

/// Create a lobby; returns the socket and the shareable code.
async fn create_lobby(url: &str, name: &str) -> (Ws, String) {
    let mut ws = open_conn(url, name).await;
    let create = serde_json::to_string(&ClientMsg::CreateLobby {
        env: zz_core::types::EnvKind::Urban,
    })
    .unwrap();
    ws.send(Message::Text(create.into())).await.unwrap();
    let state = recv_json(&mut ws).await.expect("lobby_state");
    let ServerMsg::LobbyState { code, .. } = state else {
        panic!("expected lobby_state, got {state:?}");
    };
    (ws, code)
}

async fn join_lobby(url: &str, name: &str, code: &str) -> Ws {
    let mut ws = open_conn(url, name).await;
    let join = serde_json::to_string(&ClientMsg::JoinLobby { code: code.into() }).unwrap();
    ws.send(Message::Text(join.into())).await.unwrap();
    let state = recv_json(&mut ws).await.expect("lobby_state after join");
    assert!(
        matches!(state, ServerMsg::LobbyState { .. }),
        "got {state:?}"
    );
    ws
}

async fn start_game(ws: &mut Ws) {
    let start = serde_json::to_string(&ClientMsg::StartGame).unwrap();
    ws.send(Message::Text(start.into())).await.unwrap();
}

async fn wait_game_start(ws: &mut Ws) -> u8 {
    loop {
        match recv_json(ws).await.expect("game_start") {
            ServerMsg::GameStart { your_slot, .. } => return your_slot,
            ServerMsg::LobbyState { .. } => continue, // roster churn pre-start
            other => panic!("expected game_start, got {other:?}"),
        }
    }
}

/// Next meaningful JSON control message, skipping binary frames and the
/// heartbeat pings a real client answers in passing.
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

/// Pump the socket until the next snapshot decodes.
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

fn forward_input(seq: u32, yaw: f32) -> PlayerInput {
    PlayerInput {
        seq,
        forward: true,
        yaw,
        ..Default::default()
    }
}

#[tokio::test]
async fn two_bots_move_independently() {
    pin_map();
    let url = start_server().await;
    let (mut a, code) = create_lobby(&url, "walker").await;
    let mut b = join_lobby(&url, "camper", &code).await;
    // host sees the join before starting
    let _ = recv_json(&mut a).await.expect("roster update");
    start_game(&mut a).await;
    let slot_a = wait_game_start(&mut a).await;
    let slot_b = wait_game_start(&mut b).await;
    let _b = b;
    assert_ne!(slot_a, slot_b);

    // pump until the room has integrated both joins
    let mut dec_a = SnapshotDecoder::new();
    let first = loop {
        let s = recv_snapshot(&mut a, &mut dec_a).await.expect("snapshot");
        if s.players.len() == 2 {
            break s;
        }
    };
    let start_a = first.players.iter().find(|p| p.slot == slot_a).unwrap().pos;
    let start_b = first.players.iter().find(|p| p.slot == slot_b).unwrap().pos;

    // bot A walks forward with yaw π (=> +z, away from its corner spawn and
    // into open ground) at ~30 inputs/sec; bot B stays still
    for seq in 1..=45u32 {
        let frame = encode_input(&forward_input(seq, std::f32::consts::PI));
        a.send(Message::Binary(frame.to_vec().into()))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(33)).await;
    }

    // decode the buffered delta stream in order until the server has acked
    // (and therefore applied) our final input
    let snap = loop {
        let s = recv_snapshot(&mut a, &mut dec_a)
            .await
            .expect("later snapshot");
        if s.players
            .iter()
            .find(|p| p.slot == slot_a)
            .unwrap()
            .last_acked_seq
            >= 45
        {
            break s;
        }
    };
    let end_a = snap.players.iter().find(|p| p.slot == slot_a).unwrap().pos;
    let end_b = snap.players.iter().find(|p| p.slot == slot_b).unwrap().pos;

    let moved = dequant_pos(end_a[2]) - dequant_pos(start_a[2]);
    assert!(
        moved > 4.0,
        "bot A should have covered ground toward +z, moved {moved} m"
    );
    assert_eq!(start_b, end_b, "idle bot B must not move");

    // the applied input sequence must be acknowledged in the snapshot
    let ack = snap
        .players
        .iter()
        .find(|p| p.slot == slot_a)
        .unwrap()
        .last_acked_seq;
    assert!(ack > 0, "server must ack applied inputs");
}

#[tokio::test]
async fn silent_socket_is_terminated_for_presence() {
    pin_map();
    let url = start_server().await;
    let (mut ws, _code) = create_lobby(&url, "ghost").await;
    start_game(&mut ws).await;
    let _slot = wait_game_start(&mut ws).await;

    // Say nothing and ignore pings. The server must hang up within
    // PRESENCE_TIMEOUT_MS (+ one ping interval of slack).
    let deadline = Duration::from_millis(
        zz_core::constants::PRESENCE_TIMEOUT_MS + 2 * zz_core::constants::PING_INTERVAL_MS,
    );
    let start = std::time::Instant::now();
    let closed = tokio::time::timeout(deadline, async {
        loop {
            match ws.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                _ => {} // keep draining snapshots/pings without answering
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "server never closed a silent socket");
    assert!(
        start.elapsed() >= Duration::from_millis(zz_core::constants::PRESENCE_TIMEOUT_MS - 500),
        "closed suspiciously early: {:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn sixth_player_is_rejected() {
    pin_map();
    let url = start_server().await;
    let (_host, code) = create_lobby(&url, "host").await;
    let mut conns = Vec::new();
    for i in 0..4 {
        conns.push(join_lobby(&url, &format!("p{i}"), &code).await);
    }
    // sixth player: the join must be refused with a lobby-full error
    let mut ws = open_conn(&url, "late").await;
    let join = serde_json::to_string(&ClientMsg::JoinLobby { code: code.clone() }).unwrap();
    ws.send(Message::Text(join.into())).await.unwrap();
    match recv_json(&mut ws).await {
        Some(ServerMsg::Error { message }) => assert!(message.contains("full")),
        other => panic!("expected lobby-full error, got {other:?}"),
    }
}
