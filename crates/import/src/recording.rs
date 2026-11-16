//! A finished tickwave recording read back into the model, so it can be shaped and merged as a
//! capture is. `read` decodes every block once when the recording is opened, so a damaged one is
//! refused there. `open` decodes none: its `Check` decodes the channels a caller is about to read.
//! Each channel then decodes its blocks again as it is read, and ends at a block that no longer
//! decodes, short of the count its header states, which the encoder refuses.

use std::collections::BTreeMap;
use std::sync::Arc;

use escapement_tickwave_format::decode::{BlockChannel, BlockRef, DecodeError, Footer};
use escapement_tickwave_format::header::{AnalogCoding, ChannelKind, Header};
use escapement_tickwave_format::model::{
    Analog, Channel, ChannelData, Digital, Recording, Samples, Source,
};
use escapement_tickwave_format::samples::Values;

use crate::input::Input;

/// The bytes read first for the footer: as many as the web reader takes.
const TAIL: u64 = 32 << 10;

// The decode error is in the message, not the source too: a chained report would say it twice.
#[derive(Debug, thiserror::Error)]
#[error("the recording can't be read: {0}")]
pub struct Error(DecodeError);

impl From<DecodeError> for Error {
    fn from(e: DecodeError) -> Self {
        Self(e)
    }
}

/// The recording `input` holds, with the footer's header, which says what the model doesn't: its
/// title, meta and content id.
pub fn read(input: Arc<dyn Input>) -> Result<(Header, Recording), Error> {
    let (header, _, recording) = read_bundled(input)?;
    Ok((header, recording))
}

/// Annotation files by name, as a recording bundles them.
pub type Files = BTreeMap<String, String>;

/// `read`, with the annotation files the recording bundles.
pub fn read_bundled(input: Arc<dyn Input>) -> Result<(Header, Option<Files>, Recording), Error> {
    let opened = open(input)?;
    for c in 0..opened.header.channels.len() {
        opened.check.channel(c)?;
    }
    Ok((opened.header, opened.bundled, opened.recording))
}

/// A recording opened with no block decoded.
pub struct Opened {
    pub header: Header,
    pub bundled: Option<Files>,
    pub recording: Recording,
    pub check: Check,
}

/// Decodes an opened recording's channels before they are read, so a damaged block is refused
/// before the work that reads it starts.
#[derive(Clone)]
pub struct Check(Arc<Blocks>);

impl Check {
    /// Decodes every block of channel `c`.
    pub fn channel(&self, c: usize) -> Result<(), Error> {
        for b in self.0.refs.get(c).into_iter().flatten() {
            self.0.decode(c, b)?;
        }
        Ok(())
    }
}

/// The recording `input` holds: its footer, lists, bundle and every channel's index pages read,
/// and no block.
pub fn open(input: Arc<dyn Input>) -> Result<Opened, Error> {
    let mut footer = footer(&*input)?;
    footer.load_lists(fetch(&*input))?;
    // The sidecar's files whole: the bundle holds each generator file's first line, its parts
    // the rest.
    let bundled = footer.read_sidecar(fetch(&*input))?.map(|s| s.files);
    let refs = (0..footer.header.channels.len())
        .map(|c| footer.block_refs(c, fetch(&*input)))
        .collect::<Result<Vec<_>, DecodeError>>()?;
    let blocks = Arc::new(Blocks {
        input,
        footer,
        refs,
    });
    let header = blocks.footer.header.clone();
    let listed = "every list read when the recording was opened";
    let channels = header
        .channels
        .iter()
        .enumerate()
        .map(|(c, ch)| Channel {
            name: ch.name.clone(),
            source_name: ch.source_name.clone(),
            source: ch.source,
            shaping: ch.shaping.clone(),
            data: match &ch.data {
                ChannelKind::Digital {
                    initial,
                    end,
                    edges,
                    ..
                } => ChannelData::Digital(Digital {
                    initial: *initial,
                    end: *end,
                    gaps: blocks.footer.gaps(c).expect(listed).to_vec(),
                    count: *edges,
                    // The model holds a channel's source ticks, as the file stores them.
                    edges: Box::new(blocks.clone().items(c, |data| match data {
                        BlockChannel::Digital { edges, source, .. } => source.unwrap_or(edges),
                        BlockChannel::Analog { .. } => {
                            unreachable!("a block decodes each channel as the header has it")
                        }
                    })),
                }),
                ChannelKind::Analog {
                    unit,
                    first,
                    period,
                    phase,
                    count,
                    coding,
                    significant,
                    lost_before,
                    lost_after,
                    ..
                } => ChannelData::Analog(Analog {
                    unit: unit.clone(),
                    first: *first,
                    phase: *phase,
                    period: *period,
                    count: *count,
                    breaks: blocks.footer.breaks(c).expect(listed).to_vec(),
                    significant: *significant,
                    samples: samples(&blocks, c, *coding),
                    lost_before: *lost_before,
                    lost_after: *lost_after,
                }),
            },
        })
        .collect();
    let sources = header
        .sources
        .iter()
        .map(|s| Source {
            format: s.format.clone(),
            files: s.files.clone(),
            timebase: s.timebase,
            origin: s.origin,
            align: s.align,
            stretch: s.stretch,
            instrument: s.instrument.clone(),
            software: s.software.clone(),
            begin_s: s.begin_s,
            trigger_s: s.trigger_s,
            rate: s.rate,
            extra: s.extra.clone(),
            converter: s.converter.clone(),
            title: s.title.clone(),
            meta: s.meta.clone(),
        })
        .collect();
    let recording = Recording {
        timebase: header.timebase,
        length: header.length,
        channels,
        derived: header.derived.iter().map(|d| d.declared()).collect(),
        sources,
    };
    Ok(Opened {
        header,
        bundled,
        recording,
        check: Check(blocks),
    })
}

/// Analog channel `c`'s samples, block after block, in the type its coding stores.
fn samples(blocks: &Arc<Blocks>, c: usize, coding: AnalogCoding) -> Samples {
    const KIND: &str = "a block decodes each channel as the header has it";
    fn values(data: BlockChannel) -> Values {
        match data {
            BlockChannel::Analog { values, .. } => values,
            BlockChannel::Digital { .. } => unreachable!("{KIND}"),
        }
    }
    match coding {
        AnalogCoding::F32 | AnalogCoding::Codes { .. } => {
            Samples::F32(Box::new(blocks.clone().items(c, |d| match values(d) {
                Values::F32(v) => v,
                _ => unreachable!("{KIND}"),
            })))
        }
        AnalogCoding::F64 => Samples::F64(Box::new(blocks.clone().items(c, |d| match values(d) {
            Values::F64(v) => v,
            _ => unreachable!("{KIND}"),
        }))),
        AnalogCoding::Int(int) if int.signed => Samples::I64(
            int,
            Box::new(blocks.clone().items(c, |d| match values(d) {
                Values::I64(v) => v,
                _ => unreachable!("{KIND}"),
            })),
        ),
        AnalogCoding::Int(int) => Samples::U64(
            int,
            Box::new(blocks.clone().items(c, |d| match values(d) {
                Values::U64(v) => v,
                _ => unreachable!("{KIND}"),
            })),
        ),
    }
}

/// The footer of the recording `input` holds, read from its end.
pub fn footer(input: &dyn Input) -> Result<Footer, Error> {
    let size = input.size();
    let from = size.saturating_sub(TAIL);
    Ok(match Footer::parse(&tail(input, from), size) {
        Err(DecodeError::NeedFrom(at)) if at < from => Footer::parse(&tail(input, at), size)?,
        footer => footer?,
    })
}

/// `input`'s bytes as `Footer`'s readers fetch them, refused where they run past its end.
pub fn fetch(input: &dyn Input) -> impl FnMut(u64, u32) -> Result<Vec<u8>, DecodeError> + '_ {
    move |offset, len| {
        if offset
            .checked_add(u64::from(len))
            .is_none_or(|end| end > input.size())
        {
            return Err(DecodeError::Corrupt(format!(
                "{len} bytes at byte {offset} run past the end of the file"
            )));
        }
        let mut bytes = vec![0; len as usize];
        input.read_at(offset, &mut bytes);
        Ok(bytes)
    }
}

/// Channel `c`'s block `b` of the recording `input` holds, decoded, `footer` its footer.
pub fn block(
    input: &dyn Input,
    footer: &Footer,
    c: usize,
    b: &BlockRef,
) -> Result<BlockChannel, Error> {
    let frame = fetch(input)(b.row.offset, b.row.length)?;
    Ok(footer.decode_block(c, b, &frame)?.data)
}

fn tail(input: &dyn Input, from: u64) -> Vec<u8> {
    let mut bytes = vec![0; (input.size() - from) as usize];
    input.read_at(from, &mut bytes);
    bytes
}

struct Blocks {
    input: Arc<dyn Input>,
    footer: Footer,
    /// Each channel's blocks.
    refs: Vec<Vec<BlockRef>>,
}

impl Blocks {
    fn decode(&self, c: usize, b: &BlockRef) -> Result<BlockChannel, Error> {
        block(&*self.input, &self.footer, c, b)
    }

    /// Channel `c`'s items, block after block, as `take` finds them in its data, up to a block
    /// that doesn't decode.
    fn items<T>(
        self: Arc<Self>,
        c: usize,
        take: fn(BlockChannel) -> Vec<T>,
    ) -> impl Iterator<Item = T> + Send
    where
        T: Send,
    {
        (0..self.refs[c].len())
            .map_while(move |i| self.decode(c, &self.refs[c][i]).ok().map(take))
            .flatten()
    }
}
