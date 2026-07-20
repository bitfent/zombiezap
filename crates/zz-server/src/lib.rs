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
use tower_http::services::ServeDir;

#[derive(Clone)]
struct AppState {
    lobby: mpsc::Sender<lobby::LobbyCmd>,
}

/// Build the axum router: `/healthz` + `/ws` take precedence; everything else
/// falls through to the Trunk browser build under `ZZ_WEB_DIST` (default
/// `web/dist`). Missing dist does not crash — ServeDir returns 404s.
pub fn app() -> axum::Router {
    let lobby = lobby::LobbyManager::spawn();
    let dist = std::env::var("ZZ_WEB_DIST").unwrap_or_else(|_| "web/dist".into());
    if std::path::Path::new(&dist).is_dir() {
        println!("zz-server: web build found at {dist}");
    } else {
        println!("zz-server: web build not found at {dist} (static files will 404)");
    }
    // append_index_html_on_directories defaults to true → `/` serves index.html.
    // precompressed_* serve Trunk/CI `.br`/`.gz` siblings when the client accepts them.
    let serve_dir = ServeDir::new(&dist).precompressed_br().precompressed_gzip();

    axum::Router::new()
        .route("/healthz", get(healthz))
        .route("/ws", get(ws_upgrade))
        .fallback_service(serve_dir)
        .with_state(AppState { lobby })
}

async fn healthz() -> impl IntoResponse {
    format!("{{\"ok\":true,\"protocol\":{}}}", zz_core::PROTOCOL_VERSION)
}

async fn ws_upgrade(State(state): State<AppState>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |socket| conn::handle_socket(socket, state.lobby))
}
