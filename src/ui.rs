use crate::{
    mpv::PlaybackState,
    streaming::mapping::PieceMapping,
    torrent::{Metadata, TorrentFile, TorrentStatus},
};
use anyhow::{bail, Context, Result};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    crossterm::{
        cursor::{Hide, Show},
        execute,
        style::ResetColor,
        terminal::{DisableLineWrap, EnableLineWrap, EnterAlternateScreen, LeaveAlternateScreen},
    },
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Gauge, Paragraph},
    Frame, Terminal,
};
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
    terminal: Option<LiveTerminal>,
    file: String,
    directory: String,
    url: Option<String>,
    notice: Option<(String, std::time::Instant)>,
    started: std::time::Instant,
}

/// Owns terminal setup so partial initialization, errors and cancellation all
/// restore the screen. Cooked mode preserves the existing SIGINT handler.
struct LiveTerminal(Terminal<CrosstermBackend<io::Stdout>>);

impl LiveTerminal {
    fn new() -> io::Result<Self> {
        let mut terminal = Self(Terminal::new(CrosstermBackend::new(io::stdout()))?);
        execute!(
            terminal.0.backend_mut(),
            EnterAlternateScreen,
            DisableLineWrap,
            Hide
        )?;
        // Fullscreen setup does not need Terminal::clear's cursor query, which
        // requires an input terminal and can fail when stdin is redirected.
        terminal.0.backend_mut().clear()?;
        Ok(terminal)
    }
}

impl Drop for LiveTerminal {
    fn drop(&mut self) {
        let _ = execute!(
            self.0.backend_mut(),
            ResetColor,
            EnableLineWrap,
            Show,
            LeaveAlternateScreen
        );
    }
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
        Ok(Self {
            terminal: if terminal {
                Some(LiveTerminal::new()?)
            } else {
                None
            },
            file: file.path.escape_debug().to_string(),
            directory: directory.display().to_string().escape_debug().to_string(),
            url: None,
            notice: None,
            started: std::time::Instant::now(),
        })
    }

    pub fn stream_url(&mut self, url: &str) {
        self.url = Some(url.into());
        if self.terminal.is_none() {
            println!("Stream URL: {url}\nKeep this process running. Press Ctrl+C to stop.");
        }
    }

    pub fn notice(&mut self, message: &str) {
        self.notice = Some((
            message.escape_debug().to_string(),
            std::time::Instant::now(),
        ));
        if self.terminal.is_none() {
            println!("{message}");
        }
    }

    pub fn draw(&mut self, progress: DownloadProgress<'_>) -> Result<()> {
        if let Some(mut terminal) = self.terminal.take() {
            let result = terminal
                .0
                .draw(|frame| self.render(frame, &progress))
                .map(|_| ());
            // Return ownership even on a draw error, so Drop restores the screen.
            self.terminal = Some(terminal);
            result?;
        } else {
            let mut out = io::stdout().lock();
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
            out.flush()?;
        }
        Ok(())
    }

    fn render(&self, frame: &mut Frame, p: &DownloadProgress<'_>) {
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
        let notice = self
            .notice
            .as_ref()
            .filter(|(_, updated)| updated.elapsed() < std::time::Duration::from_secs(8));
        let areas = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(if p.startup.is_some() { 4 } else { 0 }),
            Constraint::Length(if p.playback.is_some() { 3 } else { 0 }),
            Constraint::Length(if self.url.is_some() { 1 } else { 0 }),
            Constraint::Length(if notice.is_some() { 1 } else { 0 }),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(frame.area());
        frame.render_widget(
            Paragraph::new("TORRENT STREAM").style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            areas[0],
        );
        frame.render_widget(
            Paragraph::new(format!("File: {}\nStatus: {}", self.file, p.phase)),
            areas[1],
        );
        frame.render_widget(gauge("Download", done, total, Color::Cyan), areas[2]);
        frame.render_widget(
            Paragraph::new(format!(
                "Verified: {} / {}\nSpeed: {} /s    Peers: {}\nETA: {}    Elapsed: {}",
                bytes(done),
                bytes(total),
                bytes(p.status.download_rate),
                p.status.peers,
                eta,
                clock(Some(self.started.elapsed().as_secs_f64())),
            )),
            areas[3],
        );
        if let Some((done, total)) = p.startup {
            let startup =
                Layout::vertical([Constraint::Length(3), Constraint::Length(1)]).split(areas[4]);
            frame.render_widget(gauge("Startup", done, total, Color::Green), startup[0]);
            frame.render_widget(
                Paragraph::new(format!(
                    "Buffer: {} / {} verified",
                    bytes(done),
                    bytes(total)
                )),
                startup[1],
            );
        }
        if let Some(state) = p.playback {
            frame.render_widget(
                Paragraph::new(format!(
                    "Playback: {} / {}\nBuffer: ~{:.0}s    Video: ~{} /s    Piece: {}",
                    clock(state.time_pos),
                    clock(state.duration),
                    p.buffered_seconds,
                    bytes(p.bitrate.max(0.0) as u64),
                    p.piece,
                )),
                areas[5],
            );
        }
        if let Some(url) = &self.url {
            frame.render_widget(Paragraph::new(format!("Stream URL: {url}")), areas[6]);
        }
        if let Some((notice, _)) = notice {
            frame.render_widget(
                Paragraph::new(notice.as_str()).style(Style::default().fg(Color::Yellow)),
                areas[7],
            );
        }
        frame.render_widget(
            Paragraph::new(format!(
                "Saved in: {}\nCtrl+C to stop; downloaded data is retained.",
                self.directory
            )),
            areas[9],
        );
    }
}

fn percent(done: u64, total: u64) -> f64 {
    if total == 0 {
        1.0
    } else {
        done.min(total) as f64 / total as f64
    }
}

fn gauge(title: &str, done: u64, total: u64, color: Color) -> Gauge<'_> {
    let fraction = percent(done, total);
    Gauge::default()
        .block(Block::default().title(title).borders(Borders::ALL))
        .gauge_style(Style::default().fg(color))
        .ratio(fraction)
        .label(format!("{:.1}%", fraction * 100.0))
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
    use ratatui::backend::TestBackend;

    fn rendered(view: &DownloadView, progress: &DownloadProgress<'_>) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|frame| view.render(frame, progress)).unwrap();
        buffer_text(terminal.backend().buffer())
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content
            .chunks(usize::from(buffer.area.width).max(1))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }
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
        let text = rendered(
            &view,
            &DownloadProgress {
                mapping: &mapping,
                status: &status,
                phase: "Buffering before playback",
                startup: Some((50, 100)),
                playback: None,
                piece: 1,
                buffered_seconds: 0.0,
                bitrate: 0.0,
            },
        );
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
        let text = rendered(&view, &progress);
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
        let text = rendered(&view, &progress);
        assert!(text.contains("100.0%"));
        assert!(text.contains("ETA: 00:00:00"));
        assert!(text.contains("Stream URL: http://127.0.0.1:1234/stream"));
        assert!(!text.contains("Playback:"));
        assert_eq!(percent(0, 0), percent(100, 100));
        assert_eq!(percent(200, 100), percent(100, 100));
    }

    #[test]
    fn dashboard_resizes_and_clears_finished_startup_and_expired_notices() {
        let file = file(0, &format!("{}.mkv", "映画".repeat(100)));
        let mapping = PieceMapping {
            file: file.clone(),
            piece_length: 100,
        };
        let status = TorrentStatus::default();
        let mut view =
            DownloadView::new(&file, std::path::Path::new("/tmp/downloads"), true).unwrap();
        view.notice = Some(("Seek detected".into(), std::time::Instant::now()));
        let mut progress = DownloadProgress {
            mapping: &mapping,
            status: &status,
            phase: "Buffering",
            startup: Some((0, 100)),
            playback: None,
            piece: 0,
            buffered_seconds: 0.0,
            bitrate: 0.0,
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| view.render(frame, &progress))
            .unwrap();
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Startup"));
        assert!(text.contains("Seek detected"));
        assert!(!text.contains(&file.path));

        progress.startup = None;
        view.notice.as_mut().unwrap().1 -= std::time::Duration::from_secs(9);
        terminal
            .draw(|frame| view.render(frame, &progress))
            .unwrap();
        let text = buffer_text(terminal.backend().buffer());
        assert!(!text.contains("Startup"));
        assert!(!text.contains("Seek detected"));

        for (width, height) in [(32, 18), (20, 6), (1, 1), (0, 0), (100, 24)] {
            terminal.backend_mut().resize(width, height);
            terminal
                .draw(|frame| view.render(frame, &progress))
                .unwrap();
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer.area.width, width);
            assert_eq!(buffer.area.height, height);
            if width >= 32 && height >= 18 {
                let text = buffer_text(buffer);
                assert!(text.contains("TORRENT STREAM"));
                assert!(text.contains("Verified:"));
                assert!(text.contains("Ctrl+C to stop"));
            }
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
