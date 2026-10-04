#![cfg(all(feature = "integration-tests", unix))]

use anyhow::{Context, Result};
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};
use torrent_stream::torrent::Libtorrent;

#[tokio::test]
async fn ctrl_c_during_startup_buffering_keeps_plain_output_and_exits() -> Result<()> {
    let source = tempfile::tempdir()?;
    tokio::fs::write(source.path().join("movie.mkv"), vec![42u8; 1048576]).await?;
    let torrent = source.path().join("movie.torrent");
    Libtorrent::make_fixture(source.path(), "movie.mkv", &torrent).await?;
    let downloads = tempfile::tempdir()?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_torrent-stream"))
        .arg(torrent)
        .args(["--no-mpv", "--path"])
        .arg(downloads.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let line = lines
                .next_line()
                .await?
                .context("CLI exited before buffering")?;
            assert!(
                !line.contains('\x1b'),
                "piped output must not contain ANSI escapes"
            );
            if line.starts_with("Buffering before playback") {
                assert!(line.contains("0.0%"));
                assert!(line.contains("startup"));
                break Ok::<_, anyhow::Error>(());
            }
        }
    })
    .await??;
    assert!(Command::new("kill")
        .args(["-INT", &child.id().unwrap().to_string()])
        .status()
        .await?
        .success());
    assert!(tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await??
        .success());
    let mut stopped = false;
    while let Some(line) = lines.next_line().await? {
        stopped |= line.contains("Stopped. Downloaded data is retained.");
    }
    assert!(stopped);
    assert_eq!(std::fs::read_dir(downloads.path())?.count(), 1);
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn terminal_dashboard_restores_screen_before_ctrl_c_message() -> Result<()> {
    use std::{
        ffi::{c_char, c_int, c_void},
        io::Read,
        os::fd::FromRawFd,
    };
    #[link(name = "util")]
    unsafe extern "C" {
        fn openpty(
            master: *mut c_int,
            slave: *mut c_int,
            name: *mut c_char,
            termios: *const c_void,
            winsize: *const c_void,
        ) -> c_int;
    }
    let source = tempfile::tempdir()?;
    tokio::fs::write(source.path().join("movie.mkv"), vec![42u8; 1048576]).await?;
    let torrent = source.path().join("movie.torrent");
    Libtorrent::make_fixture(source.path(), "movie.mkv", &torrent).await?;
    let downloads = tempfile::tempdir()?;
    let mut master_fd = -1;
    let mut slave_fd = -1;
    // Ratatui sizes its frame from the terminal, unlike the old ANSI renderer.
    #[repr(C)]
    struct WindowSize {
        rows: u16,
        columns: u16,
        x_pixels: u16,
        y_pixels: u16,
    }
    let size = WindowSize {
        rows: 24,
        columns: 80,
        x_pixels: 0,
        y_pixels: 0,
    };
    // openpty initializes both descriptors; File owns and closes each on return.
    let result = unsafe {
        openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null(),
            (&size as *const WindowSize).cast(),
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut master = unsafe { std::fs::File::from_raw_fd(master_fd) };
    let slave = unsafe { std::fs::File::from_raw_fd(slave_fd) };
    let mut child = Command::new(env!("CARGO_BIN_EXE_torrent-stream"))
        .arg(torrent)
        .args(["--no-mpv", "--path"])
        .arg(downloads.path())
        .env("TERM", "xterm-256color")
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave))
        .kill_on_drop(true)
        .spawn()?;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(Command::new("kill")
        .args(["-INT", &child.id().unwrap().to_string()])
        .status()
        .await?
        .success());
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait()).await??;
    let mut output = String::new();
    // Linux PTY masters report EIO instead of EOF after the slave closes.
    if let Err(error) = master.read_to_string(&mut output) {
        if error.raw_os_error() != Some(5) {
            return Err(error.into());
        }
    }
    assert!(status.success(), "CLI exited with {status}: {output}");
    assert!(output.contains("\x1b[?1049h"));
    assert!(output.contains("TORRENT STREAM"));
    assert!(output.contains("Verified:"));
    assert!(output.contains("Startup"));
    assert!(
        !output.contains("\x1b[6n"),
        "dashboard must not query stdin"
    );
    let restored = output
        .find("\x1b[0m\x1b[?7h\x1b[?25h\x1b[?1049l")
        .context("terminal was not restored")?;
    let stopped = output
        .find("Stopped. Downloaded data is retained.")
        .context("missing Ctrl+C message")?;
    assert!(restored < stopped);
    Ok(())
}
