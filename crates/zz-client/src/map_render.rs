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
//!   stripes), billboards are unlit "YOUR AD HERE"-style striped panels keyed
//!   by `ad_slot`;
//! - walls are grouped into a few material families by simple heuristics
//!   (ground cover < 2 m, building walls, roofs/high boxes, perimeter) and
//!   merged into ONE mesh per family — a handful of draw calls total.

use std::f32::consts::PI;

use bevy::{
    asset::RenderAssetUsages,
    image::{Image, ImageSampler},
    mesh::Indices,
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
};
use zz_core::map::GameMap;
use zz_core::types::Aabb;

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

/// Wall material family for merged draw calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WallFamily {
    Perimeter,
    Cover,
    Building,
    Roof,
}

/// Pure vertex/index buffer produced by the geometry builder.
#[derive(Clone, Debug, Default)]
pub struct MeshGeom {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
}

impl MeshGeom {
    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    fn append_box(&mut self, aabb: &Aabb) {
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
        // +Y (top) — U along X, V along Z
        self.push_face(
            [[x0, y1, z0], [x1, y1, z0], [x1, y1, z1], [x0, y1, z1]],
            [0.0, 1.0, 0.0],
            sx,
            sz,
        );
        // -Y (bottom) — U along X, V along Z (winding flipped for outward normal)
        self.push_face(
            [[x0, y0, z1], [x1, y0, z1], [x1, y0, z0], [x0, y0, z0]],
            [0.0, -1.0, 0.0],
            sx,
            sz,
        );
        // +X — U along Z, V along Y
        self.push_face(
            [[x1, y0, z0], [x1, y0, z1], [x1, y1, z1], [x1, y1, z0]],
            [1.0, 0.0, 0.0],
            sz,
            sy,
        );
        // -X
        self.push_face(
            [[x0, y0, z1], [x0, y0, z0], [x0, y1, z0], [x0, y1, z1]],
            [-1.0, 0.0, 0.0],
            sz,
            sy,
        );
        // +Z — U along X, V along Y
        self.push_face(
            [[x0, y0, z1], [x1, y0, z1], [x1, y1, z1], [x0, y1, z1]],
            [0.0, 0.0, 1.0],
            sx,
            sy,
        );
        // -Z
        self.push_face(
            [[x1, y0, z0], [x0, y0, z0], [x0, y1, z0], [x1, y1, z0]],
            [0.0, 0.0, -1.0],
            sx,
            sy,
        );
    }

    /// Quad with CCW winding when viewed along the outward normal.
    fn push_face(&mut self, corners: [[f32; 3]; 4], normal: [f32; 3], u_size: f32, v_size: f32) {
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
        Mesh::new(
            bevy::mesh::PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, self.uvs)
        .with_inserted_indices(Indices::U32(self.indices))
    }
}

/// Classify an AABB into a wall material family.
///
/// Priority: perimeter → roof → cover → building.
pub fn classify_wall(aabb: &Aabb, arena_half: f32) -> WallFamily {
    let height = (aabb.y1 - aabb.y0).abs();
    if height >= 5.0 && is_near_perimeter(aabb, arena_half) {
        return WallFamily::Perimeter;
    }
    if aabb.y0 > 2.5 {
        return WallFamily::Roof;
    }
    if aabb.y1 < 2.0 {
        return WallFamily::Cover;
    }
    WallFamily::Building
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
pub fn build_family_meshes(walls: &[Aabb], arena_half: f32) -> [(WallFamily, MeshGeom); 4] {
    let mut buckets: [Vec<Aabb>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    for w in walls {
        let fam = classify_wall(w, arena_half);
        let idx = family_index(fam);
        buckets[idx].push(*w);
    }
    [
        (WallFamily::Perimeter, build_merged_boxes(&buckets[0])),
        (WallFamily::Cover, build_merged_boxes(&buckets[1])),
        (WallFamily::Building, build_merged_boxes(&buckets[2])),
        (WallFamily::Roof, build_merged_boxes(&buckets[3])),
    ]
}

fn family_index(f: WallFamily) -> usize {
    match f {
        WallFamily::Perimeter => 0,
        WallFamily::Cover => 1,
        WallFamily::Building => 2,
        WallFamily::Roof => 3,
    }
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

fn gen_asphalt(size: u32) -> Image {
    let mut px = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let n = hash_noise(x, y, 1);
            let speck = hash_noise(x, y, 99) > 0.97;
            let base = 0.10 + n * 0.06;
            let v = if speck { base + 0.12 } else { base };
            let c = (v.clamp(0.0, 1.0) * 255.0) as u8;
            px.extend_from_slice(&[c, c, (c as f32 * 0.95) as u8, 255]);
        }
    }
    rgba_image(size, size, px)
}

fn gen_concrete(size: u32, dark: bool) -> Image {
    let mut px = Vec::with_capacity((size * size * 4) as usize);
    let base0 = if dark { 0.28 } else { 0.42 };
    for y in 0..size {
        for x in 0..size {
            let n = hash_noise(x, y, if dark { 3 } else { 2 });
            // Faint horizontal darker bands every ~32 px.
            let band = if (y % 32) < 2 { 0.08 } else { 0.0 };
            let v = (base0 + n * 0.08 - band).clamp(0.0, 1.0);
            let c = (v * 255.0) as u8;
            px.extend_from_slice(&[c, c, c, 255]);
        }
    }
    rgba_image(size, size, px)
}

fn gen_cover(size: u32) -> Image {
    let mut px = Vec::with_capacity((size * size * 4) as usize);
    let border = 4u32;
    for y in 0..size {
        for x in 0..size {
            let n = hash_noise(x, y, 4);
            let edge = x < border || y < border || x >= size - border || y >= size - border;
            let (r, g, b) = if edge {
                (0.22, 0.14, 0.08)
            } else {
                let v = 0.40 + n * 0.10;
                (v, v * 0.72, v * 0.42)
            };
            px.extend_from_slice(&[(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8, 255]);
        }
    }
    rgba_image(size, size, px)
}

fn gen_roof(size: u32) -> Image {
    let mut px = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let n = hash_noise(x, y, 5);
            let v = 0.06 + n * 0.05;
            let c = (v.clamp(0.0, 1.0) * 255.0) as u8;
            px.extend_from_slice(&[c, c, c, 255]);
        }
    }
    rgba_image(size, size, px)
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

impl Plugin for MapRenderPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, rebuild_map_system);
    }
}

#[allow(clippy::too_many_arguments)] // Bevy system param list
fn rebuild_map_system(
    mut commands: Commands,
    current: Option<Res<CurrentMap>>,
    roots: Query<Entity, With<MapRoot>>,
    placeholders: Query<Entity, With<Placeholder>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut ambient: ResMut<GlobalAmbientLight>,
) {
    let Some(current) = current else {
        return;
    };
    if !current.is_changed() {
        return;
    }

    // Tear down previous map + any skeleton placeholders.
    for e in roots.iter() {
        commands.entity(e).despawn();
    }
    for e in placeholders.iter() {
        commands.entity(e).despawn();
    }

    let map = &current.0;
    let accent = accent_color(map.accent);

    // Soft ambient fill for the map build.
    ambient.color = Color::srgb(0.55, 0.58, 0.62);
    ambient.brightness = 120.0;

    // Procedural textures (small, nearest-filtered).
    let tex_asphalt = images.add(gen_asphalt(128));
    let tex_concrete = images.add(gen_concrete(128, false));
    let tex_perimeter = images.add(gen_concrete(128, true));
    let tex_cover = images.add(gen_cover(128));
    let tex_roof = images.add(gen_roof(128));
    let ad_handles: [Handle<Image>; 4] = [
        images.add(gen_ad(0, 256, 128)),
        images.add(gen_ad(1, 256, 128)),
        images.add(gen_ad(2, 256, 128)),
        images.add(gen_ad(3, 256, 128)),
    ];

    let family_mats: [Handle<StandardMaterial>; 4] = [
        materials.add(wall_material(
            tinted(Color::srgb(0.35, 0.35, 0.36), accent),
            Some(tex_perimeter.clone()),
            0.92,
        )),
        materials.add(wall_material(
            tinted(Color::srgb(0.48, 0.34, 0.22), accent),
            Some(tex_cover.clone()),
            0.90,
        )),
        materials.add(wall_material(
            tinted(Color::srgb(0.50, 0.50, 0.52), accent),
            Some(tex_concrete.clone()),
            0.88,
        )),
        materials.add(wall_material(
            tinted(Color::srgb(0.18, 0.18, 0.20), accent),
            Some(tex_roof.clone()),
            0.95,
        )),
    ];

    let ground_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.14, 0.14, 0.15),
        base_color_texture: Some(tex_asphalt),
        perceptual_roughness: 0.95,
        metallic: 0.0,
        ..default()
    });

    let frame_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.08, 0.08, 0.09),
        perceptual_roughness: 0.9,
        metallic: 0.05,
        unlit: false,
        ..default()
    });

    let ad_mats: [Handle<StandardMaterial>; 4] = [
        materials.add(ad_material(ad_handles[0].clone())),
        materials.add(ad_material(ad_handles[1].clone())),
        materials.add(ad_material(ad_handles[2].clone())),
        materials.add(ad_material(ad_handles[3].clone())),
    ];

    let family_geoms = build_family_meshes(&map.walls, map.arena_half);
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

            // Merged wall families (one entity each, skip empty).
            for (fam, geom) in family_geoms {
                if geom.is_empty() {
                    continue;
                }
                let mat = family_mats[family_index(fam)].clone();
                let mesh = meshes.add(geom.into_mesh());
                root.spawn((
                    Mesh3d(mesh),
                    MeshMaterial3d(mat),
                    Transform::IDENTITY,
                    Name::new(format!("Walls/{fam:?}")),
                ));
            }

            // Billboards + thin dark frames.
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

            // Map-owned sun: shadows OFF. Skeleton light is left alone (not Placeholder).
            root.spawn((
                DirectionalLight {
                    illuminance: 10_000.0,
                    shadow_maps_enabled: false,
                    ..default()
                },
                Transform::from_rotation(Quat::from_euler(
                    EulerRot::XYZ,
                    -PI * 0.35,
                    PI * 0.2,
                    0.0,
                )),
                Name::new("MapSun"),
            ));
        });
}

fn wall_material(
    base_color: Color,
    texture: Option<Handle<Image>>,
    roughness: f32,
) -> StandardMaterial {
    StandardMaterial {
        base_color,
        base_color_texture: texture,
        perceptual_roughness: roughness,
        metallic: 0.02,
        ..default()
    }
}

fn ad_material(texture: Handle<Image>) -> StandardMaterial {
    StandardMaterial {
        base_color: Color::WHITE,
        base_color_texture: Some(texture),
        unlit: true,
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

    #[test]
    fn family_classifier_representative_boxes() {
        let half = 30.0;
        // Perimeter wall on -z edge, tall.
        let peri = box_at(-5.0, 0.0, -30.0, 5.0, 6.0, -29.4);
        assert_eq!(classify_wall(&peri, half), WallFamily::Perimeter);

        // Low street cover / crate.
        let cover = box_at(1.0, 0.0, 1.0, 2.0, 1.2, 2.0);
        assert_eq!(classify_wall(&cover, half), WallFamily::Cover);

        // Building wall (mid height, interior).
        let build = box_at(0.0, 0.0, 0.0, 0.4, 3.2, 4.0);
        assert_eq!(classify_wall(&build, half), WallFamily::Building);

        // Roof slab (bottom above 2.5 m).
        let roof = box_at(0.0, 3.2, 0.0, 4.0, 3.55, 4.0);
        assert_eq!(classify_wall(&roof, half), WallFamily::Roof);
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
        let _ = gen_asphalt(16);
        let _ = gen_concrete(16, false);
        let _ = gen_concrete(16, true);
        let _ = gen_cover(16);
        let _ = gen_roof(16);
        for s in 0..4u8 {
            let _ = gen_ad(s, 32, 16);
        }

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
