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

async fn connect(url: &str, name: &str) -> (Ws, u8) {
    let (mut ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("connect");
    // welcome
    let welcome = recv_json(&mut ws).await.expect("welcome");
    assert!(matches!(welcome, ServerMsg::Welcome { .. }));
    // hello -> game_start with our slot
    let hello = serde_json::to_string(&ClientMsg::Hello { name: name.into() }).unwrap();
    ws.send(Message::Text(hello.into())).await.unwrap();
    let started = recv_json(&mut ws).await.expect("game_start");
    let ServerMsg::GameStart { your_slot, .. } = started else {
        panic!("expected game_start, got {started:?}");
    };
    (ws, your_slot)
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
    let url = start_server().await;
    let (mut a, slot_a) = connect(&url, "walker").await;
    let (_b, slot_b) = connect(&url, "camper").await;
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
    let url = start_server().await;
    let (mut ws, _slot) = connect(&url, "ghost").await;

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
    let url = start_server().await;
    let mut conns = Vec::new();
    for i in 0..5 {
        conns.push(connect(&url, &format!("p{i}")).await);
    }
    // sixth: welcome arrives, then hello must yield an error, not game_start
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let _welcome = recv_json(&mut ws).await.expect("welcome");
    let hello = serde_json::to_string(&ClientMsg::Hello {
        name: "late".into(),
    })
    .unwrap();
    ws.send(Message::Text(hello.into())).await.unwrap();
    match recv_json(&mut ws).await {
        Some(ServerMsg::Error { message }) => assert!(message.contains("full")),
        other => panic!("expected room-full error, got {other:?}"),
    }
}
