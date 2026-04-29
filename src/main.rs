use std::time::Duration;

use fayls::{app, config};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().init();

    config::load().unwrap_or_else(|err| panic!("could not load config:\n{err}"));

    let opts = SqliteConnectOptions::new()
        .filename(&config::get().database.path)
        .busy_timeout(Duration::from_secs(5))
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .pragma("temp_store", "MEMORY")
        .pragma("cache_size", "-20000")
        .pragma("threads", "4")
        .create_if_missing(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(config::get().database.max_connections)
        .connect_with(opts)
        .await
        .unwrap_or_else(|err| {
            panic!(
                "failed creating a pool for {}:\n{}",
                &config::get().database.path.display(),
                err
            )
        });

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .unwrap_or_else(|err| {
            panic!(
                "failed migrating {}:\n{}",
                &config::get().database.path.display(),
                err
            )
        });

    app::run(pool).await;
}
