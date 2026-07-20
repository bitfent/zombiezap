//! Menu + lobby overlay (egui): name entry, create/join by code, roster,
//! env picker, invite link, start. Paints seams::LobbyView verbatim and
//! pushes seams::UiIntent — no networking, no state transitions here.
//! Implementation arrives with the lobby-ui worker branch.

use bevy::prelude::*;

pub struct LobbyUiPlugin;

impl Plugin for LobbyUiPlugin {
    fn build(&self, _app: &mut App) {}
}
