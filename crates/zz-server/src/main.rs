//! ZombieZap game server. M2 brings the real thing; this stub proves the
//! workspace wiring (zz-core linkage, tokio runtime) end to end.

#[tokio::main]
async fn main() {
    println!(
        "zz-server (protocol v{}) — not serving yet",
        zz_core::PROTOCOL_VERSION
    );
}
