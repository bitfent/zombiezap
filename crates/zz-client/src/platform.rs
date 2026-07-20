//! Platform seam: native vs wasm32 differences for server URL, invite join
//! codes, and player name persistence. game.rs / lobby_ui call these only —
//! no `cfg` elsewhere for these concerns.

/// WebSocket URL for the game server.
///
/// * Native: `ZZ_SERVER` env, else `ws://127.0.0.1:8080/ws`.
/// * Wasm: same origin as the page — `wss://` when served over https, `ws://`
///   when http (local Trunk dev). Path is always `/ws`.
pub fn server_url() -> String {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::env::var("ZZ_SERVER").unwrap_or_else(|_| "ws://127.0.0.1:8080/ws".into())
    }
    #[cfg(target_arch = "wasm32")]
    {
        wasm_server_url()
    }
}

/// Optional lobby code from an invite link or env.
///
/// * Native: `ZZ_JOIN` env.
/// * Wasm: `?join=CODE` query param on `window.location`.
pub fn join_code_from_url() -> Option<String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::env::var("ZZ_JOIN").ok().and_then(normalize_join)
    }
    #[cfg(target_arch = "wasm32")]
    {
        wasm_join_code()
    }
}

/// Initial callsign for the lobby name field.
///
/// * Native: `ZZ_NAME` env, else `"survivor"`.
/// * Wasm: `localStorage["zz_name"]`, else `"survivor"`.
pub fn initial_name() -> String {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::env::var("ZZ_NAME").unwrap_or_else(|_| "survivor".into())
    }
    #[cfg(target_arch = "wasm32")]
    {
        wasm_initial_name()
    }
}

/// Persist the callsign so the next browser session pre-fills it.
/// Native is a no-op (use `ZZ_NAME` if you want a fixed name).
pub fn persist_name(name: &str) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = name;
    }
    #[cfg(target_arch = "wasm32")]
    {
        wasm_persist_name(name);
    }
}

fn normalize_join(raw: String) -> Option<String> {
    let code = raw.trim().to_uppercase();
    if code.is_empty() { None } else { Some(code) }
}

// ── wasm32 helpers ─────────────────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
fn wasm_server_url() -> String {
    let window = match web_sys::window() {
        Some(w) => w,
        None => return "ws://127.0.0.1:8080/ws".into(),
    };
    let location = window.location();
    let protocol = location.protocol().unwrap_or_else(|_| "http:".into());
    let host = location.host().unwrap_or_else(|_| "127.0.0.1:8080".into());
    let ws_scheme = if protocol == "https:" { "wss" } else { "ws" };
    format!("{ws_scheme}://{host}/ws")
}

#[cfg(target_arch = "wasm32")]
fn wasm_join_code() -> Option<String> {
    let search = web_sys::window()
        .and_then(|w| w.location().search().ok())
        .unwrap_or_default();
    // search is like "?join=ABCDE&foo=1" or ""
    let query = search.strip_prefix('?').unwrap_or(search.as_str());
    for pair in query.split('&') {
        let mut parts = pair.splitn(2, '=');
        let key = parts.next().unwrap_or("");
        let val = parts.next().unwrap_or("");
        if key == "join" {
            // URL-decode minimal: `+` → space, `%XX` not required for lobby codes
            // (codes are A–Z0–9), but strip percent-encoding if present is overkill.
            let decoded = val.replace('+', " ");
            return normalize_join(decoded);
        }
    }
    None
}

#[cfg(target_arch = "wasm32")]
fn wasm_initial_name() -> String {
    storage()
        .and_then(|s| s.get_item("zz_name").ok().flatten())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "survivor".into())
}

#[cfg(target_arch = "wasm32")]
fn wasm_persist_name(name: &str) {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return;
    }
    if let Some(s) = storage() {
        let _ = s.set_item("zz_name", trimmed);
    }
}

#[cfg(target_arch = "wasm32")]
fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}
