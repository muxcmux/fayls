use crate::config::Config;
use anyhow::Result;
use rusqlite::{DropBehavior, OptionalExtension, types::Type};
use salvo::{
    http::{HeaderValue, header},
    prelude::*,
};
use std::{
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use salvo::handler;
use tokio::{
    signal::unix::{SignalKind, signal},
    sync::mpsc::{self, Sender},
};
use walkdir::{DirEntry, WalkDir};

enum Event {
    Quit,
    Scan(Vec<PathBuf>),
    Index(Vec<DirEntry>),
}

const BATCH_SIZE: usize = 10_000;

async fn scan(paths: Vec<PathBuf>, tx: Sender<Event>) {
    let mut batch = Vec::with_capacity(BATCH_SIZE);
    for path in paths {
        for entry in WalkDir::new(path).min_depth(1) {
            match entry {
                Ok(entry) => {
                    batch.push(entry);
                    if batch.len() >= BATCH_SIZE {
                        _ = tx.send(Event::Index(std::mem::take(&mut batch))).await;
                    }
                }
                Err(error) => tracing::warn!("failed to read directory entry: {error}"),
            }
        }
    }

    if !batch.is_empty() {
        _ = tx.send(Event::Index(batch)).await;
    }
}

async fn index(entries: Vec<DirEntry>, state: AppState) -> Result<()> {
    let mut db = state.config.db();
    let mut txn = db.transaction()?;
    txn.set_drop_behavior(DropBehavior::Commit);

    let mut select_stmt = txn.prepare(
        r"
        SELECT path, parent, name, kind, size, checksum, last_modified
        FROM fayls
        WHERE path = ?1
        ",
    )?;
    let mut insert_stmt = txn.prepare(
        r"
        INSERT INTO fayls (path, parent, name, kind, size, checksum, last_modified)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        ",
    )?;

    for entry in entries {
        let path = entry.path().to_path_buf();
        let path_s = path.to_string_lossy().to_string();
        if let Some(_existing) = select_stmt
            .query_row([path_s], |row| Fayl::try_from(row))
            .optional()?
        {
            // check if checksum is the same and if not, reindex content and update
            tracing::info!("skipping {}", path.display());
        } else {
            let fayl = Fayl::from(entry);
            insert_stmt.execute(rusqlite::params![
                fayl.path.to_string_lossy().to_string(),
                fayl.parent,
                fayl.name,
                fayl.kind.to_s(),
                fayl.size,
                fayl.checksum,
                fayl.last_modified
            ])?;
            tracing::info!("indexed {}", path.display());
        }
    }

    Ok(())
}

async fn handle_event(event: Event, state: AppState) -> Result<()> {
    match event {
        Event::Scan(paths) => {
            tokio::spawn(async move {
                scan(paths, state.tx).await;
            });
        }
        Event::Index(entries) => {
            // tokio::spawn(async move {
            index(entries, state).await?;
            // });
        }
        Event::Quit => {}
    }

    Ok(())
}

#[derive(serde::Serialize)]
enum FaylKind {
    File,
    Symlink,
    Directory,
}

impl FaylKind {
    fn to_s(&self) -> String {
        match self {
            FaylKind::File => "file".into(),
            FaylKind::Symlink => "symlink".into(),
            FaylKind::Directory => "directory".into(),
        }
    }
}

#[derive(serde::Serialize)]
struct Fayl {
    path: PathBuf,
    parent: Option<String>,
    name: Option<String>,
    kind: FaylKind,
    size: u64,
    last_modified: Option<u64>,
    checksum: Option<Vec<u8>>,
}

impl From<DirEntry> for Fayl {
    fn from(entry: DirEntry) -> Self {
        let metadata = entry.metadata().ok();
        let kind = if entry.file_type().is_dir() {
            FaylKind::Directory
        } else if entry.file_type().is_symlink() {
            FaylKind::Symlink
        } else {
            FaylKind::File
        };
        let size = if entry.file_type().is_dir() {
            dir_size(entry.path())
        } else {
            metadata.as_ref().map_or(0, std::fs::Metadata::len)
        };
        let last_modified = metadata
            .and_then(|m| m.modified().ok())
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs());
        let parent = entry
            .path()
            .parent()
            .map(|f| f.to_string_lossy().to_string());
        let name = entry
            .path()
            .file_name()
            .map(|f| f.to_string_lossy().to_string());

        Fayl {
            kind,
            parent,
            name,
            size,
            last_modified,
            path: entry.into_path(),
            checksum: None,
        }
    }
}

impl TryFrom<&rusqlite::Row<'_>> for Fayl {
    type Error = rusqlite::Error;

    fn try_from(row: &rusqlite::Row<'_>) -> std::result::Result<Self, Self::Error> {
        let kind = match row.get::<_, String>(3)?.as_str() {
            "file" => FaylKind::File,
            "symlink" => FaylKind::Symlink,
            "directory" => FaylKind::Directory,
            other => {
                return Err(rusqlite::Error::FromSqlConversionFailure(
                    3,
                    Type::Text,
                    format!("invalid fayl kind: {other}").into(),
                ));
            }
        };

        Ok(Fayl {
            path: PathBuf::from(row.get::<_, String>(0)?),
            parent: row.get(1)?,
            name: row.get(2)?,
            kind,
            size: row.get::<_, u64>(4)?,
            checksum: row.get(5)?,
            last_modified: row.get::<_, Option<u64>>(6)?,
        })
    }
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

fn list_entries(path: &Path, config: &Config) -> Json<Vec<Fayl>> {
    let mut items: Vec<Fayl> = Vec::new();
    let db = config.db();
    let mut stmt = match db.prepare(
        r"
        SELECT path, parent, name, kind, size, checksum, last_modified
        FROM fayls
        WHERE parent = ?1
        ORDER BY
            CASE WHEN kind = 'directory' THEN 0 ELSE 1 END,
            name
        ",
    ) {
        Ok(stmt) => stmt,
        Err(error) => {
            tracing::error!(
                "failed to prepare list_entries query for {}: {error}",
                path.display()
            );
            return Json(items);
        }
    };
    let parent = path.to_string_lossy().to_string();

    let rows = match stmt.query_map([parent], |row| Fayl::try_from(row)) {
        Ok(rows) => rows,
        Err(error) => {
            tracing::error!(
                "failed to query entries by parent for {}: {error}",
                path.display()
            );
            return Json(items);
        }
    };

    for row in rows {
        match row {
            Ok(fayl) => items.push(fayl),
            Err(error) => tracing::warn!("failed to decode fayls row: {error}"),
        }
    }

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
async fn list_files_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(path) = req.query::<String>("path") else {
        res.status_code(StatusCode::BAD_REQUEST);
        return;
    };

    let requested_path = Path::new(&path);
    if !requested_path.exists() {
        res.status_code(StatusCode::NOT_FOUND);
        return;
    }

    let state = match depot.obtain::<AppState>() {
        Ok(state) => state,
        Err(_) => {
            tracing::error!("app state missing from depot");
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    res.render(list_entries(requested_path, &state.config));
}

#[derive(Clone)]
struct AppState {
    config: Config,
    tx: Sender<Event>,
}

pub async fn run_app(config: Config) {
    let (tx, mut rx) = mpsc::channel::<Event>(16);
    let exit_tx = tx.clone();

    let state = AppState { config, tx };

    let state_for_event = state.clone();
    let event_handler = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            match event {
                Event::Quit => {
                    break;
                }
                _ => {
                    if let Err(err) = handle_event(event, state_for_event.clone()).await {
                        tracing::error!("{err}");
                    }
                }
            }
        }
        tracing::info!("event handler finished, exiting");
    });

    let sources = state.config.app.sources.clone();

    _ = state.tx.send(Event::Scan(sources)).await;

    let acceptor = TcpListener::new(state.config.server.addr()).bind().await;

    let router = Router::new()
        .hoop(affix_state::inject(state))
        .hoop(force_json_format)
        .get(list_files_handler);

    let server = Server::new(acceptor);
    let server_handle = server.handle();

    tokio::spawn(async move {
        match signal(SignalKind::terminate()) {
            Ok(mut sigterm) => {
                _ = sigterm.recv().await;
                tracing::info!("SIGTERM received, starting graceful shutdown");
                server_handle.stop_graceful(None);
                _ = exit_tx.send(Event::Quit).await;
            }
            _ => {
                tracing::error!("failed to listen for SIGTERM");
            }
        }
    });

    server.serve(router).await;

    _ = event_handler.await;
}
