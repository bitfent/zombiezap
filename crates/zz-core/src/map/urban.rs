//! Urban arena generator — line-for-line port of
//! `legacy/packages/shared/src/mapgen.ts`.
//!
//! RNG call order and control flow must match the TypeScript original exactly
//! so maps are bit-identical in f64.

use crate::rng::Mulberry32;

use super::{
    ACCENTS, ARENA_HALF, Arena64, BARREL_SPAWN_CLEAR, BUILD_H, BUILD_H_TALL, BWALL_T, Barrel64,
    Box64, DECK_BOTTOM, DECK_TOP, DOOR, EDGE_MARGIN, GAP, HEAD_Y, PARAPET_H, PLATFORM_H,
    PLAYER_EYE, Pickup64, ROOF_T, SILL_H, SPAWN_CLEAR, STEP_RISE, STEP_RUN, Spawn64, T, TOWER_H,
    WALL_H, snap,
};

// Spawns track the arena size — always 4m in from the corners.
const SPAWN_D: f64 = ARENA_HALF - 4.0;

fn spawns() -> [Spawn64; 4] {
    let pi = core::f64::consts::PI;
    [
        Spawn64 {
            x: -SPAWN_D,
            z: -SPAWN_D,
            yaw: -pi * 0.75,
        },
        Spawn64 {
            x: SPAWN_D,
            z: SPAWN_D,
            yaw: pi * 0.25,
        },
        Spawn64 {
            x: -SPAWN_D,
            z: SPAWN_D,
            yaw: -pi * 0.25,
        },
        Spawn64 {
            x: SPAWN_D,
            z: -SPAWN_D,
            yaw: pi * 0.75,
        },
    ]
}

fn perimeter() -> [Box64; 4] {
    let h = ARENA_HALF;
    [
        Box64 {
            x0: -h - T,
            x1: h + T,
            y0: 0.0,
            y1: WALL_H,
            z0: -h - T,
            z1: -h,
        },
        Box64 {
            x0: -h - T,
            x1: h + T,
            y0: 0.0,
            y1: WALL_H,
            z0: h,
            z1: h + T,
        },
        Box64 {
            x0: -h - T,
            x1: -h,
            y0: 0.0,
            y1: WALL_H,
            z0: -h,
            z1: h,
        },
        Box64 {
            x0: h,
            x1: h + T,
            y0: 0.0,
            y1: WALL_H,
            z0: -h,
            z1: h,
        },
    ]
}

fn inflate(b: Box64, m: f64) -> Box64 {
    Box64 {
        x0: b.x0 - m,
        x1: b.x1 + m,
        y0: b.y0,
        y1: b.y1,
        z0: b.z0 - m,
        z1: b.z1 + m,
    }
}

fn overlaps_xz(a: Box64, b: Box64) -> bool {
    a.x0 < b.x1 && a.x1 > b.x0 && a.z0 < b.z1 && a.z1 > b.z0
}

fn near_spawn(b: Box64, clear: f64) -> bool {
    for s in spawns() {
        let cx = b.x0.max(s.x.min(b.x1));
        let cz = b.z0.max(s.z.min(b.z1));
        if (cx - s.x) * (cx - s.x) + (cz - s.z) * (cz - s.z) < clear * clear {
            return true;
        }
    }
    false
}

fn mirror(b: Box64) -> Box64 {
    Box64 {
        x0: -b.x1,
        x1: -b.x0,
        y0: b.y0,
        y1: b.y1,
        z0: -b.z1,
        z1: -b.z0,
    }
}

#[derive(Clone, Copy)]
enum Feature {
    Solid,
    Door,
    Window,
}

// ── building walls with door/window features ───────────────────────────────

fn wall_run(
    axis: char,
    fixed: f64,
    from: f64,
    to: f64,
    feature: Feature,
    rng: &mut Mulberry32,
    h: f64,
) -> Vec<Box64> {
    let half = BWALL_T / 2.0;
    let seg = |a: f64, b: f64, y0: f64, y1: f64| -> Box64 {
        if axis == 'x' {
            Box64 {
                x0: a,
                x1: b,
                y0,
                y1,
                z0: fixed - half,
                z1: fixed + half,
            }
        } else {
            Box64 {
                x0: fixed - half,
                x1: fixed + half,
                y0,
                y1,
                z0: a,
                z1: b,
            }
        }
    };
    if matches!(feature, Feature::Solid) || to - from < DOOR + 1.4 {
        return vec![seg(from, to, 0.0, h)];
    }
    let span = match feature {
        Feature::Door => DOOR,
        Feature::Window => (2.0f64).max((to - from) * 0.45).min(2.8),
        Feature::Solid => unreachable!(),
    };
    let at = snap(from + span / 2.0 + 0.6 + rng.next() * (to - from - span - 1.2));
    let a0 = at - span / 2.0;
    let a1 = at + span / 2.0;
    let mut out = Vec::new();
    if a0 - from > 0.3 {
        out.push(seg(from, a0, 0.0, h));
    }
    if to - a1 > 0.3 {
        out.push(seg(a1, to, 0.0, h));
    }
    if matches!(feature, Feature::Window) {
        out.push(seg(a0, a1, 0.0, SILL_H));
        out.push(seg(a0, a1, HEAD_Y, h));
    }
    out
}

fn make_building(rng: &mut Mulberry32, cx: f64, cz: f64, w: f64, d: f64) -> Vec<Box64> {
    let x0 = snap(cx - w / 2.0);
    let x1 = snap(cx + w / 2.0);
    let z0 = snap(cz - d / 2.0);
    let z1 = snap(cz + d / 2.0);
    let tall = rng.next() < 0.3;
    let h = if tall { BUILD_H_TALL } else { BUILD_H };
    let mut sides = [
        Feature::Door,
        Feature::Window,
        if rng.next() < 0.5 {
            Feature::Door
        } else {
            Feature::Window
        },
        if rng.next() < 0.6 {
            Feature::Window
        } else {
            Feature::Solid
        },
    ];
    // Fisher–Yates shuffle, TS order: for i = len-1 .. 1
    for i in (1..sides.len()).rev() {
        let j = (rng.next() * (i + 1) as f64).floor() as usize;
        sides.swap(i, j);
    }
    let mut out = Vec::new();
    out.extend(wall_run('x', z0, x0, x1, sides[0], rng, h));
    out.extend(wall_run('x', z1, x0, x1, sides[1], rng, h));
    out.extend(wall_run('z', x0, z0, z1, sides[2], rng, h));
    out.extend(wall_run('z', x1, z0, z1, sides[3], rng, h));
    out.push(Box64 {
        x0,
        x1,
        y0: h,
        y1: h + ROOF_T,
        z0,
        z1,
    });
    // two-room interior
    if w.max(d) >= 7.0 {
        if w >= d {
            let mid = snap(cx + (rng.next() - 0.5) * (w * 0.25));
            out.extend(wall_run(
                'z',
                mid,
                z0 + BWALL_T,
                z1 - BWALL_T,
                Feature::Door,
                rng,
                h,
            ));
        } else {
            let mid = snap(cz + (rng.next() - 0.5) * (d * 0.25));
            out.extend(wall_run(
                'x',
                mid,
                x0 + BWALL_T,
                x1 - BWALL_T,
                Feature::Door,
                rng,
                h,
            ));
        }
    }
    // parapet lip on tall roofs
    if tall {
        let py0 = h + ROOF_T;
        let py1 = py0 + PARAPET_H;
        let lip = 0.25;
        out.push(Box64 {
            x0,
            x1,
            y0: py0,
            y1: py1,
            z0,
            z1: z0 + lip,
        });
        out.push(Box64 {
            x0,
            x1,
            y0: py0,
            y1: py1,
            z0: z1 - lip,
            z1,
        });
        out.push(Box64 {
            x0,
            x1: x0 + lip,
            y0: py0,
            y1: py1,
            z0,
            z1,
        });
        out.push(Box64 {
            x0: x1 - lip,
            x1,
            y0: py0,
            y1: py1,
            z0,
            z1,
        });
    }
    out
}

// ── platform + stairs ──────────────────────────────────────────────────────

fn make_platform(_rng: &mut Mulberry32, cx: f64, cz: f64, w: f64, height: f64) -> Vec<Box64> {
    // TS: `void rng;` — parameter reserved, no rng() call.
    let x0 = snap(cx - w / 2.0);
    let x1 = snap(cx + w / 2.0);
    let z1 = snap(cz + w / 2.0);
    let top = Box64 {
        x0,
        x1,
        y0: 0.0,
        y1: height,
        z0: snap(cz - w / 2.0),
        z1,
    };
    let mut steps = vec![top];
    // Math.round(height / STEP_RISE) — use js_round for parity
    let n_steps = super::js_round(height / STEP_RISE) as i32;
    let mut z = top.z0;
    for i in (1..=n_steps).rev() {
        let h = i as f64 * STEP_RISE;
        let sz1 = z;
        let sz0 = snap(z - STEP_RUN);
        steps.push(Box64 {
            x0,
            x1,
            y0: 0.0,
            y1: h,
            z0: sz0,
            z1: sz1,
        });
        z = sz0;
    }
    steps
}

fn make_tower(rng: &mut Mulberry32, cx: f64, cz: f64, w: f64) -> Vec<Box64> {
    make_platform(rng, cx, cz, w, TOWER_H)
}

fn make_bridge(rng: &mut Mulberry32, cx: f64, cz: f64, w: f64, span: f64) -> Vec<Box64> {
    let half = span / 2.0 + w / 2.0;
    let mut tower_a = make_platform(rng, cx - half, cz, w, PLATFORM_H);
    let tower_b = make_platform(rng, cx + half, cz, w, PLATFORM_H);
    let deck = Box64 {
        x0: snap(cx - half + w / 2.0),
        x1: snap(cx + half - w / 2.0),
        y0: DECK_BOTTOM,
        y1: DECK_TOP,
        z0: snap(cz - w / 2.0),
        z1: snap(cz + w / 2.0),
    };
    tower_a.extend(tower_b);
    tower_a.push(deck);
    tower_a
}

fn make_l_wall(rng: &mut Mulberry32, cx: f64, cz: f64) -> Vec<Box64> {
    let t = 0.45;
    let len_a = snap(2.5 + rng.next() * 1.5);
    let len_b = snap(2.0 + rng.next() * 1.5);
    let h = 1.7 + rng.next() * 0.5;
    let x0 = snap(cx);
    let z0 = snap(cz);
    vec![
        Box64 {
            x0,
            x1: snap(x0 + len_a),
            y0: 0.0,
            y1: h,
            z0,
            z1: snap(z0 + t),
        },
        Box64 {
            x0,
            x1: snap(x0 + t),
            y0: 0.0,
            y1: h,
            z0,
            z1: snap(z0 + len_b),
        },
    ]
}

// ── f64 ray-box for spawn-LOS (matches raycast.ts; cover only) ─────────────

fn ray_box_f64(ox: f64, oy: f64, oz: f64, dx: f64, dy: f64, dz: f64, b: Box64) -> Option<f64> {
    let mut tmin = f64::NEG_INFINITY;
    let mut tmax = f64::INFINITY;
    for (ov, dv, lo, hi) in [
        (ox, dx, b.x0, b.x1),
        (oy, dy, b.y0, b.y1),
        (oz, dz, b.z0, b.z1),
    ] {
        if dv.abs() < 1e-9 {
            if ov < lo || ov > hi {
                return None;
            }
            continue;
        }
        let mut t1 = (lo - ov) / dv;
        let mut t2 = (hi - ov) / dv;
        if t1 > t2 {
            core::mem::swap(&mut t1, &mut t2);
        }
        tmin = tmin.max(t1);
        tmax = tmax.min(t2);
        if tmin > tmax {
            return None;
        }
    }
    if tmax < 0.0 {
        return None;
    }
    Some(tmin.max(0.0))
}

fn nearest_wall_t_f64(
    ox: f64,
    oy: f64,
    oz: f64,
    dx: f64,
    dy: f64,
    dz: f64,
    walls: &[Box64],
) -> Option<f64> {
    let mut best: Option<f64> = None;
    for w in walls {
        // Match math.rs style (edition 2024 let-chain).
        if let Some(t) = ray_box_f64(ox, oy, oz, dx, dy, dz, *w)
            && best.is_none_or(|b| t < b)
        {
            best = Some(t);
        }
    }
    best
}

fn footprint_of(segs: &[Box64]) -> Box64 {
    let mut x0 = f64::INFINITY;
    let mut x1 = f64::NEG_INFINITY;
    let mut z0 = f64::INFINITY;
    let mut z1 = f64::NEG_INFINITY;
    for s in segs {
        x0 = x0.min(s.x0);
        x1 = x1.max(s.x1);
        z0 = z0.min(s.z0);
        z1 = z1.max(s.z1);
    }
    Box64 {
        x0,
        x1,
        y0: 0.0,
        y1: WALL_H,
        z0,
        z1,
    }
}

/// Generate a full urban arena from `seed` — bit-identical to TS `generateArena`.
pub fn generate_arena(seed: &str) -> Arena64 {
    let mut rng = Mulberry32::from_seed(seed);
    let h = ARENA_HALF;
    let lim = h - EDGE_MARGIN;
    let mut cover: Vec<Box64> = Vec::new();
    let mut barrels: Vec<Barrel64> = Vec::new();
    let spawn_list = spawns();

    let group_fits = |segs: &[Box64], cover: &[Box64]| -> bool {
        let fp = footprint_of(segs);
        if fp.x0 < -lim || fp.x1 > lim || fp.z0 < -lim || fp.z1 > lim {
            return false;
        }
        if near_spawn(fp, SPAWN_CLEAR) {
            return false;
        }
        !cover.iter().any(|c| overlaps_xz(inflate(fp, GAP), *c))
    };

    let add_mirrored = |cover: &mut Vec<Box64>, segs: &[Box64]| {
        for piece in segs {
            cover.push(*piece);
            cover.push(mirror(*piece));
        }
    };

    let place_group = |make: &mut dyn FnMut(&mut Mulberry32) -> Vec<Box64>,
                       tries: usize,
                       rng: &mut Mulberry32,
                       cover: &mut Vec<Box64>|
     -> bool {
        for _attempt in 0..tries {
            let g = make(rng);
            let fp = footprint_of(&g);
            if group_fits(&g, cover) && !overlaps_xz(inflate(fp, GAP), mirror(fp)) {
                add_mirrored(cover, &g);
                return true;
            }
        }
        false
    };

    // ── 1) buildings ───────────────────────────────────────────────────────
    let building_pairs = 3 + (rng.next() * 3.0).floor() as i32;
    for _n in 0..building_pairs {
        place_group(
            &mut |rng| {
                let w = 5.0 + rng.next() * 4.0;
                let d = 5.0 + rng.next() * 4.0;
                let cx = (rng.next() * 2.0 - 1.0) * (lim - w / 2.0 - 0.6);
                let cz = -(4.0 + rng.next() * (lim - d / 2.0 - 4.5));
                make_building(rng, cx, cz, w, d)
            },
            40,
            &mut rng,
            &mut cover,
        );
    }

    // ── 2) platforms, bridge, tower ────────────────────────────────────────
    let platform_pairs = 1 + if rng.next() < 0.6 { 1 } else { 0 };
    for _n in 0..platform_pairs {
        place_group(
            &mut |rng| {
                let w = 3.5 + rng.next() * 1.5;
                let cx = (rng.next() * 2.0 - 1.0) * (lim - w - 1.5);
                let cz = -(5.0 + rng.next() * (lim - w - 6.0));
                make_platform(rng, cx, cz, w, PLATFORM_H)
            },
            36,
            &mut rng,
            &mut cover,
        );
    }
    if rng.next() < 0.7 {
        place_group(
            &mut |rng| {
                let w = 3.0 + rng.next();
                let span = 3.0 + rng.next() * 2.0;
                let need = w + span / 2.0 + 1.5;
                let cx = (rng.next() * 2.0 - 1.0) * (lim - need);
                let cz = -(6.0 + rng.next() * (lim - w - 7.0));
                make_bridge(rng, cx, cz, w, span)
            },
            36,
            &mut rng,
            &mut cover,
        );
    }
    if rng.next() < 0.7 {
        place_group(
            &mut |rng| {
                let w = 3.0 + rng.next() * 0.5;
                let cx = (rng.next() * 2.0 - 1.0) * (lim - w - 1.5);
                let cz = -(8.0 + rng.next() * (lim - w - 9.0));
                make_tower(rng, cx, cz, w)
            },
            36,
            &mut rng,
            &mut cover,
        );
    }

    // ── 2b) L-shaped corner walls ──────────────────────────────────────────
    let l_walls = 1 + if rng.next() < 0.5 { 1 } else { 0 };
    for _n in 0..l_walls {
        place_group(
            &mut |rng| {
                let cx = (rng.next() * 2.0 - 1.0) * (lim - 5.0);
                let cz = -(2.0 + rng.next() * (lim - 7.0));
                make_l_wall(rng, cx, cz)
            },
            30,
            &mut rng,
            &mut cover,
        );
    }

    // ── 3) optional centerpiece ────────────────────────────────────────────
    if rng.next() < 0.5 {
        let w = 1.4 + rng.next() * 1.4;
        let d = 1.4 + rng.next() * 1.4;
        let ch = 1.6 + rng.next() * 1.0;
        let c = Box64 {
            x0: snap(-w),
            x1: snap(w),
            y0: 0.0,
            y1: ch,
            z0: snap(-d),
            z1: snap(d),
        };
        if group_fits(&[c], &cover) {
            cover.push(c);
        }
    }

    // ── 4) street furniture ────────────────────────────────────────────────
    let target = 9 + (rng.next() * 6.0).floor() as i32;
    let mut placed = 0i32;
    let mut attempt = 0i32;
    while attempt < 220 && placed < target {
        attempt += 1;
        let kind = rng.next();
        let (w, d, h, mut barrel) = if kind < 0.34 {
            (
                0.8 + rng.next() * 1.0,
                0.8 + rng.next() * 1.0,
                1.0 + rng.next() * 1.2,
                false,
            )
        } else if kind < 0.62 {
            let long = 1.8 + rng.next() * 1.6;
            let flip = rng.next() < 0.5;
            (
                if flip { long } else { 0.45 },
                if flip { 0.45 } else { long },
                1.5 + rng.next() * 0.9,
                false,
            )
        } else if kind < 0.82 {
            (0.6, 0.6, 1.6 + rng.next() * 1.0, false)
        } else {
            (0.55, 0.55, 1.1, true)
        };
        let cx = snap((rng.next() * 2.0 - 1.0) * (lim - w));
        let cz = snap(-(0.8 + rng.next() * (lim - d - 0.8)));
        if barrel
            && spawn_list.iter().any(|s| {
                let hx = cx - s.x;
                let hz = cz - s.z;
                (hx * hx + hz * hz).sqrt() < BARREL_SPAWN_CLEAR
            })
        {
            barrel = false;
        }
        let piece = Box64 {
            x0: cx - w,
            x1: cx + w,
            y0: 0.0,
            y1: h,
            z0: cz - d,
            z1: cz + d,
        };
        let twin = mirror(piece);
        if group_fits(&[piece], &cover)
            && !overlaps_xz(inflate(piece, GAP), twin)
            && group_fits(&[twin], &cover)
        {
            let cover_idx = cover.len();
            add_mirrored(&mut cover, &[piece]);
            if barrel {
                barrels.push(Barrel64 {
                    x: cx,
                    z: cz,
                    wall_index: 4 + cover_idx,
                });
                barrels.push(Barrel64 {
                    x: -cx,
                    z: -cz,
                    wall_index: 4 + cover_idx + 1,
                });
            }
            placed += 1;
        }
    }

    // ── 5) fairness: spawn-to-spawn must NOT be open sight ──────────────────
    let a = spawn_list[0];
    let b = spawn_list[1];
    let dx = b.x - a.x;
    let dz = b.z - a.z;
    let dist = (dx * dx + dz * dz).sqrt();
    let t = nearest_wall_t_f64(a.x, PLAYER_EYE, a.z, dx / dist, 0.0, dz / dist, &cover);
    if t.is_none_or(|t| t > dist) {
        cover.push(Box64 {
            x0: -2.0,
            x1: 2.0,
            y0: 0.0,
            y1: 2.0,
            z0: -2.0,
            z1: 2.0,
        });
    }

    // ── 6) three mirrored pickup pairs ─────────────────────────────────────
    let mut walls: Vec<Box64> = perimeter().to_vec();
    walls.extend(cover);
    let mut arena = Arena64 {
        seed: seed.to_string(),
        walls,
        spawns: spawn_list.to_vec(),
        pickups: Vec::new(),
        accent: 0,
        barrels,
    };
    let avoid: Vec<(f64, f64, f64)> = spawn_list.iter().map(|s| (s.x, s.z, 5.0)).collect();
    let p1 = find_open_spot(&arena, &mut rng, &avoid);
    let mut avoid2 = avoid.clone();
    avoid2.push((p1.x, p1.z, 8.0));
    avoid2.push((-p1.x, -p1.z, 8.0));
    let p2 = find_open_spot(&arena, &mut rng, &avoid2);
    let mut avoid3 = avoid;
    avoid3.push((p1.x, p1.z, 8.0));
    avoid3.push((-p1.x, -p1.z, 8.0));
    avoid3.push((p2.x, p2.z, 8.0));
    avoid3.push((-p2.x, -p2.z, 8.0));
    let p3 = find_open_spot(&arena, &mut rng, &avoid3);
    arena.pickups = vec![
        p1,
        Pickup64 { x: -p1.x, z: -p1.z },
        p2,
        Pickup64 { x: -p2.x, z: -p2.z },
        p3,
        Pickup64 { x: -p3.x, z: -p3.z },
    ];
    arena.accent = ACCENTS[(rng.next() * ACCENTS.len() as f64).floor() as usize];
    arena
}

/// A clear standing spot on the ground — deterministic given `rng`.
pub fn find_open_spot(
    arena: &Arena64,
    rng: &mut Mulberry32,
    avoid: &[(f64, f64, f64)],
) -> Pickup64 {
    let lim = ARENA_HALF - 1.5;
    for _i in 0..120 {
        let x = (rng.next() * 2.0 - 1.0) * lim;
        let z = (rng.next() * 2.0 - 1.0) * lim;
        let probe = Box64 {
            x0: x - 0.6,
            x1: x + 0.6,
            y0: 0.0,
            y1: 1.8,
            z0: z - 0.6,
            z1: z + 0.6,
        };
        if arena
            .walls
            .iter()
            .any(|w| w.y0 < 1.0 && overlaps_xz(inflate(probe, 0.4), *w))
        {
            continue;
        }
        if avoid
            .iter()
            .any(|(ax, az, r)| (ax - x) * (ax - x) + (az - z) * (az - z) < r * r)
        {
            continue;
        }
        return Pickup64 { x, z };
    }
    Pickup64 { x: 0.0, z: -10.0 }
}
