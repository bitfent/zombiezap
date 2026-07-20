//! Per-connection task: demux JSON control / binary input frames, enforce
//! presence (heartbeat ping + traffic timeout) and the input flood cap, and
//! shuttle between the WebSocket and the room. The socket never touches game
//! state directly — everything goes through the room's command channel.

use axum::extract::ws::{Message, WebSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use zz_core::constants::{MAX_INPUTS_PER_SECOND, PING_INTERVAL_MS, PRESENCE_TIMEOUT_MS};
use zz_core::protocol::{BIN_INPUT, ClientMsg, ServerMsg, decode_input, parse_client_msg};

use crate::room::{OutMsg, RoomCmd};

static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(1);

pub async fn handle_socket(mut ws: WebSocket, room: mpsc::Sender<RoomCmd>) {
    let conn_id = NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed);
    let (out_tx, mut out_rx) = mpsc::channel::<OutMsg>(64);

    // welcome immediately; the room is joined on Hello
    let welcome = ServerMsg::Welcome {
        player_id: format!("c{conn_id}"),
        protocol: zz_core::PROTOCOL_VERSION,
    };
    if send_json(&mut ws, &welcome).await.is_err() {
        return;
    }

    let mut ping = tokio::time::interval(Duration::from_millis(PING_INTERVAL_MS));
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_rx = Instant::now();
    let mut joined = false;

    // input flood cap: a simple 1-second window counter
    let mut window_start = Instant::now();
    let mut window_count: u32 = 0;

    loop {
        tokio::select! {
            msg = ws.recv() => {
                let Some(Ok(msg)) = msg else { break };
                last_rx = Instant::now();
                match msg {
                    Message::Text(text) => {
                        match parse_client_msg(text.as_str()) {
                            Some(ClientMsg::Hello { name }) if !joined => {
                                joined = true;
                                let name = sanitize_name(&name);
                                let _ = room.send(RoomCmd::Join { conn_id, name, tx: out_tx.clone() }).await;
                            }
                            Some(ClientMsg::Pong { .. }) => {} // traffic already counted
                            Some(ClientMsg::Pause) if joined => {
                                let _ = room.try_send(RoomCmd::Pause { conn_id });
                            }
                            Some(ClientMsg::Resume) if joined => {
                                let _ = room.try_send(RoomCmd::Resume { conn_id });
                            }
                            Some(_) | None => {} // lobby control lands in M5; garbage drops
                        }
                    }
                    Message::Binary(bin) => {
                        if bin.first() == Some(&BIN_INPUT) && joined {
                            // flood cap before the room ever sees it
                            if window_start.elapsed() >= Duration::from_secs(1) {
                                window_start = Instant::now();
                                window_count = 0;
                            }
                            window_count += 1;
                            if window_count <= MAX_INPUTS_PER_SECOND
                                && let Some(input) = decode_input(&bin)
                            {
                                let _ = room.try_send(RoomCmd::Input { conn_id, input });
                            }
                        }
                    }
                    Message::Close(_) => break,
                    _ => {} // protocol ping/pong handled by axum
                }
            }
            _ = ping.tick() => {
                if last_rx.elapsed() >= Duration::from_millis(PRESENCE_TIMEOUT_MS) {
                    break; // silent socket: they left — free the slot for the team
                }
                let t = now_ms();
                if send_json(&mut ws, &ServerMsg::Ping { t }).await.is_err() {
                    break;
                }
            }
            out = out_rx.recv() => {
                let Some(out) = out else { break };
                let res = match out {
                    OutMsg::Json(s) => ws.send(Message::Text(s.into())).await,
                    OutMsg::Bin(b) => ws.send(Message::Binary(b.into())).await,
                };
                if res.is_err() {
                    break;
                }
            }
        }
    }

    if joined {
        let _ = room.send(RoomCmd::Leave { conn_id }).await;
    }
}

async fn send_json(ws: &mut WebSocket, msg: &ServerMsg) -> Result<(), axum::Error> {
    let s = serde_json::to_string(msg).expect("ServerMsg serializes");
    ws.send(Message::Text(s.into())).await
}

fn sanitize_name(name: &str) -> String {
    let cleaned: String = name.chars().filter(|c| !c.is_control()).take(16).collect();
    if cleaned.trim().is_empty() {
        "survivor".into()
    } else {
        cleaned.trim().into()
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
