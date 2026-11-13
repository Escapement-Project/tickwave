//! The web's randomised and property tests, ported rather than shared: columns.test.ts's two
//! seeded sweeps, with the same generator and oracle, so a failure names the same tile and grid
//! in both; and scale.test.ts's range-marks sweep.

use escapement_tickwave_format::model::{Ratio, sample_tick};
use escapement_tickwave_view::columns::{
    Grid, SAMPLES_PER_RUN, SampleRuns, Samples, analog_from_samples, fill_runs, line_samples,
};
use escapement_tickwave_view::runs::Runs;
use escapement_tickwave_view::scale::{ValueRange, range_marks};

/// Uniform in [0, 1) from a fixed seed: columns.test.ts's `seeded`, step for step in i32.
struct Seeded(i32);

impl Seeded {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x6d2b79f5);
        let seed = self.0;
        let mut t = (seed ^ ((seed as u32) >> 15) as i32).wrapping_mul(1 | seed);
        t = t.wrapping_add((t ^ ((t as u32) >> 7) as i32).wrapping_mul(61 | t)) ^ t;
        f64::from((t ^ ((t as u32) >> 14) as i32) as u32) / 4_294_967_296.0
    }

    /// `Math.floor(random() * n)`.
    fn int(&mut self, n: i64) -> i64 {
        (self.next() * n as f64).floor() as i64
    }
}

#[test]
fn the_generator_draws_what_the_webs_does() {
    // The first five draws of `seeded(0x7ac4)` and `seeded(0x51e7)`, printed by Node.
    let cases = [
        (
            0x7ac4,
            [
                0.5731641419697553,
                0.8129943795502186,
                0.6797770960256457,
                0.09095538360998034,
                0.5463480392936617,
            ],
        ),
        (
            0x51e7,
            [
                0.7041027729865164,
                0.4331863212864846,
                0.11549665429629385,
                0.3425481531303376,
                0.22993565583601594,
            ],
        ),
    ];
    for (seed, want) in cases {
        let mut random = Seeded(seed);
        assert_eq!(want.map(|_| random.next()), want, "seed {seed:#x}");
    }
}

const PERIODS: [(u64, u64); 4] = [(1, 1), (3, 2), (1, 3), (7, 1)];

/// Every sample walked, as the rule reads: column c takes the samples whose ticks lie in
/// [x0 + c × per, x0 + (c + 1) × per) and in [from, to), and then meets its neighbours.
fn walked(
    g: &Grid,
    values: &[f64],
    tile_first: u64,
    tick: impl Fn(u64) -> i64,
    from: i64,
    to: i64,
) -> (Vec<f64>, Vec<f64>) {
    let mut min = vec![f64::INFINITY; g.count];
    let mut max = vec![f64::NEG_INFINITY; g.count];
    for (k, &v) in values.iter().enumerate() {
        let t = tick(tile_first + k as u64);
        if t < from || t >= to {
            continue;
        }
        let t = t as f64;
        for c in 0..g.count {
            if t < g.x0 + c as f64 * g.per || t >= g.x0 + (c + 1) as f64 * g.per {
                continue;
            }
            if v < min[c] {
                min[c] = v;
            }
            if v > max[c] {
                max[c] = v;
            }
        }
    }
    for c in 1..g.count {
        if min[c] > max[c] || min[c - 1] > max[c - 1] {
            continue;
        }
        if max[c - 1] < min[c] {
            min[c] = max[c - 1];
        } else if min[c - 1] > max[c] {
            max[c] = min[c - 1];
        }
    }
    (min, max)
}

/// The same bits, so −0 is not +0, as the web's deepStrictEqual has it.
fn bits(xs: &[f64]) -> Vec<u64> {
    xs.iter().map(|x| x.to_bits()).collect()
}

#[test]
fn analog_from_samples_each_column_holds_exactly_the_samples_whose_ticks_lie_in_it() {
    let mut random = Seeded(0x7ac4);
    let mut grids = 0;
    for tile in 0..60 {
        let (num, den) = PERIODS[tile % PERIODS.len()];
        let period = Ratio::new(num, den).unwrap();
        let first = random.int(50);
        let len = random.int(1500) as usize;
        let mut values: Vec<f64> = (0..len)
            .map(|_| {
                if random.next() < 0.03 {
                    f64::NAN
                } else {
                    (random.next() * 1000.0).round()
                }
            })
            .collect();
        // A stretch of NaN longer than a run.
        if tile % 5 == 0 {
            let start = random.int(len as i64) as usize;
            let end = (random.int(len as i64) as usize + 150).min(len);
            for v in values.iter_mut().take(end).skip(start) {
                *v = f64::NAN;
            }
        }
        let tile_first = random.int(300) as u64;
        // The same tile as a source that sends its runs sends it, in f32 as the web's are.
        let n = values.len() / SAMPLES_PER_RUN;
        let (mut run_min, mut run_max) = (vec![0.0; n], vec![0.0; n]);
        fill_runs(&values, &mut run_min, &mut run_max, 0, n);
        let f32s = |xs: &[f64]| xs.iter().map(|&x| f64::from(x as f32)).collect::<Vec<_>>();
        let (sent_min, sent_max) = (f32s(&run_min), f32s(&run_max));
        let built = Samples {
            first: tile_first,
            values: &values,
            runs: None,
        };
        let sent = Samples {
            runs: Some(SampleRuns {
                min: &sent_min,
                max: &sent_max,
            }),
            ..built
        };
        let tick = |i: u64| sample_tick(first, period, i);
        let span = tick(tile_first + len as u64) - tick(tile_first) + 1;
        let runs = Runs {
            first,
            period,
            count: u64::MAX,
            breaks: &[],
        };
        for grid in 0..25 {
            let per = 0.3 + random.next() * span as f64 / 4.0;
            let x0 = tick(tile_first) as f64 - span as f64 / 4.0 + random.next() * span as f64;
            let g = Grid {
                x0,
                per,
                count: 1 + random.int(40) as usize,
            };
            let from = tick(tile_first) - 20 + random.int(span);
            let to = from + random.int(span);
            let (min, max) = walked(&g, &values, tile_first, tick, from, to);
            for (how, s) in [("built", &built), ("sent", &sent)] {
                let got = analog_from_samples(&g, s, &runs, from as f64, to as f64);
                assert_eq!(
                    (bits(&got.min), bits(&got.max)),
                    (bits(&min), bits(&max)),
                    "tile {tile}, grid {grid}, runs {how}"
                );
            }
            grids += 1;
        }
    }
    assert_eq!(grids, 60 * 25);
}

#[test]
fn line_samples_every_sample_that_can_reach_the_grid_and_one_drawn_sample_past_each_side() {
    let mut random = Seeded(0x51e7);
    for tile in 0..60 {
        let (num, den) = PERIODS[tile % PERIODS.len()];
        let period = Ratio::new(num, den).unwrap();
        let first = random.int(50);
        let len = random.int(400) as usize;
        let values: Vec<f64> = (0..len)
            .map(|_| {
                if random.next() < 0.2 {
                    f64::NAN
                } else {
                    random.next()
                }
            })
            .collect();
        let tile_first = random.int(300) as u64;
        let s = Samples {
            first: tile_first,
            values: &values,
            runs: None,
        };
        let tick = |i: u64| sample_tick(first, period, i);
        let span = tick(tile_first + len as u64) - tick(tile_first) + 1;
        let runs = Runs {
            first,
            period,
            count: u64::MAX,
            breaks: &[],
        };
        for grid in 0..25 {
            let x0 = tick(tile_first) as f64 - span as f64 / 4.0 + random.next() * span as f64;
            let per = 0.05 + random.next() * 2.0;
            let g = Grid {
                x0,
                per,
                count: 1 + random.int(40) as usize,
            };
            let pad = if random.next() < 0.3 {
                0.0
            } else {
                random.next() * 30.0
            };
            let from = tick(tile_first) - 20 + random.int(span);
            let to = from + random.int(span);
            let (k0, k1) = line_samples(&g, &s, &runs, from as f64, to as f64, pad);
            // As the rule reads it: the drawn samples in [from, to) with ticks in the padded
            // grid, the last drawn one before it and the first at or past its end.
            let mut want = Vec::new();
            let (mut before, mut after) = (None, None);
            for (k, v) in values.iter().enumerate() {
                let t = tick(tile_first + k as u64);
                if v.is_nan() || t < from || t >= to {
                    continue;
                }
                let t = t as f64;
                if t < g.x0 - pad {
                    before = Some(k);
                } else if t < g.x0 + g.count as f64 * g.per + pad {
                    want.push(k);
                } else if after.is_none() {
                    after = Some(k);
                }
            }
            want = before.into_iter().chain(want).chain(after).collect();
            let got: Vec<usize> = (k0..k1).filter(|&k| !values[k].is_nan()).collect();
            assert_eq!(got, want, "tile {tile}, grid {grid}");
        }
    }
}

#[test]
fn range_marks_label_round_values_inside_the_range_each_within_a_quarter_of_it_from_its_edge() {
    for k in 0..500 {
        let lo = (k as f64).sin() * 10f64.powf((k % 7) as f64 - 3.0);
        let hi = lo + 10f64.powf((k % 5) as f64 - 4.0) * (1 + k % 13) as f64;
        let m = range_marks(ValueRange { lo, hi }).unwrap();
        let w = hi - lo;
        assert!(
            m.bottom >= lo - 1e-9 * w && m.top <= hi + 1e-9 * w,
            "{} to {} outside {lo} to {hi}",
            m.bottom,
            m.top
        );
        assert!(
            hi - m.top <= w / 4.0 && m.bottom - lo <= w / 4.0,
            "{lo} to {hi}: {} and {}",
            m.bottom,
            m.top
        );
    }
}
