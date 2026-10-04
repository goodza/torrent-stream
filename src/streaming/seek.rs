use crate::mpv::PlaybackState;

#[derive(Default)]
pub struct SeekDetector {
    previous_time: Option<f64>,
    previous_generation: u64,
}
impl SeekDetector {
    pub fn update(&mut self, state: &PlaybackState) -> bool {
        let event = state.seek_generation != self.previous_generation;
        let jump = match (self.previous_time, state.time_pos) {
            (Some(old), Some(new)) => (new - old).abs() > 3.0,
            _ => false,
        };
        self.previous_time = state.time_pos;
        self.previous_generation = state.seek_generation;
        event || jump
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detects_both_directions_and_ipc_events() {
        let mut d = SeekDetector::default();
        let mut s = PlaybackState {
            time_pos: Some(100.0),
            ..Default::default()
        };
        assert!(!d.update(&s));
        s.time_pos = Some(100.5);
        assert!(!d.update(&s));
        s.time_pos = Some(104.0);
        assert!(d.update(&s));
        s.time_pos = Some(5400.0);
        assert!(d.update(&s));
        s.time_pos = Some(300.0);
        assert!(d.update(&s));
        s.seek_generation = 1;
        assert!(d.update(&s));
        assert!(!d.update(&s));
    }
}
