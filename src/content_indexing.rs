use anyhow::{Context, Result, bail};
use sqlx::{Executor, Sqlite, SqlitePool};
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};
use tokio::process::Command;

use crate::fayls::IndexablePath;

async fn extract_image_content(file_path: &Path) -> Result<String> {
    let output = Command::new("tesseract")
        .arg(file_path)
        .arg("stdout")
        .output()
        .await
        .with_context(|| format!("failed running tesseract for {}", file_path.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "tesseract failed for {}: {}",
            file_path.display(),
            stderr.trim()
        );
    }

    String::from_utf8(output.stdout).with_context(|| {
        format!(
            "tesseract output is not valid utf-8 for {}",
            file_path.display()
        )
    })
}

async fn extract_pdf_content(file_path: &Path) -> Result<String> {
    let temp_dir = tempfile::tempdir()
        .with_context(|| format!("failed creating temp dir for {}", file_path.display()))?;
    let output_prefix = temp_dir.path().join("page");

    let output = Command::new("pdftoppm")
        .arg("-png")
        .arg(file_path)
        .arg(&output_prefix)
        .output()
        .await
        .with_context(|| format!("failed running pdftoppm for {}", file_path.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "pdftoppm failed for {}: {}",
            file_path.display(),
            stderr.trim()
        );
    }

    let mut page_images = Vec::<PathBuf>::new();
    let mut entries = tokio::fs::read_dir(temp_dir.path())
        .await
        .with_context(|| {
            format!(
                "failed reading temporary images directory {}",
                temp_dir.path().display()
            )
        })?;
    while let Some(entry) = entries.next_entry().await.with_context(|| {
        format!(
            "failed reading temporary image entries from {}",
            temp_dir.path().display()
        )
    })? {
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

pub(crate) async fn index(path: IndexablePath, db: SqlitePool) -> Result<()> {
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    tracing::info!("indexed content for {}", path.0.display());
    Ok(())
    // let file_path = fayl.path();
    // let content = match tokio::fs::read_to_string(&file_path).await {
    //     Ok(content) => content,
    //     Err(_) => match extract_content_from_file(&file_path).await {
    //         Ok(content) => content,
    //         Err(err) => {
    //             tracing::error!("can't extract contents from {}\n{err}", file_path.display());
    //             return Ok(());
    //         }
    //     },
    // };
    //
    // sqlx::query(
    //     r"
    //         INSERT INTO content_index (rowid, name, content)
    //         VALUES (?, ?, ?)
    //     ",
    // )
    // .bind(fayl.id.cast_signed())
    // .bind(&fayl.name)
    // .bind(&content)
    // .execute(db)
    // .await?;
    //
    // Ok(())
}
