use fayls::{app::run_app, config::load_config};
use sqlx::SqlitePool;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().init();

    let config = load_config().unwrap_or_else(|err| panic!("could not load config:\n{err}"));

    let pool = SqlitePool::connect(&config.app.database)
        .await
        .unwrap_or_else(|err| {
            panic!(
                "failed creating a pool for {}:\n{}",
                &config.app.database, err
            )
        });

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .unwrap_or_else(|err| panic!("failed migrating {}:\n{}", &config.app.database, err));

    run_app(config, pool).await;
}
