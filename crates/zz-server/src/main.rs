//! ZombieZap game server binary: bind and serve the library's app.

#[tokio::main]
async fn main() {
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .expect("bind");
    println!(
        "zz-server (protocol v{}) listening on {}",
        zz_core::PROTOCOL_VERSION,
        listener.local_addr().unwrap()
    );
    axum::serve(listener, zz_server::app())
        .await
        .expect("serve");
}
