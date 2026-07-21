//! M23: same-frame pointer press+release latch for bevy_egui.
//!
//! bevy_egui multipass (and browser trackpad / automation taps) can drop a
//! click whose press and release land in a single Bevy frame — the button
//! appears to do nothing. Dispatchers historically worked around this with a
//! 1 px drag that spans frames (~70% success). Real users on fast trackpads
//! hit the same defect on HOST / JOIN / START / LEAVE / env buttons.
//!
//! Fix: after bevy_egui has filled [`EguiInput`] for the frame, split any
//! same-frame Primary (and other) press+release pairs so the release is
//! delivered on the *next* frame. That matches the reliable 1 px-drag
//! pattern for every egui widget at once.
//!
//! Schedule: runs in [`PreUpdate`] after
//! [`bevy_egui::EguiPreUpdateSet::ProcessInput`] and before
//! [`bevy_egui::EguiPreUpdateSet::BeginPass`] / the multipass pass loop.

use bevy::prelude::*;
use bevy_egui::{
    egui::{Event, PointerButton},
    EguiInput, EguiPreUpdateSet,
};

/// Buffered pointer-button release events to inject on the next frame.
///
/// Keyed by egui context entity so multi-context setups stay isolated.
#[derive(Resource, Default)]
pub struct EguiClickLatch {
    /// `(context_entity, release_event)` queued for the next PreUpdate.
    deferred: Vec<(Entity, Event)>,
}

pub struct EguiClickLatchPlugin;

impl Plugin for EguiClickLatchPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<EguiClickLatch>().add_systems(
            PreUpdate,
            latch_same_frame_pointer_releases
                .after(EguiPreUpdateSet::ProcessInput)
                .before(EguiPreUpdateSet::BeginPass),
        );
    }
}

/// Inject deferred releases, then park any same-frame release that pairs
/// with a press of the same button on this context.
fn latch_same_frame_pointer_releases(
    mut latch: ResMut<EguiClickLatch>,
    mut contexts: Query<(Entity, &mut EguiInput)>,
) {
    let mut deferred = std::mem::take(&mut latch.deferred);

    for (entity, mut egui_input) in &mut contexts {
        // 1) Inject releases deferred from the previous frame first.
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

        // 2) Split same-frame press+release on this context.
        let mut held = Vec::new();
        split_same_frame_releases(&mut egui_input.events, &mut held);
        for ev in held {
            latch.deferred.push((entity, ev));
        }
    }

    // Contexts that disappeared this frame drop their deferred events.
    latch.deferred.extend(deferred);
}

/// Pure event-stream transform used by the system and unit tests.
///
/// Walks `events` in order. A **release** is deferred only if a **press** of
/// the same button already appeared earlier in this same list. That way:
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

    #[test]
    fn same_frame_press_release_defers_release_only() {
        let pos = Pos2::new(100.0, 200.0);
        let mut events = vec![
            Event::PointerMoved(pos),
            press(PointerButton::Primary, pos),
            release(PointerButton::Primary, pos),
        ];
        let mut deferred = Vec::new();
        split_same_frame_releases(&mut events, &mut deferred);

        assert_eq!(events.len(), 2, "move + press stay");
        assert!(matches!(
            &events[0],
            Event::PointerMoved(p) if *p == pos
        ));
        assert!(matches!(
            &events[1],
            Event::PointerButton {
                pressed: true,
                button: PointerButton::Primary,
                ..
            }
        ));
        assert_eq!(deferred.len(), 1);
        assert!(matches!(
            &deferred[0],
            Event::PointerButton {
                pressed: false,
                button: PointerButton::Primary,
                ..
            }
        ));
    }

    #[test]
    fn press_only_unchanged() {
        let pos = Pos2::ZERO;
        let mut events = vec![press(PointerButton::Primary, pos)];
        let mut deferred = Vec::new();
        split_same_frame_releases(&mut events, &mut deferred);
        assert_eq!(events.len(), 1);
        assert!(deferred.is_empty());
    }

    #[test]
    fn release_only_unchanged() {
        let pos = Pos2::ZERO;
        let mut events = vec![release(PointerButton::Primary, pos)];
        let mut deferred = Vec::new();
        split_same_frame_releases(&mut events, &mut deferred);
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
        split_same_frame_releases(&mut events, &mut deferred);
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
            events
                .iter()
                .any(|e| matches!(
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

    #[test]
    fn inject_then_split_roundtrip_two_frames() {
        // Frame N: same-frame pair → press kept, release deferred.
        let pos = Pos2::new(40.0, 60.0);
        let mut frame_n = vec![
            Event::PointerMoved(pos),
            press(PointerButton::Primary, pos),
            release(PointerButton::Primary, pos),
        ];
        let mut deferred = Vec::new();
        split_same_frame_releases(&mut frame_n, &mut deferred);
        assert_eq!(deferred.len(), 1);

        // Frame N+1: inject deferred first, then any new events (none).
        let mut frame_n1 = deferred;
        let mut deferred2 = Vec::new();
        split_same_frame_releases(&mut frame_n1, &mut deferred2);
        assert!(deferred2.is_empty(), "release-only must not re-defer");
        assert_eq!(frame_n1.len(), 1);
        assert!(matches!(
            &frame_n1[0],
            Event::PointerButton {
                pressed: false,
                button: PointerButton::Primary,
                ..
            }
        ));
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
        split_same_frame_releases(&mut events, &mut deferred);
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
        split_same_frame_releases(&mut events, &mut deferred);
        assert_eq!(deferred.len(), 1);
        assert!(matches!(
            &deferred[0],
            Event::PointerButton {
                pressed: false,
                ..
            }
        ));
        assert_eq!(events.len(), 2); // leading release + press
    }

    /// End-to-end against real egui: same-frame press+release with forced
    /// multipass still registers when the stream is latched across frames.
    ///
    /// Without the latch, a single-frame press+release *can* work in pure
    /// egui; the bevy_egui multipass path + browser delivery is what drops
    /// clicks in production. This test locks the latch contract: after
    /// split, frame-N press + frame-N+1 release yields exactly one click.
    #[test]
    fn latched_stream_produces_exactly_one_egui_click() {
        use bevy_egui::egui::{Context, Rect};

        let ctx = Context::default();
        let mut t = 0.0_f64;
        let mut btn_rect = Rect::NOTHING;

        let empty = |t: f64| RawInputFrame::new(t);
        // Warm widget rects.
        for _ in 0..4 {
            t += 1.0 / 60.0;
            let _ = ctx.run_ui(empty(t).into_raw(), |ui| {
                btn_rect = ui.button("HOST").rect;
            });
        }
        let pos = btn_rect.center();

        // Frame N: press only (after latch).
        t += 1.0 / 60.0;
        let mut press_input = empty(t).into_raw();
        press_input.events = vec![
            Event::PointerMoved(pos),
            press(PointerButton::Primary, pos),
        ];
        let mut clicks = 0_u32;
        let _ = ctx.run_ui(press_input, |ui| {
            if ui.button("HOST").clicked() {
                clicks += 1;
            }
            ui.ctx().request_discard("simulate multipass");
        });
        assert_eq!(clicks, 0, "press alone must not click");

        // Frame N+1: deferred release.
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

    /// Approach-agnostic contract: synthesizing a same-frame press+release
    /// pair through the latch yields a press stream then a release stream
    /// that egui turns into one click. Without [`split_same_frame_releases`]
    /// the production path (bevy_egui multipass + browser) drops clicks;
    /// this test fails if the latch is a no-op.
    #[test]
    fn same_frame_pair_after_latch_is_two_frames_and_one_click() {
        use bevy_egui::egui::{Context, Rect};

        let pos_placeholder = Pos2::new(20.0, 9.0);
        let mut events = vec![
            Event::PointerMoved(pos_placeholder),
            press(PointerButton::Primary, pos_placeholder),
            release(PointerButton::Primary, pos_placeholder),
        ];
        let mut deferred = Vec::new();
        split_same_frame_releases(&mut events, &mut deferred);

        // Latch must have split — if someone deletes the deferral this fails.
        assert!(
            !deferred.is_empty(),
            "same-frame press+release must defer the release (latch no-op?)"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                Event::PointerButton {
                    pressed: true,
                    ..
                }
            )),
            "press must remain on frame N"
        );
        assert!(
            !events.iter().any(|e| matches!(
                e,
                Event::PointerButton {
                    pressed: false,
                    ..
                }
            )),
            "release must not remain on frame N"
        );

        let ctx = Context::default();
        let mut t = 0.0_f64;
        let mut btn_rect = Rect::NOTHING;
        for _ in 0..4 {
            t += 1.0 / 60.0;
            let _ = ctx.run_ui(RawInputFrame::new(t).into_raw(), |ui| {
                btn_rect = ui.button("[ HOST A GAME ]").rect;
            });
        }
        let pos = btn_rect.center();
        // Rewrite events with the real button centre (placeholder was layout-unknown).
        let mut frame_n = vec![
            Event::PointerMoved(pos),
            press(PointerButton::Primary, pos),
            release(PointerButton::Primary, pos),
        ];
        let mut deferred = Vec::new();
        split_same_frame_releases(&mut frame_n, &mut deferred);

        t += 1.0 / 60.0;
        let mut n_input = RawInputFrame::new(t).into_raw();
        n_input.events = frame_n;
        let mut clicks = 0_u32;
        let _ = ctx.run_ui(n_input, |ui| {
            if ui.button("[ HOST A GAME ]").clicked() {
                clicks += 1;
            }
        });

        t += 1.0 / 60.0;
        let mut n1_input = RawInputFrame::new(t).into_raw();
        // Re-stamp deferred release to the real pos (same as production: pos is
        // embedded in the event at capture time).
        n1_input.events = deferred
            .into_iter()
            .map(|ev| match ev {
                Event::PointerButton {
                    button,
                    pressed,
                    modifiers,
                    ..
                } => Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    modifiers,
                },
                other => other,
            })
            .collect();
        let _ = ctx.run_ui(n1_input, |ui| {
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

    /// Menu / lobby / death-screen button labels that must accept a single
    /// honest click (same-frame press+release). One latched click each → one
    /// intent. Approach-agnostic contract: if the latch is removed this test
    /// still documents which labels the dispatcher exercises.
    #[test]
    fn each_menu_button_label_one_latched_click() {
        use bevy_egui::egui::{Context, Rect};

        // Labels painted by lobby_ui that fire UiIntent (or clipboard) on click.
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
                let _ = ctx.run_ui(RawInputFrame::new(t).into_raw(), |ui| {
                    btn_rect = ui.button(label).rect;
                });
            }
            let pos = btn_rect.center();

            // Same-frame pair → latch split.
            let mut pair = vec![
                Event::PointerMoved(pos),
                press(PointerButton::Primary, pos),
                release(PointerButton::Primary, pos),
            ];
            let mut deferred = Vec::new();
            split_same_frame_releases(&mut pair, &mut deferred);
            assert_eq!(
                deferred.len(),
                1,
                "{label}: same-frame pair must defer release"
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
            t += 1.0 / 60.0;
            let mut n1 = RawInputFrame::new(t).into_raw();
            n1.events = deferred;
            let _ = ctx.run_ui(n1, |ui| {
                if ui.button(label).clicked() {
                    clicks += 1;
                }
            });
            assert_eq!(
                clicks, 1,
                "{label}: latched press+release must fire exactly once"
            );
        }
    }

    /// Bevy system path: EguiInput on an entity is split across two updates.
    #[test]
    fn bevy_latch_system_defers_release_one_frame() {
        use bevy_egui::egui::RawInput;

        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<EguiClickLatch>()
            .add_systems(Update, latch_same_frame_pointer_releases);

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

        app.update();

        {
            let input = app.world().get::<EguiInput>(entity).unwrap();
            assert!(
                input.events.iter().any(|e| matches!(
                    e,
                    Event::PointerButton {
                        pressed: true,
                        ..
                    }
                )),
                "press remains frame N"
            );
            assert!(
                !input.events.iter().any(|e| matches!(
                    e,
                    Event::PointerButton {
                        pressed: false,
                        ..
                    }
                )),
                "release deferred off frame N"
            );
            let latch = app.world().resource::<EguiClickLatch>();
            assert_eq!(latch.deferred.len(), 1);
        }

        // Clear events as write_egui_input / take() would between frames.
        app.world_mut()
            .get_mut::<EguiInput>(entity)
            .unwrap()
            .events
            .clear();

        app.update();

        {
            let input = app.world().get::<EguiInput>(entity).unwrap();
            assert_eq!(input.events.len(), 1, "deferred release injected");
            assert!(matches!(
                &input.events[0],
                Event::PointerButton {
                    pressed: false,
                    button: PointerButton::Primary,
                    ..
                }
            ));
            let latch = app.world().resource::<EguiClickLatch>();
            assert!(latch.deferred.is_empty(), "latch drained");
        }
    }
}
