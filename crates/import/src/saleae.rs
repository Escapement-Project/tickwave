//! Saleae Logic 2's binary export: a directory with one file a channel, as Logic 2 and its headless
//! server write them. Logic 8, Logic Pro 8 and Logic Pro 16 write version 0; the Logic MSO writes
//! version 1.
//!
//! Every file, little-endian: `<SALEAE>`, i32 version, i32 type (0 digital, 1 analog).
//! - Version 0, digital: u32 initial level, f64 begin, f64 end, u64 count, then `count` f64
//!   transition times in seconds. The file records no sample rate: see [`Headers::read`].
//! - Version 0, analog: f64 begin, u64 sample rate, u64 downsample, u64 count, then `count` f32
//!   volts.
//! - Version 1: i64 chunk count, then one chunk (files with more are refused). Digital: u32 initial
//!   level, f64 sample rate, f64 begin, f64 end, i64 count, then the times. Analog: f64 begin, f64
//!   trigger, f64 sample rate, u64 downsample, u64 count, then the volts.
//!
//! Only a downsample of 1 is read: no export at another has been checked.

use std::fmt;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use escapement_tickwave_format::model::{
    Analog, Channel, ChannelData, Digital, RateOrigin, Ratio, Recording, Samples, Source,
    SourceFormat,
};
use serde::Deserialize;

use crate::input::{self, Input, Items};

const MAGIC: &[u8; 8] = b"<SALEAE>";
/// The longest header, read whole before the version is known.
const LONGEST_HEADER: usize = 64;

/// How far an edge may sit from the sample grid, in samples, before the source is refused. The
/// recordings measured so far sit within 6.4e-4.
pub const GRID_TOLERANCE: f64 = 0.01;

/// The highest sample rate a version 0 export's rate is inferred as, in hertz.
const MOST_INFERRED: u64 = 1_000_000_000;

/// Edges from the start of each file that every candidate rate is tried on before it gets a pass
/// over all of them.
const PROBE: u64 = 256;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path}: not a Saleae binary export")]
    NotSaleae { path: PathBuf },
    #[error("{path}: {what} is not supported")]
    Unsupported { path: PathBuf, what: String },
    #[error("{path}: {len} bytes, but the header promises {expected}")]
    Length {
        path: PathBuf,
        len: u64,
        expected: u128,
    },
    #[error("{path}: sample rate {rate} is not a whole number of hertz")]
    Rate { path: PathBuf, rate: f64 },
    #[error("{path}: records a sample rate of {recorded}, not the {given} given")]
    RateDiffers {
        path: PathBuf,
        recorded: SampleRate,
        given: SampleRate,
    },
    #[error(
        "the export's digital files don't record their sample rate, and {0}: give the rate the \
         capture ran at"
    )]
    NoRate(&'static str),
    #[error("{path}: begins {residual:.3} samples off the digital channels' sample grid")]
    AnalogOffGrid { path: PathBuf, residual: f64 },
    #[error("{path}: edge {index} lies {residual:.3} samples off the sample grid")]
    OffGrid {
        path: PathBuf,
        index: u64,
        residual: f64,
    },
    #[error("{path}: edge {index} at tick {tick} does not follow tick {previous}")]
    NotIncreasing {
        path: PathBuf,
        index: u64,
        tick: i64,
        previous: i64,
    },
    #[error("{path}: edge {index} at tick {tick} lies outside the capture, ticks 1 to {end}")]
    OutOfCapture {
        path: PathBuf,
        index: u64,
        tick: i64,
        end: i64,
    },
    #[error("{path}: {what} differs from {first}'s")]
    Mismatch {
        path: PathBuf,
        first: PathBuf,
        what: &'static str,
    },
    #[error("no {0} in the export")]
    NoFile(String),
    #[error("no channels named")]
    NoChannels,
}

/// A Saleae export's version, which sets its files' layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Version {
    V0,
    V1,
}

/// A sample rate in whole hertz, from 1 Hz to below 2^53 Hz, as a recording's timebase holds it.
/// It reads from text as a number with an optional fraction and a `k`, `M` or `G` suffix, such as
/// `50M` or `12.5M`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct SampleRate(NonZeroU64);

impl SampleRate {
    pub fn new(hz: u64) -> Option<Self> {
        NonZeroU64::new(hz).filter(|n| n.get() < 1 << 53).map(Self)
    }

    pub fn hz(self) -> u64 {
        self.0.get()
    }

    /// The rate a timebase of whole ticks a second holds, as a Saleae recording's does.
    pub fn of_timebase(timebase: Ratio) -> Option<Self> {
        (timebase.den() == 1)
            .then(|| Self::new(timebase.num()))
            .flatten()
    }

    /// A rate a file records as a float, if it is a whole number of hertz in range.
    fn from_f64(rate: f64) -> Option<Self> {
        (rate.fract() == 0.0 && rate >= 1.0 && rate < 2f64.powi(53))
            .then(|| Self::new(rate as u64))
            .flatten()
    }
}

impl From<SampleRate> for Ratio {
    fn from(rate: SampleRate) -> Self {
        Ratio::whole(rate.0)
    }
}

impl fmt::Display for SampleRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} Hz", self.hz())
    }
}

impl TryFrom<String> for SampleRate {
    type Error = String;

    fn try_from(s: String) -> Result<Self, String> {
        s.parse()
    }
}

impl FromStr for SampleRate {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let wrong =
            || format!("expected a rate in hertz, such as 50000000, 50M or 12.5M; got {s:?}");
        let t = s.trim();
        let (number, scale) = match t.char_indices().last() {
            Some((at, 'k')) => (&t[..at], 1_000u64),
            Some((at, 'M')) => (&t[..at], 1_000_000),
            Some((at, 'G')) => (&t[..at], 1_000_000_000),
            _ => (t, 1),
        };
        let number = number.trim_end();
        let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
        let digits = |d: &str| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit());
        if !digits(whole) || !(fraction.is_empty() || digits(fraction)) {
            return Err(wrong());
        }
        let fraction = fraction.trim_end_matches('0');
        let denominator = u32::try_from(fraction.len())
            .ok()
            .and_then(|n| 10u64.checked_pow(n))
            .ok_or_else(wrong)?;
        let whole: u64 = whole.parse().map_err(|_| wrong())?;
        let part: u64 = if fraction.is_empty() {
            0
        } else {
            fraction.parse().map_err(|_| wrong())?
        };
        let scaled_part = part.checked_mul(scale).ok_or_else(wrong)?;
        if scaled_part % denominator != 0 {
            return Err(format!("{s:?} is not a whole number of hertz"));
        }
        let hz = whole
            .checked_mul(scale)
            .and_then(|w| w.checked_add(scaled_part / denominator))
            .ok_or_else(wrong)?;
        Self::new(hz).ok_or_else(|| format!("{s:?}: a rate is from 1 Hz to below 2^53 Hz"))
    }
}

/// A file whose header has been read.
struct File {
    path: PathBuf,
    input: Arc<dyn Input>,
    size: u64,
    /// The file's first bytes, as many as the longest header.
    head: Vec<u8>,
}

impl File {
    fn open(path: PathBuf, input: Arc<dyn Input>) -> Result<(Self, Version, i32), Error> {
        let size = input.size();
        let mut head = vec![0; size.min(LONGEST_HEADER as u64) as usize];
        input.read_at(0, &mut head);
        if head.len() < 16 || &head[..8] != MAGIC {
            return Err(Error::NotSaleae { path });
        }
        let file = Self {
            path,
            input,
            size,
            head,
        };
        let kind = file.i32_at(12);
        let version = match file.i32_at(8) {
            0 => Version::V0,
            1 => {
                file.need(24)?;
                let chunks = file.i64_at(16);
                if chunks != 1 {
                    return Err(file.unsupported(format!("{chunks} chunks")));
                }
                Version::V1
            }
            v => return Err(file.unsupported(format!("export version {v}"))),
        };
        Ok((file, version, kind))
    }

    fn i32_at(&self, at: usize) -> i32 {
        i32::from_le_bytes(self.head[at..at + 4].try_into().unwrap())
    }

    fn i64_at(&self, at: usize) -> i64 {
        i64::from_le_bytes(self.head[at..at + 8].try_into().unwrap())
    }

    fn f64_at(&self, at: usize) -> f64 {
        f64::from_le_bytes(self.head[at..at + 8].try_into().unwrap())
    }

    fn unsupported(&self, what: String) -> Error {
        Error::Unsupported {
            path: self.path.clone(),
            what,
        }
    }

    fn need(&self, header: usize) -> Result<(), Error> {
        if self.size < header as u64 {
            return Err(Error::Length {
                path: self.path.clone(),
                len: self.size,
                expected: header as u128,
            });
        }
        Ok(())
    }

    /// Checks the file holds its header and exactly `count` items of `size` bytes after it.
    fn check_length(&self, header: usize, count: i64, size: u128) -> Result<u64, Error> {
        let expected = header as u128 + size * count.max(0) as u128;
        if count < 0 || u128::from(self.size) != expected {
            return Err(Error::Length {
                path: self.path.clone(),
                len: self.size,
                expected,
            });
        }
        Ok(count as u64)
    }

    /// The file's `N`-byte items after its header.
    fn items<const N: usize>(&self, header: usize, count: u64) -> Items<N> {
        Items::new(Arc::clone(&self.input), header as u64, count)
    }

    fn check_rate(&self, rate: f64) -> Result<SampleRate, Error> {
        SampleRate::from_f64(rate).ok_or_else(|| Error::Rate {
            path: self.path.clone(),
            rate,
        })
    }
}

/// One digital file, its header checked.
struct DigitalFile {
    file: File,
    version: Version,
    initial: bool,
    /// None in version 0, which records none.
    rate: Option<SampleRate>,
    begin: f64,
    end: f64,
    count: u64,
    header: usize,
}

/// One analog file, its header checked.
struct AnalogFile {
    file: File,
    version: Version,
    begin: f64,
    /// None in version 0, which records none.
    trigger: Option<f64>,
    rate: SampleRate,
    count: u64,
    header: usize,
}

enum Opened {
    Digital(DigitalFile),
    Analog(AnalogFile),
}

impl Opened {
    fn file(&self) -> &File {
        match self {
            Opened::Digital(d) => &d.file,
            Opened::Analog(a) => &a.file,
        }
    }

    fn version(&self) -> Version {
        match self {
            Opened::Digital(d) => d.version,
            Opened::Analog(a) => a.version,
        }
    }

    fn rate(&self) -> Option<SampleRate> {
        match self {
            Opened::Digital(d) => d.rate,
            Opened::Analog(a) => Some(a.rate),
        }
    }
}

fn tick_of(t: f64, begin: f64, rate: SampleRate) -> f64 {
    (t - begin) * rate.hz() as f64
}

fn open(path: PathBuf, input: Arc<dyn Input>) -> Result<Opened, Error> {
    let (file, version, kind) = File::open(path, input)?;
    match (version, kind) {
        (Version::V0, 0) => {
            const HEADER: usize = 44;
            file.need(HEADER)?;
            let initial = u32::from_le_bytes(file.head[16..20].try_into().unwrap());
            let (begin, end) = (file.f64_at(20), file.f64_at(28));
            let count = file.check_length(HEADER, file.i64_at(36), 8)?;
            digital(file, version, initial, None, begin, end, count, HEADER)
        }
        (Version::V1, 0) => {
            const HEADER: usize = 60;
            file.need(HEADER)?;
            let initial = u32::from_le_bytes(file.head[24..28].try_into().unwrap());
            let (rate, begin, end) = (file.f64_at(28), file.f64_at(36), file.f64_at(44));
            let count = file.check_length(HEADER, file.i64_at(52), 8)?;
            let rate = file.check_rate(rate)?;
            digital(
                file,
                version,
                initial,
                Some(rate),
                begin,
                end,
                count,
                HEADER,
            )
        }
        (Version::V0, 1) => {
            const HEADER: usize = 48;
            file.need(HEADER)?;
            let (begin, rate) = (file.f64_at(16), file.i64_at(24) as f64);
            let downsample = file.i64_at(32);
            let count = file.check_length(HEADER, file.i64_at(40), 4)?;
            analog(file, version, begin, None, rate, downsample, count, HEADER)
        }
        (Version::V1, 1) => {
            const HEADER: usize = 64;
            file.need(HEADER)?;
            let (begin, trigger, rate) = (file.f64_at(24), file.f64_at(32), file.f64_at(40));
            let downsample = file.i64_at(48);
            let count = file.check_length(HEADER, file.i64_at(56), 4)?;
            analog(
                file,
                version,
                begin,
                Some(trigger),
                rate,
                downsample,
                count,
                HEADER,
            )
        }
        (_, kind) => Err(file.unsupported(format!("channel type {kind}"))),
    }
}

#[allow(clippy::too_many_arguments)]
fn digital(
    file: File,
    version: Version,
    initial: u32,
    rate: Option<SampleRate>,
    begin: f64,
    end: f64,
    count: u64,
    header: usize,
) -> Result<Opened, Error> {
    if initial > 1 {
        return Err(file.unsupported(format!("initial state {initial}")));
    }
    Ok(Opened::Digital(DigitalFile {
        file,
        version,
        initial: initial == 1,
        rate,
        begin,
        end,
        count,
        header,
    }))
}

#[allow(clippy::too_many_arguments)]
fn analog(
    file: File,
    version: Version,
    begin: f64,
    trigger: Option<f64>,
    rate: f64,
    downsample: i64,
    count: u64,
    header: usize,
) -> Result<Opened, Error> {
    if downsample != 1 {
        return Err(file.unsupported(format!("analog downsample {downsample}")));
    }
    let rate = file.check_rate(rate)?;
    Ok(Opened::Analog(AnalogFile {
        file,
        version,
        begin,
        trigger,
        rate,
        count,
        header,
    }))
}

impl DigitalFile {
    fn end_tick(&self, rate: SampleRate) -> i64 {
        tick_of(self.end, self.begin, rate).round() as i64
    }

    fn times(&self) -> impl Iterator<Item = f64> + Send + use<> {
        self.file
            .items::<8>(self.header, self.count)
            .map(f64::from_le_bytes)
    }

    /// Checks every edge: on the grid, strictly increasing, inside the capture. `step` hears of
    /// each run of `STEP` edges checked.
    fn check(&self, rate: SampleRate, step: &mut dyn FnMut(u64)) -> Result<(), Error> {
        let path = || self.file.path.clone();
        let end = self.end_tick(rate);
        let mut previous = 0;
        for (index, time) in (0..).zip(self.times()) {
            if index % STEP == STEP - 1 {
                step(STEP);
            }
            let exact = tick_of(time, self.begin, rate);
            let tick = exact.round();
            let residual = (exact - tick).abs();
            if residual.is_nan() || residual > GRID_TOLERANCE {
                return Err(Error::OffGrid {
                    path: path(),
                    index,
                    residual,
                });
            }
            let tick = tick as i64;
            if tick < 1 || tick > end {
                return Err(Error::OutOfCapture {
                    path: path(),
                    index,
                    tick,
                    end,
                });
            }
            if tick <= previous {
                return Err(Error::NotIncreasing {
                    path: path(),
                    index,
                    tick,
                    previous,
                });
            }
            previous = tick;
        }
        Ok(())
    }

    fn edges(&self, rate: SampleRate) -> impl Iterator<Item = i64> + Send + use<> {
        let begin = self.begin;
        self.times()
            .map(move |t| tick_of(t, begin, rate).round() as i64)
    }
}

/// Whether `times` all sit on `rate`'s grid from `begin`, as `DigitalFile::check` wants them:
/// each on a tick of its own, from tick 1, in order.
fn fits(times: impl Iterator<Item = f64>, begin: f64, rate: SampleRate) -> bool {
    let mut previous = 0.0;
    for t in times {
        let exact = tick_of(t, begin, rate);
        let tick = exact.round();
        if (exact - tick).abs() > GRID_TOLERANCE || tick < 1.0 || tick <= previous {
            return false;
        }
        previous = tick;
    }
    true
}

/// The lowest divisor of 1 GHz on whose grid every edge of `pool` sits, from `begin`. A rate
/// below the capture's puts some edge off its grid or two edges on one tick, and one above it
/// fits as well as the capture's own, so the lowest that fits is the capture's rate or a divisor
/// of it. `step` hears of each run of edges read in full passes.
fn infer_rate(
    pool: &[&DigitalFile],
    begin: f64,
    step: &mut dyn FnMut(u64),
) -> Result<SampleRate, Error> {
    if pool.iter().all(|d| d.count == 0) {
        return Err(Error::NoRate("the export has no edges to infer it from"));
    }
    let mut candidates: Vec<SampleRate> = (0..10)
        .flat_map(|a| (0..10).map(move |b| 2u64.pow(a) * 5u64.pow(b)))
        .filter(|&r| r <= MOST_INFERRED)
        .filter_map(SampleRate::new)
        .collect();
    candidates.sort_unstable_by_key(|r| r.hz());
    // A wrong rate fails within the first few edges, so each is tried on the first edges of
    // every file before it is tried on all of them.
    let probes: Vec<Vec<f64>> = pool
        .iter()
        .map(|d| {
            Items::<8>::with_chunk(
                Arc::clone(&d.file.input),
                d.header as u64,
                d.count.min(PROBE),
                PROBE as usize,
            )
            .map(f64::from_le_bytes)
            .collect()
        })
        .collect();
    for rate in candidates {
        if !probes.iter().all(|p| fits(p.iter().copied(), begin, rate)) {
            continue;
        }
        let mut read = 0;
        let mut counted = |t| {
            read += 1;
            if read % STEP == 0 {
                step(STEP);
            }
            t
        };
        if pool
            .iter()
            .all(|d| fits(d.times().map(&mut counted), begin, rate))
        {
            return Ok(rate);
        }
    }
    Err(Error::NoRate(
        "its edges sit on no grid whose rate divides 1 GHz",
    ))
}

/// What one file of an export holds, from its header.
#[derive(Clone, Debug)]
pub struct FileInfo {
    pub analog: bool,
    /// Edges in a digital file, samples in an analog one.
    pub count: u64,
    /// Where the file records it: a version 0 digital file does not.
    pub rate: Option<SampleRate>,
}

/// Reads one file's header and checks it as `read` does, without reading its edges or samples.
pub fn file_info(path: PathBuf, input: Arc<dyn Input>) -> Result<FileInfo, Error> {
    Ok(match open(path, input)? {
        Opened::Digital(d) => FileInfo {
            analog: false,
            count: d.count,
            rate: d.rate,
        },
        Opened::Analog(a) => FileInfo {
            analog: true,
            count: a.count,
            rate: Some(a.rate),
        },
    })
}

/// Edges checked between two reports.
const STEP: u64 = 1 << 16;

/// The name of a channel's file in an export, from its stem: `digital_3_0.bin`.
pub fn file_name(stem: &str) -> String {
    format!("{stem}.bin")
}

/// Reads the named channels of the export in `dir`. Each entry is the file's stem
/// (`digital_3_0`, `analog_0`) and the name to give the channel. `rate` is the sample rate in
/// hertz, if given (see [`Headers::read`]). Every file is checked whole before this returns, so
/// the iterators cannot fail.
pub fn read(
    dir: &Path,
    channels: &[(String, String)],
    rate: Option<SampleRate>,
) -> Result<Recording, Error> {
    read_files(export_files(dir)?, channels, rate)
}

/// A file of an export, opened, with the path its errors show.
pub type ExportFile = (PathBuf, Arc<dyn Input>);

/// The `.bin` files in `dir`, each opened.
pub fn export_files(dir: &Path) -> Result<Vec<ExportFile>, Error> {
    let io = |source| Error::Io {
        path: dir.to_owned(),
        source,
    };
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(io)? {
        let path = entry.map_err(io)?.path();
        if path.extension().is_some_and(|e| e == "bin") && path.is_file() {
            match input::map(&path) {
                Ok(input) => files.push((path, input)),
                Err(source) => return Err(Error::Io { path, source }),
            }
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}

/// Reads the named channels, as `read` does, from the export's files already opened: each with
/// the path errors show, named as `file_name` names it.
pub fn read_files(
    files: Vec<ExportFile>,
    channels: &[(String, String)],
    rate: Option<SampleRate>,
) -> Result<Recording, Error> {
    Ok(Headers::read(files, channels, rate)?
        .check(&mut |_| {})?
        .recording())
}

/// Where the sample rate came from, when no file the recording takes records it.
#[derive(Clone, Copy)]
enum RateFrom {
    File,
    Given,
    Inferred,
}

/// An export's named channels, their headers read and checked against each other. Its edges are
/// not read yet: `check` reads them all, and gives the `Export` that recordings come from.
pub struct Headers {
    files: Vec<Opened>,
    channels: Vec<(String, String)>,
    /// The export's other version 0 digital files, from the same begin: their edges count when a
    /// rate is inferred.
    others: Vec<DigitalFile>,
    version: Version,
    /// Known unless it is to be inferred.
    rate: Option<SampleRate>,
    from: RateFrom,
    begin: f64,
}

impl Headers {
    /// Opens the named channels' files among `files`, every `.bin` file of the export, opened as
    /// for `read_files`. Tick 0 is the digital channels' `begin`, or the first analog channel's
    /// when there are no digital ones. Analog channels must sit on that grid, at the same sample
    /// rate.
    ///
    /// The sample rate is `rate`, if given, which every file that records one must match. Without
    /// it, it is the rate a named file records. A version 0 digital file records none, so where
    /// every named file is one, `check` infers it from the edges of every version 0 digital file
    /// in the export: the lowest divisor of 1 GHz on whose grid they all sit.
    pub fn read(
        files: Vec<ExportFile>,
        channels: &[(String, String)],
        rate: Option<SampleRate>,
    ) -> Result<Self, Error> {
        if channels.is_empty() {
            return Err(Error::NoChannels);
        }
        let mut rest = files;
        let mut named = Vec::with_capacity(channels.len());
        for (stem, _) in channels {
            let name = file_name(stem);
            let at = rest
                .iter()
                .position(|(p, _)| p.file_name().is_some_and(|n| *n == *name))
                .ok_or(Error::NoFile(name))?;
            let (path, input) = rest.swap_remove(at);
            named.push(open(path, input)?);
        }
        let files = named;
        let version = files[0].version();
        let (begin, reference) = match files.iter().find_map(|f| match f {
            Opened::Digital(d) => Some(d),
            Opened::Analog(_) => None,
        }) {
            Some(d) => (d.begin, &d.file.path),
            None => {
                let Opened::Analog(a) = &files[0] else {
                    unreachable!("no digital file was found")
                };
                (a.begin, &a.file.path)
            }
        };
        let mismatch = |path: &PathBuf, what| Error::Mismatch {
            path: path.clone(),
            first: reference.clone(),
            what,
        };
        let recorded = files.iter().find_map(Opened::rate);
        for f in &files {
            if f.version() != version {
                return Err(mismatch(&f.file().path, "export version"));
            }
            if let (Some(r), Some(first)) = (f.rate(), recorded)
                && r != first
            {
                return Err(mismatch(&f.file().path, "sample rate"));
            }
            if let Opened::Digital(d) = f
                && d.begin.to_bits() != begin.to_bits()
            {
                return Err(mismatch(&d.file.path, "begin time"));
            }
        }
        let (rate, from) = match (rate, recorded) {
            (Some(given), _) => {
                if let (Some(r), Some(f)) = (recorded, files.iter().find(|f| f.rate().is_some()))
                    && r != given
                {
                    return Err(Error::RateDiffers {
                        path: f.file().path.clone(),
                        recorded: r,
                        given,
                    });
                }
                (Some(given), RateFrom::Given)
            }
            (None, Some(r)) => (Some(r), RateFrom::File),
            (None, None) => (None, RateFrom::Inferred),
        };
        // Only a rate to infer reads the rest of the export: its version 0 digital files from the
        // same begin. Any other file, Saleae's or not, is no channel of this recording.
        let others = if rate.is_none() {
            rest.into_iter()
                .filter_map(|(path, input)| match open(path, input) {
                    Ok(Opened::Digital(d))
                        if d.version == Version::V0 && d.begin.to_bits() == begin.to_bits() =>
                    {
                        Some(d)
                    }
                    _ => None,
                })
                .collect()
        } else {
            Vec::new()
        };
        Ok(Self {
            files,
            channels: channels.to_vec(),
            others,
            version,
            rate,
            from,
            begin,
        })
    }

    fn digital(&self) -> impl Iterator<Item = &DigitalFile> {
        self.files.iter().filter_map(|f| match f {
            Opened::Digital(d) => Some(d),
            Opened::Analog(_) => None,
        })
    }

    /// The edges `check` reads: every named digital file's, and, when the rate is inferred, every
    /// version 0 digital file's in the export.
    pub fn edges(&self) -> u64 {
        let named: u64 = self.digital().map(|d| d.count).sum();
        match self.rate {
            Some(_) => named,
            None => named * 2 + self.others.iter().map(|d| d.count).sum::<u64>(),
        }
    }

    /// Infers the sample rate if no file records it and none was given, then reads and checks
    /// every edge. `step` hears of each run of edges read.
    pub fn check(self, step: &mut dyn FnMut(u64)) -> Result<Export, Error> {
        let rate = match self.rate {
            Some(rate) => rate,
            None => {
                let pool: Vec<&DigitalFile> = self.digital().chain(&self.others).collect();
                infer_rate(&pool, self.begin, step)?
            }
        };
        let mut length = 0;
        let mut firsts = Vec::with_capacity(self.files.len());
        let mut trigger: Option<(f64, &PathBuf)> = None;
        for opened in &self.files {
            match opened {
                Opened::Digital(d) => {
                    length = length.max(d.end_tick(rate));
                    firsts.push(0);
                }
                Opened::Analog(a) => {
                    let exact = tick_of(a.begin, self.begin, rate);
                    let first = exact.round();
                    let residual = (exact - first).abs();
                    if residual.is_nan() || residual > GRID_TOLERANCE {
                        return Err(Error::AnalogOffGrid {
                            path: a.file.path.clone(),
                            residual,
                        });
                    }
                    if let Some(t) = a.trigger {
                        match trigger {
                            Some((first, path)) if first.to_bits() != t.to_bits() => {
                                return Err(Error::Mismatch {
                                    path: a.file.path.clone(),
                                    first: path.clone(),
                                    what: "trigger time",
                                });
                            }
                            Some(_) => {}
                            None => trigger = Some((t, &a.file.path)),
                        }
                    }
                    let first = first as i64;
                    length = length.max(first + a.count as i64);
                    firsts.push(first);
                }
            }
        }
        let trigger = trigger.map(|(t, _)| t);
        for d in self.digital() {
            d.check(rate, step)?;
        }
        Ok(Export {
            headers: self,
            rate,
            firsts,
            trigger,
            length,
        })
    }
}

/// An export whose every edge has been checked, so the iterators of the recordings it gives
/// cannot fail. It gives a fresh recording for each pass of a conversion.
pub struct Export {
    headers: Headers,
    rate: SampleRate,
    /// Each file's first tick: an analog file's first sample, 0 for a digital file.
    firsts: Vec<i64>,
    trigger: Option<f64>,
    length: i64,
}

impl Export {
    pub fn recording(&self) -> Recording {
        let h = &self.headers;
        let rate = self.rate;
        let channels = h
            .files
            .iter()
            .zip(&h.channels)
            .zip(&self.firsts)
            .map(|((opened, (stem, name)), &first)| Channel {
                name: name.clone(),
                source_name: stem.clone(),
                data: match opened {
                    Opened::Digital(d) => ChannelData::Digital(Digital {
                        initial: d.initial,
                        end: d.end_tick(rate),
                        edges: Box::new(d.edges(rate)),
                    }),
                    Opened::Analog(a) => ChannelData::Analog(Analog {
                        unit: "V".into(),
                        first,
                        period: Ratio::ONE,
                        count: a.count,
                        samples: Samples::F32(Box::new(
                            a.file.items::<4>(a.header, a.count).map(f32::from_le_bytes),
                        )),
                    }),
                },
            })
            .collect();
        let format = match h.version {
            Version::V0 => SourceFormat::SaleaeBinary0,
            Version::V1 => SourceFormat::SaleaeBinary1,
        };
        let source = Source {
            files: h
                .files
                .iter()
                .filter_map(|f| f.file().path.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .collect(),
            begin_s: Some(h.begin),
            trigger_s: self.trigger,
            rate: match h.from {
                RateFrom::File => None,
                RateFrom::Given => Some(RateOrigin::Given),
                RateFrom::Inferred => Some(RateOrigin::Inferred),
            },
            ..Source::new(format)
        };
        Recording {
            timebase: Ratio::from(rate),
            length: self.length,
            channels,
            source,
        }
    }
}
