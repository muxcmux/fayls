use anyhow::{Result, bail};
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{
    process::Command,
    sync::{
        Semaphore,
        mpsc::{UnboundedReceiver, UnboundedSender},
    },
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

use crate::{config, fayls::ContentIndexable};

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
    let temp_dir = tempfile::tempdir()?;
    let output_prefix = temp_dir.path().join("page");

    let output = Command::new(&config::get().app.pdftoppm_bin)
        .arg("-png")
        .arg(file_path)
        .arg(&output_prefix)
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "pdftoppm failed for {}: {}",
            file_path.display(),
            stderr.trim()
        );
    }

    let mut page_images = Vec::<PathBuf>::new();
    let mut entries = tokio::fs::read_dir(temp_dir.path()).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
        {
            page_images.push(path);
        }
    }

    page_images.sort();

    if page_images.is_empty() {
        bail!(
            "no images generated from {} by pdftoppm",
            file_path.display()
        );
    }

    let mut content = String::new();

    for page_image in page_images {
        let page_text = extract_image_content(&page_image).await?;
        if !content.is_empty() {
            content.push('\n');
        }
        content.push_str(&page_text);
    }

    Ok(content)
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

async fn index(indexable: &mut ContentIndexable) -> Result<()> {
    let content = extract_content_from_file(indexable.fayl().path().as_ref())
        .await
        .unwrap_or(String::new());

    indexable.index_content(&content).await?;

    tracing::info!("indexed {}", indexable.fayl().path().display());

    Ok(())
}

pub(crate) async fn start_indexing(
    mut rx: UnboundedReceiver<(ContentIndexable, usize)>,
    tx: UnboundedSender<(ContentIndexable, usize)>,
    token: CancellationToken,
) {
    let mut queue: JoinSet<()> = JoinSet::new();
    let semaphore = Arc::new(Semaphore::new(
        config::get().indexing.max_concurrent_indexers,
    ));

    while let Some((mut indexable, retry)) = rx.recv().await {
        if token.is_cancelled() {
            tracing::info!("breaking content indexing recv loop");
            break;
        }

        let tx = tx.clone();
        let permit = semaphore.clone().acquire_owned().await.unwrap();
        queue.spawn(async move {
            if let Err(err) = index(&mut indexable).await {
                if retry > config::get().indexing.max_retries {
                    tracing::error!("content indexing failed: {err}, giving up");
                } else {
                    let retry = retry + 1;
                    tracing::error!("content indexing failed: {err}, retrying ({retry})");
                    _ = tx.send((indexable, retry));
                }
            }
            drop(permit);
        });
    }

    tracing::info!("content indexing stopped");
}
