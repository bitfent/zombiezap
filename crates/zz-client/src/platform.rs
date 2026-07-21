//! Platform seam: native vs wasm32 differences for server URL, invite join
//! codes, player name persistence, touch-mode detection, the mobile
//! soft-keyboard HTML overlay bridge, and the M19 HTML-first boot screen.
//! game.rs / lobby_ui / touch call these only — no `cfg` elsewhere for these
//! concerns.

// ── Boot wall-clock (wasm-safe) ─────────────────────────────────────────────
//
// CRITICAL: never use `std::time::Instant` on `wasm32-unknown-unknown`.
// Instant::now panics with "time not implemented on this platform". With the
// ship profile (`panic = "abort"` + fat LTO) that makes everything after the
// call *unreachable*, so rustc dead-code-eliminates the entire Bevy app
// (M19 → M19b: ship wasm collapsed ~23 MB → ~5.8 MB with an empty engine).

/// Link/canary string kept live whenever [`crate::run`] is reachable.
/// `scripts/ship-web.sh` greps the ship wasm for this exact marker.
pub const BOOT_ENTRY_CANARY: &str = "zz-boot-entry-v1";

/// Monotonic-ish stamp for boot phase telemetry (native Instant / wasm Date).
#[derive(Clone, Copy, Debug)]
pub struct BootStamp {
    #[cfg(not(target_arch = "wasm32"))]
    t0: std::time::Instant,
    #[cfg(target_arch = "wasm32")]
    t0_ms: f64,
}

impl BootStamp {
    #[inline]
    pub fn now() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        {
            Self {
                t0: std::time::Instant::now(),
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            Self {
                t0_ms: wasm_boot_now_ms(),
            }
        }
    }

    /// Milliseconds since this stamp (saturating, clamped to u32::MAX).
    #[inline]
    pub fn elapsed_ms(self) -> u32 {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.t0.elapsed().as_millis().min(u128::from(u32::MAX)) as u32
        }
        #[cfg(target_arch = "wasm32")]
        {
            let dt = wasm_boot_now_ms() - self.t0_ms;
            if dt <= 0.0 {
                0
            } else if dt >= f64::from(u32::MAX) {
                u32::MAX
            } else {
                dt as u32
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn wasm_boot_now_ms() -> f64 {
    // Date.now is available without extra web-sys features; resolution is
    // enough for boot phase lines (ms). Prefer it over Instant (unsupported).
    js_sys::Date::now()
}

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
///
/// After M19 handoff on desktop the whole `#zz-start` is hidden; on touch the
/// start chrome is stripped and these toggles control the floating fields.
pub fn set_touch_text_overlays(name: bool, code: bool) {
    #[cfg(target_arch = "wasm32")]
    {
        // Inputs use `block`; the shell is a column flex.
        wasm_set_display("zz-name-input", name, "block");
        wasm_set_display("zz-code-input", code, "block");
        wasm_set_display("zz-touch-fields", name || code, "flex");
        // When leaving the menu entirely, hide the start shell if it was left
        // in post-handoff-touch mode.
        if !name && !code {
            wasm_boot_hide_all();
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (name, code);
    }
}

// ── M19 HTML-first boot screen ─────────────────────────────────────────────

/// Live CALLSIGN from the HTML start screen (wasm). Prefer over LobbyView
/// until handoff. Always readable while `#zz-name-input` exists — even when
/// `display:none` would hide it for the old touch poll path we still want
/// the value at handoff time, so this does not require display != none.
pub fn boot_html_name() -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        wasm_input_value_raw("zz-name-input")
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        None
    }
}

/// Live LOBBY CODE from the HTML start screen (wasm).
pub fn boot_html_code() -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        wasm_input_value_raw("zz-code-input")
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        None
    }
}

/// Peek a queued HOST/JOIN from `window.__zzBoot` without consuming.
pub fn boot_peek_action() -> Option<crate::seams::QueuedBootAction> {
    #[cfg(target_arch = "wasm32")]
    {
        wasm_boot_action(false)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        None
    }
}

/// Consume a queued HOST/JOIN from `window.__zzBoot` (one-shot).
pub fn boot_take_action() -> Option<crate::seams::QueuedBootAction> {
    #[cfg(target_arch = "wasm32")]
    {
        wasm_boot_action(true)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        None
    }
}

/// Tell JS the first Update frame finished (engine ready for handoff).
pub fn boot_signal_engine_ready() {
    #[cfg(target_arch = "wasm32")]
    {
        wasm_boot_call0("setEngineReady");
    }
}

/// Dismiss the full-screen HTML start chrome after handoff.
pub fn boot_dismiss_start(is_touch: bool) {
    #[cfg(target_arch = "wasm32")]
    {
        wasm_boot_handoff(is_touch);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = is_touch;
    }
}

/// Record a phase duration (ms) into `window.__zzBoot.phases` and log a line.
pub fn boot_record_phase(name: &str, ms: u32) {
    #[cfg(target_arch = "wasm32")]
    {
        wasm_boot_record_phase(name, ms);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        bevy::log::info!("[zz boot] {name}={ms}ms");
    }
}

/// Emit the one-line phase summary (`[zz boot] fetch=…`).
pub fn boot_log_phases() {
    #[cfg(target_arch = "wasm32")]
    {
        wasm_boot_call0("logPhases");
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

/// Wasm capture-phase Space from `window.__zzKeys.space` (see `web/index.html`).
/// Native always false — winit `ButtonInput` owns desktop Space.
///
/// ORd into `PlayerInput.jump` alongside winit Space and `TouchIntent.jump`
/// so a missed canvas focus / browser default action cannot drop jump.
pub fn js_space_pressed() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        wasm_js_space_pressed()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        false
    }
}

/// Refocus the Bevy canvas and blur stray text agents (egui_text_agent, HTML
/// overlays that already closed). Call on session transitions into Playing
/// so keyboard after a lobby click lands on the game. No-op on native.
pub fn refocus_canvas() {
    #[cfg(target_arch = "wasm32")]
    {
        wasm_refocus_canvas();
    }
}

// ── proximity voice (wasm bridge → web/index.html `__zzVoice`) ─────────────
// Native capture lives entirely in `voice.rs` (cpal); these exist only on wasm.

/// Request mic permission + start AudioWorklet capture. Idempotent.
/// Mic is **never** requested at startup — only on first unmute (Key M).
#[cfg(target_arch = "wasm32")]
pub fn voice_request_mic() {
    wasm_voice_call0("requestMic");
}

/// Enable/disable transmission of captured frames (privacy mute).
#[cfg(target_arch = "wasm32")]
pub fn voice_set_tx_enabled(enabled: bool) {
    wasm_voice_call1_bool("setTxEnabled", enabled);
}

/// Drain completed 16 kHz mono i16 LE PCM frames (~120 ms each).
#[cfg(target_arch = "wasm32")]
pub fn voice_drain_pcm_frames() -> Vec<Vec<u8>> {
    wasm_voice_drain_frames()
}

/// True once getUserMedia + worklet graph is live.
#[cfg(target_arch = "wasm32")]
pub fn voice_mic_ready() -> bool {
    wasm_voice_flag("ready")
}

/// True if the user denied the mic (or getUserMedia is unavailable).
#[cfg(target_arch = "wasm32")]
pub fn voice_mic_denied() -> bool {
    wasm_voice_flag("denied")
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
    if let Ok(d) = style.get_property_value("display")
        && d == "none"
    {
        return None;
    }
    Some(input.value())
}

/// Like [`wasm_input_value`] but ignores `display` (handoff reads always).
#[cfg(target_arch = "wasm32")]
fn wasm_input_value_raw(id: &str) -> Option<String> {
    use wasm_bindgen::JsCast;
    let el = document_element(id)?;
    let input: web_sys::HtmlInputElement = el.dyn_into().ok()?;
    Some(input.value())
}

#[cfg(target_arch = "wasm32")]
fn wasm_boot_obj() -> Option<js_sys::Object> {
    use wasm_bindgen::JsCast;
    let window = web_sys::window()?;
    let v = js_sys::Reflect::get(&window, &wasm_bindgen::JsValue::from_str("__zzBoot")).ok()?;
    if v.is_undefined() || v.is_null() {
        return None;
    }
    v.dyn_into::<js_sys::Object>().ok()
}

#[cfg(target_arch = "wasm32")]
fn wasm_boot_call0(method: &str) {
    use wasm_bindgen::JsCast;
    let Some(obj) = wasm_boot_obj() else {
        return;
    };
    let Ok(f) = js_sys::Reflect::get(&obj, &wasm_bindgen::JsValue::from_str(method)) else {
        return;
    };
    if let Ok(func) = f.dyn_into::<js_sys::Function>() {
        let _ = func.call0(&obj);
    }
}

#[cfg(target_arch = "wasm32")]
fn wasm_boot_handoff(is_touch: bool) {
    use wasm_bindgen::JsCast;
    let Some(obj) = wasm_boot_obj() else {
        return;
    };
    let Ok(f) = js_sys::Reflect::get(&obj, &wasm_bindgen::JsValue::from_str("handoff")) else {
        return;
    };
    if let Ok(func) = f.dyn_into::<js_sys::Function>() {
        let _ = func.call1(&obj, &wasm_bindgen::JsValue::from_bool(is_touch));
    }
}

#[cfg(target_arch = "wasm32")]
fn wasm_boot_hide_all() {
    wasm_boot_call0("hideAll");
}

#[cfg(target_arch = "wasm32")]
fn wasm_boot_record_phase(name: &str, ms: u32) {
    use wasm_bindgen::JsCast;
    let Some(obj) = wasm_boot_obj() else {
        bevy::log::info!("[zz boot] {name}={ms}ms");
        return;
    };
    let Ok(f) = js_sys::Reflect::get(&obj, &wasm_bindgen::JsValue::from_str("recordPhase")) else {
        bevy::log::info!("[zz boot] {name}={ms}ms");
        return;
    };
    if let Ok(func) = f.dyn_into::<js_sys::Function>() {
        let _ = func.call2(
            &obj,
            &wasm_bindgen::JsValue::from_str(name),
            &wasm_bindgen::JsValue::from_f64(ms as f64),
        );
    }
    bevy::log::info!("[zz boot] {name}={ms}ms");
}

#[cfg(target_arch = "wasm32")]
fn wasm_boot_action(take: bool) -> Option<crate::seams::QueuedBootAction> {
    use wasm_bindgen::JsCast;
    let obj = wasm_boot_obj()?;
    let method = if take { "takeAction" } else { "peekAction" };
    let f = js_sys::Reflect::get(&obj, &wasm_bindgen::JsValue::from_str(method)).ok()?;
    let func = f.dyn_into::<js_sys::Function>().ok()?;
    let ret = func.call0(&obj).ok()?;
    if ret.is_null() || ret.is_undefined() {
        return None;
    }
    let kind = js_sys::Reflect::get(&ret, &wasm_bindgen::JsValue::from_str("kind"))
        .ok()
        .and_then(|v| v.as_string())?;
    match kind.as_str() {
        "host" => Some(crate::seams::QueuedBootAction::Host),
        "join" => {
            let code = js_sys::Reflect::get(&ret, &wasm_bindgen::JsValue::from_str("code"))
                .ok()
                .and_then(|v| v.as_string())
                .unwrap_or_default();
            let code = code.trim().to_uppercase();
            if code.is_empty() {
                None
            } else {
                Some(crate::seams::QueuedBootAction::Join(code))
            }
        }
        _ => None,
    }
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
    if let Some(f) = unlock
        && let Ok(func) = f.dyn_into::<js_sys::Function>()
    {
        let _ = func.call0(&window);
    }
}

#[cfg(target_arch = "wasm32")]
fn wasm_js_space_pressed() -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    let Ok(keys) =
        js_sys::Reflect::get(&window, &wasm_bindgen::JsValue::from_str("__zzKeys"))
    else {
        return false;
    };
    if keys.is_undefined() || keys.is_null() {
        return false;
    }
    js_sys::Reflect::get(&keys, &wasm_bindgen::JsValue::from_str("space"))
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

#[cfg(target_arch = "wasm32")]
fn wasm_refocus_canvas() {
    use wasm_bindgen::JsCast;
    let Some(window) = web_sys::window() else {
        return;
    };
    let focus =
        js_sys::Reflect::get(&window, &wasm_bindgen::JsValue::from_str("__zzFocusCanvas"))
            .ok()
            .filter(|v| v.is_function());
    if let Some(f) = focus
        && let Ok(func) = f.dyn_into::<js_sys::Function>()
    {
        let _ = func.call0(&window);
    }
}

#[cfg(target_arch = "wasm32")]
fn wasm_voice_obj() -> Option<js_sys::Object> {
    use wasm_bindgen::JsCast;
    let window = web_sys::window()?;
    let v = js_sys::Reflect::get(&window, &wasm_bindgen::JsValue::from_str("__zzVoice")).ok()?;
    if v.is_undefined() || v.is_null() {
        return None;
    }
    v.dyn_into::<js_sys::Object>().ok()
}

#[cfg(target_arch = "wasm32")]
fn wasm_voice_call0(method: &str) {
    use wasm_bindgen::JsCast;
    let Some(obj) = wasm_voice_obj() else {
        return;
    };
    let Ok(f) = js_sys::Reflect::get(&obj, &wasm_bindgen::JsValue::from_str(method)) else {
        return;
    };
    if let Ok(func) = f.dyn_into::<js_sys::Function>() {
        let _ = func.call0(&obj);
    }
}

#[cfg(target_arch = "wasm32")]
fn wasm_voice_call1_bool(method: &str, arg: bool) {
    use wasm_bindgen::JsCast;
    let Some(obj) = wasm_voice_obj() else {
        return;
    };
    let Ok(f) = js_sys::Reflect::get(&obj, &wasm_bindgen::JsValue::from_str(method)) else {
        return;
    };
    if let Ok(func) = f.dyn_into::<js_sys::Function>() {
        let _ = func.call1(&obj, &wasm_bindgen::JsValue::from_bool(arg));
    }
}

#[cfg(target_arch = "wasm32")]
fn wasm_voice_flag(field: &str) -> bool {
    let Some(obj) = wasm_voice_obj() else {
        return false;
    };
    js_sys::Reflect::get(&obj, &wasm_bindgen::JsValue::from_str(field))
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Drain `window.__zzVoice.drainFrames()` → `Vec<Uint8Array>` of PCM frames.
#[cfg(target_arch = "wasm32")]
fn wasm_voice_drain_frames() -> Vec<Vec<u8>> {
    use wasm_bindgen::JsCast;
    let Some(obj) = wasm_voice_obj() else {
        return Vec::new();
    };
    let Ok(f) = js_sys::Reflect::get(&obj, &wasm_bindgen::JsValue::from_str("drainFrames")) else {
        return Vec::new();
    };
    let Ok(func) = f.dyn_into::<js_sys::Function>() else {
        return Vec::new();
    };
    let Ok(ret) = func.call0(&obj) else {
        return Vec::new();
    };
    let Ok(arr) = ret.dyn_into::<js_sys::Array>() else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(arr.length() as usize);
    for i in 0..arr.length() {
        let v = arr.get(i);
        // Prefer Uint8Array view of the PCM bytes.
        if let Ok(u8a) = v.clone().dyn_into::<js_sys::Uint8Array>() {
            let mut buf = vec![0u8; u8a.length() as usize];
            u8a.copy_to(&mut buf);
            if !buf.is_empty() {
                out.push(buf);
            }
            continue;
        }
        // Int16Array → LE bytes.
        if let Ok(i16a) = v.dyn_into::<js_sys::Int16Array>() {
            let len = i16a.length() as usize;
            let mut buf = Vec::with_capacity(len * 2);
            for j in 0..len {
                let s = i16a.get_index(j as u32);
                buf.extend_from_slice(&s.to_le_bytes());
            }
            if !buf.is_empty() {
                out.push(buf);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_entry_canary_matches_ship_script() {
        // scripts/ship-web.sh greps the ship wasm for this exact string.
        assert_eq!(BOOT_ENTRY_CANARY, "zz-boot-entry-v1");
        assert!(!BOOT_ENTRY_CANARY.is_empty());
    }

    #[test]
    fn boot_stamp_elapsed_is_sane() {
        let t0 = BootStamp::now();
        let ms = t0.elapsed_ms();
        // Should not jump to u32::MAX on a fresh stamp.
        assert!(ms < 60_000, "elapsed_ms={ms} unreasonably large for fresh stamp");
    }
}
