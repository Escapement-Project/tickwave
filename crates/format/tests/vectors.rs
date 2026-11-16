//! The reference vectors in testdata/format/ (its README.md), against this implementation:
//! sample places and their axis ticks, the digital overview's density and duty, the annotation
//! parts a writer cuts, and content ids.

use std::collections::BTreeMap;
use std::path::PathBuf;

use escapement_tickwave_format::decode::{Footer, Index, from_memory};
use escapement_tickwave_format::header::{
    AnalogCoding, Declaration, DeclaredChannel, DeclaredKind, Derived, Exact, IntCoding, Kept,
    SourceFormat, SourceHeader,
};
use escapement_tickwave_format::mapping::Mapping;
use escapement_tickwave_format::model::{Break, Gap, Grid, Ratio};
use escapement_tickwave_format::overview::{self, DigitalBucket};
use escapement_tickwave_format::recut::Recutter;
use escapement_tickwave_format::writer::{Bundle, Finish, WriteOptions, Writer};
use serde_json::Value;

fn vectors(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/format")
        .join(name);
    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap()
}

fn int(v: &Value) -> i64 {
    v.as_i64().unwrap_or_else(|| panic!("an integer: {v}"))
}

fn ratio(v: &Value) -> Ratio {
    Ratio::new(int(&v[0]) as u64, int(&v[1]) as u64).unwrap()
}

/// `num / den` in lowest terms, den positive.
fn lowest(num: i128, den: i128) -> (i128, i128) {
    let (mut a, mut b) = (num.unsigned_abs(), den.unsigned_abs());
    while b != 0 {
        (a, b) = (b, a % b);
    }
    let g = a.max(1) as i128;
    (num / g * den.signum(), den.abs() / g)
}

fn breaks(v: &Value) -> Vec<Break> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|b| Break {
            phase: int(&b[3]) as u32,
            ..Break::new(int(&b[0]) as u64, int(&b[1]), int(&b[2]) as u64)
        })
        .collect()
}

/// Each sample's place in `1/D` ticks, the axis tick it maps to, its exact axis time and the
/// time it covers, and each loss's span, for fractional periods and phases, breaks, a negative
/// origin and product, a stretch and a slower source.
#[test]
fn places_and_their_axis_ticks_are_the_vectors() {
    let v = vectors("places.json");
    for case in v["places"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let c = &case["channel"];
        let phase = c["grid"]["phase"]
            .as_array()
            .map(|_| ratio(&c["grid"]["phase"]));
        let grid = Grid::new(ratio(&c["grid"]["period"]), phase).unwrap();
        let (first, count) = (int(&c["first"]), int(&c["count"]) as u64);
        // The first run is the channel's; the header's breaks follow.
        let breaks = breaks(&c["breaks"]);
        let map = Mapping::new(
            int(&c["map"]["origin"]),
            int(&c["map"]["offset"]),
            ratio(&c["map"]["m"]),
        );
        for row in case["samples"].as_array().unwrap() {
            let i = int(&row["index"]) as u64;
            let e = &row["expect"];
            let at = format!("{name}, sample {i}");
            assert_eq!(grid.d() as i64, int(&e["d"]), "{at}");
            let place = grid.sample_place(first, &breaks, i);
            assert_eq!(place, i128::from(int(&e["place"])), "{at}: place");
            let d = grid.d();
            assert_eq!(
                map.axis_place(place, d),
                Some(int(&e["axis"])),
                "{at}: axis"
            );
            let time = map.axis_time(place, d).unwrap();
            let per = i128::from(d) * i128::from(map.m().den());
            let want = (
                i128::from(int(&e["axis_time"][0])),
                i128::from(int(&e["axis_time"][1])),
            );
            assert_eq!(lowest(time, per), want, "{at}: axis time");
            let span = grid.sample_time(first, &breaks, count, i, None).unwrap();
            let want = i128::from(int(&e["time"][0]))..i128::from(int(&e["time"][1]));
            assert_eq!(span, want, "{at}: time");
        }
        for row in case["lost"].as_array().unwrap() {
            let k = int(&row["break"]) as usize;
            let want = i128::from(int(&row["expect"][0]))..i128::from(int(&row["expect"][1]));
            assert_eq!(
                grid.lost_time(first, &breaks, k),
                Some(want),
                "{name}: break {k}"
            );
        }
    }
}

/// Density from edge counts, counts where a floating-point log2 errs included, and duty from a
/// bucket's kept and high ticks.
#[test]
fn density_and_duty_are_the_vectors() {
    let v = vectors("overview.json");
    for row in v["density"].as_array().unwrap() {
        let e = row["edges"].as_u64().unwrap();
        assert_eq!(
            i64::from(overview::density(e)),
            int(&row["expect"]),
            "{e} edges"
        );
    }
    for row in v["duty"].as_array().unwrap() {
        let (k, h) = (row["k"].as_u64().unwrap(), row["h"].as_u64().unwrap());
        assert_eq!(
            i64::from(overview::duty(h, k)),
            int(&row["expect"]),
            "K {k}, H {h}"
        );
    }
}

fn digital(name: &str, initial: bool, shift: u8, kept: Option<Kept>) -> DeclaredChannel {
    DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Digital { initial },
        shaping: escapement_tickwave_format::header::Shaping {
            kept,
            ..Default::default()
        },
        overview_shift: Some(shift),
        codec: None,
        requires: Vec::new(),
    }
}

fn declaration(channels: Vec<DeclaredChannel>, derived: Vec<Derived>) -> Declaration {
    let timebase = Ratio::integer(1000).unwrap();
    Declaration {
        timebase,
        channels,
        derived,
        sources: vec![SourceHeader::new(
            SourceFormat::Other("vectors".into()),
            timebase,
        )],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 4,
        requires: Vec::new(),
    }
}

/// Every bucket of channel `o`'s level `li`, from its overview blocks or, for the last level,
/// the strip.
fn buckets(file: &[u8], o: usize, li: usize) -> Vec<DigitalBucket> {
    let index = Index::of_file(file).unwrap();
    let footer = &index.footer;
    let levels = &index.levels[o];
    let level = if li < levels.len() {
        let mut out = Vec::new();
        for row in &levels[li] {
            let frame = &file[row.row.offset as usize..][..row.row.length as usize];
            let block = footer.decode_overview(o, li, row, frame).unwrap();
            for _ in 0..block.n {
                out.extend((0..block.level.buckets).map(|i| block.level.data.digital(i).unwrap()));
            }
        }
        return out;
    } else {
        footer.strip(o).unwrap()
    };
    (0..level.buckets)
        .map(|i| level.data.digital(i).unwrap())
        .collect()
}

/// A crafted channel's finest level and the next, density and duty bucket by bucket: edges
/// alone, two at one tick, a gap, a burst, and a kept span with buckets wholly outside it.
#[test]
fn a_crafted_channels_density_and_duty_bytes_are_the_vectors() {
    let v = vectors("overview.json");
    for c in v["channels"].as_array().unwrap() {
        let length = int(&c["length"]);
        let shift = int(&c["shift"]) as u8;
        let initial = c["initial"].as_bool().unwrap();
        let kept = c["kept"].as_array().map(|k| Kept {
            from: int(&k[0]),
            to: int(&k[1]),
        });
        let mut channels = vec![digital("D", initial, shift, kept)];
        // A recording that keeps spans has a channel kept from tick 0.
        if kept.is_some() {
            channels.push(digital(
                "E",
                false,
                shift,
                Some(Kept {
                    from: 0,
                    to: length,
                }),
            ));
        }
        let mut w = Writer::new(declaration(channels, vec![]), WriteOptions::default()).unwrap();
        let mut changes: Vec<(i64, Option<Gap>)> = c["edges"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| (int(t), None))
            .collect();
        for g in c["gaps"].as_array().unwrap() {
            let gap = Gap {
                from: int(&g[0]),
                to: int(&g[1]),
                level: g[2].as_bool().unwrap(),
            };
            changes.push((gap.from, Some(gap)));
        }
        changes.sort_by_key(|(t, _)| *t);
        for (t, gap) in changes {
            match gap {
                None => w.edge(0, t).unwrap(),
                Some(g) => w.gap(0, g).unwrap(),
            }
        }
        let mut file = w.take_output();
        let end = kept.map_or(length, |k| k.to);
        let ends: Vec<i64> = if kept.is_some() {
            vec![end, length]
        } else {
            vec![end]
        };
        file.extend(w.finish(length, &ends, Finish::default()).unwrap().tail);
        for (li, want) in c["expect"].as_array().unwrap().iter().enumerate() {
            let got = buckets(&file, 0, li);
            let density: Vec<i64> = got.iter().map(|b| i64::from(b.density)).collect();
            let duty: Vec<i64> = got.iter().map(|b| i64::from(b.duty)).collect();
            let want_density: Vec<i64> = want["density"]
                .as_array()
                .unwrap()
                .iter()
                .map(int)
                .collect();
            let want_duty: Vec<i64> = want["duty"].as_array().unwrap().iter().map(int).collect();
            let note = c["note"].as_str().unwrap();
            assert_eq!(density, want_density, "{note}: level {li}, density");
            assert_eq!(duty, want_duty, "{note}: level {li}, duty");
        }
    }
}

/// Two generators' annotations as a writer takes them: each generator's part goes out once its
/// lines since its last reach 64 KiB, the rest at the finish in the order of the generators'
/// first annotations; each row states its part's span, and the top index the table's.
#[test]
fn a_writers_parts_table_is_the_vectors() {
    let v = vectors("parts.json");
    let d = declaration(vec![digital("D", false, 4, None)], vec![]);
    let mut w = Writer::new(d, WriteOptions::default()).unwrap();
    let gens = v["generators"].as_array().unwrap();
    let mut records = Vec::new();
    for (k, g) in gens.iter().enumerate() {
        let source = g["source"].as_str().unwrap();
        for i in 0..int(&g["count"]) {
            let start = int(&g["first"]) + int(&g["step"]) * i;
            let id = format!("{}/{i}", &source[..1]);
            let text = "x".repeat(int(&g["text"]) as usize);
            records.push((start, k, source, id, text));
        }
    }
    records.sort_by_key(|r| (r.0, r.1));
    let last = records.iter().map(|r| r.0).max().unwrap();
    for (start, _, source, id, text) in records {
        let Value::Object(m) = serde_json::json!({"id": id, "start": start, "text": text}) else {
            unreachable!()
        };
        w.annotation(source, m).unwrap();
    }
    let mut file = w.take_output();
    let finish = Finish {
        bundle: Some(Bundle::Generated { stem: "rec".into() }),
        ..Finish::default()
    };
    file.extend(w.finish(last + 1, &[0], finish).unwrap().tail);
    let footer = Footer::parse_file(&file).unwrap();
    let rows = footer.part_rows(from_memory(&file)).unwrap();
    let want = v["expect"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), want.len());
    for (k, (row, want)) in rows.iter().zip(want).enumerate() {
        let frame = &file[row.offset as usize..][..row.length as usize];
        let part = footer.decode_part(row, frame).unwrap();
        let lines: Vec<&str> = part.lines().collect();
        let first: Value = serde_json::from_str(lines[0]).unwrap();
        let first_id = format!(
            "{}/{}",
            &want["source"].as_str().unwrap()[..1],
            want["first_line"]
        );
        assert_eq!(
            (
                part.source.as_str(),
                first["id"].as_str().unwrap(),
                lines.len() as i64,
                row.start,
                row.end
            ),
            (
                want["source"].as_str().unwrap(),
                first_id.as_str(),
                int(&want["lines"]),
                int(&want["start"]),
                int(&want["end"])
            ),
            "row {k}"
        );
    }
    let table = footer.parts.as_ref().unwrap();
    let top = &table.levels.last().unwrap()[0];
    assert_eq!(
        (top.first, top.number as i64),
        (
            int(&v["expect"]["top"]["first"]),
            int(&v["expect"]["top"]["number"])
        )
    );
}

/// The vectors' recording written as `variant` says: its derived channel's operation and
/// operands, the level and block size, and a re-cut.
fn written(recording: &Value, variant: &Value) -> Vec<u8> {
    let analog = |name: &str| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Analog {
            unit: "V".into(),
            period: ratio(&recording["period"]),
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
    let ops = variant["operands"].as_array().unwrap();
    let (a, b) = (ops[0].as_str().unwrap(), ops[1].as_str().unwrap());
    let derived = match variant["op"].as_str().unwrap() {
        "multiply" => Derived::multiply("P", "W", a, b),
        "subtract" => Derived::subtract("P", "W", a, b),
        op => panic!("{op}"),
    };
    // Each item promises its tick, so a block closes once it holds `block_bytes`.
    let options = WriteOptions {
        level: int(&variant["level"]) as i32,
        block_bytes: int(&variant["block_bytes"]) as usize,
        promise_with_items: true,
        ..WriteOptions::default()
    };
    let d = declaration(vec![analog("V"), analog("I")], vec![derived]);
    let mut w = Writer::new(d, options).unwrap();
    for c in 0..2 {
        w.run(c, 0, 0).unwrap();
    }
    for i in 0..20u64 {
        w.sample(0, i).unwrap();
        w.sample(1, i + 1).unwrap();
    }
    let mut file = w.take_output();
    let length = int(&recording["length"]);
    file.extend(w.finish(length, &[], Finish::default()).unwrap().tail);
    let Some(recut) = variant.get("recut") else {
        return file;
    };
    let mut r = Recutter::new(WriteOptions {
        level: int(&recut["level"]) as i32,
        block_bytes: int(&recut["block_bytes"]) as usize,
        ..WriteOptions::default()
    });
    r.feed(&file).unwrap();
    let mut out = r.take_output();
    out.extend(r.finish().unwrap().tail);
    assert!(out != file, "a re-cut in other blocks writes other bytes");
    out
}

/// The content id changes with the derived channel's operation and its operands' order, and not
/// with the compression level, the block size or a re-cut; each variant's is the one pinned.
#[test]
fn content_ids_are_the_vectors() {
    let v = vectors("content-ids.json");
    let (mut ids, mut pinned, mut files) = (BTreeMap::new(), BTreeMap::new(), BTreeMap::new());
    for variant in v["variants"].as_array().unwrap() {
        let name = variant["name"].as_str().unwrap().to_owned();
        let file = written(&v["recording"], variant);
        let id = Footer::parse_file(&file)
            .unwrap()
            .header
            .content_id
            .to_string();
        ids.insert(name.clone(), id);
        pinned.insert(
            name.clone(),
            variant["content_id"].as_str().unwrap_or("").to_owned(),
        );
        files.insert(name, file);
    }
    assert_eq!(ids, pinned);
    for same in v["same"].as_array().unwrap() {
        let names: Vec<&str> = same
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.as_str().unwrap())
            .collect();
        for n in &names[1..] {
            assert_eq!(ids[names[0]], ids[*n], "{} and {n}", names[0]);
            assert!(
                files[names[0]] != files[*n],
                "{} and {n} differ in their bytes",
                names[0]
            );
        }
    }
    for pair in v["different"].as_array().unwrap() {
        let (a, b) = (pair[0].as_str().unwrap(), pair[1].as_str().unwrap());
        assert_ne!(ids[a], ids[b], "{a} and {b}");
    }
}
