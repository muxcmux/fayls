use fayls::{app, config};
use sqlx::{SqlitePool, sqlite::{SqliteConnectOptions, SqliteJournalMode}};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().init();

    config::load().unwrap_or_else(|err| panic!("could not load config:\n{err}"));

    let opts = SqliteConnectOptions::new()
        .filename(&config::get().app.database)
        .journal_mode(SqliteJournalMode::Wal)
        .create_if_missing(true);

    let pool = SqlitePool::connect_with(opts).await.unwrap_or_else(|err| {
        panic!(
            "failed creating a pool for {}:\n{}",
            &config::get().app.database.display(),
            err
        )
    });

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .unwrap_or_else(|err| {
            panic!(
                "failed migrating {}:\n{}",
                &config::get().app.database.display(),
                err
            )
        });

    app::run(pool).await;
}
