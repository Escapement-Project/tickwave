//! A synthetic recorder for the demo's `--live`: a capture laid out as verge's energy captures
//! are (crates/view/examples/synth.rs), written in real time to a temporary file on a thread of
//! its own, each snapshot handed to the viewer.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use escapement_tickwave_egui::Viewer;
use escapement_tickwave_format::header::{
    AnalogCoding, Declaration, DeclaredChannel, DeclaredKind, Derived, Exact, IntCoding,
    SourceFormat, SourceHeader,
};
use escapement_tickwave_format::model::Ratio;
use escapement_tickwave_format::writer::{Bundle, Finish, WriteOptions, Writer};
use tracing::{error, info, warn};

const CLOCK_HZ: u64 = 96_000_000;
/// How often the recorder hands the viewer a snapshot, as verge's Recorder does at most.
const PUBLISH: Duration = Duration::from_millis(200);
const BATCHES_A_SECOND: u64 = 5;

/// The digital channels' edges in second `s`, in order: (tick, channel). synth.rs's.
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
    if s.is_multiple_of(13) || s % 13 == 7 {
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

fn declaration(period: u64) -> Declaration {
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
        },
        shaping: Default::default(),
    };
    let digital = |name: &str| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Digital { initial: false },
        shaping: Default::default(),
    };
    Declaration {
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
            product: ["I".into(), "V".into()],
            significant: None,
        }],
        sources: vec![SourceHeader::new(
            SourceFormat::Other("verge-energy".into()),
            timebase,
        )],
        title: Some("A synthetic energy capture, live".into()),
        meta: BTreeMap::new(),
        // The Recorder's rule: round(log2(1024 × ticks a pair)), at most 24.
        overview_shift: ((1024.0 * period as f64).log2().round() as u8).min(24),
    }
}

/// The recorder's state between batches.
struct Synth {
    writer: Writer,
    file: File,
    rate: u64,
    period: u64,
    /// Pairs written.
    pairs: u64,
    /// The xorshift noise's state.
    noise: u64,
    /// This second's edges not yet written.
    due: Vec<(i64, usize)>,
}

impl Synth {
    /// The edges left in the second under way, after its last pair.
    fn rest(&mut self) -> Result<(), String> {
        while let Some((t, c)) = self.due.pop() {
            self.writer.edge(c, t).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Writes pairs up to `to` with their edges, flushing at each whole second, and the bytes to
    /// the file.
    fn write(&mut self, to: u64) -> Result<(), String> {
        let e = |e: &dyn std::fmt::Display| e.to_string();
        while self.pairs < to {
            let i = self.pairs;
            let (s, k) = (i / self.rate, i % self.rate);
            if k == 0 {
                if s > 0 {
                    self.rest()?;
                    self.writer
                        .flush((s * CLOCK_HZ) as i64)
                        .map_err(|x| e(&x))?;
                }
                self.due = edges(s);
                self.due.reverse();
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
                    self.writer.annotation("verge", marker).map_err(|x| e(&x))?;
                }
            }
            let tick = (i * self.period) as i64;
            while let Some(&(t, c)) = self.due.last()
                && t <= tick
            {
                self.writer.edge(c, t).map_err(|x| e(&x))?;
                self.due.pop();
            }
            let x = &mut self.noise;
            *x ^= *x << 13;
            *x ^= *x >> 7;
            *x ^= *x << 17;
            // About 3.3 V with a few mV of noise; 1 to 50 mA in steps every few seconds.
            let step = (s / 5) % 10;
            self.writer
                .sample(0, 3300 - step + (*x & 7))
                .map_err(|x| e(&x))?;
            self.writer
                .sample(1, 1_000_000 + step * 5_000_000 + ((*x >> 8) & 0xfff))
                .map_err(|x| e(&x))?;
            self.pairs += 1;
        }
        self.file
            .write_all(&self.writer.take_output())
            .map_err(|x| e(&x))?;
        self.file.flush().map_err(|x| e(&x))
    }
}

/// A live recording under way, and the thread that writes it.
pub struct Live {
    /// Holds the file until the demo ends.
    _dir: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Live {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Starts recording `rate` pairs a second, for `seconds` if given, then finished and reopened.
/// The viewer is made from the first batch's snapshot.
pub fn start(
    ctx: &egui::Context,
    rate: u64,
    seconds: Option<u64>,
) -> Result<(Arc<Viewer>, Live), String> {
    if rate == 0 || !CLOCK_HZ.is_multiple_of(rate * BATCHES_A_SECOND) {
        return Err(format!(
            "{rate} pairs a second: 96 MHz must be a multiple of five times the rate"
        ));
    }
    let period = CLOCK_HZ / rate;
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let path: PathBuf = dir.path().join("live.tickwave");
    let mut writer = Writer::new(
        declaration(period),
        WriteOptions {
            level: 16,
            block_bytes: 1 << 20,
            batch: 1,
        },
    )
    .map_err(|e| e.to_string())?;
    let mut file = File::create(&path).map_err(|e| e.to_string())?;
    file.write_all(&writer.take_output())
        .map_err(|e| e.to_string())?;
    writer.run(0, 0, 0).map_err(|e| e.to_string())?;
    writer.run(1, 0, 0).map_err(|e| e.to_string())?;
    let mut synth = Synth {
        writer,
        file,
        rate,
        period,
        pairs: 0,
        noise: 0x2545_f491_4f6c_dd1d,
        due: Vec::new(),
    };
    let batch = rate / BATCHES_A_SECOND;
    synth.write(batch)?;
    let viewer =
        Arc::new(Viewer::live(ctx, &path, synth.writer.snapshot()).map_err(|e| e.to_string())?);
    info!(rate, path = %path.display(), "recording live");
    let stop = Arc::new(AtomicBool::new(false));
    let thread = {
        let (viewer, stop) = (Arc::clone(&viewer), Arc::clone(&stop));
        std::thread::Builder::new()
            .name("synthetic recorder".into())
            .spawn(move || record(synth, &viewer, &stop, seconds))
            .map_err(|e| e.to_string())?
    };
    Ok((
        viewer,
        Live {
            _dir: dir,
            stop,
            thread: Some(thread),
        },
    ))
}

/// Writes a batch every `PUBLISH`, in step with the clock, handing each snapshot to the viewer.
fn record(mut synth: Synth, viewer: &Viewer, stop: &AtomicBool, seconds: Option<u64>) {
    let started = Instant::now();
    let batch = synth.rate / BATCHES_A_SECOND;
    let end = seconds.map(|s| s * synth.rate);
    let mut behind = false;
    let mut n = 1;
    while !stop.load(Ordering::Relaxed) {
        let due = started + PUBLISH * n;
        match due.checked_duration_since(Instant::now()) {
            Some(wait) => std::thread::sleep(wait),
            None if !behind => {
                behind = true;
                warn!("the recorder fell behind the clock");
            }
            None => {}
        }
        n += 1;
        let to = (synth.pairs + batch).min(end.unwrap_or(u64::MAX));
        if let Err(e) = synth.write(to) {
            error!("the recorder stopped: {e}");
            return;
        }
        viewer.update(synth.writer.snapshot());
        if Some(synth.pairs) == end {
            if let Err(e) = synth.rest() {
                error!("the recorder stopped: {e}");
                return;
            }
            let length = (synth.pairs * synth.period) as i64;
            let finish = Finish {
                bundle: Some(Bundle::Generated {
                    stem: "live".into(),
                }),
                ..Finish::default()
            };
            let finished = synth
                .writer
                .finish(length, &[length; 6], finish)
                .map_err(|e| e.to_string())
                .and_then(|f| {
                    synth.file.write_all(&f.tail).map_err(|e| e.to_string())?;
                    synth.file.sync_all().map_err(|e| e.to_string())
                });
            match finished.and_then(|()| viewer.reopen().map_err(|e| e.to_string())) {
                Ok(()) => info!(seconds = ?seconds, "finished, and reopened"),
                Err(e) => error!("finishing: {e}"),
            }
            return;
        }
    }
}
