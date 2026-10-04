//! A verified read barrier: sparse bytes are never passed to mpv. HTTP ranges
//! expose genuine byte demand even while a seek is waiting for its first packet.
use super::mapping::PieceMapping;
use crate::torrent::TorrentStatus;
use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{header, HeaderMap, Method, StatusCode},
    response::Response,
    routing::get,
    Router,
};
use std::{io, path::PathBuf, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt},
    sync::watch,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Default)]
pub struct DownloadSnapshot {
    pub status: Arc<TorrentStatus>,
    pub error: Option<String>,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct ReadDemand {
    pub generation: u64,
    pub offset: u64,
    pub active: bool,
}
#[derive(Clone)]
struct ServerState {
    mapping: PieceMapping,
    path: PathBuf,
    status: watch::Receiver<DownloadSnapshot>,
    demand: watch::Sender<ReadDemand>,
    cancel: CancellationToken,
}
pub struct StreamServer {
    pub url: String,
    pub demand: watch::Receiver<ReadDemand>,
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for StreamServer {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.task.abort();
    }
}
impl StreamServer {
    pub async fn start(
        mapping: PieceMapping,
        path: PathBuf,
        status: watch::Receiver<DownloadSnapshot>,
    ) -> anyhow::Result<Self> {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        // Random per-run route, never derived from untrusted torrent paths.
        let mut entropy = [0u8; 16];
        tokio::fs::File::open("/dev/urandom")
            .await?
            .read_exact(&mut entropy)
            .await?;
        let token: String = entropy.iter().map(|b| format!("{b:02x}")).collect();
        let route = format!("/{token}/video.{}", mapping.file.extension());
        let url = format!("http://{address}{route}");
        let cancel = CancellationToken::new();
        let (demand_tx, demand) = watch::channel(ReadDemand::default());
        let state = ServerState {
            mapping,
            path,
            status,
            demand: demand_tx,
            cancel: cancel.clone(),
        };
        let app = Router::new().route(&route, get(serve)).with_state(state);
        let shutdown = cancel.clone();
        let task = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, app)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await
            {
                tracing::error!(%error, "stream server failed");
            }
        });
        Ok(Self {
            url,
            demand,
            cancel,
            task,
        })
    }
}

#[derive(Debug, thiserror::Error)]
#[error("invalid or unsatisfiable byte range")]
pub struct RangeError;
/// Returns [start, end), validates one RFC 9110 byte range, including suffixes.
pub fn parse_range(header: Option<&str>, size: u64) -> Result<(u64, u64), RangeError> {
    let Some(header) = header else {
        return Ok((0, size));
    };
    let range = header.strip_prefix("bytes=").ok_or(RangeError)?;
    if range.contains(',') {
        return Err(RangeError);
    }
    let (left, right) = range.split_once('-').ok_or(RangeError)?;
    if left.is_empty() {
        let suffix = right.parse::<u64>().map_err(|_| RangeError)?;
        if suffix == 0 || size == 0 {
            return Err(RangeError);
        }
        return Ok((size.saturating_sub(suffix), size));
    }
    let start = left.parse::<u64>().map_err(|_| RangeError)?;
    if start >= size {
        return Err(RangeError);
    }
    let end = if right.is_empty() {
        size
    } else {
        let last = right.parse::<u64>().map_err(|_| RangeError)?;
        if last < start {
            return Err(RangeError);
        }
        last.saturating_add(1).min(size)
    };
    Ok((start, end))
}
struct DemandGuard {
    sender: watch::Sender<ReadDemand>,
    generation: u64,
}
impl Drop for DemandGuard {
    fn drop(&mut self) {
        self.sender.send_if_modified(|s| {
            if s.generation == self.generation {
                s.active = false;
                true
            } else {
                false
            }
        });
    }
}
async fn serve(State(state): State<ServerState>, method: Method, headers: HeaderMap) -> Response {
    let size = state.mapping.file.size;
    let header = if method == Method::HEAD {
        None
    } else {
        headers.get(header::RANGE)
    };
    let range = match header.map(|h| h.to_str()).transpose() {
        Ok(h) => parse_range(h, size),
        Err(_) => Err(RangeError),
    };
    let (start, end) = match range {
        Ok(range) => range,
        Err(_) => {
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{size}"))
                .body(Body::empty())
                .unwrap()
        }
    };
    let mut response = Response::builder()
        .status(if header.is_some() {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        })
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, end - start)
        .header(header::CONTENT_TYPE, "application/octet-stream");
    if header.is_some() {
        response = response.header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{}/{size}", end - 1),
        );
    }
    if method == Method::HEAD {
        return response.body(Body::empty()).unwrap();
    }
    let mut demand = state.demand.subscribe();
    let mut generation = 0;
    state.demand.send_modify(|s| {
        s.generation = s.generation.wrapping_add(1);
        generation = s.generation;
        s.offset = start;
        s.active = true;
    });
    let guard = DemandGuard {
        sender: state.demand.clone(),
        generation,
    };
    let stream = async_stream::try_stream! {
        let _guard = guard;
        let mut position = start;
        let mut status = state.status.clone();
        let mut file = None;
        while position < end {
            if state.cancel.is_cancelled() || demand.borrow().generation != generation {
                Err(io::Error::new(io::ErrorKind::Interrupted, "stream superseded or stopped"))?;
            }
            let piece = state.mapping.piece_at(position);
            loop {
                let snapshot = status.borrow().clone();
                if let Some(error) = snapshot.error { Err(io::Error::other(error))?; }
                if snapshot.status.completed.get(piece as usize) == Some(&true) { break; }
                let result: io::Result<()> = tokio::select! {
                    _ = state.cancel.cancelled() => Err(io::Error::new(io::ErrorKind::Interrupted, "stream stopped")),
                    changed = status.changed() => changed.map_err(|_| io::Error::other("torrent task stopped")),
                    changed = demand.changed() => {
                        if changed.is_err() { Err(io::Error::other("scheduler stopped")) }
                        else if demand.borrow().generation != generation { Err(io::Error::new(io::ErrorKind::Interrupted, "superseded read")) }
                        else { Ok(()) }
                    }
                };
                result?;
            }
            if file.is_none() {
                file = Some(tokio::fs::File::open(&state.path).await?);
            }
            let file = file.as_mut().unwrap();
            let count = (state.mapping.piece_end_in_file(piece).min(end) - position).min(64 * 1024) as usize;
            let mut bytes = vec![0; count];
            file.seek(io::SeekFrom::Start(position)).await?;
            file.read_exact(&mut bytes).await?;
            position += count as u64;
            state.demand.send_if_modified(|s| {
                if s.generation == generation && state.mapping.piece_at(s.offset) != state.mapping.piece_at(position) {
                    s.offset = position; true
                } else { false }
            });
            yield Bytes::from(bytes);
        }
    };
    response.body(verified_body(stream)).unwrap()
}
fn verified_body(
    stream: impl futures_core::Stream<Item = io::Result<Bytes>> + Send + 'static,
) -> Body {
    Body::from_stream(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn byte_range_forms_and_errors() {
        assert_eq!(parse_range(None, 100).unwrap(), (0, 100));
        for (h, expected) in [
            ("bytes=0-0", (0, 1)),
            ("bytes=90-", (90, 100)),
            ("bytes=-20", (80, 100)),
            ("bytes=20-999", (20, 100)),
            ("bytes=-999", (0, 100)),
        ] {
            assert_eq!(parse_range(Some(h), 100).unwrap(), expected);
        }
        for h in [
            "bytes=-0",
            "bytes=100-",
            "bytes=20-10",
            "bytes=a-b",
            "bytes=0-1,3-4",
            "items=1-2",
        ] {
            assert!(parse_range(Some(h), 100).is_err());
        }
    }
    #[tokio::test]
    async fn blocked_reads_wait_for_holes_and_seek_cancels_them() {
        use crate::torrent::TorrentFile;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("movie.mkv");
        tokio::fs::write(&path, b"AAAABBBBCCCC").await.unwrap();
        let mapping = PieceMapping {
            file: TorrentFile {
                index: 0,
                path: "movie.mkv".into(),
                size: 12,
                offset: 0,
                symlink: false,
                pad: false,
            },
            piece_length: 4,
        };
        let (tx, rx) = watch::channel(DownloadSnapshot {
            status: Arc::new(TorrentStatus {
                completed: vec![true, false, true],
                ..Default::default()
            }),
            error: None,
        });
        let server = StreamServer::start(mapping, path, rx).await.unwrap();
        let client = reqwest::Client::new();
        let mut blocked = client
            .get(&server.url)
            .header("Range", "bytes=4-7")
            .send()
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), blocked.chunk())
                .await
                .is_err()
        );
        let jumped = client
            .get(&server.url)
            .header("Range", "bytes=8-11")
            .send()
            .await
            .unwrap();
        assert_eq!(jumped.bytes().await.unwrap().as_ref(), b"CCCC");
        assert!(blocked.chunk().await.is_err());
        let mut resumed = client
            .get(&server.url)
            .header("Range", "bytes=4-7")
            .send()
            .await
            .unwrap();
        tx.send_modify(|s| {
            s.status = Arc::new(TorrentStatus {
                completed: vec![true; 3],
                ..Default::default()
            })
        });
        assert_eq!(resumed.chunk().await.unwrap().unwrap().as_ref(), b"BBBB");
    }
}
