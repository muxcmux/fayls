pub mod app;
pub mod config;
pub(crate) mod db;
pub(crate) mod error;
pub(crate) mod fswatch;
pub(crate) mod indexing;
pub(crate) mod jobs;
pub(crate) mod upload;
pub(crate) mod web;

use std::{
    collections::HashSet,
    io,
    path::{Path, PathBuf},
};
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

pub(crate) async fn validate_folder_parent(parent: &Path) -> Result<PathBuf, &'static str> {
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

pub(crate) async fn path_is_in_configured_sources(path: &Path) -> bool {
    path_is_in_sources(path, config::get().app.canonicalized_sources()).await
}

async fn path_is_in_sources(path: &Path, sources: &HashSet<PathBuf>) -> bool {
    if !path.is_absolute() {
        return false;
    }

    let resolved = match tokio::fs::canonicalize(path).await {
        Ok(resolved) => resolved,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // A rename destination may not exist yet; resolve its parent.
            let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
                return false;
            };
            let Ok(parent) = tokio::fs::canonicalize(parent).await else {
                return false;
            };
            parent.join(name)
        }
        Err(_) => return false,
    };

    sources.iter().any(|source| resolved.starts_with(source))
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

pub(crate) async fn rename_without_overwriting(
    path: &Path,
    new_path: &Path,
) -> std::io::Result<()> {
    match tokio::fs::symlink_metadata(new_path).await {
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "destination path already exists",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tokio::fs::rename(path, new_path).await
        }
        Err(error) => Err(error),
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum MoveError {
    #[error("Select at least one path to move")]
    NoPaths,
    #[error("Choose a destination folder")]
    InvalidTarget,
    #[error("A path to move does not exist or is invalid")]
    InvalidSource,
    #[error("Cannot move a folder into itself or one of its subfolders")]
    IntoSelf,
    #[error("Cannot move a folder and one of its contents together")]
    OverlappingSources,
    #[error("A file or folder with that name already exists")]
    AlreadyExists,
    #[error(transparent)]
    Io(#[from] io::Error),
}

pub(crate) async fn move_paths(paths: &[PathBuf], target: &Path) -> Result<(), MoveError> {
    if paths.is_empty() {
        return Err(MoveError::NoPaths);
    }
    if !target.is_absolute() {
        return Err(MoveError::InvalidTarget);
    }

    let target = tokio::fs::canonicalize(target)
        .await
        .map_err(|error| match error.kind() {
            io::ErrorKind::NotFound | io::ErrorKind::NotADirectory => MoveError::InvalidTarget,
            _ => MoveError::Io(error),
        })?;
    if !tokio::fs::metadata(&target).await?.is_dir() {
        return Err(MoveError::InvalidTarget);
    }

    let mut moves = Vec::with_capacity(paths.len());
    let mut destinations = HashSet::with_capacity(paths.len());
    let mut directories = Vec::new();
    for (index, path) in paths.iter().enumerate() {
        if !path.is_absolute() {
            return Err(MoveError::InvalidSource);
        }
        let name = path.file_name().ok_or(MoveError::InvalidSource)?;
        let metadata =
            tokio::fs::symlink_metadata(path)
                .await
                .map_err(|error| match error.kind() {
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory => {
                        MoveError::InvalidSource
                    }
                    _ => MoveError::Io(error),
                })?;
        if metadata.is_dir() {
            let canonical = tokio::fs::canonicalize(path).await?;
            if target.starts_with(&canonical) {
                return Err(MoveError::IntoSelf);
            }
            directories.push((index, canonical));
        }

        let destination = target.join(name);
        if !destinations.insert(destination.clone()) {
            return Err(MoveError::AlreadyExists);
        }
        match tokio::fs::symlink_metadata(&destination).await {
            Ok(_) => return Err(MoveError::AlreadyExists),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(MoveError::Io(error)),
        }
        moves.push((path, destination));
    }

    if directories.iter().any(|(index, parent)| {
        paths
            .iter()
            .enumerate()
            .any(|(other_index, path)| other_index != *index && path.starts_with(parent))
    }) {
        return Err(MoveError::OverlappingSources);
    }

    for (path, destination) in moves {
        match rename_without_overwriting(path, &destination).await {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return Err(MoveError::AlreadyExists);
            }
            Err(error) => return Err(MoveError::Io(error)),
        }
    }
    Ok(())
}

pub(crate) async fn remove_path(path: &Path) -> std::io::Result<()> {
    let metadata = tokio::fs::symlink_metadata(path).await?;
    if metadata.is_dir() {
        tokio::fs::remove_dir_all(path).await
    } else {
        tokio::fs::remove_file(path).await
    }
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

#[cfg(test)]
mod source_path_tests {
    use super::*;

    #[tokio::test]
    async fn checks_existing_paths_and_new_names_against_sources() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let outside = temp.path().join("source-backup");
        tokio::fs::create_dir(&source).await.unwrap();
        tokio::fs::create_dir(&outside).await.unwrap();
        tokio::fs::write(source.join("file"), "inside")
            .await
            .unwrap();
        tokio::fs::write(outside.join("file"), "outside")
            .await
            .unwrap();
        let sources = HashSet::from([tokio::fs::canonicalize(&source).await.unwrap()]);

        assert!(path_is_in_sources(&source, &sources).await);
        assert!(path_is_in_sources(&source.join("file"), &sources).await);
        assert!(path_is_in_sources(&source.join("new-name"), &sources).await);
        assert!(!path_is_in_sources(&outside.join("file"), &sources).await);
        assert!(!path_is_in_sources(&outside.join("new-name"), &sources).await);
        assert!(!path_is_in_sources(&source.join("../source-backup/file"), &sources).await);
        assert!(!path_is_in_sources(Path::new("relative"), &sources).await);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_symlink_paths_that_resolve_outside_sources() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let outside = temp.path().join("outside");
        tokio::fs::create_dir(&source).await.unwrap();
        tokio::fs::create_dir(&outside).await.unwrap();
        std::os::unix::fs::symlink(&outside, source.join("external")).unwrap();
        std::os::unix::fs::symlink("missing", source.join("broken")).unwrap();
        let sources = HashSet::from([tokio::fs::canonicalize(&source).await.unwrap()]);

        assert!(!path_is_in_sources(&source.join("external"), &sources).await);
        assert!(!path_is_in_sources(&source.join("external/new-name"), &sources).await);
        assert!(path_is_in_sources(&source.join("broken"), &sources).await);
    }
}

#[cfg(test)]
mod move_tests {
    use super::*;

    #[tokio::test]
    async fn moves_files_and_directories() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let target = temp.path().join("target");
        tokio::fs::create_dir_all(source.join("folder"))
            .await
            .unwrap();
        tokio::fs::create_dir(&target).await.unwrap();
        tokio::fs::write(source.join("file"), "file contents")
            .await
            .unwrap();
        tokio::fs::write(source.join("folder/child"), "child contents")
            .await
            .unwrap();

        move_paths(&[source.join("file"), source.join("folder")], &target)
            .await
            .unwrap();

        assert_eq!(
            tokio::fs::read_to_string(target.join("file"))
                .await
                .unwrap(),
            "file contents"
        );
        assert_eq!(
            tokio::fs::read_to_string(target.join("folder/child"))
                .await
                .unwrap(),
            "child contents"
        );
        assert!(!source.join("file").exists());
        assert!(!source.join("folder").exists());
    }

    #[tokio::test]
    async fn rejects_self_descendant_and_overlapping_sources() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("parent");
        let child = parent.join("child");
        let target = temp.path().join("target");
        tokio::fs::create_dir_all(&child).await.unwrap();
        tokio::fs::create_dir(&target).await.unwrap();

        assert!(matches!(
            move_paths(&[parent.clone()], &parent).await,
            Err(MoveError::IntoSelf)
        ));
        assert!(matches!(
            move_paths(&[parent.clone()], &child).await,
            Err(MoveError::IntoSelf)
        ));
        assert!(matches!(
            move_paths(&[parent.clone(), child.clone()], &target).await,
            Err(MoveError::OverlappingSources)
        ));
        assert!(child.is_dir());

        #[cfg(unix)]
        {
            let alias = temp.path().join("alias");
            std::os::unix::fs::symlink(&child, &alias).unwrap();
            assert!(matches!(
                move_paths(&[parent], &alias).await,
                Err(MoveError::IntoSelf)
            ));
        }
    }

    #[tokio::test]
    async fn a_single_noncanonical_source_is_not_an_overlap() {
        let temp = tempfile::tempdir().unwrap();
        let child = temp.path().join("parent/child");
        let target = temp.path().join("target");
        tokio::fs::create_dir_all(&child).await.unwrap();
        tokio::fs::create_dir(&target).await.unwrap();

        let same_child = child.join("../child");
        move_paths(&[same_child], &target).await.unwrap();

        assert!(target.join("child").is_dir());
        assert!(!child.exists());
    }

    #[tokio::test]
    async fn refuses_collisions_before_moving_anything() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let target = temp.path().join("target");
        tokio::fs::create_dir(&source).await.unwrap();
        tokio::fs::create_dir(&target).await.unwrap();
        tokio::fs::write(source.join("free"), "keep").await.unwrap();
        tokio::fs::write(source.join("taken"), "source")
            .await
            .unwrap();
        tokio::fs::write(target.join("taken"), "target")
            .await
            .unwrap();

        assert!(matches!(
            move_paths(&[source.join("free"), source.join("taken")], &target).await,
            Err(MoveError::AlreadyExists)
        ));
        assert_eq!(
            tokio::fs::read_to_string(target.join("taken"))
                .await
                .unwrap(),
            "target"
        );
        assert!(source.join("free").exists());
        assert!(source.join("taken").exists());

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("missing", target.join("free")).unwrap();
            assert!(matches!(
                move_paths(&[source.join("free")], &target).await,
                Err(MoveError::AlreadyExists)
            ));
            assert!(source.join("free").exists());
        }
    }

    #[tokio::test]
    async fn requires_existing_directory_and_source() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        tokio::fs::write(&source, "source").await.unwrap();

        assert!(matches!(
            move_paths(&[], temp.path()).await,
            Err(MoveError::NoPaths)
        ));
        assert!(matches!(
            move_paths(&[source.clone()], &source).await,
            Err(MoveError::InvalidTarget)
        ));
        assert!(matches!(
            move_paths(&[source.clone()], &temp.path().join("missing")).await,
            Err(MoveError::InvalidTarget)
        ));
        assert!(matches!(
            move_paths(&[temp.path().join("missing")], temp.path()).await,
            Err(MoveError::InvalidSource)
        ));
    }
}
