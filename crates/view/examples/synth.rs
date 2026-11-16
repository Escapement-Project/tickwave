//! Writes a recording laid out as verge's energy captures are, for the view core's timing:
//!
//!     cargo run --release -p escapement-tickwave-view --example synth -- --rate 100000 \
//!         --seconds 3600 hour.tickwave --live-calls 20
//!
//! - One source, `verge-energy`, at 96 MHz. V and I, `int` 32-bit unsigned codes in mV and nA,
//!   a pair every 96 MHz / rate ticks; P, derived, their integer product in W.
//! - `range` and `under_range`, and DIO0 to DIO3, digital, with edges at their own rates.
//! - A marker every minute, bundled into the footer at finish.
//! - A flush every second, as verge's Recorder closes a block at least that often; zstd level 16.
//! - The finest overview level by the Recorder's rule, round(log2(1024 × ticks a pair)), at most
//!   24, or `--shift`'s.
//!
//! With `--live-calls N`, in the last second, before its flush, it times `Writer::snapshot` and
//! `Recording::live` over the file written so far, N times each: the open block then holds that
//! second's pairs.
//!
//! With `--follow`, it keeps one live recording of the file, as a viewer does, and after every
//! second's flush times `Writer::snapshot` and `Recording::update` with it. It reports both over
//! the first and the last tenth of the seconds: an update that reads only what came since costs
//! the same at either end.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use escapement_tickwave_format::header::{
    AnalogCoding, Declaration, DeclaredChannel, DeclaredKind, Derived, Exact, IntCoding,
    SourceFormat, SourceHeader,
};
use escapement_tickwave_format::model::Ratio;
use escapement_tickwave_format::writer::{Bundle, Finish, WriteOptions, Writer};
use escapement_tickwave_view::{Cache, Recording};
use tracing::{info, info_span};
use tracing_indicatif::IndicatifLayer;
use tracing_indicatif::span_ext::IndicatifSpanExt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

const CLOCK_HZ: u64 = 96_000_000;

#[derive(Parser)]
struct Args {
    /// Pairs a second; 96,000,000 a multiple of it.
    #[arg(long)]
    rate: u64,
    #[arg(long)]
    seconds: u64,
    /// The recording to write.
    out: PathBuf,
    /// Time `Writer::snapshot` and `Recording::live` this many times at the end.
    #[arg(long)]
    live_calls: Option<usize>,
    /// Keep a live recording updated after every second's flush, and time each update.
    #[arg(long)]
    follow: bool,
    /// The finest overview level's shift, in place of the Recorder's rule.
    #[arg(long)]
    shift: Option<u8>,
}

/// The digital channels' edges in second `s`, in order: (tick, channel).
fn edges(s: u64) -> Vec<(i64, usize)> {
    let second = (s * CLOCK_HZ) as i64;
    let mut out = Vec::new();
    let mut every = |channel: usize, period: u64, width: u64| {
        let mut t = 0;
        while t < CLOCK_HZ {
            out.push((second + t as i64, channel));
            out.push((second + (t + width) as i64, channel));
            t += period;
        }
    };
    // DIO0 a 10 Hz square wave, DIO1 1 Hz, DIO2 a 1 ms pulse every 250 ms.
    every(4, CLOCK_HZ / 10, CLOCK_HZ / 20);
    every(5, CLOCK_HZ, CLOCK_HZ / 2);
    every(6, CLOCK_HZ / 4, CLOCK_HZ / 1000);
    // range: high for 7 s of every 13; under_range a 1 ms pulse every 3 s; DIO3 every 30 s.
    if s.is_multiple_of(13) {
        out.push((second, 2));
    }
    if s % 13 == 7 {
        out.push((second, 2));
    }
    if s % 3 == 1 {
        out.push((second + 1000, 3));
        out.push((second + 1000 + CLOCK_HZ as i64 / 1000, 3));
    }
    if s % 30 == 29 {
        out.push((second + CLOCK_HZ as i64 / 2, 7));
    }
    out.sort();
    out
}

/// A median and a 95th percentile, in microseconds.
fn spread(mut times: Vec<Duration>) -> (f64, f64) {
    times.sort();
    let at = |q: f64| times[((times.len() - 1) as f64 * q).round() as usize].as_secs_f64() * 1e6;
    (at(0.5), at(0.95))
}

/// Times `Writer::snapshot` and `Recording::live` `n` times each over `out` as written so far.
fn time_live(w: &Writer, out: &PathBuf, n: usize) {
    let bytes: Arc<dyn escapement_tickwave_view::Bytes> = Arc::new(File::open(out).unwrap());
    let cache = Cache::new(256 << 20);
    let (mut snapshots, mut lives) = (Vec::new(), Vec::new());
    let mut held = 0;
    for _ in 0..n {
        let t = Instant::now();
        let snapshot = w.snapshot();
        snapshots.push(t.elapsed());
        let t = Instant::now();
        let live = Recording::live(snapshot, Arc::clone(&bytes), cache.clone()).unwrap();
        lives.push(t.elapsed());
        held = live.length();
    }
    let (sm, sp) = spread(snapshots);
    let (lm, lp) = spread(lives);
    info!(
        calls = n,
        held_ticks = held,
        snapshot_median_us = sm,
        snapshot_p95_us = sp,
        live_median_us = lm,
        live_p95_us = lp,
        "in the last second, its pairs in the open block"
    );
}

fn main() {
    let progress = IndicatifLayer::new();
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_writer(progress.get_stderr_writer()))
        .with(progress)
        .init();
    let args = Args::parse();
    assert!(
        CLOCK_HZ.is_multiple_of(args.rate),
        "96 MHz is a multiple of the rate"
    );
    let period = CLOCK_HZ / args.rate;
    let shift = args
        .shift
        .unwrap_or_else(|| ((1024.0 * period as f64).log2().round() as u8).min(24));
    let timebase = Ratio::integer(CLOCK_HZ).unwrap();
    let int = |den| {
        AnalogCoding::Int(IntCoding {
            scale: Exact::new(1, den),
            ..IntCoding::new(32, false)
        })
    };
    let analog = |name: &str, unit: &str, coding| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Analog {
            unit: unit.into(),
            period: Ratio::integer(period).unwrap(),
            coding,
            significant: Some(4),
            phase: None,
        },
        shaping: Default::default(),
        overview_shift: None,
        codec: None,
        requires: Vec::new(),
    };
    let digital = |name: &str| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Digital { initial: false },
        shaping: Default::default(),
        overview_shift: None,
        codec: None,
        requires: Vec::new(),
    };
    let declaration = Declaration {
        timebase,
        channels: vec![
            analog("V", "V", int(1000)),
            analog("I", "A", int(1_000_000_000)),
            digital("range"),
            digital("under_range"),
            digital("DIO0"),
            digital("DIO1"),
            digital("DIO2"),
            digital("DIO3"),
        ],
        derived: vec![Derived {
            name: "P".into(),
            unit: "W".into(),
            op: escapement_tickwave_format::header::Op::Multiply,
            operands: vec!["I".into(), "V".into()],
            params: Default::default(),
            requires: Vec::new(),
            significant: None,
        }],
        sources: vec![SourceHeader::new(
            SourceFormat::Other("verge-energy".into()),
            timebase,
        )],
        title: Some("A synthetic energy capture".into()),
        meta: BTreeMap::new(),
        overview_shift: shift,
        requires: Vec::new(),
    };
    info!(
        rate = args.rate,
        seconds = args.seconds,
        period,
        shift,
        "writing {}",
        args.out.display()
    );
    let mut w = Writer::new(
        declaration,
        WriteOptions {
            level: 16,
            block_bytes: 1 << 20,
            ..WriteOptions::default()
        },
    )
    .unwrap();
    let mut file = File::create(&args.out).unwrap();
    let mut followed: Option<Recording> = None;
    let mut follows: Vec<(Duration, Duration)> = Vec::new();
    file.write_all(&w.take_output()).unwrap();
    w.run(0, 0, 0).unwrap();
    w.run(1, 0, 0).unwrap();
    let started = Instant::now();
    let span = info_span!("seconds");
    span.pb_set_length(args.seconds);
    let entered = span.enter();
    let mut x = 0x2545_f491_4f6c_dd1du64;
    for s in 0..args.seconds {
        let mut due = edges(s).into_iter().peekable();
        for k in 0..args.rate {
            let i = s * args.rate + k;
            let tick = (i * period) as i64;
            while let Some(&(t, c)) = due.peek()
                && t <= tick
            {
                w.edge(c, t).unwrap();
                due.next();
            }
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            // About 3.3 V with a few mV of noise; 1 to 50 mA in steps every few seconds, with a
            // few µA of noise.
            let step = (s / 5) % 10;
            w.sample(0, 3300 - step + (x & 7)).unwrap();
            w.sample(1, 1_000_000 + step * 5_000_000 + ((x >> 8) & 0xfff))
                .unwrap();
        }
        for (t, c) in due {
            w.edge(c, t).unwrap();
        }
        if s + 1 == args.seconds
            && let Some(n) = args.live_calls
        {
            file.write_all(&w.take_output()).unwrap();
            file.flush().unwrap();
            time_live(&w, &args.out, n);
        }
        if s % 60 == 30 {
            let number = s / 60 + 1;
            let serde_json::Value::Object(marker) = serde_json::json!({
                "id": format!("marker/{number}"),
                "kind": "marker",
                "start": (s * CLOCK_HZ) as i64,
                "label": format!("Marker {number}"),
                "values": {"number": number.to_string()},
            }) else {
                unreachable!("an object")
            };
            w.annotation("verge", marker).unwrap();
        }
        w.flush(((s + 1) * CLOCK_HZ) as i64).unwrap();
        file.write_all(&w.take_output()).unwrap();
        if args.follow {
            file.flush().unwrap();
            let t = Instant::now();
            let snapshot = w.snapshot();
            let took = t.elapsed();
            let t = Instant::now();
            match &mut followed {
                None => {
                    let bytes: Arc<dyn escapement_tickwave_view::Bytes> =
                        Arc::new(File::open(&args.out).unwrap());
                    followed =
                        Some(Recording::live(snapshot, bytes, Cache::new(256 << 20)).unwrap());
                }
                Some(rec) => rec.update(snapshot).unwrap(),
            }
            follows.push((took, t.elapsed()));
        }
        span.pb_inc(1);
    }
    if args.follow {
        let tenth = (follows.len() / 10).max(1);
        for (when, part) in [
            ("first tenth", &follows[..tenth]),
            ("last tenth", &follows[follows.len() - tenth..]),
        ] {
            let (sm, sp) = spread(part.iter().map(|t| t.0).collect());
            let (um, up) = spread(part.iter().map(|t| t.1).collect());
            info!(
                when,
                seconds = part.len(),
                snapshot_median_us = sm,
                snapshot_p95_us = sp,
                update_median_us = um,
                update_p95_us = up,
                "following the recording, a snapshot and an update after each second's flush"
            );
        }
    }
    drop(entered);
    let length = (args.seconds * CLOCK_HZ) as i64;
    info!(
        seconds = started.elapsed().as_secs_f64(),
        bytes = w.position(),
        "written up to the footer"
    );
    let stem = args
        .out
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("rec")
        .to_owned();
    let finish = Finish {
        bundle: Some(Bundle::Generated { stem }),
        ..Finish::default()
    };
    let finished = w.finish(length, &[length; 6], finish).unwrap();
    file.write_all(&finished.tail).unwrap();
    file.sync_all().unwrap();
    info!(
        seconds = started.elapsed().as_secs_f64(),
        bytes = file.metadata().unwrap().len(),
        id = %finished.content_id(),
        "finished"
    );
}
