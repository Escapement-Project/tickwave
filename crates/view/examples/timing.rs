//! Times what verge-gui asks of a recording: opening it, and a view's query at the finest
//! overview level's edge and one step in, cold, warm and panned. Run in release:
//!
//!     cargo run --release -p escapement-tickwave-view --example timing -- rec.tickwave
//!
//! - Open: a new `Recording` and an empty `Cache` from the file, footer parse and all; the file
//!   is read once first, so it is in the page cache.
//! - The query: the window's 1,920 columns of I, DIO0 to DIO3 under it, I's single samples
//!   flagged by `under_range`, the strip of I over the extent, the window's statistics and its
//!   markers. Centred on the middle of the extent, at 2^shift ticks a column (the last zoom the
//!   finest level serves) and at half that (the first that decodes samples).
//! - Cold: the query on a recording opened just before, untimed, with an empty cache. Warm: the
//!   same query again. Pan: the window moved by a tenth of its width, each run further on.

use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use clap::Parser;
use escapement_tickwave_format::decode::Footer;
use escapement_tickwave_view::{Bytes, Cache, MarkerQuery, Query, Recording};
use tracing::{info, info_span};
use tracing_indicatif::IndicatifLayer;
use tracing_indicatif::span_ext::IndicatifSpanExt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[derive(Parser)]
struct Args {
    recordings: Vec<PathBuf>,
    #[arg(long, default_value_t = 1920)]
    columns: u32,
    #[arg(long, default_value_t = 20)]
    runs: usize,
    /// The cache's bytes.
    #[arg(long, default_value_t = 512 << 20)]
    cache: usize,
}

/// A median and a 95th percentile, in milliseconds.
fn spread(mut times: Vec<Duration>) -> (f64, f64) {
    times.sort();
    let at = |q: f64| times[((times.len() - 1) as f64 * q).round() as usize].as_secs_f64() * 1e3;
    (at(0.5), at(0.95))
}

fn load() -> String {
    std::fs::read_to_string("/proc/loadavg")
        .map(|l| l.split_whitespace().take(3).collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
}

fn main() {
    let progress = IndicatifLayer::new();
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_writer(progress.get_stderr_writer()))
        .with(progress)
        .init();
    let args = Args::parse();
    let cancel = AtomicBool::new(false);
    for path in &args.recordings {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        // Into the page cache.
        let mut whole = Vec::new();
        File::open(path).unwrap().read_to_end(&mut whole).unwrap();
        let size = whole.len();
        let shift = Footer::parse_file(&whole).unwrap().header.overview_shift;
        drop(whole);
        info!(%name, size, shift, load = load(), "start");
        let open = || {
            let bytes: Arc<dyn Bytes> = Arc::new(File::open(path).unwrap());
            Recording::open(bytes, Cache::new(args.cache)).unwrap()
        };
        let span = info_span!("runs", %name);
        span.pb_set_length((args.runs * 7) as u64);
        let _entered = span.enter();
        let opens: Vec<Duration> = (0..args.runs)
            .map(|_| {
                let t = Instant::now();
                let rec = open();
                let d = t.elapsed();
                drop(rec);
                span.pb_inc(1);
                d
            })
            .collect();
        let (m, p) = spread(opens);
        info!(%name, median_ms = m, p95_ms = p, "open");
        let rec = open();
        let extent = rec.extent().unwrap();
        let middle = (extent.start + extent.end) / 2;
        let query = |start: i64, per: i64| {
            let width = per * i64::from(args.columns);
            let window = start..start + width;
            Query {
                window: window.clone(),
                columns: args.columns,
                channels: vec![1, 4, 5, 6, 7],
                flags: vec![(1, 3)],
                strip: Some((1, args.columns)),
                statistics: vec![window],
                markers: MarkerQuery {
                    limit: 100,
                    ..MarkerQuery::default()
                },
            }
        };
        for (view, per) in [
            ("finest level's edge", 1i64 << shift),
            ("one step in", 1i64 << (shift - 1)),
        ] {
            let width = per * i64::from(args.columns);
            let start = middle - width / 2;
            let q = query(start, per);
            let cold: Vec<Duration> = (0..args.runs)
                .map(|_| {
                    let rec = open();
                    let t = Instant::now();
                    rec.query(&q, &cancel).unwrap();
                    span.pb_inc(1);
                    t.elapsed()
                })
                .collect();
            let rec = open();
            rec.query(&q, &cancel).unwrap();
            let warm: Vec<Duration> = (0..args.runs)
                .map(|_| {
                    let t = Instant::now();
                    rec.query(&q, &cancel).unwrap();
                    span.pb_inc(1);
                    t.elapsed()
                })
                .collect();
            let pan: Vec<Duration> = (1..=args.runs as i64)
                .map(|k| {
                    let q = query(start + k * width / 10, per);
                    let t = Instant::now();
                    rec.query(&q, &cancel).unwrap();
                    span.pb_inc(1);
                    t.elapsed()
                })
                .collect();
            for (how, times) in [("cold", cold), ("warm", warm), ("pan by 10 %", pan)] {
                let (m, p) = spread(times);
                info!(%name, view, per, how, median_ms = m, p95_ms = p, "query");
            }
        }
        info!(%name, load = load(), "end");
    }
}
