use anyhow::{Result, bail};
use pdf_oxide::PdfDocument;
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
    fayls::{ExistingFayl, FaylKind},
    web::{self, Event},
};

pub(crate) async fn get_progress() -> Result<(i64, i64), sqlx::Error> {
    sqlx::query_as::<_, (i64, i64)>(
        r"
        SELECT
        COUNT(CASE WHEN processed = 1 THEN 1 END) AS processed,
        COUNT(*) AS total
        FROM fayls
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
            "tesseract failed for {}: {}",
            file_path.display(),
            stderr.trim()
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).into())
}

async fn extract_pdf_content(file_path: &Path) -> Result<String> {
    let p = file_path.to_path_buf();
    tokio::task::spawn_blocking(|| {
        let doc = PdfDocument::open(p)?;
        let len = doc.page_count()?;
        let mut contents = Vec::with_capacity(len);

        for i in 0..len {
            contents.push(doc.extract_text(i)?);
        }

        Ok(contents.join("\n"))
    })
    .await?
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

async fn index(fayl: &mut ExistingFayl) -> Result<()> {
    let path = fayl.path_buf();

    let content = if fayl.kind == FaylKind::Directory {
        String::new()
    } else {
        extract_content_from_file(path.as_ref())
            .await
            .unwrap_or_default()
    };

    fayl.index_content(&content).await?;

    web::broadcast(Event::Progress(get_progress().await?));

    tracing::info!("indexed {}", path.display());

    Ok(())
}

pub(crate) async fn start_indexing(
    mut rx: UnboundedReceiver<(ExistingFayl, usize)>,
    tx: UnboundedSender<(ExistingFayl, usize)>,
    token: CancellationToken,
) {
    let mut queue: JoinSet<()> = JoinSet::new();
    let semaphore = Arc::new(Semaphore::new(
        config::get().indexing.max_concurrent_indexers,
    ));

    while let Some((mut fayl, retry)) = rx.recv().await {
        if token.is_cancelled() {
            tracing::info!("breaking content indexing recv loop");
            break;
        }

        let tx = tx.clone();
        let permit = semaphore.clone().acquire_owned().await.unwrap();
        queue.spawn(async move {
            if let Err(err) = index(&mut fayl).await {
                if retry > config::get().indexing.max_retries {
                    tracing::error!("content indexing failed: {err}, giving up");
                } else {
                    let retry = retry + 1;
                    tracing::error!("content indexing failed: {err}, retrying ({retry})");
                    _ = tx.send((fayl, retry));
                }
            }
            drop(permit);
        });
    }

    tracing::info!("content indexing stopped");
}
