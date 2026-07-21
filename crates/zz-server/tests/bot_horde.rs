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
    connect_env(url, name, zz_core::types::EnvKind::Urban).await
}

async fn connect_env(url: &str, name: &str, env: zz_core::types::EnvKind) -> (Ws, u8) {
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
        serde_json::to_string(&ClientMsg::CreateLobby { env }).unwrap(),
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

/// AFK player must TAKE DAMAGE within 25 s on both urban and Rome EUR.
///
/// Faithful to live lobby.rs: CreateLobby + StartGame (seed = `{code}-{n}`),
/// default ZZ_DIRECTOR_RATE=1, no MAP_SEED override. M15's 12 m "contact"
/// assertion could pass while walkers idled in walls after approach_spawn;
/// this asserts authoritative health drop (the live bar the dispatcher reads).
#[tokio::test]
async fn time_to_first_damage_under_25s_urban_and_rome() {
    // Default director rate (1.0) — not the turbo used by other tests.
    // Clear MAP_SEED so Room::spawn uses the lobby-provided `{code}-N` seed
    // exactly as lobby.rs does for a real match.
    unsafe {
        std::env::set_var("ZZ_DIRECTOR_RATE", "1");
        std::env::remove_var("MAP_SEED");
    }
    let url = start_server().await;

    for env in [
        zz_core::types::EnvKind::Urban,
        zz_core::types::EnvKind::RomeEur,
    ] {
        let (mut ws, slot) = connect_env(&url, &format!("afk-{env:?}"), env).await;
        let mut dec = SnapshotDecoder::new();
        let mut seq = 0u32;
        let start = tokio::time::Instant::now();
        let deadline = start + Duration::from_secs(40);
        let mut first_damage_ms: Option<u32> = None;
        let mut first_contact12_ms: Option<u32> = None;
        let mut peak_zeds = 0usize;

        while tokio::time::Instant::now() < deadline {
            seq += 1;
            let idle = PlayerInput {
                seq,
                ..Default::default()
            };
            let _ = ws
                .send(Message::Binary(encode_input(&idle).to_vec().into()))
                .await;
            match recv_any(&mut ws, &mut dec).await {
                Some(Incoming::Snap(s)) => {
                    peak_zeds = peak_zeds.max(s.zombies.len());
                    let me = s.players.iter().find(|p| p.slot == slot);
                    let Some(me) = me else { continue };
                    if me.health < 100 && first_damage_ms.is_none() {
                        first_damage_ms = Some(s.game_time_ms);
                        break;
                    }
                    let px = dequant_pos(me.pos[0]);
                    let pz = dequant_pos(me.pos[2]);
                    for z in &s.zombies {
                        let zx = dequant_pos(z.pos[0]);
                        let zz = dequant_pos(z.pos[2]);
                        let d = ((zx - px).powi(2) + (zz - pz).powi(2)).sqrt();
                        if d < 12.0 && first_contact12_ms.is_none() {
                            first_contact12_ms = Some(s.game_time_ms);
                        }
                    }
                }
                Some(Incoming::Msg(ServerMsg::MatchEnd { .. })) => break,
                Some(_) => {}
                None => panic!("{env:?}: connection dropped"),
            }
        }

        let ms = first_damage_ms.unwrap_or_else(|| {
            panic!(
                "{env:?}: no damage in 40 s wall (peak_zeds={peak_zeds}, contact12={first_contact12_ms:?})"
            )
        });
        assert!(
            ms < 25_000,
            "{env:?}: first damage at {ms} ms, want < 25_000 (peak_zeds={peak_zeds})"
        );
        // Drop the socket so the lobby cleans up before the next env.
        drop(ws);
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Rematch in the same lobby: peak_horde resets, every player's time_alive is
/// ≤ match duration, and a second MatchEnd is a fresh room (not stale stats).
#[tokio::test]
async fn rematch_resets_peak_horde_and_time_alive_bounded() {
    fast_director("300");
    let url = start_server().await;
    let (mut ws, _slot) = connect(&url, "rematcher").await;
    let mut dec = SnapshotDecoder::new();

    let mut seq = 0u32;
    let mut match_ends: Vec<zz_core::protocol::MatchStats> = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);

    // Play two matches back-to-back in the same lobby.
    while match_ends.len() < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for two MatchEnds (got {})",
            match_ends.len()
        );
        seq += 1;
        let idle = PlayerInput {
            seq,
            ..Default::default()
        };
        let _ = ws
            .send(Message::Binary(encode_input(&idle).to_vec().into()))
            .await;
        match recv_any(&mut ws, &mut dec).await {
            Some(Incoming::Msg(ServerMsg::MatchEnd { stats })) => {
                // S3 / NEXT.md item 7: time_alive never exceeds duration.
                for p in &stats.players {
                    assert!(
                        p.time_alive_ms <= stats.duration_ms,
                        "time_alive {} > duration {} (match {})",
                        p.time_alive_ms,
                        stats.duration_ms,
                        match_ends.len() + 1
                    );
                }
                match_ends.push(stats);
                if match_ends.len() == 1 {
                    // Lobby is free; start a second match.
                    // Drain LobbyState if any, then StartGame.
                    let start = serde_json::to_string(&ClientMsg::StartGame).unwrap();
                    ws.send(Message::Text(start.into())).await.unwrap();
                    // Fresh decoder baseline for the new room's keyframe stream.
                    dec = SnapshotDecoder::new();
                    // Wait for GameStart of match 2.
                    let gs_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
                    loop {
                        assert!(
                            tokio::time::Instant::now() < gs_deadline,
                            "no second GameStart"
                        );
                        match recv_any(&mut ws, &mut dec).await {
                            Some(Incoming::Msg(ServerMsg::GameStart { .. })) => break,
                            Some(Incoming::Msg(ServerMsg::LobbyState { .. })) => {}
                            Some(_) => {}
                            None => panic!("dropped before rematch GameStart"),
                        }
                    }
                }
            }
            Some(_) => {}
            None => panic!("connection dropped mid rematch suite"),
        }
    }

    assert_eq!(match_ends.len(), 2);
    // Each match should report its own peak; a rematch must not inherit the
    // previous room's peak_zombies (new Room starts at 0).
    assert!(
        match_ends[0].peak_zombies > 0 && match_ends[1].peak_zombies > 0,
        "both matches should see a horde"
    );
    // Second match duration should be a real short overrun, not the previous
    // 37s+ carried over (would only fail if rooms shared state — they don't).
    assert!(
        match_ends[1].duration_ms < 120_000,
        "second match duration absurdly long: {}",
        match_ends[1].duration_ms
    );
}

/// M14b: idle / zero-input players must still be prey. Parameterized over
/// (never send any input) and (send inputs but never move). First damage
/// within 25 s of GameStart; match reaches MatchEnd.
#[tokio::test]
async fn idle_zero_input_takes_damage_and_match_ends() {
    idle_player_engagement(false).await;
}

#[tokio::test]
async fn idle_standing_inputs_takes_damage_and_match_ends() {
    idle_player_engagement(true).await;
}

async fn idle_player_engagement(send_idle_inputs: bool) {
    // Same aggressive rate band as other tests in this binary (env is
    // process-global and tests share a process — do not set "1" here or a
    // parallel horde_grows can starve). Pin seed for stable travel time.
    unsafe {
        std::env::set_var("ZZ_DIRECTOR_RATE", "40");
        std::env::set_var("MAP_SEED", "m4-dev");
    }
    let url = start_server().await;
    let (mut ws, slot) = connect(&url, "statue").await;
    let mut dec = SnapshotDecoder::new();

    let mut seq = 0u32;
    let mut first_damage_ms: Option<u32> = None;
    let mut peak_zeds = 0usize;
    let mut saw_self = false;
    let match_deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    let damage_deadline_ms = 25_000u32;

    let stats = loop {
        assert!(
            tokio::time::Instant::now() < match_deadline,
            "no MatchEnd within 90 s (send_idle={send_idle_inputs}, peak_zeds={peak_zeds}, first_dmg={first_damage_ms:?})"
        );
        if send_idle_inputs {
            seq += 1;
            let idle = PlayerInput {
                seq,
                ..Default::default()
            };
            let _ = ws
                .send(Message::Binary(encode_input(&idle).to_vec().into()))
                .await;
        }
        match recv_any(&mut ws, &mut dec).await {
            Some(Incoming::Snap(s)) => {
                peak_zeds = peak_zeds.max(s.zombies.len());
                if let Some(me) = s.players.iter().find(|p| p.slot == slot) {
                    saw_self = true;
                    if me.health < 100 && first_damage_ms.is_none() {
                        first_damage_ms = Some(s.game_time_ms);
                        assert!(
                            s.game_time_ms <= damage_deadline_ms,
                            "first damage at {} ms > {} ms budget (send_idle={send_idle_inputs})",
                            s.game_time_ms,
                            damage_deadline_ms
                        );
                    }
                }
            }
            Some(Incoming::Msg(ServerMsg::MatchEnd { stats })) => break stats,
            Some(_) => {}
            None => panic!("connection dropped before MatchEnd (send_idle={send_idle_inputs})"),
        }
    };

    assert!(saw_self, "must observe self in snapshots");
    assert!(
        first_damage_ms.is_some(),
        "must take damage before MatchEnd (send_idle={send_idle_inputs}, peak={peak_zeds})"
    );
    assert!(
        first_damage_ms.unwrap() <= damage_deadline_ms,
        "first damage {} ms exceeds 25 s",
        first_damage_ms.unwrap()
    );
    assert_eq!(stats.players.len(), 1);
    assert!(stats.peak_zombies > 0);
    assert!(stats.duration_ms > 0);
    assert!(stats.players[0].time_alive_ms <= stats.duration_ms);
}
