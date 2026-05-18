use anyhow::{Result, bail};
use futures_util::StreamExt;
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio_util::sync::CancellationToken;

use tokio::{
    process::Command,
    sync::{
        Semaphore,
        mpsc::{UnboundedReceiver, UnboundedSender},
    },
    task::JoinSet,
};
use walkdir::WalkDir;

use crate::{
    app, config,
    db::{ExistingPathRecord, NewPathRecord, PathRecordKind},
    web::{self, views::Page},
};

#[derive(Debug, PartialEq, Eq, Hash)]
pub(crate) enum IndexEvent {
    Update(PathBuf),
    ForceUpdate(PathBuf),
    Remove(PathBuf),
}

impl IndexEvent {
    fn path(&self) -> &Path {
        match self {
            Self::Update(path_buf) | Self::ForceUpdate(path_buf) | Self::Remove(path_buf) => {
                path_buf.as_path()
            }
        }
    }
}

pub(crate) async fn start_scanning(
    mut rx: UnboundedReceiver<()>,
    tx: UnboundedSender<(Vec<IndexEvent>, usize)>,
    token: CancellationToken,
) {
    while rx.recv().await.is_some() {
        if token.is_cancelled() {
            break;
        }

        if let Err(e) = scan_existing().await {
            tracing::error!(error = ?e, "error scanning existing");
        }

        scan_fs(&tx, &token);
    }

    tracing::info!("scanning stopped");
}

async fn scan_existing() -> Result<(), sqlx::Error> {
    let mut all = sqlx::query_as::<_, ExistingPathRecord>("SELECT * FROM paths").fetch(app::db());

    let mut txn = app::db().begin().await?;

    while let Some(Ok(record)) = all.next().await {
        if record.path_buf().exists() {
            continue;
        }

        record.remove(&mut *txn).await?;
        let path = record.path_buf();
        tracing::info!("removing deleted file: {:?}", &path);
        web::reload(Page::from(&path));
        if config::get().app.canonicalized_sources().contains(&path) {
            web::reload(Page::root());
        }
    }

    txn.commit().await?;

    Ok(())
}

fn scan_fs(tx: &UnboundedSender<(Vec<IndexEvent>, usize)>, token: &CancellationToken) {
    let batch_size = config::get().indexing.batch_size;

    let paths = config::get().app.canonicalized_sources();
    let mut batch = Vec::with_capacity(batch_size);

    for path in paths {
        for entry in WalkDir::new(path).min_depth(0) {
            if token.is_cancelled() {
                return;
            }

            match entry {
                Ok(entry) => {
                    batch.push(IndexEvent::Update(entry.into_path()));
                    if batch.len() >= batch_size {
                        _ = tx.send((std::mem::take(&mut batch), 0));
                    }
                }
                Err(error) => tracing::warn!("failed to read directory entry: {error}"),
            }
        }
    }

    if !batch.is_empty() {
        _ = tx.send((batch, 0));
    }
}

pub(crate) async fn start_indexing(
    mut batch_rx: UnboundedReceiver<(Vec<IndexEvent>, usize)>,
    batch_tx: UnboundedSender<(Vec<IndexEvent>, usize)>,
    index_tx: UnboundedSender<(ExistingPathRecord, usize)>,
    token: CancellationToken,
) {
    let mut queue: JoinSet<()> = JoinSet::new();
    let semaphore = Arc::new(Semaphore::new(
        config::get().indexing.max_concurrent_batches,
    ));

    while let Some((batch, retry)) = batch_rx.recv().await {
        if token.is_cancelled() {
            tracing::info!("breaking indexing recv loop");
            break;
        }

        let index_tx = index_tx.clone();
        let batch_tx = batch_tx.clone();
        let token = token.clone();
        let permit = semaphore.clone().acquire_owned().await.unwrap();
        queue.spawn(async move {
            if let Err(err) = index(&batch, index_tx, token).await {
                if retry > config::get().indexing.max_retries {
                    tracing::error!("batch error: {err}, giving up");
                } else {
                    let retry = retry + 1;
                    tracing::error!("batch error: {err}, retrying ({retry})");
                    _ = batch_tx.send((batch, retry));
                }
            }
            drop(permit);
        });
    }

    tracing::info!("batch indexing stopped");
}

async fn index(
    events: &[IndexEvent],
    tx: UnboundedSender<(ExistingPathRecord, usize)>,
    token: CancellationToken,
) -> Result<()> {
    let mut to_reindex = Vec::with_capacity(events.len());
    let mut to_insert = Vec::with_capacity(events.len());
    let mut to_update = Vec::with_capacity(events.len());
    let mut to_delete = Vec::with_capacity(events.len());

    for event in events {
        if token.is_cancelled() {
            break;
        }

        let new_record = NewPathRecord::from(event);
        let existing = new_record.find_existing(app::db()).await?;

        if let IndexEvent::Remove(_) = event {
            to_delete.push(existing);
            continue;
        }

        if let Some(existing) = existing {
            if matches!(event, IndexEvent::ForceUpdate(_)) || existing.is_outdated(&new_record) {
                tracing::info!("reindexing: {}", event.path().display());
                to_update.push((existing, new_record.size, new_record.last_modified));
            } else if !existing.is_processed() {
                tracing::info!("reindexing: {}", event.path().display());
                to_reindex.push(existing);
            }
        } else {
            tracing::info!("indexing: {}", event.path().display());
            to_insert.push(new_record);
        }
    }

    let mut txn = app::db().begin().await?;

    for new in to_insert {
        let existing = new.insert(&mut *txn).await?;
        let path = existing.path_buf();
        web::reload(Page::from(&path));
        if config::get().app.canonicalized_sources().contains(&path) {
            web::reload(Page::root());
        }
        to_reindex.push(existing);
    }

    for (mut existing, size, last_modified) in to_update {
        existing.touch(size, last_modified, &mut *txn).await?;
        let path = existing.path_buf();
        web::reload(Page::from(&path));
        if config::get().app.canonicalized_sources().contains(&path) {
            web::reload(Page::root());
        }
        to_reindex.push(existing);
    }

    for record in to_delete.iter().flatten() {
        record.remove(&mut *txn).await?;
    }

    txn.commit().await?;

    for item in to_reindex {
        if token.is_cancelled() {
            break;
        }

        tx.send((item, 0))?;
    }

    Ok(())
}

pub(crate) async fn start_indexing_contents(
    mut rx: UnboundedReceiver<(ExistingPathRecord, usize)>,
    tx: UnboundedSender<(ExistingPathRecord, usize)>,
    token: CancellationToken,
) {
    let mut queue: JoinSet<()> = JoinSet::new();
    let semaphore = Arc::new(Semaphore::new(
        config::get().indexing.max_concurrent_indexers,
    ));

    while let Some((mut record, retry)) = rx.recv().await {
        if token.is_cancelled() {
            tracing::info!("breaking content indexing recv loop");
            break;
        }

        let tx = tx.clone();
        let permit = semaphore.clone().acquire_owned().await.unwrap();
        queue.spawn(async move {
            if let Err(err) = index_contents(&mut record).await {
                if retry > config::get().indexing.max_retries {
                    tracing::error!("content indexing failed: {err}, giving up");
                } else {
                    let retry = retry + 1;
                    tracing::error!("content indexing failed: {err}, retrying ({retry})");
                    _ = tx.send((record, retry));
                }
            }
            drop(permit);
        });
    }

    tracing::info!("content indexing stopped");
}

async fn index_contents(record: &mut ExistingPathRecord) -> Result<()> {
    let path = record.path_buf();

    let content = if record.kind == PathRecordKind::Directory {
        String::new()
    } else {
        extract_content_from_file(path.as_ref())
            .await
            .unwrap_or_default()
    };

    record.index_content(&content).await?;

    web::report_indexing_progress(get_progress().await?);

    tracing::info!("indexed {}", path.display());

    Ok(())
}

pub(crate) async fn get_progress() -> Result<(i64, i64), sqlx::Error> {
    sqlx::query_as::<_, (i64, i64)>(
        r"
        SELECT
        COUNT(CASE WHEN processed = 1 THEN 1 END) AS processed,
        COUNT(*) AS total
        FROM paths
    ",
    )
    .fetch_one(app::db())
    .await
}

async fn extract_image_content(file_path: &Path) -> Result<String> {
    let output = Command::new(&config::get().app.tesseract_bin)
        .arg(file_path)
        .arg("stdout")
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "tesseract failed for {}:\n{}",
            file_path.display(),
            stderr.trim()
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).into())
}

async fn extract_pdf_content(file_path: &Path) -> Result<String> {
    let output = Command::new(&config::get().app.extractpdf_bin)
        .arg(file_path)
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "extractpdf failed for {}:\n{}",
            file_path.display(),
            stderr.trim()
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).into())
}

async fn extract_content_from_file(file_path: &Path) -> Result<String> {
    match file_path.extension().into() {
        IndexableFileType::Pdf => Ok(extract_pdf_content(file_path).await?),
        IndexableFileType::Image => Ok(extract_image_content(file_path).await?),
        IndexableFileType::Ignored => Ok(String::new()),
        _ => Ok(tokio::fs::read_to_string(&file_path)
            .await
            .map_err(|e| anyhow::anyhow!(e))?),
    }
}

enum IndexableFileType {
    Pdf,
    Image,
    Ignored,
    Unknown,
    Other,
}

impl From<Option<&OsStr>> for IndexableFileType {
    fn from(value: Option<&OsStr>) -> Self {
        match value.and_then(OsStr::to_str).map(str::to_ascii_lowercase) {
            None => Self::Unknown,
            Some(s) => {
                if config::get().indexing.ignore_extensions.contains(&s) {
                    return Self::Ignored;
                }

                match s.as_ref() {
                    "pdf" => Self::Pdf,
                    "png" | "jpg" | "jpeg" | "tif" | "tiff" | "bmp" | "gif" | "webp" => Self::Image,
                    _ => Self::Other,
                }
            }
        }
    }
}
