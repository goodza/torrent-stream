pub mod ipc;
pub use ipc::{MpvClient, PlaybackState};

use anyhow::{Context, Result};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::process::{Child, Command};

pub struct Player {
    pub client: MpvClient,
    child: Child,
    _socket_dir: tempfile::TempDir,
    pub socket: PathBuf,
}
impl Player {
    pub async fn launch(url: &str, buffer_mb: u32, headless: bool) -> Result<Self> {
        let socket_dir = tempfile::Builder::new()
            .prefix("torrent-stream-ipc-")
            .tempdir()?;
        let socket = socket_dir.path().join("mpv.sock");
        let mut command = Command::new("mpv");
        command
            .args([
                "--no-config",
                "--idle=yes",
                "--no-terminal",
                "--really-quiet",
                "--cache=yes",
                "--cache-pause=yes",
                "--cache-pause-initial=yes",
                "--cache-pause-wait=3",
                "--demuxer-seekable-cache=yes",
            ])
            .arg(format!("--input-ipc-server={}", socket.display()))
            .arg(format!("--demuxer-max-bytes={}MiB", buffer_mb.clamp(1, 64)))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if headless {
            command.args(["--vo=null", "--ao=null"]);
            command.arg(format!(
                "--log-file={}",
                socket_dir.path().join("mpv.log").display()
            ));
        }
        let mut child = command
            .spawn()
            .context("mpv unavailable; install mpv or use --no-mpv")?;
        let client = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(status) = child.try_wait()? {
                    anyhow::bail!("mpv exited before IPC startup: {status}");
                }
                match MpvClient::connect(&socket).await {
                    Ok(client) => return Ok(client),
                    Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
                }
            }
        })
        .await
        .context("mpv IPC connection timeout")??;
        client.observe().await?;
        client.command(serde_json::json!(["loadfile", url])).await?;
        Ok(Self {
            client,
            child,
            _socket_dir: socket_dir,
            socket,
        })
    }
    pub fn exited(&mut self) -> Result<bool> {
        Ok(self.child.try_wait()?.is_some())
    }
    pub async fn stop(&mut self) -> Result<()> {
        if self.child.try_wait()?.is_none() {
            self.child.kill().await?;
        }
        self.child.wait().await?;
        Ok(())
    }
}
