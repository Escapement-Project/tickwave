//! The content id: a hash of what a recording holds, not of how a file stores it. Recompressing,
//! re-cutting blocks or bundling annotations leaves it unchanged; a sidecar pins it.
//!
//! BLAKE3 over: the timebase (two u64), the length (i64), then each channel's own BLAKE3 digest,
//! then each derived channel's. A channel's digest covers what its declaration fixes, then its
//! items, then what only the end settles, so a writer computes it as the items arrive. A
//! channel's gaps or breaks go into a digest of their own, which the channel's takes with their
//! count at its end, so a writer holds none of them to hash them. Every tick in it is a source
//! tick. All little-endian; strings as a u64 length and UTF-8 bytes. A moved or trimmed channel,
//! every channel of a recording of several sources, and a channel whose source does not map its
//! ticks to the axis unchanged, adds what it states between its declaration and its items.

use crate::header::{AnalogCoding, ContentId, Declaration, DeclaredKind, Derived, Shaping};
use crate::model::{Break, Gap, Ratio, Sample};

const BUFFER: usize = 1 << 16;

#[derive(Clone)]
pub struct ChannelHasher {
    hasher: blake3::Hasher,
    buffer: Vec<u8>,
}

fn put_str(hasher: &mut blake3::Hasher, s: &str) {
    hasher.update(&(s.len() as u64).to_le_bytes());
    hasher.update(s.as_bytes());
}

/// Where a channel's source lies on the axis, as its digest pins it: the source's `m` and
/// `stretch` in lowest terms, its `origin`, and its `align`, 0 where it states none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placed {
    pub m: Ratio,
    pub origin: i64,
    pub align: i64,
    pub stretch: Ratio,
}

impl Placed {
    /// Channel `c`'s source's placement, where flag 4 calls for it: the recording has several
    /// sources, or the source's `m` is not 1 or its `origin` not 0. None where it doesn't, and
    /// for a declaration whose source has no `m`.
    pub fn of(d: &Declaration, c: usize) -> Option<Option<Self>> {
        let s = d.channels.get(c)?.source as usize;
        let source = d.sources.get(s)?;
        let m = d.m(s)?;
        let one = m.num() == m.den();
        if d.sources.len() == 1 && one && source.origin == 0 {
            return Some(None);
        }
        Some(Some(Self {
            m,
            origin: source.origin,
            align: source.align.unwrap_or(0),
            stretch: lowest(source.stretch.unwrap_or(Ratio::ONE)),
        }))
    }
}

fn lowest(r: Ratio) -> Ratio {
    let (mut a, mut b) = (r.num(), r.den());
    while b != 0 {
        (a, b) = (b, a % b);
    }
    Ratio::new(r.num() / a, r.den() / a).unwrap_or(r)
}

/// A byte of flags, 1 for an offset, 2 for a kept span and 4 for a source placed on the axis,
/// then each one stated. Nothing for a channel with no flag, so its digest is the one its data
/// alone gives.
fn put_shaping(hasher: &mut blake3::Hasher, shaping: &Shaping, placed: Option<Placed>) {
    let flags = u8::from(shaping.offset.is_some())
        | u8::from(shaping.kept.is_some()) << 1
        | u8::from(placed.is_some()) << 2;
    if flags == 0 {
        return;
    }
    hasher.update(&[flags]);
    if let Some(offset) = &shaping.offset {
        hasher.update(&offset.ticks.to_le_bytes());
        put_str(hasher, &offset.kind);
    }
    if let Some(kept) = shaping.kept {
        hasher.update(&kept.from.to_le_bytes());
        hasher.update(&kept.to.to_le_bytes());
    }
    if let Some(p) = placed {
        hasher.update(&p.m.num().to_le_bytes());
        hasher.update(&p.m.den().to_le_bytes());
        hasher.update(&p.origin.to_le_bytes());
        hasher.update(&p.align.to_le_bytes());
        hasher.update(&p.stretch.num().to_le_bytes());
        hasher.update(&p.stretch.den().to_le_bytes());
    }
}

impl ChannelHasher {
    pub fn digital(name: &str, initial: bool, shaping: &Shaping, placed: Option<Placed>) -> Self {
        let mut hasher = blake3::Hasher::new();
        put_str(&mut hasher, name);
        put_str(&mut hasher, "digital");
        hasher.update(&[u8::from(initial)]);
        put_shaping(&mut hasher, shaping, placed);
        Self::from(hasher)
    }

    /// An `f32` and a `codes` channel add nothing for their coding, so the two codings of one
    /// capture's values give one digest. The period and the phase go in lowest terms, the
    /// phase as 0 and 1 where there is none.
    pub fn analog(
        name: &str,
        unit: &str,
        period: Ratio,
        phase: Option<Ratio>,
        coding: AnalogCoding,
        shaping: &Shaping,
        placed: Option<Placed>,
    ) -> Self {
        let mut hasher = blake3::Hasher::new();
        put_str(&mut hasher, name);
        put_str(&mut hasher, "analog");
        put_str(&mut hasher, unit);
        let period = lowest(period);
        hasher.update(&period.num().to_le_bytes());
        hasher.update(&period.den().to_le_bytes());
        let (num, den) = phase.map_or((0, 1), |f| {
            let f = lowest(f);
            (f.num(), f.den())
        });
        hasher.update(&num.to_le_bytes());
        hasher.update(&den.to_le_bytes());
        match coding {
            AnalogCoding::F32 | AnalogCoding::Codes { .. } => {}
            AnalogCoding::F64 => put_str(&mut hasher, "f64"),
            AnalogCoding::Int(int) => {
                put_str(&mut hasher, "int");
                hasher.update(&[int.bits, u8::from(int.signed)]);
                for ratio in [int.scale(), int.offset()] {
                    hasher.update(&ratio.num().to_le_bytes());
                    hasher.update(&(ratio.den() as u64).to_le_bytes());
                }
            }
        }
        put_shaping(&mut hasher, shaping, placed);
        Self::from(hasher)
    }

    /// The hasher for channel `c` of a checked declaration; None for no such channel, or one
    /// whose source has no `m`, which a checked declaration has neither of.
    pub fn declared(d: &Declaration, c: usize) -> Option<Self> {
        let channel = d.channels.get(c)?;
        let placed = Placed::of(d, c)?;
        Some(match &channel.data {
            DeclaredKind::Digital { initial } => {
                Self::digital(&channel.name, *initial, &channel.shaping, placed)
            }
            DeclaredKind::Analog {
                unit,
                period,
                coding,
                phase,
                ..
            } => Self::analog(
                &channel.name,
                unit,
                *period,
                *phase,
                *coding,
                &channel.shaping,
                placed,
            ),
        })
    }

    fn from(hasher: blake3::Hasher) -> Self {
        Self {
            hasher,
            buffer: Vec::with_capacity(BUFFER),
        }
    }

    fn put(&mut self, bytes: &[u8]) {
        if self.buffer.len() + bytes.len() > BUFFER {
            self.hasher.update(&self.buffer);
            self.buffer.clear();
        }
        self.buffer.extend_from_slice(bytes);
    }

    pub fn edge(&mut self, tick: i64) {
        self.put(&tick.to_le_bytes());
    }

    /// An f32 value's bits as u32, an f64's as u64, a code as an i64 or a u64.
    #[inline]
    pub fn sample(&mut self, sample: Sample) {
        match sample {
            Sample::F32(v) => self.put(&v.to_bits().to_le_bytes()),
            Sample::F64(v) => self.put(&v.to_bits().to_le_bytes()),
            Sample::U64(c) => self.put(&c.to_le_bytes()),
            Sample::I64(c) => self.put(&c.to_le_bytes()),
        }
    }

    fn items_done(&mut self) -> &mut blake3::Hasher {
        self.hasher.update(&self.buffer);
        self.buffer.clear();
        &mut self.hasher
    }

    /// `gaps` is the channel's gaps as the header lists them, hashed by a `ListHasher`.
    pub fn finish_digital(mut self, end: i64, gaps: Listed) -> blake3::Hash {
        let h = self.items_done();
        h.update(&end.to_le_bytes());
        h.update(&gaps.count.to_le_bytes());
        h.update(gaps.digest.as_bytes());
        h.finalize()
    }

    /// `breaks` is the channel's breaks as the header lists them, hashed by a `ListHasher`.
    /// `lost` is the samples lost before the first and after the last: both follow the breaks'
    /// digest where either is stated, so a channel that states neither has the id its data alone
    /// gives.
    pub fn finish_analog(
        mut self,
        first: i64,
        count: u64,
        breaks: Listed,
        lost: (u64, u64),
    ) -> blake3::Hash {
        let h = self.items_done();
        h.update(&first.to_le_bytes());
        h.update(&count.to_le_bytes());
        h.update(&breaks.count.to_le_bytes());
        h.update(breaks.digest.as_bytes());
        if lost != (0, 0) {
            h.update(&lost.0.to_le_bytes());
            h.update(&lost.1.to_le_bytes());
        }
        h.finalize()
    }
}

/// A gap or break list's count and digest, as a channel's digest takes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Listed {
    pub count: u64,
    pub digest: blake3::Hash,
}

/// Hashes a channel's gaps or breaks as they come, in the header's order, holding none.
#[derive(Clone, Debug, Default)]
pub struct ListHasher {
    hasher: blake3::Hasher,
    count: u64,
}

impl ListHasher {
    pub fn new() -> Self {
        Self::default()
    }

    /// A gap: `from` and `to` as i64, its level as one byte.
    pub fn gap(&mut self, g: Gap) {
        self.hasher.update(&g.from.to_le_bytes());
        self.hasher.update(&g.to.to_le_bytes());
        self.hasher.update(&[u8::from(g.level)]);
        self.count += 1;
    }

    /// A break: `index` as u64, `tick` as i64, `lost` as u64, `phase` as u32.
    pub fn brk(&mut self, b: Break) {
        self.hasher.update(&b.index.to_le_bytes());
        self.hasher.update(&b.tick.to_le_bytes());
        self.hasher.update(&b.lost.to_le_bytes());
        self.hasher.update(&b.phase.to_le_bytes());
        self.count += 1;
    }

    pub fn count(&self) -> u64 {
        self.count
    }

    pub fn finish(&self) -> Listed {
        Listed {
            count: self.count,
            digest: self.hasher.finalize(),
        }
    }
}

/// A whole list of gaps, hashed.
pub fn gaps_listed(gaps: &[Gap]) -> Listed {
    let mut h = ListHasher::new();
    for &g in gaps {
        h.gap(g);
    }
    h.finish()
}

/// A whole list of breaks, hashed.
pub fn breaks_listed(breaks: &[Break]) -> Listed {
    let mut h = ListHasher::new();
    for &b in breaks {
        h.brk(b);
    }
    h.finish()
}

/// A derived channel's digest: its name, unit and operation, its operands' count and names in
/// order, then its `params`' count and each key and value in the byte order of the keys. All are
/// strings, so it is the same whether or not a reader knows the operation.
pub fn derived_digest(d: &Derived) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new();
    put_str(&mut hasher, &d.name);
    put_str(&mut hasher, &d.unit);
    put_str(&mut hasher, d.op.as_str());
    hasher.update(&(d.operands.len() as u64).to_le_bytes());
    for o in &d.operands {
        put_str(&mut hasher, o);
    }
    hasher.update(&(d.params.len() as u64).to_le_bytes());
    for (k, v) in &d.params {
        put_str(&mut hasher, k);
        put_str(&mut hasher, v);
    }
    hasher.finalize()
}

/// The content id from the timebase, the length, and each channel's digest followed by each
/// derived channel's.
pub fn content_id(timebase: Ratio, length: i64, channels: &[blake3::Hash]) -> ContentId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&timebase.num().to_le_bytes());
    hasher.update(&timebase.den().to_le_bytes());
    hasher.update(&length.to_le_bytes());
    for digest in channels {
        hasher.update(digest.as_bytes());
    }
    ContentId::new(hasher.finalize())
}
