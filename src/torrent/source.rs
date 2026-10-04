use anyhow::{bail, Result};
use std::{path::PathBuf, str::FromStr};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TorrentSource {
    File(PathBuf),
    Magnet(String),
}

impl FromStr for TorrentSource {
    type Err = anyhow::Error;

    fn from_str(input: &str) -> Result<Self> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            bail!("torrent source must not be empty");
        }
        if let Some((scheme, rest)) = trimmed.split_once(':') {
            if scheme.eq_ignore_ascii_case("magnet") {
                let query = rest
                    .strip_prefix('?')
                    .filter(|query| !query.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "invalid magnet link: expected magnet:? followed by query parameters"
                        )
                    })?;
                // Normalize only the URI scheme. Hashes, tracker URLs, display
                // names and their percent escapes are left for libtorrent.
                return Ok(Self::Magnet(format!("magnet:?{query}")));
            }
        }
        // Spaces are legal in filesystem paths; never trim a file input.
        Ok(Self::File(PathBuf::from(input)))
    }
}

pub fn parse_magnet(input: &str) -> Result<String, String> {
    match input
        .parse::<TorrentSource>()
        .map_err(|error| error.to_string())?
    {
        TorrentSource::Magnet(uri) => Ok(uri),
        TorrentSource::File(_) => Err("expected a magnet link beginning with magnet:?".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_scheme_and_pasted_whitespace_preserving_query() {
        let query = "xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=Movie%20日本語&tr=https%3A%2F%2FTracker.EXAMPLE%2Fannounce&tr=udp%3A%2F%2Ftracker.example%3A80";
        assert_eq!(
            format!(" \nMAGNET:?{query}\r\n")
                .parse::<TorrentSource>()
                .unwrap(),
            TorrentSource::Magnet(format!("magnet:?{query}"))
        );
    }

    #[test]
    fn keeps_file_paths_exactly() {
        for path in [
            "movie.torrent",
            " films/日本語.torrent ",
            "./magnet:movie.torrent",
        ] {
            assert_eq!(
                path.parse::<TorrentSource>().unwrap(),
                TorrentSource::File(PathBuf::from(path))
            );
        }
    }

    #[test]
    fn rejects_malformed_magnets_and_explicit_file_inputs() {
        for input in [
            "",
            " ",
            "magnet:",
            "magnet:?",
            "magnet://example",
            "MAGNET:xt=urn:btih:x",
        ] {
            assert!(input.parse::<TorrentSource>().is_err(), "{input:?}");
        }
        assert!(parse_magnet("movie.torrent").is_err());
        assert_eq!(
            parse_magnet("magnet:?xt=urn:btmh:1220abc").unwrap(),
            "magnet:?xt=urn:btmh:1220abc"
        );
    }
}
