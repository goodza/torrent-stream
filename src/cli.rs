use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Stream a torrent video to mpv with adaptive piece priorities"
)]
pub struct Cli {
    /// Local .torrent path or magnet URI
    pub source: String,
    /// Zero-based torrent file index (as printed by --list-files)
    #[arg(long)]
    pub file: Option<usize>,
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u32).range(1..=86400))]
    pub buffer_seconds: u32,
    /// Maximum forward buffer in MiB (does not limit total disk usage)
    #[arg(long, default_value_t = 500, value_parser = clap::value_parser!(u32).range(1..=65536))]
    pub buffer_mb: u32,
    #[arg(long, default_value = "~/Downloads/torrent-stream")]
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

pub fn expand_home(path: &std::path::Path) -> anyhow::Result<PathBuf> {
    if let Ok(suffix) = path.strip_prefix("~") {
        let home = std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME is unset"))?;
        Ok(PathBuf::from(home).join(suffix))
    } else {
        Ok(path.to_owned())
    }
}
