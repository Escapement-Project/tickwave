//! Inputs a reader must refuse: frames that are not exactly one checksummed zstd frame, codes wider
//! than their coding, and headers whose extents leave the capture.

use escapement_tickwave_format::decode::{
    DecodeError, Footer, Index, MAX_DECODED, inflate, inflate_limited,
};
use escapement_tickwave_format::encode::{Options, encode};
use escapement_tickwave_format::header::AnalogCoding;
use escapement_tickwave_format::model::{
    Analog, Channel, ChannelData, Digital, Ratio, Recording, Samples, Source, SourceFormat,
};
use zstd::zstd_safe::CParameter;

mod common;
use common::{patched, rows_edited};

/// Bytes zstd cannot shrink, so it stores them in a raw block: a flipped byte changes the content
/// without breaking the frame, and only the checksum can catch it.
fn noise(n: usize) -> Vec<u8> {
    let mut x = 0x2545_f491_4f6c_dd1du64;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

fn frame(raw: &[u8]) -> Vec<u8> {
    let mut c = zstd::bulk::Compressor::new(3).unwrap();
    c.set_parameter(CParameter::ChecksumFlag(true)).unwrap();
    c.compress(raw).unwrap()
}

/// A skippable frame: magic 0x184D2A50, a length, then that many bytes.
fn skippable(content: &[u8]) -> Vec<u8> {
    let mut out = 0x184D_2A50u32.to_le_bytes().to_vec();
    out.extend_from_slice(&(content.len() as u32).to_le_bytes());
    out.extend_from_slice(content);
    out
}

fn refused(bytes: &[u8], len: usize, why: &str) {
    match inflate(bytes, len) {
        Ok(_) => panic!("{why}: accepted"),
        Err(e) => eprintln!("{why}: {e}"),
    }
}

#[test]
fn only_one_checksummed_frame_and_nothing_after_it() {
    let raw = noise(1000);
    let good = frame(&raw);
    assert_eq!(inflate(&good, raw.len()).unwrap(), raw);

    let mut flipped = good.clone();
    let mid = flipped.len() / 2;
    flipped[mid] ^= 0x10;
    refused(&flipped, raw.len(), "a flipped byte");

    let empty = frame(&[]);
    // The case a decoder of every frame lets through: the empty frame's checksum is the last one
    // read, and it matches.
    refused(
        &[flipped.clone(), empty.clone()].concat(),
        raw.len(),
        "a flipped byte, then an empty frame",
    );
    refused(
        &[good.clone(), empty].concat(),
        raw.len(),
        "an empty frame after",
    );
    refused(
        &[good.clone(), skippable(b"x")].concat(),
        raw.len(),
        "a skippable frame after",
    );
    refused(
        &[skippable(b"x"), good.clone()].concat(),
        raw.len(),
        "a skippable frame before",
    );
    refused(&skippable(&raw), raw.len(), "a skippable frame alone");
    // libzstd reads a skippable frame as holding nothing, and this one's length has bit 2 set, as
    // a checksum flag would be to a reader that skips the magic number.
    refused(
        &skippable(b"abcd"),
        0,
        "a skippable frame alone, said to hold nothing",
    );
    refused(
        &[good.clone(), vec![0]].concat(),
        raw.len(),
        "one byte after",
    );
    refused(&good[..good.len() - 1], raw.len(), "the checksum cut short");
    refused(&good, raw.len() - 1, "more bytes than the index says");
    refused(&good, raw.len() + 1, "fewer bytes than the index says");

    let mut plain = zstd::bulk::Compressor::new(3).unwrap();
    plain
        .set_parameter(CParameter::ChecksumFlag(false))
        .unwrap();
    refused(&plain.compress(&raw).unwrap(), raw.len(), "no checksum");
}

#[test]
fn a_frame_that_names_a_dictionary_is_refused() {
    let samples: Vec<Vec<u8>> = (0..200)
        .map(|i| {
            format!(
                "{{\"edge\": {i}, \"channel\": \"SWCLK\", \"level\": {}}}",
                i % 2
            )
            .into_bytes()
        })
        .collect();
    let dict = zstd::dict::from_samples(&samples, 1024).unwrap();
    let mut c = zstd::bulk::Compressor::with_dictionary(3, &dict).unwrap();
    c.set_parameter(CParameter::ChecksumFlag(true)).unwrap();
    let raw = samples.concat();
    let with_dict = c.compress(&raw).unwrap();
    // libzstd itself reads it back with the dictionary, so only the dictionary is at fault.
    let back = zstd::bulk::Decompressor::with_dictionary(&dict)
        .unwrap()
        .decompress(&with_dict, raw.len())
        .unwrap();
    assert_eq!(back, raw);
    let e = inflate(&with_dict, raw.len()).unwrap_err().to_string();
    assert!(e.contains("names a dictionary"), "{e}");
}

fn analog(coding_bits: Option<u8>, values: Vec<f32>) -> (Recording, Options) {
    let count = values.len() as u64;
    let rec = Recording {
        timebase: Ratio::integer(1_000_000_000).unwrap(),
        // Ticks 0..=10^9: the last edge and the eleventh sample sit on the final tick.
        length: 1_000_000_001,
        channels: vec![
            Channel {
                name: "C".into(),
                source_name: "c".into(),
                source: 0,
                data: ChannelData::Digital(Digital {
                    initial: false,
                    end: 1_000_000_001,
                    edges: Box::new([5, 1_000_000_000].into_iter()),
                    count: 2,
                    gaps: Vec::new(),
                }),
                shaping: Default::default(),
            },
            Channel {
                name: "V".into(),
                source_name: "v".into(),
                source: 0,
                data: ChannelData::Analog(Analog {
                    significant: None,
                    unit: "V".into(),
                    first: 0,
                    period: Ratio::integer(100_000_000).unwrap(),
                    count,
                    breaks: Vec::new(),
                    lost_before: 0,
                    lost_after: 0,
                    samples: Samples::F32(Box::new(values.into_iter())),
                    phase: None,
                }),
                shaping: Default::default(),
            },
        ],
        derived: Vec::new(),
        sources: vec![Source::new(
            SourceFormat::Other("test".into()),
            Ratio::integer(1_000_000_000).unwrap(),
        )],
    };
    let options = Options {
        analog: vec![
            None,
            coding_bits.map(|bits| AnalogCoding::Codes {
                bits,
                scale: 1.0,
                offset: 0.0,
            }),
        ],
        ..Options::default()
    };
    (rec, options)
}

#[test]
fn a_code_wider_than_its_coding_is_refused() {
    // Eleven samples, one a 16-bit code; the header then claims 12 bits.
    let mut values: Vec<f32> = (0..10).map(|i| i as f32).collect();
    values.push(40_000.0);
    let (rec, options) = analog(Some(16), values);
    let file = encode(rec, &options).unwrap();
    let decode = |file: &[u8]| Index::of_file(file)?.block_in(file, 1, 0);
    decode(&file).unwrap();
    let narrowed = patched(&file, r#""bits":16"#, r#""bits":12"#);
    let err = decode(&narrowed).expect_err("refused");
    assert!(
        err.to_string().contains("code 40000 is wider than 12 bits"),
        "{err}"
    );
}

#[test]
fn a_header_reaching_past_the_capture_is_refused() {
    let (rec, options) = analog(None, vec![0.0; 11]);
    let file = encode(rec, &options).unwrap();
    Footer::parse_file(&file).unwrap();
    for (from, to, why) in [
        (
            r#""end":1000000001"#,
            r#""end":9000000001"#,
            "ends outside the capture",
        ),
        (
            r#""first":0"#,
            r#""first":9"#,
            "samples lie outside the capture",
        ),
    ] {
        let err = Footer::parse_file(&patched(&file, from, to)).expect_err("refused");
        assert!(err.to_string().contains(why), "{err}");
    }
    let (rec, options) = analog(Some(16), vec![0.0; 11]);
    let file = encode(rec, &options).unwrap();
    let err =
        Footer::parse_file(&patched(&file, r#""bits":16"#, r#""bits":17"#)).expect_err("refused");
    assert!(err.to_string().contains("codes are 17 bits"), "{err}");
}

#[test]
fn a_block_starting_at_the_wrong_level_is_refused() {
    use escapement_tickwave_format::header::LEVEL_BIT;
    // Small blocks, so channel C's second edge starts a block of its own at the level after one
    // edge.
    let (rec, options) = analog(None, vec![0.0; 11]);
    let file = encode(
        rec,
        &Options {
            block_bytes: 1,
            ..options
        },
    )
    .unwrap();
    let blocks = Index::of_file(&file).unwrap().blocks[0].len();
    assert!(blocks >= 2);
    for block in [0, blocks - 1] {
        let bad = rows_edited(&file, 0, |rows| rows[block].word ^= LEVEL_BIT);
        let err = Index::of_file(&bad).expect_err("refused");
        assert!(
            err.to_string()
                .contains(&format!("block {block} starts at the wrong level")),
            "{err}"
        );
    }
}

#[test]
fn a_file_without_its_trailer_last_is_refused() {
    let (rec, options) = analog(None, vec![1.0; 10]);
    let file = encode(rec, &options).unwrap();
    assert!(Footer::parse_file(&file).is_ok());
    let longer = [&file[..], &[0]].concat();
    for (what, bytes) in [
        ("cut short", &file[..file.len() - 5]),
        ("cut near the start", &file[..40]),
        ("one byte after", &longer[..]),
    ] {
        let err = Footer::parse_file(bytes).expect_err(what);
        assert!(matches!(err, DecodeError::Unfinished), "{what}: {err}");
    }
    // A trailer whose footer does not end where it starts.
    let mut moved = file.clone();
    let t = moved.len() - 24;
    let offset = u64::from_le_bytes(moved[t..t + 8].try_into().unwrap()) + 1;
    moved[t..t + 8].copy_from_slice(&offset.to_le_bytes());
    let err = Footer::parse_file(&moved).expect_err("moved");
    assert!(
        err.to_string()
            .contains("does not end where the trailer starts"),
        "{err}"
    );
    // A footer that does not check.
    let mut spoiled = file.clone();
    spoiled[t - 1] ^= 1;
    let err = Footer::parse_file(&spoiled).expect_err("spoiled");
    assert!(
        err.to_string().contains("the footer record is not whole"),
        "{err}"
    );
}

#[test]
fn a_block_past_the_index_is_refused() {
    let (rec, options) = analog(None, vec![1.0; 10]);
    let file = encode(rec, &options).unwrap();
    let index = Index::of_file(&file).unwrap();
    let n = index.blocks[1].len();
    let err = index.block_in(&file, 1, n).expect_err("refused");
    assert!(err.to_string().contains(&format!("no block {n}")), "{err}");
    let err = index.overview_in(&file, 1, 0, 0).expect_err("refused");
    assert!(err.to_string().contains("no row 0"), "{err}");
}

#[test]
fn a_frame_said_to_decode_past_the_limit_is_refused_before_anything_is_allocated() {
    let raw = noise(1000);
    let good = frame(&raw);
    assert_eq!(inflate_limited(&good, raw.len(), raw.len()).unwrap(), raw);
    let err = inflate_limited(&good, raw.len(), raw.len() - 1).unwrap_err();
    assert!(
        err.to_string()
            .contains("more than this reader holds (999)"),
        "{err}"
    );
    // Above the default, whatever the frame holds: a row stating 2 GiB costs nothing to refuse.
    let err = inflate(&good, MAX_DECODED + 1).unwrap_err();
    assert!(
        err.to_string().contains("more than this reader holds"),
        "{err}"
    );
    // A strip that decodes to more than the reader is set to hold.
    let (rec, options) = analog(None, vec![1.0; 10]);
    let file = encode(rec, &options).unwrap();
    let head = Footer::parse_file(&file).unwrap();
    // V's strip is one bucket, 16 bytes: an f32 minimum and maximum and an f64 sum.
    let limited = |f: &[u8], n: usize| inflate_limited(f, n, 15);
    assert!(
        head.strip_with(0, limited).is_ok(),
        "a digital strip of a few bytes"
    );
    let err = head.strip_with(1, limited).expect_err("refused");
    assert!(
        err.to_string().contains("more than this reader holds (15)"),
        "{err}"
    );
}

#[test]
fn the_encoder_refuses_what_the_reader_would_and_says_which_channel() {
    let refused =
        |rec: Recording, options: &Options| encode(rec, options).expect_err("refused").to_string();
    let (mut rec, options) = analog(None, vec![1.0; 10]);
    rec.length = -5;
    assert_eq!(
        refused(rec, &options),
        "the length -5 does not lie past every item and block in, the last at 1000000000"
    );
    let (mut rec, options) = analog(None, vec![]);
    rec.length = 0;
    rec.channels.truncate(1);
    if let ChannelData::Digital(d) = &mut rec.channels[0].data {
        (d.end, d.edges, d.count) = (0, Box::new(std::iter::empty()), 0);
    }
    assert_eq!(
        refused(rec, &options),
        "the length 0 does not lie past every item and block in, the last at 0"
    );

    let (mut rec, options) = analog(None, vec![1.0; 10]);
    if let ChannelData::Analog(a) = &mut rec.channels[1].data {
        a.first = -3;
    }
    assert_eq!(
        refused(rec, &options),
        "V: source tick -3 lies outside 0 to 2^53"
    );

    let (mut rec, options) = analog(None, vec![1.0; 10]);
    if let ChannelData::Analog(a) = &mut rec.channels[1].data {
        a.first = 200_000_000;
    }
    assert_eq!(
        refused(rec, &options),
        "the length 1000000001 does not lie past every item and block in, the last at 1100000000"
    );

    // An empty channel has no first sample: its `first` is 0, whatever the source said.
    let (mut rec, options) = analog(None, vec![]);
    if let ChannelData::Analog(a) = &mut rec.channels[1].data {
        a.first = 2_000_000_000;
    }
    let file = encode(rec, &options).unwrap();
    let head = Footer::parse_file(&file).unwrap();
    assert!(matches!(
        head.header.channels[1].data,
        escapement_tickwave_format::header::ChannelKind::Analog {
            first: 0,
            count: 0,
            ..
        }
    ));

    let (rec, options) = analog(Some(0), vec![0.0; 10]);
    assert_eq!(
        refused(rec, &options),
        "the declaration: channel V's codes are 0 bits; a codes coding holds 1 to 16"
    );

    // A digital channel's count is what the density takes, so a source of edges states it
    // exactly: one more or one fewer than it gives is refused.
    for (promised, delivered) in [(3, 2), (1, 2), (0, 2)] {
        let (mut rec, options) = analog(None, vec![1.0; 10]);
        if let ChannelData::Digital(d) = &mut rec.channels[0].data {
            d.count = promised;
        }
        assert_eq!(
            refused(rec, &options),
            format!("C: {promised} items promised, {delivered} delivered")
        );
    }
}

#[test]
fn a_zero_term_in_a_header_ratio_is_refused() {
    let (rec, options) = analog(None, vec![1.0; 10]);
    let file = encode(rec, &options).unwrap();
    for (from, to) in [
        (
            r#""timebase":[1000000000,1]"#,
            r#""timebase":[1000000000,0]"#,
        ),
        (r#""period":[100000000,1]"#, r#""period":[100000000,0]"#),
    ] {
        let bad = patched(&file, from, to);
        let err = Footer::parse_file(&bad).expect_err("refused");
        assert!(err.to_string().contains("nonzero"), "{to}: {err}");
    }
}

#[test]
fn a_header_integer_past_2_pow_53_is_refused() {
    let (mut rec, options) = analog(None, vec![1.0; 10]);
    // 10^15 ticks a second: sixteen digits, as 2^53 + 1 has, so the patch keeps every offset. A
    // recording of one source ticks at its rate.
    rec.timebase = Ratio::integer(1_000_000_000_000_000).unwrap();
    rec.sources[0].timebase = rec.timebase;
    let file = encode(rec, &options).unwrap();
    assert!(Footer::parse_file(&file).is_ok());
    let bad = patched(
        &file,
        r#""timebase":[1000000000000000,1]"#,
        r#""timebase":[9007199254740993,1]"#,
    );
    let err = Footer::parse_file(&bad).expect_err("refused");
    assert!(err.to_string().contains("past 2^53"), "{err}");
}

#[test]
fn an_index_that_moves_samples_out_of_their_block_is_refused() {
    let (rec, mut options) = analog(None, (0..10).map(|i| i as f32).collect());
    options.block_bytes = 16;
    let file = encode(rec, &options).unwrap();
    let index = Index::of_file(&file).unwrap();
    assert!(
        index.blocks[1].len() > 2,
        "{} blocks",
        index.blocks[1].len()
    );
    // The analog channel's word in each of the first two rows: one sample moves from block 1 to
    // block 0, so the counts still add up to the header's.
    let bad = rows_edited(&file, 1, |rows| {
        assert!(rows[1].word > 0);
        rows[0].word += 1;
        rows[1].word -= 1;
    });
    let err = Index::of_file(&bad).expect_err("refused");
    assert!(
        err.to_string().contains("samples lie outside its ticks"),
        "{err}"
    );
}

/// A channel never known, its end at its start, keeps its declared `initial`, which says
/// nothing of a level: either is written and read.
#[test]
fn a_channel_never_known_keeps_its_declared_initial_level() {
    let never = |initial: bool| {
        let (mut rec, options) = analog(None, vec![0.0; 11]);
        rec.channels.push(Channel {
            name: "N".into(),
            source_name: "n".into(),
            source: 0,
            data: ChannelData::Digital(Digital {
                initial,
                end: 0,
                edges: Box::new(std::iter::empty()),
                count: 0,
                gaps: Vec::new(),
            }),
            shaping: Default::default(),
        });
        (rec, options)
    };
    for initial in [false, true] {
        let (rec, options) = never(initial);
        let file = encode(rec, &options).unwrap();
        let h = Footer::parse_file(&file).unwrap().header;
        let n = h.channels.iter().find(|c| c.name == "N").unwrap();
        assert!(
            matches!(n.data, escapement_tickwave_format::header::ChannelKind::Digital { initial: i, end: 0, edges: 0, .. } if i == initial),
            "{:?}",
            n.data
        );
        escapement_tickwave_format::verify::verify(&file[..], never(initial).0).unwrap();
    }
}

/// The analog recording's file with its length or channel C's end one tick shorter, so that
/// what sat on the final tick sits at the bound, outside it.
#[test]
fn an_item_at_the_length_or_the_end_is_refused() {
    let shortened = |channels: usize, block_bytes: usize, from: &str, to: &str| {
        let (mut rec, options) = analog(None, vec![0.0; 11]);
        rec.channels.truncate(channels);
        let file = encode(
            rec,
            &Options {
                block_bytes,
                ..options
            },
        )
        .unwrap();
        Footer::parse_file(&file).unwrap();
        let mut bad = patched(&file, r#""end":1000000001"#, r#""end":1000000000"#);
        if from != to {
            bad = patched(&bad, from, to);
        }
        bad
    };
    let (length, shorter) = (r#""length":1000000001"#, r#""length":1000000000"#);

    // V's eleventh sample, at 10^9.
    let err = Footer::parse_file(&shortened(2, 1 << 20, length, shorter)).expect_err("refused");
    assert!(
        err.to_string().contains("samples lie outside the capture"),
        "{err}"
    );

    // C's second edge, at 10^9, starts a block of its own.
    let err = Index::of_file(&shortened(1, 1, length, shorter)).expect_err("refused");
    assert!(
        err.to_string().contains("a block starts past the length"),
        "{err}"
    );

    // The same edge at C's end, in a capture that still covers it.
    let file = shortened(1, 1 << 20, length, length);
    let err = Index::of_file(&file)
        .unwrap()
        .block_in(&file, 0, 0)
        .expect_err("refused");
    assert!(
        err.to_string()
            .contains("an edge lies outside its channel's start and end"),
        "{err}"
    );
}

/// A declaration's floats go into its JSON, which holds no NaN or infinity: serde_json writes one
/// as null. A codes coding's scale or offset written so would not read back, nor a source's
/// begin or trigger time as given, so the writer refuses them.
#[test]
fn a_float_the_declaration_s_json_cannot_hold_is_refused() {
    use escapement_tickwave_format::header::{
        Declaration, DeclaredChannel, DeclaredKind, SourceHeader,
    };
    use escapement_tickwave_format::writer::{WriteError, WriteOptions, Writer};
    let declaration = |coding, begin_s| Declaration {
        timebase: Ratio::ONE,
        channels: vec![DeclaredChannel {
            name: "V".into(),
            source_name: "v".into(),
            source: 0,
            data: DeclaredKind::Analog {
                unit: "V".into(),
                period: Ratio::ONE,
                coding,
                significant: None,
                phase: None,
            },
            shaping: Default::default(),
            overview_shift: None,
            codec: None,
            requires: Vec::new(),
        }],
        derived: Vec::new(),
        sources: vec![SourceHeader {
            begin_s,
            ..SourceHeader::new(SourceFormat::Other("t".into()), Ratio::ONE)
        }],
        title: None,
        meta: Default::default(),
        overview_shift: 10,
        requires: Vec::new(),
    };
    let codes = |scale, offset| AnalogCoding::Codes {
        bits: 8,
        scale,
        offset,
    };
    assert!(
        Writer::new(
            declaration(codes(0.5, 1.0), Some(2.5)),
            WriteOptions::default()
        )
        .is_ok()
    );
    for (what, d) in [
        ("a NaN scale", declaration(codes(f64::NAN, 1.0), None)),
        (
            "an infinite offset",
            declaration(codes(0.5, f64::INFINITY), None),
        ),
        (
            "a NaN begin",
            declaration(AnalogCoding::F32, Some(f64::NAN)),
        ),
    ] {
        match Writer::new(d, WriteOptions::default()) {
            Err(WriteError::Declaration(e)) if e.contains("not a finite number") => {}
            Err(e) => panic!("{what}: {e}"),
            Ok(_) => panic!("{what}: written"),
        }
    }
}

/// The writer holds an annotation to what `resolve` asks of a generator's line, so its bundle
/// resolves: each rule broken once, refused at the call with the annotation and the rule named.
/// A refused one takes nothing in: its id stays free, and the bundle of the rest resolves.
#[test]
fn an_annotation_its_bundle_would_not_resolve_is_refused_at_the_call() {
    use escapement_tickwave_format::annotations::resolve;
    use escapement_tickwave_format::header::{
        Declaration, DeclaredChannel, DeclaredKind, SourceHeader,
    };
    use escapement_tickwave_format::writer::{Bundle, Finish, WriteError, WriteOptions, Writer};
    let channel = |name: &str, data| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data,
        shaping: Default::default(),
        overview_shift: None,
        codec: None,
        requires: Vec::new(),
    };
    let d = Declaration {
        timebase: Ratio::ONE,
        channels: vec![
            channel("D", DeclaredKind::Digital { initial: false }),
            channel(
                "V",
                DeclaredKind::Analog {
                    unit: "V".into(),
                    period: Ratio::ONE,
                    coding: AnalogCoding::F32,
                    significant: None,
                    phase: None,
                },
            ),
        ],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(
            SourceFormat::Other("t".into()),
            Ratio::ONE,
        )],
        title: None,
        meta: Default::default(),
        overview_shift: 10,
        requires: Vec::new(),
    };
    let mut w = Writer::new(d, WriteOptions::default()).unwrap();
    let start = w.take_output();
    let mut take = |source: &str, line: serde_json::Value| -> Result<(), String> {
        let serde_json::Value::Object(m) = line else {
            unreachable!()
        };
        w.annotation(source, m).map_err(|e| match e {
            WriteError::Annotation(why) => why,
            other => panic!("{other}"),
        })
    };
    use serde_json::json;
    let cases = [
        (
            json!({"id": "m/1", "start": 1, "colour": "red"}),
            "annotation \"m/1\": unknown field `colour`",
        ),
        (
            json!({"start": 1}),
            "annotation with no id: missing field `id`",
        ),
        (
            json!({"id": "m/2", "start": "1 us"}),
            "annotation \"m/2\": start is not a whole tick count: a writer's times are axis ticks",
        ),
        (
            json!({"id": "m/3", "start": 5, "end": 4}),
            "annotation \"m/3\": ends before it starts",
        ),
        (
            json!({"id": "m/4", "start": -1}),
            "annotation \"m/4\": tick -1 lies before the capture",
        ),
        (
            json!({"id": "m/5", "start": 1, "channels": ["X"]}),
            "annotation \"m/5\": no channel \"X\"",
        ),
        (
            json!({"id": "m/6", "start": 1, "end": 2, "measure": {"channel": "V", "what": "rate"}}),
            "annotation \"m/6\": measure: \"V\" is analog; measures are taken from edges",
        ),
        (
            json!({"id": "m/7", "start": 1, "measure": {"channel": "D", "what": "rate"}}),
            "annotation \"m/7\": measure needs a span, not a point",
        ),
        (
            json!({"id": "m/8", "start": 1, "ref": "x/1"}),
            "annotation \"m/8\": only hand-written annotations take a ref",
        ),
        (
            json!({"id": "m/9", "start": 1, "values": {"n": "one"}}),
            "annotation \"m/9\": values: n = \"one\" is not a decimal",
        ),
        (
            json!({"id": "a b", "start": 1}),
            "annotation \"a b\": id \"a b\" holds whitespace",
        ),
    ];
    for (line, want) in cases {
        let why = take("verge", line.clone()).expect_err(&line.to_string());
        assert!(why.starts_with(want), "{line}: {why}");
    }
    take(
        "verge",
        json!({"id": "m/1", "start": 1, "label": "a \\x07 b"}),
    )
    .unwrap();
    // An id given twice is taken: the writer holds no ids, and a reader keeps the first.
    take("verge", json!({"id": "m/1", "start": 2})).unwrap();
    take(
        "other",
        json!({"id": "m/1", "start": 3, "end": 9, "channels": ["D"]}),
    )
    .unwrap();
    let finish = Finish {
        bundle: Some(Bundle::Generated { stem: "rec".into() }),
        ..Finish::default()
    };
    let finished = w.finish(5, &[5], finish).unwrap();
    let header = finished.header;
    let mut file = start;
    file.extend(finished.tail);
    let files = Footer::parse_file(&file)
        .unwrap()
        .read_sidecar(escapement_tickwave_format::decode::from_memory(&file))
        .map(|s| s.map(|s| s.files))
        .unwrap()
        .unwrap();
    let included: Vec<_> = files
        .iter()
        .filter(|(name, _)| name.ends_with(".jsonl"))
        .map(|(name, text)| escapement_tickwave_format::annotations::Included { name, text })
        .collect();
    let resolved = resolve(&files["rec.annotations.toml"], &included, &header, None).unwrap();
    let spans: Vec<_> = resolved.iter().map(|a| (a.id.as_str(), a.end)).collect();
    // The span past the length ends there. The generators go in the order of their first
    // annotations, verge's first.
    assert_eq!(spans, [("verge/m/1", None), ("other/m/1", Some(5))]);
}

/// Bundling takes the footer record its footer was parsed from: one too short for its lengths, or
/// whose header length runs past it, is refused, not read past its end.
#[test]
fn bundling_a_record_that_is_not_the_footers_is_refused() {
    use escapement_tickwave_format::bundle::bundled_tail;
    use escapement_tickwave_format::record;
    let (rec, options) = analog(None, vec![0.0; 11]);
    let file = encode(rec, &options).unwrap();
    let footer = Footer::parse_file(&file).unwrap();
    let files: std::collections::BTreeMap<String, String> =
        [("a.annotations.toml".to_string(), String::new())].into();
    for payload in [vec![1, 2], [u32::MAX.to_le_bytes(), [0; 4]].concat()] {
        let mut wrong = Vec::new();
        record::write_footer(&mut wrong, 0, &payload);
        let record = &wrong[..wrong.len() - 24];
        let err = bundled_tail(
            record,
            &footer,
            files.clone(),
            3,
            escapement_tickwave_format::decode::from_memory(&file),
        )
        .expect_err("refused");
        assert_eq!(
            err.to_string(),
            "the footer record is not the one its footer was parsed from",
            "{payload:?}"
        );
    }
    // The right record bundles.
    let at = footer.trailer.footer_offset as usize;
    let record = &file[at..at + footer.trailer.footer_len as usize];
    bundled_tail(
        record,
        &footer,
        files,
        3,
        escapement_tickwave_format::decode::from_memory(&file),
    )
    .unwrap();
}
