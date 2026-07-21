//! Native / wasm binary entry for zz-client.
//!
//! On wasm we use `#![no_main]` + `#[wasm_bindgen(start)]` so the boot entry is
//! an explicit export that trunk's generated `init()` → `__wbindgen_start()`
//! invokes. A bare `fn main` without the attribute also works for bin crates,
//! but the explicit start documents the contract and keeps the export stable
//! for ship canaries (`scripts/ship-web.sh` greps `__wbindgen_start`).

#![cfg_attr(target_arch = "wasm32", no_main)]

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::wasm_bindgen;

/// Browser entry: construct the Bevy app (HTML-first menu already interactive).
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(start)]
pub fn main() {
    zz_client::run();
}

/// Native entry (`cargo run -p zz-client`).
#[cfg(not(target_arch = "wasm32"))]
fn main() {
    zz_client::run();
}
