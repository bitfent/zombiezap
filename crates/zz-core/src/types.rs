//! Plain data shared by every layer: geometry, bodies, inputs.

use serde::{Deserialize, Serialize};

/// Procedural / fixture map environments.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvKind {
    Urban,
    MountainTown,
    DesertTown,
    SeaTown,
    /// Rome EUR real-place fixture (OSM-baked; see `map::rome_eur`).
    RomeEur,
}

/// Axis-aligned box — the only collision primitive in the game.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Aabb {
    pub x0: f32,
    pub x1: f32,
    pub y0: f32,
    pub y1: f32,
    pub z0: f32,
    pub z1: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Spawn {
    pub x: f32,
    pub z: f32,
    pub yaw: f32,
}

/// A simulated body: feet position + vertical velocity. Horizontal velocity is
/// implicit (movement is input-driven, not momentum-driven — retro on purpose).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Body {
    pub x: f32,
    /// Feet height; 0 = ground.
    pub y: f32,
    pub z: f32,
    pub vy: f32,
    pub on_ground: bool,
}

impl Body {
    pub fn at(x: f32, z: f32) -> Self {
        Body {
            x,
            y: 0.0,
            z,
            vy: 0.0,
            on_ground: true,
        }
    }
}

/// One tick's worth of player intent. The ONLY thing a client may tell the
/// server about its player.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PlayerInput {
    pub seq: u32,
    pub forward: bool,
    pub backward: bool,
    pub left: bool,
    pub right: bool,
    pub jump: bool,
    pub fire: bool,
    pub grenade: bool,
    pub interact: bool,
    /// Rifle-butt melee swing (edge-triggered on the server).
    pub melee: bool,
    /// Manual reload (edge-triggered; also auto-started by fire on empty mag).
    pub reload: bool,
    pub yaw: f32,
    pub pitch: f32,
}
