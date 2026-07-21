//! One game room: the authoritative co-op survival simulation at 30 TPS.
//! Plain data + a plain loop (Match.ts's shape in Rust). All game timing is
//! TICK-COUNTED: pausing simply stops advancing the sim tick and everything
//! freezes; a separate wire tick keeps the snapshot stream monotonic.

mod combat;
mod director;
mod zombies;

use combat::{Grenade, explosion_damage, fire_hitscan};
use director::Director;
use tokio::sync::mpsc;
use zombies::{FlowField, SpatialHash, Zombie};
use zz_core::constants::*;
use zz_core::map::{GameMap, WalkGrid, generate_map};
use zz_core::movement::step_body;
use zz_core::protocol::{
    MatchStats, PlayerStats, RosterPlayer, ServerMsg, decode_voice, encode_voice,
};
use zz_core::snapshot::{
    Snapshot, SnapshotEncoder, WireBoom, WireGrenade, WireLoot, WirePlayer, WireShot, WireZombie,
    quant_pitch, quant_pos3, quant_yaw8, quant_yaw16,
};
use zz_core::types::{Body, EnvKind, PlayerInput};

use crate::lobby::LobbyCmd;

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
    /// Raw `BIN_VOICE` frame from the wire; room validates, rewrites slot, relays.
    Voice {
        conn_id: u64,
        frame: Vec<u8>,
    },
    Pause {
        conn_id: u64,
    },
    Resume {
        conn_id: u64,
    },
}

/// Pure helper: indices of other *live* players within [`CHAT_PROXIMITY_RADIUS`]
/// (3D euclidean) of `sender`. `positions` is parallel to the room player list:
/// `(x, y, z, alive)`. Sender is never included.
fn voice_recipients(sender: usize, positions: &[(f32, f32, f32, bool)]) -> Vec<usize> {
    if sender >= positions.len() {
        return Vec::new();
    }
    let (sx, sy, sz, _) = positions[sender];
    let r2 = CHAT_PROXIMITY_RADIUS * CHAT_PROXIMITY_RADIUS;
    positions
        .iter()
        .enumerate()
        .filter_map(|(i, &(x, y, z, alive))| {
            if i == sender || !alive {
                return None;
            }
            let dx = x - sx;
            let dy = y - sy;
            let dz = z - sz;
            if dx * dx + dy * dy + dz * dz <= r2 {
                Some(i)
            } else {
                None
            }
        })
        .collect()
}

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
    health: f32,
    alive: bool,
    kills: u16,
    ammo_mag: u8,
    ammo_reserve: u8,
    grenades: u8,
    fire_cooldown_left: u32,
    reload_left: u32,
    prev_grenade_held: bool,
    // input queue: sequence-gated, each accepted input simulated exactly once
    pending: std::collections::VecDeque<PlayerInput>,
    last_seq: u32,
    // stats
    shots_fired: u32,
    hits: u32,
    damage_dealt: f32,
    grenades_thrown: u32,
    death_sim_tick: Option<u32>,
}

const INPUTS_PER_TICK_MAX: usize = 3;
const INPUT_QUEUE_MAX: usize = 8;

impl RoomPlayer {
    fn send_json(&self, msg: &ServerMsg) {
        if let Ok(s) = serde_json::to_string(msg) {
            let _ = self.tx.try_send(OutMsg::Json(s));
        }
    }
    fn eye(&self) -> (f32, f32, f32) {
        (self.body.x, self.body.y + PLAYER_EYE, self.body.z)
    }
}

struct LootItem {
    id: u16,
    kind: u8, // 0 ammo, 1 health, 2 grenade
    x: f32,
    y: f32,
    z: f32,
    despawn_at_sim_tick: u32,
}

pub struct Room {
    cmds: mpsc::Receiver<RoomCmd>,
    players: Vec<RoomPlayer>,
    map: GameMap,
    grid: WalkGrid,
    flow: FlowField,
    hash: SpatialHash,
    zombies: Vec<Zombie>,
    grenades: Vec<Grenade>,
    loot: Vec<LootItem>,
    director: Director,
    loot_rng: zz_core::rng::Mulberry32,
    /// Advances every loop iteration — the snapshot stream sequence.
    wire_tick: u32,
    /// Advances only while unpaused and not ended — game time.
    sim_tick: u32,
    paused: bool,
    ended: bool,
    next_loot_id: u16,
    next_grenade_id: u8,
    shots: Vec<WireShot>,
    booms: Vec<WireBoom>,
    zombies_killed: u32,
    peak_zombies: u32,
    encoder: SnapshotEncoder,
    lobby_code: String,
    lobby_tx: mpsc::Sender<LobbyCmd>,
    /// wire tick at which the match ended; the room drains and exits shortly
    /// after so clients can catch the stats broadcast.
    ended_at: Option<u32>,
    saw_players: bool,
}

impl Room {
    pub fn spawn(
        env: EnvKind,
        map_seed: String,
        lobby_code: String,
        lobby_tx: mpsc::Sender<LobbyCmd>,
    ) -> mpsc::Sender<RoomCmd> {
        let (tx, rx) = mpsc::channel(256);
        // MAP_SEED pins maps for tests/ops (ShotAnte trick)
        let map_seed = std::env::var("MAP_SEED").unwrap_or(map_seed);
        let map = generate_map(env, &map_seed);
        let grid = WalkGrid::rasterize(&map.walls, map.arena_half);
        let loot_rng = zz_core::rng::Mulberry32::from_seed(&format!("{map_seed}|loot"));
        let director = Director::new(&map_seed);
        let room = Room {
            cmds: rx,
            players: Vec::new(),
            grid,
            flow: FlowField::empty(),
            hash: SpatialHash::new(),
            zombies: Vec::new(),
            grenades: Vec::new(),
            loot: Vec::new(),
            director,
            loot_rng,
            map,
            wire_tick: 0,
            sim_tick: 0,
            paused: false,
            ended: false,
            next_loot_id: 1,
            next_grenade_id: 1,
            shots: Vec::new(),
            booms: Vec::new(),
            zombies_killed: 0,
            peak_zombies: 0,
            encoder: SnapshotEncoder::new(),
            lobby_code,
            lobby_tx,
            ended_at: None,
            saw_players: false,
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
            // exit: match over and stats had ~5 s to flush, or everyone left
            let grace_over = self
                .ended_at
                .is_some_and(|t| self.wire_tick.saturating_sub(t) > TICK_RATE * 5);
            let abandoned = self.saw_players && self.players.is_empty();
            if grace_over || abandoned {
                if self.ended_at.is_none() {
                    // abandoned mid-match: still free the lobby for a rematch
                    let _ = self.lobby_tx.try_send(LobbyCmd::MatchEnded {
                        code: self.lobby_code.clone(),
                    });
                }
                break;
            }
        }
    }

    fn drain_cmds(&mut self) {
        while let Ok(cmd) = self.cmds.try_recv() {
            match cmd {
                RoomCmd::Join { conn_id, name, tx } => self.join(conn_id, name, tx),
                RoomCmd::Leave { conn_id } => self.leave(conn_id),
                RoomCmd::Input { conn_id, input } => {
                    if let Some(p) = self.players.iter_mut().find(|p| p.conn_id == conn_id) {
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
                RoomCmd::Voice { conn_id, frame } => self.relay_voice(conn_id, frame),
                RoomCmd::Pause { conn_id } => {
                    if !self.paused
                        && !self.ended
                        && let Some(p) = self.players.iter().find(|p| p.conn_id == conn_id)
                    {
                        self.paused = true;
                        let by = p.name.clone();
                        self.broadcast_json(&ServerMsg::Paused { by });
                    }
                }
                RoomCmd::Resume { conn_id } => {
                    if self.paused && self.players.iter().any(|p| p.conn_id == conn_id) {
                        self.paused = false;
                        self.broadcast_json(&ServerMsg::Resumed);
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
        let s = self.map.spawns[slot as usize % self.map.spawns.len()];
        let player = RoomPlayer {
            conn_id,
            slot,
            name,
            tx,
            body: Body::at(s.x, s.z),
            yaw: s.yaw,
            pitch: 0.0,
            health: MAX_HEALTH as f32,
            alive: true,
            kills: 0,
            ammo_mag: MAG_SIZE,
            ammo_reserve: START_RESERVE_AMMO,
            grenades: START_GRENADES,
            fire_cooldown_left: 0,
            reload_left: 0,
            prev_grenade_held: false,
            pending: std::collections::VecDeque::new(),
            last_seq: 0,
            shots_fired: 0,
            hits: 0,
            damage_dealt: 0.0,
            grenades_thrown: 0,
            death_sim_tick: None,
        };
        self.players.push(player);
        self.saw_players = true;

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
            map_seed: self.map.seed.clone(),
            env: self.map.env,
            your_slot: slot,
            players: roster,
        });
        self.encoder.force_keyframe();
    }

    fn leave(&mut self, conn_id: u64) {
        if let Some(i) = self.players.iter().position(|p| p.conn_id == conn_id) {
            let gone = self.players.remove(i);
            let msg = ServerMsg::PlayerLeft {
                id: format!("c{}", gone.conn_id),
            };
            self.broadcast_json(&msg);
        }
    }

    /// Validate a client voice frame, rewrite the speaker slot with the
    /// sender's authoritative slot (anti-spoof), and non-blockingly fan out
    /// to in-range live teammates. Malformed / oversized frames drop silently.
    fn relay_voice(&mut self, conn_id: u64, frame: Vec<u8>) {
        let Some((_spoofed_slot, pcm)) = decode_voice(&frame) else {
            // bad tag / short / empty / oversized — drop, keep the connection
            return;
        };
        let Some(sender_idx) = self.players.iter().position(|p| p.conn_id == conn_id) else {
            return;
        };
        let auth_slot = self.players[sender_idx].slot;
        let Some(out) = encode_voice(auth_slot, pcm) else {
            return;
        };
        let positions: Vec<(f32, f32, f32, bool)> = self
            .players
            .iter()
            .map(|p| (p.body.x, p.body.y, p.body.z, p.alive))
            .collect();
        for i in voice_recipients(sender_idx, &positions) {
            // same non-blocking path as snapshots — never stall the tick
            let _ = self.players[i].tx.try_send(OutMsg::Bin(out.clone()));
        }
    }

    fn step(&mut self) {
        self.wire_tick += 1;

        // After MatchEnd: keep the task alive only for the grace timer so the
        // lobby can rematch. Do NOT keep broadcasting snapshots — clients
        // share the same OutMsg channel with the next room, and post-end
        // frames corrupt the decoder + leave a stale horde on screen.
        if self.ended {
            return;
        }

        if !self.paused && !self.players.is_empty() {
            self.sim_tick += 1;
            self.sim_step();
        }

        // Snapshot: encode once, broadcast. If a client's outbox is full
        // (slow wasm tab under horde load), DROP THE FRAME — do not kick.
        // M15 thrash filled the 64-slot channel in ~2 s at 10 fps and leave()
        // froze the solo player with last-known 100 HP while zeds piled up.
        // Keyframes resync decoders after dropped deltas.
        let include_entities = self.wire_tick.is_multiple_of(SNAPSHOT_ZOMBIE_EVERY);
        let snap = self.snapshot();
        let frame = self.encoder.encode(&snap, include_entities);
        for p in &self.players {
            let _ = p.tx.try_send(OutMsg::Bin(frame.clone()));
        }
        self.shots.clear();
        self.booms.clear();
    }

    fn sim_step(&mut self) {
        let now = self.sim_tick;

        // ── players: cooldowns tick down, then queued inputs apply ─────────
        for i in 0..self.players.len() {
            let p = &mut self.players[i];
            if p.fire_cooldown_left > 0 {
                p.fire_cooldown_left -= 1;
            }
            if p.reload_left > 0 {
                p.reload_left -= 1;
                if p.reload_left == 0 {
                    let take = (MAG_SIZE - p.ammo_mag).min(p.ammo_reserve);
                    p.ammo_mag += take;
                    p.ammo_reserve -= take;
                }
            }
            if !p.alive {
                p.pending.clear();
                continue;
            }
            for _ in 0..INPUTS_PER_TICK_MAX {
                let Some(input) = self.players[i].pending.pop_front() else {
                    break;
                };
                self.apply_input(i, &input);
            }
        }

        // ── grenades ───────────────────────────────────────────────────────
        let mut exploded: Vec<Grenade> = Vec::new();
        let mut g_idx = 0;
        while g_idx < self.grenades.len() {
            let done = self.grenades[g_idx].step(&self.map.walls, self.map.arena_half);
            if done {
                exploded.push(self.grenades.swap_remove(g_idx));
            } else {
                g_idx += 1;
            }
        }
        for g in exploded {
            self.explode(&g, now);
        }

        // ── director + zombies ─────────────────────────────────────────────
        let player_views: Vec<(f32, f32, f32, f32, bool)> = self
            .players
            .iter()
            .map(|p| (p.body.x, p.body.y + PLAYER_EYE, p.body.z, p.yaw, p.alive))
            .collect();
        self.director.step(
            now,
            &self.map,
            &self.grid,
            &mut self.zombies,
            &player_views,
        );
        self.peak_zombies = self.peak_zombies.max(self.zombies.len() as u32);

        // Rebuild on the periodic cadence, and immediately when the field is
        // still empty (first ticks after join) so zombies path to idle prey
        // without waiting FLOWFIELD_REBUILD_TICKS.
        if now.is_multiple_of(FLOWFIELD_REBUILD_TICKS) || self.flow.is_empty() {
            let alive_pos: Vec<(f32, f32)> = self
                .players
                .iter()
                .filter(|p| p.alive)
                .map(|p| (p.body.x, p.body.z))
                .collect();
            self.flow = FlowField::rebuild(&self.grid, &alive_pos);
        }

        let positions: Vec<(f32, f32)> =
            self.zombies.iter().map(|z| (z.body.x, z.body.z)).collect();
        self.hash.rebuild(positions.iter().copied());
        let player_flat: Vec<(u8, f32, f32, bool)> = self
            .players
            .iter()
            .map(|p| (p.slot, p.body.x, p.body.z, p.alive))
            .collect();

        let mut bites: Vec<(u8, f32)> = Vec::new();
        for (zi, z) in self.zombies.iter_mut().enumerate() {
            let sep = self.hash.separation(zi, z.body.x, z.body.z, &positions);
            if let Some(bite) = zombies::step_zombie(
                z,
                &self.grid,
                &self.flow,
                &player_flat,
                sep,
                &self.map.walls,
                self.map.arena_half,
            ) {
                bites.push(bite);
            }
        }
        for (slot, dmg) in bites {
            self.damage_player(slot, dmg, now);
        }

        // ── loot: expiry + pickup ──────────────────────────────────────────
        self.loot.retain(|l| l.despawn_at_sim_tick > now);
        let mut taken: Vec<u16> = Vec::new();
        for p in self.players.iter_mut().filter(|p| p.alive) {
            for l in &self.loot {
                if taken.contains(&l.id) {
                    continue;
                }
                let d2 = (p.body.x - l.x).powi(2) + (p.body.z - l.z).powi(2);
                if d2 > PICKUP_RADIUS * PICKUP_RADIUS {
                    continue;
                }
                let granted = match l.kind {
                    0 if p.ammo_reserve < u8::MAX - LOOT_AMMO_AMOUNT => {
                        p.ammo_reserve += LOOT_AMMO_AMOUNT;
                        true
                    }
                    1 if p.health < MAX_HEALTH as f32 => {
                        p.health = (p.health + LOOT_HEAL_AMOUNT as f32).min(MAX_HEALTH as f32);
                        true
                    }
                    2 if p.grenades < MAX_GRENADES => {
                        p.grenades += 1;
                        true
                    }
                    _ => false,
                };
                if granted {
                    taken.push(l.id);
                }
            }
        }
        self.loot.retain(|l| !taken.contains(&l.id));

        // ── end condition: everyone dead ───────────────────────────────────
        if !self.players.is_empty() && self.players.iter().all(|p| !p.alive) {
            self.finish(now);
        }
    }

    fn apply_input(&mut self, i: usize, input: &PlayerInput) {
        {
            let walls = &self.map.walls;
            let arena_half = self.map.arena_half;
            let p = &mut self.players[i];
            p.last_seq = input.seq;
            p.yaw = input.yaw;
            p.pitch = input.pitch;
            step_body(&mut p.body, input, TICK_DT, PLAYER_SPEED, walls, arena_half);
        }

        // grenade throw: rising edge only (held button ≠ grenade hose)
        let throw = {
            let p = &mut self.players[i];
            let edge = input.grenade && !p.prev_grenade_held;
            p.prev_grenade_held = input.grenade;
            edge && p.grenades > 0
        };
        if throw {
            let (id, slot, eye, yaw, pitch) = {
                let p = &mut self.players[i];
                p.grenades -= 1;
                p.grenades_thrown += 1;
                (self.next_grenade_id, p.slot, p.eye(), p.yaw, p.pitch)
            };
            self.next_grenade_id = self.next_grenade_id.wrapping_add(1).max(1);
            self.grenades
                .push(Grenade::thrown(id, slot, eye, yaw, pitch));
        }

        // fire
        let can_fire = {
            let p = &self.players[i];
            input.fire && p.fire_cooldown_left == 0 && p.reload_left == 0 && p.ammo_mag > 0
        };
        if can_fire {
            let (slot, eye, yaw, pitch) = {
                let p = &mut self.players[i];
                p.ammo_mag -= 1;
                p.fire_cooldown_left = FIRE_COOLDOWN_TICKS;
                p.shots_fired += 1;
                (p.slot, p.eye(), p.yaw, p.pitch)
            };
            let result = fire_hitscan(eye, yaw, pitch, &self.zombies, &self.map.walls);
            let mut hit_kind = 0u8;
            if let Some(zi) = result.zombie_index {
                self.players[i].hits += 1;
                self.players[i].damage_dealt += result.damage;
                let died = {
                    let z = &mut self.zombies[zi];
                    z.health -= result.damage;
                    z.health <= 0.0
                };
                hit_kind = if died {
                    if result.headshot { 3 } else { 2 }
                } else {
                    1
                };
                if died {
                    self.kill_zombie(zi, slot);
                }
            }
            self.shots.push(WireShot {
                slot,
                end: quant_pos3(result.end.0, result.end.1, result.end.2),
                hit_kind,
            });
        }

        // auto-reload on empty (reserve permitting)
        {
            let p = &mut self.players[i];
            if p.ammo_mag == 0 && p.reload_left == 0 && p.ammo_reserve > 0 {
                p.reload_left = RELOAD_TICKS;
            }
        }
    }

    fn kill_zombie(&mut self, zi: usize, killer_slot: u8) {
        let z = self.zombies.swap_remove(zi);
        self.zombies_killed += 1;
        if let Some(p) = self.players.iter_mut().find(|p| p.slot == killer_slot) {
            p.kills += 1;
        }
        // drop roll (mutually exclusive bands)
        let roll = self.loot_rng.next();
        let kind = if roll < DROP_CHANCE_AMMO {
            Some(0u8)
        } else if roll < DROP_CHANCE_AMMO + DROP_CHANCE_HEALTH {
            Some(1)
        } else if roll < DROP_CHANCE_AMMO + DROP_CHANCE_HEALTH + DROP_CHANCE_GRENADE {
            Some(2)
        } else {
            None
        };
        if let Some(kind) = kind {
            self.loot.push(LootItem {
                id: self.next_loot_id,
                kind,
                x: z.body.x,
                y: z.body.y,
                z: z.body.z,
                despawn_at_sim_tick: self.sim_tick + LOOT_DESPAWN_TICKS,
            });
            self.next_loot_id = self.next_loot_id.wrapping_add(1).max(1);
        }
    }

    fn explode(&mut self, g: &Grenade, now: u32) {
        self.booms.push(WireBoom {
            pos: quant_pos3(g.x, g.y, g.z),
        });

        // zombies in radius (kill credit to the owner)
        let mut zi = 0;
        while zi < self.zombies.len() {
            let (dmg, died) = {
                let z = &mut self.zombies[zi];
                let d = ((z.body.x - g.x).powi(2)
                    + (z.body.y + 0.9 - g.y).powi(2)
                    + (z.body.z - g.z).powi(2))
                .sqrt();
                let dmg = explosion_damage(d);
                if dmg > 0.0 {
                    z.health -= dmg;
                }
                (dmg, z.health <= 0.0 && dmg > 0.0)
            };
            if dmg > 0.0
                && let Some(p) = self.players.iter_mut().find(|p| p.slot == g.owner_slot)
            {
                p.damage_dealt += dmg;
            }
            if died {
                self.kill_zombie(zi, g.owner_slot);
                continue; // swap_remove — recheck the same index
            }
            zi += 1;
        }

        // self-damage only (friendly fire OFF): your own carelessness hurts you
        let self_dmg = self
            .players
            .iter()
            .find(|p| p.slot == g.owner_slot && p.alive)
            .map(|p| {
                let d = ((p.body.x - g.x).powi(2)
                    + (p.body.y + 0.9 - g.y).powi(2)
                    + (p.body.z - g.z).powi(2))
                .sqrt();
                explosion_damage(d) * GRENADE_SELF_DAMAGE
            })
            .unwrap_or(0.0);
        if self_dmg > 0.0 {
            self.damage_player(g.owner_slot, self_dmg, now);
        }
    }

    fn damage_player(&mut self, slot: u8, dmg: f32, now: u32) {
        if let Some(p) = self.players.iter_mut().find(|p| p.slot == slot && p.alive) {
            p.health -= dmg;
            if p.health <= 0.0 {
                p.health = 0.0;
                p.alive = false;
                p.death_sim_tick = Some(now);
            }
        }
    }

    fn finish(&mut self, now: u32) {
        // Idempotent: the wipe check sits at the end of every sim_step while
        // anyone is dead; only the first call should broadcast stats.
        if self.ended {
            return;
        }
        self.ended = true;
        self.ended_at = Some(self.wire_tick);
        let _ = self.lobby_tx.try_send(LobbyCmd::MatchEnded {
            code: self.lobby_code.clone(),
        });
        // Match length is sim-time from tick 0 → `now` (pause does not advance
        // sim_tick). Clamp each player's time_alive so a bad death_sim_tick
        // can never report living longer than the match (NEXT.md item 7).
        let duration_ms = now as u64 * 1000 / TICK_RATE as u64;
        let stats = MatchStats {
            duration_ms,
            zombies_killed: self.zombies_killed,
            peak_zombies: self.peak_zombies,
            difficulty_reached: Director::difficulty(now),
            players: self
                .players
                .iter()
                .map(|p| {
                    let alive_tick = p.death_sim_tick.unwrap_or(now).min(now);
                    let time_alive_ms =
                        (alive_tick as u64 * 1000 / TICK_RATE as u64).min(duration_ms);
                    PlayerStats {
                        slot: p.slot,
                        name: p.name.clone(),
                        kills: p.kills as u32,
                        damage_dealt: p.damage_dealt as u32,
                        shots_fired: p.shots_fired,
                        hits: p.hits,
                        grenades_thrown: p.grenades_thrown,
                        time_alive_ms,
                    }
                })
                .collect(),
        };
        self.broadcast_json(&ServerMsg::MatchEnd { stats });
    }

    fn broadcast_json(&self, msg: &ServerMsg) {
        if let Ok(s) = serde_json::to_string(msg) {
            for p in &self.players {
                let _ = p.tx.try_send(OutMsg::Json(s.clone()));
            }
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            tick: self.wire_tick,
            game_time_ms: (self.sim_tick as u64 * 1000 / TICK_RATE as u64) as u32,
            difficulty: Director::difficulty(self.sim_tick),
            paused: self.paused,
            players: self
                .players
                .iter()
                .map(|p| WirePlayer {
                    slot: p.slot,
                    pos: quant_pos3(p.body.x, p.body.y, p.body.z),
                    yaw: quant_yaw16(p.yaw),
                    pitch: quant_pitch(p.pitch),
                    health: p.health.clamp(0.0, 255.0) as u8,
                    ammo_mag: p.ammo_mag,
                    ammo_reserve: p.ammo_reserve,
                    grenades: p.grenades,
                    kills: p.kills,
                    alive: p.alive,
                    last_acked_seq: p.last_seq,
                })
                .collect(),
            zombies: self
                .zombies
                .iter()
                .map(|z| WireZombie {
                    id: z.id,
                    kind: z.kind.wire(),
                    state: z.state,
                    pos: quant_pos3(z.body.x, z.body.y, z.body.z),
                    yaw: quant_yaw8(z.yaw),
                    // health as a 0..=255 fraction of the kind's max
                    health: ((z.health / z.kind.stats().1).clamp(0.0, 1.0) * 255.0) as u8,
                })
                .collect(),
            loot: self
                .loot
                .iter()
                .map(|l| WireLoot {
                    id: l.id,
                    kind: l.kind,
                    pos: quant_pos3(l.x, l.y, l.z),
                })
                .collect(),
            grenades: self
                .grenades
                .iter()
                .map(|g| WireGrenade {
                    id: g.id,
                    pos: quant_pos3(g.x, g.y, g.z),
                })
                .collect(),
            shots: self.shots.clone(),
            booms: self.booms.clone(),
        }
    }
}

#[cfg(test)]
mod voice_tests {
    use super::voice_recipients;
    use zz_core::constants::CHAT_PROXIMITY_RADIUS;

    #[test]
    fn voice_recipients_in_range_relayed() {
        // sender at origin; teammate 10 m away on x — inside 25 m radius
        let positions = [
            (0.0, 0.0, 0.0, true),
            (10.0, 0.0, 0.0, true),
        ];
        assert_eq!(voice_recipients(0, &positions), vec![1]);
        assert_eq!(voice_recipients(1, &positions), vec![0]);
    }

    #[test]
    fn voice_recipients_out_of_range_dropped() {
        let far = CHAT_PROXIMITY_RADIUS + 1.0;
        let positions = [
            (0.0, 0.0, 0.0, true),
            (far, 0.0, 0.0, true),
        ];
        assert!(voice_recipients(0, &positions).is_empty());
        assert!(voice_recipients(1, &positions).is_empty());
    }

    #[test]
    fn voice_recipients_sender_never_included() {
        // two others in range; sender must not appear in its own recipient list
        let positions = [
            (0.0, 0.0, 0.0, true),
            (1.0, 0.0, 0.0, true),
            (0.0, 0.0, 1.0, true),
        ];
        let recips = voice_recipients(0, &positions);
        assert_eq!(recips, vec![1, 2]);
        assert!(!recips.contains(&0));
    }

    #[test]
    fn voice_recipients_skips_dead_and_exact_radius() {
        let r = CHAT_PROXIMITY_RADIUS;
        let positions = [
            (0.0, 0.0, 0.0, true),
            (r, 0.0, 0.0, true),  // exactly on the radius — included (<=)
            (5.0, 0.0, 0.0, false), // dead — never
        ];
        assert_eq!(voice_recipients(0, &positions), vec![1]);
    }
}
