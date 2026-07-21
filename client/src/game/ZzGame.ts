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
import { Sfx } from "./Audio.ts";
import type { GameSocket } from "../network/socket.ts";

const INTERNAL_H = 540; // retro internal render height; width follows aspect
const FIRE_COOLDOWN_MS = 250;

/** How hard prediction may disagree with the server before we snap (m). */
const RECONCILE_SNAP = 0.75;
/** Gentle pull toward the server position below the snap threshold. */
const RECONCILE_PULL = 6.0; // 1/s

/** Per-kind skin palettes: [skin base, blotch, cloth] as CSS colors. */
const ZOMBIE_SKINS: Record<number, [string, string, string]> = {
  0: ["#6b8a58", "#41582f", "#4a4038"], // walker — sickly green, earth rags
  1: ["#9a9a63", "#6d6b3c", "#3b3b33"], // runner — gaunt grey-yellow
  2: ["#4e5a33", "#2f3a1c", "#5a2e24"], // brute — dark olive, wound-red rags
};

/** Per-zombie gait personality — no two shamble alike. */
interface GaitStyle {
  lurch: number; // lateral body roll amplitude
  phaseMul: number; // step frequency multiplier
  hunch: number; // forward bend
  armDrop: number; // 0 = both arms raised, 1 = right arm dangles
  legAsym: number; // how unevenly the legs swing (dragging a foot)
  stagger: number; // secondary jitter frequency (stumbler twitch)
}

/** Jointed limbs for the shamble/gait cycle (pivots at shoulder/hip). */
interface Rig {
  body: THREE.Group;
  lArm: THREE.Group;
  rArm: THREE.Group;
  lLeg: THREE.Group;
  rLeg: THREE.Group;
  phase: number;
  speed: number;
  px: number;
  pz: number;
  init: boolean;
  style?: GaitStyle;
}

interface RemoteAvatar {
  group: THREE.Group;
  rig: Rig;
  target: THREE.Vector3;
  yaw: number;
  targetYaw: number;
}

interface ZombieAvatar {
  group: THREE.Group;
  rig: Rig;
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
  /** Input sequence is monotonic per CONNECTION, not per match — the server
   *  keeps its high-water mark across rematches and drops stale-seq frames
   *  (a fresh counter made every rematch an AFK corpse). */
  private static nextSeq = 1;
  private sendAccum = 0;

  private remotes = new Map<number, RemoteAvatar>(); // slot → avatar
  private zombies = new Map<number, ZombieAvatar>(); // id → avatar
  private lootMeshes = new Map<number, THREE.Object3D>(); // id → mesh

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
    this.renderer.shadowMap.enabled = true;
    this.renderer.shadowMap.type = THREE.BasicShadowMap; // hard edges = retro
    this.renderer.shadowMap.autoUpdate = false; // static map → bake once
    this.camera = new THREE.PerspectiveCamera(80, 16 / 9, 0.05, 120);
    this.camera.rotation.order = "YXZ";
    // Aspect-true internal res: fixed height, width follows the window — the
    // crosshair aims exactly where the camera ray goes at any window shape.
    const fitViewport = () => {
      const aspect = window.innerWidth / Math.max(1, window.innerHeight);
      this.renderer.setSize(Math.round(INTERNAL_H * aspect), INTERNAL_H, false);
      this.camera.aspect = aspect;
      this.camera.updateProjectionMatrix();
    };
    fitViewport();
    window.addEventListener("resize", fitViewport);
    this.input = new Input(canvas);
    canvas.addEventListener("pointerdown", () => this.sfx.unlock(), { once: true });
    this.buildWorld();
    this.loadArena(generateArena(seed));
    this.scene.add(this.camera); // camera hosts the viewmodel
    this.buildViewmodel();
    if ((import.meta as any).env?.DEV) (window as any).__zz = this;
  }

  // ── viewmodel gun + muzzle flash (DAKKA) ─────────────────────────────────

  private gun = new THREE.Group();
  private flash = new THREE.Group();
  private flashTtl = 0;
  private recoil = 0;
  private lastShotAt = 0;
  private sfx = new Sfx();

  private buildViewmodel(): void {
    const metal = new THREE.MeshLambertMaterial({ color: 0x23262e });
    const darker = new THREE.MeshLambertMaterial({ color: 0x171a20 });
    const receiver = new THREE.Mesh(new THREE.BoxGeometry(0.07, 0.1, 0.34), metal);
    const barrel = new THREE.Mesh(new THREE.BoxGeometry(0.035, 0.035, 0.3), darker);
    barrel.position.set(0, 0.03, -0.3);
    const grip = new THREE.Mesh(new THREE.BoxGeometry(0.05, 0.12, 0.06), darker);
    grip.position.set(0, -0.09, 0.08);
    const mag = new THREE.Mesh(new THREE.BoxGeometry(0.045, 0.12, 0.07), metal);
    mag.position.set(0, -0.1, -0.04);
    this.gun.add(receiver, barrel, grip, mag);
    this.gun.position.set(0.26, -0.22, -0.55);
    this.camera.add(this.gun);

    // muzzle flash: two crossed additive quads at the barrel tip
    const flashMat = new THREE.MeshBasicMaterial({
      color: 0xffb347,
      transparent: true,
      opacity: 0.95,
      blending: THREE.AdditiveBlending,
      depthWrite: false,
    });
    const q1 = new THREE.Mesh(new THREE.PlaneGeometry(0.22, 0.22), flashMat);
    const q2 = q1.clone();
    q2.rotation.z = Math.PI / 4;
    this.flash.add(q1, q2);
    this.flash.position.set(0, 0.03, -0.48);
    this.flash.visible = false;
    this.gun.add(this.flash);
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

  /** Building facade: concrete base + two window rows (dark glass, a few lit
   *  amber). Tinted per map by the material color like the plain concrete was. */
  private static buildingTexture(base: string, seam: string): THREE.CanvasTexture {
    const c = document.createElement("canvas");
    c.width = c.height = 128;
    const g = c.getContext("2d")!;
    g.fillStyle = base;
    g.fillRect(0, 0, 128, 128);
    for (let i = 0; i < 700; i++) {
      const v = Math.floor(Math.random() * 150);
      g.fillStyle = `rgba(${v},${v},${v + 10},0.16)`;
      g.fillRect(Math.floor(Math.random() * 128), Math.floor(Math.random() * 128), 2, 2);
    }
    g.strokeStyle = seam;
    g.lineWidth = 2;
    for (const y of [0, 64, 128]) {
      g.beginPath();
      g.moveTo(0, y);
      g.lineTo(128, y);
      g.stroke();
    }
    // two floors of windows: 5 columns × 2 rows
    for (let row = 0; row < 2; row++) {
      const wy = 14 + row * 64;
      for (let col = 0; col < 5; col++) {
        const wx = 8 + col * 25;
        g.fillStyle = "#2a3138"; // frame/shadow
        g.fillRect(wx, wy, 17, 30);
        const lit = Math.random() < 0.18;
        g.fillStyle = lit ? "#e8c26a" : "#5b6b7a";
        g.fillRect(wx + 2, wy + 2, 13, 26);
        // mullion
        g.fillStyle = "#2a3138";
        g.fillRect(wx + 2, wy + 14, 13, 2);
      }
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
    // Plain block look by user decree — window facades tried and cut.
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

  /** Mottled decayed-skin texture: base tone + blotch clusters + speckle.
   *  Same canvas approach as the world's concrete — zero assets. */
  private static skinTexture(base: string, blotch: string): THREE.CanvasTexture {
    const c = document.createElement("canvas");
    c.width = c.height = 64;
    const g = c.getContext("2d")!;
    g.fillStyle = base;
    g.fillRect(0, 0, 64, 64);
    g.fillStyle = blotch;
    for (let i = 0; i < 26; i++) {
      const x = Math.random() * 64;
      const y = Math.random() * 64;
      const r = 2 + Math.random() * 6;
      g.globalAlpha = 0.35 + Math.random() * 0.35;
      g.beginPath();
      g.ellipse(x, y, r, r * (0.5 + Math.random()), Math.random() * Math.PI, 0, Math.PI * 2);
      g.fill();
    }
    g.globalAlpha = 0.2;
    for (let i = 0; i < 220; i++) {
      const v = Math.floor(Math.random() * 70);
      g.fillStyle = `rgb(${v},${v},${v})`;
      g.fillRect(Math.floor(Math.random() * 64), Math.floor(Math.random() * 64), 1, 1);
    }
    g.globalAlpha = 1;
    const tex = new THREE.CanvasTexture(c);
    tex.magFilter = THREE.NearestFilter;
    return tex;
  }

  /** Zombie face: dark sockets + glowing ember eyes on a small front plate. */
  private static faceTexture(): THREE.CanvasTexture {
    const c = document.createElement("canvas");
    c.width = 32;
    c.height = 16;
    const g = c.getContext("2d")!;
    g.fillStyle = "#181410";
    g.fillRect(0, 0, 32, 16);
    for (const ex of [9, 23]) {
      g.fillStyle = "#000000";
      g.fillRect(ex - 4, 3, 8, 8); // socket pit
      g.fillStyle = "#ff5a2a";
      g.fillRect(ex - 2, 5, 4, 4); // ember
      g.fillStyle = "#ffd23f";
      g.fillRect(ex - 1, 6, 2, 2); // hot core
    }
    const tex = new THREE.CanvasTexture(c);
    tex.magFilter = THREE.NearestFilter;
    return tex;
  }

  /** Jointed boxy rig. Limb groups pivot at the shoulder/hip so the gait
   *  cycle can swing them (ShotAnte's animateAvatar approach). Forward -Z. */
  private static makeRig(
    skinMat: THREE.Material,
    clothMat: THREE.Material,
    faceMat: THREE.Material | null,
    scale = 1,
  ): { group: THREE.Group; rig: Rig } {
    const group = new THREE.Group();
    const body = new THREE.Group();
    group.add(body);

    // torso wears cloth; head is skin
    const torso = new THREE.Mesh(new THREE.BoxGeometry(0.42, 0.62, 0.26), clothMat);
    torso.position.y = 1.14;
    const head = new THREE.Mesh(new THREE.BoxGeometry(0.34, 0.34, 0.32), skinMat);
    head.position.y = 1.62;
    body.add(torso, head);

    if (faceMat) {
      const face = new THREE.Mesh(new THREE.PlaneGeometry(0.3, 0.15), faceMat);
      face.position.set(0, 1.63, -0.165);
      face.rotation.y = Math.PI; // plane faces +Z by default; rig forward is -Z
      body.add(face);
    }

    // limbs: geometry hangs below the pivot so rotation swings from the joint
    const limb = (w: number, len: number, mat: THREE.Material) => {
      const g = new THREE.Group();
      const m = new THREE.Mesh(new THREE.BoxGeometry(w, len, w), mat);
      m.position.y = -len / 2;
      g.add(m);
      return g;
    };
    const lArm = limb(0.12, 0.52, skinMat);
    lArm.position.set(-0.3, 1.42, 0);
    const rArm = limb(0.12, 0.52, skinMat);
    rArm.position.set(0.3, 1.42, 0);
    const lLeg = limb(0.15, 0.8, clothMat);
    lLeg.position.set(-0.12, 0.8, 0);
    const rLeg = limb(0.15, 0.8, clothMat);
    rLeg.position.set(0.12, 0.8, 0);
    body.add(lArm, rArm, lLeg, rLeg);

    group.scale.setScalar(scale);
    return {
      group,
      rig: { body, lArm, rArm, lLeg, rLeg, phase: Math.random() * 6.28, speed: 0, px: 0, pz: 0, init: false },
    };
  }

  private skinMats = new Map<number, THREE.Material>();
  private faceMat: THREE.Material | null = null;
  private clothMats: THREE.Material[] = [];
  private survivorMats: { skin: THREE.Material; cloth: THREE.Material } | null = null;

  /** Wardrobe: dull street clothes gone bad — each zombie draws from this so
   *  the horde reads as former PEOPLE, not copies. */
  private clothCatalogue(): THREE.Material[] {
    if (this.clothMats.length) return this.clothMats;
    const CLOTHES: [string, string][] = [
      ["#7a3b34", "#4e241f"], // rust-red flannel
      ["#3d5470", "#293b52"], // work denim
      ["#5b5346", "#3a352c"], // brown jacket
      ["#4d5b3a", "#333e26"], // olive drab
      ["#585d63", "#3b3f44"], // grey hoodie
      ["#8a793c", "#5c5026"], // mustard shirt
      ["#4a3550", "#302136"], // faded plum
      ["#38635e", "#24443f"], // washed teal
    ];
    this.clothMats = CLOTHES.map(
      ([b, s]) => new THREE.MeshLambertMaterial({ map: ZzGame.skinTexture(b, s) }),
    );
    return this.clothMats;
  }

  private zombieRig(kind: number, id: number): { group: THREE.Group; rig: Rig } {
    let skin = this.skinMats.get(kind);
    if (!skin) {
      const [base, blotch] = ZOMBIE_SKINS[kind] ?? ZOMBIE_SKINS[0];
      skin = new THREE.MeshLambertMaterial({ map: ZzGame.skinTexture(base, blotch) });
      this.skinMats.set(kind, skin);
    }
    if (!this.faceMat) this.faceMat = new THREE.MeshBasicMaterial({ map: ZzGame.faceTexture() });

    // deterministic per-id identity: clothes, gait, accessories
    const wardrobe = this.clothCatalogue();
    const h = (id * 2654435761) >>> 0;
    const shirt = wardrobe[h % wardrobe.length];
    const pants = wardrobe[(h >> 3) % wardrobe.length];

    const scale = kind === 2 ? 1.45 : kind === 1 ? 0.95 : 1.0;
    const made = ZzGame.makeRigDressed(skin, shirt, pants, this.faceMat, scale);

    // some still wear a cap; brutes never kept theirs
    if (kind !== 2 && h % 5 === 0) {
      const cap = new THREE.Mesh(new THREE.BoxGeometry(0.36, 0.08, 0.34), pants);
      cap.position.set(0, 1.82, 0.02);
      made.rig.body.add(cap);
    }

    // gait personality from the id hash (kind sets the envelope)
    const f = (n: number) => ((h >> n) & 15) / 15; // 0..1 nibbles
    made.rig.style = {
      lurch: 0.08 + f(0) * 0.14 + (kind === 2 ? 0.05 : 0),
      phaseMul: kind === 1 ? 1.5 + f(4) * 0.5 : 0.8 + f(4) * 0.5,
      hunch: kind === 1 ? 0.24 + f(8) * 0.12 : 0.08 + f(8) * 0.18,
      armDrop: f(12) < 0.35 ? 1 : 0, // ~a third drag one dead arm
      legAsym: 0.7 + f(16) * 0.6,
      stagger: f(20) * 1.4,
    };
    made.rig.body.rotation.x = made.rig.style.hunch;
    made.rig.lArm.rotation.x = -Math.PI / 2.4;
    made.rig.rArm.rotation.x = made.rig.style.armDrop ? -0.25 : -Math.PI / 2.4;
    return made;
  }

  /** makeRig with separate shirt/pants materials (torso vs legs). */
  private static makeRigDressed(
    skinMat: THREE.Material,
    shirtMat: THREE.Material,
    pantsMat: THREE.Material,
    faceMat: THREE.Material | null,
    scale = 1,
  ): { group: THREE.Group; rig: Rig } {
    const made = ZzGame.makeRig(skinMat, shirtMat, faceMat, scale);
    // legs were built with the torso material — re-dress them as pants
    for (const leg of [made.rig.lLeg, made.rig.rLeg]) {
      const mesh = leg.children[0] as THREE.Mesh;
      mesh.material = pantsMat;
    }
    return made;
  }

  /** Drive one rig's gait from its position delta (donor recipe: low-passed
   *  speed → alternating swing + bob; zombies lurch, arms stay raised). */
  private static animateRig(rig: Rig, x: number, y: number, z: number, dt: number, zombie: boolean): void {
    if (!rig.init) {
      rig.px = x;
      rig.pz = z;
      rig.init = true;
    }
    const raw = Math.hypot(x - rig.px, z - rig.pz) / Math.max(dt, 1e-3);
    rig.px = x;
    rig.pz = z;
    rig.speed += (Math.min(raw, 9) - rig.speed) * Math.min(1, dt * 12);
    const ease = Math.min(1, dt * 12);

    if (y > 0.12) {
      rig.lLeg.rotation.x += (-0.5 - rig.lLeg.rotation.x) * ease;
      rig.rLeg.rotation.x += (0.5 - rig.rLeg.rotation.x) * ease;
      rig.body.position.y += (0 - rig.body.position.y) * ease;
    } else if (rig.speed > 0.3) {
      const amp = Math.min(0.25 + rig.speed * 0.09, 0.8);
      rig.phase += rig.speed * dt * 2.2 * (rig.style?.phaseMul ?? 1);
      const s = Math.sin(rig.phase);
      rig.lLeg.rotation.x = s * amp;
      rig.rLeg.rotation.x = -s * amp;
      rig.body.position.y = Math.abs(s) * 0.05;
      if (zombie) {
        // shamble driven by this zombie's own gait personality
        const st = rig.style ?? { lurch: 0.14, phaseMul: 1, hunch: 0.14, armDrop: 0, legAsym: 1, stagger: 0 };
        rig.body.rotation.z = s * st.lurch + Math.sin(rig.phase * (2.3 + st.stagger)) * st.stagger * 0.03;
        rig.lLeg.rotation.x = s * amp * st.legAsym;
        rig.rLeg.rotation.x = -s * amp * (2 - st.legAsym) * 0.55;
        rig.lArm.rotation.x = -Math.PI / 2.4 + Math.sin(rig.phase * 0.9) * 0.18;
        rig.rArm.rotation.x = st.armDrop
          ? -0.25 + Math.sin(rig.phase * 0.8) * 0.1 // dead arm swings loose
          : -Math.PI / 2.4 + Math.cos(rig.phase * 1.1) * 0.18;
        rig.lArm.rotation.z = Math.sin(rig.phase * 0.7) * 0.1;
      } else {
        rig.lArm.rotation.x = -s * amp * 0.7;
        rig.rArm.rotation.x = s * amp * 0.7;
      }
    } else {
      rig.lLeg.rotation.x += (0 - rig.lLeg.rotation.x) * ease;
      rig.rLeg.rotation.x += (0 - rig.rLeg.rotation.x) * ease;
      rig.body.position.y += (0 - rig.body.position.y) * ease;
      if (zombie) rig.body.rotation.z += (0 - rig.body.rotation.z) * ease;
    }
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
        if (!this.survivorMats) {
          this.survivorMats = {
            skin: new THREE.MeshLambertMaterial({ color: 0xd8b590 }),
            cloth: new THREE.MeshLambertMaterial({ map: ZzGame.skinTexture("#31527d", "#223a59") }),
          };
        }
        const made = ZzGame.makeRig(this.survivorMats.skin, this.survivorMats.cloth, null);
        this.scene.add(made.group);
        av = { group: made.group, rig: made.rig, target: new THREE.Vector3(x, y, z), yaw: 0, targetYaw: 0 };
        made.group.position.set(x, y, z);
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
        const made = this.zombieRig(z.kind, z.id);
        this.scene.add(made.group);
        av = {
          group: made.group,
          rig: made.rig,
          target: new THREE.Vector3(),
          yaw: 0,
          targetYaw: 0,
          kind: z.kind,
        };
        made.group.position.set(dequantPos(z.pos[0]), dequantPos(z.pos[1]), dequantPos(z.pos[2]));
        this.zombies.set(z.id, av);
      }
      av.target.set(dequantPos(z.pos[0]), dequantPos(z.pos[1]), dequantPos(z.pos[2]));
      av.targetYaw = (z.yaw / 256) * Math.PI * 2;
    }
    for (const [id, av] of this.zombies) {
      if (!seen.has(id)) {
        // died: crumple where it stood + a few soft chunks — no vanishing
        this.zombies.delete(id);
        this.dying.push({ group: av.group, t: 0 });
        this.spawnChunks(av.group.position, 5, 0x6a1f18);
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

    // bullets + hit feedback for this tick's shots
    for (const sh of snap.shots) {
      this.fireBullet(
        sh.slot,
        [dequantPos(sh.end[0]), dequantPos(sh.end[1]), dequantPos(sh.end[2])],
        sh.hitKind,
      );
      if (sh.slot === this.mySlot && sh.hitKind > 0) this.sfx.hit();
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

  // ── bullets, chunks, corpses ─────────────────────────────────────────────

  private bullets: { mesh: THREE.Mesh; from: THREE.Vector3; to: THREE.Vector3; t: number; dur: number; hitKind: number }[] = [];
  private chunks: { mesh: THREE.Mesh; vel: THREE.Vector3; ttl: number }[] = [];
  private dying: { group: THREE.Group; t: number }[] = [];
  private bulletMat = new THREE.MeshBasicMaterial({ color: 0xffd23f });
  private bulletGeo = new THREE.BoxGeometry(0.05, 0.05, 0.34);
  private chunkGeo = new THREE.BoxGeometry(0.12, 0.12, 0.12);
  private chunkMats = new Map<number, THREE.MeshBasicMaterial>();

  /** A visible slug flying the shot path (server hitscan is instant; this is
   *  the dakka the eye wants). Impact chunks pop when it arrives. */
  private fireBullet(slot: number, end: [number, number, number], hitKind: number): void {
    let from: THREE.Vector3;
    if (slot === this.mySlot) {
      from = new THREE.Vector3();
      this.flash.getWorldPosition(from); // leaves the actual muzzle
    } else {
      const av = this.remotes.get(slot);
      if (!av) return;
      from = av.group.position.clone().setY(av.group.position.y + PLAYER_EYE - 0.12);
    }
    const to = new THREE.Vector3(...end);
    const mesh = new THREE.Mesh(this.bulletGeo, this.bulletMat);
    mesh.position.copy(from);
    mesh.lookAt(to);
    this.scene.add(mesh);
    const dur = Math.max(0.04, from.distanceTo(to) / 95); // ~95 m/s visual speed
    this.bullets.push({ mesh, from, to, t: 0, dur, hitKind });
  }

  private spawnChunks(at: THREE.Vector3, n: number, color: number): void {
    let mat = this.chunkMats.get(color);
    if (!mat) {
      mat = new THREE.MeshBasicMaterial({ color });
      this.chunkMats.set(color, mat);
    }
    for (let i = 0; i < n; i++) {
      const mesh = new THREE.Mesh(this.chunkGeo, mat);
      mesh.position.copy(at);
      mesh.position.y += 0.9;
      this.scene.add(mesh);
      this.chunks.push({
        mesh,
        vel: new THREE.Vector3((Math.random() - 0.5) * 3.5, 1.5 + Math.random() * 2.5, (Math.random() - 0.5) * 3.5),
        ttl: 0.45 + Math.random() * 0.25,
      });
    }
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
    input.sequence = ZzGame.nextSeq;

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
      ZzGame.nextSeq++;
    }

    // local fire feedback: flash + recoil kick + gunshot crack (server owns
    // the actual hitscan; its tracer/hit arrives in the next snapshot)
    const now = performance.now();
    if (input.shoot && this.spawned && now - this.lastShotAt >= FIRE_COOLDOWN_MS) {
      this.lastShotAt = now;
      this.recoil = 1;
      this.flashTtl = 0.05;
      this.flash.visible = true;
      this.flash.rotation.z = Math.random() * Math.PI;
      this.sfx.shoot();
    }
    this.recoil += (0 - this.recoil) * Math.min(1, dt * 14);
    if (this.flashTtl > 0) {
      this.flashTtl -= dt;
      if (this.flashTtl <= 0) this.flash.visible = false;
    }
    this.gun.position.z = -0.55 + this.recoil * 0.06; // gun kicks back
    this.gun.rotation.x = this.recoil * 0.18;

    // camera (recoil lifts the view a touch)
    this.camera.position.set(this.body.x, this.body.y + PLAYER_EYE, this.body.z);
    this.camera.rotation.set(this.input.pitch + this.recoil * 0.035, this.input.yaw, 0);

    // interpolate remotes + zombies toward their latest server targets.
    // Large jumps (spawn reseats, watchdog teleports) SNAP — lerping them
    // reads as zombies zooming across the map.
    const lerpK = Math.min(1, dt * 12);
    const SNAP_DIST_SQ = 2.5 * 2.5;
    for (const av of this.remotes.values()) {
      if (av.group.position.distanceToSquared(av.target) > SNAP_DIST_SQ) {
        av.group.position.copy(av.target);
        av.yaw = av.targetYaw;
      } else {
        av.group.position.lerp(av.target, lerpK);
        av.yaw = ZzGame.angleLerp(av.yaw, av.targetYaw, lerpK);
      }
      av.group.rotation.y = av.yaw;
      ZzGame.animateRig(av.rig, av.group.position.x, av.group.position.y, av.group.position.z, dt, false);
    }
    for (const av of this.zombies.values()) {
      if (av.group.position.distanceToSquared(av.target) > SNAP_DIST_SQ) {
        av.group.position.copy(av.target);
        av.yaw = av.targetYaw;
        av.rig.init = false; // don't count the teleport as sprint speed
      } else {
        av.group.position.lerp(av.target, lerpK);
        av.yaw = ZzGame.angleLerp(av.yaw, av.targetYaw, lerpK);
      }
      av.group.rotation.y = av.yaw;
      ZzGame.animateRig(av.rig, av.group.position.x, av.group.position.y, av.group.position.z, dt, true);
    }

    // loot idle spin
    for (const mesh of this.lootMeshes.values()) mesh.rotation.y += dt * 2.0;

    // bullets in flight
    for (let i = this.bullets.length - 1; i >= 0; i--) {
      const b = this.bullets[i];
      b.t += dt;
      const k = Math.min(1, b.t / b.dur);
      b.mesh.position.lerpVectors(b.from, b.to, k);
      if (k >= 1) {
        // impact: red flesh puff on a hit, grey masonry chips on a wall
        this.spawnChunks(b.to.clone().setY(b.to.y - 0.9), 3, b.hitKind > 0 ? 0x6a1f18 : 0x8d9099);
        this.scene.remove(b.mesh);
        this.bullets.splice(i, 1);
      }
    }

    // chunk physics: pop, arc, expire
    for (let i = this.chunks.length - 1; i >= 0; i--) {
      const c = this.chunks[i];
      c.ttl -= dt;
      c.vel.y -= 9 * dt;
      c.mesh.position.addScaledVector(c.vel, dt);
      c.mesh.rotation.x += dt * 7;
      c.mesh.rotation.z += dt * 5;
      if (c.ttl <= 0 || c.mesh.position.y < 0.05) {
        this.scene.remove(c.mesh);
        this.chunks.splice(i, 1);
      }
    }

    // corpses: keel over, rest a beat, sink away
    for (let i = this.dying.length - 1; i >= 0; i--) {
      const d = this.dying[i];
      d.t += dt;
      const fall = Math.min(1, d.t / 0.4);
      d.group.rotation.z = (Math.PI / 2) * (1 - (1 - fall) * (1 - fall)); // ease-out keel
      if (d.t > 1.0) d.group.position.y -= dt * 1.6; // sink
      if (d.t > 1.6) {
        this.scene.remove(d.group);
        this.dying.splice(i, 1);
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
