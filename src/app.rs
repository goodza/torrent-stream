use crate::{
    cli::{expand_home, Cli},
    mpv::Player,
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
        tracing::debug!(changes = updates.len(), "applying piece priority changes");
        backend.priorities(updates).await?;
    }
    Ok(())
}
pub async fn run(cli: Cli) -> Result<()> {
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
    let run_dir = tempfile::Builder::new()
        .prefix("session-")
        .tempdir_in(&root)?;
    println!("Fetching metadata...");
    let backend = Libtorrent::open(source, run_dir.path(), cli.download_limit_kbps * 1024)
        .await
        .context(if is_magnet {
            "add magnet link to torrent engine"
        } else {
            "add torrent file to torrent engine"
        })?;
    let metadata = tokio::time::timeout(Duration::from_secs(cli.metadata_timeout.into()), async {
        loop {
            if let Some(m) = backend.metadata().await? {
                return Ok::<_, anyhow::Error>(m);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .context("magnet metadata timeout")??;
    metadata.validate()?;
    if cli.list_files {
        ui::list(&metadata);
        return Ok(());
    }
    let file = ui::select_async(&metadata, cli.file).await?;
    // A fresh private directory prevents pre-existing symlinks from redirecting
    // libtorrent's asynchronous disk writer. Payload paths are never reused.
    let download = run_dir.keep();
    println!(
        "Selected: {}\nSize: {:.2} GiB\nDownload directory: {}",
        file.path.escape_debug(),
        file.size as f64 / (1024.0 * 1024.0 * 1024.0),
        download.display()
    );
    backend.select(file.index).await?;
    let monitor = TorrentMonitor::start(backend.clone());
    let mapping = PieceMapping {
        file: file.clone(),
        piece_length: metadata.piece_length,
    };
    let mut scheduler = StreamScheduler::new(mapping.clone());
    let head = (u64::from(cli.startup_head_mb) * 1048576).min(file.size);
    let tail = if matches!(
        file.extension().as_str(),
        "mp4" | "mov" | "m4v" | "mkv" | "avi"
    ) {
        (u64::from(cli.startup_tail_mb) * 1048576).min(file.size)
    } else {
        0
    };
    let tail_start = file.size.saturating_sub(tail).max(head);
    let total = head + file.size - tail_start;
    println!("Buffering...");
    let mut last_progress = Instant::now();
    let mut previous_bytes = 0;
    let mut last_ui = Instant::now() - Duration::from_secs(1);
    loop {
        let status = monitor.snapshot()?;
        let done = mapping.verified_bytes(0, head, &status.completed)
            + mapping.verified_bytes(tail_start, file.size, &status.completed);
        apply(&backend, scheduler.startup(head, tail, &status.completed)).await?;
        if done > previous_bytes {
            last_progress = Instant::now();
            previous_bytes = done;
        }
        if last_ui.elapsed() >= Duration::from_secs(1) {
            println!(
                "{:.1} MiB / {:.1} MiB  {:.1} MiB/s  {} peers",
                done as f64 / 1048576.0,
                total as f64 / 1048576.0,
                status.download_rate as f64 / 1048576.0,
                status.peers
            );
            last_ui = Instant::now();
        }
        if done == total {
            break;
        }
        if last_progress.elapsed() > Duration::from_secs(cli.stall_timeout.into()) {
            bail!("startup buffering stalled: no new verified pieces; check swarm availability or increase --stall-timeout");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let server = StreamServer::start(
        mapping.clone(),
        download.join(&file.path),
        monitor.state.clone(),
    )
    .await?;
    let mut player = if cli.no_mpv {
        println!(
            "Stream URL: {}\nKeep this process running. Press Ctrl+C to stop.",
            server.url
        );
        None
    } else {
        println!("Starting mpv...");
        let player = Player::launch(&server.url, cli.buffer_mb, false).await?;
        tracing::debug!(socket = %player.socket.display(), "mpv IPC ready");
        Some(player)
    };
    let result = playback_loop(
        &cli,
        &backend,
        &monitor,
        &server,
        &mut player,
        &mut scheduler,
    )
    .await;
    let clear_result = apply(&backend, scheduler.clear()).await;
    if let Some(player) = &mut player {
        let _ = player.stop().await;
    }
    result?;
    clear_result?;
    Ok(())
}
async fn playback_loop(
    cli: &Cli,
    backend: &Libtorrent,
    monitor: &TorrentMonitor,
    server: &StreamServer,
    player: &mut Option<Player>,
    scheduler: &mut StreamScheduler,
) -> Result<()> {
    let mut controller =
        BufferController::new(cli.buffer_seconds, u64::from(cli.buffer_mb) * 1048576);
    let mut detector = SeekDetector::default();
    let mut demand_rx = server.demand.clone();
    let mut mpv_rx = player.as_ref().map(|p| p.client.state.clone());
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_ui = Instant::now() - Duration::from_secs(2);
    let mut last_warning = Instant::now() - Duration::from_secs(30);
    let started = Instant::now();
    let mut byte = 0;
    let mut last_rate = Instant::now() - Duration::from_secs(1);
    loop {
        // Range demand wakes planning immediately during seeks. High-frequency
        // property observations are sampled at 500ms to bound scheduler churn.
        tokio::select! {
            _ = tick.tick() => {}
            changed = demand_rx.changed() => { changed.context("stream server stopped")?; }
        }
        let status = monitor.snapshot()?;
        let state = mpv_rx
            .as_mut()
            .map(|rx| rx.borrow_and_update().clone())
            .unwrap_or_default();
        if let Some(error) = &state.error {
            bail!("mpv: {error}");
        }
        if state.ended {
            return Ok(());
        }
        if let Some(p) = player.as_mut() {
            if p.exited()? {
                return Ok(());
            }
            if state.disconnected {
                bail!("mpv IPC disconnected");
            }
        }
        let demand = *demand_rx.borrow_and_update();
        let seek = detector.update(&state);
        if seek {
            println!("Seek detected; buffering new location...");
            tracing::info!(time = ?state.time_pos, "Seek detected; moving streaming window");
        }
        let packet_byte = scheduler.mapping.playback_byte(&state);
        byte = crate::streaming::mapping::playback_focus(
            byte,
            &scheduler.mapping,
            &state,
            demand,
            player.is_some(),
        );
        controller.playback(&state, scheduler.mapping.file.size, packet_byte, seek);
        if last_rate.elapsed() >= Duration::from_millis(500) {
            controller.download(
                Instant::now(),
                status.downloaded,
                status.completed.iter().filter(|have| **have).count() as u64,
                status.download_rate,
            );
            last_rate = Instant::now();
        }
        let needed = if demand.active
            && demand.offset < scheduler.mapping.file.size
            && status
                .completed
                .get(scheduler.mapping.piece_at(demand.offset) as usize)
                != Some(&true)
        {
            Some(demand.offset)
        } else {
            None
        };
        apply(
            backend,
            scheduler.plan(
                byte,
                controller.target_bytes(),
                controller.bitrate,
                &status.completed,
                needed,
            ),
        )
        .await?;
        let buffered = scheduler.mapping.contiguous_bytes(byte, &status.completed);
        let seconds = controller.buffered_seconds(buffered);
        if last_ui.elapsed() >= Duration::from_secs(2) {
            ui::playback(
                &state,
                scheduler.mapping.piece_at(byte),
                seconds,
                controller.download_rate,
                controller.bitrate,
                status.peers,
            );
            tracing::debug!(byte, packet_byte = ?packet_byte, demand = ?demand, window = ?scheduler.window, buffered_bytes = buffered, piece_rate = controller.piece_rate, ratio = controller.ratio(), "stream state");
            last_ui = Instant::now();
        }
        if started.elapsed() > Duration::from_secs(10)
            && controller.ratio() < 1.0
            && buffered
                < controller
                    .target_bytes()
                    .min(scheduler.mapping.file.size - byte)
            && !state.pause
            && last_warning.elapsed() >= Duration::from_secs(30)
        {
            println!("Network is slower than video bitrate. Playback may stall.");
            last_warning = Instant::now();
        }
    }
}
