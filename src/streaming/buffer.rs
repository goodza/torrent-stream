use crate::mpv::PlaybackState;
use std::time::Instant;

/// Rates are bytes/sec. The file average bootstraps the estimate; smooth actual
/// packet byte progression supersedes it. Seeks and paused samples are excluded.
pub struct BufferController {
    pub bitrate: f64,
    pub download_rate: f64,
    pub piece_rate: f64,
    seconds: f64,
    cap: u64,
    growth: f64,
    seeded: bool,
    last_playback: Option<(f64, u64)>,
    last_download: Option<(Instant, u64, u64)>,
}
impl BufferController {
    pub fn new(seconds: u32, cap: u64) -> Self {
        Self {
            bitrate: 4.0 * 1048576.0,
            download_rate: 0.0,
            piece_rate: 0.0,
            seconds: f64::from(seconds),
            cap,
            growth: 1.0,
            seeded: false,
            last_playback: None,
            last_download: None,
        }
    }
    pub fn playback(&mut self, state: &PlaybackState, size: u64, byte: Option<u64>, seek: bool) {
        if !self.seeded {
            if let Some(duration) = state.duration.filter(|d| d.is_finite() && *d > 0.0) {
                self.bitrate = (size as f64 / duration).clamp(1024.0, 1024.0 * 1048576.0);
                self.seeded = true;
            }
        }
        if seek || state.pause || state.seeking || state.paused_for_cache {
            self.last_playback = None;
            return;
        }
        if let (Some(time), Some(byte)) = (state.time_pos, byte) {
            if let Some((previous_time, previous_byte)) = self.last_playback {
                let elapsed = time - previous_time;
                if elapsed < 1.0 {
                    return;
                }
                if (1.0..=3.0).contains(&elapsed) && byte > previous_byte {
                    let observed = (byte - previous_byte) as f64 / elapsed;
                    if observed.is_finite() && (1024.0..=1024.0 * 1048576.0).contains(&observed) {
                        self.bitrate = self.bitrate * 0.85 + observed * 0.15;
                    }
                }
            }
            self.last_playback = Some((time, byte));
        }
    }
    pub fn download(&mut self, now: Instant, bytes: u64, pieces: u64, instantaneous_rate: u64) {
        if let Some((previous, previous_bytes, previous_pieces)) = self.last_download {
            let elapsed = now.duration_since(previous).as_secs_f64().max(0.001);
            let measured = bytes.saturating_sub(previous_bytes) as f64 / elapsed;
            self.download_rate = self.download_rate * 0.8 + measured * 0.2;
            self.piece_rate = self.piece_rate * 0.8
                + pieces.saturating_sub(previous_pieces) as f64 / elapsed * 0.2;
        } else {
            self.download_rate = instantaneous_rate as f64;
        }
        self.last_download = Some((now, bytes, pieces));
        if self.ratio() > 1.5 {
            self.growth = (self.growth + 0.05).min(1.5);
        } else {
            self.growth = (self.growth - 0.02).max(1.0);
        }
    }
    pub fn ratio(&self) -> f64 {
        self.download_rate / self.bitrate.max(1.0)
    }
    pub fn target_bytes(&self) -> u64 {
        (self.bitrate * self.seconds * self.growth)
            .min(self.cap as f64)
            .max(1.0) as u64
    }
    pub fn buffered_seconds(&self, bytes: u64) -> f64 {
        bytes as f64 / self.bitrate.max(1.0)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vbr_smoothing_excludes_seek_and_pause() {
        let mut c = BufferController::new(300, 500 * 1048576);
        let mut s = PlaybackState {
            time_pos: Some(0.0),
            duration: Some(100.0),
            ..Default::default()
        };
        c.playback(&s, 100 * 1048576, Some(0), false);
        s.time_pos = Some(2.0);
        c.playback(&s, 100 * 1048576, Some(4 * 1048576), false);
        assert!((c.bitrate / 1048576.0 - 1.15).abs() < 0.001);
        let bitrate = c.bitrate;
        s.time_pos = Some(80.0);
        c.playback(&s, 100 * 1048576, Some(90 * 1048576), true);
        assert_eq!(c.bitrate, bitrate);
        s.pause = true;
        s.time_pos = Some(82.0);
        c.playback(&s, 100 * 1048576, Some(95 * 1048576), false);
        assert_eq!(c.bitrate, bitrate);
    }
    #[test]
    fn slow_network_and_abundant_network_respect_cap() {
        let mut c = BufferController::new(100, 1000);
        c.bitrate = 100.0;
        let now = Instant::now();
        c.download(now, 0, 0, 50);
        assert_eq!(c.ratio(), 0.5);
        assert_eq!(c.target_bytes(), 1000);
        c.download(now + std::time::Duration::from_secs(1), 10000, 10, 10000);
        assert!(c.ratio() > 1.5);
        assert_eq!(c.target_bytes(), 1000);
        assert!(c.piece_rate > 0.0);
    }
}
