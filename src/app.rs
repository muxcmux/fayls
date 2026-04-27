use crate::{
    api, config, content_indexing,
    fayls::{self, Indexable},
};
use sqlx::SqlitePool;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use tokio::{
    signal::unix::{SignalKind, signal},
    sync::mpsc,
};
use walkdir::DirEntry;

pub async fn run(db: SqlitePool) {
    let cfg = &config::get().app;
    let (scan_tx, scan_rx) = mpsc::channel::<()>(1);
    let (batch_tx, batch_rx) = mpsc::channel::<Vec<DirEntry>>(cfg.max_concurrent_batches);
    let (index_tx, index_rx) = mpsc::channel::<Indexable>(cfg.max_concurrent_indexes);

    let token = CancellationToken::new();
    let tracker = TaskTracker::new();

    tracker.spawn(content_indexing::start_indexing(
        db.clone(),
        index_rx,
        token.clone(),
    ));

    tracker.spawn(fayls::start_indexing(
        db.clone(),
        batch_rx,
        index_tx,
        token.clone(),
    ));

    tracker.spawn(fayls::start_scanning(scan_rx, batch_tx, token.clone()));

    tracker.close();

    _ = scan_tx.send(()).await;

    let (server, router) = api::server(db).await;
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
        () = server.serve(router) => {},
        () = tracker.wait() => {},
    }
}
