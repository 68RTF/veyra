use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Default)]
pub(crate) struct ShuffleOrder {
    played: HashSet<PathBuf>,
    history: Vec<PathBuf>,
    cursor: Option<usize>,
}

impl ShuffleOrder {
    pub(crate) fn record(&mut self, path: &Path) {
        self.played.insert(path.to_path_buf());
        let length = self.cursor.map_or(0, |cursor| cursor + 1);
        if length > 0 && self.history[length - 1] == path {
            return;
        }
        self.history.truncate(length);
        self.history.push(path.to_path_buf());
        self.cursor = Some(length);
    }

    pub(crate) fn has_previous(&self) -> bool {
        self.cursor.is_some_and(|cursor| cursor > 0)
    }

    pub(crate) fn previous(&mut self) -> Option<&Path> {
        let previous = self.cursor?.checked_sub(1)?;
        self.cursor = Some(previous);
        Some(&self.history[previous])
    }

    pub(crate) fn can_advance(&self, track_count: usize) -> bool {
        self.cursor
            .is_some_and(|cursor| cursor + 1 < self.history.len())
            || self.played.len() < track_count
    }

    pub(crate) fn next<'a>(
        &mut self,
        tracks: impl Iterator<Item = &'a Path>,
        current: &Path,
        repeat: bool,
    ) -> Option<PathBuf> {
        let forward = self.cursor.map_or(0, |cursor| cursor + 1);
        if let Some(path) = self.history.get(forward) {
            self.cursor = Some(forward);
            return Some(path.clone());
        }

        let tracks: Vec<_> = tracks.collect();
        let mut candidates: Vec<_> = tracks
            .iter()
            .copied()
            .filter(|path| !self.played.contains(*path))
            .collect();
        if candidates.is_empty() && repeat {
            self.played.clear();
            candidates.extend(tracks.iter().copied().filter(|path| *path != current));
            if candidates.is_empty() {
                candidates.extend(tracks);
            }
        }
        if candidates.is_empty() {
            return None;
        }
        let pick = glib::random_int_range(0, candidates.len() as i32);
        Some(candidates[pick as usize].to_path_buf())
    }

    pub(crate) fn remove(&mut self, path: &Path) {
        self.played.remove(path);
        let keep_through_cursor = self.cursor.map_or(0, |cursor| {
            self.history[..=cursor]
                .iter()
                .filter(|track| *track != path)
                .count()
        });
        self.history.retain(|track| track != path);
        self.cursor = keep_through_cursor.checked_sub(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plays_each_track_once_and_replays_history_in_both_directions() {
        let tracks = [Path::new("a"), Path::new("b"), Path::new("c")];
        let mut order = ShuffleOrder::default();
        order.record(tracks[0]);
        let second = order.next(tracks.into_iter(), tracks[0], false).unwrap();
        order.record(&second);
        let third = order.next(tracks.into_iter(), &second, false).unwrap();
        order.record(&third);
        assert_ne!(second, third);
        assert_ne!(third, tracks[0]);
        assert!(order.next(tracks.into_iter(), &third, false).is_none());
        assert_eq!(order.previous(), Some(second.as_path()));
        assert_eq!(order.previous(), Some(tracks[0]));
        assert_eq!(
            order.next(tracks.into_iter(), tracks[0], false),
            Some(second)
        );
        assert_eq!(
            order.next(tracks.into_iter(), tracks[0], false),
            Some(third.clone())
        );
        assert_ne!(order.next(tracks.into_iter(), &third, true), Some(third));
    }

    #[test]
    fn removing_a_track_prunes_history_and_new_tracks_enter_the_current_cycle() {
        let mut order = ShuffleOrder::default();
        let a = Path::new("a");
        let b = Path::new("b");
        let c = Path::new("c");
        order.record(a);
        order.record(b);
        order.remove(a);
        assert!(!order.has_previous());
        assert_eq!(
            order.next([b, c].into_iter(), b, false),
            Some(c.to_path_buf())
        );
    }

    #[test]
    fn repeated_cycles_never_repeat_inside_a_cycle() {
        for count in 1..=32 {
            let tracks: Vec<_> = (0..count)
                .map(|i| PathBuf::from(format!("Трек \"{i}\"\n.mp3")))
                .collect();
            let mut current = tracks[0].clone();
            let mut order = ShuffleOrder::default();
            order.record(&current);
            for cycle in 0..16 {
                let mut seen = HashSet::from([current.clone()]);
                for _ in 1..count {
                    let next = order
                        .next(tracks.iter().map(PathBuf::as_path), &current, false)
                        .unwrap();
                    assert!(seen.insert(next.clone()), "count={count} cycle={cycle}");
                    order.record(&next);
                    current = next;
                }
                assert!(order
                    .next(tracks.iter().map(PathBuf::as_path), &current, false)
                    .is_none());
                let next = order
                    .next(tracks.iter().map(PathBuf::as_path), &current, true)
                    .unwrap();
                if count > 1 {
                    assert_ne!(next, current);
                }
                order.record(&next);
                current = next;
            }
        }
    }

    #[test]
    fn removing_all_history_allows_a_new_queue() {
        let mut order = ShuffleOrder::default();
        for track in ["a", "b", "c"] {
            order.record(Path::new(track));
        }
        order.previous();
        for track in ["c", "a", "b"] {
            order.remove(Path::new(track));
        }
        assert!(!order.has_previous());
        assert!(order.previous().is_none());
        assert!(order
            .next(std::iter::empty(), Path::new("gone"), true)
            .is_none());
        assert_eq!(
            order.next([Path::new("new")].into_iter(), Path::new("gone"), false),
            Some(PathBuf::from("new"))
        );
    }
}
