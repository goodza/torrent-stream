#![cfg(feature = "integration-tests")]
use anyhow::{Context, Result};
use std::{path::Path, time::Duration};
use tokio::process::Command;
#[cfg(unix)]
use torrent_stream::downloads::CacheState;
use torrent_stream::{downloads::DownloadCache, torrent::*};

fn cli(config: &Path, downloads: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_torrent-stream"));
    command
        .arg("--config")
        .arg(config)
        .arg("--path")
        .arg(downloads)
        .kill_on_drop(true);
    command
}
async fn output(command: &mut Command) -> Result<std::process::Output> {
    Ok(
        tokio::time::timeout(Duration::from_secs(20), command.output())
            .await
            .context("CLI timeout")??,
    )
}
fn success(output: &std::process::Output) {
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn settings_and_cache_commands_skip_clipboard_and_protect_active_downloads() -> Result<()> {
    let root = tempfile::tempdir()?;
    let config = root.path().join("config.json");
    let downloads = root.path().join("downloads");
    success(&output(cli(&config, &downloads).args(["config", "set", "buffer-mb", "42"])).await?);
    let shown = output(cli(&config, &downloads).args(["config", "show"])).await?;
    success(&shown);
    assert!(String::from_utf8_lossy(&shown.stdout).contains("42"));
    assert!(
        !output(cli(&config, &downloads).args(["config", "set", "buffer-mb", "0"]))
            .await?
            .status
            .success()
    );
    tokio::fs::create_dir(&downloads).await?;
    let key = "v1-0123456789abcdef0123456789abcdef01234567";
    let cache = DownloadCache::open(&downloads, key)?;
    tokio::fs::write(cache.path.join("movie.mkv"), b"retained data").await?;
    let listed = output(cli(&config, &downloads).args(["cache", "list"])).await?;
    success(&listed);
    assert!(String::from_utf8_lossy(&listed.stdout).contains(key));
    assert!(
        !output(cli(&config, &downloads).args(["cache", "remove", key, "--yes"]))
            .await?
            .status
            .success()
    );
    assert!(cache.path.join("movie.mkv").exists());
    drop(cache);
    assert!(
        !output(cli(&config, &downloads).args(["cache", "remove", key]))
            .await?
            .status
            .success()
    );
    success(&output(cli(&config, &downloads).args(["cache", "remove", key, "--yes"])).await?);
    assert!(!downloads.join(format!("torrent-{key}")).exists());
    assert!(!downloads.join(".torrent-stream/records").join(key).exists());
    let other_key = "v1-1123456789abcdef0123456789abcdef01234567";
    let active = DownloadCache::open(&downloads, key)?;
    let inactive = DownloadCache::open(&downloads, other_key)?;
    let inactive_path = inactive.path.clone();
    drop(inactive);
    assert!(
        !output(cli(&config, &downloads).args(["cache", "clear", "--yes"]))
            .await?
            .status
            .success()
    );
    assert!(
        inactive_path.exists(),
        "clear must preflight all active locks before deletion"
    );
    drop(active);
    success(&output(cli(&config, &downloads).args(["cache", "clear", "--yes"])).await?);
    assert!(!inactive_path.exists());
    // Reset all remains available even after a manually corrupted config.
    tokio::fs::write(&config, b"not json").await?;
    success(&output(cli(&config, &downloads).args(["config", "reset"])).await?);
    Ok(())
}

#[tokio::test]
async fn download_playlist_subtitles_and_offline_magnet_metadata() -> Result<()> {
    let root = tempfile::tempdir()?;
    let fixture = root.path().join("fixture");
    let downloads = root.path().join("downloads");
    let config = root.path().join("config.json");
    tokio::fs::create_dir_all(fixture.join("series")).await?;
    tokio::fs::create_dir(&downloads).await?;
    for (name, byte) in [("ep10.mkv", 10), ("ep2.mkv", 2), ("ep2.en.srt", 3)] {
        tokio::fs::write(fixture.join("series").join(name), vec![byte; 256 * 1024]).await?;
    }
    let torrent = root.path().join("series.torrent");
    Libtorrent::make_fixture(&fixture, "series", &torrent).await?;
    let seed = Libtorrent::open(torrent.to_str().unwrap().into(), &fixture, 0).await?;
    let magnet = seed.magnet().await?;
    drop(seed);
    let key = Libtorrent::source_key(torrent.to_str().unwrap().into()).await?;
    let cache = DownloadCache::open(&downloads, &key)?;
    tokio::fs::create_dir(cache.path.join("series")).await?;
    for name in ["ep10.mkv", "ep2.mkv", "ep2.en.srt"] {
        tokio::fs::copy(
            fixture.join("series").join(name),
            cache.path.join("series").join(name),
        )
        .await?;
    }
    let record = cache.record.clone();
    drop(cache);
    let first = output(cli(&config, &downloads).arg(&torrent).args([
        "--file",
        "2",
        "--playlist",
        "--download-only",
    ]))
    .await?;
    success(&first);
    let text = String::from_utf8_lossy(&first.stdout);
    assert!(
        text.find("Selected: series/ep2.mkv").unwrap()
            < text.find("Selected: series/ep10.mkv").unwrap()
    );
    assert!(!text.contains("Stream URL:"));
    assert!(record.join("metadata.torrent").exists());
    // Saved metainfo plus verified payload must work with no seeder.
    let second = output(cli(&config, &downloads).arg(&magnet).args([
        "--file",
        "2",
        "--download-only",
        "--metadata-timeout",
        "1",
    ]))
    .await?;
    success(&second);
    assert!(String::from_utf8_lossy(&second.stdout).contains("Using saved torrent metadata."));
    // Wrong identity and malformed metadata must never be trusted.
    let other_file = fixture.join("other.mkv");
    tokio::fs::write(&other_file, b"other").await?;
    let other = root.path().join("other.torrent");
    Libtorrent::make_fixture(&fixture, "other.mkv", &other).await?;
    for bytes in [tokio::fs::read(other).await?, b"corrupt".to_vec()] {
        tokio::fs::write(record.join("metadata.torrent"), bytes).await?;
        let rejected = output(cli(&config, &downloads).arg(&magnet).args([
            "--file",
            "2",
            "--download-only",
            "--metadata-timeout",
            "1",
        ]))
        .await?;
        assert!(!rejected.status.success());
        assert!(
            String::from_utf8_lossy(&rejected.stderr).contains("Ignoring saved torrent metadata")
        );
        assert!(String::from_utf8_lossy(&rejected.stderr).contains("metadata timeout"));
    }
    Ok(())
}

#[tokio::test]
async fn download_only_fetches_payload_and_exits_and_reports_stalls() -> Result<()> {
    let root = tempfile::tempdir()?;
    let fixture = root.path().join("fixture");
    let downloads = root.path().join("downloads");
    let config = root.path().join("config.json");
    tokio::fs::create_dir(&fixture).await?;
    tokio::fs::create_dir(&downloads).await?;
    let contents = vec![53; 1024 * 1024];
    tokio::fs::write(fixture.join("movie.mkv"), &contents).await?;
    let torrent = root.path().join("movie.torrent");
    Libtorrent::make_fixture(&fixture, "movie.mkv", &torrent).await?;
    let seed = Libtorrent::open(torrent.to_str().unwrap().into(), &fixture, 0).await?;
    seed.select(0).await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = seed.status().await?;
            if status.completed.len() == 4 && status.completed.iter().all(|p| *p) {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await??;
    let magnet = format!(
        "{}&x.pe=127.0.0.1:{}",
        seed.magnet().await?,
        seed.listen_port().await?
    );
    let result = output(cli(&config, &downloads).arg(magnet).arg("--download-only")).await?;
    success(&result);
    let key = Libtorrent::source_key(torrent.to_str().unwrap().into()).await?;
    assert_eq!(
        tokio::fs::read(downloads.join(format!("torrent-{key}/movie.mkv"))).await?,
        contents
    );
    drop(seed);
    let empty = root.path().join("empty");
    tokio::fs::create_dir(&empty).await?;
    let stalled = output(cli(&config, &empty).arg(&torrent).args([
        "--download-only",
        "--stall-timeout",
        "1",
    ]))
    .await?;
    assert!(!stalled.status.success());
    assert!(String::from_utf8_lossy(&stalled.stderr).contains("stalled"));
    Ok(())
}

#[cfg(unix)]
async fn video(path: &Path, duration: &str) -> Result<()> {
    let result = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=size=160x90:rate=2",
            "-t",
            duration,
            "-c:v",
            "mpeg4",
            "-y",
        ])
        .arg(path)
        .output()
        .await?;
    success(&result);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires ffmpeg and real mpv with null outputs"]
async fn headless_playback_saves_resumes_clears_history_and_loads_subtitles() -> Result<()> {
    use std::{os::unix::fs::PermissionsExt, process::Stdio};
    use tokio::io::{AsyncBufReadExt, BufReader};
    let root = tempfile::tempdir()?;
    let fixture = root.path().join("fixture");
    let downloads = root.path().join("downloads");
    let bin = root.path().join("bin");
    let config = root.path().join("config.json");
    tokio::fs::create_dir_all(fixture.join("series")).await?;
    tokio::fs::create_dir(&downloads).await?;
    tokio::fs::create_dir(&bin).await?;
    video(&fixture.join("series/ep2.mkv"), "120").await?;
    video(&fixture.join("series/ep10.mkv"), "3").await?;
    tokio::fs::write(
        fixture.join("series/ep2.en.srt"),
        b"1\n00:00:00,000 --> 00:01:59,000\nTest subtitle\n",
    )
    .await?;
    let torrent = root.path().join("series.torrent");
    Libtorrent::make_fixture(&fixture, "series", &torrent).await?;
    let key = Libtorrent::source_key(torrent.to_str().unwrap().into()).await?;
    let cache = DownloadCache::open(&downloads, &key)?;
    tokio::fs::create_dir(cache.path.join("series")).await?;
    for name in ["ep2.mkv", "ep10.mkv", "ep2.en.srt"] {
        tokio::fs::copy(
            fixture.join("series").join(name),
            cache.path.join("series").join(name),
        )
        .await?;
    }
    let record = cache.record.clone();
    drop(cache);
    let mpv = Command::new("which").arg("mpv").output().await?;
    success(&mpv);
    let mpv = String::from_utf8(mpv.stdout)?.trim().to_owned();
    // Test-only wrapper selects null outputs without changing production behavior.
    let wrapper = bin.join("mpv");
    tokio::fs::write(&wrapper,format!("#!/bin/sh\nexec '{}' --vo=null --ao=null --speed=10 --log-file='{}'/mpv-$$.log \"$@\"\n",mpv.replace('\'',"'\\''"),root.path().display())).await?;
    tokio::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).await?;
    let path = format!("{}:{}", bin.display(), std::env::var("PATH")?);
    let mut child = cli(&config, &downloads)
        .arg(&torrent)
        .args(["--file", "2", "--playlist", "--subtitle-language", "en"])
        .env("PATH", &path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let line = lines
                .next_line()
                .await?
                .context("playback stopped before startup")?;
            if line.contains("Starting mpv") {
                break Ok::<_, anyhow::Error>(());
            }
        }
    })
    .await??;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let state = CacheState::read(&record);
            if state.bookmarks.contains_key("series/ep2.mkv") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .context("periodic bookmark not saved")?;
    success(
        &Command::new("kill")
            .args(["-INT", &child.id().unwrap().to_string()])
            .output()
            .await?,
    );
    assert!(tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await??
        .success());
    while let Some(line) = lines.next_line().await? {
        assert!(
            !line.contains("Selected: series/ep10.mkv"),
            "Ctrl+C must not advance the playlist"
        );
    }
    let saved = CacheState::read(&record);
    assert!(saved.bookmarks["series/ep2.mkv"].position >= 10.0);
    let resumed = output(
        cli(&config, &downloads)
            .arg(&torrent)
            .args(["--file", "2", "--playlist", "--subtitle-language", "en"])
            .env("PATH", &path),
    )
    .await?;
    success(&resumed);
    let text = String::from_utf8_lossy(&resumed.stdout);
    assert!(text.contains("Resuming at"), "{text}");
    assert!(
        text.contains("Selected: series/ep10.mkv"),
        "natural EOF should advance: {text}"
    );
    assert!(CacheState::read(&record).bookmarks.is_empty());
    let mut history = CacheState::read(&record);
    history.bookmarks.insert(
        "series/ep2.mkv".into(),
        torrent_stream::downloads::Bookmark {
            position: 45.0,
            duration: Some(120.0),
            size: std::fs::metadata(fixture.join("series/ep2.mkv"))?.len(),
        },
    );
    history.save(&record)?;
    let fresh = output(
        cli(&config, &downloads)
            .arg(&torrent)
            .args(["--file", "2", "--no-resume"])
            .env("PATH", &path),
    )
    .await?;
    success(&fresh);
    assert!(!String::from_utf8_lossy(&fresh.stdout).contains("Resuming at"));
    assert!(CacheState::read(&record).bookmarks.is_empty());
    let logs = std::fs::read_dir(root.path())?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".log"))
        .map(|e| std::fs::read_to_string(e.path()))
        .collect::<std::io::Result<Vec<_>>>()?
        .join("\n");
    assert!(
        logs.contains("ep2.en.srt"),
        "mpv must load the verified sidecar: {logs}"
    );
    Ok(())
}

#[tokio::test]
async fn cached_metainfo_preserves_v1_v2_hybrid_identity_and_layers() -> Result<()> {
    let root = tempfile::tempdir()?;
    let contents = vec![64; 1024 * 1024];
    tokio::fs::write(root.path().join("movie.mkv"), contents).await?;
    for version in 1..=3 {
        let torrent = root.path().join(format!("v{version}.torrent"));
        Libtorrent::make_fixture_version(root.path(), "movie.mkv", &torrent, version).await?;
        let key = Libtorrent::source_key(torrent.to_str().unwrap().into()).await?;
        let backend = Libtorrent::open(torrent.to_str().unwrap().into(), root.path(), 0).await?;
        let bytes = backend.export_metadata().await?;
        let cached = root.path().join(format!("cached-v{version}.torrent"));
        tokio::fs::write(&cached, bytes).await?;
        assert_eq!(
            Libtorrent::source_key(cached.to_str().unwrap().into()).await?,
            key
        );
        let magnet = backend.magnet().await?;
        drop(backend);
        let saved = Libtorrent::open_cached(magnet, root.path(), 0, Some(&cached)).await?;
        saved.select(0).await?;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let status = saved.status().await?;
                if !status.checking
                    && status.completed.len() == 4
                    && status.completed.iter().all(|p| *p)
                {
                    break Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .context("cached metainfo must verify without network hashes")??;
    }
    Ok(())
}

#[tokio::test]
async fn engine_retains_cache_lock_and_selection_clears_old_priorities() -> Result<()> {
    let root = tempfile::tempdir()?;
    let fixture = root.path().join("fixture");
    let downloads = root.path().join("downloads");
    tokio::fs::create_dir_all(fixture.join("series")).await?;
    tokio::fs::create_dir(&downloads).await?;
    for name in ["a.mkv", "b.mkv"] {
        tokio::fs::write(fixture.join("series").join(name), vec![7; 256 * 1024]).await?;
    }
    let torrent = root.path().join("series.torrent");
    Libtorrent::make_fixture(&fixture, "series", &torrent).await?;
    let key = Libtorrent::source_key(torrent.to_str().unwrap().into()).await?;
    let cache = std::sync::Arc::new(DownloadCache::open(&downloads, &key)?);
    let backend = Libtorrent::open(torrent.to_str().unwrap().into(), &cache.path, 0).await?;
    backend.retain_cache(cache.clone())?;
    drop(cache);
    assert!(DownloadCache::open(&downloads, &key).is_err());
    backend.select(0).await?;
    backend
        .priorities(vec![PriorityUpdate {
            piece: 0,
            priority: 7,
            deadline_ms: 0,
        }])
        .await?;
    backend.select(1).await?;
    assert_eq!(backend.priority_snapshot().await?, [0, 1]);
    let other = backend.clone();
    drop(backend);
    assert!(DownloadCache::open(&downloads, &key).is_err());
    drop(other);
    assert!(DownloadCache::open(&downloads, &key).is_ok());
    Ok(())
}
