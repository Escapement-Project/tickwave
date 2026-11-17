//! A frame of egui's graph, as instructions and estimated cycles under Callgrind: the trace of I
//! and DIO0 to DIO3 in lanes, in a window 2,000 points wide, its answer landed and nothing
//! asked, so the query thread idles and the count is the frame's alone, egui's own work
//! included. Three frames: verge's layout plain; the same with four painted spans, two of them
//! dashed; and the plain one moved past what its answer covers, the viewer let go of its file so
//! nothing is asked, its lanes drawn from their columns over the extent.
use std::hint::black_box;
use std::time::Duration;

use egui::{Pos2, RawInput, Rect, vec2};
use escapement_tickwave_bench::{CLOCK_HZ, Capture};
use escapement_tickwave_egui::{Graph, Palette, ViewState, Viewer, Window};
use gungraun::prelude::*;
use gungraun::{Callgrind, FlamegraphConfig};

const DIR: &str = env!("CARGO_TARGET_TMPDIR");
const SECOND: i64 = CLOCK_HZ as i64;

/// What a host keeps for its graph.
struct Host {
    ctx: egui::Context,
    viewer: Viewer,
    state: ViewState,
    palette: Palette,
}

fn frame(host: &mut Host) -> egui::FullOutput {
    let Host {
        ctx,
        viewer,
        state,
        palette,
    } = host;
    let input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(2000.0, 900.0))),
        ..RawInput::default()
    };
    ctx.run_ui(input, |ui| {
        ui.add(
            Graph::new(viewer, state, palette, 1)
                .lanes(&[4, 5, 6, 7])
                .motion(false),
        );
    })
}

/// Frames until the answer for the window has landed and the graph asks nothing more.
fn settle(host: &mut Host) {
    let mut quiet = 0;
    for _ in 0..20_000 {
        frame(host);
        if host.viewer.answer().is_some() && !host.viewer.busy() {
            quiet += 1;
            if quiet == 5 {
                return;
            }
        } else {
            quiet = 0;
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    panic!("no answer settled");
}

/// `capture` over 4 s to 6 s, its answer landed.
fn landed(capture: Capture) -> Host {
    let ctx = egui::Context::default();
    let viewer = Viewer::open(&ctx, &capture.path(DIR)).unwrap();
    let mut state = ViewState::default();
    state.window = Window::At(4 * SECOND..6 * SECOND);
    let mut host = Host {
        palette: Palette::from_visuals(&egui::Visuals::default()),
        ctx,
        viewer,
        state,
    };
    settle(&mut host);
    host
}

/// `landed`'s, then the whole 10 s shown with the file let go of: the answer's `around`, 2 s to
/// 8 s, drawn from it, and the rest of each row from its columns over the extent.
fn past_around(capture: Capture) -> Host {
    let mut host = landed(capture);
    host.viewer.release();
    host.state.window = Window::At(0..10 * SECOND);
    for _ in 0..5 {
        frame(&mut host);
    }
    host
}

// The host is returned, so dropping it, and the viewer's thread with it, isn't counted.
#[library_benchmark]
#[bench::plain(args = (Capture::Verge,), setup = landed)]
#[bench::painted(args = (Capture::Painted,), setup = landed)]
#[bench::past_around(args = (Capture::Verge,), setup = past_around)]
fn graph_frame(mut host: Host) -> Host {
    black_box(frame(&mut host));
    host
}

library_benchmark_group!(name = egui_graph, benchmarks = [graph_frame]);

main!(
    config = LibraryBenchmarkConfig::default()
        .tool(Callgrind::default().flamegraph(FlamegraphConfig::default())),
    library_benchmark_groups = egui_graph
);
