//! Map rendering: turns a zz-core GameMap (AABBs + billboards) into merged
//! meshes with procedural textures. The SEAM TYPES here are fixed; the
//! implementation fills in behind them.
//!
//! Contract:
//! - the app inserts/replaces [`CurrentMap`] when a map should be (re)built;
//! - this plugin despawns any previous [`MapRoot`] and every [`Placeholder`]
//!   entity, then spawns the whole map under ONE root entity tagged
//!   [`MapRoot`];
//! - zero asset files: all textures are generated in code (procedural noise /
//!   stripes), billboards are unlit backlit "YOUR AD HERE"-style striped
//!   panels keyed by `ad_slot`;
//! - walls are grouped into material families by ShotAnte heuristics
//!   (perimeter / roof / building A·B / street cover) and merged into ONE
//!   mesh per family — a handful of draw calls total;
//! - ShotAnte `concreteTexture` seam-grid panels (128px, grain + 2px seams);
//! - Urban: two map-seeded pastel building hues; other envs keep identity;
//! - edge-trim `LineList` outlines on street furniture only;
//! - warm window slits on tall Building faces, merged into ONE emissive mesh;
//! - voxel cloud box clusters under a single drift root (one transform
//!   per frame for the whole layer);
//! - M28 MountainTown-only dressing (slope skirts, pines, rocks, horizon
//!   ridge, band-tinted ground) via [`crate::mountain_dressing`] — gated on
//!   `EnvKind`, zero sim / WalkGrid impact.
//!
//! **Barrels:** `GameMap` does not carry barrel indices (only walls/spawns/…);
//! explosive-barrel meshes are skipped until zz-core exposes them.

use std::f32::consts::PI;

use bevy::{
    asset::RenderAssetUsages,
    camera::ClearColorConfig,
    image::{Image, ImageAddressMode, ImageSampler, ImageSamplerDescriptor},
    mesh::Indices,
    pbr::{DistanceFog, FogFalloff},
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
};
use zz_core::map::GameMap;
use zz_core::rng::Mulberry32;
use zz_core::types::{Aabb, EnvKind};

/// The map the world should currently display. Insert or overwrite to
/// trigger a (re)build.
#[derive(Resource)]
#[allow(dead_code)] // constructed by the M3 session wiring
pub struct CurrentMap(pub GameMap);

/// Root entity of the spawned map (despawn = whole map gone).
#[derive(Component)]
#[allow(dead_code)] // constructed by the renderer implementation
pub struct MapRoot;

/// Skeleton-scene entities that any real map replaces.
#[derive(Component)]
#[allow(dead_code)] // tags skeleton-scene entities; queried by the renderer
pub struct Placeholder;

pub struct MapRenderPlugin;

/// UV texel density: ~0.5 UV units per world metre (consistent across faces).
const UV_PER_M: f32 = 0.5;
/// How close an AABB face must be to ±arena_half to count as perimeter.
const PERIMETER_EDGE_EPS: f32 = 1.5;
/// Accent mix factor into wall family base colors.
const ACCENT_MIX: f32 = 0.08;
/// Frame thickness around billboards (metres).
const BILLBOARD_FRAME_T: f32 = 0.08;
const BILLBOARD_FRAME_DEPTH: f32 = 0.06;

// ── Per-env light & atmosphere (ShotAnte's buildWorld recipe, per env) ─────

/// Baked-shadow tuning. Factors multiply into vertex colors / the ground
/// texture at map build time; real-time shadow maps stay off everywhere.
const GROUND_SHADOW: f32 = 0.62;
/// Base light a wall face gets regardless of sun exposure (interiors).
const FACE_AMBIENT: f32 = 0.55;
/// Hard floor for any baked factor — the "never pitch black" invariant.
pub const MIN_LIGHT: f32 = 0.42;
/// Small-map historical baselines (Urban/Mountain/Desert/Sea, arena_half≈30).
const BASE_OCCLUSION_RES: u32 = 256;
const BASE_GROUND_TEX_RES: u32 = 1024;
/// Hard GPU / CPU memory cap for procedural bake textures (edge length).
const MAX_BAKE_TEX_RES: u32 = 2048;
/// Ground texture: at least this many texels per world metre.
const GROUND_TEXELS_PER_M: f32 = 2.0;
/// Occlusion grid: each cell at most this many metres across.
const MAX_OCCLUSION_CELL_M: f32 = 2.0;

/// Ground texture resolution for a map of half-extent `arena_half`.
///
/// Small towns keep the historical 1024; larger arenas grow to meet
/// ≥[`GROUND_TEXELS_PER_M`] texels/m, hard-capped at [`MAX_BAKE_TEX_RES`].
pub fn ground_tex_res(arena_half: f32) -> u32 {
    let diameter = (2.0 * arena_half).max(1.0);
    let needed = (diameter * GROUND_TEXELS_PER_M).ceil() as u32;
    needed
        .max(BASE_GROUND_TEX_RES)
        .next_power_of_two()
        .clamp(1, MAX_BAKE_TEX_RES)
}

/// Ground-occlusion bake resolution for a map of half-extent `arena_half`.
///
/// Small towns keep 256; larger arenas grow so each cell is
/// ≤[`MAX_OCCLUSION_CELL_M`] m, hard-capped at [`MAX_BAKE_TEX_RES`].
pub fn occlusion_res(arena_half: f32) -> u32 {
    let diameter = (2.0 * arena_half).max(1.0);
    let needed = (diameter / MAX_OCCLUSION_CELL_M).ceil() as u32;
    needed
        .max(BASE_OCCLUSION_RES)
        .next_power_of_two()
        .clamp(1, MAX_BAKE_TEX_RES)
}

/// Everything the atmosphere needs, per environment.
#[derive(Clone, Copy, Debug)]
pub struct EnvLighting {
    /// Sky: camera clear color and fog color.
    pub sky: Color,
    /// Hemisphere-style fill (`GlobalAmbientLight`) — what keeps interiors
    /// dim-but-readable instead of pitch black.
    pub ambient: Color,
    pub ambient_brightness: f32,
    pub sun_color: Color,
    pub sun_illuminance: f32,
    /// Unit vector pointing from the world TOWARD the sun (occlusion rays
    /// travel along this; the light itself shines along `-sun_to`).
    pub sun_to: Vec3,
}

/// Total per-env atmosphere table. Urban is the ShotAnte reference recipe
/// (sky 0x9ec9ef, warm 0xfff3da sun from (16, 28, 12)); the other three are
/// distinct moods for item 5's environments.
pub fn env_lighting(env: EnvKind) -> EnvLighting {
    match env {
        EnvKind::Urban => EnvLighting {
            sky: Color::srgb_u8(158, 201, 239),
            ambient: Color::srgb_u8(150, 158, 168),
            ambient_brightness: 950.0,
            sun_color: Color::srgb_u8(255, 243, 218),
            sun_illuminance: 11_000.0,
            sun_to: Vec3::new(16.0, 28.0, 12.0).normalize(),
        },
        EnvKind::MountainTown => EnvLighting {
            sky: Color::srgb_u8(143, 190, 222),
            ambient: Color::srgb_u8(140, 155, 170),
            ambient_brightness: 900.0,
            sun_color: Color::srgb_u8(255, 248, 235),
            sun_illuminance: 12_500.0,
            sun_to: Vec3::new(10.0, 30.0, 14.0).normalize(),
        },
        EnvKind::DesertTown => EnvLighting {
            sky: Color::srgb_u8(232, 206, 158),
            ambient: Color::srgb_u8(196, 172, 132),
            ambient_brightness: 1_050.0,
            sun_color: Color::srgb_u8(255, 230, 185),
            sun_illuminance: 13_000.0,
            sun_to: Vec3::new(20.0, 24.0, 8.0).normalize(),
        },
        EnvKind::SeaTown => EnvLighting {
            sky: Color::srgb_u8(151, 197, 222),
            ambient: Color::srgb_u8(150, 166, 178),
            ambient_brightness: 1_000.0,
            sun_color: Color::srgb_u8(255, 242, 214),
            sun_illuminance: 11_500.0,
            sun_to: Vec3::new(-14.0, 26.0, 16.0).normalize(),
        },
        // Warm Mediterranean late-afternoon: blue sky with a soft horizon,
        // golden sun from the southwest, warm grey fill so the basilica nave
        // reads dim-but-luminous. Map axes: +z is south (piazzale spawns face
        // +z toward the portal on the north facade).
        EnvKind::RomeEur => EnvLighting {
            sky: Color::srgb_u8(168, 196, 230),
            ambient: Color::srgb_u8(168, 158, 142),
            ambient_brightness: 1_000.0,
            sun_color: Color::srgb_u8(255, 236, 200),
            sun_illuminance: 12_000.0,
            sun_to: Vec3::new(-18.0, 20.0, 14.0).normalize(),
        },
    }
}

/// ShotAnte urban pastel palette (hex RGB). Two are picked per map seed.
pub const URBAN_PASTELS: [u32; 6] = [
    0x00e8_d8b8,
    0x00d9_c4ad,
    0x00c9_d6c2,
    0x00c4_cede,
    0x00dc_c6c6,
    0x00cf_d8c0,
];

/// Flat slate roof (ShotAnte `roofMat` colour).
const ROOF_SLATE: u32 = 0x004b_5364;
/// Dark edge trim on bulky street cover.
const DARK_TRIM: u32 = 0x006b_542f;
/// Edge-trim scale: sits just outside the faces (ShotAnte 1.003).
const TRIM_SCALE: f32 = 1.003;
/// Small-prop threshold (w ≤ 1.3 && d ≤ 1.3 → accent trim).
const SMALL_PROP_MAX: f32 = 1.3;

/// Wall-family base colors
/// (Perimeter, Cover, BuildingA, BuildingB, Roof) before accent mix.
/// Urban BuildingA/B are placeholders — rebuild overwrites with map pastels.
/// Each env has a distinct family so the town reads at a glance.
fn family_base_colors(env: EnvKind) -> [Color; 5] {
    match env {
        // Pastel city: near-white walls get hue from materials; slate roofs.
        EnvKind::Urban => [
            Color::WHITE, // perimeter colour lives in the texture
            Color::WHITE, // crate colour lives in the texture
            color_u32(URBAN_PASTELS[0]),
            color_u32(URBAN_PASTELS[1]),
            color_u32(ROOF_SLATE),
        ],
        // Timber-and-stone: grey rock perimeter, dark wood cover/cabins, stone roofs.
        EnvKind::MountainTown => [
            Color::srgb(0.50, 0.50, 0.52),
            Color::srgb(0.40, 0.28, 0.18),
            Color::srgb(0.38, 0.26, 0.16),
            Color::srgb(0.42, 0.30, 0.18),
            Color::srgb(0.52, 0.50, 0.46),
        ],
        // Adobe: sand/ochre walls, flat sand roofs, dusty cover crates.
        EnvKind::DesertTown => [
            Color::srgb(0.70, 0.60, 0.46),
            Color::srgb(0.68, 0.50, 0.32),
            Color::srgb(0.86, 0.72, 0.50),
            Color::srgb(0.80, 0.68, 0.48),
            Color::srgb(0.72, 0.58, 0.40),
        ],
        // Weathered dock: blue-grey warehouses, bleached timber, dock crates.
        EnvKind::SeaTown => [
            Color::srgb(0.52, 0.56, 0.60),
            Color::srgb(0.60, 0.52, 0.40),
            Color::srgb(0.56, 0.60, 0.66),
            Color::srgb(0.52, 0.58, 0.64),
            Color::srgb(0.58, 0.54, 0.46),
        ],
        // Travertine: cream stone, warm planters, warm roof stone.
        EnvKind::RomeEur => [
            Color::srgb(0.72, 0.68, 0.60),
            Color::srgb(0.78, 0.62, 0.42),
            Color::srgb(0.92, 0.86, 0.74),
            Color::srgb(0.88, 0.82, 0.70),
            Color::srgb(0.48, 0.42, 0.36),
        ],
    }
}

fn color_u32(rgb: u32) -> Color {
    Color::srgb_u8(
        ((rgb >> 16) & 0xff) as u8,
        ((rgb >> 8) & 0xff) as u8,
        (rgb & 0xff) as u8,
    )
}

/// Pick two distinct pastel hues from [`URBAN_PASTELS`] with a map-seeded rng
/// (`"{seed}-paint"`, matching ShotAnte's paint stream).
pub fn pick_urban_pastels(seed: &str) -> (Color, Color) {
    let mut rng = Mulberry32::from_seed(&format!("{seed}-paint"));
    let n = URBAN_PASTELS.len();
    let ia = (rng.next() * n as f64).floor() as usize % n;
    let mut ib = (rng.next() * n as f64).floor() as usize % n;
    if ia == ib {
        ib = (ib + 1) % n;
    }
    (color_u32(URBAN_PASTELS[ia]), color_u32(URBAN_PASTELS[ib]))
}

/// Per-env RGB multipliers applied when baking wall/cover/roof textures so the
/// procedural detail itself carries the env hue (not only the material base).
fn env_texture_tint(env: EnvKind) -> [f32; 3] {
    match env {
        EnvKind::Urban => [1.0, 1.0, 1.0],
        EnvKind::MountainTown => [0.88, 0.78, 0.66], // warm timber wash
        EnvKind::DesertTown => [1.08, 0.96, 0.72],   // sand / ochre
        EnvKind::SeaTown => [0.86, 0.92, 0.98],      // cool bleached blue-grey
        EnvKind::RomeEur => [1.06, 1.00, 0.90],      // cream travertine
    }
}

/// Wall material family for merged draw calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WallFamily {
    Perimeter,
    Cover,
    /// Building facade hue A (map side `(x0+z0) > 0`).
    BuildingA,
    /// Building facade hue B (map side `(x0+z0) ≤ 0`).
    BuildingB,
    Roof,
}

impl WallFamily {
    pub fn is_building(self) -> bool {
        matches!(self, WallFamily::BuildingA | WallFamily::BuildingB)
    }
}

/// Pure vertex/index buffer produced by the geometry builder.
#[derive(Clone, Debug, Default)]
pub struct MeshGeom {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
    /// Baked light factors as vertex colors (empty = attribute omitted).
    /// Filled by [`bake_face_colors`]; StandardMaterial multiplies them in.
    pub colors: Vec<[f32; 4]>,
}

impl MeshGeom {
    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    /// Append a solid box (six faces, CCW outward). Used by map families and
    /// mountain dressing rock outcrops.
    pub fn append_box(&mut self, aabb: &Aabb) {
        let x0 = aabb.x0;
        let x1 = aabb.x1;
        let y0 = aabb.y0;
        let y1 = aabb.y1;
        let z0 = aabb.z0;
        let z1 = aabb.z1;

        let sx = (x1 - x0).abs();
        let sy = (y1 - y0).abs();
        let sz = (z1 - z0).abs();

        // Six faces: +Y, -Y, +X, -X, +Z, -Z. Each: 4 verts, 2 tris.
        // UV axes track the two spanning world axes of the face.
        // Corner order is CCW when the attribute normal points toward the
        // viewer so cross(P1−P0, P2−P0) matches the vertex normal (Back cull).
        // +Y (top) — U along ±X, V along ±Z; cross(+X,−Z)=+Y
        self.push_face(
            [[x0, y1, z1], [x1, y1, z1], [x1, y1, z0], [x0, y1, z0]],
            [0.0, 1.0, 0.0],
            sx,
            sz,
        );
        // -Y (bottom) — U along ±X, V along ±Z; cross(+X,+Z)=−Y
        self.push_face(
            [[x0, y0, z0], [x1, y0, z0], [x1, y0, z1], [x0, y0, z1]],
            [0.0, -1.0, 0.0],
            sx,
            sz,
        );
        // +X — U along ±Z, V along ±Y; cross(−Z,+Y)=+X
        self.push_face(
            [[x1, y0, z1], [x1, y0, z0], [x1, y1, z0], [x1, y1, z1]],
            [1.0, 0.0, 0.0],
            sz,
            sy,
        );
        // -X — U along ±Z, V along ±Y; cross(+Z,+Y)=−X
        self.push_face(
            [[x0, y0, z0], [x0, y0, z1], [x0, y1, z1], [x0, y1, z0]],
            [-1.0, 0.0, 0.0],
            sz,
            sy,
        );
        // +Z — U along X, V along Y; cross(+X,+Y)=+Z
        self.push_face(
            [[x0, y0, z1], [x1, y0, z1], [x1, y1, z1], [x0, y1, z1]],
            [0.0, 0.0, 1.0],
            sx,
            sy,
        );
        // -Z — U along −X, V along Y; cross(−X,+Y)=−Z
        self.push_face(
            [[x1, y0, z0], [x0, y0, z0], [x0, y1, z0], [x1, y1, z0]],
            [0.0, 0.0, -1.0],
            sx,
            sy,
        );
    }

    /// Quad with CCW winding when the outward normal points toward the viewer
    /// (geometric normal from indices matches `normal`).
    pub fn push_face(
        &mut self,
        corners: [[f32; 3]; 4],
        normal: [f32; 3],
        u_size: f32,
        v_size: f32,
    ) {
        let base = self.positions.len() as u32;
        let u_max = u_size * UV_PER_M;
        let v_max = v_size * UV_PER_M;
        let uvs = [[0.0, 0.0], [u_max, 0.0], [u_max, v_max], [0.0, v_max]];
        for i in 0..4 {
            self.positions.push(corners[i]);
            self.normals.push(normal);
            self.uvs.push(uvs[i]);
        }
        // two triangles: 0-1-2, 0-2-3
        self.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    fn into_mesh(self) -> Mesh {
        let mesh = Mesh::new(
            bevy::mesh::PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, self.uvs)
        .with_inserted_indices(Indices::U32(self.indices));
        if self.colors.is_empty() {
            mesh
        } else {
            mesh.with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, self.colors)
        }
    }
}

// ── Baked sun shadows (once per map build; zero per-frame cost) ────────────

/// Does a ray from `origin` along `dir` hit any AABB? Slab method.
fn ray_hits_any(origin: Vec3, dir: Vec3, boxes: &[Aabb]) -> bool {
    let inv = Vec3::new(1.0 / dir.x, 1.0 / dir.y, 1.0 / dir.z);
    for b in boxes {
        let t1 = (b.x0 - origin.x) * inv.x;
        let t2 = (b.x1 - origin.x) * inv.x;
        let t3 = (b.y0 - origin.y) * inv.y;
        let t4 = (b.y1 - origin.y) * inv.y;
        let t5 = (b.z0 - origin.z) * inv.z;
        let t6 = (b.z1 - origin.z) * inv.z;
        let tmin = t1.min(t2).max(t3.min(t4)).max(t5.min(t6));
        let tmax = t1.max(t2).min(t3.max(t4)).min(t5.max(t6));
        if tmax >= tmin.max(0.0) {
            return true;
        }
    }
    false
}

/// Sun-occlusion factor for a ground point: 1.0 in the open, [`GROUND_SHADOW`]
/// under a building's shadow; 4 jittered samples soften the edge.
fn ground_light_at(x: f32, z: f32, sun_to: Vec3, walls: &[Aabb]) -> f32 {
    const JITTER: [(f32, f32); 4] = [(-0.35, -0.35), (0.35, -0.35), (-0.35, 0.35), (0.35, 0.35)];
    let mut lit = 0.0;
    for (jx, jz) in JITTER {
        let origin = Vec3::new(x + jx, 0.05, z + jz);
        if !ray_hits_any(origin, sun_to, walls) {
            lit += 0.25;
        }
    }
    GROUND_SHADOW + (1.0 - GROUND_SHADOW) * lit
}

/// Bake the ground occlusion grid: `res`×`res` factors covering
/// [-half, half]² (row-major, +x fastest). Each factor is in
/// [[`GROUND_SHADOW`], 1.0].
pub fn bake_ground_occlusion(res: u32, half: f32, sun_to: Vec3, walls: &[Aabb]) -> Vec<f32> {
    let mut grid = Vec::with_capacity((res * res) as usize);
    for iz in 0..res {
        let z = ((iz as f32 + 0.5) / res as f32 - 0.5) * 2.0 * half;
        for ix in 0..res {
            let x = ((ix as f32 + 0.5) / res as f32 - 0.5) * 2.0 * half;
            grid.push(ground_light_at(x, z, sun_to, walls));
        }
    }
    grid
}

/// Bilinear sample of the occlusion grid at texture-space (u, v) in 0..1.
fn sample_occlusion(grid: &[f32], res: u32, u: f32, v: f32) -> f32 {
    let fx = (u * res as f32 - 0.5).clamp(0.0, (res - 1) as f32);
    let fz = (v * res as f32 - 0.5).clamp(0.0, (res - 1) as f32);
    let x0 = fx as u32;
    let z0 = fz as u32;
    let x1 = (x0 + 1).min(res - 1);
    let z1 = (z0 + 1).min(res - 1);
    let kx = fx - x0 as f32;
    let kz = fz - z0 as f32;
    let g = |x: u32, z: u32| grid[(z * res + x) as usize];
    let a = g(x0, z0) * (1.0 - kx) + g(x1, z0) * kx;
    let b = g(x0, z1) * (1.0 - kx) + g(x1, z1) * kx;
    a * (1.0 - kz) + b * kz
}

/// Baked light factor for one wall face: ambient base + sun diffuse gated by
/// a shadow ray from the face center. Clamped to [[`MIN_LIGHT`], 1.0] — a
/// face under a roof comes out dim-but-readable, never black.
pub fn face_light_factor(center: Vec3, normal: Vec3, sun_to: Vec3, walls: &[Aabb]) -> f32 {
    let ndl = normal.dot(sun_to).max(0.0);
    let vis = if ndl > 0.0 && !ray_hits_any(center + normal * 0.05, sun_to, walls) {
        1.0
    } else {
        0.0
    };
    (FACE_AMBIENT + (1.0 - FACE_AMBIENT) * ndl * vis).clamp(MIN_LIGHT, 1.0)
}

/// Fill `geom.colors` with per-face baked light (faces are 4-vertex quads —
/// exactly how [`MeshGeom::push_face`] lays them out).
pub fn bake_face_colors(geom: &mut MeshGeom, sun_to: Vec3, walls: &[Aabb]) {
    let faces = geom.positions.len() / 4;
    let mut colors = Vec::with_capacity(geom.positions.len());
    for f in 0..faces {
        let i = f * 4;
        let c = geom.positions[i..i + 4]
            .iter()
            .fold(Vec3::ZERO, |acc, p| acc + Vec3::from_array(*p))
            / 4.0;
        let n = Vec3::from_array(geom.normals[i]);
        let l = face_light_factor(c, n, sun_to, walls);
        for _ in 0..4 {
            colors.push([l, l, l, 1.0]);
        }
    }
    geom.colors = colors;
}

/// Classify an AABB into a wall material family.
///
/// ShotAnte paint rules (`loadArena`):
/// - perimeter: tall edge walls (geometry heuristic; urban first-4 walls)
/// - roof: `y0 >= 2.5`
/// - building: `y1 > 2.5 || (y0 ≈ 0 && y1 ≈ 1.3)` (walls + doorway
///   headers/sills) — hue A/B by `(x0 + z0) > 0`
/// - else: street cover / crates
pub fn classify_wall(aabb: &Aabb, arena_half: f32) -> WallFamily {
    let height = (aabb.y1 - aabb.y0).abs();
    if height >= 5.0 && is_near_perimeter(aabb, arena_half) {
        return WallFamily::Perimeter;
    }
    // Legacy: roof = y0 >= 2.5
    if aabb.y0 >= 2.5 {
        return WallFamily::Roof;
    }
    // Legacy: building = y1 > 2.5 || (y0 == 0 && y1 == 1.3)
    let is_sill = aabb.y0.abs() <= 1e-3 && (aabb.y1 - 1.3).abs() <= 1e-2;
    let is_building = aabb.y1 > 2.5 || is_sill;
    if is_building {
        if (aabb.x0 + aabb.z0) > 0.0 {
            WallFamily::BuildingA
        } else {
            WallFamily::BuildingB
        }
    } else {
        WallFamily::Cover
    }
}

fn is_near_perimeter(aabb: &Aabb, arena_half: f32) -> bool {
    let half = arena_half;
    near_abs(aabb.x0, half)
        || near_abs(aabb.x1, half)
        || near_abs(aabb.z0, half)
        || near_abs(aabb.z1, half)
}

fn near_abs(v: f32, half: f32) -> bool {
    (v.abs() - half).abs() <= PERIMETER_EDGE_EPS
}

/// Merge every AABB into one [`MeshGeom`] (pure; no Bevy assets).
pub fn build_merged_boxes(boxes: &[Aabb]) -> MeshGeom {
    let mut geom = MeshGeom::default();
    for b in boxes {
        geom.append_box(b);
    }
    geom
}

/// Group walls by family and build one merged mesh per non-empty family.
pub fn build_family_meshes(walls: &[Aabb], arena_half: f32) -> [(WallFamily, MeshGeom); 5] {
    let mut buckets: [Vec<Aabb>; 5] = Default::default();
    for w in walls {
        let fam = classify_wall(w, arena_half);
        let idx = family_index(fam);
        buckets[idx].push(*w);
    }
    [
        (WallFamily::Perimeter, build_merged_boxes(&buckets[0])),
        (WallFamily::Cover, build_merged_boxes(&buckets[1])),
        (WallFamily::BuildingA, build_merged_boxes(&buckets[2])),
        (WallFamily::BuildingB, build_merged_boxes(&buckets[3])),
        (WallFamily::Roof, build_merged_boxes(&buckets[4])),
    ]
}

fn family_index(f: WallFamily) -> usize {
    match f {
        WallFamily::Perimeter => 0,
        WallFamily::Cover => 1,
        WallFamily::BuildingA => 2,
        WallFamily::BuildingB => 3,
        WallFamily::Roof => 4,
    }
}

// ── Edge trim (ShotAnte LineSegments on street furniture only) ─────────────

/// Line-list positions: pairs of endpoints (24 verts = 12 edges per box).
#[derive(Clone, Debug, Default)]
pub struct LineGeom {
    pub positions: Vec<[f32; 3]>,
}

impl LineGeom {
    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    /// 12 edges of a box scaled by `scale` about its centre (ShotAnte 1.003).
    pub fn append_scaled_box_edges(&mut self, aabb: &Aabb, scale: f32) {
        let cx = (aabb.x0 + aabb.x1) * 0.5;
        let cy = (aabb.y0 + aabb.y1) * 0.5;
        let cz = (aabb.z0 + aabb.z1) * 0.5;
        let hx = (aabb.x1 - aabb.x0).abs() * 0.5 * scale;
        let hy = (aabb.y1 - aabb.y0).abs() * 0.5 * scale;
        let hz = (aabb.z1 - aabb.z0).abs() * 0.5 * scale;
        // 8 corners: index bits x=1, y=2, z=4
        let mut c = [[0.0f32; 3]; 8];
        for (i, corner) in c.iter_mut().enumerate() {
            let sx = if i & 1 == 0 { -1.0 } else { 1.0 };
            let sy = if i & 2 == 0 { -1.0 } else { 1.0 };
            let sz = if i & 4 == 0 { -1.0 } else { 1.0 };
            *corner = [cx + sx * hx, cy + sy * hy, cz + sz * hz];
        }
        // 12 edges (pairs of corner indices)
        const EDGES: [[usize; 2]; 12] = [
            [0, 1], [2, 3], [4, 5], [6, 7], // x-parallel
            [0, 2], [1, 3], [4, 6], [5, 7], // y-parallel
            [0, 4], [1, 5], [2, 6], [3, 7], // z-parallel
        ];
        for [a, b] in EDGES {
            self.positions.push(c[a]);
            self.positions.push(c[b]);
        }
    }

    fn into_mesh(self) -> Mesh {
        let n = self.positions.len();
        let normals = vec![[0.0, 1.0, 0.0]; n];
        let uvs = vec![[0.0, 0.0]; n];
        Mesh::new(
            bevy::mesh::PrimitiveTopology::LineList,
            RenderAssetUsages::default(),
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    }
}

/// Which trim colour bucket a cover box belongs to (if any).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrimBucket {
    Accent,
    Dark,
}

/// Street-furniture trim: only Cover-family boxes. Small props → accent;
/// bulky cover → dark. Buildings, roofs, perimeter excluded.
pub fn classify_trim(aabb: &Aabb, arena_half: f32) -> Option<TrimBucket> {
    if classify_wall(aabb, arena_half) != WallFamily::Cover {
        return None;
    }
    let w = (aabb.x1 - aabb.x0).abs();
    let d = (aabb.z1 - aabb.z0).abs();
    if w <= SMALL_PROP_MAX && d <= SMALL_PROP_MAX {
        Some(TrimBucket::Accent)
    } else {
        Some(TrimBucket::Dark)
    }
}

/// Build merged accent + dark edge-trim line lists for a wall set.
pub fn build_edge_trim(walls: &[Aabb], arena_half: f32) -> (LineGeom, LineGeom) {
    let mut accent = LineGeom::default();
    let mut dark = LineGeom::default();
    for w in walls {
        match classify_trim(w, arena_half) {
            Some(TrimBucket::Accent) => accent.append_scaled_box_edges(w, TRIM_SCALE),
            Some(TrimBucket::Dark) => dark.append_scaled_box_edges(w, TRIM_SCALE),
            None => {}
        }
    }
    (accent, dark)
}

/// Billboard quad in local space, facing +Z. Sized `w` × `h`, centred at origin.
pub fn build_billboard_quad(w: f32, h: f32) -> MeshGeom {
    let mut geom = MeshGeom::default();
    let hw = w * 0.5;
    let hh = h * 0.5;
    geom.push_face(
        [
            [-hw, -hh, 0.0],
            [hw, -hh, 0.0],
            [hw, hh, 0.0],
            [-hw, hh, 0.0],
        ],
        [0.0, 0.0, 1.0],
        w,
        h,
    );
    // Force UVs to 0..1 for ad textures (not world-scaled).
    geom.uvs = vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    geom
}

/// Orientation: wall 0=-z faces +z, 1=+z faces -z, 2=-x faces +x, 3=+x faces -x.
pub fn billboard_facing_rotation(wall: u8) -> Quat {
    match wall {
        0 => Quat::IDENTITY,                   // face +Z
        1 => Quat::from_rotation_y(PI),        // face -Z
        2 => Quat::from_rotation_y(PI * 0.5),  // face +X
        3 => Quat::from_rotation_y(-PI * 0.5), // face -X
        _ => Quat::IDENTITY,
    }
}

// ── Glowing window slits (one merged emissive mesh per map) ──────────────────

/// Minimum Building-family height (m) to receive warm window slits.
const WINDOW_MIN_HEIGHT: f32 = 2.5;
const WINDOW_W: f32 = 0.7;
const WINDOW_H: f32 = 0.9;
/// Nudge slits just outside the wall face so they don't z-fight.
const WINDOW_OUTSET: f32 = 0.04;
/// Hard cap so a dense city never floods the single window mesh.
const MAX_WINDOWS_PER_MAP: usize = 512;

/// One emissive window quad in world space (axis-aligned normal).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowSlit {
    pub center: [f32; 3],
    /// Outward unit normal on a cardinal axis.
    pub normal: [f32; 3],
    pub half_w: f32,
    pub half_h: f32,
}

/// Deterministic integer hash (xorshift-ish). Pure; used for window layout.
fn hash_u32(mut n: u32) -> u32 {
    n = n.wrapping_mul(2654435761);
    n ^= n >> 16;
    n = n.wrapping_mul(2246822519);
    n ^= n >> 13;
    n
}

/// Place warm window slits on Building-family boxes taller than
/// [`WINDOW_MIN_HEIGHT`]. Layout is a pure function of wall index + face +
/// cell indices — same walls → same quads. Count is hard-capped at
/// [`MAX_WINDOWS_PER_MAP`].
pub fn place_window_slits(walls: &[Aabb], arena_half: f32) -> Vec<WindowSlit> {
    let mut out = Vec::new();
    for (wi, w) in walls.iter().enumerate() {
        if !classify_wall(w, arena_half).is_building() {
            continue;
        }
        let height = (w.y1 - w.y0).abs();
        if height < WINDOW_MIN_HEIGHT {
            continue;
        }
        let sx = (w.x1 - w.x0).abs();
        let sz = (w.z1 - w.z0).abs();
        let mid_x = (w.x0 + w.x1) * 0.5;
        let mid_z = (w.z0 + w.z1) * 0.5;

        // Four street-facing vertical faces: (+Z, -Z, +X, -X).
        // (normal, face_width, face_x_or_mid, face_z_or_mid, axis_is_x_face)
        let faces: [([f32; 3], f32, f32, f32, bool); 4] = [
            ([0.0, 0.0, 1.0], sx, mid_x, w.z1, false),
            ([0.0, 0.0, -1.0], sx, mid_x, w.z0, false),
            ([1.0, 0.0, 0.0], sz, w.x1, mid_z, true),
            ([-1.0, 0.0, 0.0], sz, w.x0, mid_z, true),
        ];

        for (fi, (normal, face_w, fx, fz, x_face)) in faces.iter().enumerate() {
            if *face_w < WINDOW_W + 0.5 {
                continue;
            }
            let seed = (wi as u32)
                .wrapping_mul(2654435761)
                .wrapping_add((fi as u32).wrapping_mul(1597334677));
            let h = hash_u32(seed);
            // ~1/5 of faces stay blank (irregular street rhythm).
            if h.is_multiple_of(5) {
                continue;
            }
            let max_cols = (((*face_w - 0.4) / (WINDOW_W + 0.55)).floor() as i32).max(1);
            let cols = 1 + (h % max_cols as u32) as i32;
            let usable_h = height - 1.0;
            if usable_h < WINDOW_H {
                continue;
            }
            let max_rows = ((usable_h / (WINDOW_H + 0.7)).floor() as i32).max(1);
            let rows = 1 + ((h >> 8) % max_rows as u32) as i32;

            for r in 0..rows {
                for c in 0..cols {
                    if out.len() >= MAX_WINDOWS_PER_MAP {
                        return out;
                    }
                    let cell = hash_u32(seed.wrapping_add((r as u32) * 31 + (c as u32) * 17));
                    // Sparse irregular pattern inside the grid.
                    if cell.is_multiple_of(7) {
                        continue;
                    }
                    let u = (c as f32 + 0.5) / cols as f32;
                    // Mid-band on the facade (above ground floor sill, below eaves).
                    let v = 0.32 + (r as f32 + 0.5) / rows as f32 * 0.48;
                    let along = (u - 0.5) * (*face_w - WINDOW_W);
                    let y = w.y0 + v * height;
                    let (px, pz) = if *x_face {
                        (fx + normal[0] * WINDOW_OUTSET, fz + along)
                    } else {
                        (fx + along, fz + normal[2] * WINDOW_OUTSET)
                    };
                    out.push(WindowSlit {
                        center: [px, y, pz],
                        normal: *normal,
                        half_w: WINDOW_W * 0.5,
                        half_h: WINDOW_H * 0.5,
                    });
                }
            }
        }
    }
    out
}

/// Merge window slits into one dual-sided-friendly MeshGeom (single draw call).
pub fn build_window_mesh(slits: &[WindowSlit]) -> MeshGeom {
    let mut geom = MeshGeom::default();
    for s in slits {
        let n = Vec3::from_array(s.normal);
        let up = Vec3::Y;
        let right = n.cross(up);
        let right_len = right.length();
        if right_len < 1e-4 {
            continue;
        }
        let right = right / right_len;
        let c = Vec3::from_array(s.center);
        let hw = s.half_w;
        let hh = s.half_h;
        // CCW when outward normal points toward the viewer.
        // `right = n × Y`, so U along −right, V along +Y yields cross = n
        // (U along +right would invert and get culled from the street).
        let corners = [
            (c + right * hw - up * hh).to_array(),
            (c - right * hw - up * hh).to_array(),
            (c - right * hw + up * hh).to_array(),
            (c + right * hw + up * hh).to_array(),
        ];
        geom.push_face(corners, s.normal, s.half_w * 2.0, s.half_h * 2.0);
    }
    geom
}

// ── Procedural textures ────────────────────────────────────────────────────

/// Tiny deterministic hash → [0, 1). Not rand.
fn hash_noise(x: u32, y: u32, salt: u32) -> f32 {
    let mut n = x
        .wrapping_mul(374761393)
        .wrapping_add(y.wrapping_mul(668265263))
        .wrapping_add(salt.wrapping_mul(2246822519));
    n = (n ^ (n >> 13)).wrapping_mul(1274126177);
    n ^= n >> 16;
    (n & 0xffff) as f32 / 65535.0
}

fn rgba_image(width: u32, height: u32, pixels: Vec<u8>) -> Image {
    let mut image = Image::new(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        pixels,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    image.sampler = ImageSampler::nearest();
    image
}

/// Nearest + repeat sampler for world-UV tiled textures. The plain nearest
/// sampler clamps to edge — walls taller/wider than one UV tile smeared their
/// last texel row before this.
fn tiled(mut image: Image) -> Image {
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        ..ImageSamplerDescriptor::nearest()
    });
    image
}

/// ShotAnte `concreteTexture(base, seam, panels, grain)` — 128-style panel
/// canvas: flat base, 900× 2×2 grain specks, 2 px seam grid both axes.
/// Deterministic hash noise (no `rand`). Nearest + repeat applied by caller.
///
/// Returns raw RGBA bytes of length `size * size * 4`.
pub fn concrete_texture_rgba(
    size: u32,
    base: [u8; 3],
    seam: [u8; 3],
    panels: u32,
    grain: f32,
) -> Vec<u8> {
    let n = (size * size) as usize;
    let mut px = vec![0u8; n * 4];
    for i in 0..n {
        let o = i * 4;
        px[o] = base[0];
        px[o + 1] = base[1];
        px[o + 2] = base[2];
        px[o + 3] = 255;
    }
    // 900 random 2×2 px grain specks — hash replaces Math.random.
    for i in 0..900u32 {
        let u = (hash_noise(i, 0, 701) * size as f32).floor() as u32 % size;
        let v = (hash_noise(i, 1, 702) * size as f32).floor() as u32 % size;
        let g = (hash_noise(i, 2, 703) * grain).floor() as i32;
        let gr = g.clamp(0, 255) as u8;
        let gb = (g + 10).clamp(0, 255) as u8;
        const A: f32 = 0.16;
        for dy in 0..2u32 {
            for dx in 0..2u32 {
                let x = (u + dx) % size;
                let y = (v + dy) % size;
                let o = ((y * size + x) * 4) as usize;
                px[o] = ((gr as f32) * A + px[o] as f32 * (1.0 - A)) as u8;
                px[o + 1] = ((gr as f32) * A + px[o + 1] as f32 * (1.0 - A)) as u8;
                px[o + 2] = ((gb as f32) * A + px[o + 2] as f32 * (1.0 - A)) as u8;
            }
        }
    }
    // 2 px seam grid every size/panels.
    let panels = panels.max(1);
    let step = size as f32 / panels as f32;
    for i in 0..=panels {
        let line = (i as f32 * step).round() as i32;
        for t in 0..2i32 {
            let row = (line + t).clamp(0, size as i32 - 1) as u32;
            for x in 0..size {
                let o = ((row * size + x) * 4) as usize;
                px[o] = seam[0];
                px[o + 1] = seam[1];
                px[o + 2] = seam[2];
            }
            let col = (line + t).clamp(0, size as i32 - 1) as u32;
            for y in 0..size {
                let o = ((y * size + col) * 4) as usize;
                px[o] = seam[0];
                px[o + 1] = seam[1];
                px[o + 2] = seam[2];
            }
        }
    }
    px
}

/// Apply an RGB multiplier to every opaque texel (env identity wash).
fn tint_rgba(mut px: Vec<u8>, tint: [f32; 3]) -> Vec<u8> {
    for chunk in px.chunks_exact_mut(4) {
        chunk[0] = ((chunk[0] as f32 * tint[0]).clamp(0.0, 255.0)) as u8;
        chunk[1] = ((chunk[1] as f32 * tint[1]).clamp(0.0, 255.0)) as u8;
        chunk[2] = ((chunk[2] as f32 * tint[2]).clamp(0.0, 255.0)) as u8;
    }
    px
}

/// Sample a concrete tile at integer pattern coords (nearest, wrap).
fn sample_concrete_px(tile: &[u8], tile_size: u32, u: i32, v: i32) -> [u8; 3] {
    let s = tile_size as i32;
    let x = u.rem_euclid(s) as u32;
    let y = v.rem_euclid(s) as u32;
    let o = ((y * tile_size + x) * 4) as usize;
    [tile[o], tile[o + 1], tile[o + 2]]
}

/// Whole-arena ground texture: ShotAnte asphalt concrete tiled at 2 m/tile,
/// baked shadow mask multiplied in. UV 0..1 across the map (not GPU-tiled).
///
/// MountainTown: soft per-band tint (low grassy / mid scree / high frost) —
/// zero runtime cost, baked once with the shadow mask.
fn gen_ground_lit(
    size: u32,
    half: f32,
    occlusion: &[f32],
    occ_res: u32,
    env: EnvKind,
) -> Image {
    // Legacy: base #8d9099, seam #797d88, panels 4, grain 120; one tile / 2 m.
    const TILE_M: f32 = 2.0;
    const PAT: u32 = 128;
    let tile = concrete_texture_rgba(PAT, [0x8d, 0x90, 0x99], [0x79, 0x7d, 0x88], 4, 120.0);
    let mut px = Vec::with_capacity((size * size * 4) as usize);
    let diameter = 2.0 * half;
    let mountain = env == EnvKind::MountainTown;
    for y in 0..size {
        for x in 0..size {
            let u = (x as f32 + 0.5) / size as f32;
            let w = (y as f32 + 0.5) / size as f32;
            let wx = -half + u * diameter;
            let wz = -half + w * diameter;
            // Map world → pattern texel (one 128px tile per 2 m).
            let pu = ((wx.rem_euclid(TILE_M) / TILE_M) * PAT as f32).floor() as i32;
            let pv = ((wz.rem_euclid(TILE_M) / TILE_M) * PAT as f32).floor() as i32;
            let mut rgb = sample_concrete_px(&tile, PAT, pu, pv);
            let light = sample_occlusion(occlusion, occ_res, u, w);
            // Rim darken (keep existing ground anchoring).
            let dx = u * 2.0 - 1.0;
            let dz = w * 2.0 - 1.0;
            let rim = (dx * dx + dz * dz).sqrt().clamp(0.0, 1.0);
            let rim_dark = 1.0 - rim * rim * 0.28;
            let factor = (light * rim_dark).clamp(0.0, 1.0);
            for c in &mut rgb {
                *c = ((*c as f32) * factor).clamp(0.0, 255.0) as u8;
            }
            if mountain {
                let tint = crate::mountain_dressing::mountain_band_tint(wz);
                rgb[0] = ((rgb[0] as f32) * tint[0]).clamp(0.0, 255.0) as u8;
                rgb[1] = ((rgb[1] as f32) * tint[1]).clamp(0.0, 255.0) as u8;
                rgb[2] = ((rgb[2] as f32) * tint[2]).clamp(0.0, 255.0) as u8;
            }
            px.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
    }
    rgba_image(size, size, px)
}

/// Vertical sky gradient texture (zenith → horizon), env-tinted.
/// Sampled on a large inverted dome / far sky ring (zero asset files).
fn gen_sky_gradient(size: u32, zenith: Color, horizon: Color) -> Image {
    let z = zenith.to_srgba();
    let h = horizon.to_srgba();
    let mut px = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        // v=0 at bottom of texture → horizon; v=1 at top → zenith.
        // UV on the dome maps so the upper hemisphere samples zenith.
        let t = y as f32 / (size - 1).max(1) as f32;
        // Bias toward horizon haze near the bottom third.
        let k = t.powf(0.65);
        let r = h.red + (z.red - h.red) * k;
        let g = h.green + (z.green - h.green) * k;
        let b = h.blue + (z.blue - h.blue) * k;
        for _x in 0..size {
            px.extend_from_slice(&[
                (r * 255.0) as u8,
                (g * 255.0) as u8,
                (b * 255.0) as u8,
                255,
            ]);
        }
    }
    rgba_image(size, size, px)
}

/// Horizon haze colour: sky pulled toward a warmer / env-tinted edge.
fn horizon_haze(sky: Color, env: EnvKind) -> Color {
    let s = sky.to_srgba();
    let (wr, wg, wb) = match env {
        EnvKind::DesertTown => (1.05, 0.92, 0.70),
        EnvKind::RomeEur => (1.02, 0.95, 0.82),
        EnvKind::SeaTown => (0.95, 1.0, 1.05),
        EnvKind::MountainTown => (0.98, 1.0, 1.05),
        EnvKind::Urban => (1.0, 0.98, 0.95),
    };
    // Mix sky with a brighter warm rim so the horizon reads as haze.
    Color::srgb(
        (s.red * wr * 0.55 + 0.45 * wr.min(1.0)).clamp(0.0, 1.0),
        (s.green * wg * 0.55 + 0.40 * wg.min(1.0)).clamp(0.0, 1.0),
        (s.blue * wb * 0.55 + 0.38).clamp(0.0, 1.0),
    )
}

/// Building wall texture: ShotAnte near-white panels (`#ffffff` / `#d8d2c4`,
/// panels 2, grain 150). Hue comes from the material base_color; env tint
/// washes non-urban maps so they keep identity.
fn gen_building_tex(env: EnvKind, size: u32) -> Image {
    let tint = env_texture_tint(env);
    let px = concrete_texture_rgba(size, [0xff, 0xff, 0xff], [0xd8, 0xd2, 0xc4], 2, 150.0);
    rgba_image(size, size, tint_rgba(px, tint))
}

/// Perimeter: ShotAnte `#aeb6c4` / `#9aa2b2`, panels 2, grain 140 (+ env tint).
fn gen_perimeter_tex(env: EnvKind, size: u32) -> Image {
    let tint = env_texture_tint(env);
    let px = concrete_texture_rgba(size, [0xae, 0xb6, 0xc4], [0x9a, 0xa2, 0xb2], 2, 140.0);
    rgba_image(size, size, tint_rgba(px, tint))
}

/// Crate / street cover: ShotAnte `#caa36a` / `#a8814c`, panels 2, grain 110.
fn gen_cover(size: u32, tint: [f32; 3]) -> Image {
    let px = concrete_texture_rgba(size, [0xca, 0xa3, 0x6a], [0xa8, 0x81, 0x4c], 2, 110.0);
    rgba_image(size, size, tint_rgba(px, tint))
}

/// Voxel cloud clusters (ShotAnte): ~9 clusters of 2–4 white boxes, y 18–30,
/// spread ±2.6·arena_half. ONE merged mesh. Deterministic from seed.
pub fn build_voxel_clouds(seed: &str, arena_half: f32) -> MeshGeom {
    let mut rng = Mulberry32::from_seed(&format!("{seed}-clouds"));
    let mut geom = MeshGeom::default();
    let span = arena_half * 2.6;
    for _ in 0..9 {
        let cx = (rng.next() as f32 * 2.0 - 1.0) * span;
        let cy = 18.0 + rng.next() as f32 * 12.0;
        let cz = (rng.next() as f32 * 2.0 - 1.0) * span;
        let puffs = 2 + (rng.next() * 3.0).floor() as i32;
        for _ in 0..puffs {
            let w = 3.0 + rng.next() as f32 * 4.0;
            let h = 1.2 + rng.next() as f32;
            let d = 2.0 + rng.next() as f32 * 2.0;
            let px = cx + (rng.next() as f32 * 2.0 - 1.0) * 3.0;
            let py = cy + (rng.next() as f32 - 0.5);
            let pz = cz + (rng.next() as f32 * 2.0 - 1.0) * 2.0;
            let box_ = Aabb {
                x0: px - w * 0.5,
                x1: px + w * 0.5,
                y0: py - h * 0.5,
                y1: py + h * 0.5,
                z0: pz - d * 0.5,
                z1: pz + d * 0.5,
            };
            geom.append_box(&box_);
        }
    }
    geom
}

/// Loud placeholder ad: flat bg, 6 px border, diagonal stripes or checker.
fn gen_ad(slot: u8, width: u32, height: u32) -> Image {
    let bgs: [[u8; 3]; 4] = [
        [255, 40, 80],  // hot pink-red
        [40, 200, 255], // cyan
        [255, 210, 30], // yellow
        [120, 60, 255], // purple
    ];
    let borders: [[u8; 3]; 4] = [[20, 20, 20], [10, 10, 60], [40, 20, 0], [255, 255, 255]];
    let i = (slot % 4) as usize;
    let bg = bgs[i];
    let bd = borders[i];
    let border = 6u32;
    let mut px = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let on_border = x < border || y < border || x >= width - border || y >= height - border;
            let (r, g, b) = if on_border {
                (bd[0], bd[1], bd[2])
            } else {
                // Stripe (even slots) or checker (odd slots) across the middle band.
                let mid = y > height / 4 && y < (height * 3) / 4;
                if mid {
                    let pattern = if slot.is_multiple_of(2) {
                        // diagonal stripes
                        ((x + y) / 12).is_multiple_of(2)
                    } else {
                        // checker
                        ((x / 16) + (y / 16)).is_multiple_of(2)
                    };
                    if pattern {
                        (
                            255u8.saturating_sub(bg[0] / 2),
                            255u8.saturating_sub(bg[1] / 2),
                            255u8.saturating_sub(bg[2] / 2),
                        )
                    } else {
                        (bg[0], bg[1], bg[2])
                    }
                } else {
                    (bg[0], bg[1], bg[2])
                }
            };
            px.extend_from_slice(&[r, g, b, 255]);
        }
    }
    rgba_image(width, height, px)
}

fn accent_color(accent: u32) -> Color {
    Color::srgb_u32(accent & 0x00ff_ffff)
}

fn tinted(base: Color, accent: Color) -> Color {
    base.mix(&accent, ACCENT_MIX)
}

// ── Plugin ─────────────────────────────────────────────────────────────────

/// Parent of voxel cloud mesh; one entity, drifted slowly each frame.
#[derive(Component)]
struct CloudDriftRoot;

/// Slow drift amplitude (m) and period scale (rad/s). Clouds sit at y 18–30
/// in mesh space (ShotAnte); the root stays at origin so fog doesn't erase them.
const CLOUD_DRIFT_AMP: f32 = 12.0;
const CLOUD_DRIFT_SPEED: f32 = 0.04;

/// Identity of the last map we fully built. Survives `is_changed` clearing so
/// a `CurrentMap` inserted after this system runs in the same frame is still
/// built on the next tick — and a missing `MapRoot` forces a recovery rebuild.
#[derive(Resource, Default)]
struct BuiltMapKey {
    seed: String,
    env: Option<EnvKind>,
}

impl Plugin for MapRenderPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BuiltMapKey>()
            // Ensure the set exists even if GamePlugin is not loaded (headless tests).
            .configure_sets(Update, crate::game::GameSessionSet)
            .add_systems(
                Update,
                // After GameSessionSet so a same-frame GameStart → CurrentMap is
                // applied before we decide to rebuild (BuiltMapKey also recovers
                // if ordering ever misses a frame).
                (rebuild_map_system, drift_clouds_system)
                    .chain()
                    .after(crate::game::GameSessionSet),
            );
    }
}

/// Single cheap transform on the cloud root — the only per-frame dressing cost.
fn drift_clouds_system(time: Res<Time>, mut q: Query<&mut Transform, With<CloudDriftRoot>>) {
    let t = time.elapsed_secs();
    for mut xf in &mut q {
        xf.translation.x = (t * CLOUD_DRIFT_SPEED).sin() * CLOUD_DRIFT_AMP;
        xf.translation.z = (t * CLOUD_DRIFT_SPEED * 0.73 + 1.1).cos() * CLOUD_DRIFT_AMP * 0.7;
    }
}

#[allow(clippy::too_many_arguments)] // Bevy system param list
fn rebuild_map_system(
    mut commands: Commands,
    current: Option<Res<CurrentMap>>,
    roots: Query<Entity, With<MapRoot>>,
    placeholders: Query<Entity, With<Placeholder>>,
    mut cameras: Query<(Entity, &mut Camera), With<Camera3d>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut ambient: ResMut<GlobalAmbientLight>,
    mut clear_color: ResMut<ClearColor>,
    mut built: ResMut<BuiltMapKey>,
) {
    let Some(current) = current else {
        return;
    };

    let map = &current.0;
    let has_root = !roots.is_empty();
    let key_matches = built.env == Some(map.env) && built.seed == map.seed;
    // Rebuild when:
    //  - CurrentMap just changed (GameStart / rematch), OR
    //  - we have a map resource but no MapRoot (first-frame order miss / lost), OR
    //  - placeholders still linger under an existing map (partial build).
    let has_placeholders = !placeholders.is_empty();
    if key_matches && has_root && !has_placeholders && !current.is_changed() {
        return;
    }

    // Tear down previous map + any skeleton placeholders.
    for e in roots.iter() {
        commands.entity(e).despawn();
    }
    for e in placeholders.iter() {
        commands.entity(e).despawn();
    }

    let accent = accent_color(map.accent);

    // ── Atmosphere: sky, fog, hemisphere-style fill (per env) ──────────────
    let light = env_lighting(map.env);
    clear_color.0 = light.sky;
    ambient.color = light.ambient;
    ambient.brightness = light.ambient_brightness;
    // Fog scales with the arena: streets stay crisp, env-tinted horizon haze.
    // Force each 3D camera clear to the env sky so the retro target never
    // keeps the menu-dark clear across a rematch (S5).
    let haze = horizon_haze(light.sky, map.env);
    for (cam, mut camera) in cameras.iter_mut() {
        camera.clear_color = ClearColorConfig::Custom(light.sky);
        commands.entity(cam).insert(DistanceFog {
            color: haze,
            falloff: FogFalloff::Linear {
                start: map.arena_half * 1.2,
                end: map.arena_half * 3.6,
            },
            ..default()
        });
    }

    // Procedural sky gradient + sun disc (zero assets).
    let sky_tex = images.add(gen_sky_gradient(32, light.sky, haze));
    let sky_mat = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        base_color_texture: Some(sky_tex),
        unlit: true,
        // Inside of a sphere/dome: flip culling so we see the interior.
        cull_mode: Some(bevy::render::render_resource::Face::Front),
        ..default()
    });
    let sun_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.96, 0.75),
        emissive: LinearRgba::rgb(
            light.sun_color.to_srgba().red * 12.0,
            light.sun_color.to_srgba().green * 10.0,
            light.sun_color.to_srgba().blue * 6.0,
        ),
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        cull_mode: None,
        ..default()
    });

    // ── Baked sun shadows (once, here; nothing shadow-related per frame) ───
    // Resolutions derive from arena_half so the 500 m Rome EUR arena stays
    // ≥2 texels/m / ≤2 m cells without changing the four small-town maps.
    let occ_res = occlusion_res(map.arena_half);
    let ground_res = ground_tex_res(map.arena_half);
    let ground_occlusion =
        bake_ground_occlusion(occ_res, map.arena_half, light.sun_to, &map.walls);

    // Procedural textures (small, nearest-filtered; wall textures tile).
    // Per-env tint + family generators so each town reads at a glance.
    let tex_tint = env_texture_tint(map.env);
    let tex_ground = images.add(gen_ground_lit(
        ground_res,
        map.arena_half,
        &ground_occlusion,
        occ_res,
        map.env,
    ));
    let tex_building = images.add(tiled(gen_building_tex(map.env, 128)));
    let tex_perimeter = images.add(tiled(gen_perimeter_tex(map.env, 128)));
    let tex_cover = images.add(tiled(gen_cover(128, tex_tint)));
    let ad_handles: [Handle<Image>; 4] = [
        images.add(gen_ad(0, 256, 128)),
        images.add(gen_ad(1, 256, 128)),
        images.add(gen_ad(2, 256, 128)),
        images.add(gen_ad(3, 256, 128)),
    ];

    // Near-white textures carry the panel detail; family bases carry the hue.
    // Urban: two map-seeded pastels for BuildingA/B; roof is flat slate.
    let mut bases = family_base_colors(map.env);
    if map.env == EnvKind::Urban {
        let (hue_a, hue_b) = pick_urban_pastels(&map.seed);
        bases[2] = hue_a;
        bases[3] = hue_b;
        bases[4] = color_u32(ROOF_SLATE);
    }
    let family_mats: [Handle<StandardMaterial>; 5] = [
        // Perimeter: colour in texture (Urban base WHITE).
        materials.add(wall_material(tinted(bases[0], accent), Some(tex_perimeter), 0.95)),
        // Cover crates: colour in texture.
        materials.add(wall_material(tinted(bases[1], accent), Some(tex_cover), 0.95)),
        // Building A / B: pastel × near-white seam texture.
        materials.add(wall_material(
            tinted(bases[2], accent),
            Some(tex_building.clone()),
            0.95,
        )),
        materials.add(wall_material(
            tinted(bases[3], accent),
            Some(tex_building),
            0.95,
        )),
        // Roof: flat slate (no texture — MeshLambert flat colour).
        materials.add(wall_material(tinted(bases[4], accent), None, 0.98)),
    ];

    // Tone lives in the texture (shadow mask baked in): keep base white.
    let ground_mat = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        base_color_texture: Some(tex_ground),
        perceptual_roughness: 0.98,
        metallic: 0.0,
        reflectance: 0.0,
        ..default()
    });

    let frame_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.08, 0.08, 0.09),
        perceptual_roughness: 0.95,
        metallic: 0.0,
        reflectance: 0.0,
        unlit: false,
        ..default()
    });

    let ad_mats: [Handle<StandardMaterial>; 4] = [
        materials.add(ad_material(ad_handles[0].clone())),
        materials.add(ad_material(ad_handles[1].clone())),
        materials.add(ad_material(ad_handles[2].clone())),
        materials.add(ad_material(ad_handles[3].clone())),
    ];

    // Warm window glow: one emissive material + one merged mesh for the map.
    let window_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.88, 0.55),
        emissive: LinearRgba::rgb(5.5, 3.6, 1.4),
        unlit: true,
        alpha_mode: AlphaMode::Opaque,
        // Visible from street; thin quads, no backface needed.
        cull_mode: Some(bevy::render::render_resource::Face::Back),
        ..default()
    });

    // Voxel clouds: unlit white, fog disabled so they stay visible past haze.
    let cloud_mat = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        unlit: true,
        fog_enabled: false,
        cull_mode: None,
        reflectance: 0.0,
        ..default()
    });

    // Edge trim: accent (map) for small props, dark brown for bulky cover.
    let accent_trim_mat = materials.add(StandardMaterial {
        base_color: accent,
        unlit: true,
        fog_enabled: false,
        reflectance: 0.0,
        ..default()
    });
    let dark_trim_mat = materials.add(StandardMaterial {
        base_color: color_u32(DARK_TRIM),
        unlit: true,
        fog_enabled: false,
        reflectance: 0.0,
        ..default()
    });

    let family_geoms = build_family_meshes(&map.walls, map.arena_half);
    let (accent_trim, dark_trim) = build_edge_trim(&map.walls, map.arena_half);
    let cloud_geom = build_voxel_clouds(&map.seed, map.arena_half);
    let window_slits = place_window_slits(&map.walls, map.arena_half);
    let half = map.arena_half;

    commands
        .spawn((
            MapRoot,
            Transform::IDENTITY,
            Visibility::default(),
            Name::new("MapRoot"),
        ))
        .with_children(|root| {
            // Ground: full extent 2*arena_half.
            let ground_mesh = meshes.add(Mesh::from(Plane3d::new(Vec3::Y, Vec2::splat(half))));
            root.spawn((
                Mesh3d(ground_mesh),
                MeshMaterial3d(ground_mat),
                Transform::IDENTITY,
                Name::new("Ground"),
            ));

            // Merged wall families (one entity each, skip empty) with baked
            // per-face sun light in the vertex colors.
            for (fam, mut geom) in family_geoms {
                if geom.is_empty() {
                    continue;
                }
                bake_face_colors(&mut geom, light.sun_to, &map.walls);
                let mat = family_mats[family_index(fam)].clone();
                let mesh = meshes.add(geom.into_mesh());
                root.spawn((
                    Mesh3d(mesh),
                    MeshMaterial3d(mat),
                    Transform::IDENTITY,
                    Name::new(format!("Walls/{fam:?}")),
                ));
            }

            // Edge trim on street furniture only — two LineList draw calls.
            if !accent_trim.is_empty() {
                let mesh = meshes.add(accent_trim.into_mesh());
                root.spawn((
                    Mesh3d(mesh),
                    MeshMaterial3d(accent_trim_mat),
                    Transform::IDENTITY,
                    Name::new("Trim/Accent"),
                ));
            }
            if !dark_trim.is_empty() {
                let mesh = meshes.add(dark_trim.into_mesh());
                root.spawn((
                    Mesh3d(mesh),
                    MeshMaterial3d(dark_trim_mat),
                    Transform::IDENTITY,
                    Name::new("Trim/Dark"),
                ));
            }

            // Warm window slits — single mesh, single emissive material.
            if !window_slits.is_empty() {
                let wgeom = build_window_mesh(&window_slits);
                if !wgeom.is_empty() {
                    let mesh = meshes.add(wgeom.into_mesh());
                    root.spawn((
                        Mesh3d(mesh),
                        MeshMaterial3d(window_mat),
                        Transform::IDENTITY,
                        Name::new("Windows"),
                    ));
                }
            }

            // Billboards + thin dark frames (ads are unlit + mild emissive lift).
            for (i, bb) in map.billboards.iter().enumerate() {
                let slot = (bb.ad_slot % 4) as usize;
                let quad = build_billboard_quad(bb.w, bb.h);
                let mesh = meshes.add(quad.into_mesh());
                let rot = billboard_facing_rotation(bb.wall);
                root.spawn((
                    Mesh3d(mesh),
                    MeshMaterial3d(ad_mats[slot].clone()),
                    Transform::from_translation(Vec3::new(bb.x, bb.y, bb.z)).with_rotation(rot),
                    Name::new(format!("Billboard/{i}")),
                ));

                // Frame: slightly larger thin box behind/around the panel.
                let fw = bb.w + BILLBOARD_FRAME_T * 2.0;
                let fh = bb.h + BILLBOARD_FRAME_T * 2.0;
                let fd = BILLBOARD_FRAME_DEPTH;
                let frame_aabb = Aabb {
                    x0: -fw * 0.5,
                    x1: fw * 0.5,
                    y0: -fh * 0.5,
                    y1: fh * 0.5,
                    z0: -fd,
                    z1: 0.0,
                };
                let mut frame_geom = MeshGeom::default();
                frame_geom.append_box(&frame_aabb);
                let frame_mesh = meshes.add(frame_geom.into_mesh());
                // Nudge frame slightly behind the ad face.
                let back = rot * Vec3::new(0.0, 0.0, -0.01);
                root.spawn((
                    Mesh3d(frame_mesh),
                    MeshMaterial3d(frame_mat.clone()),
                    Transform::from_translation(Vec3::new(bb.x, bb.y, bb.z) + back)
                        .with_rotation(rot),
                    Name::new(format!("BillboardFrame/{i}")),
                ));
            }

            // Voxel cloud clusters (ShotAnte): one merged white mesh under a
            // slow-drift root. Unlit + fog off so they read past distance haze.
            if !cloud_geom.is_empty() {
                let cloud_mesh = meshes.add(cloud_geom.into_mesh());
                root.spawn((
                    CloudDriftRoot,
                    Mesh3d(cloud_mesh),
                    MeshMaterial3d(cloud_mat),
                    Transform::IDENTITY,
                    Visibility::default(),
                    Name::new("Clouds"),
                ));
            }

            // Map-owned sun (the skeleton backdrop light is a Placeholder and
            // is gone by now). Real-time shadow maps stay OFF — shadows were
            // baked above, once, at map build.
            root.spawn((
                DirectionalLight {
                    color: light.sun_color,
                    illuminance: light.sun_illuminance,
                    shadow_maps_enabled: false,
                    ..default()
                },
                Transform::IDENTITY.looking_to(-light.sun_to, Vec3::Y),
                Name::new("MapSun"),
            ));

            // Far sky dome: inverted UV sphere with vertical gradient texture.
            // Radius past fog end so it reads as atmosphere, not geometry.
            let sky_r = half * 5.5;
            let sky_mesh = meshes.add(Sphere::new(sky_r).mesh().uv(24, 12));
            root.spawn((
                Mesh3d(sky_mesh),
                MeshMaterial3d(sky_mat),
                Transform::from_xyz(0.0, 0.0, 0.0),
                Name::new("SkyDome"),
            ));

            // Sun disc billboard: bright emissive quad along sun_to.
            let sun_dist = half * 4.2;
            let sun_pos = light.sun_to * sun_dist;
            let sun_size = half * 0.18;
            let sun_mesh = meshes.add(Mesh::from(Plane3d::new(
                -light.sun_to,
                Vec2::splat(sun_size),
            )));
            root.spawn((
                Mesh3d(sun_mesh),
                MeshMaterial3d(sun_mat),
                Transform::from_translation(sun_pos)
                    .looking_to(-light.sun_to, Vec3::Y),
                Name::new("SunDisc"),
            ));

            // ── M28 mountain identity (env-gated; zero sim impact) ─────────
            if map.env == EnvKind::MountainTown {
                spawn_mountain_dressing(root, map, &mut meshes, &mut materials, &mut images);
            }
        });

    // Mark this seed/env as built so we don't re-bake every frame (which
    // despawns the extractable MapRoot and leaves a stale retro buffer).
    built.seed = map.seed.clone();
    built.env = Some(map.env);
}

fn wall_material(
    base_color: Color,
    texture: Option<Handle<Image>>,
    roughness: f32,
) -> StandardMaterial {
    // Lambert-flat: zero specular sheen so faces read like MeshLambertMaterial.
    StandardMaterial {
        base_color,
        base_color_texture: texture,
        perceptual_roughness: roughness,
        metallic: 0.0,
        reflectance: 0.0,
        ..default()
    }
}

/// M28: mountain-only props under MapRoot — slope skirts, pines, rocks, ridge.
/// A handful of merged meshes / materials (PS2 budget).
fn spawn_mountain_dressing(
    root: &mut ChildSpawnerCommands,
    map: &GameMap,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
) {
    use crate::mountain_dressing::{
        build_mountain_dressing, gen_bark_tex, gen_earth_scree_tex, gen_needle_tex, gen_stone_tex,
    };

    let dress = build_mountain_dressing(map);

    let earth_tex = images.add(tiled(gen_earth_scree_tex(64)));
    let needle_tex = images.add(tiled(gen_needle_tex(64)));
    let bark_tex = images.add(tiled(gen_bark_tex(64)));
    let stone_tex = images.add(tiled(gen_stone_tex(64)));

    let earth_mat = materials.add(wall_material(Color::WHITE, Some(earth_tex), 0.97));
    let needle_mat = materials.add(wall_material(Color::WHITE, Some(needle_tex), 0.95));
    let bark_mat = materials.add(wall_material(Color::WHITE, Some(bark_tex), 0.96));
    let stone_mat = materials.add(wall_material(Color::WHITE, Some(stone_tex), 0.97));
    // Ridge: unlit, vertex colors carry rock/snow two-tone; fog on for distance.
    let ridge_mat = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        unlit: true,
        fog_enabled: true,
        perceptual_roughness: 1.0,
        metallic: 0.0,
        reflectance: 0.0,
        cull_mode: Some(bevy::render::render_resource::Face::Back),
        ..default()
    });

    if !dress.slopes.is_empty() {
        let mesh = meshes.add(dress.slopes.into_mesh());
        root.spawn((
            Mesh3d(mesh),
            MeshMaterial3d(earth_mat),
            Transform::IDENTITY,
            Name::new("Mountain/Slopes"),
        ));
    }
    if !dress.pine_trunks.is_empty() {
        let mesh = meshes.add(dress.pine_trunks.into_mesh());
        root.spawn((
            Mesh3d(mesh),
            MeshMaterial3d(bark_mat),
            Transform::IDENTITY,
            Name::new("Mountain/PineTrunks"),
        ));
    }
    if !dress.pine_needles.is_empty() {
        let mesh = meshes.add(dress.pine_needles.into_mesh());
        root.spawn((
            Mesh3d(mesh),
            MeshMaterial3d(needle_mat.clone()),
            Transform::IDENTITY,
            Name::new("Mountain/PineNeedles"),
        ));
    }
    if !dress.pine_far.is_empty() {
        let mesh = meshes.add(dress.pine_far.into_mesh());
        root.spawn((
            Mesh3d(mesh),
            MeshMaterial3d(needle_mat),
            Transform::IDENTITY,
            Name::new("Mountain/PineFar"),
        ));
    }
    if !dress.rocks.is_empty() {
        let mesh = meshes.add(dress.rocks.into_mesh());
        root.spawn((
            Mesh3d(mesh),
            MeshMaterial3d(stone_mat),
            Transform::IDENTITY,
            Name::new("Mountain/Rocks"),
        ));
    }
    if !dress.ridge.is_empty() {
        let mesh = meshes.add(dress.ridge.into_mesh());
        root.spawn((
            Mesh3d(mesh),
            MeshMaterial3d(ridge_mat),
            Transform::IDENTITY,
            Name::new("Mountain/Ridge"),
        ));
    }
}

/// Unlit ad panel with a mild emissive lift so signage reads as backlit.
fn ad_material(texture: Handle<Image>) -> StandardMaterial {
    StandardMaterial {
        base_color: Color::WHITE,
        base_color_texture: Some(texture),
        unlit: true,
        // Subtle warm lift — bright enough to read as backlit, not a neon bloom.
        emissive: LinearRgba::rgb(0.55, 0.48, 0.40),
        // Single-sided, faces into the arena per orientation.
        cull_mode: Some(bevy::render::render_resource::Face::Back),
        alpha_mode: AlphaMode::Opaque,
        ..default()
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use zz_core::map::generate_map;
    use zz_core::types::EnvKind;

    fn box_at(x0: f32, y0: f32, z0: f32, x1: f32, y1: f32, z1: f32) -> Aabb {
        Aabb {
            x0,
            x1,
            y0,
            y1,
            z0,
            z1,
        }
    }

    /// Every triangle's geometric normal (from index winding) must agree with
    /// its attribute normals. Back-face culling depends on this.
    fn assert_winding_matches_attribute_normals(geom: &MeshGeom, label: &str) {
        assert!(
            !geom.indices.is_empty(),
            "{label}: expected non-empty mesh"
        );
        assert_eq!(
            geom.indices.len() % 3,
            0,
            "{label}: index count not multiple of 3"
        );
        for (ti, tri) in geom.indices.chunks_exact(3).enumerate() {
            let i0 = tri[0] as usize;
            let i1 = tri[1] as usize;
            let i2 = tri[2] as usize;
            let p0 = Vec3::from_array(geom.positions[i0]);
            let p1 = Vec3::from_array(geom.positions[i1]);
            let p2 = Vec3::from_array(geom.positions[i2]);
            let geometric = (p1 - p0).cross(p2 - p0);
            assert!(
                geometric.length_squared() > 1e-12,
                "{label} tri {ti}: degenerate triangle"
            );
            for &vi in &[i0, i1, i2] {
                let attr = Vec3::from_array(geom.normals[vi]);
                let d = geometric.dot(attr);
                assert!(
                    d > 0.0,
                    "{label} tri {ti} vert {vi}: geometric {geometric:?} · attr {attr:?} = {d} (must be > 0)"
                );
            }
        }
    }

    #[test]
    fn append_box_winding_matches_outward_normals() {
        let mut geom = MeshGeom::default();
        geom.append_box(&box_at(0.0, 0.0, 0.0, 1.0, 1.0, 1.0));
        assert_eq!(geom.positions.len(), 24);
        assert_eq!(geom.indices.len(), 36);
        assert_winding_matches_attribute_normals(&geom, "unit box");
        // Non-unit extents (anisotropic faces) too.
        let mut tall = MeshGeom::default();
        tall.append_box(&box_at(-1.0, 0.0, -2.0, 3.0, 4.0, 1.0));
        assert_winding_matches_attribute_normals(&tall, "aniso box");
    }

    #[test]
    fn billboard_quad_winding_matches_outward_normals() {
        let q = build_billboard_quad(2.0, 1.0);
        assert_eq!(q.positions.len(), 4);
        assert_winding_matches_attribute_normals(&q, "billboard");
    }

    #[test]
    fn window_mesh_winding_matches_outward_normals() {
        // One slit on each cardinal so every normal axis is exercised.
        let slits = [
            WindowSlit {
                center: [0.0, 1.0, 2.0],
                normal: [0.0, 0.0, 1.0],
                half_w: 0.35,
                half_h: 0.45,
            },
            WindowSlit {
                center: [0.0, 1.0, -2.0],
                normal: [0.0, 0.0, -1.0],
                half_w: 0.35,
                half_h: 0.45,
            },
            WindowSlit {
                center: [2.0, 1.0, 0.0],
                normal: [1.0, 0.0, 0.0],
                half_w: 0.35,
                half_h: 0.45,
            },
            WindowSlit {
                center: [-2.0, 1.0, 0.0],
                normal: [-1.0, 0.0, 0.0],
                half_w: 0.35,
                half_h: 0.45,
            },
        ];
        let geom = build_window_mesh(&slits);
        assert_eq!(geom.positions.len(), slits.len() * 4);
        assert_winding_matches_attribute_normals(&geom, "window mesh");
    }

    #[test]
    fn bake_face_colors_maps_to_correct_face_normals() {
        // Face order is still +Y,−Y,+X,−X,+Z,−Z; baking keys off attribute
        // normals + face centres, so a sun along +Y must light the top face
        // brighter than the bottom.
        let boxes = [box_at(0.0, 0.0, 0.0, 2.0, 2.0, 2.0)];
        let mut geom = build_merged_boxes(&boxes);
        let sun = Vec3::Y;
        bake_face_colors(&mut geom, sun, &boxes);
        // 4 verts per face; face 0 = +Y, face 1 = −Y.
        let top = geom.colors[0][0];
        let bottom = geom.colors[4][0];
        assert!(
            top > bottom,
            "top face ({top}) should be brighter than bottom ({bottom}) under +Y sun"
        );
        // +Z (face 4) and −Z (face 5) are both edge-on to +Y sun → equal ambient-ish.
        let pz = geom.colors[16][0];
        let nz = geom.colors[20][0];
        assert!((pz - nz).abs() < 1e-5, "+Z/{pz} vs −Z/{nz}");
    }

    #[test]
    fn family_classifier_representative_boxes() {
        let half = 30.0;
        // Perimeter wall on -z edge, tall.
        let peri = box_at(-5.0, 0.0, -30.0, 5.0, 6.0, -29.4);
        assert_eq!(classify_wall(&peri, half), WallFamily::Perimeter);

        // Low street cover / crate.
        let cover = box_at(1.0, 0.0, 1.0, 2.0, 1.2, 2.0);
        assert_eq!(classify_wall(&cover, half), WallFamily::Cover);

        // Building wall (mid height, interior) — side by (x0+z0).
        let build_a = box_at(1.0, 0.0, 1.0, 1.4, 3.2, 5.0); // x0+z0 > 0
        assert_eq!(classify_wall(&build_a, half), WallFamily::BuildingA);
        let build_b = box_at(-4.0, 0.0, -2.0, -3.6, 3.2, 2.0); // x0+z0 < 0
        assert_eq!(classify_wall(&build_b, half), WallFamily::BuildingB);

        // Doorway sill (y0=0, y1=1.3) must be building, not crate wood.
        let sill = box_at(2.0, 0.0, 2.0, 3.8, 1.3, 2.4);
        assert!(classify_wall(&sill, half).is_building());

        // Roof slab (bottom at/above 2.5 m).
        let roof = box_at(0.0, 3.2, 0.0, 4.0, 3.55, 4.0);
        assert_eq!(classify_wall(&roof, half), WallFamily::Roof);
        let roof_eq = box_at(0.0, 2.5, 0.0, 4.0, 2.85, 4.0);
        assert_eq!(classify_wall(&roof_eq, half), WallFamily::Roof);
    }

    #[test]
    fn concrete_texture_seam_grid_and_deterministic() {
        const SIZE: u32 = 128;
        const PANELS: u32 = 4;
        let a = concrete_texture_rgba(SIZE, [0x8d, 0x90, 0x99], [0x79, 0x7d, 0x88], PANELS, 120.0);
        let b = concrete_texture_rgba(SIZE, [0x8d, 0x90, 0x99], [0x79, 0x7d, 0x88], PANELS, 120.0);
        assert_eq!(a, b, "texture bake must be deterministic");
        assert_eq!(a.len(), (SIZE * SIZE * 4) as usize);

        let seam = [0x79u8, 0x7d, 0x88];
        let step = SIZE as f32 / PANELS as f32;
        // Seams at i*step for i in 0..=panels — sample a few rows/cols.
        for i in 0..=PANELS {
            let line = (i as f32 * step).round() as u32;
            let line = line.min(SIZE - 1);
            // Horizontal seam row
            let o = ((line * SIZE + SIZE / 2) * 4) as usize;
            assert_eq!(&a[o..o + 3], &seam[..], "H seam at row {line}");
            // Vertical seam col
            let o2 = (((SIZE / 2) * SIZE + line) * 4) as usize;
            assert_eq!(&a[o2..o2 + 3], &seam[..], "V seam at col {line}");
        }
        // Interior of a panel (away from seams) is not pure seam colour.
        let mid = (step * 0.5).round() as u32;
        let o = ((mid * SIZE + mid) * 4) as usize;
        assert_ne!(&a[o..o + 3], &seam[..], "panel interior should not be seam");
    }

    #[test]
    fn edge_trim_24_verts_per_box_and_buckets() {
        let half = 30.0;
        // Small prop → accent; bulky cover → dark; building/roof/perim excluded.
        let small = box_at(0.0, 0.0, 0.0, 1.0, 1.0, 1.0);
        let bulky = box_at(5.0, 0.0, 5.0, 8.0, 1.5, 8.0);
        let building = box_at(10.0, 0.0, 10.0, 10.4, 3.2, 14.0);
        let roof = box_at(10.0, 3.2, 10.0, 14.0, 3.55, 14.0);
        let peri = box_at(-5.0, 0.0, -30.0, 5.0, 6.0, -29.4);

        assert_eq!(classify_trim(&small, half), Some(TrimBucket::Accent));
        assert_eq!(classify_trim(&bulky, half), Some(TrimBucket::Dark));
        assert_eq!(classify_trim(&building, half), None);
        assert_eq!(classify_trim(&roof, half), None);
        assert_eq!(classify_trim(&peri, half), None);

        let mut lg = LineGeom::default();
        lg.append_scaled_box_edges(&small, TRIM_SCALE);
        assert_eq!(lg.positions.len(), 24, "12 edges × 2 verts");

        let (accent, dark) = build_edge_trim(&[small, bulky, building, roof, peri], half);
        assert_eq!(accent.positions.len(), 24);
        assert_eq!(dark.positions.len(), 24);
    }

    #[test]
    fn urban_pastel_hues_deterministic_and_distinct() {
        let (a1, b1) = pick_urban_pastels("preview");
        let (a2, b2) = pick_urban_pastels("preview");
        assert_eq!(a1.to_srgba().red, a2.to_srgba().red);
        assert_eq!(b1.to_srgba().red, b2.to_srgba().red);
        // Hues must differ (force-differ when rng collides).
        let ca = a1.to_srgba();
        let cb = b1.to_srgba();
        let d = (ca.red - cb.red).abs() + (ca.green - cb.green).abs() + (ca.blue - cb.blue).abs();
        assert!(d > 1e-4, "pastel pair must differ");
        // Different seeds can (and usually do) differ.
        let (a3, _) = pick_urban_pastels("other-seed-xyz");
        // Not a hard assert they differ (possible collision), just that both run.
        let _ = a3;
    }

    #[test]
    fn voxel_clouds_deterministic_and_nonempty() {
        let a = build_voxel_clouds("preview", 30.0);
        let b = build_voxel_clouds("preview", 30.0);
        assert_eq!(a.positions.len(), b.positions.len());
        assert!(!a.is_empty());
        // 9 clusters × 2–4 puffs × 24 verts → at least 9*2*24.
        assert!(a.positions.len() >= 9 * 2 * 24);
        assert_eq!(a.positions.len() % 24, 0);
    }

    #[test]
    fn merged_mesh_vertex_and_index_counts() {
        let boxes = [
            box_at(0.0, 0.0, 0.0, 1.0, 2.0, 1.0),
            box_at(2.0, 0.0, 2.0, 3.0, 1.0, 4.0),
            box_at(-1.0, 0.5, -1.0, 0.0, 1.5, 0.0),
        ];
        let n = boxes.len();
        let geom = build_merged_boxes(&boxes);
        // 6 faces × 4 verts × N, 6 faces × 6 indices × N
        assert_eq!(geom.positions.len(), 6 * 4 * n);
        assert_eq!(geom.indices.len(), 6 * 6 * n);
    }

    #[test]
    fn vertices_lie_inside_source_boxes() {
        let boxes = [
            box_at(-2.0, 0.0, -3.0, 1.0, 4.0, 2.0),
            box_at(5.0, 1.0, 5.0, 6.5, 1.8, 7.0),
        ];
        let geom = build_merged_boxes(&boxes);
        let eps = 0.001;
        for (vi, p) in geom.positions.iter().enumerate() {
            let face = vi / 4; // 4 verts per face
            let box_i = face / 6;
            let b = &boxes[box_i];
            assert!(
                p[0] >= b.x0 - eps
                    && p[0] <= b.x1 + eps
                    && p[1] >= b.y0 - eps
                    && p[1] <= b.y1 + eps
                    && p[2] >= b.z0 - eps
                    && p[2] <= b.z1 + eps,
                "vertex {vi} {p:?} outside box {box_i} {b:?}"
            );
        }
    }

    #[test]
    fn uv_scale_tracks_face_world_size() {
        // 2×3×4 box → face sizes determine UV extents at 0.5/m
        let b = box_at(0.0, 0.0, 0.0, 2.0, 3.0, 4.0);
        let geom = build_merged_boxes(&[b]);
        // +Y face is verts 0..4, U along X (2m → 1.0), V along Z (4m → 2.0)
        assert!((geom.uvs[1][0] - 2.0 * UV_PER_M).abs() < 1e-5);
        assert!((geom.uvs[2][1] - 4.0 * UV_PER_M).abs() < 1e-5);
        // +X face starts at vert 8, U along Z (4m → 2.0), V along Y (3m → 1.5)
        let base = 8;
        assert!((geom.uvs[base + 1][0] - 4.0 * UV_PER_M).abs() < 1e-5);
        assert!((geom.uvs[base + 2][1] - 3.0 * UV_PER_M).abs() < 1e-5);
    }

    #[test]
    fn preview_seed_builds_without_panic() {
        let map = generate_map(EnvKind::Urban, "preview");
        assert!(map.arena_half > 0.0);
        assert!(!map.walls.is_empty());

        let families = build_family_meshes(&map.walls, map.arena_half);
        let mut total_verts = 0usize;
        let mut non_empty = 0usize;
        for (fam, geom) in &families {
            if !geom.is_empty() {
                non_empty += 1;
                total_verts += geom.positions.len();
                // Sanity: counts match 24 verts / 36 indices per box.
                assert_eq!(geom.positions.len() % 24, 0, "{fam:?}");
                assert_eq!(geom.indices.len() % 36, 0, "{fam:?}");
            }
        }
        // Ground + up to 4 wall families + billboards (each ad + frame) + light
        // are draw-ish entities; material families for walls are `non_empty`.
        assert!(
            non_empty >= 1,
            "expected at least one wall family for preview"
        );
        assert!(total_verts > 0);

        // Billboard quads also pure-build cleanly.
        for bb in &map.billboards {
            let q = build_billboard_quad(bb.w, bb.h);
            assert_eq!(q.positions.len(), 4);
            assert_eq!(q.indices.len(), 6);
            let _ = billboard_facing_rotation(bb.wall);
        }

        // Procedural textures must not panic.
        let occ = bake_ground_occlusion(8, map.arena_half, Vec3::new(0.4, 0.8, 0.3), &map.walls);
        let _ = gen_ground_lit(16, map.arena_half, &occ, 8, EnvKind::Urban);
        let tint = env_texture_tint(EnvKind::Urban);
        let _ = gen_cover(16, tint);
        let _ = gen_building_tex(EnvKind::Urban, 16);
        let _ = gen_building_tex(EnvKind::MountainTown, 16);
        let _ = gen_building_tex(EnvKind::DesertTown, 16);
        let _ = gen_building_tex(EnvKind::SeaTown, 16);
        let _ = gen_perimeter_tex(EnvKind::Urban, 16);
        let _ = build_voxel_clouds(&map.seed, map.arena_half);
        let (at, dt) = build_edge_trim(&map.walls, map.arena_half);
        assert!(at.positions.len().is_multiple_of(24) || at.is_empty());
        assert!(dt.positions.len().is_multiple_of(24) || dt.is_empty());
        for s in 0..4u8 {
            let _ = gen_ad(s, 32, 16);
        }
        // Window slits: pure placement + mesh, deterministic.
        let slits = place_window_slits(&map.walls, map.arena_half);
        assert!(slits.len() <= MAX_WINDOWS_PER_MAP);
        let _ = build_window_mesh(&slits);

        eprintln!(
            "preview map: walls={}, billboards={}, non_empty_wall_families={} (draw families), wall_verts={}, accent={:#08x}",
            map.walls.len(),
            map.billboards.len(),
            non_empty,
            total_verts,
            map.accent
        );
    }

    #[test]
    fn env_lighting_is_total_and_distinct() {
        let envs = [
            EnvKind::Urban,
            EnvKind::MountainTown,
            EnvKind::DesertTown,
            EnvKind::SeaTown,
            EnvKind::RomeEur,
        ];
        let all: Vec<EnvLighting> = envs.iter().map(|e| env_lighting(*e)).collect();
        for (i, a) in all.iter().enumerate() {
            assert!(a.ambient_brightness > 0.0);
            assert!(a.sun_illuminance > 0.0);
            assert!((a.sun_to.length() - 1.0).abs() < 1e-5, "sun_to normalized");
            assert!(a.sun_to.y > 0.0, "sun above the horizon");
            for (j, b) in all.iter().enumerate().skip(i + 1) {
                let ca = a.sky.to_srgba();
                let cb = b.sky.to_srgba();
                let d = (ca.red - cb.red).abs() + (ca.green - cb.green).abs()
                    + (ca.blue - cb.blue).abs();
                assert!(d > 0.02, "skies of env {i} and {j} indistinguishable");
            }
        }
        // Rome: golden sun from the southwest, warm ambient, ~12k lux.
        let rome = env_lighting(EnvKind::RomeEur);
        assert!((rome.sun_illuminance - 12_000.0).abs() < 1.0);
        assert!(rome.sun_to.x < 0.0, "southwest → −x (west)");
        assert!(rome.sun_to.z > 0.0, "southwest → +z (south on this map)");
        assert!((rome.ambient_brightness - 1_000.0).abs() < 1.0);
    }

    #[test]
    fn bake_resolutions_preserve_small_towns_and_scale_rome() {
        // Historical small-map values must not regress (arena_half ≈ 30).
        assert_eq!(ground_tex_res(30.0), 1024);
        assert_eq!(occlusion_res(30.0), 256);
        assert_eq!(ground_tex_res(40.0), 1024);
        assert_eq!(occlusion_res(40.0), 256);

        // Rome EUR: 500 m arena (arena_half = 250).
        let g = ground_tex_res(250.0);
        let o = occlusion_res(250.0);
        assert!(g <= 2048, "ground tex hard cap");
        assert!(o <= 2048, "occlusion hard cap");
        // ≥ 2 texels/m across the diameter.
        let diameter = 500.0;
        assert!(
            g as f32 / diameter >= 2.0 - 1e-3,
            "ground {g} too coarse for {diameter} m"
        );
        // Occlusion cell ≤ 2 m.
        assert!(
            diameter / o as f32 <= 2.0 + 1e-3,
            "occlusion cell {} m too large",
            diameter / o as f32
        );
    }

    #[test]
    fn rome_eur_bake_time_and_resolution() {
        use std::time::Instant;

        let map = generate_map(EnvKind::RomeEur, "preview");
        assert_eq!(map.arena_half, 250.0);
        assert_eq!(map.env, EnvKind::RomeEur);
        assert!(!map.walls.is_empty());

        let occ_r = occlusion_res(map.arena_half);
        let ground_r = ground_tex_res(map.arena_half);
        let light = env_lighting(EnvKind::RomeEur);

        let t0 = Instant::now();
        let occ = bake_ground_occlusion(occ_r, map.arena_half, light.sun_to, &map.walls);
        let occ_ms = t0.elapsed().as_secs_f64() * 1000.0;

        let t1 = Instant::now();
        let _tex = gen_ground_lit(ground_r, map.arena_half, &occ, occ_r, EnvKind::RomeEur);
        let ground_ms = t1.elapsed().as_secs_f64() * 1000.0;

        let t2 = Instant::now();
        let families = build_family_meshes(&map.walls, map.arena_half);
        let mut face_verts = 0usize;
        for (_fam, mut geom) in families {
            if geom.is_empty() {
                continue;
            }
            bake_face_colors(&mut geom, light.sun_to, &map.walls);
            face_verts += geom.positions.len();
        }
        let face_ms = t2.elapsed().as_secs_f64() * 1000.0;
        let total_ms = occ_ms + ground_ms + face_ms;

        eprintln!(
            "Rome EUR bake: walls={}, arena_half={}, occ_res={}, ground_res={}, \
             occ={occ_ms:.1}ms ground={ground_ms:.1}ms faces={face_ms:.1}ms \
             total={total_ms:.1}ms face_verts={face_verts}",
            map.walls.len(),
            map.arena_half,
            occ_r,
            ground_r,
        );

        // Debug budget < 8 s; release target is tighter but this suite runs
        // without --release by default.
        assert!(
            total_ms < 8_000.0,
            "Rome bake took {total_ms:.0} ms (debug budget 8000 ms)"
        );
        assert_eq!(occ.len(), (occ_r * occ_r) as usize);
    }

    #[test]
    fn rome_family_bases_are_travertine_warm() {
        let rome = family_base_colors(EnvKind::RomeEur);
        let urban = family_base_colors(EnvKind::Urban);
        // Building base should be creammer (higher r+g) than urban placeholder pastels
        // are not the runtime urban look — Urban A/B are seed-picked pastels.
        // Compare Rome building vs Mountain timber instead for warmth.
        let rb = rome[2].to_srgba();
        let mb = family_base_colors(EnvKind::MountainTown)[2].to_srgba();
        assert!(rb.red + rb.green > mb.red + mb.green);
        let _ = urban; // Urban defaults exist for non-seed fallback.
    }

    #[test]
    fn family_bases_are_distinct_per_env() {
        let envs = [
            EnvKind::Urban,
            EnvKind::MountainTown,
            EnvKind::DesertTown,
            EnvKind::SeaTown,
            EnvKind::RomeEur,
        ];
        let bases: Vec<[Color; 5]> = envs.iter().map(|e| family_base_colors(*e)).collect();
        // Non-urban envs must differ (Urban BuildingA/B are seed-picked pastels at rebuild).
        for (i, a) in bases.iter().enumerate().skip(1) {
            for (j, b) in bases.iter().enumerate().skip(i + 1) {
                let ca = a[2].to_srgba();
                let cb = b[2].to_srgba();
                let d = (ca.red - cb.red).abs()
                    + (ca.green - cb.green).abs()
                    + (ca.blue - cb.blue).abs();
                assert!(
                    d > 0.05,
                    "building bases of env {i} and {j} too similar (d={d})"
                );
            }
        }
        // Mountain building should read darker/warmer wood than urban pastel placeholder.
        let m = bases[1][2].to_srgba();
        let u = bases[0][2].to_srgba();
        assert!(m.red + m.green + m.blue < u.red + u.green + u.blue);
        // Desert building warmer (higher r, lower b ratio) than sea blue-grey.
        let d = bases[2][2].to_srgba();
        let s = bases[3][2].to_srgba();
        assert!(d.red > s.red);
        assert!(s.blue > d.blue);
    }

    #[test]
    fn wall_material_is_lambert_flat() {
        let m = wall_material(Color::WHITE, None, 0.95);
        assert!((m.reflectance - 0.0).abs() < 1e-6);
        assert!((m.metallic - 0.0).abs() < 1e-6);
        assert!(m.perceptual_roughness >= 0.9);
    }

    #[test]
    fn window_slits_deterministic_and_bounded() {
        let map = generate_map(EnvKind::Urban, "preview");
        let a = place_window_slits(&map.walls, map.arena_half);
        let b = place_window_slits(&map.walls, map.arena_half);
        assert_eq!(a, b, "same map must yield identical window slits");
        assert!(a.len() <= MAX_WINDOWS_PER_MAP);
        // Tall building boxes should produce at least a few windows on urban.
        assert!(
            !a.is_empty(),
            "urban preview should place some window slits"
        );
        // Short cover never gets windows.
        let cover = [box_at(0.0, 0.0, 0.0, 2.0, 1.0, 2.0)];
        assert!(place_window_slits(&cover, 30.0).is_empty());
        // Tall building does.
        let tall = [box_at(0.0, 0.0, 0.0, 4.0, 4.0, 0.4)];
        let tall_slits = place_window_slits(&tall, 30.0);
        assert!(!tall_slits.is_empty());
        // Mesh verts = 4 per slit.
        let geom = build_window_mesh(&tall_slits);
        assert_eq!(geom.positions.len(), tall_slits.len() * 4);
        assert_eq!(geom.indices.len(), tall_slits.len() * 6);
        // Bound: even a dense wall list cannot exceed the hard cap.
        let many: Vec<Aabb> = (0..200)
            .map(|i| {
                let x = (i % 20) as f32 * 5.0;
                let z = (i / 20) as f32 * 5.0;
                box_at(x, 0.0, z, x + 3.0, 5.0, z + 0.4)
            })
            .collect();
        let dense = place_window_slits(&many, 80.0);
        assert!(dense.len() <= MAX_WINDOWS_PER_MAP);
    }

    #[test]
    fn ad_material_has_emissive_lift() {
        // Pure unit check: the helper sets a non-zero emissive so ads read backlit.
        let mat = ad_material(Handle::default());
        assert!(mat.unlit);
        assert!(mat.emissive.red > 0.1);
        assert!(mat.emissive.green > 0.1);
    }

    #[test]
    fn sun_occlusion_open_vs_blocked() {
        let sun_to = Vec3::new(0.5, 0.8, 0.3).normalize();
        // Tall wall to the sun side of the probe point.
        let wall = box_at(2.0, 0.0, -4.0, 4.0, 8.0, 4.0);
        let walls = [wall];

        // Open ground far from the wall, away from the sun: fully lit.
        assert!((ground_light_at(-20.0, 0.0, sun_to, &walls) - 1.0).abs() < 1e-5);
        // Right behind the wall relative to the sun: fully shadowed.
        let shadowed = ground_light_at(0.5, 0.0, sun_to, &walls);
        assert!((shadowed - GROUND_SHADOW).abs() < 1e-5, "{shadowed}");
        // The invariant: factors never fall below the shadow floor.
        const {
            assert!(GROUND_SHADOW >= MIN_LIGHT);
        }
    }

    #[test]
    fn face_light_never_pitch_black() {
        let sun_to = Vec3::new(0.5, 0.8, 0.3).normalize();
        let building = box_at(-5.0, 0.0, -5.0, 5.0, 4.0, 5.0);
        // A face pointing straight away from the sun, fully enclosed.
        let f = face_light_factor(
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(-0.5, -0.8, -0.3).normalize(),
            sun_to,
            &[building],
        );
        assert!((MIN_LIGHT..1.0).contains(&f), "{f}");
        // A sunlit face in the open reaches full brightness.
        let lit = face_light_factor(Vec3::new(50.0, 1.0, 50.0), sun_to, sun_to, &[building]);
        assert!(lit > 0.95, "{lit}");
    }

    #[test]
    fn baked_face_colors_cover_every_vertex() {
        let boxes = [
            box_at(0.0, 0.0, 0.0, 2.0, 3.0, 2.0),
            box_at(5.0, 0.0, 5.0, 6.0, 1.0, 6.0),
        ];
        let mut geom = build_merged_boxes(&boxes);
        bake_face_colors(&mut geom, Vec3::new(0.4, 0.8, 0.2).normalize(), &boxes);
        assert_eq!(geom.colors.len(), geom.positions.len());
        for c in &geom.colors {
            assert!(c[0] >= MIN_LIGHT && c[0] <= 1.0, "{c:?}");
            assert_eq!(c[3], 1.0);
        }
    }

    #[test]
    fn ground_occlusion_grid_shape_and_bounds() {
        let walls = [box_at(-2.0, 0.0, -2.0, 2.0, 6.0, 2.0)];
        let sun_to = Vec3::new(0.6, 0.7, 0.2).normalize();
        let grid = bake_ground_occlusion(16, 20.0, sun_to, &walls);
        assert_eq!(grid.len(), 16 * 16);
        assert!(grid.iter().all(|f| (GROUND_SHADOW..=1.0).contains(f)));
        // The box must shadow SOMETHING and leave open ground lit.
        assert!(grid.iter().any(|f| *f < 1.0));
        assert!(grid.iter().any(|f| (*f - 1.0).abs() < 1e-5));
    }

    #[test]
    fn billboard_facing_inward() {
        // wall 0 faces +Z: local +Z maps to world +Z
        let r0 = billboard_facing_rotation(0);
        let f0 = r0 * Vec3::Z;
        assert!(f0.z > 0.9, "{f0:?}");

        let r1 = billboard_facing_rotation(1);
        let f1 = r1 * Vec3::Z;
        assert!(f1.z < -0.9, "{f1:?}");

        let r2 = billboard_facing_rotation(2);
        let f2 = r2 * Vec3::Z;
        assert!(f2.x > 0.9, "{f2:?}");

        let r3 = billboard_facing_rotation(3);
        let f3 = r3 * Vec3::Z;
        assert!(f3.x < -0.9, "{f3:?}");
    }
}
