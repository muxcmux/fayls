use crate::{
    api,
    config::Config,
    content_indexing,
    fayls::{self, IndexablePath},
};
use bounded_join_set::JoinSet;
use sqlx::{Pool, Sqlite};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use tokio::{
    signal::unix::{SignalKind, signal},
    sync::mpsc,
};
use walkdir::DirEntry;

pub async fn run(config: Config, db: Pool<Sqlite>) {
    let (scan_tx, mut scan_rx) = mpsc::channel::<()>(1);
    let (batch_tx, mut batch_rx) = mpsc::channel::<Vec<DirEntry>>(100);
    let (index_tx, mut index_rx) = mpsc::channel::<IndexablePath>(10_000);

    let token = CancellationToken::new();
    let tracker = TaskTracker::new();

    let scan_token = token.clone();
    let sources = config.app.sources.clone();
    tracker.spawn(async move {
        while scan_rx.recv().await.is_some() {
            if scan_token.is_cancelled() {
                break;
            }

            fayls::scan(&sources, &batch_tx, &scan_token).await;
        }

        tracing::info!("scanning done");
    });

    let index_token = token.clone();
    let index_db = db.clone();
    tracker.spawn(async move {
        while let Some(batch) = batch_rx.recv().await {
            if index_token.is_cancelled() {
                break;
            }

            if let Err(err) = fayls::index(batch, &index_db, &index_tx, &index_token).await {
                tracing::error!("{err}");
            }
        }

        tracing::info!("batch indexing done");
    });

    let content_index_token = token.clone();
    let content_index_db = db.clone();
    tracker.spawn(async move {
        let mut queue: JoinSet<()> = JoinSet::new(5);

        while let Some(path) = index_rx.recv().await {
            if content_index_token.is_cancelled() {
                break;
            }

            let content_index_db = content_index_db.clone();
            queue.spawn(async move {
                if let Err(err) = content_indexing::index(path, content_index_db).await {
                    tracing::error!("{err}");
                }
            });
        }

        while queue.join_next().await.is_some() {
            if content_index_token.is_cancelled() {
                break;
            }
        }

        tracing::info!("content indexing done");
    });

    let (server, router) = api::server(config, db).await;
    let server_handle = server.handle();

    _ = scan_tx.send(()).await;

    server.serve(router).await;

    match signal(SignalKind::terminate()) {
        Ok(mut sigterm) => {
            _ = sigterm.recv().await;
            tracing::info!("SIGTERM received, stopping...");
            token.cancel();
            server_handle.stop_graceful(None);
        }
        _ => {
            tracing::error!("failed to listen for SIGTERM");
        }
    }
}
