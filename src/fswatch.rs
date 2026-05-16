use std::{collections::HashSet, time::Duration};

use anyhow::Result;
use notify_debouncer_full::{
    DebouncedEvent, RecommendedCache, new_debouncer_opt,
    notify::{self, EventKindMask, RecommendedWatcher, RecursiveMode},
};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio_util::sync::CancellationToken;
use walkdir::WalkDir;

use crate::{config, indexing::IndexEvent};

pub(crate) async fn watch(token: CancellationToken, tx: UnboundedSender<(Vec<IndexEvent>, usize)>) {
    if let Err(e) = monitor_fs(token, tx).await {
        tracing::warn!(error = ?e, "fs watch error, filesystem monitoring disabled");
    }
}

async fn monitor_fs(
    token: CancellationToken,
    index_tx: UnboundedSender<(Vec<IndexEvent>, usize)>,
) -> Result<()> {
    let (fs_tx, mut fs_rx) = mpsc::unbounded_channel();
    let notify_config = notify::Config::default().with_event_kinds(EventKindMask::CORE);

    let mut debouncer = new_debouncer_opt::<_, RecommendedWatcher, RecommendedCache>(
        Duration::from_millis(100),
        None,
        fs_tx,
        RecommendedCache::new(),
        notify_config,
    )?;

    for source in config::get().app.canonicalized_sources() {
        debouncer.watch(source, RecursiveMode::Recursive)?;
    }

    while let Some(result) = fs_rx.recv().await {
        if token.is_cancelled() {
            break;
        }

        match result {
            Ok(events) => handle_events(events, &index_tx),
            Err(errors) => errors
                .iter()
                .for_each(|e| tracing::warn!(error = ?e, "fs events failed")),
        }
    }

    Ok(())
}

fn handle_events(
    events: Vec<DebouncedEvent>,
    index_tx: &UnboundedSender<(Vec<IndexEvent>, usize)>,
) {
    let mut index_events = HashSet::with_capacity(events.len() * 3);

    for event in events {
        for path in &event.paths {
            if path.exists() {
                if path.is_dir() {
                    for entry in WalkDir::new(path).min_depth(0).into_iter().flatten() {
                        index_events.insert(IndexEvent::Update(entry.into_path()));
                    }
                } else {
                    index_events.insert(IndexEvent::Update(path.clone()));
                }
            } else {
                index_events.insert(IndexEvent::Remove(path.clone()));
            }

            if !config::get().app.canonicalized_sources().contains(path) {
                for parent in path.ancestors().skip(1) {
                    let is_root = config::get().app.canonicalized_sources().contains(parent);
                    index_events.insert(IndexEvent::ForceUpdate(parent.to_path_buf()));

                    if is_root {
                        break;
                    }
                }
            }
        }
    }

    _ = index_tx.send((index_events.into_iter().collect(), 0));
}
