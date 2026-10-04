use crate::torrent::{source::parse_magnet, TorrentSource};
use clap::{ArgGroup, Parser, ValueHint};
use clap_complete::Shell;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Stream a torrent video to mpv with adaptive piece priorities",
    group(ArgGroup::new("input").args(["source", "magnet"]).multiple(false))
)]
pub struct Cli {
    /// Print a shell completion script and exit (no torrent source needed)
    #[arg(long, value_enum, value_name = "SHELL", exclusive = true)]
    pub generate_completion: Option<Shell>,
    /// Local .torrent path or magnet URI
    #[arg(value_hint = ValueHint::FilePath, required_unless_present_any = ["magnet", "generate_completion"])]
    pub source: Option<TorrentSource>,
    /// Magnet link (alternative to passing it as the positional source)
    #[arg(short = 'm', long, value_name = "URI", value_parser = parse_magnet, value_hint = ValueHint::Other)]
    pub magnet: Option<String>,
    /// Zero-based torrent file index (as printed by --list-files)
    #[arg(long)]
    pub file: Option<usize>,
    /// Desired forward buffer in seconds
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u32).range(1..=86400))]
    pub buffer_seconds: u32,
    /// Maximum forward buffer in MiB (does not limit total disk usage)
    #[arg(long, default_value_t = 500, value_parser = clap::value_parser!(u32).range(1..=65536))]
    pub buffer_mb: u32,
    /// Parent directory for downloads (defaults to the current working directory)
    #[arg(long, visible_alias = "path", default_value = ".", value_hint = ValueHint::DirPath)]
    pub download_dir: PathBuf,
    /// Launch mpv (the default)
    #[arg(long, conflicts_with = "no_mpv")]
    pub mpv: bool,
    /// Print a safe stream URL and keep downloading until Ctrl+C
    #[arg(long)]
    pub no_mpv: bool,
    /// List torrent files and exit
    #[arg(long)]
    pub list_files: bool,
    /// Show detailed download and playback diagnostics
    #[arg(long)]
    pub verbose: bool,
    /// Verified initial file bytes required before playback, in MiB
    #[arg(long, default_value_t = 32, value_parser = clap::value_parser!(u32).range(1..=65536))]
    pub startup_head_mb: u32,
    /// Tail metadata buffer, enabled for MP4/MOV/M4V/MKV/AVI
    #[arg(long, default_value_t = 16, value_parser = clap::value_parser!(u32).range(0..=65536))]
    pub startup_tail_mb: u32,
    /// Seconds to wait for torrent metadata
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
        match (&self.source, &self.magnet) {
            (Some(source), None) => Ok(source.clone()),
            (None, Some(magnet)) => Ok(TorrentSource::Magnet(magnet.clone())),
            _ => anyhow::bail!("provide exactly one torrent file or magnet link"),
        }
    }
}

pub fn expand_home(path: &std::path::Path) -> anyhow::Result<PathBuf> {
    if let Ok(suffix) = path.strip_prefix("~") {
        let home = std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME is unset"))?;
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
    fn requires_one_source_and_rejects_conflicting_inputs() {
        assert!(Cli::try_parse_from(["torrent-stream"]).is_err());
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
}
