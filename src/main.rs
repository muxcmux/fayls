use std::process::exit;

use fayls::{app::run_app, config::load_config};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().init();

    let config = match load_config() {
        Ok(c) => c,
        Err(err) => {
            tracing::error!("could not load config: {err}");
            exit(1)
        }
    };

    run_app(config).await;
}
