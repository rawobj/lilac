//! Play queue: a context (album, playlist, ...) played in order or shuffled,
//! plus the explicit "Add to queue" list that always takes priority.

use std::collections::VecDeque;

use gpui_kit::SharedString;

use crate::model::{ContextUri, Track};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Repeat {
    #[default]
    Off,
    Context,
    Track,
}

impl Repeat {
    pub fn next(self) -> Self {
        match self {
            Repeat::Off => Repeat::Context,
            Repeat::Context => Repeat::Track,
            Repeat::Track => Repeat::Off,
        }
    }
}

/// Small xorshift generator. Shuffle order does not need to be cryptographic.
struct Rng(u64);

impl Rng {
    fn new() -> Self {
        let mut seed = [0u8; 8];
        let _ = getrandom::fill(&mut seed);
        Rng(u64::from_le_bytes(seed) | 1)
    }

    fn below(&mut self, bound: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % bound as u64) as usize
    }
}

#[derive(Default)]
pub struct Queue {
    pub context: Option<ContextUri>,
    pub context_name: SharedString,
    tracks: Vec<Track>,
    /// Indices into `tracks` in play order.
    order: Vec<u32>,
    /// Position in `order` of the context track that is playing or was last played.
    cursor: usize,
    /// Tracks queued by hand, played before the context continues.
    pub user: VecDeque<Track>,
    shuffled: bool,
}

impl Queue {
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty() && self.user.is_empty()
    }

    pub fn shuffled(&self) -> bool {
        self.shuffled
    }

    /// A read-only copy of what Spotify says plays next. Spotify does not say
    /// which entries were queued by hand, so all of them count as upcoming.
    pub fn mirror(current: Option<Track>, upcoming: Vec<Track>) -> Self {
        let mut tracks = Vec::with_capacity(upcoming.len() + 1);
        tracks.push(current.unwrap_or_default());
        tracks.extend(upcoming);
        Queue {
            order: (0..tracks.len() as u32).collect(),
            tracks,
            ..Queue::default()
        }
    }

    /// Replace the context and start at `index` (an index into `tracks`).
    pub fn set_context(
        &mut self,
        context: Option<ContextUri>,
        name: SharedString,
        tracks: Vec<Track>,
        index: usize,
        shuffle: bool,
    ) -> Option<Track> {
        self.context = context;
        self.context_name = name;
        self.tracks = tracks;
        self.shuffled = shuffle;
        self.order = (0..self.tracks.len() as u32).collect();
        let index = index.min(self.tracks.len().saturating_sub(1));
        self.cursor = index;
        if shuffle {
            self.shuffle_keeping(index);
        }
        self.current().cloned()
    }

    /// Append more tracks of the same context (lazily loaded pages).
    pub fn extend_context(&mut self, tracks: Vec<Track>) {
        let start = self.tracks.len() as u32;
        self.tracks.extend(tracks);
        let mut added: Vec<u32> = (start..self.tracks.len() as u32).collect();
        if self.shuffled {
            let mut rng = Rng::new();
            for i in (1..added.len()).rev() {
                added.swap(i, rng.below(i + 1));
            }
        }
        self.order.extend(added);
    }

    /// The context track at the cursor.
    pub fn current(&self) -> Option<&Track> {
        self.order
            .get(self.cursor)
            .and_then(|ix| self.tracks.get(*ix as usize))
    }

    pub fn context_len(&self) -> usize {
        self.tracks.len()
    }

    /// Advance and return what to play next. `None` means the queue ran out.
    pub fn advance(&mut self, repeat: Repeat) -> Option<Track> {
        if let Some(track) = self.user.pop_front() {
            return Some(track);
        }
        if self.order.is_empty() {
            return None;
        }
        if self.cursor + 1 < self.order.len() {
            self.cursor += 1;
        } else if repeat == Repeat::Context {
            self.cursor = 0;
        } else {
            return None;
        }
        self.current().cloned()
    }

    /// Step back to the previous context track.
    pub fn retreat(&mut self, repeat: Repeat) -> Option<Track> {
        if self.order.is_empty() {
            return None;
        }
        if self.cursor > 0 {
            self.cursor -= 1;
        } else if repeat == Repeat::Context {
            self.cursor = self.order.len() - 1;
        }
        self.current().cloned()
    }

    /// What `advance` would return, without moving. Used to preload.
    pub fn peek(&self, repeat: Repeat) -> Option<&Track> {
        if let Some(track) = self.user.front() {
            return Some(track);
        }
        if self.cursor + 1 < self.order.len() {
            self.tracks.get(self.order[self.cursor + 1] as usize)
        } else if repeat == Repeat::Context && !self.order.is_empty() {
            self.tracks.get(self.order[0] as usize)
        } else {
            None
        }
    }

    pub fn set_shuffle(&mut self, on: bool) {
        if on == self.shuffled {
            return;
        }
        self.shuffled = on;
        if self.order.is_empty() {
            return;
        }
        let current = self.order[self.cursor] as usize;
        if on {
            self.shuffle_keeping(current);
        } else {
            self.order = (0..self.tracks.len() as u32).collect();
            self.cursor = current;
        }
    }

    /// Shuffle so that track `keep` is first and the cursor points at it.
    fn shuffle_keeping(&mut self, keep: usize) {
        let mut rng = Rng::new();
        for i in (1..self.order.len()).rev() {
            self.order.swap(i, rng.below(i + 1));
        }
        if let Some(pos) = self.order.iter().position(|ix| *ix as usize == keep) {
            self.order.swap(0, pos);
        }
        self.cursor = 0;
    }

    pub fn push_user(&mut self, track: Track) {
        self.user.push_back(track);
    }

    pub fn push_user_front(&mut self, track: Track) {
        self.user.push_front(track);
    }

    pub fn remove_user(&mut self, index: usize) {
        if index < self.user.len() {
            self.user.remove(index);
        }
    }

    pub fn clear_user(&mut self) {
        self.user.clear();
    }

    /// Context tracks still to come, in play order.
    pub fn upcoming(&self) -> impl Iterator<Item = &Track> {
        self.order
            .iter()
            .skip(self.cursor + 1)
            .filter_map(|ix| self.tracks.get(*ix as usize))
    }

    /// Jump to the `n`-th upcoming context track.
    pub fn skip_to_upcoming(&mut self, n: usize) -> Option<Track> {
        let target = self.cursor + 1 + n;
        if target < self.order.len() {
            self.cursor = target;
            self.current().cloned()
        } else {
            None
        }
    }

    /// Take the `n`-th user-queued track, dropping the ones before it.
    pub fn skip_to_user(&mut self, n: usize) -> Option<Track> {
        if n >= self.user.len() {
            return None;
        }
        self.user.drain(..n);
        self.user.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracks(n: usize) -> Vec<Track> {
        (0..n)
            .map(|i| Track {
                id: i.to_string().into(),
                name: format!("t{i}").into(),
                ..Default::default()
            })
            .collect()
    }

    fn id(t: Option<Track>) -> String {
        t.map(|t| t.id.to_string()).unwrap_or_default()
    }

    #[test]
    fn plays_in_order_and_stops_at_the_end() {
        let mut q = Queue::default();
        assert_eq!(
            id(q.set_context(None, "x".into(), tracks(3), 1, false)),
            "1"
        );
        assert_eq!(id(q.advance(Repeat::Off)), "2");
        assert!(q.advance(Repeat::Off).is_none());
        assert_eq!(id(q.advance(Repeat::Context)), "0");
        assert_eq!(id(q.retreat(Repeat::Off)), "0");
        assert_eq!(id(q.retreat(Repeat::Context)), "2");
    }

    #[test]
    fn user_queue_takes_priority_and_context_resumes() {
        let mut q = Queue::default();
        q.set_context(None, "x".into(), tracks(3), 0, false);
        q.push_user(Track {
            id: "u1".into(),
            ..Default::default()
        });
        q.push_user_front(Track {
            id: "u0".into(),
            ..Default::default()
        });
        assert_eq!(q.peek(Repeat::Off).unwrap().id.as_ref(), "u0");
        assert_eq!(id(q.advance(Repeat::Off)), "u0");
        assert_eq!(id(q.advance(Repeat::Off)), "u1");
        assert_eq!(id(q.advance(Repeat::Off)), "1");
    }

    #[test]
    fn shuffle_keeps_current_and_covers_everything() {
        let mut q = Queue::default();
        q.set_context(None, "x".into(), tracks(50), 7, true);
        assert_eq!(q.current().unwrap().id.as_ref(), "7");
        let mut seen = vec![7usize];
        while let Some(t) = q.advance(Repeat::Off) {
            seen.push(t.id.parse().unwrap());
        }
        seen.sort_unstable();
        assert_eq!(seen, (0..50).collect::<Vec<_>>());
    }

    #[test]
    fn unshuffle_restores_position() {
        let mut q = Queue::default();
        q.set_context(None, "x".into(), tracks(10), 4, false);
        q.set_shuffle(true);
        assert_eq!(q.current().unwrap().id.as_ref(), "4");
        q.set_shuffle(false);
        assert_eq!(q.current().unwrap().id.as_ref(), "4");
        assert_eq!(id(q.advance(Repeat::Off)), "5");
    }

    #[test]
    fn skipping_into_upcoming() {
        let mut q = Queue::default();
        q.set_context(None, "x".into(), tracks(5), 0, false);
        assert_eq!(q.upcoming().count(), 4);
        assert_eq!(id(q.skip_to_upcoming(2)), "3");
        assert_eq!(q.upcoming().count(), 1);
        q.extend_context(tracks(2));
        assert_eq!(q.upcoming().count(), 3);
    }
}
