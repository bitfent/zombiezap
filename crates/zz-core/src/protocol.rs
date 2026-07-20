//! Wire protocol: JSON control messages + the binary input frame.
//!
//! Control plane is JSON via serde. Binary plane is hand-rolled little-endian
//! (`BIN_INPUT` frames). No new dependencies beyond serde / serde_json.

use serde::{Deserialize, Serialize};

use crate::types::{EnvKind, PlayerInput};

// ── binary frame tags ──────────────────────────────────────────────────────

/// Binary frame tags (first byte of every binary WebSocket frame).
pub const BIN_INPUT: u8 = 0;
pub const BIN_SNAPSHOT: u8 = 1;

/// Length of a `BIN_INPUT` frame in bytes.
pub const INPUT_FRAME_LEN: usize = 14;

// ── client → server (JSON) ─────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Hello { name: String },
    CreateLobby { env: EnvKind },
    JoinLobby { code: String },
    SetEnv { env: EnvKind },
    StartGame,
    LeaveLobby,
    Pause,
    Resume,
    Pong { t: u64 },
}

// ── server → client (JSON) ─────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Welcome {
        player_id: String,
        protocol: u32,
    },
    LobbyState {
        code: String,
        host_id: String,
        players: Vec<LobbyPlayer>,
        env: EnvKind,
        invite_url: Option<String>,
    },
    GameStart {
        map_seed: String,
        env: EnvKind,
        your_slot: u8,
        players: Vec<RosterPlayer>,
    },
    Paused {
        by: String,
    },
    Resumed,
    PlayerLeft {
        id: String,
    },
    Ping {
        t: u64,
    },
    MatchEnd {
        stats: MatchStats,
    },
    Error {
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LobbyPlayer {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RosterPlayer {
    pub slot: u8,
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MatchStats {
    pub duration_ms: u64,
    pub zombies_killed: u32,
    pub peak_zombies: u32,
    pub difficulty_reached: u8,
    pub players: Vec<PlayerStats>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlayerStats {
    pub slot: u8,
    pub name: String,
    pub kills: u32,
    pub damage_dealt: u32,
    pub shots_fired: u32,
    pub hits: u32,
    pub grenades_thrown: u32,
    pub time_alive_ms: u64,
}

// ── binary input codec ─────────────────────────────────────────────────────

/// Buttons bits: 0 forward, 1 backward, 2 left, 3 right, 4 jump, 5 fire, 6 grenade, 7 interact.
fn pack_buttons(input: &PlayerInput) -> u8 {
    let mut b = 0u8;
    if input.forward {
        b |= 1 << 0;
    }
    if input.backward {
        b |= 1 << 1;
    }
    if input.left {
        b |= 1 << 2;
    }
    if input.right {
        b |= 1 << 3;
    }
    if input.jump {
        b |= 1 << 4;
    }
    if input.fire {
        b |= 1 << 5;
    }
    if input.grenade {
        b |= 1 << 6;
    }
    if input.interact {
        b |= 1 << 7;
    }
    b
}

fn unpack_buttons(b: u8) -> (bool, bool, bool, bool, bool, bool, bool, bool) {
    (
        (b & (1 << 0)) != 0,
        (b & (1 << 1)) != 0,
        (b & (1 << 2)) != 0,
        (b & (1 << 3)) != 0,
        (b & (1 << 4)) != 0,
        (b & (1 << 5)) != 0,
        (b & (1 << 6)) != 0,
        (b & (1 << 7)) != 0,
    )
}

/// Encode a `PlayerInput` as the 14-byte `BIN_INPUT` frame:
/// `[0]=BIN_INPUT` tag, `[1..5]=seq u32 LE`, `[5]=buttons bitfield`,
/// `[6..10]=yaw f32 LE`, `[10..14]=pitch f32 LE`.
pub fn encode_input(input: &PlayerInput) -> [u8; INPUT_FRAME_LEN] {
    let mut frame = [0u8; INPUT_FRAME_LEN];
    frame[0] = BIN_INPUT;
    frame[1..5].copy_from_slice(&input.seq.to_le_bytes());
    frame[5] = pack_buttons(input);
    frame[6..10].copy_from_slice(&input.yaw.to_le_bytes());
    frame[10..14].copy_from_slice(&input.pitch.to_le_bytes());
    frame
}

/// Decode a `BIN_INPUT` frame. `None` on wrong length or wrong tag. Never panics.
pub fn decode_input(frame: &[u8]) -> Option<PlayerInput> {
    if frame.len() != INPUT_FRAME_LEN {
        return None;
    }
    if frame[0] != BIN_INPUT {
        return None;
    }
    let seq = u32::from_le_bytes(frame[1..5].try_into().ok()?);
    let (forward, backward, left, right, jump, fire, grenade, interact) = unpack_buttons(frame[5]);
    let yaw = f32::from_le_bytes(frame[6..10].try_into().ok()?);
    let pitch = f32::from_le_bytes(frame[10..14].try_into().ok()?);
    Some(PlayerInput {
        seq,
        forward,
        backward,
        left,
        right,
        jump,
        fire,
        grenade,
        interact,
        yaw,
        pitch,
    })
}

/// Tolerant JSON parse: `None` on malformed/unknown (drop, don't error) —
/// the server treats garbage as absent, like the legacy `parseClientMessage`.
pub fn parse_client_msg(raw: &str) -> Option<ClientMsg> {
    serde_json::from_str(raw).ok()
}

// ── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::EnvKind;

    fn rt_client(msg: ClientMsg) {
        let s = serde_json::to_string(&msg).expect("serialize ClientMsg");
        let back: ClientMsg = serde_json::from_str(&s).expect("deserialize ClientMsg");
        assert_eq!(msg, back);
    }

    fn rt_server(msg: ServerMsg) {
        let s = serde_json::to_string(&msg).expect("serialize ServerMsg");
        let back: ServerMsg = serde_json::from_str(&s).expect("deserialize ServerMsg");
        assert_eq!(msg, back);
    }

    #[test]
    fn client_msg_round_trips() {
        rt_client(ClientMsg::Hello {
            name: "Alice".into(),
        });
        rt_client(ClientMsg::CreateLobby {
            env: EnvKind::Urban,
        });
        rt_client(ClientMsg::JoinLobby {
            code: "ABCDE".into(),
        });
        rt_client(ClientMsg::SetEnv {
            env: EnvKind::DesertTown,
        });
        rt_client(ClientMsg::StartGame);
        rt_client(ClientMsg::LeaveLobby);
        rt_client(ClientMsg::Pause);
        rt_client(ClientMsg::Resume);
        rt_client(ClientMsg::Pong { t: 42 });
    }

    #[test]
    fn server_msg_round_trips() {
        rt_server(ServerMsg::Welcome {
            player_id: "p1".into(),
            protocol: 1,
        });
        rt_server(ServerMsg::LobbyState {
            code: "ABCDE".into(),
            host_id: "p1".into(),
            players: vec![LobbyPlayer {
                id: "p1".into(),
                name: "Alice".into(),
            }],
            env: EnvKind::Urban,
            invite_url: Some("https://example.com/join/ABCDE".into()),
        });
        rt_server(ServerMsg::LobbyState {
            code: "XYZ".into(),
            host_id: "h".into(),
            players: vec![],
            env: EnvKind::SeaTown,
            invite_url: None,
        });
        rt_server(ServerMsg::GameStart {
            map_seed: "seed-1".into(),
            env: EnvKind::MountainTown,
            your_slot: 0,
            players: vec![RosterPlayer {
                slot: 0,
                id: "p1".into(),
                name: "Alice".into(),
            }],
        });
        rt_server(ServerMsg::Paused { by: "p1".into() });
        rt_server(ServerMsg::Resumed);
        rt_server(ServerMsg::PlayerLeft { id: "p2".into() });
        rt_server(ServerMsg::Ping { t: 99 });
        rt_server(ServerMsg::MatchEnd {
            stats: MatchStats {
                duration_ms: 60_000,
                zombies_killed: 10,
                peak_zombies: 5,
                difficulty_reached: 2,
                players: vec![PlayerStats {
                    slot: 0,
                    name: "Alice".into(),
                    kills: 3,
                    damage_dealt: 100,
                    shots_fired: 20,
                    hits: 8,
                    grenades_thrown: 1,
                    time_alive_ms: 55_000,
                }],
            },
        });
        rt_server(ServerMsg::Error {
            message: "nope".into(),
        });
    }

    #[test]
    fn exact_json_canaries() {
        assert_eq!(
            serde_json::to_string(&ClientMsg::JoinLobby {
                code: "ABCDE".into()
            })
            .unwrap(),
            r#"{"type":"join_lobby","code":"ABCDE"}"#
        );
        assert_eq!(
            serde_json::to_string(&ClientMsg::StartGame).unwrap(),
            r#"{"type":"start_game"}"#
        );
        let create = ClientMsg::CreateLobby {
            env: EnvKind::MountainTown,
        };
        let s = serde_json::to_string(&create).unwrap();
        assert!(
            s.contains(r#""env":"mountain_town""#),
            "expected mountain_town in {s}"
        );
        assert!(
            s.contains(r#""type":"create_lobby""#),
            "expected type in {s}"
        );
    }

    #[test]
    fn input_frame_round_trip() {
        let all_false = PlayerInput {
            seq: 0,
            forward: false,
            backward: false,
            left: false,
            right: false,
            jump: false,
            fire: false,
            grenade: false,
            interact: false,
            yaw: 0.0,
            pitch: 0.0,
        };
        assert_eq!(decode_input(&encode_input(&all_false)), Some(all_false));

        let all_true = PlayerInput {
            seq: 1,
            forward: true,
            backward: true,
            left: true,
            right: true,
            jump: true,
            fire: true,
            grenade: true,
            interact: true,
            yaw: 1.5,
            pitch: -0.25,
        };
        assert_eq!(decode_input(&encode_input(&all_true)), Some(all_true));

        let mixed = PlayerInput {
            seq: u32::MAX,
            forward: true,
            backward: false,
            left: true,
            right: false,
            jump: false,
            fire: true,
            grenade: false,
            interact: true,
            yaw: -3.1,
            pitch: 0.77,
        };
        assert_eq!(decode_input(&encode_input(&mixed)), Some(mixed));
    }

    #[test]
    fn decode_input_rejects_bad_frames() {
        assert_eq!(decode_input(&[]), None);
        assert_eq!(decode_input(&[0u8; 13]), None);
        assert_eq!(decode_input(&[0u8; 15]), None);

        let mut good = encode_input(&PlayerInput::default());
        good[0] = BIN_SNAPSHOT; // wrong tag
        assert_eq!(decode_input(&good), None);
        good[0] = 0xFF;
        assert_eq!(decode_input(&good), None);
    }

    #[test]
    fn parse_client_msg_tolerant() {
        let valid = r#"{"type":"hello","name":"Bob"}"#;
        assert_eq!(
            parse_client_msg(valid),
            Some(ClientMsg::Hello { name: "Bob".into() })
        );
        assert_eq!(parse_client_msg("{"), None);
        assert_eq!(parse_client_msg(r#"{"type":"bogus"}"#), None);
        assert_eq!(parse_client_msg(""), None);
    }
}
