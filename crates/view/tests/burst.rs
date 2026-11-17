//! A long idle recording with one dense burst (web/fixtures/burst.tickwave, as
//! crates/format/examples/burst.rs writes it): a window past the channel's finest overview level
//! whose columns would read more than the bound's edges draws that level's buckets, and decodes
//! none of the burst; once the window narrows enough, its columns read the edges.

use std::ops::Range;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use escapement_tickwave_format::decode::{DecodeError, Footer};
use escapement_tickwave_view::columns::State;
use escapement_tickwave_view::{Bytes, Cache, Drawn, Query, Recording};

mod common;

/// A file in memory that records each read.
struct Counted {
    bytes: Vec<u8>,
    reads: Mutex<Vec<Range<u64>>>,
}

impl Bytes for Counted {
    fn size(&self) -> std::io::Result<u64> {
        Ok(self.bytes.len() as u64)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        self.reads
            .lock()
            .unwrap()
            .push(offset..offset + buf.len() as u64);
        let at = offset as usize;
        buf.copy_from_slice(&self.bytes[at..at + buf.len()]);
        Ok(())
    }
}

/// The burst's first edge: 30 minutes and a tick. It runs 40 ms.
const AT: i64 = 1_800_000_000_001;

#[test]
fn a_window_reads_the_bursts_edges_only_once_its_columns_hold_few_enough() {
    let file = common::fixture("burst.tickwave");
    // The blocks of D that hold the burst's edges.
    let blocks: Vec<u64> = Footer::parse_file(&file)
        .unwrap()
        .block_refs(0, |at, len| {
            Ok::<_, DecodeError>(file[at as usize..][..len as usize].to_vec())
        })
        .unwrap()
        .iter()
        .filter(|b| b.start < AT + 40_000_000 && b.end > AT)
        .map(|b| b.row.offset)
        .collect();
    assert!(blocks.len() >= 19, "{} blocks", blocks.len());
    let bytes = Arc::new(Counted {
        bytes: file,
        reads: Mutex::default(),
    });
    let rec = Recording::open(bytes.clone(), Cache::new(256 << 20)).unwrap();
    let cancel = AtomicBool::new(false);
    let decoded = |window: Range<i64>| {
        bytes.reads.lock().unwrap().clear();
        let mut q = Query::new(window, 2_744);
        q.channels = vec![0];
        let a = rec.query(&q, &cancel).unwrap();
        let Drawn::Digital { columns, .. } = &a.channels[0] else {
            panic!("{:?}", a.channels[0]);
        };
        let busy = columns.state.iter().filter(|&&s| s == State::Busy).count();
        // Read from the edges, a column's edges are whole and sum to those over the columns;
        // from the buckets, a share of each bucket's.
        let edges: f64 = columns.edges.iter().sum();
        let reads = bytes.reads.lock().unwrap();
        let n = blocks
            .iter()
            .filter(|&&o| reads.iter().any(|r| r.start == o))
            .count();
        (n, busy, edges)
    };
    // 20 s about the burst: a column of 7.3 ms, finer than the finest level's 2^23 ticks, and
    // columns over 60 s holding all 20 million edges. Only the two blocks at the columns' ends,
    // which the count from the index reads, are decoded; the burst's buckets drawn.
    let (n, busy, _) = decoded(AT - 10_000_000_000..AT + 10_000_000_000);
    assert!(n <= 2, "{n} of {} blocks decoded", blocks.len());
    assert!(busy > 0);
    // Half a millisecond in the middle of the burst, away from the blocks the count read: its
    // columns hold 750,000 edges, under the bound, and read them.
    let (n, busy, edges) = decoded(AT + 20_000_000..AT + 20_500_000);
    assert!(n > 0);
    assert!(busy > 0);
    assert_eq!(edges, 750_000.0);
}
