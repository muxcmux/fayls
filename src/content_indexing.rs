use anyhow::{Result, bail};
use bounded_join_set::JoinSet;
use sqlx::SqlitePool;
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};
use tokio::{process::Command, sync::mpsc::Receiver};
use tokio_util::sync::CancellationToken;

use crate::{
    config,
    fayls::{Fayl, Indexable},
};

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
    let ext = file_path
        .extension()
        .and_then(OsStr::to_str)
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| anyhow::anyhow!("{} has no file extension", file_path.display()))?;

    match ext.as_str() {
        "pdf" => extract_pdf_content(file_path).await,
        "png" | "jpg" | "jpeg" | "tif" | "tiff" | "bmp" | "gif" | "webp" => {
            extract_image_content(file_path).await
        }
        _ => bail!("unsupported file extension for content extraction: {ext}"),
    }
}

async fn index(fayl: Fayl, db: SqlitePool) -> Result<()> {
    let file_path = fayl.path();
    let content = match tokio::fs::read_to_string(&file_path).await {
        Ok(content) => content,
        Err(_) => match extract_content_from_file(&file_path).await {
            Ok(content) => content,
            Err(err) => {
                tracing::error!("can't extract contents from {}\n{err}", file_path.display());
                return Ok(());
            }
        },
    };

    fayl.index(&db, &content).await?;

    Ok(())
}

pub(crate) async fn start_indexing(
    db: SqlitePool,
    mut rx: Receiver<Indexable>,
    token: CancellationToken,
) {
    let mut queue: JoinSet<()> = JoinSet::new(5);

    while let Some(path) = rx.recv().await {
        if token.is_cancelled() {
            break;
        }

        let db = db.clone();
        queue.spawn(async move {
            if let Err(err) = index(path.0, db).await {
                tracing::error!("{err}");
            }
        });
    }

    while queue.join_next().await.is_some() {
        if token.is_cancelled() {
            break;
        }
    }

    tracing::info!("content indexing stopped");
}
