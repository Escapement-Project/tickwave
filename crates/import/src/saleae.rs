//! Saleae Logic 2's binary export, version 1: a directory with one file a channel, as Logic 2 and
//! its headless server write them.
//!
//! Every file, little-endian: `<SALEAE>`, i32 version 1, i32 type, i64 chunk count, then one
//! chunk (files with more are refused).
//! - Digital (type 0): u32 initial level, f64 sample rate, f64 begin, f64 end, i64 count, then
//!   `count` f64 transition times in seconds.
//! - Analog (type 1): f64 begin, f64 trigger, f64 sample rate, u64 downsample, u64 count, then
//!   `count` f32 volts. Only a downsample of 1 is read: no export at another has been checked.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tickwave_format::model::{
    Analog, Channel, ChannelData, Digital, Ratio, Recording, Samples, Source,
};

use crate::input::{self, Input, Items};

const MAGIC: &[u8; 8] = b"<SALEAE>";
const DIGITAL_HEADER: usize = 60;
const ANALOG_HEADER: usize = 64;

/// How far an edge may sit from the sample grid, in samples, before the source is refused. The
/// recordings measured so far sit within 6.4e-4.
pub const GRID_TOLERANCE: f64 = 0.01;

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
    #[error("no channels named")]
    NoChannels,
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
    fn open(path: PathBuf, input: Arc<dyn Input>) -> Result<(Self, i32), Error> {
        let size = input.size();
        let mut head = vec![0; size.min(ANALOG_HEADER as u64) as usize];
        input.read_at(0, &mut head);
        if head.len() < 24 || &head[..8] != MAGIC {
            return Err(Error::NotSaleae { path });
        }
        let file = Self {
            path,
            input,
            size,
            head,
        };
        let (version, kind, chunks) = (file.i32_at(8), file.i32_at(12), file.i64_at(16));
        if version != 1 {
            return Err(file.unsupported(format!("export version {version}")));
        }
        if chunks != 1 {
            return Err(file.unsupported(format!("{chunks} chunks")));
        }
        Ok((file, kind))
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

    fn check_rate(&self, rate: f64) -> Result<(), Error> {
        if rate >= 1.0 && rate.fract() == 0.0 && rate < 2f64.powi(53) {
            Ok(())
        } else {
            Err(Error::Rate {
                path: self.path.clone(),
                rate,
            })
        }
    }
}

/// One digital file, its header checked.
struct DigitalFile {
    file: File,
    initial: bool,
    rate: f64,
    begin: f64,
    end: f64,
    count: u64,
}

/// One analog file, its header checked.
struct AnalogFile {
    file: File,
    begin: f64,
    trigger: f64,
    rate: f64,
    count: u64,
}

enum Opened {
    Digital(DigitalFile),
    Analog(AnalogFile),
}

fn tick_of(t: f64, begin: f64, rate: f64) -> f64 {
    (t - begin) * rate
}

fn open(path: PathBuf, input: Arc<dyn Input>) -> Result<Opened, Error> {
    let (file, kind) = File::open(path, input)?;
    match kind {
        0 => {
            file.need(DIGITAL_HEADER)?;
            let initial = u32::from_le_bytes(file.head[24..28].try_into().unwrap());
            let (rate, begin, end) = (file.f64_at(28), file.f64_at(36), file.f64_at(44));
            let count = file.check_length(DIGITAL_HEADER, file.i64_at(52), 8)?;
            if initial > 1 {
                return Err(file.unsupported(format!("initial state {initial}")));
            }
            file.check_rate(rate)?;
            Ok(Opened::Digital(DigitalFile {
                file,
                initial: initial == 1,
                rate,
                begin,
                end,
                count,
            }))
        }
        1 => {
            file.need(ANALOG_HEADER)?;
            let (begin, trigger, rate) = (file.f64_at(24), file.f64_at(32), file.f64_at(40));
            let downsample = file.i64_at(48);
            let count = file.check_length(ANALOG_HEADER, file.i64_at(56), 4)?;
            if downsample != 1 {
                return Err(file.unsupported(format!("analog downsample {downsample}")));
            }
            file.check_rate(rate)?;
            Ok(Opened::Analog(AnalogFile {
                file,
                begin,
                trigger,
                rate,
                count,
            }))
        }
        kind => Err(file.unsupported(format!("channel type {kind}"))),
    }
}

impl DigitalFile {
    fn end_tick(&self) -> i64 {
        tick_of(self.end, self.begin, self.rate).round() as i64
    }

    /// Checks every edge: on the grid, strictly increasing, inside the capture. `step` hears of
    /// each run of `STEP` edges checked.
    fn check(&self, step: &mut dyn FnMut(u64)) -> Result<(), Error> {
        let path = || self.file.path.clone();
        let end = self.end_tick();
        let mut previous = 0;
        let times = self.file.items::<8>(DIGITAL_HEADER, self.count);
        for (index, time) in (0..).zip(times) {
            if index % STEP == STEP - 1 {
                step(STEP);
            }
            let exact = tick_of(f64::from_le_bytes(time), self.begin, self.rate);
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

    fn edges(&self) -> impl Iterator<Item = i64> + Send + use<> {
        let (begin, rate) = (self.begin, self.rate);
        self.file
            .items::<8>(DIGITAL_HEADER, self.count)
            .map(move |t| tick_of(f64::from_le_bytes(t), begin, rate).round() as i64)
    }
}

/// What one file of an export holds, from its header.
#[derive(Clone, Debug)]
pub struct FileInfo {
    pub analog: bool,
    /// Edges in a digital file, samples in an analog one.
    pub count: u64,
    /// Samples a second.
    pub rate: f64,
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
            rate: a.rate,
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
/// (`digital_3_0`, `analog_0`) and the name to give the channel. Every file is checked whole
/// before this returns, so the iterators cannot fail.
///
/// Tick 0 is the digital channels' `begin`, or the first analog channel's when there are no
/// digital ones. Analog channels must sit on that grid, at the same sample rate.
pub fn read(dir: &Path, channels: &[(String, String)]) -> Result<Recording, Error> {
    if channels.is_empty() {
        return Err(Error::NoChannels);
    }
    let files = channels
        .iter()
        .map(|(stem, _)| {
            let path = dir.join(file_name(stem));
            match input::map(&path) {
                Ok(input) => Ok((path, input)),
                Err(source) => Err(Error::Io { path, source }),
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    read_files(files, channels)
}

/// Reads the named channels, as `read` does, from their files already opened: `files[i]` holds
/// `channels[i]`'s, named as `file_name` names it, with the path errors show.
pub fn read_files(
    files: Vec<(PathBuf, Arc<dyn Input>)>,
    channels: &[(String, String)],
) -> Result<Recording, Error> {
    Ok(Headers::read(files, channels)?
        .check(&mut |_| {})?
        .recording())
}

/// An export's named channels, their headers read and checked against each other. Its edges are
/// not read yet: `check` reads them all, and gives the `Export` that recordings come from.
pub struct Headers {
    files: Vec<Opened>,
    channels: Vec<(String, String)>,
    /// Each file's first tick: an analog file's first sample, 0 for a digital file.
    firsts: Vec<i64>,
    rate: f64,
    begin: f64,
    trigger: Option<f64>,
    length: i64,
}

impl Headers {
    /// Opens the named channels' files, already opened as for `read_files`. Tick 0 is the digital
    /// channels' `begin`, or the first analog channel's when there are no digital ones. Analog
    /// channels must sit on that grid, at the same sample rate.
    pub fn read(
        files: Vec<(PathBuf, Arc<dyn Input>)>,
        channels: &[(String, String)],
    ) -> Result<Self, Error> {
        if channels.is_empty() {
            return Err(Error::NoChannels);
        }
        assert_eq!(files.len(), channels.len(), "one file a channel");
        let files = files
            .into_iter()
            .map(|(path, input)| open(path, input))
            .collect::<Result<Vec<_>, _>>()?;
        let (rate, begin, reference) = match files.iter().find_map(|f| match f {
            Opened::Digital(d) => Some(d),
            Opened::Analog(_) => None,
        }) {
            Some(d) => (d.rate, d.begin, &d.file.path),
            None => {
                let Opened::Analog(a) = &files[0] else {
                    unreachable!("no digital file was found")
                };
                (a.rate, a.begin, &a.file.path)
            }
        };
        let mut length = 0;
        let mut firsts = Vec::with_capacity(files.len());
        let mut trigger = None;
        for opened in &files {
            let mismatch = |path: &PathBuf, what| Error::Mismatch {
                path: path.clone(),
                first: reference.clone(),
                what,
            };
            match opened {
                Opened::Digital(d) => {
                    if d.rate.to_bits() != rate.to_bits() {
                        return Err(mismatch(&d.file.path, "sample rate"));
                    }
                    if d.begin.to_bits() != begin.to_bits() {
                        return Err(mismatch(&d.file.path, "begin time"));
                    }
                    length = length.max(d.end_tick());
                    firsts.push(0);
                }
                Opened::Analog(a) => {
                    if a.rate.to_bits() != rate.to_bits() {
                        return Err(mismatch(&a.file.path, "sample rate"));
                    }
                    let exact = tick_of(a.begin, begin, rate);
                    let first = exact.round();
                    let residual = (exact - first).abs();
                    if residual.is_nan() || residual > GRID_TOLERANCE {
                        return Err(Error::AnalogOffGrid {
                            path: a.file.path.clone(),
                            residual,
                        });
                    }
                    if trigger.is_some_and(|t: f64| t.to_bits() != a.trigger.to_bits()) {
                        return Err(mismatch(&a.file.path, "trigger time"));
                    }
                    trigger = Some(a.trigger);
                    let first = first as i64;
                    length = length.max(first + a.count as i64);
                    firsts.push(first);
                }
            }
        }
        Ok(Self {
            files,
            channels: channels.to_vec(),
            firsts,
            rate,
            begin,
            trigger,
            length,
        })
    }

    /// The edges `check` reads, in every digital file.
    pub fn edges(&self) -> u64 {
        self.files
            .iter()
            .map(|f| match f {
                Opened::Digital(d) => d.count,
                Opened::Analog(_) => 0,
            })
            .sum()
    }

    /// Reads and checks every edge. `step` hears of each run of edges checked.
    pub fn check(self, step: &mut dyn FnMut(u64)) -> Result<Export, Error> {
        for f in &self.files {
            if let Opened::Digital(d) = f {
                d.check(step)?;
            }
        }
        Ok(Export(self))
    }
}

/// An export whose every edge has been checked, so the iterators of the recordings it gives
/// cannot fail. It gives a fresh recording for each pass of a conversion.
pub struct Export(Headers);

impl Export {
    pub fn recording(&self) -> Recording {
        let h = &self.0;
        let channels = h
            .files
            .iter()
            .zip(&h.channels)
            .zip(&h.firsts)
            .map(|((opened, (stem, name)), &first)| Channel {
                name: name.clone(),
                source_name: stem.clone(),
                data: match opened {
                    Opened::Digital(d) => ChannelData::Digital(Digital {
                        initial: d.initial,
                        end: d.end_tick(),
                        edges: Box::new(d.edges()),
                    }),
                    Opened::Analog(a) => ChannelData::Analog(Analog {
                        unit: "V".into(),
                        first,
                        period: Ratio::integer(1),
                        count: a.count,
                        samples: Samples::F32(Box::new(
                            a.file
                                .items::<4>(ANALOG_HEADER, a.count)
                                .map(f32::from_le_bytes),
                        )),
                    }),
                },
            })
            .collect();
        let mut source = Source {
            format: "saleae-binary-1".into(),
            files: h
                .files
                .iter()
                .map(|f| match f {
                    Opened::Digital(d) => &d.file.path,
                    Opened::Analog(a) => &a.file.path,
                })
                .filter_map(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .collect(),
            begin_s: Some(h.begin),
            ..Source::default()
        };
        if let Some(t) = h.trigger {
            source.extra.insert("trigger_s".into(), t.to_string());
        }
        Recording {
            timebase: Ratio::integer(h.rate as u64),
            length: h.length,
            channels,
            source,
        }
    }
}
