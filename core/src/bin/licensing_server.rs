//! Licensing server binary — local-only PoC.
//!
//! Defaults to bind 0.0.0.0:8787 with data dir `./data/licensing/`.
//! Override via env vars:
//!   DIMMY_LICENSING_BIND="127.0.0.1:9000"
//!   DIMMY_LICENSING_DATA="/abs/path/to/data"
//!   DIMMY_LICENSING_PUBLIC_URL="http://example.test:9000"  (used in magic links)
//!
//! See `docs/dev/licensing-poc.md` for the full PoC walkthrough.

#[cfg(feature = "licensing-server")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use std::path::PathBuf;

    // Init lightweight logging — RUST_LOG=info,sqlx=warn for routine use.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,sqlx=warn")),
        )
        .init();

    let bind = std::env::var("DIMMY_LICENSING_BIND").unwrap_or_else(|_| "0.0.0.0:8787".into());
    let public_url =
        std::env::var("DIMMY_LICENSING_PUBLIC_URL").unwrap_or_else(|_| format!("http://{}", bind));
    let data_dir: PathBuf = std::env::var("DIMMY_LICENSING_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./data/licensing"));

    dimmy_lib::license_server::serve(&bind, &data_dir, &public_url).await
}

#[cfg(not(feature = "licensing-server"))]
fn main() {
    eprintln!(
        "licensing_server binary requires the `licensing-server` feature.\n\
         Build with: cargo build --bin licensing_server --features licensing-server"
    );
    std::process::exit(1);
}
