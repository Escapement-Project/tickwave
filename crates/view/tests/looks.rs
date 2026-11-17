//! How a digital row draws each column (`shade::looks`): lone edges stroked, and busy columns alone
//! and short runs drawn as one stroke at their centre, each at the opacity the model gives its
//! spacing, pulses beside pulses at full ink; edges within a device pixel of each other joined in a
//! run with the columns between them, its band at the run's rate and each column's rails a part of
//! the band's opacity by the share over the columns about it; and a level held briefly between two
//! runs at the larger of their bands.

use escapement_tickwave_format::model::Ratio;
use escapement_tickwave_view::columns::{DigitalColumns, State};
use escapement_tickwave_view::shade::{self, Look};

/// A grid of 1,000 ticks a device column on an axis of 1,000,000 ticks a second: a column is a
/// millisecond.
const PER: f64 = 1000.0;

/// Columns low but where `edges` puts an edge, at `(column, at)`, or a busy column of `n` edges
/// at `(column, -n)`: the level flips at each edge, and a busy column's time is half high.
fn cols(count: usize, edges: &[(usize, f64)]) -> DigitalColumns {
    let mut c = DigitalColumns {
        state: vec![State::Low; count],
        at: vec![0.0; count],
        edges: vec![0.0; count],
        high: vec![0.0; count],
        known: vec![PER; count],
    };
    let mut level = false;
    let mut next = edges.iter().peekable();
    for k in 0..count {
        match next.peek() {
            Some(&&(e, at)) if e == k && at < 0.0 => {
                c.state[k] = State::Busy;
                c.edges[k] = -at;
                c.high[k] = PER / 2.0;
                level ^= (-at) as usize % 2 == 1;
                next.next();
            }
            Some(&&(e, at)) if e == k => {
                c.state[k] = if level { State::Fall } else { State::Rise };
                c.at[k] = at;
                c.edges[k] = 1.0;
                c.high[k] = if level { at * PER } else { (1.0 - at) * PER };
                level = !level;
                next.next();
            }
            _ => {
                c.state[k] = if level { State::High } else { State::Low };
                c.high[k] = if level { PER } else { 0.0 };
            }
        }
    }
    c
}

/// Columns from edges at `xs`, in columns from the row's first, rising first: a column with one
/// edge rises or falls at it, one with more is busy, and each column's time high is the edges'.
fn edges_at(count: usize, xs: &[f64]) -> DigitalColumns {
    let mut c = DigitalColumns {
        state: vec![State::Low; count],
        at: vec![0.0; count],
        edges: vec![0.0; count],
        high: vec![0.0; count],
        known: vec![PER; count],
    };
    let mut level = false;
    let mut xs = xs.iter().copied().peekable();
    for k in 0..count {
        let before = level;
        let (mut high, mut from, mut n) = (0.0, 0.0, 0);
        while let Some(x) = xs.next_if(|&x| x < (k + 1) as f64) {
            let x = x - k as f64;
            if level {
                high += x - from;
            }
            (from, level, n) = (x, !level, n + 1);
            c.at[k] = x;
        }
        if level {
            high += 1.0 - from;
        }
        c.high[k] = high * PER;
        c.edges[k] = n as f64;
        c.state[k] = match (n, before) {
            (0, true) => State::High,
            (0, false) => State::Low,
            (1, true) => State::Fall,
            (1, false) => State::Rise,
            _ => State::Busy,
        };
    }
    c
}

fn looks(c: &DigitalColumns) -> Vec<Look> {
    looks_at(c, 2.0)
}

fn looks_at(c: &DigitalColumns, width: f64) -> Vec<Look> {
    let mhz = Ratio::integer(1_000_000).unwrap();
    shade::looks(c, 0..c.state.len(), width, PER, mhz)
}

#[test]
fn a_lone_edge_with_none_beside_it_is_stroked_at_full_ink() {
    let l = looks(&cols(20, &[(10, 0.5)]));
    assert_eq!(l[10], Look::Stroke(1.0));
    assert!(
        l.iter()
            .enumerate()
            .all(|(k, x)| k == 10 || *x == Look::Flat)
    );
}

#[test]
fn a_lone_edges_stroke_covers_its_stretch_as_the_band_at_its_spacing_would() {
    // Edges 4 columns apart, a line 2 wide: a gap of 2 between strokes, so none joins. An edge
    // every 4 ms is 250 a second.
    let l = looks(&cols(30, &[(6, 0.5), (10, 0.5), (14, 0.5)]));
    let want = shade::stroke(shade::band(250.0), 4.0, 2.0);
    assert!(want < 1.0, "{want}");
    for k in [6, 10, 14] {
        assert_eq!(l[k], Look::Stroke(want), "column {k}");
    }
    // An edge 4 columns from one neighbour and 12 from the other takes the nearer.
    let l = looks(&cols(40, &[(6, 0.5), (10, 0.5), (22, 0.5)]));
    assert_eq!(l[10], Look::Stroke(want));
    assert_eq!(l[22], Look::Stroke(1.0), "12 columns from its neighbour");
}

#[test]
fn a_run_takes_its_rate_and_each_columns_rails_the_share_over_the_columns_about_it() {
    // Edges 2 columns apart: strokes 2 wide leave no gap. Five edges over columns 4 to 12, nine
    // columns: 5 edges in 9 ms. Columns 4 to 12 are high 500, 1000, 500, 0, 500, 1000, 500, 0 and
    // 500 ticks of their 1,000; each column's rails take the two either side within the run.
    let l = looks(&cols(
        20,
        &[(4, 0.5), (6, 0.5), (8, 0.5), (10, 0.5), (12, 0.5)],
    ));
    let band = shade::band(5.0 / 0.009);
    // Each rail takes the band's opacity times its level's share over the larger share.
    let rail = |share: f64| band * (share / share.max(1.0 - share)).max(shade::RAIL_FLOOR);
    let shares = [
        2000.0 / 3000.0, // 4: columns 4 to 6
        0.5,             // 5: 4 to 7, 2,000 of 4,000
        0.5,             // 6: 4 to 8, 2,500 of 5,000
        0.6,             // 7: 5 to 9, 3,000 of 5,000
        0.5,
        0.4, // 9: 7 to 11, 2,000 of 5,000
        0.5,
        0.5,             // 11: 9 to 12, 2,000 of 4,000
        1000.0 / 3000.0, // 12: 10 to 12
    ];
    for (k, share) in (4..=12).zip(shares) {
        let Look::Shade { band: b, high, low } = l[k] else {
            panic!("column {k}: {:?}", l[k])
        };
        assert_eq!(b, band, "column {k}");
        assert!(
            (high - rail(share)).abs() < 1e-12 && (low - rail(1.0 - share)).abs() < 1e-12,
            "column {k}: {high} and {low}, the window's share {share}"
        );
    }
    assert_eq!((l[3], l[13]), (Look::Flat, Look::Flat));
}

#[test]
fn a_runs_rails_take_the_floor_of_the_band_where_their_window_holds_an_edge() {
    // Busy columns 4 to 8, two edges each and high a tick each, then low columns: the run is
    // columns 4 to 8, its first and last 4 apart, past the join distance of 3; each window's share
    // high is under a hundredth, so its high rail takes the floor's part of the band, the low rail
    // the band whole.
    let mut c = cols(20, &[(4, -2.0), (5, -2.0), (6, -2.0), (7, -2.0), (8, -2.0)]);
    c.high[4..=8].fill(1.0);
    let band = shade::band(10.0 / 0.005);
    for (k, x) in looks(&c).into_iter().enumerate().take(9).skip(4) {
        let Look::Shade { band: b, high, low } = x else {
            panic!("column {k}: {x:?}")
        };
        assert_eq!(b, band, "column {k}");
        assert_eq!(high, band * shade::RAIL_FLOOR, "column {k}");
        assert_eq!(low, band, "column {k}");
    }
}

#[test]
fn a_column_with_no_data_ends_a_run() {
    // Edges 1.2 columns apart, centre to centre, either side of a column with no data: close
    // enough to join, but the line stops and starts again there, so each is a lone edge.
    let mut c = cols(20, &[(4, 0.9), (6, 0.1)]);
    c.state[5] = State::None;
    let l = looks(&c);
    let want = shade::stroke(shade::band(1.0 / 0.0012), 1.2, 2.0);
    for k in [4, 6] {
        assert!(
            matches!(l[k], Look::Stroke(a) if (a - want).abs() < 1e-12),
            "column {k}: {:?}, a stroke at {want}",
            l[k]
        );
    }
}

#[test]
fn a_busy_column_alone_is_a_pulse_at_its_middle_stroked_as_a_lone_edge_is() {
    // A pair in one millisecond, as verge's under-range flag gives one, with nothing beside it:
    // full ink.
    let l = looks(&cols(20, &[(10, -2.0)]));
    assert_eq!(
        l[10],
        Look::Pulse {
            alpha: 1.0,
            at: 10.5
        }
    );
    // An edge in the middle of column 13, 3 columns from it centre to centre: apart (a gap of 1
    // between their paint), so a pulse at the rate of its 2 edges over the 3 ms, and the edge a
    // stroke at its own 1 over them.
    let l = looks(&cols(20, &[(10, -2.0), (13, 0.5)]));
    let want = shade::stroke(shade::band(2.0 / 0.003), 3.0, 2.0);
    assert!(want < 1.0, "{want}");
    assert_eq!(
        l[10],
        Look::Pulse {
            alpha: want,
            at: 10.5
        }
    );
    assert_eq!(
        l[13],
        Look::Stroke(shade::stroke(shade::band(1.0 / 0.003), 3.0, 2.0))
    );
}

#[test]
fn a_pulse_reads_alike_wherever_the_column_grid_falls() {
    // A pulse of two edges 0.6 columns apart, alone: inside one column it is a busy column alone;
    // across a column boundary, a rise and a fall in neighbouring columns, which join. Both are one
    // full-ink pulse at the pulse's middle, every column of it holding it.
    let inside = looks(&cols(20, &[(10, -2.0)]));
    let across = looks(&cols(20, &[(10, 0.7), (11, 0.3)]));
    assert_eq!(
        inside[10],
        Look::Pulse {
            alpha: 1.0,
            at: 10.5
        }
    );
    for k in [10, 11] {
        assert_eq!(
            across[k],
            Look::Pulse {
                alpha: 1.0,
                at: 11.0
            },
            "column {k}"
        );
    }
    for l in [&inside, &across] {
        assert!(
            l.iter()
                .enumerate()
                .all(|(k, x)| (10..=11).contains(&k) || *x == Look::Flat)
        );
    }
}

#[test]
fn a_run_shorter_than_the_join_distance_is_a_pulse_and_a_longer_one_is_shaded() {
    // A line 2 wide joins edges under 3 columns apart. Three edges 1.5 and 1.4 columns apart span
    // 2.9 columns, first to last: under the join distance, so any two of them would join alone, and
    // the run is a pulse at its middle, at full ink with nothing beside it. Three 1.5 and 1.6 apart
    // span 3.1: the band and rails of a run.
    let short = looks(&cols(20, &[(8, 0.5), (10, 0.0), (11, 0.4)]));
    for (look, k) in short[8..=11].iter().zip(8..) {
        assert_eq!(
            *look,
            Look::Pulse {
                alpha: 1.0,
                at: (8.5 + 11.4) / 2.0
            },
            "column {k}"
        );
    }
    let long = looks(&cols(20, &[(8, 0.5), (10, 0.0), (11, 0.6)]));
    for (look, k) in long[8..=11].iter().zip(8..) {
        assert!(matches!(look, Look::Shade { .. }), "column {k}: {look:?}");
    }
}

#[test]
fn a_pulse_with_a_neighbour_near_takes_the_stroke_its_spacing_gives() {
    // A pulse across columns 10 and 11, its middle at 11.0, and a lone edge at 15.5: 4.5 apart, a
    // gap of 2.5 between their paint. The pulse's 2 edges over the 4.5 ms to it, the edge's 1.
    let l = looks(&cols(30, &[(10, 0.7), (11, 0.3), (15, 0.5)]));
    let want = shade::stroke(shade::band(2.0 / 0.0045), 4.5, 2.0);
    assert!(want < 1.0, "{want}");
    for k in [10, 11] {
        assert_eq!(
            l[k],
            Look::Pulse {
                alpha: want,
                at: 11.0
            },
            "column {k}"
        );
    }
    assert_eq!(
        l[15],
        Look::Stroke(shade::stroke(shade::band(1.0 / 0.0045), 4.5, 2.0))
    );
}

#[test]
fn pulses_near_each_other_each_take_full_ink_at_every_spacing() {
    // Two flags, each a pair of edges 0.2 columns apart, from 0.5 to 30 columns apart and at every
    // tenth of a column against the grid. Wherever they draw as two pulses, a pulse's spacing
    // counts no pulse beside it, so both are full ink; from 3.2 to about 6 columns apart the
    // spacing would give less.
    let mut two = 0;
    for s in (5..=300).map(|s| f64::from(s) / 10.0) {
        for x in (100..110).map(|o| f64::from(o) / 10.0) {
            let l = looks(&edges_at(60, &[x, x + 0.2, x + s, x + s + 0.2]));
            let mut places = Vec::new();
            for (k, look) in l.iter().enumerate() {
                if let Look::Pulse { alpha, at } = *look {
                    assert_eq!(alpha, 1.0, "flags {s} apart from {x}: column {k}, {look:?}");
                    if !places.contains(&at) {
                        places.push(at);
                    }
                }
            }
            two += usize::from(places.len() == 2);
        }
    }
    assert!(two > 2600, "{two} of 2,960 rows two pulses");
}

#[test]
fn a_short_pause_between_two_runs_keeps_its_gap_and_takes_the_larger_band() {
    // A line 3 wide, the default weight at two device pixels a CSS pixel: edges under 4 apart
    // join. A run of three edges 3 apart, then a busier one of seven edges 1 apart; the first's
    // last edge at 8.5, its paint to 10; the second's first at 15.4 or 15.6, its paint from 13.9 or
    // 14.1: a bare 3.9 or 4.1 between, columns 9 to 14 high.
    let row = |b: f64| {
        let mut xs = vec![2.5, 5.5, 8.5];
        xs.extend((0..7).map(|j| 15.0 + b + f64::from(j)));
        edges_at(30, &xs)
    };
    let (first, second) = (shade::band(3.0 / 0.007), shade::band(7.0 / 0.007));
    assert!(second > first, "{first} {second}");
    let near = looks_at(&row(0.4), 3.0);
    for (look, k) in near[9..=14].iter().zip(9..) {
        assert_eq!(*look, Look::Held(second), "3.9 between: column {k}");
    }
    let far = looks_at(&row(0.6), 3.0);
    for (look, k) in far[9..=14].iter().zip(9..) {
        assert_eq!(*look, Look::Flat, "4.1 between: column {k}");
    }
    // Two runs either way, each at its own band.
    for l in [&near, &far] {
        assert!(
            matches!(l[8], Look::Shade { band, .. } if band == first),
            "{:?}",
            l[8]
        );
        assert!(
            matches!(l[15], Look::Shade { band, .. } if band == second),
            "{:?}",
            l[15]
        );
    }
}

#[test]
fn no_rail_in_a_run_is_darker_than_its_band() {
    // At line widths of 1 to 8 device pixels, edges 1 to 9 columns apart: wherever a column
    // shades, both its rails are at most its band's opacity, a level held inside a run, with no
    // edge in its window, included.
    let mut held = 0;
    for width in 1..=8 {
        for apart in 1..=9 {
            let xs: Vec<f64> = (0..8).map(|i| 2.5 + f64::from(apart * i)).collect();
            let l = looks_at(&edges_at(80, &xs), f64::from(width));
            for (k, look) in l.iter().enumerate() {
                if let Look::Shade { band, high, low } = *look {
                    assert!(
                        high <= band + 1e-12 && low <= band + 1e-12,
                        "width {width}, edges {apart} apart: column {k}, {look:?}"
                    );
                    held += usize::from(high == 0.0 || low == 0.0);
                }
            }
        }
    }
    assert!(held > 0, "no column held a level inside a run");
}
