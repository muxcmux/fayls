use crate::config::Config;
use salvo::{
    http::{HeaderValue, header},
    prelude::*,
};
use std::{
    path::{Path, PathBuf},
    time::{Duration, UNIX_EPOCH},
};

use salvo::handler;
use tokio::{
    signal::unix::{SignalKind, signal},
    sync::mpsc::{self, Sender},
    time::sleep,
};
use walkdir::{DirEntry, WalkDir};

enum Event {
    Quit,
    FullReindex(Vec<PathBuf>),
    ReindexDirEntry(DirEntry),
}

struct FullReindexJob {
    paths: Vec<PathBuf>,
    tx: Sender<Event>,
}

impl FullReindexJob {
    fn new(paths: Vec<PathBuf>, tx: Sender<Event>) -> Self {
        Self { paths, tx }
    }

    async fn run(&self) {
        for path in &self.paths {
            tracing::info!("Reindexing {}", path.display());
            for entry in WalkDir::new(path).min_depth(1) {
                match entry {
                    Ok(entry) => {
                        _ = self.tx.send(Event::ReindexDirEntry(entry)).await;
                    }
                    Err(error) => tracing::warn!("failed to read directory entry: {error}"),
                }
            }
        }
    }
}

async fn handle_event(event: Event, state: AppState) {
    match event {
        Event::FullReindex(paths) => {
            tokio::spawn(async move {
                let job = FullReindexJob::new(paths, state.tx);
                job.run().await;
            });
        }
        Event::ReindexDirEntry(entry) => {
            let fsentry: FsEntry = entry.into();
            fsentry.reindex().await;
        }
        Event::Quit => {}
    }
}

#[derive(serde::Serialize)]
enum FsEntryKind {
    File,
    Symlink,
    Directory,
}

impl FsEntryKind {
    fn rank(&self) -> u8 {
        match self {
            FsEntryKind::Directory => 0,
            _ => 1,
        }
    }
}
#[derive(serde::Serialize)]
struct FsEntry {
    path: PathBuf,
    kind: FsEntryKind,
    last_modified: Option<jiff::Zoned>,
    size: u64,
}

impl From<DirEntry> for FsEntry {
    fn from(entry: DirEntry) -> Self {
        let metadata = entry.metadata().ok();
        let kind = if entry.file_type().is_dir() {
            FsEntryKind::Directory
        } else if entry.file_type().is_symlink() {
            FsEntryKind::Symlink
        } else {
            FsEntryKind::File
        };
        let size = if entry.file_type().is_dir() {
            dir_size(entry.path())
        } else {
            metadata.as_ref().map_or(0, std::fs::Metadata::len)
        };
        let last_modified = metadata.and_then(|m| zoned_from_systemtime(m.modified().ok()?));

        FsEntry {
            kind,
            size,
            last_modified,
            path: entry.into_path(),
        }
    }
}

impl FsEntry {
    async fn reindex(&self) {
        sleep(Duration::new(0, 999999)).await;
        tracing::info!("Indexed {}", self.path.display());
    }
}

fn zoned_from_systemtime(system_time: std::time::SystemTime) -> Option<jiff::Zoned> {
    let secs = system_time.duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(jiff::Zoned::new(
        jiff::Timestamp::from_second(secs.try_into().ok()?).ok()?,
        jiff::tz::TimeZone::system(),
    ))
}

fn dir_size(path: &Path) -> u64 {
    WalkDir::new(path)
        .min_depth(1)
        .into_iter()
        .filter_map(|entry| match entry {
            Ok(entry) if entry.file_type().is_file() => Some(entry),
            Ok(_) => None,
            Err(error) => {
                tracing::warn!(
                    "failed to read directory tree entry under {}: {error}",
                    path.display()
                );
                None
            }
        })
        .fold(0, |acc, entry| match entry.metadata() {
            Ok(metadata) => acc.saturating_add(metadata.len()),
            Err(error) => {
                tracing::warn!(
                    "failed to read metadata for {}: {error}",
                    entry.path().display()
                );
                acc
            }
        })
}

fn list_entries(path: &Path) -> Json<Vec<FsEntry>> {
    let mut items: Vec<FsEntry> = Vec::new();

    for entry in WalkDir::new(path).min_depth(1).max_depth(1) {
        match entry {
            Ok(entry) => items.push(entry.into()),
            Err(error) => tracing::warn!("failed to read directory entry: {error}"),
        }
    }

    items.sort_unstable_by(|a, b| match a.kind.rank().cmp(&b.kind.rank()) {
        std::cmp::Ordering::Equal => a.path.cmp(&b.path),
        rest => rest,
    });

    Json(items)
}

#[handler]
async fn force_json_format(
    req: &mut Request,
    depot: &mut Depot,
    res: &mut Response,
    ctrl: &mut FlowCtrl,
) {
    req.headers_mut()
        .insert(header::ACCEPT, HeaderValue::from_static("application/json"));

    ctrl.call_next(req, depot, res).await;

    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
}

#[handler]
async fn list_files_handler(req: &mut Request, res: &mut Response) {
    let Some(path) = req.query::<String>("path") else {
        res.status_code(StatusCode::BAD_REQUEST);
        return;
    };

    let requested_path = Path::new(&path);
    if !requested_path.exists() {
        res.status_code(StatusCode::NOT_FOUND);
        return;
    }

    res.render(list_entries(requested_path));
}

#[derive(Clone)]
struct AppState {
    config: Config,
    tx: Sender<Event>,
}

pub async fn run_app(config: Config) {
    let (tx, mut rx) = mpsc::channel::<Event>(8);
    let exit_tx = tx.clone();

    let state = AppState { config, tx };

    let state_for_event = state.clone();
    let event_handler = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            match event {
                Event::Quit => {
                    break;
                }
                _ => handle_event(event, state_for_event.clone()).await,
            }
        }
        tracing::info!("event handler finished, exiting");
    });

    let sources = state.config.app.sources.clone();

    _ = state.tx.send(Event::FullReindex(sources)).await;

    let acceptor = TcpListener::new(state.config.server.addr()).bind().await;

    let router = Router::new()
        .hoop(affix_state::inject(state))
        .hoop(force_json_format)
        .get(list_files_handler);

    let server = Server::new(acceptor);
    let server_handle = server.handle();

    tokio::spawn(async move {
        match (
            signal(SignalKind::terminate()),
            signal(SignalKind::interrupt()),
        ) {
            (Ok(mut sigterm), Ok(mut sigint)) => {
                tokio::select! {
                    _ = sigterm.recv() => {
                        tracing::info!("SIGTERM received, starting graceful shutdown");
                        server_handle.stop_graceful(None);
                        _ = exit_tx.send(Event::Quit).await;
                    }
                    _ = sigint.recv() => {
                        tracing::info!("SIGINT received, starting graceful shutdown");
                        server_handle.stop_graceful(None);
                        _ = exit_tx.send(Event::Quit).await;
                    }
                }
            }
            _ => {
                tracing::error!("failed to listen for signals");
            }
        }
    });

    server.serve(router).await;

    _ = event_handler.await;
}
