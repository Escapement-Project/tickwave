//! A finished tickwave recording read back into the model, so it can be shaped and merged as a
//! capture is. Every block is decoded once when the recording is opened, so a damaged one is
//! refused there; each channel then decodes the blocks again as it is read.

use std::sync::Arc;

use escapement_tickwave_format::decode::{BlockChannel, DecodeError, Footer};
use escapement_tickwave_format::header::{AnalogCoding, ChannelKind, Header};
use escapement_tickwave_format::model::{
    Analog, Channel, ChannelData, Digital, Recording, Samples, Source,
};
use escapement_tickwave_format::samples::Values;

use crate::input::Input;

/// The bytes read first for the footer: as many as the web reader takes.
const TAIL: u64 = 32 << 10;

const CHECKED: &str = "every block decoded when the recording was opened";

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
/// title, meta, bundled annotations and content id.
pub fn read(input: Arc<dyn Input>) -> Result<(Header, Recording), Error> {
    let footer = footer(&*input)?;
    let blocks = Arc::new(Blocks { input, footer });
    for i in 0..blocks.footer.rows.len() {
        blocks.decode(i)?;
    }
    let header = blocks.footer.header.clone();
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
                    gaps,
                    edges,
                } => ChannelData::Digital(Digital {
                    initial: *initial,
                    end: *end,
                    gaps: gaps.clone(),
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
                    count,
                    breaks,
                    coding,
                    significant,
                } => ChannelData::Analog(Analog {
                    unit: unit.clone(),
                    first: *first,
                    period: *period,
                    count: *count,
                    breaks: breaks.clone(),
                    significant: *significant,
                    samples: samples(&blocks, c, *coding),
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
        derived: header.derived.clone(),
        sources,
    };
    Ok((header, recording))
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

/// Block `i` of the recording `input` holds, decoded, `footer` its footer.
pub fn block(input: &dyn Input, footer: &Footer, i: usize) -> Result<Vec<BlockChannel>, Error> {
    let (offset, length) = footer.block_location(i);
    let mut frame = vec![0; length as usize];
    input.read_at(offset, &mut frame);
    Ok(footer.decode_block(i, &frame)?.channels)
}

fn tail(input: &dyn Input, from: u64) -> Vec<u8> {
    let mut bytes = vec![0; (input.size() - from) as usize];
    input.read_at(from, &mut bytes);
    bytes
}

struct Blocks {
    input: Arc<dyn Input>,
    footer: Footer,
}

impl Blocks {
    fn decode(&self, i: usize) -> Result<Vec<BlockChannel>, Error> {
        block(&*self.input, &self.footer, i)
    }

    /// Channel `c`'s items, block after block, as `take` finds them in its data.
    fn items<T>(
        self: Arc<Self>,
        c: usize,
        take: fn(BlockChannel) -> Vec<T>,
    ) -> impl Iterator<Item = T> + Send
    where
        T: Send,
    {
        (0..self.footer.rows.len()).flat_map(move |i| {
            let mut channels = self.decode(i).expect(CHECKED);
            take(channels.swap_remove(c))
        })
    }
}
