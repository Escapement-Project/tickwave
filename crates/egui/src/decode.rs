//! A decode view's analyses: a recipe bound over a recording, its host's forward pass running on a
//! worker thread, and what a view draws of each analysis. `Graph::decoded` draws them under a
//! graph, and its side panel's Decoders tab changes them: a setting changed decodes the
//! analysis's chain again, a decoder added joins the host, and one removed binds the recipe again
//! without it.
//!
//! The forward pass keeps no items. Each analysis's tally of what it decoded, counted as items
//! close, draws zoomed out; zoomed in, the window the graph shows and a width either side of it
//! is derived again in a window chain from the nearest checkpoint before it, whose items the
//! tracks and the panel read. The chain a new one replaces stays drawn until the new one is.

use crate::graph::{Graph, Shown};
use crate::palette::Palette;
use crate::panel::{Analysis as PanelAnalysis, Change, Configured, Panel, PanelState, Stack, Tab};
use crate::state::Window;
use crate::track::{
    self, Density, Labels, Track, condition, decoder_roles, driver_paints, track_height,
};
use egui::{Response, Ui, Widget};
use escapement_tickwave_decode::recipe::{self, Analysis as Planned, Recipe, Registry};
use escapement_tickwave_decode::{
    AnalysisId, Host, HostOptions, Id, Item, Keeping, RunId, Schema, Setting, SettingType, Span,
    Store, Tick, WindowId,
};
use escapement_tickwave_view::format::mapping::Mapping;
use escapement_tickwave_view::format::model::Ratio;
use escapement_tickwave_view::{ChannelKind, Recording};
use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{JoinHandle, Thread};
use std::time::{Duration, Instant};

/// Why opening the recording failed.
pub type BindFailure = Box<dyn std::error::Error + Send + Sync>;

/// Opens the recording a decode binds its recipe over, again on `reopen`.
type Open = dyn Fn() -> Result<Arc<Recording>, BindFailure> + Send + Sync;

/// The most items a window may hold, by the tally's count, for a chain to derive it; a window
/// with more draws as the tally's density.
pub const WINDOW_ITEMS_AT_MOST: u64 = 50_000;

/// The longest window a chain derives ahead of the forward pass, from a local start: the tally
/// cannot say how many items a window it has not reached holds, so a longer one waits for it.
pub const AHEAD_AT_MOST: Duration = Duration::from_millis(10);

/// The window a view shows, on the axis, over a plot `width` points wide, its ticks
/// `per_second` a second.
#[derive(Clone, Debug)]
pub(crate) struct Asked {
    pub(crate) window: Range<i64>,
    pub(crate) width: f32,
    pub(crate) per_second: f64,
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

/// A recording's analyses, as a recipe names them, decoded on a worker thread. Send and Sync: a
/// host keeps it beside its `Viewer`, and may call `release` and `reopen` from another thread.
pub struct Decode {
    shared: Arc<Shared>,
    open: Arc<Open>,
    registry: Arc<Registry>,
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
    /// The analyses as the reader has them, which a reopen binds.
    recipe: Recipe,
    /// Why the last change the Decoders tab asked for was refused.
    refused: Option<String>,
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
    recording: Arc<Recording>,
    /// A wire decoder's window of input, in ticks.
    window: Tick,
    /// The recording's digital channels, by name: what a wire port can read.
    channels: Vec<String>,
    budget: usize,
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
    /// What the last finished run before a setting changed counted, while the new one has not
    /// finished: where items lie, for deciding which windows to derive ahead of the pass.
    pub(crate) estimate: Option<Density>,
    /// The channel whose source's timeline its items are on.
    timeline: usize,
    /// The lane of the analysis it is stacked on, and of the wire analysis under it all.
    pub(crate) reads: Option<usize>,
    pub(crate) root: usize,
}

impl Lane {
    /// What its pass counted, or, where the pass has not `counted` what is asked about, what the
    /// pass before a setting changed counted, while that is kept.
    fn counting(&self, counted: bool) -> &Density {
        match (counted, &self.estimate) {
            (false, Some(e)) => e,
            _ => &self.tally,
        }
    }
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
    /// Starts a worker that opens the recording with `open`, binds `recipe` over it through
    /// `registry`, and runs the forward pass, and returns at once. A refusal shows in `progress`.
    /// `reopen` opens and binds again, the recipe as the reader has changed it.
    pub fn start(
        ctx: &egui::Context,
        options: DecodeOptions,
        registry: Registry,
        recipe: Recipe,
        open: impl Fn() -> Result<Arc<Recording>, BindFailure> + Send + Sync + 'static,
    ) -> Self {
        let d = Self {
            shared: Arc::new(Shared {
                inner: Mutex::new(Inner {
                    phase: Phase::Binding,
                    shown: None,
                    recipe,
                    refused: None,
                }),
                wants: AtomicBool::new(false),
                stop: AtomicBool::new(false),
                asked: Mutex::new(None),
            }),
            open: Arc::new(open),
            registry: Arc::new(registry),
            ctx: ctx.clone(),
            options,
            worker: Mutex::new(None),
        };
        d.spawn(None);
        d
    }

    /// Runs a worker; with a host bound already, from it, else binding first.
    fn spawn(&self, bound: Option<Running>) {
        let shared = Arc::clone(&self.shared);
        let (open, registry) = (Arc::clone(&self.open), Arc::clone(&self.registry));
        let (ctx, options) = (self.ctx.clone(), self.options.clone());
        shared.stop.store(false, Ordering::Release);
        let handle = std::thread::Builder::new()
            .name("tickwave decode".into())
            .spawn(move || {
                let bind = || {
                    let recipe = lock(&shared).recipe.clone();
                    Running::bind(&*open, &registry, &recipe, &options)
                };
                work(&shared, bound.map_or_else(bind, Ok), &ctx, &options);
            })
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

    /// Opens the recording and binds the recipe again, on the calling thread, and starts the
    /// pass from the beginning.
    pub fn reopen(&self) -> Result<(), BindFailure> {
        self.stop();
        let recipe = self.lock().recipe.clone();
        match Running::bind(&*self.open, &self.registry, &recipe, &self.options) {
            Ok(bound) => {
                self.lock().phase = Phase::Binding;
                self.spawn(Some(bound));
                Ok(())
            }
            Err(why) => {
                self.lock().phase = Phase::Refused(why.clone());
                Err(why.into())
            }
        }
    }

    /// The recipe as the reader has it: the one started with, as the Decoders tab changed it.
    pub fn recipe(&self) -> Recipe {
        self.lock().recipe.clone()
    }

    /// Binds `recipe` in place of the one bound, on the worker, from the beginning.
    fn rebuild(&self, recipe: Recipe) {
        self.stop();
        let mut g = self.lock();
        (g.phase, g.recipe, g.refused) = (Phase::Binding, recipe, None);
        drop(g);
        self.spawn(None);
    }

    /// Wakes the worker, parked where it had nothing to run.
    pub(crate) fn wake(&self) {
        if let Some((_, thread)) = self.lock_worker().as_ref() {
            thread.unpark();
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
        self.wake();
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

/// The worker: runs the host `bound` gave, or shows why there is none, stepping it until stopped,
/// taking in the window the view asks for and what the forward pass tallied after each step. It
/// parks while nothing runs.
fn work(
    shared: &Shared,
    bound: Result<Running, String>,
    ctx: &egui::Context,
    options: &DecodeOptions,
) {
    let running = match bound {
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
    /// Opens the recording and binds `recipe` over it.
    fn bind(
        open: &Open,
        registry: &Registry,
        recipe: &Recipe,
        options: &DecodeOptions,
    ) -> Result<Self, String> {
        let recording = open().map_err(|e| e.to_string())?;
        let window = recipe::window(&recording);
        let b = (recipe.bind(registry, &recording, window, &[], HostOptions::default()))
            .map_err(|e| e.to_string())?;
        let channels = (recording.channels().iter())
            .filter(|c| matches!(c.kind, ChannelKind::Digital))
            .map(|c| c.name.clone())
            .collect();
        let mut r = Self {
            host: b.host,
            lanes: Vec::with_capacity(b.analyses.len()),
            chains: vec![],
            recording,
            window,
            channels,
            budget: options.checkpoint_budget,
        };
        for a in b.analyses {
            r.lane(a)?;
        }
        Ok(r)
    }

    /// Draws analysis `a`, as the recipe bound it, in a lane of its own after the others: its
    /// forward pass keeps no items and tallies them, and a wire analysis keeps checkpoints.
    fn lane(&mut self, a: recipe::Bound) -> Result<(), String> {
        let reads = match self.host.bound(a.id) {
            Some(up) => Some(
                (self.lanes.iter().position(|l| l.id == up.analysis))
                    .ok_or("an analysis reads one the view does not draw")?,
            ),
            None => None,
        };
        let mut root = self.lanes.len();
        if let Some(up) = reads {
            root = self.lanes[up].root;
        }
        self.host.set_keep(a.id, vec![]);
        self.host.tally(a.id);
        if reads.is_none() {
            let keeping = Keeping::new(Tick::MAX, self.budget).with_events(100_000);
            (self.host.keep_checkpoints(a.id, keeping)).map_err(|e| e.to_string())?;
        }
        self.lanes.push(Lane {
            id: a.id,
            name: a.label,
            map: a.map,
            schema: self.host.definition(a.id).schema.clone(),
            labels: Labels::default(),
            tally: Density::default(),
            run: self.host.run_id(a.id),
            estimate: None,
            timeline: a.timeline,
            reads,
            root,
        });
        self.chains.push(Chains::default());
        Ok(())
    }

    /// Binds the analysis in lane `k` with `settings`: the host decodes its chain again.
    pub(crate) fn rebind(
        &mut self,
        k: usize,
        settings: &[(String, Setting)],
    ) -> Result<(), String> {
        let given: Vec<(&str, Setting)> = (settings.iter())
            .map(|(n, v)| (n.as_str(), v.clone()))
            .collect();
        let root = self.lanes[k].root;
        let whole = self.host.finished(self.lanes[root].id).is_some();
        (self.host.rebind(self.lanes[k].id, &given)).map_err(|e| e.to_string())?;
        self.restarted(root, whole);
        Ok(())
    }

    /// Adds `planned`, as the recipe would bind it, in a lane after the others. Stacked on a
    /// chain that has run, the host decodes the chain again.
    fn add(&mut self, registry: &Registry, planned: &Planned) -> Result<(), String> {
        let bound: Vec<recipe::Bound> = (self.lanes.iter())
            .map(|l| recipe::Bound {
                id: l.id,
                label: l.name.clone(),
                timeline: l.timeline,
                map: l.map,
            })
            .collect();
        let runs: Vec<RunId> = self.lanes.iter().map(|l| self.host.run_id(l.id)).collect();
        let whole: Vec<bool> = (self.lanes.iter())
            .map(|l| self.host.finished(l.id).is_some())
            .collect();
        let a = (planned.add(
            &mut self.host,
            registry,
            &self.recording,
            &bound,
            self.window,
            &[],
        ))
        .map_err(|e| e.to_string())?;
        self.lane(a)?;
        let k = self.lanes.len() - 1;
        let root = self.lanes[k].root;
        if let Some(up) = self.lanes[k].reads
            && self.host.run_id(self.lanes[up].id) != runs[up]
        {
            self.restarted(root, whole[root]);
        }
        Ok(())
    }

    /// After the host decoded wire analysis lane `root`'s chain again: its lanes count afresh, and
    /// its window chains, which the host retired, are gone. Where the pass before had finished,
    /// `whole`, what it counted is the estimate; a pass cut short counts nothing past where it
    /// reached, so an estimate from an earlier finished pass stays.
    fn restarted(&mut self, root: usize, whole: bool) {
        for l in self.lanes.iter_mut().filter(|l| l.root == root) {
            let counted = std::mem::take(&mut l.tally);
            if whole {
                l.estimate = Some(counted);
            }
            l.run = self.host.run_id(l.id);
        }
        self.chains[root] = Chains::default();
    }

    /// Takes in the window the view shows: each wire analysis whose items there are few enough
    /// derives it and a width either side in a chain, unless its chains cover it already. Ahead
    /// of the forward pass, where the tally has not counted them, only a short one, unless a pass
    /// before a setting changed counted them.
    pub(crate) fn ask(&mut self, asked: &Asked) -> Result<(), String> {
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
            let lanes = || self.lanes.iter().filter(|l| l.root == root);
            let map = self.lanes[root].map;
            let span = Span {
                start: map.first_at(wide.start),
                end: map.first_at(wide.end),
            };
            let counted = self.host.finished(self.lanes[root].id).is_some()
                || self.host.consumed(self.lanes[root].id) >= span.end;
            let estimated = self.lanes[root].estimate.is_some();
            let skip = if counted || estimated {
                let items: u64 = lanes()
                    .map(|l| l.counting(counted).count(wide.start, wide.end, 0.0).0)
                    .sum();
                let crowded = self.lanes[root].counting(counted).crowded(window, width);
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
        for k in 0..self.lanes.len() {
            if self
                .host
                .finished(self.lanes[self.lanes[k].root].id)
                .is_some()
            {
                self.lanes[k].estimate = None;
            }
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
    /// The window a panel control asked for, to bring its item into view: set on the graph's view
    /// state before the graph draws again.
    jump: Option<Range<i64>>,
}

impl DecodeState {
    /// Whether a panel control's window waits to be set.
    #[cfg(test)]
    pub(crate) fn jumping(&self) -> bool {
        self.jump.is_some()
    }
}

/// The window that brings an item spanning `item` (axis ticks) into view from `window`: none
/// where the window shows it whole. Else the window keeps its span and centres the item, and an
/// item longer than the window fills it with a quarter of its length either side.
pub(crate) fn bring(window: &Range<i64>, item: Range<i64>) -> Option<Range<i64>> {
    if window.start <= item.start && item.end <= window.end {
        return None;
    }
    let span = window.end.saturating_sub(window.start);
    let length = item.end.saturating_sub(item.start);
    if length <= span {
        let start = (item.start.saturating_add(length / 2)).saturating_sub(span / 2);
        return Some(start..start.saturating_add(span));
    }
    let pad = length / 4;
    Some(item.start.saturating_sub(pad)..item.end.saturating_add(pad))
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
///
/// An item chosen in the panel that the window does not show whole comes into view in the same
/// frame, drawn in a second pass: the window keeps its span with the item in its middle, or fits
/// an item longer than it with a quarter of the item's length either side. It jumps there,
/// without easing. A click on a track selects what the plot already shows, and moves nothing.
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
            mut graph,
            decode,
            state,
            place,
            painted,
        } = self;
        if let Some(window) = state.jump.take() {
            graph.state_mut().window = Window::At(window);
        }
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
pub(crate) struct Source<'a> {
    pub(crate) items: &'a BTreeMap<Id, Item>,
    store: &'a Store,
    density: &'a Density,
    settled: Tick,
    lost: &'a [Span],
    provisional: bool,
}

impl Running {
    pub(crate) fn source(&self, k: usize, window: &Range<i64>) -> Source<'_> {
        let lane = &self.lanes[k];
        let chain = (self.chains[lane.root].drawn.as_ref())
            .filter(|c| c.axis.start < window.end && window.start < c.axis.end)
            .and_then(|c| {
                Some((
                    c,
                    self.host.window_run(c.id, lane.id)?,
                    c.densities.get(k)?.as_ref()?,
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

/// The side panel over what the tracks drew, `width` wide, beside the plot or `under` it; then
/// what a control in it asked for.
fn panel(
    ui: &mut Ui,
    decode: &Decode,
    state: &mut DecodeState,
    palette: &Palette,
    timebase: Ratio,
    width: f32,
    under: Option<f32>,
) {
    let mut g = decode.lock();
    let Inner {
        phase,
        recipe,
        refused,
        ..
    } = &mut *g;
    let Phase::Running(r) = phase else {
        return;
    };
    let Some(window) = state.shown.as_ref().map(|s| s.window.clone()) else {
        return;
    };
    // With nothing decoded, the only tab with anything to show.
    if r.lanes.is_empty() {
        state.panel.tab = Tab::Decoders;
    }
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
    let length = r.recording.length().max(1);
    let configured = (r.lanes.iter())
        .map(|l| {
            let id = r.host.name(l.id);
            let inputs =
                (recipe.analyses.iter().find(|a| a.id == id)).map_or(&[][..], |a| &a.inputs);
            let done = r.host.finished(l.id).is_none().then(|| {
                let at = l.map.axis_saturating(r.host.settled(l.id)).clamp(0, length);
                at as f64 / length as f64
            });
            Configured {
                id,
                inputs,
                label: &l.name,
                definition: r.host.definition(l.id),
                bound: r.host.settings(l.id),
                reads: l.reads,
                done,
                items: l.tally.items(),
            }
        })
        .collect();
    let mut change = None;
    let stack = Stack {
        analyses: configured,
        registry: &decode.registry,
        channels: &r.channels,
        refused: refused.as_deref(),
        change: &mut change,
    };
    let map = r.lanes.first().map_or(Mapping::IDENTITY, |l| l.map);
    let p = Panel::new(palette, &mut state.panel, &analyses, width, timebase, map).stack(stack);
    let response = ui.add(match under {
        Some(h) => p.under(h),
        None => p,
    });
    // An item a panel control chose, where the window does not show it whole: the window that
    // brings it into view, set before the graph draws again. The panel draws after the graph, so
    // this frame is drawn once more, which sends the new window's queries a frame sooner; a host
    // that allows egui one pass gets it next frame.
    let chosen = Panel::went(&response).and_then(|(k, id)| {
        let item = analyses.get(k)?.store.items().get(&id)?;
        let map = r.lanes[k].map;
        let end = item.end.unwrap_or(item.start);
        Some(map.axis_saturating(item.start)..map.axis_saturating(end))
    });
    if let Some(to) = chosen.and_then(|item| bring(&window, item)) {
        state.jump = Some(to);
        ui.ctx().request_discard("a panel control moved the window");
        ui.ctx().request_repaint();
    }
    let Some(change) = change else {
        return;
    };
    match change {
        Change::Settings(k, settings) => match r.rebind(k, &settings) {
            Ok(()) => {
                *refused = None;
                let id = r.host.name(r.lanes[k].id);
                let def = r.host.definition(r.lanes[k].id);
                if let Some(a) = recipe.analyses.iter_mut().find(|a| a.id == id) {
                    a.settings = not_default(def, settings);
                }
            }
            Err(why) => *refused = Some(why),
        },
        Change::Add(added) => {
            let added = added.fresh(|id| recipe.analyses.iter().any(|a| a.id == id));
            *refused = None;
            for a in added.analyses {
                match r.add(&decode.registry, &a) {
                    Ok(()) => recipe.analyses.push(a),
                    Err(why) => {
                        *refused = Some(why);
                        break;
                    }
                }
            }
        }
        Change::Remove(k) => {
            let without = recipe.without(r.host.name(r.lanes[k].id));
            drop(g);
            state.panel.forget();
            decode.rebuild(without);
            ui.ctx().request_repaint();
            return;
        }
    }
    drop(g);
    decode.wake();
    ui.ctx().request_repaint();
}

/// Of `settings`, those not at their default, as a recipe keeps them.
fn not_default(
    def: &escapement_tickwave_decode::Definition,
    settings: Vec<(String, Setting)>,
) -> Vec<(String, Setting)> {
    (settings.into_iter())
        .filter(|(n, v)| {
            let Some(d) = def.settings.iter().find(|d| d.name == n.as_str()) else {
                return true;
            };
            let default = match &d.ty {
                SettingType::Flag { default } => Setting::Flag(*default),
                SettingType::Int { default, .. } => Setting::Int(*default),
                SettingType::Choice { names, default } => match names.get(*default) {
                    Some(n) => Setting::Name(n.clone()),
                    None => return true,
                },
                SettingType::List { default, .. } => Setting::List(default.clone()),
                _ => return true,
            };
            *v != default
        })
        .collect()
}
