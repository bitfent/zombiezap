//! Snapshot codec: round-trips, delta chains, section carry, and the
//! never-panic guarantee under garbage/truncated input.

use zz_core::rng::Mulberry32;
use zz_core::snapshot::*;

fn player(slot: u8) -> WirePlayer {
    WirePlayer {
        slot,
        pos: [slot as i16 * 100, 0, -200],
        yaw: 1000 * slot as u16,
        pitch: 0,
        health: 100,
        ammo_mag: 12,
        ammo_reserve: 48,
        grenades: 2,
        kills: 0,
        alive: true,
        last_acked_seq: 0,
    }
}

fn zombie(id: u16) -> WireZombie {
    WireZombie {
        id,
        kind: 0,
        state: 0,
        pos: [id as i16 * 10, 0, 500],
        yaw: 0,
        health: 100,
    }
}

fn base_snapshot() -> Snapshot {
    Snapshot {
        tick: 1,
        game_time_ms: 33,
        difficulty: 0,
        paused: false,
        players: (0..5).map(player).collect(),
        zombies: (0..50).map(zombie).collect(),
        loot: vec![WireLoot {
            id: 1,
            kind: 0,
            pos: [0, 0, 0],
        }],
        grenades: vec![],
        shots: vec![],
        booms: vec![],
    }
}

#[test]
fn keyframe_round_trips_exactly() {
    let mut enc = SnapshotEncoder::new();
    let mut dec = SnapshotDecoder::new();
    let snap = base_snapshot();
    let frame = enc.encode(&snap, true);
    let out = dec.decode(&frame).expect("decodes");
    assert_eq!(out, snap);
}

#[test]
fn delta_chain_reconstructs_final_state() {
    let mut enc = SnapshotEncoder::new();
    let mut dec = SnapshotDecoder::new();
    let mut snap = base_snapshot();
    let mut rng = Mulberry32::from_seed("delta-chain");

    let first = enc.encode(&snap, true);
    dec.decode(&first).expect("keyframe");

    for step in 0..40u32 {
        snap.tick += 1;
        snap.game_time_ms += 33;
        // players drift
        for p in snap.players.iter_mut() {
            p.pos[0] += (rng.next() * 10.0) as i16 - 5;
            p.yaw = p.yaw.wrapping_add((rng.next() * 500.0) as u16);
            if step == 20 {
                p.health = p.health.saturating_sub(7);
            }
        }
        // zombies drift, die, spawn
        for z in snap.zombies.iter_mut() {
            z.pos[2] -= (rng.next() * 20.0) as i16; // approach
            if rng.next() < 0.1 {
                z.health = z.health.saturating_sub(34);
            }
        }
        if step % 7 == 0 && !snap.zombies.is_empty() {
            snap.zombies
                .remove((rng.next() * snap.zombies.len() as f64) as usize);
        }
        if step % 5 == 0 {
            snap.zombies.push(zombie(1000 + step as u16));
        }
        // loot appears/disappears occasionally
        if step == 13 {
            snap.loot.push(WireLoot {
                id: 2,
                kind: 1,
                pos: [100, 0, 100],
            });
        }
        if step == 29 {
            snap.loot.clear();
        }
        // entity sections only every 2nd tick, like the real server
        let include = step % 2 == 0;
        let frame = enc.encode(&snap, include);
        let out = dec.decode(&frame).expect("delta decodes");
        assert_eq!(out.players, snap.players, "players at step {step}");
        if include {
            assert_eq!(out.zombies, snap.zombies, "zombies at step {step}");
            assert_eq!(out.loot, snap.loot, "loot at step {step}");
        }
    }
}

#[test]
fn skipped_entity_sections_carry_baseline() {
    let mut enc = SnapshotEncoder::new();
    let mut dec = SnapshotDecoder::new();
    let mut snap = base_snapshot();

    dec.decode(&enc.encode(&snap, true)).unwrap();
    let zombies_before = snap.zombies.clone();

    // server mutates zombies but skips the section this tick
    snap.tick += 1;
    for z in snap.zombies.iter_mut() {
        z.pos[0] += 50;
    }
    let out = dec.decode(&enc.encode(&snap, false)).unwrap();
    assert_eq!(
        out.zombies, zombies_before,
        "decoder must keep the old zombies"
    );

    // next tick includes the section — decoder catches up to current positions
    snap.tick += 1;
    let out = dec.decode(&enc.encode(&snap, true)).unwrap();
    assert_eq!(out.zombies, snap.zombies);
}

#[test]
fn keyframe_forced_periodically_and_on_demand() {
    let mut enc = SnapshotEncoder::new();
    let mut snap = base_snapshot();
    let first = enc.encode(&snap, true);
    assert_eq!(first[1] & 1, 1, "first frame is a keyframe");
    for _ in 0..zz_core::constants::KEYFRAME_EVERY {
        snap.tick += 1;
        let f = enc.encode(&snap, true);
        if f[1] & 1 == 1 {
            return; // periodic keyframe arrived within the window
        }
    }
    // one more must be a keyframe
    snap.tick += 1;
    let f = enc.encode(&snap, true);
    assert_eq!(f[1] & 1, 1, "periodic keyframe overdue");
}

#[test]
fn fresh_decoder_rejects_delta_needs_keyframe() {
    let mut enc = SnapshotEncoder::new();
    let mut snap = base_snapshot();
    enc.encode(&snap, true); // keyframe consumed by nobody
    snap.tick += 1;
    let delta = enc.encode(&snap, true);
    let mut dec = SnapshotDecoder::new();
    assert_eq!(dec.decode(&delta), Err(CodecError::NoBaseline));

    // after force_keyframe the fresh decoder can join mid-stream
    enc.force_keyframe();
    snap.tick += 1;
    let key = enc.encode(&snap, true);
    assert!(dec.decode(&key).is_ok());
}

#[test]
fn tick_regression_is_desync() {
    let mut enc = SnapshotEncoder::new();
    let mut dec = SnapshotDecoder::new();
    let mut snap = base_snapshot();
    snap.tick = 100;
    dec.decode(&enc.encode(&snap, true)).unwrap();
    snap.tick = 101;
    let newer = enc.encode(&snap, true);
    dec.decode(&newer).unwrap();
    // replaying the same delta again must fail (tick not advancing)
    assert_eq!(dec.decode(&newer), Err(CodecError::Desync));
}

#[test]
fn garbage_never_panics() {
    let mut rng = Mulberry32::from_seed("garbage");
    for round in 0..2000 {
        let len = (rng.next() * 300.0) as usize;
        let buf: Vec<u8> = (0..len).map(|_| (rng.next() * 256.0) as u8).collect();
        let mut dec = SnapshotDecoder::new();
        let _ = dec.decode(&buf); // any Err is fine; a panic is the bug
        let _ = round;
    }
}

#[test]
fn truncation_never_panics_and_errors() {
    let mut enc = SnapshotEncoder::new();
    let snap = base_snapshot();
    let frame = enc.encode(&snap, true);
    for cut in 0..frame.len() {
        let mut dec = SnapshotDecoder::new();
        assert!(
            dec.decode(&frame[..cut]).is_err(),
            "cut at {cut} must error"
        );
    }
}

#[test]
fn horde_delta_stays_small() {
    let mut enc = SnapshotEncoder::new();
    let mut snap = base_snapshot();
    snap.zombies = (0..200).map(zombie).collect();
    enc.encode(&snap, true);

    snap.tick += 1;
    // every zombie shuffles a small step (fits i8 deltas)
    for z in snap.zombies.iter_mut() {
        z.pos[0] += 15;
        z.pos[2] -= 19;
    }
    let frame = enc.encode(&snap, true);
    assert!(
        frame.len() < 2048,
        "200 moving zombies should delta-encode under 2 KB, got {}",
        frame.len()
    );
}

#[test]
fn quantization_helpers_round_trip_within_precision() {
    for v in [-100.0f32, -3.7, 0.0, 0.007, 39.99, 200.0] {
        let q = quant_pos(v);
        assert!((dequant_pos(q) - v).abs() <= 0.5 / POS_SCALE + 1e-6);
    }
    for yaw in [0.0f32, 1.0, 3.1, 6.4, -2.5, 12.0] {
        let q = quant_yaw16(yaw);
        let back = dequant_yaw16(q);
        let diff = (back - yaw.rem_euclid(core::f32::consts::TAU)).abs();
        assert!(diff < 1e-3, "yaw {yaw} → {back}");
    }
}
