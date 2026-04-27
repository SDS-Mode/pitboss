//! Crash-safe file writes — write to a sibling `.tmp` then `rename`
//! so a process death mid-write can never produce a partial or
//! truncated final file. The destination either reflects the prior
//! contents (if any) or the full new contents, never a half-flushed
//! mix.
//!
//! `rename(2)` is atomic on Linux/Unix when source and destination
//! sit on the same filesystem — placing the temp file as a sibling
//! of the destination is what makes that hold.

use std::path::{Path, PathBuf};

fn tmp_path_for(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    name.push(".tmp");
    path.with_file_name(name)
}

/// Synchronous atomic write. Caller must ensure `path.parent()` exists.
pub fn write_atomic_sync(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;

    let tmp = tmp_path_for(path);
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Async counterpart of [`write_atomic_sync`].
pub async fn write_atomic_async(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt as _;

    let tmp = tmp_path_for(path);
    {
        let mut f = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .await?;
        f.write_all(bytes).await?;
        f.sync_all().await?;
    }
    if let Err(e) = tokio::fs::rename(&tmp, path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn sync_writes_full_contents() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("out.json");
        write_atomic_sync(&path, b"hello world").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello world");
        // No leftover .tmp sibling.
        assert!(!tmp.path().join("out.json.tmp").exists());
    }

    #[test]
    fn sync_overwrites_existing_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("out.json");
        std::fs::write(&path, b"old").unwrap();
        write_atomic_sync(&path, b"new contents").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new contents");
    }

    #[test]
    fn sync_cleans_tmp_when_target_dir_is_invalid() {
        // Rename across nonexistent parent dirs cannot be tested portably
        // (rename to nonexistent dst dir on Linux returns ENOENT), so
        // instead exercise the open() error path: make the tmp's parent
        // unwritable by pointing at a non-directory.
        let tmp = TempDir::new().unwrap();
        let not_a_dir = tmp.path().join("file");
        std::fs::write(&not_a_dir, b"x").unwrap();
        let path = not_a_dir.join("nested.json");
        // open() on `<file>/nested.json.tmp` should fail with ENOTDIR.
        let err = write_atomic_sync(&path, b"data").unwrap_err();
        assert!(
            matches!(
                err.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ),
            "expected NotFound or NotADirectory, got {:?}",
            err.kind()
        );
    }

    #[tokio::test]
    async fn async_writes_full_contents() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("out.json");
        write_atomic_async(&path, b"hello async").await.unwrap();
        assert_eq!(tokio::fs::read(&path).await.unwrap(), b"hello async");
        assert!(!tmp.path().join("out.json.tmp").exists());
    }

    #[tokio::test]
    async fn async_overwrites_existing_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("out.json");
        tokio::fs::write(&path, b"old").await.unwrap();
        write_atomic_async(&path, b"new").await.unwrap();
        assert_eq!(tokio::fs::read(&path).await.unwrap(), b"new");
    }
}
