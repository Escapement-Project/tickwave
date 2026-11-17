//! The `Writer` fed arbitrary calls after a declaration: edges, gaps, runs at any phase, samples,
//! annotations, flushes, snapshots read by a live view, output taken, and the finish. A call that
//! breaks a rule returns an error. A recording it finishes reads back: at random, in order, and in
//! the view, its markers by range and whole and its edges counted.
#![no_main]

use std::collections::BTreeMap;
use std::io;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use arbitrary::Arbitrary;
use escapement_tickwave_format::decode::Footer;
use escapement_tickwave_format::header::{
    AnalogCoding, Declaration, DeclaredChannel, DeclaredKind, Derived, Exact, IntCoding, Kept,
    Offset, Shaping, SourceFormat, SourceHeader,
};
use escapement_tickwave_format::mapping::Mapping;
use escapement_tickwave_format::model::{Break, Gap, Grid, Ratio, Sample};
use escapement_tickwave_format::reader::{Ending, Reader};
use escapement_tickwave_format::writer::{Bundle, Compression, Finish, WriteOptions, Writer};
use escapement_tickwave_view::{Bytes, Cache, Query, Recording};
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
struct Source {
    rate: (u32, u16),
    origin: i16,
    stretch: Option<(u16, u16)>,
    align: Option<i16>,
}

#[derive(Arbitrary, Debug)]
enum Coding {
    F32,
    F64,
    Int {
        bits: u8,
        signed: bool,
        scale: Option<(i16, u16)>,
        offset: Option<(i16, u16)>,
    },
    Codes {
        bits: u8,
        scale: f64,
        offset: f64,
    },
}

#[derive(Arbitrary, Debug)]
enum Kind {
    Digital(bool),
    Analog {
        period: (u16, u16),
        coding: Coding,
        significant: Option<u8>,
    },
}

#[derive(Arbitrary, Debug)]
struct Chan {
    source: u8,
    kind: Kind,
    offset: Option<i16>,
    kept: (u16, u32),
}

/// A tick some way on from the last. `Far` and `Pow` reach at most 2^(shift + 22) ticks on,
/// 1,024 overview blocks of the finest channel's finest level: a span near the most tickwave
/// keeps writes a million blocks alike, seconds of an instrumented build's time for no new path.
/// `Wild` still reaches every tick, the bound's past included, and the format's tests write at
/// the bound.
#[derive(Arbitrary, Debug)]
enum Tick {
    Step(u8),
    Far(u32),
    /// 2^p ticks on.
    Pow(u8),
    Wild(i64),
}

#[derive(Arbitrary, Debug)]
enum Op {
    Edge(u8, Tick),
    Gap {
        c: u8,
        at: Tick,
        len: u16,
        level: bool,
    },
    Run {
        c: u8,
        at: Tick,
        lost: u16,
    },
    Samples {
        c: u8,
        n: u8,
        value: u64,
        raw: Option<RawSample>,
    },
    Annotation {
        source: u8,
        at: Tick,
        end: Option<u16>,
        id: bool,
        junk: bool,
    },
    Flush(Tick),
    Snapshot,
    Take,
    Consume(u16),
}

#[derive(Arbitrary, Debug, Clone, Copy)]
enum RawSample {
    F32(f32),
    F64(f64),
    U64(u64),
    I64(i64),
}

#[derive(Arbitrary, Debug)]
struct Plan {
    sources: Vec<Source>,
    timebase: Option<(u32, u16)>,
    chans: Vec<Chan>,
    derived: Vec<(u8, u8)>,
    shift: u8,
    shaped: bool,
    block_bytes: u16,
    /// Bit 0: each item promises its own tick. Bit 1: frames compress on rayon's threads.
    writing: u8,
    ops: Vec<Op>,
    length: Tick,
    ends: Vec<Tick>,
    bundle: u8,
    drop_gaps: bool,
    extra: Option<(u8, String)>,
    // Added after the corpus was made, and last, so that its inputs decode as they did up to
    // `extra`.
    /// Each channel's finest overview shift, by channel; None takes the recording's.
    shifts: Vec<Option<u8>>,
    /// Samples lost after an analog channel's last, by channel.
    lost_after: Vec<(u8, u16)>,
    /// Each analog channel's declared phase, `num % den` over `den`, by channel.
    phases: Vec<Option<(u16, u16)>>,
    /// Each `Op::Run`'s phase in turn, in `1/D` ticks of its channel's grid.
    run_phases: Vec<u16>,
    /// Each derived channel's operation: a subtraction where true.
    subtract: Vec<bool>,
}

fn ratio((n, d): (u32, u16)) -> Ratio {
    Ratio::new(u64::from(n).max(1), u64::from(d).max(1)).unwrap()
}

fn coding(c: &Coding) -> AnalogCoding {
    let exact = |r: Option<(i16, u16)>| r.and_then(|(n, d)| Exact::new(i64::from(n), i64::from(d)));
    match *c {
        Coding::F32 => AnalogCoding::F32,
        Coding::F64 => AnalogCoding::F64,
        Coding::Int {
            bits,
            signed,
            scale,
            offset,
        } => AnalogCoding::Int(IntCoding {
            bits,
            signed,
            scale: exact(scale),
            offset: exact(offset),
        }),
        Coding::Codes {
            bits,
            scale,
            offset,
        } => AnalogCoding::Codes {
            bits,
            scale,
            offset,
        },
    }
}

fn declaration(p: &Plan) -> Declaration {
    let sources: Vec<SourceHeader> = p
        .sources
        .iter()
        .take(3)
        .map(|s| SourceHeader {
            origin: i64::from(s.origin),
            align: s.align.map(i64::from),
            stretch: s.stretch.map(|(n, d)| ratio((u32::from(n), d))),
            ..SourceHeader::new(SourceFormat::Other("fuzz".into()), ratio(s.rate))
        })
        .collect();
    let timebase = match (sources.as_slice(), p.timebase) {
        ([one], _) | ([one, ..], None) => one.timebase,
        (_, Some(t)) => ratio(t),
        ([], _) => Ratio::ONE,
    };
    let n = sources.len().max(1) as u8;
    let channels: Vec<DeclaredChannel> = p
        .chans
        .iter()
        .take(6)
        .enumerate()
        .map(|(i, c)| DeclaredChannel {
            name: format!("c{i}"),
            source_name: format!("s{i}"),
            source: u32::from(c.source % n),
            data: match &c.kind {
                Kind::Digital(initial) => DeclaredKind::Digital { initial: *initial },
                Kind::Analog {
                    period,
                    coding: k,
                    significant,
                } => DeclaredKind::Analog {
                    unit: "V".into(),
                    period: ratio((u32::from(period.0), period.1)),
                    coding: coding(k),
                    significant: *significant,
                    phase: p.phases.get(i).copied().flatten().and_then(|(n, d)| {
                        let d = u64::from(d.max(1));
                        Ratio::new(u64::from(n) % d, d)
                    }),
                },
            },
            shaping: Shaping {
                offset: c.offset.map(|t| Offset {
                    ticks: i64::from(t),
                    kind: Offset::DELAY.into(),
                }),
                kept: None,
            },
            overview_shift: p.shifts.get(i).copied().flatten().map(|s| s % 63),
            codec: None,
            requires: Vec::new(),
        })
        .collect();
    let derived = p
        .derived
        .iter()
        .take(2)
        .enumerate()
        .map(|(i, &(a, b))| Derived {
            name: format!("d{i}"),
            unit: "W".into(),
            op: if p.subtract.get(i).copied().unwrap_or(false) {
                escapement_tickwave_format::header::Op::Subtract
            } else {
                escapement_tickwave_format::header::Op::Multiply
            },
            operands: vec![
                format!("c{}", usize::from(a) % channels.len().max(1)),
                format!("c{}", usize::from(b) % channels.len().max(1)),
            ],
            params: Default::default(),
            requires: Vec::new(),
            significant: None,
        })
        .collect();
    let mut d = Declaration {
        timebase,
        channels,
        derived,
        sources,
        title: None,
        meta: BTreeMap::new(),
        overview_shift: p.shift % 63,
        requires: Vec::new(),
    };
    // Kept spans where the format calls for them, the first channel's from at the axis's tick 0.
    let shaped = p.shaped
        || d.sources.len() > 1
        || d.sources.iter().any(|s| s.stretch.is_some())
        || d.channels.iter().any(|c| c.shaping.offset.is_some());
    if shaped {
        for (i, c) in p.chans.iter().take(d.channels.len()).enumerate() {
            let Some(map) = d.mapping(i) else { continue };
            let from = map.first_at(0) + if i == 0 { 0 } else { i64::from(c.kept.0) };
            d.channels[i].shaping.kept = Some(Kept {
                from,
                to: from + 1 + i64::from(c.kept.1),
            });
        }
    }
    d
}

/// The file as the writer has handed it out, which a live view reads as it grows.
#[derive(Clone, Default)]
struct Shared(Arc<Mutex<Vec<u8>>>);

impl Bytes for Shared {
    fn size(&self) -> io::Result<u64> {
        Ok(self.0.lock().unwrap().len() as u64)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let file = self.0.lock().unwrap();
        let at = usize::try_from(offset).map_err(|_| io::ErrorKind::UnexpectedEof)?;
        let src = file
            .get(at..at + buf.len())
            .ok_or(io::ErrorKind::UnexpectedEof)?;
        buf.copy_from_slice(src);
        Ok(())
    }
}

/// What the target knows of each channel, to place its calls' ticks.
struct Track {
    map: Mapping,
    /// The last source tick it gave the channel, or its start.
    last: i64,
    analog: Option<Analog>,
}

struct Analog {
    grid: Grid,
    coding: AnalogCoding,
    /// The latest run: its tick, first sample and phase.
    run: Option<(i64, u64, u32)>,
    count: u64,
}

fuzz_target!(|p: Plan| {
    if std::env::var_os("WRITER_PLAN").is_some() {
        eprintln!("{p:#?}");
    }
    let d = declaration(&p);
    let tracks: Option<Vec<Track>> = (0..d.channels.len())
        .map(|c| {
            let map = d.mapping(c)?;
            let start = d.start(c).unwrap_or(0);
            Some(Track {
                map,
                last: start,
                analog: match &d.channels[c].data {
                    DeclaredKind::Analog { coding, .. } => Some(Analog {
                        grid: d.channels[c].grid().unwrap_or(Grid::WHOLE),
                        coding: *coding,
                        run: None,
                        count: 0,
                    }),
                    DeclaredKind::Digital { .. } => None,
                },
            })
        })
        .collect();
    let options = WriteOptions {
        level: 1,
        block_bytes: usize::from(p.block_bytes),
        promise_with_items: p.writing & 1 != 0,
        compression: if p.writing & 2 != 0 {
            Compression::Offload
        } else {
            Compression::Spread
        },
    };
    let Ok(mut w) = Writer::new(d, options) else {
        return;
    };
    let mut tracks = tracks.expect("a declaration the writer takes maps every channel");
    let n = tracks.len();
    let file = Shared::default();
    let take = |w: &mut Writer| file.0.lock().unwrap().extend(w.take_output());
    let mut live: Option<Recording> = None;
    let cancel = AtomicBool::new(false);
    // The latest axis tick of an item the writer took.
    let mut last_axis = 0i64;
    let shift = w.declaration().overview_shift;
    let finest = w
        .declaration()
        .channels
        .iter()
        .map(|c| c.overview_shift.unwrap_or(shift))
        .fold(shift, u8::min);
    let cap = (u32::from(finest) + 22).min(62);
    let on = |base: i64, at: &Tick| -> i64 {
        match *at {
            Tick::Step(s) => base.saturating_add(i64::from(s)),
            Tick::Far(f) => base.saturating_add(i64::from(f) & ((1 << cap) - 1)),
            Tick::Pow(p) => base.saturating_add(1 << (u32::from(p) % (cap + 1))),
            Tick::Wild(k) => k,
        }
    };
    let source_tick = |t: &Track, at: &Tick| on(t.last, at);
    let axis_tick = on;
    // The runs started so far, which picks each one's phase.
    let mut runs = 0usize;
    for op in &p.ops {
        match op {
            Op::Edge(c, at) if n > 0 => {
                let c = usize::from(*c) % n;
                let t = &mut tracks[c];
                let k = source_tick(t, at);
                if w.edge(c, k).is_ok() {
                    t.last = k;
                    last_axis = last_axis.max(t.map.axis(k).unwrap_or(0));
                }
            }
            Op::Gap { c, at, len, level } if n > 0 => {
                let c = usize::from(*c) % n;
                let t = &mut tracks[c];
                let from = source_tick(t, at);
                let to = from.saturating_add(1 + i64::from(*len));
                let gap = Gap {
                    from,
                    to,
                    level: *level,
                };
                if w.gap(c, gap).is_ok() {
                    t.last = to;
                }
            }
            Op::Run { c, at, lost } if n > 0 => {
                let c = usize::from(*c) % n;
                let t = &mut tracks[c];
                let k = source_tick(t, at);
                // The first run takes the declared phase; a later one the plan's, where it has one.
                let phase = match (&t.analog, p.run_phases.get(runs)) {
                    (Some(a), Some(&f)) if a.run.is_some() => u32::from(f) % a.grid.d() as u32,
                    (Some(a), _) => a.grid.phase(),
                    (None, _) => 0,
                };
                runs += 1;
                if w.run_at(c, k, phase, u64::from(*lost)).is_ok()
                    && let Some(a) = &mut t.analog
                {
                    let phase = if a.count == 0 { a.grid.phase() } else { phase };
                    a.run = Some((k, a.count, phase));
                }
            }
            Op::Samples {
                c,
                n: many,
                value,
                raw,
            } if n > 0 => {
                let c = usize::from(*c) % n;
                let t = &mut tracks[c];
                let Some(a) = &mut t.analog else {
                    let _ = w.sample(c, *value);
                    continue;
                };
                for i in 0..u64::from(*many).max(1) {
                    let v = value.wrapping_add(i);
                    let sample = match (raw, a.coding) {
                        (Some(RawSample::F32(x)), _) => Sample::F32(*x),
                        (Some(RawSample::F64(x)), _) => Sample::F64(*x),
                        (Some(RawSample::U64(x)), _) => Sample::U64(*x),
                        (Some(RawSample::I64(x)), _) => Sample::I64(*x),
                        (None, AnalogCoding::F32) => Sample::F32(v as f32),
                        (None, AnalogCoding::F64) => Sample::F64(v as f64),
                        (
                            None,
                            AnalogCoding::Codes {
                                scale,
                                offset,
                                bits,
                            },
                        ) => {
                            let code = (v % (1 << bits.clamp(1, 16))) as u16;
                            Sample::F32(escapement_tickwave_format::header::code_value(
                                code, scale, offset,
                            ))
                        }
                        (None, AnalogCoding::Int(int)) => {
                            let bits = u32::from(int.bits.clamp(1, 64));
                            let v = if bits == 64 { v } else { v % (1 << bits) };
                            if int.signed {
                                Sample::I64((v << (64 - bits)) as i64 >> (64 - bits))
                            } else {
                                Sample::U64(v)
                            }
                        }
                    };
                    match w.sample(c, sample) {
                        Ok(()) => {
                            a.count += 1;
                            if let Some((tick, index, phase)) = a.run {
                                let grid = a.grid;
                                let run = Break {
                                    index,
                                    tick,
                                    lost: 0,
                                    phase,
                                };
                                let place = grid.floor(grid.place(run, a.count - 1));
                                t.last = place;
                                last_axis = last_axis.max(t.map.axis(place).unwrap_or(0));
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
            Op::Annotation {
                source,
                at,
                end,
                id,
                junk,
            } => {
                let start = axis_tick(last_axis, at);
                let mut a = serde_json::Map::new();
                a.insert("start".into(), start.into());
                if let Some(e) = end {
                    a.insert("end".into(), (start.saturating_add(i64::from(*e))).into());
                }
                if *id {
                    a.insert("id".into(), format!("mark/{start}").into());
                }
                if *junk {
                    a.insert("junk".into(), 1.into());
                }
                // Three generators, and three names no generator's file can take.
                let name = match source % 6 {
                    s @ 0..3 => format!("gen{s}"),
                    3 => String::new(),
                    4 => "a b".into(),
                    _ => "x/../y".into(),
                };
                let _ = w.annotation(&name, a);
            }
            Op::Flush(at) => {
                let before = axis_tick(last_axis + 1, at);
                if w.flush(before).is_ok() {
                    last_axis = last_axis.max(before.saturating_sub(1));
                }
            }
            Op::Snapshot => {
                let snapshot = w.snapshot();
                for c in 0..n {
                    let _ = snapshot.unwritten_blocks(c);
                }
                for o in 0.. {
                    let Some(overview) = snapshot.overview(o) else {
                        break;
                    };
                    for li in 0..overview.levels.len() {
                        let _ = snapshot.unwritten_overview(o, li);
                    }
                }
                take(&mut w);
                let view = match live.as_mut() {
                    Some(r) => r.update(snapshot).map(|()| r),
                    None => Recording::live(snapshot, Arc::new(file.clone()), Cache::new(1 << 20))
                        .map(|r| live.insert(r)),
                };
                if let Ok(r) = view {
                    #[expect(
                        clippy::single_range_in_vec_init,
                        reason = "one interval to measure, not its ticks"
                    )]
                    let statistics = vec![0..r.length()];
                    let q = {
                        let mut query = Query::new(0..r.length().max(1), 64);
                        query.channels = (0..r.channels().len()).collect();
                        query.statistics = statistics;
                        query
                    };
                    let _ = r.query(&q, &cancel);
                }
            }
            Op::Take => take(&mut w),
            Op::Consume(k) => {
                let k = usize::from(*k).min(w.output().len());
                file.0.lock().unwrap().extend_from_slice(&w.output()[..k]);
                w.consume(k);
            }
            _ => {}
        }
    }
    let ends: Vec<i64> = tracks
        .iter()
        .filter(|t| t.analog.is_none())
        .zip(p.ends.iter().chain(std::iter::repeat(&Tick::Step(1))))
        .map(|(t, at)| source_tick(t, at).saturating_add(1))
        .collect();
    let finish = Finish {
        extra: p
            .extra
            .iter()
            .map(|(s, v)| BTreeMap::from([(format!("k{s}"), v.clone())]))
            .collect(),
        bundle: match p.bundle % 3 {
            0 => None,
            1 => Some(Bundle::Generated { stem: "w".into() }),
            _ => Some(Bundle::Files(BTreeMap::from([(
                "w.annotations.toml".into(),
                "format = 1".into(),
            )]))),
        },
        drop_gaps_past_end: p.drop_gaps,
        lost_after: p
            .lost_after
            .iter()
            .map(|&(c, lost)| (usize::from(c) % n.max(1), u64::from(lost)))
            .collect(),
        view: None,
    };
    take(&mut w);
    let length = axis_tick(last_axis + 1, &p.length);
    let Ok(finished) = w.finish(length, &ends, finish) else {
        return;
    };
    let mut bytes = std::mem::take(&mut *file.0.lock().unwrap());
    bytes.extend_from_slice(&finished.tail);
    // Reading a file this large back costs the target more than the writer took to make it.
    if bytes.len() > 16 << 20 {
        return;
    }
    let footer = Footer::parse_file(&bytes).expect("a finished recording reads");
    assert_eq!(footer.header.content_id, finished.header.content_id);
    let mut reader = Reader::default();
    reader.feed(&bytes);
    while reader
        .next_event()
        .expect("a finished recording reads in order")
        .is_some()
    {}
    assert_eq!(reader.end().unwrap(), Ending::Complete);
    // The writer takes only annotations a generator's file can hold, so a bundle of them
    // resolves; a bundle of files a toml one names is the caller's, and may not.
    let length = footer.header.length;
    match Recording::open(Arc::new(bytes), Cache::new(1 << 20)) {
        Ok(rec) => {
            // Markers by range, then whole; edges counted from the index against those read.
            let half = length / 2;
            let _ = rec.markers(0..half, None, 16);
            let _ = rec.markers(half..length, None, 16);
            let whole = rec.load_markers();
            if whole.is_ok() {
                let _ = (rec.marker_count(), rec.markers_left_out());
            }
            for c in 0..n {
                if let Ok(count) = rec.edge_count(c, half / 2, length, &cancel) {
                    let read = rec.edges(c, half / 2, length, &cancel).unwrap().1;
                    assert_eq!(count, read.len() as u64, "channel {c}'s edges counted");
                }
            }
        }
        Err(escapement_tickwave_view::Error::Annotations(_)) if p.bundle % 3 == 2 => {}
        Err(e) => panic!("the view refuses a finished recording: {e}"),
    }
});
