//! What each sample covers and where lost samples sit, on the run's grid (FORMAT.md, "Gaps"),
//! exactly, in `1/d` source ticks. Expected values are worked by hand.

use std::path::PathBuf;

use escapement_tickwave_format::decode::Footer;
use escapement_tickwave_format::header::ChannelKind;
use escapement_tickwave_format::model::{Break, Grid, Ratio};

/// Samples 0 to 4 from tick 0, every 3/2 ticks (places 0, 1.5, 3, 4.5, 6); two lost; then samples
/// 5 to 9 from tick 30 (30, 31.5, 33, 34.5, 36). The grid's `d` is 2: half ticks.
fn grid() -> Grid {
    Grid::new(Ratio::new(3, 2).unwrap(), None).unwrap()
}

const BREAKS: [Break; 1] = [Break {
    index: 5,
    tick: 30,
    lost: 2,
    phase: 0,
}];

/// Each sample's time in half ticks, cut at `length` ticks.
fn supports(breaks: &[Break], count: u64, length: i64) -> Vec<(i128, i128)> {
    let cap = i128::from(length) * 2;
    (0..count)
        .map(|i| {
            let s = grid().sample_time(0, breaks, count, i, Some(cap)).unwrap();
            (s.start, s.end)
        })
        .collect()
}

#[test]
fn a_sample_covers_its_place_to_the_next_on_its_runs_grid() {
    // Inside each run, to the next sample's place; a run's last sample a full period, 3 halves,
    // since the next run starts later.
    assert_eq!(
        supports(&BREAKS, 10, 40),
        [
            (0, 3),
            (3, 6),
            (6, 9),
            (9, 12),
            (12, 15),
            (60, 63),
            (63, 66),
            (66, 69),
            (69, 72),
            (72, 75)
        ]
    );
    // Never past the cap: at 36 ticks, 72 halves, sample 9 covers nothing.
    assert_eq!(supports(&BREAKS, 10, 36)[8..], [(69, 72), (72, 72)]);
    assert_eq!(grid().sample_time(0, &BREAKS, 10, 10, None), None);
    // `sample_tick` is the whole tick a place lies in.
    let ticks: Vec<i64> = (0..10).map(|i| grid().sample_tick(0, &BREAKS, i)).collect();
    assert_eq!(ticks, [0, 1, 3, 4, 6, 30, 31, 33, 34, 36]);
}

#[test]
fn a_runs_last_sample_ends_at_the_next_runs_start_if_that_comes_sooner() {
    // The run starts again at 6, sample 4's place, which then covers nothing.
    let at_six = [Break {
        index: 5,
        tick: 6,
        lost: 0,
        phase: 0,
    }];
    assert_eq!(
        supports(&at_six, 7, 40)[4..],
        [(12, 12), (12, 15), (15, 18)]
    );
    // At 7 and a half, exactly its next place.
    let at_seven_and_a_half = [Break {
        index: 5,
        tick: 7,
        lost: 0,
        phase: 1,
    }];
    assert_eq!(
        supports(&at_seven_and_a_half, 6, 40)[4..],
        [(12, 15), (15, 18)]
    );
}

#[test]
fn the_lost_samples_sit_where_the_run_before_the_break_would_have_put_them() {
    // Places 5 and 7 of the first run: 7.5 and 10.5, 15 and 21 halves.
    assert_eq!(grid().lost_time(0, &BREAKS, 0), Some(15..21));
    // More lost than fit before the run starts again: the span ends there.
    let short = [Break {
        index: 5,
        tick: 9,
        lost: 4,
        phase: 0,
    }];
    assert_eq!(grid().lost_time(0, &short, 0), Some(15..18));
    // The run starts again before place 5: the lost time is empty, at the break.
    let early = [Break {
        index: 5,
        tick: 6,
        lost: 1,
        phase: 0,
    }];
    assert_eq!(grid().lost_time(0, &early, 0), Some(12..12));
    // A pause loses nothing.
    let pause = [Break {
        index: 5,
        tick: 30,
        lost: 0,
        phase: 0,
    }];
    assert_eq!(grid().lost_time(0, &pause, 0), None);
    assert_eq!(grid().lost_time(0, &BREAKS, 1), None);
    // A second break's lost samples sit on the grid of the run at 30, from sample 5: places 8
    // and 11 are 34.5 and 39, 69 and 78 halves.
    let two = [
        BREAKS[0],
        Break {
            index: 8,
            tick: 50,
            lost: 3,
            phase: 0,
        },
    ];
    assert_eq!(grid().lost_time(0, &two, 1), Some(69..78));
    // A loss too long for any tick ends at the break, not past it.
    let huge = [Break {
        index: 5,
        tick: 30,
        lost: u64::MAX,
        phase: 0,
    }];
    let wide = Grid::new(Ratio::new(1 << 52, 1).unwrap(), None).unwrap();
    assert_eq!(wide.lost_time(0, &huge, 0), Some(30..30));
}

#[test]
fn every_place_of_a_run_is_covered_once() {
    // Each run's times run end to end from its start, and each sample's is its place to the next
    // sample's inside the run, or a full period at its end unless the next run or the cap comes
    // sooner.
    let periods = [(1, 1), (3, 2), (1, 3), (7, 1), (22, 7), (1_000_001, 1_000)];
    for (num, den) in periods {
        for phase in [None, Some(Ratio::new(1, 4).unwrap())] {
            let grid = Grid::new(Ratio::new(num, den).unwrap(), phase).unwrap();
            // Each run starts after the one before it ends: 18 and 25 periods on.
            let p = num.div_ceil(den) as i64;
            let breaks = [
                Break {
                    index: 17,
                    tick: 5 + 18 * p,
                    lost: 3,
                    phase: 0,
                },
                Break {
                    index: 40,
                    tick: 5 + 43 * p,
                    lost: 0,
                    phase: 0,
                },
            ];
            let count = 64;
            let cap = i128::from(5 + 70 * p) * i128::from(grid.d());
            let place = |i| grid.sample_place(5, &breaks, i);
            for i in 0..count {
                let s = grid.sample_time(5, &breaks, count, i, Some(cap)).unwrap();
                assert_eq!(s.start, place(i), "{num}/{den}, sample {i}");
                match breaks.iter().find(|b| b.index == i + 1) {
                    None if i + 1 < count => assert_eq!(s.end, place(i + 1), "{num}/{den}, {i}"),
                    next => {
                        let capped = next.is_some_and(|b| s.end == grid.start(*b)) || s.end == cap;
                        assert!(
                            capped || s.end - s.start == grid.step() as i128,
                            "{num}/{den}, {i}: {s:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn the_energy_fixtures_loss_is_the_fifty_pairs_between_its_runs() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/fixtures/energy.tickwave");
    let footer = Footer::parse_file(&std::fs::read(path).unwrap()).unwrap();
    let breaks = footer.breaks(0).unwrap();
    let ChannelKind::Analog { first, count, .. } = &footer.header.channels[0].data else {
        panic!("V is analog");
    };
    let grid = footer.grid(0).unwrap();
    assert_eq!(grid.d(), 1);
    // 1,000 pairs every 960 ticks from 0, 50 lost, the next run at 1,008,000, as the web's energy
    // test has it; the pause at 2,000,000 loses nothing.
    assert_eq!(grid.lost_time(*first, breaks, 0), Some(960_000..1_008_000));
    assert_eq!(grid.lost_time(*first, breaks, 1), None);
    let cap = i128::from(footer.header.length);
    let support = |i| grid.sample_time(*first, breaks, *count, i, Some(cap));
    assert_eq!(support(999), Some(959_040..960_000));
    assert_eq!(support(count - 1), Some(2_767_040..2_768_000));
}
