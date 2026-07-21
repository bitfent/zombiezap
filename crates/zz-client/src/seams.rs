//! Shared seams between the session logic (game.rs, owns all networking and
//! state transitions) and the presentation modules (lobby_ui, hud, fx, audio
//! — which ONLY read these resources and push intents). Plain VecDeque queues
//! instead of Bevy events keep the contract engine-version-proof.
//!
//! The dead_code allow covers the window between defining this contract and
//! the worker branches that consume it — remove once hud/lobby_ui/audio land.
#![allow(dead_code)]

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

/// HOST / JOIN queued on the HTML start screen before the engine is ready.
/// Fired exactly once after handoff (see `apply_boot_handoff`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueuedBootAction {
    Host,
    Join(String),
}

/// M19 instant-boot handoff state.
///
/// The HTML start screen is interactive while Bevy boots. Name/code and an
/// optional HOST/JOIN intent live here until `engine_ready` + session Menu,
/// then name/code land in [`LobbyView`] and the intent is pushed once.
#[derive(Resource, Default, Debug)]
pub struct HtmlBoot {
    /// First Update frame completed (wasm) / immediate on native.
    pub engine_ready: bool,
    /// Name/code applied and start chrome dismissed (or native no-op).
    pub handed_off: bool,
    /// Queued HOST/JOIN has been pushed to [`UiQueue`] (or there was none).
    pub intent_fired: bool,
    /// Action queued from HTML (or tests) before/at handoff.
    pub queued: Option<QueuedBootAction>,
    /// Test/native inject for callsign when DOM is unavailable.
    pub inject_name: Option<String>,
    /// Test/native inject for lobby code when DOM is unavailable.
    pub inject_code: Option<String>,
}

/// Inputs for the pure handoff helper (DOM values + session readiness).
#[derive(Clone, Debug, Default)]
pub struct BootHandoffInput {
    pub html_name: Option<String>,
    pub html_code: Option<String>,
    /// Fresh peek of a pending HTML action (does not consume).
    pub peek_action: Option<QueuedBootAction>,
    /// Consume the pending HTML action (one-shot).
    pub take_action: Option<QueuedBootAction>,
    pub session_is_menu: bool,
    pub is_touch: bool,
}

/// Side effects requested by [`apply_boot_handoff`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BootHandoffEffect {
    /// Call platform handoff / hide start chrome.
    pub dismiss_start: bool,
    /// Touch vs desktop dismiss path.
    pub dismiss_touch: bool,
    /// Signal JS that the engine finished first Update.
    pub signal_engine_ready: bool,
}

/// Pure handoff: prefill LobbyView, fire queued HOST/JOIN once, dismiss HTML.
///
/// Returns effects for the platform layer. Idempotent after `handed_off` and
/// `intent_fired` are both set.
pub fn apply_boot_handoff(
    boot: &mut HtmlBoot,
    lobby: &mut LobbyView,
    draft_code: &mut String,
    queue: &mut UiQueue,
    input: BootHandoffInput,
) -> BootHandoffEffect {
    let mut effect = BootHandoffEffect::default();

    // Absorb a pending HTML action before ready (so a click is not lost).
    if !boot.intent_fired
        && let Some(a) = input.peek_action
    {
        boot.queued = Some(a);
    }

    // Name/code: prefer live HTML, else inject (tests), else leave as-is.
    let name = input
        .html_name
        .or_else(|| boot.inject_name.clone())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(n) = name
        && n != lobby.name
    {
        lobby.name = n;
    }
    let code = input
        .html_code
        .or_else(|| boot.inject_code.clone())
        .map(|s| {
            s.chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .flat_map(|c| c.to_uppercase())
                .take(5)
                .collect::<String>()
        })
        .filter(|s| !s.is_empty());
    if let Some(c) = code {
        *draft_code = c.clone();
        if lobby.join_prefill.is_none() {
            lobby.join_prefill = Some(c);
        }
    }

    if boot.engine_ready {
        effect.signal_engine_ready = true;
    }

    // Fire intent only once, only when the menu session can process it.
    if boot.engine_ready && input.session_is_menu && !boot.intent_fired {
        // Prefer take_action (consumes JS queue) over a previously peeked value.
        let action = input.take_action.or_else(|| boot.queued.take());
        boot.queued = None;
        match action {
            Some(QueuedBootAction::Host) => {
                queue.0.push_back(UiIntent::CreateLobby(EnvKind::Urban));
            }
            Some(QueuedBootAction::Join(code)) => {
                queue.0.push_back(UiIntent::JoinLobby(code));
            }
            None => {}
        }
        boot.intent_fired = true;
    }

    // Dismiss HTML start once ready + (menu or already fired intent path).
    if boot.engine_ready && !boot.handed_off && (boot.intent_fired || input.session_is_menu) {
        boot.handed_off = true;
        effect.dismiss_start = true;
        effect.dismiss_touch = input.is_touch;
    }

    effect
}

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
    /// One-shot prefill for the join-code field (`?join=` / `ZZ_JOIN`).
    /// lobby_ui copies this into its draft once, then leaves it alone.
    pub join_prefill: Option<String>,
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
    /// Dry-fire click (empty mag, no reserve / blocked fire).
    DryClick,
    /// Melee swing whoosh.
    MeleeSwing,
    /// Melee impact thunk.
    MeleeHit,
    /// Reload clack (start or end of the two-click cycle).
    ReloadClack,
    /// Proximity zombie growl; volume already distance-scaled by game.rs.
    Growl { volume: f32 },
}

#[derive(Resource, Default)]
pub struct SfxQueue(pub VecDeque<Sfx>);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sfx_growl_is_constructible() {
        let g = Sfx::Growl { volume: 0.5 };
        match g {
            Sfx::Growl { volume } => assert!((volume - 0.5).abs() < 1e-6),
            _ => panic!("expected Growl"),
        }
    }

    #[test]
    fn queued_host_before_ready_fires_once_after_ready() {
        let mut boot = HtmlBoot {
            queued: Some(QueuedBootAction::Host),
            inject_name: Some("alpha".into()),
            ..Default::default()
        };
        let mut lobby = LobbyView {
            name: "survivor".into(),
            ..Default::default()
        };
        let mut draft = String::new();
        let mut queue = UiQueue::default();

        // Not ready: must not fire.
        let _ = apply_boot_handoff(
            &mut boot,
            &mut lobby,
            &mut draft,
            &mut queue,
            BootHandoffInput {
                session_is_menu: true,
                ..Default::default()
            },
        );
        assert!(queue.0.is_empty());
        assert!(!boot.intent_fired);
        assert_eq!(lobby.name, "alpha");

        // Ready + Menu: fire CreateLobby once.
        boot.engine_ready = true;
        let e1 = apply_boot_handoff(
            &mut boot,
            &mut lobby,
            &mut draft,
            &mut queue,
            BootHandoffInput {
                session_is_menu: true,
                is_touch: false,
                ..Default::default()
            },
        );
        assert!(boot.intent_fired);
        assert!(boot.handed_off);
        assert!(e1.dismiss_start);
        assert_eq!(queue.0.len(), 1);
        assert!(matches!(
            queue.0.front(),
            Some(UiIntent::CreateLobby(EnvKind::Urban))
        ));

        // Second call: still exactly one intent.
        let _ = apply_boot_handoff(
            &mut boot,
            &mut lobby,
            &mut draft,
            &mut queue,
            BootHandoffInput {
                session_is_menu: true,
                take_action: Some(QueuedBootAction::Host),
                ..Default::default()
            },
        );
        assert_eq!(queue.0.len(), 1);
    }

    #[test]
    fn name_code_prefill_lands_in_lobby_view() {
        let mut boot = HtmlBoot {
            engine_ready: true,
            inject_name: Some("bravo".into()),
            inject_code: Some("xy12z".into()),
            ..Default::default()
        };
        let mut lobby = LobbyView::default();
        let mut draft = String::new();
        let mut queue = UiQueue::default();

        let _ = apply_boot_handoff(
            &mut boot,
            &mut lobby,
            &mut draft,
            &mut queue,
            BootHandoffInput {
                session_is_menu: true,
                ..Default::default()
            },
        );
        assert_eq!(lobby.name, "bravo");
        assert_eq!(draft, "XY12Z");
        assert_eq!(lobby.join_prefill.as_deref(), Some("XY12Z"));
        assert!(boot.handed_off);
        assert!(boot.intent_fired);
        assert!(queue.0.is_empty());
    }

    #[test]
    fn queued_join_fires_with_code() {
        let mut boot = HtmlBoot {
            engine_ready: true,
            queued: Some(QueuedBootAction::Join("ABCDE".into())),
            ..Default::default()
        };
        let mut lobby = LobbyView {
            name: "c".into(),
            ..Default::default()
        };
        let mut draft = String::new();
        let mut queue = UiQueue::default();
        let _ = apply_boot_handoff(
            &mut boot,
            &mut lobby,
            &mut draft,
            &mut queue,
            BootHandoffInput {
                session_is_menu: true,
                ..Default::default()
            },
        );
        assert!(matches!(
            queue.0.front(),
            Some(UiIntent::JoinLobby(c)) if c == "ABCDE"
        ));
    }
}
