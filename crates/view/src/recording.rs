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
use escapement_tickwave_format::mapping::Mapping;
use escapement_tickwave_format::model::{Break, Gap, Ratio, Sample};
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
#[cfg(feature = "live")]
use escapement_tickwave_format::decode::{Block, BlockChannel};
#[cfg(feature = "live")]
use escapement_tickwave_format::header::DeclaredKind;
#[cfg(feature = "live")]
use escapement_tickwave_format::model::run_sample_tick;
#[cfg(feature = "live")]
use escapement_tickwave_format::snapshot::Snapshot;

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
    /// `update` on a finished recording, which takes no snapshot.
    #[error("the recording is finished: it takes no snapshot")]
    NotLive,
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
    #[inline]
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

/// Where a channel's data lies, and for an analog one where its samples sit. A digital channel's
/// ticks are axis ticks; an analog channel's runs are its source's, which `map` places.
#[derive(Clone, Debug)]
pub(crate) enum Place {
    Digital {
        initial: bool,
        /// Where the level is first known: the channel's start, or past a gap from there.
        start: i64,
        end: i64,
        /// The gap from the channel's start, if there is one: its level is the level at `start`.
        leading: Option<Gap>,
        /// The gaps after `start`.
        gaps: Vec<Gap>,
        /// Every gap the header states, one from the start included.
        gap_count: usize,
        /// The channel's start in its source's ticks, and its mapping, for a live channel's gaps.
        source_start: i64,
        map: Mapping,
    },
    Analog {
        first: i64,
        period: Ratio,
        count: u64,
        breaks: Vec<Break>,
        /// The samples the breaks say were lost, summed.
        lost: u64,
        /// Where the last sample's time ends on the axis, or the first sample's tick with none.
        end: i64,
        rule: Rule,
        map: Mapping,
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
                map,
                ..
            } => Some(Runs {
                first: *first,
                period: *period,
                count: *count,
                breaks,
                map: *map,
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
    pub snapshot: Snapshot,
    /// The queued blocks, then the open one.
    pub unwritten: Vec<Arc<Decoded>>,
    /// Each block level's queued overview blocks.
    pub queued_overview: Vec<Vec<Arc<OverviewBlock>>>,
    seen: Seen,
}

/// How much of the writer's append-only lists the tables hold: the written blocks, and each
/// channel's gaps or runs, and the annotation records.
#[cfg(feature = "live")]
struct Seen {
    rows: usize,
    gaps: Vec<usize>,
    runs: Vec<usize>,
    annotations: usize,
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
    /// From the first marker's start to the latest any reaches: what markers add to the extent.
    marker_span: Option<Range<i64>>,
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

/// A gap's source ticks on the axis.
fn gap_on_axis(map: &Mapping, g: &Gap) -> Gap {
    Gap {
        from: map.axis_saturating(g.from),
        to: map.axis_saturating(g.to),
        level: g.level,
    }
}

/// A digital channel's place from its start, end and gaps, all source ticks.
fn digital_place(initial: bool, start: i64, end: i64, gaps: &[Gap], map: Mapping) -> Place {
    // A gap from the channel's start says where its level is first known; the others are gaps in
    // it.
    let leading = gaps
        .first()
        .filter(|g| g.from == start)
        .map(|g| gap_on_axis(&map, g));
    Place::Digital {
        initial,
        start: leading.map_or(map.axis_saturating(start), |g| g.to),
        end: map.axis_saturating(end),
        leading,
        gaps: gaps[usize::from(leading.is_some())..]
            .iter()
            .map(|g| gap_on_axis(&map, g))
            .collect(),
        gap_count: gaps.len(),
        source_start: start,
        map,
    }
}

fn analog_place(
    first: i64,
    period: Ratio,
    count: u64,
    breaks: Vec<Break>,
    length: i64,
    rule: Rule,
    map: Mapping,
) -> Place {
    let runs = Runs {
        first,
        period,
        count,
        breaks: &breaks,
        map,
    };
    let end = match count {
        0 => runs.start(),
        n => length.min(place_after(&runs, n - 1)),
    };
    let lost = breaks.iter().map(|b| b.lost).sum();
    Place::Analog {
        first,
        period,
        count,
        breaks,
        lost,
        end,
        rule,
        map,
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
            lost,
            end,
            map,
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
            lost: *lost,
            end: *end,
            rule: Rule::Product(product),
            map: *map,
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
    for (i, c) in h.channels.iter().enumerate() {
        // A checked header maps every channel.
        let map = h.mapping(i).unwrap_or(Mapping::IDENTITY);
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
                let start = h.start(i).unwrap_or(0);
                built
                    .places
                    .push(digital_place(*initial, start, *end, gaps, map));
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
                    map,
                ));
            }
        }
    }
    add_derived(&mut built, &h.derived, products);
    built
}

/// The ticks `a` marks: its span, or one tick for a point.
fn marked(a: &Annotation) -> Range<i64> {
    a.start..a.end.unwrap_or(a.start).max(a.start + 1)
}

fn widen(extent: &mut Option<Range<i64>>, r: Range<i64>) {
    if r.end > r.start {
        *extent = Some(match extent.take() {
            None => r,
            Some(e) => e.start.min(r.start)..e.end.max(r.end),
        });
    }
}

fn span_of(markers: &[Marker]) -> Option<Range<i64>> {
    let mut span = None;
    for m in markers {
        widen(&mut span, marked(&m.annotation));
    }
    span
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
    #[tracing::instrument(level = "debug", name = "open", skip_all)]
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
            marker_span: span_of(&markers),
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
        self.marker_span = span_of(&self.markers);
        self.extent = self.find_extent();
        Ok(self)
    }

    /// A file a writer in this process writes. `snapshot` is `Writer::snapshot()` taken after the
    /// bytes it covers are in the file (writer.rs:1009-1010); give it each newer one through
    /// `update`. Keep the same cache across a recording's snapshots: written blocks don't change.
    /// The recording ends where what the writer holds does, and so do its digital channels.
    #[cfg(feature = "live")]
    #[tracing::instrument(level = "debug", name = "live", skip_all)]
    pub fn live(snapshot: Snapshot, bytes: Arc<dyn Bytes>, cache: Cache) -> Result<Self, Error> {
        let d = snapshot.declaration();
        let products = format_overview::products(&d.channels, &d.derived);
        let mut built = Built {
            channels: Vec::new(),
            places: Vec::new(),
        };
        for (i, ch) in d.channels.iter().enumerate() {
            // A writer takes only a declaration that maps every channel.
            let map = d.mapping(i).unwrap_or(Mapping::IDENTITY);
            match &ch.data {
                DeclaredKind::Digital { initial } => {
                    built.channels.push(ChannelInfo {
                        name: ch.name.clone(),
                        kind: ChannelKind::Digital,
                        unit: String::new(),
                        resolution: None,
                        significant: None,
                    });
                    let start = d.start(i).unwrap_or(0);
                    built
                        .places
                        .push(digital_place(*initial, start, start, &[], map));
                }
                DeclaredKind::Analog {
                    unit,
                    period,
                    coding,
                    significant,
                } => {
                    built
                        .channels
                        .push(analog_info(&ch.name, unit, *coding, *significant));
                    built.places.push(analog_place(
                        0,
                        *period,
                        0,
                        Vec::new(),
                        0,
                        Rule::Coding(*coding),
                        map,
                    ));
                }
            }
        }
        add_derived(&mut built, &d.derived, &products);
        let n = d.channels.len();
        let mut rec = Self {
            timebase: d.timebase,
            length: 0,
            sources: d.sources.clone(),
            products,
            bytes,
            cache,
            channels: built.channels,
            places: built.places,
            blocks: Vec::new(),
            levels: Vec::new(),
            markers: Vec::new(),
            marker_span: None,
            extent: None,
            store: Store::Live(Box::new(Live {
                snapshot,
                unwritten: Vec::new(),
                queued_overview: Vec::new(),
                seen: Seen {
                    rows: 0,
                    gaps: vec![0; n],
                    runs: vec![0; n],
                    annotations: 0,
                },
            })),
        };
        rec.advance()?;
        Ok(rec)
    }

    /// Takes a newer snapshot of the writer this recording was made live from. Its work is what
    /// the writer added since the last one: the blocks written, gaps, runs and annotations since,
    /// and the blocks not yet written, decoded again. On an error the recording keeps the last
    /// snapshot.
    #[cfg(feature = "live")]
    #[tracing::instrument(level = "debug", name = "update", skip_all)]
    pub fn update(&mut self, snapshot: Snapshot) -> Result<(), Error> {
        let Store::Live(live) = &mut self.store else {
            return Err(Error::NotLive);
        };
        let previous = std::mem::replace(&mut live.snapshot, snapshot);
        let advanced = self.advance();
        if advanced.is_err()
            && let Store::Live(live) = &mut self.store
        {
            live.snapshot = previous;
        }
        advanced
    }

    /// Brings the tables up to the live snapshot, reading only past what `seen` counts.
    #[cfg(feature = "live")]
    fn advance(&mut self) -> Result<(), Error> {
        let Self {
            store,
            places,
            blocks,
            levels,
            markers,
            marker_span,
            products,
            length,
            ..
        } = self;
        let Store::Live(live) = store else {
            unreachable!("advance is for a live recording");
        };
        let Live {
            snapshot,
            unwritten,
            queued_overview,
            seen,
        } = &mut **live;
        let d = snapshot.declaration();
        let stored = d.channels.len();

        // What can fail goes first, so an error leaves the tables as they were: the blocks and
        // overview blocks not yet written, decoded again each time, few as they are.
        let derived_of = |block: &Block| {
            (0..d.derived.len())
                .map(|i| snapshot.derived(block, i))
                .collect::<Result<Vec<_>, _>>()
        };
        let mut fresh = Vec::new();
        for block in snapshot.queued_blocks()? {
            let derived = derived_of(&block)?;
            fresh.push(Arc::new(Decoded { block, derived }));
        }
        let open = snapshot.open_block()?;
        let open_derived = derived_of(&open)?;
        let block_levels = snapshot.overview_blocks();
        let mut fresh_overview = Vec::with_capacity(block_levels.len());
        for level in 0..block_levels.len() {
            fresh_overview.push(
                snapshot
                    .queued_overview_blocks(level)?
                    .into_iter()
                    .map(Arc::new)
                    .collect::<Vec<_>>(),
            );
        }

        // The gaps and runs since the last snapshot, each channel's count, and the latest tick.
        let open_start = open.start;
        let mut last = open_start - 1;
        for (c, (place, ch)) in places.iter_mut().zip(&open.channels).enumerate() {
            match (place, ch) {
                (
                    Place::Digital {
                        start,
                        leading,
                        gaps,
                        gap_count,
                        source_start,
                        map,
                        ..
                    },
                    BlockChannel::Digital { edges, .. },
                ) => {
                    last = last.max(edges.last().copied().unwrap_or(i64::MIN));
                    for g in snapshot.gaps_from(c, seen.gaps[c]) {
                        // A gap from the channel's start says where its level is first known;
                        // the others are gaps in it.
                        let on_axis = gap_on_axis(map, g);
                        if *gap_count == 0 && g.from == *source_start {
                            *start = on_axis.to;
                            *leading = Some(on_axis);
                        } else {
                            gaps.push(on_axis);
                        }
                        *gap_count += 1;
                    }
                    seen.gaps[c] = *gap_count;
                }
                (
                    Place::Analog {
                        first,
                        period,
                        count,
                        breaks,
                        lost,
                        map,
                        ..
                    },
                    BlockChannel::Analog {
                        first_index,
                        values,
                    },
                ) => {
                    for r in snapshot.runs_from(c, seen.runs[c]) {
                        if seen.runs[c] == 0 {
                            *first = r.tick;
                        } else {
                            breaks.push(*r);
                            *lost += r.lost;
                        }
                        seen.runs[c] += 1;
                    }
                    *count = first_index + values.len() as u64;
                    if !values.is_empty() {
                        let k = run_sample_tick(*first, *period, breaks, *count - 1);
                        last = last.max(map.axis_saturating(k));
                    }
                }
                _ => unreachable!("a block's channels follow the declaration"),
            }
        }
        *length = open_start.max(last + 1).max(0);
        for place in &mut places[..stored] {
            match place {
                Place::Digital { end, .. } => *end = *length,
                Place::Analog {
                    first,
                    period,
                    count,
                    breaks,
                    end,
                    map,
                    ..
                } => {
                    let runs = Runs {
                        first: *first,
                        period: *period,
                        count: *count,
                        breaks,
                        map: *map,
                    };
                    *end = match *count {
                        0 => runs.start(),
                        n => (*length).min(place_after(&runs, n - 1)),
                    };
                }
            }
        }
        // A derived channel's samples sit where its first operand's do.
        for (k, &(a, _, _)) in products.iter().enumerate() {
            let (operands, derived) = places.split_at_mut(stored);
            let (
                Place::Analog {
                    first,
                    count,
                    breaks,
                    lost,
                    end,
                    ..
                },
                Place::Analog {
                    first: d_first,
                    count: d_count,
                    breaks: d_breaks,
                    lost: d_lost,
                    end: d_end,
                    ..
                },
            ) = (&operands[a], &mut derived[k])
            else {
                unreachable!("a checked declaration's operands are analog");
            };
            (*d_first, *d_count, *d_lost, *d_end) = (*first, *count, *lost, *end);
            d_breaks.extend_from_slice(&breaks[d_breaks.len()..]);
        }

        // The written blocks since the last snapshot, then those not yet written, again.
        blocks.truncate(seen.rows);
        for i in seen.rows..snapshot.block_count() {
            let row = snapshot.row(i).expect("a written block");
            blocks.push(BlockAt {
                start: row.start,
                end: snapshot.block_end(i).expect("a written block"),
                from: BlockFrom::Frame {
                    index: i,
                    offset: row.offset,
                    length: row.length,
                },
            });
        }
        seen.rows = snapshot.block_count();
        for (j, u) in fresh.iter().enumerate() {
            blocks.push(BlockAt {
                start: u.block.start,
                end: u.block.end,
                from: BlockFrom::Unwritten(j),
            });
        }
        if *length > open_start {
            blocks.push(BlockAt {
                start: open_start,
                end: *length,
                from: BlockFrom::Unwritten(fresh.len()),
            });
        }
        fresh.push(Arc::new(Decoded {
            block: Block {
                end: (*length).max(open_start),
                ..open
            },
            derived: open_derived,
        }));
        *unwritten = fresh;

        // The levels a file finished at this length would keep, so a live view chooses as that
        // file's would. A few a recording, built again.
        let plan = format_overview::plan((*length).max(1), d.overview_shift);
        levels.clear();
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
                    queued: fresh_overview[level].len(),
                });
            }
            parts.push(Part::Held(i));
            levels.push(LevelAt {
                shift: held.shift,
                parts,
                covered,
            });
        }
        *queued_overview = fresh_overview;

        // The annotation records since the last snapshot, each a line of its generator's file.
        // A recorder's come in order of their ticks, so each goes in at the end, as a rule.
        for r in snapshot.annotations_from(seen.annotations) {
            let annotation = crate::markers::generated(&r.source, &r.annotation, seen.annotations);
            let marker = Marker {
                cursor: MarkerCursor {
                    tick: annotation.start,
                    arrival: seen.annotations,
                },
                annotation,
            };
            seen.annotations += 1;
            widen(marker_span, marked(&marker.annotation));
            let at = markers.partition_point(|m| m.cursor <= marker.cursor);
            markers.insert(at, marker);
        }
        self.extent = self.find_extent();
        Ok(())
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

    /// Where channel `channel`'s source ticks lie on the axis (FORMAT.md, "Times"): its source's
    /// `m`, origin and its own offset, for a readout exact to the source's tick. A derived
    /// channel's is its operands'.
    pub fn mapping(&self, channel: usize) -> Result<Mapping, Error> {
        match self.place(channel)? {
            Place::Digital { map, .. } | Place::Analog { map, .. } => Ok(*map),
        }
    }

    /// An analog or derived channel's samples the instrument lost between runs, its breaks'
    /// `lost` summed; 0 for a digital channel or one past the last.
    pub fn lost(&self, channel: usize) -> u64 {
        match self.places.get(channel) {
            Some(Place::Analog { lost, .. }) => *lost,
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
        let stored = self.places.len() - self.products.len();
        for place in &self.places[..stored] {
            if let Some(runs) = place.runs()
                && runs.count > 0
                && let Some(last) = runs.support(runs.count - 1, self.length)
            {
                widen(&mut extent, runs.start()..last.end);
            }
        }
        if extent.is_none() {
            for place in &self.places[..stored] {
                if let Place::Digital { start, end, .. } = place {
                    widen(&mut extent, *start..*end);
                }
            }
        }
        if let Some(span) = &self.marker_span {
            widen(&mut extent, span.clone());
        }
        extent
    }

    /// Analog or derived channel `channel`'s runs: its first sample's tick, period, count and
    /// breaks, as the recording holds them now, in its source's ticks, and its mapping to the
    /// axis, which the runs' methods apply.
    pub fn runs(&self, channel: usize) -> Result<Runs<'_>, Error> {
        self.place(channel)?.runs().ok_or(Error::NoChannel(channel))
    }

    /// Where channel `channel`'s data lies: a digital channel's known span and gaps, an analog
    /// one's runs and the end of its last sample's time.
    pub fn known(&self, channel: usize) -> Result<ChannelPlace<'_>, Error> {
        Ok(self.place(channel)?.known())
    }

    pub(crate) fn place(&self, channel: usize) -> Result<&Place, Error> {
        self.places.get(channel).ok_or(Error::NoChannel(channel))
    }
}

#[cfg(all(test, feature = "live"))]
mod tests {
    use std::sync::Arc;

    use escapement_tickwave_format::header::{
        Declaration, DeclaredChannel, DeclaredKind, SourceFormat, SourceHeader,
    };
    use escapement_tickwave_format::model::{Gap, Ratio};
    use escapement_tickwave_format::writer::{WriteOptions, Writer};

    use super::{Recording, Store};
    use crate::Cache;

    /// An update reads past what the recording holds, not the whole writer's lists again.
    #[test]
    fn an_update_takes_only_what_came_since() {
        let timebase = Ratio::integer(1000).unwrap();
        let d = Declaration {
            timebase,
            channels: vec![DeclaredChannel {
                name: "D".into(),
                source_name: "d".into(),
                source: 0,
                data: DeclaredKind::Digital { initial: false },
                shaping: Default::default(),
            }],
            derived: Vec::new(),
            sources: vec![SourceHeader::new(
                SourceFormat::Other("crafted".into()),
                timebase,
            )],
            title: None,
            meta: Default::default(),
            overview_shift: 4,
        };
        let mut w = Writer::new(d, WriteOptions::default()).unwrap();
        let mut file = w.take_output();
        let mut rec: Option<Recording> = None;
        for k in 0..50i64 {
            w.edge(0, 10 * k).unwrap();
            if k % 10 == 9 {
                w.gap(
                    0,
                    Gap {
                        from: 10 * k + 1,
                        to: 10 * k + 5,
                        level: k % 2 == 0,
                    },
                )
                .unwrap();
            }
            w.flush(10 * k + 6).unwrap();
            let snapshot = w.snapshot();
            file.extend(w.take_output());
            match &mut rec {
                None => {
                    rec = Some(
                        Recording::live(snapshot, Arc::new(file.clone()), Cache::new(1 << 20))
                            .unwrap(),
                    )
                }
                Some(r) => {
                    r.bytes = Arc::new(file.clone());
                    r.update(snapshot).unwrap();
                }
            }
            let r = rec.as_ref().unwrap();
            let Store::Live(live) = &r.store else {
                unreachable!()
            };
            assert_eq!(
                live.seen.rows,
                live.snapshot.block_count(),
                "after edge {k}"
            );
            assert_eq!(
                live.seen.gaps[0],
                live.snapshot.gaps(0).count(),
                "after edge {k}"
            );
            // The written blocks, then the open one where it holds anything: an empty one has no
            // ticks.
            assert_eq!(
                r.blocks.len(),
                live.snapshot.block_count(),
                "after edge {k}"
            );
        }
    }
}
