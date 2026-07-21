// The game: a retro low-res Three.js arena with two modes sharing one world —
//   practice : offline, client-side hitscan against dummy targets (Phase 1)
//   duel     : server-authoritative 1v1 over WebSocket (Phase 2)
// Self-movement is PREDICTED with the same stepBody() the server runs; the
// server remains the only authority on hits, health, and score.

import * as THREE from "three";
import { mergeGeometries } from "three/examples/jsm/utils/BufferGeometryUtils.js";
import {
  ARENA_HALF,
  BARREL_DMG_MAX,
  BARREL_DMG_MIN,
  BARREL_RADIUS_DMG,
  DAMAGE_PER_HIT,
  FIRE_COOLDOWN_MS,
  INTERP_DELAY_MS,
  KILLS_TO_WIN,
  MAX_HEALTH,
  PICKUP_HEAL,
  PICKUP_RADIUS,
  PICKUP_RESPAWN_MS,
  PLAYER_EYE,
  SHOT_RANGE,
  dirFromAngles,
  findOpenSpot,
  generateArena,
  nearestWallHit,
  nearestWallT,
  seededRandom,
  randomSeed,
  rayBox,
  stepBody,
  SnapshotDecoder,
  type Arena,
  type Body,
  type Box,
  type GameSnapshot,
  type MatchResult,
  type PlayerInput,
  type Vec3,
} from "@shotante/shared";
import { Input } from "./Input.ts";
import { Hud } from "./Hud.ts";
import { Sfx } from "./Audio.ts";
import { Voice } from "./Voice.ts";

const INTERNAL_W = 960; // retro internal render res — chunky but CS-readable
const INTERNAL_H = 540;
// Per-player colors by roster index: player 0 red, player 1 blue — so the two
// duelists are always visually distinct (and consistent across both screens).
const TEAM_COLORS = [0xff3355, 0x3b82f6];
// INTERP_DELAY_MS (shared): render the opponent ~150ms in the past so the buffer
// always has a sample on either side of "now" — a late snapshot rides the last
// segment instead of freezing/teleporting. The server uses the SAME value for
// lag compensation, so both agree on when the shooter saw the world.
const SEND_INTERVAL_MS = 33; // ~30 inputs/sec

// Pooled transient FX: tracers, impact sparks, explosion fireballs are allocated
// ONCE and reused (ttl<=0 == free, mesh hidden). Quake's lesson: never allocate
// in the hot loop — a `new`/`dispose` per shot is exactly what causes GC hitches
// mid-firefight.
const TRACER_POOL = 48;
const SPARK_POOL = 48;
const BOOM_POOL = 24;

// Two-bone arms (upper + forearm, bent elbow) holding the rifle, posed by STATE
// instead of one static cluster. The gun is PARENTED to the right hand, so it
// rides the arm through every pose and can never float. A relaxed low-ready
// CARRY pose blends up to a shouldered AIM pose while firing, with a recoil kick.
// Each pose names the 4 driven joints (right/left shoulder + elbow, each as
// pitch x / yaw y); animateAvatar lerps between them by `aim` (0..1).
// Only the RIGHT (firing) arm is posed by these — the left (support) arm is
// IK'd to the rifle foregrip every frame, so it's not a fixed pose.
const POSE_CARRY = { rshx: 0.5, rshy: 0.22, relx: 1.15, rely: 0 };
const POSE_AIM = { rshx: 1.05, rshy: 0.12, relx: 0.75, rely: 0 };
const FIRE_HOLD_MS = 450; // stay shouldered this long after the last shot
const DOWN = new THREE.Vector3(0, -1, 0); // limbs hang along -Y by default
interface FxItem<M extends THREE.Object3D> {
  mesh: M;
  ttl: number;
}

// A short ring buffer of timestamped server states per opponent. We interpolate
// between the two samples bracketing (now - INTERP_DELAY_MS) instead of just the
// last two — that's what absorbs network jitter.
interface RemoteAvatar {
  group: THREE.Group;
  buf: { t: number; x: number; y: number; z: number; yaw: number }[];
}

// Procedural animation rig for a boxy avatar (Minecraft-style: rigid box limbs
// rotated at the joint). Stored on group.userData.rig; driven from movement —
// speed comes from the position delta, "airborne" from vertical position, so no
// extra server data is needed.
interface AvatarRig {
  body: THREE.Object3D; // torso/arms/legs container — bobs while moving
  lleg: THREE.Object3D;
  rleg: THREE.Object3D;
  rsh: THREE.Object3D; // right shoulder + elbow (firing arm, posed by carry/aim)
  rel: THREE.Object3D;
  hand: THREE.Object3D; // right-hand node; the gun's position is glued to it
  gun: THREE.Object3D; // child of body — barrel always forward, position = hand
  larm: THREE.Object3D; // left (support) arm pivot — oriented to the foregrip
  larmBox: THREE.Mesh; // the support-arm box — stretched to reach the foregrip
  foregrip: THREE.Object3D; // anchor on the rifle handguard the left hand holds
  phase: number; // walk-cycle phase
  speed: number; // low-passed horizontal speed (m/s)
  aim: number; // 0 = carry, 1 = aiming/shooting (eased)
  recoil: number; // 0..1 kick, spikes on each shot and decays
  fireUntil: number; // performance.now() until which we stay shouldered
  px: number;
  pz: number; // previous position, for the speed estimate
  init: boolean;
}

interface PracticeTarget {
  mesh: THREE.Mesh;
  alive: boolean;
  hp: number; // 4 hits to destroy — same TTK as a real duel
  box: Box; // hit region == the visible mesh, exactly
}

export type GameMode = "practice" | "duel";

export class Game {
  readonly input: Input;
  private renderer: THREE.WebGLRenderer;
  private scene = new THREE.Scene();
  private camera: THREE.PerspectiveCamera;
  private hud = new Hud();
  private sfx = new Sfx();
  private voice = new Voice();

  private mode: GameMode = "practice";
  private arena!: Arena; // set by loadArena() in the constructor
  private mapGroup: THREE.Group | null = null;
  private pickupMeshes: THREE.Group[] = [];

  // explosive barrels: liveWalls is arena.walls minus detonated barrels —
  // prediction/occlusion must drop the SAME box the server drops
  private liveWalls: Box[] = [];
  private barrelIntact: boolean[] = [];
  private barrelMeshes: (THREE.Group | null)[] = [];
  private barrelByBox = new Map<Box, number>();
  private boomPool: FxItem<THREE.Mesh>[] = [];
  private practiceHealth = MAX_HEALTH;
  private practicePickups: { active: boolean; respawnAt: number }[] = [];
  private running = false;
  private body: Body = { x: 0, y: 0, z: 0, vy: 0, onGround: true };
  private lastFrame = 0;
  private tracerPool: FxItem<THREE.Line>[] = [];
  private sparkPool: FxItem<THREE.Mesh>[] = [];
  private lockHint = document.getElementById("lock-hint")!;
  private sensToast = document.getElementById("sens-toast")!;
  private sensToastTimer: ReturnType<typeof setTimeout> | null = null;

  // proximity VOICE: a "speaking ring" above the opponent while they talk and
  // are in range (screen-projected each frame), and a small mic-state pill.
  private speakRing = document.getElementById("speaking-ring")!;
  private micPill = document.getElementById("mic")!;
  private bubbleVec = new THREE.Vector3();

  // practice state
  private targets: PracticeTarget[] = [];
  private practiceHits = 0;
  private lastLocalShot = 0;

  // duel state
  selfId: string | null = null;
  onInput: ((input: PlayerInput) => void) | null = null;
  private playerColors = new Map<string, number>(); // id -> avatar color (by roster index)
  private remotes = new Map<string, RemoteAvatar>();
  private avTmp = new THREE.Vector3(); // scratch for gluing the gun to the hand
  private avTmp2 = new THREE.Vector3(); // scratch for the left-arm reach
  private lastSent = 0;
  private prevPlayers = new Map<string, { health: number; alive: boolean }>();
  private selfAlive = true;
  private lastPhase: GameSnapshot["phase"] | null = null;
  private clutchOn = false; // clutch-time music already running

  // replay: spectator playback of a recorded match
  private replayMode = false;
  private replaySnaps: { t: number; snap: GameSnapshot }[] = [];
  private replayIdx = 0;
  private replayStart = 0;
  private replayLast = 0;

  // attract mode (menu backdrop): two scripted bots duel under an orbiting
  // camera — pure decoration, runs only while the menu is open
  private attractOn = false;
  private attractBots: {
    avatar: THREE.Group;
    body: Body;
    yaw: number;
    strafe: -1 | 1;
    flipAt: number;
    shootAt: number;
  }[] = [];
  private attractLast = 0;
  private attractSeq = 0;

  constructor(canvas: HTMLCanvasElement) {
    this.renderer = new THREE.WebGLRenderer({ canvas, antialias: false });
    this.renderer.setSize(INTERNAL_W, INTERNAL_H, false); // CSS upscales, pixelated
    this.renderer.shadowMap.enabled = true;
    this.renderer.shadowMap.type = THREE.BasicShadowMap; // hard edges = retro
    // The map is STATIC for the whole match, so its shadows are too — bake them
    // ONCE (on map load, and again when a barrel blows up) instead of re-rendering
    // the entire scene into the shadow map every single frame. Moving players use
    // cheap blob shadows instead of casting into this map. This is the single
    // biggest frame-budget win.
    this.renderer.shadowMap.autoUpdate = false;
    this.camera = new THREE.PerspectiveCamera(80, INTERNAL_W / INTERNAL_H, 0.05, 120);
    this.camera.rotation.order = "YXZ";
    this.input = new Input(canvas);
    this.input.onSensitivityChange = (mult) => {
      this.sensToast.textContent = `SENSITIVITY ×${mult.toFixed(2)}`;
      this.sensToast.classList.remove("hidden");
      if (this.sensToastTimer) clearTimeout(this.sensToastTimer);
      this.sensToastTimer = setTimeout(() => this.sensToast.classList.add("hidden"), 900);
    };
    this.voice.setSpatial(() => this.voiceSpatial());
    this.buildWorld();
    this.initPools();
    this.loadArena(generateArena("menu-backdrop"));
  }

  /** Pre-allocate the transient-FX meshes once and park them in the scene
   *  (hidden). addTracer/boomVfx recycle these instead of new-ing geometry. */
  private initPools(): void {
    for (let i = 0; i < TRACER_POOL; i++) {
      const geo = new THREE.BufferGeometry().setFromPoints([
        new THREE.Vector3(),
        new THREE.Vector3(),
      ]);
      const line = new THREE.Line(
        geo,
        new THREE.LineBasicMaterial({ color: 0xff8a3d, transparent: true, opacity: 1 }),
      );
      line.visible = false;
      line.frustumCulled = false; // endpoints move every reuse — don't cull stale bounds
      this.scene.add(line);
      this.tracerPool.push({ mesh: line, ttl: 0 });
    }
    for (let i = 0; i < SPARK_POOL; i++) {
      const m = new THREE.Mesh(
        new THREE.BoxGeometry(0.14, 0.14, 0.14),
        new THREE.MeshBasicMaterial({ color: 0xffd23f }),
      );
      m.visible = false;
      this.scene.add(m);
      this.sparkPool.push({ mesh: m, ttl: 0 });
    }
    for (let i = 0; i < BOOM_POOL; i++) {
      const m = new THREE.Mesh(
        new THREE.SphereGeometry(0.6, 10, 8),
        new THREE.MeshBasicMaterial({ color: 0xff9a3d, transparent: true, opacity: 0.9 }),
      );
      m.visible = false;
      this.scene.add(m);
      this.boomPool.push({ mesh: m, ttl: 0 });
    }
  }

  /** Grab a free pool slot, or recycle the one closest to expiry if all busy. */
  private acquire<M extends THREE.Object3D>(pool: FxItem<M>[]): FxItem<M> {
    let pick = pool[0];
    for (const it of pool) {
      if (it.ttl <= 0) return it;
      if (it.ttl < pick.ttl) pick = it;
    }
    return pick;
  }

  /** Hide every active FX (new map / new match) without disposing the pool. */
  private resetPools(): void {
    for (const p of [...this.tracerPool, ...this.sparkPool, ...this.boomPool]) {
      p.ttl = 0;
      p.mesh.visible = false;
    }
  }

  // ── world ────────────────────────────────────────────────────────────────
  /** Interpolate a yaw angle the short way around the circle (handles the
   *  ±π wrap so a spinning opponent never snaps the long way). */
  private static angleLerp(a: number, b: number, k: number): number {
    let d = b - a;
    while (d > Math.PI) d -= Math.PI * 2;
    while (d < -Math.PI) d += Math.PI * 2;
    return a + d * k;
  }

  /** Procedural texture: concrete with panel seams + speckle grain (zero assets). */
  private static concreteTexture(base: string, seam: string, panels: number, grain: number): THREE.CanvasTexture {
    const c = document.createElement("canvas");
    c.width = c.height = 128;
    const g = c.getContext("2d")!;
    g.fillStyle = base;
    g.fillRect(0, 0, 128, 128);
    for (let i = 0; i < 900; i++) {
      const v = Math.floor(Math.random() * grain);
      g.fillStyle = `rgba(${v},${v},${v + 10},0.16)`;
      g.fillRect(Math.floor(Math.random() * 128), Math.floor(Math.random() * 128), 2, 2);
    }
    g.strokeStyle = seam;
    g.lineWidth = 2;
    const step = 128 / panels;
    for (let i = 0; i <= panels; i++) {
      g.beginPath(); g.moveTo(0, i * step); g.lineTo(128, i * step); g.stroke();
      g.beginPath(); g.moveTo(i * step, 0); g.lineTo(i * step, 128); g.stroke();
    }
    const tex = new THREE.CanvasTexture(c);
    tex.wrapS = tex.wrapT = THREE.RepeatWrapping;
    tex.magFilter = THREE.NearestFilter;
    return tex;
  }

  private buildWorld(): void {
    // Krunker-flavored daylight: visible sky, bright sun, hard retro shadows.
    const sky = new THREE.Color(0x9ec9ef);
    this.scene.background = sky;
    // fog scales with the arena: streets stay crisp, horizon hazes
    this.scene.fog = new THREE.Fog(sky, ARENA_HALF * 1.4, ARENA_HALF * 4);
    this.scene.add(new THREE.HemisphereLight(0xbfd9f5, 0x6e6a5e, 0.85));
    this.scene.add(new THREE.AmbientLight(0xffffff, 0.18));
    const sun = new THREE.DirectionalLight(0xfff3da, 1.5);
    sun.position.set(16, 28, 12);
    sun.castShadow = true;
    // 1024 is plenty now that the map bakes once and stays put (was 2048, re-rendered
    // every frame). Halving each axis quarters the shadow-pass fill cost.
    sun.shadow.mapSize.set(1024, 1024);
    // shadow frustum covers the WHOLE arena (was ±18 — shadows used to pop
    // out near the edges of the map)
    const sh = ARENA_HALF + 6;
    sun.shadow.camera.left = sun.shadow.camera.bottom = -sh;
    sun.shadow.camera.right = sun.shadow.camera.top = sh;
    sun.shadow.camera.near = 2;
    sun.shadow.camera.far = 90;
    this.scene.add(sun);

    // sun-bleached asphalt with pavement seams (procedural)
    const tex = Game.concreteTexture("#8d9099", "#797d88", 4, 120);
    tex.repeat.set(ARENA_HALF, ARENA_HALF);
    const floor = new THREE.Mesh(
      new THREE.PlaneGeometry(ARENA_HALF * 2, ARENA_HALF * 2),
      new THREE.MeshLambertMaterial({ map: tex }),
    );
    floor.rotation.x = -Math.PI / 2;
    floor.receiveShadow = true;
    this.scene.add(floor);
  }

  /** Build (or rebuild) the per-match map geometry from a generated arena.
   *  Disposes the previous map so seed-after-seed play never leaks GPU memory. */
  private loadArena(arena: Arena): void {
    this.arena = arena;
    this.liveWalls = [...arena.walls];
    this.barrelIntact = arena.barrels.map(() => true);
    this.barrelMeshes = arena.barrels.map(() => null);
    this.barrelByBox = new Map(arena.barrels.map((b, i) => [arena.walls[b.wallIndex], i]));
    this.practicePickups = arena.pickups.map(() => ({ active: true, respawnAt: 0 }));
    this.resetPools(); // park any in-flight tracers/sparks/booms from the last map
    if (this.mapGroup) {
      this.scene.remove(this.mapGroup);
      this.mapGroup.traverse((o) => {
        const m = o as THREE.Mesh;
        m.geometry?.dispose?.();
        if (m.material) (Array.isArray(m.material) ? m.material : [m.material]).forEach((x) => x.dispose());
      });
    }
    const group = new THREE.Group();
    const rng = seededRandom(`${arena.seed}-paint`);
    // pastel city palette — pick two building hues per map
    const PASTELS = [0xe8d8b8, 0xd9c4ad, 0xc9d6c2, 0xc4cede, 0xdcc6c6, 0xcfd8c0];
    const hueA = PASTELS[Math.floor(rng() * PASTELS.length)];
    const hueB = PASTELS[Math.floor(rng() * PASTELS.length)];
    const perimTex = Game.concreteTexture("#aeb6c4", "#9aa2b2", 2, 140);
    const buildTex = Game.concreteTexture("#ffffff", "#d8d2c4", 2, 150);
    const crateTex = Game.concreteTexture("#caa36a", "#a8814c", 2, 110);
    const perimMat = new THREE.MeshLambertMaterial({ map: perimTex });
    const roofMat = new THREE.MeshLambertMaterial({ color: 0x4b5364 });
    const buildMatA = new THREE.MeshLambertMaterial({ map: buildTex, color: hueA });
    const buildMatB = new THREE.MeshLambertMaterial({ map: buildTex, color: hueB });
    // polygonOffset pushes the faces back a hair in the depth buffer so the
    // edge trim wins cleanly — without it the lines z-fight (patchy color)
    const crateMat = new THREE.MeshLambertMaterial({
      map: crateTex,
      polygonOffset: true,
      polygonOffsetFactor: 1,
      polygonOffsetUnits: 1,
    });
    const accentTrim = new THREE.LineBasicMaterial({ color: arena.accent });
    const darkTrim = new THREE.LineBasicMaterial({ color: 0x6b542f }); // crate-tone edge
    // explosive barrels: unmistakably red, with a hazard band — "shoot me"
    const barrelMat = new THREE.MeshLambertMaterial({ color: 0xc23230 });
    const barrelBandMat = new THREE.MeshLambertMaterial({ color: 0x2a2622 });

    // Batch every static wall into ONE mesh per material instead of one mesh
    // (plus a trim LineSegments) PER wall. Quake's lesson: a draw call per brush
    // is what chokes the GPU/driver — a dense map drops from hundreds of calls to
    // a handful. Barrels stay individual: they're destructible, so they must
    // disappear independently at runtime.
    const solidBuckets = new Map<THREE.Material, THREE.BufferGeometry[]>();
    const bucket = (m: THREE.Material, g: THREE.BufferGeometry) => {
      const a = solidBuckets.get(m);
      if (a) a.push(g);
      else solidBuckets.set(m, [g]);
    };
    const accentEdges: THREE.BufferGeometry[] = [];
    const darkEdges: THREE.BufferGeometry[] = [];

    arena.walls.forEach((b, i) => {
      const barrelIdx = this.barrelByBox.get(b);
      if (barrelIdx !== undefined) {
        // rendered round at last — and visually distinct from safe cover
        const r = (b.x1 - b.x0) / 2;
        const h = b.y1 - b.y0;
        const grp = new THREE.Group();
        const drum = new THREE.Mesh(new THREE.CylinderGeometry(r, r, h, 10), barrelMat);
        drum.position.y = h / 2;
        drum.castShadow = true;
        const band = new THREE.Mesh(new THREE.CylinderGeometry(r * 1.04, r * 1.04, h * 0.16, 10), barrelBandMat);
        band.position.y = h * 0.55;
        const lid = new THREE.Mesh(new THREE.CylinderGeometry(r * 0.9, r * 0.9, 0.06, 10), barrelBandMat);
        lid.position.y = h + 0.03;
        grp.add(drum, band, lid);
        grp.position.set((b.x0 + b.x1) / 2, b.y0, (b.z0 + b.z1) / 2);
        group.add(grp);
        this.barrelMeshes[barrelIdx] = grp;
        return;
      }
      const w = b.x1 - b.x0;
      const h = b.y1 - b.y0;
      const d = b.z1 - b.z0;
      const cx = (b.x0 + b.x1) / 2;
      const cy = (b.y0 + b.y1) / 2;
      const cz = (b.z0 + b.z1) / 2;
      // paint by geometry: perimeter | roof | building wall (A/B by side) | crate
      const isRoof = b.y0 >= 2.5;
      const isBuilding = b.y1 > 2.5 || (b.y0 === 0 && b.y1 === 1.3); // walls, headers, sills
      const mat =
        i < 4 ? perimMat : isRoof ? roofMat : isBuilding ? ((b.x0 + b.z0) > 0 ? buildMatA : buildMatB) : crateMat;
      const geo = new THREE.BoxGeometry(w, h, d).translate(cx, cy, cz); // pre-baked world pos
      bucket(mat, geo);
      if (i >= 4 && !isRoof && !isBuilding) {
        // edge trim on street furniture only — subtle dark outline on bulky
        // cover, the map's accent color reserved for small props. Bake the 1.003
        // "sit just outside the faces" scale into the geometry so it survives the
        // merge (no per-instance transform after batching).
        const small = w <= 1.3 && d <= 1.3;
        const eg = new THREE.EdgesGeometry(new THREE.BoxGeometry(w, h, d))
          .scale(1.003, 1.003, 1.003)
          .translate(cx, cy, cz);
        (small ? accentEdges : darkEdges).push(eg);
      }
    });

    for (const [mat, geos] of solidBuckets) {
      const mesh = new THREE.Mesh(mergeGeometries(geos), mat);
      mesh.castShadow = mat !== perimMat; // perimeter (i<4) never cast, matching the old per-wall flag
      mesh.receiveShadow = true;
      group.add(mesh);
    }
    if (accentEdges.length) group.add(new THREE.LineSegments(mergeGeometries(accentEdges), accentTrim));
    if (darkEdges.length) group.add(new THREE.LineSegments(mergeGeometries(darkEdges), darkTrim));

    // voxel clouds — decoration only, deterministic per map, all merged into one
    // mesh (they never move or change)
    const cloudMat = new THREE.MeshLambertMaterial({ color: 0xffffff, fog: false });
    const cloudGeos: THREE.BufferGeometry[] = [];
    for (let i = 0; i < 9; i++) {
      const cx = (rng() * 2 - 1) * ARENA_HALF * 2.6;
      const cy = 18 + rng() * 12;
      const cz = (rng() * 2 - 1) * ARENA_HALF * 2.6;
      const puffs = 2 + Math.floor(rng() * 3);
      for (let j = 0; j < puffs; j++) {
        const w = 3 + rng() * 4;
        cloudGeos.push(
          new THREE.BoxGeometry(w, 1.2 + rng(), 2 + rng() * 2).translate(
            cx + (rng() * 2 - 1) * 3,
            cy + (rng() - 0.5),
            cz + (rng() * 2 - 1) * 2,
          ),
        );
      }
    }
    group.add(new THREE.Mesh(mergeGeometries(cloudGeos), cloudMat));

    // health pickups: glowing retro crosses (visibility driven by snapshots in
    // duels; always shown in practice as scenery)
    this.pickupMeshes = arena.pickups.map((pk) => {
      const cross = new THREE.Group();
      const mat = new THREE.MeshLambertMaterial({ color: 0x2bff88, emissive: 0x0fae4e });
      const a = new THREE.Mesh(new THREE.BoxGeometry(0.7, 0.22, 0.22), mat);
      const b2 = new THREE.Mesh(new THREE.BoxGeometry(0.22, 0.7, 0.22), mat);
      cross.add(a, b2);
      cross.position.set(pk.x, 1.0, pk.z);
      group.add(cross);
      return cross;
    });

    this.mapGroup = group;
    this.scene.add(group);
    // Bake the static shadows ONCE for this map (autoUpdate is off). Three.js
    // renders the shadow pass on the next frame, then clears the flag itself.
    this.renderer.shadowMap.needsUpdate = true;
  }

  /** Boxy avatar holding a rifle — elongated cubes for torso/head/limbs, rigged
   *  Minecraft-style: each arm/leg is a rigid box hanging from a pivot at the
   *  joint, so animateAvatar() can swing it. Forward is -Z. Roughly fills the
   *  server's ~1.7m hitbox. The animated limbs are separate meshes (can't be
   *  baked into one merge), so this is ~8 draw calls vs the static 4 — fine for
   *  1v1; revisit with a skinned mesh / instancing for large team modes. */
  private makeAvatar(color: number): THREE.Group {
    const root = new THREE.Group();
    const mat = new THREE.MeshLambertMaterial({ color });
    const gunMat = new THREE.MeshLambertMaterial({ color: 0x23262e });

    const body = new THREE.Group(); // torso + head + limbs; bobs while running
    root.add(body);

    // torso + head are one merged box mesh — they never move independently
    const torso = new THREE.BoxGeometry(0.42, 0.62, 0.26).translate(0, 1.14, 0);
    const head = new THREE.BoxGeometry(0.34, 0.34, 0.32).translate(0, 1.62, 0);
    const torsoHead = new THREE.Mesh(mergeGeometries([torso, head]), mat);
    body.add(torsoHead);

    // dark face patch so you can read which way they're looking (forward = -Z)
    const face = new THREE.Mesh(
      new THREE.BoxGeometry(0.2, 0.1, 0.06),
      new THREE.MeshBasicMaterial({ color: 0x0a0a12 }),
    );
    face.position.set(0, 1.64, -0.17);
    body.add(face);

    // a limb: an empty pivot AT the joint with an elongated cube hanging below,
    // so rotating the pivot.x swings the limb forward/back from the joint
    const limb = (jointX: number, jointY: number, w: number, len: number): THREE.Object3D => {
      const pivot = new THREE.Object3D();
      pivot.position.set(jointX, jointY, 0);
      const m = new THREE.Mesh(new THREE.BoxGeometry(w, len, w), mat);
      m.position.y = -len / 2; // hang below the joint
      pivot.add(m);
      body.add(pivot);
      return pivot;
    };
    const lleg = limb(-0.12, 0.86, 0.17, 0.84); // feet ~ground at full extension
    const rleg = limb(0.12, 0.86, 0.17, 0.84);

    // RIGHT (firing) arm: two-bone (shoulder → upper → elbow → forearm → hand).
    // Posed by the carry/aim joints; its hand carries the rifle.
    const shoulder = new THREE.Object3D();
    shoulder.position.set(0.27, 1.42, 0);
    const rUpper = new THREE.Mesh(new THREE.BoxGeometry(0.13, 0.3, 0.13), mat);
    rUpper.position.y = -0.15;
    shoulder.add(rUpper);
    const elbow = new THREE.Object3D();
    elbow.position.y = -0.3;
    shoulder.add(elbow);
    const rFore = new THREE.Mesh(new THREE.BoxGeometry(0.115, 0.3, 0.115), mat);
    rFore.position.y = -0.15;
    elbow.add(rFore);
    const hand = new THREE.Object3D();
    hand.position.y = -0.32; // fingertips — the gun is glued here
    elbow.add(hand);
    body.add(shoulder);

    // rifle is a child of BODY (barrel along -Z = forward, magazine down), so its
    // orientation is always correct; animateAvatar glues its POSITION to the right
    // hand each frame, so it tracks the grip without ever floating or twisting.
    const part = (w: number, h: number, d: number, x: number, y: number, z: number) =>
      new THREE.BoxGeometry(w, h, d).translate(x, y, z);
    const rifle = new THREE.Mesh(
      mergeGeometries([
        part(0.07, 0.11, 0.55, 0, 0, -0.2), // receiver
        part(0.045, 0.045, 0.4, 0, 0.015, -0.66), // barrel
        part(0.05, 0.16, 0.09, 0, -0.12, -0.12), // magazine
        part(0.06, 0.12, 0.2, 0, -0.025, 0.18), // stock
      ]),
      gunMat,
    );
    body.add(rifle);

    // foregrip anchor + a left-hand block ON the rifle handguard — part of the gun,
    // so the support hand is perfectly placed however the gun moves.
    const foregrip = new THREE.Object3D();
    foregrip.position.set(0, -0.02, -0.4);
    const lHandBlk = new THREE.Mesh(new THREE.BoxGeometry(0.13, 0.13, 0.15), mat);
    foregrip.add(lHandBlk);
    rifle.add(foregrip);

    // LEFT (support) arm: a single STRETCHY bone from the shoulder, oriented +
    // scaled every frame to reach the foregrip anchor — anchored to the rifle the
    // same way the right hand holds the grip, so it can never hang off in space.
    const larm = new THREE.Object3D();
    larm.position.set(-0.27, 1.42, 0);
    const larmBox = new THREE.Mesh(new THREE.BoxGeometry(0.12, 1, 0.12), mat); // height 1 → scale.y = length
    larmBox.position.y = -0.25;
    larmBox.scale.y = 0.5;
    larm.add(larmBox);
    body.add(larm);

    const rig: AvatarRig = {
      body, lleg, rleg,
      rsh: shoulder, rel: elbow, hand, gun: rifle,
      larm, larmBox, foregrip,
      phase: 0, speed: 0, aim: 0, recoil: 0, fireUntil: 0, px: 0, pz: 0, init: false,
    };
    Game.applyWeaponPose(rig, 0, 0, 0); // seat the firing arm in the carry pose

    // Blob shadow on the GROUND (child of root, not body, so it doesn't bob).
    // Players move, so they can't bake into the frozen static shadow map.
    const blob = new THREE.Mesh(
      new THREE.CircleGeometry(0.42, 16),
      new THREE.MeshBasicMaterial({ color: 0x000000, transparent: true, opacity: 0.28, depthWrite: false }),
    );
    blob.rotation.x = -Math.PI / 2;
    blob.position.y = 0.02;
    root.add(blob);

    root.userData.rig = rig;
    return root;
  }

  /** Pose the arms by blending the carry pose (a=0) to the aim pose (a=1), with a
   *  recoil kick and a small shoulder sway. The gun is parented to the right hand
   *  so it follows automatically — recoil jerks the whole right arm (and gun)
   *  back/up. Both end poses keep the hands on the gun, so any blend holds. */
  private static applyWeaponPose(rig: AvatarRig, a: number, recoil: number, sway: number): void {
    const L = (k: keyof typeof POSE_CARRY) => POSE_CARRY[k] + (POSE_AIM[k] - POSE_CARRY[k]) * a;
    rig.rsh.rotation.set(L("rshx") + sway - recoil * 0.28, L("rshy"), 0); // recoil kicks the gun arm
    rig.rel.rotation.set(L("relx") + recoil * 0.18, L("rely"), 0);
    // the LEFT arm isn't posed here — it's IK'd to the foregrip in animateAvatar
  }

  /** Kick an avatar into its shoulder/aim pose with a recoil pulse — called when
   *  that avatar fires (opponent snapshots / attract bots). */
  private triggerShot(root: THREE.Object3D): void {
    const rig = root.userData.rig as AvatarRig | undefined;
    if (!rig) return;
    rig.fireUntil = performance.now() + FIRE_HOLD_MS;
    rig.recoil = 1;
  }

  /** Drive a boxy avatar's walk/run/jump + weapon state from its motion. `x,z` =
   *  current world position (speed is the frame-to-frame delta), `y` = height
   *  (airborne when off the ground), `now` = performance.now() for the fire
   *  timer. Purely cosmetic — no server data needed beyond the shot events that
   *  call triggerShot(). */
  private animateAvatar(root: THREE.Object3D, x: number, z: number, y: number, dt: number, now: number): void {
    const rig = root.userData.rig as AvatarRig | undefined;
    if (!rig) return;
    if (!rig.init) {
      rig.px = x;
      rig.pz = z;
      rig.init = true;
    }
    const raw = Math.hypot(x - rig.px, z - rig.pz) / Math.max(dt, 1e-3);
    rig.px = x;
    rig.pz = z;
    // low-pass the speed so interpolation jitter doesn't make the gait stutter
    rig.speed += (Math.min(raw, 9) - rig.speed) * Math.min(1, dt * 12);
    const ease = Math.min(1, dt * 12);

    // ── legs + torso bob (independent of the weapon state) ──
    let sway = 0;
    if (y > 0.12) {
      rig.lleg.rotation.x += (-0.5 - rig.lleg.rotation.x) * ease; // airborne tuck
      rig.rleg.rotation.x += (0.5 - rig.rleg.rotation.x) * ease;
      rig.body.position.y += (0 - rig.body.position.y) * ease;
    } else if (rig.speed > 0.4) {
      const amp = Math.min(0.25 + rig.speed * 0.09, 0.8);
      rig.phase += rig.speed * dt * 2.2;
      const s = Math.sin(rig.phase);
      rig.lleg.rotation.x = s * amp; // alternating gait
      rig.rleg.rotation.x = -s * amp;
      rig.body.position.y = Math.abs(s) * 0.05; // bob
      sway = s * amp * 0.1; // shoulders sway a touch while running
    } else {
      rig.lleg.rotation.x += (0 - rig.lleg.rotation.x) * ease;
      rig.rleg.rotation.x += (0 - rig.rleg.rotation.x) * ease;
      rig.body.position.y += (0 - rig.body.position.y) * ease;
    }

    // ── weapon state: blend carry → aim while firing, then decay back ──
    const aimTarget = now < rig.fireUntil ? 1 : 0;
    rig.aim += (aimTarget - rig.aim) * Math.min(1, dt * 10);
    rig.recoil += (0 - rig.recoil) * Math.min(1, dt * 14); // kick decays fast
    Game.applyWeaponPose(rig, rig.aim, rig.recoil, sway);

    // glue the gun's POSITION to the right hand (so it never floats) while keeping
    // its rotation in body space (so the barrel always points forward). Muzzle
    // rides slightly down in carry, level in aim, kicks up on recoil.
    rig.hand.updateWorldMatrix(true, false);
    rig.hand.getWorldPosition(this.avTmp);
    rig.body.worldToLocal(this.avTmp);
    rig.gun.position.copy(this.avTmp);
    rig.gun.rotation.set(0.14 * (1 - rig.aim) - rig.recoil * 0.35, 0, 0);

    // anchor the LEFT (support) arm to the rifle foregrip — orient the bone from
    // the shoulder toward the anchor and stretch it to reach, so the support hand
    // is glued to the handguard exactly like the right hand holds the grip
    rig.foregrip.updateWorldMatrix(true, false);
    rig.foregrip.getWorldPosition(this.avTmp);
    rig.body.worldToLocal(this.avTmp);
    const sp = rig.larm.position;
    this.avTmp2.set(this.avTmp.x - sp.x, this.avTmp.y - sp.y, this.avTmp.z - sp.z);
    const reach = this.avTmp2.length() || 1e-3;
    this.avTmp2.multiplyScalar(1 / reach);
    rig.larm.quaternion.setFromUnitVectors(DOWN, this.avTmp2);
    rig.larmBox.scale.y = reach;
    rig.larmBox.position.y = -reach / 2;
  }

  // ── lifecycle ────────────────────────────────────────────────────────────
  startPractice(): void {
    this.stopAttract();
    this.mode = "practice";
    this.practiceHits = 0;
    this.practiceHealth = MAX_HEALTH;
    this.loadArena(generateArena(randomSeed())); // fresh map every session
    const spawn = this.arena.spawns[0];
    this.spawnSelf(spawn);
    this.clearTargets();
    for (let i = 0; i < 3; i++) this.addTarget(this.openTargetSpot());
    this.hud.show();
    this.hud.setScore("TARGETS DOWN: 0");
    this.hud.setTimeLeft(0);
    this.hud.setHealth(MAX_HEALTH, MAX_HEALTH);
    this.hud.setBanner(null);
    this.run();
  }

  startDuel(
    selfId: string,
    players: { id: string; name: string }[],
    countdownMs: number,
    mapSeed: string,
  ): void {
    this.stopAttract();
    this.mode = "duel";
    this.selfId = selfId;
    this.clutchOn = false;
    this.lastPhase = null;
    this.loadArena(generateArena(mapSeed)); // identical to the server's arena
    this.clearTargets();
    this.clearRemotes();
    this.prevPlayers.clear();
    this.selfAlive = true;
    // assign team colors by roster order (0 red, 1 blue)
    this.playerColors.clear();
    players.forEach((p, i) => this.playerColors.set(p.id, TEAM_COLORS[i] ?? TEAM_COLORS[0]));
    const selfIndex = players.findIndex((p) => p.id === selfId);
    this.spawnSelf(this.arena.spawns[selfIndex === -1 ? 0 : selfIndex]);
    // remote avatars are created lazily by applySnapshot()
    this.hud.show();
    this.voice.startMatch();
    this.micPill.classList.remove("hidden");
    this.hud.setScore("0 — 0");
    this.hud.setHealth(MAX_HEALTH, MAX_HEALTH);
    let left = Math.ceil(countdownMs / 1000);
    this.hud.setBanner(`DUEL STARTS IN ${left}`);
    this.sfx.countdown();
    const iv = setInterval(() => {
      left -= 1;
      if (left <= 0) {
        clearInterval(iv);
        this.hud.setBanner(null);
        this.sfx.startHorn();
      } else {
        this.hud.setBanner(`DUEL STARTS IN ${left}`);
        this.sfx.countdown();
      }
    }, 1000);
    this.run();
  }

  uiClick(): void {
    this.sfx.uiClick();
  }

  /** Show/clear a HUD banner from the network layer (reconnect / opponent-dropped
   *  prompts). Accepts the same HTML the in-game banners use. */
  showStatusBanner(html: string | null): void {
    this.hud.setBanner(html);
  }

  /** Unlock audio from the first user gesture (mobile autoplay policy). Primes
   *  both the SFX context and the voice PLAYBACK context — the latter so a
   *  player can hear proximity voice even if they never grant their own mic. */
  unlockAudio(): void {
    this.sfx.unlock();
    this.voice.unlockOutput();
  }

  stop(): void {
    this.running = false;
    this.replayMode = false;
    this.clutchOn = false;
    this.sfx.stopMusic();
    this.voice.endMatch();
    this.micPill.classList.add("hidden");
    this.speakRing.classList.add("hidden");
    this.hud.hide();
    document.exitPointerLock?.();
  }

  // ── attract mode (menu backdrop) ─────────────────────────────────────────
  /** The menu IS a match preview: two scripted stick figures strafe and trade
   *  tracers on a fresh procedural map while the camera slowly orbits. */
  startAttract(): void {
    if (this.attractOn || this.running) return;
    this.attractOn = true;
    this.loadArena(generateArena(randomSeed()));
    const rng = seededRandom(`${this.arena.seed}-attract`);
    const a = findOpenSpot(this.arena, rng, []);
    const b = findOpenSpot(this.arena, rng, [{ x: a.x, z: a.z, r: 12 }]);
    for (const { spot, color } of [
      { spot: a, color: TEAM_COLORS[0] },
      { spot: b, color: TEAM_COLORS[1] },
    ]) {
      const avatar = this.makeAvatar(color);
      this.scene.add(avatar);
      this.attractBots.push({
        avatar,
        body: { x: spot.x, y: 0, z: spot.z, vy: 0, onGround: true },
        yaw: 0,
        strafe: rng() < 0.5 ? -1 : 1,
        flipAt: 0,
        shootAt: performance.now() + 800 + rng() * 800,
      });
    }
    this.attractLast = performance.now();
    const loop = (now: number) => {
      if (!this.attractOn) return;
      const dt = Math.min(0.05, (now - this.attractLast) / 1000);
      this.attractLast = now;
      this.attractStep(now, dt);
      requestAnimationFrame(loop);
    };
    requestAnimationFrame(loop);
  }

  stopAttract(): void {
    if (!this.attractOn) return;
    this.attractOn = false;
    for (const bot of this.attractBots) this.scene.remove(bot.avatar);
    this.attractBots = [];
  }

  private attractStep(now: number, dt: number): void {
    const [A, B] = this.attractBots;
    for (const [me, foe] of [
      [A, B],
      [B, A],
    ] as const) {
      const dx = foe.body.x - me.body.x;
      const dz = foe.body.z - me.body.z;
      me.yaw = Math.atan2(-dx, -dz); // face the opponent (forward is -Z)
      if (now >= me.flipAt) {
        me.strafe = Math.random() < 0.5 ? -1 : 1;
        me.flipAt = now + 900 + Math.random() * 1600;
      }
      const dist = Math.hypot(dx, dz);
      const input: PlayerInput = {
        sequence: this.attractSeq++,
        forward: dist > 14,
        backward: dist < 6,
        left: me.strafe < 0,
        right: me.strafe > 0,
        jump: Math.random() < dt * 0.25, // an occasional hop
        shoot: false,
        yaw: me.yaw,
        pitch: 0,
      };
      stepBody(me.body, input, dt, this.liveWalls); // real physics — no clipping
      me.avatar.position.set(me.body.x, me.body.y, me.body.z);
      me.avatar.rotation.y = me.yaw;
      this.animateAvatar(me.avatar, me.body.x, me.body.z, me.body.y, dt, now);

      if (now >= me.shootAt) {
        me.shootAt = now + 500 + Math.random() * 1100;
        this.triggerShot(me.avatar); // shoulder + recoil while firing
        const o = { x: me.body.x, y: me.body.y + 1.3, z: me.body.z };
        const aim = {
          x: foe.body.x + (Math.random() - 0.5) * 1.4 - o.x,
          y: foe.body.y + 0.6 + Math.random() * 0.9 - o.y,
          z: foe.body.z + (Math.random() - 0.5) * 1.4 - o.z,
        };
        const len = Math.hypot(aim.x, aim.y, aim.z) || 1;
        const d = { x: aim.x / len, y: aim.y / len, z: aim.z / len };
        const wallT = nearestWallT(o, d, this.liveWalls);
        const t = Math.min(wallT ?? len, len); // tracers stop at cover
        this.addTracer(o, { x: o.x + d.x * t, y: o.y + d.y * t, z: o.z + d.z * t });
      }
    }

    // slow cinematic orbit above the rooftops, drifting toward the action
    const ang = now * 0.00009;
    const r = ARENA_HALF * 0.9;
    this.camera.position.set(
      Math.cos(ang) * r,
      11 + Math.sin(now * 0.00013) * 2.5,
      Math.sin(ang) * r,
    );
    this.camera.lookAt((A.body.x + B.body.x) / 2, 1.2, (A.body.z + B.body.z) / 2);

    this.updateEffects(dt);
    this.renderer.render(this.scene, this.camera);
  }

  private spawnSelf(spawn: { x: number; z: number; yaw: number }): void {
    this.body = { x: spawn.x, y: 0, z: spawn.z, vy: 0, onGround: true };
    this.input.setView(spawn.yaw, 0);
  }

  private run(): void {
    if (this.running) return;
    this.running = true;
    this.lastFrame = performance.now();
    const loop = (now: number) => {
      if (!this.running) return;
      const dt = Math.min(0.05, (now - this.lastFrame) / 1000);
      this.lastFrame = now;
      this.frame(now, dt);
      requestAnimationFrame(loop);
    };
    requestAnimationFrame(loop);
  }

  // ── per-frame ────────────────────────────────────────────────────────────
  private frame(now: number, dt: number): void {
    const input = this.input.frame();

    // predict / move self (dead players don't move)
    if (this.mode === "practice" || this.selfAlive) {
      const wasAirborne = !this.body.onGround;
      const fallSpeed = -this.body.vy;
      stepBody(this.body, input, dt, this.liveWalls);
      if (wasAirborne && this.body.onGround && fallSpeed > 4) this.sfx.land();
    }
    this.camera.position.set(this.body.x, this.body.y + PLAYER_EYE, this.body.z);
    this.camera.rotation.set(this.input.pitch, this.input.yaw, 0);

    if (this.mode === "practice") {
      if (input.shoot && now - this.lastLocalShot >= FIRE_COOLDOWN_MS) {
        this.lastLocalShot = now;
        this.practiceShot();
      }
      // health pickups work in practice too (locally authoritative) — walk
      // over a cross while hurt and it heals, then respawns on the same
      // timer as a real duel
      this.arena.pickups.forEach((pk, i) => {
        const st = this.practicePickups[i];
        if (!st) return;
        if (!st.active) {
          if (now >= st.respawnAt) {
            st.active = true;
            if (this.pickupMeshes[i]) this.pickupMeshes[i].visible = true;
          }
          return;
        }
        if (this.practiceHealth >= MAX_HEALTH) return; // full HP: not consumed
        const dsq = (this.body.x - pk.x) ** 2 + (this.body.z - pk.z) ** 2;
        if (dsq < PICKUP_RADIUS * PICKUP_RADIUS) {
          this.practiceHealth = Math.min(MAX_HEALTH, this.practiceHealth + PICKUP_HEAL);
          this.hud.setHealth(this.practiceHealth, MAX_HEALTH);
          this.sfx.heal();
          st.active = false;
          st.respawnAt = now + PICKUP_RESPAWN_MS;
          if (this.pickupMeshes[i]) this.pickupMeshes[i].visible = false;
        }
      });
    } else {
      // ship inputs to the server at ~30Hz
      if (now - this.lastSent >= SEND_INTERVAL_MS && this.onInput) {
        this.lastSent = now;
        this.onInput(input);
      }
      this.interpolateRemotes(now, dt);
    }

    this.updateEffects(dt);

    // desktop: nudge the player to capture the mouse when it isn't
    this.lockHint.classList.toggle("hidden", !this.running || this.input.locked);

    if (this.mode === "duel") this.updateVoiceUi();

    this.renderer.render(this.scene, this.camera);
  }

  /** Position + animate every opponent avatar from its buffered snapshot history,
   *  rendered INTERP_DELAY_MS in the past. Shared by live play and replay. */
  private interpolateRemotes(now: number, dt: number): void {
    const renderT = now - INTERP_DELAY_MS;
    for (const r of this.remotes.values()) {
      const buf = r.buf;
      if (buf.length === 0) continue;
      // two samples bracketing renderT; clamp (hold) at the ends so a late packet
      // pauses on the last segment rather than teleporting
      let a = buf[0];
      let b = buf[buf.length - 1];
      if (renderT <= a.t) b = a;
      else if (renderT >= b.t) a = b;
      else {
        for (let i = 0; i < buf.length - 1; i++) {
          if (renderT >= buf[i].t && renderT <= buf[i + 1].t) {
            a = buf[i];
            b = buf[i + 1];
            break;
          }
        }
      }
      const k = Math.max(0, Math.min(1, (renderT - a.t) / ((b.t - a.t) || 1)));
      const ix = a.x + (b.x - a.x) * k;
      const iy = a.y + (b.y - a.y) * k;
      const iz = a.z + (b.z - a.z) * k;
      r.group.position.set(ix, iy, iz);
      r.group.rotation.y = Game.angleLerp(a.yaw, b.yaw, k);
      this.animateAvatar(r.group, ix, iz, iy, dt, now);
    }
  }

  // ── replay (spectator playback) ────────────────────────────────────────────
  /** Play back a recorded match: rebuild the arena from the seed and feed the
   *  decoded snapshot stream through applySnapshot on its own clock, under a slow
   *  orbit camera. selfId = null, so BOTH duelists render as remote avatars. */
  startReplay(
    meta: { mapSeed: string; players: { id: string; name: string }[]; countdownMs: number },
    frames: { t: number; bytes: Uint8Array }[],
  ): void {
    this.stopAttract();
    this.running = false;
    this.replayMode = true;
    this.mode = "duel";
    this.selfId = null;
    this.loadArena(generateArena(meta.mapSeed));
    this.clearTargets();
    this.clearRemotes();
    this.prevPlayers.clear();
    this.resetPools();
    this.hud.show();
    this.hud.setHealth(MAX_HEALTH, MAX_HEALTH);
    this.hud.setBanner(`▶ REPLAY · ${meta.players.map((p) => p.name).join("  vs  ")}`);
    const dec = new SnapshotDecoder();
    this.replaySnaps = frames.map((f) => ({
      t: f.t,
      snap: dec.decode(f.bytes.slice().buffer as ArrayBuffer),
    }));
    this.replayIdx = 0;
    this.replayStart = performance.now();
    this.replayLast = this.replayStart;
    const loop = (now: number) => {
      if (!this.replayMode) return;
      this.replayStep(now);
      requestAnimationFrame(loop);
    };
    requestAnimationFrame(loop);
  }

  stopReplay(): void {
    this.replayMode = false;
    this.replaySnaps = [];
    this.hud.hide();
  }

  private replayStep(now: number): void {
    const dt = Math.min(0.05, (now - this.replayLast) / 1000);
    this.replayLast = now;
    const elapsed = now - this.replayStart;
    while (this.replayIdx < this.replaySnaps.length && this.replaySnaps[this.replayIdx].t <= elapsed) {
      this.applySnapshot(this.replaySnaps[this.replayIdx].snap);
      this.replayIdx++;
    }
    if (this.replayIdx >= this.replaySnaps.length) this.hud.setBanner("▶ REPLAY ENDED — tap MENU");
    this.interpolateRemotes(now, dt);

    // slow orbit around the midpoint of the duelists
    let cx = 0;
    let cz = 0;
    let n = 0;
    for (const r of this.remotes.values()) {
      cx += r.group.position.x;
      cz += r.group.position.z;
      n++;
    }
    if (n > 0) {
      cx /= n;
      cz /= n;
    }
    const ang = now * 0.00012;
    const rad = ARENA_HALF * 0.7;
    this.camera.position.set(cx + Math.cos(ang) * rad, 9 + Math.sin(now * 0.00015) * 2, cz + Math.sin(ang) * rad);
    this.camera.rotation.set(0, 0, 0);
    this.camera.lookAt(cx, 1.2, cz);
    this.updateEffects(dt);
    this.renderer.render(this.scene, this.camera);
  }

  /** Tracer fade, spark shrink, pickup spin — shared by play and attract loops.
   *  Walks the fixed FX pools in place; nothing is added/removed/disposed. */
  private updateEffects(dt: number): void {
    for (const t of this.tracerPool) {
      if (t.ttl <= 0) continue;
      t.ttl -= dt;
      (t.mesh.material as THREE.LineBasicMaterial).opacity = Math.max(0, t.ttl / 0.12);
      if (t.ttl <= 0) t.mesh.visible = false;
    }

    for (const s of this.sparkPool) {
      if (s.ttl <= 0) continue;
      s.ttl -= dt;
      s.mesh.scale.setScalar(Math.max(0.001, s.ttl / 0.2));
      if (s.ttl <= 0) s.mesh.visible = false;
    }

    // pickups idle-spin (pure decoration; the server owns the pickup logic)
    for (const pk of this.pickupMeshes) pk.rotation.y += dt * 2.2;

    // explosion fireballs: expand fast, fade out
    for (const b of this.boomPool) {
      if (b.ttl <= 0) continue;
      b.ttl -= dt;
      const k = 1 - Math.max(0, b.ttl / 0.45);
      b.mesh.scale.setScalar(1 + k * (BARREL_RADIUS_DMG - 1));
      (b.mesh.material as THREE.MeshBasicMaterial).opacity = Math.max(0, b.ttl / 0.45) * 0.9;
      if (b.ttl <= 0) b.mesh.visible = false;
    }
  }

  // ── proximity voice ──────────────────────────────────────────────────────
  /** Request the mic from a user gesture (the homepage banner). */
  enableMic(): Promise<boolean> {
    return this.voice.requestMic();
  }
  micPermission(): "unknown" | "granted" | "denied" {
    return this.voice.permission;
  }
  /** Wire outgoing voice frames to the socket. */
  bindVoiceOut(send: (buf: ArrayBuffer) => void): void {
    this.voice.onFrame = send;
  }
  /** Feed an incoming voice frame received over the socket. */
  receiveVoiceFrame(buf: ArrayBuffer): void {
    this.voice.playFrame(buf);
  }
  /** Toggle your own mic (privacy). Returns the new muted state. */
  toggleSelfMute(): boolean {
    return this.voice.toggleSelfMute();
  }

  /** Live distance/bearing to the opponent, for Voice's fade/muffle/pan and
   *  send gating. Mirrors the explosion-panning math so audio cues agree. */
  voiceSpatial(): { dist: number; pan: number } | null {
    if (this.mode !== "duel") return null;
    const remote = this.remotes.values().next().value as RemoteAvatar | undefined;
    if (!remote) return null;
    const p = remote.group.position; // interpolated render position
    const dx = p.x - this.body.x;
    const dz = p.z - this.body.z;
    const srcYaw = Math.atan2(-dx, -dz);
    let diff = srcYaw - this.input.yaw;
    while (diff > Math.PI) diff -= Math.PI * 2;
    while (diff < -Math.PI) diff += Math.PI * 2;
    return { dist: Math.hypot(dx, dz), pan: -Math.sin(diff) };
  }

  /** Mic-state pill (OFF / DENIED / LIVE, lit while transmitting) + the
   *  opponent's speaking ring, refreshed each frame during a duel. */
  private updateVoiceUi(): void {
    const perm = this.voice.permission;
    const tx = this.voice.isTransmitting();
    this.micPill.textContent =
      perm === "denied"
        ? "MIC OFF"
        : perm !== "granted"
          ? "NO MIC"
          : this.voice.muted
            ? "MUTED"
            : tx
              ? "● LIVE"
              : "MIC ON";
    this.micPill.classList.toggle("denied", perm !== "granted");
    this.micPill.classList.toggle("muted", perm === "granted" && this.voice.muted);
    this.micPill.classList.toggle("live", perm === "granted" && !this.voice.muted && tx);

    // Speaking ring: only while the opponent is audibly talking and in range.
    const sp = this.voiceSpatial();
    const remote = this.remotes.values().next().value as RemoteAvatar | undefined;
    if (!this.voice.isReceiving() || !sp || sp.dist > 20 || !remote || !remote.group.visible) {
      this.speakRing.classList.add("hidden");
      return;
    }
    const p = remote.group.position;
    this.bubbleVec.set(p.x, p.y + 2.1, p.z).project(this.camera);
    if (this.bubbleVec.z > 1) {
      this.speakRing.classList.add("hidden"); // behind the camera
      return;
    }
    this.speakRing.style.left = `${(this.bubbleVec.x * 0.5 + 0.5) * 100}%`;
    this.speakRing.style.top = `${(-this.bubbleVec.y * 0.5 + 0.5) * 100}%`;
    this.speakRing.classList.remove("hidden");
  }

  // ── practice mode ────────────────────────────────────────────────────────
  private addTarget(at: { x: number; z: number }): void {
    const mesh = new THREE.Mesh(
      new THREE.BoxGeometry(0.8, 1.7, 0.4),
      new THREE.MeshLambertMaterial({ color: 0xff3355 }),
    );
    mesh.position.set(at.x, 0.85, at.z);
    this.scene.add(mesh);
    this.targets.push({
      mesh,
      alive: true,
      hp: MAX_HEALTH, // 4 hits, exactly like a duel opponent
      box: { x0: at.x - 0.4, x1: at.x + 0.4, y0: 0, y1: 1.7, z0: at.z - 0.2, z1: at.z + 0.2 },
    });
  }

  private openTargetSpot(): { x: number; z: number } {
    const avoid = [
      { x: this.body.x, z: this.body.z, r: 5 },
      ...this.targets.map((t) => ({ x: (t.box.x0 + t.box.x1) / 2, z: (t.box.z0 + t.box.z1) / 2, r: 2.5 })),
    ];
    return findOpenSpot(this.arena, Math.random, avoid);
  }

  private clearTargets(): void {
    for (const t of this.targets) this.scene.remove(t.mesh);
    this.targets = [];
  }

  private killTarget(t: PracticeTarget): void {
    t.alive = false;
    t.mesh.visible = false;
    this.practiceHits += 1;
    this.hud.setScore(`TARGETS DOWN: ${this.practiceHits}`);
    this.sfx.kill();
    setTimeout(() => {
      // pop back up somewhere new — keeps the range interesting
      const spot = this.openTargetSpot();
      t.mesh.position.set(spot.x, 0.85, spot.z);
      t.box = { x0: spot.x - 0.4, x1: spot.x + 0.4, y0: 0, y1: 1.7, z0: spot.z - 0.2, z1: spot.z + 0.2 };
      t.hp = MAX_HEALTH;
      (t.mesh.material as THREE.MeshLambertMaterial).color.setHex(0xff3355);
      t.alive = true;
      t.mesh.visible = true;
    }, 800);
  }

  private practiceShot(): void {
    this.sfx.shoot();
    const o: Vec3 = { x: this.body.x, y: this.body.y + PLAYER_EYE, z: this.body.z };
    const d = dirFromAngles(this.input.yaw, this.input.pitch);
    const wallHit = nearestWallHit(o, d, this.liveWalls);
    let bestT = wallHit !== null ? Math.min(wallHit.t, SHOT_RANGE) : SHOT_RANGE;
    let hit: PracticeTarget | null = null;
    for (const t of this.targets) {
      if (!t.alive) continue;
      const ht = rayBox(o, d, t.box); // what you see is what you hit
      if (ht !== null && ht < bestT) {
        bestT = ht;
        hit = t;
      }
    }
    this.addTracer(o, { x: o.x + d.x * bestT, y: o.y + d.y * bestT, z: o.z + d.z * bestT });
    if (hit) {
      hit.hp -= DAMAGE_PER_HIT;
      this.hud.hitMarker();
      // damage feedback: flash white, and fade toward dark as hp drops
      const mat = hit.mesh.material as THREE.MeshLambertMaterial;
      mat.emissive.setHex(0xffffff);
      setTimeout(() => mat.emissive.setHex(0x000000), 70);
      if (hit.hp > 0) {
        this.sfx.hit();
        mat.color.setHex([0xff3355, 0xd02a47, 0xa12039, 0x731628][4 - Math.ceil(hit.hp / DAMAGE_PER_HIT)] ?? 0xff3355);
      } else {
        this.killTarget(hit);
      }
      return;
    }
    // shot stopped on a wall — practice mode runs the explosion locally
    if (wallHit && wallHit.t <= SHOT_RANGE) {
      const barrelIdx = this.barrelByBox.get(this.liveWalls[wallHit.index]);
      if (barrelIdx !== undefined) this.practiceExplosion(barrelIdx);
    }
  }

  /** Practice-mode detonation: same rules as the server — falloff damage to
   *  self, knocks out targets in radius, chains to nearby barrels. */
  private practiceExplosion(idx: number): void {
    const centers = this.detonate(idx);
    if (centers.length === 0) return;
    this.playBoomSound(centers);
    for (const c of centers) {
      // self damage
      const d = Math.hypot(this.body.x - c.x, this.body.y + 0.9 - c.y, this.body.z - c.z);
      if (d <= BARREL_RADIUS_DMG) {
        const dmg = Math.round(BARREL_DMG_MAX - (BARREL_DMG_MAX - BARREL_DMG_MIN) * (d / BARREL_RADIUS_DMG));
        this.practiceHealth = Math.max(0, this.practiceHealth - dmg);
        this.hud.setHealth(this.practiceHealth, MAX_HEALTH);
        this.sfx.hurt();
        if (this.practiceHealth <= 0) {
          this.sfx.death();
          this.hud.setBanner("FRAGGED BY A BARREL — respawning…");
          setTimeout(() => {
            this.practiceHealth = MAX_HEALTH;
            this.hud.setHealth(MAX_HEALTH, MAX_HEALTH);
            this.hud.setBanner(null);
            this.spawnSelf(this.arena.spawns[0]);
          }, 1200);
        }
      }
      // targets in radius go down
      for (const t of this.targets) {
        if (!t.alive) continue;
        const tx = (t.box.x0 + t.box.x1) / 2;
        const tz = (t.box.z0 + t.box.z1) / 2;
        if (Math.hypot(tx - c.x, 0.85 - c.y, tz - c.z) <= BARREL_RADIUS_DMG) this.killTarget(t);
      }
    }
  }

  /** Destroy barrel `idx` and everything it chains into; spawns the VFX and
   *  returns all explosion centers. Used by practice (locally authoritative)
   *  and by snapshots (server already decided — this just catches up). */
  private detonate(idx: number): Vec3[] {
    const centers: Vec3[] = [];
    const queue = [idx];
    while (queue.length > 0) {
      const i = queue.pop()!;
      if (!this.barrelIntact[i]) continue;
      const c = this.removeBarrel(i);
      centers.push(c);
      this.boomVfx(c);
      this.arena.barrels.forEach((nb, j) => {
        if (!this.barrelIntact[j]) return;
        const nbox = this.arena.walls[nb.wallIndex];
        if (Math.hypot(nb.x - c.x, (nbox.y0 + nbox.y1) / 2 - c.y, nb.z - c.z) <= BARREL_RADIUS_DMG) {
          queue.push(j);
        }
      });
    }
    return centers;
  }

  /** Take barrel `idx` out of the world: mesh, prediction walls, occlusion. */
  private removeBarrel(i: number): Vec3 {
    this.barrelIntact[i] = false;
    const b = this.arena.barrels[i];
    const box = this.arena.walls[b.wallIndex];
    this.liveWalls = this.liveWalls.filter((w) => w !== box);
    this.barrelByBox.delete(box);
    const mesh = this.barrelMeshes[i];
    if (mesh) {
      this.mapGroup?.remove(mesh);
      mesh.traverse((o) => {
        const m = o as THREE.Mesh;
        m.geometry?.dispose?.();
      });
      this.barrelMeshes[i] = null;
      // the world geometry changed — re-bake the static shadow map once so the
      // destroyed barrel's shadow disappears
      this.renderer.shadowMap.needsUpdate = true;
    }
    return { x: b.x, y: (box.y0 + box.y1) / 2, z: b.z };
  }

  private boomVfx(c: Vec3): void {
    const b = this.acquire(this.boomPool);
    const s = b.mesh;
    (s.material as THREE.MeshBasicMaterial).opacity = 0.9;
    s.scale.setScalar(1);
    s.position.set(c.x, c.y, c.z);
    s.visible = true;
    b.ttl = 0.45;
  }

  /** ONE boom per chain — panned toward the nearest blast, attenuated by
   *  distance, with a longer tail the bigger the chain. */
  private playBoomSound(centers: Vec3[]): void {
    const near = centers.reduce((best, c) => {
      const d = (c.x - this.body.x) ** 2 + (c.z - this.body.z) ** 2;
      const bd = (best.x - this.body.x) ** 2 + (best.z - this.body.z) ** 2;
      return d < bd ? c : best;
    }, centers[0]);
    const dx = near.x - this.body.x;
    const dz = near.z - this.body.z;
    const srcYaw = Math.atan2(-dx, -dz);
    let diff = srcYaw - this.input.yaw;
    while (diff > Math.PI) diff -= Math.PI * 2;
    while (diff < -Math.PI) diff += Math.PI * 2;
    this.sfx.explosion(-Math.sin(diff), Math.hypot(dx, dz), centers.length);
  }

  // ── duel mode: snapshots in ──────────────────────────────────────────────
  applySnapshot(snap: GameSnapshot): void {
    const now = performance.now();
    this.hud.setTimeLeft(snap.timeLeftMs, snap.phase === "SUDDEN_DEATH");
    // clutch-time music: tense pulse under 30s, faster in sudden death
    if (snap.phase === "SUDDEN_DEATH" && this.lastPhase !== "SUDDEN_DEATH") {
      this.clutchOn = true;
      this.sfx.startClutch(true);
    } else if (snap.phase === "IN_PROGRESS" && snap.timeLeftMs < 30_000 && !this.clutchOn) {
      this.clutchOn = true;
      this.sfx.startClutch(false);
    }
    this.lastPhase = snap.phase;

    let selfState = null;
    const scores: string[] = [];
    for (const p of snap.players) {
      scores.push(`${p.kills}`);
      const prev = this.prevPlayers.get(p.id);
      if (p.id === this.selfId) {
        selfState = p;
        if (prev && p.health < prev.health) this.sfx.hurt();
        if (prev && p.health > prev.health && p.alive && prev.alive) this.sfx.heal();
        if (prev && prev.alive && !p.alive) {
          this.hud.setBanner("FRAGGED — respawning…");
          this.sfx.death();
        }
        if (prev && !prev.alive && p.alive) {
          this.hud.setBanner(null);
          this.spawnSelf({ x: p.x, z: p.z, yaw: p.yaw });
        }
      } else {
        let avatar = this.remotes.get(p.id);
        if (!avatar) {
          const group = this.makeAvatar(this.playerColors.get(p.id) ?? 0xff3355);
          this.scene.add(group);
          avatar = { group, buf: [] };
          this.remotes.set(p.id, avatar);
        }
        avatar.buf.push({ t: now, x: p.x, y: p.y, z: p.z, yaw: p.yaw });
        // keep the buffer short: a couple of samples on either side of the
        // interp window is all we ever read
        while (avatar.buf.length > 2 && now - avatar.buf[0].t > 1000) avatar.buf.shift();
        avatar.group.visible = p.alive;
      }
      this.prevPlayers.set(p.id, { health: p.health, alive: p.alive });
    }

    if (selfState) {
      this.selfAlive = selfState.alive;
      this.hud.setHealth(selfState.health, MAX_HEALTH);
      // soft server correction: only when prediction drifted noticeably
      const dx = selfState.x - this.body.x;
      const dz = selfState.z - this.body.z;
      if (dx * dx + dz * dz > 0.8 * 0.8) {
        this.body.x += dx * 0.35;
        this.body.z += dz * 0.35;
      }
    }
    this.hud.setScore(scores.join(" — "));

    if (snap.pickups) {
      snap.pickups.forEach((active, i) => {
        if (this.pickupMeshes[i]) this.pickupMeshes[i].visible = active;
      });
    }

    // barrels the server detonated: drop the same boxes locally so prediction
    // and occlusion stay in lockstep (VFX comes from the booms list below)
    if (snap.barrels) {
      snap.barrels.forEach((intact, i) => {
        if (!intact && this.barrelIntact[i]) this.removeBarrel(i);
      });
    }
    if (snap.booms && snap.booms.length > 0) {
      for (const bm of snap.booms) this.boomVfx(bm);
      this.playBoomSound(snap.booms);
    }

    for (const shot of snap.shots) {
      this.addTracer(
        { x: shot.ox, y: shot.oy, z: shot.oz },
        { x: shot.ex, y: shot.ey, z: shot.ez },
      );
      if (shot.shooterId === this.selfId) {
        this.sfx.shoot();
        if (shot.hitId) {
          this.hud.hitMarker();
          shot.killed ? this.sfx.kill() : this.sfx.hit();
        }
      } else {
        // opponent's shot, panned toward where it came from — you can HEAR
        // which side they're shooting from
        const srcYaw = Math.atan2(-(shot.ox - this.body.x), -(shot.oz - this.body.z));
        let diff = srcYaw - this.input.yaw;
        while (diff > Math.PI) diff -= Math.PI * 2;
        while (diff < -Math.PI) diff += Math.PI * 2;
        this.sfx.enemyShoot(-Math.sin(diff));
        // shoulder their avatar + kick it (so you can SEE them firing)
        const av = this.remotes.get(shot.shooterId);
        if (av) this.triggerShot(av.group);
      }
      if (shot.killed && shot.hitId) {
        const shooter = snap.players.find((p) => p.id === shot.shooterId);
        const victim = snap.players.find((p) => p.id === shot.hitId);
        this.hud.feed(`<strong>${shooter?.name ?? "?"}</strong> fragged ${victim?.name ?? "?"}`);
      }
    }
  }

  showMatchEnd(result: MatchResult, selfId: string | null): void {
    this.clutchOn = false;
    this.sfx.stopMusic();
    // Match is over — stop transmitting immediately (don't wait for the menu
    // return) so the open mic can't leak into the end screen.
    this.voice.endMatch();
    this.micPill.classList.add("hidden");
    const won = result.winnerId === selfId;
    if (result.winnerId !== null) (won ? this.sfx.victory() : this.sfx.defeat());
    const line =
      result.winnerId === null
        ? "MATCH VOID"
        : won
          ? "VICTORY — POT IS YOURS"
          : "DEFEAT";
    const score = result.players.map((p) => `${p.name} ${p.kills}`).join(" · ");
    this.hud.setBanner(`${line}<br><span style="font-size:60%">${score} (first to ${KILLS_TO_WIN})<br>click MENU to play again</span>`);
  }

  private clearRemotes(): void {
    for (const r of this.remotes.values()) this.scene.remove(r.group);
    this.remotes.clear();
  }

  private addTracer(from: Vec3, to: Vec3): void {
    const t = this.acquire(this.tracerPool);
    const pos = t.mesh.geometry.attributes.position as THREE.BufferAttribute;
    pos.setXYZ(0, from.x, from.y - 0.12, from.z);
    pos.setXYZ(1, to.x, to.y, to.z);
    pos.needsUpdate = true;
    (t.mesh.material as THREE.LineBasicMaterial).opacity = 1;
    t.mesh.visible = true;
    t.ttl = 0.12;

    // impact spark at the endpoint — you can SEE where the shot landed and
    // correct your aim, instead of guessing
    const s = this.acquire(this.sparkPool);
    s.mesh.position.set(to.x, to.y, to.z);
    s.mesh.scale.setScalar(1);
    s.mesh.visible = true;
    s.ttl = 0.2;
  }
}
