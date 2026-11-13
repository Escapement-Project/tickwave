//! An append-only list a snapshot shares with the writer: whole chunks behind `Arc`s, and a tail
//! the writer copies only when a snapshot still holds it.

use std::sync::Arc;

/// Elements in one sealed chunk.
const CHUNK: usize = 1024;

#[derive(Clone, Debug)]
pub struct Chunks<T> {
    sealed: Vec<Arc<[T]>>,
    tail: Arc<Vec<T>>,
}

impl<T> Default for Chunks<T> {
    fn default() -> Self {
        Self {
            sealed: Vec::new(),
            tail: Arc::new(Vec::new()),
        }
    }
}

impl<T: Clone> Chunks<T> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, value: T) {
        Arc::make_mut(&mut self.tail).push(value);
        if self.tail.len() == CHUNK {
            let full = std::mem::take(Arc::make_mut(&mut self.tail));
            self.sealed.push(full.into());
        }
    }

    /// Takes the last element off; it must be in the tail.
    pub fn pop_tail(&mut self) -> Option<T> {
        Arc::make_mut(&mut self.tail).pop()
    }

    pub fn len(&self) -> usize {
        self.sealed.len() * CHUNK + self.tail.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, i: usize) -> Option<&T> {
        match self.sealed.get(i / CHUNK) {
            Some(chunk) => chunk.get(i % CHUNK),
            None => self.tail.get(i - self.sealed.len() * CHUNK),
        }
    }

    pub fn last(&self) -> Option<&T> {
        self.tail.last().or_else(|| self.sealed.last()?.last())
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &T> + '_ {
        self.sealed
            .iter()
            .flat_map(|c| c.iter())
            .chain(self.tail.iter())
    }

    /// The elements from `start` on. It steps over whole chunks to get there, so a reader that
    /// takes only what was added since it last looked does work for what's new alone.
    pub fn iter_from(&self, start: usize) -> impl Iterator<Item = &T> + '_ {
        let chunk = (start / CHUNK).min(self.sealed.len());
        let within = if chunk < self.sealed.len() {
            start % CHUNK
        } else {
            0
        };
        let tail = start
            .saturating_sub(self.sealed.len() * CHUNK)
            .min(self.tail.len());
        self.sealed[chunk..]
            .iter()
            .flat_map(|c| c.iter())
            .skip(within)
            .chain(self.tail[tail..].iter())
    }

    pub fn to_vec(&self) -> Vec<T> {
        self.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_keeps_what_it_saw_while_the_list_grows() {
        let mut list = Chunks::new();
        for i in 0..2500 {
            list.push(i);
        }
        let seen = list.clone();
        for i in 2500..4000 {
            list.push(i);
        }
        assert_eq!(seen.len(), 2500);
        assert_eq!(seen.to_vec(), (0..2500).collect::<Vec<_>>());
        assert_eq!(list.to_vec(), (0..4000).collect::<Vec<_>>());
        assert_eq!(
            (list.get(1023), list.get(1024), list.get(3999)),
            (Some(&1023), Some(&1024), Some(&3999))
        );
        assert_eq!(list.get(4000), None);
        for start in [0, 1, 1023, 1024, 1025, 3071, 3072, 3999, 4000, 5000] {
            assert!(
                list.iter_from(start).copied().eq(start.min(4000)..4000),
                "from {start}"
            );
        }
        assert_eq!(list.last(), Some(&3999));
        // The sealed chunks are the same allocations in both.
        assert!(Arc::ptr_eq(&seen.sealed[0], &list.sealed[0]));
    }
}
