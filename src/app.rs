use crate::config::Config;
use anyhow::Result;
use crc_fast::checksum_file;
use salvo::{
    http::{HeaderValue, StatusError, header},
    prelude::*,
};
use sqlx::{
    Decode, Pool, Sqlite, SqlitePool,
    sqlite::{SqliteArgumentValue, SqliteTypeInfo, SqliteValueRef},
};
use std::{
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
use tokio_util::sync::CancellationToken;

use salvo::handler;
use tokio::{
    signal::unix::{SignalKind, signal},
    sync::mpsc::{self, Sender},
};
use walkdir::{DirEntry, WalkDir};

enum Event {
    Scan(Vec<PathBuf>),
    Index(Vec<DirEntry>),
}

async fn scan(paths: Vec<PathBuf>, batch_size: usize, tx: Sender<Event>) {
    let mut batch = Vec::with_capacity(batch_size);
    for path in paths {
        for entry in WalkDir::new(path).min_depth(1) {
            match entry {
                Ok(entry) => {
                    batch.push(entry);
                    if batch.len() >= batch_size {
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

async fn index(entries: Vec<DirEntry>, db: &SqlitePool) -> Result<()> {
    let mut txn = db.begin().await?;

    for entry in entries {
        let existing = sqlx::query_as::<_, Fayl>(
            r"
            SELECT path, parent, kind, size, checksum, last_modified
            FROM fayls
            WHERE path = ?
        ",
        )
        .bind(entry.path().to_string_lossy().as_ref())
        .fetch_optional(&mut *txn)
        .await?;

        if let Some(fayl) = existing {
            tracing::info!("skipping {}", fayl.path);
        } else {
            let fayl: Fayl = entry.into();
            sqlx::query(
                r"
                INSERT INTO fayls (path, parent, kind, size, checksum, last_modified)
                VALUES (?, ?, ?, ?, ?, ?)
            ",
            )
            .bind(&fayl.path)
            .bind(&fayl.parent)
            .bind(&fayl.kind)
            .bind(fayl.size.cast_signed())
            .bind(fayl.checksum.map(u64::cast_signed))
            .bind(fayl.last_modified.map(u64::cast_signed))
            .execute(&mut *txn)
            .await?;
            tracing::info!("indexed: {}", fayl.path);
        }
    }

    txn.commit().await?;

    Ok(())
}

async fn handle_event(event: Event, ctx: &EventContext<'_>) -> Result<()> {
    match event {
        Event::Scan(paths) => {
            let tx = ctx.tx.clone();
            let batch_size = ctx.config.app.batch_size;
            tokio::spawn(async move {
                scan(paths, batch_size, tx).await;
            });
        }
        Event::Index(entries) => {
            index(entries, ctx.db).await?;
        }
    }

    Ok(())
}

#[derive(serde::Serialize)]
enum FaylKind {
    File,
    Symlink,
    Directory,
}

impl sqlx::Type<Sqlite> for FaylKind {
    fn type_info() -> SqliteTypeInfo {
        <String as sqlx::Type<Sqlite>>::type_info()
    }
}

impl<'r> sqlx::Decode<'r, Sqlite> for FaylKind {
    fn decode(value: SqliteValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let s = <String as Decode<Sqlite>>::decode(value)?;

        match s.as_str() {
            "file" => Ok(FaylKind::File),
            "symlink" => Ok(FaylKind::Symlink),
            "directory" => Ok(FaylKind::Directory),
            _ => Err(format!("invalid status: {s}").into()),
        }
    }
}

impl<'q> sqlx::Encode<'q, Sqlite> for FaylKind {
    fn encode_by_ref(
        &self,
        buf: &mut Vec<SqliteArgumentValue<'q>>,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        let s = match self {
            FaylKind::File => "file",
            FaylKind::Symlink => "symlink",
            FaylKind::Directory => "directory",
        };

        <&str as sqlx::Encode<Sqlite>>::encode(s, buf)
    }
}

#[derive(serde::Serialize, sqlx::FromRow)]
struct Fayl {
    path: String,
    parent: Option<String>,
    kind: FaylKind,
    size: u64,
    last_modified: Option<u64>,
    checksum: Option<u64>,
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

        let checksum = checksum_file(
            crc_fast::CrcAlgorithm::Crc32Iscsi,
            &entry.path().to_string_lossy(),
            None,
        )
        .ok();

        Fayl {
            kind,
            size,
            last_modified,
            checksum,
            parent: entry
                .path()
                .parent()
                .map(|p| p.to_string_lossy().into_owned()),
            path: entry.path().to_string_lossy().into_owned(),
        }
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

async fn list_entries(path: &Path, db: &SqlitePool) -> Json<Vec<Fayl>> {
    let items = sqlx::query_as::<_, Fayl>(
        r"
        SELECT path, parent, kind, size, checksum, last_modified
        FROM fayls
        WHERE parent = ?
        ORDER BY
            CASE WHEN kind = 'directory' THEN 0 ELSE 1 END,
            path
    ",
    )
    .bind(path.to_string_lossy().as_ref())
    .fetch_all(db)
    .await
    .unwrap();

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
async fn list_files_handler(
    req: &mut Request,
    depot: &mut Depot,
    res: &mut Response,
) -> Result<(), StatusError> {
    let path = req
        .query::<String>("path")
        .ok_or_else(StatusError::bad_request)?;

    let requested_path = Path::new(&path);
    if !requested_path.exists() {
        return Err(StatusError::not_found());
    }

    let state = depot.obtain::<AppState>().map_err(|_| {
        tracing::error!("app state missing from depot");
        StatusError::internal_server_error()
    })?;

    res.render(list_entries(requested_path, &state.db).await);
    Ok(())
}

#[derive(Clone)]
struct AppState {
    config: Config,
    db: Pool<Sqlite>,
    tx: Sender<Event>,
}

struct EventContext<'a> {
    config: &'a Config,
    db: &'a Pool<Sqlite>,
    tx: Sender<Event>,
}

pub async fn run_app(config: Config, db: Pool<Sqlite>) {
    let (tx, mut rx) = mpsc::channel::<Event>(16);

    let event_config = config.clone();
    let event_tx = tx.clone();
    let event_db = db.clone();

    let cancellation_token = CancellationToken::new();
    let cloned_token = cancellation_token.clone();

    let mut handle_event = async move || {
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
            () = handle_event() => {
                tracing::info!("event handler finished");
            }
        }
    });

    let state = AppState { config, db, tx };
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
