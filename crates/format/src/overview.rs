//! The overview: each channel summarised in buckets of 2^shift axis ticks, so a wide view draws
//! without fetching blocks. Each channel and derived channel has a ladder of its own, levels from
//! its finest shift in steps of 4 up to its strip, the first with at most 1,024 buckets. A digital
//! bucket holds the level at its start and whether the level changes inside it. An analog bucket
//! holds the least and greatest of its samples and their sum, in the types FORMAT.md's table
//! gives; a derived channel's buckets follow from its operands' samples.
//!
//! A level decodes, for its `n` buckets, to:
//! - digital: the level bits, then the change bits, each `ceil(n / 8)` bytes, bucket i in bit
//!   `i % 8` of byte `i / 8`;
//! - analog: n minimums, then n maximums, then n sums. A bucket with no sample holds a minimum
//!   above its maximum, and a sum of 0.

use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;

use crate::header::{AnalogCoding, DeclaredChannel, DeclaredKind, Derived, Stored};
use crate::model::{Gap, Sample};
use crate::samples::Product;

/// The step between levels, as a shift.
pub const STEP: u8 = 4;
/// The coarsest shift a level can have.
pub const MAX_SHIFT: u8 = 62;
/// A channel's ladder stops at the first level with at most this many buckets: its strip.
pub const STRIP_BUCKETS: u64 = 1024;
/// Buckets in one overview block.
pub const BLOCK_BUCKETS: u64 = 1 << 12;
/// The buckets a writer seals into one shared piece of a level it holds, so a snapshot shares
/// them rather than copying. Divides `BLOCK_BUCKETS` and is a multiple of 8.
pub const CHUNK: u64 = 1024;

/// Enough buckets to cover ticks `0..length`: `ceil(length / 2^shift)`, and at least one.
pub fn buckets(length: i64, shift: u8) -> u64 {
    ((length.max(1) - 1) as u64 >> shift) + 1
}

/// A channel's levels for a recording of `length`, as shifts, finest first: from `finest` in
/// steps of 4 to the first with at most 1,024 buckets, or to shift 62. The last is the strip; the
/// others go in overview blocks.
pub fn ladder(length: i64, finest: u8) -> Vec<u8> {
    let mut out = Vec::new();
    let mut shift = finest.min(MAX_SHIFT);
    loop {
        out.push(shift);
        if buckets(length, shift) <= STRIP_BUCKETS || shift + STEP > MAX_SHIFT {
            return out;
        }
        shift += STEP;
    }
}

/// The overview blocks a level is cut into.
pub fn block_count(length: i64, shift: u8) -> u64 {
    buckets(length, shift).div_ceil(BLOCK_BUCKETS)
}

/// The buckets overview block `k` of a level holds: `(first bucket, count)`.
pub fn block_buckets(length: i64, shift: u8, k: u64) -> (u64, u64) {
    let first = k.saturating_mul(BLOCK_BUCKETS);
    (
        first,
        BLOCK_BUCKETS.min(buckets(length, shift).saturating_sub(first)),
    )
}

/// The bytes an `int` channel's minimum or maximum takes: the smallest of 1, 2, 4 and 8 that
/// holds its `bits`.
pub fn int_width(bits: u8) -> u8 {
    match bits {
        0..=8 => 1,
        9..=16 => 2,
        17..=32 => 4,
        _ => 8,
    }
}

/// What one channel's buckets hold: a digital channel's bits, or an analog one's numbers in the
/// type it is summarised in, an integer's minimum and maximum `width` bytes wide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    Digital,
    Analog { stored: Stored, width: u8 },
}

impl Column {
    pub fn of_coding(coding: AnalogCoding) -> Self {
        let stored = coding.stored();
        let width = match coding {
            AnalogCoding::Int(int) => int_width(int.bits),
            AnalogCoding::F64 => 8,
            AnalogCoding::F32 | AnalogCoding::Codes { .. } => 4,
        };
        Self::Analog { stored, width }
    }

    pub fn of_product(product: Product) -> Self {
        let width = match product {
            Product::Int { bits, .. } => int_width(bits),
            Product::F64 { .. } => 8,
        };
        Self::Analog {
            stored: product.stored(),
            width,
        }
    }

    /// The decoded size of `n` buckets.
    pub fn decoded_len(self, n: u64) -> u64 {
        match self {
            Column::Digital => 2 * n.div_ceil(8),
            Column::Analog { stored, width } => {
                let (value, sum) = match stored {
                    Stored::F32 => (4, 8),
                    Stored::F64 => (8, 8),
                    Stored::U64 | Stored::I64 => (u64::from(width), 16),
                };
                n.saturating_mul(2 * value + sum)
            }
        }
    }
}

/// A number an analog bucket holds, and the sum it adds up to.
pub trait Num: Copy + fmt::Debug + PartialOrd + Send + Sync + 'static {
    type Sum: Copy + fmt::Debug + Send + Sync + 'static;
    /// An empty bucket's minimum: the type's largest.
    const GREATEST: Self;
    /// An empty bucket's maximum: the type's smallest.
    const LEAST: Self;
    const NO_SUM: Self::Sum;
    const SUM_SIZE: usize;
    /// Whether it is stored in fewer bytes than it holds in memory.
    const NARROWS: bool;
    fn lesser(self, other: Self) -> Self;
    fn greater(self, other: Self) -> Self;
    fn add(sum: Self::Sum, v: Self) -> Self::Sum;
    fn add_sum(a: Self::Sum, b: Self::Sum) -> Self::Sum;
    fn is_nan(self) -> bool;
    /// The same bits: a float compares by its bits, so a NaN sum equals itself.
    fn same(a: Self, b: Self) -> bool;
    fn same_sum(a: Self::Sum, b: Self::Sum) -> bool;
    /// The value's `width` low bytes, or the whole float.
    fn put(self, width: usize, out: &mut Vec<u8>);
    /// The largest and the smallest number `width` bytes hold: an empty bucket's, stored.
    fn empty_at(width: usize) -> (Self, Self);
    fn put_sum(sum: Self::Sum, out: &mut Vec<u8>);
    /// A value from its stored bytes, sign- or zero-extended.
    fn read(bytes: &[u8]) -> Self;
    fn read_sum(bytes: &[u8]) -> Self::Sum;
    fn stored_size(width: usize) -> usize;
}

macro_rules! float {
    ($t:ty, $size:literal) => {
        impl Num for $t {
            type Sum = f64;
            const GREATEST: Self = <$t>::INFINITY;
            const LEAST: Self = <$t>::NEG_INFINITY;
            const NO_SUM: f64 = 0.0;
            const SUM_SIZE: usize = 8;
            const NARROWS: bool = false;
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
            fn put(self, _: usize, out: &mut Vec<u8>) {
                out.extend_from_slice(&self.to_le_bytes());
            }
            fn empty_at(_: usize) -> (Self, Self) {
                (Self::GREATEST, Self::LEAST)
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
            fn stored_size(_: usize) -> usize {
                $size
            }
        }
    };
}

float!(f32, 4);
float!(f64, 8);

impl Num for u64 {
    type Sum = i128;
    const GREATEST: Self = u64::MAX;
    const LEAST: Self = 0;
    const NO_SUM: i128 = 0;
    const SUM_SIZE: usize = 16;
    const NARROWS: bool = true;
    fn lesser(self, other: Self) -> Self {
        self.min(other)
    }
    fn greater(self, other: Self) -> Self {
        self.max(other)
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
    fn put(self, width: usize, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.to_le_bytes()[..width]);
    }
    fn empty_at(width: usize) -> (Self, Self) {
        (u64::MAX >> (64 - 8 * width), 0)
    }
    fn put_sum(sum: i128, out: &mut Vec<u8>) {
        out.extend_from_slice(&sum.to_le_bytes());
    }
    fn read(bytes: &[u8]) -> Self {
        let mut b = [0; 8];
        b[..bytes.len()].copy_from_slice(bytes);
        u64::from_le_bytes(b)
    }
    fn read_sum(bytes: &[u8]) -> i128 {
        i128::from_le_bytes(bytes.try_into().unwrap())
    }
    fn stored_size(width: usize) -> usize {
        width
    }
}

impl Num for i64 {
    type Sum = i128;
    const GREATEST: Self = i64::MAX;
    const LEAST: Self = i64::MIN;
    const NO_SUM: i128 = 0;
    const SUM_SIZE: usize = 16;
    const NARROWS: bool = true;
    fn lesser(self, other: Self) -> Self {
        self.min(other)
    }
    fn greater(self, other: Self) -> Self {
        self.max(other)
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
    fn put(self, width: usize, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.to_le_bytes()[..width]);
    }
    fn empty_at(width: usize) -> (Self, Self) {
        let bits = 8 * width as u32;
        (i64::MAX >> (64 - bits), i64::MIN >> (64 - bits))
    }
    fn put_sum(sum: i128, out: &mut Vec<u8>) {
        out.extend_from_slice(&sum.to_le_bytes());
    }
    fn read(bytes: &[u8]) -> Self {
        let mut b = [0; 8];
        b[..bytes.len()].copy_from_slice(bytes);
        let spare = 64 - 8 * bytes.len() as u32;
        (i64::from_le_bytes(b) << spare) >> spare
    }
    fn read_sum(bytes: &[u8]) -> i128 {
        i128::from_le_bytes(bytes.try_into().unwrap())
    }
    fn stored_size(width: usize) -> usize {
        width
    }
}

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

    /// Whether it holds no sample: a minimum above its maximum.
    pub fn is_empty(&self) -> bool {
        self.min > self.max
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

    /// The stored layout: an empty bucket's minimum and maximum as the largest and the smallest
    /// number `width` bytes hold.
    fn write(&self, width: usize, out: &mut Vec<u8>) {
        if T::NARROWS {
            let (lo, hi) = T::empty_at(width);
            for (i, &v) in self.min.iter().enumerate() {
                let empty = v > self.max[i];
                if empty { lo } else { v }.put(width, out);
            }
            for (i, &v) in self.max.iter().enumerate() {
                let empty = self.min[i] > v;
                if empty { hi } else { v }.put(width, out);
            }
        } else {
            for &v in self.min.iter().chain(&self.max) {
                v.put(width, out);
            }
        }
        for &s in &self.sum {
            T::put_sum(s, out);
        }
    }

    /// The stored layout read back, an empty bucket's minimum and maximum as the type's
    /// extremes, as a writer holds them.
    fn parse(c: &mut Cursor<'_>, n: usize, width: usize) -> Option<Self> {
        let size = T::stored_size(width);
        let nums = |b: &[u8]| b.chunks_exact(size).map(T::read).collect::<Vec<T>>();
        let mut min = nums(c.take(n.checked_mul(size)?)?);
        let mut max = nums(c.take(n.checked_mul(size)?)?);
        if T::NARROWS {
            for (lo, hi) in min.iter_mut().zip(&mut max) {
                if *lo > *hi {
                    (*lo, *hi) = (T::GREATEST, T::LEAST);
                }
            }
        }
        Some(Self {
            min,
            max,
            sum: c
                .take(n.checked_mul(T::SUM_SIZE)?)?
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

impl ChannelOverview {
    fn empty(column: Column) -> Self {
        match column {
            Column::Digital => Self::Digital {
                level: Vec::new(),
                change: Vec::new(),
            },
            Column::Analog { stored, .. } => Self::Analog(AnalogOverview::new(stored)),
        }
    }

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

/// Some of one channel's level: `buckets` buckets of `2^shift` ticks.
#[derive(Clone, Debug, PartialEq)]
pub struct Level {
    pub shift: u8,
    pub buckets: u64,
    pub data: ChannelOverview,
}

impl Level {
    /// A level of no buckets yet, for `column`.
    pub fn empty(shift: u8, column: Column) -> Level {
        Level {
            shift,
            buckets: 0,
            data: ChannelOverview::empty(column),
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
            data: match &self.data {
                ChannelOverview::Digital { level, change } => ChannelOverview::Digital {
                    level: bits(level),
                    change: bits(change),
                },
                ChannelOverview::Analog(a) => {
                    ChannelOverview::Analog(map_each!(a, c => c.slice(first as usize, n as usize)))
                }
            },
        }
    }

    /// `other`'s buckets after this level's, whose count is a multiple of 8.
    pub fn append(&mut self, other: &Level) {
        debug_assert!(self.buckets.is_multiple_of(8));
        match (&mut self.data, &other.data) {
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
        self.buckets += other.buckets;
    }

    /// Bucket `self.buckets` takes `value`.
    fn push(&mut self, value: &Value) {
        let i = self.buckets;
        match (&mut self.data, value) {
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
        self.buckets += 1;
    }

    /// The level's stored bytes: an overview block's or a strip's whole payload.
    pub fn write(&self, column: Column, out: &mut Vec<u8>) {
        match &self.data {
            ChannelOverview::Digital { level, change } => {
                out.extend_from_slice(level);
                out.extend_from_slice(change);
            }
            ChannelOverview::Analog(a) => {
                let width = match column {
                    Column::Analog { width, .. } => usize::from(width),
                    Column::Digital => 8,
                };
                each!(a, c => c.write(width, out))
            }
        }
    }

    pub fn to_bytes(&self, column: Column) -> Vec<u8> {
        let mut out = Vec::with_capacity(column.decoded_len(self.buckets) as usize);
        self.write(column, &mut out);
        out
    }

    /// Parses `n` buckets of `column`. None unless `bytes` is exactly their size.
    pub fn parse(bytes: &[u8], shift: u8, n: u64, column: Column) -> Option<Level> {
        let len = usize::try_from(n).ok()?;
        let mut c = Cursor { bytes, pos: 0 };
        let data = match column {
            Column::Analog { stored, width } => {
                let width = usize::from(width);
                ChannelOverview::Analog(match stored {
                    Stored::F32 => AnalogOverview::F32(Columns::parse(&mut c, len, width)?),
                    Stored::F64 => AnalogOverview::F64(Columns::parse(&mut c, len, width)?),
                    Stored::U64 => AnalogOverview::U64(Columns::parse(&mut c, len, width)?),
                    Stored::I64 => AnalogOverview::I64(Columns::parse(&mut c, len, width)?),
                })
            }
            Column::Digital => {
                let level = c.take(bit_bytes(n))?.to_vec();
                let change = c.take(bit_bytes(n))?.to_vec();
                ChannelOverview::Digital { level, change }
            }
        };
        (c.pos == bytes.len()).then_some(Level {
            shift,
            buckets: n,
            data,
        })
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

/// A coarser level derived from a finer one. `shift` must exceed `fine.shift`; `length` sets the
/// coarser level's buckets.
pub fn coarsen(fine: &Level, shift: u8, length: i64) -> Level {
    let k = shift - fine.shift;
    let n = buckets(length, shift);
    let children = |b: u64| (b << k).min(fine.buckets)..((b + 1) << k).min(fine.buckets);
    let data = match &fine.data {
        ChannelOverview::Digital { level, change } => {
            let mut out_level = vec![0; bit_bytes(n)];
            let mut out_change = vec![0; bit_bytes(n)];
            for b in 0..n {
                let range = children(b);
                if !range.is_empty() && bit(level, range.start) {
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
    };
    Level {
        shift,
        buckets: n,
        data,
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

/// One bucket of one channel.
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

    /// Bucket `b`'s value, its events taken.
    fn take(&mut self, b: u64) -> Value {
        match self {
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
        }
    }
}

/// A level's settled buckets not yet cut into overview blocks, from bucket `first`: whole chunks
/// of `CHUNK` buckets, shared, then the rest.
struct Store {
    shift: u8,
    column: Column,
    first: u64,
    sealed: VecDeque<Arc<Level>>,
    open: Level,
}

impl Store {
    fn new(shift: u8, column: Column) -> Self {
        Self {
            shift,
            column,
            first: 0,
            sealed: VecDeque::new(),
            open: Level::empty(shift, column),
        }
    }

    fn n(&self) -> u64 {
        self.sealed.len() as u64 * CHUNK + self.open.buckets
    }

    fn push(&mut self, value: &Value) {
        self.open.push(value);
        if self.open.buckets == CHUNK {
            let full = std::mem::replace(&mut self.open, Level::empty(self.shift, self.column));
            self.sealed.push_back(Arc::new(full));
        }
    }

    /// Every bucket held, as one level, and none held after.
    fn take_all(&mut self) -> Level {
        let mut out = Level::empty(self.shift, self.column);
        for chunk in self.sealed.drain(..) {
            out.append(&chunk);
        }
        let open = std::mem::replace(&mut self.open, Level::empty(self.shift, self.column));
        out.append(&open);
        self.first += out.buckets;
        out
    }

    fn held(&self) -> HeldLevel {
        let mut chunks: Vec<Arc<Level>> = self.sealed.iter().cloned().collect();
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

/// Overview blocks `k..k + n` of one level, alike, settled and not yet handed on as a record: a
/// run ends when a block with other bytes settles, or at the finish.
#[derive(Clone, Debug)]
pub struct Run {
    pub k: u64,
    pub n: u64,
    pub raw: Arc<Vec<u8>>,
}

/// An overview block record's content: blocks `k..k + n` of level `li` of the ladder, each
/// `raw`, decoded.
#[derive(Clone, Debug, PartialEq)]
pub struct Settled {
    pub li: usize,
    pub shift: u8,
    pub k: u64,
    pub n: u32,
    pub raw: Arc<Vec<u8>>,
}

/// One level of a ladder as a writer holds it, for a snapshot.
#[derive(Clone, Debug)]
pub struct HeldRung {
    pub shift: u8,
    /// Whether the level goes in overview blocks: it has more buckets than a strip holds.
    pub blocks: bool,
    /// The run of alike blocks not yet handed on.
    pub run: Option<Run>,
    /// The settled buckets after the last block.
    pub held: HeldLevel,
}

struct Rung {
    shift: u8,
    blocks: bool,
    store: Store,
    /// For a level coarser than the finest: the bucket its children are being folded into.
    acc: Option<(u64, Value)>,
    run: Option<Run>,
    /// The bytes of a block of buckets that each hold this value, the last one asked for.
    quiet: Option<(Value, Arc<Vec<u8>>)>,
}

/// What a ladder comes to at the end.
pub struct LadderEnd {
    /// The strip: the coarsest level, every bucket.
    pub strip: Level,
    /// The shifts of the levels in overview blocks, finest first.
    pub levels: Vec<u8>,
    /// The overview blocks the end settles, after those `take_settled` gave.
    pub settled: Vec<Settled>,
}

/// Each channel's column, then each derived channel's, as a declaration has them.
pub fn columns(channels: &[DeclaredChannel], derived: &[Derived]) -> Vec<Column> {
    let mut out: Vec<Column> = channels
        .iter()
        .map(|c| match &c.data {
            DeclaredKind::Digital { .. } => Column::Digital,
            DeclaredKind::Analog { coding, .. } => Column::of_coding(*coding),
        })
        .collect();
    out.extend(
        products(channels, derived)
            .iter()
            .map(|&(_, _, p)| Column::of_product(p)),
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

/// Builds one channel's overview as its items arrive. The items and gap ends arrive in tick
/// order, and `settle` says when every item before a tick is in, so the buckets before it cannot
/// change. It holds the levels that may still be the strip, each level's buckets after its last
/// whole block, and each level's open run, and hands on overview blocks as runs end.
pub struct Ladder {
    column: Column,
    finest: u8,
    intake: Intake,
    /// Finest buckets before this have settled.
    base: u64,
    rungs: Vec<Rung>,
    settled: Vec<Settled>,
}

impl Ladder {
    /// A ladder from shift `finest`. `initial` is a digital channel's level at its start; an
    /// analog column takes None.
    pub fn new(finest: u8, column: Column, initial: Option<bool>) -> Self {
        let finest = finest.min(MAX_SHIFT);
        let intake = match column {
            Column::Analog { stored, .. } => Intake::Analog {
                stored,
                events: VecDeque::new(),
            },
            Column::Digital => {
                let initial = initial.unwrap_or(false);
                Intake::Digital {
                    level: initial,
                    entering: initial,
                    pending: VecDeque::new(),
                    events: VecDeque::new(),
                }
            }
        };
        let rungs = (finest..=MAX_SHIFT)
            .step_by(STEP as usize)
            .map(|shift| Rung {
                shift,
                blocks: false,
                store: Store::new(shift, column),
                acc: None,
                run: None,
                quiet: None,
            })
            .collect();
        Self {
            column,
            finest,
            intake,
            base: 0,
            rungs,
            settled: Vec::new(),
        }
    }

    pub fn finest(&self) -> u8 {
        self.finest
    }

    pub fn column(&self) -> Column {
        self.column
    }

    /// The axis tick before which every bucket of the finest level has settled.
    pub fn settled_to(&self) -> u64 {
        self.base
    }

    fn digital_event(&mut self, tick: i64, after: impl FnOnce(bool) -> bool) {
        let bucket = tick as u64 >> self.finest;
        debug_assert!(bucket >= self.base, "an event in a settled bucket");
        let Intake::Digital { level, events, .. } = &mut self.intake else {
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

    /// Takes the gap ends that come before: those whose `to` lies before source tick `source`,
    /// or, with none, before axis tick `axis`.
    fn reach(&mut self, source: Option<i64>, axis: i64) {
        loop {
            let Intake::Digital { pending, .. } = &mut self.intake else {
                return;
            };
            match pending.front() {
                Some(&(g, t)) if source.map_or(t < axis, |k| g.to < k) => {
                    pending.pop_front();
                    self.digital_event(t, |_| g.level);
                }
                _ => return,
            }
        }
    }

    /// An edge at source tick `source`, axis tick `tick`, after the channel's earlier edges and
    /// gap ends: a gap end at the same axis tick that comes before it in source ticks takes
    /// effect first.
    pub fn edge(&mut self, source: i64, tick: i64) {
        self.reach(Some(source), tick);
        self.digital_event(tick, |level| !level);
    }

    /// A gap, in the channel's source's ticks, whose `to` lies at axis tick `to`: its end takes
    /// effect once the items pass it.
    pub fn gap(&mut self, gap: Gap, to: i64) {
        if let Intake::Digital { pending, .. } = &mut self.intake {
            pending.push_back((gap, to));
        }
    }

    /// A sample at axis tick `tick`, in the type the channel stores. A NaN leaves its bucket as
    /// it was.
    #[inline]
    pub fn sample(&mut self, tick: i64, sample: Sample) {
        let bucket = tick as u64 >> self.finest;
        debug_assert!(bucket >= self.base, "a sample in a settled bucket");
        let Intake::Analog { stored, events } = &mut self.intake else {
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

    /// Every item and gap end before axis tick `tick` is in, and the channel's length is at
    /// least `tick`. Settles the finest buckets wholly before it and the coarser buckets they
    /// complete; a level that now has more buckets than a strip goes in overview blocks.
    pub fn settle(&mut self, tick: i64) {
        self.settle_some(tick, usize::MAX);
    }

    /// `settle`, stopping once `most` overview blocks wait in `take_settled`: whether it reached
    /// `tick`. Called again with the same tick, it goes on from where it stopped, so a long span
    /// settles a few blocks at a time.
    pub fn settle_some(&mut self, tick: i64, most: usize) -> bool {
        let tick = tick.max(0);
        self.reach(None, tick);
        for li in 0..self.rungs.len() {
            let rung = &mut self.rungs[li];
            if !rung.blocks && buckets(tick, rung.shift) > STRIP_BUCKETS && tick > 0 {
                rung.blocks = true;
                self.cut(li);
            }
        }
        self.settle_to(tick as u64 >> self.finest, most)
    }

    /// Settles finest buckets `base..upto`, or until `most` overview blocks wait: whether it
    /// reached `upto`. A run of buckets with no event goes in at once.
    fn settle_to(&mut self, upto: u64, most: usize) -> bool {
        while self.base < upto {
            if self.settled.len() >= most {
                return false;
            }
            let b = self.base;
            let next = self.intake.next_bucket().unwrap_or(u64::MAX).min(upto);
            if next > b {
                let quiet = self.intake.quiet();
                self.push_quiet(0, b, next - b, &quiet);
                self.base = next;
                continue;
            }
            let value = self.intake.take(b);
            self.push_one(0, b, &value);
            self.base += 1;
        }
        true
    }

    /// The bytes of a whole block of buckets that each hold `value`.
    fn quiet_block(&mut self, li: usize, value: &Value) -> Arc<Vec<u8>> {
        let column = self.column;
        let rung = &mut self.rungs[li];
        if let Some((v, raw)) = &rung.quiet
            && v == value
        {
            return Arc::clone(raw);
        }
        let mut level = Level::empty(rung.shift, column);
        for _ in 0..BLOCK_BUCKETS {
            level.push(value);
        }
        let raw = Arc::new(level.to_bytes(column));
        rung.quiet = Some((*value, Arc::clone(&raw)));
        raw
    }

    /// Bucket `b` of level `li` settles: store it, and fold it into the next level.
    fn push_one(&mut self, li: usize, b: u64, value: &Value) {
        self.store(li, value);
        let Some(next) = self.rungs.get_mut(li + 1) else {
            return;
        };
        let parent = b >> STEP;
        let done = match &mut next.acc {
            Some((p, acc)) if *p == parent => {
                acc.fold(value);
                None
            }
            open => open.replace((parent, *value)),
        };
        // A child of a later parent: the one before has all its children.
        if let Some((p, acc)) = done {
            self.push_one(li + 1, p, &acc);
        }
    }

    /// One bucket into level `li`'s store, a whole block cut from it as it completes one.
    fn store(&mut self, li: usize, value: &Value) {
        let rung = &mut self.rungs[li];
        rung.store.push(value);
        if rung.blocks && rung.store.n() == BLOCK_BUCKETS {
            self.cut(li);
        }
    }

    /// Buckets `b..b + n` of level `li` settle, each holding `value`, a bucket with no event:
    /// store them, a whole block at a time once the level goes in blocks, and fold them into the
    /// next level, whose whole buckets among them hold the same.
    fn push_quiet(&mut self, li: usize, b: u64, n: u64, value: &Value) {
        if n == 0 {
            return;
        }
        if n == 1 {
            return self.push_one(li, b, value);
        }
        let mut left = n;
        if self.rungs[li].blocks {
            // To the end of the block the level holds a part of, then whole blocks, then the rest.
            let held = self.rungs[li].store.n();
            if held > 0 {
                let fill = left.min(BLOCK_BUCKETS - held);
                for _ in 0..fill {
                    self.store(li, value);
                }
                left -= fill;
            }
            let whole = left / BLOCK_BUCKETS;
            if whole > 0 {
                let raw = self.quiet_block(li, value);
                let k = self.rungs[li].store.first / BLOCK_BUCKETS;
                self.rungs[li].store.first += whole * BLOCK_BUCKETS;
                self.extend_run(li, k, whole, raw);
                left -= whole * BLOCK_BUCKETS;
            }
        }
        for _ in 0..left {
            self.store(li, value);
        }
        let Some(next) = self.rungs.get_mut(li + 1) else {
            return;
        };
        let (first, last) = (b >> STEP, (b + n - 1) >> STEP);
        // The parent the run starts in: its earlier children are in `acc`, and quiet children
        // fold into it as nothing.
        let done = match &mut next.acc {
            Some((p, acc)) if *p == first => {
                acc.fold(value);
                None
            }
            open => open.replace((first, *value)),
        };
        if let Some((p, acc)) = done {
            self.push_one(li + 1, p, &acc);
        }
        if last > first {
            let Some((p, acc)) = self.rungs[li + 1].acc.take() else {
                unreachable!("the run's first parent is open")
            };
            self.push_one(li + 1, p, &acc);
            self.push_quiet(li + 1, first + 1, last - first - 1, value);
            self.rungs[li + 1].acc = Some((last, *value));
        }
    }

    /// Cuts level `li`'s whole held block, once it goes in blocks.
    fn cut(&mut self, li: usize) {
        let column = self.column;
        let rung = &mut self.rungs[li];
        if !rung.blocks || rung.store.n() < BLOCK_BUCKETS {
            return;
        }
        let k = rung.store.first / BLOCK_BUCKETS;
        let raw = Arc::new(rung.store.take_all().to_bytes(column));
        self.extend_run(li, k, 1, raw);
    }

    /// Blocks `k..k + n` of level `li` settle, each `raw`: they extend its run where they follow
    /// it with its bytes, and otherwise end it and start the next.
    fn extend_run(&mut self, li: usize, k: u64, n: u64, raw: Arc<Vec<u8>>) {
        let shift = self.rungs[li].shift;
        let rung = &mut self.rungs[li];
        let alike = rung
            .run
            .as_ref()
            .is_some_and(|r| r.k + r.n == k && (Arc::ptr_eq(&r.raw, &raw) || *r.raw == *raw));
        if !alike && let Some(r) = rung.run.take() {
            emit(&mut self.settled, li, shift, r);
        }
        let rung = &mut self.rungs[li];
        let run = rung.run.get_or_insert(Run {
            k,
            n: 0,
            raw: Arc::clone(&raw),
        });
        run.n += n;
        // A record's `n` is a u32: a longer run is several records.
        while run.n > u64::from(u32::MAX) {
            let whole = Run {
                k: run.k,
                n: u64::from(u32::MAX),
                raw: Arc::clone(&run.raw),
            };
            run.k += u64::from(u32::MAX);
            run.n -= u64::from(u32::MAX);
            emit(&mut self.settled, li, shift, whole);
        }
    }

    /// The overview blocks settled since the last call, in the order they settled.
    pub fn take_settled(&mut self) -> Vec<Settled> {
        std::mem::take(&mut self.settled)
    }

    /// Each level as the ladder holds it, finest first.
    pub fn held(&self) -> Vec<HeldRung> {
        self.rungs
            .iter()
            .map(|r| HeldRung {
                shift: r.shift,
                blocks: r.blocks,
                run: r.run.clone(),
                held: r.store.held(),
            })
            .collect()
    }

    /// The channel ends at `length`; every item is in. Gap ends at or past it are dropped.
    pub fn finish(mut self, length: i64) -> LadderEnd {
        self.settle(length);
        self.settle_to(buckets(length, self.finest), usize::MAX);
        // The last bucket of each coarser level has only the children there are.
        for li in 1..self.rungs.len() {
            if let Some((p, acc)) = self.rungs[li].acc.take() {
                self.push_one(li, p, &acc);
            }
        }
        let shifts = ladder(length, self.finest);
        let strip_at = shifts.len() - 1;
        let column = self.column;
        for li in 0..strip_at {
            let shift = self.rungs[li].shift;
            debug_assert!(self.rungs[li].blocks, "a level past a strip's buckets");
            self.cut(li);
            let last = block_count(length, shift) - 1;
            let rung = &mut self.rungs[li];
            let run = rung.run.take();
            if rung.store.n() > 0 {
                let raw = Arc::new(rung.store.take_all().to_bytes(column));
                if let Some(r) = run {
                    emit(&mut self.settled, li, shift, r);
                }
                emit(&mut self.settled, li, shift, Run { k: last, n: 1, raw });
            } else {
                // The level ends on a whole block, the run's last: a record of its own.
                let r = run.expect("a level in blocks holds its last block");
                debug_assert_eq!(r.k + r.n - 1, last);
                let raw = Arc::clone(&r.raw);
                if r.n > 1 {
                    emit(
                        &mut self.settled,
                        li,
                        shift,
                        Run {
                            k: r.k,
                            n: r.n - 1,
                            raw: Arc::clone(&raw),
                        },
                    );
                }
                emit(&mut self.settled, li, shift, Run { k: last, n: 1, raw });
            }
        }
        let strip = self.rungs[strip_at].store.take_all();
        debug_assert_eq!(strip.buckets, buckets(length, shifts[strip_at]));
        LadderEnd {
            strip,
            levels: shifts[..strip_at].to_vec(),
            settled: self.settled,
        }
    }
}

fn emit(settled: &mut Vec<Settled>, li: usize, shift: u8, run: Run) {
    settled.push(Settled {
        li,
        shift,
        k: run.k,
        n: run.n as u32,
        raw: run.raw,
    });
}

/// A derived channel's operands' samples waiting for their partner: sample i of one is paired
/// with sample i of the other.
pub struct Pair {
    pub name: String,
    pub operands: [(usize, String); 2],
    pub product: Product,
    /// Samples of one operand the other has not reached, with their ticks; never both at once.
    waiting: [VecDeque<(i64, Sample)>; 2],
}

/// Why a derived channel's operands' samples do not pair.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("derived channel {0}: {1}")]
pub struct Unpaired(pub String, pub String);

impl Pair {
    pub fn new(name: String, operands: [(usize, String); 2], product: Product) -> Self {
        Self {
            name,
            operands,
            product,
            waiting: [VecDeque::new(), VecDeque::new()],
        }
    }

    /// Each derived channel's pair, from a checked declaration.
    pub fn of(channels: &[DeclaredChannel], derived: &[Derived]) -> Vec<Self> {
        products(channels, derived)
            .into_iter()
            .zip(derived)
            .map(|((a, b, product), d)| {
                Self::new(
                    d.name.clone(),
                    [(a, d.product[0].clone()), (b, d.product[1].clone())],
                    product,
                )
            })
            .collect()
    }

    fn mismatch(&self, other: usize, mine: usize, t: i64, tick: i64) -> Unpaired {
        Unpaired(
            self.name.clone(),
            format!(
                "{} has a sample at tick {t} where {} has one at {tick}",
                self.operands[other].1, self.operands[mine].1
            ),
        )
    }

    /// Whether a sample of channel `c` at `tick` pairs with what the other operand has waiting:
    /// `take` would refuse it otherwise.
    #[inline]
    pub fn pairs_with(&self, c: usize, tick: i64) -> Result<(), Unpaired> {
        for mine in 0..2 {
            let other = 1 - mine;
            if self.operands[mine].0 != c || self.operands[other].0 == c {
                continue;
            }
            if let Some(&(t, _)) = self.waiting[other].front()
                && t != tick
            {
                return Err(self.mismatch(other, mine, t, tick));
            }
        }
        Ok(())
    }

    /// A sample of channel `c` at `tick`: the derived sample it completes, if it completes one.
    pub fn take(
        &mut self,
        c: usize,
        tick: i64,
        sample: Sample,
    ) -> Result<Option<Sample>, Unpaired> {
        Ok(match [self.operands[0].0 == c, self.operands[1].0 == c] {
            [true, true] => Some(self.product.sample(sample, sample)),
            [false, false] => None,
            [mine_a, _] => {
                let (mine, other) = if mine_a { (0, 1) } else { (1, 0) };
                let Some((t, partner)) = self.waiting[other].pop_front() else {
                    self.waiting[mine].push_back((tick, sample));
                    return Ok(None);
                };
                if t != tick {
                    return Err(self.mismatch(other, mine, t, tick));
                }
                Some(if mine_a {
                    self.product.sample(sample, partner)
                } else {
                    self.product.sample(partner, sample)
                })
            }
        })
    }

    /// An operand sample still waiting for the other operand's.
    pub fn unpaired(&self) -> Option<Unpaired> {
        self.waiting.iter().enumerate().find_map(|(side, waiting)| {
            waiting.front().map(|&(t, _)| {
                Unpaired(
                    self.name.clone(),
                    format!(
                        "{} has a sample at tick {t} that {} lacks",
                        self.operands[side].1,
                        self.operands[1 - side].1
                    ),
                )
            })
        })
    }

    /// The samples waiting, of either operand.
    pub fn waiting(&self) -> usize {
        self.waiting[0].len() + self.waiting[1].len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::{AnalogCoding, DeclaredChannel, DeclaredKind, Derived, Exact, IntCoding};
    use crate::model::Ratio;

    /// One channel's ladder fed by `feed`, finished at `length`: every level, finest first, each
    /// whole, those in overview blocks put together from their blocks, then the strip; and the
    /// overview blocks it handed on.
    fn levels(
        finest: u8,
        column: Column,
        initial: Option<bool>,
        length: i64,
        feed: impl FnOnce(&mut Ladder),
    ) -> (Vec<Level>, Vec<Settled>) {
        let mut ladder = Ladder::new(finest, column, initial);
        feed(&mut ladder);
        let mut settled = ladder.take_settled();
        let end = ladder.finish(length);
        settled.extend(end.settled);
        let mut out = Vec::new();
        for (li, &shift) in end.levels.iter().enumerate() {
            let mut level = Level::empty(shift, column);
            let mut blocks: Vec<&Settled> = settled.iter().filter(|s| s.li == li).collect();
            blocks.sort_by_key(|s| s.k);
            for s in blocks {
                assert_eq!(
                    level.buckets,
                    s.k * BLOCK_BUCKETS,
                    "2^{shift}: blocks follow on"
                );
                for j in 0..u64::from(s.n) {
                    let (_, n) = block_buckets(length, shift, s.k + j);
                    level.append(&Level::parse(&s.raw, shift, n, column).expect("a block parses"));
                }
            }
            assert_eq!(level.buckets, buckets(length, shift), "2^{shift}");
            out.push(level);
        }
        out.push(end.strip);
        (out, settled)
    }

    fn analog(coding: AnalogCoding) -> Column {
        Column::of_coding(coding)
    }

    fn digital(level: &Level) -> Vec<(bool, bool)> {
        (0..level.buckets)
            .map(|i| level.data.digital(i).unwrap())
            .collect()
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

    fn columns<T: Num>(level: &Level) -> &Columns<T>
    where
        AnalogOverview: AsColumns<T>,
    {
        level.data.analog().unwrap().columns()
    }

    /// Each bucket as (min, max, sum).
    fn buckets_of<T: Num>(c: &Columns<T>) -> Vec<(T, T, T::Sum)> {
        (0..c.len())
            .map(|i| (c.min[i], c.max[i], c.sum[i]))
            .collect()
    }

    /// The first bucket where two levels differ, so a failure names one rather than printing
    /// millions.
    fn difference(a: &Level, b: &Level) -> Option<String> {
        if (a.shift, a.buckets) != (b.shift, b.buckets) {
            return Some(format!(
                "shift and buckets {:?} against {:?}",
                (a.shift, a.buckets),
                (b.shift, b.buckets)
            ));
        }
        (0..a.buckets).find_map(|i| {
            let same = match (&a.data, &b.data) {
                (
                    ChannelOverview::Analog(AnalogOverview::F32(x)),
                    ChannelOverview::Analog(AnalogOverview::F32(y)),
                ) => x.bucket(i as usize) == y.bucket(i as usize),
                (x, y) => x.digital(i) == y.digital(i),
            };
            (!same).then(|| format!("bucket {i}"))
        })
    }

    #[test]
    fn digital_buckets_by_hand() {
        // Buckets of 16 ticks over 0..64: edges at 3 (in bucket 0), 40 and 41 (bucket 2).
        let (levels, settled) = levels(4, Column::Digital, Some(true), 64, |l| {
            for t in [3, 40, 41] {
                l.edge(t, t);
            }
        });
        assert!(settled.is_empty());
        assert_eq!(levels.len(), 1, "the strip alone");
        // Bucket 0 starts high and changes; 1 starts low, quiet; 2 starts low and changes
        // (low, high, low); 3 starts low, quiet.
        assert_eq!(
            digital(&levels[0]),
            [(true, true), (false, false), (false, true), (false, false)]
        );
    }

    #[test]
    fn a_gap_end_sets_its_bucket_and_the_level_from_there() {
        // Buckets of 16 ticks over 0..80. An edge at 5, a gap from 6 to 40 ending low, a settle
        // inside the gap, then an edge at 70.
        let (levels, _) = levels(4, Column::Digital, Some(false), 80, |l| {
            l.edge(5, 5);
            l.settle(20);
            l.gap(
                Gap {
                    from: 6,
                    to: 40,
                    level: false,
                },
                40,
            );
            l.edge(70, 70);
        });
        // 0: low, changes at 5. 1: high, carried through the gap. 2: high entering, the gap
        // ends low at 40. 3: low. 4: low, changes at 70.
        assert_eq!(
            digital(&levels[0]),
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
        let (d, _) = levels(4, Column::Digital, Some(false), length, |l| {
            for &t in &edges {
                l.edge(t, t);
            }
        });
        let (a, _) = levels(4, analog(AnalogCoding::F32), None, length, |l| {
            for (i, &t) in edges.iter().enumerate() {
                l.sample(t, Sample::F32(i as f32));
            }
        });
        for levels in [d, a] {
            // 2^4 has 4,096 buckets, in blocks; 2^8's 256 are the strip.
            let shifts: Vec<u8> = levels.iter().map(|l| l.shift).collect();
            assert_eq!(shifts, [4, 8]);
            assert_eq!(coarsen(&levels[0], 8, length), levels[1]);
        }
    }

    #[test]
    fn ladders_and_block_counts_follow_the_length() {
        // 120 s at 1 GS/s from 2^12: 2^28 is the first with at most 1,024 buckets (448).
        assert_eq!(ladder(120_000_000_000, 12), [12, 16, 20, 24, 28]);
        // 2^22 ticks fill exactly 1,024 buckets of 2^12, which is coarse enough.
        assert_eq!(ladder(1 << 22, 12), [12]);
        assert_eq!(ladder((1 << 22) + 1, 12), [12, 16]);
        // 2^30 ticks at 2^12 a bucket: 2^18 buckets, 64 blocks of 4,096.
        assert_eq!(block_count(1 << 30, 12), 64);
        assert_eq!(block_count((1 << 30) + 1, 12), 65);
        assert_eq!(block_buckets((1 << 30) + 1, 12, 64), (64 * 4096, 1));
    }

    #[test]
    fn round_trips() {
        let (d, _) = levels(4, Column::Digital, Some(true), 1000, |l| l.edge(17, 17));
        let column = analog(AnalogCoding::F32);
        let (a, _) = levels(4, column, None, 1000, |l| {
            l.sample(3, Sample::F32(1.5));
            l.sample(900, Sample::F32(-2.0));
        });
        for (level, column) in [(&d[0], Column::Digital), (&a[0], column)] {
            let bytes = level.to_bytes(column);
            assert_eq!(
                Level::parse(&bytes, 4, level.buckets, column).as_ref(),
                Some(level)
            );
            assert_eq!(Level::parse(&bytes[1..], 4, level.buckets, column), None);
        }
    }

    #[test]
    fn analog_buckets_hold_min_max_and_sum_and_an_empty_one_holds_min_above_max() {
        // Buckets of 16 ticks over 0..64. Samples at 1, 2 and 3 (bucket 0), none in bucket 1, 40
        // and 47 (bucket 2), NaN alone at 50 (bucket 3).
        let f = [
            (1, 1.5f32),
            (2, -2.0),
            (3, 4.25),
            (40, 0.5),
            (47, 0.25),
            (50, f32::NAN),
        ];
        let strip = |coding: AnalogCoding, samples: &[(i64, Sample)]| {
            let column = analog(coding);
            let (mut levels, _) = levels(4, column, None, 64, |l| {
                for &(t, s) in samples {
                    l.sample(t, s);
                }
            });
            let level = levels.pop().unwrap();
            // Stored and read back at the column's width, the same buckets.
            let bytes = level.to_bytes(column);
            assert_eq!(Level::parse(&bytes, 4, 4, column).as_ref(), Some(&level));
            level
        };
        let (inf, ninf) = (f32::INFINITY, f32::NEG_INFINITY);
        // By hand: 1.5 - 2 + 4.25 = 3.75; 0.5 + 0.25 = 0.75. An empty bucket and a bucket of NaN
        // alone hold +inf and -inf, and a sum of 0.
        let samples: Vec<(i64, Sample)> = f.iter().map(|&(t, v)| (t, Sample::F32(v))).collect();
        assert_eq!(
            buckets_of(columns::<f32>(&strip(AnalogCoding::F32, &samples))),
            [
                (-2.0, 4.25, 3.75),
                (inf, ninf, 0.0),
                (0.25, 0.5, 0.75),
                (inf, ninf, 0.0)
            ]
        );
        let samples: Vec<(i64, Sample)> = f
            .iter()
            .map(|&(t, v)| (t, Sample::F64(f64::from(v))))
            .collect();
        assert_eq!(
            buckets_of(columns::<f64>(&strip(AnalogCoding::F64, &samples))),
            [
                (-2.0, 4.25, 3.75),
                (f64::INFINITY, f64::NEG_INFINITY, 0.0),
                (0.25, 0.5, 0.75),
                (f64::INFINITY, f64::NEG_INFINITY, 0.0)
            ]
        );
        let int = |bits, signed| AnalogCoding::Int(IntCoding::new(bits, signed));
        // u64::MAX + 0 + 7, exactly, past what a u64 holds.
        let u: Vec<(i64, Sample)> = [(1, u64::MAX), (2, 0), (3, 7), (40, 1), (47, 2)]
            .into_iter()
            .map(|(t, v)| (t, Sample::U64(v)))
            .collect();
        assert_eq!(
            buckets_of(columns::<u64>(&strip(int(64, false), &u))),
            [
                (0, u64::MAX, i128::from(u64::MAX) + 7),
                (u64::MAX, 0, 0),
                (1, 2, 3),
                (u64::MAX, 0, 0)
            ]
        );
        // i64::MIN + i64::MAX - 1 = -2.
        let s: Vec<(i64, Sample)> = [(1, i64::MIN), (2, i64::MAX), (3, -1), (40, -5), (47, 5)]
            .into_iter()
            .map(|(t, v)| (t, Sample::I64(v)))
            .collect();
        assert_eq!(
            buckets_of(columns::<i64>(&strip(int(64, true), &s))),
            [
                (i64::MIN, i64::MAX, -2),
                (i64::MAX, i64::MIN, 0),
                (-5, 5, 0),
                (i64::MAX, i64::MIN, 0)
            ]
        );
        // At two bytes: an empty bucket's stored extremes, 0xFFFF and 0 or 0x7FFF and 0x8000,
        // read back as an empty bucket's, and a negative code sign-extended.
        let u: Vec<(i64, Sample)> = [(1, 65_535), (2, 0), (3, 7), (40, 1), (47, 2)]
            .into_iter()
            .map(|(t, v)| (t, Sample::U64(v)))
            .collect();
        assert_eq!(
            buckets_of(columns::<u64>(&strip(int(16, false), &u))),
            [
                (0, 65_535, 65_542),
                (u64::MAX, 0, 0),
                (1, 2, 3),
                (u64::MAX, 0, 0)
            ]
        );
        let s: Vec<(i64, Sample)> = [(1, -32_768), (2, 32_767), (3, -1), (40, -5), (47, 5)]
            .into_iter()
            .map(|(t, v)| (t, Sample::I64(v)))
            .collect();
        assert_eq!(
            buckets_of(columns::<i64>(&strip(int(16, true), &s))),
            [
                (-32_768, 32_767, -2),
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
        let column = analog(AnalogCoding::F64);
        let (levels_at_1, _) = levels(0, column, None, 1, |l| {
            for v in [1e16, 1.0, -1e16] {
                l.sample(0, Sample::F64(v));
            }
        });
        assert_eq!(columns::<f64>(&levels_at_1[0]).sum, [0.0]);

        // Coarse: buckets of 1 tick, of 16 and of 256. Children 0, 1, 2 hold 1e16, 1 and -1e16:
        // the coarse bucket adds them in that order, (1e16 + 1) - 1e16 = 0, not the samples'
        // exact 1. 20,000 ticks: 1,250 buckets of 16 is past 1,024, so 2^8 is the strip.
        let length = 20_000;
        let (levels, _) = levels(0, column, None, length, |l| {
            for (t, v) in [(0, 1e16), (1, 1.0), (2, -1e16)] {
                l.sample(t, Sample::F64(v));
            }
        });
        let shifts: Vec<u8> = levels.iter().map(|l| l.shift).collect();
        assert_eq!(shifts, [0, 4, 8]);
        assert_eq!(columns::<f64>(&levels[1]).sum[0], 0.0);
        assert_eq!(columns::<f64>(&levels[2]).sum[0], 0.0);
        // The same as folding the finer level, which `coarsen` does on its own.
        assert_eq!(coarsen(&levels[0], 4, length), levels[1]);
    }

    #[test]
    fn a_tie_between_minus_and_plus_zero_goes_to_the_first_and_a_sum_starts_at_plus_zero() {
        // Buckets of 1 tick and of 16. +0 then -0 at tick 0; -0 then +0 at tick 1; -0 alone at
        // tick 2; as f32 and as f64.
        let ticks = [(0, 0.0f32), (0, -0.0), (1, -0.0), (1, 0.0), (2, -0.0)];
        let sign = |v: f64| if v.is_sign_negative() { '-' } else { '+' };
        let (f, _) = levels(0, analog(AnalogCoding::F32), None, 20_000, |l| {
            for &(t, v) in &ticks {
                l.sample(t, Sample::F32(v));
            }
        });
        let (d, _) = levels(0, analog(AnalogCoding::F64), None, 20_000, |l| {
            for &(t, v) in &ticks {
                l.sample(t, Sample::F64(f64::from(v)));
            }
        });
        let fc = columns::<f32>(&f[0]);
        let dc = columns::<f64>(&d[0]);
        let signs = |v: &[f64]| v[..3].iter().map(|&v| sign(v)).collect::<String>();
        let widen = |v: &[f32]| v.iter().map(|&v| f64::from(v)).collect::<Vec<_>>();
        for (min, max, sum) in [
            (
                signs(&widen(&fc.min)),
                signs(&widen(&fc.max)),
                signs(&fc.sum),
            ),
            (signs(&dc.min), signs(&dc.max), signs(&dc.sum)),
        ] {
            // The first of each tie, and a sum from +0 never -0: +0 + -0 is +0.
            assert_eq!(
                (min.as_str(), max.as_str(), sum.as_str()),
                ("+--", "+--", "+++")
            );
        }
        // A coarse bucket folds its children in order: bucket 0's +0, then bucket 1's -0.
        let coarse = columns::<f32>(&f[1]);
        assert!(coarse.min[0].is_sign_positive() && coarse.max[0].is_sign_positive());
        // An empty bucket's sum is +0.
        assert!(fc.sum[5] == 0.0 && fc.sum[5].is_sign_positive());
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
        let (levels, _) = levels(17, analog(coding), None, 1 << 18, |l| {
            for t in 0..100_000 {
                l.sample(t, Sample::U64(u64::from(u32::MAX)));
            }
        });
        let c = columns::<u64>(&levels[0]);
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
        let coded = |name: &str, coding| DeclaredChannel {
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
            overview_shift: None,
        };
        let p = Derived {
            name: "P".into(),
            unit: "W".into(),
            product: ["I".into(), "V".into()],
            significant: None,
        };
        let channels = [coded("V", v), coded("I", i)];
        let new = || Pair::of(&channels, std::slice::from_ref(&p)).remove(0);
        let mut pair = new();
        let column = Column::of_product(pair.product);
        // V's samples first, then I's, as their blocks hand them out.
        let (levels, _) = levels(4, column, None, 16, |l| {
            for (c, t, code) in [(0, 0, 7), (0, 1, 11), (1, 0, 3), (1, 1, 5)] {
                if let Some(s) = pair.take(c, t, Sample::U64(code)).unwrap() {
                    l.sample(t, s);
                }
            }
        });
        assert_eq!(buckets_of(columns::<u64>(&levels[0])), [(21, 55, 76)]);

        // An operand sample whose partner sits at another tick, or never comes, is refused.
        let mut pair = new();
        pair.take(0, 0, Sample::U64(7)).unwrap();
        let err = pair.take(1, 1, Sample::U64(3)).unwrap_err();
        assert_eq!(
            err.to_string(),
            "derived channel P: V has a sample at tick 0 where I has one at 1"
        );
        let mut pair = new();
        pair.take(0, 0, Sample::U64(7)).unwrap();
        assert_eq!(
            pair.unpaired().unwrap().to_string(),
            "derived channel P: V has a sample at tick 0 that I lacks"
        );
    }

    /// Clusters of edges and samples with long spans of nothing between them, which the ladder
    /// fills in bulk: every level's buckets, the finest from its overview blocks, against
    /// buckets worked out from the items alone, and every coarser level against `coarsen` of
    /// those.
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
        // Settling at a few points between the items, as blocks would close.
        let settles = [5000i64, 2_000_000, 6_000_001, 7_000_100];
        let feed = |l: &mut Ladder, items: &mut dyn Iterator<Item = (i64, Option<f32>)>| {
            let mut next = 0;
            for (t, sample) in items {
                while settles.get(next).is_some_and(|&at| at <= t) {
                    l.settle(settles[next]);
                    next += 1;
                }
                match sample {
                    None => l.edge(t, t),
                    Some(v) => l.sample(t, Sample::F32(v)),
                }
            }
            for &at in &settles[next..] {
                l.settle(at);
            }
        };
        let column = analog(AnalogCoding::F32);
        let (d, dset) = levels(shift, Column::Digital, Some(true), length, |l| {
            feed(l, &mut edges.iter().map(|&t| (t, None)))
        });
        let (a, aset) = levels(shift, column, None, length, |l| {
            feed(l, &mut samples.iter().map(|&(t, v)| (t, Some(v))))
        });

        // The finest levels, by hand.
        let n = buckets(length, shift);
        let mut level_bits = vec![0u8; bit_bytes(n)];
        let mut change_bits = vec![0u8; bit_bytes(n)];
        let mut want_a = Columns::<f32>::new();
        let mut level = true;
        let (mut e, mut s) = (0, 0);
        for i in 0..n {
            let end = ((i + 1) << shift) as i64;
            if level {
                set(&mut level_bits, i);
            }
            while edges.get(e).is_some_and(|&t| t < end) {
                level = !level;
                set(&mut change_bits, i);
                e += 1;
            }
            let mut bucket = Bucket::<f32>::EMPTY;
            while samples.get(s).is_some_and(|&(t, _)| t < end) {
                bucket.take(samples[s].1);
                s += 1;
            }
            want_a.push(bucket);
        }
        let want_d = Level {
            shift,
            buckets: n,
            data: ChannelOverview::Digital {
                level: level_bits,
                change: change_bits,
            },
        };
        let want_a = Level {
            shift,
            buckets: n,
            data: ChannelOverview::Analog(AnalogOverview::F32(want_a)),
        };
        for (got, want, settled) in [(&d, &want_d, &dset), (&a, &want_a, &aset)] {
            assert_eq!(difference(&got[0], want), None, "the finest level");
            // The blocks of nothing join runs: far fewer records than blocks.
            let finest = settled.iter().filter(|s| s.li == 0);
            assert_eq!(finest.clone().map(|s| u64::from(s.n)).sum::<u64>(), 512);
            let records = finest.count();
            assert!(records < 30, "{records} records for 512 blocks");
            for coarse in &got[1..] {
                let whole = coarsen(want, coarse.shift, length);
                assert_eq!(difference(coarse, &whole), None, "level {}", coarse.shift);
            }
        }
    }
}
