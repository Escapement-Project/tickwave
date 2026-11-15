//! Bundling: a copy of a recording with its sidecar's files in an annotation bundle record, for a
//! single-file share. Only the bundle record and the footer change: the records before them stay
//! where they are.

use std::collections::BTreeMap;

use crate::compress::compress;
use crate::decode::{DecodeError, Footer, corrupt};
use crate::header::MINOR;
use crate::record::{self, Kind};

/// The annotation bundle record's payload for `files`: their decoded length, then one zstd frame
/// of their JSON, at `level`.
pub fn bundle_payload(
    files: &BTreeMap<String, String>,
    level: i32,
) -> Result<Vec<u8>, DecodeError> {
    let json = serde_json::to_vec(files).expect("a map of strings serialises");
    let frame = compress(&json, level).map_err(|e| DecodeError::Zstd(e.to_string()))?;
    let mut out = Vec::with_capacity(4 + frame.len());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(&frame);
    Ok(out)
}

/// What bundling changes: keep the file's first `keep` bytes, then append `tail`. `record` is
/// the file's footer record, framing included, which `footer` parsed. A bundle the file holds
/// already is replaced.
pub fn bundled_tail(
    record: &[u8],
    footer: &Footer,
    annotations: BTreeMap<String, String>,
    level: i32,
) -> Result<(u64, Vec<u8>), DecodeError> {
    // The footer is written again from what this version reads; a later minor version's fields
    // would be lost while the file still claimed them.
    if footer.trailer.minor > MINOR {
        return Err(corrupt(format!(
            "the file is format 1.{}, newer than this tool's 1.{MINOR}: bundling would drop what it adds",
            footer.trailer.minor
        )));
    }
    let payload = match record::next(record, record.len()) {
        record::Next::Record { kind, payload, len }
            if kind == Kind::Footer as u8 && len == record.len() =>
        {
            payload
        }
        _ => return Err(corrupt("the footer record is not whole")),
    };
    let not_its = || corrupt("the footer record is not the one its footer was parsed from");
    let header_len = payload
        .first_chunk::<4>()
        .map(|b| u32::from_le_bytes(*b) as usize)
        .ok_or_else(not_its)?;
    let top = record::FOOTER_HEAD_LEN + header_len;
    if payload.len() < top.saturating_add(12) {
        return Err(not_its());
    }
    let keep = footer
        .bundle
        .map_or(footer.trailer.footer_offset, |(at, _)| at);
    let mut tail = Vec::new();
    let bundle = bundle_payload(&annotations, level)?;
    record::write(&mut tail, Kind::Bundle, &[&bundle]);
    let place = (keep, tail.len() as u32);
    let mut new = payload.to_vec();
    new[top..top + 8].copy_from_slice(&place.0.to_le_bytes());
    new[top + 8..top + 12].copy_from_slice(&place.1.to_le_bytes());
    record::write_footer(&mut tail, keep, &new);
    Ok((keep, tail))
}

/// `file` with `annotations` (file name to text) in its annotation bundle, replacing any already
/// there. `footer` is `file`'s.
pub fn bundle(
    file: &[u8],
    footer: &Footer,
    annotations: BTreeMap<String, String>,
    level: i32,
) -> Result<Vec<u8>, DecodeError> {
    let at = footer.trailer.footer_offset as usize;
    let len = footer.trailer.footer_len as usize;
    let record = file
        .get(at..at + len)
        .ok_or_else(|| corrupt("the footer lies past the file's end"))?;
    let (keep, tail) = bundled_tail(record, footer, annotations, level)?;
    let mut out = Vec::with_capacity(keep as usize + tail.len());
    out.extend_from_slice(&file[..keep as usize]);
    out.extend_from_slice(&tail);
    Ok(out)
}
