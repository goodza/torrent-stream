use crate::{
    mpv::PlaybackState,
    streaming::mapping::PieceMapping,
    torrent::{Metadata, TorrentFile, TorrentStatus},
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
/// A live terminal dashboard, with ordinary line output for logs and pipes.
/// No raw input mode is needed, so normal terminal Ctrl+C handling stays intact.
pub struct DownloadView {
    terminal: bool,
    file: String,
    directory: String,
    url: Option<String>,
    notice: Option<(String, std::time::Instant)>,
    started: std::time::Instant,
}

pub struct DownloadProgress<'a> {
    pub mapping: &'a PieceMapping,
    pub status: &'a TorrentStatus,
    pub phase: &'a str,
    pub startup: Option<(u64, u64)>,
    pub playback: Option<&'a PlaybackState>,
    pub piece: u32,
    pub buffered_seconds: f64,
    pub bitrate: f64,
}

impl DownloadView {
    pub fn new(file: &TorrentFile, directory: &std::path::Path, verbose: bool) -> Result<Self> {
        let terminal = io::stdout().is_terminal()
            && std::env::var("TERM").is_ok_and(|term| term != "dumb")
            && !verbose;
        let view = Self {
            terminal,
            file: file.path.escape_debug().to_string(),
            directory: directory.display().to_string().escape_debug().to_string(),
            url: None,
            notice: None,
            started: std::time::Instant::now(),
        };
        if terminal {
            // Alternate screen, hidden cursor, no line wrapping. Disabling wrap
            // keeps long paths from shifting the frame on narrow terminals.
            let mut out = io::stdout().lock();
            write!(out, "\x1b[?1049h\x1b[?25l\x1b[?7l")?;
            out.flush()?;
        }
        Ok(view)
    }

    pub fn stream_url(&mut self, url: &str) {
        self.url = Some(url.into());
        if !self.terminal {
            println!("Stream URL: {url}\nKeep this process running. Press Ctrl+C to stop.");
        }
    }

    pub fn notice(&mut self, message: &str) {
        self.notice = Some((
            message.escape_debug().to_string(),
            std::time::Instant::now(),
        ));
        if !self.terminal {
            println!("{message}");
        }
    }

    pub fn draw(&self, progress: DownloadProgress<'_>) -> Result<()> {
        let mut out = io::stdout().lock();
        if self.terminal {
            write!(out, "\x1b[H\x1b[2J\x1b[1;36mTORRENT STREAM\x1b[0m\r\n")?;
            // Explicit CRLF also works with terminal output processing disabled.
            write!(out, "{}", self.frame(&progress).replace('\n', "\r\n"))?;
        } else {
            // Keep each snapshot compact and free of terminal control codes.
            let verified = progress.mapping.verified_bytes(
                0,
                progress.mapping.file.size,
                &progress.status.completed,
            );
            writeln!(
                out,
                "{}  {:.1}%  {} / {}  {} /s  {} peers{}",
                progress.phase,
                percent(verified, progress.mapping.file.size) * 100.0,
                bytes(verified),
                bytes(progress.mapping.file.size),
                bytes(progress.status.download_rate),
                progress.status.peers,
                progress.startup.map_or_else(String::new, |(done, total)| {
                    format!("  startup {} / {}", bytes(done), bytes(total))
                }),
            )?;
            if let Some(state) = progress.playback {
                writeln!(
                    out,
                    "Playback {} / {}  piece {}  buffer ~{:.0}s  video ~{} /s",
                    clock(state.time_pos),
                    clock(state.duration),
                    progress.piece,
                    progress.buffered_seconds,
                    bytes(progress.bitrate.max(0.0) as u64)
                )?;
            }
        }
        out.flush()?;
        Ok(())
    }

    fn frame(&self, p: &DownloadProgress<'_>) -> String {
        let total = p.mapping.file.size;
        // Count verified bytes of the selected file, excluding other files and
        // bytes outside shared boundary pieces. Wire totals include overhead.
        let done = p.mapping.verified_bytes(0, total, &p.status.completed);
        let eta = if done == total {
            "00:00:00".into()
        } else if p.status.download_rate == 0 {
            "--:--:--".into()
        } else {
            clock(Some((total - done) as f64 / p.status.download_rate as f64))
        };
        let mut text = format!(
            "\nFile: {}\nStatus: {}\n\nDownload {}\nVerified: {} / {}\nSpeed: {} /s    Peers: {}\nETA: {}    Elapsed: {}\n",
            self.file, p.phase, bar(done, total), bytes(done), bytes(total),
            bytes(p.status.download_rate), p.status.peers, eta,
            clock(Some(self.started.elapsed().as_secs_f64())),
        );
        if let Some((done, total)) = p.startup {
            text.push_str(&format!(
                "\nStartup  {}\nBuffer: {} / {} verified\n",
                bar(done, total),
                bytes(done),
                bytes(total)
            ));
        }
        if let Some(state) = p.playback {
            text.push_str(&format!(
                "\nPlayback: {} / {}\nBuffer: ~{:.0}s    Video: ~{} /s    Piece: {}\n",
                clock(state.time_pos),
                clock(state.duration),
                p.buffered_seconds,
                bytes(p.bitrate.max(0.0) as u64),
                p.piece,
            ));
        }
        if let Some(url) = &self.url {
            text.push_str(&format!("\nStream URL: {url}\n"));
        }
        if let Some((notice, updated)) = &self.notice {
            if updated.elapsed() < std::time::Duration::from_secs(8) {
                text.push_str(&format!("\n{notice}\n"));
            }
        }
        text.push_str(&format!(
            "\nSaved in: {}\n\nCtrl+C to stop; downloaded data is retained.\n",
            self.directory
        ));
        text
    }
}

impl Drop for DownloadView {
    fn drop(&mut self) {
        if self.terminal {
            let mut out = io::stdout().lock();
            let _ = write!(out, "\x1b[0m\x1b[?7h\x1b[?25h\x1b[?1049l");
            let _ = out.flush();
        }
    }
}

fn percent(done: u64, total: u64) -> f64 {
    if total == 0 {
        1.0
    } else {
        done.min(total) as f64 / total as f64
    }
}

fn bar(done: u64, total: u64) -> String {
    let fraction = percent(done, total);
    let filled = (fraction * 30.0) as usize;
    format!(
        "[{}{}] {:5.1}%",
        "#".repeat(filled),
        "-".repeat(30 - filled),
        fraction * 100.0
    )
}

fn bytes(value: u64) -> String {
    for (scale, unit) in [
        (1u64 << 40, "TiB"),
        (1 << 30, "GiB"),
        (1 << 20, "MiB"),
        (1 << 10, "KiB"),
    ] {
        if value >= scale {
            return format!("{:.1} {unit}", value as f64 / scale as f64);
        }
    }
    format!("{value} B")
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
    fn dashboard_counts_only_selected_verified_bytes_and_escapes_paths() {
        let file = TorrentFile {
            offset: 150,
            size: 210,
            path: "movie\x1b[2J\n.mkv".into(),
            ..file(1, "movie.mkv")
        };
        let mapping = PieceMapping {
            file: file.clone(),
            piece_length: 100,
        };
        let status = TorrentStatus {
            // Pieces 1 and 3 contribute only 50 and 60 bytes to this file.
            completed: vec![true, true, false, true, true],
            downloaded: 9999,
            download_rate: 10,
            peers: 4,
        };
        let view = DownloadView::new(&file, std::path::Path::new("/tmp/downloads"), true).unwrap();
        let text = view.frame(&DownloadProgress {
            mapping: &mapping,
            status: &status,
            phase: "Buffering before playback",
            startup: Some((50, 100)),
            playback: None,
            piece: 1,
            buffered_seconds: 0.0,
            bitrate: 0.0,
        });
        assert!(text.contains("Verified: 110 B / 210 B"));
        assert!(text.contains("52.4%"));
        assert!(text.contains("ETA: 00:00:10"));
        assert!(text.contains("Peers: 4"));
        assert!(text.contains("Buffer: 50 B / 100 B verified"));
        assert!(text.contains("movie\\u{1b}[2J\\n.mkv"));
        assert!(!text.contains('\x1b'));
        assert!(!text.contains("9999"));
    }

    #[test]
    fn dashboard_handles_idle_complete_and_external_player_states() {
        let file = file(0, "movie.mp4");
        let mapping = PieceMapping {
            file: file.clone(),
            piece_length: 100,
        };
        let status = TorrentStatus::default();
        let mut view =
            DownloadView::new(&file, std::path::Path::new("/tmp/downloads"), true).unwrap();
        // Set directly to avoid emitting a URL to the test's stdout.
        view.url = Some("http://127.0.0.1:1234/stream".into());
        let state = PlaybackState {
            time_pos: Some(10.0),
            duration: Some(60.0),
            pause: true,
            ..Default::default()
        };
        let mut progress = DownloadProgress {
            mapping: &mapping,
            status: &status,
            phase: "Paused",
            startup: None,
            playback: Some(&state),
            piece: 0,
            buffered_seconds: 12.0,
            bitrate: 1024.0,
        };
        let text = view.frame(&progress);
        assert!(text.contains("ETA: --:--:--"));
        assert!(text.contains("Playback: 00:00:10 / 00:01:00"));
        assert!(text.contains("Status: Paused"));
        assert!(text.contains("Buffer: ~12s"));
        let status = TorrentStatus {
            completed: vec![true],
            ..status
        };
        progress.status = &status;
        progress.playback = None;
        progress.phase = "Downloading / external player";
        let text = view.frame(&progress);
        assert!(text.contains("100.0%"));
        assert!(text.contains("ETA: 00:00:00"));
        assert!(text.contains("Stream URL: http://127.0.0.1:1234/stream"));
        assert!(!text.contains("Playback:"));
        assert_eq!(bar(0, 0), bar(100, 100));
        assert_eq!(bar(200, 100), bar(100, 100));
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
