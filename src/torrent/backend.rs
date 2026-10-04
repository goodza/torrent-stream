use anyhow::{bail, Result};
use serde::Deserialize;
use std::{
    ops::RangeInclusive,
    path::{Component, Path},
};

#[derive(Clone, Debug, Deserialize)]
pub struct TorrentFile {
    pub index: usize,
    pub path: String,
    pub size: u64,
    pub offset: u64,
    pub symlink: bool,
    pub pad: bool,
}
impl TorrentFile {
    pub fn is_video(&self) -> bool {
        !self.pad
            && self.size > 0
            && matches!(
                self.extension().as_str(),
                "mkv" | "mp4" | "webm" | "avi" | "mov" | "m4v" | "ts"
            )
    }
    pub fn extension(&self) -> String {
        Path::new(&self.path)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
    }
    pub fn pieces(&self, piece_length: u64) -> RangeInclusive<u32> {
        (self.offset / piece_length) as u32..=((self.offset + self.size - 1) / piece_length) as u32
    }
}
#[derive(Clone, Debug, Deserialize)]
pub struct Metadata {
    pub piece_length: u64,
    pub piece_count: u32,
    pub files: Vec<TorrentFile>,
}
impl Metadata {
    pub fn validate(&self) -> Result<()> {
        if self.piece_length == 0 || self.piece_count == 0 || self.piece_count > 5_000_000 {
            bail!("unsupported torrent piece geometry");
        }
        let mut previous_end = 0;
        let mut paths = std::collections::HashSet::new();
        for (index, file) in self.files.iter().enumerate() {
            validate_path(&file.path)?;
            if file.symlink {
                bail!("torrent symlinks are not allowed: {}", file.path);
            }
            if file.index != index || !paths.insert(&file.path) {
                bail!("invalid or duplicate torrent file entry");
            }
            let end = file
                .offset
                .checked_add(file.size)
                .ok_or_else(|| anyhow::anyhow!("file offset overflow"))?;
            if file.offset < previous_end || end > self.piece_length * u64::from(self.piece_count) {
                bail!("invalid file offsets");
            }
            previous_end = end;
        }
        Ok(())
    }
}
pub fn validate_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.contains('\\')
        || path.contains('\0')
        || path
            .split('/')
            .any(|s| s == ".." || s == "." || s.is_empty())
        || !Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
    {
        bail!("unsafe torrent path: {path:?}");
    }
    Ok(())
}
#[derive(Clone, Debug, Default, Deserialize)]
pub struct TorrentStatus {
    pub download_rate: u64,
    pub downloaded: u64,
    pub peers: u32,
    pub completed: Vec<bool>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct PriorityUpdate {
    pub piece: u32,
    pub priority: u32,
    pub deadline_ms: i32,
}
pub trait TorrentBackend: Send + Sync {
    fn metadata(&self) -> impl std::future::Future<Output = Result<Option<Metadata>>> + Send;
    fn select(&self, file: usize) -> impl std::future::Future<Output = Result<()>> + Send;
    fn priorities(
        &self,
        updates: Vec<PriorityUpdate>,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    fn status(&self) -> impl std::future::Future<Output = Result<TorrentStatus>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths_are_untrusted() {
        for path in [
            "../../escape",
            "/tmp/x",
            "a/../b",
            "a\\b",
            "a//b",
            "./a",
            "a\0b",
        ] {
            assert!(validate_path(path).is_err(), "{path:?}");
        }
        validate_path("Фильм/日本語.mkv").unwrap();
    }
    #[test]
    fn recognizes_videos_case_insensitively() {
        let f = TorrentFile {
            index: 0,
            path: "Movie.MKV".into(),
            size: 1,
            offset: 0,
            symlink: false,
            pad: false,
        };
        assert!(f.is_video());
    }
}
