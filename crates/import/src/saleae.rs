//! Saleae Logic 2's binary export, version 1: a directory with one file a channel, as Logic 2 and
//! its headless server write them.
//!
//! Every file, little-endian: `<SALEAE>`, i32 version 1, i32 type, i64 chunk count, then one
//! chunk (files with more are refused).
//! - Digital (type 0): u32 initial level, f64 sample rate, f64 begin, f64 end, i64 count, then
//!   `count` f64 transition times in seconds.
//! - Analog (type 1): f64 begin, f64 trigger, f64 sample rate, u64 downsample, u64 count, then
//!   `count` f32 volts. Only a downsample of 1 is read: no export at another has been checked.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono_format::model::{
    Analog, Channel, ChannelData, Digital, Ratio, Recording, Samples, Source,
};
use memmap2::Mmap;

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
        len: usize,
        expected: u128,
    },
    #[error("{path}: sample rate {rate} is not a whole number of hertz")]
    Rate { path: PathBuf, rate: f64 },
    #[error("{path}: begins {residual:.3} samples off the digital channels' sample grid")]
    AnalogOffGrid { path: PathBuf, residual: f64 },
    #[error("{path}: edge {index} lies {residual:.3} samples off the sample grid")]
    OffGrid {
        path: PathBuf,
        index: usize,
        residual: f64,
    },
    #[error("{path}: edge {index} at tick {tick} does not follow tick {previous}")]
    NotIncreasing {
        path: PathBuf,
        index: usize,
        tick: i64,
        previous: i64,
    },
    #[error("{path}: edge {index} at tick {tick} lies outside the capture, ticks 1 to {end}")]
    OutOfCapture {
        path: PathBuf,
        index: usize,
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

/// A mapped file whose header has been read.
struct Mapped {
    path: PathBuf,
    map: Arc<Mmap>,
}

impl Mapped {
    fn open(path: &Path) -> Result<(Self, i32), Error> {
        let io = |source| Error::Io {
            path: path.to_owned(),
            source,
        };
        let file = File::open(path).map_err(io)?;
        // SAFETY: nothing may truncate or write the export while it is mapped. chrono never
        // writes its input, and a converter run over a finished export holds that.
        let map = unsafe { Mmap::map(&file) }.map_err(io)?;
        let path = path.to_owned();
        if map.len() < 24 || &map[..8] != MAGIC {
            return Err(Error::NotSaleae { path });
        }
        let mapped = Self {
            path,
            map: Arc::new(map),
        };
        let (version, kind, chunks) = (mapped.i32_at(8), mapped.i32_at(12), mapped.i64_at(16));
        if version != 1 {
            return Err(mapped.unsupported(format!("export version {version}")));
        }
        if chunks != 1 {
            return Err(mapped.unsupported(format!("{chunks} chunks")));
        }
        Ok((mapped, kind))
    }

    fn i32_at(&self, at: usize) -> i32 {
        i32::from_le_bytes(self.map[at..at + 4].try_into().unwrap())
    }

    fn i64_at(&self, at: usize) -> i64 {
        i64::from_le_bytes(self.map[at..at + 8].try_into().unwrap())
    }

    fn f64_at(&self, at: usize) -> f64 {
        f64::from_le_bytes(self.map[at..at + 8].try_into().unwrap())
    }

    fn unsupported(&self, what: String) -> Error {
        Error::Unsupported {
            path: self.path.clone(),
            what,
        }
    }

    fn need(&self, header: usize) -> Result<(), Error> {
        if self.map.len() < header {
            return Err(Error::Length {
                path: self.path.clone(),
                len: self.map.len(),
                expected: header as u128,
            });
        }
        Ok(())
    }

    /// Checks the file holds its header and exactly `count` items of `size` bytes after it.
    fn check_length(&self, header: usize, count: i64, size: u128) -> Result<usize, Error> {
        let expected = header as u128 + size * count.max(0) as u128;
        if count < 0 || self.map.len() as u128 != expected {
            return Err(Error::Length {
                path: self.path.clone(),
                len: self.map.len(),
                expected,
            });
        }
        Ok(count as usize)
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

/// One digital file, mapped and checked.
struct DigitalFile {
    file: Mapped,
    initial: bool,
    rate: f64,
    begin: f64,
    end: f64,
    count: usize,
}

/// One analog file, mapped and checked.
struct AnalogFile {
    file: Mapped,
    begin: f64,
    trigger: f64,
    rate: f64,
    count: usize,
}

enum Opened {
    Digital(DigitalFile),
    Analog(AnalogFile),
}

fn tick_of(t: f64, begin: f64, rate: f64) -> f64 {
    (t - begin) * rate
}

fn time_at(map: &Mmap, index: usize) -> f64 {
    let at = DIGITAL_HEADER + 8 * index;
    f64::from_le_bytes(map[at..at + 8].try_into().unwrap())
}

fn open(path: &Path) -> Result<Opened, Error> {
    let (file, kind) = Mapped::open(path)?;
    match kind {
        0 => {
            file.need(DIGITAL_HEADER)?;
            let initial = u32::from_le_bytes(file.map[24..28].try_into().unwrap());
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

    /// Checks every edge: on the grid, strictly increasing, inside the capture.
    fn check(&self) -> Result<(), Error> {
        let path = || self.file.path.clone();
        let end = self.end_tick();
        let mut previous = 0;
        for index in 0..self.count {
            let exact = tick_of(time_at(&self.file.map, index), self.begin, self.rate);
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

    fn edges(&self) -> EdgeIter {
        EdgeIter {
            map: Arc::clone(&self.file.map),
            next: 0,
            count: self.count,
            begin: self.begin,
            rate: self.rate,
        }
    }
}

struct EdgeIter {
    map: Arc<Mmap>,
    next: usize,
    count: usize,
    begin: f64,
    rate: f64,
}

impl Iterator for EdgeIter {
    type Item = i64;

    fn next(&mut self) -> Option<i64> {
        if self.next == self.count {
            return None;
        }
        let t = time_at(&self.map, self.next);
        self.next += 1;
        Some(tick_of(t, self.begin, self.rate).round() as i64)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = self.count - self.next;
        (left, Some(left))
    }
}

struct SampleIter {
    map: Arc<Mmap>,
    next: usize,
    count: usize,
}

impl Iterator for SampleIter {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.next == self.count {
            return None;
        }
        let at = ANALOG_HEADER + 4 * self.next;
        self.next += 1;
        Some(f32::from_le_bytes(self.map[at..at + 4].try_into().unwrap()))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = self.count - self.next;
        (left, Some(left))
    }
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
        .map(|(stem, _)| open(&dir.join(format!("{stem}.bin"))))
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
    let mut out = Vec::with_capacity(files.len());
    let mut trigger = None;
    for (opened, (stem, name)) in files.iter().zip(channels) {
        let mismatch = |path: &PathBuf, what| Error::Mismatch {
            path: path.clone(),
            first: reference.clone(),
            what,
        };
        let data = match opened {
            Opened::Digital(d) => {
                if d.rate.to_bits() != rate.to_bits() {
                    return Err(mismatch(&d.file.path, "sample rate"));
                }
                if d.begin.to_bits() != begin.to_bits() {
                    return Err(mismatch(&d.file.path, "begin time"));
                }
                d.check()?;
                length = length.max(d.end_tick());
                ChannelData::Digital(Digital {
                    initial: d.initial,
                    end: d.end_tick(),
                    edges: Box::new(d.edges()),
                })
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
                ChannelData::Analog(Analog {
                    unit: "V".into(),
                    first,
                    period: Ratio::integer(1),
                    count: a.count as u64,
                    samples: Samples::F32(Box::new(SampleIter {
                        map: Arc::clone(&a.file.map),
                        next: 0,
                        count: a.count,
                    })),
                })
            }
        };
        out.push(Channel {
            name: name.clone(),
            source_name: stem.clone(),
            data,
        });
    }
    let mut source = Source {
        format: "saleae-binary-1".into(),
        files: files
            .iter()
            .map(|f| match f {
                Opened::Digital(d) => &d.file.path,
                Opened::Analog(a) => &a.file.path,
            })
            .filter_map(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .collect(),
        begin_s: Some(begin),
        ..Source::default()
    };
    if let Some(t) = trigger {
        source.extra.insert("trigger_s".into(), t.to_string());
    }
    Ok(Recording {
        timebase: Ratio::integer(rate as u64),
        length,
        channels: out,
        source,
    })
}
