//! How a digital row draws each column (`shade::looks`): lone edges, and busy columns alone,
//! stroked at the opacity the model gives their spacing; edges within a device pixel of each other
//! joined in a run with the columns between them, its band at the run's rate and each column's
//! rails at the share over the columns about it.

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

fn looks(c: &DigitalColumns) -> Vec<Look> {
    let mhz = Ratio::integer(1_000_000).unwrap();
    shade::looks(c, 0..c.state.len(), 2.0, PER, mhz)
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
            (high - share).abs() < 1e-12 && (low - (1.0 - share)).abs() < 1e-12,
            "column {k}: {high} and {low}, the window's share {share}"
        );
    }
    assert_eq!((l[3], l[13]), (Look::Flat, Look::Flat));
}

#[test]
fn a_runs_rails_take_the_floor_where_their_window_holds_an_edge() {
    // Busy columns 4 to 6, two edges each and high a tick each, then low columns: the run is
    // columns 4 to 6; each window's share high is under a hundredth, so its high rail the floor.
    let mut c = cols(20, &[(4, -2.0), (5, -2.0), (6, -2.0)]);
    c.high[4..=6].fill(1.0);
    for (k, x) in looks(&c).into_iter().enumerate().take(7).skip(4) {
        let Look::Shade { high, low, .. } = x else {
            panic!("column {k}: {x:?}")
        };
        assert_eq!(high, shade::FLOOR, "column {k}");
        assert!(low > 0.99, "column {k}: {low}");
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
fn a_busy_column_alone_is_a_stroke_as_a_lone_edge_is() {
    // A pair in one millisecond, as verge's under-range flag gives one, with nothing beside it:
    // full ink.
    let l = looks(&cols(20, &[(10, -2.0)]));
    assert_eq!(l[10], Look::Stroke(1.0));
    // An edge in the middle of column 13, 3 columns from it centre to centre: apart (a gap of 1
    // between their paint), so a stroke at the rate of its 2 edges over the 3 ms, and the edge at
    // its own 1 over them.
    let l = looks(&cols(20, &[(10, -2.0), (13, 0.5)]));
    let want = shade::stroke(shade::band(2.0 / 0.003), 3.0, 2.0);
    assert!(want < 1.0, "{want}");
    assert_eq!(l[10], Look::Stroke(want));
    assert_eq!(
        l[13],
        Look::Stroke(shade::stroke(shade::band(1.0 / 0.003), 3.0, 2.0))
    );
}
