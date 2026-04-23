use fayls::{app::run_app, config::load_config};
use sqlx::{SqlitePool, sqlite::SqliteConnectOptions};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().init();

    let config = load_config().unwrap_or_else(|err| panic!("could not load config:\n{err}"));

    let opts = SqliteConnectOptions::new()
        .filename(&config.app.database)
        .create_if_missing(true);

    let pool = SqlitePool::connect_with(opts).await.unwrap_or_else(|err| {
        panic!(
            "failed creating a pool for {}:\n{}",
            &config.app.database.display(),
            err
        )
    });

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .unwrap_or_else(|err| {
            panic!(
                "failed migrating {}:\n{}",
                &config.app.database.display(),
                err
            )
        });

    run_app(config, pool).await;
}
