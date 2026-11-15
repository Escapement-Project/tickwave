//! Writes the recordings in web/fixtures/crafted/: each a real file with one thing in its footer
//! or an index page changed to break a reader that trusts what a file states, the changed
//! record's check made again. Run from the repository:
//!
//!     cargo run -p escapement-tickwave-format --example craft -- web/fixtures/crafted

use std::collections::BTreeMap;
use std::path::PathBuf;

use escapement_tickwave_format::decode::Footer;
use escapement_tickwave_format::header::{
    BlockRow, ChannelKind, Declaration, DeclaredChannel, DeclaredKind, Header, LEVEL_BIT,
    OverviewLevel, SourceFormat, SourceHeader,
};
use escapement_tickwave_format::index::PAGE_HEAD;
use escapement_tickwave_format::model::Ratio;
use escapement_tickwave_format::record::{self, Kind};
use escapement_tickwave_format::writer::{Finish, WriteOptions, Writer};

const LENGTH: i64 = 1_000_000;

/// Two digital channels, A with three edges, B with none, a block each.
fn base() -> Vec<u8> {
    let channel = |name: &str| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Digital { initial: false },
        shaping: Default::default(),
        overview_shift: None,
    };
    let timebase = Ratio::integer(1_000_000_000).unwrap();
    let declaration = Declaration {
        timebase,
        channels: vec![channel("A"), channel("B")],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(
            SourceFormat::Other("crafted".into()),
            timebase,
        )],
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
        w.finish(LENGTH, &[LENGTH, LENGTH], Finish::default())
            .unwrap()
            .tail,
    );
    out
}

/// The footer record's payload in `file`.
fn footer_payload(file: &[u8], footer: &Footer) -> Vec<u8> {
    let at = footer.trailer.footer_offset as usize + record::PAYLOAD_AT;
    let len = footer.trailer.footer_len as usize - record::FRAMING;
    file[at..at + len].to_vec()
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

/// `file` with its footer's header written again after `edit` changes it.
fn reheadered(file: &[u8], edit: impl FnOnce(&mut Header)) -> Vec<u8> {
    let footer = Footer::parse_file(file).unwrap();
    let payload = footer_payload(file, &footer);
    let header_len = u32::from_le_bytes(payload[..4].try_into().unwrap()) as usize;
    let mut header = footer.header.clone();
    edit(&mut header);
    let json = serde_json::to_vec(&header).unwrap();
    let mut out = Vec::new();
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(&payload[4..8]);
    out.extend_from_slice(&json);
    out.extend_from_slice(&payload[8 + header_len..]);
    with_footer(file, &footer, &out)
}

/// `file` with channel A's index page's bytes changed by `edit`, its check made again.
fn page_edited(file: &[u8], edit: impl FnOnce(&mut [u8])) -> Vec<u8> {
    let footer = Footer::parse_file(file).unwrap();
    let entry = footer.channels[0].blocks.as_ref().unwrap().levels[0][0];
    let at = entry.offset as usize;
    let mut payload = file[at + record::PAYLOAD_AT..at + entry.length as usize - 8].to_vec();
    edit(&mut payload);
    let mut fixed = Vec::new();
    record::write(&mut fixed, Kind::IndexPage, &[&payload]);
    let mut out = file.to_vec();
    out[at..at + fixed.len()].copy_from_slice(&fixed);
    out
}

/// `file` with channel A's first block row changed by `edit`.
fn row_edited(file: &[u8], edit: impl FnOnce(&mut BlockRow)) -> Vec<u8> {
    page_edited(file, |payload| {
        let row_at = PAGE_HEAD..PAGE_HEAD + BlockRow::SIZE;
        let mut row = BlockRow::parse(&payload[row_at.clone()]);
        edit(&mut row);
        let mut bytes = Vec::new();
        row.write(&mut bytes);
        payload[row_at].copy_from_slice(&bytes);
    })
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
    let mut payload = footer_payload(&file, &footer);
    payload[..4].copy_from_slice(&0xFFFF_FF00u32.to_le_bytes());
    write("header-wrap", with_footer(&file, &footer, &payload));

    // The same overview level 2,048 times, a million blocks each.
    write(
        "levels",
        reheadered(&file, |h| {
            h.channels[0].overview_blocks = vec![
                OverviewLevel {
                    shift: 12,
                    blocks: 1 << 20,
                    rows: 1 << 20,
                };
                2048
            ]
        }),
    );

    // A block whose row says it decodes to 2 GiB.
    write(
        "big-decoded",
        row_edited(&file, |row| row.decoded_length = 1 << 31),
    );

    // 2^28 + 1 edges in a block whose section holds three.
    write(
        "huge-count",
        reheadered(
            &row_edited(&file, |row| {
                row.word = ((1 << 28) + 1) | (row.word & LEVEL_BIT)
            }),
            |h| {
                if let ChannelKind::Digital { edges, .. } = &mut h.channels[0].data {
                    *edges = (1 << 28) + 1;
                }
            },
        ),
    );

    // A page whose head says it holds 2^32 - 1 rows, where its one row is all it holds.
    write(
        "page-count",
        page_edited(&file, |payload| {
            payload[15..19].copy_from_slice(&u32::MAX.to_le_bytes())
        }),
    );
}
