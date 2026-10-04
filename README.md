# torrent-stream

A Rust CLI that starts playing a torrent video before it finishes downloading.
It tracks mpv over JSON IPC and moves a tiered piece-priority window with playback
and seeks. A verified read barrier prevents sparse-file holes from reaching mpv.

## Install and run

Download prebuilt binaries from [GitHub Releases](https://github.com/goodza/torrent-stream/releases).
Extract the entire archive, keeping its bundled libraries beside the executable.
Install mpv separately and put it on `PATH`. Available platforms:

| Archive target | Platform |
| --- | --- |
| `x86_64-unknown-linux-gnu` | Linux x64, Ubuntu 22.04 / glibc 2.35 or newer |
| `x86_64-apple-darwin` | macOS Intel, macOS 15 or newer |
| `aarch64-apple-darwin` | macOS Apple Silicon, macOS 15 or newer |
| `x86_64-pc-windows-msvc` | Windows x64, Windows 10/11 |

Linux/macOS packages are `.tar.gz`; Windows packages are `.zip`. Native torrent
and TLS libraries are bundled, so development headers are needed only when
building from source. Windows also requires the Microsoft Visual C++ 2015–2022
x64 runtime. macOS builds are ad-hoc signed, without Apple notarization.
Each release includes `SHA256SUMS` for the archives.

```bash
./torrent-stream movie.torrent
./torrent-stream --magnet 'magnet:?xt=urn:btih:...'
./torrent-stream # Read a magnet link from the clipboard
```

Windows PowerShell:

```powershell
.\torrent-stream.exe --magnet 'magnet:?xt=urn:btih:...' --path 'D:\Movies'
```

### Build from source on Ubuntu

Requires stable Rust, a C++17 compiler, libtorrent **2.x** development headers,
Boost headers, OpenSSL development headers, pkg-config, and mpv. Ubuntu:

```bash
sudo apt-get update
sudo apt-get install build-essential pkg-config libtorrent-rasterbar-dev libboost-dev libssl-dev mpv
git clone https://github.com/goodza/torrent-stream.git
cd torrent-stream
cargo build --release
./target/release/torrent-stream movie.torrent
./target/release/torrent-stream 'magnet:?xt=urn:btih:...'
./target/release/torrent-stream --magnet 'magnet:?xt=urn:btih:...'
```

The release binary links dynamically to `libtorrent-rasterbar` and OpenSSL. They
must also be available on the machine running a source-built binary. No Python runtime,
external torrent client, or shell invocation is involved in normal operation.

Downloads default to the current working directory (`pwd`). Each run creates a
private `session-*` subdirectory there and prints its full path. Use
`--path <path>` (or `--download-dir <path>`) to choose another parent directory:

```bash
./target/release/torrent-stream movie.torrent --path /mnt/movies
```

```bash
./target/release/torrent-stream movie.torrent \
  --file 2 --buffer-seconds 300 --buffer-mb 500 \
  --download-dir ~/Downloads/torrent-stream --mpv

./target/release/torrent-stream movie.torrent --list-files
./target/release/torrent-stream movie.torrent --no-mpv
./target/release/torrent-stream movie.torrent --verbose --download-limit-kbps 512
```

On an interactive terminal, downloads appear in a live Ratatui dashboard with a
verified progress bar for the selected file, speed, peer count, estimated time
remaining, startup buffering progress, and playback/cache status. It refreshes
once per second and restores the terminal on exit, including Ctrl+C and errors.
Progress gauges and layout adapt to terminal resizing; long lines are clipped on
narrow terminals. Redirected output, `TERM=dumb`, and
`--verbose` use plain text status updates suitable for logs.

mpv launches by default. `--no-mpv` prints a loopback stream URL and keeps the
process running until Ctrl+C; an external player can open that URL. In this mode
the scheduler follows actual read demand because playback IPC is unavailable.
One reader is supported per session: a new GET supersedes the previous reader.
HEAD requests do not affect playback or priorities.

Magnet links can be passed positionally or with `--magnet` (`-m`). Quote the
entire URI so the shell preserves query parameters such as `&tr=`. For example:

```bash
./target/release/torrent-stream -m 'magnet:?xt=urn:btih:YOUR_INFO_HASH&tr=YOUR_ENCODED_TRACKER' \
  --buffer-seconds 300 --metadata-timeout 180
./target/release/torrent-stream --magnet 'magnet:?xt=urn:btih:YOUR_INFO_HASH' --list-files
```

When neither a torrent file nor a magnet link is supplied, the tool tries to read
a magnet link from the system clipboard. Copy the link, then run `torrent-stream`
(other options such as `--list-files` and `--path` still work). Explicit inputs
always take precedence. Empty, non-magnet, or unavailable clipboard contents
produce an error with instructions for supplying a source directly. Clipboard
access is native on Windows and macOS; Linux supports X11/XWayland and Wayland
compositors with a data-control protocol (other Wayland desktops need XWayland).

The tool fetches metadata from peers before listing or selecting files. Tracker
and peer hints in the URI are passed to libtorrent. Scheme casing and surrounding
pasted whitespace are normalized; query parameters are preserved. A torrent file
and `--magnet` cannot be supplied together. Hash validation is handled by libtorrent,
and unavailable metadata fails after `--metadata-timeout` seconds.

`--file` uses the zero-based **torrent** index, including any padding entries
printed by `--list-files`. One recognized video is selected automatically;
multiple videos prompt on a terminal, or require `--file` in scripts. Recognized
extensions: MKV, MP4, WebM, AVI, MOV, M4V, TS, case-insensitively. An explicit
index may select another nonempty, non-padding format that mpv supports.

| Option | Default | Meaning |
| --- | --- | --- |
| `--buffer-seconds` | 300 | Desired forward buffer at estimated local bitrate |
| `--buffer-mb` | 500 | Forward buffer cap in MiB; not a disk quota |
| `--startup-head-mb` | 32 | Verified initial file bytes required before launch |
| `--startup-tail-mb` | 16 | Verified tail bytes for MP4/MOV/M4V/MKV/AVI; 0 disables |
| `--metadata-timeout` | 120 | Seconds to resolve metadata |
| `--stall-timeout` | 180 | Startup seconds without new verified buffering progress |
| `--download-limit-kbps` | 0 | KiB/s limit, including LAN peers; 0 is unlimited |
| `--download-dir`, `--path` | `.` (current working directory) | Parent directory for private session data |
| `--verbose` | off | Byte offsets, priority windows, speed, seeks, completion rate |

The buffer size is rounded outward to whole torrent pieces. An unusually large
piece can therefore exceed the byte cap. Startup head/tail buffers are independent
of the forward cap. Small files may finish entirely during startup buffering.

## Backend selection

The inspected [librqbit API](https://docs.rs/librqbit/latest/librqbit/struct.ManagedTorrent.html)
and [streaming implementation](https://github.com/ikatson/rqbit/blob/master/crates/librqbit/src/torrent_state/streaming.rs)
provide a blocking, seekable reader with an internal fixed 32 MiB lookahead,
but no public arbitrary piece-priority/deadline setter. It is a capable native
engine; its exposed streaming API does not implement this tool's configurable
playback-aware priority tiers. [lava_torrent](https://docs.rs/lava_torrent/latest/lava_torrent/)
parses and creates metadata and is not a torrent network engine. The published
[libtorrent-sys bindings](https://docs.rs/crate/libtorrent-sys/latest) were also
evaluated; this project uses a narrow in-tree C ABI instead of depending on that
older wrapper's coverage of the required API.

[libtorrent 2.x](https://www.libtorrent.org/reference-Torrent_Handle.html) exposes
individual `piece_priority`, `set_piece_deadline`, `reset_piece_deadline`, and
verified completion. The implementation calls these real engine APIs. It does
not enable libtorrent's sequential-download flag. Tokio, scheduling, mapping,
IPC, HTTP, buffering, UI, and error handling are Rust; `native/backend.cpp`
contains the exception-contained C++ adapter. `TorrentBackend` keeps these
details out of the planner. Native calls run on blocking workers behind a
per-engine mutex; no Rust pointer outlives its native session.

## Data flow and correctness

```text
torrent / magnet → libtorrent → partially downloaded files
                         ↑                ↓ verified pieces only
                  priority deltas    loopback HTTP byte ranges → mpv
                         ↑                                    ↓
                  moving scheduler ← actual read demand + JSON IPC
```

mpv reads the partial local file through a loopback HTTP range bridge. Directly
opening a sparse file is unsafe: the filesystem returns zeros for undownloaded
regions, and pausing the player cannot reliably stop its background demuxer.
The bridge waits on verified completion before **each piece**, never skipping a
hole or returning zeros for missing data. It supports closed, open-ended, and
suffix byte ranges. Seeking opens a new range, cancels the old body, and immediately
prioritizes the new demand. No preceding portion of the movie is required.

The planner prioritizes every missing piece in its continuous playback window:

| Approximate distance ahead | Priority |
| --- | --- |
| 0–20 seconds | 7 (critical) |
| 20–60 seconds | 6 |
| 60–180 seconds | 5 |
| 180 seconds to window end | 3 |
| Selected file outside the window | 1 |
| Pieces unrelated to the selected file | 0 |

The first eight missing pieces at the contiguous frontier receive ordered
deadlines and critical priority. Completing a piece moves that frontier even
while the user pauses. Container probes and blocked reads outside the playback
window get a small urgent window too. Only changed priorities/deadlines are
sent to libtorrent; a seek removes the old window's deadlines and elevated
priorities. Already-issued network requests may finish after reprioritization.
Peers can still finish pieces out of order; the read barrier ensures that these
temporary holes never reach the decoder.

[mpv's `stream-pos`](https://mpv.io/manual/stable/#properties) is the source offset
of the most recent packet passed to a decoder, rather than the end of read-ahead.
It is preferred when present, in bounds, and consistent with `stream-end`.
While seeking, the packet can still belong to the old location, so actual HTTP
read demand takes precedence until playback restarts. Unsupported properties
remain optional; read demand supplies a safe fallback. Playback time is **never**
linearly converted into a byte offset.

File-relative offsets are translated as `(file.offset + byte) / piece_length`.
Files sharing a boundary piece are handled correctly. Buffer accounting stops at
the first missing piece; downloaded islands farther ahead do not inflate it.
Tail buffering supports MP4's end-of-file `moov` and MKV/AVI indexes. Metadata
outside these initial buffers is serviced on demand by mpv's demuxer.

The file average seeds the byte/sec estimate once duration is known. Smoothed
packet-byte progression then updates it, excluding seeks and paused samples.
Download and piece-completion rates are smoothed too. Sustained spare bandwidth
can expand the forward target up to 1.5× the requested duration within the byte
cap. Buffer seconds are estimates for VBR media. A slow-network warning is
rate-limited. mpv's own cache pause/resume handles stalls and seeks while preserving
the user's `pause` property. Properties are observed over JSON IPC; planning and
torrent status run at 500 ms, with immediate wakeups for changed range demand.

## Security, errors, and lifecycle

All subprocess arguments are explicit; no `sh -c`. Torrent paths are validated
as relative paths without traversal, empty components, or symlinks. libtorrent
also normalizes incoming metadata paths. Payload downloads stay disabled until
metadata has been checked. Each invocation uses a fresh private (mode-0700 on Unix)
session directory inside the requested parent, avoiding existing symlinks that
could redirect writes. Downloaded data is retained after playback, errors, and
Ctrl+C; listing-only directories are removed. The random stream route is served
on loopback only and exposes just the selected file.

Unix IPC sockets reside in a private temporary directory and are removed on exit.
Windows uses mpv's JSON IPC over a unique named pipe, closed when mpv exits.
mpv is terminated when the CLI exits. Invalid metadata, metadata timeouts,
out-of-range selections, missing mpv, disk errors (including disk full), startup
stalls, player errors, and IPC disconnection produce actionable errors. Missing
files cannot be read until libtorrent has verified and written their pieces.

Current scope: Linux, macOS and Windows, one torrent/reader per invocation, a fresh download session
per run. Automatic cross-run resume/cache reuse is not implemented. The selected
file continues to download at low priority outside the forward window, so disk
usage can reach the entire selected file plus shared boundary data. Unknown or
corrupt container indexes can still prevent seeking; the bridge guarantees
verified bytes, not repair of malformed source media. Torrents with no peers or
unavailable pieces cannot be made streamable by scheduling alone.

## Tests

```bash
cargo test --locked
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

Executed checks and their scope are recorded in [VALIDATION.md](VALIDATION.md).

The regular suite covers tiers and delta updates, normal playback, small/large
forward and backward seeks, beginning/end boundaries, different piece sizes,
shared pieces, Unicode/path validation, VBR fallback and smoothing, bitrate
limits, IPC response correlation, and range reads that wait at holes and cancel
on seeks.

Real local-swarm tests require `ffmpeg` in addition to mpv and approximately
3 GiB free temporary disk space. They generate synthetic video files of at least
1 GiB and real piece hashes, run local libtorrent seeders and downloaders with
an enforced 512 KiB/s cap, and drive real headless mpv through JSON IPC:

```bash
sudo apt-get install ffmpeg
cargo test --locked --features integration-tests --test swarm -- \
  --ignored --test-threads=1 --nocapture
```

These opt-in tests cover large MKV, large MP4 with a trailing `moov`, multi-file
Unicode video with a non-aligned offset, magnet metadata exchange, slow downloads,
forward/backward seeking, manual pause/resume, real priority movement, starting
before full download, and IPC cleanup. They inspect libtorrent's actual piece
priorities, verify that less than a quarter of the torrent has downloaded after
seeks, and check mpv's diagnostic log for sparse-file corruption errors. No public
swarm is necessary. `integration-tests` adds fixture/inspection APIs only; normal
builds exclude them.

## Automated binary releases

[Build and release](https://github.com/goodza/torrent-stream/actions/workflows/release.yml)
builds and tests all four targets on pushes to `main`, pull requests, and manual
workflow runs. Archives are available as workflow artifacts. Windows tests include
real named-pipe IPC; all platforms test native metadata parsing and magnet exchange.
The large mpv swarm tests remain opt-in on Unix.

To publish a release, update the version in `Cargo.toml` and `Cargo.lock`, commit
and push, then push the corresponding version tag:

```bash
git tag -a v0.1.1 -m 'Release v0.1.1'
git push origin v0.1.1
```

Tag pushes build the archives, relocate and smoke-test each packaged binary,
then automatically publish a GitHub Release with all archives, checksums, and
generated release notes. The tag must match the Cargo version. A failure on any
platform prevents publishing; rerunning a failed tag workflow is supported.
Tags containing a prerelease suffix publish as GitHub prereleases. Only the
publishing job has `contents: write`; no additional release secret is needed.

### Build from source on macOS and Windows

macOS requires libtorrent 2.x, Boost, OpenSSL and pkg-config. For a Homebrew
installation, `brew install libtorrent-rasterbar pkg-config mpv` supplies these;
then use `cargo build --release`. CI builds libtorrent 2.0.12 from source on Linux
and macOS, using system Boost/OpenSSL on Linux and Homebrew dependencies on macOS.

Windows requires Rust's MSVC toolchain, Visual Studio C++ Build Tools, and vcpkg.
Install `libtorrent[core,deprfun]:x64-windows` with vcpkg, set `VCPKG_ROOT` to its checkout,
`VCPKGRS_TRIPLET=x64-windows` and `VCPKGRS_DYNAMIC=1`, and add
`$VCPKG_ROOT/installed/x64-windows/bin` to `PATH` before building/running. Use
libtorrent's `deprfun` feature (ABI 2), matching the adapter's file-storage API.
The release workflow pins the vcpkg revision and bundles its DLLs and notices.
CI uses `ci/triplets/x64-windows-release.cmake` to build only release native
libraries; Rust unit tests also link against this release runtime.

For a non-system libtorrent installation, `LIBTORRENT_PREFIX` can point to a
prefix containing `include` and `lib` (or Ubuntu's `lib/x86_64-linux-gnu`); set `LD_LIBRARY_PATH` to
that library directory for execution. Standard Ubuntu installs use pkg-config
and require neither variable.

For a project-local installation, put the extracted headers and shared libraries
under `.local/libtorrent/usr` and create `.cargo/config.toml`:

```toml
[env]
LIBTORRENT_PREFIX = { value = ".local/libtorrent/usr", relative = true }

[build]
rustflags = ["-C", "link-arg=-Wl,-rpath,$ORIGIN/../../.local/libtorrent/usr/lib/x86_64-linux-gnu"]
```

Then plain `cargo build --release` and `./target/release/torrent-stream` work
without additional environment variables. This runtime path assumes Cargo's
default `target/debug` or `target/release` directory. The local dependency tree
and machine-specific Cargo configuration are ignored by Git. For an installed
or relocated binary, install the runtime libraries system-wide or configure
`LD_LIBRARY_PATH` for its installation prefix.
