//! The overview: each channel summarised in buckets of 2^shift ticks, so a wide view draws without
//! fetching blocks. A digital bucket holds the level at its start and whether the level changes
//! inside it. An analog bucket holds the least and greatest value in it.
//!
//! Decoded layout, for each level in the header's order and each channel within it:
//! - digital: the level bits, then the change bits, each `ceil(n / 8)` bytes, bucket i in bit
//!   `i % 8` of byte `i / 8`;
//! - analog: n minimums, then n maximums, as f32. A bucket with no sample holds +inf and -inf.

use std::collections::VecDeque;

use crate::header::{DeclaredChannel, DeclaredKind};
use crate::model::Gap;

/// The step between levels, as a shift.
pub const STEP: u8 = 4;
/// The coarsest shift a level can have.
pub const MAX_SHIFT: u8 = 62;
/// A level goes in the footer only if it has at most this many buckets.
pub const HEAD_BUCKETS: u64 = 1 << 17;
/// No coarser level is needed once one has this few buckets.
pub const WHOLE_VIEW: u64 = 1024;

/// Enough buckets to cover ticks `0..length`: `ceil(length / 2^shift)`, and at least one.
pub fn buckets(length: i64, shift: u8) -> u64 {
    ((length.max(1) - 1) as u64 >> shift) + 1
}

/// Buckets in one overview block, for a level kept outside the footer.
pub const BLOCK_BUCKETS: u64 = 1 << 16;
/// The finest level tickwave's converter declares has at most this many buckets: a finer one
/// would make the overview as large as the data.
pub const CONVERTER_BUCKETS: u64 = 1 << 26;
/// The finest shift tickwave's converter declares.
pub const CONVERTER_FINEST: u8 = 12;

/// The finest level a converter declares for a recording of `length`: the first of 12, 16, 20
/// and so on with at most 2^26 buckets.
pub fn converter_finest(length: i64) -> u8 {
    let mut shift = CONVERTER_FINEST;
    while buckets(length, shift) > CONVERTER_BUCKETS && shift + STEP <= MAX_SHIFT {
        shift += STEP;
    }
    shift
}

/// Which levels a recording keeps, as shifts, finest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// In the footer, fetched with it.
    pub head: Vec<u8>,
    /// In overview blocks, fetched as a view needs them.
    pub blocks: Vec<u8>,
}

/// The levels of a recording of `length` whose finest level is `finest`: every fourth shift from
/// it, up to the first with at most 1,024 buckets.
pub fn plan(length: i64, finest: u8) -> Plan {
    let mut plan = Plan {
        head: Vec::new(),
        blocks: Vec::new(),
    };
    let mut shift = finest;
    loop {
        let n = buckets(length, shift);
        if n <= HEAD_BUCKETS {
            plan.head.push(shift);
        } else {
            plan.blocks.push(shift);
        }
        if n <= WHOLE_VIEW || shift + STEP > MAX_SHIFT {
            return plan;
        }
        shift += STEP;
    }
}

/// The number of overview blocks a level kept outside the footer is cut into.
pub fn block_count(length: i64, shift: u8) -> u64 {
    buckets(length, shift).div_ceil(BLOCK_BUCKETS)
}

/// The buckets overview block `k` of a level holds: `(first bucket, count)`.
pub fn block_buckets(length: i64, shift: u8, k: u64) -> (u64, u64) {
    let first = k * BLOCK_BUCKETS;
    (first, BLOCK_BUCKETS.min(buckets(length, shift) - first))
}

#[derive(Clone, Debug, PartialEq)]
pub enum ChannelOverview {
    Digital { level: Vec<u8>, change: Vec<u8> },
    Analog { min: Vec<f32>, max: Vec<f32> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Level {
    pub shift: u8,
    pub buckets: u64,
    pub channels: Vec<ChannelOverview>,
}

impl Level {
    /// Buckets `first..first + n` as a level of their own. `first` is a multiple of 8, and the
    /// slice either ends on a multiple of 8 or at the level's end, so the bit arrays cut on byte
    /// boundaries.
    pub fn slice(&self, first: u64, n: u64) -> Level {
        assert!(first.is_multiple_of(8) && (n.is_multiple_of(8) || first + n == self.buckets));
        let bits = |b: &Vec<u8>| b[(first / 8) as usize..][..bit_bytes(n)].to_vec();
        let floats = |v: &Vec<f32>| v[first as usize..][..n as usize].to_vec();
        Level {
            shift: self.shift,
            buckets: n,
            channels: self
                .channels
                .iter()
                .map(|c| match c {
                    ChannelOverview::Digital { level, change } => ChannelOverview::Digital {
                        level: bits(level),
                        change: bits(change),
                    },
                    ChannelOverview::Analog { min, max } => ChannelOverview::Analog {
                        min: floats(min),
                        max: floats(max),
                    },
                })
                .collect(),
        }
    }
}

fn bit(bits: &[u8], i: u64) -> bool {
    bits[(i / 8) as usize] & (1 << (i % 8)) != 0
}

fn set(bits: &mut [u8], i: u64) {
    bits[(i / 8) as usize] |= 1 << (i % 8);
}

fn bit_bytes(n: u64) -> usize {
    n.div_ceil(8) as usize
}

impl ChannelOverview {
    /// The level at bucket `i`'s start and whether it changes inside, for a digital channel.
    pub fn digital(&self, i: u64) -> Option<(bool, bool)> {
        match self {
            Self::Digital { level, change } => Some((bit(level, i), bit(change, i))),
            Self::Analog { .. } => None,
        }
    }
}

/// A coarser level derived from a finer one. `shift` must exceed `fine.shift`.
pub fn coarsen(fine: &Level, shift: u8, length: i64) -> Level {
    let k = shift - fine.shift;
    let n = buckets(length, shift);
    let children = |b: u64| (b << k)..((b + 1) << k).min(fine.buckets);
    let channels = fine
        .channels
        .iter()
        .map(|c| match c {
            ChannelOverview::Digital { level, change } => {
                let mut out_level = vec![0; bit_bytes(n)];
                let mut out_change = vec![0; bit_bytes(n)];
                for b in 0..n {
                    let range = children(b);
                    if bit(level, range.start) {
                        set(&mut out_level, b);
                    }
                    if range.clone().any(|i| bit(change, i)) {
                        set(&mut out_change, b);
                    }
                }
                ChannelOverview::Digital {
                    level: out_level,
                    change: out_change,
                }
            }
            ChannelOverview::Analog { min, max } => {
                let range = |b: u64| children(b).start as usize..children(b).end as usize;
                ChannelOverview::Analog {
                    min: (0..n)
                        .map(|b| min[range(b)].iter().fold(f32::INFINITY, |a, &v| a.min(v)))
                        .collect(),
                    max: (0..n)
                        .map(|b| {
                            max[range(b)]
                                .iter()
                                .fold(f32::NEG_INFINITY, |a, &v| a.max(v))
                        })
                        .collect(),
                }
            }
        })
        .collect();
    Level {
        shift,
        buckets: n,
        channels,
    }
}

pub fn write(levels: &[Level], out: &mut Vec<u8>) {
    for level in levels {
        for channel in &level.channels {
            match channel {
                ChannelOverview::Digital { level, change } => {
                    out.extend_from_slice(level);
                    out.extend_from_slice(change);
                }
                ChannelOverview::Analog { min, max } => {
                    for v in min.iter().chain(max) {
                        out.extend_from_slice(&v.to_le_bytes());
                    }
                }
            }
        }
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let slice = self.bytes.get(self.pos..self.pos + n)?;
        self.pos += n;
        Some(slice)
    }
}

fn parse_channels(c: &mut Cursor<'_>, n: u64, analog: &[bool]) -> Option<Vec<ChannelOverview>> {
    let floats = |b: &[u8]| {
        b.as_chunks::<4>()
            .0
            .iter()
            .map(|&c| f32::from_le_bytes(c))
            .collect()
    };
    let mut channels = Vec::with_capacity(analog.len());
    for &is_analog in analog {
        channels.push(if is_analog {
            let min = floats(c.take(4 * n as usize)?);
            let max = floats(c.take(4 * n as usize)?);
            ChannelOverview::Analog { min, max }
        } else {
            let level = c.take(bit_bytes(n))?.to_vec();
            let change = c.take(bit_bytes(n))?.to_vec();
            ChannelOverview::Digital { level, change }
        });
    }
    Some(channels)
}

/// The decoded size of `n` buckets of every channel.
pub fn decoded_len(n: u64, analog: &[bool]) -> u64 {
    analog
        .iter()
        .map(|&a| if a { 8 * n } else { 2 * bit_bytes(n) as u64 })
        .sum()
}

/// Parses the footer's decoded overview. `analog[c]` says whether channel c is analog. None if
/// `bytes` is not exactly the size the levels need.
pub fn parse(bytes: &[u8], shifts: &[u8], length: i64, analog: &[bool]) -> Option<Vec<Level>> {
    let mut cursor = Cursor { bytes, pos: 0 };
    let mut levels = Vec::with_capacity(shifts.len());
    for &shift in shifts {
        let n = buckets(length, shift);
        levels.push(Level {
            shift,
            buckets: n,
            channels: parse_channels(&mut cursor, n, analog)?,
        });
    }
    (cursor.pos == bytes.len()).then_some(levels)
}

/// Parses one decoded overview block of `n` buckets.
pub fn parse_block(bytes: &[u8], shift: u8, n: u64, analog: &[bool]) -> Option<Level> {
    let mut cursor = Cursor { bytes, pos: 0 };
    let channels = parse_channels(&mut cursor, n, analog)?;
    (cursor.pos == bytes.len()).then_some(Level {
        shift,
        buckets: n,
        channels,
    })
}

/// One bucket of one channel.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Value {
    Digital { level: bool, change: bool },
    Analog { min: f32, max: f32 },
}

impl Value {
    fn empty_analog() -> Self {
        Value::Analog {
            min: f32::INFINITY,
            max: f32::NEG_INFINITY,
        }
    }

    /// A coarser bucket's value, taking `next`, a later child, into account.
    fn fold(self, next: Value) -> Value {
        match (self, next) {
            (Value::Digital { level, change }, Value::Digital { change: c, .. }) => {
                Value::Digital {
                    level,
                    change: change || c,
                }
            }
            (Value::Analog { min, max }, Value::Analog { min: lo, max: hi }) => Value::Analog {
                min: min.min(lo),
                max: max.max(hi),
            },
            _ => unreachable!("a channel keeps its kind"),
        }
    }
}

/// A digital channel's events in one finest bucket: the level entering it and the level after.
#[derive(Clone, Copy)]
struct DigitalEvent {
    bucket: u64,
    entering: bool,
    after: bool,
}

enum Intake {
    Digital {
        /// The level after every event taken.
        level: bool,
        /// The level entering the first unsettled bucket.
        entering: bool,
        /// Gap ends not yet reached, in order.
        pending: VecDeque<Gap>,
        events: VecDeque<DigitalEvent>,
    },
    Analog {
        events: VecDeque<(u64, f32, f32)>,
    },
}

/// A level's settled buckets not yet handed on, from bucket `first`.
struct Store {
    first: u64,
    n: u64,
    columns: Vec<ChannelOverview>,
}

impl Store {
    fn new(analog: &[bool], first: u64) -> Self {
        Self {
            first,
            n: 0,
            columns: analog
                .iter()
                .map(|&a| {
                    if a {
                        ChannelOverview::Analog {
                            min: Vec::new(),
                            max: Vec::new(),
                        }
                    } else {
                        ChannelOverview::Digital {
                            level: Vec::new(),
                            change: Vec::new(),
                        }
                    }
                })
                .collect(),
        }
    }

    fn push(&mut self, values: &[Value]) {
        let i = self.n;
        for (column, value) in self.columns.iter_mut().zip(values) {
            match (column, *value) {
                (
                    ChannelOverview::Digital { level, change },
                    Value::Digital {
                        level: l,
                        change: c,
                    },
                ) => {
                    if i.is_multiple_of(8) {
                        level.push(0);
                        change.push(0);
                    }
                    if l {
                        set(level, i);
                    }
                    if c {
                        set(change, i);
                    }
                }
                (ChannelOverview::Analog { min, max }, Value::Analog { min: lo, max: hi }) => {
                    min.push(lo);
                    max.push(hi);
                }
                _ => unreachable!("a channel keeps its kind"),
            }
        }
        self.n += 1;
    }

    /// The first `n` buckets as a level of their own; `n` is a multiple of 8 or all of them.
    fn split_front(&mut self, shift: u8, n: u64) -> Level {
        let bytes = bit_bytes(n);
        let channels = self
            .columns
            .iter_mut()
            .map(|c| match c {
                ChannelOverview::Digital { level, change } => ChannelOverview::Digital {
                    level: level.drain(..bytes).collect(),
                    change: change.drain(..bytes).collect(),
                },
                ChannelOverview::Analog { min, max } => ChannelOverview::Analog {
                    min: min.drain(..n as usize).collect(),
                    max: max.drain(..n as usize).collect(),
                },
            })
            .collect();
        self.first += n;
        self.n -= n;
        Level {
            shift,
            buckets: n,
            channels,
        }
    }
}

struct LevelBuild {
    shift: u8,
    /// Set once the level has more than 2^17 buckets: it goes in overview blocks.
    promoted: bool,
    store: Store,
    /// For a level coarser than the finest: the bucket its children are being folded into.
    open: Option<(u64, Vec<Value>)>,
}

/// An overview block whose buckets have settled: block `k` of the level with `shift`, decoded.
#[derive(Debug, PartialEq)]
pub struct Settled {
    pub shift: u8,
    pub k: u64,
    pub level: Level,
}

/// What the overview comes to at the end.
pub struct Finished {
    /// The levels the footer holds, finest first.
    pub head: Vec<Level>,
    pub plan: Plan,
}

/// Builds the overview as a recording's items arrive. Each channel's items and gap ends arrive in
/// tick order, and `settle` says when every item before a tick is in, so the buckets before it
/// cannot change. It holds the levels that may still go in the footer, and hands on overview
/// blocks as they settle.
pub struct Builder {
    finest: u8,
    intake: Vec<Intake>,
    /// Finest buckets before this have settled.
    base: u64,
    levels: Vec<LevelBuild>,
    settled: Vec<Settled>,
}

impl Builder {
    /// The overview of `channels`, as their declaration has them, from level `finest`.
    pub fn new(finest: u8, channels: &[DeclaredChannel]) -> Self {
        let intake = channels
            .iter()
            .map(|c| match c.data {
                DeclaredKind::Analog { .. } => Intake::Analog {
                    events: VecDeque::new(),
                },
                DeclaredKind::Digital { initial } => Intake::Digital {
                    level: initial,
                    entering: initial,
                    pending: VecDeque::new(),
                    events: VecDeque::new(),
                },
            })
            .collect();
        let analog: Vec<bool> = channels
            .iter()
            .map(|c| matches!(c.data, DeclaredKind::Analog { .. }))
            .collect();
        let levels = (finest..=MAX_SHIFT)
            .step_by(STEP as usize)
            .map(|shift| LevelBuild {
                shift,
                promoted: false,
                store: Store::new(&analog, 0),
                open: None,
            })
            .collect();
        Self {
            finest,
            intake,
            base: 0,
            levels,
            settled: Vec::new(),
        }
    }

    pub fn finest(&self) -> u8 {
        self.finest
    }

    fn digital_event(&mut self, c: usize, tick: i64, after: impl FnOnce(bool) -> bool) {
        let bucket = tick as u64 >> self.finest;
        debug_assert!(bucket >= self.base, "an event in a settled bucket");
        let Intake::Digital { level, events, .. } = &mut self.intake[c] else {
            unreachable!("an edge on a digital channel")
        };
        let entering = *level;
        *level = after(*level);
        match events.back_mut() {
            Some(e) if e.bucket == bucket => e.after = *level,
            _ => events.push_back(DigitalEvent {
                bucket,
                entering,
                after: *level,
            }),
        }
    }

    /// Takes the gap ends of digital channel `c` before `tick`.
    fn reach(&mut self, c: usize, tick: i64) {
        loop {
            let Intake::Digital { pending, .. } = &mut self.intake[c] else {
                return;
            };
            match pending.front() {
                Some(g) if g.to < tick => {
                    let g = pending.pop_front().unwrap();
                    self.digital_event(c, g.to, |_| g.level);
                }
                _ => return,
            }
        }
    }

    /// An edge of digital channel `c`, after its earlier edges and gap ends.
    pub fn edge(&mut self, c: usize, tick: i64) {
        self.reach(c, tick);
        self.digital_event(c, tick, |level| !level);
    }

    /// A gap of digital channel `c`: its end takes effect once the items pass it.
    pub fn gap(&mut self, c: usize, gap: Gap) {
        if let Intake::Digital { pending, .. } = &mut self.intake[c] {
            pending.push_back(gap);
        }
    }

    /// A sample of analog channel `c`. A NaN leaves its bucket as it was.
    pub fn sample(&mut self, c: usize, tick: i64, value: f32) {
        let bucket = tick as u64 >> self.finest;
        debug_assert!(bucket >= self.base, "a sample in a settled bucket");
        let Intake::Analog { events } = &mut self.intake[c] else {
            unreachable!("a sample on an analog channel")
        };
        match events.back_mut() {
            Some((b, min, max)) if *b == bucket => {
                *min = min.min(value);
                *max = max.max(value);
            }
            _ => events.push_back((
                bucket,
                f32::INFINITY.min(value),
                f32::NEG_INFINITY.max(value),
            )),
        }
    }

    /// Every item and gap end before `tick` is in. Settles the finest buckets wholly before it,
    /// and the coarser buckets they complete.
    pub fn settle(&mut self, tick: i64) {
        for c in 0..self.intake.len() {
            self.reach(c, tick);
        }
        self.settle_to(tick as u64 >> self.finest);
        for li in 0..self.levels.len() {
            let shift = self.levels[li].shift;
            if !self.levels[li].promoted && 17 + u32::from(shift) < 63 && tick > 1 << (17 + shift) {
                self.levels[li].promoted = true;
            }
            self.cut_blocks(li);
        }
    }

    /// Settles finest buckets `base..upto`.
    fn settle_to(&mut self, upto: u64) {
        let mut values = Vec::with_capacity(self.intake.len());
        while self.base < upto {
            let b = self.base;
            values.clear();
            for intake in &mut self.intake {
                values.push(match intake {
                    Intake::Digital {
                        entering, events, ..
                    } => match events.front() {
                        Some(e) if e.bucket == b => {
                            let e = events.pop_front().unwrap();
                            *entering = e.after;
                            Value::Digital {
                                level: e.entering,
                                change: true,
                            }
                        }
                        _ => Value::Digital {
                            level: *entering,
                            change: false,
                        },
                    },
                    Intake::Analog { events } => match events.front() {
                        Some(&(eb, min, max)) if eb == b => {
                            events.pop_front();
                            Value::Analog { min, max }
                        }
                        _ => Value::empty_analog(),
                    },
                });
            }
            self.push(0, b, &values);
            self.base += 1;
        }
    }

    /// Bucket `b` of level `li` has settled: store it, and fold it into the next level.
    fn push(&mut self, li: usize, b: u64, values: &[Value]) {
        self.levels[li].store.push(values);
        if self.levels[li].promoted && self.levels[li].store.n >= BLOCK_BUCKETS {
            self.cut_blocks(li);
        }
        let Some(next) = self.levels.get_mut(li + 1) else {
            return;
        };
        let parent = b >> STEP;
        let done = match &mut next.open {
            Some((p, acc)) if *p == parent => {
                for (a, v) in acc.iter_mut().zip(values) {
                    *a = a.fold(*v);
                }
                None
            }
            open => open.replace((parent, values.to_vec())),
        };
        // A child of a later parent: the one before has all its children.
        if let Some((p, acc)) = done {
            self.push(li + 1, p, &acc);
        }
    }

    /// Hands on level `li`'s whole overview blocks once it goes in overview blocks.
    fn cut_blocks(&mut self, li: usize) {
        let level = &mut self.levels[li];
        if !level.promoted {
            return;
        }
        while level.store.n >= BLOCK_BUCKETS {
            let k = level.store.first / BLOCK_BUCKETS;
            let block = level.store.split_front(level.shift, BLOCK_BUCKETS);
            self.settled.push(Settled {
                shift: level.shift,
                k,
                level: block,
            });
        }
    }

    /// The overview blocks settled since the last call, in the order they settled.
    pub fn take_settled(&mut self) -> Vec<Settled> {
        std::mem::take(&mut self.settled)
    }

    /// The recording ends at `length`; every item is in. Gap ends at or past it are dropped. The
    /// overview blocks this settles are left for `take_settled`.
    pub fn finish(mut self, length: i64) -> (Finished, Vec<Settled>) {
        self.settle(length);
        self.settle_to(buckets(length, self.finest));
        // The last bucket of each coarser level has only the children there are.
        for li in 1..self.levels.len() {
            if let Some((p, acc)) = self.levels[li].open.take() {
                self.push(li, p, &acc);
            }
        }
        let plan = plan(length, self.finest);
        let mut head = Vec::with_capacity(plan.head.len());
        for li in 0..self.levels.len() {
            let shift = self.levels[li].shift;
            if plan.blocks.contains(&shift) {
                self.levels[li].promoted = true;
                self.cut_blocks(li);
                let level = &mut self.levels[li];
                if level.store.n > 0 {
                    let k = level.store.first / BLOCK_BUCKETS;
                    let n = level.store.n;
                    let block = level.store.split_front(shift, n);
                    self.settled.push(Settled {
                        shift,
                        k,
                        level: block,
                    });
                }
            } else if plan.head.contains(&shift) {
                let level = &mut self.levels[li];
                debug_assert_eq!(level.store.first, 0, "a footer level was never cut");
                let n = level.store.n;
                head.push(level.store.split_front(shift, n));
            }
        }
        (Finished { head, plan }, self.settled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::AnalogCoding;
    use crate::model::Ratio;

    fn dig(initial: bool) -> DeclaredChannel {
        DeclaredChannel {
            name: "D".into(),
            source_name: "d".into(),
            data: DeclaredKind::Digital { initial },
        }
    }

    fn ana() -> DeclaredChannel {
        DeclaredChannel {
            name: "A".into(),
            source_name: "a".into(),
            data: DeclaredKind::Analog {
                unit: "V".into(),
                period: Ratio::ONE,
                coding: AnalogCoding::F32,
            },
        }
    }

    fn digital(level: &Level, c: usize) -> Vec<(bool, bool)> {
        (0..level.buckets)
            .map(|i| level.channels[c].digital(i).unwrap())
            .collect()
    }

    #[test]
    fn digital_buckets_by_hand() {
        // Buckets of 16 ticks over 0..64: edges at 3 (in bucket 0), 40 and 41 (bucket 2).
        let mut b = Builder::new(4, &[dig(true)]);
        for t in [3, 40, 41] {
            b.edge(0, t);
        }
        let (done, settled) = b.finish(64);
        assert!(settled.is_empty());
        // Bucket 0 starts high and changes; 1 starts low, quiet; 2 starts low and changes
        // (low, high, low); 3 starts low, quiet.
        assert_eq!(
            digital(&done.head[0], 0),
            [(true, true), (false, false), (false, true), (false, false)]
        );
    }

    #[test]
    fn a_gap_end_sets_its_bucket_and_the_level_from_there() {
        // Buckets of 16 ticks over 0..80. An edge at 5, a gap from 6 to 40 ending high, a settle
        // inside the gap, then an edge at 70.
        let mut b = Builder::new(4, &[dig(false)]);
        b.edge(0, 5);
        b.settle(20);
        b.gap(
            0,
            Gap {
                from: 6,
                to: 40,
                level: false,
            },
        );
        b.edge(0, 70);
        let (done, _) = b.finish(80);
        // 0: low, changes at 5. 1: high, carried through the gap. 2: high entering, the gap
        // ends low at 40. 3: low. 4: low, changes at 70.
        assert_eq!(
            digital(&done.head[0], 0),
            [
                (false, true),
                (true, false),
                (true, true),
                (false, false),
                (false, true)
            ]
        );
    }

    #[test]
    fn coarse_levels_fold_the_fine_ones() {
        let edges = [5i64, 70, 71, 300, 1000, 1001, 1002, 4095];
        let length = 1 << 16;
        let mut b = Builder::new(4, &[dig(false), ana()]);
        for (i, &t) in edges.iter().enumerate() {
            b.edge(0, t);
            b.sample(1, t, i as f32);
        }
        let (done, _) = b.finish(length);
        assert_eq!(done.plan.head, [4, 8]);
        for (fine, coarse) in done.head.iter().zip(&done.head[1..]) {
            assert_eq!(coarsen(fine, coarse.shift, length), *coarse);
        }
    }

    #[test]
    fn a_level_past_2_17_buckets_comes_out_in_blocks_as_it_settles() {
        // Shift 0: one bucket a tick. 2^17 + 3 ticks need 2^17 + 3 buckets, so shift 0 goes in
        // blocks: two whole ones once the items pass tick 2^17, then the last three buckets.
        let length = (1 << 17) + 3;
        let mut b = Builder::new(0, &[dig(false)]);
        b.edge(0, 9);
        b.settle(1 << 17);
        assert!(
            b.take_settled().is_empty(),
            "2^17 buckets may still fit the footer"
        );
        b.settle((1 << 17) + 1);
        let first: Vec<_> = b.take_settled().iter().map(|s| (s.shift, s.k)).collect();
        assert_eq!(first, [(0, 0), (0, 1)]);
        let (done, last) = b.finish(length);
        let last: Vec<_> = last
            .iter()
            .map(|s| (s.shift, s.k, s.level.buckets))
            .collect();
        assert_eq!(last, [(0, 2, 3)]);
        assert_eq!(done.plan.blocks, [0]);
        assert_eq!(done.plan.head, [4, 8]);
    }

    #[test]
    fn plans_split_head_and_blocks() {
        // 120 s at 1 GS/s: 2^12 (29.3M buckets) and 2^16 (1.8M) are too many for the footer;
        // 2^28 is the first with at most 1,024 (448).
        let p = plan(120_000_000_000, 12);
        assert_eq!(
            (p.head.as_slice(), p.blocks.as_slice()),
            (&[20, 24, 28][..], &[12, 16][..])
        );
        // 2^22 ticks fill exactly 1,024 buckets of 2^12, which is coarse enough.
        assert_eq!(plan(1 << 22, 12).head, [12]);
        assert_eq!(plan((1 << 22) + 1, 12).head, [12, 16]);
        assert_eq!(
            plan(1_000_000, 12),
            Plan {
                head: vec![12],
                blocks: vec![]
            }
        );
        assert_eq!(block_count(1 << 30, 12), 4);
        assert_eq!(block_count((1 << 30) + 1, 12), 5);
        assert_eq!(block_buckets((1 << 30) + 1, 12, 4), (4 * 65536, 1));
        // 3,600 s at 1 ps a tick: the converter starts where a level has at most 2^26 buckets.
        let length = 3_600_000_000_000_000;
        let finest = converter_finest(length);
        assert_eq!(finest, 28);
        assert!(buckets(length, finest) <= CONVERTER_BUCKETS);
        assert!(buckets(length, finest - STEP) > CONVERTER_BUCKETS);
        assert_eq!(converter_finest(1000), 12);
    }

    #[test]
    fn round_trips() {
        let mut b = Builder::new(4, &[dig(true), ana()]);
        b.edge(0, 17);
        b.sample(1, 3, 1.5);
        b.sample(1, 900, -2.0);
        let (done, _) = b.finish(1000);
        let mut bytes = Vec::new();
        write(&done.head, &mut bytes);
        assert_eq!(
            parse(&bytes, &done.plan.head, 1000, &[false, true]).as_ref(),
            Some(&done.head)
        );
        assert_eq!(
            parse(&bytes[1..], &done.plan.head, 1000, &[false, true]),
            None
        );
    }
}
