//! Which sample sits at or around a tick, across a break. The channel and its expected values are
//! web/test/gaps.test.ts's, worked by hand there; the end of the channel's data is added here.

use escapement_tickwave_format::model::{Break, Ratio};
use escapement_tickwave_view::runs::{Runs, index_at_or_after, sample_at_or_before};

/// Samples 0 to 4 from tick 0, every 3/2 ticks (ticks 0, 1, 3, 4, 6); two lost; then samples 5
/// to 9 from tick 30 (ticks 30, 31, 33, 34, 36). Its data ends at 40.
const BREAKS: [Break; 1] = [Break {
    index: 5,
    tick: 30,
    lost: 2,
}];
const W: Runs = Runs {
    first: 0,
    period: Ratio::new(3, 2).unwrap(),
    count: 10,
    breaks: &BREAKS,
};

#[test]
fn a_samples_tick_follows_the_run_that_holds_it() {
    let ticks: Vec<i64> = (0..10).map(|i| W.tick(i)).collect();
    assert_eq!(ticks, [0, 1, 3, 4, 6, 30, 31, 33, 34, 36]);
}

#[test]
fn the_first_sample_at_or_after_a_tick_across_a_break() {
    // Nothing of the first run lies at or after 7, so the second run's first sample.
    let at = [6.0, 7.0, 29.0, 30.0, 31.0, 32.0, 36.0, 37.0].map(|t| index_at_or_after(&W, t));
    assert_eq!(at, [4, 5, 5, 5, 6, 7, 9, 10]);
    // The value a cursor reads: the last sample at or before it.
    let before = [6.0, 29.0, 30.0, 35.0].map(|t| sample_at_or_before(&W, 40, t));
    assert_eq!(before, [4, 4, 5, 8].map(Some));
}

#[test]
fn a_cursor_reads_nothing_before_the_first_sample_or_from_the_end_of_the_data() {
    assert_eq!(sample_at_or_before(&W, 40, 39.5), Some(9));
    assert_eq!(sample_at_or_before(&W, 40, 40.0), None);
    assert_eq!(sample_at_or_before(&W, 40, -0.5), None);
    let plain = Runs {
        first: 0,
        period: Ratio::new(1, 1).unwrap(),
        count: 100,
        breaks: &[],
    };
    assert_eq!(sample_at_or_before(&plain, 100, 99.0), Some(99));
    assert_eq!(sample_at_or_before(&plain, 100, 100.0), None);
    assert_eq!(
        sample_at_or_before(&Runs { count: 0, ..plain }, 100, 5.0),
        None
    );
}
