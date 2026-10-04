//! Opt-in real libtorrent/mpv validation. Generates >=1 GiB video files locally;
//! no public torrent, tracker, or copyrighted media is used.
#![cfg(feature = "integration-tests")]
use anyhow::{bail, Context, Result};
use serde_json::json;
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    process::Command,
    sync::{mpsc, watch},
};
use torrent_stream::{
    mpv::Player,
    streaming::{
        buffer::BufferController,
        mapping::{playback_focus, PieceMapping},
        scheduler::StreamScheduler,
        seek::SeekDetector,
        server::{DownloadSnapshot, StreamServer},
    },
    torrent::*,
};

async fn metadata(backend: &Libtorrent) -> Result<Metadata> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(m) = backend.metadata().await? {
                m.validate()?;
                return Ok(m);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .context("fixture metadata timeout")?
}
async fn ffmpeg(args: &[&str]) -> Result<()> {
    let output = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .output()
        .await?;
    if !output.status.success() {
        bail!("ffmpeg: {}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(())
}
async fn generate(
    root: &Path,
    container: &str,
    multi: bool,
) -> Result<(std::path::PathBuf, String)> {
    let sample = root.join("sample.mp4");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=1280x720:rate=24",
        "-t",
        "4",
        "-c:v",
        "mpeg4",
        "-q:v",
        "2",
        "-an",
        sample.to_str().unwrap(),
    ])
    .await?;
    let sample_size = tokio::fs::metadata(&sample).await?.len();
    let loops = ((1024 * 1048576u64) / sample_size + 4).to_string();
    let name = if multi {
        "bundle".to_string()
    } else {
        format!("movie.{container}")
    };
    let video = if multi {
        tokio::fs::create_dir(root.join(&name)).await?;
        tokio::fs::write(root.join(&name).join("000-readme.txt"), vec![42; 12345]).await?;
        root.join(&name).join(format!("Фильм.{container}"))
    } else {
        root.join(&name)
    };
    ffmpeg(&[
        "-stream_loop",
        &loops,
        "-i",
        sample.to_str().unwrap(),
        "-c",
        "copy",
        video.to_str().unwrap(),
    ])
    .await?;
    assert!(
        tokio::fs::metadata(&video).await?.len() >= 1024 * 1048576,
        "large fixture must be >=1 GiB"
    );
    tokio::fs::remove_file(sample).await?;
    let torrent = root.join("fixture.torrent");
    Libtorrent::make_fixture(root, &name, &torrent).await?;
    Ok((torrent, name))
}
struct AbortTask(tokio::task::JoinHandle<()>);
impl Drop for AbortTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn wait_playing(player: &Player, target: Option<f64>, timeout: u64) -> Result<()> {
    let started = Instant::now();
    loop {
        let state = player.client.state.borrow().clone();
        if let Some(e) = state.error {
            bail!("mpv playback error: {e}");
        }
        if state.ended || state.disconnected {
            bail!("mpv stopped during test");
        }
        if state.time_pos.is_some_and(|t| {
            target
                .map(|wanted| (t - wanted).abs() < 5.0)
                .unwrap_or(t > 0.3)
        }) && !state.paused_for_cache
            && !state.seeking
        {
            return Ok(());
        }
        if started.elapsed() > Duration::from_secs(timeout) {
            bail!("mpv did not resume: {state:?}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
async fn scenario(container: &str, multi: bool, magnet: bool) -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("torrent_stream=debug")
        .try_init();
    let seed_dir = tempfile::tempdir()?;
    let (torrent, _) = generate(seed_dir.path(), container, multi).await?;
    eprintln!("fixture generated: {container}, multi={multi}, magnet={magnet}");
    let seed = Libtorrent::open(torrent.to_str().unwrap().into(), seed_dir.path(), 0).await?;
    let m = metadata(&seed).await?;
    let file = m.files.iter().find(|f| f.is_video()).unwrap().clone();
    if multi {
        assert_ne!(
            file.offset % m.piece_length,
            0,
            "fixture must exercise a shared boundary piece"
        );
    }
    seed.select(file.index).await?;
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let s = seed.status().await?;
            if s.completed.len() == m.piece_count as usize && s.completed.iter().all(|p| *p) {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .context("seeder file verification timeout")??;
    let port = seed.listen_port().await?;
    let source = if magnet {
        seed.magnet().await?
    } else {
        torrent.to_str().unwrap().into()
    };
    let download = tempfile::tempdir()?;
    // Deliberately rate-limited: playback has to buffer rather than seeing holes.
    let backend = Libtorrent::open(source, download.path(), 512 * 1024).await?;
    backend.connect_peer("127.0.0.1", port).await?;
    let m = metadata(&backend).await?;
    let file = m.files.iter().find(|f| f.is_video()).unwrap().clone();
    backend.select(file.index).await?;
    let mapping = PieceMapping {
        file: file.clone(),
        piece_length: m.piece_length,
    };
    let mut scheduler = StreamScheduler::new(mapping.clone());
    let head = 2 * 1048576;
    let tail = 2 * 1048576;
    backend
        .priorities(scheduler.startup(head, tail, &[]))
        .await?;
    let startup = Instant::now();
    loop {
        let s = backend.status().await?;
        if mapping.contiguous_bytes(0, &s.completed) >= head
            && mapping.contiguous_bytes(file.size - tail, &s.completed) >= tail
        {
            break;
        }
        if startup.elapsed() > Duration::from_secs(90) {
            bail!(
                "startup timed out: rate={} peers={} pieces={}",
                s.download_rate,
                s.peers,
                s.completed.iter().filter(|p| **p).count()
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(
        backend
            .status()
            .await?
            .completed
            .iter()
            .filter(|p| **p)
            .count()
            < m.piece_count as usize / 10,
        "must start before full download"
    );
    eprintln!(
        "initial head/tail downloaded in {:.1}s",
        startup.elapsed().as_secs_f64()
    );
    let (status_tx, status_rx) = watch::channel(DownloadSnapshot::default());
    let server =
        StreamServer::start(mapping.clone(), download.path().join(&file.path), status_rx).await?;
    let mut player = Player::launch(&server.url, 4, true).await?;
    let socket_path = player.socket.clone();
    let (plan_tx, mut plan_rx) = mpsc::unbounded_channel();
    let controller_backend = backend.clone();
    let controller_mpv = player.client.clone();
    let mut demand = server.demand.clone();
    let planner_mapping = mapping.clone();
    let task = AbortTask(tokio::spawn(async move {
        let result: Result<()> = async {
            let mut interval = tokio::time::interval(Duration::from_millis(250));
            let mut detector = SeekDetector::default();
            let mut controller = BufferController::new(20, 8 * 1048576);
            let mut focus = 0;
            loop {
                tokio::select! { _ = interval.tick() => {}, changed = demand.changed() => { changed?; } }
                let status = controller_backend.status().await?;
                status_tx.send(DownloadSnapshot { status: Arc::new(status.clone()), error: None })?;
                let state = controller_mpv.state.borrow().clone();
                let read = *demand.borrow_and_update();
                let seek = detector.update(&state);
                focus = playback_focus(focus, &planner_mapping, &state, read, true);
                controller.playback(&state, planner_mapping.file.size, planner_mapping.playback_byte(&state), seek);
                let needed = read.active.then_some(read.offset);
                let updates = scheduler.plan(focus, controller.target_bytes(), controller.bitrate, &status.completed, needed);
                controller_backend.priorities(updates).await?;
                let _ = plan_tx.send(Ok((focus, scheduler.window.clone(), seek)));
            }
        }.await;
        if let Err(error) = result {
            let _ = plan_tx.send(Err(error));
        }
    }));
    wait_playing(&player, None, 120).await?;
    let state = player.client.state.borrow().clone();
    let duration = state.duration.context("mpv must expose fixture duration")?;
    assert!(
        state.stream_pos.is_some(),
        "mpv must expose actual source packet offsets"
    );
    eprintln!(
        "playing {container}: duration={duration:.1}s stream-pos={:?}",
        state.stream_pos
    );
    // Manual pause must remain paused despite an active downloading scheduler.
    player.client.pause(true).await?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(player.client.state.borrow().pause);
    player.client.pause(false).await?;
    wait_playing(&player, None, 30).await?;
    for target in [duration * 0.75, duration * 0.25] {
        while plan_rx.try_recv().is_ok() {}
        player
            .client
            .command(json!(["seek", target, "absolute", "exact"]))
            .await?;
        let seek_started = Instant::now();
        let expected_direction = target > duration * 0.5;
        let mut observed_seek = false;
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let (focus, window, seek) = plan_rx.recv().await.context("planner stopped")??;
                observed_seek |= seek;
                if (expected_direction && focus > file.size / 2)
                    || (!expected_direction && focus > file.size / 10 && focus < file.size / 2)
                {
                    let priorities = backend.priority_snapshot().await?;
                    assert!(window.is_some());
                    // Verify the REAL engine has moved critical priorities to this window.
                    let critical = priorities.iter().enumerate().any(|(p, priority)| {
                        *priority == 7 && window.as_ref().unwrap().contains(&(p as u32))
                    });
                    assert!(
                        critical,
                        "real backend did not apply critical piece priorities"
                    );
                    // The middle of the old window must lose its streaming priority.
                    let old_byte = if expected_direction {
                        1048576
                    } else {
                        file.size * 3 / 4
                    };
                    assert!(priorities[mapping.piece_at(old_byte) as usize] <= 1);
                    break Ok::<_, anyhow::Error>(());
                }
            }
        })
        .await
        .context("seek did not move the priority window within 10s")??;
        assert!(observed_seek, "must observe a real mpv seek event");
        wait_playing(&player, Some(target), 120).await?;
        let completed = backend
            .status()
            .await?
            .completed
            .iter()
            .filter(|p| **p)
            .count();
        assert!(
            completed < m.piece_count as usize / 4,
            "seek downloaded preceding movie instead of target window"
        );
        eprintln!(
            "seek to {target:.1}s resumed in {:.1}s, {completed}/{} pieces",
            seek_started.elapsed().as_secs_f64(),
            m.piece_count
        );
    }
    drop(task);
    let log = tokio::fs::read_to_string(socket_path.parent().unwrap().join("mpv.log")).await?;
    assert!(
        !log.contains("Corrupt file detected") && !log.contains("Invalid data found"),
        "mpv observed corrupt or missing data"
    );
    player.stop().await?;
    drop(player);
    assert!(!socket_path.exists(), "IPC socket must be cleaned up");
    drop(server);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "generates >=1 GiB MKV and runs a local swarm plus real mpv"]
async fn large_mkv_seek_and_pause() -> Result<()> {
    scenario("mkv", false, false).await
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "generates >=1 GiB MP4, multi-file shared pieces, magnet and real mpv"]
async fn large_mp4_multifile_magnet_slow_seek() -> Result<()> {
    scenario("mp4", true, true).await
}
