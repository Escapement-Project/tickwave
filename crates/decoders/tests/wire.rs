//! SWD read from a recording through the wire source, against the same wires served directly.

mod common;

use common::*;
use escapement_tickwave_decode::{
    Counts, Host, HostOptions, InputSpec, Source, Span, Tick, name, wire::WireSource,
};
use escapement_tickwave_decoders::{adiv5, swd};
use escapement_tickwave_format::{
    header::{Declaration, DeclaredChannel, DeclaredKind, SourceFormat, SourceHeader},
    model::Ratio,
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
    let timebase = Ratio::integer(1_000_000).unwrap();
    let digital = |n: &str| DeclaredChannel {
        name: n.into(),
        source_name: n.to_lowercase(),
        source: 0,
        data: DeclaredKind::Digital { initial: false },
        shaping: Default::default(),
        overview_shift: None,
        codec: None,
        requires: Vec::new(),
    };
    let d = Declaration {
        timebase,
        channels: vec![digital("SWCLK"), digital("SWDIO")],
        derived: vec![],
        sources: vec![SourceHeader::new(SourceFormat::Other("t".into()), timebase)],
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
    let mut file = wr.take_output();
    let mut edges: Vec<(Tick, usize)> = (w.lines.iter().enumerate())
        .flat_map(|(c, l)| l.edges.iter().map(move |&t| (t, c)))
        .collect();
    edges.sort();
    for (t, c) in edges {
        wr.edge(c, t).unwrap();
    }
    file.extend(wr.take_output());
    file.extend(
        &wr.finish(w.end, &[w.end, w.end], Finish::default())
            .unwrap()
            .tail,
    );
    file
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
        swd::Swd::factory(window, 1).unwrap(),
        all(),
    ))
    .unwrap();
    let a = (h.add(
        name("adi"),
        adiv5::adi_definition(),
        InputSpec::Upstream(adiv5::adi_subscription(s)),
        adiv5::Adi::factory(),
        all(),
    ))
    .unwrap();
    let m = (h.add(
        name("mem"),
        adiv5::mem_definition(),
        InputSpec::Upstream(adiv5::mem_subscription(a)),
        adiv5::Mem::factory(),
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
