//! A conversion's passes, which the CLI and the browser share, so both write the same bytes from
//! the same source: exact analog codes, the encoding, and a check of the output against the
//! source.

use std::collections::BTreeMap;
use std::sync::Arc;

use escapement_tickwave_format::decode::{VerifyError, verify_limited};
use escapement_tickwave_format::encode::{EncodeError, Options, encode};
use escapement_tickwave_format::header::AnalogCoding;
use escapement_tickwave_format::model::{Channel, ChannelData, Recording, Samples};

use crate::{codes, saleae, sigrok, vcd};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Saleae(#[from] saleae::Error),
    #[error(transparent)]
    Sigrok(#[from] sigrok::Error),
    #[error(transparent)]
    Vcd(#[from] vcd::Error),
    #[error("no channel {name:?}; the source has {has:?}")]
    NoChannel { name: String, has: Vec<String> },
    #[error("a unit for {0:?}: no channel by that name")]
    UnitNoChannel(String),
    #[error("a unit for {0}: it is digital")]
    UnitOnDigital(String),
    #[error("encoding: {0}")]
    Encode(#[from] EncodeError),
    #[error("the output does not match the source: {0}")]
    Verify(#[from] VerifyError),
}

/// What a conversion is asked for, beyond the source and its channels.
#[derive(Clone, Debug)]
pub struct Settings {
    pub title: Option<String>,
    /// Free metadata, such as `probe = verge`.
    pub meta: BTreeMap<String, String>,
    /// The zstd level.
    pub level: i32,
    /// The decoded size each block aims for, in KiB.
    pub block_kib: u32,
    /// Analog channels' units, by channel name, where the source does not state one.
    pub units: Vec<(String, String)>,
    /// Store analog values as exact integer codes where some reproduce them.
    pub codes: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            title: None,
            meta: BTreeMap::new(),
            level: 19,
            block_kib: 1024,
            units: Vec::new(),
            codes: true,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Pass<'a> {
    /// Reading a Saleae export's edges to check them, once, before the passes below.
    Check,
    /// Looking for one analog channel's exact codes.
    Codes(&'a str),
    Encode,
    /// Checking the output against the source.
    Verify,
}

/// Called with each run of items a pass has taken.
pub type Step = Arc<dyn Fn(u64) + Send + Sync>;

/// Where a conversion reports how far it has got.
pub trait Progress {
    /// A pass starts over `total` items.
    fn start(&self, pass: Pass<'_>, total: u64) -> Step;
    /// The pass has taken its last item.
    fn end(&self, pass: Pass<'_>) {
        let _ = pass;
    }
}

/// Each analog channel's name, and the bits of its exact codes where it has some.
pub type Found = Vec<(String, Option<u8>)>;

pub struct Converted {
    /// The recording, checked against the source.
    pub file: Vec<u8>,
    pub codes: Found,
}

/// Keeps the channels named in `wanted`, in its order, renamed; all of them when it is empty.
pub fn select(recording: Recording, wanted: &[(String, String)]) -> Result<Recording, Error> {
    if wanted.is_empty() {
        return Ok(recording);
    }
    let mut channels: Vec<Option<Channel>> = recording.channels.into_iter().map(Some).collect();
    let mut out = Vec::with_capacity(wanted.len());
    for (from, to) in wanted {
        let found = channels
            .iter_mut()
            .find(|c| c.as_ref().is_some_and(|c| c.name == *from))
            .and_then(Option::take);
        let Some(mut channel) = found else {
            return Err(Error::NoChannel {
                name: from.clone(),
                has: channels.iter().flatten().map(|c| c.name.clone()).collect(),
            });
        };
        channel.name = to.clone();
        out.push(channel);
    }
    Ok(Recording {
        channels: out,
        ..recording
    })
}

/// Sets analog channels' units, by channel name.
pub fn set_units(mut recording: Recording, units: &[(String, String)]) -> Result<Recording, Error> {
    for (name, unit) in units {
        let channel = recording
            .channels
            .iter_mut()
            .find(|c| c.name == *name)
            .ok_or_else(|| Error::UnitNoChannel(name.clone()))?;
        let ChannelData::Analog(a) = &mut channel.data else {
            return Err(Error::UnitOnDigital(name.clone()));
        };
        a.unit = unit.clone();
    }
    Ok(recording)
}

/// Items taken between two reports.
const STEP: u64 = 1 << 16;

/// Wraps every channel's iterator so `progress` hears of the items taken, after starting `pass`
/// with their total.
pub fn counted(recording: Recording, progress: &dyn Progress, pass: Pass<'_>) -> Recording {
    let total: u64 = recording
        .channels
        .iter()
        .map(|c| match &c.data {
            ChannelData::Digital(d) => d.edges.size_hint().0 as u64,
            ChannelData::Analog(a) => a.count,
        })
        .sum();
    let step = progress.start(pass, total);
    let tick = |step: Step| {
        let mut n = 0u64;
        move || {
            n += 1;
            if n.is_multiple_of(STEP) {
                step(STEP);
            }
        }
    };
    let channels = recording
        .channels
        .into_iter()
        .map(|mut c| {
            c.data = match c.data {
                ChannelData::Digital(mut d) => {
                    let mut tick = tick(Arc::clone(&step));
                    d.edges = Box::new(d.edges.inspect(move |_| tick()));
                    ChannelData::Digital(d)
                }
                ChannelData::Analog(mut a) => {
                    let mut tick = tick(Arc::clone(&step));
                    let Samples::F32(samples) = a.samples;
                    a.samples = Samples::F32(Box::new(samples.inspect(move |_| tick())));
                    ChannelData::Analog(a)
                }
            };
            c
        })
        .collect();
    Recording {
        channels,
        ..recording
    }
}

/// Checks a Saleae export's edges, reporting them to `progress` as `Pass::Check`, so that every
/// pass of a conversion can then take a recording from it without checking again.
pub fn check_saleae(
    headers: saleae::Headers,
    progress: &dyn Progress,
) -> Result<saleae::Export, Error> {
    let step = progress.start(Pass::Check, headers.edges());
    let export = headers.check(&mut |n| step(n))?;
    progress.end(Pass::Check);
    Ok(export)
}

/// Finds exact integer codes for each analog channel: None for a digital one, or where the values
/// have none.
fn recover_codes(
    recording: Recording,
    progress: &dyn Progress,
) -> (Vec<Option<AnalogCoding>>, Found) {
    let mut found = Vec::new();
    let codings = recording
        .channels
        .into_iter()
        .map(|c| match c.data {
            ChannelData::Digital(_) => None,
            ChannelData::Analog(a) => {
                let name = c.name.clone();
                let one = Recording {
                    timebase: recording.timebase,
                    length: recording.length,
                    channels: vec![Channel {
                        data: ChannelData::Analog(a),
                        ..c
                    }],
                    source: Default::default(),
                };
                let pass = Pass::Codes(&name);
                let ChannelData::Analog(a) = counted(one, progress, pass).channels.remove(0).data
                else {
                    unreachable!("the channel was built analog")
                };
                let Samples::F32(samples) = a.samples;
                let codes = codes::recover_codes(samples);
                progress.end(pass);
                found.push((name.clone(), codes.as_ref().map(|k| k.bits)));
                codes.map(|k| k.coding())
            }
        })
        .collect();
    (codings, found)
}

/// Converts what `open` reads, and checks the output against the source. `open` is called once
/// for each pass: to find analog codes, to encode, and to check. `max` is the most the check
/// decodes from one frame.
pub fn convert(
    open: &dyn Fn() -> Result<Recording, Error>,
    settings: &Settings,
    max: usize,
    progress: &dyn Progress,
) -> Result<Converted, Error> {
    let open = || set_units(open()?, &settings.units);
    let (analog, codes) = if settings.codes {
        recover_codes(open()?, progress)
    } else {
        (Vec::new(), Vec::new())
    };
    let options = Options {
        level: settings.level,
        block_bytes: settings.block_kib as usize * 1024,
        title: settings.title.clone(),
        meta: settings.meta.clone(),
        converter: Some(format!(
            "tickwave {} (zstd {}, blocks of {} KiB)",
            env!("CARGO_PKG_VERSION"),
            settings.level,
            settings.block_kib
        )),
        analog,
    };
    let file = encode(counted(open()?, progress, Pass::Encode), &options)?;
    progress.end(Pass::Encode);
    verify_limited(&file, counted(open()?, progress, Pass::Verify), max)?;
    progress.end(Pass::Verify);
    Ok(Converted { file, codes })
}
