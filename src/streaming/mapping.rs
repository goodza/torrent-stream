use crate::{mpv::PlaybackState, torrent::TorrentFile};
use std::ops::RangeInclusive;

/// File-relative bytes are always translated using the torrent-global file offset.
/// End offsets are exclusive; shared boundary pieces belong to both files.
#[derive(Clone, Debug)]
pub struct PieceMapping {
    pub file: TorrentFile,
    pub piece_length: u64,
}
impl PieceMapping {
    pub fn piece_at(&self, byte: u64) -> u32 {
        ((self.file.offset + byte.min(self.file.size - 1)) / self.piece_length) as u32
    }
    pub fn range(&self, start: u64, end: u64) -> Option<RangeInclusive<u32>> {
        let start = start.min(self.file.size);
        let end = end.min(self.file.size);
        (end > start).then(|| self.piece_at(start)..=self.piece_at(end - 1))
    }
    pub fn piece_end_in_file(&self, piece: u32) -> u64 {
        ((u64::from(piece) + 1) * self.piece_length)
            .saturating_sub(self.file.offset)
            .min(self.file.size)
    }
    /// The contiguous verified prefix from a byte position, stopping at the FIRST hole.
    pub fn contiguous_bytes(&self, start: u64, have: &[bool]) -> u64 {
        if start >= self.file.size {
            return 0;
        }
        let mut end = start;
        for piece in self.piece_at(start)..=self.piece_at(self.file.size - 1) {
            if have.get(piece as usize) != Some(&true) {
                break;
            }
            end = self.piece_end_in_file(piece);
        }
        end.saturating_sub(start)
    }
    pub fn verified_bytes(&self, start: u64, end: u64, have: &[bool]) -> u64 {
        let end = end.min(self.file.size);
        let Some(pieces) = self.range(start, end) else {
            return 0;
        };
        pieces
            .filter(|p| have.get(*p as usize) == Some(&true))
            .map(|p| {
                let left = (u64::from(p) * self.piece_length)
                    .saturating_sub(self.file.offset)
                    .max(start);
                self.piece_end_in_file(p).min(end).saturating_sub(left)
            })
            .sum()
    }
    /// `stream-pos` is a packet offset, not the end of demuxer read-ahead.
    /// Reject positions from a source with different geometry. If unavailable, the
    /// caller uses actual HTTP read demand; duration is NEVER used to map position.
    pub fn playback_byte(&self, state: &PlaybackState) -> Option<u64> {
        if state.stream_end.is_some_and(|end| end != self.file.size) {
            return None;
        }
        state.stream_pos.filter(|pos| *pos < self.file.size)
    }
}

/// During a seek, the decoder's last packet may belong to the old location.
/// Range-read demand takes precedence until mpv reports playback restarted.
pub fn playback_focus(
    previous: u64,
    mapping: &PieceMapping,
    state: &PlaybackState,
    demand: super::server::ReadDemand,
    has_mpv: bool,
) -> u64 {
    let read = demand
        .active
        .then_some(demand.offset.min(mapping.file.size - 1));
    if !has_mpv || state.seeking {
        read.unwrap_or(previous)
    } else {
        mapping.playback_byte(state).or(read).unwrap_or(previous)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    pub fn mapping(offset: u64, size: u64, piece_length: u64) -> PieceMapping {
        PieceMapping {
            file: TorrentFile {
                index: 1,
                path: "video.mkv".into(),
                size,
                offset,
                symlink: false,
                pad: false,
            },
            piece_length,
        }
    }
    #[test]
    fn multi_file_partial_boundaries() {
        let m = mapping(150, 210, 100);
        assert_eq!(m.piece_at(0), 1);
        assert_eq!(m.piece_at(50), 2);
        assert_eq!(m.range(0, 210), Some(1..=3));
        assert_eq!(m.range(0, 50), Some(1..=1));
        assert_eq!(m.range(50, 150), Some(2..=2));
        assert_eq!(m.range(210, 210), None);
        assert_eq!(m.verified_bytes(0, 210, &[false, true, false, true]), 110);
        assert_eq!(m.contiguous_bytes(0, &[false, true, false, true]), 50);
    }
    #[test]
    fn exact_eof_and_piece_sizes() {
        for length in [1, 16384, 262144, 1048576] {
            let m = mapping(length - 1, length + 1, length);
            assert_eq!(m.range(0, m.file.size), Some(0..=1));
            assert_eq!(m.contiguous_bytes(0, &[true, true]), m.file.size);
        }
    }
    #[test]
    fn no_linear_vbr_position_estimate() {
        let m = mapping(0, 1000, 100);
        let mut state = PlaybackState {
            time_pos: Some(500.0),
            duration: Some(1000.0),
            ..Default::default()
        };
        assert_eq!(m.playback_byte(&state), None);
        state.stream_pos = Some(900);
        assert_eq!(m.playback_byte(&state), Some(900));
        state.stream_end = Some(999);
        assert_eq!(m.playback_byte(&state), None);
    }
    #[test]
    fn seek_read_demand_overrides_stale_decoder_packet() {
        let m = mapping(150, 1000, 100);
        let mut state = PlaybackState {
            stream_pos: Some(10),
            seeking: true,
            ..Default::default()
        };
        let read = super::super::server::ReadDemand {
            generation: 2,
            offset: 800,
            active: true,
        };
        assert_eq!(playback_focus(10, &m, &state, read, true), 800);
        state.seeking = false;
        state.stream_pos = Some(805);
        assert_eq!(playback_focus(800, &m, &state, read, true), 805);
        assert_eq!(playback_focus(800, &m, &state, read, false), 800);
        state.stream_pos = None;
        assert_eq!(playback_focus(10, &m, &state, read, true), 800);
    }
}
