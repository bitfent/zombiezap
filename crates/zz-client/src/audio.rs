//! Zero-asset synthesized SFX (the ShotAnte oscillator philosophy): consumes
//! seams::SfxQueue, renders short procedural buffers, plays with distance
//! attenuation. Implementation arrives with the audio worker branch.

use bevy::prelude::*;

pub struct AudioPlugin;

impl Plugin for AudioPlugin {
    fn build(&self, _app: &mut App) {}
}
