//! Menu + lobby overlay (egui): name entry, create/join by code, roster,
//! env picker, invite link, start. Paints seams::LobbyView verbatim and
//! pushes seams::UiIntent — no networking, no state transitions here.

use bevy::prelude::*;
use bevy_egui::{
    EguiContexts, EguiPlugin, EguiPrimaryContextPass,
    egui::{self, Align2, Color32, FontId, Frame, Margin, RichText, Stroke, Theme},
};
use zz_core::types::EnvKind;

use crate::game::Session;
use crate::seams::{LobbyView, UiIntent, UiQueue};

/// Cyan accent for interactive controls (#2ee6d6).
const ACCENT: Color32 = Color32::from_rgb(0x2e, 0xe6, 0xd6);
/// Near-black card fill.
const PANEL_BG: Color32 = Color32::from_rgb(0x0c, 0x0e, 0x12);
/// Subtle border on the card.
const PANEL_BORDER: Color32 = Color32::from_rgb(0x22, 0x28, 0x30);
/// Body text.
const TEXT: Color32 = Color32::from_rgb(0xdc, 0xe0, 0xe6);
/// Muted secondary text.
const MUTED: Color32 = Color32::from_rgb(0x7a, 0x82, 0x8e);
/// Status line (non-error).
const AMBER: Color32 = Color32::from_rgb(0xf0, 0xb4, 0x3c);
/// Status line when it looks like an error.
const ERROR_RED: Color32 = Color32::from_rgb(0xf0, 0x55, 0x55);
/// Fixed-ish card width.
const CARD_WIDTH: f32 = 420.0;
/// Max players shown in the lobby roster (open slots fill the rest).
const MAX_SLOTS: usize = 5;

const ENV_OPTIONS: [(&str, EnvKind); 4] = [
    ("URBAN", EnvKind::Urban),
    ("MOUNTAIN", EnvKind::MountainTown),
    ("DESERT", EnvKind::DesertTown),
    ("SEA", EnvKind::SeaTown),
];

pub struct LobbyUiPlugin;

impl Plugin for LobbyUiPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<EguiPlugin>() {
            app.add_plugins(EguiPlugin::default());
        }
        app.init_resource::<LobbyDraft>()
            .add_systems(EguiPrimaryContextPass, paint_lobby_ui);
    }
}

/// Local draft fields that are not part of LobbyView (join code input).
#[derive(Resource, Default)]
struct LobbyDraft {
    join_code: String,
}

fn paint_lobby_ui(
    mut contexts: EguiContexts,
    session: Res<Session>,
    lobby: Res<LobbyView>,
    mut queue: ResMut<UiQueue>,
    mut draft: ResMut<LobbyDraft>,
) -> Result {
    // Playing / Ended: HUD owns the screen — paint nothing.
    match *session {
        Session::Playing { .. } | Session::Ended { .. } => return Ok(()),
        Session::Boot | Session::Connecting | Session::Menu | Session::InLobby => {}
    }

    let ctx = contexts.ctx_mut()?;
    apply_dark_style(ctx);

    egui::Area::new(egui::Id::new("zz_lobby_card"))
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .interactable(true)
        .show(ctx, |ui| {
            Frame::NONE
                .fill(PANEL_BG)
                .stroke(Stroke::new(1.0, PANEL_BORDER))
                .corner_radius(10.0)
                .inner_margin(Margin::symmetric(28, 24))
                .show(ui, |ui| {
                    ui.set_width(CARD_WIDTH);
                    ui.spacing_mut().item_spacing.y = 12.0;
                    ui.spacing_mut().button_padding = egui::vec2(14.0, 8.0);

                    match *session {
                        Session::Boot | Session::Connecting => paint_connecting(ui),
                        Session::Menu => paint_menu(ui, &lobby, &mut draft, &mut queue),
                        Session::InLobby => paint_in_lobby(ui, &lobby, &mut queue),
                        Session::Playing { .. } | Session::Ended { .. } => {}
                    }
                });
        });

    Ok(())
}

fn apply_dark_style(ctx: &egui::Context) {
    ctx.set_theme(Theme::Dark);
    ctx.style_mut_of(Theme::Dark, |style| {
        style.override_font_id = Some(FontId::monospace(14.0));
        style.visuals.override_text_color = Some(TEXT);
        style.visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, ACCENT);
        style.visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, ACCENT);
        style.visuals.widgets.active.fg_stroke = Stroke::new(1.0, ACCENT);
        style.visuals.selection.bg_fill = Color32::from_rgba_unmultiplied(0x2e, 0xe6, 0xd6, 48);
        style.visuals.selection.stroke = Stroke::new(1.0, ACCENT);
        style.visuals.widgets.inactive.bg_fill = Color32::from_rgb(0x16, 0x1a, 0x20);
        style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(0x1e, 0x26, 0x2e);
        style.visuals.widgets.active.bg_fill = Color32::from_rgb(0x14, 0x3a, 0x38);
        style.visuals.extreme_bg_color = Color32::from_rgb(0x08, 0x0a, 0x0c);
    });
}

fn paint_connecting(ui: &mut egui::Ui) {
    ui.vertical_centered(|ui| {
        title(ui);
        ui.add_space(8.0);
        ui.label(
            RichText::new("connecting…")
                .color(MUTED)
                .monospace()
                .size(15.0),
        );
    });
}

fn paint_menu(ui: &mut egui::Ui, lobby: &LobbyView, draft: &mut LobbyDraft, queue: &mut UiQueue) {
    ui.vertical_centered(|ui| {
        title(ui);
    });
    ui.add_space(4.0);

    ui.label(
        RichText::new("CALLSIGN")
            .color(MUTED)
            .size(11.0)
            .monospace(),
    );
    let mut name = lobby.name.clone();
    let name_edit = ui.add(
        egui::TextEdit::singleline(&mut name)
            .desired_width(CARD_WIDTH)
            .hint_text("survivor")
            .font(FontId::monospace(15.0)),
    );
    if name_edit.changed() {
        queue.0.push_back(UiIntent::SetName(name));
    }

    ui.add_space(6.0);

    if accent_button(ui, "[ HOST A GAME ]", true).clicked() {
        queue.0.push_back(UiIntent::CreateLobby(EnvKind::Urban));
    }

    ui.add_space(4.0);
    ui.vertical_centered(|ui| {
        ui.label(
            RichText::new("— or join a friend —")
                .color(MUTED)
                .size(12.0)
                .monospace(),
        );
    });

    ui.label(RichText::new("CODE").color(MUTED).size(11.0).monospace());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        let code_edit = ui.add(
            egui::TextEdit::singleline(&mut draft.join_code)
                .desired_width(140.0)
                .char_limit(5)
                .hint_text("XXXXX")
                .font(FontId::monospace(18.0)),
        );
        if code_edit.changed() {
            draft.join_code = normalize_code(&draft.join_code);
        }
        if accent_button(ui, "[ JOIN ]", true).clicked() {
            queue
                .0
                .push_back(UiIntent::JoinLobby(draft.join_code.clone()));
        }
    });

    status_line(ui, &lobby.status);
}

fn paint_in_lobby(ui: &mut egui::Ui, lobby: &LobbyView, queue: &mut UiQueue) {
    ui.vertical_centered(|ui| {
        title(ui);
        ui.add_space(4.0);
        ui.label(
            RichText::new("LOBBY CODE")
                .color(MUTED)
                .size(11.0)
                .monospace(),
        );
        // Huge selectable code for easy copy-select.
        ui.add(
            egui::Label::new(
                RichText::new(if lobby.code.is_empty() {
                    "·····"
                } else {
                    lobby.code.as_str()
                })
                .color(ACCENT)
                .monospace()
                .size(36.0)
                .strong(),
            )
            .selectable(true),
        );
    });

    if let Some(url) = lobby.invite_url.as_ref() {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::Label::new(RichText::new(url).color(MUTED).monospace().size(11.0))
                    .selectable(true)
                    .wrap(),
            );
        });
        if accent_button(ui, "[ COPY LINK ]", true).clicked() {
            // egui → bevy_egui process_output → EguiClipboard (native + wasm).
            ui.ctx().copy_text(url.clone());
        }
    }

    ui.add_space(6.0);
    ui.label(RichText::new("ROSTER").color(MUTED).size(11.0).monospace());
    Frame::NONE
        .fill(Color32::from_rgb(0x10, 0x12, 0x16))
        .inner_margin(Margin::symmetric(12, 8))
        .corner_radius(6.0)
        .show(ui, |ui| {
            ui.set_width(CARD_WIDTH - 8.0);
            for i in 0..MAX_SLOTS {
                if let Some((name, is_host, is_me)) = lobby.players.get(i) {
                    let mut line = name.clone();
                    if *is_host {
                        line.push_str("  ★");
                    }
                    if *is_me {
                        line.push_str("  (you)");
                    }
                    ui.label(RichText::new(line).color(TEXT).monospace().size(14.0));
                } else {
                    ui.label(
                        RichText::new("— open —")
                            .color(MUTED)
                            .monospace()
                            .size(14.0),
                    );
                }
            }
        });

    ui.add_space(4.0);
    ui.label(
        RichText::new("ENVIRONMENT")
            .color(MUTED)
            .size(11.0)
            .monospace(),
    );
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        for (label, env) in ENV_OPTIONS {
            let selected = lobby.env == Some(env);
            let enabled = lobby.is_host;
            let text = if selected {
                RichText::new(label).color(ACCENT).monospace().strong()
            } else {
                RichText::new(label)
                    .color(if enabled { TEXT } else { MUTED })
                    .monospace()
            };
            let resp = ui.add_enabled_ui(enabled, |ui| ui.selectable_label(selected, text));
            if resp.inner.clicked() && lobby.env != Some(env) {
                queue.0.push_back(UiIntent::SetEnv(env));
            }
        }
    });
    if let Some(env) = lobby.env {
        ui.label(
            RichText::new(format!("current: {}", env_label(env)))
                .color(MUTED)
                .size(11.0)
                .monospace(),
        );
    }

    ui.add_space(8.0);
    if lobby.is_host {
        if accent_button(ui, "[ START ]", true)
            .on_hover_text("begin the match")
            .clicked()
        {
            queue.0.push_back(UiIntent::StartGame);
        }
    } else {
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new("waiting for host…")
                    .color(MUTED)
                    .monospace()
                    .size(14.0),
            );
        });
    }

    ui.add_space(4.0);
    if ui
        .add(
            egui::Button::new(
                RichText::new("[ LEAVE ]")
                    .color(MUTED)
                    .monospace()
                    .size(12.0),
            )
            .frame(false),
        )
        .clicked()
    {
        queue.0.push_back(UiIntent::LeaveLobby);
    }

    status_line(ui, &lobby.status);
}

fn env_label(env: EnvKind) -> &'static str {
    match env {
        EnvKind::Urban => "URBAN",
        EnvKind::MountainTown => "MOUNTAIN",
        EnvKind::DesertTown => "DESERT",
        EnvKind::SeaTown => "SEA",
    }
}

fn title(ui: &mut egui::Ui) {
    ui.label(
        RichText::new("Z  O  M  B  I  E  Z  A  P")
            .color(TEXT)
            .monospace()
            .size(22.0)
            .strong(),
    );
}

fn accent_button(ui: &mut egui::Ui, label: &str, wide: bool) -> egui::Response {
    let text = RichText::new(label)
        .color(ACCENT)
        .monospace()
        .size(15.0)
        .strong();
    let mut btn = egui::Button::new(text)
        .fill(Color32::from_rgb(0x12, 0x1c, 0x1c))
        .stroke(Stroke::new(1.0, ACCENT))
        .corner_radius(4.0);
    if wide {
        btn = btn.min_size(egui::vec2(ui.available_width(), 36.0));
    }
    ui.add(btn)
}

fn status_line(ui: &mut egui::Ui, status: &str) {
    if status.is_empty() {
        return;
    }
    ui.add_space(4.0);
    let color = if looks_like_error(status) {
        ERROR_RED
    } else {
        AMBER
    };
    ui.label(RichText::new(status).color(color).monospace().size(12.0));
}

fn looks_like_error(status: &str) -> bool {
    let s = status.to_ascii_lowercase();
    s.contains("error")
        || s.contains("fail")
        || s.contains("disconnect")
        || s.contains("invalid")
        || s.contains("denied")
        || s.contains("full")
        || s.contains("not found")
}

fn normalize_code(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_uppercase())
        .take(5)
        .collect()
}
