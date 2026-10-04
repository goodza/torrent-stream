use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::{collections::HashMap, path::Path, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    sync::{mpsc, oneshot, watch},
};

#[derive(Clone, Debug, Default)]
pub struct PlaybackState {
    pub time_pos: Option<f64>,
    pub duration: Option<f64>,
    pub pause: bool,
    pub cache_duration: Option<f64>,
    pub cache_time: Option<f64>,
    pub file_size: Option<u64>,
    pub stream_pos: Option<u64>,
    pub stream_end: Option<u64>,
    pub paused_for_cache: bool,
    pub seeking: bool,
    pub seek_generation: u64,
    pub ended: bool,
    pub error: Option<String>,
    pub disconnected: bool,
}
impl PlaybackState {
    fn event(&mut self, value: &Value) {
        match value["event"].as_str().unwrap_or("") {
            "seek" => {
                self.seek_generation += 1;
                self.seeking = true;
            }
            "playback-restart" => self.seeking = false,
            "end-file" => {
                self.ended = true;
                if value["reason"] == "error" {
                    self.error = Some(
                        value["file_error"]
                            .as_str()
                            .unwrap_or("mpv failed to decode media")
                            .into(),
                    );
                }
            }
            "property-change" => {
                let data = &value["data"];
                match value["name"].as_str().unwrap_or("") {
                    "time-pos" => self.time_pos = data.as_f64(),
                    "duration" => self.duration = data.as_f64(),
                    "pause" => self.pause = data.as_bool().unwrap_or(false),
                    "demuxer-cache-duration" => self.cache_duration = data.as_f64(),
                    "demuxer-cache-time" => self.cache_time = data.as_f64(),
                    "file-size" => self.file_size = data.as_u64(),
                    "stream-pos" => self.stream_pos = data.as_u64(),
                    "stream-end" => self.stream_end = data.as_u64(),
                    "paused-for-cache" => self.paused_for_cache = data.as_bool().unwrap_or(false),
                    "seeking" => self.seeking = data.as_bool().unwrap_or(false),
                    _ => {}
                }
            }
            _ => {}
        }
    }
}
struct Request {
    command: Value,
    response: oneshot::Sender<Result<Value>>,
}
#[derive(Clone)]
pub struct MpvClient {
    commands: mpsc::Sender<Request>,
    pub state: watch::Receiver<PlaybackState>,
}
impl MpvClient {
    pub async fn connect(path: &Path) -> Result<Self> {
        #[cfg(unix)]
        let stream = tokio::net::UnixStream::connect(path).await?;
        #[cfg(windows)]
        let stream = tokio::net::windows::named_pipe::ClientOptions::new().open(path)?;
        Ok(Self::from_stream(stream))
    }
    fn from_stream<T: AsyncRead + AsyncWrite + Unpin + Send + 'static>(stream: T) -> Self {
        let (commands, mut requests) = mpsc::channel::<Request>(32);
        let (state_tx, state) = watch::channel(PlaybackState::default());
        tokio::spawn(async move {
            let (read, mut write) = tokio::io::split(stream);
            let mut lines = BufReader::new(read).lines();
            let mut pending: HashMap<u64, oneshot::Sender<Result<Value>>> = HashMap::new();
            let mut next_id = 0u64;
            loop {
                tokio::select! {
                    request = requests.recv() => {
                        let Some(request) = request else { break; };
                        pending.retain(|_, reply| !reply.is_closed());
                        next_id += 1;
                        let mut bytes = json!({"command": request.command, "request_id": next_id}).to_string().into_bytes(); bytes.push(b'\n');
                        if write.write_all(&bytes).await.is_err() { break; }
                        pending.insert(next_id, request.response);
                    }
                    line = lines.next_line() => {
                        let line = match line { Ok(Some(line)) => line, _ => break };
                        let value: Value = match serde_json::from_str(&line) { Ok(value) => value, Err(_) => continue };
                        if let Some(id) = value["request_id"].as_u64() {
                            if let Some(reply) = pending.remove(&id) {
                                let response = if value["error"] == "success" { Ok(value["data"].clone()) }
                                    else { Err(anyhow::anyhow!("mpv command failed: {}", value["error"])) };
                                let _ = reply.send(response);
                            }
                        } else if value.get("event").is_some() {
                            state_tx.send_modify(|state| state.event(&value));
                        }
                    }
                }
            }
            state_tx.send_modify(|s| s.disconnected = true);
            for (_, reply) in pending {
                let _ = reply.send(Err(anyhow::anyhow!("mpv IPC disconnected")));
            }
        });
        Self { commands, state }
    }
    pub async fn command(&self, command: Value) -> Result<Value> {
        let (response, reply) = oneshot::channel();
        tokio::time::timeout(Duration::from_secs(5), async {
            self.commands
                .send(Request { command, response })
                .await
                .context("mpv IPC disconnected")?;
            reply.await.context("mpv IPC disconnected")?
        })
        .await
        .context("mpv IPC command timeout")?
    }
    pub async fn observe(&self) -> Result<()> {
        for (id, name) in [
            "time-pos",
            "duration",
            "pause",
            "demuxer-cache-duration",
            "demuxer-cache-time",
            "file-size",
            "stream-pos",
            "stream-end",
            "paused-for-cache",
            "seeking",
        ]
        .iter()
        .enumerate()
        {
            // Unsupported properties emit unavailable values, handled as Option.
            self.command(json!(["observe_property", id, name])).await?;
        }
        Ok(())
    }
    pub async fn pause(&self, pause: bool) -> Result<()> {
        if self.state.borrow().disconnected {
            bail!("mpv IPC disconnected");
        }
        self.command(json!(["set_property", "pause", pause]))
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn correlates_responses_and_observes_seek() {
        let (client_stream, server_stream) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            let (r, mut w) = tokio::io::split(server_stream);
            let mut lines = BufReader::new(r).lines();
            let request: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            w.write_all(b"{\"event\":\"seek\"}\n{\"event\":\"property-change\",\"name\":\"stream-pos\",\"data\":123456}\n").await.unwrap();
            let response =
                json!({"request_id": request["request_id"], "error": "success", "data": 42});
            w.write_all(format!("{response}\n").as_bytes())
                .await
                .unwrap();
        });
        let client = MpvClient::from_stream(client_stream);
        assert_eq!(
            client
                .command(json!(["get_property", "duration"]))
                .await
                .unwrap(),
            42
        );
        assert_eq!(client.state.borrow().stream_pos, Some(123456));
        assert_eq!(client.state.borrow().seek_generation, 1);
        server.await.unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn connects_to_windows_named_pipe() {
        let dir = tempfile::tempdir().unwrap();
        let path = std::path::PathBuf::from(format!(
            r"\\.\pipe\torrent-stream-test-{}-{}",
            std::process::id(),
            dir.path().file_name().unwrap().to_str().unwrap()
        ));
        let pipe = tokio::net::windows::named_pipe::ServerOptions::new()
            .first_pipe_instance(true)
            .create(&path)
            .unwrap();
        let client = MpvClient::connect(&path).await.unwrap();
        pipe.connect().await.unwrap();
        let server = tokio::spawn(async move {
            let (r, mut w) = tokio::io::split(pipe);
            let mut lines = BufReader::new(r).lines();
            let request: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            let response =
                json!({"request_id": request["request_id"], "error": "success", "data": 123});
            w.write_all(format!("{response}\n").as_bytes())
                .await
                .unwrap();
        });
        assert_eq!(
            client
                .command(json!(["get_property", "duration"]))
                .await
                .unwrap(),
            123
        );
        server.await.unwrap();
    }
}
