//! An analog channel's samples lost before its first and after its last, as a view reads them:
//! from a finished file's header, and from a writer's snapshots while it writes.

#![cfg(feature = "live")]

use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use escapement_tickwave_format::header::{
    AnalogCoding, Declaration, DeclaredChannel, DeclaredKind, Derived, IntCoding, SourceFormat,
    SourceHeader,
};
use escapement_tickwave_format::model::Ratio;
use escapement_tickwave_format::writer::{Finish, WriteOptions, Writer};
use escapement_tickwave_view::{Bytes, Cache, Drawn, Query, Recording};

/// A file a writer still appends to.
#[derive(Default)]
struct Growing(Mutex<Vec<u8>>);

impl Bytes for Growing {
    fn size(&self) -> std::io::Result<u64> {
        Ok(self.0.lock().unwrap().len() as u64)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        let file = self.0.lock().unwrap();
        let at = offset as usize;
        buf.copy_from_slice(&file[at..at + buf.len()]);
        Ok(())
    }
}

const BEFORE: u64 = 3;
const AFTER: u64 = 5;
/// V, I, DIO0, then P = I × V; one past the last.
const V: usize = 0;
const I: usize = 1;
const DIO0: usize = 2;
const P: usize = 3;
const PAST: usize = 4;

fn declaration() -> Declaration {
    let timebase = Ratio::integer(1000).unwrap();
    let analog = |name: &str| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Analog {
            unit: String::new(),
            period: Ratio::integer(10).unwrap(),
            coding: AnalogCoding::Int(IntCoding::new(16, false)),
            significant: None,
        },
        shaping: Default::default(),
        overview_shift: None,
    };
    Declaration {
        timebase,
        channels: vec![
            analog("V"),
            analog("I"),
            DeclaredChannel {
                name: "DIO0".into(),
                source_name: "dio0".into(),
                source: 0,
                data: DeclaredKind::Digital { initial: false },
                shaping: Default::default(),
                overview_shift: None,
            },
        ],
        derived: vec![Derived {
            name: "P".into(),
            unit: String::new(),
            product: ["I".into(), "V".into()],
            significant: None,
        }],
        sources: vec![SourceHeader::new(
            SourceFormat::Other("crafted".into()),
            timebase,
        )],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 4,
    }
}

/// Each channel's `(lost_before, lost_after)`, one past the last included.
fn losses(r: &Recording) -> Vec<(u64, u64)> {
    (0..=PAST)
        .map(|c| (r.lost_before(c), r.lost_after(c)))
        .collect()
}

/// What `losses` gives with `before` and `after` on V, I and P.
fn expected(before: u64, after: u64) -> Vec<(u64, u64)> {
    (0..=PAST)
        .map(|c| match c {
            V | I | P => (before, after),
            DIO0 | PAST => (0, 0),
            _ => unreachable!(),
        })
        .collect()
}

#[test]
fn an_analog_channels_losses_at_either_end_read_live_and_finished() {
    let mut w = Writer::new(
        declaration(),
        WriteOptions {
            level: 3,
            block_bytes: 64,
            promise_with_items: true,
            ..WriteOptions::default()
        },
    )
    .unwrap();
    let file = Arc::new(Growing::default());
    let publish = |w: &mut Writer| {
        let snapshot = w.snapshot();
        file.0.lock().unwrap().extend(w.take_output());
        snapshot
    };
    // Before any run, nothing is lost; then the first run's 3 lost, places -3 to -1 from tick 30,
    // which a recording made afresh from each snapshot and one updated from the first agree on.
    let mut followed = Recording::live(publish(&mut w), file.clone(), Cache::new(1 << 20)).unwrap();
    assert_eq!(losses(&followed), expected(0, 0));
    w.run(V, 30, BEFORE).unwrap();
    w.run(I, 30, BEFORE).unwrap();
    for k in 0..40u64 {
        w.sample(V, 100 + k).unwrap();
        w.sample(I, 200 + k).unwrap();
        if k % 10 == 9 {
            w.flush(30 + 10 * (k as i64 + 1)).unwrap();
            let snapshot = publish(&mut w);
            let fresh = Recording::live(snapshot.clone(), file.clone(), Cache::new(1 << 20));
            followed.update(snapshot).unwrap();
            for (r, which) in [(&fresh.unwrap(), "fresh"), (&followed, "updated")] {
                // Only the finish states what is lost after the last sample.
                assert_eq!(losses(r), expected(BEFORE, 0), "{which}, after pair {k}");
            }
        }
    }
    // The last sample at place 39, tick 420; the 5 lost after it end at place 45, tick 480.
    let finished = w
        .finish(
            500,
            &[500],
            Finish {
                lost_after: BTreeMap::from([(V, AFTER), (I, AFTER)]),
                ..Finish::default()
            },
        )
        .unwrap();
    let mut bytes = file.0.lock().unwrap().clone();
    bytes.extend(finished.tail);
    let finished = Recording::open(Arc::new(bytes), Cache::new(1 << 20)).unwrap();
    assert_eq!(losses(&finished), expected(BEFORE, AFTER));
}

/// What a view draws of V's query over `extent` in columns of 10 ticks: its lost spans, and the
/// ticks of its pale columns.
fn pale(r: &Recording, extent: std::ops::Range<i64>) -> (Vec<std::ops::Range<i64>>, Vec<i64>) {
    let cancel = AtomicBool::new(false);
    let columns = ((extent.end - extent.start) / 10) as u32;
    let a = r
        .query(
            &Query {
                window: extent.clone(),
                columns,
                channels: vec![V, P],
                ..Query::default()
            },
            &cancel,
        )
        .unwrap();
    let Drawn::Analog { columns, lost, .. } = &a.channels[0] else {
        panic!("V is analog")
    };
    let Drawn::Analog { lost: p, .. } = &a.channels[1] else {
        panic!("P is analog")
    };
    assert_eq!(lost, p, "P's losses are V's");
    let ticks = (0..a.grid.count)
        .filter(|&k| columns.pale[k])
        .map(|k| (a.grid.x0 + k as f64 * a.grid.per) as i64)
        .filter(|t| extent.contains(t))
        .collect();
    (lost.clone(), ticks)
}

/// The spans lost before V's first sample and after its last are pale as a break's are, live and
/// finished; the extent reaches over them, so statistics count them unmeasured.
#[test]
fn the_spans_lost_at_either_end_are_pale_and_unmeasured() {
    let mut w = Writer::new(
        declaration(),
        WriteOptions {
            level: 3,
            block_bytes: 64,
            promise_with_items: true,
            ..WriteOptions::default()
        },
    )
    .unwrap();
    let file = Arc::new(Growing::default());
    file.0.lock().unwrap().extend(w.take_output());
    w.run(V, 30, BEFORE).unwrap();
    w.run(I, 30, BEFORE).unwrap();
    for k in 0..40u64 {
        w.sample(V, 100 + k).unwrap();
        w.sample(I, 200 + k).unwrap();
    }
    w.flush(430).unwrap();
    let snapshot = w.snapshot();
    file.0.lock().unwrap().extend(w.take_output());
    // Places -3 to -1 before the first sample at 30: ticks 0 to 30, three columns.
    let live = Recording::live(snapshot, file.clone(), Cache::new(1 << 20)).unwrap();
    assert_eq!(live.extent(), Some(0..430));
    let (lost, pale_at) = pale(&live, 0..430);
    assert_eq!(
        (lost.len(), lost.first(), pale_at),
        (1, Some(&(0..30)), vec![0, 10, 20])
    );
    // The 5 lost after the last, places 40 to 44: ticks 430 to 480.
    let finished = w
        .finish(
            500,
            &[500],
            Finish {
                lost_after: BTreeMap::from([(V, AFTER), (I, AFTER)]),
                ..Finish::default()
            },
        )
        .unwrap();
    let mut bytes = file.0.lock().unwrap().clone();
    bytes.extend(finished.tail);
    let finished = Recording::open(Arc::new(bytes), Cache::new(1 << 20)).unwrap();
    let extent = finished.extent().unwrap();
    assert_eq!(extent, 0..480);
    assert_eq!(
        pale(&finished, 0..480),
        (
            vec![0..30, 430..480],
            vec![0, 10, 20, 430, 440, 450, 460, 470]
        )
    );
    // 40 samples of 10 ticks measured; the 30 ticks before and 50 after are not.
    let s = finished
        .statistics(0..500, &AtomicBool::new(false))
        .unwrap();
    for c in [V, P] {
        let v = s.channels[c].as_ref().unwrap();
        assert_eq!(
            (v.samples, v.measured, v.unmeasured(&s.interval, &extent)),
            (40, 400, 80),
            "channel {c}"
        );
    }
}
