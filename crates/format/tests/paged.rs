//! A reader that reads a table a page at a time (FORMAT.md, "Index"): `TableIndex::visit` and
//! `locate` walk the pages, and `Footer::block_page` and `level_page` turn each into the blocks
//! and rows `block_refs` and `level_refs` give, checked as those check the whole table.

use std::collections::BTreeMap;
use std::ops::ControlFlow;

use escapement_tickwave_format::decode::{BlockRef, DecodeError, Footer, LevelRef, from_memory};
use escapement_tickwave_format::header::{
    AnalogCoding, Declaration, DeclaredChannel, DeclaredKind, IntCoding, SourceFormat, SourceHeader,
};
use escapement_tickwave_format::index::{Located, Page};
use escapement_tickwave_format::model::Ratio;
use escapement_tickwave_format::writer::{Finish, WriteOptions, Writer};

const D: usize = 0;
const A: usize = 1;
const L: usize = 2;
/// D's edges, a block each: 18 pages and a directory.
const EDGES: i64 = 70_000;
/// A's samples, a block each, in runs of 1,000 with a break between: 2 pages.
const SAMPLES: u64 = 5_000;
/// L's edges, one in each overview block of its finest level, at shift 0: 2 pages of rows.
const MARKS: i64 = 4_100;
const LENGTH: i64 = MARKS * 4096 + 4096;

/// D, an edge every 10 ticks; A, a sample every 7 ticks from tick 3, a run of 1,000 after each
/// 2,000-tick pause; L, an edge in each 4,096-tick block of its finest level, each at another
/// place in it, so no two blocks are alike.
fn recording() -> Vec<u8> {
    let timebase = Ratio::integer(1_000_000_000).unwrap();
    let digital = |name: &str, shift| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Digital { initial: false },
        shaping: Default::default(),
        overview_shift: shift,
    };
    let d = Declaration {
        timebase,
        channels: vec![
            digital("D", None),
            DeclaredChannel {
                name: "A".into(),
                source_name: "a".into(),
                source: 0,
                data: DeclaredKind::Analog {
                    unit: "V".into(),
                    period: Ratio::integer(7).unwrap(),
                    coding: AnalogCoding::Int(IntCoding::new(16, false)),
                    significant: None,
                },
                shaping: Default::default(),
                overview_shift: None,
            },
            digital("L", Some(0)),
        ],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(SourceFormat::Other("t".into()), timebase)],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 40,
    };
    let mut w = Writer::new(
        d,
        WriteOptions {
            block_bytes: 1,
            promise_with_items: true,
            level: 1,
            ..WriteOptions::default()
        },
    )
    .unwrap();
    let mut file = w.take_output();
    // Every item in axis order, as the encoder takes them.
    let mut items: Vec<(i64, usize, u64)> = (0..EDGES).map(|k| (10 * k, D, 0)).collect();
    let mut at = 3;
    for i in 0..SAMPLES {
        if i % 1000 == 0 && i > 0 {
            at += 2000;
        }
        items.push((at, A, i));
        at += 7;
    }
    items.extend((0..MARKS).map(|k| (k * 4096 + 1 + (k * 37) % 4000, L, 0)));
    items.sort();
    for (t, c, i) in items {
        match c {
            A => {
                if i % 1000 == 0 {
                    w.run(A, t, 0).unwrap();
                }
                w.sample(A, i % 50_000).unwrap();
            }
            _ => w.edge(c, t).unwrap(),
        }
        file.extend(w.take_output());
    }
    let f = w
        .finish(LENGTH, &[LENGTH, LENGTH], Finish::default())
        .unwrap();
    file.extend(&f.tail);
    file
}

/// Channel `c`'s block table read a page at a time: each page's place and blocks.
fn by_page(footer: &Footer, file: &[u8], c: usize) -> Vec<(Located, Vec<BlockRef>)> {
    let table = footer.channels[c].blocks.as_ref().unwrap();
    let mut out = Vec::new();
    table
        .visit(&mut from_memory(file), |at, page| {
            out.push((at, footer.block_page(c, &at, &page, None)?));
            Ok::<_, DecodeError>(ControlFlow::Continue(()))
        })
        .unwrap();
    out
}

#[test]
fn a_tables_pages_read_one_at_a_time_give_the_blocks_and_rows_the_whole_table_does() {
    let file = recording();
    let mut footer = Footer::parse_file(&file).unwrap();
    footer.load_lists(from_memory(&file)).unwrap();
    for (c, pages) in [(D, 18), (A, 2)] {
        let whole = footer.block_refs(c, from_memory(&file)).unwrap();
        let paged = by_page(&footer, &file, c);
        assert_eq!(paged.len(), pages, "channel {c}");
        // The whole table's blocks, but a page's last has no next word: the next block is on the
        // next page.
        let mut want = whole.clone();
        let mut flat: Vec<BlockRef> = Vec::new();
        for (_, blocks) in &paged {
            flat.extend(blocks);
            want[flat.len() - 1].next_word = None;
        }
        assert_eq!(flat.len(), whole.len(), "channel {c}");
        for (i, (a, b)) in flat.iter().zip(&want).enumerate() {
            assert_eq!(a, b, "channel {c}, block {i}");
        }
    }
    let table = &footer.channels[L].levels[0].1;
    let mut rows: Vec<LevelRef> = Vec::new();
    let mut pages = 0;
    table
        .visit(&mut from_memory(&file), |at, page| {
            rows.extend(footer.level_page(L, 0, &at, &page)?);
            pages += 1;
            Ok::<_, DecodeError>(ControlFlow::Continue(()))
        })
        .unwrap();
    assert_eq!(pages, 2);
    assert_eq!(rows, footer.level_refs(L, 0, from_memory(&file)).unwrap());
}

/// A page whose blocks don't reach where the next page starts, or reach past it, or whose
/// first block's items are counted from elsewhere, is refused, as the whole table would be.
#[test]
fn a_page_that_does_not_meet_the_next_is_refused() {
    let file = recording();
    let mut footer = Footer::parse_file(&file).unwrap();
    footer.load_lists(from_memory(&file)).unwrap();
    let mut pages: Vec<(Located, Page)> = Vec::new();
    footer.channels[A]
        .blocks
        .as_ref()
        .unwrap()
        .visit(&mut from_memory(&file), |at, page| {
            pages.push((at, page));
            Ok::<_, DecodeError>(ControlFlow::Continue(()))
        })
        .unwrap();
    let (at, page) = &pages[0];
    let (first, number) = at.next.unwrap();
    let refused = |at: Located, page: &Page| {
        footer
            .block_page(A, &at, page, None)
            .expect_err("a page that doesn't meet the next")
            .to_string()
    };
    for next in [
        (first, number + 1),
        (first, number - 1),
        (first - 7 * 4096, number),
    ] {
        let err = refused(
            Located {
                next: Some(next),
                ..*at
            },
            page,
        );
        assert!(
            err.contains("doesn't end where the next starts"),
            "{next:?}: {err}"
        );
    }
    // Counted one sample on, with the next page's start moved to match: each block's sample
    // then lies past its ticks.
    let Page::Blocks { number: n, rows } = page else {
        panic!("a block page")
    };
    let shifted = Page::Blocks {
        number: n + 1,
        rows: rows.clone(),
    };
    let err = refused(
        Located {
            next: Some((first, number + 1)),
            ..*at
        },
        &shifted,
    );
    assert!(err.contains("samples lie outside its ticks"), "{err}");
    // L's rows, said to end a block early.
    let mut level: Vec<(Located, Page)> = Vec::new();
    footer.channels[L].levels[0]
        .1
        .visit(&mut from_memory(&file), |at, page| {
            level.push((at, page));
            Ok::<_, DecodeError>(ControlFlow::Continue(()))
        })
        .unwrap();
    let (at, page) = &level[0];
    let (k, place) = at.next.unwrap();
    let err = footer
        .level_page(
            L,
            0,
            &Located {
                next: Some((k - 1, place)),
                ..*at
            },
            page,
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("doesn't stand for its blocks"), "{err}");
    let (at, page) = &level[1];
    assert!(footer.level_page(L, 0, at, page).is_ok());
}
