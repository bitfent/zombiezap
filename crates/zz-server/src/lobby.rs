//! Lobby manager: one task owning every lobby and the rooms they spawn.
//! ShotAnte's durable-offer/ephemeral-presence split collapses to the simple
//! co-op case: a lobby IS the room's waiting state, keyed by a 5-letter code,
//! entirely in memory (no DB by design). Rematch = the room ends and the
//! roster lands back in the same lobby code.

use std::collections::HashMap;
use tokio::sync::mpsc;
use zz_core::constants::{LOBBY_CODE_ALPHABET, LOBBY_CODE_LEN, MAX_PLAYERS};
use zz_core::protocol::{LobbyPlayer, ServerMsg};
use zz_core::types::EnvKind;

use crate::room::{OutMsg, Room, RoomCmd};

pub enum LobbyCmd {
    /// A connection appeared: `out` for JSON pushes, `bind` to hand it a room
    /// input channel when a match starts.
    Register {
        conn_id: u64,
        out: mpsc::Sender<OutMsg>,
        bind: mpsc::Sender<mpsc::Sender<RoomCmd>>,
    },
    Deregister {
        conn_id: u64,
    },
    Hello {
        conn_id: u64,
        name: String,
    },
    Create {
        conn_id: u64,
        env: EnvKind,
    },
    Join {
        conn_id: u64,
        code: String,
    },
    SetEnv {
        conn_id: u64,
        env: EnvKind,
    },
    Start {
        conn_id: u64,
    },
    Leave {
        conn_id: u64,
    },
    /// From a room task: the match ended (or emptied) — roster returns to the
    /// lobby for a rematch.
    MatchEnded {
        code: String,
    },
}

struct ConnInfo {
    name: String,
    out: mpsc::Sender<OutMsg>,
    bind: mpsc::Sender<mpsc::Sender<RoomCmd>>,
    lobby: Option<String>,
}

struct Lobby {
    host: u64,
    members: Vec<u64>,
    env: EnvKind,
    in_match: bool,
    matches_played: u32,
}

pub struct LobbyManager {
    cmds: mpsc::Receiver<LobbyCmd>,
    self_tx: mpsc::Sender<LobbyCmd>,
    conns: HashMap<u64, ConnInfo>,
    lobbies: HashMap<String, Lobby>,
    code_rng: zz_core::rng::Mulberry32,
    public_url: Option<String>,
}

impl LobbyManager {
    pub fn spawn() -> mpsc::Sender<LobbyCmd> {
        let (tx, rx) = mpsc::channel(256);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos();
        let mgr = LobbyManager {
            cmds: rx,
            self_tx: tx.clone(),
            conns: HashMap::new(),
            lobbies: HashMap::new(),
            code_rng: zz_core::rng::Mulberry32::new(nanos ^ 0x5eed_c0de),
            public_url: std::env::var("PUBLIC_URL").ok(),
        };
        tokio::spawn(mgr.run());
        tx
    }

    async fn run(mut self) {
        while let Some(cmd) = self.cmds.recv().await {
            self.handle(cmd);
        }
    }

    fn handle(&mut self, cmd: LobbyCmd) {
        match cmd {
            LobbyCmd::Register { conn_id, out, bind } => {
                self.conns.insert(
                    conn_id,
                    ConnInfo {
                        name: "survivor".into(),
                        out,
                        bind,
                        lobby: None,
                    },
                );
            }
            LobbyCmd::Deregister { conn_id } => {
                self.leave_current(conn_id);
                self.conns.remove(&conn_id);
            }
            LobbyCmd::Hello { conn_id, name } => {
                if let Some(c) = self.conns.get_mut(&conn_id) {
                    c.name = name;
                }
            }
            LobbyCmd::Create { conn_id, env } => {
                self.leave_current(conn_id);
                let code = self.fresh_code();
                self.lobbies.insert(
                    code.clone(),
                    Lobby {
                        host: conn_id,
                        members: vec![conn_id],
                        env,
                        in_match: false,
                        matches_played: 0,
                    },
                );
                if let Some(c) = self.conns.get_mut(&conn_id) {
                    c.lobby = Some(code.clone());
                }
                self.broadcast_lobby(&code);
            }
            LobbyCmd::Join { conn_id, code } => {
                let code = code.trim().to_uppercase();
                match self.lobbies.get(&code) {
                    None => {
                        self.send_error(conn_id, "no lobby with that code");
                        return;
                    }
                    Some(l) if l.members.len() >= MAX_PLAYERS => {
                        self.send_error(conn_id, "lobby is full");
                        return;
                    }
                    Some(l) if l.in_match => {
                        self.send_error(conn_id, "match in progress — try after this run");
                        return;
                    }
                    Some(l) if l.members.contains(&conn_id) => {
                        self.broadcast_lobby(&code);
                        return;
                    }
                    Some(_) => {}
                }
                self.leave_current(conn_id);
                if let Some(lobby) = self.lobbies.get_mut(&code) {
                    lobby.members.push(conn_id);
                }
                if let Some(c) = self.conns.get_mut(&conn_id) {
                    c.lobby = Some(code.clone());
                }
                self.broadcast_lobby(&code);
            }
            LobbyCmd::SetEnv { conn_id, env } => {
                let Some(code) = self.lobby_of(conn_id) else {
                    return;
                };
                if let Some(lobby) = self.lobbies.get_mut(&code)
                    && lobby.host == conn_id
                    && !lobby.in_match
                {
                    lobby.env = env;
                    self.broadcast_lobby(&code);
                }
            }
            LobbyCmd::Start { conn_id } => {
                let Some(code) = self.lobby_of(conn_id) else {
                    return;
                };
                let Some(lobby) = self.lobbies.get_mut(&code) else {
                    return;
                };
                if lobby.host != conn_id || lobby.in_match {
                    return;
                }
                lobby.in_match = true;
                lobby.matches_played += 1;
                let seed = format!("{code}-{}", lobby.matches_played);
                let room_tx = Room::spawn(lobby.env, seed, code.clone(), self.self_tx.clone());
                let members = lobby.members.clone();
                for m in members {
                    if let Some(c) = self.conns.get(&m) {
                        // bind the conn's input path to the new room, then join
                        let _ = c.bind.try_send(room_tx.clone());
                        let _ = room_tx.try_send(RoomCmd::Join {
                            conn_id: m,
                            name: c.name.clone(),
                            tx: c.out.clone(),
                        });
                    }
                }
            }
            LobbyCmd::Leave { conn_id } => {
                self.leave_current(conn_id);
            }
            LobbyCmd::MatchEnded { code } => {
                if let Some(lobby) = self.lobbies.get_mut(&code) {
                    lobby.in_match = false;
                    // drop members whose connections vanished mid-match
                    let alive: Vec<u64> = lobby
                        .members
                        .iter()
                        .copied()
                        .filter(|m| self.conns.contains_key(m))
                        .collect();
                    if alive.is_empty() {
                        self.lobbies.remove(&code);
                        return;
                    }
                    if !alive.contains(&lobby.host) {
                        lobby.host = alive[0];
                    }
                    lobby.members = alive;
                    self.broadcast_lobby(&code);
                }
            }
        }
    }

    fn lobby_of(&self, conn_id: u64) -> Option<String> {
        self.conns.get(&conn_id).and_then(|c| c.lobby.clone())
    }

    /// Remove a connection from its lobby (host migration, empty-lobby GC).
    fn leave_current(&mut self, conn_id: u64) {
        let Some(code) = self.lobby_of(conn_id) else {
            return;
        };
        if let Some(c) = self.conns.get_mut(&conn_id) {
            c.lobby = None;
        }
        let Some(lobby) = self.lobbies.get_mut(&code) else {
            return;
        };
        lobby.members.retain(|m| *m != conn_id);
        if lobby.members.is_empty() {
            // in-match rooms drain on their own; the lobby record can go now
            self.lobbies.remove(&code);
            return;
        }
        if lobby.host == conn_id {
            lobby.host = lobby.members[0];
        }
        self.broadcast_lobby(&code);
    }

    fn fresh_code(&mut self) -> String {
        loop {
            let code: String = (0..LOBBY_CODE_LEN)
                .map(|_| {
                    let i = (self.code_rng.next() * LOBBY_CODE_ALPHABET.len() as f64) as usize;
                    LOBBY_CODE_ALPHABET[i.min(LOBBY_CODE_ALPHABET.len() - 1)] as char
                })
                .collect();
            if !self.lobbies.contains_key(&code) {
                return code;
            }
        }
    }

    fn broadcast_lobby(&self, code: &str) {
        let Some(lobby) = self.lobbies.get(code) else {
            return;
        };
        let players: Vec<LobbyPlayer> = lobby
            .members
            .iter()
            .filter_map(|m| {
                self.conns.get(m).map(|c| LobbyPlayer {
                    id: format!("c{m}"),
                    name: c.name.clone(),
                })
            })
            .collect();
        let msg = ServerMsg::LobbyState {
            code: code.to_string(),
            host_id: format!("c{}", lobby.host),
            players,
            env: lobby.env,
            invite_url: self
                .public_url
                .as_ref()
                .map(|u| format!("{}/?join={}", u.trim_end_matches('/'), code)),
        };
        if let Ok(json) = serde_json::to_string(&msg) {
            for m in &lobby.members {
                if let Some(c) = self.conns.get(m) {
                    let _ = c.out.try_send(OutMsg::Json(json.clone()));
                }
            }
        }
    }

    fn send_error(&self, conn_id: u64, message: &str) {
        if let (Some(c), Ok(json)) = (
            self.conns.get(&conn_id),
            serde_json::to_string(&ServerMsg::Error {
                message: message.into(),
            }),
        ) {
            let _ = c.out.try_send(OutMsg::Json(json));
        }
    }
}
