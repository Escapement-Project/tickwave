//! The egui renderer's demo: a recording in a `Graph` with a `Strip` under it.
//!
//!     cargo run --release -p escapement-tickwave-egui --features decode --example demo -- web/fixtures/energy.tickwave
//!     cargo run --release -p escapement-tickwave-egui --features decode --example demo -- --live 100000
//!
//! With `--live N` it records a synthetic capture at N pairs a second, laid out as verge's energy
//! captures are, and shows it as it is written; with `--seconds S` as well, the recorder
//! finishes after S seconds and the demo reopens the recording finished; with `--from S` it starts
//! S seconds in, written as fast as they can be. A recording's stored view (ANNOTATIONS.md, "The
//! stored view") gives the channels drawn, the window it opens at and the trace's colour, as a
//! page's view takes them; a recording that stores no colour draws its trace in ink. The other
//! colours are the project's design tokens, from `site/tokens.css`. `--trace FILE` writes every
//! span as a Chrome trace, for Perfetto or chrome://tracing, and `--quit-after S` closes the
//! window after S seconds. For measuring, `--window FROM..TO` opens at a window, in seconds, and
//! `--repaint` draws every frame though nothing changes.
//!
//! `--recipe FILE` decodes the recording as the file's `[[decode]]` tables say, through the
//! reference decoders, and draws the decoded tracks under the graph with the side panel beside it:
//!
//!     cargo run --release -p escapement-tickwave-egui --features decode --example demo -- \
//!         web/fixtures/identify-swd.tickwave --recipe crates/decoders/recipes/swd.toml
//!
//! A recording with no analog channel then draws its digital ones as lanes alone.

mod live;
mod tokens;

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use escapement_tickwave_decode::HostOptions;
use escapement_tickwave_decoders::recipe::{self, Recipe};
use escapement_tickwave_egui::view::cursor::timeline_map;
use escapement_tickwave_egui::view::{Cache, ChannelKind, Recording};
use escapement_tickwave_egui::{
    BindFailure, Decode, DecodeOptions, DecodeState, Decoding, Graph, Place, Strip, Tracked,
    ViewState, Viewer, Window, Y,
};
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
    /// With `--live`, write this many seconds as fast as they can be before showing it, to
    /// follow a recording that has run that long.
    #[arg(long, requires = "live", default_value_t = 0)]
    from: u64,
    /// Close the window this many seconds after its first frame.
    #[arg(long)]
    quit_after: Option<f64>,
    /// Write every span, at debug level, as a Chrome trace to this file.
    #[arg(long)]
    trace: Option<PathBuf>,
    /// Open at this window, `FROM..TO` in seconds, rather than following.
    #[arg(long, value_parser = seconds)]
    window: Option<(f64, f64)>,
    /// Draw every frame, though nothing changes, to measure a frame's cost.
    #[arg(long)]
    repaint: bool,
    /// Decode the recording as this file's `[[decode]]` tables say.
    #[arg(long, requires = "recording")]
    recipe: Option<PathBuf>,
}

/// `FROM..TO`, in seconds.
fn seconds(s: &str) -> Result<(f64, f64), String> {
    let (a, b) = s.split_once("..").ok_or("FROM..TO")?;
    let n = |x: &str| x.trim().parse::<f64>().map_err(|e| format!("{x}: {e}"));
    Ok((n(a)?, n(b)?))
}

/// The window a live recording follows, in seconds, as verge's bench opens.
const LIVE_SPAN: i64 = 10;
/// The strip's height, and the room between it and the graph, in points.
const STRIP: f32 = 64.0;
const SPACE: f32 = 8.0;
/// The side panel's width beside the graph.
const PANEL: f32 = 360.0;

/// Binds `recipe` over the recording at `path`, as `Decode` asks of its host.
fn bind(
    path: PathBuf,
    recipe: Recipe,
) -> impl Fn() -> Result<Decoding, BindFailure> + Send + Sync + 'static {
    move || {
        let file = std::fs::File::open(&path)?;
        let rec = Arc::new(Recording::open(Arc::new(file), Cache::new(64 << 20))?);
        let b = recipe.bind(&rec, recipe::window(&rec), &[], HostOptions::default())?;
        let mut analyses = Vec::with_capacity(b.analyses.len());
        for a in b.analyses {
            analyses.push(Tracked {
                id: a.id,
                name: a.label,
                map: timeline_map(&rec, a.timeline)?,
            });
        }
        Ok(Decoding {
            host: b.host,
            analyses,
        })
    }
}

struct Demo {
    viewer: Arc<Viewer>,
    state: ViewState,
    faces: [tokens::Face; 2],
    night: bool,
    /// None draws the lanes alone.
    trace: Option<usize>,
    flag: Option<usize>,
    lanes: Vec<usize>,
    decode: Option<(Decode, DecodeState)>,
    /// The window to follow when Follow is pressed.
    follow: Window,
    /// The fixed range Fit Y replaced, for Fit Y pressed again.
    unfit: Option<Y>,
    quit_after: Option<f64>,
    /// egui's time at the first frame, which `quit_after` counts from.
    first: Option<f64>,
    repaint: bool,
    live: Option<live::Live>,
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
                let (viewer, live) = live::start(ctx, rate, args.seconds, args.from)?;
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
        // The view the recording stores, where it stores one that reads.
        let stored = viewer.recording(|r| r.stored_view()).ok().flatten();
        let (trace, flag, lanes) = viewer.recording(|r| {
            let channels = r.channels();
            let analog = |c: &&escapement_tickwave_egui::view::ChannelInfo| {
                !matches!(c.kind, ChannelKind::Digital)
            };
            // Its channels, in its order: the first analog one the trace, the digital ones lanes.
            let named: Option<Vec<usize>> = (stored.as_ref())
                .and_then(|v| v.channels.as_ref())
                .map(|names| names.iter().filter_map(|n| r.channel(n)).collect());
            let trace = match &named {
                Some(named) => named.iter().copied().find(|&i| analog(&&channels[i])),
                None => r.channel("I").filter(|&i| analog(&&channels[i])),
            }
            .or_else(|| channels.iter().position(|c| analog(&c)));
            let digital = |c: &escapement_tickwave_egui::view::ChannelInfo| {
                matches!(c.kind, ChannelKind::Digital)
            };
            let lanes: Vec<usize> = match &named {
                Some(named) => (named.iter().copied())
                    .filter(|&i| digital(&channels[i]))
                    .collect(),
                None => (channels.iter().enumerate())
                    .filter(|(_, c)| digital(c) && c.name.starts_with("DIO"))
                    .map(|(i, _)| i)
                    .collect(),
            };
            // A protocol capture names no DIO lanes: its digital channels, in order.
            let lanes = match lanes.is_empty() && named.is_none() {
                true => (channels.iter().enumerate())
                    .filter(|(_, c)| digital(c))
                    .map(|(i, _)| i)
                    .collect(),
                false => lanes,
            };
            (trace, r.channel("under_range"), lanes)
        });
        let decode = match (&args.recipe, &args.recording) {
            (Some(file), Some(path)) => {
                let text = std::fs::read_to_string(file)
                    .map_err(|e| format!("{}: {e}", file.display()))?;
                let recipe =
                    Recipe::parse(&text).map_err(|e| format!("{}: {e}", file.display()))?;
                let bind = bind(path.clone(), recipe);
                Some((
                    Decode::start(ctx, DecodeOptions::default(), bind),
                    DecodeState::default(),
                ))
            }
            _ => None,
        };
        if trace.is_none() && decode.is_none() {
            return Err("the recording has no analog channel to draw".into());
        }
        info!(trace, ?flag, ?lanes, "drawing");
        let stored_span = (stored.as_ref())
            .filter(|v| v.start.is_some() || v.end.is_some())
            .map(|v| {
                let length = viewer.recording(|r| r.length());
                Window::At(v.start.unwrap_or(0)..v.end.unwrap_or(length))
            });
        let window = match (args.window, stored_span) {
            (Some((a, b)), _) => {
                let timebase = viewer.recording(|r| r.timebase());
                let tick = |s: f64| (s * timebase.num() as f64 / timebase.den() as f64) as i64;
                Window::At(tick(a)..tick(b))
            }
            (None, Some(span)) => span,
            (None, None) => follow.clone(),
        };
        Ok(Self {
            viewer,
            state: {
                let mut state = ViewState::default();
                state.window = window;
                state
            },
            faces,
            night,
            trace,
            flag,
            lanes,
            decode,
            follow,
            unfit: None,
            quit_after: args.quit_after,
            first: None,
            repaint: args.repaint,
            live,
        })
    }

    /// The host's now while the recording is being written.
    fn now(&self) -> Option<i64> {
        self.live.as_ref().and_then(live::Live::now)
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
                    Window::At(self.state.window.resolve(extent.as_ref(), self.now()))
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
        if self.repaint {
            ui.ctx().request_repaint();
        }
        if let Some(after) = self.quit_after {
            let now = ui.input(|i| i.time);
            let after = after + *self.first.get_or_insert(now);
            if now >= after {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                // A frame at the deadline, though nothing else asks for one.
                ui.ctx().request_repaint_after_secs((after - now) as f32);
            }
        }
        egui::Panel::top("controls").show(ui, |ui| self.controls(ui));
        let now = self.now();
        let face = &self.faces[usize::from(self.night)];
        egui::CentralPanel::default().show(ui, |ui| {
            let tracks = (self.decode.as_ref()).map_or(0.0, |(d, s)| d.tracks_height(s));
            let height = ui.available_height() - STRIP - SPACE - tracks;
            let graph = match self.trace {
                Some(trace) => Graph::new(&self.viewer, &mut self.state, &face.palette, trace),
                None => Graph::lanes_only(&self.viewer, &mut self.state, &face.palette),
            }
            .flag(self.flag)
            .lanes(&self.lanes)
            .include_zero(true)
            .now(now)
            .height(height);
            match &mut self.decode {
                Some((decode, state)) => {
                    ui.add(graph.decoded(decode, state).panel(Place::Beside(PANEL)));
                }
                None => {
                    ui.add(graph);
                }
            }
            ui.add_space(SPACE);
            ui.add(
                Strip::new(&self.viewer, &mut self.state, &face.palette)
                    .include_zero(true)
                    .now(now)
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
