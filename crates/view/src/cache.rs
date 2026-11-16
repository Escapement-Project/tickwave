//! Decoded blocks, overview blocks, index pages and annotation parts of one file, the least
//! recently used dropped first once they hold more than the cache's bytes.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use escapement_tickwave_format::decode::{
    Block, BlockChannel, BlockRef, LevelRef, List, OverviewBlock,
};
use escapement_tickwave_format::index::Table;
use escapement_tickwave_format::samples::Values;
use lru::LruCache;

/// About the bytes a decoded block holds.
pub(crate) fn block_size(block: &Block) -> usize {
    64 + match &block.data {
        BlockChannel::Digital { edges, source, .. } => {
            (edges.len() + source.as_ref().map_or(0, Vec::len)) * 8
        }
        BlockChannel::Analog { values, .. } => match values {
            Values::F32(v) => v.len() * 4,
            _ => values.len() * 8,
        },
    }
}

/// About the bytes a list of entries holds.
pub(crate) fn list_size<T>(list: &[T]) -> usize {
    64 + std::mem::size_of_val(list)
}

/// A frame or a page by where it sits in the file. What is written never changes, so a key holds
/// across a live recording's snapshots, and across the same file finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Key {
    Block(u64),
    Overview(u64),
    /// A directory page's record of channel `.0`'s table `.1`, by its offset: its bytes, which
    /// each read checks against the entry it comes by.
    Page(usize, Table, u64),
    /// Page `.2` at depth 0 of channel `.0`'s table `.1`: its blocks or rows, checked against
    /// where the table's shape puts it, so the same record listed at another place is checked
    /// there again.
    Leaf(usize, Table, u64),
    /// An annotation part's markers, by its frame's offset.
    Part(u64),
}

#[derive(Clone)]
pub(crate) enum Entry {
    Block(Arc<Block>),
    Overview(Arc<OverviewBlock>),
    Blocks(Arc<Vec<BlockRef>>),
    Rows(Arc<Vec<LevelRef>>),
    List(Arc<List>),
    Bytes(Arc<Vec<u8>>),
    Part(Arc<crate::markers::PartMarkers>),
}

struct Held {
    lru: LruCache<Key, (Entry, usize)>,
    bytes: usize,
    limit: usize,
}

/// Decoded blocks, overview blocks and index pages of one file, least recently used out, by
/// bytes held. Give each file its own: entries are keyed by where they sit in it.
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

/// What one read of a recording (a query, or a call such as `edges`) has taken, held until it
/// ends whatever the cache drops meanwhile, so it reads and decodes each frame and page once
/// however many of its parts need it.
pub(crate) struct Reading<'a> {
    pub cancel: &'a std::sync::atomic::AtomicBool,
    pins: RefCell<HashMap<Key, Entry>>,
}

impl<'a> Reading<'a> {
    pub fn new(cancel: &'a std::sync::atomic::AtomicBool) -> Self {
        Self {
            cancel,
            pins: RefCell::new(HashMap::new()),
        }
    }

    /// The entry for `key`: the one this read took already, else the cache's, else made by
    /// `make`.
    pub fn get_or_make<E>(
        &self,
        cache: &Cache,
        key: Key,
        make: impl FnOnce() -> Result<(Entry, usize), E>,
    ) -> Result<Entry, E> {
        if let Some(entry) = self.pins.borrow().get(&key) {
            return Ok(entry.clone());
        }
        let entry = cache.get_or_make(key, make)?;
        self.pins.borrow_mut().insert(key, entry.clone());
        Ok(entry)
    }
}
