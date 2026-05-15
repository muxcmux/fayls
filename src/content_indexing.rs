use anyhow::{Result, bail};
use std::{ffi::OsStr, path::Path, sync::Arc};
use tokio::{
    process::Command,
    sync::{
        Semaphore,
        mpsc::{UnboundedReceiver, UnboundedSender},
    },
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

use crate::{
    app, config,
    path_indexing::{ExistingPathRecord, PathRecordKind},
    web::{self, Event},
};

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

async fn index(record: &mut ExistingPathRecord) -> Result<()> {
    let path = record.path_buf();

    let content = if record.kind == PathRecordKind::Directory {
        String::new()
    } else {
        extract_content_from_file(path.as_ref())
            .await
            .unwrap_or_default()
    };

    record.index_content(&content).await?;

    web::broadcast(&Event::Progress(get_progress().await?));

    tracing::info!("indexed {}", path.display());

    Ok(())
}

pub(crate) async fn start_indexing(
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
            if let Err(err) = index(&mut record).await {
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
