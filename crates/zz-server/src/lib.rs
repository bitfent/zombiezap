//! ZombieZap game server library: the axum app, connection plumbing, the
//! lobby manager, and the room simulation. The binary in main.rs is a thin
//! bind-and-serve wrapper; integration tests start the same `app()` on an
//! ephemeral port. Everything lives in memory — no database by design.

pub mod conn;
pub mod lobby;
pub mod room;

use axum::extract::{State, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use tokio::sync::mpsc;

#[derive(Clone)]
struct AppState {
    lobby: mpsc::Sender<lobby::LobbyCmd>,
}

pub fn app() -> axum::Router {
    let lobby = lobby::LobbyManager::spawn();
    axum::Router::new()
        .route("/healthz", get(healthz))
        .route("/ws", get(ws_upgrade))
        .with_state(AppState { lobby })
}

async fn healthz() -> impl IntoResponse {
    format!("{{\"ok\":true,\"protocol\":{}}}", zz_core::PROTOCOL_VERSION)
}

async fn ws_upgrade(State(state): State<AppState>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |socket| conn::handle_socket(socket, state.lobby))
}
