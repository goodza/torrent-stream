use crate::{
    mpv::PlaybackState,
    torrent::{Metadata, TorrentFile},
};
use anyhow::{bail, Context, Result};
use std::io::{self, IsTerminal, Write};

pub fn list(metadata: &Metadata) {
    for f in &metadata.files {
        println!(
            "{:>3}  {:>10.1} MiB  {}{}",
            f.index,
            f.size as f64 / 1048576.0,
            f.path.escape_debug(),
            if f.pad { " [padding]" } else { "" }
        );
    }
}
pub fn select(metadata: &Metadata, index: Option<usize>) -> Result<TorrentFile> {
    if let Some(index) = index {
        let file = metadata
            .files
            .get(index)
            .context("--file index is out of range")?;
        if file.pad || file.size == 0 {
            bail!("cannot play a padding or empty file");
        }
        return Ok(file.clone());
    }
    let videos: Vec<_> = metadata.files.iter().filter(|f| f.is_video()).collect();
    if videos.is_empty() {
        bail!("torrent has no recognized video; use --list-files and --file to select explicitly");
    }
    if videos.len() == 1 {
        return Ok(videos[0].clone());
    }
    list(metadata);
    if !io::stdin().is_terminal() {
        bail!("multiple video files; choose with --file <index>");
    }
    print!("Select file index: ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    select(
        metadata,
        Some(input.trim().parse().context("invalid file index")?),
    )
}
pub async fn select_async(metadata: &Metadata, index: Option<usize>) -> Result<TorrentFile> {
    if index.is_some()
        || metadata.files.iter().filter(|f| f.is_video()).count() <= 1
        || !io::stdin().is_terminal()
    {
        return select(metadata, index);
    }
    let metadata = metadata.clone();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    // A plain detached thread, rather than Tokio's blocking pool: stdin can
    // block indefinitely and must not hold runtime shutdown open after Ctrl+C.
    std::thread::Builder::new()
        .name("file-selection".into())
        .spawn(move || {
            let _ = sender.send(select(&metadata, index));
        })?;
    receiver.await.context("file selection thread stopped")?
}
pub fn clock(seconds: Option<f64>) -> String {
    match seconds.filter(|s| s.is_finite() && *s >= 0.0) {
        Some(s) => {
            let s = s as u64;
            format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
        }
        None => "--:--:--".into(),
    }
}
pub fn playback(
    state: &PlaybackState,
    piece: u32,
    seconds: f64,
    download: f64,
    bitrate: f64,
    peers: u32,
) {
    println!("{} / {}  piece {piece}  buffer ~{seconds:.0}s  download {:.1} MiB/s  video ~{:.1} MiB/s  peers {peers}{}", clock(state.time_pos), clock(state.duration), download / 1048576.0, bitrate / 1048576.0,
        if state.pause { " [paused]" } else if state.paused_for_cache || state.seeking { " [buffering]" } else { "" });
}
#[cfg(test)]
mod tests {
    use super::*;
    fn file(index: usize, path: &str) -> TorrentFile {
        TorrentFile {
            index,
            path: path.into(),
            size: 100,
            offset: index as u64 * 100,
            symlink: false,
            pad: false,
        }
    }
    #[test]
    fn automatic_video_and_explicit_selection() {
        let m = Metadata {
            piece_length: 100,
            piece_count: 3,
            files: vec![file(0, "readme.txt"), file(1, "movie.MP4")],
        };
        assert_eq!(select(&m, None).unwrap().index, 1);
        assert_eq!(select(&m, Some(0)).unwrap().index, 0);
        assert!(select(&m, Some(99)).is_err());
        let m = Metadata {
            files: vec![file(0, "readme.txt")],
            ..m
        };
        assert!(select(&m, None).is_err());
    }
}
