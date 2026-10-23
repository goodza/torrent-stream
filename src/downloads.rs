use crate::torrent::Metadata;
use anyhow::{bail, Context, Result};
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

/// Keep an exclusive lock alive until the torrent engine has been dropped.
pub struct DownloadCache {
    pub record: PathBuf,
    pub path: PathBuf,
    _lock: File,
    _legacy_lock: File,
}

impl DownloadCache {
    pub fn open(root: &Path, key: &str) -> Result<Self> {
        validate_key(key)?;
        let record = control(root)?.join("records").join(key);
        private_dir(&record)?;
        let lock = lock_key(root, key)?;
        let path = payload_path(root, key)?;
        let legacy = root.join(format!("torrent-{key}"));
        if path != legacy && !path.try_exists()? && legacy.try_exists()? {
            validate_tree(&legacy)?;
            fs::rename(&legacy, &path)?;
        }
        let builder = directory_builder(false);
        match builder.create(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e).context("create torrent cache"),
        }
        validate_tree(&path)?;
        let legacy_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.join(".torrent-stream.lock"))?;
        legacy_lock
            .try_lock()
            .context("torrent download is already in use by another process")?;
        Ok(Self {
            path,
            record,
            _lock: lock,
            _legacy_lock: legacy_lock,
        })
    }

    /// Rename only while the torrent engine is stopped, keeping both locks held.
    pub fn name_folder(&mut self, root: &Path, key: &str, title: &str) -> Result<()> {
        if self.record.join("folder.json").exists() {
            return Ok(());
        }
        let naming_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(control(root)?.join("locks/folders.lock"))?;
        naming_lock.lock()?;
        let name = folder_name(title);
        let mut candidate = name.clone();
        let mut suffix = 0;
        while root.join(&candidate).try_exists()? {
            suffix += 1;
            candidate = if suffix == 1 {
                format!("{name} ({key})")
            } else {
                format!("{name} ({key}-{suffix})")
            };
        }
        // Persist first so an interrupted rename can recover from the legacy path.
        atomic_write(
            &self.record.join("folder.json"),
            &serde_json::to_vec(&candidate)?,
        )?;
        let destination = root.join(candidate);
        fs::rename(&self.path, &destination).context("rename download folder")?;
        self.path = destination;
        Ok(())
    }
}

fn folder_name(title: &str) -> String {
    let mut name: String = title
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    while name.len() > 160 {
        name.pop();
    }
    let name = name.trim().trim_end_matches(['.', ' ']);
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.starts_with('.')
        || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit())
    {
        format!("Movie{name}")
    } else {
        name.to_owned()
    }
}

fn payload_path(root: &Path, key: &str) -> Result<PathBuf> {
    let mapping = root
        .join(".torrent-stream/records")
        .join(key)
        .join("folder.json");
    match fs::read(&mapping) {
        Ok(bytes) => {
            let name: String =
                serde_json::from_slice(&bytes).context("invalid download folder record")?;
            anyhow::ensure!(
                valid_folder_component(&name),
                "invalid download folder name"
            );
            Ok(root.join(name))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(root.join(format!("torrent-{key}")))
        }
        Err(e) => Err(e.into()),
    }
}

fn valid_folder_component(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.ends_with(['.', ' '])
        && !name
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
}

pub fn validate_key(key: &str) -> Result<()> {
    anyhow::ensure!(
        key.starts_with("v1-") && key.len() == 43 || key.starts_with("v2-") && key.len() == 67,
        "invalid torrent cache key"
    );
    anyhow::ensure!(
        key[3..].bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid torrent cache key"
    );
    Ok(())
}

fn directory_builder(recursive: bool) -> fs::DirBuilder {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(recursive);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
}
fn private_dir(path: &Path) -> Result<()> {
    if !path.exists() {
        directory_builder(true).create(path)?;
    }
    validate_tree(path)
}

pub fn control(root: &Path) -> Result<PathBuf> {
    let path = root.join(".torrent-stream");
    private_dir(&path)?;
    Ok(path)
}
fn lock_key(root: &Path, key: &str) -> Result<File> {
    validate_key(key)?;
    let locks = control(root)?.join("locks");
    private_dir(&locks)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(locks.join(format!("{key}.lock")))?;
    lock.try_lock()
        .context("torrent download is already in use by another process")?;
    Ok(lock)
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|e| e.error)
        .context("save state atomically")?;
    Ok(())
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct CacheState {
    pub title: String,
    pub last_used: u64,
    pub bookmarks: std::collections::BTreeMap<String, Bookmark>,
}
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Bookmark {
    pub position: f64,
    pub duration: Option<f64>,
    pub size: u64,
}
impl CacheState {
    pub fn read(record: &Path) -> Self {
        match fs::read(record.join("state.json")) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(state) => state,
                Err(error) => {
                    eprintln!("Ignoring invalid playback history: {error}");
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }
    pub fn save(&mut self, record: &Path) -> Result<()> {
        self.last_used = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        atomic_write(
            &record.join("state.json"),
            &serde_json::to_vec_pretty(self)?,
        )
    }
    pub fn position(&self, file: &crate::torrent::TorrentFile) -> Option<f64> {
        self.bookmarks
            .get(&file.path)
            .filter(|b| b.size == file.size)
            .and_then(|b| valid_position(b.position, b.duration))
    }
    pub fn update(
        &mut self,
        file: &crate::torrent::TorrentFile,
        state: &crate::mpv::PlaybackState,
    ) {
        if state.end_reason.as_deref() == Some("eof") {
            self.bookmarks.remove(&file.path);
        } else if let Some(position) = state.time_pos.filter(|p| p.is_finite() && *p >= 0.0) {
            if valid_position(position, state.duration).is_some() {
                self.bookmarks.insert(
                    file.path.clone(),
                    Bookmark {
                        position,
                        duration: state.duration,
                        size: file.size,
                    },
                );
            } else {
                self.bookmarks.remove(&file.path);
            }
        }
    }
}
fn valid_position(position: f64, duration: Option<f64>) -> Option<f64> {
    (position.is_finite()
        && position >= 10.0
        && !duration
            .filter(|d| d.is_finite() && *d > 0.0)
            .is_some_and(|d| position >= d - 30.0))
    .then_some(position)
}

pub fn manage(root: &Path, action: &crate::cli::CacheAction) -> Result<()> {
    use crate::cli::CacheAction;
    let mut ids = Vec::new();
    if root.exists() {
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            if let Some(key) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.strip_prefix("torrent-"))
            {
                if validate_key(key).is_ok() {
                    ids.push(key.to_owned());
                }
            }
        }
    }
    let records = root.join(".torrent-stream/records");
    if records.try_exists()? {
        validate_tree(&records)?;
        for entry in fs::read_dir(&records)? {
            let key = entry?.file_name().to_string_lossy().into_owned();
            if validate_key(&key).is_ok() && payload_path(root, &key)?.try_exists()? {
                ids.push(key);
            }
        }
    }
    ids.sort();
    ids.dedup();
    if matches!(action, CacheAction::List) {
        if ids.is_empty() {
            println!("No saved downloads.");
        }
        for key in ids {
            let path = payload_path(root, &key)?;
            validate_tree(&path)?;
            let record = root.join(".torrent-stream/records").join(&key);
            let state = CacheState::read(&record);
            let progress = state
                .bookmarks
                .iter()
                .map(|(file, b)| {
                    format!(
                        "{} @ {}",
                        file.escape_debug(),
                        crate::ui::clock(Some(b.position))
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let (logical, allocated) = tree_size(&path)?;
            let age = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs()
                .saturating_sub(state.last_used);
            let last_used = if state.last_used == 0 {
                "unknown".into()
            } else {
                format!("{age}s ago")
            };
            let allocation = allocated
                .map(|n| format!(", {n} bytes on disk"))
                .unwrap_or_default();
            println!(
                "{key}  {logical} bytes{allocation}  last used {last_used}  {}  {progress}",
                state.title.escape_debug()
            );
        }
        return Ok(());
    }
    let yes = match action {
        CacheAction::Remove { id, yes } => {
            let id = id.strip_prefix("torrent-").unwrap_or(id);
            validate_key(id)?;
            anyhow::ensure!(ids.iter().any(|key| key == id), "download not found: {id}");
            ids.retain(|key| key == id);
            *yes
        }
        CacheAction::Clear { yes } => *yes,
        CacheAction::List => unreachable!(),
    };
    if ids.is_empty() {
        println!("No saved downloads.");
        return Ok(());
    }
    if !yes {
        use std::io::{IsTerminal, Write};
        anyhow::ensure!(
            std::io::stdin().is_terminal(),
            "use --yes to remove downloads without an interactive terminal"
        );
        print!(
            "Remove {} saved download(s) and their playback history? [y/N] ",
            ids.len()
        );
        std::io::stdout().flush()?;
        let mut reply = String::new();
        std::io::stdin().read_line(&mut reply)?;
        if !reply.trim().eq_ignore_ascii_case("y") {
            return Ok(());
        }
    }
    // Preflight every entry before clear, avoiding partial deletion on active caches.
    let mut guards = Vec::new();
    for key in &ids {
        let lock = lock_key(root, key)?;
        let path = payload_path(root, key)?;
        validate_tree(&path)?;
        let legacy_path = path.join(".torrent-stream.lock");
        if legacy_path.exists() {
            let legacy = OpenOptions::new()
                .read(true)
                .write(true)
                .open(legacy_path)?;
            legacy.try_lock().context("download is active")?;
            drop(legacy); // Windows cannot delete an open lock file.
        }
        guards.push(lock);
    }
    for key in ids {
        fs::remove_dir_all(payload_path(root, &key)?)?;
        let record = root.join(".torrent-stream/records").join(&key);
        if record.exists() {
            validate_tree(&record)?;
            fs::remove_dir_all(record)?;
        }
        println!("Removed {key}");
    }
    Ok(())
}
fn tree_size(path: &Path) -> Result<(u64, Option<u64>)> {
    let mut size = 0;
    #[cfg(unix)]
    let mut allocated = Some(0);
    #[cfg(not(unix))]
    let mut allocated = None;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let (nested, disk) = tree_size(&entry.path())?;
            size += nested;
            allocated = allocated.zip(disk).map(|(a, b)| a + b);
        } else if entry.file_name() != ".torrent-stream.lock" {
            let metadata = entry.metadata()?;
            size += metadata.len();
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                allocated = allocated.map(|a| a + metadata.blocks() * 512);
            }
        }
    }
    Ok((size, allocated))
}

pub fn validate_tree(path: &Path) -> Result<()> {
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
        meta.file_attributes() & 0x400 != 0 // FILE_ATTRIBUTE_REPARSE_POINT
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
    fn movie_folders_preserve_payload_resume_and_cache_management() {
        let root = tempfile::tempdir().unwrap();
        let mut cache = DownloadCache::open(root.path(), KEY).unwrap();
        let old = cache.path.clone();
        fs::write(cache.path.join("Movie.mkv"), b"saved").unwrap();
        cache.name_folder(root.path(), KEY, "Movie (2026)").unwrap();
        assert_eq!(cache.path, root.path().join("Movie (2026)"));
        assert!(!old.exists());
        assert!(DownloadCache::open(root.path(), KEY).is_err());
        drop(cache);
        let cache = DownloadCache::open(root.path(), KEY).unwrap();
        assert_eq!(fs::read(cache.path.join("Movie.mkv")).unwrap(), b"saved");
        assert!(manage(root.path(), &crate::cli::CacheAction::Clear { yes: true }).is_err());
        drop(cache);
        manage(
            root.path(),
            &crate::cli::CacheAction::Remove {
                id: KEY.into(),
                yes: true,
            },
        )
        .unwrap();
        assert!(!root.path().join("Movie (2026)").exists());
    }

    #[test]
    fn movie_folder_collisions_preserve_existing_directories() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("Movie")).unwrap();
        fs::write(root.path().join("Movie/keep"), b"original").unwrap();
        let mut cache = DownloadCache::open(root.path(), KEY).unwrap();
        cache.name_folder(root.path(), KEY, "Movie").unwrap();
        assert_eq!(cache.path, root.path().join(format!("Movie ({KEY})")));
        assert_eq!(
            fs::read(root.path().join("Movie/keep")).unwrap(),
            b"original"
        );
    }

    #[test]
    fn interrupted_folder_rename_recovers_on_open() {
        let root = tempfile::tempdir().unwrap();
        let cache = DownloadCache::open(root.path(), KEY).unwrap();
        fs::write(cache.path.join("movie.mkv"), b"saved").unwrap();
        fs::write(cache.record.join("folder.json"), br#""Movie""#).unwrap();
        drop(cache);
        let cache = DownloadCache::open(root.path(), KEY).unwrap();
        assert_eq!(cache.path, root.path().join("Movie"));
        assert_eq!(fs::read(cache.path.join("movie.mkv")).unwrap(), b"saved");
    }

    #[test]
    fn movie_names_are_safe_and_unicode_is_preserved() {
        assert_eq!(folder_name("Фильм (2026)"), "Фильм (2026)");
        for title in [
            "../Movie",
            "CON",
            "",
            ".torrent-stream",
            "Movie: Part 2?",
            "..",
        ] {
            assert!(valid_folder_component(&folder_name(title)), "{title}");
        }
        let root = tempfile::tempdir().unwrap();
        let cache = DownloadCache::open(root.path(), KEY).unwrap();
        fs::write(cache.record.join("folder.json"), br#""../outside""#).unwrap();
        drop(cache);
        assert!(DownloadCache::open(root.path(), KEY).is_err());
    }

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

    #[test]
    fn bookmarks_ignore_early_and_finished_positions_and_isolate_files() {
        let file = crate::torrent::TorrentFile {
            index: 0,
            path: "episode.mkv".into(),
            size: 100,
            offset: 0,
            symlink: false,
            pad: false,
        };
        let mut history = CacheState::default();
        let mut state = crate::mpv::PlaybackState {
            time_pos: Some(45.0),
            duration: Some(120.0),
            ..Default::default()
        };
        history.update(&file, &state);
        assert_eq!(history.position(&file), Some(45.0));
        assert_eq!(
            history.position(&crate::torrent::TorrentFile {
                path: "other.mkv".into(),
                ..file.clone()
            }),
            None
        );
        state.time_pos = Some(5.0);
        history.update(&file, &state);
        assert_eq!(history.position(&file), None);
        state.time_pos = Some(100.0);
        history.update(&file, &state);
        assert_eq!(history.position(&file), None);
        state.time_pos = Some(45.0);
        history.update(&file, &state);
        state.end_reason = Some("eof".into());
        history.update(&file, &state);
        assert_eq!(history.position(&file), None);
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
