//! Moved and trimmed channels in the file: `offset`, `offset_kind` and `kept` through the
//! declaration and the footer, in the content id, and the reader's checks of `kept` against the
//! data.

use escapement_tickwave_format::decode::Footer;
use escapement_tickwave_format::encode::{Options, encode};
use escapement_tickwave_format::header::{ChannelKind, ContentId};
use escapement_tickwave_format::model::{
    Analog, Channel, ChannelData, Digital, Gap, Kept, Offset, Ratio, Recording, Samples, Shaping,
    Source, SourceFormat,
};
use escapement_tickwave_format::reader::{Ending, Event, Reader};

mod common;
use common::patched;

const LENGTH: i64 = 5000;

fn offset(ticks: i64) -> Option<Offset> {
    Some(Offset {
        ticks,
        kind: Offset::DELAY.into(),
    })
}

fn kept(from: i64, to: i64) -> Option<Kept> {
    Some(Kept { from, to })
}

/// Channel C, cut from tick 100 to 4000 and moved by -40: a gap from tick 0 to 150 where an
/// instrument gap the cut lies in joins the cut, then edges, and the instrument's end at 3000,
/// before the cut's. Channel D, moved only, so kept from tick 0 to the length, as every channel
/// states a kept span once one does. Analog V, cut from 200 to 3500, its first sample at 201 and
/// its last at 3401: `first` above `from`. With `shaped` false, no channel states anything.
fn recording(shaped: bool) -> Recording {
    let shaping = |s: Shaping| if shaped { s } else { Shaping::default() };
    Recording {
        timebase: Ratio::integer(1_000_000).unwrap(),
        length: LENGTH,
        channels: vec![
            Channel {
                name: "C".into(),
                source_name: "c".into(),
                data: ChannelData::Digital(Digital {
                    initial: false,
                    end: 3000,
                    edges: Box::new([300, 400, 2000].into_iter()),
                    gaps: vec![Gap {
                        from: 0,
                        to: 150,
                        level: true,
                    }],
                }),
                shaping: shaping(Shaping {
                    offset: offset(-40),
                    kept: kept(100, 4000),
                }),
            },
            Channel {
                name: "D".into(),
                source_name: "d".into(),
                data: ChannelData::Digital(Digital {
                    initial: true,
                    end: LENGTH,
                    edges: Box::new([10, 20].into_iter()),
                    gaps: Vec::new(),
                }),
                shaping: shaping(Shaping {
                    offset: offset(1_000_000_000_000_000),
                    kept: kept(0, LENGTH),
                }),
            },
            Channel {
                name: "V".into(),
                source_name: "v".into(),
                data: ChannelData::Analog(Analog {
                    unit: "V".into(),
                    first: 201,
                    period: Ratio::integer(100).unwrap(),
                    count: 33,
                    breaks: Vec::new(),
                    samples: Samples::F32(Box::new((0..33).map(|i| i as f32))),
                }),
                shaping: shaping(Shaping {
                    offset: None,
                    kept: kept(200, 3500),
                }),
            },
        ],
        source: Source::new(SourceFormat::Other("test".into())),
    }
}

fn file(shaped: bool) -> Vec<u8> {
    encode(recording(shaped), &Options::default()).unwrap()
}

fn shapings() -> [Shaping; 3] {
    [
        Shaping {
            offset: offset(-40),
            kept: kept(100, 4000),
        },
        Shaping {
            offset: offset(1_000_000_000_000_000),
            kept: kept(0, LENGTH),
        },
        Shaping {
            offset: None,
            kept: kept(200, 3500),
        },
    ]
}

#[test]
fn offsets_and_kept_spans_read_back_from_the_declaration_and_the_footer() {
    let file = file(true);
    let head = Footer::parse_file(&file).unwrap();
    let footer: Vec<Shaping> = head
        .header
        .channels
        .iter()
        .map(|c| c.shaping.clone())
        .collect();
    assert_eq!(footer, shapings());

    let mut reader = Reader::default();
    reader.feed(&file);
    let Some(Event::Declaration(d)) = reader.next_event().unwrap() else {
        panic!("the declaration comes first");
    };
    let declared: Vec<Shaping> = d.channels.iter().map(|c| c.shaping.clone()).collect();
    assert_eq!(declared, shapings());
    // And the whole file reads in order, its footer checked against the declaration.
    while reader.next_event().unwrap().is_some() {}
    assert!(matches!(reader.end().unwrap(), Ending::Complete));

    let unshaped = Footer::parse_file(&self::file(false)).unwrap();
    assert!(
        unshaped
            .header
            .channels
            .iter()
            .all(|c| c.shaping.is_whole())
    );
}

/// FORMAT.md's content id, computed here from its text rather than by the crate's hasher.
fn content_id_by_hand(shaped: bool) -> ContentId {
    fn s(h: &mut blake3::Hasher, text: &str) {
        h.update(&(text.len() as u64).to_le_bytes());
        h.update(text.as_bytes());
    }
    fn flags(h: &mut blake3::Hasher, shaping: &Shaping) {
        let mut flags = 0u8;
        if shaping.offset.is_some() {
            flags |= 1;
        }
        if shaping.kept.is_some() {
            flags |= 2;
        }
        if flags == 0 {
            return;
        }
        h.update(&[flags]);
        if let Some(o) = &shaping.offset {
            h.update(&o.ticks.to_le_bytes());
            s(h, &o.kind);
        }
        if let Some(k) = &shaping.kept {
            h.update(&k.from.to_le_bytes());
            h.update(&k.to.to_le_bytes());
        }
    }
    let shapings = if shaped {
        shapings()
    } else {
        Default::default()
    };
    let mut digests = Vec::new();
    for (name, initial, edges, end, gaps, shaping) in [
        (
            "C",
            false,
            &[300i64, 400, 2000][..],
            3000i64,
            &[(0i64, 150i64, true)][..],
            &shapings[0],
        ),
        ("D", true, &[10, 20][..], LENGTH, &[][..], &shapings[1]),
    ] {
        let mut h = blake3::Hasher::new();
        s(&mut h, name);
        s(&mut h, "digital");
        h.update(&[u8::from(initial)]);
        flags(&mut h, shaping);
        for e in edges {
            h.update(&e.to_le_bytes());
        }
        h.update(&end.to_le_bytes());
        h.update(&(gaps.len() as u64).to_le_bytes());
        for &(from, to, level) in gaps {
            h.update(&from.to_le_bytes());
            h.update(&to.to_le_bytes());
            h.update(&[u8::from(level)]);
        }
        digests.push(h.finalize());
    }
    let mut h = blake3::Hasher::new();
    s(&mut h, "V");
    s(&mut h, "analog");
    s(&mut h, "V");
    h.update(&100u64.to_le_bytes());
    h.update(&1u64.to_le_bytes());
    flags(&mut h, &shapings[2]);
    for i in 0..33 {
        h.update(&(i as f32).to_bits().to_le_bytes());
    }
    h.update(&201i64.to_le_bytes());
    h.update(&33u64.to_le_bytes());
    h.update(&0u64.to_le_bytes());
    digests.push(h.finalize());

    let mut h = blake3::Hasher::new();
    h.update(&1_000_000u64.to_le_bytes());
    h.update(&1u64.to_le_bytes());
    h.update(&LENGTH.to_le_bytes());
    for d in &digests {
        h.update(d.as_bytes());
    }
    ContentId::new(h.finalize())
}

#[test]
fn the_content_id_is_format_mds_and_pins_each_stated_field() {
    let id = |shaped| Footer::parse_file(&file(shaped)).unwrap().header.content_id;
    assert_eq!(id(false), content_id_by_hand(false));
    assert_eq!(id(true), content_id_by_hand(true));
    assert_ne!(id(true), id(false));

    // Each field alone changes the id. Kept spans go from every channel or none.
    let with = |edit: &dyn Fn(&mut [Channel])| {
        let mut rec = recording(true);
        edit(&mut rec.channels);
        Footer::parse_file(&encode(rec, &Options::default()).unwrap())
            .unwrap()
            .header
            .content_id
    };
    type Edit<'a> = (&'a str, &'a dyn Fn(&mut [Channel]));
    let edits: [Edit; 6] = [
        ("offset", &|c| {
            c[0].shaping.offset.as_mut().unwrap().ticks = -41
        }),
        ("offset_kind", &|c| {
            c[0].shaping.offset.as_mut().unwrap().kind = "align".into()
        }),
        ("kept from", &|c| {
            c[0].shaping.kept.as_mut().unwrap().from = 99
        }),
        ("kept to", &|c| {
            c[0].shaping.kept.as_mut().unwrap().to = 4001
        }),
        ("no offset", &|c| c[0].shaping.offset = None),
        ("V's kept to", &|c| {
            c[2].shaping.kept.as_mut().unwrap().to = 3499
        }),
    ];
    let mut seen = vec![id(true)];
    for (what, edit) in edits {
        let changed = with(edit);
        assert!(!seen.contains(&changed), "{what} leaves the id as it was");
        seen.push(changed);
    }
}

#[test]
fn a_kept_span_the_data_contradicts_is_refused_and_named() {
    let file = file(true);
    // Each patch keeps its text's length, so every offset in the file stays right.
    for (from, to, why) in [
        (
            r#""offset_kind":"delay""#,
            r#""offset_kinx":"delay""#,
            "channel C states an offset without an offset_kind",
        ),
        (
            r#""offset":-40"#,
            r#""offsex":-40"#,
            "channel C states an offset_kind without an offset",
        ),
        (
            r#""offset":1000000000000000"#,
            r#""offset":9007199254740993"#,
            "channel D's offset is past 2^53",
        ),
        (
            r#""kept":[100,4000]"#,
            r#""kept":[100,6000]"#,
            "channel C is kept from 100 to 6000, which is empty or leaves the capture's 0 to 5000",
        ),
        (
            r#""kept":[100,4000]"#,
            r#""kept":[-10,4000]"#,
            "channel C is kept from -10 to 4000",
        ),
        (
            r#""kept":[100,4000]"#,
            r#""kept":[4000,100]"#,
            "channel C is kept from 4000 to 100",
        ),
        (
            r#""kept":[100,4000]"#,
            r#""kept":[151,4000]"#,
            "channel C is kept from 151, but no gap from tick 0 covers the ticks before",
        ),
        (
            r#""gaps":[[0,150,true]]"#,
            r#""gaps":[[1,150,true]]"#,
            "channel C is kept from 100, but no gap from tick 0 covers the ticks before",
        ),
        (
            r#""kept":[100,4000]"#,
            r#""kept":[100,2999]"#,
            "channel C is known to 3000, past where it is kept to, 2999",
        ),
        (
            r#""kept":[200,3500]"#,
            r#""kept":[202,3500]"#,
            "channel V is kept from 202 to 3500, but its samples run from 201 to 3401",
        ),
        (
            r#""kept":[200,3500]"#,
            r#""kept":[200,3401]"#,
            "channel V is kept from 200 to 3401, but its samples run from 201 to 3401",
        ),
    ] {
        let bad = patched(&file, from, to);
        let err = Footer::parse_file(&bad).expect_err(to).to_string();
        assert!(err.contains(why), "{to}: {err}");
    }

    // The bounds themselves read: a gap from 0 reaching exactly the cut, an end exactly at the
    // kept span's, and V's first sample on its `from`, its last just before its `to`.
    for (from, to) in [
        (r#""kept":[100,4000]"#, r#""kept":[150,4000]"#),
        (r#""kept":[100,4000]"#, r#""kept":[100,3000]"#),
        (r#""kept":[200,3500]"#, r#""kept":[201,3500]"#),
        (r#""kept":[200,3500]"#, r#""kept":[200,3402]"#),
    ] {
        Footer::parse_file(&patched(&file, from, to)).unwrap_or_else(|e| panic!("{to}: {e}"));
    }
}

/// The inequalities FORMAT.md allows: a cut next to an instrument's gap, which the gap from tick
/// 0 then runs past, and an instrument that stops before the cut's end. The file `recording`
/// writes has both, on channel C.
#[test]
fn a_valid_file_may_join_a_cut_to_the_instruments_gap_and_end_before_the_cut() {
    let head = Footer::parse_file(&file(true)).unwrap();
    let c = &head.header.channels[0];
    let ChannelKind::Digital { end, gaps, .. } = &c.data else {
        panic!("C is digital");
    };
    let kept = c.shaping.kept.unwrap();
    assert!(gaps[0].from == 0 && gaps[0].to > kept.from, "{gaps:?}");
    assert!(*end < kept.to, "{end}");
    let ChannelKind::Analog { first, .. } = &head.header.channels[2].data else {
        panic!("V is analog");
    };
    assert!(*first > head.header.channels[2].shaping.kept.unwrap().from);
}

/// A channel never known: its gap from tick 0 reached its end, which a writer then sets to 0 and
/// lists no gap. A kept span from above 0 is then covered.
#[test]
fn a_channel_cut_where_it_was_never_known_reads() {
    let mut rec = recording(true);
    let ChannelData::Digital(d) = &mut rec.channels[0].data else {
        unreachable!()
    };
    d.end = 0;
    d.edges = Box::new(std::iter::empty());
    d.gaps.clear();
    let file = encode(rec, &Options::default()).unwrap();
    let head = Footer::parse_file(&file).unwrap();
    assert!(matches!(
        head.header.channels[0].data,
        ChannelKind::Digital { end: 0, .. }
    ));
    // With no gap or end to check, only the span's own order refuses this one.
    let bad = patched(&file, r#""kept":[100,4000]"#, r#""kept":[4000,100]"#);
    let err = Footer::parse_file(&bad).expect_err("refused").to_string();
    assert!(
        err.contains("channel C is kept from 4000 to 100, which is empty"),
        "{err}"
    );
}

/// C and V of `recording`, V kept from `v_from`: C is kept from 100, so only V can start at 0.
fn without_d(v_from: i64) -> Recording {
    let mut rec = recording(true);
    rec.channels.remove(1);
    rec.channels[1].shaping.kept = kept(v_from, 3500);
    rec
}

#[test]
fn every_channel_states_a_kept_span_or_none_does_and_one_starts_at_tick_0() {
    // All or none: one channel's span taken away.
    let file = file(true);
    for (from, to, channel) in [
        (r#""kept":[0,5000]"#, r#""kepx":[0,5000]"#, "D"),
        (r#""kept":[100,4000]"#, r#""kepx":[100,4000]"#, "C"),
    ] {
        let err = Footer::parse_file(&patched(&file, from, to))
            .expect_err(to)
            .to_string();
        assert!(
            err.contains(&format!(
                "channel {channel} states no kept span, but other channels do: a recording \
                 states one on every channel or on none"
            )),
            "{to}: {err}"
        );
    }

    // One starts at 0: V's span moved off tick 0 leaves none there. V's samples, from 201, and
    // C's gap still fit.
    let file = encode(without_d(0), &Options::default()).unwrap();
    Footer::parse_file(&file).unwrap();
    let bad = patched(&file, r#""kept":[0,3500]"#, r#""kept":[9,3500]"#);
    let err = Footer::parse_file(&bad).expect_err("refused").to_string();
    assert!(
        err.contains("no channel is kept from tick 0, where the recording starts"),
        "{err}"
    );
    // A writer refuses to write one.
    let err = encode(without_d(200), &Options::default())
        .expect_err("refused")
        .to_string();
    assert!(
        err.contains("no channel is kept from tick 0, where the recording starts"),
        "{err}"
    );

    // Every channel kept with one from 0 reads (above), and so does a file that states nothing.
    Footer::parse_file(&self::file(false)).unwrap();

    // A moved channel needs every channel's span: an offset with none anywhere is refused, by a
    // reader and by a writer, naming the first channel moved.
    let why = "channel C is moved, but no channel states a kept span: a recording with a channel \
               moved states one on every channel";
    let mut bad = self::file(true);
    for kept in [r#"[100,4000]"#, r#"[0,5000]"#, r#"[200,3500]"#] {
        bad = patched(
            &bad,
            &format!(r#""kept":{kept}"#),
            &format!(r#""kepx":{kept}"#),
        );
    }
    let err = Footer::parse_file(&bad).expect_err("refused").to_string();
    assert!(err.contains(why), "{err}");
    let mut rec = recording(true);
    rec.channels.iter_mut().for_each(|c| c.shaping.kept = None);
    let err = encode(rec, &Options::default())
        .expect_err("refused")
        .to_string();
    assert!(err.contains(why), "{err}");
}

/// Repair refuses a moved or trimmed recording wherever it was cut off: its kept spans were
/// declared for the whole recording. A whole recording cut off at the same bytes repairs, and cut
/// in its trailer it comes back with its own content id.
#[test]
fn a_moved_or_trimmed_recording_cut_off_is_not_repaired() {
    use escapement_tickwave_format::repair::{RepairError, repair};
    let small = Options {
        block_bytes: 8,
        ..Options::default()
    };
    let shaped = encode(recording(true), &small).unwrap();
    // C known to the length, as repair takes every digital channel to be.
    let mut rec = recording(false);
    let ChannelData::Digital(c) = &mut rec.channels[0].data else {
        unreachable!()
    };
    c.end = LENGTH;
    let whole = encode(rec, &small).unwrap();
    assert!(Footer::parse_file(&shaped).unwrap().rows.len() > 4);
    for percent in [30, 50, 70, 90, 100] {
        let at = |file: &[u8]| (file.len() * percent / 100).min(file.len() - 1);
        let err = match repair(&shaped[..at(&shaped)], 3) {
            Ok(_) => panic!("cut at {percent} % repaired"),
            Err(e) => e,
        };
        assert!(matches!(&err, RepairError::Shaped(c) if c == "C"), "{err}");
        assert_eq!(
            err.to_string(),
            "channel C was moved or trimmed when the recording was converted, so it isn't \
             repaired: convert it again from its source"
        );

        let cut = &whole[..at(&whole)];
        let repaired = repair(cut, 3).unwrap_or_else(|e| panic!("cut at {percent} %: {e}"));
        let mut file = cut[..repaired.keep as usize].to_vec();
        file.extend_from_slice(&repaired.tail);
        let head = Footer::parse_file(&file).unwrap();
        assert_eq!(head.header, repaired.header);
        if percent == 100 {
            let id = Footer::parse_file(&whole).unwrap().header.content_id;
            assert_eq!(head.header.content_id, id);
        }
    }
}
