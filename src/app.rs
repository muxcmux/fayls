use crate::config;
use std::{sync::OnceLock, time::Duration};

use crate::{
    content_indexing,
    fayls::{self, ContentIndexable},
    web,
};
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use tokio::{
    signal::unix::{SignalKind, signal},
    sync::mpsc,
};
use walkdir::DirEntry;

static SQLITE: OnceLock<SqlitePool> = OnceLock::new();

/// # Panics
/// when called before calling `load_db`
#[inline]
pub fn db() -> &'static SqlitePool {
    SQLITE.get().expect("Db not initialized")
}

/// # Panics
/// when called more than once
pub async fn load_db() {
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

    SQLITE.set(pool).expect("Db is already initialized");
}

pub async fn run() {
    let (scan_tx, scan_rx) = mpsc::unbounded_channel::<()>();
    let (batch_tx, batch_rx) = mpsc::unbounded_channel::<(Vec<DirEntry>, usize)>();
    let (index_tx, index_rx) = mpsc::unbounded_channel::<(ContentIndexable, usize)>();

    let token = CancellationToken::new();
    let tracker = TaskTracker::new();

    tracker.spawn(content_indexing::start_indexing(
        index_rx,
        index_tx.clone(),
        token.clone(),
    ));

    tracker.spawn(fayls::start_indexing_batches(
        batch_rx,
        batch_tx.clone(),
        index_tx,
        token.clone(),
    ));

    tracker.spawn(fayls::start_scanning(scan_rx, batch_tx, token.clone()));

    tracker.close();

    _ = scan_tx.send(());

    let (server, router) = web::server().await;
    let server_handle = server.handle();

    tokio::spawn(async move {
        if let Ok(mut sigterm) = signal(SignalKind::terminate()) {
            sigterm.recv().await;
            tracing::info!("received sigterm");
            token.cancel();
            server_handle.stop_graceful(None);
        }
    });

    tokio::select! {
        () = tracker.wait() => {},
        () = server.serve(router) => {},
    }
}
