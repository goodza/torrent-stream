use crate::torrent::Metadata;
use anyhow::{bail, Context, Result};
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

/// Keep an exclusive lock alive until the torrent engine has been dropped.
pub(crate) struct DownloadCache {
    pub path: PathBuf,
    _lock: File,
}

impl DownloadCache {
    pub fn open(root: &Path, key: &str) -> Result<Self> {
        anyhow::ensure!(
            key.starts_with("v1-") && key.len() == 43 || key.starts_with("v2-") && key.len() == 67,
            "invalid torrent cache key"
        );
        anyhow::ensure!(
            key[3..].bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid torrent cache key"
        );
        let path = root.join(format!("torrent-{key}"));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e).context("create torrent cache"),
        }
        validate_tree(&path)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.join(".torrent-stream.lock"))?;
        lock.try_lock()
            .context("torrent download is already in use by another process")?;
        Ok(Self { path, _lock: lock })
    }
}

fn validate_tree(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if is_link(&meta) || !meta.is_dir() {
        bail!("unsafe download directory: {}", path.display());
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let meta = fs::symlink_metadata(entry.path())?;
        if is_link(&meta) || (!meta.is_dir() && !meta.is_file()) {
            bail!("unsafe cached download path: {}", entry.path().display());
        }
        if meta.is_dir() {
            validate_tree(&entry.path())?;
        }
    }
    Ok(())
}

fn is_link(meta: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        return meta.file_attributes() & 0x400 != 0; // FILE_ATTRIBUTE_REPARSE_POINT
    }
    #[cfg(not(windows))]
    meta.file_type().is_symlink()
}

/// Older versions retained random session folders. Copy candidate data so that
/// a filename collision can never overwrite the original. Libtorrent hashes
/// every imported piece before exposing it or requesting any missing payload.
pub(crate) fn import_sessions(root: &Path, target: &Path, metadata: &Metadata) -> Result<bool> {
    let mut imported = false;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_name().to_string_lossy().starts_with("session-")
            || validate_tree(&entry.path()).is_err()
        {
            continue;
        }
        for file in metadata.files.iter().filter(|f| !f.pad && f.size > 0) {
            let destination = target.join(&file.path);
            if destination.try_exists()? {
                continue;
            }
            let source = entry.path().join(&file.path);
            let Ok(attr) = fs::symlink_metadata(&source) else {
                continue;
            };
            if !attr.is_file() || attr.len() != file.size {
                continue;
            }
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&source, &destination).context("import previous download")?;
            imported = true;
        }
    }
    Ok(imported)
}

#[cfg(test)]
mod tests {
    use super::*;
    const KEY: &str = "v1-0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn reuses_directory_and_excludes_concurrent_writers() {
        let root = tempfile::tempdir().unwrap();
        let cache = DownloadCache::open(root.path(), KEY).unwrap();
        fs::write(cache.path.join("movie.mkv"), b"saved").unwrap();
        assert!(DownloadCache::open(root.path(), KEY).is_err());
        let path = cache.path.clone();
        drop(cache);
        let cache = DownloadCache::open(root.path(), KEY).unwrap();
        assert_eq!(cache.path, path);
        assert_eq!(fs::read(cache.path.join("movie.mkv")).unwrap(), b"saved");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_cache_symlinks() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let cache = DownloadCache::open(root.path(), KEY).unwrap();
        let path = cache.path.clone();
        drop(cache);
        symlink(outside.path(), path.join("redirect")).unwrap();
        assert!(DownloadCache::open(root.path(), KEY).is_err());
        fs::remove_file(path.join("redirect")).unwrap();
        fs::remove_dir_all(&path).unwrap();
        symlink(outside.path(), &path).unwrap();
        assert!(DownloadCache::open(root.path(), KEY).is_err());
    }
}
