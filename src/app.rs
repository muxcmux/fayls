use crate::{api, config::Config, fayls};
use anyhow::Result;
use sqlx::{Pool, Sqlite};
use tokio_util::sync::CancellationToken;

use tokio::{
    signal::unix::{SignalKind, signal},
    sync::mpsc::{self, Sender},
};
use walkdir::DirEntry;

pub enum Event {
    Scan,
    Index(Vec<DirEntry>),
}

async fn handle_event(event: Event, ctx: &EventContext<'_>) -> Result<()> {
    match event {
        Event::Scan => {
            let tx = ctx.tx.clone();
            let batch_size = ctx.config.app.batch_size;
            let paths = ctx.config.app.sources.clone();
            tokio::spawn(async move {
                fayls::scan(paths, batch_size, tx).await;
            });
        }
        Event::Index(entries) => {
            fayls::index(entries, ctx.db).await?;
        }
    }

    Ok(())
}

struct EventContext<'a> {
    config: &'a Config,
    db: &'a Pool<Sqlite>,
    tx: Sender<Event>,
}

pub async fn run(config: Config, db: Pool<Sqlite>) {
    let (tx, mut rx) = mpsc::channel::<Event>(16);

    let event_config = config.clone();
    let event_tx = tx.clone();
    let event_db = db.clone();

    let cancellation_token = CancellationToken::new();
    let cloned_token = cancellation_token.clone();

    let mut handle_event_closure = async move || {
        while let Some(event) = rx.recv().await {
            let ctx = EventContext {
                config: &event_config,
                db: &event_db,
                tx: event_tx.clone(),
            };
            if let Err(err) = handle_event(event, &ctx).await {
                tracing::error!("{err}");
            }
        }
    };

    let event_handler = tokio::spawn(async move {
        tokio::select! {
            () = cloned_token.cancelled() => {
                tracing::info!("event handler stopped");
            }
            () = handle_event_closure() => {
                tracing::info!("event handler finished");
            }
        }
    });

    _ = tx.send(Event::Scan).await;

    let (server, router) = api::server(config, db, tx).await;
    let server_handle = server.handle();

    tokio::spawn(async move {
        match signal(SignalKind::terminate()) {
            Ok(mut sigterm) => {
                _ = sigterm.recv().await;
                tracing::info!("SIGTERM received, stopping...");
                server_handle.stop_graceful(None);
                cancellation_token.cancel();
            }
            _ => {
                tracing::error!("failed to listen for SIGTERM");
            }
        }
    });

    server.serve(router).await;

    _ = event_handler.await;
}
