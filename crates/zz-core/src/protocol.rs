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
/// Proximity voice: `[tag, speaker_slot, 16 kHz mono i16 LE PCM…]`.
pub const BIN_VOICE: u8 = 2;

/// Length of a `BIN_INPUT` frame in bytes.
///
/// Layout (M21+): `[0]=tag`, `[1..5]=seq u32 LE`, `[5]=buttons`, `[6]=flags2`
/// (bit0 melee, bit1 reload; bits 2–7 reserved 0), `[7..11]=yaw f32 LE`,
/// `[11..15]=pitch f32 LE`. Pre-M21 14-byte frames decode to `None` — acceptable
/// pre-release (no wire-compat commitment yet).
pub const INPUT_FRAME_LEN: usize = 15;

/// Max PCM payload for a `BIN_VOICE` frame: 16 kHz × 0.120 s × 2 bytes/sample.
pub const MAX_VOICE_PAYLOAD: usize = 3840;

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

/// Second flags byte: bit0 melee, bit1 reload; bits 2–7 reserved zero.
fn pack_flags2(input: &PlayerInput) -> u8 {
    let mut b = 0u8;
    if input.melee {
        b |= 1 << 0;
    }
    if input.reload {
        b |= 1 << 1;
    }
    b
}

fn unpack_flags2(b: u8) -> (bool, bool) {
    ((b & (1 << 0)) != 0, (b & (1 << 1)) != 0)
}

/// Encode a `PlayerInput` as the 15-byte `BIN_INPUT` frame:
/// `[0]=BIN_INPUT` tag, `[1..5]=seq u32 LE`, `[5]=buttons bitfield`,
/// `[6]=flags2 (melee|reload)`, `[7..11]=yaw f32 LE`, `[11..15]=pitch f32 LE`.
pub fn encode_input(input: &PlayerInput) -> [u8; INPUT_FRAME_LEN] {
    let mut frame = [0u8; INPUT_FRAME_LEN];
    frame[0] = BIN_INPUT;
    frame[1..5].copy_from_slice(&input.seq.to_le_bytes());
    frame[5] = pack_buttons(input);
    frame[6] = pack_flags2(input);
    frame[7..11].copy_from_slice(&input.yaw.to_le_bytes());
    frame[11..15].copy_from_slice(&input.pitch.to_le_bytes());
    frame
}

/// Decode a `BIN_INPUT` frame. `None` on wrong length or wrong tag. Never panics.
/// Pre-M21 14-byte frames are rejected (length mismatch).
pub fn decode_input(frame: &[u8]) -> Option<PlayerInput> {
    if frame.len() != INPUT_FRAME_LEN {
        return None;
    }
    if frame[0] != BIN_INPUT {
        return None;
    }
    let seq = u32::from_le_bytes(frame[1..5].try_into().ok()?);
    let (forward, backward, left, right, jump, fire, grenade, interact) = unpack_buttons(frame[5]);
    let (melee, reload) = unpack_flags2(frame[6]);
    let yaw = f32::from_le_bytes(frame[7..11].try_into().ok()?);
    let pitch = f32::from_le_bytes(frame[11..15].try_into().ok()?);
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
        melee,
        reload,
        yaw,
        pitch,
    })
}

/// Encode a proximity-voice frame: `[0]=BIN_VOICE`, `[1]=slot`, `[2..]=pcm`.
/// `None` if `pcm` is empty or longer than [`MAX_VOICE_PAYLOAD`]. Never panics.
pub fn encode_voice(slot: u8, pcm: &[u8]) -> Option<Vec<u8>> {
    if pcm.is_empty() || pcm.len() > MAX_VOICE_PAYLOAD {
        return None;
    }
    let mut frame = Vec::with_capacity(2 + pcm.len());
    frame.push(BIN_VOICE);
    frame.push(slot);
    frame.extend_from_slice(pcm);
    Some(frame)
}

/// Decode a `BIN_VOICE` frame. `None` on wrong tag, short, empty PCM, or
/// oversized payload. Never panics.
pub fn decode_voice(frame: &[u8]) -> Option<(u8, &[u8])> {
    if frame.len() < 3 {
        return None;
    }
    if frame[0] != BIN_VOICE {
        return None;
    }
    let pcm = &frame[2..];
    if pcm.is_empty() || pcm.len() > MAX_VOICE_PAYLOAD {
        return None;
    }
    Some((frame[1], pcm))
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
            melee: false,
            reload: false,
            yaw: 0.0,
            pitch: 0.0,
        };
        assert_eq!(decode_input(&encode_input(&all_false)), Some(all_false));
        assert_eq!(encode_input(&all_false).len(), INPUT_FRAME_LEN);

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
            melee: true,
            reload: true,
            yaw: 1.5,
            pitch: -0.25,
        };
        let enc = encode_input(&all_true);
        assert_eq!(enc.len(), 15);
        assert_eq!(enc[6] & 0b11, 0b11, "melee|reload bits set");
        assert_eq!(enc[6] & !0b11, 0, "reserved flags2 bits zero");
        assert_eq!(decode_input(&enc), Some(all_true));

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
            melee: true,
            reload: false,
            yaw: -3.1,
            pitch: 0.77,
        };
        assert_eq!(decode_input(&encode_input(&mixed)), Some(mixed));

        let only_reload = PlayerInput {
            seq: 9,
            reload: true,
            yaw: 0.5,
            ..Default::default()
        };
        let back = decode_input(&encode_input(&only_reload)).unwrap();
        assert!(back.reload && !back.melee);
    }

    #[test]
    fn decode_input_rejects_bad_frames() {
        assert_eq!(decode_input(&[]), None);
        assert_eq!(decode_input(&[0u8; 13]), None);
        // Pre-M21 14-byte frames are no longer accepted.
        assert_eq!(decode_input(&[0u8; 14]), None);
        assert_eq!(decode_input(&[0u8; 16]), None);

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

    #[test]
    fn voice_frame_round_trip() {
        let pcm = [0x01u8, 0x00, 0xFF, 0x7F];
        let frame = encode_voice(3, &pcm).expect("encode");
        assert_eq!(frame[0], BIN_VOICE);
        assert_eq!(frame[1], 3);
        assert_eq!(&frame[2..], &pcm);
        let (slot, back) = decode_voice(&frame).expect("decode");
        assert_eq!(slot, 3);
        assert_eq!(back, &pcm);

        // max-size payload
        let max = vec![0xABu8; MAX_VOICE_PAYLOAD];
        let frame = encode_voice(0, &max).expect("max encode");
        assert_eq!(frame.len(), 2 + MAX_VOICE_PAYLOAD);
        let (slot, back) = decode_voice(&frame).expect("max decode");
        assert_eq!(slot, 0);
        assert_eq!(back, max.as_slice());
    }

    #[test]
    fn decode_voice_rejects_bad_frames() {
        assert_eq!(decode_voice(&[]), None);
        assert_eq!(decode_voice(&[BIN_VOICE]), None);
        assert_eq!(decode_voice(&[BIN_VOICE, 0]), None); // empty PCM
        assert_eq!(decode_voice(&[BIN_INPUT, 0, 1, 2]), None); // wrong tag
        assert_eq!(decode_voice(&[BIN_SNAPSHOT, 0, 1]), None);
        assert_eq!(decode_voice(&[0xFF, 0, 1]), None);

        // oversized PCM (MAX + 1 bytes after header)
        let mut over = vec![BIN_VOICE, 1];
        over.extend(std::iter::repeat_n(0u8, MAX_VOICE_PAYLOAD + 1));
        assert_eq!(decode_voice(&over), None);
    }

    #[test]
    fn encode_voice_rejects_empty_and_oversized() {
        assert_eq!(encode_voice(0, &[]), None);
        let over = vec![0u8; MAX_VOICE_PAYLOAD + 1];
        assert_eq!(encode_voice(0, &over), None);
    }
}
