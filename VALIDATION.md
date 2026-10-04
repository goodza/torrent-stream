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

## Download dashboard

The live download dashboard passed 31 tests with `cargo test --locked
--all-features`; the two large-file swarm tests remained ignored. Rendering
checks cover selected-file progress across shared boundary pieces, terminal
control characters in paths, zero download speed, completion, startup buffering,
paused playback, and the external-player URL. CLI tests verify plain piped output
and Ctrl+C during startup buffering. A Linux pseudo-terminal test verifies the
live frame and screen/cursor/wrapping restoration before the shutdown message.
A manual pseudo-terminal startup-stall check also confirmed restoration before
the error message.

The release build, Clippy with all targets/features and warnings denied, formatting,
and whitespace checks passed. No new runtime dependencies were added.

## Clipboard magnet fallback

`cargo test --locked --all-features` passed 35 tests; the two large-file swarm
tests remained ignored. New tests cover missing sources with other CLI options,
clipboard magnet normalization and query preservation, explicit sources bypassing
clipboard access, invalid/empty clipboard text, and clipboard read failures.
The binary was also checked without a desktop clipboard connection and reported
the actionable error before creating a download session.

Clippy with all targets/features and warnings denied, formatting, and whitespace
checks passed using Rust 1.94 nightly and the local libtorrent prefix above.
Native clipboard access uses arboard with text-only and Wayland data-control
support. Actual desktop clipboard reads on Linux, macOS, and Windows were not
exercised locally.

## Ratatui dashboard

The dashboard now renders through Ratatui 0.30.2 and its Crossterm backend.
`cargo test --locked --all-features` passed 36 tests; the two large-file swarm
tests remained ignored. Ratatui TestBackend checks cover verified progress,
startup and playback states, external-player URLs, escaped paths, clipping of
long Unicode paths, expired notices, and resizing down to an empty terminal.
Repeated draws clear completed startup sections and expired notices.

The Linux pseudo-terminal test verified real widget output with redirected stdin
and restoration of wrapping, cursor visibility, and the original screen before
the Ctrl+C message. Initialization clears the fullscreen backend without querying
the cursor or enabling raw mode. Existing plain-output and CLI tests also passed.
Clippy with all targets/features and warnings denied, formatting, and whitespace
checks passed using Rust 1.94 nightly and the local libtorrent prefix above.
macOS and Windows terminal rendering were not exercised locally.

## Cross-platform binary releases

[The v0.1.1 release workflow](https://github.com/goodza/torrent-stream/actions/runs/37219962545)
passed on 2026-10-04 and automatically published
[four binary archives and SHA256SUMS](https://github.com/goodza/torrent-stream/releases/tag/v0.1.1).
Every target passed locked stable-Rust tests, Clippy with all targets/features
and warnings denied, formatting, and a release build:

| Target | Native backend | Result |
| --- | --- | --- |
| Ubuntu 22.04 x64 | libtorrent 2.0.12 built from pinned source | Passed |
| macOS 15 Intel | libtorrent 2.0.12 built from pinned source | Passed |
| macOS 15 Apple Silicon | libtorrent 2.0.12 built from pinned source | Passed |
| Windows Server 2022 x64 | libtorrent 2.1.2 with ABI 2, pinned vcpkg | Passed |

Each archive includes native torrent/TLS libraries and license notices. The
workflow extracts the archive into a different directory, removes native
development library overrides, runs the packaged binary, and parses a torrent
whose input path and payload filename contain Unicode. Windows additionally
limits PATH to system directories for this smoke test, preventing development
DLL paths from hiding missing packaged dependencies.

Windows tests exercised actual named-pipe IPC, unsafe Win32 path rejection,
verified range reads that block at holes, and real local magnet metadata exchange
with both explicit magnet flags. Unix CLI tests also checked application streaming,
Ctrl+C, and retained download data. The large mpv swarm
tests remained ignored in CI; their earlier Linux results are recorded above.

The initial v0.1.0 tag failed Windows validation and published no release.
v0.1.1 replaced the Unix-only entropy source with the platform OS randomness API.
Older Ubuntu system libtorrent caused local magnet timeouts during initial CI;
release builds now use the tested 2.0.12 source. Windows native dependencies are
cached immediately after compilation, and only release native libraries are
built. Version tags must match Cargo.toml, and publication requires every target
to succeed.

CI did not exercise desktop mpv rendering/audio or real desktop clipboard reads.
