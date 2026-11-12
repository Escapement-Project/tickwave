//! Edits to a finished file's footer that keep it whole, so a reader sees what an edit says rather
//! than a record that does not check.

#![allow(dead_code)]

use escapement_tickwave_format::header::BlockRow;

/// `file` with its footer's payload edited in place, and the footer record's check made again.
pub fn refooted(file: &[u8], edit: impl FnOnce(&mut [u8])) -> Vec<u8> {
    let mut out = file.to_vec();
    let t = out.len() - 24;
    let at = u64::from_le_bytes(out[t..t + 8].try_into().unwrap()) as usize;
    let len = u32::from_le_bytes(out[t + 8..t + 12].try_into().unwrap()) as usize;
    let payload = at + 5..at + len - 8;
    edit(&mut out[payload.clone()]);
    let mut h = blake3::Hasher::new();
    h.update(&[out[at]]);
    h.update(&(payload.len() as u32).to_le_bytes());
    h.update(&out[payload]);
    out[at + len - 8..at + len].copy_from_slice(&h.finalize().as_bytes()[..8]);
    out
}

/// `file` with `from` replaced by `to` in its footer's header, which must be the same length so
/// that every offset stays right.
pub fn patched(file: &[u8], from: &str, to: &str) -> Vec<u8> {
    assert_eq!(from.len(), to.len());
    refooted(file, |footer| {
        let at = footer
            .windows(from.len())
            .position(|w| w == from.as_bytes())
            .unwrap_or_else(|| panic!("{from} is not in the header"));
        footer[at..at + to.len()].copy_from_slice(to.as_bytes());
    })
}

/// Where row `i`'s word for channel `c` sits in the footer's payload.
pub fn word_at(footer: &[u8], channels: usize, i: usize, c: usize) -> usize {
    let header_len = u32::from_le_bytes(footer[..4].try_into().unwrap()) as usize;
    8 + header_len + i * BlockRow::size(channels) + 24 + 4 * c
}
