use salvo::prelude::*;
use salvo::{http::HeaderValue, http::header};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use walkdir::{DirEntry, WalkDir};

#[derive(serde::Serialize)]
enum FsEntryKind {
    File,
    Symlink,
    Directory,
}

impl FsEntryKind {
    fn rank(&self) -> u8 {
        match self {
            FsEntryKind::Directory => 0,
            _ => 1,
        }
    }
}
#[derive(serde::Serialize)]
struct FsEntry {
    path: PathBuf,
    kind: FsEntryKind,
    last_modified: Option<jiff::Zoned>,
    size: u64,
}

impl From<DirEntry> for FsEntry {
    fn from(entry: DirEntry) -> Self {
        let metadata = entry.metadata().ok();
        let kind = if entry.file_type().is_dir() {
            FsEntryKind::Directory
        } else if entry.file_type().is_symlink() {
            FsEntryKind::Symlink
        } else {
            FsEntryKind::File
        };
        let size = if entry.file_type().is_dir() {
            dir_size(entry.path())
        } else {
            metadata.as_ref().map_or(0, std::fs::Metadata::len)
        };
        let last_modified = metadata.and_then(|m| zoned_from_systemtime(m.modified().ok()?));

        FsEntry {
            kind,
            size,
            last_modified,
            path: entry.into_path(),
        }
    }
}

fn zoned_from_systemtime(system_time: std::time::SystemTime) -> Option<jiff::Zoned> {
    let secs = system_time.duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(jiff::Zoned::new(
        jiff::Timestamp::from_second(secs.try_into().ok()?).ok()?,
        jiff::tz::TimeZone::system(),
    ))
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

fn list_entries(path: &Path) -> Json<Vec<FsEntry>> {
    let mut items: Vec<FsEntry> = Vec::new();

    for entry in WalkDir::new(path).min_depth(1).max_depth(1) {
        match entry {
            Ok(entry) => items.push(entry.into()),
            Err(error) => tracing::warn!("failed to read directory entry: {error}"),
        }
    }

    items.sort_unstable_by(|a, b| match a.kind.rank().cmp(&b.kind.rank()) {
        std::cmp::Ordering::Equal => a.path.cmp(&b.path),
        rest => rest,
    });

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
async fn list_files_handler(req: &mut Request, res: &mut Response) {
    let Some(path) = req.query::<String>("path") else {
        res.status_code(StatusCode::BAD_REQUEST);
        return;
    };

    let requested_path = Path::new(&path);
    if !requested_path.exists() {
        res.status_code(StatusCode::NOT_FOUND);
        return;
    }

    res.render(list_entries(requested_path));
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().init();

    let acceptor = TcpListener::new("127.0.0.1:8080").bind().await;

    let router = Router::new()
        .hoop(force_json_format)
        .get(list_files_handler);

    Server::new(acceptor).serve(router).await;
}
