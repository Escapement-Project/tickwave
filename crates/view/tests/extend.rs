//! A followed window's columns at fixed ticks, and its answer extended from the one before it:
//! held against a fresh query of the same recording, field for field, after every snapshot of a
//! writer driven at random.

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use escapement_tickwave_format::decode::from_memory;
use escapement_tickwave_format::header::{
    AnalogCoding, Declaration, DeclaredChannel, DeclaredKind, Derived, Exact, IntCoding,
    SourceFormat, SourceHeader,
};
use escapement_tickwave_format::model::{Gap, Kept, Ratio, Shaping};
use escapement_tickwave_format::snapshot::Snapshot;
use escapement_tickwave_format::writer::{Compression, Finish, WriteOptions, Writer};
use escapement_tickwave_view::{Answer, Bytes, Cache, Drawn, MarkerQuery, Query, Recording};

/// A file a writer still appends to, which records where each read starts.
#[derive(Default)]
struct Growing {
    bytes: Mutex<Vec<u8>>,
    reads: Mutex<Vec<u64>>,
}

impl Bytes for Growing {
    fn size(&self) -> std::io::Result<u64> {
        Ok(self.bytes.lock().unwrap().len() as u64)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        self.reads.lock().unwrap().push(offset);
        let file = self.bytes.lock().unwrap();
        let at = offset as usize;
        buf.copy_from_slice(&file[at..at + buf.len()]);
        Ok(())
    }
}

/// xorshift, so a failure repeats from its seed.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        let x = &mut self.0;
        *x ^= *x << 13;
        *x ^= *x >> 7;
        *x ^= *x << 17;
        *x
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    fn one_in(&mut self, n: u64) -> bool {
        self.below(n) == 0
    }
}

const V: usize = 0;
const I: usize = 1;
const F: usize = 2;
const D0: usize = 3;
const D1: usize = 4;
const P: usize = 5;
/// V and I's period, in source 0's ticks, which are the axis's.
const PAIR: i64 = 10;
/// F's period in source 1's ticks, each 5/2 of the axis's.
const STEP: i64 = 3;

/// Source 1's tick `k` on the axis: 1,000 Hz over 400 Hz.
fn axis1(k: i64) -> i64 {
    (5 * k).div_euclid(2)
}

/// The first source 1 tick at or after axis tick `t`.
fn first1(t: i64) -> i64 {
    -(-2 * t).div_euclid(5)
}

/// V and I at a pair every 10 ticks with P their product, F on a second source whose ticks are
/// 5/2 of the axis's, D0 low at first on the first source and D1 high at first on the second;
/// overview buckets from 2^`shift` ticks. Two sources, so every channel states the span it
/// keeps: to axis tick `to`.
fn declaration(shift: u8, to: i64) -> Declaration {
    let axis = Ratio::integer(1000).unwrap();
    let int = |den| {
        AnalogCoding::Int(IntCoding {
            scale: Exact::new(1, den),
            ..IntCoding::new(32, false)
        })
    };
    // Source 1's last tick at or before `to`.
    let kept = |source: u32| Shaping {
        offset: None,
        kept: Some(Kept {
            from: 0,
            to: if source == 0 { to } else { 2 * to / 5 },
        }),
    };
    let analog = |name: &str, source: u32, period: i64, coding| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source,
        data: DeclaredKind::Analog {
            unit: "V".into(),
            period: Ratio::integer(period as u64).unwrap(),
            coding,
            significant: Some(4),
        },
        shaping: kept(source),
        overview_shift: None,
    };
    let digital = |name: &str, source: u32, initial| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source,
        data: DeclaredKind::Digital { initial },
        shaping: kept(source),
        overview_shift: None,
    };
    Declaration {
        timebase: axis,
        channels: vec![
            analog("V", 0, PAIR, int(1000)),
            analog("I", 0, PAIR, int(1_000_000_000)),
            analog("F", 1, STEP, AnalogCoding::F32),
            digital("D0", 0, false),
            digital("D1", 1, true),
        ],
        derived: vec![Derived {
            name: "P".into(),
            unit: "W".into(),
            product: ["I".into(), "V".into()],
            significant: None,
        }],
        sources: vec![
            SourceHeader::new(SourceFormat::Other("a".into()), axis),
            SourceHeader::new(
                SourceFormat::Other("b".into()),
                Ratio::integer(400).unwrap(),
            ),
        ],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: shift,
    }
}

/// A writer driven in random batches: pairs and F's samples in runs that pause and lose
/// samples, edges and gaps on both digital channels, flushes, and marks.
struct Recorder {
    w: Writer,
    file: Arc<Growing>,
    r: Random,
    /// No item may come before this axis tick: the latest one's, or the last flush's.
    floor: i64,
    /// The next pair's tick, and F's next sample's source tick; None before their first run.
    pair: Option<i64>,
    step: Option<i64>,
    /// Each digital channel's least source tick for its next edge.
    edge: [i64; 2],
    /// Each digital channel has had an edge or a gap.
    begun: [bool; 2],
    pairs: u64,
    fs: u64,
}

impl Recorder {
    fn new(seed: u64, to: i64) -> Self {
        let mut r = Random(seed);
        let options = WriteOptions {
            level: 1,
            block_bytes: 32 + r.below(400) as usize,
            compression: if r.below(2) == 0 {
                Compression::Spread
            } else {
                Compression::Offload
            },
            promise_with_items: true,
        };
        let mut w = Writer::new(declaration(3, to), options).unwrap();
        let file = Arc::new(Growing::default());
        file.bytes.lock().unwrap().extend(w.take_output());
        Self {
            w,
            file,
            r,
            floor: 0,
            pair: None,
            step: None,
            edge: [0; 2],
            begun: [false; 2],
            pairs: 0,
            fs: 0,
        }
    }

    /// Maybe an edge of digital channel `d` at an axis tick from the floor to `to`.
    fn edge(&mut self, d: usize, to: i64) {
        if !self.r.one_in(3) {
            return;
        }
        let (c, at) = match d {
            0 => (D0, self.edge[0].max(self.floor)),
            _ => (D1, self.edge[1].max(first1(self.floor))),
        };
        let axis = |k| if d == 0 { k } else { axis1(k) };
        let span = (to - axis(at)).max(0) as u64;
        let mut k = at + self.r.below(span + 1) as i64;
        while k > at && axis(k) > to {
            k -= 1;
        }
        if axis(k) < self.floor || axis(k) > to {
            return;
        }
        self.w.edge(c, k).unwrap();
        self.floor = self.floor.max(axis(k));
        self.edge[d] = k;
        self.begun[d] = true;
    }

    /// Maybe a gap of digital channel `d`, from past its last edge, some ticks long.
    fn gap(&mut self, d: usize) {
        if !self.r.one_in(6) {
            return;
        }
        let c = if d == 0 { D0 } else { D1 };
        // D0 is low at first, so a gap may start it; D1, high at first, may not.
        let from = match (d, self.begun[d]) {
            (0, false) if self.r.one_in(2) => 0,
            _ => self.edge[d] + 1 + self.r.below(20) as i64,
        };
        let to = from + 1 + self.r.below(80) as i64;
        let level = self.r.one_in(2);
        if self.w.gap(c, Gap { from, to, level }).is_ok() {
            self.edge[d] = to + 1;
            self.begun[d] = true;
        }
    }

    /// One batch: up to 12 ticks of items or up to 150, then a snapshot.
    fn batch(&mut self) -> Snapshot {
        let most = if self.r.one_in(2) { 12 } else { 150 };
        let until = self.floor + 1 + self.r.below(most) as i64;
        let pairs = !self.r.one_in(5);
        let fs = !self.r.one_in(4);
        for d in 0..2 {
            self.gap(d);
        }
        // A run starts where none has, where a pause left the floor past the next place, and
        // now and then of itself: a pause, or samples lost.
        if pairs && (self.pair.is_none_or(|p| p < self.floor) || self.r.one_in(12)) {
            let at = self.pair.unwrap_or(0).max(self.floor) + self.r.below(40) as i64;
            let lost = if self.pairs == 0 { 0 } else { self.r.below(3) };
            self.w.run(V, at, lost).unwrap();
            self.w.run(I, at, lost).unwrap();
            self.pair = Some(at);
        }
        if fs && (self.step.is_none_or(|s| axis1(s) < self.floor) || self.r.one_in(12)) {
            let at = self.step.unwrap_or(0).max(first1(self.floor)) + self.r.below(15) as i64;
            let lost = if self.fs == 0 { 0 } else { self.r.below(3) };
            self.w.run(F, at, lost).unwrap();
            self.step = Some(at);
        }
        loop {
            let next_pair = self.pair.filter(|_| pairs);
            let next_f = self.step.filter(|_| fs).map(axis1);
            let next = match (next_pair, next_f) {
                (Some(a), Some(b)) => a.min(b),
                (Some(a), None) | (None, Some(a)) => a,
                (None, None) => until,
            };
            let next = next.min(until);
            self.edge(0, next);
            self.edge(1, next);
            if next >= until {
                break;
            }
            if next_pair == Some(next) {
                let n = self.pairs;
                self.w.sample(V, 3300 + n % 7).unwrap();
                self.w.sample(I, 1_000_000 + (n * 7919) % 50_000).unwrap();
                self.pairs += 1;
                self.pair = Some(next + PAIR);
            } else {
                let k = self.step.unwrap();
                let x = (self.r.below(2000) as f32 - 1000.0) / 8.0;
                self.w.sample(F, x).unwrap();
                self.fs += 1;
                self.step = Some(k + STEP);
            }
            self.floor = self.floor.max(next);
        }
        if self.r.one_in(10) {
            let start = (self.floor - self.r.below(40) as i64).max(0);
            let serde_json::Value::Object(m) = serde_json::json!({
                "id": format!("mark/{start}/{}", self.r.next()),
                "kind": "marker",
                "start": start,
                "end": start + self.r.below(30) as i64,
            }) else {
                unreachable!("an object")
            };
            self.w.annotation("test", m).unwrap();
        }
        // A flush at the floor or after it, by no more than any next item allows.
        if self.r.one_in(4) {
            let next = [self.pair, self.step.map(axis1)]
                .into_iter()
                .flatten()
                .filter(|&t| t > self.floor)
                .min()
                .unwrap_or(self.floor + 50);
            let before = self.floor + 1 + self.r.below((next - self.floor) as u64) as i64;
            if self.w.flush(before).is_ok() {
                self.floor = before;
            }
        }
        let snapshot = self.w.snapshot();
        self.file.bytes.lock().unwrap().extend(self.w.take_output());
        snapshot
    }
}

/// A followed window `width` ticks wide, as a viewer resolves one against the extent and a
/// now `ahead` of it: ending at the later, starting no earlier than the extent.
fn followed(extent: &Range<i64>, width: i64, ahead: i64) -> Range<i64> {
    let end = extent.end + ahead;
    let start = (end - width).max(extent.start);
    start..end.max(start + 1)
}

fn query(window: Range<i64>, columns: u32, strip: Option<(usize, u32)>) -> Query {
    Query {
        statistics: vec![window.clone(), 100..300],
        window,
        columns,
        channels: vec![I, D0, F, D1, P, V],
        flags: vec![(I, D0), (F, D1)],
        strip,
        markers: MarkerQuery {
            limit: 3,
            list: Some((None, 2)),
            successor_of: None,
        },
        aligned: true,
    }
}

/// `got` and `want` the same, field for field, each column's floats to the bit.
fn same(got: &Answer, want: &Answer, at: &str) {
    assert_eq!(
        (&got.window, &got.around, &got.grid),
        (&want.window, &want.around, &want.grid),
        "{at}: the layout"
    );
    assert_eq!(got.channels.len(), want.channels.len(), "{at}");
    for (i, (x, y)) in got.channels.iter().zip(&want.channels).enumerate() {
        let at = format!("{at}, channel {i} of the query");
        match (x, y) {
            (
                Drawn::Analog {
                    columns: a,
                    samples: s,
                    lost: l,
                    fit: f,
                },
                Drawn::Analog {
                    columns: b,
                    samples: t,
                    lost: m,
                    fit: g,
                },
            ) => {
                let column = |c: &escapement_tickwave_view::AnalogColumns, k: usize| {
                    (
                        c.min[k].to_bits(),
                        c.max[k].to_bits(),
                        c.joined[k],
                        c.pale[k],
                    )
                };
                assert_eq!(a.min.len(), b.min.len(), "{at}");
                for k in 0..a.min.len() {
                    assert_eq!(
                        column(a, k),
                        column(b, k),
                        "{at}, column {k}: got {}..{} joined {} pale {}, want {}..{} joined {} pale {}",
                        a.min[k],
                        a.max[k],
                        a.joined[k],
                        a.pale[k],
                        b.min[k],
                        b.max[k],
                        b.joined[k],
                        b.pale[k]
                    );
                }
                assert_eq!(s, t, "{at}: samples");
                assert_eq!(l, m, "{at}: lost");
                assert_eq!(f, g, "{at}: fit");
            }
            (
                Drawn::Digital {
                    columns: a,
                    gaps: g,
                },
                Drawn::Digital {
                    columns: b,
                    gaps: h,
                },
            ) => {
                assert_eq!(a.state.len(), b.state.len(), "{at}");
                for k in 0..a.state.len() {
                    assert_eq!(
                        (a.state[k], a.at[k].to_bits()),
                        (b.state[k], b.at[k].to_bits()),
                        "{at}, column {k}"
                    );
                }
                assert_eq!(g, h, "{at}: gaps");
            }
            _ => panic!("{at}: different kinds"),
        }
    }
    assert_eq!(got.strip, want.strip, "{at}: strip");
    assert_eq!(got.statistics, want.statistics, "{at}: statistics");
    assert_eq!(got.markers, want.markers, "{at}: markers");
    assert_eq!(
        (&got.extent, got.revision),
        (&want.extent, want.revision),
        "{at}"
    );
    // Whatever the fields above leave out.
    assert_eq!(got, want, "{at}");
}

#[test]
fn an_aligned_grid_has_whole_columns_from_tick_0() {
    // Worked by hand, and in live-tail-2026-11-14/scripts/grid.py: (width, columns, end) to
    // the window, `around`, and the grid's (x0, per, count).
    type Row = ((i64, u32, i64), Range<i64>, Range<i64>, (f64, f64, usize));
    let rows: [Row; 7] = [
        // 15 ticks a column; the end moved on to 67 × 15; `around` from 7 × 7 columns back.
        ((100, 7, 1000), 905..1005, 735..1110, (735.0, 15.0, 25)),
        // A tick later, inside the same column: the same layout.
        ((100, 7, 1001), 905..1005, 735..1110, (735.0, 15.0, 25)),
        // Into the next column: one column more, `around` from the same tick.
        ((100, 7, 1006), 920..1020, 735..1125, (735.0, 15.0, 26)),
        // A width a whole number of columns, at a column's end: what the plain grid gives too.
        (
            (1000, 10, 5000),
            4000..5000,
            3000..6000,
            (3000.0, 100.0, 30),
        ),
        (
            (1000, 10, 5050),
            4100..5100,
            3000..6100,
            (3000.0, 100.0, 31),
        ),
        // Before tick 0.
        ((10, 4, -7), -16..-6, -36..6, (-36.0, 3.0, 14)),
        // Fewer ticks than columns: a tick a column.
        ((3, 10, 25), 22..25, 0..35, (0.0, 1.0, 35)),
    ];
    let rec = Recording::open(Arc::new(finished()), Cache::new(1 << 20)).unwrap();
    let cancel = AtomicBool::new(false);
    for ((width, columns, end), window, around, (x0, per, count)) in rows {
        let q = Query {
            window: end - width..end,
            columns,
            aligned: true,
            ..Query::default()
        };
        let a = rec.query(&q, &cancel).unwrap();
        let at = format!("{width} ticks in {columns} columns to {end}");
        assert_eq!((a.window, a.around), (window, around), "{at}");
        assert_eq!(
            (a.grid.x0, a.grid.per, a.grid.count),
            (x0, per, count),
            "{at}"
        );
    }
    // Unaligned, the first row's grid starts a width before the window, a seventh of it a column.
    let a = rec
        .query(
            &Query {
                window: 900..1000,
                columns: 7,
                ..Query::default()
            },
            &cancel,
        )
        .unwrap();
    assert_eq!((a.window, a.around.clone()), (900..1000, 800..1100));
    assert_eq!(
        (a.grid.x0, a.grid.per, a.grid.count),
        (800.0, 100.0 / 7.0, 21)
    );
}

/// At least 3,000 ticks of the recorder's items, finished at 4,000.
fn finished() -> Vec<u8> {
    let mut rec = Recorder::new(0x51ed_270b_27a4_3c19, 4000);
    while rec.floor < 3000 {
        rec.batch();
    }
    let mut file = rec.file.bytes.lock().unwrap().clone();
    file.extend(
        rec.w
            .finish(4000, &[4000, 1600], Finish::default())
            .unwrap()
            .tail,
    );
    file
}

/// One way of following the recording: its window and columns, a now ahead of the data or
/// not, the strip it asks for, and how many snapshots it lets pass between answers.
struct Follow {
    width: i64,
    columns: u32,
    ahead: bool,
    strip: Option<(usize, u32)>,
    every: u64,
}

#[test]
fn an_extended_answer_is_a_fresh_querys_after_every_snapshot() {
    let follows = [
        // Columns of 6, 8 and 9 ticks either side of the finest bucket's 8.
        (40, 7, false, Some((I, 13)), 1),
        (64, 8, true, None, 1),
        (63, 8, false, Some((D1, 5)), 1),
        (72, 8, false, None, 1),
        (170, 10, true, Some((F, 7)), 1),
        // Columns that read the overview: 112 ticks (buckets of 8 and 128 fit), 273.
        (1000, 9, false, Some((P, 9)), 1),
        (3000, 11, true, None, 1),
        // A tick a column, and one column.
        (50, 60, false, None, 1),
        (2000, 1, false, Some((I, 3)), 1),
        // Three snapshots between answers.
        (600, 13, true, Some((V, 11)), 3),
    ]
    .map(|(width, columns, ahead, strip, every)| Follow {
        width,
        columns,
        ahead,
        strip,
        every,
    });
    let cancel = AtomicBool::new(false);
    let (mut extended, mut refused) = (vec![0u32; follows.len()], vec![0u32; follows.len()]);
    for seed in 1..=6u64 {
        let mut rec = Recorder::new(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15), 1 << 40);
        let cache = Cache::new(if seed % 2 == 0 { 0 } else { 1 << 20 });
        let mut live: Option<Recording> = None;
        let mut held: Vec<Option<(Answer, Query)>> = follows.iter().map(|_| None).collect();
        let mut ahead = Random(seed);
        // Each follow's now, which only moves on, and runs ahead of the data by up to 40 ticks.
        let mut nows = vec![i64::MIN; follows.len()];
        for n in 0..250u64 {
            let snapshot = rec.batch();
            match &mut live {
                None => {
                    live =
                        Some(Recording::live(snapshot, rec.file.clone(), cache.clone()).unwrap());
                }
                Some(r) => r.update(snapshot).unwrap(),
            }
            let r = live.as_ref().unwrap();
            let Some(extent) = r.extent() else {
                continue;
            };
            for (k, f) in follows.iter().enumerate() {
                if n % f.every != 0 {
                    continue;
                }
                if f.ahead {
                    nows[k] = nows[k].max(extent.end + ahead.below(40) as i64);
                }
                let now = nows[k].max(extent.end) - extent.end;
                let q = query(followed(&extent, f.width, now), f.columns, f.strip);
                let at = format!(
                    "seed {seed}, snapshot {n}, {} ticks in {} columns, window {:?}",
                    f.width, f.columns, q.window
                );
                let fresh = r.query(&q, &cancel).unwrap();
                let next = match &held[k] {
                    Some((answer, asked)) => match r.extend(answer, asked, &q, &cancel).unwrap() {
                        Some(e) => {
                            same(&e, &fresh, &at);
                            extended[k] += 1;
                            e
                        }
                        None => {
                            refused[k] += 1;
                            fresh
                        }
                    },
                    None => fresh,
                };
                held[k] = Some((next, q));
            }
        }
    }
    // Every way of following extended at least as many answers as it refused: for a width that
    // grew while the window began at the extent's, for `around`'s start moving on, or for a
    // window moved more than its width.
    for (k, f) in follows.iter().enumerate() {
        eprintln!(
            "{} ticks in {} columns: {} extended, {} refused",
            f.width, f.columns, extended[k], refused[k]
        );
        assert!(
            extended[k] >= refused[k] && extended[k] > 100,
            "{} ticks in {} columns: {} extended, {} refused",
            f.width,
            f.columns,
            extended[k],
            refused[k]
        );
    }
}

/// A live recording of V and I at a pair every 10 ticks to tick 1,990, in blocks of 16 samples,
/// each item promising its tick, read through `file`, and its writer.
fn pairs_to_1990(file: &Arc<Growing>) -> (Writer, Recording) {
    let options = WriteOptions {
        level: 1,
        block_bytes: 64,
        promise_with_items: true,
        ..WriteOptions::default()
    };
    // Buckets of 256 ticks, wider than the columns asked of it: they read the samples.
    let mut w = Writer::new(declaration(8, 1 << 40), options).unwrap();
    file.bytes.lock().unwrap().extend(w.take_output());
    w.run(V, 0, 0).unwrap();
    w.run(I, 0, 0).unwrap();
    for n in 0..200u64 {
        w.sample(V, 3300 + n % 7).unwrap();
        w.sample(I, 1_000_000 + n * 37).unwrap();
    }
    let snapshot = w.snapshot();
    file.bytes.lock().unwrap().extend(w.take_output());
    let rec = Recording::live(snapshot, file.clone(), Cache::new(0)).unwrap();
    (w, rec)
}

fn pairs_query(window: Range<i64>) -> Query {
    Query {
        window,
        columns: 10,
        channels: vec![I],
        aligned: true,
        ..Query::default()
    }
}

#[test]
fn an_extend_keeps_the_columns_before_the_first_change_and_reads_from_there() {
    let file = Arc::new(Growing::default());
    let (mut w, mut rec) = pairs_to_1990(&file);
    let cancel = AtomicBool::new(false);
    // The last pair's time ends at the length, 1,991. 400 ticks in 10 columns are 40 ticks each:
    // the window ends at 50 × 40, `around` runs from 30 × 40 to 60 × 40.
    let asked = pairs_query(1591..1991);
    let mut held = rec.query(&asked, &cancel).unwrap();
    assert_eq!(
        (held.window.clone(), held.around.clone()),
        (1600..2000, 1200..2400)
    );
    // Marked: column 18, which no snapshot changes, and column 19, which holds the last pair at
    // 1,990 and so the first tick that can change.
    let Drawn::Analog { columns, .. } = &mut held.channels[0] else {
        panic!("I is analog")
    };
    for k in [18, 19] {
        columns.min[k] = -1.0;
    }
    // Three pairs more, to 2,020: the length 2,021, the window ending at 51 × 40.
    for n in 200..203u64 {
        w.sample(V, 3300 + n % 7).unwrap();
        w.sample(I, 1_000_000 + n * 37).unwrap();
    }
    w.flush(2021).unwrap();
    let snapshot = w.snapshot();
    file.bytes.lock().unwrap().extend(w.take_output());
    rec.update(snapshot.clone()).unwrap();
    let q = pairs_query(1621..2021);
    file.reads.lock().unwrap().clear();
    let extended = rec.extend(&held, &asked, &q, &cancel).unwrap().unwrap();
    let reads = std::mem::take(&mut *file.reads.lock().unwrap());
    let fresh = rec.query(&q, &cancel).unwrap();
    assert_eq!(extended.window, 1640..2040);
    let (Drawn::Analog { columns: e, .. }, Drawn::Analog { columns: f, .. }) =
        (&extended.channels[0], &fresh.channels[0])
    else {
        panic!("I is analog")
    };
    // Column 18 is the held one, mark and all; 19 on are worked out again, as the fresh query's.
    assert_eq!(e.min[18], -1.0);
    assert_ne!(f.min[18], -1.0);
    assert_eq!(&e.min[19..], &f.min[19..]);
    assert_eq!(&e.max, &f.max);
    // Only the blocks from column 19's start, 1,960, were read, where the fresh query read
    // every block from `around`'s start.
    let bytes = file.bytes.lock().unwrap().clone();
    let refs = snapshot.block_refs(I, &[], from_memory(&bytes)).unwrap();
    let block_of = |offset: u64| {
        *refs
            .iter()
            .find(|b| (b.row.offset..b.row.offset + u64::from(b.row.length)).contains(&offset))
            .unwrap()
    };
    assert!(!reads.is_empty());
    for offset in &reads {
        let b = block_of(*offset);
        assert!(b.end > 1960, "the block at {}: {b:?}", b.start);
    }
    file.reads.lock().unwrap().clear();
    rec.query(&q, &cancel).unwrap();
    let all = file.reads.lock().unwrap().len();
    assert!(all > 4 * reads.len(), "{all} against {}", reads.len());
}

#[test]
fn an_extend_answers_only_what_it_can_answer_exactly() {
    let file = Arc::new(Growing::default());
    let (mut w, mut rec) = pairs_to_1990(&file);
    let cancel = AtomicBool::new(false);
    let asked = pairs_query(1591..1991);
    let held = rec.query(&asked, &cancel).unwrap();
    let extend = |rec: &Recording, q: &Query| rec.extend(&held, &asked, q, &cancel).unwrap();
    // The same window, and one 12 ticks on: both answered, and as a fresh query answers.
    for q in [asked.clone(), pairs_query(1603..2003)] {
        let e = extend(&rec, &q).unwrap();
        same(
            &e,
            &rec.query(&q, &cancel).unwrap(),
            &format!("{:?}", q.window),
        );
    }
    let refused = |q: Query, why: &str| assert!(extend(&rec, &q).is_none(), "{why}");
    refused(
        Query {
            channels: vec![V],
            ..pairs_query(1603..2003)
        },
        "another channel",
    );
    refused(
        Query {
            aligned: false,
            ..pairs_query(1603..2003)
        },
        "not aligned",
    );
    refused(pairs_query(1560..1960), "moved back");
    refused(pairs_query(1603..2004), "wider");
    // The window's end column from 50 to 60: `around` from 40 × 40, not 30 × 40.
    refused(pairs_query(1991..2391), "around's start moved on");
    let a = rec.query(&pairs_query(1991..2391), &cancel).unwrap();
    assert_eq!(a.around.start, 1600);
    // Another recording's answer, though the data is the same.
    let other = pairs_to_1990(&Arc::new(Growing::default())).1;
    assert!(
        other
            .extend(&held, &asked, &asked, &cancel)
            .unwrap()
            .is_none()
    );
    // 64 snapshots on, the changes since are all kept; at 65 the first has gone.
    for n in 0..65u64 {
        w.flush(1991 + n as i64).unwrap();
        let snapshot = w.snapshot();
        file.bytes.lock().unwrap().extend(w.take_output());
        rec.update(snapshot).unwrap();
        let answered = extend(&rec, &asked).is_some();
        assert_eq!(answered, n < 64, "after {} snapshots", n + 1);
    }
}

#[test]
fn a_finished_recording_extends_a_window_moved_on_within_its_page() {
    let rec = Recording::open(Arc::new(finished()), Cache::new(1 << 20)).unwrap();
    let cancel = AtomicBool::new(false);
    let mut asked = query(1000..1300, 12, Some((I, 10)));
    let mut held = rec.query(&asked, &cancel).unwrap();
    let mut extended = 0;
    for start in (1003..1700).step_by(7) {
        let q = query(start..start + 300, 12, Some((I, 10)));
        let fresh = rec.query(&q, &cancel).unwrap();
        if let Some(e) = rec.extend(&held, &asked, &q, &cancel).unwrap() {
            same(&e, &fresh, &format!("{:?}", q.window));
            extended += 1;
        }
        (held, asked) = (fresh, q);
    }
    assert!(extended > 80, "{extended}");
}

/// A writer whose channels keep their own order: V, I and D0 lead, F and D1 lag behind them by up
/// to 300 ticks, each channel promising only what it has, and D0 now and then reports a gap that
/// started before the last snapshot's end.
struct Lagging {
    w: Writer,
    file: Arc<Growing>,
    r: Random,
    /// The leaders' and the laggards' promises.
    lead: i64,
    lag: i64,
    /// The next pair's tick, and F's next sample's source tick.
    pair: i64,
    step: i64,
    /// D0's and D1's least source tick for their next edge.
    edge: [i64; 2],
    /// Where D0's last edge or gap ends: a gap may start after it.
    after: i64,
    pairs: u64,
}

impl Lagging {
    fn new(seed: u64) -> Self {
        let mut r = Random(seed);
        let options = WriteOptions {
            level: 1,
            block_bytes: 32 + r.below(200) as usize,
            compression: Compression::Spread,
            promise_with_items: false,
        };
        let mut w = Writer::new(declaration(3, 1 << 40), options).unwrap();
        let file = Arc::new(Growing::default());
        file.bytes.lock().unwrap().extend(w.take_output());
        w.run(V, 0, 0).unwrap();
        w.run(I, 0, 0).unwrap();
        w.run(F, 0, 0).unwrap();
        Self {
            w,
            file,
            r,
            lead: 0,
            lag: 0,
            pair: 0,
            step: 0,
            edge: [0; 2],
            after: -1,
            pairs: 0,
        }
    }

    /// Edges of digital channel `d` from its least next tick, and its promise, up to before axis
    /// tick `to`.
    fn edges(&mut self, d: usize, to: i64) {
        let (c, axis): (usize, fn(i64) -> i64) = if d == 0 { (D0, |k| k) } else { (D1, axis1) };
        let promised = if d == 0 { self.lead } else { first1(self.lag) };
        self.edge[d] = self.edge[d].max(promised);
        while self.r.one_in(2) {
            let k = self.edge[d] + self.r.below(40) as i64;
            if axis(k) >= to {
                break;
            }
            self.w.edge(c, k).unwrap();
            self.edge[d] = k + 1;
            if d == 0 {
                self.after = k;
            }
        }
    }

    fn batch(&mut self) -> Snapshot {
        let lead = self.lead + 1 + self.r.below(150) as i64;
        while self.pair < lead {
            let n = self.pairs;
            self.w.sample(V, 3300 + n % 7).unwrap();
            self.w.sample(I, 1_000_000 + (n * 7919) % 50_000).unwrap();
            self.pairs += 1;
            self.pair += PAIR;
        }
        self.edges(0, lead);
        // A gap of D0 reported now, from past its last edge to before its promise: it can start
        // before its promise in the last snapshot, and before that snapshot's end.
        if self.r.one_in(5) {
            let from = self.after + 1 + self.r.below(10) as i64;
            let to = lead - 1 - self.r.below(5) as i64;
            let level = self.r.one_in(2);
            if to > from && self.w.gap(D0, Gap { from, to, level }).is_ok() {
                self.edge[0] = to + 1;
                self.after = to;
            }
        }
        for c in [V, I, D0] {
            self.w.promise_channel(c, lead).unwrap();
        }
        self.lead = lead;
        let lag = self.lag.max(lead - self.r.below(300) as i64);
        while axis1(self.step) < lag {
            let x = (self.r.below(2000) as f32 - 1000.0) / 8.0;
            self.w.sample(F, x).unwrap();
            self.step += STEP;
        }
        self.edges(1, lag);
        for c in [F, D1] {
            self.w.promise_channel(c, lag).unwrap();
        }
        self.lag = lag;
        let snapshot = self.w.snapshot();
        self.file.bytes.lock().unwrap().extend(self.w.take_output());
        snapshot
    }
}

/// A channel that lags the others gets its items before the last snapshot's end, and a gap
/// reported late starts before it: an extend reads each channel again from its own promise, so
/// its answer is still a fresh query's.
#[test]
fn an_extend_follows_a_channel_that_lags_and_a_gap_reported_late() {
    let cancel = AtomicBool::new(false);
    let follows = [(64, 8), (170, 10), (600, 13), (3000, 11)];
    let mut extended = vec![0u32; follows.len()];
    for seed in 1..=4u64 {
        let mut rec = Lagging::new(seed.wrapping_mul(0x2545_f491_4f6c_dd1d));
        let mut live: Option<Recording> = None;
        let mut held: Vec<Option<(Answer, Query)>> = follows.iter().map(|_| None).collect();
        for n in 0..200u64 {
            let snapshot = rec.batch();
            match &mut live {
                None => {
                    let cache = Cache::new(1 << 20);
                    live = Some(Recording::live(snapshot, rec.file.clone(), cache).unwrap());
                }
                Some(r) => r.update(snapshot).unwrap(),
            }
            let r = live.as_ref().unwrap();
            let Some(extent) = r.extent() else {
                continue;
            };
            for (k, &(width, columns)) in follows.iter().enumerate() {
                let q = query(followed(&extent, width, 0), columns, Some((F, 7)));
                let at = format!("seed {seed}, snapshot {n}, {width} ticks in {columns} columns");
                let fresh = r.query(&q, &cancel).unwrap();
                let next = match &held[k] {
                    Some((answer, asked)) => match r.extend(answer, asked, &q, &cancel).unwrap() {
                        Some(e) => {
                            same(&e, &fresh, &at);
                            extended[k] += 1;
                            e
                        }
                        None => fresh,
                    },
                    None => fresh,
                };
                held[k] = Some((next, q));
            }
        }
    }
    for (k, &(width, columns)) in follows.iter().enumerate() {
        assert!(
            extended[k] > 100,
            "{width} ticks in {columns} columns: {} extended",
            extended[k]
        );
    }
}
