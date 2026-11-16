//! Bundling: a copy of a recording with its sidecar's files in it, for a single-file share. The
//! hand-written file and each generator file's first line go in the annotation bundle record, and
//! each generator file's other lines in annotation parts that stand alone, in the order the
//! hand-written file includes them (ANNOTATIONS.md, "Bundling"). Where the file's parts all follow
//! its data, as a bundle's do, the records before them stay where they are and the footer is
//! written again; otherwise the copy is written again through a `Recutter`.

use std::collections::BTreeMap;

use crate::compress::compress;
use crate::decode::{DecodeError, Footer, corrupt};
use crate::header::MINOR;
use crate::index::{Table, TableWriter};
use crate::parts::PartRow;
use crate::record::{self, Kind};
use crate::recut::{RecutError, Recutter};
use crate::writer::{Bundle, Bundling, WriteOptions, place_dues, write_part_record};

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

/// What bundling a file changes.
#[derive(Debug)]
pub enum Tail {
    /// Keep the file's first `keep` bytes, then append `tail`.
    Append { keep: u64, tail: Vec<u8> },
    /// The file's parts lie among its data, as a writer's are, and its annotation records stay
    /// where they are, so the file is written again: through a `Recutter` given these files as
    /// its `sidecar`.
    Recut(BTreeMap<String, String>),
}

/// What bundling `annotations` changes. `record` is the file's footer record, framing included,
/// which `footer` parsed; `fetch` reads the file's bytes, for the parts table where it has one.
/// The annotations a bundle placed before are replaced: the copy starts its tail where they
/// started.
pub fn bundled_tail<E: From<DecodeError>>(
    record: &[u8],
    footer: &Footer,
    annotations: BTreeMap<String, String>,
    level: i32,
    mut fetch: impl FnMut(u64, u32) -> Result<Vec<u8>, E>,
) -> Result<Tail, E> {
    // The footer is written again from what this version reads; a later minor version's fields
    // would be lost while the file still claimed them.
    if footer.trailer.minor > MINOR {
        return Err(corrupt(format!(
            "the file is format 1.{}, newer than this tool's 1.{MINOR}: bundling would drop what it adds",
            footer.trailer.minor
        ))
        .into());
    }
    let payload = match record::next(record, record.len()) {
        record::Next::Record { kind, payload, len }
            if kind == Kind::Footer as u8 && len == record.len() =>
        {
            payload
        }
        _ => return Err(corrupt("the footer record is not whole").into()),
    };
    let not_its = || corrupt("the footer record is not the one its footer was parsed from");
    let u32_at = |at: usize| {
        payload
            .get(at..at + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize)
    };
    let (Some(header_len), Some(index_len)) = (u32_at(0), u32_at(4)) else {
        return Err(not_its().into());
    };
    let top = record::FOOTER_HEAD_LEN + header_len;
    let strips = top + index_len;
    // The top index past the bundle's place and the parts table: each channel's tables.
    let parts_len = footer.parts.as_ref().map_or(0, |t| {
        2 + t.levels.last().map_or(0, Vec::len) * crate::index::Entry::SIZE
    });
    let channels = top + 12 + parts_len;
    if payload.len() < strips || channels > strips {
        return Err(not_its().into());
    }
    let mut keep = footer
        .bundle
        .map_or(footer.trailer.footer_offset, |(at, _)| at);
    if footer.parts.is_some() {
        let rows = footer.part_rows(&mut fetch)?;
        let first = rows
            .iter()
            .map(|r| r.offset - (record::PAYLOAD_AT + 4) as u64)
            .min()
            .unwrap_or(keep);
        // A part before the last of the data's index pages lies among the data.
        let data_end = footer
            .channels
            .iter()
            .flat_map(|c| {
                c.blocks
                    .iter()
                    .chain(c.levels.iter().map(|(_, t)| t))
                    .chain(c.list.iter())
            })
            .flat_map(|t| t.levels.last().into_iter().flatten())
            .map(|e| e.offset + u64::from(e.length))
            .max()
            .unwrap_or(0);
        if first < data_end {
            return Ok(Tail::Recut(annotations));
        }
        keep = keep.min(first);
    }
    let timebase = footer.header.timebase;
    let Bundling::Files { files, parts } =
        Bundling::of(Some(Bundle::Files(annotations)), &[], false, timebase)
            .map_err(|e| corrupt(e.to_string()))?
    else {
        unreachable!("files given give files")
    };
    let mut tail = Vec::new();
    let mut table: TableWriter<PartRow> = TableWriter::new(0, Table::Parts, false);
    for (raw, span) in parts {
        let row = write_part_record(&mut tail, keep, &raw, span, level)
            .map_err(|e| corrupt(e.to_string()))?;
        let due = table.push(row);
        place_dues(&mut table, due, &mut tail, keep);
    }
    loop {
        let due = table.finish_next(0);
        if due.is_none() {
            break;
        }
        place_dues(&mut table, due, &mut tail, keep);
    }
    let at = keep + tail.len() as u64;
    let bundle = bundle_payload(&files, level)?;
    record::write(&mut tail, Kind::Bundle, &[&bundle]);
    let place = (at, (keep + tail.len() as u64 - at) as u32);
    let mut header = footer.header.clone();
    header.annotation_parts = (table.count() > 0).then_some(table.count());
    let json = serde_json::to_vec(&header).expect("a header always serialises");
    let mut index = Vec::with_capacity(index_len + 2 + 16 * crate::index::Entry::SIZE);
    index.extend_from_slice(&place.0.to_le_bytes());
    index.extend_from_slice(&place.1.to_le_bytes());
    if let Some((depth, entries)) = table.top() {
        index.push(depth);
        index.push(entries.len() as u8);
        for e in &entries {
            e.write(&mut index);
        }
    }
    index.extend_from_slice(&payload[channels..strips]);
    let new = record::footer_bytes(&json, &index, &payload[strips..]).map_err(|n| {
        corrupt(format!(
            "the footer would be {n} bytes, past its u32 length"
        ))
    })?;
    record::write_footer(&mut tail, keep, &new);
    Ok(Tail::Append { keep, tail })
}

/// `file` written again through a `Recutter` with `annotations` (file name to text) in place of
/// its own: its blocks cut to the usual size, its content the same.
pub fn recut_with(
    file: &[u8],
    annotations: BTreeMap<String, String>,
    level: i32,
) -> Result<Vec<u8>, RecutError> {
    let mut r = Recutter::new(WriteOptions {
        level,
        ..WriteOptions::default()
    });
    r.sidecar(annotations);
    r.feed(file)?;
    let mut out = r.take_output();
    out.extend(r.finish()?.tail);
    Ok(out)
}

/// `file` with `annotations` (file name to text) bundled in it, replacing any already there.
/// `footer` is `file`'s.
pub fn bundle(
    file: &[u8],
    footer: &Footer,
    annotations: BTreeMap<String, String>,
    level: i32,
) -> Result<Vec<u8>, RecutError> {
    let at = footer.trailer.footer_offset as usize;
    let len = footer.trailer.footer_len as usize;
    let record = file
        .get(at..at + len)
        .ok_or_else(|| corrupt("the footer lies past the file's end"))?;
    let tail = bundled_tail(
        record,
        footer,
        annotations,
        level,
        crate::decode::from_memory(file),
    )?;
    Ok(match tail {
        Tail::Append { keep, tail } => {
            let mut out = Vec::with_capacity(keep as usize + tail.len());
            out.extend_from_slice(&file[..keep as usize]);
            out.extend_from_slice(&tail);
            out
        }
        Tail::Recut(annotations) => recut_with(file, annotations, level)?,
    })
}
