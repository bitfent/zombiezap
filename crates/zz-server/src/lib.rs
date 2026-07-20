//! ZombieZap game server library: the axum app, connection plumbing, and the
//! room simulation. The binary in main.rs is a thin bind-and-serve wrapper;
//! integration tests start the same `app()` on an ephemeral port.

pub mod conn;
pub mod room;

use axum::extract::{State, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use tokio::sync::mpsc;

#[derive(Clone)]
struct AppState {
    room: mpsc::Sender<room::RoomCmd>,
}

pub fn app() -> axum::Router {
    let (walls, arena_half) = room::test_map();
    let seed = std::env::var("MAP_SEED").unwrap_or_else(|_| "m2-test".into());
    let room = room::Room::spawn(seed, walls, arena_half);
    axum::Router::new()
        .route("/healthz", get(healthz))
        .route("/ws", get(ws_upgrade))
        .with_state(AppState { room })
}

async fn healthz() -> impl IntoResponse {
    format!("{{\"ok\":true,\"protocol\":{}}}", zz_core::PROTOCOL_VERSION)
}

async fn ws_upgrade(State(state): State<AppState>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |socket| conn::handle_socket(socket, state.room))
}
