// Generates the ZombieZap share package into web/:
//   og.jpg (1200×630), favicon-16.png, favicon-32.png,
//   apple-touch-icon.png (180), favicon.ico (16+32 PNG icons)
//
// Pure-JS pipeline (no native deps / no network):
//   committed pixel fonts → opentype.js glyph paths → RGBA buffer
//   → pngjs / jpeg-js encoders
//
// Re-run from repo root (or scripts/):
//   node scripts/gen-brand.mjs
//
// Determinism: PNG output is byte-stable for identical inputs. JPEG is
// encoded with jpeg-js at quality 85; the encoder is deterministic for a
// given RGBA buffer and quality on the same package version, but is not
// guaranteed stable across jpeg-js major upgrades.

import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import opentype from "opentype.js";
import { PNG } from "pngjs";
import jpeg from "jpeg-js";

const HERE = dirname(fileURLToPath(import.meta.url));
const WEB = join(HERE, "..", "web");

const pixFont = opentype.parse(readFileSync(join(HERE, "PressStart2P.ttf")).buffer);
const bodyFont = opentype.parse(readFileSync(join(HERE, "VT323.ttf")).buffer);

// Palette — near-black + high-contrast zombie green (Discord-preview legible)
const BG = [10, 12, 10, 255];
const BORDER = [36, 52, 36, 255];
const TEXT = [232, 255, 232, 255];
const MUTED = [120, 148, 120, 255];
const ZGREEN = [61, 220, 74, 255];
const ZGREEN_HOT = [110, 255, 120, 255];
const ZGREEN_DIM = [40, 140, 50, 255];
const SCAN = [0, 0, 0, 26]; // ~0.10 opacity over black

// ── RGBA bitmap ────────────────────────────────────────────────────────────
class Bitmap {
  constructor(w, h) {
    this.w = w;
    this.h = h;
    this.data = new Uint8ClampedArray(w * h * 4);
  }

  clear(c) {
    const { data } = this;
    for (let i = 0; i < data.length; i += 4) {
      data[i] = c[0];
      data[i + 1] = c[1];
      data[i + 2] = c[2];
      data[i + 3] = c[3];
    }
  }

  setPx(x, y, c) {
    if (x < 0 || y < 0 || x >= this.w || y >= this.h) return;
    const i = (y * this.w + x) * 4;
    const a = c[3] / 255;
    if (a >= 1) {
      this.data[i] = c[0];
      this.data[i + 1] = c[1];
      this.data[i + 2] = c[2];
      this.data[i + 3] = 255;
      return;
    }
    // src-over
    const da = this.data[i + 3] / 255;
    const outA = a + da * (1 - a);
    if (outA <= 0) return;
    this.data[i] = Math.round((c[0] * a + this.data[i] * da * (1 - a)) / outA);
    this.data[i + 1] = Math.round((c[1] * a + this.data[i + 1] * da * (1 - a)) / outA);
    this.data[i + 2] = Math.round((c[2] * a + this.data[i + 2] * da * (1 - a)) / outA);
    this.data[i + 3] = Math.round(outA * 255);
  }

  fillRect(x0, y0, w, h, c) {
    const x1 = Math.min(this.w, Math.ceil(x0 + w));
    const y1 = Math.min(this.h, Math.ceil(y0 + h));
    const xs = Math.max(0, Math.floor(x0));
    const ys = Math.max(0, Math.floor(y0));
    for (let y = ys; y < y1; y++) {
      for (let x = xs; x < x1; x++) this.setPx(x, y, c);
    }
  }

  // Horizontal line 1px
  hline(x0, x1, y, c) {
    if (y < 0 || y >= this.h) return;
    const a = Math.max(0, Math.min(this.w - 1, Math.min(x0, x1)));
    const b = Math.max(0, Math.min(this.w - 1, Math.max(x0, x1)));
    for (let x = a; x <= b; x++) this.setPx(x, y, c);
  }

  vline(x, y0, y1, c) {
    if (x < 0 || x >= this.w) return;
    const a = Math.max(0, Math.min(this.h - 1, Math.min(y0, y1)));
    const b = Math.max(0, Math.min(this.h - 1, Math.max(y0, y1)));
    for (let y = a; y <= b; y++) this.setPx(x, y, c);
  }
}

// ── path flatten + nonzero scanline fill (opentype glyph outlines) ─────────
function cubic(p0, p1, p2, p3, t) {
  const u = 1 - t;
  return u * u * u * p0 + 3 * u * u * t * p1 + 3 * u * t * t * p2 + t * t * t * p3;
}

function quad(p0, p1, p2, t) {
  const u = 1 - t;
  return u * u * p0 + 2 * u * t * p1 + t * t * p2;
}

function flattenPath(path, steps = 6) {
  /** @type {number[][][]} */
  const contours = [];
  let cur = [];
  let cx = 0;
  let cy = 0;
  let sx = 0;
  let sy = 0;
  for (const cmd of path.commands) {
    switch (cmd.type) {
      case "M":
        if (cur.length) contours.push(cur);
        cur = [[cmd.x, cmd.y]];
        cx = sx = cmd.x;
        cy = sy = cmd.y;
        break;
      case "L":
        cur.push([cmd.x, cmd.y]);
        cx = cmd.x;
        cy = cmd.y;
        break;
      case "C":
        for (let i = 1; i <= steps; i++) {
          const t = i / steps;
          cur.push([
            cubic(cx, cmd.x1, cmd.x2, cmd.x, t),
            cubic(cy, cmd.y1, cmd.y2, cmd.y, t),
          ]);
        }
        cx = cmd.x;
        cy = cmd.y;
        break;
      case "Q":
        for (let i = 1; i <= steps; i++) {
          const t = i / steps;
          cur.push([quad(cx, cmd.x1, cmd.x, t), quad(cy, cmd.y1, cmd.y, t)]);
        }
        cx = cmd.x;
        cy = cmd.y;
        break;
      case "Z":
        if (cur.length) {
          cur.push([sx, sy]);
          contours.push(cur);
          cur = [];
        }
        cx = sx;
        cy = sy;
        break;
      default:
        break;
    }
  }
  if (cur.length) contours.push(cur);
  return contours;
}

function fillContours(bmp, contours, color) {
  // Collect edges and y-range
  let minY = Infinity;
  let maxY = -Infinity;
  /** @type {{x0:number,y0:number,x1:number,y1:number}[]} */
  const edges = [];
  for (const c of contours) {
    for (let i = 0; i < c.length - 1; i++) {
      const [x0, y0] = c[i];
      const [x1, y1] = c[i + 1];
      if (y0 === y1) continue;
      edges.push({ x0, y0, x1, y1 });
      if (y0 < minY) minY = y0;
      if (y1 < minY) minY = y1;
      if (y0 > maxY) maxY = y0;
      if (y1 > maxY) maxY = y1;
    }
  }
  if (!edges.length) return;
  const yStart = Math.max(0, Math.floor(minY));
  const yEnd = Math.min(bmp.h - 1, Math.ceil(maxY));
  for (let y = yStart; y <= yEnd; y++) {
    const scanY = y + 0.5;
    /** @type {number[]} */
    const xs = [];
    for (const e of edges) {
      const { x0, y0, x1, y1 } = e;
      if ((scanY < y0 && scanY < y1) || (scanY >= y0 && scanY >= y1)) continue;
      const t = (scanY - y0) / (y1 - y0);
      xs.push(x0 + t * (x1 - x0));
    }
    xs.sort((a, b) => a - b);
    // nonzero: pair intersections
    for (let i = 0; i + 1 < xs.length; i += 2) {
      const a = Math.max(0, Math.floor(xs[i]));
      const b = Math.min(bmp.w - 1, Math.ceil(xs[i + 1]));
      for (let x = a; x <= b; x++) bmp.setPx(x, y, color);
    }
  }
}

function fillPath(bmp, path, color) {
  fillContours(bmp, flattenPath(path), color);
}

// ── text helpers (mirror legacy gen-brand) ─────────────────────────────────
const advance = (font, text, size) => font.getAdvanceWidth(text, size);

function drawRun(bmp, font, text, x, baselineY, size, fill) {
  if (!text) return x;
  let cur = x;
  for (const ch of text) {
    if (ch !== " ") {
      const path = font.getPath(ch, cur, baselineY, size);
      fillPath(bmp, path, fill);
    }
    cur += advance(font, ch, size);
  }
  return cur;
}

function drawLine(bmp, font, runs, x, baselineY, size) {
  let cur = x;
  for (const [text, fill] of runs) {
    cur = drawRun(bmp, font, text, cur, baselineY, size, fill);
  }
  return cur - x;
}

function fitSize(font, text, maxWidth, max) {
  let s = max;
  while (s > 8 && advance(font, text, s) > maxWidth) s -= 1;
  return s;
}

// ── pixel-grid "Z" mark (favicon + lockup) ─────────────────────────────────
// 16×16 grid, zombie-green Z on near-black — reads at 16px Discord size.
const Z_PIXELS = (() => {
  const g = Array.from({ length: 16 }, () => Array(16).fill(0));
  // top bar
  for (let c = 2; c <= 13; c++) {
    g[2][c] = 1;
    g[3][c] = 1;
  }
  // bottom bar
  for (let c = 2; c <= 13; c++) {
    g[12][c] = 1;
    g[13][c] = 1;
  }
  // diagonal
  const diag = [
    [4, 12],
    [4, 13],
    [5, 11],
    [5, 12],
    [6, 9],
    [6, 10],
    [6, 11],
    [7, 8],
    [7, 9],
    [7, 10],
    [8, 6],
    [8, 7],
    [8, 8],
    [9, 5],
    [9, 6],
    [9, 7],
    [10, 3],
    [10, 4],
    [10, 5],
    [11, 2],
    [11, 3],
    [11, 4],
  ];
  for (const [r, c] of diag) g[r][c] = 1;
  return g;
})();

function drawZMark(bmp, ox, oy, pixel, color, borderColor = null) {
  const n = 16;
  if (borderColor) {
    bmp.fillRect(ox - pixel, oy - pixel, n * pixel + 2 * pixel, n * pixel + 2 * pixel, borderColor);
  }
  bmp.fillRect(ox, oy, n * pixel, n * pixel, BG);
  for (let r = 0; r < n; r++) {
    for (let c = 0; c < n; c++) {
      if (Z_PIXELS[r][c]) {
        bmp.fillRect(ox + c * pixel, oy + r * pixel, pixel, pixel, color);
      }
    }
  }
}

// ── encoders ───────────────────────────────────────────────────────────────
function encodePng(bmp) {
  const png = new PNG({ width: bmp.w, height: bmp.h });
  // pngjs expects Buffer; copy RGBA
  Buffer.from(bmp.data.buffer, bmp.data.byteOffset, bmp.data.byteLength).copy(png.data);
  return PNG.sync.write(png, { colorType: 6, deflateLevel: 9 });
}

function encodeJpeg(bmp, quality = 85) {
  // jpeg-js expects {data: Buffer|Uint8Array, width, height}
  const frame = {
    data: Buffer.from(bmp.data.buffer, bmp.data.byteOffset, bmp.data.byteLength),
    width: bmp.w,
    height: bmp.h,
  };
  return jpeg.encode(frame, quality).data;
}

/** ICO with embedded PNG images (Vista+ / all modern browsers). */
function encodeIco(pngEntries) {
  // pngEntries: [{ size, png: Buffer }, ...]  size is width=height
  const count = pngEntries.length;
  const header = 6 + 16 * count;
  let offset = header;
  const sizes = [];
  for (const e of pngEntries) {
    sizes.push({ w: e.size, h: e.size, bytes: e.png.length, offset });
    offset += e.png.length;
  }
  const out = Buffer.alloc(offset);
  out.writeUInt16LE(0, 0); // reserved
  out.writeUInt16LE(1, 2); // type = icon
  out.writeUInt16LE(count, 4);
  let o = 6;
  for (const s of sizes) {
    out.writeUInt8(s.w >= 256 ? 0 : s.w, o++);
    out.writeUInt8(s.h >= 256 ? 0 : s.h, o++);
    out.writeUInt8(0, o++); // color palette
    out.writeUInt8(0, o++); // reserved
    out.writeUInt16LE(1, o);
    o += 2; // color planes
    out.writeUInt16LE(32, o);
    o += 2; // bits per pixel
    out.writeUInt32LE(s.bytes, o);
    o += 4;
    out.writeUInt32LE(s.offset, o);
    o += 4;
  }
  for (let i = 0; i < count; i++) {
    pngEntries[i].png.copy(out, sizes[i].offset);
  }
  return out;
}

function writeBin(path, buf) {
  writeFileSync(path, buf);
  console.log("✓", path.replace(WEB + "/", "web/"), `${buf.length} bytes`);
}

// ── favicon set ────────────────────────────────────────────────────────────
function renderFavicon(size) {
  const bmp = new Bitmap(size, size);
  bmp.clear(BG);
  // Scale 16-grid to size; leave 1 cell of padding via drawZMark border
  const pixel = Math.max(1, Math.floor(size / 16));
  const used = pixel * 16;
  const ox = Math.floor((size - used) / 2);
  const oy = Math.floor((size - used) / 2);
  // subtle border ring
  bmp.fillRect(0, 0, size, size, BORDER);
  bmp.fillRect(Math.max(1, Math.floor(size * 0.04)), Math.max(1, Math.floor(size * 0.04)), size - 2 * Math.max(1, Math.floor(size * 0.04)), size - 2 * Math.max(1, Math.floor(size * 0.04)), BG);
  drawZMark(bmp, ox, oy, pixel, ZGREEN);
  return bmp;
}

// ── OG card 1200×630 ───────────────────────────────────────────────────────
function renderOg() {
  const W = 1200;
  const H = 630;
  const bmp = new Bitmap(W, H);
  bmp.clear(BG);

  // grid
  const gridC = [BORDER[0], BORDER[1], BORDER[2], 64];
  for (let x = 0; x <= W; x += 48) bmp.vline(x, 0, H - 1, gridC);
  for (let y = 0; y <= H; y += 48) bmp.hline(0, W - 1, y, gridC);

  // frame
  const frame = [ZGREEN[0], ZGREEN[1], ZGREEN[2], 90];
  for (let t = 0; t < 3; t++) {
    bmp.fillRect(22 + t, 22 + t, W - 44 - 2 * t, 1, frame);
    bmp.fillRect(22 + t, H - 23 - t, W - 44 - 2 * t, 1, frame);
    bmp.fillRect(22 + t, 22 + t, 1, H - 44 - 2 * t, frame);
    bmp.fillRect(W - 23 - t, 22 + t, 1, H - 44 - 2 * t, frame);
  }

  // corner brackets
  const br = 7;
  const bl = 46;
  const corners = [
    [34, 34, bl, br],
    [34, 34, br, bl],
    [W - 34 - bl, 34, bl, br],
    [W - 34 - br, 34, br, bl],
    [34, H - 34 - br, bl, br],
    [34, H - 34 - bl, br, bl],
    [W - 34 - bl, H - 34 - br, bl, br],
    [W - 34 - br, H - 34 - bl, br, bl],
  ];
  for (const [x, y, w, h] of corners) bmp.fillRect(x, y, w, h, ZGREEN);

  const px = 80;
  const maxTextW = W - px * 2;

  // top-left: Z mark + zombiezap.com
  const mp = 5;
  const markX = px;
  const markY = 56;
  drawZMark(bmp, markX, markY, mp, ZGREEN_HOT, BORDER);
  const urlSize = 18;
  drawLine(
    bmp,
    pixFont,
    [
      ["zombiezap", TEXT],
      [".com", ZGREEN],
    ],
    markX + 16 * mp + 24,
    markY + (16 * mp) / 2 + urlSize * 0.36,
    urlSize,
  );

  // hero: ZOMBIEZAP
  const heroSize = Math.min(96, fitSize(pixFont, "ZOMBIEZAP", maxTextW, 110));
  const heroY = 300;
  drawLine(bmp, pixFont, [["ZOMBIEZAP", ZGREEN]], px, heroY, heroSize);

  // tagline (pixel font, muted green-white)
  const tagText = "free co-op zombie survival in your browser";
  const tagSize = fitSize(pixFont, tagText, maxTextW, 22);
  drawLine(bmp, pixFont, [[tagText, TEXT]], px, heroY + 70, tagSize);

  // value line (VT323 body)
  const sub1 = "1–5 players  ·  no account  ·  no install";
  const subSize = fitSize(bodyFont, sub1, maxTextW, 40);
  drawLine(bmp, bodyFont, [[sub1, ZGREEN_DIM]], px, heroY + 140, subSize);

  // footer
  drawLine(
    bmp,
    pixFont,
    [
      ["host a lobby", ZGREEN],
      ["  //  share the code  //  survive together", MUTED],
    ],
    px,
    H - 46,
    14,
  );

  // scanlines last
  for (let y = 0; y < H; y += 4) {
    bmp.fillRect(0, y, W, 2, SCAN);
  }

  return bmp;
}

// ── main ───────────────────────────────────────────────────────────────────
function main() {
  mkdirSync(WEB, { recursive: true });

  // Favicons
  const fav16 = renderFavicon(16);
  const fav32 = renderFavicon(32);
  const apple = renderFavicon(180);
  const png16 = encodePng(fav16);
  const png32 = encodePng(fav32);
  const pngApple = encodePng(apple);
  writeBin(join(WEB, "favicon-16.png"), png16);
  writeBin(join(WEB, "favicon-32.png"), png32);
  writeBin(join(WEB, "apple-touch-icon.png"), pngApple);

  const ico = encodeIco([
    { size: 16, png: png16 },
    { size: 32, png: png32 },
  ]);
  writeBin(join(WEB, "favicon.ico"), ico);

  // OG JPEG
  const og = renderOg();
  const jpg = encodeJpeg(og, 85);
  if (jpg.length >= 300 * 1024) {
    console.warn(`warning: og.jpg is ${jpg.length} bytes (≥ 300 KB); re-encode lower quality`);
  }
  writeBin(join(WEB, "og.jpg"), jpg);
  console.log(`  og.jpg ${og.w}×${og.h}, quality 85, ${(jpg.length / 1024).toFixed(1)} KB`);
}

main();
