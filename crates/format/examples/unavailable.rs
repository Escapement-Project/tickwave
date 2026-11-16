//! Writes web/fixtures/unavailable.tickwave: a recording with channels a reader of this version
//! can't read (FORMAT.md, "Unavailable channels"), and annotations that name them. Nothing in it
//! comes from a capture. Run from the repository:
//!
//!     cargo run -p escapement-tickwave-format --example unavailable -- web/fixtures
//!
//! - One source at 1 MHz, 100,000 ticks.
//! - V, I and J, `int` 16-bit unsigned codes in mV, a sample every 100 ticks. V requires a
//!   feature, `frobs`.
//! - D, digital, an edge every 1,000 ticks; E, digital, a few edges, stored with a codec, `rle9`.
//! - Derived: P = I × V, unavailable since V is; Q = I × J, which requires `frobs` itself; and
//!   S = I − J, which this version reads.
//! - Seven annotations from `verge`, bundled. First one that starts past the length, `z`,
//!   written before the recording knew where it would end; then one each on I and V, a rate of
//!   E, on I, on no channel, on J, and a second `z` inside the length. A reader leaves out the
//!   two that name V or E, and the first `z`, which claims no id: the second stays.
//!
//! The writer takes no `requires` or `codec`, since this version defines none, so they go into
//! the footer's header after it finishes. The content id covers neither, so it stays the
//! recording's.

use std::collections::BTreeMap;
use std::path::PathBuf;

use escapement_tickwave_format::decode::Footer;
use escapement_tickwave_format::header::{
    AnalogCoding, Declaration, DeclaredChannel, DeclaredKind, Derived, Exact, IntCoding,
    SourceFormat, SourceHeader,
};
use escapement_tickwave_format::model::Ratio;
use escapement_tickwave_format::record::write_footer;
use escapement_tickwave_format::writer::{Bundle, Finish, WriteOptions, Writer};

const LENGTH: i64 = 100_000;
const PERIOD: i64 = 100;

fn main() {
    let dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("the directory to write into"),
    );
    let timebase = Ratio::integer(1_000_000).unwrap();
    let analog = |name: &str| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Analog {
            unit: "V".into(),
            period: Ratio::integer(PERIOD as u64).unwrap(),
            coding: AnalogCoding::Int(IntCoding {
                scale: Exact::new(1, 1000),
                ..IntCoding::new(16, false)
            }),
            significant: None,
            phase: None,
        },
        shaping: Default::default(),
        overview_shift: None,
        codec: None,
        requires: Vec::new(),
    };
    let digital = |name: &str| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Digital { initial: false },
        shaping: Default::default(),
        overview_shift: None,
        codec: None,
        requires: Vec::new(),
    };
    let declaration = Declaration {
        timebase,
        channels: vec![
            analog("V"),
            analog("I"),
            analog("J"),
            digital("D"),
            digital("E"),
        ],
        derived: vec![
            Derived::multiply("P", "W", "I", "V"),
            Derived::multiply("Q", "V²", "I", "J"),
            Derived::subtract("S", "V", "I", "J"),
        ],
        sources: vec![SourceHeader::new(
            SourceFormat::Other("unavailable".into()),
            timebase,
        )],
        title: Some("Channels this version can't read".into()),
        meta: BTreeMap::new(),
        overview_shift: 10,
        requires: Vec::new(),
    };
    let mut w = Writer::new(
        declaration,
        WriteOptions {
            level: 19,
            ..WriteOptions::default()
        },
    )
    .unwrap();
    for c in 0..3 {
        w.run(c, 0, 0).unwrap();
    }
    // A recorder takes an annotation before it knows where the recording ends.
    let serde_json::Value::Object(past) = serde_json::json!({"id": "z", "start": 150_000}) else {
        unreachable!("an object")
    };
    w.annotation("verge", past).unwrap();
    let mut annotations = vec![
        (1_000, serde_json::json!({"id": "a", "start": 1_000, "channels": ["I", "V"]})),
        (
            2_000,
            serde_json::json!({"id": "b", "start": 2_000, "end": 5_000, "measure": {"channel": "E", "what": "rate"}}),
        ),
        (3_000, serde_json::json!({"id": "c", "start": 3_000, "channels": ["I"]})),
        (4_000, serde_json::json!({"id": "d", "start": 4_000, "label": "everywhere"})),
        (5_000, serde_json::json!({"id": "e", "start": 5_000, "channels": ["J"]})),
        (6_000, serde_json::json!({"id": "z", "start": 6_000})),
    ]
    .into_iter()
    .peekable();
    for i in 0..LENGTH / PERIOD {
        let t = i * PERIOD;
        if t % 1_000 == 0 && t > 0 {
            w.edge(3, t).unwrap();
        }
        if [20_000, 20_500, 60_000].contains(&t) {
            w.edge(4, t).unwrap();
        }
        while let Some((_, a)) = annotations.next_if(|(at, _)| *at <= t) {
            let serde_json::Value::Object(a) = a else {
                unreachable!("an object")
            };
            w.annotation("verge", a).unwrap();
        }
        let i = i as u64;
        w.sample(0, 3300 + i % 7).unwrap();
        w.sample(1, 1000 + (i * 37) % 200).unwrap();
        w.sample(2, 900 + (i * 13) % 100).unwrap();
    }
    let mut file = w.take_output();
    let finished = w
        .finish(
            LENGTH,
            &[LENGTH; 2],
            Finish {
                bundle: Some(Bundle::Generated {
                    stem: "unavailable".into(),
                }),
                ..Finish::default()
            },
        )
        .unwrap();
    file.extend(finished.tail);

    let mut header = Footer::parse_file(&file).unwrap().header;
    header.channels[0].requires = vec!["frobs".into()];
    header.channels[4].codec = Some("rle9".into());
    header.derived[1].requires = vec!["frobs".into()];
    let out = with_header(&file, &serde_json::to_vec(&header).unwrap());
    std::fs::write(dir.join("unavailable.tickwave"), &out).unwrap();
    println!(
        "unavailable.tickwave: {} bytes, {}",
        out.len(),
        header.content_id
    );
}

/// `file` with its footer's header replaced by `json`, the top index and the strips kept.
fn with_header(file: &[u8], json: &[u8]) -> Vec<u8> {
    let t = file.len() - 24;
    let at = u64::from_le_bytes(file[t..t + 8].try_into().unwrap()) as usize;
    let len = u32::from_le_bytes(file[t + 8..t + 12].try_into().unwrap()) as usize;
    // The footer record's payload: past its kind and length, before its check.
    let payload = &file[at + 5..at + len - 8];
    let header_len = u32::from_le_bytes(payload[..4].try_into().unwrap()) as usize;
    let mut footer = Vec::new();
    footer.extend_from_slice(&(json.len() as u32).to_le_bytes());
    footer.extend_from_slice(&payload[4..8]);
    footer.extend_from_slice(json);
    footer.extend_from_slice(&payload[8 + header_len..]);
    let mut out = file[..at].to_vec();
    write_footer(&mut out, 0, &footer);
    out
}
