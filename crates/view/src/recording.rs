//! A recording a view reads: a finished file, through its footer, or one a writer in this process
//! is still writing, through the writer's snapshot.

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

use escapement_tickwave_format::annotations::{self, Annotation, Included};
use escapement_tickwave_format::decode::{DecodeError, FIRST_READ, Footer};
use escapement_tickwave_format::header::{
    AnalogCoding, ChannelKind as HeaderKind, Exact, Header, MAGIC, PREAMBLE_LEN, SourceHeader,
};
use escapement_tickwave_format::model::{Break, Gap, Ratio, Sample, sample_support};
use escapement_tickwave_format::overview::{self as format_overview};
use escapement_tickwave_format::samples::{self, Product};

use crate::bytes::{Bytes, read};
use crate::cache::Cache;
#[cfg(feature = "live")]
use crate::cache::Decoded;
use crate::markers::{Marker, MarkerCursor};
use crate::runs::{Runs, place_after};
use crate::spans::ChannelPlace;
#[cfg(feature = "live")]
use escapement_tickwave_format::decode::OverviewBlock;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Decode(#[from] DecodeError),
    /// The file has no trailer: cut off by a crash, or still being written. The Recutter's repair
    /// makes it whole; opening it never repairs it in memory.
    #[error("the recording has no footer: it was cut off, or is still being written")]
    Unfinished,
    #[error("cancelled")]
    Cancelled,
    #[error("a sum is past what 128 bits hold")]
    Overflow,
    #[error("no channel {0}")]
    NoChannel(usize),
    #[error("the annotations: {0}")]
    Annotations(String),
}

/// What a channel holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelKind {
    Digital,
    Analog,
    /// The product of two analog channels, by index (FORMAT.md, "Derived channels").
    Derived {
        operands: (usize, usize),
    },
}

/// The finest a channel's readings show.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Resolution {
    /// An `int` channel or an integer product: a value is `code × scale + offset` exactly, and
    /// one code step is |scale|. `readouts::format_code` reads it.
    Code { scale: Exact, offset: Exact },
    /// A `codes` channel's step, in its unit.
    Step(f64),
}

/// A channel as a view names and reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelInfo {
    pub name: String,
    pub kind: ChannelKind,
    /// Empty for a digital channel, and for an analog one whose source names none.
    pub unit: String,
    /// None for a digital channel and a float one.
    pub resolution: Option<Resolution>,
    pub significant: Option<u8>,
}

/// How a channel's stored samples become values.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Rule {
    Coding(AnalogCoding),
    Product(Product),
}

impl Rule {
    pub fn value(self, s: Sample) -> f64 {
        match self {
            Self::Coding(c) => samples::value_f64(s, c),
            Self::Product(p) => p.value(s),
        }
    }

    /// Whether every value is an integer code, so its sums are exact.
    pub fn exact(self) -> Option<(Exact, Exact)> {
        match self {
            Self::Coding(AnalogCoding::Int(int)) => Some((int.scale(), int.offset())),
            Self::Product(Product::Int { num, den, .. }) => {
                Some((Exact::new(num as i64, den as i64)?, Exact::ZERO))
            }
            _ => None,
        }
    }
}

/// Where a channel's data lies, and for an analog one where its samples sit.
#[derive(Clone, Debug)]
pub(crate) enum Place {
    Digital {
        initial: bool,
        /// Where the level is first known: past a gap from tick 0.
        start: i64,
        end: i64,
        /// The gaps after `start`.
        gaps: Vec<Gap>,
        /// Every gap the header states, one from tick 0 included.
        gap_count: usize,
    },
    Analog {
        first: i64,
        period: Ratio,
        count: u64,
        breaks: Vec<Break>,
        /// Where the last sample's time ends, or `first` with none.
        end: i64,
        rule: Rule,
    },
}

impl Place {
    pub fn runs(&self) -> Option<Runs<'_>> {
        match self {
            Self::Analog {
                first,
                period,
                count,
                breaks,
                ..
            } => Some(Runs {
                first: *first,
                period: *period,
                count: *count,
                breaks,
            }),
            Self::Digital { .. } => None,
        }
    }

    pub fn known(&self) -> ChannelPlace<'_> {
        match self {
            Self::Digital {
                start, end, gaps, ..
            } => ChannelPlace::Digital {
                start: *start,
                end: *end,
                gaps,
            },
            Self::Analog { end, .. } => ChannelPlace::Analog {
                runs: self.runs().expect("analog"),
                end: *end,
            },
        }
    }
}

/// A block, and where its decoded items come from.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BlockAt {
    pub start: i64,
    pub end: i64,
    pub from: BlockFrom,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum BlockFrom {
    /// Block `index` of the footer's or the snapshot's rows: its frame in the file.
    Frame {
        index: usize,
        offset: u64,
        length: u32,
    },
    /// A block the writer has not written, decoded when the recording was made.
    #[cfg(feature = "live")]
    Unwritten(usize),
}

/// One overview level: its shift, and where its buckets come from, in bucket order.
#[derive(Clone, Debug)]
pub(crate) struct LevelAt {
    pub shift: u8,
    pub parts: Vec<Part>,
    /// Buckets `0..covered` are in it; a live recording's later ones haven't settled.
    pub covered: u64,
}

impl LevelAt {
    /// The tick where its buckets end, saturated where it passes an i64.
    pub fn end(&self) -> i64 {
        i64::try_from(u128::from(self.covered) << self.shift).unwrap_or(i64::MAX)
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Part {
    /// The footer's level, every bucket.
    Head(usize),
    /// The footer's level `header.overview_blocks[level]`, every block.
    Blocks(usize),
    /// The snapshot's `overview_blocks()[level]`: `written` blocks in the file, then `queued`.
    #[cfg(feature = "live")]
    LiveBlocks {
        level: usize,
        written: usize,
        queued: usize,
    },
    /// The snapshot's `overview()[level]`: the buckets the writer holds.
    #[cfg(feature = "live")]
    Held(usize),
}

pub(crate) enum Store {
    Finished(Box<Footer>),
    #[cfg(feature = "live")]
    Live(Box<Live>),
}

#[cfg(feature = "live")]
pub(crate) struct Live {
    pub snapshot: escapement_tickwave_format::snapshot::Snapshot,
    /// The queued blocks, then the open one.
    pub unwritten: Vec<Arc<Decoded>>,
    /// Each block level's queued overview blocks.
    pub queued_overview: Vec<Vec<Arc<OverviewBlock>>>,
}

/// A recording a view reads.
pub struct Recording {
    pub(crate) bytes: Arc<dyn Bytes>,
    pub(crate) cache: Cache,
    pub(crate) store: Store,
    pub(crate) timebase: Ratio,
    /// The ticks it covers: a finished file's length, or the end of what a writer holds.
    pub(crate) length: i64,
    pub(crate) channels: Vec<ChannelInfo>,
    pub(crate) places: Vec<Place>,
    pub(crate) blocks: Vec<BlockAt>,
    /// Finest first.
    pub(crate) levels: Vec<LevelAt>,
    pub(crate) markers: Vec<Marker>,
    pub(crate) sources: Vec<SourceHeader>,
    pub(crate) products: Vec<(usize, usize, Product)>,
    extent: Option<Range<i64>>,
}

/// Each channel's info and place, then each derived channel's, from what a header or a
/// declaration states.
struct Built {
    channels: Vec<ChannelInfo>,
    places: Vec<Place>,
}

fn analog_info(
    name: &str,
    unit: &str,
    coding: AnalogCoding,
    significant: Option<u8>,
) -> ChannelInfo {
    ChannelInfo {
        name: name.into(),
        kind: ChannelKind::Analog,
        unit: unit.into(),
        resolution: match coding {
            AnalogCoding::Int(int) => Some(Resolution::Code {
                scale: int.scale(),
                offset: int.offset(),
            }),
            AnalogCoding::Codes { scale, .. } => Some(Resolution::Step(scale)),
            AnalogCoding::F32 | AnalogCoding::F64 => None,
        },
        significant,
    }
}

fn digital_place(initial: bool, end: i64, gaps: &[Gap]) -> Place {
    // A gap from tick 0 says where the channel's data starts; the others are gaps in it.
    let leading = gaps.first().is_some_and(|g| g.from == 0);
    Place::Digital {
        initial,
        start: if leading { gaps[0].to } else { 0 },
        end,
        gaps: gaps[usize::from(leading)..].to_vec(),
        gap_count: gaps.len(),
    }
}

fn analog_place(
    first: i64,
    period: Ratio,
    count: u64,
    breaks: Vec<Break>,
    length: i64,
    rule: Rule,
) -> Place {
    let runs = Runs {
        first,
        period,
        count,
        breaks: &breaks,
    };
    let end = match count {
        0 => first,
        n => length.min(place_after(&runs, n - 1)),
    };
    Place::Analog {
        first,
        period,
        count,
        breaks,
        end,
        rule,
    }
}

/// Adds the derived channels, placed as their first operand's samples are.
fn add_derived(
    built: &mut Built,
    derived: &[escapement_tickwave_format::header::Derived],
    products: &[(usize, usize, Product)],
) {
    for (d, &(a, b, product)) in derived.iter().zip(products) {
        let Place::Analog {
            first,
            period,
            count,
            breaks,
            end,
            ..
        } = &built.places[a]
        else {
            unreachable!("a checked header's operands are analog")
        };
        built.places.push(Place::Analog {
            first: *first,
            period: *period,
            count: *count,
            breaks: breaks.clone(),
            end: *end,
            rule: Rule::Product(product),
        });
        built.channels.push(ChannelInfo {
            name: d.name.clone(),
            kind: ChannelKind::Derived { operands: (a, b) },
            unit: d.unit.clone(),
            resolution: match product {
                Product::Int { num, den, .. } => {
                    Exact::new(num as i64, den as i64).map(|scale| Resolution::Code {
                        scale,
                        offset: Exact::ZERO,
                    })
                }
                Product::F64 { .. } => None,
            },
            significant: d.significant,
        });
    }
}

fn header_channels(h: &Header, products: &[(usize, usize, Product)]) -> Built {
    let mut built = Built {
        channels: Vec::new(),
        places: Vec::new(),
    };
    for c in &h.channels {
        match &c.data {
            HeaderKind::Digital {
                initial, end, gaps, ..
            } => {
                built.channels.push(ChannelInfo {
                    name: c.name.clone(),
                    kind: ChannelKind::Digital,
                    unit: String::new(),
                    resolution: None,
                    significant: None,
                });
                built.places.push(digital_place(*initial, *end, gaps));
            }
            HeaderKind::Analog {
                unit,
                first,
                period,
                count,
                breaks,
                coding,
                significant,
            } => {
                built
                    .channels
                    .push(analog_info(&c.name, unit, *coding, *significant));
                built.places.push(analog_place(
                    *first,
                    *period,
                    *count,
                    breaks.clone(),
                    h.length,
                    Rule::Coding(*coding),
                ));
            }
        }
    }
    add_derived(&mut built, &h.derived, products);
    built
}

/// Markers from a sidecar, in the order it gives them, which is their arrival.
fn markers_of(annotations: Vec<Annotation>) -> Vec<Marker> {
    let mut markers: Vec<Marker> = annotations
        .into_iter()
        .enumerate()
        .map(|(arrival, annotation)| Marker {
            cursor: MarkerCursor {
                tick: annotation.start,
                arrival,
            },
            annotation,
        })
        .collect();
    markers.sort_by_key(|m| m.cursor);
    markers
}

/// The hand-written file of a bundle and the files it includes.
fn bundled(files: &BTreeMap<String, String>) -> Option<(&str, Vec<Included<'_>>)> {
    let hand = files.iter().find(|(name, _)| name.ends_with(".toml"))?;
    let included = files
        .iter()
        .filter(|(name, _)| *name != hand.0)
        .map(|(name, text)| Included { name, text })
        .collect();
    Some((hand.1, included))
}

impl Recording {
    /// A finished file. `Error::Unfinished` for a file with no trailer (cut off by a crash), so
    /// the host can offer the Recutter's repair; `open` never repairs in memory. The file's
    /// bundled annotations are its markers.
    pub fn open(bytes: Arc<dyn Bytes>, cache: Cache) -> Result<Self, Error> {
        let size = bytes.size()?;
        if size < PREAMBLE_LEN as u64 || read(&*bytes, 0, MAGIC.len())? != MAGIC {
            return Err(DecodeError::NotTickwave.into());
        }
        let mut from = size.saturating_sub(FIRST_READ as u64);
        let footer = loop {
            let tail = read(&*bytes, from, (size - from) as usize)?;
            match Footer::parse(&tail, size) {
                Ok(f) => break f,
                Err(DecodeError::NeedFrom(at)) if at < from => from = at,
                Err(DecodeError::Unfinished) => return Err(Error::Unfinished),
                Err(e) => return Err(e.into()),
            }
        };
        let markers = match footer.header.annotations.as_ref().map(bundled) {
            None => Vec::new(),
            Some(None) => {
                return Err(Error::Annotations(
                    "the bundle holds no hand-written file".into(),
                ));
            }
            Some(Some((hand, included))) => markers_of(
                annotations::resolve(hand, &included, &footer.header, None)
                    .map_err(|problems| Error::Annotations(problems.join("; ")))?,
            ),
        };
        let Built { channels, places } = header_channels(&footer.header, &footer.products);
        let h = &footer.header;
        let blocks = (0..footer.rows.len())
            .map(|i| {
                let (offset, length) = footer.block_location(i);
                BlockAt {
                    start: footer.rows[i].start,
                    end: footer.block_end(i),
                    from: BlockFrom::Frame {
                        index: i,
                        offset,
                        length,
                    },
                }
            })
            .collect();
        let mut levels: Vec<LevelAt> = h
            .overview
            .iter()
            .enumerate()
            .map(|(i, &shift)| LevelAt {
                shift,
                parts: vec![Part::Head(i)],
                covered: format_overview::buckets(h.length, shift),
            })
            .chain(h.overview_blocks.iter().enumerate().map(|(i, l)| LevelAt {
                shift: l.shift,
                parts: vec![Part::Blocks(i)],
                covered: format_overview::buckets(h.length, l.shift),
            }))
            .collect();
        levels.sort_by_key(|l| l.shift);
        let mut rec = Self {
            timebase: h.timebase,
            length: h.length,
            sources: h.sources.clone(),
            products: footer.products.clone(),
            bytes,
            cache,
            channels,
            places,
            blocks,
            levels,
            markers,
            extent: None,
            store: Store::Finished(Box::new(footer)),
        };
        rec.extent = rec.find_extent();
        Ok(rec)
    }

    /// Markers from a sidecar the host passes, in place of the file's bundled ones: `hand` the
    /// hand-written file's TOML, and the files it includes. Every problem it finds, joined.
    pub fn with_sidecar(mut self, hand: &str, included: &[Included<'_>]) -> Result<Self, Error> {
        let footer = match &self.store {
            Store::Finished(footer) => footer,
            #[cfg(feature = "live")]
            Store::Live(_) => {
                return Err(Error::Annotations(
                    "a recording still being written takes its writer's annotations".into(),
                ));
            }
        };
        self.markers = markers_of(
            annotations::resolve(hand, included, &footer.header, None)
                .map_err(|problems| Error::Annotations(problems.join("; ")))?,
        );
        self.extent = self.find_extent();
        Ok(self)
    }

    /// A file a writer in this process writes. `snapshot` is `Writer::snapshot()` taken after the
    /// bytes it covers are in the file (writer.rs:1009-1010). Keep the same cache across a
    /// recording's snapshots: written blocks don't change. The recording ends where what the
    /// writer holds does, and so do its digital channels.
    #[cfg(feature = "live")]
    pub fn live(
        snapshot: escapement_tickwave_format::snapshot::Snapshot,
        bytes: Arc<dyn Bytes>,
        cache: Cache,
    ) -> Result<Self, Error> {
        let d = snapshot.declaration().clone();
        let products = format_overview::products(&d.channels, &d.derived);
        let derived_of = |block: &escapement_tickwave_format::decode::Block| {
            (0..d.derived.len())
                .map(|i| snapshot.derived(block, i))
                .collect::<Result<Vec<_>, _>>()
        };
        let mut unwritten = Vec::new();
        for block in snapshot.queued_blocks()? {
            let derived = derived_of(&block)?;
            unwritten.push(Arc::new(Decoded { block, derived }));
        }
        let mut open = snapshot.open_block()?;
        // Each analog channel's open samples follow those of every block before the open one.
        let mut before: Vec<u64> = vec![0; d.channels.len()];
        for row in snapshot.rows() {
            for (b, &word) in before.iter_mut().zip(&row.channels) {
                *b += u64::from(word);
            }
        }
        for u in &unwritten {
            for (b, ch) in before.iter_mut().zip(&u.block.channels) {
                if let Some(v) = ch.values() {
                    *b += v.len() as u64;
                }
            }
        }
        for (ch, &b) in open.channels.iter_mut().zip(&before) {
            if let escapement_tickwave_format::decode::BlockChannel::Analog {
                first_index, ..
            } = ch
            {
                *first_index = b;
            }
        }
        let open_derived = derived_of(&open)?;
        let open_start = open.start;
        // Each channel's runs and count, and the latest item's tick.
        let mut last = open_start - 1;
        let mut runs: Vec<(Vec<Break>, u64)> = Vec::new();
        for (c, ch) in open.channels.iter().enumerate() {
            match ch {
                escapement_tickwave_format::decode::BlockChannel::Digital { edges, .. } => {
                    last = last.max(edges.last().copied().unwrap_or(i64::MIN));
                    runs.push((Vec::new(), 0));
                }
                escapement_tickwave_format::decode::BlockChannel::Analog {
                    first_index,
                    values,
                } => {
                    let all: Vec<Break> = snapshot.runs(c).copied().collect();
                    let count = first_index + values.len() as u64;
                    if count > 0 && !values.is_empty() {
                        let first = all.first().map_or(0, |r| r.tick);
                        let tick = escapement_tickwave_format::model::run_sample_tick(
                            first,
                            match &d.channels[c].data {
                                escapement_tickwave_format::header::DeclaredKind::Analog {
                                    period,
                                    ..
                                } => *period,
                                _ => unreachable!("an analog block channel is an analog channel"),
                            },
                            &all[1.min(all.len())..],
                            count - 1,
                        );
                        last = last.max(tick);
                    }
                    runs.push((all, count));
                }
            }
        }
        let length = open_start.max(last + 1).max(0);
        let mut built = Built {
            channels: Vec::new(),
            places: Vec::new(),
        };
        for (c, ch) in d.channels.iter().enumerate() {
            match &ch.data {
                escapement_tickwave_format::header::DeclaredKind::Digital { initial } => {
                    built.channels.push(ChannelInfo {
                        name: ch.name.clone(),
                        kind: ChannelKind::Digital,
                        unit: String::new(),
                        resolution: None,
                        significant: None,
                    });
                    let gaps: Vec<Gap> = snapshot.gaps(c).copied().collect();
                    built.places.push(digital_place(*initial, length, &gaps));
                }
                escapement_tickwave_format::header::DeclaredKind::Analog {
                    unit,
                    period,
                    coding,
                    significant,
                } => {
                    built
                        .channels
                        .push(analog_info(&ch.name, unit, *coding, *significant));
                    let (all, count) = &runs[c];
                    built.places.push(analog_place(
                        all.first().map_or(0, |r| r.tick),
                        *period,
                        *count,
                        all.iter().skip(1).copied().collect(),
                        length,
                        Rule::Coding(*coding),
                    ));
                }
            }
        }
        add_derived(&mut built, &d.derived, &products);

        let mut blocks: Vec<BlockAt> = (0..snapshot.block_count())
            .map(|i| {
                let row = snapshot.row(i).expect("a written block");
                BlockAt {
                    start: row.start,
                    end: snapshot.block_end(i).expect("a written block"),
                    from: BlockFrom::Frame {
                        index: i,
                        offset: row.offset,
                        length: row.length,
                    },
                }
            })
            .collect();
        for (j, u) in unwritten.iter().enumerate() {
            blocks.push(BlockAt {
                start: u.block.start,
                end: u.block.end,
                from: BlockFrom::Unwritten(j),
            });
        }
        if length > open_start {
            blocks.push(BlockAt {
                start: open_start,
                end: length,
                from: BlockFrom::Unwritten(unwritten.len()),
            });
        }
        unwritten.push(Arc::new(Decoded {
            block: escapement_tickwave_format::decode::Block {
                end: length.max(open_start),
                ..open
            },
            derived: open_derived,
        }));

        let block_levels = snapshot.overview_blocks();
        let mut queued_overview = Vec::with_capacity(block_levels.len());
        for level in 0..block_levels.len() {
            queued_overview.push(
                snapshot
                    .queued_overview_blocks(level)?
                    .into_iter()
                    .map(Arc::new)
                    .collect::<Vec<_>>(),
            );
        }
        // The levels a file finished at this length would keep, so a live view chooses as that
        // file's would.
        let plan = format_overview::plan(length.max(1), d.overview_shift);
        let mut levels = Vec::new();
        for (i, held) in snapshot.overview().iter().enumerate() {
            let covered = held.first + held.buckets();
            let planned = plan.head.contains(&held.shift) || plan.blocks.contains(&held.shift);
            if covered == 0 || !planned {
                continue;
            }
            let mut parts = Vec::new();
            if let Some(level) = block_levels.iter().position(|l| l.shift == held.shift) {
                parts.push(Part::LiveBlocks {
                    level,
                    written: block_levels[level].blocks as usize,
                    queued: queued_overview[level].len(),
                });
            }
            parts.push(Part::Held(i));
            levels.push(LevelAt {
                shift: held.shift,
                parts,
                covered,
            });
        }
        // Each record's annotation, as a line of its generator's file.
        let markers = markers_of(
            snapshot
                .annotations()
                .enumerate()
                .map(|(arrival, r)| crate::markers::generated(&r.source, &r.annotation, arrival))
                .collect(),
        );
        let Built { channels, places } = built;
        let mut rec = Self {
            timebase: d.timebase,
            length,
            sources: d.sources.clone(),
            products,
            bytes,
            cache,
            channels,
            places,
            blocks,
            levels,
            markers,
            extent: None,
            store: Store::Live(Box::new(Live {
                snapshot,
                unwritten,
                queued_overview,
            })),
        };
        rec.extent = rec.find_extent();
        Ok(rec)
    }

    /// Ticks a second.
    pub fn timebase(&self) -> Ratio {
        self.timebase
    }

    /// The ticks the recording covers, `0..length`: a finished file's length, or where what a
    /// writer holds ends.
    pub fn length(&self) -> i64 {
        self.length
    }

    /// Each stored channel, then each derived one; a channel's index here is the one a query
    /// names.
    pub fn channels(&self) -> &[ChannelInfo] {
        &self.channels
    }

    pub fn channel(&self, name: &str) -> Option<usize> {
        self.channels.iter().position(|c| c.name == name)
    }

    /// The sources with their `extra`: the declaration's while live, the footer's once finished.
    pub fn sources(&self) -> &[SourceHeader] {
        &self.sources
    }

    /// An analog or derived channel's samples the instrument lost between runs, its breaks'
    /// `lost` summed; 0 for a digital channel or one past the last.
    pub fn lost(&self, channel: usize) -> u64 {
        match self.places.get(channel) {
            Some(Place::Analog { breaks, .. }) => breaks.iter().map(|b| b.lost).sum(),
            _ => 0,
        }
    }

    /// A digital channel's gaps, one from tick 0 included; 0 for an analog channel.
    pub fn gaps(&self, channel: usize) -> usize {
        match self.places.get(channel) {
            Some(Place::Digital { gap_count, .. }) => *gap_count,
            _ => 0,
        }
    }

    pub fn marker_count(&self) -> usize {
        self.markers.len()
    }

    /// From the first sample to the end of the last sample's time, over every analog channel,
    /// widened by the markers. A recording with no analog sample takes its digital channels'
    /// known span instead. None before any data and any marker.
    pub fn extent(&self) -> Option<Range<i64>> {
        self.extent.clone()
    }

    fn find_extent(&self) -> Option<Range<i64>> {
        let mut extent: Option<Range<i64>> = None;
        fn widen(extent: &mut Option<Range<i64>>, r: Range<i64>) {
            if r.end > r.start {
                *extent = Some(match extent.take() {
                    None => r,
                    Some(e) => e.start.min(r.start)..e.end.max(r.end),
                });
            }
        }
        let stored = self.places.len() - self.products.len();
        for place in &self.places[..stored] {
            if let Place::Analog {
                first,
                period,
                count,
                breaks,
                ..
            } = place
                && *count > 0
            {
                let last = sample_support(*first, *period, breaks, *count, self.length, count - 1)
                    .expect("the last sample");
                widen(&mut extent, *first..last.end);
            }
        }
        if extent.is_none() {
            for place in &self.places[..stored] {
                if let Place::Digital { start, end, .. } = place {
                    widen(&mut extent, *start..*end);
                }
            }
        }
        for m in &self.markers {
            let a = &m.annotation;
            widen(
                &mut extent,
                a.start..a.end.unwrap_or(a.start).max(a.start + 1),
            );
        }
        extent
    }

    pub(crate) fn place(&self, channel: usize) -> Result<&Place, Error> {
        self.places.get(channel).ok_or(Error::NoChannel(channel))
    }
}
