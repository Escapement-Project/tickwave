//! The egui renderer's demo: a recording in a `Graph` with a `Strip` under it.
//!
//!     cargo run --release -p escapement-tickwave-egui --example demo -- web/fixtures/energy.tickwave
//!     cargo run --release -p escapement-tickwave-egui --example demo -- --live 100000
//!
//! With `--live N` it records a synthetic capture at N pairs a second, laid out as verge's energy
//! captures are, and shows it as it is written; with `--seconds S` as well, the recorder
//! finishes after S seconds and the demo reopens the recording finished. The colours are the
//! project's design tokens, from `site/tokens.css`. `--trace FILE` writes every span as a Chrome
//! trace, for Perfetto or chrome://tracing, and `--quit-after S` closes the window after S
//! seconds.

mod live;
mod tokens;

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use escapement_tickwave_egui::view::ChannelKind;
use escapement_tickwave_egui::{Graph, Strip, ViewState, Viewer, Window, Y};
use tracing::{error, info};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[derive(Parser)]
struct Args {
    /// A finished recording to open.
    #[arg(required_unless_present = "live")]
    recording: Option<PathBuf>,
    /// Record a synthetic capture at this many pairs a second and show it as it is written.
    #[arg(long, conflicts_with = "recording")]
    live: Option<u64>,
    /// With `--live`, finish the recording after this many seconds and reopen it finished.
    #[arg(long, requires = "live")]
    seconds: Option<u64>,
    /// Close the window after this many seconds.
    #[arg(long)]
    quit_after: Option<f64>,
    /// Write every span, at debug level, as a Chrome trace to this file.
    #[arg(long)]
    trace: Option<PathBuf>,
}

/// The window a live recording follows, in seconds, as verge's bench opens.
const LIVE_SPAN: i64 = 10;
/// The strip's height, and the room between it and the graph, in points.
const STRIP: f32 = 64.0;
const SPACE: f32 = 8.0;

struct Demo {
    viewer: Arc<Viewer>,
    state: ViewState,
    faces: [tokens::Face; 2],
    night: bool,
    trace: usize,
    flag: Option<usize>,
    lanes: Vec<usize>,
    /// The window to follow when Follow is pressed.
    follow: Window,
    /// The fixed range Fit Y replaced, for Fit Y pressed again.
    unfit: Option<Y>,
    quit_after: Option<f64>,
    _live: Option<live::Live>,
}

impl Demo {
    fn new(
        cc: &eframe::CreationContext<'_>,
        args: &Args,
        faces: [tokens::Face; 2],
    ) -> Result<Self, String> {
        let ctx = &cc.egui_ctx;
        let night = ctx.theme() == egui::Theme::Dark;
        ctx.set_visuals(tokens::visuals(&faces[usize::from(night)]));
        let (viewer, live, follow) = match (&args.recording, args.live) {
            (_, Some(rate)) => {
                let (viewer, live) = live::start(ctx, rate, args.seconds)?;
                let timebase = viewer.recording(|r| r.timebase());
                let span = LIVE_SPAN * (timebase.num() / timebase.den()) as i64;
                (viewer, Some(live), Window::Follow { span })
            }
            (Some(path), None) => {
                let viewer =
                    Viewer::open(ctx, path).map_err(|e| format!("{}: {e}", path.display()))?;
                info!(path = %path.display(), "opened");
                (Arc::new(viewer), None, ViewState::default().window)
            }
            (None, None) => return Err("a recording or --live".into()),
        };
        let (trace, flag, lanes) = viewer.recording(|r| {
            let channels = r.channels();
            let analog = |c: &&escapement_tickwave_egui::view::ChannelInfo| {
                !matches!(c.kind, ChannelKind::Digital)
            };
            let trace = r
                .channel("I")
                .filter(|&i| analog(&&channels[i]))
                .or_else(|| channels.iter().position(|c| analog(&c)));
            let lanes = channels
                .iter()
                .enumerate()
                .filter(|(_, c)| {
                    matches!(c.kind, ChannelKind::Digital) && c.name.starts_with("DIO")
                })
                .map(|(i, _)| i)
                .collect();
            (trace, r.channel("under_range"), lanes)
        });
        let trace = trace.ok_or("the recording has no analog channel to draw")?;
        info!(trace, ?flag, ?lanes, "drawing");
        Ok(Self {
            viewer,
            state: ViewState {
                window: follow.clone(),
                ..ViewState::default()
            },
            faces,
            night,
            trace,
            flag,
            lanes,
            follow,
            unfit: None,
            quit_after: args.quit_after,
            _live: live,
        })
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.toggle_value(&mut self.night, "Night").clicked() {
                ui.ctx()
                    .set_visuals(tokens::visuals(&self.faces[usize::from(self.night)]));
            }
            ui.toggle_value(&mut self.state.select_mode, "Select");
            let mut fit = matches!(self.state.y, Y::Fit);
            if ui.toggle_value(&mut fit, "Fit Y").clicked() {
                if fit {
                    self.unfit = Some(self.state.y);
                    self.state.y = Y::Fit;
                } else {
                    // Pressed again: the range it replaced, where there was one.
                    self.state.y = self.unfit.take().unwrap_or(Y::Fit);
                }
            }
            let mut following = matches!(self.state.window, Window::Follow { .. });
            if ui.toggle_value(&mut following, "Follow").clicked() {
                self.state.window = if following {
                    self.follow.clone()
                } else {
                    let extent = self.viewer.answer().and_then(|a| a.extent.clone());
                    Window::At(self.state.window.resolve(extent.as_ref()))
                };
            }
            if let Some(failure) = self.viewer.failure() {
                ui.colored_label(
                    self.faces[usize::from(self.night)].palette.attention_ink,
                    failure,
                );
            }
        });
    }
}

impl eframe::App for Demo {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if let Some(after) = self.quit_after {
            let now = ui.input(|i| i.time);
            if now >= after {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                // A frame at the deadline, though nothing else asks for one.
                ui.ctx().request_repaint_after_secs((after - now) as f32);
            }
        }
        let face = &self.faces[usize::from(self.night)].clone();
        egui::Panel::top("controls").show(ui, |ui| self.controls(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            let graph = Graph::new(&self.viewer, &mut self.state, &face.palette, self.trace)
                .flag(self.flag)
                .lanes(&self.lanes)
                .include_zero(true)
                .height(ui.available_height() - STRIP - SPACE);
            ui.add(graph);
            ui.add_space(SPACE);
            ui.add(
                Strip::new(&self.viewer, &mut self.state, &face.palette)
                    .include_zero(true)
                    .height(STRIP),
            );
        });
    }
}

fn main() -> std::process::ExitCode {
    let args = Args::parse();
    let (chrome, _flush) = match &args.trace {
        Some(path) => {
            let (layer, guard) = tracing_chrome::ChromeLayerBuilder::new()
                .file(path)
                .include_args(true)
                .build();
            let debug = tracing_subscriber::filter::LevelFilter::DEBUG;
            (Some(layer.with_filter(debug)), Some(guard))
        }
        None => (None, None),
    };
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        ))
        .with(chrome)
        .init();
    let faces = match (tokens::face(false), tokens::face(true)) {
        (Ok(day), Ok(night)) => [day, night],
        (Err(e), _) | (_, Err(e)) => {
            error!("site/tokens.css: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("tickwave")
            .with_inner_size([1280.0, 760.0]),
        ..Default::default()
    };
    let result = eframe::run_native(
        "tickwave demo",
        options,
        Box::new(move |cc| Ok(Box::new(Demo::new(cc, &args, faces)?))),
    );
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            error!("{e}");
            std::process::ExitCode::FAILURE
        }
    }
}
