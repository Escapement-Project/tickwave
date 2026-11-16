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

/// `file` with channel `c`'s first index page of its blocks edited: its rows through `edit`, and
/// the page record's check made again, so a reader sees what the edit says.
pub fn rows_edited(file: &[u8], c: usize, edit: impl FnOnce(&mut [BlockRow])) -> Vec<u8> {
    let footer = escapement_tickwave_format::decode::Footer::parse_file(file).unwrap();
    let entry = footer.channels[c].blocks.as_ref().unwrap().levels[0][0];
    let at = entry.offset as usize;
    let len = entry.length as usize;
    let payload = &file[at + 5..at + len - 8];
    let head = escapement_tickwave_format::index::PAGE_HEAD;
    let (rows, _) = payload[head..].as_chunks::<{ BlockRow::SIZE }>();
    let mut rows: Vec<BlockRow> = rows.iter().map(|r| BlockRow::parse(r)).collect();
    edit(&mut rows);
    let mut edited = payload[..head].to_vec();
    for r in &rows {
        r.write(&mut edited);
    }
    let mut record = Vec::new();
    escapement_tickwave_format::record::write(
        &mut record,
        escapement_tickwave_format::record::Kind::IndexPage,
        &[&edited],
    );
    let mut out = file.to_vec();
    out[at..at + len].copy_from_slice(&record);
    out
}

/// `file` with its footer's header replaced by `header`, the top index and the strips kept.
pub fn with_header(file: &[u8], header: &escapement_tickwave_format::header::Header) -> Vec<u8> {
    let t = file.len() - 24;
    let at = u64::from_le_bytes(file[t..t + 8].try_into().unwrap()) as usize;
    let len = u32::from_le_bytes(file[t + 8..t + 12].try_into().unwrap()) as usize;
    let payload = &file[at + 5..at + len - 8];
    let header_len = u32::from_le_bytes(payload[..4].try_into().unwrap()) as usize;
    let json = serde_json::to_vec(header).unwrap();
    let mut new = Vec::new();
    new.extend_from_slice(&(json.len() as u32).to_le_bytes());
    new.extend_from_slice(&payload[4..8]);
    new.extend_from_slice(&json);
    new.extend_from_slice(&payload[8 + header_len..]);
    let mut out = file[..at].to_vec();
    escapement_tickwave_format::record::write_footer(&mut out, 0, &new);
    out
}

/// Each generator's lines in `file`'s sidecar, joined from its bundle and parts, by generator.
pub fn generated(file: &[u8]) -> std::collections::BTreeMap<String, Vec<String>> {
    let footer = escapement_tickwave_format::decode::Footer::parse_file(file).unwrap();
    let sidecar = footer
        .read_sidecar(escapement_tickwave_format::decode::from_memory(file))
        .unwrap();
    let mut out = std::collections::BTreeMap::new();
    for text in sidecar.iter().flat_map(|s| s.files.values()) {
        if let Some(source) = escapement_tickwave_format::annotations::generator_source(text) {
            let lines = text.lines().skip(1).filter(|l| !l.trim().is_empty());
            out.insert(source, lines.map(str::to_owned).collect());
        }
    }
    out
}
