#![cfg(feature = "integration-tests")]
use anyhow::{Context, Result};
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};
use torrent_stream::torrent::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_lists_selects_streams_magnet_and_handles_ctrl_c() -> Result<()> {
    let root = tempfile::tempdir()?;
    let contents = vec![123u8; 3 * 1048576];
    tokio::fs::write(root.path().join("Фильм.mkv"), &contents).await?;
    let torrent = root.path().join("fixture.torrent");
    Libtorrent::make_fixture(root.path(), "Фильм.mkv", &torrent).await?;
    let executable = env!("CARGO_BIN_EXE_torrent-stream");
    let downloads = tempfile::tempdir()?;
    let listed = Command::new(executable)
        .arg(&torrent)
        .arg("--list-files")
        .arg("--download-dir")
        .arg(downloads.path())
        .output()
        .await?;
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    assert!(String::from_utf8_lossy(&listed.stdout).contains("Фильм.mkv"));
    assert_eq!(
        std::fs::read_dir(downloads.path())?.count(),
        0,
        "listing must clean its private directory"
    );
    let invalid = Command::new(executable)
        .arg(&torrent)
        .args(["--file", "99", "--no-mpv", "--download-dir"])
        .arg(downloads.path())
        .output()
        .await?;
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("out of range"));

    let seed = Libtorrent::open(torrent.to_str().unwrap().into(), root.path(), 0).await?;
    seed.select(0).await?;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if seed
                .status()
                .await?
                .completed
                .iter()
                .filter(|p| **p)
                .count()
                == 12
            {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await??;
    let source = format!(
        "  {}&x.pe=127.0.0.1:{}\n",
        seed.magnet().await?.replacen("magnet:", "MAGNET:", 1),
        seed.listen_port().await?
    );
    let mut child = Command::new(executable)
        .arg(source)
        .args([
            "--no-mpv",
            "--startup-head-mb",
            "1",
            "--startup-tail-mb",
            "0",
            "--buffer-mb",
            "1",
            "--download-dir",
        ])
        .arg(downloads.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let url = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let line = lines
                .next_line()
                .await?
                .context("CLI exited before publishing stream URL")?;
            if let Some(url) = line.strip_prefix("Stream URL: ") {
                break Ok::<_, anyhow::Error>(url.to_owned());
            }
        }
    })
    .await
    .context("CLI buffering timeout")??;
    let response = reqwest::Client::new()
        .get(&url)
        .header("Range", "bytes=17-31")
        .send()
        .await?;
    assert_eq!(response.status(), 206);
    assert_eq!(response.bytes().await?.as_ref(), &contents[17..32]);
    let signal = Command::new("kill")
        .args(["-INT", &child.id().unwrap().to_string()])
        .status()
        .await?;
    assert!(signal.success());
    let exit = tokio::time::timeout(Duration::from_secs(10), child.wait()).await??;
    assert!(exit.success());
    assert!(lines.next_line().await?.is_some());
    assert!(
        std::fs::read_dir(downloads.path())?.count() > 0,
        "Ctrl+C must retain downloaded data"
    );
    assert!(
        reqwest::get(&url).await.is_err(),
        "HTTP listener must close on Ctrl+C"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_magnet_flags_fetch_real_metadata() -> Result<()> {
    let root = tempfile::tempdir()?;
    tokio::fs::write(root.path().join("movie.mp4"), vec![42u8; 256 * 1024]).await?;
    let torrent = root.path().join("fixture.torrent");
    Libtorrent::make_fixture(root.path(), "movie.mp4", &torrent).await?;
    let seed = Libtorrent::open(torrent.to_str().unwrap().into(), root.path(), 0).await?;
    seed.select(0).await?;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let status = seed.status().await?;
            if status.completed.len() == 1 && status.completed[0] {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await??;
    let uri = format!(
        " \n{}&x.pe=127.0.0.1:{}\n",
        seed.magnet().await?.replacen("magnet:", "MaGnEt:", 1),
        seed.listen_port().await?
    );
    let downloads = tempfile::tempdir()?;
    for flag in ["--magnet", "-m"] {
        let response = tokio::time::timeout(
            Duration::from_secs(30),
            Command::new(env!("CARGO_BIN_EXE_torrent-stream"))
                .args([
                    flag,
                    &uri,
                    "--list-files",
                    "--metadata-timeout",
                    "20",
                    "--download-dir",
                ])
                .arg(downloads.path())
                .output(),
        )
        .await??;
        assert!(
            response.status.success(),
            "{flag}: {}",
            String::from_utf8_lossy(&response.stderr)
        );
        assert!(String::from_utf8_lossy(&response.stdout).contains("movie.mp4"));
    }
    Ok(())
}

#[tokio::test]
async fn invalid_torrent_and_metadata_timeout_are_actionable() -> Result<()> {
    let root = tempfile::tempdir()?;
    let invalid = root.path().join("invalid.torrent");
    tokio::fs::write(&invalid, "garbage").await?;
    let executable = env!("CARGO_BIN_EXE_torrent-stream");
    let response = Command::new(executable)
        .arg(invalid)
        .args(["--list-files", "--download-dir"])
        .arg(root.path())
        .output()
        .await?;
    assert!(!response.status.success());
    assert!(String::from_utf8_lossy(&response.stderr).contains("libtorrent"));
    let response = Command::new(executable)
        .args([
            "--magnet",
            "magnet:?xt=urn:btih:not-a-valid-hash",
            "--list-files",
            "--download-dir",
        ])
        .arg(root.path())
        .output()
        .await?;
    assert!(!response.status.success());
    let error = String::from_utf8_lossy(&response.stderr);
    assert!(error.contains("add magnet link to torrent engine"));
    assert!(error.contains("libtorrent"));
    assert!(!error.contains("invalid torrent path"));
    let response = tokio::time::timeout(
        Duration::from_secs(10),
        Command::new(executable)
            .arg("magnet:?xt=urn:btih:0000000000000000000000000000000000000001")
            .args(["--list-files", "--metadata-timeout", "1", "--download-dir"])
            .arg(root.path())
            .output(),
    )
    .await??;
    assert!(!response.status.success());
    assert!(String::from_utf8_lossy(&response.stderr).contains("metadata timeout"));
    Ok(())
}
