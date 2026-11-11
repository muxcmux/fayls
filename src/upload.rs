use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use salvo::{Request, Response, http::StatusCode, writing::Text};
use serde::{Deserialize, Serialize};
use tokio::{fs, io::AsyncWriteExt};

use crate::{config, validate_folder_parent};

pub(crate) const CHUNK_SIZE: u64 = 2 * 1024 * 1024;
const FORM_OVERHEAD: usize = 64 * 1024;

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
struct UploadMeta {
    parent: PathBuf,
    name: String,
    total_size: u64,
    chunk_count: u64,
}

type UploadResult<T = ()> = Result<T, (StatusCode, &'static str)>;

fn bad(message: &'static str) -> (StatusCode, &'static str) {
    (StatusCode::BAD_REQUEST, message)
}

#[allow(clippy::needless_pass_by_value)]
fn io_error(error: std::io::Error) -> (StatusCode, &'static str) {
    tracing::error!(?error, "upload I/O failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Could not save the upload",
    )
}

fn field<'a>(fields: &'a multimap::MultiMap<String, String>, name: &str) -> UploadResult<&'a str> {
    fields
        .get(name)
        .map(String::as_str)
        .ok_or(bad("Missing upload field"))
}

fn number(fields: &multimap::MultiMap<String, String>, name: &str) -> UploadResult<u64> {
    field(fields, name)?
        .parse()
        .map_err(|_| bad("Invalid upload field"))
}

fn validate_name(name: &str) -> UploadResult {
    if name.is_empty()
        || name != name.trim()
        || matches!(name, "." | "..")
        || name.contains('/')
        || name.contains('\\')
        || name.chars().any(char::is_control)
    {
        return Err(bad("Invalid file name"));
    }
    Ok(())
}

fn validate_id(id: &str) -> UploadResult {
    if id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        return Err(bad("Invalid upload ID"));
    }
    Ok(())
}

pub(crate) async fn receive(req: &mut Request, res: &mut Response) {
    match receive_inner(req).await {
        Ok(()) => {
            res.status_code(StatusCode::OK);
            res.render(Text::Plain("OK"));
        }
        Err((status, message)) => {
            res.status_code(status);
            res.render(Text::Plain(message));
        }
    }
}

#[allow(clippy::cast_possible_truncation)]
async fn receive_inner(req: &mut Request) -> UploadResult {
    let form = req
        .form_data_max_size(CHUNK_SIZE as usize + FORM_OVERHEAD)
        .await
        .map_err(|_| {
            (
                StatusCode::PAYLOAD_TOO_LARGE,
                "Upload chunk is too large or malformed",
            )
        })?;
    let parent = PathBuf::from(field(&form.fields, "parent")?);
    let file = form.files.get("file").ok_or(bad("Missing file data"))?;
    let name = file.name().ok_or(bad("Missing file name"))?;
    validate_name(name)?;
    let parent = validate_folder_parent(&parent).await.map_err(bad)?;
    if !form.fields.contains_key("dzuuid") {
        if file.size() > config::get().app.max_upload_file_size || file.size() > CHUNK_SIZE {
            return Err((
                StatusCode::PAYLOAD_TOO_LARGE,
                "File exceeds the upload size limit",
            ));
        }
        return finish_direct(&parent, name, file.path()).await;
    }
    let id = field(&form.fields, "dzuuid")?;
    validate_id(id)?;
    let index = number(&form.fields, "dzchunkindex")?;
    let count = number(&form.fields, "dztotalchunkcount")?;
    let total_size = number(&form.fields, "dztotalfilesize")?;
    let chunk_size = number(&form.fields, "dzchunksize")?;
    let offset = number(&form.fields, "dzchunkbyteoffset")?;
    let max_size = config::get().app.max_upload_file_size;
    if total_size > max_size {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            "File exceeds the upload size limit",
        ));
    }
    let expected_count = total_size.div_ceil(CHUNK_SIZE).max(1);
    if chunk_size != CHUNK_SIZE
        || count != expected_count
        || index >= count
        || offset != index * CHUNK_SIZE
        || file.size() != total_size.saturating_sub(offset).min(CHUNK_SIZE)
    {
        return Err(bad("Invalid upload chunk"));
    }

    let meta = UploadMeta {
        parent,
        name: name.to_owned(),
        total_size,
        chunk_count: count,
    };
    let upload_root = config::get().app.cache_dir.join("upload-staging");
    fs::create_dir_all(&upload_root).await.map_err(io_error)?;
    let dir = upload_root.join(id);
    if index == 0 {
        match fs::create_dir(&dir).await {
            Ok(()) => {
                let bytes =
                    serde_json::to_vec(&meta).map_err(|_| bad("Invalid upload metadata"))?;
                fs::write(dir.join("meta.json"), bytes)
                    .await
                    .map_err(io_error)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(io_error(error)),
        }
    }
    let stored: UploadMeta = serde_json::from_slice(
        &fs::read(dir.join("meta.json"))
            .await
            .map_err(|_| bad("Upload session does not exist"))?,
    )
    .map_err(|_| bad("Invalid upload session"))?;
    if stored != meta {
        return Err(bad("Upload details changed"));
    }
    if dir.join("complete").exists() {
        return Ok(());
    }

    save_chunk(&dir, index, file.path(), file.size()).await?;

    if index + 1 == count {
        finish(&dir, &meta, id).await?;
    }
    Ok(())
}

async fn save_chunk(dir: &Path, index: u64, source: &Path, expected_size: u64) -> UploadResult {
    let chunk_path = dir.join(index.to_string());
    if let Ok(metadata) = fs::metadata(&chunk_path).await {
        return if metadata.len() == expected_size {
            Ok(())
        } else {
            Err(bad("Upload chunk conflicts with an existing chunk"))
        };
    }

    // Publish a chunk only after its full contents have been copied. A process
    // interruption can leave a .part file, but retries never see a partial chunk.
    let temporary = dir.join(format!(".{index}.{}.part", nanoid::nanoid!(12)));
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .await
        .map_err(io_error)?;
    let result = async {
        let mut input = fs::File::open(source).await?;
        let copied = tokio::io::copy(&mut input, &mut output).await?;
        output.flush().await?;
        if copied != expected_size {
            return Err(std::io::Error::other("upload chunk size changed"));
        }
        fs::hard_link(&temporary, &chunk_path).await
    }
    .await;
    _ = fs::remove_file(&temporary).await;
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if fs::metadata(&chunk_path).await.map_err(io_error)?.len() == expected_size {
                Ok(())
            } else {
                Err(bad("Upload chunk conflicts with an existing chunk"))
            }
        }
        Err(error) => Err(io_error(error)),
    }
}

async fn finish_direct(parent: &Path, name: &str, source: &Path) -> UploadResult {
    let destination = parent.join(name);
    if fs::symlink_metadata(&destination).await.is_ok() {
        return Err((
            StatusCode::CONFLICT,
            "A file or folder with that name already exists",
        ));
    }
    let temporary = parent.join(format!(".{}.uploading", nanoid::nanoid!(20)));
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .await
        .map_err(io_error)?;
    let result = async {
        let mut input = fs::File::open(source).await?;
        tokio::io::copy(&mut input, &mut output).await?;
        output.flush().await?;
        fs::hard_link(&temporary, &destination).await
    }
    .await;
    _ = fs::remove_file(&temporary).await;
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Err((
            StatusCode::CONFLICT,
            "A file or folder with that name already exists",
        )),
        Err(error) => Err(io_error(error)),
    }
}

async fn finish(dir: &Path, meta: &UploadMeta, id: &str) -> UploadResult {
    let destination = meta.parent.join(&meta.name);
    if fs::symlink_metadata(&destination).await.is_ok() {
        return Err((
            StatusCode::CONFLICT,
            "A file or folder with that name already exists",
        ));
    }
    let temporary = meta.parent.join(format!(".{id}.uploading"));
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .await
        .map_err(io_error)?;
    let result = async {
        let mut written = 0;
        for index in 0..meta.chunk_count {
            let mut chunk = fs::File::open(dir.join(index.to_string())).await?;
            written += tokio::io::copy(&mut chunk, &mut output).await?;
        }
        output.flush().await?;
        if written != meta.total_size {
            return Err(std::io::Error::other("assembled upload size mismatch"));
        }
        fs::hard_link(&temporary, &destination).await?;
        Ok::<(), std::io::Error>(())
    }
    .await;
    _ = fs::remove_file(&temporary).await;
    if let Err(error) = result {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            return Err((
                StatusCode::CONFLICT,
                "A file or folder with that name already exists",
            ));
        }
        return Err(io_error(error));
    }
    fs::write(dir.join("complete"), b"")
        .await
        .map_err(io_error)?;
    for index in 0..meta.chunk_count {
        _ = fs::remove_file(dir.join(index.to_string())).await;
    }
    Ok(())
}

pub(crate) async fn cleanup_stale() {
    let root = config::get().app.cache_dir.join("upload-staging");
    let Ok(mut entries) = fs::read_dir(root).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let Ok(metadata) = entry.metadata().await else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if SystemTime::now()
            .duration_since(modified)
            .is_ok_and(|age| age > Duration::from_hours(24))
        {
            _ = fs::remove_dir_all(entry.path()).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_names_and_ids_cannot_escape_their_directories() {
        for name in ["", ".", "..", "../outside", "sub/file", "sub\\file", "a\0b"] {
            assert!(validate_name(name).is_err(), "{name:?}");
        }
        assert!(validate_name("photo.JPG").is_ok());
        assert!(validate_id("ad2c6b45-7fab-4acd-8881-60e671100a15").is_ok());
        assert!(validate_id("../../outside").is_err());
    }

    #[tokio::test]
    async fn assembles_chunks_without_overwriting_a_file() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("files");
        let dir = root.path().join("chunks");
        fs::create_dir(&parent).await.unwrap();
        fs::create_dir(&dir).await.unwrap();
        fs::write(dir.join("0"), b"hello ").await.unwrap();
        fs::write(dir.join("1"), b"world").await.unwrap();
        let meta = UploadMeta {
            parent: parent.clone(),
            name: "note.txt".into(),
            total_size: 11,
            chunk_count: 2,
        };
        finish(&dir, &meta, "upload-id").await.unwrap();
        assert_eq!(
            fs::read(parent.join("note.txt")).await.unwrap(),
            b"hello world"
        );
        assert!(dir.join("complete").exists());
        assert!(!dir.join("0").exists());

        let replacement = root.path().join("replacement");
        fs::write(&replacement, b"overwrite").await.unwrap();
        assert_eq!(
            finish_direct(&parent, "note.txt", &replacement).await,
            Err((
                StatusCode::CONFLICT,
                "A file or folder with that name already exists"
            ))
        );
        assert_eq!(
            fs::read(parent.join("note.txt")).await.unwrap(),
            b"hello world"
        );
    }

    #[tokio::test]
    async fn retry_uses_a_complete_chunk_after_an_interruption() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let dir = root.path().join("chunks");
        fs::create_dir(&dir).await.unwrap();
        fs::write(&source, b"chunk data").await.unwrap();
        fs::write(dir.join(".0.interrupted.part"), b"partial")
            .await
            .unwrap();

        save_chunk(&dir, 0, &source, 10).await.unwrap();
        save_chunk(&dir, 0, &source, 10).await.unwrap();
        assert_eq!(fs::read(dir.join("0")).await.unwrap(), b"chunk data");
    }
}
