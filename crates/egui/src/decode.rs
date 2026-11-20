//! A decode view's analyses: a decode host whose forward pass runs on a worker thread, and what a
//! view draws of each analysis. `Graph::decoded` draws them under a graph.
//!
//! The forward pass keeps no items. Each analysis's tally of what it decoded, counted as items
//! close, draws zoomed out; zoomed in, the window the graph shows and a width either side of it
//! is derived again in a window chain from the nearest checkpoint before it, whose items the
//! tracks and the panel read. The chain a new one replaces stays drawn until the new one is.

use crate::graph::{Graph, Shown};
use crate::palette::Palette;
use crate::panel::{Analysis as PanelAnalysis, Panel, PanelState};
use crate::track::{
    self, Density, Labels, Track, condition, decoder_roles, driver_paints, track_height,
};
use egui::{Response, Ui, Widget};
use escapement_tickwave_decode::{
    AnalysisId, Host, Id, Item, Keeping, RunId, Schema, Span, Store, Tick, WindowId,
};
use escapement_tickwave_view::format::mapping::Mapping;
use escapement_tickwave_view::format::model::Ratio;
use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{JoinHandle, Thread};
use std::time::{Duration, Instant};

/// Why binding failed: a recipe's refusal, the recording's, or the host's.
pub type BindFailure = Box<dyn std::error::Error + Send + Sync>;

/// The most items a window may hold, by the tally's count, for a chain to derive it; a window
/// with more draws as the tally's density.
pub const WINDOW_ITEMS_AT_MOST: u64 = 50_000;

/// The longest window a chain derives ahead of the forward pass, from a local start: the tally
/// cannot say how many items a window it has not reached holds, so a longer one waits for it.
pub const AHEAD_AT_MOST: Duration = Duration::from_millis(10);

/// The window a view shows, on the axis, over a plot `width` points wide, its ticks
/// `per_second` a second.
#[derive(Clone, Debug)]
struct Asked {
    window: Range<i64>,
    width: f32,
    per_second: f64,
}

/// What a bind function gives `Decode`: a bound host, and how a view draws each analysis.
pub struct Decoding {
    pub host: Host,
    /// The analyses the view draws, in the order of their tracks.
    pub analyses: Vec<Tracked>,
}

/// An analysis as a view draws it.
pub struct Tracked {
    pub id: AnalysisId,
    /// Its track's name.
    pub name: String,
    /// Its timeline on the recording's axis: its channel's, as `view::cursor::timeline_map` gives
    /// it.
    pub map: Mapping,
}

/// How `Decode` runs. `Default`: a 64 MiB checkpoint budget, and a repaint at most every 100 ms
/// while the pass runs.
#[derive(Clone, Debug)]
pub struct DecodeOptions {
    /// What the checkpoints may hold, in bytes.
    pub checkpoint_budget: usize,
    /// The least time between repaints while the pass runs.
    pub repaint_every: Duration,
}

impl Default for DecodeOptions {
    fn default() -> Self {
        Self {
            checkpoint_budget: 64 << 20,
            repaint_every: Duration::from_millis(100),
        }
    }
}

/// Where decoding stands.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Progress {
    /// The worker is binding.
    Binding,
    /// Each analysis's, in the order of their tracks.
    Running(Vec<Reached>),
    /// Why binding or a turn was refused.
    Refused(String),
    /// Released, until `reopen`.
    Released,
}

/// How far an analysis's forward pass has gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reached {
    /// Nothing before it on its timeline will change.
    pub settled: Tick,
    /// Where it finished, once it has.
    pub finished: Option<Tick>,
}

/// A recording's analyses, decoded on a worker thread. Send and Sync: a host keeps it beside its
/// `Viewer`, and may call `release` and `reopen` from another thread.
pub struct Decode {
    shared: Arc<Shared>,
    bind: Arc<dyn Fn() -> Result<Decoding, BindFailure> + Send + Sync>,
    ctx: egui::Context,
    options: DecodeOptions,
    worker: Mutex<Option<(JoinHandle<()>, Thread)>>,
}

struct Shared {
    inner: Mutex<Inner>,
    /// The UI waits for `inner`: the worker leaves it be between steps.
    wants: AtomicBool,
    stop: AtomicBool,
    /// The window the view shows, each frame.
    asked: Mutex<Option<Asked>>,
}

pub(crate) struct Inner {
    pub(crate) phase: Phase,
    /// The window last asked for, kept across a reopen.
    shown: Option<Asked>,
}

pub(crate) enum Phase {
    Binding,
    Running(Box<Running>),
    Refused(String),
    Released,
}

pub(crate) struct Running {
    pub(crate) host: Host,
    pub(crate) lanes: Vec<Lane>,
    /// Each wire analysis's window chains, by the lane of its wire analysis.
    pub(crate) chains: Vec<Chains>,
}

/// An analysis as the view draws it.
pub(crate) struct Lane {
    pub(crate) id: AnalysisId,
    pub(crate) name: String,
    pub(crate) map: Mapping,
    pub(crate) schema: Schema,
    pub(crate) labels: Labels,
    /// What the forward pass decoded, tallied as items closed: what draws zoomed out.
    pub(crate) tally: Density,
    /// The forward run the tally counts; a run that replaces it counts afresh.
    run: RunId,
    /// The lane of the analysis it is stacked on, and of the wire analysis under it all.
    pub(crate) reads: Option<usize>,
    pub(crate) root: usize,
}

/// A wire analysis's window chains: the one drawn, and the one being derived to replace it.
#[derive(Default)]
pub(crate) struct Chains {
    pub(crate) drawn: Option<Chain>,
    pub(crate) pending: Option<Chain>,
}

pub(crate) struct Chain {
    pub(crate) id: WindowId,
    /// What it derives, on its wire analysis's timeline, and the same on the axis.
    span: Span,
    pub(crate) axis: Range<i64>,
    pub(crate) provisional: bool,
    /// Each lane's density over the chain's store, by lane; None for a lane not in the chain.
    pub(crate) densities: Vec<Option<Density>>,
}

impl Decode {
    /// Starts a worker that calls `bind` and runs the forward pass of the host it gives, and
    /// returns at once. `bind` opens the recording and binds its analyses; a refusal shows in
    /// `progress`. `reopen` calls it again.
    pub fn start(
        ctx: &egui::Context,
        options: DecodeOptions,
        bind: impl Fn() -> Result<Decoding, BindFailure> + Send + Sync + 'static,
    ) -> Self {
        let d = Self {
            shared: Arc::new(Shared {
                inner: Mutex::new(Inner {
                    phase: Phase::Binding,
                    shown: None,
                }),
                wants: AtomicBool::new(false),
                stop: AtomicBool::new(false),
                asked: Mutex::new(None),
            }),
            bind: Arc::new(bind),
            ctx: ctx.clone(),
            options,
            worker: Mutex::new(None),
        };
        d.spawn(None);
        d
    }

    /// Runs a worker; with a host bound already, from it, else binding first.
    fn spawn(&self, bound: Option<Decoding>) {
        let shared = Arc::clone(&self.shared);
        let bind = Arc::clone(&self.bind);
        let (ctx, options) = (self.ctx.clone(), self.options.clone());
        shared.stop.store(false, Ordering::Release);
        let handle = std::thread::Builder::new()
            .name("tickwave decode".into())
            .spawn(move || work(&shared, &*bind, bound, &ctx, &options))
            .expect("the decode worker starts");
        let thread = handle.thread().clone();
        *self.lock_worker() = Some((handle, thread));
    }

    fn lock_worker(&self) -> MutexGuard<'_, Option<(JoinHandle<()>, Thread)>> {
        self.worker.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Stops the worker after the step under way and waits for it.
    fn stop(&self) {
        self.shared.stop.store(true, Ordering::Release);
        let worker = self.lock_worker().take();
        if let Some((handle, thread)) = worker {
            thread.unpark();
            let _ = handle.join();
        }
    }

    /// Lets go of the file, as `Viewer::release` does: stops the worker after the step under way
    /// and drops the host, its checkpoints with it. The tracks draw nothing until `reopen`.
    pub fn release(&self) {
        self.stop();
        self.lock().phase = Phase::Released;
    }

    /// Binds again, on the calling thread, and starts the pass from the beginning.
    pub fn reopen(&self) -> Result<(), BindFailure> {
        self.stop();
        match (self.bind)() {
            Ok(bound) => {
                self.lock().phase = Phase::Binding;
                self.spawn(Some(bound));
                Ok(())
            }
            Err(e) => {
                self.lock().phase = Phase::Refused(e.to_string());
                Err(e)
            }
        }
    }

    /// Where decoding stands.
    pub fn progress(&self) -> Progress {
        let g = self.lock();
        match &g.phase {
            Phase::Binding => Progress::Binding,
            Phase::Refused(why) => Progress::Refused(why.clone()),
            Phase::Released => Progress::Released,
            Phase::Running(r) => Progress::Running(
                (r.lanes.iter())
                    .map(|l| Reached {
                        settled: r.host.settled(l.id),
                        finished: r.host.finished(l.id),
                    })
                    .collect(),
            ),
        }
    }

    /// What the checkpoints hold now, in bytes.
    pub fn checkpoint_bytes(&self) -> usize {
        let g = self.lock();
        let Phase::Running(r) = &g.phase else {
            return 0;
        };
        (r.lanes.iter())
            .filter(|l| l.reads.is_none())
            .filter_map(|l| r.host.kept(l.id))
            .map(|k| k.bytes)
            .sum()
    }

    /// Hands the worker the window the view shows, on the axis, over a plot `width` points wide,
    /// the axis's ticks `per_second` a second.
    pub(crate) fn show(&self, window: Range<i64>, width: f32, per_second: f64) {
        let mut asked = self.shared.asked.lock().unwrap_or_else(|e| e.into_inner());
        *asked = Some(Asked {
            window,
            width,
            per_second,
        });
        drop(asked);
        if let Some((_, thread)) = self.lock_worker().as_ref() {
            thread.unpark();
        }
    }

    /// The analyses as the view draws them, the worker kept off them until the guard drops.
    pub(crate) fn lock(&self) -> InnerGuard<'_> {
        self.shared.wants.store(true, Ordering::Release);
        let g = self.shared.inner.lock().unwrap_or_else(|e| e.into_inner());
        self.shared.wants.store(false, Ordering::Release);
        InnerGuard(g)
    }
}

impl Drop for Decode {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(crate) struct InnerGuard<'a>(MutexGuard<'a, Inner>);

impl std::ops::Deref for InnerGuard<'_> {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl std::ops::DerefMut for InnerGuard<'_> {
    fn deref_mut(&mut self) -> &mut Inner {
        &mut self.0
    }
}

/// The worker: binds where it has no host, then steps it until stopped, taking in the window the
/// view asks for and what the forward pass tallied after each step. It parks while nothing runs.
fn work(
    shared: &Shared,
    bind: &(dyn Fn() -> Result<Decoding, BindFailure> + Send + Sync),
    bound: Option<Decoding>,
    ctx: &egui::Context,
    options: &DecodeOptions,
) {
    let bound = match bound.map_or_else(bind, Ok) {
        Ok(b) => b,
        Err(e) => {
            lock(shared).phase = Phase::Refused(e.to_string());
            ctx.request_repaint();
            return;
        }
    };
    let running = match Running::new(bound, options) {
        Ok(r) => r,
        Err(why) => {
            lock(shared).phase = Phase::Refused(why);
            ctx.request_repaint();
            return;
        }
    };
    let mut g = lock(shared);
    g.phase = Phase::Running(Box::new(running));
    let shown = g.shown.clone();
    drop(g);
    if let Some(s) = shown {
        let mut asked = shared.asked.lock().unwrap_or_else(|e| e.into_inner());
        asked.get_or_insert(s);
    }
    ctx.request_repaint();
    let mut repainted = Instant::now();
    loop {
        while shared.wants.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        if shared.stop.load(Ordering::Acquire) {
            return;
        }
        let asked = (shared.asked.lock().unwrap_or_else(|e| e.into_inner())).take();
        let mut g = lock(shared);
        if let Some(a) = &asked {
            g.shown = Some(a.clone());
        }
        let Phase::Running(r) = &mut g.phase else {
            return;
        };
        if let Some(a) = asked
            && let Err(why) = r.ask(&a)
        {
            g.phase = Phase::Refused(why);
            ctx.request_repaint();
            return;
        }
        let ran = match r.host.step() {
            Ok(ran) => ran,
            Err(why) => {
                g.phase = Phase::Refused(why.to_string());
                ctx.request_repaint();
                return;
            }
        };
        let drawn = r.after_step();
        drop(g);
        if drawn {
            ctx.request_repaint();
        } else if repainted.elapsed() >= options.repaint_every {
            ctx.request_repaint();
            repainted = Instant::now();
        }
        if !ran {
            ctx.request_repaint();
            std::thread::park();
        }
    }
}

fn lock(shared: &Shared) -> MutexGuard<'_, Inner> {
    shared.inner.lock().unwrap_or_else(|e| e.into_inner())
}

impl Running {
    fn new(bound: Decoding, options: &DecodeOptions) -> Result<Self, String> {
        let Decoding { mut host, analyses } = bound;
        let slot = |id: AnalysisId| analyses.iter().position(|t| t.id == id);
        let mut lanes = Vec::with_capacity(analyses.len());
        for t in &analyses {
            let reads = host.bound(t.id).map(|r| r.analysis);
            let reads = match reads {
                Some(up) => Some(slot(up).ok_or("an analysis reads one the view does not draw")?),
                None => None,
            };
            lanes.push(Lane {
                id: t.id,
                name: t.name.clone(),
                map: t.map,
                schema: host.definition(t.id).schema.clone(),
                labels: Labels::default(),
                tally: Density::default(),
                run: host.run_id(t.id),
                reads,
                root: 0,
            });
        }
        for k in 0..lanes.len() {
            let mut root = k;
            while let Some(up) = lanes[root].reads {
                root = up;
            }
            lanes[k].root = root;
        }
        for l in &lanes {
            host.set_keep(l.id, vec![]);
            host.tally(l.id);
            if l.reads.is_none() {
                let keeping =
                    Keeping::new(Tick::MAX, options.checkpoint_budget).with_events(100_000);
                host.keep_checkpoints(l.id, keeping)
                    .map_err(|e| e.to_string())?;
            }
        }
        let chains = (0..lanes.len()).map(|_| Chains::default()).collect();
        Ok(Self {
            host,
            lanes,
            chains,
        })
    }

    /// Takes in the window the view shows: each wire analysis whose items there are few enough
    /// derives it and a width either side in a chain, unless its chains cover it already. Ahead
    /// of the forward pass, where the tally has not counted them, only a short one.
    fn ask(&mut self, asked: &Asked) -> Result<(), String> {
        let (window, width) = (&asked.window, asked.width);
        let w = window.end.saturating_sub(window.start).max(1);
        let wide = window.start.saturating_sub(w)..window.end.saturating_add(w);
        for root in 0..self.lanes.len() {
            if self.lanes[root].reads.is_some() {
                continue;
            }
            let covers = |c: &Option<Chain>| {
                c.as_ref()
                    .is_some_and(|c| c.axis.start <= window.start && window.end <= c.axis.end)
            };
            if covers(&self.chains[root].drawn) || covers(&self.chains[root].pending) {
                continue;
            }
            let items: u64 = (self.lanes.iter())
                .filter(|l| l.root == root)
                .map(|l| l.tally.count(wide.start, wide.end, 0.0).0)
                .sum();
            let map = self.lanes[root].map;
            let span = Span {
                start: map.first_at(wide.start),
                end: map.first_at(wide.end),
            };
            let counted = self.host.finished(self.lanes[root].id).is_some()
                || self.host.consumed(self.lanes[root].id) >= span.end;
            let skip = if counted {
                let crowded = self.lanes[root].tally.crowded(window, width);
                items > WINDOW_ITEMS_AT_MOST || (crowded && items > 0)
            } else {
                let seconds = wide.end.saturating_sub(wide.start) as f64 / asked.per_second;
                seconds > AHEAD_AT_MOST.as_secs_f64()
            };
            if skip {
                continue;
            }
            let root_id = self.lanes[root].id;
            let derived = self.host.derive(root_id, span).map_err(|e| e.to_string())?;
            let densities = (self.lanes.iter())
                .map(|l| (l.root == root).then(Density::default))
                .collect();
            let chain = Chain {
                id: derived.window,
                span,
                axis: wide.clone(),
                provisional: derived.provisional,
                densities,
            };
            if let Some(old) = self.chains[root].pending.replace(chain) {
                self.host.retire(old.id);
            }
        }
        Ok(())
    }

    /// After a step: the tally of what each forward run closed, and each chain's density; a
    /// pending chain whose window is drawn replaces the drawn one. Whether one did.
    fn after_step(&mut self) -> bool {
        for l in &mut self.lanes {
            let run = self.host.run_id(l.id);
            if run != l.run {
                (l.tally, l.run) = (Density::default(), run);
            }
            let closed = self.host.closed(l.id);
            l.tally.tally(&closed, &l.map);
        }
        let mut swapped = false;
        for root in 0..self.chains.len() {
            // A window derived ahead of the forward pass, without context, is derived again from a
            // checkpoint once the pass is past it; it stays drawn until the new one is.
            let redo = (self.chains[root].drawn.as_ref())
                .filter(|c| c.provisional && self.chains[root].pending.is_none())
                .filter(|c| self.host.consumed(self.lanes[root].id) >= c.span.end)
                .map(|c| (c.span, c.axis.clone()));
            if let Some((span, axis)) = redo
                && let Ok(d) = self.host.derive(self.lanes[root].id, span)
            {
                let densities = (self.lanes.iter())
                    .map(|l| (l.root == root).then(Density::default))
                    .collect();
                self.chains[root].pending = Some(Chain {
                    id: d.window,
                    span,
                    axis,
                    provisional: d.provisional,
                    densities,
                });
            }
            // Drawn once no analysis in it will emit an item starting before the window's end: a
            // decoder can hold what it has read past, as SWD's recogniser holds rises ahead.
            let ready = self.chains[root].pending.as_ref().is_some_and(|p| {
                (self.lanes.iter().filter(|l| l.root == root)).all(|l| {
                    (self.host.window_run(p.id, l.id))
                        .is_some_and(|r| r.finished().is_some() || r.start_frontier() >= p.span.end)
                })
            });
            if ready {
                let new = self.chains[root].pending.take();
                if let Some(old) = std::mem::replace(&mut self.chains[root].drawn, new) {
                    self.host.retire(old.id);
                }
                swapped = true;
            }
            let Chains { drawn, pending } = &mut self.chains[root];
            for c in [drawn, pending].into_iter().flatten() {
                for (k, d) in c.densities.iter_mut().enumerate() {
                    let (Some(d), Some(run)) = (d, self.host.window_run(c.id, self.lanes[k].id))
                    else {
                        continue;
                    };
                    d.grow(
                        run.store().items(),
                        &self.lanes[k].map,
                        condition,
                        run.start_frontier(),
                    );
                }
            }
        }
        swapped
    }
}

/// A decode view's state, which its host owns, as it owns a graph's `ViewState`.
#[derive(Clone, Debug, Default)]
pub struct DecodeState {
    pub panel: PanelState,
    /// What the graph drew last frame, where this frame's decoded paint comes from.
    shown: Option<Shown>,
}

/// Where a decode view puts its side panel.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Place {
    /// Nowhere: the tracks alone. A click still selects an item.
    #[default]
    Nowhere,
    /// Beside the plot at this width, or under it, `UNDER` tall, where the width does not fit.
    Beside(f32),
    /// Under the plot, this tall.
    Under(f32),
}

/// How tall a panel that does not fit beside the plot is under it.
pub const UNDER: f32 = 240.0;

impl<'a> Graph<'a> {
    /// This graph with `decode`'s tracks under it, each on the graph's time axis.
    pub fn decoded(self, decode: &'a Decode, state: &'a mut DecodeState) -> DecodedGraph<'a> {
        DecodedGraph {
            graph: self,
            decode,
            state,
            place: Place::Nowhere,
            painted: None,
        }
    }
}

/// A graph with a recording's decoded tracks under it and, where the host places it, the side
/// panel: `Graph::decoded`. Its response is the graph's, so `Graph::shown` reads it.
pub struct DecodedGraph<'a> {
    graph: Graph<'a>,
    decode: &'a Decode,
    state: &'a mut DecodeState,
    place: Place,
    painted: Option<&'a [String]>,
}

impl<'a> DecodedGraph<'a> {
    pub fn panel(mut self, place: Place) -> Self {
        self.place = place;
        self
    }

    /// Paints `channels` by who drives each part of the first analysis's items, as
    /// `Graph::decoded_paint` does.
    pub fn painted(mut self, channels: &'a [String]) -> Self {
        self.painted = Some(channels);
        self
    }
}

impl Decode {
    /// The decoded tracks' height as `state` leaves them, item spacing included: for a host that
    /// lays out before it draws.
    pub fn tracks_height(&self, _state: &DecodeState) -> f32 {
        let gap = self.ctx.global_style().spacing.item_spacing.y;
        let g = self.lock();
        let Phase::Running(r) = &g.phase else {
            return 0.0;
        };
        (r.lanes.iter())
            .map(|l| track_height(&l.schema, l.reads.is_none()) + gap)
            .sum()
    }
}

impl Widget for DecodedGraph<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let DecodedGraph {
            graph,
            decode,
            state,
            place,
            painted,
        } = self;
        let (viewer, palette) = graph.borrows();
        let timebase = viewer.recording(|r| r.timebase());
        let place = match place {
            Place::Beside(w) if !Panel::fits_beside(ui.available_width(), w) => Place::Under(UNDER),
            p => p,
        };
        match place {
            Place::Beside(w) => {
                ui.horizontal_top(|ui| {
                    let reserved = Panel::reserved(&state.panel, w);
                    let gap = ui.spacing().item_spacing.x;
                    let response = (ui.vertical(|ui| {
                        ui.set_width(ui.available_width() - reserved - gap);
                        tracked(ui, graph, decode, state, painted)
                    }))
                    .inner;
                    panel(ui, decode, state, palette, timebase, w, None);
                    response
                })
                .inner
            }
            Place::Under(h) => {
                let response = tracked(ui, graph, decode, state, painted);
                panel(
                    ui,
                    decode,
                    state,
                    palette,
                    timebase,
                    ui.available_width(),
                    Some(h),
                );
                response
            }
            Place::Nowhere => tracked(ui, graph, decode, state, painted),
        }
    }
}

static NOTHING: BTreeMap<Id, Item> = BTreeMap::new();

/// What a lane draws this frame: the drawn chain's items where it meets the window, else none, over
/// the tally's density.
struct Source<'a> {
    items: &'a BTreeMap<Id, Item>,
    store: &'a Store,
    density: &'a Density,
    settled: Tick,
    lost: &'a [Span],
    provisional: bool,
}

impl Running {
    fn source(&self, k: usize, window: &Range<i64>) -> Source<'_> {
        let lane = &self.lanes[k];
        let chain = (self.chains[lane.root].drawn.as_ref())
            .filter(|c| c.axis.start < window.end && window.start < c.axis.end)
            .and_then(|c| {
                Some((
                    c,
                    self.host.window_run(c.id, lane.id)?,
                    c.densities[k].as_ref()?,
                ))
            });
        match chain {
            Some((c, run, density)) => Source {
                items: run.store().items(),
                store: run.store(),
                density,
                // The chain has emitted every item starting before its frontier; one still open
                // there draws as open, not as input not decoded yet.
                settled: run.start_frontier(),
                lost: run.lost(),
                provisional: c.provisional,
            },
            None => Source {
                items: &NOTHING,
                store: self.host.store(lane.id),
                density: &lane.tally,
                settled: self.host.settled(lane.id),
                lost: self.host.lost(lane.id),
                provisional: false,
            },
        }
    }
}

/// The graph, then a track for each analysis on its time axis.
fn tracked(
    ui: &mut Ui,
    graph: Graph<'_>,
    decode: &Decode,
    state: &mut DecodeState,
    painted: Option<&[String]>,
) -> Response {
    let (viewer, palette) = graph.borrows();
    let (mut paints, mut roles, mut gutter) = (vec![], vec![], 0.0f32);
    let g = decode.lock();
    if let Phase::Running(r) = &g.phase {
        for lane in &r.lanes {
            gutter = gutter.max(track::gutter(ui, palette, &lane.name, &lane.schema));
        }
        // The decoded paint from the first analysis, on the window drawn last frame.
        if let (Some(_), Some(s), Some(lane)) = (painted, &state.shown, r.lanes.first()) {
            let src = r.source(0, &s.window);
            paints = driver_paints(
                &lane.schema,
                src.items,
                src.density,
                &lane.map,
                &s.window,
                s.plot.end - s.plot.start,
            );
            roles = decoder_roles(&lane.schema);
        }
    }
    drop(g);
    let graph = match painted {
        Some(channels) => graph.decoded_paint(&paints, channels, &roles),
        None => graph,
    }
    .gutter(gutter);
    let response = ui.add(graph);
    let Some(shown) = Graph::shown(&response) else {
        return response;
    };
    let tb = viewer.recording(|r| r.timebase());
    let per_second = tb.num() as f64 / tb.den() as f64;
    decode.show(
        shown.window.clone(),
        shown.plot.end - shown.plot.start,
        per_second,
    );
    let g = decode.lock();
    if let Phase::Running(r) = &g.phase {
        for (k, lane) in r.lanes.iter().enumerate() {
            let src = r.source(k, &shown.window);
            let selected = (state.panel.selected)
                .filter(|(s, _)| *s == k)
                .map(|(_, id)| id);
            let track = Track::new(
                &shown,
                &lane.name,
                &lane.schema,
                src.items,
                src.density,
                &lane.labels,
                palette,
                lane.map,
            )
            .settled(src.settled)
            .lost(src.lost)
            .parts(lane.reads.is_none())
            .provisional(src.provisional)
            .selected(selected)
            .lit(state.panel.lit)
            .timebase(tb);
            let r = ui.add(track);
            if let Some(id) = Track::clicked(&r) {
                state.panel.selected = Some((k, id));
            }
        }
    }
    drop(g);
    state.shown = Some(shown);
    response
}

/// The side panel over what the tracks drew, `width` wide, beside the plot or `under` it.
fn panel(
    ui: &mut Ui,
    decode: &Decode,
    state: &mut DecodeState,
    palette: &Palette,
    timebase: Ratio,
    width: f32,
    under: Option<f32>,
) {
    let g = decode.lock();
    let Phase::Running(r) = &g.phase else {
        return;
    };
    let Some(window) = state.shown.as_ref().map(|s| s.window.clone()) else {
        return;
    };
    let analyses: Vec<_> = (0..r.lanes.len())
        .map(|k| {
            let lane = &r.lanes[k];
            PanelAnalysis {
                name: &lane.name,
                schema: &lane.schema,
                labels: &lane.labels,
                store: r.source(k, &window).store,
                reads: lane.reads,
            }
        })
        .collect();
    let map = r.lanes.first().map_or(Mapping::IDENTITY, |l| l.map);
    let p = Panel::new(palette, &mut state.panel, &analyses, width, timebase, map);
    ui.add(match under {
        Some(h) => p.under(h),
        None => p,
    });
}
