//! The content id: a hash of what a recording holds, not of how a file stores it. Recompressing,
//! re-cutting blocks or bundling annotations leaves it unchanged; a sidecar pins it.
//!
//! BLAKE3 over: the timebase (two u64), the length (i64), then each channel's own BLAKE3 digest.
//! A channel's digest covers what its declaration fixes, then its items, then what only the end
//! settles, so a writer computes it as the items arrive. All little-endian; strings as a u64
//! length and UTF-8 bytes.

use crate::header::ContentId;
use crate::model::{Break, Gap, Ratio};

const BUFFER: usize = 1 << 16;

pub struct ChannelHasher {
    hasher: blake3::Hasher,
    buffer: Vec<u8>,
}

fn put_str(hasher: &mut blake3::Hasher, s: &str) {
    hasher.update(&(s.len() as u64).to_le_bytes());
    hasher.update(s.as_bytes());
}

impl ChannelHasher {
    pub fn digital(name: &str, initial: bool) -> Self {
        let mut hasher = blake3::Hasher::new();
        put_str(&mut hasher, name);
        put_str(&mut hasher, "digital");
        hasher.update(&[u8::from(initial)]);
        Self::from(hasher)
    }

    pub fn analog(name: &str, unit: &str, period: Ratio) -> Self {
        let mut hasher = blake3::Hasher::new();
        put_str(&mut hasher, name);
        put_str(&mut hasher, "analog");
        put_str(&mut hasher, unit);
        hasher.update(&period.num().to_le_bytes());
        hasher.update(&period.den().to_le_bytes());
        Self::from(hasher)
    }

    fn from(hasher: blake3::Hasher) -> Self {
        Self {
            hasher,
            buffer: Vec::with_capacity(BUFFER),
        }
    }

    fn put(&mut self, bytes: &[u8]) {
        if self.buffer.len() + bytes.len() > BUFFER {
            self.hasher.update(&self.buffer);
            self.buffer.clear();
        }
        self.buffer.extend_from_slice(bytes);
    }

    pub fn edge(&mut self, tick: i64) {
        self.put(&tick.to_le_bytes());
    }

    pub fn sample(&mut self, value: f32) {
        self.put(&value.to_bits().to_le_bytes());
    }

    fn items_done(&mut self) -> &mut blake3::Hasher {
        self.hasher.update(&self.buffer);
        self.buffer.clear();
        &mut self.hasher
    }

    pub fn finish_digital(mut self, end: i64, gaps: &[Gap]) -> blake3::Hash {
        let h = self.items_done();
        h.update(&end.to_le_bytes());
        h.update(&(gaps.len() as u64).to_le_bytes());
        for g in gaps {
            h.update(&g.from.to_le_bytes());
            h.update(&g.to.to_le_bytes());
            h.update(&[u8::from(g.level)]);
        }
        h.finalize()
    }

    pub fn finish_analog(mut self, first: i64, count: u64, breaks: &[Break]) -> blake3::Hash {
        let h = self.items_done();
        h.update(&first.to_le_bytes());
        h.update(&count.to_le_bytes());
        h.update(&(breaks.len() as u64).to_le_bytes());
        for b in breaks {
            h.update(&b.index.to_le_bytes());
            h.update(&b.tick.to_le_bytes());
        }
        h.finalize()
    }
}

pub fn content_id(timebase: Ratio, length: i64, channels: &[blake3::Hash]) -> ContentId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&timebase.num().to_le_bytes());
    hasher.update(&timebase.den().to_le_bytes());
    hasher.update(&length.to_le_bytes());
    for digest in channels {
        hasher.update(digest.as_bytes());
    }
    ContentId::new(hasher.finalize())
}
