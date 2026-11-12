//! Bundling: a copy of a recording with its sidecar's files in the header, for a single-file
//! share. Only the footer changes: the records before it stay where they are.

use std::collections::BTreeMap;

use crate::decode::{DecodeError, Footer};
use crate::header::MINOR;
use crate::record;

/// What bundling changes: keep the file's first `keep` bytes, then append `tail`.
pub fn bundled_tail(
    footer: &Footer,
    annotations: BTreeMap<String, String>,
) -> Result<(u64, Vec<u8>), DecodeError> {
    // The header is written again from what this version reads; a later minor version's fields
    // would be lost while the file still claimed them.
    if footer.trailer.minor > MINOR {
        return Err(DecodeError::Corrupt(format!(
            "the file is format 1.{}, newer than this tool's 1.{MINOR}: bundling would drop what it adds",
            footer.trailer.minor
        )));
    }
    let mut header = footer.header.clone();
    header.annotations = Some(annotations);
    let overview_rows: Vec<_> = footer.overview_rows.iter().flatten().copied().collect();
    let bytes = record::footer_bytes(
        &header,
        &footer.rows,
        &overview_rows,
        &footer.overview_frame,
    )
    .map_err(|n| DecodeError::Corrupt(format!("the bundled footer is {n} bytes, past 4 GiB")))?;
    let keep = footer.trailer.footer_offset;
    let mut tail = Vec::new();
    record::write_footer(&mut tail, keep, &bytes);
    Ok((keep, tail))
}

/// `file` with `annotations` (file name to text) in its header, replacing any already there.
/// `footer` is `file`'s.
pub fn bundle(
    file: &[u8],
    footer: &Footer,
    annotations: BTreeMap<String, String>,
) -> Result<Vec<u8>, DecodeError> {
    let (keep, tail) = bundled_tail(footer, annotations)?;
    let mut out = Vec::with_capacity(keep as usize + tail.len());
    out.extend_from_slice(&file[..keep as usize]);
    out.extend_from_slice(&tail);
    Ok(out)
}
