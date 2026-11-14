//! The overview: each channel summarised in buckets of 2^shift axis ticks, so a wide view draws
//! without fetching blocks. A digital bucket holds the level at its start and whether the level
//! changes inside it. An analog bucket holds the least and greatest of its samples and their sum,
//! in the types FORMAT.md's table gives; a derived channel's buckets follow the channels'.
//!
//! Decoded layout, for each level in the header's order, each channel in order, then each derived
//! channel:
//! - digital: the level bits, then the change bits, each `ceil(n / 8)` bytes, bucket i in bit
//!   `i % 8` of byte `i / 8`;
//! - analog: n minimums, then n maximums, then n sums. A bucket with no sample holds a minimum
//!   above its maximum, and a sum of 0.

use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;

use crate::header::{DeclaredChannel, DeclaredKind, Derived, Stored};
use crate::model::{Gap, Sample};
use crate::samples::Product;

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
pub const BLOCK_BUCKETS: u64 = 1 << 12;
/// The buckets a writer seals into one shared piece of a level it holds, so a snapshot shares
/// them rather than copying. Divides `BLOCK_BUCKETS` and is a multiple of 8.
pub const CHUNK: u64 = 1024;

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

/// What one channel's buckets hold: a digital channel's bits, or an analog one's numbers in the
/// type it is summarised in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    Digital,
    Analog(Stored),
}

/// A number an analog bucket holds, and the sum it adds up to.
pub trait Num: Copy + fmt::Debug + Send + Sync + 'static {
    type Sum: Copy + fmt::Debug + Send + Sync + 'static;
    /// An empty bucket's minimum: the type's largest.
    const GREATEST: Self;
    /// An empty bucket's maximum: the type's smallest.
    const LEAST: Self;
    const NO_SUM: Self::Sum;
    const SIZE: usize;
    const SUM_SIZE: usize;
    fn lesser(self, other: Self) -> Self;
    fn greater(self, other: Self) -> Self;
    fn add(sum: Self::Sum, v: Self) -> Self::Sum;
    fn add_sum(a: Self::Sum, b: Self::Sum) -> Self::Sum;
    fn is_nan(self) -> bool;
    /// The same bits: a float compares by its bits, so a NaN sum equals itself.
    fn same(a: Self, b: Self) -> bool;
    fn same_sum(a: Self::Sum, b: Self::Sum) -> bool;
    fn put(self, out: &mut Vec<u8>);
    fn put_sum(sum: Self::Sum, out: &mut Vec<u8>);
    fn read(bytes: &[u8]) -> Self;
    fn read_sum(bytes: &[u8]) -> Self::Sum;
}

macro_rules! float {
    ($t:ty, $size:literal) => {
        impl Num for $t {
            type Sum = f64;
            const GREATEST: Self = <$t>::INFINITY;
            const LEAST: Self = <$t>::NEG_INFINITY;
            const NO_SUM: f64 = 0.0;
            const SIZE: usize = $size;
            const SUM_SIZE: usize = 8;
            // `min` and `max` leave a tie between -0 and +0 to the platform; the format gives it
            // to the earlier, `self`. Neither side is ever NaN: `take` leaves NaN out.
            fn lesser(self, other: Self) -> Self {
                if other < self { other } else { self }
            }
            fn greater(self, other: Self) -> Self {
                if other > self { other } else { self }
            }
            fn add(sum: f64, v: Self) -> f64 {
                sum + f64::from(v)
            }
            fn add_sum(a: f64, b: f64) -> f64 {
                a + b
            }
            fn is_nan(self) -> bool {
                self.is_nan()
            }
            fn same(a: Self, b: Self) -> bool {
                a.to_bits() == b.to_bits()
            }
            fn same_sum(a: f64, b: f64) -> bool {
                a.to_bits() == b.to_bits()
            }
            fn put(self, out: &mut Vec<u8>) {
                out.extend_from_slice(&self.to_le_bytes());
            }
            fn put_sum(sum: f64, out: &mut Vec<u8>) {
                out.extend_from_slice(&sum.to_le_bytes());
            }
            fn read(bytes: &[u8]) -> Self {
                <$t>::from_le_bytes(bytes.try_into().unwrap())
            }
            fn read_sum(bytes: &[u8]) -> f64 {
                f64::from_le_bytes(bytes.try_into().unwrap())
            }
        }
    };
}

macro_rules! integer {
    ($t:ty) => {
        impl Num for $t {
            type Sum = i128;
            const GREATEST: Self = <$t>::MAX;
            const LEAST: Self = <$t>::MIN;
            const NO_SUM: i128 = 0;
            const SIZE: usize = 8;
            const SUM_SIZE: usize = 16;
            fn lesser(self, other: Self) -> Self {
                Ord::min(self, other)
            }
            fn greater(self, other: Self) -> Self {
                Ord::max(self, other)
            }
            fn add(sum: i128, v: Self) -> i128 {
                sum + i128::from(v)
            }
            fn add_sum(a: i128, b: i128) -> i128 {
                a + b
            }
            fn is_nan(self) -> bool {
                false
            }
            fn same(a: Self, b: Self) -> bool {
                a == b
            }
            fn same_sum(a: i128, b: i128) -> bool {
                a == b
            }
            fn put(self, out: &mut Vec<u8>) {
                out.extend_from_slice(&self.to_le_bytes());
            }
            fn put_sum(sum: i128, out: &mut Vec<u8>) {
                out.extend_from_slice(&sum.to_le_bytes());
            }
            fn read(bytes: &[u8]) -> Self {
                <$t>::from_le_bytes(bytes.try_into().unwrap())
            }
            fn read_sum(bytes: &[u8]) -> i128 {
                i128::from_le_bytes(bytes.try_into().unwrap())
            }
        }
    };
}

float!(f32, 4);
float!(f64, 8);
integer!(u64);
integer!(i64);

/// One bucket of an analog channel.
#[derive(Clone, Copy, Debug)]
pub struct Bucket<T: Num> {
    pub min: T,
    pub max: T,
    pub sum: T::Sum,
}

impl<T: Num> Bucket<T> {
    pub const EMPTY: Self = Self {
        min: T::GREATEST,
        max: T::LEAST,
        sum: T::NO_SUM,
    };

    /// A sample counts in the bucket; a NaN leaves it as it was.
    fn take(&mut self, v: T) {
        if v.is_nan() {
            return;
        }
        self.min = self.min.lesser(v);
        self.max = self.max.greater(v);
        self.sum = T::add(self.sum, v);
    }

    /// A later child's bucket folded in.
    fn fold(&mut self, next: &Self) {
        self.min = self.min.lesser(next.min);
        self.max = self.max.greater(next.max);
        self.sum = T::add_sum(self.sum, next.sum);
    }
}

impl<T: Num> PartialEq for Bucket<T> {
    fn eq(&self, other: &Self) -> bool {
        T::same(self.min, other.min)
            && T::same(self.max, other.max)
            && T::same_sum(self.sum, other.sum)
    }
}

/// An analog channel's buckets: n minimums, n maximums and n sums.
#[derive(Clone, Debug)]
pub struct Columns<T: Num> {
    pub min: Vec<T>,
    pub max: Vec<T>,
    pub sum: Vec<T::Sum>,
}

impl<T: Num> Columns<T> {
    fn new() -> Self {
        Self {
            min: Vec::new(),
            max: Vec::new(),
            sum: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.min.len()
    }

    pub fn is_empty(&self) -> bool {
        self.min.is_empty()
    }

    pub fn bucket(&self, i: usize) -> Bucket<T> {
        Bucket {
            min: self.min[i],
            max: self.max[i],
            sum: self.sum[i],
        }
    }

    fn push(&mut self, b: Bucket<T>) {
        self.min.push(b.min);
        self.max.push(b.max);
        self.sum.push(b.sum);
    }

    fn slice(&self, from: usize, n: usize) -> Self {
        Self {
            min: self.min[from..][..n].to_vec(),
            max: self.max[from..][..n].to_vec(),
            sum: self.sum[from..][..n].to_vec(),
        }
    }

    fn append(&mut self, other: &Self) {
        self.min.extend_from_slice(&other.min);
        self.max.extend_from_slice(&other.max);
        self.sum.extend_from_slice(&other.sum);
    }

    fn write(&self, out: &mut Vec<u8>) {
        for &v in self.min.iter().chain(&self.max) {
            v.put(out);
        }
        for &s in &self.sum {
            T::put_sum(s, out);
        }
    }

    fn parse(c: &mut Cursor<'_>, n: usize) -> Option<Self> {
        let nums = |b: &[u8]| b.chunks_exact(T::SIZE).map(T::read).collect();
        Some(Self {
            min: nums(c.take(n * T::SIZE)?),
            max: nums(c.take(n * T::SIZE)?),
            sum: c
                .take(n * T::SUM_SIZE)?
                .chunks_exact(T::SUM_SIZE)
                .map(T::read_sum)
                .collect(),
        })
    }

    /// Buckets of a coarser level, each folding the children `children(b)` in order.
    fn coarsen(&self, n: u64, children: impl Fn(u64) -> std::ops::Range<u64>) -> Self {
        let mut out = Self::new();
        for b in 0..n {
            let mut acc = Bucket::EMPTY;
            for i in children(b) {
                acc.fold(&self.bucket(i as usize));
            }
            out.push(acc);
        }
        out
    }
}

impl<T: Num> PartialEq for Columns<T> {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && (0..self.len()).all(|i| self.bucket(i) == other.bucket(i))
    }
}

/// An analog or derived channel's buckets in the type it is summarised in.
#[derive(Clone, Debug, PartialEq)]
pub enum AnalogOverview {
    F32(Columns<f32>),
    F64(Columns<f64>),
    U64(Columns<u64>),
    I64(Columns<i64>),
}

macro_rules! each {
    ($e:expr, $c:ident => $body:expr) => {
        match $e {
            AnalogOverview::F32($c) => $body,
            AnalogOverview::F64($c) => $body,
            AnalogOverview::U64($c) => $body,
            AnalogOverview::I64($c) => $body,
        }
    };
}

macro_rules! map_each {
    ($e:expr, $c:ident => $body:expr) => {
        match $e {
            AnalogOverview::F32($c) => AnalogOverview::F32($body),
            AnalogOverview::F64($c) => AnalogOverview::F64($body),
            AnalogOverview::U64($c) => AnalogOverview::U64($body),
            AnalogOverview::I64($c) => AnalogOverview::I64($body),
        }
    };
}

impl AnalogOverview {
    fn new(stored: Stored) -> Self {
        match stored {
            Stored::F32 => Self::F32(Columns::new()),
            Stored::F64 => Self::F64(Columns::new()),
            Stored::U64 => Self::U64(Columns::new()),
            Stored::I64 => Self::I64(Columns::new()),
        }
    }

    pub fn len(&self) -> usize {
        each!(self, c => c.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Each bucket's minimum and maximum in f64, and its sum in f64: the stored numbers, not
    /// an `int` channel's values.
    pub fn as_f64(&self) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
        match self {
            Self::F32(c) => (
                c.min.iter().map(|&v| f64::from(v)).collect(),
                c.max.iter().map(|&v| f64::from(v)).collect(),
                c.sum.clone(),
            ),
            Self::F64(c) => (c.min.clone(), c.max.clone(), c.sum.clone()),
            Self::U64(c) => (
                c.min.iter().map(|&v| v as f64).collect(),
                c.max.iter().map(|&v| v as f64).collect(),
                c.sum.iter().map(|&v| v as f64).collect(),
            ),
            Self::I64(c) => (
                c.min.iter().map(|&v| v as f64).collect(),
                c.max.iter().map(|&v| v as f64).collect(),
                c.sum.iter().map(|&v| v as f64).collect(),
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ChannelOverview {
    Digital { level: Vec<u8>, change: Vec<u8> },
    Analog(AnalogOverview),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Level {
    pub shift: u8,
    pub buckets: u64,
    /// Each channel, then each derived channel.
    pub channels: Vec<ChannelOverview>,
}

impl Level {
    /// A level of no buckets yet, for `columns`.
    pub fn empty(shift: u8, columns: &[Column]) -> Level {
        Level {
            shift,
            buckets: 0,
            channels: columns
                .iter()
                .map(|c| match c {
                    Column::Digital => ChannelOverview::Digital {
                        level: Vec::new(),
                        change: Vec::new(),
                    },
                    Column::Analog(stored) => ChannelOverview::Analog(AnalogOverview::new(*stored)),
                })
                .collect(),
        }
    }

    /// Buckets `first..first + n` as a level of their own. `first` is a multiple of 8, and the
    /// slice either ends on a multiple of 8 or at the level's end, so the bit arrays cut on byte
    /// boundaries.
    pub fn slice(&self, first: u64, n: u64) -> Level {
        assert!(first.is_multiple_of(8) && (n.is_multiple_of(8) || first + n == self.buckets));
        let bits = |b: &Vec<u8>| b[(first / 8) as usize..][..bit_bytes(n)].to_vec();
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
                    ChannelOverview::Analog(a) => ChannelOverview::Analog(
                        map_each!(a, c => c.slice(first as usize, n as usize)),
                    ),
                })
                .collect(),
        }
    }

    /// `other`'s buckets after this level's, whose count is a multiple of 8.
    fn append(&mut self, other: &Level) {
        debug_assert!(self.buckets.is_multiple_of(8));
        for (mine, theirs) in self.channels.iter_mut().zip(&other.channels) {
            match (mine, theirs) {
                (
                    ChannelOverview::Digital { level, change },
                    ChannelOverview::Digital {
                        level: l,
                        change: c,
                    },
                ) => {
                    level.extend_from_slice(l);
                    change.extend_from_slice(c);
                }
                (ChannelOverview::Analog(a), ChannelOverview::Analog(b)) => match (a, b) {
                    (AnalogOverview::F32(a), AnalogOverview::F32(b)) => a.append(b),
                    (AnalogOverview::F64(a), AnalogOverview::F64(b)) => a.append(b),
                    (AnalogOverview::U64(a), AnalogOverview::U64(b)) => a.append(b),
                    (AnalogOverview::I64(a), AnalogOverview::I64(b)) => a.append(b),
                    _ => unreachable!("a channel keeps its type"),
                },
                _ => unreachable!("a channel keeps its kind"),
            }
        }
        self.buckets += other.buckets;
    }

    /// Bucket `self.buckets` takes `values`, one a column.
    fn push(&mut self, values: &[Value]) {
        let i = self.buckets;
        for (column, value) in self.channels.iter_mut().zip(values) {
            match (column, value) {
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
                    if *l {
                        set(level, i);
                    }
                    if *c {
                        set(change, i);
                    }
                }
                (ChannelOverview::Analog(a), Value::Analog(v)) => match (a, v) {
                    (AnalogOverview::F32(c), AnalogValue::F32(b)) => c.push(*b),
                    (AnalogOverview::F64(c), AnalogValue::F64(b)) => c.push(*b),
                    (AnalogOverview::U64(c), AnalogValue::U64(b)) => c.push(*b),
                    (AnalogOverview::I64(c), AnalogValue::I64(b)) => c.push(*b),
                    _ => unreachable!("a channel keeps its type"),
                },
                _ => unreachable!("a channel keeps its kind"),
            }
        }
        self.buckets += 1;
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
            Self::Analog(_) => None,
        }
    }

    pub fn analog(&self) -> Option<&AnalogOverview> {
        match self {
            Self::Analog(a) => Some(a),
            Self::Digital { .. } => None,
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
            ChannelOverview::Analog(a) => {
                ChannelOverview::Analog(map_each!(a, c => c.coarsen(n, children)))
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
        write_level(level, out);
    }
}

/// One level as the decoded overview lays it out: an overview block's whole payload.
pub fn write_level(level: &Level, out: &mut Vec<u8>) {
    for channel in &level.channels {
        match channel {
            ChannelOverview::Digital { level, change } => {
                out.extend_from_slice(level);
                out.extend_from_slice(change);
            }
            ChannelOverview::Analog(a) => each!(a, c => c.write(out)),
        }
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let slice = self.bytes.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(slice)
    }
}

fn parse_channels(c: &mut Cursor<'_>, n: u64, columns: &[Column]) -> Option<Vec<ChannelOverview>> {
    let n = usize::try_from(n).ok()?;
    let mut channels = Vec::with_capacity(columns.len());
    for column in columns {
        channels.push(match column {
            Column::Analog(stored) => ChannelOverview::Analog(match stored {
                Stored::F32 => AnalogOverview::F32(Columns::parse(c, n)?),
                Stored::F64 => AnalogOverview::F64(Columns::parse(c, n)?),
                Stored::U64 => AnalogOverview::U64(Columns::parse(c, n)?),
                Stored::I64 => AnalogOverview::I64(Columns::parse(c, n)?),
            }),
            Column::Digital => {
                let level = c.take(bit_bytes(n as u64))?.to_vec();
                let change = c.take(bit_bytes(n as u64))?.to_vec();
                ChannelOverview::Digital { level, change }
            }
        });
    }
    Some(channels)
}

/// The decoded size of `n` buckets of every column.
pub fn decoded_len(n: u64, columns: &[Column]) -> u64 {
    columns
        .iter()
        .map(|c| match c {
            Column::Digital => 2 * bit_bytes(n) as u64,
            Column::Analog(Stored::F32) => n * (2 * 4 + 8),
            Column::Analog(Stored::F64) => n * (2 * 8 + 8),
            Column::Analog(Stored::U64 | Stored::I64) => n * (2 * 8 + 16),
        })
        .sum()
}

/// Parses the footer's decoded overview. None if `bytes` is not exactly the size the levels need.
pub fn parse(bytes: &[u8], shifts: &[u8], length: i64, columns: &[Column]) -> Option<Vec<Level>> {
    let mut cursor = Cursor { bytes, pos: 0 };
    let mut levels = Vec::with_capacity(shifts.len());
    for &shift in shifts {
        let n = buckets(length, shift);
        levels.push(Level {
            shift,
            buckets: n,
            channels: parse_channels(&mut cursor, n, columns)?,
        });
    }
    (cursor.pos == bytes.len()).then_some(levels)
}

/// Parses one decoded overview block of `n` buckets.
pub fn parse_block(bytes: &[u8], shift: u8, n: u64, columns: &[Column]) -> Option<Level> {
    let mut cursor = Cursor { bytes, pos: 0 };
    let channels = parse_channels(&mut cursor, n, columns)?;
    (cursor.pos == bytes.len()).then_some(Level {
        shift,
        buckets: n,
        channels,
    })
}

/// One bucket of an analog column.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AnalogValue {
    F32(Bucket<f32>),
    F64(Bucket<f64>),
    U64(Bucket<u64>),
    I64(Bucket<i64>),
}

impl AnalogValue {
    fn empty(stored: Stored) -> Self {
        match stored {
            Stored::F32 => Self::F32(Bucket::EMPTY),
            Stored::F64 => Self::F64(Bucket::EMPTY),
            Stored::U64 => Self::U64(Bucket::EMPTY),
            Stored::I64 => Self::I64(Bucket::EMPTY),
        }
    }

    fn take(&mut self, sample: Sample) {
        match (self, sample) {
            (Self::F32(b), Sample::F32(v)) => b.take(v),
            (Self::F64(b), Sample::F64(v)) => b.take(v),
            (Self::U64(b), Sample::U64(v)) => b.take(v),
            (Self::I64(b), Sample::I64(v)) => b.take(v),
            (b, s) => unreachable!("a {} sample in a {b:?} bucket", s.type_name()),
        }
    }

    fn fold(&mut self, next: &Self) {
        match (self, next) {
            (Self::F32(a), Self::F32(b)) => a.fold(b),
            (Self::F64(a), Self::F64(b)) => a.fold(b),
            (Self::U64(a), Self::U64(b)) => a.fold(b),
            (Self::I64(a), Self::I64(b)) => a.fold(b),
            _ => unreachable!("a channel keeps its type"),
        }
    }
}

/// One bucket of one column.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Value {
    Digital { level: bool, change: bool },
    Analog(AnalogValue),
}

impl Value {
    /// A coarser bucket's value, taking `next`, a later child, into account.
    fn fold(&mut self, next: &Value) {
        match (self, next) {
            (Value::Digital { change, .. }, Value::Digital { change: c, .. }) => *change |= *c,
            (Value::Analog(a), Value::Analog(b)) => a.fold(b),
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
        /// Gap ends not yet reached, in order: each gap, and its `to` on the axis.
        pending: VecDeque<(Gap, i64)>,
        events: VecDeque<DigitalEvent>,
    },
    Analog {
        stored: Stored,
        events: VecDeque<(u64, AnalogValue)>,
    },
}

impl Intake {
    /// The bucket of the next event not yet settled.
    fn next_bucket(&self) -> Option<u64> {
        match self {
            Intake::Digital { events, .. } => events.front().map(|e| e.bucket),
            Intake::Analog { events, .. } => events.front().map(|&(b, _)| b),
        }
    }

    /// A bucket with no event: a digital channel's level carried in, an analog one empty.
    fn quiet(&self) -> Value {
        match self {
            Intake::Digital { entering, .. } => Value::Digital {
                level: *entering,
                change: false,
            },
            Intake::Analog { stored, .. } => Value::Analog(AnalogValue::empty(*stored)),
        }
    }

    fn sample(&mut self, bucket: u64, sample: Sample) {
        let Intake::Analog { stored, events } = self else {
            unreachable!("a sample on an analog channel")
        };
        match events.back_mut() {
            Some((b, v)) if *b == bucket => v.take(sample),
            _ => {
                let mut v = AnalogValue::empty(*stored);
                v.take(sample);
                events.push_back((bucket, v));
            }
        }
    }
}

/// A derived channel's operands' samples waiting for their partner: sample i of one is paired
/// with sample i of the other.
struct Pair {
    name: String,
    operands: [(usize, String); 2],
    product: Product,
    /// Samples of one operand the other has not reached, with their ticks; never both at once.
    waiting: [VecDeque<(i64, Sample)>; 2],
}

/// Why a derived channel's operands' samples do not pair.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("derived channel {0}: {1}")]
pub struct Unpaired(pub String, pub String);

/// A level's settled buckets not yet handed on, from bucket `first`: whole chunks of `CHUNK`
/// buckets, shared, then the rest.
struct Store {
    shift: u8,
    columns: Vec<Column>,
    first: u64,
    sealed: Vec<Arc<Level>>,
    open: Level,
    /// The last chunk of buckets all alike, and their values: a span with no item fills its
    /// chunks with this one.
    uniform: Option<(Vec<Value>, Arc<Level>)>,
    /// The last overview block made of that chunk alone, which the next such block shares.
    uniform_block: Option<(Arc<Level>, Arc<Level>)>,
}

impl Store {
    fn new(shift: u8, columns: &[Column], first: u64) -> Self {
        Self {
            shift,
            columns: columns.to_vec(),
            first,
            sealed: Vec::new(),
            open: Level::empty(shift, columns),
            uniform: None,
            uniform_block: None,
        }
    }

    /// `n` buckets that each take `values`. Whole chunks share one level of them.
    fn push_n(&mut self, values: &[Value], mut n: u64) {
        while n > 0 && self.open.buckets > 0 {
            self.push(values);
            n -= 1;
        }
        if n >= CHUNK {
            let chunk = match &self.uniform {
                Some((v, chunk)) if v.as_slice() == values => Arc::clone(chunk),
                _ => {
                    let mut level = Level::empty(self.shift, &self.columns);
                    for _ in 0..CHUNK {
                        level.push(values);
                    }
                    let chunk = Arc::new(level);
                    self.uniform = Some((values.to_vec(), Arc::clone(&chunk)));
                    chunk
                }
            };
            while n >= CHUNK {
                self.sealed.push(Arc::clone(&chunk));
                n -= CHUNK;
            }
        }
        for _ in 0..n {
            self.push(values);
        }
    }

    /// The first overview block's worth of buckets, `BLOCK_BUCKETS`, shared with the last block
    /// like it where every chunk of both is the uniform one.
    fn split_block(&mut self) -> Arc<Level> {
        let per_block = (BLOCK_BUCKETS / CHUNK) as usize;
        let uniform = self.uniform.as_ref().map(|(_, chunk)| Arc::clone(chunk));
        let alike = uniform.as_ref().filter(|u| {
            self.sealed.len() >= per_block
                && self.sealed[..per_block].iter().all(|c| Arc::ptr_eq(c, u))
        });
        if let (Some(u), Some((chunk, block))) = (alike, &self.uniform_block)
            && Arc::ptr_eq(chunk, u)
        {
            let block = Arc::clone(block);
            self.sealed.drain(..per_block);
            self.first += BLOCK_BUCKETS;
            return block;
        }
        let alike = alike.cloned();
        let block = Arc::new(self.split_front(BLOCK_BUCKETS));
        if let Some(u) = alike {
            self.uniform_block = Some((u, Arc::clone(&block)));
        }
        block
    }

    fn n(&self) -> u64 {
        self.sealed.len() as u64 * CHUNK + self.open.buckets
    }

    fn push(&mut self, values: &[Value]) {
        self.open.push(values);
        if self.open.buckets == CHUNK {
            let full = std::mem::replace(&mut self.open, Level::empty(self.shift, &self.columns));
            self.sealed.push(Arc::new(full));
        }
    }

    /// The first `n` buckets as a level of their own: a whole number of chunks, or all of them.
    fn split_front(&mut self, n: u64) -> Level {
        let mut out = Level::empty(self.shift, &self.columns);
        if n == self.n() {
            for chunk in self.sealed.drain(..) {
                out.append(&chunk);
            }
            let open = std::mem::replace(&mut self.open, Level::empty(self.shift, &self.columns));
            out.append(&open);
        } else {
            assert!(n.is_multiple_of(CHUNK), "a level is cut on chunks");
            for chunk in self.sealed.drain(..(n / CHUNK) as usize) {
                out.append(&chunk);
            }
        }
        self.first += n;
        out
    }

    /// What the store holds, shared.
    fn held(&self) -> HeldLevel {
        let mut chunks = self.sealed.clone();
        if self.open.buckets > 0 {
            chunks.push(Arc::new(self.open.clone()));
        }
        HeldLevel {
            shift: self.shift,
            first: self.first,
            chunks,
        }
    }
}

/// A level's buckets a writer holds, from bucket `first`, in chunks of up to `CHUNK` buckets
/// each, every one but the last full.
#[derive(Clone, Debug)]
pub struct HeldLevel {
    pub shift: u8,
    pub first: u64,
    pub chunks: Vec<Arc<Level>>,
}

impl HeldLevel {
    pub fn buckets(&self) -> u64 {
        self.chunks.iter().map(|c| c.buckets).sum()
    }
}

struct LevelBuild {
    /// Set once the level has more than 2^17 buckets: it goes in overview blocks.
    promoted: bool,
    store: Store,
    /// For a level coarser than the finest: the bucket its children are being folded into.
    open: Option<(u64, Vec<Value>)>,
}

/// An overview block whose buckets have settled: block `k` of the level with `shift`, decoded.
/// Blocks of a span with no item share one level.
#[derive(Debug, PartialEq)]
pub struct Settled {
    pub shift: u8,
    pub k: u64,
    pub level: Arc<Level>,
}

/// What the overview comes to at the end.
pub struct Finished {
    /// The levels the footer holds, finest first.
    pub head: Vec<Level>,
    pub plan: Plan,
}

/// Each channel's column, then each derived channel's, as a declaration has them.
pub fn columns(channels: &[DeclaredChannel], derived: &[Derived]) -> Vec<Column> {
    let mut out: Vec<Column> = channels
        .iter()
        .map(|c| match &c.data {
            DeclaredKind::Digital { .. } => Column::Digital,
            DeclaredKind::Analog { coding, .. } => Column::Analog(coding.stored()),
        })
        .collect();
    out.extend(
        products(channels, derived)
            .iter()
            .map(|(_, _, p)| Column::Analog(p.stored())),
    );
    out
}

/// Each derived channel's operands, by channel index, and its rule, from a declaration the
/// reader or the writer has checked.
pub fn products(channels: &[DeclaredChannel], derived: &[Derived]) -> Vec<(usize, usize, Product)> {
    let find = |name: &str| {
        channels
            .iter()
            .position(|c| c.name == name)
            .expect("a checked declaration's derived channels name its channels")
    };
    derived
        .iter()
        .map(|d| {
            let (a, b) = (find(&d.product[0]), find(&d.product[1]));
            let coding = |c: usize| channels[c].coding().expect("analog operands");
            (a, b, Product::of(coding(a), coding(b)))
        })
        .collect()
}

/// Builds the overview as a recording's items arrive. Each channel's items and gap ends arrive in
/// tick order, and `settle` says when every item before a tick is in, so the buckets before it
/// cannot change. It holds the levels that may still go in the footer, and hands on overview
/// blocks as they settle. A derived channel's samples come from its operands'.
pub struct Builder {
    finest: u8,
    intake: Vec<Intake>,
    pairs: Vec<Pair>,
    /// Finest buckets before this have settled.
    base: u64,
    levels: Vec<LevelBuild>,
    settled: Vec<Settled>,
}

impl Builder {
    /// The overview of `channels` and `derived`, as a checked declaration has them, from level
    /// `finest`.
    pub fn new(finest: u8, channels: &[DeclaredChannel], derived: &[Derived]) -> Self {
        let columns = columns(channels, derived);
        let intake = channels
            .iter()
            .map(|c| match &c.data {
                DeclaredKind::Analog { coding, .. } => Intake::Analog {
                    stored: coding.stored(),
                    events: VecDeque::new(),
                },
                DeclaredKind::Digital { initial } => Intake::Digital {
                    level: *initial,
                    entering: *initial,
                    pending: VecDeque::new(),
                    events: VecDeque::new(),
                },
            })
            .chain(columns[channels.len()..].iter().map(|c| match c {
                Column::Analog(stored) => Intake::Analog {
                    stored: *stored,
                    events: VecDeque::new(),
                },
                Column::Digital => unreachable!("a derived channel is analog"),
            }))
            .collect();
        let pairs = products(channels, derived)
            .into_iter()
            .zip(derived)
            .map(|((a, b, product), d)| Pair {
                name: d.name.clone(),
                operands: [(a, d.product[0].clone()), (b, d.product[1].clone())],
                product,
                waiting: [VecDeque::new(), VecDeque::new()],
            })
            .collect();
        let levels = (finest..=MAX_SHIFT)
            .step_by(STEP as usize)
            .map(|shift| LevelBuild {
                promoted: false,
                store: Store::new(shift, &columns, 0),
                open: None,
            })
            .collect();
        Self {
            finest,
            intake,
            pairs,
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

    /// Takes the gap ends of digital channel `c` that come before: those whose `to` lies before
    /// source tick `source`, or, with none, before axis tick `axis`.
    fn reach(&mut self, c: usize, source: Option<i64>, axis: i64) {
        loop {
            let Intake::Digital { pending, .. } = &mut self.intake[c] else {
                return;
            };
            match pending.front() {
                Some(&(g, t)) if source.map_or(t < axis, |k| g.to < k) => {
                    pending.pop_front();
                    self.digital_event(c, t, |_| g.level);
                }
                _ => return,
            }
        }
    }

    /// An edge of digital channel `c` at source tick `source`, axis tick `tick`, after its
    /// earlier edges and gap ends: a gap end at the same axis tick that comes before it in source
    /// ticks takes effect first.
    pub fn edge(&mut self, c: usize, source: i64, tick: i64) {
        self.reach(c, Some(source), tick);
        self.digital_event(c, tick, |level| !level);
    }

    /// A gap of digital channel `c`, in its source's ticks, whose `to` lies at axis tick `to`:
    /// its end takes effect once the items pass it.
    pub fn gap(&mut self, c: usize, gap: Gap, to: i64) {
        if let Intake::Digital { pending, .. } = &mut self.intake[c] {
            pending.push_back((gap, to));
        }
    }

    /// A sample of analog channel `c`, in the type its coding stores, and the derived samples it
    /// completes. A NaN leaves its bucket as it was. Err when the derived channel's other operand
    /// has its sample of the same index at another tick.
    pub fn sample(&mut self, c: usize, tick: i64, sample: Sample) -> Result<(), Unpaired> {
        let bucket = tick as u64 >> self.finest;
        debug_assert!(bucket >= self.base, "a sample in a settled bucket");
        self.intake[c].sample(bucket, sample);
        let first_derived = self.intake.len() - self.pairs.len();
        for (d, pair) in self.pairs.iter_mut().enumerate() {
            let product = match [pair.operands[0].0 == c, pair.operands[1].0 == c] {
                [true, true] => pair.product.sample(sample, sample),
                [false, false] => continue,
                [mine_a, _] => {
                    let (mine, other) = if mine_a { (0, 1) } else { (1, 0) };
                    let Some((t, partner)) = pair.waiting[other].pop_front() else {
                        pair.waiting[mine].push_back((tick, sample));
                        continue;
                    };
                    if t != tick {
                        return Err(Unpaired(
                            pair.name.clone(),
                            format!(
                                "{} has a sample at tick {t} where {} has one at {tick}",
                                pair.operands[other].1, pair.operands[mine].1
                            ),
                        ));
                    }
                    if mine_a {
                        pair.product.sample(sample, partner)
                    } else {
                        pair.product.sample(partner, sample)
                    }
                }
            };
            self.intake[first_derived + d].sample(bucket, product);
        }
        Ok(())
    }

    /// Whether a sample of channel `c` at `tick` pairs with what its derived channels' other
    /// operands have waiting: `sample` would refuse it otherwise.
    pub fn pairs_with(&self, c: usize, tick: i64) -> Result<(), Unpaired> {
        for pair in &self.pairs {
            for mine in 0..2 {
                let other = 1 - mine;
                if pair.operands[mine].0 != c || pair.operands[other].0 == c {
                    continue;
                }
                if let Some(&(t, _)) = pair.waiting[other].front()
                    && t != tick
                {
                    return Err(Unpaired(
                        pair.name.clone(),
                        format!(
                            "{} has a sample at tick {t} where {} has one at {tick}",
                            pair.operands[other].1, pair.operands[mine].1
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    /// A derived channel's operand sample still waiting for the other operand's.
    pub fn unpaired(&self) -> Option<Unpaired> {
        self.pairs.iter().find_map(|pair| {
            pair.waiting.iter().enumerate().find_map(|(side, waiting)| {
                waiting.front().map(|&(t, _)| {
                    Unpaired(
                        pair.name.clone(),
                        format!(
                            "{} has a sample at tick {t} that {} lacks",
                            pair.operands[side].1,
                            pair.operands[1 - side].1
                        ),
                    )
                })
            })
        })
    }

    /// Every item and gap end before `tick` is in. Settles the finest buckets wholly before it,
    /// and the coarser buckets they complete. Err when a derived channel's operand has a sample
    /// the other lacks.
    pub fn settle(&mut self, tick: i64) -> Result<(), Unpaired> {
        if let Some(e) = self.unpaired() {
            return Err(e);
        }
        for c in 0..self.intake.len() {
            self.reach(c, None, tick);
        }
        self.settle_to(tick as u64 >> self.finest);
        for li in 0..self.levels.len() {
            let shift = self.levels[li].store.shift;
            if !self.levels[li].promoted && 17 + u32::from(shift) < 63 && tick > 1 << (17 + shift) {
                self.levels[li].promoted = true;
            }
            self.cut_blocks(li);
        }
        Ok(())
    }

    /// Settles finest buckets `base..upto`. A run of buckets with no event goes in at once.
    fn settle_to(&mut self, upto: u64) {
        let mut values = Vec::with_capacity(self.intake.len());
        while self.base < upto {
            let b = self.base;
            let next = self
                .intake
                .iter()
                .filter_map(Intake::next_bucket)
                .min()
                .unwrap_or(u64::MAX)
                .min(upto);
            if next > b {
                values.clear();
                values.extend(self.intake.iter().map(Intake::quiet));
                self.push_run(0, b, next - b, &values);
                self.base = next;
                continue;
            }
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
                    Intake::Analog { stored, events } => match events.front() {
                        Some(&(eb, v)) if eb == b => {
                            events.pop_front();
                            Value::Analog(v)
                        }
                        _ => Value::Analog(AnalogValue::empty(*stored)),
                    },
                });
            }
            self.push(0, b, &values);
            self.base += 1;
        }
    }

    /// Buckets `b..b + n` of level `li` have settled, each holding `values`, a bucket with no
    /// event: store them, and fold them into the next level, whose whole buckets among them are
    /// alike too.
    fn push_run(&mut self, li: usize, b: u64, n: u64, values: &[Value]) {
        if n == 1 {
            return self.push(li, b, values);
        }
        self.levels[li].store.push_n(values, n);
        if self.levels[li].promoted && self.levels[li].store.n() >= BLOCK_BUCKETS {
            self.cut_blocks(li);
        }
        let Some(next) = self.levels.get_mut(li + 1) else {
            return;
        };
        let (first, last) = (b >> STEP, (b + n - 1) >> STEP);
        // The parent the run starts in: its earlier children are in `open`, and a run's values
        // fold into anything as nothing.
        let done = match &mut next.open {
            Some((p, acc)) if *p == first => {
                for (a, v) in acc.iter_mut().zip(values) {
                    a.fold(v);
                }
                None
            }
            open => open.replace((first, values.to_vec())),
        };
        if let Some((p, acc)) = done {
            self.push(li + 1, p, &acc);
        }
        if last > first {
            let Some((p, acc)) = self.levels[li + 1].open.take() else {
                unreachable!("the run's first parent is open")
            };
            self.push(li + 1, p, &acc);
            if last > first + 1 {
                self.push_run(li + 1, first + 1, last - first - 1, values);
            }
            self.levels[li + 1].open = Some((last, values.to_vec()));
        }
    }

    /// Bucket `b` of level `li` has settled: store it, and fold it into the next level.
    fn push(&mut self, li: usize, b: u64, values: &[Value]) {
        self.levels[li].store.push(values);
        if self.levels[li].promoted && self.levels[li].store.n() >= BLOCK_BUCKETS {
            self.cut_blocks(li);
        }
        let Some(next) = self.levels.get_mut(li + 1) else {
            return;
        };
        let parent = b >> STEP;
        let done = match &mut next.open {
            Some((p, acc)) if *p == parent => {
                for (a, v) in acc.iter_mut().zip(values) {
                    a.fold(v);
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
        while level.store.n() >= BLOCK_BUCKETS {
            let k = level.store.first / BLOCK_BUCKETS;
            let block = level.store.split_block();
            self.settled.push(Settled {
                shift: level.store.shift,
                k,
                level: block,
            });
        }
    }

    /// The overview blocks settled since the last call, in the order they settled.
    pub fn take_settled(&mut self) -> Vec<Settled> {
        std::mem::take(&mut self.settled)
    }

    /// The settled buckets each level holds, finest first, shared: a level that may still go in
    /// the footer from bucket 0, one in overview blocks from its first block not yet handed on.
    pub fn held(&self) -> Vec<HeldLevel> {
        self.levels.iter().map(|l| l.store.held()).collect()
    }

    /// The recording ends at `length`; every item is in. Gap ends at or past it are dropped. The
    /// overview blocks this settles are left for `take_settled`.
    pub fn finish(mut self, length: i64) -> Result<(Finished, Vec<Settled>), Unpaired> {
        self.settle(length)?;
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
            let shift = self.levels[li].store.shift;
            if plan.blocks.contains(&shift) {
                self.levels[li].promoted = true;
                self.cut_blocks(li);
                let level = &mut self.levels[li];
                if level.store.n() > 0 {
                    let k = level.store.first / BLOCK_BUCKETS;
                    let n = level.store.n();
                    let block = Arc::new(level.store.split_front(n));
                    self.settled.push(Settled {
                        shift,
                        k,
                        level: block,
                    });
                }
            } else if plan.head.contains(&shift) {
                let level = &mut self.levels[li];
                debug_assert_eq!(level.store.first, 0, "a footer level was never cut");
                let n = level.store.n();
                head.push(level.store.split_front(n));
            }
        }
        Ok((Finished { head, plan }, self.settled))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::{AnalogCoding, Exact, IntCoding};
    use crate::model::Ratio;

    fn dig(initial: bool) -> DeclaredChannel {
        DeclaredChannel {
            name: "D".into(),
            source_name: "d".into(),
            source: 0,
            data: DeclaredKind::Digital { initial },
            shaping: Default::default(),
        }
    }

    fn coded(name: &str, coding: AnalogCoding) -> DeclaredChannel {
        DeclaredChannel {
            name: name.into(),
            source_name: name.to_lowercase(),
            source: 0,
            data: DeclaredKind::Analog {
                unit: "V".into(),
                period: Ratio::ONE,
                coding,
                significant: None,
            },
            shaping: Default::default(),
        }
    }

    fn ana() -> DeclaredChannel {
        coded("A", AnalogCoding::F32)
    }

    fn digital(level: &Level, c: usize) -> Vec<(bool, bool)> {
        (0..level.buckets)
            .map(|i| level.channels[c].digital(i).unwrap())
            .collect()
    }

    #[test]
    fn digital_buckets_by_hand() {
        // Buckets of 16 ticks over 0..64: edges at 3 (in bucket 0), 40 and 41 (bucket 2).
        let mut b = Builder::new(4, &[dig(true)], &[]);
        for t in [3, 40, 41] {
            b.edge(0, t, t);
        }
        let (done, settled) = b.finish(64).unwrap();
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
        let mut b = Builder::new(4, &[dig(false)], &[]);
        b.edge(0, 5, 5);
        b.settle(20).unwrap();
        b.gap(
            0,
            Gap {
                from: 6,
                to: 40,
                level: false,
            },
            40,
        );
        b.edge(0, 70, 70);
        let (done, _) = b.finish(80).unwrap();
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
        let mut b = Builder::new(4, &[dig(false), ana()], &[]);
        for (i, &t) in edges.iter().enumerate() {
            b.edge(0, t, t);
            b.sample(1, t, Sample::F32(i as f32)).unwrap();
        }
        let (done, _) = b.finish(length).unwrap();
        assert_eq!(done.plan.head, [4, 8]);
        for (fine, coarse) in done.head.iter().zip(&done.head[1..]) {
            assert_eq!(coarsen(fine, coarse.shift, length), *coarse);
        }
    }

    #[test]
    fn a_level_past_2_17_buckets_comes_out_in_blocks_as_it_settles() {
        // Shift 0: one bucket a tick. 2^17 + 3 ticks need 2^17 + 3 buckets, so shift 0 goes in
        // blocks: 32 whole ones of 4,096 once the items pass tick 2^17, then the last three
        // buckets.
        let length = (1 << 17) + 3;
        let mut b = Builder::new(0, &[dig(false)], &[]);
        b.edge(0, 9, 9);
        b.settle(1 << 17).unwrap();
        assert!(
            b.take_settled().is_empty(),
            "2^17 buckets may still fit the footer"
        );
        b.settle((1 << 17) + 1).unwrap();
        let first: Vec<_> = b.take_settled().iter().map(|s| (s.shift, s.k)).collect();
        assert_eq!(first, (0..32).map(|k| (0, k)).collect::<Vec<_>>());
        let (done, last) = b.finish(length).unwrap();
        let last: Vec<_> = last
            .iter()
            .map(|s| (s.shift, s.k, s.level.buckets))
            .collect();
        assert_eq!(last, [(0, 32, 3)]);
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
        // 2^30 ticks at 2^12 a bucket: 2^18 buckets, 64 blocks of 4,096.
        assert_eq!(block_count(1 << 30, 12), 64);
        assert_eq!(block_count((1 << 30) + 1, 12), 65);
        assert_eq!(block_buckets((1 << 30) + 1, 12, 64), (64 * 4096, 1));
    }

    #[test]
    fn round_trips() {
        let mut b = Builder::new(4, &[dig(true), ana()], &[]);
        b.edge(0, 17, 17);
        b.sample(1, 3, Sample::F32(1.5)).unwrap();
        b.sample(1, 900, Sample::F32(-2.0)).unwrap();
        let (done, _) = b.finish(1000).unwrap();
        let mut bytes = Vec::new();
        write(&done.head, &mut bytes);
        let columns = [Column::Digital, Column::Analog(Stored::F32)];
        assert_eq!(
            parse(&bytes, &done.plan.head, 1000, &columns).as_ref(),
            Some(&done.head)
        );
        assert_eq!(parse(&bytes[1..], &done.plan.head, 1000, &columns), None);
    }

    fn columns<T: Num>(level: &Level, c: usize) -> &Columns<T>
    where
        AnalogOverview: AsColumns<T>,
    {
        level.channels[c].analog().unwrap().columns()
    }

    trait AsColumns<T: Num> {
        fn columns(&self) -> &Columns<T>;
    }

    macro_rules! as_columns {
        ($t:ty, $v:ident) => {
            impl AsColumns<$t> for AnalogOverview {
                fn columns(&self) -> &Columns<$t> {
                    match self {
                        AnalogOverview::$v(c) => c,
                        other => panic!("not {}: {other:?}", stringify!($v)),
                    }
                }
            }
        };
    }
    as_columns!(f32, F32);
    as_columns!(f64, F64);
    as_columns!(u64, U64);
    as_columns!(i64, I64);

    /// Each bucket as (min, max, sum).
    fn buckets_of<T: Num>(c: &Columns<T>) -> Vec<(T, T, T::Sum)> {
        (0..c.len())
            .map(|i| (c.min[i], c.max[i], c.sum[i]))
            .collect()
    }

    #[test]
    fn analog_buckets_hold_min_max_and_sum_and_an_empty_one_holds_min_above_max() {
        // Buckets of 16 ticks over 0..64, one channel of each stored type. Samples at 1, 2 and 3
        // (bucket 0), none in bucket 1, 40 and 47 (bucket 2), NaN alone at 50 (bucket 3).
        let int = |signed| AnalogCoding::Int(IntCoding::new(64, signed));
        let channels = [
            coded("F", AnalogCoding::F32),
            coded("D", AnalogCoding::F64),
            coded("U", int(false)),
            coded("S", int(true)),
        ];
        let mut b = Builder::new(4, &channels, &[]);
        let f = [
            (1, 1.5f32),
            (2, -2.0),
            (3, 4.25),
            (40, 0.5),
            (47, 0.25),
            (50, f32::NAN),
        ];
        let u = [(1, u64::MAX), (2, 0), (3, 7), (40, 1), (47, 2)];
        let s = [(1, i64::MIN), (2, i64::MAX), (3, -1), (40, -5), (47, 5)];
        for &(t, v) in &f {
            b.sample(0, t, Sample::F32(v)).unwrap();
            b.sample(1, t, Sample::F64(f64::from(v))).unwrap();
        }
        for &(t, v) in &u {
            b.sample(2, t, Sample::U64(v)).unwrap();
        }
        for &(t, v) in &s {
            b.sample(3, t, Sample::I64(v)).unwrap();
        }
        let (done, _) = b.finish(64).unwrap();
        let level = &done.head[0];
        // By hand: 1.5 - 2 + 4.25 = 3.75; 0.5 + 0.25 = 0.75. An empty bucket and a bucket of NaN
        // alone hold +inf and -inf, and a sum of 0.
        let (inf, ninf) = (f32::INFINITY, f32::NEG_INFINITY);
        assert_eq!(
            buckets_of(columns::<f32>(level, 0)),
            [
                (-2.0, 4.25, 3.75),
                (inf, ninf, 0.0),
                (0.25, 0.5, 0.75),
                (inf, ninf, 0.0)
            ]
        );
        assert_eq!(
            buckets_of(columns::<f64>(level, 1)),
            [
                (-2.0, 4.25, 3.75),
                (f64::INFINITY, f64::NEG_INFINITY, 0.0),
                (0.25, 0.5, 0.75),
                (f64::INFINITY, f64::NEG_INFINITY, 0.0)
            ]
        );
        // u64::MAX + 0 + 7, exactly, past what a u64 holds.
        assert_eq!(
            buckets_of(columns::<u64>(level, 2)),
            [
                (0, u64::MAX, i128::from(u64::MAX) + 7),
                (u64::MAX, 0, 0),
                (1, 2, 3),
                (u64::MAX, 0, 0)
            ]
        );
        // i64::MIN + i64::MAX - 1 = -2.
        assert_eq!(
            buckets_of(columns::<i64>(level, 3)),
            [
                (i64::MIN, i64::MAX, -2),
                (i64::MAX, i64::MIN, 0),
                (-5, 5, 0),
                (i64::MAX, i64::MIN, 0)
            ]
        );
    }

    #[test]
    fn a_float_sum_adds_in_index_order_and_a_coarse_one_its_children_in_order() {
        // 1e16, then 1, then -1e16 in f64: in index order (1e16 + 1) rounds to 1e16, so the sum is
        // 0. Any other order that adds 1 last gives 1.
        let mut b = Builder::new(0, &[coded("D", AnalogCoding::F64)], &[]);
        b.sample(0, 0, Sample::F64(1e16)).unwrap();
        b.sample(0, 0, Sample::F64(1.0)).unwrap();
        b.sample(0, 0, Sample::F64(-1e16)).unwrap();
        let (done, _) = b.finish(1).unwrap();
        assert_eq!(columns::<f64>(&done.head[0], 0).sum, [0.0]);

        // Coarse: buckets of 1 tick, and of 16. Children 0, 1, 2 hold 1e16, 1 and -1e16: the
        // coarse bucket adds them in that order, (1e16 + 1) - 1e16 = 0, not the samples' exact 1.
        let mut b = Builder::new(0, &[coded("D", AnalogCoding::F64)], &[]);
        for (t, v) in [(0, 1e16), (1, 1.0), (2, -1e16)] {
            b.sample(0, t, Sample::F64(v)).unwrap();
        }
        // 20,000 ticks: 1,250 buckets of 16 is past 1,024, so a level of 256 follows.
        let length = 20_000;
        let (done, settled) = b.finish(length).unwrap();
        assert!(settled.is_empty());
        assert_eq!(done.plan.head, [0, 4, 8]);
        assert_eq!(columns::<f64>(&done.head[1], 0).sum[0], 0.0);
        assert_eq!(columns::<f64>(&done.head[2], 0).sum[0], 0.0);
        // The same as folding the finer level, which `coarsen` does on its own.
        assert_eq!(coarsen(&done.head[0], 4, length), done.head[1]);
    }

    #[test]
    fn a_tie_between_minus_and_plus_zero_goes_to_the_first_and_a_sum_starts_at_plus_zero() {
        // Buckets of 1 tick and of 16. f32 +0 then -0 at tick 0; -0 then +0 at tick 1; -0 alone
        // at tick 2; f64 the same.
        let mut b = Builder::new(
            0,
            &[coded("F", AnalogCoding::F32), coded("D", AnalogCoding::F64)],
            &[],
        );
        for (t, v) in [(0, 0.0f32), (0, -0.0), (1, -0.0), (1, 0.0), (2, -0.0)] {
            b.sample(0, t, Sample::F32(v)).unwrap();
            b.sample(1, t, Sample::F64(f64::from(v))).unwrap();
        }
        let (done, _) = b.finish(20_000).unwrap();
        let sign = |v: f64| if v.is_sign_negative() { '-' } else { '+' };
        let f = columns::<f32>(&done.head[0], 0);
        let d = columns::<f64>(&done.head[0], 1);
        for (min, max, sum) in [
            (
                f.min[..3]
                    .iter()
                    .map(|&v| sign(f64::from(v)))
                    .collect::<String>(),
                f.max[..3]
                    .iter()
                    .map(|&v| sign(f64::from(v)))
                    .collect::<String>(),
                f.sum[..3].iter().map(|&v| sign(v)).collect::<String>(),
            ),
            (
                d.min[..3].iter().map(|&v| sign(v)).collect::<String>(),
                d.max[..3].iter().map(|&v| sign(v)).collect::<String>(),
                d.sum[..3].iter().map(|&v| sign(v)).collect::<String>(),
            ),
        ] {
            // The first of each tie, and a sum from +0 never -0: +0 + -0 is +0.
            assert_eq!(
                (min.as_str(), max.as_str(), sum.as_str()),
                ("+--", "+--", "+++")
            );
        }
        // A coarse bucket folds its children in order: bucket 0's +0, then bucket 1's -0.
        let coarse = columns::<f32>(&done.head[1], 0);
        assert!(coarse.min[0].is_sign_positive() && coarse.max[0].is_sign_positive());
        // An empty bucket's sum is +0.
        assert!(f.sum[5] == 0.0 && f.sum[5].is_sign_positive());
    }

    #[test]
    fn an_int_channels_sums_are_exact_at_full_scale() {
        // verge's case: u32 codes at full scale, 100,000 samples to a bucket of 2^17 ticks (one
        // a tick for 100,000 ticks). By hand: 100,000 x (2^32 - 1) = 429,496,729,500,000.
        let coding = AnalogCoding::Int(IntCoding {
            bits: 32,
            signed: false,
            scale: Exact::new(1, 1_000_000_000),
            offset: None,
        });
        let mut b = Builder::new(17, &[coded("I", coding)], &[]);
        for t in 0..100_000 {
            b.sample(0, t, Sample::U64(u64::from(u32::MAX))).unwrap();
        }
        let (done, _) = b.finish(1 << 18).unwrap();
        let c = columns::<u64>(&done.head[0], 0);
        assert_eq!(c.sum, [429_496_729_500_000, 0]);
        assert_eq!(
            (c.min[0], c.max[0]),
            (u64::from(u32::MAX), u64::from(u32::MAX))
        );
    }

    #[test]
    fn derived_buckets_follow_the_channels_and_pair_their_operands() {
        // V int 32 u [1,1000], I int 32 u [1,1e9]: P = I x V, an integer product. Samples at 0
        // and 1 in one 16-tick bucket: I 3 and 5, V 7 and 11, so P 21 and 55, sum 76.
        let v = AnalogCoding::Int(IntCoding {
            bits: 32,
            signed: false,
            scale: Exact::new(1, 1000),
            offset: None,
        });
        let i = AnalogCoding::Int(IntCoding {
            scale: Exact::new(1, 1_000_000_000),
            ..IntCoding::new(32, false)
        });
        let p = Derived {
            name: "P".into(),
            unit: "W".into(),
            product: ["I".into(), "V".into()],
            significant: None,
        };
        let channels = [coded("V", v), coded("I", i)];
        let mut b = Builder::new(4, &channels, std::slice::from_ref(&p));
        // V's samples first, then I's, as a block hands them out.
        b.sample(0, 0, Sample::U64(7)).unwrap();
        b.sample(0, 1, Sample::U64(11)).unwrap();
        b.sample(1, 0, Sample::U64(3)).unwrap();
        b.sample(1, 1, Sample::U64(5)).unwrap();
        let (done, _) = b.finish(16).unwrap();
        assert_eq!(done.head[0].channels.len(), 3);
        assert_eq!(buckets_of(columns::<u64>(&done.head[0], 2)), [(21, 55, 76)]);

        // An operand sample whose partner sits at another tick, or never comes, is refused.
        let mut b = Builder::new(4, &channels, std::slice::from_ref(&p));
        b.sample(0, 0, Sample::U64(7)).unwrap();
        let err = b.sample(1, 1, Sample::U64(3)).unwrap_err();
        assert_eq!(
            err.to_string(),
            "derived channel P: V has a sample at tick 0 where I has one at 1"
        );
        let mut b = Builder::new(4, &channels, std::slice::from_ref(&p));
        b.sample(0, 0, Sample::U64(7)).unwrap();
        let err = b.settle(5).unwrap_err();
        assert_eq!(
            err.to_string(),
            "derived channel P: V has a sample at tick 0 that I lacks"
        );
    }

    /// Where two levels first differ, for a message that names a bucket rather than printing
    /// millions of them.
    fn difference(a: &Level, b: &Level) -> Option<String> {
        if (a.shift, a.buckets) != (b.shift, b.buckets) {
            return Some(format!(
                "shift and buckets {:?} against {:?}",
                (a.shift, a.buckets),
                (b.shift, b.buckets)
            ));
        }
        for i in 0..a.buckets {
            for (c, (x, y)) in a.channels.iter().zip(&b.channels).enumerate() {
                let same = match (x, y) {
                    (
                        ChannelOverview::Analog(AnalogOverview::F32(x)),
                        ChannelOverview::Analog(AnalogOverview::F32(y)),
                    ) => x.bucket(i as usize) == y.bucket(i as usize),
                    _ => x.digital(i) == y.digital(i),
                };
                if !same {
                    return Some(format!("bucket {i}, channel {c}"));
                }
            }
        }
        None
    }

    /// Clusters of edges and samples with long spans of nothing between them, which the builder
    /// fills in bulk: every level's buckets, the finest from its overview blocks, against buckets
    /// worked out from the items alone, and every coarser level against `coarsen` of those.
    #[test]
    fn spans_with_no_item_fill_as_the_items_alone_give() {
        let shift = 2;
        // 2^23 ticks: 2^21 buckets of 4, so the finest level goes in 512 overview blocks.
        let length: i64 = 1 << 23;
        let mut edges: Vec<i64> = vec![0, 0, 3, 17, 4095, 4096, 16_383];
        // A cluster at a chunk's start, one at a block's start, one inside a block far on.
        edges.extend([4096 * 4, 4096 * 4 + 1, (1 << 14) * 4, 6_000_001, 6_000_003]);
        edges.push(length - 1);
        let samples: Vec<(i64, f32)> = [(0, -0.0), (1, -0.0), (40_000, 2.5), (6_000_002, -1.0)]
            .into_iter()
            .chain((0..50).map(|i| (7_000_000 + 4 * i, i as f32)))
            .collect();
        let mut b = Builder::new(shift, &[dig(true), ana()], &[]);
        let (mut e, mut s) = (0, 0);
        let mut settled: Vec<Settled> = Vec::new();
        // Items in tick order, settling at a few points between them as blocks would close.
        let settles = [5000i64, 2_000_000, 6_000_001, 7_000_100];
        let mut next_settle = 0;
        loop {
            let te = edges.get(e).copied().unwrap_or(i64::MAX);
            let ts = samples.get(s).map_or(i64::MAX, |&(t, _)| t);
            let t = te.min(ts);
            while settles.get(next_settle).is_some_and(|&at| at <= t) {
                b.settle(settles[next_settle]).unwrap();
                settled.extend(b.take_settled());
                next_settle += 1;
            }
            if t == i64::MAX {
                break;
            }
            if te <= ts {
                b.edge(0, te, te);
                e += 1;
            } else {
                b.sample(1, ts, Sample::F32(samples[s].1)).unwrap();
                s += 1;
            }
        }
        let (done, last) = b.finish(length).unwrap();
        settled.extend(last);

        // The finest level, by hand.
        let n = buckets(length, shift);
        let mut want = Level::empty(shift, &[Column::Digital, Column::Analog(Stored::F32)]);
        let mut level = true;
        let (mut e, mut s) = (0, 0);
        for i in 0..n {
            let end = ((i + 1) << shift) as i64;
            let entering = level;
            let mut change = false;
            while edges.get(e).is_some_and(|&t| t < end) {
                level = !level;
                change = true;
                e += 1;
            }
            let mut bucket = AnalogValue::empty(Stored::F32);
            while samples.get(s).is_some_and(|&(t, _)| t < end) {
                bucket.take(Sample::F32(samples[s].1));
                s += 1;
            }
            want.push(&[
                Value::Digital {
                    level: entering,
                    change,
                },
                Value::Analog(bucket),
            ]);
        }
        let mut finest = Level::empty(
            shift,
            &want
                .channels
                .iter()
                .map(|c| match c {
                    ChannelOverview::Digital { .. } => Column::Digital,
                    ChannelOverview::Analog(_) => Column::Analog(Stored::F32),
                })
                .collect::<Vec<_>>(),
        );
        let mut blocks: Vec<&Settled> = settled.iter().filter(|s| s.shift == shift).collect();
        blocks.sort_by_key(|s| s.k);
        assert_eq!(blocks.len(), 512);
        for (k, block) in blocks.iter().enumerate() {
            assert_eq!(block.k, k as u64);
            finest.append(&block.level);
        }
        assert_eq!(difference(&finest, &want), None, "the finest level");
        // The blocks of nothing share one level: far fewer levels than blocks.
        let distinct = blocks
            .iter()
            .enumerate()
            .filter(|&(i, b)| i == 0 || !Arc::ptr_eq(&b.level, &blocks[i - 1].level))
            .count();
        assert!(distinct < 20, "{distinct} distinct blocks of 512");
        for coarse in &done.head {
            let whole = coarsen(&want, coarse.shift, length);
            assert_eq!(difference(coarse, &whole), None, "level {}", coarse.shift);
        }
        let mut coarser: Vec<&Settled> = settled.iter().filter(|s| s.shift != shift).collect();
        coarser.sort_by_key(|s| (s.shift, s.k));
        for block in coarser {
            let whole = coarsen(&want, block.shift, length);
            let at = block.k * BLOCK_BUCKETS;
            assert_eq!(
                difference(&block.level, &whole.slice(at, block.level.buckets)),
                None,
                "level {}, block {}",
                block.shift,
                block.k
            );
        }
    }
}
