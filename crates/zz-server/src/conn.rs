//! Per-connection task: demux JSON control / binary input frames, enforce
//! presence (heartbeat ping + traffic timeout) and the input flood cap, and
//! shuttle between the WebSocket, the lobby manager, and (once a match
//! starts) the room. The socket never touches game state directly.

use axum::extract::ws::{Message, WebSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use zz_core::constants::{MAX_INPUTS_PER_SECOND, PING_INTERVAL_MS, PRESENCE_TIMEOUT_MS};
use zz_core::protocol::{BIN_INPUT, ClientMsg, ServerMsg, decode_input, parse_client_msg};

use crate::lobby::LobbyCmd;
use crate::room::{OutMsg, RoomCmd};

static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(1);

pub async fn handle_socket(mut ws: WebSocket, lobby: mpsc::Sender<LobbyCmd>) {
    let conn_id = NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed);
    let (out_tx, mut out_rx) = mpsc::channel::<OutMsg>(64);
    let (bind_tx, mut bind_rx) = mpsc::channel::<mpsc::Sender<RoomCmd>>(4);

    if lobby
        .send(LobbyCmd::Register {
            conn_id,
            out: out_tx.clone(),
            bind: bind_tx,
        })
        .await
        .is_err()
    {
        return;
    }

    let welcome = ServerMsg::Welcome {
        player_id: format!("c{conn_id}"),
        protocol: zz_core::PROTOCOL_VERSION,
    };
    if send_json(&mut ws, &welcome).await.is_err() {
        let _ = lobby.send(LobbyCmd::Deregister { conn_id }).await;
        return;
    }

    let mut ping = tokio::time::interval(Duration::from_millis(PING_INTERVAL_MS));
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_rx = Instant::now();
    let mut room: Option<mpsc::Sender<RoomCmd>> = None;

    // input flood cap: a simple 1-second window counter
    let mut window_start = Instant::now();
    let mut window_count: u32 = 0;

    loop {
        tokio::select! {
            msg = ws.recv() => {
                let Some(Ok(msg)) = msg else { break };
                last_rx = Instant::now();
                match msg {
                    Message::Text(text) => match parse_client_msg(text.as_str()) {
                        Some(ClientMsg::Hello { name }) => {
                            let name = sanitize_name(&name);
                            let _ = lobby.send(LobbyCmd::Hello { conn_id, name }).await;
                        }
                        Some(ClientMsg::CreateLobby { env }) => {
                            let _ = lobby.send(LobbyCmd::Create { conn_id, env }).await;
                        }
                        Some(ClientMsg::JoinLobby { code }) => {
                            let _ = lobby.send(LobbyCmd::Join { conn_id, code }).await;
                        }
                        Some(ClientMsg::SetEnv { env }) => {
                            let _ = lobby.send(LobbyCmd::SetEnv { conn_id, env }).await;
                        }
                        Some(ClientMsg::StartGame) => {
                            let _ = lobby.send(LobbyCmd::Start { conn_id }).await;
                        }
                        Some(ClientMsg::LeaveLobby) => {
                            if let Some(r) = &room {
                                let _ = r.try_send(RoomCmd::Leave { conn_id });
                            }
                            room = None;
                            let _ = lobby.send(LobbyCmd::Leave { conn_id }).await;
                        }
                        Some(ClientMsg::Pause) => {
                            if let Some(r) = &room {
                                let _ = r.try_send(RoomCmd::Pause { conn_id });
                            }
                        }
                        Some(ClientMsg::Resume) => {
                            if let Some(r) = &room {
                                let _ = r.try_send(RoomCmd::Resume { conn_id });
                            }
                        }
                        Some(ClientMsg::Pong { .. }) => {} // traffic already counted
                        None => {}                         // garbage drops
                    },
                    Message::Binary(bin) => {
                        if bin.first() == Some(&BIN_INPUT)
                            && let Some(r) = &room
                        {
                            if window_start.elapsed() >= Duration::from_secs(1) {
                                window_start = Instant::now();
                                window_count = 0;
                            }
                            window_count += 1;
                            if window_count <= MAX_INPUTS_PER_SECOND
                                && let Some(input) = decode_input(&bin)
                            {
                                let _ = r.try_send(RoomCmd::Input { conn_id, input });
                            }
                        }
                    }
                    Message::Close(_) => break,
                    _ => {} // protocol ping/pong handled by axum
                }
            }
            new_room = bind_rx.recv() => {
                let Some(new_room) = new_room else { break };
                room = Some(new_room);
            }
            _ = ping.tick() => {
                if last_rx.elapsed() >= Duration::from_millis(PRESENCE_TIMEOUT_MS) {
                    break; // silent socket: they left — free the slot for the team
                }
                let t = now_ms();
                if send_json(&mut ws, &ServerMsg::Ping { t }).await.is_err() {
                    break;
                }
                // Protocol-level ping too: browsers auto-pong these in the
                // network process even while the tab is throttled/backgrounded
                // (the app-level pong above needs the render loop). A throttled
                // tab stays present; a closed tab still dies with the socket.
                if ws.send(Message::Ping(Vec::new().into())).await.is_err() {
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

    if let Some(r) = &room {
        let _ = r.try_send(RoomCmd::Leave { conn_id });
    }
    let _ = lobby.send(LobbyCmd::Deregister { conn_id }).await;
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
