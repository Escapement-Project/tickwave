//! Statistics against an oracle that walks every decoded sample.

use std::sync::atomic::AtomicBool;

use escapement_tickwave_view::{ChannelStatistics, Recording, Sum};

mod common;
use common::{Oracle, Truth, fixture, open, oracle, synth_cases};

fn same(got: &ChannelStatistics, want: &Truth, at: &str) {
    assert_eq!(
        (got.samples, got.measured, got.min, got.max),
        (want.samples, want.measured, want.min, want.max),
        "{at}"
    );
    match (got.sum, want.codes) {
        (Sum::Exact { codes_ticks, .. }, Some(codes)) => assert_eq!(codes_ticks, codes, "{at}"),
        (Sum::Float(s), None) => assert_eq!(s, want.float, "{at}"),
        (sum, codes) => panic!("{at}: {sum:?} against {codes:?}"),
    }
}

/// Every channel's statistics over each interval, against the oracle.
fn check(rec: &Recording, truth: &[Option<Oracle>], intervals: &[(i64, i64)]) {
    let cancel = AtomicBool::new(false);
    for &(a, b) in intervals {
        let s = rec.statistics(a..b, &cancel).unwrap();
        assert_eq!(s.channels.len(), truth.len());
        for (c, (got, want)) in s.channels.iter().zip(truth).enumerate() {
            match (got, want) {
                (Some(got), Some(o)) => same(got, &o.over(a, b), &format!("channel {c}, {a}..{b}")),
                (None, None) => {}
                _ => panic!("channel {c}: {got:?}"),
            }
        }
    }
}

/// Intervals around each of `ticks`, and some across the whole: one tick before to one after,
/// a period and a bucket's worth either side.
fn around(ticks: &[i64], length: i64) -> Vec<(i64, i64)> {
    let mut out = vec![
        (0, length),
        (-100, length + 100),
        (-50, 0),
        (length, length + 9),
    ];
    for &t in ticks {
        for d in [0, 1, 3, 9, 10, 11, 16, 17, 255, 4096, 70_000] {
            out.push((t - d, t + d + 1));
            out.push((t, t + d + 1));
            out.push((t - d, t));
        }
    }
    out
}

#[test]
fn the_synthetic_cases_against_every_sample() {
    let file = synth_cases(1 << 16);
    let rec = open(&file);
    let truth = oracle(&file);
    // Run ends, the zero-length last sample, the loss, the pause, the length, and bucket
    // boundaries at every level.
    let a = truth[0].as_ref().unwrap();
    let run_ends: Vec<i64> = [40_000, 65_000, 95_000, 145_000, 205_000, 325_000]
        .iter()
        .map(|&i: &usize| a.samples[i - 1].0)
        .collect();
    let mut ticks = run_ends.clone();
    ticks.extend([1 << 12, 1 << 16, 3 << 20, 123_457, 1_000_003]);
    check(&rec, &truth, &around(&ticks, rec.length()));
    // And an interval of every width from every start, small.
    let mut small = Vec::new();
    for a in (run_ends[1] - 30)..(run_ends[1] + 30) {
        for w in 0..25 {
            small.push((a, a + w));
        }
    }
    check(&rec, &truth, &small);
}

#[test]
fn intervals_drawn_at_random() {
    let file = synth_cases(1 << 14);
    let rec = open(&file);
    let truth = oracle(&file);
    let length = rec.length();
    // xorshift, so a failure repeats.
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let intervals: Vec<(i64, i64)> = (0..300)
        .map(|_| {
            let a = (next() % (length as u64 + 200)) as i64 - 100;
            let w = (next() % (1 << (next() % 23))) as i64;
            (a, a + w)
        })
        .collect();
    check(&rec, &truth, &intervals);
}

#[test]
fn the_energy_fixtures_power_is_the_exact_sum_of_its_codes_products() {
    let file = fixture("energy.tickwave");
    let rec = open(&file);
    let truth = oracle(&file);
    let cancel = AtomicBool::new(false);
    let extent = rec.extent().unwrap();
    // P is channel 5: V, I, range, under_range, DIO0, then P. Its codes are I's nA times V's mV,
    // at 1/10^12 W.
    let (v, i) = (truth[0].as_ref().unwrap(), truth[1].as_ref().unwrap());
    let want: i128 = v
        .samples
        .iter()
        .zip(&i.samples)
        .map(|(v, i)| v.3.unwrap() * i.3.unwrap() * i128::from(v.1 - v.0))
        .sum();
    let s = rec.statistics(extent.clone(), &cancel).unwrap();
    let p = s.channels[5].as_ref().unwrap();
    let Sum::Exact {
        codes_ticks,
        scale,
        offset,
    } = p.sum
    else {
        panic!("{:?}", p.sum)
    };
    assert_eq!(codes_ticks, want);
    assert_eq!(
        (scale.num(), scale.den(), offset.num()),
        (1, 1_000_000_000_000, 0)
    );
    assert_eq!((p.samples, p.measured), (2600, 2600 * 960));
    // The capture lost 50 pairs, and paused: the extent's other ticks.
    assert_eq!(p.unmeasured(&extent, &extent), 2_768_000 - 2600 * 960);
    check(
        &rec,
        &truth,
        &around(
            &[959_040, 960_000, 1_008_000, 1_775_040, 2_000_000],
            rec.length(),
        ),
    );
}

#[test]
fn a_window_past_the_capture_counts_only_the_capture_as_unmeasured() {
    let rec = open(&fixture("energy.tickwave"));
    let cancel = AtomicBool::new(false);
    let extent = rec.extent().unwrap();
    let past = extent.end - 1000..extent.end + 1_000_000;
    let s = rec.statistics(past.clone(), &cancel).unwrap();
    let v = s.channels[0].as_ref().unwrap();
    // The last pair's 960 ticks end the extent; 40 of them lie before the window.
    assert_eq!(v.measured, 1000);
    assert_eq!(v.unmeasured(&past, &extent), 0);
    let before = -500_000..500;
    let s = rec.statistics(before.clone(), &cancel).unwrap();
    assert_eq!(
        s.channels[0].as_ref().unwrap().unmeasured(&before, &extent),
        0
    );
}

#[test]
fn a_mean_and_an_integral_from_the_exact_sum() {
    let rec = open(&fixture("energy.tickwave"));
    let cancel = AtomicBool::new(false);
    // The first pair alone: V 3.300 V for 960 ticks at 96 MHz, 10 us.
    let s = rec.statistics(0..960, &cancel).unwrap();
    let v = s.channels[0].as_ref().unwrap();
    assert_eq!((v.samples, v.measured), (1, 960));
    assert_eq!(v.mean(), Some(3.3));
    assert_eq!(v.integral(rec.timebase()), 3.3e-5);
    // Nothing measured, no mean.
    let s = rec.statistics(2_800_000..2_800_001, &cancel).unwrap();
    assert_eq!(s.channels[0].as_ref().unwrap().mean(), None);
}
