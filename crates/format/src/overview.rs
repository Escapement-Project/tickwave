//! The overview: each channel summarised in buckets of 2^shift ticks, so a wide view draws without
//! fetching blocks. A digital bucket holds the level at its start and whether the level changes
//! inside it. An analog bucket holds the least and greatest value in it.
//!
//! Decoded layout, for each level in the header's order and each channel within it:
//! - digital: the level bits, then the change bits, each `ceil(n / 8)` bytes, bucket i in bit
//!   `i % 8` of byte `i / 8`;
//! - analog: n minimums, then n maximums, as f32. A bucket with no sample holds +inf and -inf.

/// The finest bucket considered, and the step between levels, as shifts.
pub const FINEST: u8 = 12;
pub const STEP: u8 = 4;
/// A level goes in the head only if it has at most this many buckets.
pub const HEAD_BUCKETS: u64 = 1 << 17;
/// No coarser level is needed once one has this few buckets.
pub const WHOLE_VIEW: u64 = 1024;

pub fn buckets(length: i64, shift: u8) -> u64 {
    (length.max(0) as u64 >> shift) + 1
}

/// Buckets in one overview block, for a level kept outside the head.
pub const BLOCK_BUCKETS: u64 = 1 << 16;
/// A level too large for the head is kept in blocks if it has at most this many buckets; a finer
/// one is left out.
pub const BLOCK_LEVEL_BUCKETS: u64 = 1 << 26;

/// Which levels a recording keeps, as shifts, finest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// In the head, fetched with it.
    pub head: Vec<u8>,
    /// In overview blocks after the data blocks, fetched as a view needs them.
    pub blocks: Vec<u8>,
}

impl Plan {
    /// The finest level kept anywhere: the one the others are derived from.
    pub fn finest(&self) -> u8 {
        self.blocks
            .first()
            .or(self.head.first())
            .copied()
            .unwrap_or(FINEST)
    }
}

pub fn plan(length: i64) -> Plan {
    let mut plan = Plan {
        head: Vec::new(),
        blocks: Vec::new(),
    };
    let mut shift = FINEST;
    loop {
        let n = buckets(length, shift);
        if n <= HEAD_BUCKETS {
            plan.head.push(shift);
        } else if n <= BLOCK_LEVEL_BUCKETS {
            plan.blocks.push(shift);
        }
        if n <= WHOLE_VIEW || shift + STEP > 62 {
            return plan;
        }
        shift += STEP;
    }
}

/// The number of overview blocks a level kept outside the head is cut into.
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

pub struct DigitalBuilder {
    shift: u8,
    buckets: u64,
    level: Vec<u8>,
    change: Vec<u8>,
    filled: u64,
    current: bool,
}

impl DigitalBuilder {
    pub fn new(length: i64, shift: u8, initial: bool) -> Self {
        let buckets = buckets(length, shift);
        Self {
            shift,
            buckets,
            level: vec![0; bit_bytes(buckets)],
            change: vec![0; bit_bytes(buckets)],
            filled: 0,
            current: initial,
        }
    }

    fn fill_to(&mut self, last: u64) {
        while self.filled <= last.min(self.buckets - 1) {
            if self.current {
                set(&mut self.level, self.filled);
            }
            self.filled += 1;
        }
    }

    /// `tick` lies in `0..=length` and follows the previous edge.
    pub fn edge(&mut self, tick: i64) {
        let b = tick as u64 >> self.shift;
        // Buckets up to b start before this edge, at the level before it.
        self.fill_to(b);
        set(&mut self.change, b);
        self.current = !self.current;
    }

    pub fn finish(mut self) -> ChannelOverview {
        self.fill_to(self.buckets - 1);
        ChannelOverview::Digital {
            level: self.level,
            change: self.change,
        }
    }
}

pub struct AnalogBuilder {
    shift: u8,
    min: Vec<f32>,
    max: Vec<f32>,
}

impl AnalogBuilder {
    pub fn new(length: i64, shift: u8) -> Self {
        let n = buckets(length, shift) as usize;
        Self {
            shift,
            min: vec![f32::INFINITY; n],
            max: vec![f32::NEG_INFINITY; n],
        }
    }

    /// A NaN value leaves its bucket as it was.
    pub fn sample(&mut self, tick: i64, value: f32) {
        let b = (tick as u64 >> self.shift) as usize;
        self.min[b] = self.min[b].min(value);
        self.max[b] = self.max[b].max(value);
    }

    pub fn finish(self) -> ChannelOverview {
        ChannelOverview::Analog {
            min: self.min,
            max: self.max,
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
pub fn decoded_len(n: u64, analog: &[bool]) -> usize {
    analog
        .iter()
        .map(|&a| if a { 8 * n as usize } else { 2 * bit_bytes(n) })
        .sum()
}

/// Parses the head's decoded overview. `analog[c]` says whether channel c is analog. None if
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digital_buckets_by_hand() {
        // Buckets of 16 ticks over 0..=63: edges at 3 (in bucket 0), 40 and 41 (bucket 2).
        let mut b = DigitalBuilder::new(63, 4, true);
        for t in [3, 40, 41] {
            b.edge(t);
        }
        let c = b.finish();
        let got: Vec<_> = (0..4).map(|i| c.digital(i).unwrap()).collect();
        // Bucket 0 starts high and changes; 1 starts low, quiet; 2 starts low and changes
        // (low, high, low); 3 starts low, quiet.
        assert_eq!(
            got,
            [(true, true), (false, false), (false, true), (false, false)]
        );
    }

    #[test]
    fn coarsening_matches_building_coarse() {
        let edges = [5i64, 70, 71, 300, 1000, 1001, 1002, 4095];
        let length = 4095;
        let mut fine = DigitalBuilder::new(length, 4, false);
        let mut coarse = DigitalBuilder::new(length, 8, false);
        for &t in &edges {
            fine.edge(t);
            coarse.edge(t);
        }
        let fine = Level {
            shift: 4,
            buckets: buckets(length, 4),
            channels: vec![fine.finish()],
        };
        let derived = coarsen(&fine, 8, length);
        assert_eq!(derived.channels[0], coarse.finish());
    }

    #[test]
    fn plans_split_head_and_blocks() {
        // 120 s at 1 GS/s: 2^12 (29.3M buckets) and 2^16 (1.8M) are too many for the head, few
        // enough for blocks; 2^28 is the first with at most 1,024 (448).
        let p = plan(120_000_000_000);
        assert_eq!(
            (p.head.as_slice(), p.blocks.as_slice()),
            (&[20, 24, 28][..], &[12, 16][..])
        );
        assert_eq!(p.finest(), 12);
        // 1 ms at 1 GS/s: one level in the head covers it all.
        assert_eq!(
            plan(1_000_000),
            Plan {
                head: vec![12],
                blocks: vec![]
            }
        );
        // 2^30 ticks: 2^12 has 262,145 buckets, so five blocks, the last holding one bucket.
        assert_eq!(block_count(1 << 30, 12), 5);
        assert_eq!(block_buckets(1 << 30, 12, 4), (4 * 65536, 1));
        // 3,600 s at 1 ps a tick: 2^12 and 2^16 would pass 2^26 buckets, so they are left out.
        let p = plan(3_600_000_000_000_000);
        assert!(
            p.blocks
                .iter()
                .all(|&s| buckets(3_600_000_000_000_000, s) <= BLOCK_LEVEL_BUCKETS)
        );
        assert!(!p.blocks.contains(&12) && !p.blocks.contains(&16));
    }

    #[test]
    fn round_trips() {
        let mut d = DigitalBuilder::new(1000, 4, true);
        d.edge(17);
        let mut a = AnalogBuilder::new(1000, 4);
        a.sample(3, 1.5);
        a.sample(900, -2.0);
        let levels = vec![Level {
            shift: 4,
            buckets: buckets(1000, 4),
            channels: vec![d.finish(), a.finish()],
        }];
        let mut bytes = Vec::new();
        write(&levels, &mut bytes);
        assert_eq!(parse(&bytes, &[4], 1000, &[false, true]), Some(levels));
        assert_eq!(parse(&bytes[1..], &[4], 1000, &[false, true]), None);
    }
}
