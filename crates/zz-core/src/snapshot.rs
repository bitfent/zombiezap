//! Binary wire codec for per-tick game snapshots, with delta compression.
//! Direct descendant of ShotAnte's snapshotCodec.ts, extended for hordes.
//!
//! Design (unchanged lessons): little-endian, keyframe + change-mask deltas,
//! periodic keyframes bound error and resync fresh streams; over reliable
//! ordered WebSocket the receiver's last-decoded snapshot IS the sender's
//! last-encoded one, so the baseline is implicit — no ack handshake. One
//! encoder per room (the same bytes broadcast to everyone), one decoder per
//! client.
//!
//! Extensions for ZombieZap: entity-id'd zombies with a changed-bitset and
//! i8 position deltas (i16 absolute fallback), a loot section that is only
//! present when it changed, and quantized integer state everywhere — the
//! codec operates on already-quantized values so round-trips are EXACT and
//! server/client can never disagree on what was sent.

use crate::types::Aabb;

/// Frame tag — first byte of every snapshot frame. Kept in sync with the
/// transport-level tags in `protocol` (BIN_SNAPSHOT).
const TAG_SNAPSHOT: u8 = 1;

// Header flags.
const F_KEYFRAME: u8 = 1;
const F_ZOMBIES: u8 = 2; // zombie section present this frame
const F_LOOT: u8 = 4; // loot section present this frame
const F_PAUSED: u8 = 8;

// Player delta mask bits (u16).
const PM_X: u16 = 1 << 0;
const PM_Y: u16 = 1 << 1;
const PM_Z: u16 = 1 << 2;
const PM_YAW: u16 = 1 << 3;
const PM_PITCH: u16 = 1 << 4;
const PM_HEALTH: u16 = 1 << 5;
const PM_MAG: u16 = 1 << 6;
const PM_RESERVE: u16 = 1 << 7;
const PM_GRENADES: u16 = 1 << 8;
const PM_KILLS: u16 = 1 << 9;
const PM_ALIVE: u16 = 1 << 10;
const PM_ACK: u16 = 1 << 11;

// Zombie delta mask bits (u8).
const ZM_POS_I8: u8 = 1 << 0; // dx,dy,dz as i8 (1/128 m units)
const ZM_POS_I16: u8 = 1 << 1; // absolute i16 fallback (big jump/teleport)
const ZM_YAW: u8 = 1 << 2;
const ZM_STATE: u8 = 1 << 3;
const ZM_HEALTH: u8 = 1 << 4;

/// Position quantization: 1/128 m (±256 m range, ~8 mm precision).
pub const POS_SCALE: f32 = 128.0;

pub fn quant_pos(v: f32) -> i16 {
    libm::roundf(v * POS_SCALE) as i16
}

pub fn dequant_pos(v: i16) -> f32 {
    v as f32 / POS_SCALE
}

/// Yaw quantization for players: full turn mapped to u16.
pub fn quant_yaw16(yaw: f32) -> u16 {
    let tau = core::f32::consts::TAU;
    let norm = yaw.rem_euclid(tau) / tau;
    (norm * 65536.0) as u32 as u16
}

pub fn dequant_yaw16(q: u16) -> f32 {
    q as f32 / 65536.0 * core::f32::consts::TAU
}

/// Coarse yaw for zombies: full turn mapped to u8 (~1.4° steps).
pub fn quant_yaw8(yaw: f32) -> u8 {
    let tau = core::f32::consts::TAU;
    let norm = yaw.rem_euclid(tau) / tau;
    (norm * 256.0) as u32 as u8
}

pub fn dequant_yaw8(q: u8) -> f32 {
    q as f32 / 256.0 * core::f32::consts::TAU
}

/// Pitch quantization: [-π/2, π/2] mapped to i8.
pub fn quant_pitch(pitch: f32) -> i8 {
    let half = core::f32::consts::FRAC_PI_2;
    libm::roundf((pitch / half).clamp(-1.0, 1.0) * 127.0) as i8
}

pub fn dequant_pitch(q: i8) -> f32 {
    q as f32 / 127.0 * core::f32::consts::FRAC_PI_2
}

// ── wire state ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WirePlayer {
    pub slot: u8,
    pub pos: [i16; 3],
    pub yaw: u16,
    pub pitch: i8,
    pub health: u8,
    pub ammo_mag: u8,
    pub ammo_reserve: u8,
    pub grenades: u8,
    pub kills: u16,
    pub alive: bool,
    /// Highest input sequence the server has applied for THIS player —
    /// drives client-side reconciliation (drop acked, replay the rest).
    pub last_acked_seq: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WireZombie {
    pub id: u16,
    pub kind: u8,
    pub state: u8,
    pub pos: [i16; 3],
    pub yaw: u8,
    pub health: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WireLoot {
    pub id: u16,
    pub kind: u8,
    pub pos: [i16; 3],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WireGrenade {
    pub id: u8,
    pub pos: [i16; 3],
}

/// Transient: a shot fired this window (tracer from the shooter to `end`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WireShot {
    pub slot: u8,
    pub end: [i16; 3],
    /// 0 = wall/miss, 1 = zombie hit, 2 = zombie killed, 3 = headshot kill.
    pub hit_kind: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WireBoom {
    pub pos: [i16; 3],
}

/// One tick's world state as it crosses the wire. All values pre-quantized;
/// zombie/loot vectors reflect the sections' PRESENCE rules (see encoder).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub tick: u32,
    pub game_time_ms: u32,
    pub difficulty: u8,
    pub paused: bool,
    pub players: Vec<WirePlayer>,
    pub zombies: Vec<WireZombie>,
    pub loot: Vec<WireLoot>,
    pub grenades: Vec<WireGrenade>,
    pub shots: Vec<WireShot>,
    pub booms: Vec<WireBoom>,
}

// ── byte reader/writer (never panics) ──────────────────────────────────────

struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn i8(&mut self, v: i8) {
        self.0.push(v as u8);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i16(&mut self, v: i16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn pos(&mut self, p: [i16; 3]) {
        for v in p {
            self.i16(v);
        }
    }
}

struct Reader<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Reader { buf, at: 0 }
    }
    fn u8(&mut self) -> Result<u8, CodecError> {
        let v = *self.buf.get(self.at).ok_or(CodecError::Truncated)?;
        self.at += 1;
        Ok(v)
    }
    fn i8(&mut self) -> Result<i8, CodecError> {
        Ok(self.u8()? as i8)
    }
    fn u16(&mut self) -> Result<u16, CodecError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn i16(&mut self) -> Result<i16, CodecError> {
        let b = self.take(2)?;
        Ok(i16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, CodecError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn pos(&mut self) -> Result<[i16; 3], CodecError> {
        Ok([self.i16()?, self.i16()?, self.i16()?])
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], CodecError> {
        let end = self.at.checked_add(n).ok_or(CodecError::Truncated)?;
        let s = self.buf.get(self.at..end).ok_or(CodecError::Truncated)?;
        self.at = end;
        Ok(s)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecError {
    Truncated,
    BadTag,
    /// Delta frame arrived with no baseline (fresh decoder) — need a keyframe.
    NoBaseline,
    /// Tick went backwards or the roster shape changed without a keyframe —
    /// stream is unusable, reconnect/resync.
    Desync,
}

// ── encoder ────────────────────────────────────────────────────────────────

/// Stateful per-room encoder. Holds the last snapshot as the delta baseline
/// and forces a keyframe periodically (and on demand for rejoiners).
#[derive(Default)]
pub struct SnapshotEncoder {
    baseline: Option<Snapshot>,
    since_key: u32,
}

impl SnapshotEncoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop the baseline so the next frame is a keyframe (join/rejoin resync).
    pub fn force_keyframe(&mut self) {
        self.baseline = None;
    }

    /// `include_entities`: zombie+loot sections ride only every Nth tick
    /// (SNAPSHOT_ZOMBIE_EVERY); pass false to skip them on off ticks. On a
    /// keyframe they are always included regardless.
    pub fn encode(&mut self, s: &Snapshot, include_entities: bool) -> Vec<u8> {
        let key = self.baseline.is_none() || self.since_key >= crate::constants::KEYFRAME_EVERY;
        let base = self.baseline.take();

        let mut w = Writer(Vec::with_capacity(256));
        w.u8(TAG_SNAPSHOT);

        let zombies_present = key || include_entities;
        // Loot: present only when changed (or keyframe/entity tick where it differs).
        let loot_changed = match &base {
            Some(b) => s.loot != b.loot,
            None => true,
        };
        let loot_present = key || (include_entities && loot_changed);

        let mut flags = 0u8;
        if key {
            flags |= F_KEYFRAME;
        }
        if zombies_present {
            flags |= F_ZOMBIES;
        }
        if loot_present {
            flags |= F_LOOT;
        }
        if s.paused {
            flags |= F_PAUSED;
        }
        w.u8(flags);
        w.u32(s.tick);
        w.u32(s.game_time_ms);
        w.u8(s.difficulty);

        // players
        w.u8(s.players.len() as u8);
        if key {
            for p in &s.players {
                Self::write_player_full(&mut w, p);
            }
        } else {
            let b = base.as_ref().expect("delta implies baseline");
            for (p, bp) in s.players.iter().zip(b.players.iter()) {
                Self::write_player_delta(&mut w, p, bp);
            }
        }

        // zombies
        if zombies_present {
            if key {
                w.u16(s.zombies.len() as u16);
                for z in &s.zombies {
                    Self::write_zombie_full(&mut w, z);
                }
            } else {
                let b = base.as_ref().expect("delta implies baseline");
                Self::write_zombie_delta(&mut w, &s.zombies, &b.zombies);
            }
        }

        // loot (full section when present — small and rarely changing)
        if loot_present {
            w.u16(s.loot.len() as u16);
            for l in &s.loot {
                w.u16(l.id);
                w.u8(l.kind);
                w.pos(l.pos);
            }
        }

        // grenades in flight — tiny, always full
        w.u8(s.grenades.len() as u8);
        for g in &s.grenades {
            w.u8(g.id);
            w.pos(g.pos);
        }

        // transient events — always full
        w.u8(s.shots.len() as u8);
        for sh in &s.shots {
            w.u8(sh.slot);
            w.pos(sh.end);
            w.u8(sh.hit_kind);
        }
        w.u8(s.booms.len() as u8);
        for bm in &s.booms {
            w.pos(bm.pos);
        }

        // The stored baseline must reflect what the DECODER will now hold:
        // sections that weren't sent keep their previous decoded value.
        let mut stored = s.clone();
        if let Some(b) = &base {
            if !zombies_present {
                stored.zombies = b.zombies.clone();
            }
            if !loot_present {
                stored.loot = b.loot.clone();
            }
        }
        self.baseline = Some(stored);
        self.since_key = if key { 0 } else { self.since_key + 1 };
        w.0
    }

    fn write_player_full(w: &mut Writer, p: &WirePlayer) {
        w.u8(p.slot);
        w.pos(p.pos);
        w.u16(p.yaw);
        w.i8(p.pitch);
        w.u8(p.health);
        w.u8(p.ammo_mag);
        w.u8(p.ammo_reserve);
        w.u8(p.grenades);
        w.u16(p.kills);
        w.u8(p.alive as u8);
        w.u32(p.last_acked_seq);
    }

    fn write_player_delta(w: &mut Writer, p: &WirePlayer, b: &WirePlayer) {
        let mut mask = 0u16;
        if p.pos[0] != b.pos[0] {
            mask |= PM_X;
        }
        if p.pos[1] != b.pos[1] {
            mask |= PM_Y;
        }
        if p.pos[2] != b.pos[2] {
            mask |= PM_Z;
        }
        if p.yaw != b.yaw {
            mask |= PM_YAW;
        }
        if p.pitch != b.pitch {
            mask |= PM_PITCH;
        }
        if p.health != b.health {
            mask |= PM_HEALTH;
        }
        if p.ammo_mag != b.ammo_mag {
            mask |= PM_MAG;
        }
        if p.ammo_reserve != b.ammo_reserve {
            mask |= PM_RESERVE;
        }
        if p.grenades != b.grenades {
            mask |= PM_GRENADES;
        }
        if p.kills != b.kills {
            mask |= PM_KILLS;
        }
        if p.alive != b.alive {
            mask |= PM_ALIVE;
        }
        if p.last_acked_seq != b.last_acked_seq {
            mask |= PM_ACK;
        }
        w.u16(mask);
        if mask & PM_X != 0 {
            w.i16(p.pos[0]);
        }
        if mask & PM_Y != 0 {
            w.i16(p.pos[1]);
        }
        if mask & PM_Z != 0 {
            w.i16(p.pos[2]);
        }
        if mask & PM_YAW != 0 {
            w.u16(p.yaw);
        }
        if mask & PM_PITCH != 0 {
            w.i8(p.pitch);
        }
        if mask & PM_HEALTH != 0 {
            w.u8(p.health);
        }
        if mask & PM_MAG != 0 {
            w.u8(p.ammo_mag);
        }
        if mask & PM_RESERVE != 0 {
            w.u8(p.ammo_reserve);
        }
        if mask & PM_GRENADES != 0 {
            w.u8(p.grenades);
        }
        if mask & PM_KILLS != 0 {
            w.u16(p.kills);
        }
        if mask & PM_ALIVE != 0 {
            w.u8(p.alive as u8);
        }
        if mask & PM_ACK != 0 {
            w.u32(p.last_acked_seq);
        }
    }

    fn write_zombie_full(w: &mut Writer, z: &WireZombie) {
        w.u16(z.id);
        w.u8(z.kind);
        w.u8(z.state);
        w.pos(z.pos);
        w.u8(z.yaw);
        w.u8(z.health);
    }

    /// Delta section: removed ids, added records, then a changed-bitset over
    /// the SURVIVORS (baseline order minus removed) with per-zombie masks.
    fn write_zombie_delta(w: &mut Writer, cur: &[WireZombie], base: &[WireZombie]) {
        // Index current by id. Ids are a wrapping u16 counter, unique among live.
        let cur_by_id = |id: u16| cur.iter().find(|z| z.id == id);

        let removed: Vec<u16> = base
            .iter()
            .map(|z| z.id)
            .filter(|id| cur_by_id(*id).is_none())
            .collect();
        let added: Vec<&WireZombie> = cur
            .iter()
            .filter(|z| !base.iter().any(|b| b.id == z.id))
            .collect();
        let survivors: Vec<(&WireZombie, &WireZombie)> = base
            .iter()
            .filter_map(|b| cur_by_id(b.id).map(|c| (c, b)))
            .collect();

        w.u16(removed.len() as u16);
        for id in &removed {
            w.u16(*id);
        }
        w.u16(added.len() as u16);
        for z in &added {
            Self::write_zombie_full(w, z);
        }

        // changed bitset over survivors, baseline order
        let mut bits = vec![0u8; survivors.len().div_ceil(8)];
        let changed: Vec<bool> = survivors.iter().map(|(c, b)| c != b).collect();
        for (i, ch) in changed.iter().enumerate() {
            if *ch {
                bits[i / 8] |= 1 << (i % 8);
            }
        }
        w.0.extend_from_slice(&bits);

        for (i, (c, b)) in survivors.iter().enumerate() {
            if !changed[i] {
                continue;
            }
            let dx = c.pos[0] as i32 - b.pos[0] as i32;
            let dy = c.pos[1] as i32 - b.pos[1] as i32;
            let dz = c.pos[2] as i32 - b.pos[2] as i32;
            let fits_i8 = [dx, dy, dz].iter().all(|d| (-128..=127).contains(d));
            let moved = dx != 0 || dy != 0 || dz != 0;
            let mut mask = 0u8;
            if moved {
                mask |= if fits_i8 { ZM_POS_I8 } else { ZM_POS_I16 };
            }
            if c.yaw != b.yaw {
                mask |= ZM_YAW;
            }
            if c.state != b.state {
                mask |= ZM_STATE;
            }
            if c.health != b.health {
                mask |= ZM_HEALTH;
            }
            w.u8(mask);
            if mask & ZM_POS_I8 != 0 {
                w.i8(dx as i8);
                w.i8(dy as i8);
                w.i8(dz as i8);
            } else if mask & ZM_POS_I16 != 0 {
                w.pos(c.pos);
            }
            if mask & ZM_YAW != 0 {
                w.u8(c.yaw);
            }
            if mask & ZM_STATE != 0 {
                w.u8(c.state);
            }
            if mask & ZM_HEALTH != 0 {
                w.u8(c.health);
            }
        }
    }
}

// ── decoder ────────────────────────────────────────────────────────────────

/// Stateful per-connection decoder. Reconstructs each snapshot from the
/// keyframe/delta against the previously decoded baseline.
#[derive(Default)]
pub struct SnapshotDecoder {
    baseline: Option<Snapshot>,
}

impl SnapshotDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn decode(&mut self, frame: &[u8]) -> Result<Snapshot, CodecError> {
        let mut r = Reader::new(frame);
        if r.u8()? != TAG_SNAPSHOT {
            return Err(CodecError::BadTag);
        }
        let flags = r.u8()?;
        let key = flags & F_KEYFRAME != 0;
        let tick = r.u32()?;
        let game_time_ms = r.u32()?;
        let difficulty = r.u8()?;

        if !key {
            match &self.baseline {
                None => return Err(CodecError::NoBaseline),
                Some(b) if tick <= b.tick => return Err(CodecError::Desync),
                _ => {}
            }
        }

        // players
        let pc = r.u8()? as usize;
        let players = if key {
            let mut v = Vec::with_capacity(pc);
            for _ in 0..pc {
                v.push(Self::read_player_full(&mut r)?);
            }
            v
        } else {
            let base = self.baseline.as_ref().expect("checked above");
            if pc != base.players.len() {
                return Err(CodecError::Desync);
            }
            let mut v = base.players.clone();
            for p in v.iter_mut() {
                Self::read_player_delta(&mut r, p)?;
            }
            v
        };

        // zombies
        let zombies = if flags & F_ZOMBIES != 0 {
            if key {
                let n = r.u16()? as usize;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(Self::read_zombie_full(&mut r)?);
                }
                v
            } else {
                let base = self.baseline.as_ref().expect("checked above");
                Self::read_zombie_delta(&mut r, &base.zombies)?
            }
        } else {
            self.baseline
                .as_ref()
                .map(|b| b.zombies.clone())
                .unwrap_or_default()
        };

        // loot
        let loot = if flags & F_LOOT != 0 {
            let n = r.u16()? as usize;
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(WireLoot {
                    id: r.u16()?,
                    kind: r.u8()?,
                    pos: r.pos()?,
                });
            }
            v
        } else {
            self.baseline
                .as_ref()
                .map(|b| b.loot.clone())
                .unwrap_or_default()
        };

        // grenades
        let gn = r.u8()? as usize;
        let mut grenades = Vec::with_capacity(gn);
        for _ in 0..gn {
            grenades.push(WireGrenade {
                id: r.u8()?,
                pos: r.pos()?,
            });
        }

        // events
        let sn = r.u8()? as usize;
        let mut shots = Vec::with_capacity(sn);
        for _ in 0..sn {
            shots.push(WireShot {
                slot: r.u8()?,
                end: r.pos()?,
                hit_kind: r.u8()?,
            });
        }
        let bn = r.u8()? as usize;
        let mut booms = Vec::with_capacity(bn);
        for _ in 0..bn {
            booms.push(WireBoom { pos: r.pos()? });
        }

        let snap = Snapshot {
            tick,
            game_time_ms,
            difficulty,
            paused: flags & F_PAUSED != 0,
            players,
            zombies,
            loot,
            grenades,
            shots,
            booms,
        };
        self.baseline = Some(snap.clone());
        Ok(snap)
    }

    fn read_player_full(r: &mut Reader) -> Result<WirePlayer, CodecError> {
        Ok(WirePlayer {
            slot: r.u8()?,
            pos: r.pos()?,
            yaw: r.u16()?,
            pitch: r.i8()?,
            health: r.u8()?,
            ammo_mag: r.u8()?,
            ammo_reserve: r.u8()?,
            grenades: r.u8()?,
            kills: r.u16()?,
            alive: r.u8()? != 0,
            last_acked_seq: r.u32()?,
        })
    }

    fn read_player_delta(r: &mut Reader, p: &mut WirePlayer) -> Result<(), CodecError> {
        let mask = r.u16()?;
        if mask & PM_X != 0 {
            p.pos[0] = r.i16()?;
        }
        if mask & PM_Y != 0 {
            p.pos[1] = r.i16()?;
        }
        if mask & PM_Z != 0 {
            p.pos[2] = r.i16()?;
        }
        if mask & PM_YAW != 0 {
            p.yaw = r.u16()?;
        }
        if mask & PM_PITCH != 0 {
            p.pitch = r.i8()?;
        }
        if mask & PM_HEALTH != 0 {
            p.health = r.u8()?;
        }
        if mask & PM_MAG != 0 {
            p.ammo_mag = r.u8()?;
        }
        if mask & PM_RESERVE != 0 {
            p.ammo_reserve = r.u8()?;
        }
        if mask & PM_GRENADES != 0 {
            p.grenades = r.u8()?;
        }
        if mask & PM_KILLS != 0 {
            p.kills = r.u16()?;
        }
        if mask & PM_ALIVE != 0 {
            p.alive = r.u8()? != 0;
        }
        if mask & PM_ACK != 0 {
            p.last_acked_seq = r.u32()?;
        }
        Ok(())
    }

    fn read_zombie_full(r: &mut Reader) -> Result<WireZombie, CodecError> {
        Ok(WireZombie {
            id: r.u16()?,
            kind: r.u8()?,
            state: r.u8()?,
            pos: r.pos()?,
            yaw: r.u8()?,
            health: r.u8()?,
        })
    }

    fn read_zombie_delta(
        r: &mut Reader,
        base: &[WireZombie],
    ) -> Result<Vec<WireZombie>, CodecError> {
        let rn = r.u16()? as usize;
        let mut removed = Vec::with_capacity(rn);
        for _ in 0..rn {
            removed.push(r.u16()?);
        }
        let an = r.u16()? as usize;
        let mut added = Vec::with_capacity(an);
        for _ in 0..an {
            added.push(Self::read_zombie_full(r)?);
        }

        let mut survivors: Vec<WireZombie> = base
            .iter()
            .filter(|z| !removed.contains(&z.id))
            .copied()
            .collect();

        let bits = r.take(survivors.len().div_ceil(8))?.to_vec();
        for (i, z) in survivors.iter_mut().enumerate() {
            if bits[i / 8] & (1 << (i % 8)) == 0 {
                continue;
            }
            let mask = r.u8()?;
            if mask & ZM_POS_I8 != 0 {
                let dx = r.i8()? as i32;
                let dy = r.i8()? as i32;
                let dz = r.i8()? as i32;
                z.pos[0] = (z.pos[0] as i32 + dx) as i16;
                z.pos[1] = (z.pos[1] as i32 + dy) as i16;
                z.pos[2] = (z.pos[2] as i32 + dz) as i16;
            } else if mask & ZM_POS_I16 != 0 {
                z.pos = r.pos()?;
            }
            if mask & ZM_YAW != 0 {
                z.yaw = r.u8()?;
            }
            if mask & ZM_STATE != 0 {
                z.state = r.u8()?;
            }
            if mask & ZM_HEALTH != 0 {
                z.health = r.u8()?;
            }
        }
        survivors.extend(added);
        Ok(survivors)
    }
}

/// Convenience: quantize a raw world position triple.
pub fn quant_pos3(x: f32, y: f32, z: f32) -> [i16; 3] {
    [quant_pos(x), quant_pos(y), quant_pos(z)]
}

/// Rasterization helper used by AABB-related tests elsewhere; kept here so the
/// codec module has no dependency on map generation.
pub fn aabb_center(b: &Aabb) -> [f32; 3] {
    [
        (b.x0 + b.x1) * 0.5,
        (b.y0 + b.y1) * 0.5,
        (b.z0 + b.z1) * 0.5,
    ]
}
