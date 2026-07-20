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

use bevy::prelude::*;
use zz_core::map::GameMap;

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

impl Plugin for MapRenderPlugin {
    fn build(&self, _app: &mut App) {
        // implementation arrives with the map-render worker branch
    }
}
