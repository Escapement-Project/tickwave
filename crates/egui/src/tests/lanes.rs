//! A graph of lanes alone, over a recording of digital channels only, as `verge dio record`
//! writes one: finished, and followed as it is written.

use std::fs::File;
use std::io::Write as _;

use escapement_tickwave_format::snapshot::Snapshot;

use super::*;
use crate::Window;
use crate::graph::{AXIS_HEIGHT, LABEL_TOP, LANE_HEIGHT};

/// DIO0 to DIO3 on a 1 MHz axis: DIO0 a 100 Hz square wave, DIO1 a 10 Hz one, DIO2 a 1 ms pulse
/// each second, DIO3 quiet; a marker each second, and a flush at each second's end, as a
/// recorder writes them.
struct Pins {
    writer: Writer,
    written: i64,
    markers: u64,
}

impl Pins {
    fn new() -> (Self, Vec<u8>) {
        let timebase = Ratio::integer(SECOND as u64).unwrap();
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
            channels: ["DIO0", "DIO1", "DIO2", "DIO3"].map(digital).to_vec(),
            derived: Vec::new(),
            sources: vec![SourceHeader::new(
                SourceFormat::Other("verge-dio".into()),
                timebase,
            )],
            title: None,
            meta: BTreeMap::new(),
            overview_shift: 12,
            requires: Vec::new(),
        };
        let options = WriteOptions {
            level: 1,
            block_bytes: 1 << 16,
            promise_with_items: true,
            ..WriteOptions::default()
        };
        let mut writer = Writer::new(declaration, options).unwrap();
        let head = writer.take_output();
        let pins = Self {
            writer,
            written: 0,
            markers: 0,
        };
        (pins, head)
    }

    /// Every edge in [from, to), in order: (tick, channel).
    fn edges(from: i64, to: i64) -> Vec<(i64, usize)> {
        let mut out = Vec::new();
        for (c, half) in [(0, 5 * MS), (1, 50 * MS)] {
            let mut t = (from + half - 1) / half * half;
            while t < to {
                out.push((t, c));
                t += half;
            }
        }
        for s in from / SECOND..=to / SECOND {
            for t in [s * SECOND + 200 * MS, s * SECOND + 201 * MS] {
                if (from..to).contains(&t) {
                    out.push((t, 2));
                }
            }
        }
        out.sort();
        out
    }

    /// Writes on to `to`, a whole number of seconds, and the bytes it adds.
    fn write(&mut self, to: i64) -> Vec<u8> {
        while self.written < to {
            let end = self.written + SECOND;
            for (t, c) in Self::edges(self.written, end) {
                self.writer.edge(c, t).unwrap();
            }
            self.markers += 1;
            let serde_json::Value::Object(marker) = serde_json::json!({
                "id": format!("marker/{}", self.markers),
                "kind": "marker",
                "start": self.written + 500 * MS,
                "label": "pins",
                "values": {"number": self.markers.to_string()},
            }) else {
                unreachable!("an object")
            };
            self.writer.annotation("verge", marker).unwrap();
            self.writer.flush(end).unwrap();
            self.written = end;
        }
        self.writer.take_output()
    }
}

fn lanes_only(h: &mut Harness<'_, Host>) {
    let host = h.state_mut();
    (host.trace, host.flag, host.lanes) = (None, None, vec![0, 1, 2, 3]);
}

/// Lane `k`'s marks: its meshes' corners in the trace's colour at any opacity, as a run's band
/// and rails are drawn.
fn marks(h: &Harness<'_, Host>, k: usize) -> Vec<Pos2> {
    let (lane, trace) = (drawn(h).layout.lanes[k], palette().trace.unwrap());
    shapes(h)
        .iter()
        .filter(|s| s.clip_rect == lane)
        .filter_map(|s| match &s.shape {
            Shape::Mesh(mesh) => Some(mesh.vertices.clone()),
            _ => None,
        })
        .flatten()
        .filter(|v| {
            let a = v.color.a();
            a > 0 && v.color == trace.gamma_multiply(f32::from(a) / 255.0)
        })
        .map(|v| v.pos)
        .collect()
}

#[test]
fn a_graph_of_lanes_alone_asks_for_its_lanes_and_draws_no_value_axis() {
    let (mut pins, mut file) = Pins::new();
    file.extend(pins.write(5 * SECOND));
    let length = 5 * SECOND;
    let tail = pins.writer.finish(
        length,
        &[length; 4],
        Finish {
            bundle: Some(Bundle::Generated {
                stem: "pins".into(),
            }),
            ..Finish::default()
        },
    );
    file.extend(tail.unwrap().tail);
    let (mut h, _gate) = showing(file, ViewState::default(), true);
    lanes_only(&mut h);
    let a = wait(&mut h);
    assert_eq!(a.extent, Some(0..length));
    let sent = viewer(&h).sent().unwrap();
    assert_eq!(
        (sent.channels.clone(), sent.strip, sent.flags.clone()),
        (vec![0, 1, 2, 3], None, Vec::new())
    );
    // Its height: the markers' label rows, the four lanes and the time axis.
    let l = drawn(&h).layout;
    let band = LABEL_TOP + 3.0 * l.label_row;
    assert!((l.plot.height() - band).abs() < 0.01, "{:?}", l.plot);
    assert!(
        (l.rect.height() - (band + 4.0 * LANE_HEIGHT + AXIS_HEIGHT)).abs() < 0.01,
        "{:?}",
        l.rect
    );
    // In the gutter, the lanes' names alone: no value axis.
    let gutter: Vec<String> = texts(&h)
        .into_iter()
        .filter(|(_, r)| r.right() <= l.plot.left())
        .map(|(t, _)| t)
        .collect();
    assert_eq!(gutter, ["DIO0", "DIO1", "DIO2", "DIO3"]);
    // DIO0 to DIO2 change and draw; DIO3, quiet, is a flat line.
    for k in 0..4 {
        assert!(!marks(&h, k).is_empty(), "lane {k}");
    }
}

#[test]
fn a_live_recording_of_lanes_alone_is_followed_as_it_is_written() {
    let (mut pins, head) = Pins::new();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pins.tickwave");
    let mut file = File::create(&path).unwrap();
    file.write_all(&head).unwrap();
    let mut write = |pins: &mut Pins, to: i64| -> Snapshot {
        file.write_all(&pins.write(to)).unwrap();
        file.flush().unwrap();
        pins.writer.snapshot()
    };
    let snapshot = write(&mut pins, 2 * SECOND);
    let mut h = harness(ViewState {
        window: Window::Follow { span: SECOND },
        ..ViewState::default()
    });
    lanes_only(&mut h);
    h.state_mut().viewer = Some(Viewer::live(&h.ctx, &path, snapshot).unwrap());
    let a = followed(&mut h, 2 * SECOND);
    assert_eq!(a.extent, Some(0..2 * SECOND));
    assert!(!a.markers.window.entries.is_empty(), "the second's marker");
    // Two seconds more: the window follows to the new end, and the lanes draw up to it and no
    // further.
    let snapshot = write(&mut pins, 4 * SECOND);
    viewer(&h).update(snapshot);
    let a = followed(&mut h, 4 * SECOND);
    assert_eq!(a.extent, Some(0..4 * SECOND));
    assert_eq!(drawn(&h).window, 3 * SECOND..4 * SECOND);
    let l = drawn(&h).layout;
    for k in 0..3 {
        let xs = marks(&h, k);
        assert!(!xs.is_empty(), "lane {k}");
        let right = xs.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max);
        assert!(
            right <= l.plot.right() + 1.0,
            "lane {k} past the plot: {right}"
        );
    }
    // DIO0's 100 Hz: in the last second's window, a hundred edges a thousand points across
    // stand apart, each a stroke, so its lane's marks reach the plot's right end.
    let xs = marks(&h, 0);
    let right = xs.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max);
    assert!(right > l.plot.right() - 12.0, "DIO0 ends at {right}");
}
