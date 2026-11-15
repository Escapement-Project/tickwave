//! `tickwave verify FILE` with no `--against`: a recording checked on its own, as a recorder's file
//! has no other source.

use std::path::Path;
use std::process::Command;

use escapement_tickwave_format::decode::Index;
use escapement_tickwave_format::encode::{Options, encode};
use escapement_tickwave_format::model::{
    Channel, ChannelData, Digital, Ratio, Recording, Source, SourceFormat,
};
use escapement_tickwave_format::record::{self, Kind};

fn verify(file: &Path) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_tickwave"))
        .args(["verify".as_ref(), file.as_os_str()])
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A digital channel with an edge every few ticks, at random, and a fine overview: many overview
/// blocks, most of them unlike the others.
fn recording() -> Vec<u8> {
    let mut x = 0x2545_f491_4f6c_dd1du64;
    let mut t = 0i64;
    let edges: Vec<i64> = (0..200_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            t += 1 + (x % 7) as i64;
            t
        })
        .collect();
    let end = t + 1;
    let timebase = Ratio::integer(1_000_000).unwrap();
    encode(
        Recording {
            timebase,
            length: end,
            channels: vec![Channel {
                name: "D".into(),
                source_name: "d".into(),
                source: 0,
                data: ChannelData::Digital(Digital {
                    initial: false,
                    end,
                    count: edges.len() as u64,
                    edges: Box::new(edges.into_iter()),
                    gaps: Vec::new(),
                }),
                shaping: Default::default(),
            }],
            derived: Vec::new(),
            sources: vec![Source::new(SourceFormat::Other("probe".into()), timebase)],
        },
        &Options {
            level: 3,
            block_bytes: 4 << 10,
            overview_shift: Some(2),
            ..Options::default()
        },
    )
    .unwrap()
}

/// `file` with `bytes` written at `at`, inside the `kind` record at `record_at`, and that
/// record's check made again.
fn spliced(file: &[u8], kind: Kind, record_at: usize, at: usize, bytes: &[u8]) -> Vec<u8> {
    let mut out = file.to_vec();
    out[at..at + bytes.len()].copy_from_slice(bytes);
    let n = u32::from_le_bytes(out[record_at + 1..record_at + 5].try_into().unwrap()) as usize;
    let payload = record_at + record::PAYLOAD_AT..record_at + record::PAYLOAD_AT + n;
    let mut h = blake3::Hasher::new();
    h.update(&[kind as u8]);
    h.update(&(n as u32).to_le_bytes());
    h.update(&out[payload.clone()]);
    out[payload.end..payload.end + 8].copy_from_slice(&h.finalize().as_bytes()[..8]);
    out
}

#[test]
fn a_recording_checks_on_its_own_and_one_changed_inside_a_record_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let file = recording();
    let path = dir.path().join("rec.tickwave");
    std::fs::write(&path, &file).unwrap();
    let (ok, err) = verify(&path);
    assert!(ok, "{err}");
    assert!(err.contains("checks on its own"), "{err}");

    let index = Index::of_file(&file).unwrap();
    // A byte inside one block's frame, its record's check made again.
    let row = index.blocks[0][3].row;
    let at = row.offset as usize + row.length as usize / 2;
    let record_at = row.offset as usize - record::PAYLOAD_AT - record::BlockHead::LEN;
    let changed = spliced(&file, Kind::Block, record_at, at, &[file[at] ^ 0x5a]);
    let tampered = dir.path().join("block.tickwave");
    std::fs::write(&tampered, &changed).unwrap();
    let (ok, err) = verify(&tampered);
    assert!(!ok, "a changed block passed");
    assert!(err.contains("can't be read"), "{err}");

    // An overview block's frame replaced by a later one's of the same length and other bytes, so
    // every place the pages give stays right, its record's check made again.
    let rows = &index.levels[0][0];
    let frame = |r: &escapement_tickwave_format::decode::LevelRef| {
        &file[r.row.offset as usize..][..r.row.length as usize]
    };
    let (r, other) = (0..rows.len())
        .find_map(|k| {
            rows[k + 1..]
                .iter()
                .find(|o| o.row.length == rows[k].row.length && frame(o) != frame(&rows[k]))
                .map(|o| (rows[k], frame(o).to_vec()))
        })
        .expect("two overview blocks' frames of one length");
    let at = r.row.offset as usize;
    let record_at = at - record::PAYLOAD_AT - record::OverviewHead::LEN;
    let changed = spliced(&file, Kind::OverviewBlock, record_at, at, &other);
    let tampered = dir.path().join("overview.tickwave");
    std::fs::write(&tampered, &changed).unwrap();
    let (ok, err) = verify(&tampered);
    assert!(!ok, "a changed overview block passed");
    assert!(
        err.contains(&format!("block {}: differs from the content", r.k)),
        "{err}"
    );
}
