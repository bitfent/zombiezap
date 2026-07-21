// ZombieZap match renderer + local prediction, ZzSnapshot-native.
// World look and avatar rig are lifted from ShotAnte's Game.ts (the donor in
// src/reference/) — same procedural concrete textures, Lambert materials,
// baked-once shadows, merged static geometry. The session/duel layer is gone;
// this class exists only between game_start and match_end.
//
// Prediction: stepBody() here is the same function body the Rust server runs
// (shared ancestry, golden-pinned), applied to the same walls generated from
// the same seed — so the camera agrees with the authoritative sim. v1
// reconciliation is snap-toward-server on divergence; input-replay comes later.

import * as THREE from "three";
import { mergeGeometries } from "three/examples/jsm/utils/BufferGeometryUtils.js";
import {
  ARENA_HALF,
  PLAYER_EYE,
  TICK_DT,
  generateArena,
  seededRandom,
  stepBody,
  encodeInput,
  dequantPos,
  dequantYaw16,
  type Arena,
  type Body,
  type PlayerInput,
  type RosterPlayer,
  type WireZombie,
  type ZzSnapshot,
} from "@shotante/shared";
import { Input } from "./Input.ts";
import type { GameSocket } from "../network/socket.ts";

const INTERNAL_W = 960; // retro internal render res — chunky but readable
const INTERNAL_H = 540;

/** How hard prediction may disagree with the server before we snap (m). */
const RECONCILE_SNAP = 0.75;
/** Gentle pull toward the server position below the snap threshold. */
const RECONCILE_PULL = 6.0; // 1/s

const ZOMBIE_COLORS: Record<number, number> = {
  0: 0x5a7d4a, // walker — sickly green
  1: 0x8a8a55, // runner — gaunt grey-yellow
  2: 0x44502e, // brute — dark olive
};

interface RemoteAvatar {
  group: THREE.Group;
  target: THREE.Vector3;
  yaw: number;
  targetYaw: number;
}

interface ZombieAvatar {
  group: THREE.Group;
  target: THREE.Vector3;
  yaw: number;
  targetYaw: number;
  kind: number;
}

export class ZzGame {
  private renderer: THREE.WebGLRenderer;
  private scene = new THREE.Scene();
  private camera: THREE.PerspectiveCamera;
  private input: Input;
  private socket: GameSocket;

  private arena!: Arena;
  private walls: Arena["walls"] = [];
  private mapGroup: THREE.Group | null = null;

  private mySlot: number;
  private roster: RosterPlayer[];
  private body: Body = { x: 0, y: 0, z: 0, vy: 0, onGround: true };
  private spawned = false;
  private sequence = 1;
  private sendAccum = 0;

  private remotes = new Map<number, RemoteAvatar>(); // slot → avatar
  private zombies = new Map<number, ZombieAvatar>(); // id → avatar
  private lootMeshes = new Map<number, THREE.Object3D>(); // id → mesh
  private tracers: { line: THREE.Line; ttl: number }[] = [];

  private lastSnap: ZzSnapshot | null = null;
  private raf = 0;
  private lastT = 0;
  running = false;

  /** Latest self state for the HUD (health/ammo/etc.), refreshed per snapshot. */
  onSelfState: ((p: ZzSnapshot["players"][number]) => void) | null = null;

  constructor(
    canvas: HTMLCanvasElement,
    socket: GameSocket,
    seed: string,
    mySlot: number,
    roster: RosterPlayer[],
  ) {
    this.socket = socket;
    this.mySlot = mySlot;
    this.roster = roster;
    this.renderer = new THREE.WebGLRenderer({ canvas, antialias: false });
    this.renderer.setSize(INTERNAL_W, INTERNAL_H, false); // CSS upscales, pixelated
    this.renderer.shadowMap.enabled = true;
    this.renderer.shadowMap.type = THREE.BasicShadowMap; // hard edges = retro
    this.renderer.shadowMap.autoUpdate = false; // static map → bake once
    this.camera = new THREE.PerspectiveCamera(80, INTERNAL_W / INTERNAL_H, 0.05, 120);
    this.camera.rotation.order = "YXZ";
    this.input = new Input(canvas);
    this.buildWorld();
    this.loadArena(generateArena(seed));
  }

  // ── world (ShotAnte recipe) ──────────────────────────────────────────────

  private static concreteTexture(
    base: string,
    seam: string,
    panels: number,
    grain: number,
  ): THREE.CanvasTexture {
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
      g.beginPath();
      g.moveTo(0, i * step);
      g.lineTo(128, i * step);
      g.stroke();
      g.beginPath();
      g.moveTo(i * step, 0);
      g.lineTo(i * step, 128);
      g.stroke();
    }
    const tex = new THREE.CanvasTexture(c);
    tex.wrapS = tex.wrapT = THREE.RepeatWrapping;
    tex.magFilter = THREE.NearestFilter;
    return tex;
  }

  private buildWorld(): void {
    const sky = new THREE.Color(0x9ec9ef);
    this.scene.background = sky;
    this.scene.fog = new THREE.Fog(sky, ARENA_HALF * 1.4, ARENA_HALF * 4);
    this.scene.add(new THREE.HemisphereLight(0xbfd9f5, 0x6e6a5e, 0.85));
    this.scene.add(new THREE.AmbientLight(0xffffff, 0.18));
    const sun = new THREE.DirectionalLight(0xfff3da, 1.5);
    sun.position.set(16, 28, 12);
    sun.castShadow = true;
    sun.shadow.mapSize.set(1024, 1024);
    const sh = ARENA_HALF + 6;
    sun.shadow.camera.left = sun.shadow.camera.bottom = -sh;
    sun.shadow.camera.right = sun.shadow.camera.top = sh;
    sun.shadow.camera.near = 2;
    sun.shadow.camera.far = 90;
    this.scene.add(sun);

    const tex = ZzGame.concreteTexture("#8d9099", "#797d88", 4, 120);
    tex.repeat.set(ARENA_HALF, ARENA_HALF);
    const floor = new THREE.Mesh(
      new THREE.PlaneGeometry(ARENA_HALF * 2, ARENA_HALF * 2),
      new THREE.MeshLambertMaterial({ map: tex }),
    );
    floor.rotation.x = -Math.PI / 2;
    floor.receiveShadow = true;
    this.scene.add(floor);
  }

  private loadArena(arena: Arena): void {
    this.arena = arena;
    this.walls = arena.walls;
    const group = new THREE.Group();
    const rng = seededRandom(`${arena.seed}-paint`);
    const PASTELS = [0xe8d8b8, 0xd9c4ad, 0xc9d6c2, 0xc4cede, 0xdcc6c6, 0xcfd8c0];
    const hueA = PASTELS[Math.floor(rng() * PASTELS.length)];
    const hueB = PASTELS[Math.floor(rng() * PASTELS.length)];
    const perimTex = ZzGame.concreteTexture("#aeb6c4", "#9aa2b2", 2, 140);
    const buildTex = ZzGame.concreteTexture("#ffffff", "#d8d2c4", 2, 150);
    const crateTex = ZzGame.concreteTexture("#caa36a", "#a8814c", 2, 110);
    const perimMat = new THREE.MeshLambertMaterial({ map: perimTex });
    const roofMat = new THREE.MeshLambertMaterial({ color: 0x4b5364 });
    const buildMatA = new THREE.MeshLambertMaterial({ map: buildTex, color: hueA });
    const buildMatB = new THREE.MeshLambertMaterial({ map: buildTex, color: hueB });
    const crateMat = new THREE.MeshLambertMaterial({
      map: crateTex,
      polygonOffset: true,
      polygonOffsetFactor: 1,
      polygonOffsetUnits: 1,
    });
    const accentTrim = new THREE.LineBasicMaterial({ color: arena.accent });
    const darkTrim = new THREE.LineBasicMaterial({ color: 0x6b542f });

    const solidBuckets = new Map<THREE.Material, THREE.BufferGeometry[]>();
    const bucket = (m: THREE.Material, g: THREE.BufferGeometry) => {
      const a = solidBuckets.get(m);
      if (a) a.push(g);
      else solidBuckets.set(m, [g]);
    };
    const accentEdges: THREE.BufferGeometry[] = [];
    const darkEdges: THREE.BufferGeometry[] = [];

    arena.walls.forEach((b, i) => {
      const w = b.x1 - b.x0;
      const h = b.y1 - b.y0;
      const d = b.z1 - b.z0;
      const cx = (b.x0 + b.x1) / 2;
      const cy = (b.y0 + b.y1) / 2;
      const cz = (b.z0 + b.z1) / 2;
      const isRoof = b.y0 >= 2.5;
      const isBuilding = b.y1 > 2.5 || (b.y0 === 0 && b.y1 === 1.3);
      const mat =
        i < 4
          ? perimMat
          : isRoof
            ? roofMat
            : isBuilding
              ? (b.x0 + b.z0) > 0
                ? buildMatA
                : buildMatB
              : crateMat;
      const geo = new THREE.BoxGeometry(w, h, d).translate(cx, cy, cz);
      bucket(mat, geo);
      if (i >= 4 && !isRoof && !isBuilding) {
        const small = w <= 1.3 && d <= 1.3;
        const eg = new THREE.EdgesGeometry(new THREE.BoxGeometry(w, h, d))
          .scale(1.003, 1.003, 1.003)
          .translate(cx, cy, cz);
        (small ? accentEdges : darkEdges).push(eg);
      }
    });

    for (const [mat, geos] of solidBuckets) {
      const mesh = new THREE.Mesh(mergeGeometries(geos), mat);
      mesh.castShadow = mat !== perimMat;
      mesh.receiveShadow = true;
      group.add(mesh);
    }
    if (accentEdges.length)
      group.add(new THREE.LineSegments(mergeGeometries(accentEdges), accentTrim));
    if (darkEdges.length) group.add(new THREE.LineSegments(mergeGeometries(darkEdges), darkTrim));

    // voxel clouds — deterministic per map, one merged mesh
    const cloudMat = new THREE.MeshLambertMaterial({ color: 0xffffff, fog: false });
    const cloudGeos: THREE.BufferGeometry[] = [];
    for (let i = 0; i < 9; i++) {
      const ccx = (rng() * 2 - 1) * ARENA_HALF * 2.6;
      const ccy = 18 + rng() * 12;
      const ccz = (rng() * 2 - 1) * ARENA_HALF * 2.6;
      const puffs = 2 + Math.floor(rng() * 3);
      for (let j = 0; j < puffs; j++) {
        const w = 3 + rng() * 4;
        cloudGeos.push(
          new THREE.BoxGeometry(w, 1.2 + rng(), 2 + rng() * 2).translate(
            ccx + (rng() * 2 - 1) * 3,
            ccy + (rng() - 0.5),
            ccz + (rng() * 2 - 1) * 2,
          ),
        );
      }
    }
    group.add(new THREE.Mesh(mergeGeometries(cloudGeos), cloudMat));

    this.mapGroup = group;
    this.scene.add(group);
    this.renderer.shadowMap.needsUpdate = true; // bake static shadows once
  }

  // ── avatars ──────────────────────────────────────────────────────────────

  /** Boxy Minecraft-style rig (ShotAnte's makeAvatar, simplified: static limbs
   *  v1 — gait animation returns with the polish pass). Forward is -Z. */
  private static makeRig(color: number, scale = 1): THREE.Group {
    const root = new THREE.Group();
    const mat = new THREE.MeshLambertMaterial({ color });
    const dark = new THREE.MeshLambertMaterial({
      color: new THREE.Color(color).multiplyScalar(0.55),
    });

    const torso = new THREE.BoxGeometry(0.42, 0.62, 0.26).translate(0, 1.14, 0);
    const head = new THREE.BoxGeometry(0.34, 0.34, 0.32).translate(0, 1.62, 0);
    root.add(new THREE.Mesh(mergeGeometries([torso, head]), mat));

    // face patch: read the facing at a glance
    const face = new THREE.Mesh(new THREE.BoxGeometry(0.26, 0.12, 0.02), dark);
    face.position.set(0, 1.64, -0.17);
    root.add(face);

    const armGeo = new THREE.BoxGeometry(0.12, 0.5, 0.12);
    const lArm = new THREE.Mesh(armGeo, dark);
    lArm.position.set(-0.3, 1.18, 0);
    const rArm = new THREE.Mesh(armGeo, dark);
    rArm.position.set(0.3, 1.18, 0);
    const legGeo = new THREE.BoxGeometry(0.15, 0.8, 0.15);
    const lLeg = new THREE.Mesh(legGeo, dark);
    lLeg.position.set(-0.12, 0.4, 0);
    const rLeg = new THREE.Mesh(legGeo, dark);
    rLeg.position.set(0.12, 0.4, 0);
    root.add(lArm, rArm, lLeg, rLeg);

    root.traverse((o) => {
      (o as THREE.Mesh).castShadow = false;
    });
    root.scale.setScalar(scale);
    return root;
  }

  private zombieRig(kind: number): THREE.Group {
    const color = ZOMBIE_COLORS[kind] ?? ZOMBIE_COLORS[0];
    const scale = kind === 2 ? 1.45 : kind === 1 ? 0.95 : 1.0;
    const g = ZzGame.makeRig(color, scale);
    // zombie arms forward — the classic silhouette
    const [, , lArm, rArm] = g.children as THREE.Mesh[];
    if (lArm && rArm) {
      lArm.rotation.x = -Math.PI / 2.3;
      lArm.position.z = -0.22;
      lArm.position.y = 1.3;
      rArm.rotation.x = -Math.PI / 2.3;
      rArm.position.z = -0.22;
      rArm.position.y = 1.3;
    }
    return g;
  }

  // ── snapshots ────────────────────────────────────────────────────────────

  applySnapshot(snap: ZzSnapshot): void {
    this.lastSnap = snap;

    for (const p of snap.players) {
      const x = dequantPos(p.pos[0]);
      const y = dequantPos(p.pos[1]);
      const z = dequantPos(p.pos[2]);
      if (p.slot === this.mySlot) {
        if (!this.spawned) {
          // first authoritative position — teleport the body there
          this.body.x = x;
          this.body.y = y;
          this.body.z = z;
          this.input.setView(dequantYaw16(p.yaw), 0);
          this.spawned = true;
        } else {
          const dx = x - this.body.x;
          const dy = y - this.body.y;
          const dz = z - this.body.z;
          const err = Math.hypot(dx, dy, dz);
          if (err > RECONCILE_SNAP) {
            this.body.x = x;
            this.body.y = y;
            this.body.z = z;
          }
          // small errors get pulled in update() (RECONCILE_PULL)
          this.serverPos.set(x, y, z);
        }
        this.onSelfState?.(p);
        continue;
      }
      // remote player
      let av = this.remotes.get(p.slot);
      if (!av && p.alive) {
        const group = ZzGame.makeRig(0x3a6ea5);
        this.scene.add(group);
        av = { group, target: new THREE.Vector3(x, y, z), yaw: 0, targetYaw: 0 };
        group.position.set(x, y, z);
        this.remotes.set(p.slot, av);
      }
      if (av) {
        av.target.set(x, y, z);
        av.targetYaw = dequantYaw16(p.yaw);
        av.group.visible = p.alive;
      }
    }

    // zombies: reconcile the id set
    const seen = new Set<number>();
    for (const z of snap.zombies) {
      seen.add(z.id);
      let av = this.zombies.get(z.id);
      if (!av) {
        const group = this.zombieRig(z.kind);
        this.scene.add(group);
        av = {
          group,
          target: new THREE.Vector3(),
          yaw: 0,
          targetYaw: 0,
          kind: z.kind,
        };
        group.position.set(dequantPos(z.pos[0]), dequantPos(z.pos[1]), dequantPos(z.pos[2]));
        this.zombies.set(z.id, av);
      }
      av.target.set(dequantPos(z.pos[0]), dequantPos(z.pos[1]), dequantPos(z.pos[2]));
      av.targetYaw = (z.yaw / 256) * Math.PI * 2;
    }
    for (const [id, av] of this.zombies) {
      if (!seen.has(id)) {
        this.scene.remove(av.group);
        this.zombies.delete(id);
      }
    }

    // loot
    const lootSeen = new Set<number>();
    for (const l of snap.loot) {
      lootSeen.add(l.id);
      if (!this.lootMeshes.has(l.id)) {
        const mesh = this.lootMesh(l.kind);
        mesh.position.set(dequantPos(l.pos[0]), dequantPos(l.pos[1]) + 0.35, dequantPos(l.pos[2]));
        this.scene.add(mesh);
        this.lootMeshes.set(l.id, mesh);
      }
    }
    for (const [id, mesh] of this.lootMeshes) {
      if (!lootSeen.has(id)) {
        this.scene.remove(mesh);
        this.lootMeshes.delete(id);
      }
    }

    // tracers for this tick's shots
    for (const sh of snap.shots) {
      this.addTracer(sh.slot, [
        dequantPos(sh.end[0]),
        dequantPos(sh.end[1]),
        dequantPos(sh.end[2]),
      ]);
    }
  }

  private lootMesh(kind: number): THREE.Object3D {
    // 0 ammo (amber box), 1 health (green cross), 2 grenade (olive), 3 supply (blue)
    if (kind === 1) {
      const cross = new THREE.Group();
      const mat = new THREE.MeshLambertMaterial({ color: 0x2bff88, emissive: 0x0fae4e });
      cross.add(new THREE.Mesh(new THREE.BoxGeometry(0.7, 0.22, 0.22), mat));
      cross.add(new THREE.Mesh(new THREE.BoxGeometry(0.22, 0.7, 0.22), mat));
      return cross;
    }
    const colors: Record<number, number> = { 0: 0xf2d83d, 2: 0x73872e, 3: 0x408cf2 };
    return new THREE.Mesh(
      new THREE.BoxGeometry(0.45, 0.45, 0.45),
      new THREE.MeshLambertMaterial({ color: colors[kind] ?? 0xf2d83d }),
    );
  }

  private addTracer(slot: number, end: [number, number, number]): void {
    let from: THREE.Vector3;
    if (slot === this.mySlot) {
      from = new THREE.Vector3(this.body.x, this.body.y + PLAYER_EYE - 0.12, this.body.z);
    } else {
      const av = this.remotes.get(slot);
      if (!av) return;
      from = av.group.position.clone().setY(av.group.position.y + PLAYER_EYE - 0.12);
    }
    const geo = new THREE.BufferGeometry().setFromPoints([from, new THREE.Vector3(...end)]);
    const line = new THREE.Line(
      geo,
      new THREE.LineBasicMaterial({ color: 0xff8a3d, transparent: true, opacity: 1 }),
    );
    line.frustumCulled = false;
    this.scene.add(line);
    this.tracers.push({ line, ttl: 0.09 });
  }

  // ── loop ─────────────────────────────────────────────────────────────────

  private serverPos = new THREE.Vector3();

  start(): void {
    this.running = true;
    this.lastT = performance.now();
    const tick = (t: number) => {
      if (!this.running) return;
      const dt = Math.min(0.05, (t - this.lastT) / 1000);
      this.lastT = t;
      this.update(dt);
      this.renderer.render(this.scene, this.camera);
      this.raf = requestAnimationFrame(tick);
    };
    this.raf = requestAnimationFrame(tick);
  }

  stop(): void {
    this.running = false;
    cancelAnimationFrame(this.raf);
  }

  private update(dt: number): void {
    const input = this.input.frame();
    input.sequence = this.sequence;

    if (this.spawned) {
      // predict locally with the shared sim
      stepBody(this.body, input, dt, this.walls);
      // gentle pull toward the latest authoritative position
      if (this.serverPos.lengthSq() > 0) {
        const k = Math.min(1, RECONCILE_PULL * dt) * 0.15;
        this.body.x += (this.serverPos.x - this.body.x) * k;
        this.body.z += (this.serverPos.z - this.body.z) * k;
      }
    }

    // send input at the server tick rate
    this.sendAccum += dt;
    if (this.sendAccum >= TICK_DT) {
      this.sendAccum %= TICK_DT;
      this.socket.sendBinary(encodeInput(input));
      this.sequence++;
    }

    // camera
    this.camera.position.set(this.body.x, this.body.y + PLAYER_EYE, this.body.z);
    this.camera.rotation.set(this.input.pitch, this.input.yaw, 0);

    // interpolate remotes + zombies toward their latest server targets
    const lerpK = Math.min(1, dt * 12);
    for (const av of this.remotes.values()) {
      av.group.position.lerp(av.target, lerpK);
      av.yaw = ZzGame.angleLerp(av.yaw, av.targetYaw, lerpK);
      av.group.rotation.y = av.yaw;
    }
    for (const av of this.zombies.values()) {
      av.group.position.lerp(av.target, lerpK);
      av.yaw = ZzGame.angleLerp(av.yaw, av.targetYaw, lerpK);
      av.group.rotation.y = av.yaw;
    }

    // loot idle spin
    for (const mesh of this.lootMeshes.values()) mesh.rotation.y += dt * 2.0;

    // expire tracers
    for (let i = this.tracers.length - 1; i >= 0; i--) {
      const tr = this.tracers[i];
      tr.ttl -= dt;
      if (tr.ttl <= 0) {
        this.scene.remove(tr.line);
        tr.line.geometry.dispose();
        this.tracers.splice(i, 1);
      }
    }
  }

  private static angleLerp(a: number, b: number, k: number): number {
    let d = b - a;
    while (d > Math.PI) d -= Math.PI * 2;
    while (d < -Math.PI) d += Math.PI * 2;
    return a + d * k;
  }

  get pointerLocked(): boolean {
    return this.input.locked;
  }

  dispose(): void {
    this.stop();
    this.renderer.dispose();
  }
}
