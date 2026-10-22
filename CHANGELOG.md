# Changelog

## v0.2.0

- Reuse verified downloads across runs, including data retained by older versions.
- Save magnet metadata for offline reopening; preserve v1, v2, and hybrid torrent metadata.
- Automatically save and resume playback position, with `--no-resume` to start over.
- Add naturally ordered episode playlists and background prefetch with `--playlist`.
- Download and load matching torrent subtitles, with preferred audio/subtitle languages.
- Add persistent settings through `config show`, `config set`, and `config reset`.
- Add explicit cache listing, removal, and clearing, with active-download protection.
- Add `--download-only` to finish selected files and exit without mpv.
- Fix a timing-dependent cache-lock release and Windows-only lint errors found in release CI.

Downloads remain in the configured download directory. Cache cleanup is explicit;
no automatic eviction is enabled. mpv is required for integrated playback, while
download-only and external-player modes work without it.
