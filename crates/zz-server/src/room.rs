//! One game room: authoritative simulation at a fixed tick rate, snapshots
//! out. Plain data + a plain loop (the Match.ts shape, in Rust): a Vec of
//! players ticked by a tokio interval. All game timing is TICK-COUNTED, not
//! wall clock — pausing is "don't advance the tick" and everything freezes.

use tokio::sync::mpsc;
use zz_core::constants::*;
use zz_core::movement::step_body;
use zz_core::protocol::{RosterPlayer, ServerMsg};
use zz_core::snapshot::{
    Snapshot, SnapshotEncoder, WirePlayer, quant_pitch, quant_pos3, quant_yaw16,
};
use zz_core::types::{Aabb, Body, PlayerInput};

/// What a connection can ask of a room.
pub enum RoomCmd {
    Join {
        conn_id: u64,
        name: String,
        tx: mpsc::Sender<OutMsg>,
    },
    Leave {
        conn_id: u64,
    },
    Input {
        conn_id: u64,
        input: PlayerInput,
    },
}

/// What the room pushes back through a connection's outbox.
#[derive(Clone)]
pub enum OutMsg {
    Json(String),
    Bin(Vec<u8>),
}

struct RoomPlayer {
    conn_id: u64,
    slot: u8,
    name: String,
    tx: mpsc::Sender<OutMsg>,
    body: Body,
    yaw: f32,
    pitch: f32,
    health: u8,
    alive: bool,
    kills: u16,
    ammo_mag: u8,
    ammo_reserve: u8,
    grenades: u8,
    /// Sequence-gated input QUEUE: every accepted input is simulated exactly
    /// once, so client-side prediction + replay reconciliation can match the
    /// server bit-for-bit even when inputs arrive in bursts. Bounded; the
    /// conn-level flood cap (90/s = 3× tick rate) bounds the drain work.
    pending: std::collections::VecDeque<PlayerInput>,
    last_seq: u32,
}

/// Max queued inputs applied per tick (catch-up headroom for bursty arrival
/// without letting a backlog fast-forward a player).
const INPUTS_PER_TICK_MAX: usize = 3;
/// Queue bound; beyond this the oldest inputs drop (the client will re-predict).
const INPUT_QUEUE_MAX: usize = 8;

impl RoomPlayer {
    fn send_json(&self, msg: &ServerMsg) {
        if let Ok(s) = serde_json::to_string(msg) {
            let _ = self.tx.try_send(OutMsg::Json(s));
        }
    }
}

pub struct Room {
    cmds: mpsc::Receiver<RoomCmd>,
    players: Vec<RoomPlayer>,
    walls: Vec<Aabb>,
    arena_half: f32,
    map_seed: String,
    tick: u32,
    encoder: SnapshotEncoder,
}

/// Placeholder map until the mapgen port lands: flat ground, a perimeter,
/// and a few boxes to collide with. Same Aabb vocabulary as the real thing.
pub fn test_map() -> (Vec<Aabb>, f32) {
    let h = ARENA_HALF as f32;
    let t = 0.6;
    let wall_h = 6.0;
    let walls = vec![
        Aabb {
            x0: -h - t,
            x1: h + t,
            y0: 0.0,
            y1: wall_h,
            z0: -h - t,
            z1: -h,
        },
        Aabb {
            x0: -h - t,
            x1: h + t,
            y0: 0.0,
            y1: wall_h,
            z0: h,
            z1: h + t,
        },
        Aabb {
            x0: -h - t,
            x1: -h,
            y0: 0.0,
            y1: wall_h,
            z0: -h,
            z1: h,
        },
        Aabb {
            x0: h,
            x1: h + t,
            y0: 0.0,
            y1: wall_h,
            z0: -h,
            z1: h,
        },
        // street furniture to bump into / climb
        Aabb {
            x0: 3.0,
            x1: 5.0,
            y0: 0.0,
            y1: 1.2,
            z0: -6.0,
            z1: -4.0,
        },
        Aabb {
            x0: -8.0,
            x1: -6.5,
            y0: 0.0,
            y1: 0.5,
            z0: 2.0,
            z1: 3.5,
        },
    ];
    (walls, h)
}

impl Room {
    pub fn spawn(map_seed: String, walls: Vec<Aabb>, arena_half: f32) -> mpsc::Sender<RoomCmd> {
        let (tx, rx) = mpsc::channel(256);
        let room = Room {
            cmds: rx,
            players: Vec::new(),
            walls,
            arena_half,
            map_seed,
            tick: 0,
            encoder: SnapshotEncoder::new(),
        };
        tokio::spawn(room.run());
        tx
    }

    async fn run(mut self) {
        let mut ticker = tokio::time::interval(std::time::Duration::from_micros(
            1_000_000 / TICK_RATE as u64,
        ));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            self.drain_cmds();
            self.step();
        }
    }

    fn drain_cmds(&mut self) {
        while let Ok(cmd) = self.cmds.try_recv() {
            match cmd {
                RoomCmd::Join { conn_id, name, tx } => self.join(conn_id, name, tx),
                RoomCmd::Leave { conn_id } => self.leave(conn_id),
                RoomCmd::Input { conn_id, input } => {
                    if let Some(p) = self.players.iter_mut().find(|p| p.conn_id == conn_id) {
                        // sequence gate: stale/replayed inputs drop
                        let newest = p.pending.back().map_or(p.last_seq, |i| i.seq);
                        if input.seq > newest {
                            let mut input = input;
                            input.pitch = input.pitch.clamp(-MAX_PITCH, MAX_PITCH);
                            if p.pending.len() >= INPUT_QUEUE_MAX {
                                p.pending.pop_front();
                            }
                            p.pending.push_back(input);
                        }
                    }
                }
            }
        }
    }

    fn join(&mut self, conn_id: u64, name: String, tx: mpsc::Sender<OutMsg>) {
        let used: Vec<u8> = self.players.iter().map(|p| p.slot).collect();
        let Some(slot) = (0..MAX_PLAYERS as u8).find(|s| !used.contains(s)) else {
            let _ = tx.try_send(OutMsg::Json(
                serde_json::to_string(&ServerMsg::Error {
                    message: "room full".into(),
                })
                .unwrap(),
            ));
            return;
        };
        // spawn corners, spread by slot
        let d = self.arena_half - 4.0;
        let spots = [(-d, -d), (d, d), (-d, d), (d, -d), (0.0, -d)];
        let (x, z) = spots[slot as usize % spots.len()];

        let player = RoomPlayer {
            conn_id,
            slot,
            name,
            tx,
            body: Body::at(x, z),
            yaw: 0.0,
            pitch: 0.0,
            health: MAX_HEALTH,
            alive: true,
            kills: 0,
            ammo_mag: MAG_SIZE,
            ammo_reserve: START_RESERVE_AMMO,
            grenades: START_GRENADES,
            pending: std::collections::VecDeque::new(),
            last_seq: 0,
        };
        self.players.push(player);

        let roster: Vec<RosterPlayer> = self
            .players
            .iter()
            .map(|p| RosterPlayer {
                slot: p.slot,
                id: format!("c{}", p.conn_id),
                name: p.name.clone(),
            })
            .collect();
        let joined = self.players.last().unwrap();
        joined.send_json(&ServerMsg::GameStart {
            map_seed: self.map_seed.clone(),
            env: zz_core::types::EnvKind::Urban,
            your_slot: slot,
            players: roster,
        });
        // a fresh stream needs a keyframe to sync onto
        self.encoder.force_keyframe();
    }

    fn leave(&mut self, conn_id: u64) {
        if let Some(i) = self.players.iter().position(|p| p.conn_id == conn_id) {
            let gone = self.players.remove(i);
            let msg = ServerMsg::PlayerLeft {
                id: format!("c{}", gone.conn_id),
            };
            for p in &self.players {
                p.send_json(&msg);
            }
        }
    }

    fn step(&mut self) {
        self.tick += 1;

        // Apply queued inputs — each exactly once (up to a small per-tick cap),
        // so the applied-step count equals the accepted-input count and the
        // client's replayed prediction can match the server exactly.
        for p in self.players.iter_mut() {
            if !p.alive {
                p.pending.clear();
                continue;
            }
            for _ in 0..INPUTS_PER_TICK_MAX {
                let Some(input) = p.pending.pop_front() else {
                    break;
                };
                p.last_seq = input.seq;
                p.yaw = input.yaw;
                p.pitch = input.pitch;
                step_body(
                    &mut p.body,
                    &input,
                    TICK_DT,
                    PLAYER_SPEED,
                    &self.walls,
                    self.arena_half,
                );
            }
        }

        // snapshot: encode once, broadcast to everyone; drop slow consumers
        let include_entities = self.tick.is_multiple_of(SNAPSHOT_ZOMBIE_EVERY);
        let snap = self.snapshot();
        let frame = self.encoder.encode(&snap, include_entities);
        let mut dropped: Vec<u64> = Vec::new();
        for p in &self.players {
            if p.tx.try_send(OutMsg::Bin(frame.clone())).is_err() {
                dropped.push(p.conn_id);
            }
        }
        for id in dropped {
            self.leave(id);
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            tick: self.tick,
            game_time_ms: (self.tick as u64 * 1000 / TICK_RATE as u64) as u32,
            difficulty: 0,
            paused: false,
            players: self
                .players
                .iter()
                .map(|p| WirePlayer {
                    slot: p.slot,
                    pos: quant_pos3(p.body.x, p.body.y, p.body.z),
                    yaw: quant_yaw16(p.yaw),
                    pitch: quant_pitch(p.pitch),
                    health: p.health,
                    ammo_mag: p.ammo_mag,
                    ammo_reserve: p.ammo_reserve,
                    grenades: p.grenades,
                    kills: p.kills,
                    alive: p.alive,
                    last_acked_seq: p.last_seq,
                })
                .collect(),
            zombies: Vec::new(),
            loot: Vec::new(),
            grenades: Vec::new(),
            shots: Vec::new(),
            booms: Vec::new(),
        }
    }
}
