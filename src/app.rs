use crate::{
    cli::{expand_home, Cli},
    downloads::{self, CacheState, DownloadCache},
    library::{self, Priorities},
    mpv::{Player, PlayerOptions},
    streaming::{
        buffer::BufferController,
        mapping::PieceMapping,
        scheduler::StreamScheduler,
        seek::SeekDetector,
        server::{DownloadSnapshot, StreamServer},
    },
    torrent::*,
    ui,
};
use anyhow::{bail, Context, Result};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

struct TorrentMonitor {
    state: watch::Receiver<DownloadSnapshot>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for TorrentMonitor {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl TorrentMonitor {
    fn start(backend: Libtorrent) -> Self {
        let (tx, state) = watch::channel(DownloadSnapshot::default());
        let task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(500));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                match backend.status().await {
                    Ok(status) => {
                        if tx
                            .send(DownloadSnapshot {
                                status: Arc::new(status),
                                error: None,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) => {
                        tx.send_modify(|s| s.error = Some(format!("{error:#}")));
                        break;
                    }
                }
            }
        });
        Self { state, task }
    }
    fn snapshot(&self) -> Result<Arc<TorrentStatus>> {
        let snapshot = self.state.borrow().clone();
        if let Some(error) = snapshot.error {
            bail!("{error}");
        }
        Ok(snapshot.status)
    }
}
async fn apply(backend: &Libtorrent, updates: Vec<PriorityUpdate>) -> Result<()> {
    if !updates.is_empty() {
        backend.priorities(updates).await?;
    }
    Ok(())
}

pub async fn run(cli: Cli) -> Result<()> {
    run_with_shutdown(cli, CancellationToken::new()).await
}
pub async fn run_with_shutdown(cli: Cli, shutdown: CancellationToken) -> Result<()> {
    let mut session = tokio::select! {
        _ = shutdown.cancelled() => return Ok(()),
        session = Session::open(cli, shutdown.clone()) => match session? { Some(session) => session, None => return Ok(()) },
    };
    session.run().await
}

struct Session {
    cli: Cli,
    backend: Libtorrent,
    monitor: TorrentMonitor,
    metadata: Metadata,
    selected: TorrentFile,
    history: CacheState,
    shutdown: CancellationToken,
    // Drop engine/monitor before the cache's exclusive locks.
    cache: Arc<DownloadCache>,
}
struct Sidecars {
    files: Vec<TorrentFile>,
    loaded: Vec<bool>,
    disabled: Vec<bool>,
    progress: Vec<u64>,
    last_progress: Vec<Instant>,
    preferred: Option<usize>,
}
impl Sidecars {
    fn new(metadata: &Metadata, file: &TorrentFile, cli: &Cli) -> Self {
        let files = if cli.no_subtitles {
            vec![]
        } else {
            library::subtitles(metadata, file, &cli.subtitle_language)
        };
        let count = files.len();
        Self {
            files,
            loaded: vec![false; count],
            disabled: vec![false; count],
            progress: vec![0; count],
            last_progress: vec![Instant::now(); count],
            preferred: None,
        }
    }
    fn auxiliary(&self, piece_length: u64) -> Vec<(u32, u32)> {
        self.files
            .iter()
            .enumerate()
            .filter(|(i, _)| !self.disabled[*i])
            .flat_map(|(_, f)| f.pieces(piece_length).map(|p| (p, 2)))
            .collect()
    }
    fn ready(&self, piece_length: u64, have: &[bool]) -> bool {
        self.files.iter().enumerate().all(|(i, f)| {
            self.disabled[i] || mapping(f, piece_length).verified_bytes(0, f.size, have) == f.size
        })
    }
    async fn update(
        &mut self,
        session: &Session,
        status: &TorrentStatus,
        player: Option<&Player>,
        view: &mut ui::DownloadView,
    ) {
        for (i, file) in self.files.iter().enumerate() {
            if self.disabled[i] || self.loaded[i] {
                continue;
            }
            let bytes = mapping(file, session.metadata.piece_length).verified_bytes(
                0,
                file.size,
                &status.completed,
            );
            if bytes > self.progress[i] {
                self.progress[i] = bytes;
                self.last_progress[i] = Instant::now();
            }
            if bytes == file.size {
                if let Some(player) = player {
                    let path = session.cache.path.join(&file.path);
                    // JSON preserves Unicode and prevents command interpolation.
                    let mode = if self.preferred.is_none_or(|selected| i < selected) {
                        "select"
                    } else {
                        "auto"
                    };
                    match player
                        .client
                        .command(serde_json::json!(["sub-add", path, mode]))
                        .await
                    {
                        Ok(_) => {
                            self.loaded[i] = true;
                            if mode == "select" {
                                self.preferred = Some(i);
                            }
                        }
                        Err(error) => {
                            self.disabled[i] = true;
                            view.notice(&format!(
                                "Could not load subtitle {}: {error}",
                                file.path.escape_debug()
                            ));
                        }
                    }
                }
            } else if self.last_progress[i].elapsed()
                >= Duration::from_secs(session.cli.stall_timeout.into())
            {
                self.disabled[i] = true;
                view.notice(&format!(
                    "Subtitle unavailable; continuing: {}",
                    file.path.escape_debug()
                ));
            }
        }
    }
}
fn mapping(file: &TorrentFile, piece_length: u64) -> PieceMapping {
    PieceMapping {
        file: file.clone(),
        piece_length,
    }
}

impl Session {
    async fn open(cli: Cli, shutdown: CancellationToken) -> Result<Option<Self>> {
        let input = cli.input()?;
        let is_magnet = matches!(input, TorrentSource::Magnet(_));
        let source = match input {
            TorrentSource::Magnet(uri) => uri,
            TorrentSource::File(path) => {
                let path = tokio::fs::canonicalize(path)
                    .await
                    .context("invalid torrent path")?;
                let attr = tokio::fs::metadata(&path).await?;
                if !attr.is_file() || attr.len() > 64 * 1048576 {
                    bail!("torrent input must be a regular file no larger than 64 MiB");
                }
                path.to_str()
                    .context("torrent source path must be valid UTF-8")?
                    .to_owned()
            }
        };
        let root = expand_home(&cli.download_dir)?;
        tokio::fs::create_dir_all(&root)
            .await
            .context("create download directory")?;
        let root = tokio::fs::canonicalize(root).await?;
        let context = if is_magnet {
            "add magnet link to torrent engine"
        } else {
            "add torrent file to torrent engine"
        };
        let key = Libtorrent::source_key(source.clone())
            .await
            .context(context)?;
        let listing_dir;
        let cache = if cli.list_files {
            None
        } else {
            Some(Arc::new(DownloadCache::open(&root, &key)?))
        };
        let download = match &cache {
            Some(cache) => cache.path.clone(),
            None => {
                listing_dir = tempfile::Builder::new()
                    .prefix("session-")
                    .tempdir_in(&root)?;
                listing_dir.path().to_owned()
            }
        };
        let record = root.join(".torrent-stream/records").join(&key);
        let cached = record.join("metadata.torrent");
        let mut use_cached = false;
        if is_magnet && cached.exists() {
            let valid = async {
                downloads::validate_tree(&root.join(".torrent-stream"))?;
                let attr = tokio::fs::metadata(&cached).await?;
                anyhow::ensure!(attr.len() <= 64 * 1048576, "cached metadata is too large");
                anyhow::ensure!(
                    Libtorrent::source_key(
                        cached
                            .to_str()
                            .context("metadata path must be Unicode")?
                            .into()
                    )
                    .await?
                        == key,
                    "cached metadata identity mismatch"
                );
                Ok::<_, anyhow::Error>(())
            }
            .await;
            match valid {
                Ok(()) => {
                    use_cached = true;
                    println!("Using saved torrent metadata.");
                }
                Err(error) => eprintln!("Ignoring saved torrent metadata: {error:#}"),
            }
        }
        println!("Fetching metadata...");
        let backend = Libtorrent::open_cached(
            source,
            &download,
            cli.download_limit_kbps * 1024,
            use_cached.then_some(cached.as_path()),
        )
        .await
        .context(context)?;
        if let Some(cache) = &cache {
            backend.retain_cache(cache.clone())?;
        }
        let metadata =
            tokio::time::timeout(Duration::from_secs(cli.metadata_timeout.into()), async {
                loop {
                    if let Some(m) = backend.metadata().await? {
                        return Ok::<_, anyhow::Error>(m);
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            })
            .await
            .context("magnet metadata timeout")??;
        metadata.validate()?;
        if cli.list_files {
            ui::list(&metadata);
            return Ok(None);
        }
        let cache = cache.context("missing download cache")?;
        anyhow::ensure!(
            metadata.files.iter().all(|f| !f
                .path
                .split('/')
                .next()
                .unwrap_or("")
                .eq_ignore_ascii_case(".torrent-stream.lock")),
            "torrent path conflicts with download lock"
        );
        save_metadata(&backend, &cache.record).await;
        let selected = ui::select_async(&metadata, cli.file).await?;
        anyhow::ensure!(
            !cli.playlist || selected.is_video(),
            "playlist selection must be a recognized video"
        );
        while backend.status().await?.checking {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if downloads::import_sessions(&root, &download, &metadata)? {
            backend.recheck().await?;
        }
        let mut history = CacheState::read(&cache.record);
        history.title = metadata
            .files
            .first()
            .map(|f| f.path.split('/').next().unwrap_or(&f.path).to_owned())
            .unwrap_or_default();
        history.save(&cache.record)?;
        let monitor = TorrentMonitor::start(backend.clone());
        Ok(Some(Self {
            cli,
            backend,
            monitor,
            metadata,
            selected,
            history,
            shutdown,
            cache,
        }))
    }
    async fn run(&mut self) -> Result<()> {
        let files = library::playlist(&self.metadata, &self.selected, self.cli.playlist);
        for (index, file) in files.iter().enumerate() {
            if self.shutdown.is_cancelled() {
                break;
            }
            if !self.episode(file, files.get(index + 1)).await? {
                break;
            }
        }
        // Refresh metadata after downloading; newly available v2 layers are retained.
        if !self.shutdown.is_cancelled() {
            save_metadata(&self.backend, &self.cache.record).await;
        }
        Ok(())
    }
    async fn episode(&mut self, file: &TorrentFile, next: Option<&TorrentFile>) -> Result<bool> {
        println!(
            "Selected: {}\nSize: {:.2} GiB\nDownload directory: {}",
            file.path.escape_debug(),
            file.size as f64 / (1024.0 * 1024.0 * 1024.0),
            self.cache.path.display()
        );
        self.backend.select(file.index).await?;
        let mut composer = Priorities::new(&self.metadata, file);
        let mut scheduler = StreamScheduler::new(mapping(file, self.metadata.piece_length));
        let mut view = ui::DownloadView::new(file, &self.cache.path, self.cli.verbose)?;
        let mut subtitles = Sidecars::new(&self.metadata, file, &self.cli);
        let (head, tail) = startup_sizes(&self.cli, file);
        let tail_start = file.size.saturating_sub(tail).max(head);
        let total = if self.cli.download_only {
            file.size
        } else {
            head + file.size - tail_start
        };
        let mut progress_at = Instant::now();
        let mut previous = 0;
        let mut last_ui = Instant::now() - Duration::from_secs(1);
        loop {
            if self.shutdown.is_cancelled() {
                return Ok(false);
            }
            let status = self.monitor.snapshot()?;
            let done = if self.cli.download_only {
                scheduler
                    .mapping
                    .verified_bytes(0, file.size, &status.completed)
            } else {
                scheduler.mapping.verified_bytes(0, head, &status.completed)
                    + scheduler
                        .mapping
                        .verified_bytes(tail_start, file.size, &status.completed)
            };
            let updates = if self.cli.download_only {
                vec![]
            } else {
                scheduler.startup(head, tail, &status.completed)
            };
            apply(
                &self.backend,
                composer.compose(updates, subtitles.auxiliary(self.metadata.piece_length)),
            )
            .await?;
            subtitles.update(self, &status, None, &mut view).await;
            if done > previous {
                previous = done;
                progress_at = Instant::now();
            }
            if last_ui.elapsed() >= Duration::from_secs(1) || done == total {
                view.draw(ui::DownloadProgress {
                    mapping: &scheduler.mapping,
                    status: &status,
                    phase: if self.cli.download_only {
                        "Downloading"
                    } else {
                        "Buffering before playback"
                    },
                    startup: Some((done, total)),
                    playback: None,
                    piece: 0,
                    buffered_seconds: 0.0,
                    bitrate: 0.0,
                })?;
                last_ui = Instant::now();
            }
            if done == total
                && (!self.cli.download_only
                    || subtitles.ready(self.metadata.piece_length, &status.completed))
            {
                break;
            }
            if done < total
                && progress_at.elapsed() > Duration::from_secs(self.cli.stall_timeout.into())
            {
                bail!("startup buffering stalled: no new verified pieces; check swarm availability or increase --stall-timeout");
            }
            tokio::select! { _ = self.shutdown.cancelled() => return Ok(false), _ = tokio::time::sleep(Duration::from_millis(500)) => {} }
        }
        if self.cli.download_only {
            drop(view);
            println!(
                "Downloaded: {}",
                self.cache
                    .path
                    .join(&file.path)
                    .display()
                    .to_string()
                    .escape_debug()
            );
            return Ok(true);
        }

        let server = StreamServer::start(
            scheduler.mapping.clone(),
            self.cache.path.join(&file.path),
            self.monitor.state.clone(),
        )
        .await?;
        let mut player = if self.cli.no_mpv {
            view.stream_url(&server.url);
            None
        } else {
            let start = (!self.cli.no_resume)
                .then(|| self.history.position(file))
                .flatten();
            if let Some(start) = start {
                view.notice(&format!("Resuming at {}", ui::clock(Some(start))));
            }
            view.notice("Starting mpv...");
            Some(
                Player::launch_with_options(
                    &server.url,
                    self.cli.buffer_mb,
                    false,
                    &PlayerOptions {
                        start,
                        no_subtitles: self.cli.no_subtitles,
                        audio_languages: &self.cli.audio_language,
                        subtitle_languages: &self.cli.subtitle_language,
                    },
                )
                .await?,
            )
        };
        let mut playing = Playing {
            server,
            scheduler,
            composer,
            subtitles,
            view,
        };
        let result = self.playback_loop(&mut playing, &mut player, next).await;
        // Persist before terminating mpv, whose final unavailable property events
        // may otherwise discard the last useful sample.
        let save = if let Some(player) = &player {
            self.history
                .update(file, &player.client.state.borrow().clone());
            self.history.save(&self.cache.record)
        } else {
            Ok(())
        };
        if let Some(player) = &mut player {
            let _ = player.stop().await;
        }
        // Server, priorities, and demand are recreated for each episode.
        result.and_then(|advance| {
            save?;
            Ok(advance)
        })
    }
    async fn playback_loop(
        &mut self,
        playing: &mut Playing,
        player: &mut Option<Player>,
        next: Option<&TorrentFile>,
    ) -> Result<bool> {
        let mut controller = BufferController::new(
            self.cli.buffer_seconds,
            u64::from(self.cli.buffer_mb) * 1048576,
        );
        let mut detector = SeekDetector::default();
        let mut demand_rx = playing.server.demand.clone();
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_ui = Instant::now() - Duration::from_secs(1);
        let mut last_warning = Instant::now() - Duration::from_secs(30);
        let mut last_save = Instant::now();
        let started = Instant::now();
        let mut byte = 0;
        loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => return Ok(false),
                _ = tick.tick() => {},
                changed = demand_rx.changed() => { changed.context("stream server stopped")?; }
            }
            let status = self.monitor.snapshot()?;
            let state = player
                .as_ref()
                .map(|p| p.client.state.borrow().clone())
                .unwrap_or_default();
            if let Some(error) = &state.error {
                bail!("mpv: {error}");
            }
            if state.ended {
                return Ok(state.end_reason.as_deref() == Some("eof"));
            }
            if let Some(p) = player.as_mut() {
                if p.exited()? {
                    return Ok(false);
                }
                if state.disconnected {
                    bail!("mpv IPC disconnected");
                }
            }
            if player.is_some() && last_save.elapsed() >= Duration::from_secs(5) {
                self.history.update(&playing.scheduler.mapping.file, &state);
                self.history.save(&self.cache.record)?;
                last_save = Instant::now();
            }
            playing
                .subtitles
                .update(self, &status, player.as_ref(), &mut playing.view)
                .await;
            let demand = *demand_rx.borrow_and_update();
            let seek = detector.update(&state);
            if seek {
                playing
                    .view
                    .notice("Seek detected; buffering new location...");
            }
            let packet_byte = playing.scheduler.mapping.playback_byte(&state);
            byte = crate::streaming::mapping::playback_focus(
                byte,
                &playing.scheduler.mapping,
                &state,
                demand,
                player.is_some(),
            );
            controller.playback(
                &state,
                playing.scheduler.mapping.file.size,
                packet_byte,
                seek,
            );
            controller.download(
                Instant::now(),
                status.downloaded,
                status.completed.iter().filter(|p| **p).count() as u64,
                status.download_rate,
            );
            let needed = (demand.active
                && demand.offset < playing.scheduler.mapping.file.size
                && status
                    .completed
                    .get(playing.scheduler.mapping.piece_at(demand.offset) as usize)
                    != Some(&true))
            .then_some(demand.offset);
            let buffered = playing
                .scheduler
                .mapping
                .contiguous_bytes(byte, &status.completed);
            let seconds = controller.buffered_seconds(buffered);
            let mut auxiliary = playing.subtitles.auxiliary(self.metadata.piece_length);
            if library::should_prefetch(
                &state,
                buffered,
                controller.target_bytes(),
                playing.scheduler.mapping.file.size - byte,
            ) {
                if let Some(next) = next {
                    let map = mapping(next, self.metadata.piece_length);
                    let (head, tail) = startup_sizes(&self.cli, next);
                    for range in [
                        map.range(0, head),
                        map.range(next.size.saturating_sub(tail), next.size),
                    ]
                    .into_iter()
                    .flatten()
                    {
                        auxiliary.extend(range.map(|p| (p, 1)));
                    }
                }
            }
            let updates = playing.scheduler.plan(
                byte,
                controller.target_bytes(),
                controller.bitrate,
                &status.completed,
                needed,
            );
            apply(&self.backend, playing.composer.compose(updates, auxiliary)).await?;
            if last_ui.elapsed() >= Duration::from_secs(1) {
                let phase = if player.is_none() {
                    "Downloading / external player"
                } else if state.pause {
                    "Paused"
                } else if state.seeking || state.paused_for_cache {
                    "Buffering"
                } else {
                    "Playing"
                };
                playing.view.draw(ui::DownloadProgress {
                    mapping: &playing.scheduler.mapping,
                    status: &status,
                    phase,
                    startup: None,
                    playback: player.is_some().then_some(&state),
                    piece: playing.scheduler.mapping.piece_at(byte),
                    buffered_seconds: seconds,
                    bitrate: controller.bitrate,
                })?;
                tracing::debug!(byte, packet_byte = ?packet_byte, window = ?playing.scheduler.window, buffered_bytes = buffered, "stream state");
                last_ui = Instant::now();
            }
            if started.elapsed() > Duration::from_secs(10)
                && controller.ratio() < 1.0
                && buffered
                    < controller
                        .target_bytes()
                        .min(playing.scheduler.mapping.file.size - byte)
                && !state.pause
                && last_warning.elapsed() >= Duration::from_secs(30)
            {
                playing
                    .view
                    .notice("Network is slower than video bitrate. Playback may stall.");
                last_warning = Instant::now();
            }
        }
    }
}
struct Playing {
    server: StreamServer,
    scheduler: StreamScheduler,
    composer: Priorities,
    subtitles: Sidecars,
    view: ui::DownloadView,
}
fn startup_sizes(cli: &Cli, file: &TorrentFile) -> (u64, u64) {
    let head = (u64::from(cli.startup_head_mb) * 1048576).min(file.size);
    let tail = if matches!(
        file.extension().as_str(),
        "mp4" | "mov" | "m4v" | "mkv" | "avi"
    ) {
        (u64::from(cli.startup_tail_mb) * 1048576).min(file.size)
    } else {
        0
    };
    (head, tail)
}
async fn save_metadata(backend: &Libtorrent, record: &std::path::Path) {
    match backend
        .export_metadata()
        .await
        .and_then(|bytes| downloads::atomic_write(&record.join("metadata.torrent"), &bytes))
    {
        Ok(()) => {}
        Err(error) => eprintln!("Could not save torrent metadata: {error:#}"),
    }
}
