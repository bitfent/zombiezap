// Keyboard + pointer-lock mouse AND touch capture -> PlayerInput frames.
//
// Desktop: WASD + mouse-look (pointer lock), click to shoot, [ ] to adjust
// sensitivity (persisted). Touch: left-half virtual stick to move, right-half
// drag to aim, FIRE/JUMP buttons. Both paths produce the same PlayerInput, so
// the game and the server never know the difference.

import { MAX_PITCH, type PlayerInput } from "@shotante/shared";

const BASE_SENSITIVITY = 0.0023;
const TOUCH_AIM_FACTOR = 2.4; // finger travel is scarcer than mouse travel
const SENS_KEY = "shotante.sens";
const STICK_DEADZONE = 0.25;

export function isTouchDevice(): boolean {
  const override = new URLSearchParams(location.search).get("touch");
  if (override === "1") return true;
  if (override === "0") return false;
  return navigator.maxTouchPoints > 0 && matchMedia("(pointer: coarse)").matches;
}

export class Input {
  yaw = 0;
  pitch = 0;
  sensMultiplier = 1;
  readonly touch: boolean;
  private keys = new Set<string>();
  private shooting = false;
  private sequence = 0;
  private canvas: HTMLCanvasElement;
  onSensitivityChange: ((mult: number) => void) | null = null;

  // touch state
  private stickId: number | null = null;
  private stickOrigin = { x: 0, y: 0 };
  private stickVec = { x: 0, y: 0 }; // -1..1
  private aimId: number | null = null;
  private aimLast = { x: 0, y: 0 };
  private touchFire = false;
  private touchJump = false;

  constructor(canvas: HTMLCanvasElement) {
    this.canvas = canvas;
    this.touch = isTouchDevice();
    const saved = Number(localStorage.getItem(SENS_KEY));
    if (saved > 0.2 && saved < 5) this.sensMultiplier = saved;

    // Typing (chat box, callsign) must never move the player.
    const isTyping = (e: KeyboardEvent) =>
      (e.target as HTMLElement | null)?.tagName === "INPUT";
    window.addEventListener("keydown", (e) => {
      if (isTyping(e)) return;
      if (!e.repeat) this.keys.add(e.code);
      if (e.code === "Space") e.preventDefault();
      if (e.code === "BracketLeft") this.adjustSensitivity(-0.15);
      if (e.code === "BracketRight") this.adjustSensitivity(+0.15);
    });
    window.addEventListener("keyup", (e) => this.keys.delete(e.code));

    if (this.touch) this.initTouch();
    else this.initMouse();
  }

  private adjustSensitivity(delta: number): void {
    this.sensMultiplier = Math.min(3, Math.max(0.3, +(this.sensMultiplier + delta).toFixed(2)));
    localStorage.setItem(SENS_KEY, String(this.sensMultiplier));
    this.onSensitivityChange?.(this.sensMultiplier);
  }

  // ── desktop ──
  private initMouse(): void {
    this.canvas.addEventListener("mousedown", () => {
      if (document.pointerLockElement === this.canvas) this.shooting = true;
      else this.canvas.requestPointerLock();
    });
    window.addEventListener("mouseup", () => (this.shooting = false));
    window.addEventListener("mousemove", (e) => {
      if (document.pointerLockElement !== this.canvas) return;
      const s = BASE_SENSITIVITY * this.sensMultiplier;
      this.yaw -= e.movementX * s;
      this.pitch -= e.movementY * s;
      this.pitch = Math.max(-MAX_PITCH, Math.min(MAX_PITCH, this.pitch));
    });
  }

  // ── touch ──
  private initTouch(): void {
    const fire = document.getElementById("btn-fire");
    const jump = document.getElementById("btn-jump");
    fire?.addEventListener("touchstart", (e) => {
      e.preventDefault();
      e.stopPropagation();
      this.touchFire = true;
    });
    fire?.addEventListener("touchend", (e) => {
      e.preventDefault();
      this.touchFire = false;
    });
    jump?.addEventListener("touchstart", (e) => {
      e.preventDefault();
      e.stopPropagation();
      this.touchJump = true;
    });
    jump?.addEventListener("touchend", (e) => {
      e.preventDefault();
      this.touchJump = false;
    });

    const stickBase = document.getElementById("stick")!;
    const knob = document.getElementById("stick-knob")!;

    window.addEventListener(
      "touchstart",
      (e) => {
        for (const t of Array.from(e.changedTouches)) {
          const el = t.target as HTMLElement;
          if (el.closest?.("#btn-fire,#btn-jump,#menu,button,input")) continue;
          if (t.clientX < window.innerWidth * 0.45 && this.stickId === null) {
            this.stickId = t.identifier;
            this.stickOrigin = { x: t.clientX, y: t.clientY };
            stickBase.style.left = `${t.clientX}px`;
            stickBase.style.top = `${t.clientY}px`;
            stickBase.classList.add("active");
          } else if (this.aimId === null) {
            this.aimId = t.identifier;
            this.aimLast = { x: t.clientX, y: t.clientY };
          }
        }
      },
      { passive: false },
    );

    window.addEventListener(
      "touchmove",
      (e) => {
        // Only consume the move when it belongs to a touch we actually own
        // (the movement stick or the aim drag). Any other finger — e.g. one
        // scrolling the menu/lobby overlay — must fall through to the browser
        // so native scrolling keeps working. Unconditionally preventing the
        // default here is what used to lock the mobile homepage.
        let handled = false;
        for (const t of Array.from(e.changedTouches)) {
          if (t.identifier === this.stickId) {
            const R = 56;
            let dx = t.clientX - this.stickOrigin.x;
            let dy = t.clientY - this.stickOrigin.y;
            const len = Math.hypot(dx, dy);
            if (len > R) {
              dx = (dx / len) * R;
              dy = (dy / len) * R;
            }
            this.stickVec = { x: dx / R, y: dy / R };
            knob.style.transform = `translate(${dx}px, ${dy}px)`;
            handled = true;
          } else if (t.identifier === this.aimId) {
            const s = BASE_SENSITIVITY * this.sensMultiplier * TOUCH_AIM_FACTOR;
            this.yaw -= (t.clientX - this.aimLast.x) * s;
            this.pitch -= (t.clientY - this.aimLast.y) * s;
            this.pitch = Math.max(-MAX_PITCH, Math.min(MAX_PITCH, this.pitch));
            this.aimLast = { x: t.clientX, y: t.clientY };
            handled = true;
          }
        }
        if (handled && e.cancelable) e.preventDefault();
      },
      { passive: false },
    );

    const endTouch = (e: TouchEvent) => {
      for (const t of Array.from(e.changedTouches)) {
        if (t.identifier === this.stickId) {
          this.stickId = null;
          this.stickVec = { x: 0, y: 0 };
          knob.style.transform = "translate(0,0)";
          stickBase.classList.remove("active");
        }
        if (t.identifier === this.aimId) this.aimId = null;
      }
    };
    window.addEventListener("touchend", endTouch);
    window.addEventListener("touchcancel", endTouch);
  }

  get locked(): boolean {
    return this.touch || document.pointerLockElement === this.canvas;
  }

  frame(): PlayerInput {
    return {
      sequence: this.sequence++,
      forward: this.keys.has("KeyW") || this.keys.has("ArrowUp") || this.stickVec.y < -STICK_DEADZONE,
      backward: this.keys.has("KeyS") || this.keys.has("ArrowDown") || this.stickVec.y > STICK_DEADZONE,
      left: this.keys.has("KeyA") || this.keys.has("ArrowLeft") || this.stickVec.x < -STICK_DEADZONE,
      right: this.keys.has("KeyD") || this.keys.has("ArrowRight") || this.stickVec.x > STICK_DEADZONE,
      jump: this.keys.has("Space") || this.touchJump,
      shoot: this.shooting || this.touchFire,
      yaw: this.yaw,
      pitch: this.pitch,
    };
  }

  setView(yaw: number, pitch: number): void {
    this.yaw = yaw;
    this.pitch = pitch;
  }
}
