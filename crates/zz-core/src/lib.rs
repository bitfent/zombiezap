//! ZombieZap shared simulation core.
//!
//! Everything in this crate runs identically on the server (native) and in the
//! browser client (wasm32): movement, raycasting, map generation, the wire
//! protocol, and the snapshot codec. There is deliberately no second
//! implementation of any of it anywhere else.

pub const PROTOCOL_VERSION: u32 = 1;

#[cfg(test)]
mod tests {
    #[test]
    fn smoke() {
        assert_eq!(super::PROTOCOL_VERSION, 1);
    }
}
