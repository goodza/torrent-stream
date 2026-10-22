use crate::torrent::{Metadata, TorrentFile};
use std::{cmp::Ordering, path::Path};

pub fn playlist(metadata: &Metadata, selected: &TorrentFile, enabled: bool) -> Vec<TorrentFile> {
    if !enabled {
        return vec![selected.clone()];
    }
    let mut files: Vec<_> = metadata
        .files
        .iter()
        .filter(|f| f.is_video())
        .cloned()
        .collect();
    files.sort_by(|a, b| natural_cmp(&a.path, &b.path).then(a.index.cmp(&b.index)));
    let start = files
        .iter()
        .position(|f| f.index == selected.index)
        .unwrap_or(0);
    files.into_iter().skip(start).collect()
}
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let a = a.to_lowercase();
    let b = b.to_lowercase();
    let mut left = a.as_bytes();
    let mut right = b.as_bytes();
    while !left.is_empty() && !right.is_empty() {
        if left[0].is_ascii_digit() && right[0].is_ascii_digit() {
            let n = left.iter().take_while(|c| c.is_ascii_digit()).count();
            let m = right.iter().take_while(|c| c.is_ascii_digit()).count();
            let x = left[..n]
                .iter()
                .position(|c| *c != b'0')
                .unwrap_or(n.saturating_sub(1));
            let y = right[..m]
                .iter()
                .position(|c| *c != b'0')
                .unwrap_or(m.saturating_sub(1));
            let order = (n - x).cmp(&(m - y)).then(left[x..n].cmp(&right[y..m]));
            if order != Ordering::Equal {
                return order;
            }
            left = &left[n..];
            right = &right[m..];
        } else {
            let order = left[0].cmp(&right[0]);
            if order != Ordering::Equal {
                return order;
            }
            left = &left[1..];
            right = &right[1..];
        }
    }
    left.len().cmp(&right.len()).then(a.cmp(&b))
}
fn language(code: &str) -> &str {
    match code {
        "eng" => "en",
        "rus" => "ru",
        "jpn" => "ja",
        "deu" | "ger" => "de",
        "fra" | "fre" => "fr",
        "spa" => "es",
        "ita" => "it",
        "zho" | "chi" => "zh",
        _ => code,
    }
}
pub fn subtitles(
    metadata: &Metadata,
    video: &TorrentFile,
    preferences: &[String],
) -> Vec<TorrentFile> {
    let path = Path::new(&video.path);
    let parent = path.parent().unwrap_or(Path::new(""));
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    let single = metadata.files.iter().filter(|f| f.is_video()).count() == 1;
    let mut candidates: Vec<_> = metadata
        .files
        .iter()
        .filter(|f| !f.pad && f.size > 0 && matches!(f.extension().as_str(), "srt" | "ass"))
        .filter(|f| {
            let sub = Path::new(&f.path);
            let dir = sub.parent().unwrap_or(Path::new(""));
            let in_subs = dir.strip_prefix(parent).ok().is_some_and(|p| {
                p.components()
                    .next()
                    .is_some_and(|c| c.as_os_str().to_string_lossy().eq_ignore_ascii_case("Subs"))
            });
            if dir != parent && !in_subs {
                return false;
            }
            let name = sub
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase();
            name == stem
                || name
                    .strip_prefix(&stem)
                    .is_some_and(|suffix| suffix.starts_with(['.', '-', '_', ' ']))
                || single
        })
        .cloned()
        .collect();
    candidates.sort_by_key(|f| {
        let lower = f.path.to_lowercase();
        let tokens: Vec<_> = lower.split(|c: char| !c.is_alphanumeric()).collect();
        let rank = preferences
            .iter()
            .position(|p| {
                tokens
                    .iter()
                    .any(|t| language(t) == language(&p.to_lowercase()))
            })
            .unwrap_or(preferences.len());
        (rank, lower.contains("forced"), f.path.clone())
    });
    candidates
}

/// Compose independent intents before sending deltas to the native engine.
pub struct Priorities {
    current: std::ops::RangeInclusive<u32>,
    stream: std::collections::BTreeMap<u32, crate::torrent::PriorityUpdate>,
    previous: std::collections::BTreeMap<u32, crate::torrent::PriorityUpdate>,
}
impl Priorities {
    pub fn new(metadata: &Metadata, file: &TorrentFile) -> Self {
        Self {
            current: file.pieces(metadata.piece_length),
            stream: Default::default(),
            previous: Default::default(),
        }
    }
    fn baseline(&self, piece: u32) -> crate::torrent::PriorityUpdate {
        crate::torrent::PriorityUpdate {
            piece,
            priority: u32::from(self.current.contains(&piece)),
            deadline_ms: -1,
        }
    }
    pub fn compose(
        &mut self,
        updates: Vec<crate::torrent::PriorityUpdate>,
        auxiliary: impl IntoIterator<Item = (u32, u32)>,
    ) -> Vec<crate::torrent::PriorityUpdate> {
        for update in updates {
            if update == self.baseline(update.piece) {
                self.stream.remove(&update.piece);
            } else {
                self.stream.insert(update.piece, update);
            }
        }
        let mut desired = self.stream.clone();
        for (piece, priority) in auxiliary {
            let base = self.baseline(piece);
            let update = desired.entry(piece).or_insert(base);
            update.priority = update.priority.max(priority);
        }
        let mut changes: Vec<_> = self
            .previous
            .keys()
            .filter(|p| !desired.contains_key(p))
            .map(|p| self.baseline(*p))
            .collect();
        changes.extend(desired.iter().filter_map(|(piece, update)| {
            (self
                .previous
                .get(piece)
                .copied()
                .unwrap_or_else(|| self.baseline(*piece))
                != *update)
                .then_some(*update)
        }));
        self.previous = desired;
        changes
    }
}
pub fn should_prefetch(
    state: &crate::mpv::PlaybackState,
    buffered: u64,
    target: u64,
    remaining: u64,
) -> bool {
    !state.seeking
        && !state.paused_for_cache
        && buffered >= target.min(remaining)
        && state
            .duration
            .zip(state.time_pos)
            .is_some_and(|(duration, time)| {
                duration.is_finite()
                    && time.is_finite()
                    && (0.0..=60.0).contains(&(duration - time))
            })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn file(index: usize, path: &str) -> TorrentFile {
        TorrentFile {
            index,
            path: path.into(),
            size: 100,
            offset: index as u64 * 100,
            symlink: false,
            pad: false,
        }
    }
    fn metadata() -> Metadata {
        Metadata {
            files: vec![
                file(0, "ep10.mkv"),
                file(1, "ep2.mkv"),
                file(2, "ep1.mkv"),
                file(3, "ep2.en.srt"),
                file(4, "ep2.ru.ass"),
                file(5, "ep10.en.srt"),
            ],
            piece_length: 100,
            piece_count: 6,
        }
    }
    #[test]
    fn natural_playlist_starts_at_selected_and_subtitles_are_ranked() {
        let m = metadata();
        let selected = &m.files[1];
        assert_eq!(
            playlist(&m, selected, true)
                .iter()
                .map(|f| f.index)
                .collect::<Vec<_>>(),
            [1, 0]
        );
        assert_eq!(
            subtitles(&m, selected, &["rus".into()])
                .iter()
                .map(|f| f.index)
                .collect::<Vec<_>>(),
            [4, 3]
        );
    }
    #[test]
    fn prefetch_yields_to_playback_and_seeks() {
        let mut state = crate::mpv::PlaybackState {
            duration: Some(120.0),
            time_pos: Some(90.0),
            ..Default::default()
        };
        assert!(should_prefetch(&state, 100, 100, 200));
        assert!(!should_prefetch(&state, 99, 100, 200));
        state.seeking = true;
        assert!(!should_prefetch(&state, 100, 100, 200));
        state.seeking = false;
        state.paused_for_cache = true;
        assert!(!should_prefetch(&state, 100, 100, 200));
    }
    #[test]
    fn auxiliary_intents_never_override_current_read_and_are_removed() {
        let m = metadata();
        let mut p = Priorities::new(&m, &m.files[0]);
        let critical = crate::torrent::PriorityUpdate {
            piece: 0,
            priority: 7,
            deadline_ms: 0,
        };
        let changes = p.compose(vec![critical], [(0, 1), (1, 1)]);
        assert!(changes.contains(&critical));
        let changes = p.compose(vec![], []);
        assert_eq!(
            changes,
            [crate::torrent::PriorityUpdate {
                piece: 1,
                priority: 0,
                deadline_ms: -1
            }]
        );
    }
}
