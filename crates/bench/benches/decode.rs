//! Decoding and drawing decoded output, as instructions and estimated cycles under Callgrind, on
//! synthetic SWD: no recording, no file and no query thread, so a count is the pattern's alone.
//!
//! - `swd_session`: SWD, ADIv5 and MEM-AP over 2,000 transfers through the host, from windows already
//!   built: the per-transfer path from framing to admission and delivery.
//! - `track_frame`: one egui frame of an SWD track over those transfers, zoomed in on six of them with
//!   labels and parts, on half of one with its bits, and zoomed out on all of them as density
//!   bins.
use std::hint::black_box;
use std::sync::atomic::AtomicBool;

use egui::{Pos2, RawInput, Rect, vec2};
use escapement_tickwave_decode::{
    Host, InputSpec, Observed, Schema, Source, SourceError, Span, Store, Tick, Window, name,
};
use escapement_tickwave_decoders::{adiv5, swd};
use escapement_tickwave_egui::track::{Density, Labels, Template, Track, condition};
use escapement_tickwave_egui::{Palette, Shown};
use escapement_tickwave_view::format::mapping::Mapping;
use escapement_tickwave_view::format::model::Ratio;
use gungraun::prelude::*;
use gungraun::{Callgrind, FlamegraphConfig};

/// A bit every 10 ticks.
const PERIOD: Tick = 10;
const TRANSFERS: usize = 2_000;

/// SWD bits for a session of SELECT, TAR and DRW reads and writes, one turnaround cycle.
fn bits() -> Vec<bool> {
    let mut out = vec![false; 8];
    let mut put = |ap: bool, read: bool, addr: u8, data: u32| {
        let (a2, a3) = (addr & 4 != 0, addr & 8 != 0);
        out.extend([
            true,
            ap,
            read,
            a2,
            a3,
            ap ^ read ^ a2 ^ a3,
            false,
            true,
            true,
        ]);
        out.extend([true, false, false]);
        let data: Vec<bool> = (0..32).map(|k| data >> k & 1 != 0).collect();
        let parity = data.iter().filter(|&&b| b).count() % 2 == 1;
        if !read {
            out.push(true);
        }
        out.extend(data);
        out.push(parity);
        if read {
            out.push(true);
        }
        out.extend([false, false]);
    };
    put(false, false, 0x8, 0);
    put(true, false, 0x0, 0x2300_0012);
    put(true, false, 0x4, 0x2000_0000);
    for k in 3..TRANSFERS as u32 {
        put(true, k % 3 == 0, 0xC, k.wrapping_mul(2_654_435_761));
    }
    out
}

/// SWCLK and SWDIO for `bits`, served from windows built once.
struct Wires {
    events: Vec<(Tick, u8, bool)>,
    end: Tick,
}

impl Wires {
    fn new() -> Self {
        let mut events = vec![];
        let mut level = false;
        for (k, &b) in bits().iter().enumerate() {
            let at = 10 + k as Tick * PERIOD;
            if b != level {
                events.push((at, 1, b));
                level = b;
            }
            events.push((at + PERIOD / 2, 0, true));
            events.push((at + PERIOD, 0, false));
        }
        let end = events.last().map_or(0, |e| e.0) + PERIOD;
        Self { events, end }
    }
}

impl Source for Wires {
    fn window(
        &mut self,
        from: Tick,
        upto: Tick,
        _: &AtomicBool,
    ) -> Result<Option<Window>, SourceError> {
        let mut w = Window::new(from, upto);
        let before = self.events.partition_point(|e| e.0 < from);
        let level = |port: u8| {
            self.events[..before]
                .iter()
                .rev()
                .find(|e| e.1 == port)
                .is_some_and(|e| e.2)
        };
        for port in 0..2 {
            let known = from > 0;
            w.events.push(Observed::Start {
                port,
                level: known.then(|| level(port)),
            });
        }
        if from <= 0 {
            for port in 0..2 {
                w.events.push(Observed::Known {
                    at: 0,
                    port,
                    level: false,
                });
            }
        }
        let after = self.events.partition_point(|e| e.0 < upto.min(self.end));
        for &(at, port, level) in &self.events[before..after] {
            w.events.push(Observed::Edge { at, port, level });
        }
        if self.end < upto {
            w.end = Some(self.end);
        }
        Ok(Some(w))
    }
}

fn all() -> Vec<Span> {
    vec![Span {
        start: Tick::MIN,
        end: Tick::MAX,
    }]
}

/// The three analyses over the session, `keep` saying where their stores keep whole items.
fn host(keep: Vec<Span>) -> Host {
    let mut h = Host::default();
    let s = h
        .add(
            name("swd"),
            swd::definition(),
            InputSpec::Wire {
                source: Box::new(Wires::new()),
                local: None,
            },
            swd::Swd::factory(4096, 1).unwrap(),
            keep.clone(),
        )
        .unwrap();
    let a = h
        .add(
            name("adi"),
            adiv5::adi_definition(),
            InputSpec::Upstream(adiv5::adi_subscription(s)),
            adiv5::Adi::factory(),
            keep.clone(),
        )
        .unwrap();
    h.add(
        name("mem"),
        adiv5::mem_definition(),
        InputSpec::Upstream(adiv5::mem_subscription(a)),
        adiv5::Mem::factory(),
        keep,
    )
    .unwrap();
    h
}

/// The host, its stores keeping every item or counting them only.
fn session(keep_all: bool) -> Host {
    host(if keep_all { all() } else { vec![] })
}

#[library_benchmark]
#[bench::kept(args = (true,), setup = session)]
#[bench::counted(args = (false,), setup = session)]
fn swd_session(mut h: Host) -> Host {
    h.run_until(|_| false).unwrap();
    black_box(&h);
    h
}

/// What a host keeps to draw a track.
struct Drawn {
    ctx: egui::Context,
    palette: Palette,
    schema: Schema,
    store: Store,
    density: Density,
    labels: Labels,
    shown: Shown,
}

fn drawn(window: std::ops::Range<i64>) -> Drawn {
    let mut h = host(all());
    h.run_until(|_| false).unwrap();
    let a = escapement_tickwave_decode::AnalysisId(0);
    let store = h.store(a).clone();
    let map = Mapping::new(0, 0, Ratio::integer(1).unwrap());
    let density = Density::of(store.items.values(), &map, condition);
    let labels = Labels::default().kind(
        swd::TRANSFER,
        vec![
            Template::default()
                .field(swd::DIR)
                .text(" ")
                .field(swd::PORT)
                .text(" ")
                .field(swd::A)
                .text(" ")
                .field(swd::ACK)
                .text(" ")
                .field(swd::DATA),
            Template::default().field(swd::PORT),
        ],
    );
    let ctx = egui::Context::default();
    let mut d = Drawn {
        palette: Palette::from_visuals(&egui::Visuals::default()),
        schema: h.definition(a).schema.clone(),
        store,
        density,
        labels,
        shown: Shown::new(window, 100.0..2000.0),
        ctx,
    };
    // Fonts and their glyphs laid out once, as a host's frames after its first.
    for _ in 0..3 {
        frame(&mut d);
    }
    d
}

fn frame(d: &mut Drawn) -> egui::FullOutput {
    let input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(2000.0, 200.0))),
        ..RawInput::default()
    };
    let map = Mapping::new(0, 0, Ratio::integer(1).unwrap());
    d.ctx.run_ui(input, |ui| {
        ui.add(
            Track::new(
                &d.shown,
                "SWD",
                &d.schema,
                &d.store.items,
                &d.density,
                &d.labels,
                &d.palette,
                map,
            )
            .parts(true),
        );
    })
}

/// Six transfers in from the session's middle.
fn zoomed_in() -> Drawn {
    let mid = 10 + (TRANSFERS as Tick / 2) * 48 * PERIOD;
    drawn(mid..mid + 6 * 48 * PERIOD)
}

/// Half a transfer: its request and acknowledge bits.
fn bits_in() -> Drawn {
    let mid = 10 + (TRANSFERS as Tick / 2) * 48 * PERIOD;
    drawn(mid..mid + 24 * PERIOD)
}

fn zoomed_out() -> Drawn {
    drawn(0..10 + TRANSFERS as Tick * 48 * PERIOD)
}

#[library_benchmark]
#[bench::bits(setup = bits_in)]
#[bench::zoomed_in(setup = zoomed_in)]
#[bench::zoomed_out(setup = zoomed_out)]
fn track_frame(mut d: Drawn) -> Drawn {
    black_box(frame(&mut d));
    d
}

library_benchmark_group!(name = decode, benchmarks = [swd_session, track_frame]);

main!(
    config = LibraryBenchmarkConfig::default()
        .tool(Callgrind::default().flamegraph(FlamegraphConfig::default())),
    library_benchmark_groups = decode
);
