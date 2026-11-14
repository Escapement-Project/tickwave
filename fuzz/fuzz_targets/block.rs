//! `decode_block` and `decode_overview_block` on every block a footer that parses lists, its
//! derived channels, and the importer's `recording::read` with every item it gives.
#![no_main]

use std::sync::Arc;

use escapement_tickwave_format::decode::{self, Footer};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let file = escapement_tickwave_fuzz::file(data);
    if let Ok(footer) = Footer::parse_file(&file) {
        for i in 0..footer.rows.len() {
            let (offset, length) = footer.block_location(i);
            let frame = &file[offset as usize..][..length as usize];
            if let Ok(block) = footer.decode_block(i, frame) {
                for d in 0..footer.products.len() {
                    let _ = footer.derived(&block, d);
                }
            }
            let _ =
                footer.decode_block_with(i, frame, |f, n| decode::inflate_limited(f, n, 1 << 12));
            let _ = footer.decode_block(i, &frame[..frame.len() / 2]);
        }
        for level in 0..footer.overview_rows.len() {
            for k in 0..footer.overview_rows[level].len() {
                let (offset, length) = footer.overview_block_location(level, k);
                let frame = &file[offset as usize..][..length as usize];
                let _ = footer.decode_overview_block(level, k, frame);
            }
            let _ = footer.overview_blocks_covering(level, i64::MIN, i64::MAX);
        }
        let _ = footer.blocks_covering(i64::MIN, i64::MAX);
        for c in 0..footer.header.channels.len() + 1 {
            let _ = footer.sample_axis_tick(c, 0);
            let _ = footer.sample_axis_tick(c, u64::MAX);
        }
    }
    let input: Arc<[u8]> = file.into();
    if let Ok((_, recording)) = escapement_tickwave_import::recording::read(Arc::new(input)) {
        escapement_tickwave_fuzz::drain(recording);
    }
});
