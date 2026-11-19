use std::{
    fmt::Write as _,
    fs::File,
    io::{self, Seek, Write},
    path::{Path, PathBuf},
};

use salvo::Response;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use walkdir::WalkDir;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use crate::error::{AppResult, Error};

const CHUNK_SIZE: usize = 64 * 1024;

struct ChannelWriter {
    sender: mpsc::Sender<io::Result<Vec<u8>>>,
}

impl Write for ChannelWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        for chunk in bytes.chunks(CHUNK_SIZE) {
            self.sender
                .blocking_send(Ok(chunk.to_vec()))
                .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "download disconnected"))?;
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn stream_directory(path: &Path, res: &mut Response) -> AppResult {
    let directory_name = path
        .file_name()
        .ok_or(Error::BadRequest("invalid directory path"))?
        .to_string_lossy();
    let filename = format!("{directory_name}.zip");

    set_zip_headers(res, &filename)?;

    let (sender, receiver) = mpsc::channel(2);
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let output = ChannelWriter {
            sender: sender.clone(),
        };
        if let Err(error) = write_directory_zip(&path, output) {
            let message = error.to_string();
            if sender.blocking_send(Err(io::Error::other(error))).is_err() {
                return; // The client disconnected.
            }
            tracing::warn!(path = %path.display(), error = %message, "directory download failed");
        }
    });

    // No Content-Length: Hyper frames this stream with chunked encoding on HTTP/1.1.
    res.stream(ReceiverStream::new(receiver));
    Ok(())
}

pub(super) fn stream_paths<P: AsRef<Path>>(
    paths: impl IntoIterator<Item = P>,
    res: &mut Response,
) -> AppResult {
    let paths = paths_with_same_parent(paths)?;
    set_zip_headers(res, "archive.zip")?;

    let (sender, receiver) = mpsc::channel(2);
    tokio::task::spawn_blocking(move || {
        let output = ChannelWriter {
            sender: sender.clone(),
        };
        if let Err(error) = write_paths_zip(&paths, output) {
            let message = error.to_string();
            if sender.blocking_send(Err(io::Error::other(error))).is_err() {
                return; // The client disconnected.
            }
            tracing::warn!(error = %message, "archive download failed");
        }
    });

    res.stream(ReceiverStream::new(receiver));
    Ok(())
}

fn paths_with_same_parent<P: AsRef<Path>>(
    paths: impl IntoIterator<Item = P>,
) -> AppResult<Vec<PathBuf>> {
    let mut paths = paths.into_iter();
    let first = paths.next().ok_or(Error::BadRequest("no paths selected"))?;
    let first = first.as_ref();
    let parent = first
        .parent()
        .filter(|_| first.file_name().is_some())
        .ok_or(Error::BadRequest("invalid selected path"))?;
    let mut selected = vec![first.to_path_buf()];

    for path in paths {
        let path = path.as_ref();
        if path.parent() != Some(parent) || path.file_name().is_none() {
            return Err(Error::BadRequest(
                "selected paths must have the same parent",
            ));
        }
        selected.push(path.to_path_buf());
    }

    Ok(selected)
}

fn set_zip_headers(res: &mut Response, filename: &str) -> AppResult {
    res.add_header("content-type", "application/zip", true)
        .map_err(|error| anyhow::anyhow!(error))?;
    res.add_header("content-disposition", attachment_header(filename), true)
        .map_err(|error| anyhow::anyhow!(error))?;
    res.add_header("cache-control", "no-store", true)
        .map_err(|error| anyhow::anyhow!(error))?;
    Ok(())
}

fn attachment_header(filename: &str) -> String {
    let fallback: String = filename
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ' ') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let mut encoded = String::new();
    for byte in filename.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            write!(&mut encoded, "%{byte:02X}").expect("writing to a String cannot fail");
        }
    }
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

fn write_directory_zip<W: Write>(root: &Path, output: W) -> anyhow::Result<W> {
    let directory_name = root
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("invalid directory path"))?
        .to_string_lossy();
    let mut archive = ZipWriter::new_stream(output);
    archive.add_directory(directory_name.as_ref(), SimpleFileOptions::default())?;

    add_path_entries(&mut archive, root, 1)?;

    Ok(archive.finish()?.into_inner())
}

fn write_paths_zip<W: Write>(paths: &[PathBuf], output: W) -> anyhow::Result<W> {
    let mut archive = ZipWriter::new_stream(output);
    for path in paths {
        add_path_entries(&mut archive, path, 0)?;
    }
    Ok(archive.finish()?.into_inner())
}

fn add_path_entries<W: Write + Seek>(
    archive: &mut ZipWriter<W>,
    root: &Path,
    min_depth: usize,
) -> anyhow::Result<()> {
    let root_name = root
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("invalid archive path"))?
        .to_string_lossy();
    let file_options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .large_file(true);

    if min_depth == 0 && std::fs::symlink_metadata(root)?.file_type().is_symlink() {
        let target = std::fs::read_link(root)?;
        archive.add_symlink(
            root_name.as_ref(),
            target.to_string_lossy(),
            SimpleFileOptions::default(),
        )?;
        return Ok(());
    }

    for entry in WalkDir::new(root).follow_links(false).min_depth(min_depth) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(root)?;
        let mut archive_name = root_name.to_string();
        for component in relative.components() {
            archive_name.push('/');
            archive_name.push_str(&component.as_os_str().to_string_lossy());
        }

        if entry.file_type().is_dir() {
            archive.add_directory(archive_name, SimpleFileOptions::default())?;
        } else if entry.file_type().is_symlink() {
            let target = std::fs::read_link(entry.path())?;
            archive.add_symlink(
                archive_name,
                target.to_string_lossy(),
                SimpleFileOptions::default(),
            )?;
        } else if entry.file_type().is_file() {
            let mut file = File::open(entry.path())?;
            archive.start_file(archive_name, file_options)?;
            io::copy(&mut file, &mut *archive)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read};

    use super::*;

    #[test]
    fn zip_contains_nested_files_and_empty_directories() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("my folder");
        std::fs::create_dir_all(root.join("nested/empty")).unwrap();
        std::fs::write(root.join("nested/file.txt"), "hello").unwrap();

        let bytes = write_directory_zip(&root, Vec::new()).unwrap();
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        assert!(archive.by_name("my folder/").unwrap().is_dir());
        assert!(archive.by_name("my folder/nested/empty/").unwrap().is_dir());
        let mut contents = String::new();
        archive
            .by_name("my folder/nested/file.txt")
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert_eq!(contents, "hello");
    }

    #[test]
    fn zip_contains_multiple_selected_files_and_directories() {
        let parent = tempfile::tempdir().unwrap();
        let file = parent.path().join("one.txt");
        let directory = parent.path().join("folder");
        std::fs::write(&file, "one").unwrap();
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("two.txt"), "two").unwrap();

        let paths = paths_with_same_parent([&file, &directory]).unwrap();
        let bytes = write_paths_zip(&paths, Vec::new()).unwrap();
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        assert_eq!(archive.len(), 3);
        assert!(archive.by_name("folder/").unwrap().is_dir());
        let mut contents = String::new();
        archive
            .by_name("one.txt")
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert_eq!(contents, "one");
        contents.clear();
        archive
            .by_name("folder/two.txt")
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert_eq!(contents, "two");
    }

    #[test]
    fn selected_paths_must_have_the_same_immediate_parent() {
        assert!(
            paths_with_same_parent([Path::new("/path/to/file"), Path::new("/path/to/other")])
                .is_ok()
        );
        for other in ["/path/to", "/path/to/file/subdir", "/different/other"] {
            assert!(matches!(
                paths_with_same_parent([Path::new("/path/to/file"), Path::new(other)]),
                Err(Error::BadRequest(
                    "selected paths must have the same parent"
                ))
            ));
        }
        assert!(matches!(
            paths_with_same_parent(Vec::<PathBuf>::new()),
            Err(Error::BadRequest("no paths selected"))
        ));
    }

    #[test]
    fn attachment_name_encodes_unicode_and_quotes() {
        assert_eq!(
            attachment_header("a \"café\".zip"),
            "attachment; filename=\"a _caf__.zip\"; filename*=UTF-8''a%20%22caf%C3%A9%22.zip"
        );
    }

    #[cfg(unix)]
    #[test]
    fn zip_keeps_symlinks_without_following_them() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("folder");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(parent.path().join("private.txt"), "outside").unwrap();
        std::os::unix::fs::symlink("../private.txt", root.join("link")).unwrap();

        let bytes = write_directory_zip(&root, Vec::new()).unwrap();
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        assert_eq!(archive.len(), 2);
        let mut target = String::new();
        archive
            .by_name("folder/link")
            .unwrap()
            .read_to_string(&mut target)
            .unwrap();
        assert_eq!(target, "../private.txt");
    }

    #[cfg(unix)]
    #[test]
    fn selected_symlink_is_archived_without_following_it() {
        let parent = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("private.txt"), "outside").unwrap();
        let link = parent.path().join("link");
        let file = parent.path().join("file.txt");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        std::fs::write(&file, "inside").unwrap();

        let paths = paths_with_same_parent([&link, &file]).unwrap();
        let bytes = write_paths_zip(&paths, Vec::new()).unwrap();
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        assert_eq!(archive.len(), 2);
        assert!(archive.by_name("link").is_ok());
        assert!(archive.by_name("link/private.txt").is_err());
    }
}
