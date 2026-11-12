//! Writes the recordings in web/fixtures/crafted/: each a real file with one thing in its footer or
//! a block changed to break a reader that trusts what a file states. Run from the repository:
//!
//!     cargo run -p escapement-tickwave-format --example craft -- web/fixtures/crafted

use std::collections::BTreeMap;
use std::path::PathBuf;

use escapement_tickwave_format::decode::Footer;
use escapement_tickwave_format::header::{
    BlockRow, Declaration, DeclaredChannel, DeclaredKind, Header, LEVEL_BIT, OverviewRow,
    SourceFormat, SourceHeader,
};
use escapement_tickwave_format::model::Ratio;
use escapement_tickwave_format::record::{self, BlockHead, Kind};
use escapement_tickwave_format::writer::{WriteOptions, Writer};

const LENGTH: i64 = 1_000_000;

/// Two digital channels, A with three edges, B with none, in one block.
fn base() -> Vec<u8> {
    let channel = |name: &str| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        data: DeclaredKind::Digital { initial: false },
    };
    let declaration = Declaration {
        timebase: Ratio::integer(1_000_000_000).unwrap(),
        channels: vec![channel("A"), channel("B")],
        source: SourceHeader::new(SourceFormat::Other("crafted".into())),
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 12,
    };
    let mut w = Writer::new(declaration, WriteOptions::default()).unwrap();
    for t in [10, 20, 30] {
        w.edge(0, t).unwrap();
    }
    let mut out = w.take_output();
    out.extend(
        w.finish(LENGTH, &[LENGTH, LENGTH], BTreeMap::new())
            .unwrap()
            .tail,
    );
    out
}

/// `file`'s records before its footer, then a footer of `payload`.
fn with_footer(file: &[u8], footer: &Footer, payload: &[u8]) -> Vec<u8> {
    let keep = footer.trailer.footer_offset as usize;
    let mut out = file[..keep].to_vec();
    let mut tail = Vec::new();
    record::write_footer(&mut tail, keep as u64, payload);
    out.extend(tail);
    out
}

/// `file` with its footer written again after `edit` changes what it says.
fn refooted(
    file: &[u8],
    edit: impl FnOnce(&mut Header, &mut Vec<BlockRow>, &mut Vec<OverviewRow>),
) -> Vec<u8> {
    let footer = Footer::parse_file(file).unwrap();
    let (mut header, mut rows) = (footer.header.clone(), footer.rows.clone());
    let mut overview_rows: Vec<OverviewRow> =
        footer.overview_rows.iter().flatten().copied().collect();
    edit(&mut header, &mut rows, &mut overview_rows);
    let payload =
        record::footer_bytes(&header, &rows, &overview_rows, &footer.overview_frame).unwrap();
    with_footer(file, &footer, &payload)
}

fn frame(raw: &[u8]) -> Vec<u8> {
    let mut c = zstd::bulk::Compressor::new(3).unwrap();
    c.set_parameter(zstd::stream::raw::CParameter::ChecksumFlag(true))
        .unwrap();
    c.compress(raw).unwrap()
}

fn main() {
    let dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("the directory to write into"),
    );
    let file = base();
    let footer = Footer::parse_file(&file).unwrap();
    let write = |name: &str, bytes: Vec<u8>| {
        std::fs::write(dir.join(format!("{name}.tickwave")), bytes).unwrap();
        println!("{name}.tickwave");
    };

    // A header length near 4 GiB: a 32-bit sum of the footer's lengths wraps to a small one.
    let mut payload = record::footer_bytes(
        &footer.header,
        &footer.rows,
        &footer
            .overview_rows
            .iter()
            .flatten()
            .copied()
            .collect::<Vec<_>>(),
        &footer.overview_frame,
    )
    .unwrap();
    payload[..4].copy_from_slice(&0xFFFF_FF00u32.to_le_bytes());
    write("header-wrap", with_footer(&file, &footer, &payload));

    // The same overview level 2,048 times: together more than WASM can hold.
    write(
        "levels",
        refooted(&file, |h, _, _| h.overview = vec![h.overview[0]; 2048]),
    );

    // A block whose row says it decodes to 2 GiB.
    write(
        "big-decoded",
        refooted(&file, |_, rows, _| rows[0].decoded_length = 1 << 31),
    );

    // 2^28 + 1 edges in a block whose section holds three.
    write(
        "huge-count",
        refooted(&file, |h, rows, _| {
            rows[0].channels[0] = ((1 << 28) + 1) | (rows[0].channels[0] & LEVEL_BIT);
            if let escapement_tickwave_format::header::ChannelKind::Digital { edges, .. } =
                &mut h.channels[0].data
            {
                *edges = (1 << 28) + 1;
            }
        }),
    );

    // Section lengths whose 32-bit sum wraps to the block's size: a block of its own, in place of
    // the base's, and the footer pointing at it.
    let mut raw = Vec::new();
    raw.extend_from_slice(&u32::MAX.to_le_bytes());
    raw.extend_from_slice(&3u32.to_le_bytes());
    raw.extend_from_slice(&[0; 2]);
    // In 32 bits, 0xFFFFFFFF + 3 wraps to 2: with the 8-byte table, the 10 bytes the block holds.
    let crafted = frame(&raw);
    let row = &footer.rows[0];
    let head_len = BlockHead::len(2);
    let record_at = row.offset as usize - record::PAYLOAD_AT - head_len;
    let record_end = row.offset as usize + row.length as usize + 8;
    let head = BlockHead::parse(&file[record_at + record::PAYLOAD_AT..], 2)
        .unwrap()
        .0;
    let mut out = file[..record_at].to_vec();
    record::write(
        &mut out,
        Kind::Block,
        &[
            &BlockHead {
                decoded_length: raw.len() as u32,
                words: vec![head.words[0] & LEVEL_BIT, 0],
                ..head
            }
            .to_bytes(),
            &crafted,
        ],
    );
    assert_eq!(
        record_end, footer.trailer.footer_offset as usize,
        "one block, then the footer"
    );
    let mut header = footer.header.clone();
    if let escapement_tickwave_format::header::ChannelKind::Digital { edges, .. } =
        &mut header.channels[0].data
    {
        *edges = 0;
    }
    let rows = vec![BlockRow {
        start: 0,
        offset: (record_at + record::PAYLOAD_AT + head_len) as u64,
        length: crafted.len() as u32,
        decoded_length: raw.len() as u32,
        channels: vec![row.channels[0] & LEVEL_BIT, 0],
    }];
    let payload = record::footer_bytes(&header, &rows, &[], &footer.overview_frame).unwrap();
    let at = out.len() as u64;
    let mut tail = Vec::new();
    record::write_footer(&mut tail, at, &payload);
    out.extend(tail);
    write("sections-wrap", out);
}
