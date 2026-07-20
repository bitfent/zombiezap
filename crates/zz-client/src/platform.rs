//! Platform seam: native vs wasm32 differences for server URL, invite join
//! codes, player name persistence, touch-mode detection, and the mobile
//! soft-keyboard HTML overlay bridge. game.rs / lobby_ui / touch call these
//! only — no `cfg` elsewhere for these concerns.

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

/// Whether the client should run in touch-control mode.
///
/// * Wasm: `?touch=1` forces on, `?touch=0` forces off; otherwise
///   `navigator.maxTouchPoints > 0`.
/// * Native: `ZZ_TOUCH=1` forces on; otherwise false (mouse/keyboard).
///
/// Call once at startup and cache in a resource — query string / env do not
/// change mid-session.
pub fn is_touch_mode() -> bool {
    if let Some(forced) = touch_override() {
        return forced;
    }
    #[cfg(target_arch = "wasm32")]
    {
        wasm_max_touch_points() > 0
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        false
    }
}

/// Explicit touch override: `Some(true)` / `Some(false)` / `None` (autodetect).
fn touch_override() -> Option<bool> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        match std::env::var("ZZ_TOUCH").ok().as_deref() {
            Some("1") | Some("true") | Some("TRUE") => Some(true),
            Some("0") | Some("false") | Some("FALSE") => Some(false),
            _ => None,
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        wasm_query_param("touch").and_then(|v| match v.as_str() {
            "1" | "true" => Some(true),
            "0" | "false" => Some(false),
            _ => None,
        })
    }
}

/// Mark `<body class="touch">` so CSS landscape-hint / overlay rules apply.
/// No-op on native.
pub fn apply_touch_body_class(enabled: bool) {
    #[cfg(target_arch = "wasm32")]
    {
        wasm_set_body_class("touch", enabled);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = enabled;
    }
}

/// Show/hide the HTML name + lobby-code `<input>` overlays used on touch
/// devices (egui text fields do not summon mobile soft keyboards).
///
/// * `name` — menu name field
/// * `code` — menu join-code field
///
/// No-op on native (touch soft-keyboard bridge is wasm-only).
pub fn set_touch_text_overlays(name: bool, code: bool) {
    #[cfg(target_arch = "wasm32")]
    {
        // Inputs use `block`; the shell is a column flex.
        wasm_set_display("zz-name-input", name, "block");
        wasm_set_display("zz-code-input", code, "block");
        wasm_set_display("zz-touch-fields", name || code, "flex");
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (name, code);
    }
}

/// Read the HTML name overlay value (wasm). Returns `None` when the overlay
/// is hidden or missing.
pub fn html_name_value() -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        wasm_input_value("zz-name-input")
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        None
    }
}

/// Read the HTML join-code overlay value (wasm).
pub fn html_code_value() -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        wasm_input_value("zz-code-input")
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        None
    }
}

/// Seed the HTML name overlay from the Rust-side callsign (once at startup /
/// when LobbyView name changes from server path).
pub fn set_html_name_value(name: &str) {
    #[cfg(target_arch = "wasm32")]
    {
        wasm_set_input_value("zz-name-input", name);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = name;
    }
}

/// Seed the HTML join-code overlay.
pub fn set_html_code_value(code: &str) {
    #[cfg(target_arch = "wasm32")]
    {
        wasm_set_input_value("zz-code-input", code);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = code;
    }
}

/// Browser autoplay policy: resume suspended `AudioContext`s on first user
/// gesture. Idempotent; no-op on native (desktop audio starts free).
///
/// Implementation: `web/index.html` installs a one-shot pointer/keydown
/// listener that walks `window.__zzAudioContexts` and calls `.resume()`.
/// Bevy/rodio contexts register themselves when created; we also poke any
/// live `AudioContext` exposed on the page. Calling this from Rust after
/// the first pointer event is a second safety net (the HTML listener is
/// the primary path because it runs inside the user-gesture stack).
pub fn unlock_audio() {
    #[cfg(target_arch = "wasm32")]
    {
        wasm_unlock_audio();
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
    wasm_query_param("join").and_then(normalize_join)
}

#[cfg(target_arch = "wasm32")]
fn wasm_query_param(key: &str) -> Option<String> {
    let search = web_sys::window()
        .and_then(|w| w.location().search().ok())
        .unwrap_or_default();
    let query = search.strip_prefix('?').unwrap_or(search.as_str());
    for pair in query.split('&') {
        let mut parts = pair.splitn(2, '=');
        let k = parts.next().unwrap_or("");
        let val = parts.next().unwrap_or("");
        if k == key {
            return Some(val.replace('+', " "));
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

#[cfg(target_arch = "wasm32")]
fn wasm_max_touch_points() -> i32 {
    web_sys::window()
        .map(|w| w.navigator().max_touch_points())
        .unwrap_or(0)
}

#[cfg(target_arch = "wasm32")]
fn wasm_set_body_class(class: &str, enabled: bool) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(document) = window.document() else {
        return;
    };
    let Some(body) = document.body() else {
        return;
    };
    let list = body.class_list();
    if enabled {
        let _ = list.add_1(class);
    } else {
        let _ = list.remove_1(class);
    }
}

#[cfg(target_arch = "wasm32")]
fn wasm_set_display(id: &str, visible: bool, when_visible: &str) {
    use wasm_bindgen::JsCast;
    let Some(el) = document_element(id) else {
        return;
    };
    let Ok(html) = el.dyn_into::<web_sys::HtmlElement>() else {
        return;
    };
    let style = html.style();
    let _ = style.set_property("display", if visible { when_visible } else { "none" });
}

#[cfg(target_arch = "wasm32")]
fn wasm_input_value(id: &str) -> Option<String> {
    use wasm_bindgen::JsCast;
    let el = document_element(id)?;
    let input: web_sys::HtmlInputElement = el.dyn_into().ok()?;
    // Only report when the overlay is actually shown (display != none).
    let style = input.style();
    if let Ok(d) = style.get_property_value("display") {
        if d == "none" {
            return None;
        }
    }
    Some(input.value())
}

#[cfg(target_arch = "wasm32")]
fn wasm_set_input_value(id: &str, value: &str) {
    use wasm_bindgen::JsCast;
    let Some(el) = document_element(id) else {
        return;
    };
    let Ok(input) = el.dyn_into::<web_sys::HtmlInputElement>() else {
        return;
    };
    input.set_value(value);
}

#[cfg(target_arch = "wasm32")]
fn document_element(id: &str) -> Option<web_sys::Element> {
    web_sys::window()?.document()?.get_element_by_id(id)
}

#[cfg(target_arch = "wasm32")]
fn wasm_unlock_audio() {
    // Invoke the page-level unlock helper installed by web/index.html.
    // Using Reflect keeps the dep graph free of js-sys Function bindings.
    use wasm_bindgen::JsCast;
    let Some(window) = web_sys::window() else {
        return;
    };
    let unlock = js_sys::Reflect::get(&window, &wasm_bindgen::JsValue::from_str("__zzUnlockAudio"))
        .ok()
        .filter(|v| v.is_function());
    if let Some(f) = unlock {
        if let Ok(func) = f.dyn_into::<js_sys::Function>() {
            let _ = func.call0(&window);
        }
    }
}
