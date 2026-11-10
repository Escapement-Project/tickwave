//! A source's file as a reader sees it: a size, and bytes read from any offset. Natively a mapped
//! file or bytes in memory; in the browser, a dropped file read a slice at a time.

use std::io;
use std::path::Path;
use std::sync::Arc;

use memmap2::Mmap;

pub trait Input: Send + Sync {
    fn size(&self) -> u64;
    /// Fills `buf` with the bytes from `offset`; the caller keeps within `size`. A read cannot
    /// fail here: an input that can lose its file mid-run (the browser's, when the file changes on
    /// disk) stops the whole run itself, by throwing through the WASM call.
    fn read_at(&self, offset: u64, buf: &mut [u8]);
}

impl<T: AsRef<[u8]> + Send + Sync> Input for T {
    fn size(&self) -> u64 {
        self.as_ref().len() as u64
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) {
        let at = offset as usize;
        buf.copy_from_slice(&self.as_ref()[at..at + buf.len()]);
    }
}

/// Maps a file.
pub fn map(path: &Path) -> io::Result<Arc<dyn Input>> {
    let file = std::fs::File::open(path)?;
    // SAFETY: nothing may truncate or write the file while it is mapped. tickwave never writes
    // its input, and a converter run over a finished capture holds that.
    let map = unsafe { Mmap::map(&file) }?;
    Ok(Arc::new(map))
}

/// Reads an input whole into memory.
pub fn read_all(input: &dyn Input) -> Vec<u8> {
    let mut bytes = vec![0; input.size() as usize];
    input.read_at(0, &mut bytes);
    bytes
}

/// Bytes read to take a file's items a run at a time.
const CHUNK: usize = 1 << 20;

/// A run of `N`-byte items at the end of an input, read a chunk at a time.
pub(crate) struct Items<const N: usize> {
    input: Arc<dyn Input>,
    /// Where the next chunk starts.
    next: u64,
    /// Items not yet read into `buf`.
    left: u64,
    buf: Vec<u8>,
    at: usize,
    per_chunk: usize,
}

impl<const N: usize> Items<N> {
    /// `count` items from `start`, which the caller has checked the input holds.
    pub(crate) fn new(input: Arc<dyn Input>, start: u64, count: u64) -> Self {
        Self::with_chunk(input, start, count, CHUNK / N)
    }

    pub(crate) fn with_chunk(
        input: Arc<dyn Input>,
        start: u64,
        count: u64,
        per_chunk: usize,
    ) -> Self {
        Self {
            input,
            next: start,
            left: count,
            buf: Vec::new(),
            at: 0,
            per_chunk: per_chunk.max(1),
        }
    }

    /// Items still to come.
    pub(crate) fn remaining(&self) -> u64 {
        self.left + ((self.buf.len() - self.at) / N) as u64
    }
}

impl<const N: usize> Iterator for Items<N> {
    type Item = [u8; N];

    fn next(&mut self) -> Option<[u8; N]> {
        if self.at == self.buf.len() {
            if self.left == 0 {
                return None;
            }
            let take = self.left.min(self.per_chunk as u64) as usize;
            self.buf.resize(take * N, 0);
            self.input.read_at(self.next, &mut self.buf);
            self.next += (take * N) as u64;
            self.left -= take as u64;
            self.at = 0;
        }
        let item = self.buf[self.at..self.at + N].try_into().expect("N bytes");
        self.at += N;
        Some(item)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = usize::try_from(self.remaining()).unwrap_or(usize::MAX);
        (left, Some(left))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_cross_chunks_as_they_lie() {
        let bytes: Vec<u8> = (0..=255u8).cycle().take(5 + 8 * 23).collect();
        let input: Arc<dyn Input> = Arc::new(bytes.clone());
        let whole: Vec<[u8; 8]> = bytes[5..]
            .chunks(8)
            .map(|c| c.try_into().unwrap())
            .collect();
        for per_chunk in [1, 2, 7, 22, 23, 24, 1000] {
            let mut items = Items::<8>::with_chunk(Arc::clone(&input), 5, 23, per_chunk);
            assert_eq!(items.size_hint(), (23, Some(23)));
            items.next();
            assert_eq!(items.remaining(), 22, "{per_chunk} a chunk");
            let rest: Vec<_> = items.collect();
            assert_eq!(rest, whole[1..], "{per_chunk} a chunk");
        }
    }
}
