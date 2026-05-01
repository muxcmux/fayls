use fayls::{app, config};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    config::load().unwrap_or_else(|err| panic!("could not load config:\n{err}"));

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or(EnvFilter::new(&config::get().app.log_level)),
        )
        .init();

    app::load_db().await;

    sqlx::migrate!("./migrations")
        .run(app::db())
        .await
        .unwrap_or_else(|err| {
            panic!(
                "failed migrating {}:\n{}",
                &config::get().database.path.display(),
                err
            )
        });

    app::run().await;
}
