#!/usr/bin/env python3
"""Prototype OSM → AABB baker for the Rome EUR map.

Reads rome-corridor.json (Overpass out geom), projects to metres around the
corridor centroid, decomposes each building polygon into axis-aligned boxes
via 1 m rasterization + greedy rectangle cover, and emits:
  - rome-preview.svg   top-down preview (streets, buildings, basilica)
  - rome-stats.txt     numbers for the report
"""
import json, math, sys

S = "/private/tmp/claude-501/-Users-klauskopper-Desktop-zombiezap-zombiezap/f605c19d-9288-41de-9659-ae267b373273/scratchpad"
BASILICA_WAY = 23432370

d = json.load(open(f"{S}/rome-corridor.json"))
els = d["elements"]

streets = [e for e in els if e.get("tags", {}).get("highway") and e["tags"]["highway"] != "pedestrian" and "building" not in e.get("tags", {})]
builds = [e for e in els if "building" in e.get("tags", {})]
squares = [e for e in els if e.get("tags", {}).get("highway") == "pedestrian" or e.get("tags", {}).get("place") == "square"]

# ── projection ─────────────────────────────────────────────────────────────
pts = [p for e in streets for p in e.get("geometry", [])]
if not pts:
    sys.exit("no street geometry — check the query result")
lat0 = sum(p["lat"] for p in pts) / len(pts)
lon0 = sum(p["lon"] for p in pts) / len(pts)
MLAT = 111320.0
MLON = 111320.0 * math.cos(math.radians(lat0))

def xy(p):
    # x east, z south (screen-friendly); metres relative to centroid
    return ((p["lon"] - lon0) * MLON, -(p["lat"] - lat0) * MLAT)

def parse_height(tags):
    h = tags.get("height")
    if h:
        try:
            return float(str(h).replace("m", "").strip())
        except ValueError:
            pass
    lv = tags.get("building:levels")
    if lv:
        try:
            return float(lv) * 3.2 + 1.0
        except ValueError:
            pass
    return 9.0  # EUR block default

# ── polygon → AABB decomposition (1 m grid + greedy rectangles) ────────────
def point_in_poly(x, z, poly):
    inside = False
    n = len(poly)
    for i in range(n):
        x1, z1 = poly[i]
        x2, z2 = poly[(i + 1) % n]
        if (z1 > z) != (z2 > z):
            t = (z - z1) / (z2 - z1)
            if x < x1 + t * (x2 - x1):
                inside = not inside
    return inside

def decompose(poly, cell=1.0):
    xs = [p[0] for p in poly]; zs = [p[1] for p in poly]
    x0, x1 = min(xs), max(xs); z0, z1 = min(zs), max(zs)
    nx = max(1, int(math.ceil((x1 - x0) / cell)))
    nz = max(1, int(math.ceil((z1 - z0) / cell)))
    if nx * nz > 200_000:
        return [(x0, z0, x1, z1)]  # degenerate guard
    grid = [[point_in_poly(x0 + (i + .5) * cell, z0 + (j + .5) * cell, poly)
             for i in range(nx)] for j in range(nz)]
    rects = []
    for j in range(nz):
        for i in range(nx):
            if not grid[j][i]:
                continue
            # grow right
            w = 1
            while i + w < nx and grid[j][i + w]:
                w += 1
            # grow down while the full row fits
            h = 1
            while j + h < nz and all(grid[j + h][i:i + w]):
                h += 1
            for jj in range(j, j + h):
                for ii in range(i, i + w):
                    grid[jj][ii] = False
            rects.append((x0 + i * cell, z0 + j * cell,
                          x0 + (i + w) * cell, z0 + (j + h) * cell))
    return rects

boxes = []       # (x0, z0, x1, z1, height, is_basilica)
basilica_poly = None
for e in builds:
    poly = [xy(p) for p in e.get("geometry", [])][:-1]  # drop closing dup
    if len(poly) < 3:
        continue
    h = parse_height(e.get("tags", {}))
    is_bas = e["id"] == BASILICA_WAY
    if is_bas:
        basilica_poly = poly
    for r in decompose(poly):
        # skip slivers under 1 m²
        if (r[2] - r[0]) * (r[3] - r[1]) < 1.0:
            continue
        boxes.append((*r, h, is_bas))

# ── extents ────────────────────────────────────────────────────────────────
allx = [b[0] for b in boxes] + [b[2] for b in boxes]
allz = [b[1] for b in boxes] + [b[3] for b in boxes]
ext = (min(allx), min(allz), max(allx), max(allz))
half = max(abs(v) for v in ext)

stats = [
    f"street segments: {len(streets)}  buildings: {len(builds)}  squares/pedestrian: {len(squares)}",
    f"AABBs after decomposition: {len(boxes)} (avg {len(boxes)/max(1,len(builds)):.1f}/building)",
    f"extent: {ext[2]-ext[0]:.0f} m x {ext[3]-ext[1]:.0f} m  -> arena_half ~ {half:.0f} m (i16 wire limit 255.9)",
    f"basilica: {'FOUND, ' + str(sum(1 for b in boxes if b[5])) + ' boxes' if basilica_poly else 'MISSING'}",
]
heights = sorted(set(round(b[4], 1) for b in boxes))
stats.append(f"height range: {heights[0]}..{heights[-1]} m")

open(f"{S}/rome-stats.txt", "w").write("\n".join(stats) + "\n")
print("\n".join(stats))

# ── SVG preview ────────────────────────────────────────────────────────────
PAD = 20
W = ext[2] - ext[0] + PAD * 2
H = ext[3] - ext[1] + PAD * 2
def sx(x): return x - ext[0] + PAD
def sz(z): return z - ext[1] + PAD

svg = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W:.0f} {H:.0f}" width="{W*2:.0f}" height="{H*2:.0f}">']
svg.append(f'<rect width="{W:.0f}" height="{H:.0f}" fill="#0c0f0c"/>')
# squares/pedestrian areas
for e in squares:
    poly = [xy(p) for p in e.get("geometry", [])]
    if len(poly) < 3: continue
    pts_s = " ".join(f"{sx(x):.1f},{sz(z):.1f}" for x, z in poly)
    svg.append(f'<polygon points="{pts_s}" fill="#1a241d" stroke="none"/>')
# streets
for e in streets:
    poly = [xy(p) for p in e.get("geometry", [])]
    pts_s = " ".join(f"{sx(x):.1f},{sz(z):.1f}" for x, z in poly)
    svg.append(f'<polyline points="{pts_s}" fill="none" stroke="#3a4440" stroke-width="8" stroke-linecap="round"/>')
# buildings
for (x0, z0, x1, z1, h, isb) in boxes:
    if isb:
        fill, stroke = "#8a6d2f", "#e8c35a"
    else:
        t = min(1.0, h / 30.0)
        g = int(70 + t * 90)
        fill, stroke = f"#2f{g:02x}38", "#57c96a"
    svg.append(f'<rect x="{sx(x0):.1f}" y="{sz(z0):.1f}" width="{x1-x0:.1f}" height="{z1-z0:.1f}" fill="{fill}" stroke="{stroke}" stroke-width="0.4"/>')
# scale bar (50 m)
svg.append(f'<line x1="{PAD}" y1="{H-8}" x2="{PAD+50}" y2="{H-8}" stroke="#9adf9f" stroke-width="2"/>')
svg.append(f'<text x="{PAD}" y="{H-12}" fill="#9adf9f" font-size="10" font-family="monospace">50 m</text>')
svg.append(f'<text x="{PAD}" y="{PAD-6}" fill="#9adf9f" font-size="12" font-family="monospace">EUR corridor: Viale SS. Pietro e Paolo + Via Eufrate — {len(boxes)} AABBs, true scale</text>')
svg.append("</svg>")
open(f"{S}/rome-preview.svg", "w").write("\n".join(svg))
print("wrote rome-preview.svg")
