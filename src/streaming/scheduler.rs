use super::mapping::PieceMapping;
use crate::torrent::PriorityUpdate;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Intent {
    priority: u32,
    deadline_ms: i32,
}

/// Independently testable planner. Piece 0 has no special role after startup.
/// Missing pieces nearest the contiguous frontier get ordered deadlines; all
/// remaining holes in the forward window remain scheduled at decreasing tiers.
pub struct StreamScheduler {
    pub mapping: PieceMapping,
    previous: BTreeMap<u32, Intent>,
    pub window: Option<std::ops::RangeInclusive<u32>>,
}
impl StreamScheduler {
    pub fn new(mapping: PieceMapping) -> Self {
        Self {
            mapping,
            previous: BTreeMap::new(),
            window: None,
        }
    }
    pub fn plan(
        &mut self,
        byte: u64,
        target_bytes: u64,
        bitrate: f64,
        have: &[bool],
        demand: Option<u64>,
    ) -> Vec<PriorityUpdate> {
        let end = byte
            .saturating_add(target_bytes)
            .min(self.mapping.file.size);
        self.window = self.mapping.range(byte, end);
        let mut desired = BTreeMap::new();
        if let Some(range) = self.window.clone() {
            for piece in range.clone() {
                if have.get(piece as usize) == Some(&true) {
                    continue;
                }
                let start = (u64::from(piece) * self.mapping.piece_length)
                    .saturating_sub(self.mapping.file.offset);
                let seconds = start.saturating_sub(byte) as f64 / bitrate.max(1.0);
                let priority = if seconds < 20.0 {
                    7
                } else if seconds < 60.0 {
                    6
                } else if seconds < 180.0 {
                    5
                } else {
                    3
                };
                desired.insert(
                    piece,
                    Intent {
                        priority,
                        deadline_ms: -1,
                    },
                );
            }
            // This frontier moves on completion even when playback is paused.
            for (ordinal, piece) in range
                .filter(|p| have.get(*p as usize) != Some(&true))
                .take(8)
                .enumerate()
            {
                desired.insert(
                    piece,
                    Intent {
                        priority: 7,
                        deadline_ms: (ordinal as i32) * 150,
                    },
                );
            }
        }
        if let Some(demand) = demand.filter(|offset| *offset < self.mapping.file.size) {
            // Container probes and blocked reads can be outside the playback window.
            // They must be serviced immediately without sequentially filling the gap.
            if let Some(range) = self
                .mapping
                .range(demand, demand.saturating_add(self.mapping.piece_length * 4))
            {
                for (ordinal, piece) in range
                    .filter(|p| have.get(*p as usize) != Some(&true))
                    .enumerate()
                {
                    let intent = desired.entry(piece).or_insert(Intent {
                        priority: 7,
                        deadline_ms: -1,
                    });
                    intent.priority = 7;
                    let deadline = ordinal as i32 * 100;
                    if intent.deadline_ms < 0 || deadline < intent.deadline_ms {
                        intent.deadline_ms = deadline;
                    }
                }
            }
        }
        self.diff(desired)
    }
    pub fn startup(&mut self, head: u64, tail: u64, have: &[bool]) -> Vec<PriorityUpdate> {
        let mut desired = BTreeMap::new();
        for (start, end) in [
            (0, head.min(self.mapping.file.size)),
            (
                self.mapping.file.size.saturating_sub(tail),
                self.mapping.file.size,
            ),
        ] {
            if let Some(range) = self.mapping.range(start, end) {
                for (ordinal, piece) in range
                    .filter(|p| have.get(*p as usize) != Some(&true))
                    .enumerate()
                {
                    desired.entry(piece).or_insert(Intent {
                        priority: 7,
                        deadline_ms: (ordinal as i32).saturating_mul(150),
                    });
                }
            }
        }
        self.diff(desired)
    }
    pub fn clear(&mut self) -> Vec<PriorityUpdate> {
        self.diff(BTreeMap::new())
    }
    fn diff(&mut self, desired: BTreeMap<u32, Intent>) -> Vec<PriorityUpdate> {
        let mut updates = Vec::new();
        for &piece in self.previous.keys() {
            if !desired.contains_key(&piece) {
                updates.push(PriorityUpdate {
                    piece,
                    priority: 1,
                    deadline_ms: -1,
                });
            }
        }
        for (&piece, intent) in &desired {
            if self.previous.get(&piece) != Some(intent) {
                updates.push(PriorityUpdate {
                    piece,
                    priority: intent.priority,
                    deadline_ms: intent.deadline_ms,
                });
            }
        }
        self.previous = desired;
        updates
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::torrent::TorrentFile;
    fn scheduler(offset: u64, size: u64, length: u64) -> StreamScheduler {
        StreamScheduler::new(PieceMapping {
            file: TorrentFile {
                index: 1,
                path: "video.mkv".into(),
                size,
                offset,
                symlink: false,
                pad: false,
            },
            piece_length: length,
        })
    }
    #[test]
    fn normal_playback_tiers_and_no_churn() {
        let mut s = scheduler(0, 1000, 1);
        let updates = s.plan(10, 300, 1.0, &[], None);
        for (piece, priority) in [(10, 7), (29, 7), (30, 6), (70, 5), (190, 3)] {
            assert_eq!(
                updates.iter().find(|u| u.piece == piece).unwrap().priority,
                priority
            );
        }
        assert!(s.plan(10, 300, 1.0, &[], None).is_empty());
        let diff = s.plan(11, 300, 1.0, &[], None);
        assert!(diff.len() < 25);
        assert!(diff.iter().any(|u| u.piece == 10 && u.priority == 1));
    }
    #[test]
    fn small_large_and_backward_seeks_demote_old_windows() {
        let mut s = scheduler(0, 10000, 10);
        s.plan(100, 300, 10.0, &[], None);
        for byte in [180, 8000, 200] {
            let old = s.previous.clone();
            let updates = s.plan(byte, 300, 10.0, &[], None);
            assert_eq!(
                s.window,
                Some((byte / 10) as u32..=((byte + 299) / 10) as u32)
            );
            for piece in old.keys().filter(|p| !s.previous.contains_key(p)) {
                assert!(updates
                    .iter()
                    .any(|u| u.piece == *piece && u.priority == 1 && u.deadline_ms == -1));
            }
            assert!(s.previous.contains_key(&((byte / 10) as u32)));
        }
    }
    #[test]
    fn beginning_end_and_multi_file_boundaries() {
        let mut s = scheduler(150, 210, 100);
        let updates = s.plan(0, 10000, 10.0, &[], None);
        assert_eq!(s.window, Some(1..=3));
        assert_eq!(updates.len(), 3);
        let updates = s.plan(209, 1000, 1.0, &[], None);
        assert_eq!(s.window, Some(3..=3));
        assert!(updates.iter().all(|u| (1..=3).contains(&u.piece)));
    }
    #[test]
    fn frontier_never_skips_holes() {
        let mut s = scheduler(0, 10000, 100);
        let mut have = vec![false; 100];
        have[10] = true;
        have[12] = true;
        s.plan(1000, 3000, 100.0, &have, None);
        assert_eq!(s.previous[&11].deadline_ms, 0);
        assert_eq!(s.previous[&13].deadline_ms, 150);
        assert_eq!(s.mapping.contiguous_bytes(1000, &have), 100);
        have[11] = true;
        s.plan(1000, 3000, 100.0, &have, None);
        assert_eq!(s.previous[&13].deadline_ms, 0);
        assert_eq!(s.mapping.contiguous_bytes(1000, &have), 300);
    }
    #[test]
    fn probes_do_not_download_preceding_file() {
        let mut s = scheduler(1000, 10000, 100);
        s.plan(0, 100, 100.0, &[], Some(9000));
        assert!(s.previous.contains_key(&100));
        assert!(!s.previous.contains_key(&50));
        assert!(s
            .previous
            .keys()
            .all(|p| *p == 10 || (100..=103).contains(p)));
    }
    #[test]
    fn startup_merges_overlapping_head_and_tail() {
        let mut s = scheduler(0, 10, 1);
        let updates = s.startup(8, 8, &[]);
        assert_eq!(updates.len(), 10);
        assert!(s.startup(8, 8, &[]).is_empty());
        assert_eq!(s.clear().len(), 10);
    }
}
