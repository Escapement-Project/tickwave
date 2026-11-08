//! The content id: a hash of what a recording holds, not of how a file stores it. Recompressing,
//! re-cutting blocks or bundling annotations leaves it unchanged; a sidecar pins it.
//!
//! BLAKE3 over: the timebase (two u64), the length (i64), then each channel's own BLAKE3 digest.
//! A channel's digest covers its name, its kind's fields and then every item: a digital channel's
//! edge ticks as i64, an analog channel's values as f32 bits. All little-endian; strings as a u64
//! length and UTF-8 bytes.

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
    pub fn digital(name: &str, initial: bool, end: i64) -> Self {
        let mut hasher = blake3::Hasher::new();
        put_str(&mut hasher, name);
        put_str(&mut hasher, "digital");
        hasher.update(&[u8::from(initial)]);
        hasher.update(&end.to_le_bytes());
        Self::from(hasher)
    }

    pub fn analog(name: &str, unit: &str, first: i64, period: [u64; 2], count: u64) -> Self {
        let mut hasher = blake3::Hasher::new();
        put_str(&mut hasher, name);
        put_str(&mut hasher, "analog");
        put_str(&mut hasher, unit);
        hasher.update(&first.to_le_bytes());
        hasher.update(&period[0].to_le_bytes());
        hasher.update(&period[1].to_le_bytes());
        hasher.update(&count.to_le_bytes());
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

    pub fn finish(mut self) -> blake3::Hash {
        self.hasher.update(&self.buffer);
        self.hasher.finalize()
    }
}

pub fn content_id(timebase: [u64; 2], length: i64, channels: &[blake3::Hash]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&timebase[0].to_le_bytes());
    hasher.update(&timebase[1].to_le_bytes());
    hasher.update(&length.to_le_bytes());
    for digest in channels {
        hasher.update(digest.as_bytes());
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}
