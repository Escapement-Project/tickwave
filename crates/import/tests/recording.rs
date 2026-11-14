//! Reading a finished recording back into the model: what comes out is what the file holds, as
//! `verify` checks it, and converting it again gives the same content.

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

use escapement_tickwave_format::decode::{Footer, MAX_DECODED};
use escapement_tickwave_format::header::{ChannelKind, Header, SourceHeader};
use escapement_tickwave_format::model::{
    Analog, Break, Channel, ChannelData, Digital, Gap, Ratio, Recording, Samples, Shaping, Source,
    SourceFormat,
};
use escapement_tickwave_format::verify::verify;
use escapement_tickwave_import::convert::{self, Pass, Progress, Settings, Step};
use escapement_tickwave_import::recording::read;

struct Quiet;

impl Progress for Quiet {
    fn start(&self, _: Pass<'_>, _: u64) -> Step {
        Arc::new(|_| {})
    }
}

fn fixtures() -> Vec<(String, Vec<u8>)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/fixtures");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "tickwave"))
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read(&p).unwrap())
        })
        .collect();
    files.sort();
    assert!(files.len() >= 7, "{} fixtures", files.len());
    files
}

fn convert(open: &dyn Fn() -> Result<Recording, convert::Error>, settings: &Settings) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    convert::convert(open, settings, MAX_DECODED, &Quiet, &mut out).unwrap();
    out.into_inner()
}

#[test]
fn every_fixture_reads_back_as_its_file_holds_it() {
    // Digital, analog in codes, gaps from tick 0 and inside, a bundled sidecar, small blocks.
    for (name, file) in fixtures() {
        let (header, recording) = read(Arc::new(file.clone())).unwrap();
        assert_eq!(header, Footer::parse_file(&file).unwrap().header, "{name}");
        // What the converter states of itself is its own, not the source's.
        let sources: Vec<SourceHeader> = recording
            .sources
            .iter()
            .zip(&header.sources)
            .map(|(s, h)| SourceHeader {
                format: s.format.clone(),
                files: s.files.clone(),
                timebase: s.timebase,
                origin: s.origin,
                align: s.align,
                stretch: s.stretch,
                instrument: s.instrument.clone(),
                software: s.software.clone(),
                begin_s: s.begin_s,
                trigger_s: s.trigger_s,
                rate: s.rate,
                extra: s.extra.clone(),
                converter: h.converter.clone(),
                title: s.title.clone(),
                meta: s.meta.clone(),
            })
            .collect();
        assert_eq!(sources, header.sources, "{name}");
        assert_eq!(recording.derived, header.derived, "{name}");
        let checked = verify(&file[..], recording).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(checked.content_id, header.content_id, "{name}");
    }
}

#[test]
fn a_recording_converts_again_to_the_same_content() {
    for (name, file) in fixtures() {
        let id = Footer::parse_file(&file).unwrap().header.content_id;
        let open = || Ok(read(Arc::new(file.clone())).unwrap().1);
        let again = convert(&open, &Settings::default());
        let header = Footer::parse_file(&again).unwrap().header;
        assert_eq!(header.content_id, id, "{name}");
        // Codes found again where the file had them.
        let codings = |h: &Header| {
            h.channels
                .iter()
                .map(|c| match c.data {
                    ChannelKind::Analog { coding, .. } => Some(coding),
                    ChannelKind::Digital { .. } => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            codings(&header),
            codings(&Footer::parse_file(&file).unwrap().header),
            "{name}"
        );
    }
}

/// 100 ticks at 1 GS/s. G: high, unknown at 30..40 (low after) and 60..70 (high after), edges at
/// 10, 20, 50 and 80. W: samples 0 to 4 from tick 0, 5 to 9 from tick 30, every 1.5 ticks.
fn with_gaps() -> Recording {
    Recording {
        timebase: Ratio::integer(1_000_000_000).unwrap(),
        length: 100,
        channels: vec![
            Channel {
                name: "G".into(),
                source_name: "g".into(),
                source: 0,
                data: ChannelData::Digital(Digital {
                    initial: true,
                    end: 100,
                    edges: Box::new([10, 20, 50, 80].into_iter()),
                    count: 4,
                    gaps: vec![
                        Gap {
                            from: 30,
                            to: 40,
                            level: false,
                        },
                        Gap {
                            from: 60,
                            to: 70,
                            level: true,
                        },
                    ],
                }),
                shaping: Shaping::default(),
            },
            Channel {
                name: "W".into(),
                source_name: "w".into(),
                source: 0,
                data: ChannelData::Analog(Analog {
                    significant: None,
                    unit: "V".into(),
                    first: 0,
                    period: Ratio::new(3, 2).unwrap(),
                    count: 10,
                    breaks: vec![Break {
                        index: 5,
                        tick: 30,
                        lost: 0,
                    }],
                    samples: Samples::F32(Box::new((0..10).map(|i| i as f32 * 0.1))),
                }),
                shaping: Shaping::default(),
            },
        ],
        derived: Vec::new(),
        sources: vec![Source::new(
            SourceFormat::Other("test".into()),
            Ratio::integer(1_000_000_000).unwrap(),
        )],
    }
}

#[test]
fn gaps_breaks_moves_and_cuts_come_back_as_written() {
    let settings = Settings {
        offsets: vec![("W".into(), "4".into())],
        keeps: vec![("G".into(), Some("15".into()), Some("90".into()))],
        block_kib: 1,
        ..Settings::default()
    };
    let file = convert(&|| Ok(with_gaps()), &settings);
    let (header, recording) = read(Arc::new(file.clone())).unwrap();
    assert!(header.channels.iter().all(|c| c.shaping.kept.is_some()));
    assert!(header.channels[1].shaping.offset.is_some());
    let shapings: Vec<_> = recording
        .channels
        .iter()
        .map(|c| c.shaping.clone())
        .collect();
    assert_eq!(
        shapings,
        header
            .channels
            .iter()
            .map(|c| c.shaping.clone())
            .collect::<Vec<_>>()
    );
    verify(&file[..], recording).unwrap();
}

#[test]
fn a_damaged_block_is_refused_when_the_recording_is_opened() {
    let (_, mut file) = fixtures()
        .into_iter()
        .find(|(n, _)| n == "identify-swd.4k.tickwave")
        .unwrap();
    let footer = Footer::parse_file(&file).unwrap();
    assert!(footer.rows.len() > 2);
    let (offset, length) = footer.block_location(footer.rows.len() - 2);
    file[(offset + u64::from(length) / 2) as usize] ^= 0x40;
    let err = read(Arc::new(file)).err().unwrap();
    assert!(
        err.to_string().starts_with("the recording can't be read: "),
        "{err}"
    );
}

#[test]
fn a_file_too_short_or_not_a_recording_is_refused() {
    for bytes in [Vec::new(), b"not a recording".to_vec(), vec![0; 40_000]] {
        assert!(read(Arc::new(bytes)).is_err());
    }
}

#[test]
fn a_footer_longer_than_the_first_read_is_read_whole() {
    // Two million edges in 1 KiB blocks: some two thousand index rows.
    let many = || {
        Ok(Recording {
            timebase: Ratio::integer(1_000_000_000).unwrap(),
            length: 6_000_001,
            channels: vec![Channel {
                name: "D".into(),
                source_name: "d".into(),
                source: 0,
                data: ChannelData::Digital(Digital {
                    initial: false,
                    end: 6_000_001,
                    edges: Box::new((1..=2_000_000).map(|i| i * 3)),
                    count: 2_000_000,
                    gaps: Vec::new(),
                }),
                shaping: Shaping::default(),
            }],
            derived: Vec::new(),
            sources: vec![Source::new(
                SourceFormat::Other("test".into()),
                Ratio::integer(1_000_000_000).unwrap(),
            )],
        })
    };
    let settings = Settings {
        level: 1,
        block_kib: 1,
        ..Settings::default()
    };
    let file = convert(&many, &settings);
    let footer = Footer::parse_file(&file).unwrap();
    let footer_len = footer.trailer.footer_len;
    assert!(footer_len > 40 << 10, "the footer is {footer_len} bytes");
    let (header, recording) = read(Arc::new(file.clone())).unwrap();
    assert_eq!(header, footer.header);
    verify(&file[..], recording).unwrap();
}
