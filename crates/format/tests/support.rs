//! What each sample covers and where lost samples sit, on the run's grid (FORMAT.md, "Gaps").
//! Expected values are worked by hand.

use std::path::PathBuf;

use escapement_tickwave_format::decode::Footer;
use escapement_tickwave_format::header::ChannelKind;
use escapement_tickwave_format::model::{Break, Ratio, lost_span, run_sample_tick, sample_support};

/// Samples 0 to 4 from tick 0, every 3/2 ticks (ticks 0, 1, 3, 4, 6); two lost; then samples 5
/// to 9 from tick 30 (ticks 30, 31, 33, 34, 36). The web's gaps test has the same channel.
const PERIOD: Ratio = Ratio::new(3, 2).unwrap();
const BREAKS: [Break; 1] = [Break {
    index: 5,
    tick: 30,
    lost: 2,
    phase: 0,
}];

fn supports(breaks: &[Break], count: u64, length: i64) -> Vec<(i64, i64)> {
    (0..count)
        .map(|i| {
            let s = sample_support(0, PERIOD, breaks, count, length, i).unwrap();
            (s.start, s.end)
        })
        .collect()
}

#[test]
fn a_sample_covers_its_place_to_the_next_on_its_runs_grid() {
    // Inside each run, to the next sample's tick. Sample 4's next place is floor(5 × 3/2) = 7,
    // before the run at 30; sample 9's is 30 + floor(5 × 3/2) = 37. At most one period, 1.5,
    // would leave 7 unmeasured after 6 + 1.5, and stop sample 9 at 37.5.
    assert_eq!(
        supports(&BREAKS, 10, 40),
        [
            (0, 1),
            (1, 3),
            (3, 4),
            (4, 6),
            (6, 7),
            (30, 31),
            (31, 33),
            (33, 34),
            (34, 36),
            (36, 37)
        ]
    );
    // Never past the length: at 36, sample 9 covers nothing.
    assert_eq!(supports(&BREAKS, 10, 36)[8..], [(34, 36), (36, 36)]);
    assert_eq!(sample_support(0, PERIOD, &BREAKS, 10, 40, 10), None);
}

#[test]
fn a_runs_last_sample_ends_at_the_next_runs_tick_if_that_comes_sooner() {
    // The run starts again at 6, the tick of sample 4, which then covers nothing.
    let at_six = [Break {
        index: 5,
        tick: 6,
        lost: 0,
        phase: 0,
    }];
    assert_eq!(supports(&at_six, 7, 40)[4..], [(6, 6), (6, 7), (7, 9)]);
    // At 7, exactly its next place.
    let at_seven = [Break {
        index: 5,
        tick: 7,
        lost: 0,
        phase: 0,
    }];
    assert_eq!(supports(&at_seven, 6, 40)[4..], [(6, 7), (7, 8)]);
}

#[test]
fn the_lost_samples_sit_where_the_run_before_the_break_would_have_put_them() {
    // Places 5 and 7 of the first run: floor(7.5) = 7 and floor(10.5) = 10. A period after
    // sample 4, at 6, would start them at 7.5.
    assert_eq!(lost_span(0, PERIOD, &BREAKS, 0), Some(7..10));
    // More lost than fit before the run starts again: the span ends there.
    let short = [Break {
        index: 5,
        tick: 9,
        lost: 4,
        phase: 0,
    }];
    assert_eq!(lost_span(0, PERIOD, &short, 0), Some(7..9));
    // The run starts again before place 5: the lost time is empty, at the break.
    let early = [Break {
        index: 5,
        tick: 6,
        lost: 1,
        phase: 0,
    }];
    assert_eq!(lost_span(0, PERIOD, &early, 0), Some(6..6));
    // A pause loses nothing.
    let pause = [Break {
        index: 5,
        tick: 30,
        lost: 0,
        phase: 0,
    }];
    assert_eq!(lost_span(0, PERIOD, &pause, 0), None);
    assert_eq!(lost_span(0, PERIOD, &BREAKS, 1), None);
    // A second break's lost samples sit on the grid of the run at 30, from sample 5: places 8
    // and 11 are 30 + floor(4.5) = 34 and 30 + floor(9) = 39.
    let two = [
        BREAKS[0],
        Break {
            index: 8,
            tick: 50,
            lost: 3,
            phase: 0,
        },
    ];
    assert_eq!(lost_span(0, PERIOD, &two, 1), Some(34..39));
    // A loss too long for any tick ends at the break, not past it.
    let huge = [Break {
        index: 5,
        tick: 30,
        lost: u64::MAX,
        phase: 0,
    }];
    assert_eq!(
        lost_span(0, Ratio::new(1 << 52, 1).unwrap(), &huge, 0),
        Some(30..30)
    );
}

#[test]
fn every_tick_of_a_run_is_covered_once() {
    // Each run's supports run end to end from its first tick, and each sample's is its tick to
    // the next sample's inside the run, or a step of floor or ceil of the period at its end.
    let periods = [(1, 1), (3, 2), (1, 3), (7, 1), (22, 7), (1_000_001, 1_000)];
    for (num, den) in periods {
        let period = Ratio::new(num, den).unwrap();
        let breaks = [
            Break {
                index: 17,
                tick: 9_000,
                lost: 3,
                phase: 0,
            },
            Break {
                index: 40,
                tick: 60_000,
                lost: 0,
                phase: 0,
            },
        ];
        let count = 64;
        let length = 120_000;
        let tick = |i| run_sample_tick(5, period, &breaks, i);
        let (lo, hi) = (num / den, num.div_ceil(den));
        for i in 0..count {
            let s = sample_support(5, period, &breaks, count, length, i).unwrap();
            assert_eq!(s.start, tick(i), "{num}/{den}, sample {i}");
            match breaks.iter().find(|b| b.index == i + 1) {
                None if i + 1 < count => assert_eq!(s.end, tick(i + 1), "{num}/{den}, {i}"),
                next => {
                    let step = (s.end - s.start) as u64;
                    let capped = next.is_some_and(|b| s.end == b.tick) || s.end == length;
                    assert!(
                        capped || (lo..=hi).contains(&step),
                        "{num}/{den}, {i}: {s:?}"
                    );
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
    let ChannelKind::Analog {
        first,
        period,
        count,
        ..
    } = &footer.header.channels[0].data
    else {
        panic!("V is analog");
    };
    // 1,000 pairs every 960 ticks from 0, 50 lost, the next run at 1,008,000, as the web's energy
    // test has it; the pause at 2,000,000 loses nothing.
    assert_eq!(
        lost_span(*first, *period, breaks, 0),
        Some(960_000..1_008_000)
    );
    assert_eq!(lost_span(*first, *period, breaks, 1), None);
    let support = |i| sample_support(*first, *period, breaks, *count, footer.header.length, i);
    assert_eq!(support(999), Some(959_040..960_000));
    assert_eq!(support(count - 1), Some(2_767_040..2_768_000));
}
