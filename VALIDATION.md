# Validation record

Validated on Linux/Ubuntu on 2026-10-04 with Rust **1.99.0 stable**, Cargo 1.99.0,
libtorrent-rasterbar 2.0.12, mpv 0.41.0, FFmpeg 8.0.1, and OpenSSL 3.5.5.
Native development packages were extracted into a temporary prefix for this
workspace; normal installations use the Ubuntu packages documented in README.

## Checks executed

- Stable release build succeeded.
- Stable full suite, including the opt-in swarm tests: **23 passed**, 130.60 seconds.
- Clippy with all targets and features, denying warnings: passed (machine's
  existing Rust 1.94 nightly toolchain).
- `cargo fmt --all -- --check`: passed.

The full test invocation was equivalent to:

```bash
cargo test --locked --all-features -- --include-ignored --test-threads=1
```

An isolated stable toolchain and local libtorrent prefix were used through
Cargo configuration. Neither is embedded into the project or release binary.

## Real playback scenarios

Both scenarios generated synthetic video files of at least **1 GiB**, generated
real torrent piece hashes, and connected independent libtorrent seeder/downloader
sessions over loopback. Download bandwidth was capped at **512 KiB/s**, including
local peers. Playback ran through the verified HTTP bridge in real headless mpv.

| Scenario | Result |
| --- | --- |
| Large MKV, local .torrent | Passed |
| Large MP4, magnet metadata exchange | Passed |
| Multi-file torrent, Unicode video path and non-aligned file offset | Passed |
| Initial head and tail buffering; playback before full download | Passed |
| Slow transfer with blocking verified reads | Passed |
| Seek to 75% of duration, then backward to 25% | Passed |
| Real engine priorities move to the new window within 10 seconds | Passed |
| Old window loses streaming priority and deadlines | Passed |
| Playback resumes after both seeks with less than 25% downloaded | Passed |
| Manual pause stays paused, then resumes | Passed |
| mpv log has no “Corrupt file detected” or “Invalid data found” | Passed |
| IPC socket removed after player shutdown | Passed |

CLI subprocess tests additionally verified file listing, explicit selection errors,
magnet streaming through the actual application, byte-range response contents,
Ctrl+C termination, closed HTTP listener, retained downloaded data, invalid
torrent errors, and metadata timeout errors.

Unit tests cover priority tiers/deltas, small/large/backward seeks, EOF and shared
piece geometry, first-hole buffer accounting, unsupported mpv byte properties,
VBR smoothing, bandwidth adaptation, IPC request correlation, and a read that
waits at an unavailable piece and is cancelled by a new range request.

## Scope of evidence

The magnet CLI update passed 27 regular tests (the two large-file swarm tests
remain opt-in), including real peer metadata exchange with both `--magnet` and
`-m`, positional streaming with a mixed-case scheme and pasted whitespace, and
invalid-hash errors. Its release build, Clippy, and formatting checks passed.

Public trackers/DHT swarms, desktop rendering/audio devices, disk-full injection,
and arbitrary third-party malformed containers were not exercised. Network and
storage errors are surfaced by the native adapter; real packet reads and seeks
were verified using the local swarm. Cross-run download resume is outside this
version's scope.
