//! ZombieZap shared simulation core.
//!
//! Everything in this crate runs identically on the server (native) and in the
//! browser client (wasm32): movement, raycasting, map generation, the wire
//! protocol, and the snapshot codec. There is deliberately no second
//! implementation of any of it anywhere else. No I/O, no async, no engine.

pub mod constants;
pub mod math;
pub mod movement;
pub mod protocol;
pub mod rng;
pub mod snapshot;
pub mod types;

pub use constants::PROTOCOL_VERSION;
