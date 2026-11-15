//! A recording a view reads: a finished file, through its footer, or one a writer in this process
//! is still writing, through the writer's snapshot.

use std::borrow::Cow;
use std::collections::BTreeMap;
#[cfg(feature = "live")]
use std::collections::VecDeque;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock, RwLockReadGuard};

use escapement_tickwave_format::annotations::{self, Annotation, Included};
#[cfg(feature = "live")]
use escapement_tickwave_format::decode::LevelRef;
use escapement_tickwave_format::decode::{BlockRef, DecodeError, FIRST_READ, Footer, List};
use escapement_tickwave_format::header::{
    AnalogCoding, ChannelKind as HeaderKind, Exact, MAGIC, PREAMBLE_LEN, SourceHeader,
};
use escapement_tickwave_format::index::{Located, TableIndex};
use escapement_tickwave_format::mapping::Mapping;
use escapement_tickwave_format::model::{Break, Gap, Ratio, Sample};
use escapement_tickwave_format::overview::{self as format_overview, Level};
use escapement_tickwave_format::samples::{self, Product};

use crate::bytes::{Bytes, read};
use crate::cache::{Cache, Entry, Key, Reading, list_size};
use crate::markers::{Marker, MarkerCursor};
use crate::runs::{Runs, place_after};
use crate::spans::ChannelPlace;
#[cfg(feature = "live")]
use escapement_tickwave_format::decode::{Block, BlockChannel, OverviewBlock};
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
        /// The samples lost before the first and after the last (FORMAT.md, "Gaps").
        lost_before: u64,
        lost_after: u64,
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

/// One channel's block, and where its decoded items come from.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(
    not(feature = "live"),
    expect(
        dead_code,
        reason = "only a live recording's table is searched by its ticks"
    )
)]
pub(crate) struct BlockAt {
    pub start: i64,
    pub end: i64,
    pub from: BlockFrom,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum BlockFrom {
    /// A block in the file, as its channel's index places it.
    Frame(BlockRef),
    /// Block `j` of the channel's blocks the writer has not written, decoded when the recording
    /// was made.
    #[cfg(feature = "live")]
    Unwritten(usize),
}

/// One level of a channel's overview: its shift, and where its buckets come from, in bucket
/// order.
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
    /// The footer's strip of the channel, every bucket.
    Strip,
    /// The footer's level `li` of the channel's levels in overview blocks, every block.
    Blocks(usize),
    /// The snapshot's level `li` of the channel's ladder: its blocks in the file, then those not
    /// yet written.
    #[cfg(feature = "live")]
    LiveBlocks(usize),
    /// The snapshot's level `li`: the buckets the writer holds after its blocks.
    #[cfg(feature = "live")]
    Held(usize),
}

pub(crate) enum Store {
    Finished(Box<Finished>),
    #[cfg(feature = "live")]
    Live(Box<Live>),
}

/// A finished file as far as a view has read it: its footer, and what it read since, each part
/// once a query first asks for it. Its blocks and overview rows are read a page at a time, for
/// the span a query covers, into the cache.
pub(crate) struct Finished {
    /// The footer, a channel's gaps or breaks read into it from their pages when the channel is
    /// first asked for.
    footer: RwLock<Footer>,
    /// Each channel's strip, then each derived channel's, decoded when first asked for.
    pub strips: Vec<OnceLock<Level>>,
    /// Each channel's place where its list, or its first operand's, is in pages: set once they
    /// are read. The recording's `places` hold such a channel with an empty list.
    listed: Vec<Option<OnceLock<Place>>>,
}

impl Finished {
    /// The footer, read-locked. Call nothing that can reach `Recording::place` while holding it:
    /// that takes the write lock to read a list, and would wait on this guard forever.
    pub fn footer(&self) -> RwLockReadGuard<'_, Footer> {
        // A thread that panicked holding the lock left the footer whole: a list goes in at the
        // end of its read, once checked.
        self.footer.read().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(feature = "live")]
pub(crate) struct Live {
    pub snapshot: Snapshot,
    /// Each channel's blocks in the file.
    refs: Vec<Vec<BlockRef>>,
    /// Each channel's blocks not yet written, then its open one, decoded.
    pub unwritten: Vec<Vec<Arc<Block>>>,
    /// Each channel's ladder, then each derived channel's: each level's rows in the file, and its
    /// overview blocks not yet written and its open run, decoded. Empty for a level not in
    /// blocks.
    pub rows: Vec<Vec<Vec<LevelRef>>>,
    pub unwritten_overview: Vec<Vec<Vec<Arc<OverviewBlock>>>>,
    seen: Seen,
    /// What the latest snapshots changed, oldest first, at most `CHANGES` of them.
    changes: VecDeque<Change>,
}

/// The snapshots whose changes a live recording keeps: an answer older than these is not
/// extended.
#[cfg(feature = "live")]
const CHANGES: usize = 64;

/// Which recording an answer was made from, and how many of its writer's snapshots it had taken
/// then: what `Recording::extend` reads to know what changed since.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Revision {
    recording: u64,
    snapshots: u64,
}

/// A cancel flag nothing raises: the extent's reads run to their end.
static NEVER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Each recording's own number, so an answer is never extended by another's changes.
static RECORDINGS: AtomicU64 = AtomicU64::new(0);

impl Revision {
    fn new() -> Self {
        Self {
            recording: RECORDINGS.fetch_add(1, Ordering::Relaxed),
            snapshots: 0,
        }
    }
}

/// What snapshot `snapshots` changed: by channel index, the first axis tick whose data can
/// differ; and by channel index, each overview level whose settled buckets moved, by shift, the
/// first tick read from a different level.
#[cfg(feature = "live")]
struct Change {
    snapshots: u64,
    channels: Vec<i64>,
    levels: Vec<Vec<(u8, i64)>>,
}

/// What the snapshots since an answer changed, together.
pub(crate) struct Changed {
    channels: Vec<i64>,
    levels: Vec<Vec<(u8, i64)>>,
}

impl Changed {
    /// The first axis tick where channel `c`'s columns of `per` ticks can differ: its data's,
    /// or that of one of its levels whose buckets fit in a column.
    pub fn from(&self, c: usize, per: f64) -> i64 {
        let fits = |shift: u8| {
            1u64.checked_shl(u32::from(shift))
                .map_or(f64::MAX, |b| b as f64)
                <= per
        };
        let levels = self.levels.get(c).into_iter().flatten();
        let data = self.channels.get(c).copied().unwrap_or(i64::MIN);
        levels
            .filter(|l| fits(l.0))
            .map(|l| l.1)
            .fold(data, i64::min)
    }
}

/// How much of the writer's append-only lists the tables hold: each channel's gaps or runs, and
/// the annotation records.
#[cfg(feature = "live")]
struct Seen {
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
    /// A finished file's channel whose list is in pages holds an empty list here until it is
    /// read: `place` gives the channel's place, reading it.
    pub(crate) places: Vec<Place>,
    /// A live recording's blocks of each channel; a finished file's are read a page at a time.
    /// A derived channel's samples come from its operands'.
    #[cfg(feature = "live")]
    pub(crate) blocks: Vec<Vec<BlockAt>>,
    /// Each channel's overview levels, then each derived channel's, finest first.
    pub(crate) levels: Vec<Vec<LevelAt>>,
    /// A finished file's are read from its bundle when first asked for; what went wrong, where
    /// that failed.
    markers: OnceLock<Result<Markers, String>>,
    /// Whether a finished file's footer places an annotation bundle; None once its markers come
    /// from elsewhere, a sidecar or a writer.
    bundled: Option<bool>,
    pub(crate) sources: Vec<SourceHeader>,
    pub(crate) products: Vec<(usize, usize, Product)>,
    /// The data's extent, markers aside: worked out when first asked for, since a finished
    /// file's places can need their lists read.
    extent: OnceLock<Option<Range<i64>>>,
    /// The extent a sketch takes, from what the header and the footer state.
    sketch_extent: OnceLock<Option<Range<i64>>>,
    pub(crate) revision: Revision,
}

/// A recording's markers in order, and from the first's start to the latest any reaches: what
/// they add to the extent.
#[derive(Default)]
pub(crate) struct Markers {
    pub list: Vec<Marker>,
    pub span: Option<Range<i64>>,
}

impl Markers {
    fn of(list: Vec<Marker>) -> Self {
        Self {
            span: span_of(&list),
            list,
        }
    }
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

#[allow(clippy::too_many_arguments)]
fn analog_place(
    first: i64,
    period: Ratio,
    count: u64,
    breaks: Vec<Break>,
    lost_before: u64,
    lost_after: u64,
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
        lost_before,
        lost_after,
        end,
        rule,
        map,
    }
}

/// A derived channel's place: its first operand's samples, valued by its product.
fn derived_place(operand: &Place, product: Product) -> Place {
    let Place::Analog {
        first,
        period,
        count,
        breaks,
        lost,
        lost_before,
        lost_after,
        end,
        map,
        ..
    } = operand
    else {
        unreachable!("a checked header's operands are analog")
    };
    Place::Analog {
        first: *first,
        period: *period,
        count: *count,
        breaks: breaks.clone(),
        lost: *lost,
        lost_before: *lost_before,
        lost_after: *lost_after,
        end: *end,
        rule: Rule::Product(product),
        map: *map,
    }
}

fn derived_info(
    d: &escapement_tickwave_format::header::Derived,
    operands: (usize, usize),
    product: Product,
) -> ChannelInfo {
    ChannelInfo {
        name: d.name.clone(),
        kind: ChannelKind::Derived { operands },
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
    }
}

/// Adds the derived channels, placed as their first operand's samples are.
fn add_derived(
    built: &mut Built,
    derived: &[escapement_tickwave_format::header::Derived],
    products: &[(usize, usize, Product)],
) {
    for (d, &(a, b, product)) in derived.iter().zip(products) {
        built.places.push(derived_place(&built.places[a], product));
        built.channels.push(derived_info(d, (a, b), product));
    }
}

/// Stored channel `i` of `footer`, with its gaps or breaks as the footer holds them: none for a
/// list in pages not yet read.
fn stored_place(footer: &Footer, i: usize) -> Place {
    let h = &footer.header;
    let c = &h.channels[i];
    // A checked header maps every channel.
    let map = h.mapping(i).unwrap_or(Mapping::IDENTITY);
    match &c.data {
        HeaderKind::Digital { initial, end, .. } => {
            let gaps = footer.gaps(i).unwrap_or_default();
            let start = h.start(i).unwrap_or(0);
            let mut place = digital_place(*initial, start, *end, gaps, map);
            if let Place::Digital { gap_count, .. } = &mut place {
                *gap_count = c.data.list_len() as usize;
            }
            place
        }
        HeaderKind::Analog {
            first,
            period,
            count,
            coding,
            lost_before,
            lost_after,
            ..
        } => analog_place(
            *first,
            *period,
            *count,
            footer.breaks(i).unwrap_or_default().to_vec(),
            *lost_before,
            *lost_after,
            h.length,
            Rule::Coding(*coding),
            map,
        ),
    }
}

/// What a span of a finished channel's ticks needs of its gaps or breaks in pages (see `List`),
/// and whether those reach the list's last page. A digital channel's gap from its start is among
/// them where the span reaches the list's first page; past that page the gap ends before the
/// span, so it changes nothing there.
pub(crate) struct Slice {
    pub list: List,
    pub to_end: bool,
}

/// Stored channel `i` of `footer` from `slice` of its list. A span's place: exact for the ticks
/// of the span the slice was read for, and the sample either side of it. Where the slice holds
/// the list's last run its data's end is the channel's, else the length, past every span, and
/// no samples lost after its last: only the last run places those, and the slice doesn't hold it.
fn sliced_place(footer: &Footer, i: usize, slice: Slice) -> Place {
    let h = &footer.header;
    let c = &h.channels[i];
    let map = h.mapping(i).unwrap_or(Mapping::IDENTITY);
    let mut place = match (&c.data, slice.list) {
        (HeaderKind::Digital { initial, end, .. }, List::Gaps(gaps)) => {
            let start = h.start(i).unwrap_or(0);
            digital_place(*initial, start, *end, &gaps, map)
        }
        (
            HeaderKind::Analog {
                first,
                period,
                count,
                coding,
                lost_before,
                lost_after,
                ..
            },
            List::Breaks(breaks),
        ) => analog_place(
            *first,
            *period,
            *count,
            breaks,
            *lost_before,
            *lost_after,
            h.length,
            Rule::Coding(*coding),
            map,
        ),
        _ => stored_place(footer, i),
    };
    match &mut place {
        Place::Digital { gap_count, .. } => *gap_count = c.data.list_len() as usize,
        Place::Analog {
            end, lost_after, ..
        } if !slice.to_end => {
            *end = h.length;
            *lost_after = 0;
        }
        Place::Analog { .. } => {}
    }
    place
}

/// A channel's place from its header alone, for a sketch: no gap or break, and an analog
/// channel's data running to the length, which bounds wherever its last run ends. The samples
/// lost after its last are left out, since only its last run places them.
fn header_place(place: &Place, length: i64) -> Place {
    let mut place = place.clone();
    if let Place::Analog {
        end, lost_after, ..
    } = &mut place
    {
        *end = length;
        *lost_after = 0;
    }
    place
}

/// The channels of `footer`, those whose lists are in pages not yet read with none.
fn header_channels(footer: &Footer) -> Built {
    let h = &footer.header;
    let mut built = Built {
        channels: Vec::new(),
        places: Vec::new(),
    };
    for (i, c) in h.channels.iter().enumerate() {
        built.channels.push(match &c.data {
            HeaderKind::Digital { .. } => ChannelInfo {
                name: c.name.clone(),
                kind: ChannelKind::Digital,
                unit: String::new(),
                resolution: None,
                significant: None,
            },
            HeaderKind::Analog {
                unit,
                coding,
                significant,
                ..
            } => analog_info(&c.name, unit, *coding, *significant),
        });
        built.places.push(stored_place(footer, i));
    }
    let derived: Vec<_> = h.derived.iter().map(|d| d.declared()).collect();
    add_derived(&mut built, &derived, &footer.products);
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
    /// A finished file, from its trailer and footer: nothing else is read until asked for. Its
    /// bundled annotations are its markers, read when first asked for. `Error::Unfinished` for a
    /// file with no trailer (cut off by a crash), so the host can offer the Recutter's repair;
    /// `open` never repairs in memory.
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
        let Built { channels, places } = header_channels(&footer);
        let h = &footer.header;
        let stored = h.channels.len();
        // A channel's list in pages, or its first operand's, is read when it is first asked for.
        let listed = (0..places.len())
            .map(|o| {
                let c = o.checked_sub(stored).map_or(o, |k| footer.products[k].0);
                (!footer.list_known(c)).then(OnceLock::new)
            })
            .collect();
        let levels = footer
            .channels
            .iter()
            .map(|ch| {
                let in_blocks = ch.levels.iter().enumerate().map(|(li, (l, _))| LevelAt {
                    shift: l.shift,
                    parts: vec![Part::Blocks(li)],
                    covered: format_overview::buckets(h.length, l.shift),
                });
                in_blocks
                    .chain([LevelAt {
                        shift: ch.strip_shift,
                        parts: vec![Part::Strip],
                        covered: format_overview::buckets(h.length, ch.strip_shift),
                    }])
                    .collect()
            })
            .collect();
        Ok(Self {
            timebase: h.timebase,
            length: h.length,
            sources: h.sources.clone(),
            products: footer.products.clone(),
            bytes,
            cache,
            channels,
            places,
            #[cfg(feature = "live")]
            blocks: vec![Vec::new(); stored],
            levels,
            markers: OnceLock::new(),
            bundled: Some(footer.bundle.is_some()),
            extent: OnceLock::new(),
            sketch_extent: OnceLock::new(),
            store: Store::Finished(Box::new(Finished {
                strips: (0..footer.channels.len())
                    .map(|_| OnceLock::new())
                    .collect(),
                footer: RwLock::new(footer),
                listed,
            })),
            revision: Revision::new(),
        })
    }

    /// Markers from a sidecar the host passes, in place of the file's bundled ones: `hand` the
    /// hand-written file's TOML, and the files it includes. Every problem it finds, joined.
    pub fn with_sidecar(mut self, hand: &str, included: &[Included<'_>]) -> Result<Self, Error> {
        let resolved = match &self.store {
            Store::Finished(f) => annotations::resolve(hand, included, &f.footer().header, None),
            #[cfg(feature = "live")]
            Store::Live(_) => {
                return Err(Error::Annotations(
                    "a recording still being written takes its writer's annotations".into(),
                ));
            }
        };
        let list =
            markers_of(resolved.map_err(|problems| Error::Annotations(problems.join("; ")))?);
        self.markers = OnceLock::from(Ok(Markers::of(list)));
        self.bundled = None;
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
                        0,
                        0,
                        Rule::Coding(*coding),
                        map,
                    ));
                }
            }
        }
        add_derived(&mut built, &d.derived, &products);
        let n = d.channels.len();
        let overviews = built.places.len();
        let mut rec = Self {
            timebase: d.timebase,
            length: 0,
            sources: d.sources.clone(),
            products,
            bytes,
            cache,
            channels: built.channels,
            places: built.places,
            blocks: vec![Vec::new(); n],
            levels: vec![Vec::new(); overviews],
            markers: OnceLock::from(Ok(Markers::default())),
            bundled: None,
            extent: OnceLock::new(),
            sketch_extent: OnceLock::new(),
            store: Store::Live(Box::new(Live {
                snapshot,
                refs: vec![Vec::new(); n],
                unwritten: vec![Vec::new(); n],
                rows: vec![Vec::new(); overviews],
                unwritten_overview: vec![Vec::new(); overviews],
                seen: Seen {
                    gaps: vec![0; n],
                    runs: vec![0; n],
                    annotations: 0,
                },
                changes: VecDeque::new(),
            })),
            revision: Revision::new(),
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
            bytes,
            store,
            places,
            blocks,
            levels,
            markers,
            products,
            length,
            revision,
            ..
        } = self;
        let Store::Live(live) = store else {
            unreachable!("advance is for a live recording");
        };
        let Live {
            snapshot,
            refs,
            unwritten,
            rows,
            unwritten_overview,
            seen,
            changes,
        } = &mut **live;
        let d = snapshot.declaration();
        let stored = d.channels.len();
        let mut fetch = |offset: u64, len: u32| -> Result<Vec<u8>, Error> {
            Ok(read(&**bytes, offset, len as usize)?)
        };

        // What can fail goes first, so an error leaves the tables as they were: each channel's
        // pages past those it has, and the blocks and overview blocks not yet written, decoded
        // again each time, few as they are.
        let mut fresh_refs = Vec::with_capacity(stored);
        let mut fresh = Vec::with_capacity(stored);
        for (c, known) in refs.iter().enumerate() {
            fresh_refs.push(snapshot.block_refs(c, known, &mut fetch)?);
            fresh.push(snapshot.unwritten_blocks(c)?);
        }
        let mut fresh_rows = Vec::with_capacity(places.len());
        let mut fresh_overview = Vec::with_capacity(places.len());
        for (o, known_rows) in rows.iter().enumerate() {
            let ladder = snapshot.overview(o).expect("an overview for every channel");
            let (mut level_rows, mut level_blocks) = (Vec::new(), Vec::new());
            for (li, level) in ladder.levels.iter().enumerate() {
                if level.in_blocks {
                    let known = known_rows.get(li).map_or(&[][..], |r| &r[..]);
                    level_rows.push(snapshot.level_refs(o, li, known, &mut fetch)?);
                    let blocks = snapshot.unwritten_overview(o, li)?;
                    level_blocks.push(blocks.into_iter().map(Arc::new).collect::<Vec<_>>());
                } else {
                    level_rows.push(Vec::new());
                    level_blocks.push(Vec::new());
                }
            }
            fresh_rows.push(level_rows);
            fresh_overview.push(level_blocks);
        }

        // The gaps and runs since the last snapshot, each channel's count, and the latest tick.
        // Every item a writer takes is at or after its channel's latest one, which ended the old
        // length, so a channel's data changes from there, and from each new gap's start.
        let settled = (*length).saturating_sub(1);
        let mut changed = vec![settled; places.len()];
        let mut new_length = 0i64;
        for (c, place) in places[..stored].iter_mut().enumerate() {
            let open = fresh[c].last().expect("a channel's open block");
            let mut last = open.start - 1;
            match (place, &open.data) {
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
                        changed[c] = changed[c].min(on_axis.from);
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
                        lost_before,
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
                            *lost_before = r.lost;
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
                _ => unreachable!("a block's channel follows the declaration"),
            }
            new_length = new_length.max(open.start).max(last + 1);
        }
        *length = new_length.max(0);
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
        // A derived channel's samples sit where its operands' do, as many as both hold.
        for (k, &(a, b, _)) in products.iter().enumerate() {
            changed[stored + k] = changed[a].min(changed[b]);
            let (operands, derived) = places.split_at_mut(stored);
            let (
                Place::Analog {
                    first,
                    period,
                    count,
                    breaks,
                    lost,
                    lost_before,
                    map,
                    ..
                },
                Place::Analog { count: other, .. },
                Place::Analog {
                    first: d_first,
                    count: d_count,
                    breaks: d_breaks,
                    lost: d_lost,
                    lost_before: d_lost_before,
                    end: d_end,
                    ..
                },
            ) = (&operands[a], &operands[b], &mut derived[k])
            else {
                unreachable!("a checked declaration's operands are analog");
            };
            let n = (*count).min(*other);
            let runs = Runs {
                first: *first,
                period: *period,
                count: n,
                breaks,
                map: *map,
            };
            let end = match n {
                0 => runs.start(),
                n => (*length).min(place_after(&runs, n - 1)),
            };
            (*d_first, *d_count, *d_lost, *d_lost_before, *d_end) =
                (*first, n, *lost, *lost_before, end);
            d_breaks.extend_from_slice(&breaks[d_breaks.len()..]);
        }

        // Each channel's written blocks, then those not yet written, the open one to the length.
        for c in 0..stored {
            let open = fresh[c].len() - 1;
            blocks[c] = fresh_refs[c]
                .iter()
                .map(|b| BlockAt {
                    start: b.start,
                    end: b.end,
                    from: BlockFrom::Frame(*b),
                })
                .chain(fresh[c].iter().enumerate().filter_map(|(j, u)| {
                    let end = if j == open { *length } else { u.end };
                    (end > u.start).then_some(BlockAt {
                        start: u.start,
                        end,
                        from: BlockFrom::Unwritten(j),
                    })
                }))
                .collect();
            let o = &mut fresh[c][open];
            o.end = (*length).max(o.start);
        }
        *refs = fresh_refs;
        *unwritten = fresh
            .into_iter()
            .map(|blocks| blocks.into_iter().map(Arc::new).collect())
            .collect();

        // Each channel's levels a file finished at this length would keep, so a live view
        // chooses as that file's would. A few a channel, built again.
        let before: Vec<Vec<(u8, i64)>> = levels
            .iter()
            .map(|ls| ls.iter().map(|l| (l.shift, l.end())).collect())
            .collect();
        for (o, channel_levels) in levels.iter_mut().enumerate() {
            channel_levels.clear();
            let finest = d.shift(o).expect("a shift for every channel");
            let kept = format_overview::ladder((*length).max(1), finest);
            let ladder = snapshot.overview(o).expect("an overview for every channel");
            for (li, level) in ladder.levels.iter().enumerate() {
                let covered = level.held.first + level.held.buckets();
                if covered == 0 || !kept.contains(&level.shift) {
                    continue;
                }
                let mut parts = Vec::new();
                if level.in_blocks {
                    parts.push(Part::LiveBlocks(li));
                }
                parts.push(Part::Held(li));
                channel_levels.push(LevelAt {
                    shift: level.shift,
                    parts,
                    covered,
                });
            }
        }
        *rows = fresh_rows;
        *unwritten_overview = fresh_overview;
        // A tick is read from the coarsest level a column fits whose buckets reach it, so where
        // a level's settled buckets end moves the source of the ticks between its old end and
        // its new one. A level not kept ends at 0.
        let end_of =
            |ls: &[(u8, i64)], shift: u8| ls.iter().find(|l| l.0 == shift).map_or(0, |l| l.1);
        let mut moved = Vec::with_capacity(levels.len());
        for (o, channel_levels) in levels.iter().enumerate() {
            let before = &before[o];
            let after: Vec<(u8, i64)> = channel_levels.iter().map(|l| (l.shift, l.end())).collect();
            let mut channel_moved: Vec<(u8, i64)> = Vec::new();
            for &(shift, _) in before.iter().chain(&after) {
                let (old, new) = (end_of(before, shift), end_of(&after, shift));
                if old != new && !channel_moved.iter().any(|m| m.0 == shift) {
                    channel_moved.push((shift, old.min(new)));
                }
            }
            moved.push(channel_moved);
        }
        revision.snapshots += 1;
        if changes.len() == CHANGES {
            changes.pop_front();
        }
        changes.push_back(Change {
            snapshots: revision.snapshots,
            channels: changed,
            levels: moved,
        });

        // The annotation records since the last snapshot, each a line of its generator's file.
        // A recorder's come in order of their ticks, so each goes in at the end, as a rule.
        let Some(Ok(Markers {
            list: markers,
            span: marker_span,
        })) = markers.get_mut()
        else {
            unreachable!("a live recording's markers are its writer's")
        };
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
        self.extent = OnceLock::new();
        self.sketch_extent = OnceLock::new();
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
    /// `lost` summed; 0 for a digital channel, one past the last, or one whose breaks don't read
    /// (its queries say why).
    pub fn lost(&self, channel: usize) -> u64 {
        match self.place(channel).ok() {
            Some(Place::Analog { lost, .. }) => *lost,
            _ => 0,
        }
    }

    /// An analog or derived channel's samples the instrument lost before its first sample
    /// (FORMAT.md, "Gaps"); 0 for a digital channel or one past the last.
    pub fn lost_before(&self, channel: usize) -> u64 {
        match self.places.get(channel) {
            Some(Place::Analog { lost_before, .. }) => *lost_before,
            _ => 0,
        }
    }

    /// An analog or derived channel's samples the instrument lost after its last sample; 0 for a
    /// digital channel or one past the last. A live recording's is 0: only the writer's finish
    /// states it.
    pub fn lost_after(&self, channel: usize) -> u64 {
        match self.places.get(channel) {
            Some(Place::Analog { lost_after, .. }) => *lost_after,
            _ => 0,
        }
    }

    /// A digital channel's gaps, one from tick 0 included; 0 for an analog channel. A finished
    /// file's header states the count, so it reads nothing.
    pub fn gaps(&self, channel: usize) -> usize {
        match self.places.get(channel) {
            Some(Place::Digital { gap_count, .. }) => *gap_count,
            _ => 0,
        }
    }

    /// The markers, a finished file's read from its bundle if they aren't yet; 0 where they
    /// don't read (a query asking for markers says why).
    pub fn marker_count(&self) -> usize {
        self.read_markers().map_or(0, |m| m.list.len())
    }

    /// Reads a finished file's markers from its bundle if they aren't read yet:
    /// `Error::Annotations` on every call while the bundle doesn't resolve, where
    /// `marker_count`, `markers` and `extent` give none. Threads that ask at once read the bundle
    /// once.
    pub fn load_markers(&self) -> Result<(), Error> {
        self.read_markers().map(|_| ())
    }

    /// Whether the recording has markers, reading nothing: a finished file whose footer places an
    /// annotation bundle, whatever it holds and whether or not it resolves, or one given a
    /// sidecar with any; a live one whose writer has taken any. So a file whose bundle doesn't
    /// resolve has markers, lost: `load_markers` says why.
    pub fn has_markers(&self) -> bool {
        match self.bundled {
            Some(bundled) => bundled,
            None => self.read_markers().is_ok_and(|m| !m.list.is_empty()),
        }
    }

    /// The markers, a finished file's read from its bundle the first time.
    pub(crate) fn read_markers(&self) -> Result<&Markers, Error> {
        self.markers
            .get_or_init(|| self.bundled_markers().map_err(|e| e.to_string()))
            .as_ref()
            .map_err(|e| Error::Annotations(e.clone()))
    }

    /// The store of a finished file; None for a live recording.
    fn finished(&self) -> Option<&Finished> {
        match &self.store {
            Store::Finished(f) => Some(f),
            #[cfg(feature = "live")]
            Store::Live(_) => None,
        }
    }

    fn bundled_markers(&self) -> Result<Markers, Error> {
        let Some(f) = self.finished() else {
            return Ok(Markers::default());
        };
        let _span = tracing::debug_span!("read markers").entered();
        let footer = f.footer();
        let fetch = |offset: u64, len: u32| -> Result<Vec<u8>, Error> {
            Ok(read(&*self.bytes, offset, len as usize)?)
        };
        let list = match footer.read_bundle(fetch)?.as_ref().map(bundled) {
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
        Ok(Markers::of(list))
    }

    /// From the first sample to the end of the last sample's time, over every analog channel,
    /// widened by the markers, a finished file's read from its bundle if they aren't yet. A
    /// recording with no analog sample takes its digital channels' known span instead. None
    /// before any data and any marker.
    pub fn extent(&self) -> Option<Range<i64>> {
        let mut extent = self.data_extent();
        if let Ok(Markers {
            span: Some(span), ..
        }) = self.read_markers()
        {
            widen(&mut extent, span.clone());
        }
        extent
    }

    /// `extent`, the markers aside. An analog channel whose list is in pages reads the pages its
    /// last run needs; a digital one, read only where no analog channel has a sample, the pages
    /// its start needs.
    pub(crate) fn data_extent(&self) -> Option<Range<i64>> {
        let reading = Reading::new(&NEVER);
        let place = |c: usize| {
            let span = match self.places.get(c) {
                Some(Place::Analog { .. }) => self.length.saturating_sub(1)..self.length,
                _ => 0..1,
            };
            self.place_over(&reading, c, span).ok()
        };
        self.extent.get_or_init(|| self.find_extent(place)).clone()
    }

    /// `data_extent` from what the header and the footer state, reading nothing: a channel whose
    /// list is in pages has its data run to the length (`header_place`).
    pub(crate) fn sketch_extent(&self) -> Option<Range<i64>> {
        let place = |c: usize| self.sketch_place(c).ok();
        self.sketch_extent
            .get_or_init(|| self.find_extent(place))
            .clone()
    }

    /// The extent of the channels whose places `place` gives: one whose list doesn't read is
    /// left out, and its queries say why.
    fn find_extent<'a>(
        &'a self,
        place: impl Fn(usize) -> Option<Cow<'a, Place>>,
    ) -> Option<Range<i64>> {
        let mut extent: Option<Range<i64>> = None;
        let stored = self.places.len() - self.products.len();
        // The analog channels' places, and the digital ones' only where no analog channel has a
        // sample.
        let analog = |c: &usize| matches!(self.places[*c], Place::Analog { .. });
        let places: Vec<Cow<'a, Place>> = (0..stored).filter(analog).filter_map(&place).collect();
        for place in &places {
            // From the first sample to where the last one's time ends, which a place from the
            // header alone puts at the length.
            if let Place::Analog { count, end, .. } = &**place
                && *count > 0
                && let Some(runs) = place.runs()
            {
                widen(&mut extent, runs.start()..*end);
            }
        }
        if extent.is_none() {
            for place in (0..stored).filter(|c| !analog(c)).filter_map(&place) {
                if let Place::Digital { start, end, .. } = &*place {
                    widen(&mut extent, *start..*end);
                }
            }
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

    /// Channel `channel`'s place. A finished file's channel whose list, or first operand's, is
    /// in pages reads it the first time.
    pub(crate) fn place(&self, channel: usize) -> Result<&Place, Error> {
        let place = self.places.get(channel).ok_or(Error::NoChannel(channel))?;
        let Some(f) = self.finished() else {
            return Ok(place);
        };
        let Some(cell) = f.listed.get(channel).and_then(Option::as_ref) else {
            return Ok(place);
        };
        if let Some(listed) = cell.get() {
            return Ok(listed);
        }
        let stored = self.places.len() - self.products.len();
        let listed = match channel.checked_sub(stored) {
            Some(k) => {
                let (a, _, product) = self.products[k];
                derived_place(self.place(a)?, product)
            }
            None => {
                let bytes = &self.bytes;
                let mut fetch = |offset: u64, len: u32| -> Result<Vec<u8>, Error> {
                    Ok(read(&**bytes, offset, len as usize)?)
                };
                let mut footer = f.footer.write().unwrap_or_else(|e| e.into_inner());
                let _span = tracing::debug_span!("read list", channel).entered();
                footer.load_list(channel, &mut fetch)?;
                stored_place(&footer, channel)
            }
        };
        Ok(cell.get_or_init(|| listed))
    }

    /// Whether channel `channel` is a finished file's whose list, or first operand's, is in pages
    /// not read whole: its `places` entry holds no gap or break.
    pub(crate) fn paged(&self, channel: usize) -> bool {
        match &self.store {
            Store::Finished(f) => f
                .listed
                .get(channel)
                .and_then(Option::as_ref)
                .is_some_and(|cell| cell.get().is_none()),
            #[cfg(feature = "live")]
            Store::Live(_) => false,
        }
    }

    /// Channel `channel`'s place, exact over the axis ticks of `span` and the sample either side
    /// of it. A channel whose list is in pages, and not read whole, reads only the pages of its
    /// list that `span` needs (see `List`), through `r`.
    pub(crate) fn place_over(
        &self,
        r: &Reading,
        channel: usize,
        span: Range<i64>,
    ) -> Result<Cow<'_, Place>, Error> {
        if !self.paged(channel) {
            return Ok(Cow::Borrowed(self.place(channel)?));
        }
        let Some(f) = self.finished() else {
            unreachable!("only a finished file's lists are in pages");
        };
        let stored = self.places.len() - self.products.len();
        if let Some(k) = channel.checked_sub(stored) {
            let (a, _, product) = self.products[k];
            let operand = self.place_over(r, a, span)?;
            return Ok(Cow::Owned(derived_place(&operand, product)));
        }
        let slice = self.list_over(r, f, channel, span)?;
        Ok(Cow::Owned(sliced_place(&f.footer(), channel, slice)))
    }

    /// Channel `channel`'s place for a sketch, which reads nothing past the footer: a channel
    /// whose list is in pages not read whole is its header's alone (`header_place`).
    pub(crate) fn sketch_place(&self, channel: usize) -> Result<Cow<'_, Place>, Error> {
        if !self.paged(channel) {
            return Ok(Cow::Borrowed(self.place(channel)?));
        }
        let place = self.places.get(channel).ok_or(Error::NoChannel(channel))?;
        Ok(Cow::Owned(header_place(place, self.length)))
    }

    /// Stored channel `c`'s gaps or breaks in pages that the axis ticks of `span` need (see
    /// `List`), each page read once into the cache and checked by `Footer::list_page`.
    pub(crate) fn list_over(
        &self,
        r: &Reading,
        f: &Finished,
        c: usize,
        span: Range<i64>,
    ) -> Result<Slice, Error> {
        let (table, map) = {
            let footer = f.footer();
            let table = footer
                .channels
                .get(c)
                .and_then(|ch| ch.list.clone())
                .ok_or(Error::NoChannel(c))?;
            (table, footer.mapping(c).ok_or(Error::NoChannel(c))?)
        };
        let gaps = table.table == escapement_tickwave_format::index::Table::Gaps;
        // The list's keys are source ticks: a tick either way of the span's, so no rounding
        // leaves out an entry it needs.
        let (lo, hi) = (
            map.first_at(span.start).saturating_sub(1),
            map.first_at(span.end).saturating_add(1),
        );
        let first_entry = table.levels.last().and_then(|l| l.first()).copied();
        // The page that starts at or holds key `k`.
        let page_at = |k: i64| -> Result<Option<Located>, Error> {
            Ok(self
                .pages(r, &table, c, k..k.saturating_add(1))?
                .into_iter()
                .next())
        };
        let mut pages = self.pages(r, &table, c, lo..hi)?;
        if pages.is_empty()
            && let Some(e) = first_entry
            && let Some(at) = page_at(e.first)?
        {
            // A span before the list's first entry: the first page bounds its run.
            pages.push(at);
        }
        let mut lists: Vec<(u64, Arc<List>)> = Vec::with_capacity(pages.len() + 2);
        for at in &pages {
            lists.push((at.j, self.list_page(r, f, &table, c, at)?));
        }
        let key = |l: &List, i: usize| match l {
            List::Gaps(g) => g[i].from,
            List::Breaks(b) => b[i].tick,
        };
        let len = |l: &List| match l {
            List::Gaps(g) => g.len(),
            List::Breaks(b) => b.len(),
        };
        // Two entries at or before the span's start: the page before the first where it holds
        // fewer.
        if let Some((j, first)) = lists.first().cloned() {
            let before = (0..len(&first)).filter(|&i| key(&first, i) <= lo).count();
            if before < 2
                && j > 0
                && let Some(e) = pages.first().map(|p| p.entry)
                && let Some(at) = page_at(e.first.saturating_sub(1))?
                && at.j + 1 == j
            {
                lists.insert(0, (at.j, self.list_page(r, f, &table, c, &at)?));
            }
        }
        // A channel's breaks through the first at or past the span's end: the page after the
        // last where it holds none.
        if !gaps
            && let Some(last) = pages.last()
            && let Some((next, _)) = last.next
            && lists
                .last()
                .is_some_and(|(_, l)| (0..len(l)).all(|i| key(l, i) < hi))
            && let Some(at) = page_at(next)?
            && at.j == last.j + 1
        {
            lists.push((at.j, self.list_page(r, f, &table, c, &at)?));
            pages.push(at);
        }
        let to_end = pages.iter().any(|p| p.next.is_none());
        // Each page was checked against where the next starts, so they follow on.
        let list = if gaps {
            let mut out: Vec<Gap> = Vec::new();
            for (_, l) in &lists {
                if let List::Gaps(g) = &**l {
                    out.extend_from_slice(g);
                }
            }
            List::Gaps(out)
        } else {
            let mut out: Vec<Break> = Vec::new();
            for (_, l) in &lists {
                if let List::Breaks(b) = &**l {
                    out.extend_from_slice(b);
                }
            }
            List::Breaks(out)
        };
        Ok(Slice { list, to_end })
    }

    /// Page `at` of stored channel `c`'s list, read once into the cache and checked.
    fn list_page(
        &self,
        r: &Reading,
        f: &Finished,
        table: &TableIndex,
        c: usize,
        at: &Located,
    ) -> Result<Arc<List>, Error> {
        let key = Key::Leaf(c, table.table, at.j);
        let entry = r.get_or_make(&self.cache, key, || {
            let _span = tracing::debug_span!("read list page", channel = c, page = at.j).entered();
            let bytes = read(&*self.bytes, at.entry.offset, at.entry.length as usize)?;
            let page = table.parse_page(0, at.j, &at.entry, &bytes)?;
            let list = f.footer().list_page(c, at, &page)?;
            let size = match &list {
                List::Gaps(g) => list_size(g),
                List::Breaks(b) => list_size(b),
            };
            Ok::<_, Error>((Entry::List(Arc::new(list)), size))
        })?;
        match entry {
            Entry::List(l) => Ok(l),
            _ => Err(DecodeError::Corrupt(format!(
                "channel {c}: a list page is listed where another kind of record is"
            ))
            .into()),
        }
    }

    /// What changed since revision `r` of this recording; None for another recording's, a later
    /// one, or one older than the changes kept.
    pub(crate) fn changed_since(&self, r: Revision) -> Option<Changed> {
        let now = self.revision;
        if r.recording != now.recording || r.snapshots > now.snapshots {
            return None;
        }
        let changed = Changed {
            channels: vec![i64::MAX; self.places.len()],
            levels: vec![Vec::new(); self.places.len()],
        };
        if r.snapshots == now.snapshots {
            return Some(changed);
        }
        match &self.store {
            Store::Finished(_) => None,
            #[cfg(feature = "live")]
            Store::Live(live) => {
                if live
                    .changes
                    .front()
                    .is_none_or(|c| c.snapshots > r.snapshots + 1)
                {
                    return None;
                }
                let mut changed = changed;
                for c in live.changes.iter().filter(|c| c.snapshots > r.snapshots) {
                    for (x, &y) in changed.channels.iter_mut().zip(&c.channels) {
                        *x = (*x).min(y);
                    }
                    for (mine, theirs) in changed.levels.iter_mut().zip(&c.levels) {
                        for &(shift, at) in theirs {
                            match mine.iter_mut().find(|l| l.0 == shift) {
                                Some(l) => l.1 = l.1.min(at),
                                None => mine.push((shift, at)),
                            }
                        }
                    }
                }
                Some(changed)
            }
        }
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
                overview_shift: None,
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
            let ch = live.snapshot.channel(0).unwrap();
            assert_eq!(
                live.refs[0].len(),
                ch.blocks.rows as usize + ch.rows.len(),
                "after edge {k}"
            );
            assert_eq!(
                live.seen.gaps[0],
                live.snapshot.gaps_from(0, 0).count(),
                "after edge {k}"
            );
            // The written blocks, those not yet written, then the open one where it holds
            // anything: an empty one has no ticks.
            assert_eq!(
                r.blocks[0].len(),
                live.refs[0].len() + live.unwritten[0].len() - 1,
                "after edge {k}"
            );
        }
    }
}
