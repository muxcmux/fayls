pub mod app;
pub mod config;
pub(crate) mod db;
pub(crate) mod error;
pub(crate) mod fswatch;
pub(crate) mod indexing;
pub(crate) mod jobs;
pub(crate) mod web;

use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub(crate) fn dir_size(path: &Path) -> u64 {
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

fn validate_folder_name(name: &str) -> Result<Vec<&str>, &'static str> {
    if name.is_empty() || name.trim().is_empty() {
        return Err("Enter a folder name");
    }
    if name.contains('\\') || name.chars().any(char::is_control) {
        return Err("Enter a valid folder name");
    }
    let segments = name.split('/').collect::<Vec<_>>();
    for segment in &segments {
        if segment.is_empty() || matches!(*segment, "." | "..") {
            return Err("Enter a valid folder name");
        }
        if *segment != segment.trim() {
            return Err("Folder names cannot start or end with whitespace");
        }
    }
    Ok(segments)
}

async fn validate_folder_parent(parent: &Path) -> Result<PathBuf, &'static str> {
    if !parent.is_absolute() {
        return Err("Choose a valid parent folder");
    }
    let canonical = tokio::fs::canonicalize(parent)
        .await
        .map_err(|_| "Parent folder does not exist")?;
    if !config::get()
        .app
        .canonicalized_sources()
        .iter()
        .any(|source| canonical.starts_with(source))
    {
        return Err("Parent folder is outside the configured sources");
    }
    let metadata = tokio::fs::metadata(&canonical)
        .await
        .map_err(|_| "Parent folder does not exist")?;
    if !metadata.is_dir() {
        return Err("Parent must be a folder");
    }
    if metadata.permissions().readonly() {
        return Err("Parent folder is not writable");
    }
    Ok(canonical)
}

pub(crate) async fn create_folder(parent: &Path, name: &str) -> Result<(), &'static str> {
    let segments = validate_folder_name(name)?;
    let parent = validate_folder_parent(parent).await?;
    create_folder_path(&parent, &segments).await
}

async fn create_folder_path(parent: &Path, segments: &[&str]) -> Result<(), &'static str> {
    let mut path = parent.to_path_buf();
    for (index, segment) in segments.iter().enumerate() {
        path.push(segment);
        match tokio::fs::create_dir(&path).await {
            Ok(()) => {}
            Err(error)
                if error.kind() == std::io::ErrorKind::AlreadyExists
                    && index + 1 < segments.len() =>
            {
                // Never follow an existing symlink while creating deeper folders.
                if !tokio::fs::symlink_metadata(&path)
                    .await
                    .is_ok_and(|metadata| metadata.is_dir())
                {
                    return Err("An existing path component is not a folder");
                }
            }
            Err(error) => {
                return Err(match error.kind() {
                    std::io::ErrorKind::AlreadyExists => {
                        "A file or folder with that name already exists"
                    }
                    std::io::ErrorKind::PermissionDenied => "Parent folder is not writable",
                    std::io::ErrorKind::InvalidInput => "Enter a valid folder name",
                    _ => "Could not create folder",
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod folder_tests {
    use super::*;

    #[test]
    fn rejects_unsafe_folder_names() {
        for name in [
            "",
            " ",
            ".",
            "..",
            "../escape",
            "/absolute",
            "a//b",
            "a/./b",
            "a/../b",
            "a/ b",
            "a\\b",
            "a\0b",
            " trailing ",
        ] {
            assert!(validate_folder_name(name).is_err(), "{name:?}");
        }
        assert!(validate_folder_name("My folder").is_ok());
        assert_eq!(
            validate_folder_name("folder/subfolder/test"),
            Ok(vec!["folder", "subfolder", "test"])
        );
    }

    #[tokio::test]
    async fn creates_nested_folders_without_overwriting_existing_paths() {
        let temp = tempfile::tempdir().unwrap();
        let segments = validate_folder_name("folder/subfolder/test").unwrap();
        create_folder_path(temp.path(), &segments).await.unwrap();
        assert!(temp.path().join("folder/subfolder/test").is_dir());
        create_folder_path(temp.path(), &["folder", "subfolder", "another"])
            .await
            .unwrap();
        assert!(temp.path().join("folder/subfolder/another").is_dir());
        assert_eq!(
            create_folder_path(temp.path(), &segments).await,
            Err("A file or folder with that name already exists")
        );

        tokio::fs::write(temp.path().join("file"), "contents")
            .await
            .unwrap();
        assert_eq!(
            create_folder_path(temp.path(), &["file", "child"]).await,
            Err("An existing path component is not a folder")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn refuses_to_follow_symlinks_in_nested_folder_names() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), temp.path().join("link")).unwrap();

        assert_eq!(
            create_folder_path(temp.path(), &["link", "child"]).await,
            Err("An existing path component is not a folder")
        );
        assert!(!outside.path().join("child").exists());
    }

    #[tokio::test]
    async fn rejects_invalid_parent_before_creation() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing");
        assert_eq!(
            create_folder(Path::new("relative"), "child").await,
            Err("Choose a valid parent folder")
        );
        assert_eq!(
            create_folder(&missing, "child").await,
            Err("Parent folder does not exist")
        );
        assert!(!missing.exists());
    }
}
