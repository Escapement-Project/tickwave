//! `decode_block` and `decode_overview` on every block and overview row the index of a footer that
//! parses lists, its derived channels and strips, and the importer's `recording::read` with every
//! item it gives.
#![no_main]

use std::sync::Arc;

use escapement_tickwave_format::decode::{self, BlockRef, Footer, derived_values, from_memory};
use libfuzzer_sys::fuzz_target;

/// The frame at `offset`, sliced without a check: the index holds every frame before the page
/// that lists it, so a frame past the file's end is a reader's bug, and panics here.
fn frame(file: &[u8], offset: u64, length: u32) -> &[u8] {
    &file[offset as usize..][..length as usize]
}

fuzz_target!(|data: &[u8]| {
    let file = escapement_tickwave_fuzz::file(data);
    if let Ok(mut footer) = Footer::parse_file(&file) {
        // The gaps and breaks `decode_block` checks a block against.
        let _ = footer.load_lists(from_memory(&file));
        let refs: Vec<Vec<BlockRef>> = (0..footer.header.channels.len())
            .map(|c| footer.block_refs(c, from_memory(&file)).unwrap_or_default())
            .collect();
        for (c, refs) in refs.iter().enumerate() {
            for b in refs {
                let frame = frame(&file, b.row.offset, b.row.length);
                let _ = footer.decode_block(c, b, frame);
                let _ = footer
                    .decode_block_with(c, b, frame, |f, n| decode::inflate_limited(f, n, 1 << 12));
                let _ = footer.decode_block(c, b, &frame[..frame.len() / 2]);
            }
            let _ = Footer::blocks_covering(refs, i64::MIN, i64::MAX);
        }
        // Each derived channel from its operands' blocks at the same place in their channels.
        for &(a, b, product) in footer.products.iter().flatten() {
            let (Some(ra), Some(rb)) = (refs.get(a), refs.get(b)) else {
                continue;
            };
            for (x, y) in ra.iter().zip(rb) {
                let x = footer.decode_block(a, x, frame(&file, x.row.offset, x.row.length));
                let y = footer.decode_block(b, y, frame(&file, y.row.offset, y.row.length));
                if let (Ok(x), Ok(y)) = (x, y) {
                    let _ = derived_values(&x.data, &y.data, product);
                }
            }
        }
        for o in 0..footer.overview_channels() {
            let _ = footer.strip(o);
            for li in 0..footer.channels[o].levels.len() {
                let Ok(rows) = footer.level_refs(o, li, from_memory(&file)) else {
                    continue;
                };
                for r in &rows {
                    let _ =
                        footer.decode_overview(o, li, r, frame(&file, r.row.offset, r.row.length));
                }
                let shift = footer.channels[o].levels[li].0.shift;
                let _ = Footer::rows_covering(&rows, shift, i64::MIN, i64::MAX);
            }
        }
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
