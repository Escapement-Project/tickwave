//! Decoded blocks and overview blocks of one file, the least recently used dropped first once
//! they hold more than the cache's bytes.

use std::sync::{Arc, Mutex};

use escapement_tickwave_format::decode::{Block, OverviewBlock};
use escapement_tickwave_format::samples::Values;
use lru::LruCache;

/// A block decoded, with its derived channels' samples.
#[derive(Debug)]
pub(crate) struct Decoded {
    pub block: Block,
    /// Each derived channel's samples in the block.
    pub derived: Vec<Values>,
}

impl Decoded {
    /// About the bytes it holds.
    pub fn size(&self) -> usize {
        use escapement_tickwave_format::decode::BlockChannel;
        let values = |v: &Values| match v {
            Values::F32(v) => v.len() * 4,
            _ => v.len() * 8,
        };
        64 + self
            .block
            .channels
            .iter()
            .map(|c| match c {
                BlockChannel::Digital { edges, .. } => edges.len() * 8 + 32,
                BlockChannel::Analog { values: v, .. } => values(v) + 32,
            })
            .sum::<usize>()
            + self.derived.iter().map(values).sum::<usize>()
    }
}

/// A frame by where it sits in the file. A written frame never changes, so a key holds across a
/// live recording's snapshots, and across the same file finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Key {
    Block(u64),
    Overview(u64),
}

#[derive(Clone)]
pub(crate) enum Entry {
    Block(Arc<Decoded>),
    Overview(Arc<OverviewBlock>),
}

struct Held {
    lru: LruCache<Key, (Entry, usize)>,
    bytes: usize,
    limit: usize,
}

/// Decoded blocks and overview blocks of one file, least recently used out, by bytes held. Give
/// each file its own: entries are keyed by where their frames sit in it.
#[derive(Clone)]
pub struct Cache {
    held: Arc<Mutex<Held>>,
}

impl Cache {
    /// A cache that holds about `bytes` of decoded data. An entry larger than that is handed out
    /// and not kept.
    pub fn new(bytes: usize) -> Self {
        Self {
            held: Arc::new(Mutex::new(Held {
                lru: LruCache::unbounded(),
                bytes: 0,
                limit: bytes,
            })),
        }
    }

    /// About the bytes the cache holds.
    pub fn bytes(&self) -> usize {
        self.lock().bytes
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Held> {
        // A thread that panicked holding the lock left the map whole: every change to it is one
        // call.
        self.held.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The entry for `key`, made by `make` if the cache doesn't hold it. `make` runs without the
    /// lock, so two threads can make the same entry; both get one.
    pub(crate) fn get_or_make<E>(
        &self,
        key: Key,
        make: impl FnOnce() -> Result<(Entry, usize), E>,
    ) -> Result<Entry, E> {
        if let Some((entry, _)) = self.lock().lru.get(&key) {
            return Ok(entry.clone());
        }
        let (entry, size) = make()?;
        let mut held = self.lock();
        if let Some((_, old)) = held.lru.put(key, (entry.clone(), size)) {
            held.bytes -= old;
        }
        held.bytes += size;
        while held.bytes > held.limit {
            let Some((_, (_, size))) = held.lru.pop_lru() else {
                break;
            };
            held.bytes -= size;
        }
        Ok(entry)
    }
}
