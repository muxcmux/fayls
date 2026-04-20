use std::process::exit;

use fayls::{app::run_app, config::load_config};

use refinery::embed_migrations;
use rusqlite::Connection;

embed_migrations!();

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

    let mut conn = Connection::open(&config.app.database).unwrap_or_else(|err| {
        panic!(
            "Failed connecting to {} ({})",
            &config.app.database.display(),
            err
        )
    });

    migrations::runner().run(&mut conn).unwrap_or_else(|err| {
        panic!(
            "Failed migrating {} ({})",
            &config.app.database.display(),
            err
        )
    });

    drop(conn);

    run_app(config).await;
}
