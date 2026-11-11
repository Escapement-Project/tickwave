//! Inputs a reader must refuse: frames that are not exactly one checksummed zstd frame, codes wider
//! than their coding, and headers whose extents leave the capture.

use escapement_tickwave_format::decode::{Head, MAX_DECODED, inflate, inflate_limited};
use escapement_tickwave_format::encode::{Options, encode};
use escapement_tickwave_format::header::{AnalogCoding, BlockRow, PREAMBLE_LEN};
use escapement_tickwave_format::model::{
    Analog, Channel, ChannelData, Digital, Ratio, Recording, Samples, Source, SourceFormat,
};
use zstd::zstd_safe::CParameter;

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
    refused(&skippable(&raw), raw.len(), "a skippable frame alone");
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
    refused(&with_dict, raw.len(), "a dictionary");
}

fn analog(coding_bits: Option<u8>, values: Vec<f32>) -> (Recording, Options) {
    let count = values.len() as u64;
    let rec = Recording {
        timebase: Ratio::integer(1_000_000_000).unwrap(),
        length: 1_000_000_000,
        channels: vec![
            Channel {
                name: "C".into(),
                source_name: "c".into(),
                data: ChannelData::Digital(Digital {
                    initial: false,
                    end: 1_000_000_000,
                    edges: Box::new([5, 1_000_000_000].into_iter()),
                }),
            },
            Channel {
                name: "V".into(),
                source_name: "v".into(),
                data: ChannelData::Analog(Analog {
                    unit: "V".into(),
                    first: 0,
                    period: Ratio::integer(100_000_000).unwrap(),
                    count,
                    samples: Samples::F32(Box::new(values.into_iter())),
                }),
            },
        ],
        source: Source::new(SourceFormat::Other("test".into())),
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

/// `file` with `from` replaced by `to` in its header, which must be the same length so that every
/// offset stays right.
fn patched(file: &[u8], from: &str, to: &str) -> Vec<u8> {
    assert_eq!(from.len(), to.len());
    let at = file
        .windows(from.len())
        .position(|w| w == from.as_bytes())
        .unwrap_or_else(|| panic!("{from} is not in the header"));
    let mut out = file.to_vec();
    out[at..at + to.len()].copy_from_slice(to.as_bytes());
    out
}

#[test]
fn a_code_wider_than_its_coding_is_refused() {
    // Eleven samples, one a 16-bit code; the header then claims 12 bits.
    let mut values: Vec<f32> = (0..10).map(|i| i as f32).collect();
    values.push(40_000.0);
    let (rec, options) = analog(Some(16), values);
    let file = encode(rec, &options).unwrap();
    let decode = |file: &[u8]| {
        let head = Head::parse(file)?;
        let (offset, len) = head.block_location(0);
        head.decode_block(0, &file[offset as usize..][..len as usize])
    };
    decode(&file).unwrap();
    let narrowed = patched(&file, r#""bits":16"#, r#""bits":12"#);
    let err = decode(&narrowed).err().expect("refused");
    assert!(
        err.to_string().contains("code 40000 is wider than 12 bits"),
        "{err}"
    );
}

#[test]
fn a_header_reaching_past_the_capture_is_refused() {
    let (rec, options) = analog(None, vec![0.0; 11]);
    let file = encode(rec, &options).unwrap();
    Head::parse(&file).unwrap();
    for (from, to, why) in [
        (
            r#""end":1000000000"#,
            r#""end":9000000000"#,
            "ends outside the capture",
        ),
        (
            r#""first":0"#,
            r#""first":9"#,
            "samples lie outside the capture",
        ),
    ] {
        let err = Head::parse(&patched(&file, from, to))
            .err()
            .expect("refused");
        assert!(err.to_string().contains(why), "{err}");
    }
    let (rec, options) = analog(Some(16), vec![0.0; 11]);
    let file = encode(rec, &options).unwrap();
    let err = Head::parse(&patched(&file, r#""bits":16"#, r#""bits":17"#))
        .err()
        .expect("refused");
    assert!(err.to_string().contains("codes are 17 bits"), "{err}");
}

#[test]
fn a_block_starting_at_the_wrong_level_is_refused() {
    use escapement_tickwave_format::header::{BlockRow, LEVEL_BIT, PREAMBLE_LEN};
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
    let head = Head::parse(&file).unwrap();
    assert!(head.rows.len() > 2);
    let row_size = BlockRow::size(2);
    for block in [0, head.rows.len() - 1] {
        // Channel C's word: after the row's 24 fixed bytes.
        let at = PREAMBLE_LEN + head.preamble.header_len as usize + block * row_size + 24;
        let mut bad = file.clone();
        let word = u32::from_le_bytes(bad[at..at + 4].try_into().unwrap()) ^ LEVEL_BIT;
        bad[at..at + 4].copy_from_slice(&word.to_le_bytes());
        let err = Head::parse(&bad).err().expect("refused");
        assert!(
            err.to_string()
                .contains(&format!("block {block} starts at the wrong level")),
            "{err}"
        );
    }
}

#[test]
fn a_whole_file_shorter_or_longer_than_its_preamble_says_is_refused() {
    let (rec, options) = analog(None, vec![1.0; 10]);
    let file = encode(rec, &options).unwrap();
    assert!(Head::parse_file(&file).is_ok());
    for (what, bytes) in [
        ("cut short", &file[..file.len() - 5]),
        ("cut inside the head", &file[..40]),
    ] {
        let err = Head::parse_file(bytes).err().expect(what);
        assert!(
            err.to_string()
                .contains(&format!("the file is {} bytes", bytes.len())),
            "{what}: {err}"
        );
    }
    let longer = [&file[..], &[0]].concat();
    let err = Head::parse_file(&longer).err().expect("one byte after");
    assert!(
        err.to_string().contains("where its preamble needs"),
        "{err}"
    );
}

#[test]
fn a_block_past_the_index_is_refused() {
    let (rec, options) = analog(None, vec![1.0; 10]);
    let file = encode(rec, &options).unwrap();
    let head = Head::parse_file(&file).unwrap();
    let n = head.rows.len();
    let err = head.decode_block(n, &[]).err().expect("refused");
    assert!(err.to_string().contains(&format!("no block {n}")), "{err}");
    let levels = head.overview_rows.len();
    let err = head
        .decode_overview_block(levels, 0, &[])
        .err()
        .expect("refused");
    assert!(err.to_string().contains("no overview block"), "{err}");
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
    // A head whose overview needs more than the reader is set to hold.
    let (rec, options) = analog(None, vec![1.0; 10]);
    let file = encode(rec, &options).unwrap();
    assert!(Head::parse_file_limited(&file, MAX_DECODED).is_ok());
    let err = Head::parse_file_limited(&file, 16).err().expect("refused");
    assert!(
        err.to_string().contains("more than this reader holds (16)"),
        "{err}"
    );
}

#[test]
fn the_encoder_refuses_what_the_reader_would_and_says_which_channel() {
    let refused =
        |rec: Recording, options: &Options| encode(rec, options).expect_err("refused").to_string();
    let (mut rec, options) = analog(None, vec![1.0; 10]);
    rec.length = -5;
    assert_eq!(refused(rec, &options), "the length -5 is negative");

    let (mut rec, options) = analog(None, vec![1.0; 10]);
    if let ChannelData::Analog(a) = &mut rec.channels[1].data {
        a.first = -3;
    }
    assert_eq!(
        refused(rec, &options),
        "V: the first sample is at tick -3, before the recording starts"
    );

    // An empty channel still starts inside the capture.
    let (mut rec, options) = analog(None, vec![]);
    if let ChannelData::Analog(a) = &mut rec.channels[1].data {
        a.first = 2_000_000_000;
    }
    assert!(refused(rec, &options).starts_with("V: samples run past the recording's end"));

    let (rec, options) = analog(Some(0), vec![0.0; 10]);
    assert_eq!(
        refused(rec, &options),
        "V: codes of 0 bits; a coding holds 1 to 16"
    );
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
        let err = Head::parse_file(&bad).err().expect("refused");
        assert!(err.to_string().contains("nonzero"), "{to}: {err}");
    }
}

#[test]
fn a_header_integer_past_2_pow_53_is_refused() {
    let (mut rec, options) = analog(None, vec![1.0; 10]);
    // 10^15 ticks a second: sixteen digits, as 2^53 + 1 has, so the patch keeps every offset.
    rec.timebase = Ratio::integer(1_000_000_000_000_000).unwrap();
    let file = encode(rec, &options).unwrap();
    assert!(Head::parse_file(&file).is_ok());
    let bad = patched(
        &file,
        r#""timebase":[1000000000000000,1]"#,
        r#""timebase":[9007199254740993,1]"#,
    );
    let err = Head::parse_file(&bad).err().expect("refused");
    assert!(err.to_string().contains("past 2^53"), "{err}");
}

#[test]
fn an_index_that_moves_samples_out_of_their_block_is_refused() {
    let (rec, mut options) = analog(None, (0..10).map(|i| i as f32).collect());
    options.block_bytes = 16;
    let file = encode(rec, &options).unwrap();
    let head = Head::parse_file(&file).unwrap();
    assert!(head.rows.len() > 2, "{} blocks", head.rows.len());
    // The analog channel's word, channel 1, in each of the first two rows: one sample moves from
    // block 1 to block 0, so the counts still add up to the header's.
    let index = PREAMBLE_LEN + head.preamble.header_len as usize;
    let row = BlockRow::size(2);
    let word = |file: &[u8], i: usize| {
        let at = index + i * row + 24 + 4;
        (at, u32::from_le_bytes(file[at..at + 4].try_into().unwrap()))
    };
    let mut bad = file.clone();
    let (at0, w0) = word(&file, 0);
    let (at1, w1) = word(&file, 1);
    assert!(w1 > 0);
    bad[at0..at0 + 4].copy_from_slice(&(w0 + 1).to_le_bytes());
    bad[at1..at1 + 4].copy_from_slice(&(w1 - 1).to_le_bytes());
    let err = Head::parse_file(&bad).err().expect("refused");
    assert!(
        err.to_string().contains("samples lie outside its ticks"),
        "{err}"
    );
}
