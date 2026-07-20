//! Shared seams between the session logic (game.rs, owns all networking and
//! state transitions) and the presentation modules (lobby_ui, hud, fx, audio
//! — which ONLY read these resources and push intents). Plain VecDeque queues
//! instead of Bevy events keep the contract engine-version-proof.

use bevy::prelude::*;
use std::collections::VecDeque;
use zz_core::protocol::MatchStats;
use zz_core::snapshot::Snapshot;
use zz_core::types::EnvKind;

/// What the UI wants the session to do. lobby_ui pushes; game.rs drains and
/// translates into protocol messages / state changes.
pub enum UiIntent {
    SetName(String),
    CreateLobby(EnvKind),
    JoinLobby(String),
    SetEnv(EnvKind),
    StartGame,
    LeaveLobby,
    /// Dismiss the stats screen and return to the (already rejoined) lobby.
    BackToLobby,
    PauseToggle,
}

#[derive(Resource, Default)]
pub struct UiQueue(pub VecDeque<UiIntent>);

/// Read-only mirror of lobby state for the UI to paint. game.rs writes it
/// from LobbyState messages; lobby_ui renders it verbatim.
#[derive(Resource, Default, Clone)]
pub struct LobbyView {
    /// Player's chosen callsign (persisted across lobbies in-session).
    pub name: String,
    pub code: String,
    pub invite_url: Option<String>,
    /// (name, is_host, is_me) per occupant, slot order.
    pub players: Vec<(String, bool, bool)>,
    pub env: Option<EnvKind>,
    pub is_host: bool,
    /// One-line status/error surfaced under the controls.
    pub status: String,
}

/// The latest decoded world snapshot — HUD reads self stats, fx reads
/// loot/grenades, everyone reads paused/difficulty. Never mutated by readers.
#[derive(Resource, Default)]
pub struct LatestSnapshot(pub Option<Snapshot>);

/// Stats of the finished match, for the death/stats overlay.
#[derive(Resource, Default)]
pub struct LastStats(pub Option<MatchStats>);

/// Match roster from game_start: (slot, name, is_me). HUD teammate rows and
/// stats attribution read this.
#[derive(Resource, Default)]
pub struct Roster(pub Vec<(u8, String, bool)>);

/// Transient visual events derived from snapshots (shots are per-snapshot
/// transients on the wire; game.rs re-emits them here for fx to consume).
pub enum VisualEvent {
    /// Tracer from a player's muzzle to the endpoint. hit_kind: 0 wall/miss,
    /// 1 zombie hit, 2 zombie killed, 3 headshot kill.
    Shot {
        slot: u8,
        end: Vec3,
        hit_kind: u8,
        from_me: bool,
    },
    Boom {
        pos: Vec3,
    },
}

#[derive(Resource, Default)]
pub struct FxQueue(pub VecDeque<VisualEvent>);

/// Sound triggers, already gated/derived by game.rs (own-hurt detection,
/// pickup detection etc.). audio.rs consumes and synthesizes.
pub enum Sfx {
    Shoot { from_me: bool },
    HitConfirm,
    KillConfirm { headshot: bool },
    Explosion { dist: f32 },
    Hurt,
    Pickup,
    TeamWipe,
    Click,
}

#[derive(Resource, Default)]
pub struct SfxQueue(pub VecDeque<Sfx>);
