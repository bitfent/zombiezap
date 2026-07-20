//! Proximity voice relay: two bots in a started match; frames fan out by
//! authoritative position, speaker slot is rewritten (anti-spoof), self-echo
//! is suppressed, and malformed frames never kill the socket.

use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use zz_core::protocol::{
    BIN_VOICE, ClientMsg, ServerMsg, decode_voice, encode_voice,
};
use zz_core::snapshot::SnapshotDecoder;

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
            ServerMsg::LobbyState { .. } => continue,
            other => panic!("expected game_start, got {other:?}"),
        }
    }
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

/// Next BIN_VOICE frame, skipping snapshots / control / pings.
async fn recv_voice(ws: &mut Ws) -> Option<Vec<u8>> {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .ok()??
            .ok()?;
        match msg {
            Message::Binary(b) if b.first() == Some(&BIN_VOICE) => return Some(b.to_vec()),
            Message::Close(_) => return None,
            _ => continue,
        }
    }
}

/// Drain until a snapshot shows `n` players (room has integrated both joins).
async fn wait_roster(ws: &mut Ws, n: usize) {
    let mut dec = SnapshotDecoder::new();
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("timeout waiting for roster")
            .expect("ws closed")
            .expect("ws err");
        if let Message::Binary(b) = msg
            && let Ok(snap) = dec.decode(&b)
            && snap.players.len() == n
        {
            return;
        }
    }
}

#[tokio::test]
async fn proximity_voice_relay_slot_rewrite_and_self_echo() {
    pin_map();
    let url = start_server().await;
    let (mut a, code) = create_lobby(&url, "talker").await;
    let mut b = join_lobby(&url, "listener", &code).await;
    let _ = recv_json(&mut a).await.expect("roster update");
    start_game(&mut a).await;
    let slot_a = wait_game_start(&mut a).await;
    let slot_b = wait_game_start(&mut b).await;
    assert_ne!(slot_a, slot_b);

    // Spawns are clustered — both start well inside CHAT_PROXIMITY_RADIUS.
    wait_roster(&mut a, 2).await;
    wait_roster(&mut b, 2).await;

    // Spoofed client slot (0xFF) must be rewritten to A's authoritative slot.
    let pcm: Vec<u8> = (0u8..64).collect();
    let spoofed = encode_voice(0xFF, &pcm).expect("encode voice");
    a.send(Message::Binary(spoofed.into())).await.unwrap();

    let relayed = recv_voice(&mut b).await.expect("B should receive voice");
    let (slot, got_pcm) = decode_voice(&relayed).expect("valid BIN_VOICE");
    assert_eq!(slot, slot_a, "server must rewrite speaker slot");
    assert_eq!(got_pcm, pcm.as_slice(), "PCM must be unchanged");

    // A must not hear its own frame back.
    let echo = tokio::time::timeout(Duration::from_millis(400), recv_voice(&mut a)).await;
    assert!(
        echo.is_err(),
        "speaker must not receive self-echo, got {echo:?}"
    );
}

#[tokio::test]
async fn malformed_voice_keeps_connection_alive() {
    pin_map();
    let url = start_server().await;
    let (mut a, code) = create_lobby(&url, "talker").await;
    let mut b = join_lobby(&url, "listener", &code).await;
    let _ = recv_json(&mut a).await.expect("roster update");
    start_game(&mut a).await;
    let slot_a = wait_game_start(&mut a).await;
    let _slot_b = wait_game_start(&mut b).await;

    wait_roster(&mut a, 2).await;
    wait_roster(&mut b, 2).await;

    // Malformed frames: wrong tag, too short, empty PCM header only, oversized.
    let oversized = {
        let mut v = vec![BIN_VOICE, 0];
        v.extend(std::iter::repeat_n(0u8, zz_core::protocol::MAX_VOICE_PAYLOAD + 1));
        v
    };
    for bad in [
        vec![0xFFu8, 1, 2, 3],               // unknown tag
        vec![BIN_VOICE],                     // truncated
        vec![BIN_VOICE, 0],                  // empty PCM
        oversized,
    ] {
        a.send(Message::Binary(bad.into())).await.unwrap();
    }

    // Connection must still accept a valid frame and relay it.
    let pcm = b"hello-voice-pcm-bytes!!".to_vec();
    let good = encode_voice(99, &pcm).expect("encode");
    a.send(Message::Binary(good.into())).await.unwrap();

    let relayed = recv_voice(&mut b).await.expect("relay after malformed");
    let (slot, got) = decode_voice(&relayed).expect("decode");
    assert_eq!(slot, slot_a);
    assert_eq!(got, pcm.as_slice());

    // A is still live: snapshots (or pings) keep arriving.
    let still_up = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match a.next().await {
                Some(Ok(Message::Binary(_))) | Some(Ok(Message::Text(_))) => break true,
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break false,
                _ => continue,
            }
        }
    })
    .await;
    assert_eq!(still_up, Ok(true), "speaker socket must stay open");
}
