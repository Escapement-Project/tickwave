//! Bundling: a copy of a recording with its sidecar's files in the header, for a single-file
//! share. The blocks and the overviews are copied as they are; only the header and the index's
//! offsets change.

use std::collections::BTreeMap;

use crate::decode::{DecodeError, Head};
use crate::header::{BlockRow, OverviewRow, PREAMBLE_LEN, Preamble};

/// `file` with `annotations` (file name to text) in its header, replacing any already there.
pub fn bundle(file: &[u8], annotations: BTreeMap<String, String>) -> Result<Vec<u8>, DecodeError> {
    let head = Head::parse(file)?;
    let mut header = head.header.clone();
    header.annotations = Some(annotations);
    let header_json = serde_json::to_vec(&header).expect("a header always serialises");
    let old = head.preamble;
    let overview =
        &file[PREAMBLE_LEN + (old.header_len + old.index_len) as usize..old.head_len as usize];
    let index_len = old.index_len as usize;
    let head_len = PREAMBLE_LEN + header_json.len() + index_len + overview.len();
    let head_len = u32::try_from(head_len)
        .map_err(|_| DecodeError::Corrupt("the bundled head passes 4 GiB".into()))?;
    let shift = i64::from(head_len) - i64::from(old.head_len);
    let moved = |offset: u64| {
        offset
            .checked_add_signed(shift)
            .expect("blocks follow the head")
    };
    let mut index = Vec::with_capacity(index_len);
    for row in &head.rows {
        BlockRow {
            offset: moved(row.offset),
            ..row.clone()
        }
        .write(&mut index);
    }
    for row in head.overview_rows.iter().flatten() {
        OverviewRow {
            offset: moved(row.offset),
            ..*row
        }
        .write(&mut index);
    }
    let blocks = &file[old.head_len as usize..];
    let preamble = Preamble {
        header_len: header_json.len() as u32,
        head_len,
        file_len: u64::from(head_len) + blocks.len() as u64,
        ..old
    };
    let mut out = Vec::with_capacity(preamble.file_len as usize);
    out.extend_from_slice(&preamble.to_bytes());
    out.extend_from_slice(&header_json);
    out.extend_from_slice(&index);
    out.extend_from_slice(overview);
    out.extend_from_slice(blocks);
    Ok(out)
}
