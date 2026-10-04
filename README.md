# torrent-stream

A Linux Rust CLI that starts playing a torrent video before it finishes downloading.
It tracks mpv over JSON IPC and moves a tiered piece-priority window with playback
and seeks. A verified read barrier prevents sparse-file holes from reaching mpv.

## Install and run

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
must also be installed on the machine running the binary. No Python runtime,
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

## Tab completion

Generate a completion script from the current CLI flags with
`--generate-completion <shell>`. This prints only the script and exits; no torrent
source is needed. Supported shells: Zsh, Bash, Fish, PowerShell, and Elvish.

For the current **Zsh** session, run from the project directory:

```zsh
autoload -Uz compinit
compinit
source <(./target/release/torrent-stream --generate-completion zsh)
```

To enable it in future sessions, save the script once:

```zsh
mkdir -p ~/.zfunc
./target/release/torrent-stream --generate-completion zsh > ~/.zfunc/_torrent-stream
```

Add these lines to `~/.zshrc`, placing the `fpath` line before any existing
`compinit` call (or before loading a framework such as Oh My Zsh):

```zsh
fpath=(~/.zfunc $fpath)
autoload -Uz compinit
compinit
```

For **Bash**, enable it in the current session with:

```bash
source <(./target/release/torrent-stream --generate-completion bash)
```

For persistence, save the script and source it from `~/.bashrc`:

```bash
mkdir -p ~/.local/share/bash-completion/completions
./target/release/torrent-stream --generate-completion bash > ~/.local/share/bash-completion/completions/torrent-stream
```

```bash
# Add to ~/.bashrc:
source ~/.local/share/bash-completion/completions/torrent-stream
```

For **Fish**, save the script in its automatically loaded completion directory:

```fish
mkdir -p ~/.config/fish/completions
./target/release/torrent-stream --generate-completion fish > ~/.config/fish/completions/torrent-stream.fish
```

Then type `./target/release/torrent-stream --bu` and press **Tab** to suggest
`--buffer-seconds` and `--buffer-mb`. Zsh and Fish also show flag descriptions;
download directory values complete as directories. If the binary is on your
`PATH`, the same completions work with `torrent-stream`. Regenerate saved scripts
after upgrading the binary to include any new flags.

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
metadata has been checked. Each invocation uses a fresh private mode-0700
session directory inside the requested parent, avoiding existing symlinks that
could redirect writes. Downloaded data is retained after playback, errors, and
Ctrl+C; listing-only directories are removed. The random stream route is served
on loopback only and exposes just the selected file.

The IPC socket resides in a private temporary directory and is removed on exit.
mpv is terminated when the CLI exits. Invalid metadata, metadata timeouts,
out-of-range selections, missing mpv, disk errors (including disk full), startup
stalls, player errors, and IPC disconnection produce actionable errors. Missing
files cannot be read until libtorrent has verified and written their pieces.

Current scope: Linux, one torrent/reader per invocation, a fresh download session
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

For a non-system libtorrent installation, `LIBTORRENT_PREFIX` can point to a
prefix containing `include` and `lib/x86_64-linux-gnu`; set `LD_LIBRARY_PATH` to
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
