use crate::torrent::{source::parse_magnet, TorrentSource};
use anyhow::Context;
use clap::{ArgGroup, Parser};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Stream a torrent video to mpv with adaptive piece priorities",
    group(ArgGroup::new("input").args(["source", "magnet"]).multiple(false))
)]
pub struct Cli {
    /// Local .torrent path or magnet URI (reads a magnet from the clipboard if omitted)
    pub source: Option<TorrentSource>,
    /// Magnet link (alternative to passing it as the positional source)
    #[arg(short = 'm', long, value_name = "URI", value_parser = parse_magnet)]
    pub magnet: Option<String>,
    /// Zero-based torrent file index (as printed by --list-files)
    #[arg(long)]
    pub file: Option<usize>,
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u32).range(1..=86400))]
    pub buffer_seconds: u32,
    /// Maximum forward buffer in MiB (does not limit total disk usage)
    #[arg(long, default_value_t = 500, value_parser = clap::value_parser!(u32).range(1..=65536))]
    pub buffer_mb: u32,
    /// Parent directory for downloads (defaults to the current working directory)
    #[arg(long, visible_alias = "path", default_value = ".")]
    pub download_dir: PathBuf,
    #[arg(long, conflicts_with = "no_mpv")]
    pub mpv: bool,
    /// Print a safe stream URL and keep downloading until Ctrl+C
    #[arg(long)]
    pub no_mpv: bool,
    #[arg(long)]
    pub list_files: bool,
    #[arg(long)]
    pub verbose: bool,
    #[arg(long, default_value_t = 32, value_parser = clap::value_parser!(u32).range(1..=65536))]
    pub startup_head_mb: u32,
    /// Tail metadata buffer, enabled for MP4/MOV/M4V/MKV/AVI
    #[arg(long, default_value_t = 16, value_parser = clap::value_parser!(u32).range(0..=65536))]
    pub startup_tail_mb: u32,
    #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u32).range(1..=86400))]
    pub metadata_timeout: u32,
    /// Fail if startup makes no verified progress for this many seconds
    #[arg(long, default_value_t = 180, value_parser = clap::value_parser!(u32).range(1..=86400))]
    pub stall_timeout: u32,
    /// Download limit in KiB/s; zero means unlimited
    #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u32).range(0..=2097151))]
    pub download_limit_kbps: u32,
}

impl Cli {
    pub fn input(&self) -> anyhow::Result<TorrentSource> {
        self.input_with_clipboard(|| Ok(arboard::Clipboard::new()?.get_text()?))
    }

    fn input_with_clipboard(
        &self,
        read_clipboard: impl FnOnce() -> anyhow::Result<String>,
    ) -> anyhow::Result<TorrentSource> {
        match (&self.source, &self.magnet) {
            (Some(source), None) => Ok(source.clone()),
            (None, Some(magnet)) => Ok(TorrentSource::Magnet(magnet.clone())),
            (None, None) => {
                let text = read_clipboard().context(
                    "no torrent source supplied and could not read clipboard; pass a torrent file or --magnet URI",
                )?;
                let magnet = parse_magnet(&text).map_err(anyhow::Error::msg).context(
                    "no torrent source supplied and clipboard does not contain a valid magnet link; copy a magnet link or pass a torrent file or --magnet URI",
                )?;
                Ok(TorrentSource::Magnet(magnet))
            }
            (Some(_), Some(_)) => anyhow::bail!("provide exactly one torrent file or magnet link"),
        }
    }
}

pub fn expand_home(path: &std::path::Path) -> anyhow::Result<PathBuf> {
    if let Ok(suffix) = path.strip_prefix("~") {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .ok_or_else(|| anyhow::anyhow!("HOME and USERPROFILE are unset"))?;
        Ok(PathBuf::from(home).join(suffix))
    } else {
        Ok(path.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_positional_and_explicit_magnet_inputs() {
        let uri = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567";
        for args in [
            vec!["torrent-stream", uri],
            vec!["torrent-stream", "--magnet", uri],
            vec!["torrent-stream", "-m", uri],
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            assert_eq!(cli.input().unwrap(), TorrentSource::Magnet(uri.into()));
        }
        assert_eq!(
            Cli::try_parse_from(["torrent-stream", "movie.torrent"])
                .unwrap()
                .input()
                .unwrap(),
            TorrentSource::File("movie.torrent".into())
        );
    }

    #[test]
    fn rejects_conflicting_and_invalid_inputs() {
        assert!(Cli::try_parse_from([
            "torrent-stream",
            "movie.torrent",
            "--magnet",
            "magnet:?xt=urn:btih:x"
        ])
        .is_err());
        assert!(Cli::try_parse_from(["torrent-stream", "--magnet", "movie.torrent"]).is_err());
        assert!(Cli::try_parse_from(["torrent-stream", "magnet:"]).is_err());
    }

    #[test]
    fn missing_source_uses_clipboard_with_normalization() {
        let query = "xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=Movie%20日本語&tr=udp%3A%2F%2Ftracker.example%3A80";
        for args in [
            vec!["torrent-stream"],
            vec!["torrent-stream", "--list-files", "--path", "downloads"],
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            assert_eq!(
                cli.input_with_clipboard(|| Ok(format!(" \nMAGNET:?{query}\r\n")))
                    .unwrap(),
                TorrentSource::Magnet(format!("magnet:?{query}"))
            );
        }
    }

    #[test]
    fn explicit_sources_do_not_access_clipboard() {
        let uri = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567";
        for args in [
            vec!["torrent-stream", "movie.torrent"],
            vec!["torrent-stream", uri],
            vec!["torrent-stream", "--magnet", uri],
            vec!["torrent-stream", "-m", uri],
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            assert_eq!(
                cli.input_with_clipboard(|| panic!("explicit source must bypass clipboard"))
                    .unwrap(),
                cli.input().unwrap()
            );
        }
    }

    #[test]
    fn invalid_clipboard_contents_are_actionable_and_not_echoed() {
        let cli = Cli::try_parse_from(["torrent-stream"]).unwrap();
        for text in [
            "",
            " \n",
            "movie.torrent",
            "private clipboard text",
            "magnet:",
            "magnet:?",
            "magnet:private-clipboard-query",
        ] {
            let error = cli.input_with_clipboard(|| Ok(text.into())).unwrap_err();
            let message = format!("{error:#}");
            assert!(message.contains("clipboard does not contain a valid magnet link"));
            assert!(message.contains("--magnet"));
            if text.contains("private") || text == "movie.torrent" {
                assert!(!message.contains(text), "{message}");
            }
        }
    }

    #[test]
    fn unreadable_clipboard_is_actionable() {
        let cli = Cli::try_parse_from(["torrent-stream"]).unwrap();
        let error = cli
            .input_with_clipboard(|| anyhow::bail!("clipboard unavailable"))
            .unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("could not read clipboard"));
        assert!(message.contains("--magnet"));
        assert!(message.contains("clipboard unavailable"));
    }
}
