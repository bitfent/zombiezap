//! M23b: pointer event latch for bevy_egui — order MOVE before PRESS before RELEASE.
//!
//! bevy_egui multipass (and browser trackpad / automation taps) can drop clicks
//! when several pointer events land in a single Bevy frame:
//!
//! 1. **M23** — same-frame press+release: the button appears to do nothing.
//! 2. **M23b** — teleport click: CursorMoved + press (+ release) in one winit
//!    batch. egui hit-tests / hover from the previous frame still see the
//!    pointer *not* over the button when the press is applied, so the click
//!    never arms. Drag-clicks work because they MOVE one frame earlier.
//!
//! Fix: after bevy_egui has filled [`EguiInput`] for the frame, re-stage any
//! batch that packs move+press together or press+release together so egui
//! sees them across frames:
//!
//! - frame N: pointer-move only
//! - frame N+1: press
//! - frame N+2: release
//!
//! Events that already span frames pass through unchanged (no extra latency
//! for normal human input). Only co-batched pairs are split.
//!
//! Schedule: runs in [`PreUpdate`] after
//! [`bevy_egui::EguiPreUpdateSet::ProcessInput`] and before
//! [`bevy_egui::EguiPreUpdateSet::BeginPass`] / the multipass pass loop.

use bevy::prelude::*;
use bevy_egui::{
    egui::{Event, PointerButton},
    EguiInput, EguiPreUpdateSet,
};

/// Buffered pointer events to inject on the next frame.
///
/// Keyed by egui context entity so multi-context setups stay isolated.
#[derive(Resource, Default)]
pub struct EguiClickLatch {
    /// `(context_entity, event)` queued for the next PreUpdate.
    deferred: Vec<(Entity, Event)>,
}

pub struct EguiClickLatchPlugin;

impl Plugin for EguiClickLatchPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<EguiClickLatch>().add_systems(
            PreUpdate,
            latch_pointer_event_order
                .after(EguiPreUpdateSet::ProcessInput)
                .before(EguiPreUpdateSet::BeginPass),
        );
    }
}

/// Inject deferred events, then park any same-frame move+press or press+release
/// that would otherwise be processed in one egui pass.
fn latch_pointer_event_order(
    mut latch: ResMut<EguiClickLatch>,
    mut contexts: Query<(Entity, &mut EguiInput)>,
) {
    let mut deferred = std::mem::take(&mut latch.deferred);

    for (entity, mut egui_input) in &mut contexts {
        // 1) Inject events deferred from the previous frame first.
        let mut inject = Vec::new();
        deferred.retain(|(ctx, ev)| {
            if *ctx == entity {
                inject.push(ev.clone());
                false
            } else {
                true
            }
        });
        if !inject.is_empty() {
            inject.append(&mut egui_input.events);
            egui_input.events = inject;
        }

        // 2) Re-stage same-frame move+press and/or press+release on this context.
        let mut held = Vec::new();
        split_same_frame_pointer_batches(&mut egui_input.events, &mut held);
        for ev in held {
            latch.deferred.push((entity, ev));
        }
    }

    // Contexts that disappeared this frame drop their deferred events.
    latch.deferred.extend(deferred);
}

/// Pure event-stream transform used by the system and unit tests.
///
/// Walks `events` in order and enforces cross-frame ordering for co-batched
/// pointer gestures:
///
/// 1. If a [`Event::PointerMoved`] and a button **press** share this list,
///    keep moves (and other non-button events); defer every press and every
///    release of a button that pressed in this list.
/// 2. Else if a **press** and a later **release** of the same button share
///    this list, keep the press; defer that release (M23).
/// 3. Otherwise leave the stream alone (already spans frames).
///
/// Applying (1) then (2) across successive frames turns a single-frame
/// teleport click `[Move, Press, Release]` into three frames without
/// delaying input that already arrives one event class per frame.
pub fn split_same_frame_pointer_batches(events: &mut Vec<Event>, deferred_out: &mut Vec<Event>) {
    let has_move = events.iter().any(|e| matches!(e, Event::PointerMoved(_)));
    let has_press = events.iter().any(|e| {
        matches!(
            e,
            Event::PointerButton {
                pressed: true,
                ..
            }
        )
    });

    if has_move && has_press {
        defer_presses_and_matching_releases(events, deferred_out);
        return;
    }

    // M23: press+release only (no co-batched move).
    split_same_frame_releases(events, deferred_out);
}

/// Keep non-button events (including moves). Defer every press and every
/// release of a button that also presses in this list.
fn defer_presses_and_matching_releases(events: &mut Vec<Event>, deferred_out: &mut Vec<Event>) {
    let mut press_seen = [false; POINTER_BUTTON_COUNT];
    for ev in events.iter() {
        if let Event::PointerButton {
            button,
            pressed: true,
            ..
        } = ev
            && let Some(i) = pointer_button_index(*button)
        {
            press_seen[i] = true;
        }
    }

    let mut kept = Vec::with_capacity(events.len());
    for ev in events.drain(..) {
        match ev {
            Event::PointerButton {
                button,
                pressed: true,
                pos,
                modifiers,
            } => {
                // Always defer presses when co-batched with a move.
                deferred_out.push(Event::PointerButton {
                    button,
                    pressed: true,
                    pos,
                    modifiers,
                });
            }
            Event::PointerButton {
                button,
                pressed: false,
                pos,
                modifiers,
            } => {
                if pointer_button_index(button).is_some_and(|i| press_seen[i]) {
                    deferred_out.push(Event::PointerButton {
                        button,
                        pressed: false,
                        pos,
                        modifiers,
                    });
                } else {
                    // Orphan release (previous frame leftover) stays.
                    kept.push(Event::PointerButton {
                        button,
                        pressed: false,
                        pos,
                        modifiers,
                    });
                }
            }
            other => kept.push(other),
        }
    }
    *events = kept;
}

/// M23 helper: a **release** is deferred only if a **press** of the same
/// button already appeared earlier in this same list.
///
/// - same-frame press→release splits across frames;
/// - a release injected from the previous frame (no preceding press in the
///   list) is left alone even if a *new* press follows later in the frame.
pub fn split_same_frame_releases(events: &mut Vec<Event>, deferred_out: &mut Vec<Event>) {
    let mut press_seen = [false; POINTER_BUTTON_COUNT];
    let mut kept = Vec::with_capacity(events.len());
    for ev in events.drain(..) {
        match &ev {
            Event::PointerButton {
                button,
                pressed: true,
                ..
            } => {
                if let Some(i) = pointer_button_index(*button) {
                    press_seen[i] = true;
                }
                kept.push(ev);
            }
            Event::PointerButton {
                button,
                pressed: false,
                ..
            } => {
                if pointer_button_index(*button).is_some_and(|i| press_seen[i]) {
                    deferred_out.push(ev);
                } else {
                    kept.push(ev);
                }
            }
            _ => kept.push(ev),
        }
    }
    *events = kept;
}

const POINTER_BUTTON_COUNT: usize = 5;

fn pointer_button_index(button: PointerButton) -> Option<usize> {
    match button {
        PointerButton::Primary => Some(0),
        PointerButton::Secondary => Some(1),
        PointerButton::Middle => Some(2),
        PointerButton::Extra1 => Some(3),
        PointerButton::Extra2 => Some(4),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_egui::egui::{Modifiers, Pos2};

    fn press(button: PointerButton, pos: Pos2) -> Event {
        Event::PointerButton {
            pos,
            button,
            pressed: true,
            modifiers: Modifiers::NONE,
        }
    }

    fn release(button: PointerButton, pos: Pos2) -> Event {
        Event::PointerButton {
            pos,
            button,
            pressed: false,
            modifiers: Modifiers::NONE,
        }
    }

    fn is_primary_press(ev: &Event) -> bool {
        matches!(
            ev,
            Event::PointerButton {
                pressed: true,
                button: PointerButton::Primary,
                ..
            }
        )
    }

    fn is_primary_release(ev: &Event) -> bool {
        matches!(
            ev,
            Event::PointerButton {
                pressed: false,
                button: PointerButton::Primary,
                ..
            }
        )
    }

    // ── pure stream: M23 press+release (no move) ──────────────────────────

    #[test]
    fn same_frame_press_release_defers_release_only() {
        let pos = Pos2::new(100.0, 200.0);
        // Press+release only — no PointerMoved co-batched.
        let mut events = vec![
            press(PointerButton::Primary, pos),
            release(PointerButton::Primary, pos),
        ];
        let mut deferred = Vec::new();
        split_same_frame_pointer_batches(&mut events, &mut deferred);

        assert_eq!(events.len(), 1, "press stays");
        assert!(is_primary_press(&events[0]));
        assert_eq!(deferred.len(), 1);
        assert!(is_primary_release(&deferred[0]));
    }

    #[test]
    fn press_only_unchanged() {
        let pos = Pos2::ZERO;
        let mut events = vec![press(PointerButton::Primary, pos)];
        let mut deferred = Vec::new();
        split_same_frame_pointer_batches(&mut events, &mut deferred);
        assert_eq!(events.len(), 1);
        assert!(deferred.is_empty());
    }

    #[test]
    fn release_only_unchanged() {
        let pos = Pos2::ZERO;
        let mut events = vec![release(PointerButton::Primary, pos)];
        let mut deferred = Vec::new();
        split_same_frame_pointer_batches(&mut events, &mut deferred);
        assert_eq!(events.len(), 1);
        assert!(deferred.is_empty());
    }

    #[test]
    fn move_only_unchanged() {
        let pos = Pos2::new(1.0, 2.0);
        let mut events = vec![Event::PointerMoved(pos)];
        let mut deferred = Vec::new();
        split_same_frame_pointer_batches(&mut events, &mut deferred);
        assert_eq!(events.len(), 1);
        assert!(deferred.is_empty());
    }

    #[test]
    fn only_defers_buttons_that_also_pressed() {
        let pos = Pos2::new(1.0, 2.0);
        let mut events = vec![
            press(PointerButton::Primary, pos),
            release(PointerButton::Primary, pos),
            release(PointerButton::Secondary, pos), // no matching press
        ];
        let mut deferred = Vec::new();
        split_same_frame_pointer_batches(&mut events, &mut deferred);
        assert_eq!(deferred.len(), 1);
        assert!(matches!(
            &deferred[0],
            Event::PointerButton {
                button: PointerButton::Primary,
                pressed: false,
                ..
            }
        ));
        assert!(
            events.iter().any(|e| matches!(
                e,
                Event::PointerButton {
                    button: PointerButton::Secondary,
                    pressed: false,
                    ..
                }
            )),
            "orphan secondary release stays"
        );
    }

    // ── M23b: teleport click = move+press(+release) same frame ────────────

    /// HEAD (M23-only) left move+press on the same frame — that is the live
    /// failure mode for plain CDP clicks. Must defer the press.
    #[test]
    fn teleport_move_press_release_defers_press_and_release() {
        let pos = Pos2::new(100.0, 200.0);
        let mut events = vec![
            Event::PointerMoved(pos),
            press(PointerButton::Primary, pos),
            release(PointerButton::Primary, pos),
        ];
        let mut deferred = Vec::new();
        split_same_frame_pointer_batches(&mut events, &mut deferred);

        assert_eq!(events.len(), 1, "frame N: move only");
        assert!(matches!(&events[0], Event::PointerMoved(p) if *p == pos));
        assert_eq!(deferred.len(), 2, "press + release deferred");
        assert!(is_primary_press(&deferred[0]));
        assert!(is_primary_release(&deferred[1]));
    }

    #[test]
    fn move_plus_press_defers_press_only() {
        let pos = Pos2::new(40.0, 60.0);
        let mut events = vec![Event::PointerMoved(pos), press(PointerButton::Primary, pos)];
        let mut deferred = Vec::new();
        split_same_frame_pointer_batches(&mut events, &mut deferred);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Event::PointerMoved(_)));
        assert_eq!(deferred.len(), 1);
        assert!(is_primary_press(&deferred[0]));
    }

    /// Three-frame roundtrip of a single-batch teleport click.
    #[test]
    fn teleport_batch_stages_across_three_frames() {
        let pos = Pos2::new(40.0, 60.0);
        // Frame N: move+press+release → move kept, press+release deferred.
        let mut frame_n = vec![
            Event::PointerMoved(pos),
            press(PointerButton::Primary, pos),
            release(PointerButton::Primary, pos),
        ];
        let mut deferred = Vec::new();
        split_same_frame_pointer_batches(&mut frame_n, &mut deferred);
        assert_eq!(frame_n.len(), 1);
        assert!(matches!(frame_n[0], Event::PointerMoved(_)));
        assert_eq!(deferred.len(), 2);

        // Frame N+1: inject deferred → press kept, release re-deferred (M23).
        let mut frame_n1 = deferred;
        let mut deferred2 = Vec::new();
        split_same_frame_pointer_batches(&mut frame_n1, &mut deferred2);
        assert_eq!(frame_n1.len(), 1);
        assert!(is_primary_press(&frame_n1[0]));
        assert_eq!(deferred2.len(), 1);
        assert!(is_primary_release(&deferred2[0]));

        // Frame N+2: release only — pass through.
        let mut frame_n2 = deferred2;
        let mut deferred3 = Vec::new();
        split_same_frame_pointer_batches(&mut frame_n2, &mut deferred3);
        assert!(deferred3.is_empty());
        assert_eq!(frame_n2.len(), 1);
        assert!(is_primary_release(&frame_n2[0]));
    }

    #[test]
    fn inject_then_split_roundtrip_two_frames() {
        // Frame N: same-frame press+release (no move) → press kept, release deferred.
        let pos = Pos2::new(40.0, 60.0);
        let mut frame_n = vec![
            press(PointerButton::Primary, pos),
            release(PointerButton::Primary, pos),
        ];
        let mut deferred = Vec::new();
        split_same_frame_pointer_batches(&mut frame_n, &mut deferred);
        assert_eq!(deferred.len(), 1);

        // Frame N+1: inject deferred first, then any new events (none).
        let mut frame_n1 = deferred;
        let mut deferred2 = Vec::new();
        split_same_frame_pointer_batches(&mut frame_n1, &mut deferred2);
        assert!(deferred2.is_empty(), "release-only must not re-defer");
        assert_eq!(frame_n1.len(), 1);
        assert!(is_primary_release(&frame_n1[0]));
    }

    #[test]
    fn injected_release_not_redeferred_when_new_press_follows() {
        let pos = Pos2::new(10.0, 10.0);
        // Previous frame's deferred release is prepended, then a fresh press.
        let mut events = vec![
            release(PointerButton::Primary, pos),
            press(PointerButton::Primary, pos),
        ];
        let mut deferred = Vec::new();
        split_same_frame_pointer_batches(&mut events, &mut deferred);
        assert!(
            deferred.is_empty(),
            "orphan leading release must not re-defer"
        );
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn release_then_press_then_release_defers_only_second_release() {
        let pos = Pos2::ZERO;
        let mut events = vec![
            release(PointerButton::Primary, pos), // previous frame leftover
            press(PointerButton::Primary, pos),
            release(PointerButton::Primary, pos), // same-frame pair
        ];
        let mut deferred = Vec::new();
        split_same_frame_pointer_batches(&mut events, &mut deferred);
        assert_eq!(deferred.len(), 1);
        assert!(is_primary_release(&deferred[0]));
        assert_eq!(events.len(), 2); // leading release + press
    }

    /// Human path already spans frames: move on frame N, press on N+1 —
    /// must NOT add latency (no co-batched move+press).
    #[test]
    fn already_spanned_move_then_press_not_restaged() {
        let pos = Pos2::new(8.0, 8.0);
        let mut move_frame = vec![Event::PointerMoved(pos)];
        let mut d = Vec::new();
        split_same_frame_pointer_batches(&mut move_frame, &mut d);
        assert!(d.is_empty());

        let mut press_frame = vec![press(PointerButton::Primary, pos)];
        split_same_frame_pointer_batches(&mut press_frame, &mut d);
        assert!(d.is_empty());
        assert_eq!(press_frame.len(), 1);
    }

    // ── egui end-to-end ───────────────────────────────────────────────────

    /// End-to-end against real egui: teleport click staged across three frames
    /// registers exactly one click (with multipass discard forced).
    #[test]
    fn latched_teleport_stream_produces_exactly_one_egui_click() {
        use bevy_egui::egui::{Context, Rect};

        let ctx = Context::default();
        let mut t = 0.0_f64;
        let mut btn_rect = Rect::NOTHING;

        let empty = |t: f64| RawInputFrame::new(t);
        // Warm widget rects with pointer far away (stale hover).
        for _ in 0..4 {
            t += 1.0 / 60.0;
            let mut warm = empty(t).into_raw();
            warm.events = vec![Event::PointerMoved(Pos2::new(1.0, 1.0))];
            let _ = ctx.run_ui(warm, |ui| {
                btn_rect = ui.button("HOST").rect;
            });
        }
        let pos = btn_rect.center();

        // Frame N: move only (after latch).
        t += 1.0 / 60.0;
        let mut move_input = empty(t).into_raw();
        move_input.events = vec![Event::PointerMoved(pos)];
        let mut clicks = 0_u32;
        let _ = ctx.run_ui(move_input, |ui| {
            if ui.button("HOST").clicked() {
                clicks += 1;
            }
            ui.ctx().request_discard("simulate multipass");
        });
        assert_eq!(clicks, 0, "move alone must not click");

        // Frame N+1: press only.
        t += 1.0 / 60.0;
        let mut press_input = empty(t).into_raw();
        press_input.events = vec![press(PointerButton::Primary, pos)];
        let _ = ctx.run_ui(press_input, |ui| {
            if ui.button("HOST").clicked() {
                clicks += 1;
            }
            ui.ctx().request_discard("simulate multipass");
        });
        assert_eq!(clicks, 0, "press alone must not click");

        // Frame N+2: deferred release.
        t += 1.0 / 60.0;
        let mut release_input = empty(t).into_raw();
        release_input.events = vec![release(PointerButton::Primary, pos)];
        let _ = ctx.run_ui(release_input, |ui| {
            if ui.button("HOST").clicked() {
                clicks += 1;
            }
            ui.ctx().request_discard("simulate multipass");
        });
        assert_eq!(clicks, 1, "latched release must land exactly once");
    }

    /// Approach-agnostic contract: a same-frame teleport batch through the
    /// latch yields move → press → release across three frames and one click.
    /// Without move-before-press staging this documents the M23b failure mode.
    #[test]
    fn same_frame_teleport_after_latch_is_three_frames_and_one_click() {
        use bevy_egui::egui::{Context, Rect};

        let ctx = Context::default();
        let mut t = 0.0_f64;
        let mut btn_rect = Rect::NOTHING;
        for _ in 0..4 {
            t += 1.0 / 60.0;
            let mut warm = RawInputFrame::new(t).into_raw();
            warm.events = vec![Event::PointerMoved(Pos2::new(1.0, 1.0))];
            let _ = ctx.run_ui(warm, |ui| {
                btn_rect = ui.button("[ HOST A GAME ]").rect;
            });
        }
        let pos = btn_rect.center();

        let mut batch = vec![
            Event::PointerMoved(pos),
            press(PointerButton::Primary, pos),
            release(PointerButton::Primary, pos),
        ];
        let mut deferred = Vec::new();
        split_same_frame_pointer_batches(&mut batch, &mut deferred);

        assert!(
            batch.iter().all(|e| matches!(e, Event::PointerMoved(_))),
            "frame N must be move-only after latch"
        );
        assert!(
            deferred.iter().any(is_primary_press),
            "press must be deferred off the move frame (latch no-op for M23b?)"
        );
        assert!(
            deferred.iter().any(is_primary_release),
            "release must be deferred off the move frame"
        );

        let mut clicks = 0_u32;

        // Frame N: move
        t += 1.0 / 60.0;
        let mut n = RawInputFrame::new(t).into_raw();
        n.events = batch;
        let _ = ctx.run_ui(n, |ui| {
            if ui.button("[ HOST A GAME ]").clicked() {
                clicks += 1;
            }
        });

        // Frame N+1: inject deferred → re-split press vs release
        let mut n1_events = deferred;
        let mut deferred2 = Vec::new();
        split_same_frame_pointer_batches(&mut n1_events, &mut deferred2);
        t += 1.0 / 60.0;
        let mut n1 = RawInputFrame::new(t).into_raw();
        n1.events = n1_events;
        let _ = ctx.run_ui(n1, |ui| {
            if ui.button("[ HOST A GAME ]").clicked() {
                clicks += 1;
            }
        });

        // Frame N+2: release
        t += 1.0 / 60.0;
        let mut n2 = RawInputFrame::new(t).into_raw();
        n2.events = deferred2;
        let _ = ctx.run_ui(n2, |ui| {
            if ui.button("[ HOST A GAME ]").clicked() {
                clicks += 1;
            }
        });
        assert_eq!(clicks, 1);
    }

    struct RawInputFrame {
        t: f64,
    }

    impl RawInputFrame {
        fn new(t: f64) -> Self {
            Self { t }
        }

        fn into_raw(self) -> bevy_egui::egui::RawInput {
            use bevy_egui::egui::{Rect, Vec2};
            bevy_egui::egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 720.0))),
                time: Some(self.t),
                predicted_dt: 1.0 / 60.0,
                ..Default::default()
            }
        }
    }

    /// Menu / lobby button labels: one latched teleport click each → one
    /// intent. If the latch is removed / M23b regresses, this fails.
    #[test]
    fn each_menu_button_label_one_latched_teleport_click() {
        use bevy_egui::egui::{Context, Rect};

        let labels = [
            "[ HOST A GAME ]",
            "[ JOIN ]",
            "[ START ]",
            "[ LEAVE ]",
            "URBAN",
            "MOUNTAIN",
            "DESERT",
            "SEA",
            "ROME EUR",
            "[ COPY LINK ]",
        ];

        for label in labels {
            let ctx = Context::default();
            let mut t = 0.0_f64;
            let mut btn_rect = Rect::NOTHING;
            for _ in 0..4 {
                t += 1.0 / 60.0;
                let mut warm = RawInputFrame::new(t).into_raw();
                warm.events = vec![Event::PointerMoved(Pos2::new(1.0, 1.0))];
                let _ = ctx.run_ui(warm, |ui| {
                    btn_rect = ui.button(label).rect;
                });
            }
            let pos = btn_rect.center();

            // Single-frame teleport batch → three-frame latch.
            let mut pair = vec![
                Event::PointerMoved(pos),
                press(PointerButton::Primary, pos),
                release(PointerButton::Primary, pos),
            ];
            let mut deferred = Vec::new();
            split_same_frame_pointer_batches(&mut pair, &mut deferred);
            assert!(
                pair.iter().all(|e| matches!(e, Event::PointerMoved(_))),
                "{label}: frame N must be move-only"
            );
            assert!(
                deferred.iter().any(is_primary_press),
                "{label}: press deferred"
            );

            let mut clicks = 0_u32;
            t += 1.0 / 60.0;
            let mut n = RawInputFrame::new(t).into_raw();
            n.events = pair;
            let _ = ctx.run_ui(n, |ui| {
                if ui.button(label).clicked() {
                    clicks += 1;
                }
            });

            let mut n1_events = deferred;
            let mut deferred2 = Vec::new();
            split_same_frame_pointer_batches(&mut n1_events, &mut deferred2);
            t += 1.0 / 60.0;
            let mut n1 = RawInputFrame::new(t).into_raw();
            n1.events = n1_events;
            let _ = ctx.run_ui(n1, |ui| {
                if ui.button(label).clicked() {
                    clicks += 1;
                }
            });

            t += 1.0 / 60.0;
            let mut n2 = RawInputFrame::new(t).into_raw();
            n2.events = deferred2;
            let _ = ctx.run_ui(n2, |ui| {
                if ui.button(label).clicked() {
                    clicks += 1;
                }
            });
            assert_eq!(
                clicks, 1,
                "{label}: latched teleport click must fire exactly once"
            );
        }
    }

    /// Bevy system path: EguiInput teleport batch is staged across three updates.
    #[test]
    fn bevy_latch_system_stages_teleport_across_three_frames() {
        use bevy_egui::egui::RawInput;

        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<EguiClickLatch>()
            .add_systems(Update, latch_pointer_event_order);

        let pos = Pos2::new(64.0, 32.0);
        let entity = app
            .world_mut()
            .spawn(EguiInput(RawInput {
                events: vec![
                    Event::PointerMoved(pos),
                    press(PointerButton::Primary, pos),
                    release(PointerButton::Primary, pos),
                ],
                ..Default::default()
            }))
            .id();

        // Frame N: move only.
        app.update();
        {
            let input = app.world().get::<EguiInput>(entity).unwrap();
            assert!(
                input
                    .events
                    .iter()
                    .all(|e| matches!(e, Event::PointerMoved(_))),
                "frame N: move only"
            );
            assert!(
                !input.events.iter().any(is_primary_press),
                "press deferred off frame N"
            );
            let latch = app.world().resource::<EguiClickLatch>();
            assert_eq!(latch.deferred.len(), 2, "press + release queued");
        }

        app.world_mut()
            .get_mut::<EguiInput>(entity)
            .unwrap()
            .events
            .clear();

        // Frame N+1: press only.
        app.update();
        {
            let input = app.world().get::<EguiInput>(entity).unwrap();
            assert_eq!(input.events.len(), 1, "press injected");
            assert!(is_primary_press(&input.events[0]));
            let latch = app.world().resource::<EguiClickLatch>();
            assert_eq!(latch.deferred.len(), 1, "release still queued");
        }

        app.world_mut()
            .get_mut::<EguiInput>(entity)
            .unwrap()
            .events
            .clear();

        // Frame N+2: release.
        app.update();
        {
            let input = app.world().get::<EguiInput>(entity).unwrap();
            assert_eq!(input.events.len(), 1, "deferred release injected");
            assert!(is_primary_release(&input.events[0]));
            let latch = app.world().resource::<EguiClickLatch>();
            assert!(latch.deferred.is_empty(), "latch drained");
        }
    }

    /// M23 regression: press+release without a co-batched move still splits
    /// across exactly two frames (no extra latency).
    #[test]
    fn bevy_latch_system_defers_release_one_frame() {
        use bevy_egui::egui::RawInput;

        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<EguiClickLatch>()
            .add_systems(Update, latch_pointer_event_order);

        let pos = Pos2::new(64.0, 32.0);
        let entity = app
            .world_mut()
            .spawn(EguiInput(RawInput {
                events: vec![
                    press(PointerButton::Primary, pos),
                    release(PointerButton::Primary, pos),
                ],
                ..Default::default()
            }))
            .id();

        app.update();

        {
            let input = app.world().get::<EguiInput>(entity).unwrap();
            assert!(
                input.events.iter().any(is_primary_press),
                "press remains frame N"
            );
            assert!(
                !input.events.iter().any(is_primary_release),
                "release deferred off frame N"
            );
            let latch = app.world().resource::<EguiClickLatch>();
            assert_eq!(latch.deferred.len(), 1);
        }

        app.world_mut()
            .get_mut::<EguiInput>(entity)
            .unwrap()
            .events
            .clear();

        app.update();

        {
            let input = app.world().get::<EguiInput>(entity).unwrap();
            assert_eq!(input.events.len(), 1, "deferred release injected");
            assert!(is_primary_release(&input.events[0]));
            let latch = app.world().resource::<EguiClickLatch>();
            assert!(latch.deferred.is_empty(), "latch drained");
        }
    }

    /// Real Bevy message path: `CursorMoved` + `MouseButtonInput` Pressed +
    /// Released in one frame, converted the same way bevy_egui does (cached
    /// pointer position → `PointerMoved` / `PointerButton` on `EguiInput`),
    /// then the latch stages them. This is the automation teleport path —
    /// must fail if only M23 press/release split is present.
    #[test]
    fn bevy_messages_teleport_click_stages_through_latch() {
        use bevy::ecs::message::Messages;
        use bevy::input::mouse::MouseButtonInput;
        use bevy::input::{ButtonState, mouse::MouseButton};
        use bevy::math::Vec2;
        use bevy::window::CursorMoved;
        use bevy_egui::egui::RawInput;
        use bevy_egui::input::EguiContextPointerPosition;

        /// Minimal stand-in for bevy_egui's InitReading → FocusContext →
        /// WriteEguiEvents pipeline: CursorMoved updates the cached pointer
        /// position and emits PointerMoved; MouseButtonInput emits
        /// PointerButton at the *cached* pos (stale if move is missing).
        fn write_bevy_pointer_messages_to_egui_input(
            mut cursor_moved: MessageReader<CursorMoved>,
            mut mouse_buttons: MessageReader<MouseButtonInput>,
            mut contexts: Query<(&mut EguiInput, &mut EguiContextPointerPosition)>,
        ) {
            for (mut egui_input, mut ptr) in &mut contexts {
                for msg in cursor_moved.read() {
                    let pos = Pos2::new(msg.position.x, msg.position.y);
                    ptr.position = pos;
                    egui_input.events.push(Event::PointerMoved(pos));
                }
                for msg in mouse_buttons.read() {
                    let button = match msg.button {
                        MouseButton::Left => PointerButton::Primary,
                        MouseButton::Right => PointerButton::Secondary,
                        MouseButton::Middle => PointerButton::Middle,
                        MouseButton::Back => PointerButton::Extra1,
                        MouseButton::Forward => PointerButton::Extra2,
                        _ => continue,
                    };
                    let pressed = matches!(msg.state, ButtonState::Pressed);
                    egui_input.events.push(Event::PointerButton {
                        pos: ptr.position,
                        button,
                        pressed,
                        modifiers: Modifiers::NONE,
                    });
                }
            }
        }

        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<EguiClickLatch>()
            .init_resource::<Messages<CursorMoved>>()
            .init_resource::<Messages<MouseButtonInput>>()
            .add_systems(
                Update,
                (
                    write_bevy_pointer_messages_to_egui_input,
                    latch_pointer_event_order,
                )
                    .chain(),
            );

        let window = app.world_mut().spawn_empty().id();
        // Stale hover: pointer was far from the button before the teleport.
        let entity = app
            .world_mut()
            .spawn((
                EguiInput(RawInput::default()),
                EguiContextPointerPosition {
                    position: Pos2::new(1.0, 1.0),
                },
            ))
            .id();

        let btn_pos = Vec2::new(320.0, 240.0);
        // Single-frame automation batch — the live failure mode.
        app.world_mut().write_message(CursorMoved {
            window,
            position: btn_pos,
            delta: None,
        });
        app.world_mut().write_message(MouseButtonInput {
            window,
            button: MouseButton::Left,
            state: ButtonState::Pressed,
        });
        app.world_mut().write_message(MouseButtonInput {
            window,
            button: MouseButton::Left,
            state: ButtonState::Released,
        });

        // Frame N: messages → EguiInput → latch keeps move only.
        app.update();
        {
            let input = app.world().get::<EguiInput>(entity).unwrap();
            assert!(
                input
                    .events
                    .iter()
                    .any(|e| matches!(e, Event::PointerMoved(p) if p.x == btn_pos.x)),
                "CursorMoved must become PointerMoved on the button"
            );
            assert!(
                !input.events.iter().any(is_primary_press),
                "press must not share the move frame (M23b)"
            );
            assert!(
                !input.events.iter().any(is_primary_release),
                "release must not share the move frame"
            );
            let latch = app.world().resource::<EguiClickLatch>();
            assert!(
                latch.deferred.iter().any(|(_, e)| is_primary_press(e)),
                "press deferred"
            );
            assert!(
                latch.deferred.iter().any(|(_, e)| is_primary_release(e)),
                "release deferred"
            );
            // Cached pointer must be the button (bevy_egui press pos source).
            let ptr = app.world().get::<EguiContextPointerPosition>(entity).unwrap();
            assert_eq!(ptr.position, Pos2::new(btn_pos.x, btn_pos.y));
        }

        app.world_mut()
            .get_mut::<EguiInput>(entity)
            .unwrap()
            .events
            .clear();

        // Frame N+1: press.
        app.update();
        {
            let input = app.world().get::<EguiInput>(entity).unwrap();
            assert!(
                input.events.iter().any(is_primary_press),
                "press on frame N+1"
            );
            assert!(
                !input.events.iter().any(is_primary_release),
                "release still deferred"
            );
        }

        app.world_mut()
            .get_mut::<EguiInput>(entity)
            .unwrap()
            .events
            .clear();

        // Frame N+2: release — full click gesture has been delivered in order.
        app.update();
        {
            let input = app.world().get::<EguiInput>(entity).unwrap();
            assert!(
                input.events.iter().any(is_primary_release),
                "release on frame N+2"
            );
            let latch = app.world().resource::<EguiClickLatch>();
            assert!(latch.deferred.is_empty());
        }
    }
}
