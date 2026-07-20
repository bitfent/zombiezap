//! In-match HUD + combat visuals + stats overlay. Reads seam resources only
//! (LatestSnapshot, FxQueue, LastStats, Session run conditions).
//! Implementation arrives with the hud worker branch.

use bevy::prelude::*;

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, _app: &mut App) {}
}
