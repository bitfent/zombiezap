// Generates the ShotAnte brand assets into apps/web/public:
//   favicon.svg, favicon-32.png, favicon-16.png, apple-touch-icon.png, og.png
//
// The mark is a pixel-art duel reticle (cyan ticks + red center dot — the
// in-game crosshair). Text on og.png is real glyph OUTLINES from the same OFL
// pixel fonts the site uses (opentype.js -> SVG paths -> sharp), so the social
// card matches the game exactly. Re-run with: npm run gen:brand

import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { readFileSync } from "node:fs";
import sharp from "sharp";
import opentype from "opentype.js";

const HERE = dirname(fileURLToPath(import.meta.url));
const PUB = join(HERE, "..", "apps", "web", "public");
const pixFont = opentype.parse(readFileSync(join(HERE, "PressStart2P.ttf")).buffer);
const bodyFont = opentype.parse(readFileSync(join(HERE, "VT323.ttf")).buffer);

const BG = "#0a0a12";
const BORDER = "#2c2c44";
const TEXT = "#e8ecff";
const MUTED = "#8a90b8";
const RED = "#ff3355";
const RED_HOT = "#ff5577";
const CYAN = "#2ee6d6";
const AMBER = "#ffd23f";

// ── the reticle mark, on a 16×16 pixel grid ────────────────────────────────
const tick = [];
for (const r of [1, 2, 3, 4, 11, 12, 13, 14]) for (const c of [7, 8]) tick.push([r, c]);
for (const c of [1, 2, 3, 4, 11, 12, 13, 14]) for (const r of [7, 8]) tick.push([r, c]);
const DOT = [
  [7, 7], [7, 8], [8, 7], [8, 8],
];

function rects(pixels, color, p, ox = 0, oy = 0) {
  return pixels
    .map(([r, c]) => `<rect x="${ox + c * p}" y="${oy + r * p}" width="${p}" height="${p}" fill="${color}"/>`)
    .join("");
}

// ── favicon.svg (16×16 grid → 256px) ───────────────────────────────────────
const P = 16;
const faviconSvg = `<svg xmlns="http://www.w3.org/2000/svg" width="256" height="256" viewBox="0 0 256 256" shape-rendering="crispEdges">
<rect width="256" height="256" fill="${BG}"/>
<rect x="4" y="4" width="248" height="248" fill="none" stroke="${BORDER}" stroke-width="8"/>
${rects(tick, CYAN, P)}
${rects(DOT, RED, P)}
</svg>`;

async function svgToPng(svg, size, out) {
  await sharp(Buffer.from(svg)).resize(size, size).png().toFile(join(PUB, out));
  console.log("✓", out, `${size}×${size}`);
}

// ── text → SVG paths (one <path> per glyph; both fonts are monospaced) ─────
const advance = (font, text, size) => font.getAdvanceWidth(text, size);

function runPath(font, text, x, baselineY, size, fill) {
  if (!text) return "";
  let cur = x;
  let svg = "";
  for (const ch of text) {
    if (ch !== " ") {
      const d = font.getPath(ch, cur, baselineY, size).toPathData(1);
      if (d) svg += `<path d="${d}" fill="${fill}"/>`;
    }
    cur += advance(font, ch, size);
  }
  return svg;
}

function line(font, runs, x, baselineY, size) {
  let cur = x;
  let svg = "";
  for (const [text, fill] of runs) {
    svg += runPath(font, text, cur, baselineY, size, fill);
    cur += advance(font, text, size);
  }
  return { svg, width: cur - x };
}

function fitSize(font, text, maxWidth, max) {
  let s = max;
  while (s > 8 && advance(font, text, s) > maxWidth) s -= 1;
  return s;
}

async function main() {
  const fs = await import("node:fs/promises");
  await fs.mkdir(PUB, { recursive: true });
  await fs.writeFile(join(PUB, "favicon.svg"), faviconSvg);
  console.log("✓ favicon.svg");
  await svgToPng(faviconSvg, 32, "favicon-32.png");
  await svgToPng(faviconSvg, 16, "favicon-16.png");
  await svgToPng(faviconSvg, 180, "apple-touch-icon.png");

  // ── og.png (1200×630) ────────────────────────────────────────────────────
  const W = 1200, H = 630;
  const px = 80;
  const maxTextW = W - px * 2;

  let grid = "";
  for (let x = 0; x <= W; x += 48) grid += `<line x1="${x}" y1="0" x2="${x}" y2="${H}" stroke="${BORDER}" stroke-opacity="0.25"/>`;
  for (let y = 0; y <= H; y += 48) grid += `<line x1="0" y1="${y}" x2="${W}" y2="${y}" stroke="${BORDER}" stroke-opacity="0.25"/>`;

  let scan = "";
  for (let y = 0; y < H; y += 4) scan += `<rect x="0" y="${y}" width="${W}" height="2" fill="#000000" opacity="0.10"/>`;

  const br = 7, bl = 46;
  const corners = `
<rect x="34" y="34" width="${bl}" height="${br}" fill="${RED}"/><rect x="34" y="34" width="${br}" height="${bl}" fill="${RED}"/>
<rect x="${W - 34 - bl}" y="34" width="${bl}" height="${br}" fill="${RED}"/><rect x="${W - 34 - br}" y="34" width="${br}" height="${bl}" fill="${RED}"/>
<rect x="34" y="${H - 34 - br}" width="${bl}" height="${br}" fill="${RED}"/><rect x="34" y="${H - 34 - bl}" width="${br}" height="${bl}" fill="${RED}"/>
<rect x="${W - 34 - bl}" y="${H - 34 - br}" width="${bl}" height="${br}" fill="${RED}"/><rect x="${W - 34 - br}" y="${H - 34 - bl}" width="${br}" height="${bl}" fill="${RED}"/>`;

  // top-left: reticle mark + url
  const mp = 5;
  const markX = px, markY = 56;
  const urlSize = 18;
  const lockup = line(
    pixFont,
    [["shotante", TEXT], [".com", RED]],
    markX + 16 * mp + 24,
    markY + (16 * mp) / 2 + urlSize * 0.36,
    urlSize,
  ).svg;

  // hero: SHOTANTE
  const heroSize = Math.min(118, fitSize(pixFont, "SHOTANTE", maxTextW, 130));
  const heroY = 318;
  const hero = line(pixFont, [["SHOT", TEXT], ["ANTE", RED]], px, heroY, heroSize).svg;

  // tagline (pixel, amber)
  const tagText = "ANTE UP // DUEL // WINNER TAKES THE POT";
  const tagSize = fitSize(pixFont, tagText, maxTextW, 24);
  const tag = line(pixFont, [[tagText, AMBER]], px, heroY + 74, tagSize).svg;

  // value prop (VT323)
  const sub1Text = "Retro 1v1 duels in your browser. No install.";
  const sub2Text = "Ante ETH or USDC on Base - first to 5 kills takes the pot.";
  const subSize = fitSize(bodyFont, sub2Text, maxTextW, 40);
  const sub1 = line(bodyFont, [[sub1Text, CYAN]], px, heroY + 142, subSize).svg;
  const sub2 = line(bodyFont, [[sub2Text, MUTED]], px, heroY + 142 + subSize + 6, subSize).svg;

  // fine print
  const foot = line(
    pixFont,
    [["free practice", CYAN], ["  //  2-minute duels  //  non-custodial escrow", MUTED]],
    px,
    H - 46,
    14,
  ).svg;

  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H}" viewBox="0 0 ${W} ${H}">
<rect width="${W}" height="${H}" fill="${BG}"/>
<g shape-rendering="crispEdges">
${grid}
<rect x="22" y="22" width="${W - 44}" height="${H - 44}" fill="none" stroke="${CYAN}" stroke-opacity="0.35" stroke-width="3"/>
${corners}
${rects(tick, CYAN, mp, markX, markY)}
${rects(DOT, RED_HOT, mp, markX, markY)}
</g>
${lockup}
${hero}
${tag}
${sub1}
${sub2}
${foot}
<g shape-rendering="crispEdges">${scan}</g>
</svg>`;

  await sharp(Buffer.from(svg)).png({ compressionLevel: 9, effort: 10 }).toFile(join(PUB, "og.png"));
  console.log("✓ og.png", `${W}×${H}`, `hero ${heroSize}px`);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
