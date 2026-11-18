//! SWD read from a recording through the wire source, against the same wires served directly.

mod common;

use common::*;
use escapement_tickwave_decode::{
    Counts, Host, HostOptions, InputSpec, Observed, Source, Span, Tick, name,
    wire::{BindWire, WireSource},
};
use escapement_tickwave_decoders::{adiv5, swd};
use escapement_tickwave_format::{
    header::{
        Declaration, DeclaredChannel, DeclaredKind, Kept, Shaping, SourceFormat, SourceHeader,
    },
    model::{Gap, Ratio},
    writer::{Finish, WriteOptions, Writer},
};
use escapement_tickwave_view::{Cache, Recording};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, atomic::AtomicBool},
};

const P: Tick = 10;

/// `w`'s lines written as a recording's digital channels, in small blocks.
fn recording(w: &Wires) -> Vec<u8> {
    written(w, [w.end; 2], [0, 0])
}

/// `w`'s lines as a recording's digital channels in small blocks, their gaps the recording's,
/// each channel ending at `ends` and taken from source `sources`.
fn written(w: &Wires, ends: [Tick; 2], sources: [u32; 2]) -> Vec<u8> {
    let end = ends[0].max(ends[1]);
    // A recording of several sources states each channel's kept span.
    let kept = (sources[0] != sources[1]).then_some(Kept { from: 0, to: end });
    let (mut wr, mut file) = writer(sources, kept);
    let mut at: Vec<(Tick, usize, Option<Tick>)> = vec![];
    for (c, l) in w.lines.iter().enumerate() {
        let inside = |t: Tick| l.gaps.iter().any(|g| g.start <= t && t < g.end);
        let edges = l.edges.iter().filter(|&&t| !inside(t) && t < ends[c]);
        at.extend(edges.map(|&t| (t, c, None)));
        at.extend(l.gaps.iter().map(|g| (g.start, c, Some(g.end))));
    }
    at.sort();
    for (t, c, gap) in at {
        match gap {
            None => wr.edge(c, t).unwrap(),
            Some(to) => {
                let level = w.lines[c].level_at(to).unwrap();
                wr.gap(c, Gap { from: t, to, level }).unwrap();
            }
        }
    }
    file.extend(wr.take_output());
    file.extend(&wr.finish(end, &ends, Finish::default()).unwrap().tail);
    file
}

/// A writer of SWCLK and SWDIO from `sources`, each keeping `kept`, in small blocks, and the
/// bytes it wrote first.
fn writer(sources: [u32; 2], kept: Option<Kept>) -> (Writer, Vec<u8>) {
    let timebase = Ratio::integer(1_000_000).unwrap();
    let digital = |n: &str, source: u32| DeclaredChannel {
        name: n.into(),
        source_name: n.to_lowercase(),
        source,
        data: DeclaredKind::Digital { initial: false },
        shaping: Shaping {
            kept,
            ..Default::default()
        },
        overview_shift: None,
        codec: None,
        requires: Vec::new(),
    };
    let header = || SourceHeader::new(SourceFormat::Other("t".into()), timebase);
    let d = Declaration {
        timebase,
        channels: vec![digital("SWCLK", sources[0]), digital("SWDIO", sources[1])],
        derived: vec![],
        sources: (0..=sources[0].max(sources[1])).map(|_| header()).collect(),
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 6,
        requires: Vec::new(),
    };
    let options = WriteOptions {
        block_bytes: 64,
        ..WriteOptions::default()
    };
    let mut wr = Writer::new(d, options).unwrap();
    let file = wr.take_output();
    (wr, file)
}

fn all() -> Vec<Span> {
    vec![Span {
        start: Tick::MIN,
        end: Tick::MAX,
    }]
}

fn decoded(source: Box<dyn Source>, window: Tick) -> [Counts; 3] {
    decoded_with(HostOptions::default(), source, window)
}

fn decoded_with(options: HostOptions, source: Box<dyn Source>, window: Tick) -> [Counts; 3] {
    let mut h = Host::new(options);
    let s = (h.add(
        name("swd"),
        swd::definition(),
        InputSpec::Wire {
            source,
            local: None,
        },
        swd::Swd::factory(window),
        &[],
        all(),
    ))
    .unwrap();
    let a = (h.add(
        name("adi"),
        adiv5::adi_definition(),
        InputSpec::Upstream(adiv5::adi_subscription(s)),
        adiv5::Adi::factory(),
        &[],
        all(),
    ))
    .unwrap();
    let m = (h.add(
        name("mem"),
        adiv5::mem_definition(),
        InputSpec::Upstream(adiv5::mem_subscription(a)),
        adiv5::Mem::factory(),
        &[],
        all(),
    ))
    .unwrap();
    h.run_until(|_| false).unwrap();
    [s, a, m].map(|x| h.store(x).counts)
}

#[test]
fn swd_through_the_wire_source_matches_the_wires_served_directly() {
    let ts = vec![
        dp_read(0x0, 0x0BC1_1477),
        dp_write(0x8, 0x0000_0000),
        ap_write(0x0, 0x2300_0012),
        ap_write(0x4, 0x2000_0000),
        ap_read(0xC, 0),
        ap_read(0xC, 0x1111_1111),
        dp_read(0xC, 0x2222_2222),
    ];
    let (b, _) = bits(&ts);
    let w = wires(&b, P);
    let file = recording(&w);
    let rec = Arc::new(Recording::open(Arc::new(file), Cache::new(8 << 20)).unwrap());
    let want = decoded(Box::new(w.clone()), 64);
    assert!(want[2].closes > 0, "MEM-AP decoded something");
    for window in [37, 64, 1000, 1 << 20] {
        let source = WireSource::new(rec.clone(), vec![0, 1]).unwrap();
        assert_eq!(source.end(), Some(w.end));
        assert_eq!(
            decoded(Box::new(source), window),
            want,
            "windows of {window}"
        );
    }
    let mut reads = HostOptions::default();
    reads.window_ticks = 3;
    let source = WireSource::new(rec.clone(), vec![0, 1]).unwrap();
    assert_eq!(
        decoded_with(reads, Box::new(source), 1 << 20),
        want,
        "reads of 3 ticks"
    );
}

#[test]
fn a_read_holds_at_most_its_bound_of_ticks_and_the_next_carries_on_from_it() {
    let ts = vec![dp_read(0x0, 0x0BC1_1477), ap_read(0xC, 0x1111_1111)];
    let (b, _) = bits(&ts);
    let w = wires(&b, P);
    let rec = Arc::new(Recording::open(Arc::new(recording(&w)), Cache::new(8 << 20)).unwrap());
    let never = AtomicBool::new(false);
    let observed = |w: escapement_tickwave_decode::Window| -> Vec<_> {
        w.events.into_iter().filter(|e| e.at().is_some()).collect()
    };
    let mut whole = WireSource::new(rec.clone(), vec![0, 1]).unwrap();
    let want = observed(
        whole
            .window(0, Tick::MAX, usize::MAX, &never)
            .unwrap()
            .unwrap(),
    );
    let mut source = WireSource::new(rec, vec![0, 1]).unwrap();
    let (mut from, mut got) = (0, vec![]);
    loop {
        let r = source.window(from, Tick::MAX, 3, &never).unwrap().unwrap();
        let ticks: BTreeSet<Tick> = r.events.iter().filter_map(|e| e.at()).collect();
        assert!(ticks.len() <= 3, "{} ticks from {from}", ticks.len());
        assert!(ticks.iter().all(|&t| from <= t && t < r.upto));
        let (upto, end) = (r.upto, r.end);
        got.extend(observed(r));
        if end.is_some() {
            break;
        }
        assert!(upto > from, "a read moves on");
        from = upto;
    }
    assert_eq!(got, want);
    assert_eq!(source.seeks(), 1, "each read carries on from the last");
}

#[test]
fn a_checkpoint_key_follows_the_recordings_content_not_its_length() {
    let (b, _) = bits(&[dp_read(0x0, 0x0BC1_1477)]);
    let (c, _) = bits(&[dp_read(0x0, 0x2BA0_1477)]);
    let (wb, wc) = (wires(&b, P), wires(&c, P));
    assert_eq!(wb.end, wc.end, "the same length and timebase");
    let open = |w: &Wires| {
        let file = recording(w);
        let rec = Arc::new(Recording::open(Arc::new(file), Cache::new(8 << 20)).unwrap());
        WireSource::new(rec, vec![0, 1]).unwrap().binding()
    };
    assert_ne!(open(&wb), open(&wc), "other content");
    assert_eq!(open(&wb), open(&wb), "the same file opened again");
}

#[test]
fn a_port_left_out_is_not_read_and_the_others_keep_their_numbers() {
    let (b, _) = bits(&[dp_read(0x0, 0x0BC1_1477)]);
    let w = wires(&b, P);
    let rec = Arc::new(Recording::open(Arc::new(recording(&w)), Cache::new(8 << 20)).unwrap());
    let never = AtomicBool::new(false);
    let read = |ports: Vec<Option<usize>>| {
        let mut s = WireSource::with_ports(rec.clone(), ports).unwrap();
        let w = s.window(0, Tick::MAX, usize::MAX, &never).unwrap().unwrap();
        (s.ports(), s.binding(), w.events)
    };
    let port = |e: &Observed| match *e {
        Observed::Start { port, .. }
        | Observed::Edge { port, .. }
        | Observed::Known { port, .. }
        | Observed::Unknown { port, .. } => port,
        _ => u8::MAX,
    };
    let (both, key, all) = read(vec![Some(0), Some(1)]);
    let (bound, without, only) = read(vec![None, Some(1)]);
    assert_eq!((both, bound), (vec![true, true], vec![false, true]));
    let data: Vec<_> = all.iter().filter(|e| port(e) == 1).copied().collect();
    assert!(data.len() > 2, "SWDIO changes");
    assert_eq!(only, data, "port 1's observations, still numbered 1");
    assert_ne!(key, without, "a port left out is another binding");
}

fn open(file: Vec<u8>) -> Arc<Recording> {
    Arc::new(Recording::open(Arc::new(file), Cache::new(8 << 20)).unwrap())
}

/// SWD's items and lost input over `source`, as text: what a decoder made of it, compared whole.
fn swd_items(source: Box<dyn Source>) -> (String, Vec<Span>) {
    let mut h = Host::default();
    let s = (h.add(
        name("swd"),
        swd::definition(),
        InputSpec::Wire {
            source,
            local: None,
        },
        swd::Swd::factory(64),
        &[],
        all(),
    ))
    .unwrap();
    h.run_until(|_| false).unwrap();
    assert!(h.finished(s).is_some());
    (format!("{:?}", h.store(s).items), h.lost(s).to_vec())
}

fn session() -> Wires {
    let ts = vec![
        dp_read(0x0, 0x0BC1_1477),
        dp_write(0x8, 0x0000_0000),
        ap_write(0x4, 0x2000_0000),
        ap_read(0xC, 0x1111_1111),
        dp_read(0xC, 0x2222_2222),
    ];
    wires(&bits(&ts).0, P)
}

#[test]
fn channels_of_two_sources_are_refused() {
    let w = session();
    let rec = open(written(&w, [w.end; 2], [0, 1]));
    assert!(matches!(
        WireSource::new(rec, vec![0, 1]),
        Err(BindWire::Sources(0, 1))
    ));
}

#[test]
fn the_input_ends_where_the_first_channel_does() {
    let mut w = session();
    let short = w.end - 40 * P;
    let rec = open(written(&w, [w.end, short], [0, 0]));
    let source = WireSource::new(rec, vec![0, 1]).unwrap();
    assert_eq!(source.end(), Some(short));
    let got = swd_items(Box::new(source));
    w.end = short;
    assert_eq!(got, swd_items(Box::new(w)), "as the wires ending there");
}

#[test]
fn gaps_reach_the_decoder_as_unknown_then_known_input() {
    let mut w = session();
    // SWDIO unknown across most of the second transfer.
    let gap = Span {
        start: 10 + 60 * P,
        end: 10 + 80 * P,
    };
    w.lines[1].gaps.push(gap);
    let rec = open(recording(&w));
    let mut source = WireSource::new(rec.clone(), vec![0, 1]).unwrap();
    let never = AtomicBool::new(false);
    let all = source
        .window(0, Tick::MAX, usize::MAX, &never)
        .unwrap()
        .unwrap();
    let on = |want: fn(&Observed) -> bool| {
        all.events
            .iter()
            .filter(|e| want(e))
            .filter_map(|e| e.at())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        on(|e| matches!(e, Observed::Unknown { port: 1, .. })),
        vec![gap.start, w.end]
    );
    assert!(on(|e| matches!(e, Observed::Known { port: 1, .. })).contains(&gap.end));
    let (items, lost) = swd_items(Box::new(WireSource::new(rec, vec![0, 1]).unwrap()));
    assert!(!lost.is_empty(), "the gap is lost input");
    assert_eq!(
        (items, lost),
        swd_items(Box::new(w)),
        "as the wires served directly"
    );
}

#[test]
fn a_window_that_does_not_follow_on_from_the_last_seeks_and_reads_the_same() {
    let w = session();
    let rec = open(recording(&w));
    let never = AtomicBool::new(false);
    let read = |s: &mut WireSource, from, upto| {
        let w = s.window(from, upto, usize::MAX, &never).unwrap().unwrap();
        (w.events, w.upto, w.end)
    };
    let mut source = WireSource::new(rec.clone(), vec![0, 1]).unwrap();
    read(&mut source, 0, 200);
    let next = read(&mut source, 200, 400);
    assert_eq!(source.seeks(), 1, "the second carries on");
    let later = read(&mut source, 900, 1_100);
    let back = read(&mut source, 300, 500);
    assert_eq!(source.seeks(), 3, "each that does not carry on seeks");
    let fresh = |from, upto| {
        read(
            &mut WireSource::new(rec.clone(), vec![0, 1]).unwrap(),
            from,
            upto,
        )
    };
    assert_eq!(next, fresh(200, 400));
    assert_eq!(later, fresh(900, 1_100));
    assert_eq!(back, fresh(300, 500));
}

#[test]
fn a_live_recordings_window_ends_where_it_is_settled_and_its_input_has_no_end_yet() {
    let w = session();
    let (mut wr, mut file) = writer([0, 0], None);
    let mut edges: Vec<(Tick, usize)> = (w.lines.iter().enumerate())
        .flat_map(|(c, l)| l.edges.iter().map(move |&t| (t, c)))
        .collect();
    edges.sort();
    let half = w.end / 2;
    for &(t, c) in edges.iter().filter(|e| e.0 < half) {
        wr.edge(c, t).unwrap();
    }
    wr.flush(half).unwrap();
    file.extend(wr.take_output());
    let rec = Recording::live(wr.snapshot(), Arc::new(file), Cache::new(8 << 20)).unwrap();
    let mut source = WireSource::new(Arc::new(rec), vec![0, 1]).unwrap();
    assert_eq!(source.end(), None);
    let never = AtomicBool::new(false);
    let got = source
        .window(0, Tick::MAX, usize::MAX, &never)
        .unwrap()
        .unwrap();
    assert!(got.end.is_none(), "not finished");
    assert!(got.upto > 0 && got.upto <= half, "settled at {}", got.upto);
    let finished = open(recording(&w));
    let mut whole = WireSource::new(finished, vec![0, 1]).unwrap();
    let want = whole
        .window(0, got.upto, usize::MAX, &never)
        .unwrap()
        .unwrap();
    assert_eq!(
        got.events, want.events,
        "the finished recording's, up to there"
    );
}
