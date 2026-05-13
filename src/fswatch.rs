use std::{collections::HashSet, time::Duration};

use anyhow::Result;
use notify_debouncer_full::{
    DebouncedEvent, RecommendedCache, new_debouncer_opt,
    notify::{self, EventKindMask, RecommendedWatcher, RecursiveMode},
};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio_util::sync::CancellationToken;

use crate::{config, path_indexing::EntryFromPathBuf};

pub(crate) async fn watch(
    token: CancellationToken,
    tx: UnboundedSender<(Vec<EntryFromPathBuf>, usize)>,
) {
    if let Err(e) = monitor_fs(token, tx).await {
        tracing::warn!(error = ?e, "fs watch error, filesystem monitoring disabled");
    }
}

async fn monitor_fs(
    token: CancellationToken,
    tx: UnboundedSender<(Vec<EntryFromPathBuf>, usize)>,
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
            Ok(events) => handle_events(events, &tx),
            Err(errors) => errors
                .iter()
                .for_each(|e| tracing::warn!(error = ?e, "fs events failed")),
        }
    }

    Ok(())
}

fn handle_events(
    events: Vec<DebouncedEvent>,
    tx: &UnboundedSender<(Vec<EntryFromPathBuf>, usize)>,
) {
    let mut entries = HashSet::with_capacity(events.len() * 3);

    for event in events {
        for path in &event.paths {
            entries.insert(path.clone());

            let mut path = path.clone();
            while let Some(parent) = path.parent() {
                entries.insert(parent.to_path_buf());

                if config::get().app.canonicalized_sources().contains(&path) {
                    break;
                }

                path = parent.to_path_buf();
            }
        }
    }

    _ = tx.send((entries.into_iter().map(EntryFromPathBuf::from).collect(), 0));
}
