//! The host: analyses added in order, each with one current run; reads performed outside any
//! decoder's turn; output admitted whole and fanned out to the analyses stacked on it; frontiers,
//! lost input, replacement, checkpoints and cooperative cancellation.

use crate::{
    output::{Id, KindId, Op, Out, Ref, SeqId, Span, Status, Tick},
    schema::{CONTRACT, Contract, Definition, Mismatch, Name, Resolved, SchemaError},
    store::{Field, Refused, Store},
};
use std::{
    collections::{BTreeSet, HashMap, HashSet, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering::Relaxed},
    },
};

/// An analysis of a host, as `Host::add` numbers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AnalysisId(pub u16);

impl AnalysisId {
    fn index(self) -> usize {
        usize::from(self.0)
    }
}

/// One execution of an analysis. Replacing the run makes a new generation; nothing from an older
/// generation is admitted afterwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RunId {
    pub analysis: AnalysisId,
    pub generation: u64,
}

/// A request within a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestId {
    pub run: RunId,
    pub seq: u64,
}

/// A wire observation. A window holds its ports' observations in exact order on the analysis's
/// timeline; observations of different ports at one tick are adjacent, in no order a decoder may
/// rely on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Observed {
    /// Port `port`'s level just before the window's first tick, before anything at that tick;
    /// None where it is not known. Each port's comes first in the window, whether or not the
    /// source read from before it.
    Start { port: u8, level: Option<bool> },
    /// Port `port` changed to `level` at `at`.
    Edge { at: Tick, port: u8, level: bool },
    /// Port `port`'s level is known from `at`: its channel starts, or a gap ends there.
    Known { at: Tick, port: u8, level: bool },
    /// Port `port`'s level is not known from `at`: a gap starts, or its channel ends there.
    Unknown { at: Tick, port: u8 },
}

impl Observed {
    /// Where it lies; None for a window's start levels.
    pub fn at(&self) -> Option<Tick> {
        match *self {
            Observed::Start { .. } => None,
            Observed::Edge { at, .. }
            | Observed::Known { at, .. }
            | Observed::Unknown { at, .. } => Some(at),
        }
    }
}

/// The observations in `from..upto`. `end` is set where the recording ends inside the window.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Window {
    pub from: Tick,
    pub upto: Tick,
    pub events: Vec<Observed>,
    pub end: Option<Tick>,
}

impl Window {
    /// An empty window over `from..upto`.
    pub fn new(from: Tick, upto: Tick) -> Self {
        Self {
            from,
            upto,
            events: vec![],
            end: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Request {
    Window { from: Tick, upto: Tick },
}

#[non_exhaustive]
pub struct Reply {
    pub id: RequestId,
    pub window: Window,
}

impl Reply {
    pub fn new(id: RequestId, window: Window) -> Self {
        Self { id, window }
    }
}

/// Where a wire analysis's input comes from. The host calls it outside the decoder's turn.
pub trait Source: Send {
    /// The observations in `from..upto`, or up to where a live recording is settled. None where
    /// `cancel` stopped the read: the source is left as it was.
    fn window(
        &mut self,
        from: Tick,
        upto: Tick,
        cancel: &AtomicBool,
    ) -> Result<Option<Window>, SourceError>;
}

/// Why a source could not read a window: the recording's error, kept whole.
#[derive(Clone, Debug, thiserror::Error)]
#[error("{0}")]
pub struct SourceError(Arc<dyn std::error::Error + Send + Sync>);

impl SourceError {
    pub fn new(e: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self(Arc::new(e))
    }

    /// The error the source met.
    pub fn inner(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
        &*self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Mode {
    /// Every operation on a subscribed item, as its batch commits.
    Pieces,
    /// The item's fields, links and sequence lengths, once it closes. No elements.
    Completed,
}

/// A stacked analysis's input: kinds of an upstream analysis, by name, delivered in one mode. A
/// pieces subscription names the sequences it reads; others are not delivered.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Subscription {
    pub upstream: AnalysisId,
    pub kinds: Vec<Name>,
    pub mode: Mode,
    pub sequences: Vec<Name>,
}

impl Subscription {
    /// No kinds and no sequences yet.
    pub fn new(upstream: AnalysisId, mode: Mode) -> Self {
        Self {
            upstream,
            kinds: vec![],
            mode,
            sequences: vec![],
        }
    }

    pub fn with_kinds<N: Into<Name>>(mut self, kinds: impl IntoIterator<Item = N>) -> Self {
        self.kinds = kinds.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_sequences<N: Into<Name>>(mut self, sequences: impl IntoIterator<Item = N>) -> Self {
        self.sequences = sequences.into_iter().map(Into::into).collect();
        self
    }
}

/// A subscription's names resolved against its upstream's schema.
#[derive(Clone, Debug)]
struct Resolution {
    kinds: Vec<KindId>,
    mode: Mode,
    sequences: Vec<(KindId, SeqId)>,
}

/// An item as a completed subscription receives it.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Completed {
    pub id: Id,
    pub parent: Option<Id>,
    pub start: Tick,
    pub end: Tick,
    pub status: Status,
    pub fields: Vec<Field>,
    pub links: Vec<(crate::LinkId, Ref)>,
    pub lengths: Vec<(SeqId, u32)>,
}

impl Completed {
    pub fn field(&self, f: crate::FieldId) -> Option<&Field> {
        self.fields.iter().find(|x| x.field == f)
    }

    pub fn length(&self, s: SeqId) -> u32 {
        self.lengths
            .iter()
            .find(|(n, _)| *n == s)
            .map_or(0, |l| l.1)
    }
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Delivery {
    Piece {
        from: RunId,
        batch: u64,
        op: Op,
    },
    Item {
        from: RunId,
        batch: u64,
        item: Completed,
    },
    /// The upstream run's frontiers after a batch: no new item starts before `start`, and nothing
    /// before `settled` changes.
    Frontier {
        from: RunId,
        start: Tick,
        settled: Tick,
    },
    /// The upstream run's input was lost over `span`, at this place among its output.
    Lost {
        from: RunId,
        batch: u64,
        span: Span,
    },
    /// The upstream run has finished: everything it emitted has been delivered.
    End {
        from: RunId,
        end: Tick,
    },
}

impl Delivery {
    pub fn from(&self) -> RunId {
        match self {
            Delivery::Piece { from, .. }
            | Delivery::Item { from, .. }
            | Delivery::Frontier { from, .. }
            | Delivery::Lost { from, .. }
            | Delivery::End { from, .. } => *from,
        }
    }

    fn source_mut(&mut self) -> &mut RunId {
        match self {
            Delivery::Piece { from, .. }
            | Delivery::Item { from, .. }
            | Delivery::Frontier { from, .. }
            | Delivery::Lost { from, .. }
            | Delivery::End { from, .. } => from,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Pause {
    /// Waiting for a reply or a delivery.
    Input,
    /// More to do with input already held; call again.
    Work,
    Finished,
}

/// One turn's view of the host: the reply that is ready, deliveries, and the batch being written.
pub struct Exchange<'a> {
    pub run: RunId,
    pub out: Out,
    reply: Option<Window>,
    taken: Option<Tick>,
    inbox: &'a mut VecDeque<Delivery>,
    budget: usize,
    request: Option<Request>,
    twice: bool,
    /// Whether the decoder asked for a delivery after the budget ran out.
    exhausted: bool,
    end: Option<Tick>,
    frontier: Option<Tick>,
    upstream: Option<Tick>,
    lost: Vec<(usize, Span)>,
    received: Vec<Span>,
    interrupt: &'a AtomicBool,
}

impl Exchange<'_> {
    /// Whether the host asks the turn to stop. The decoder returns at its next safe point with
    /// what it has emitted, after at least one unit of progress, and carries on next turn.
    pub fn interrupted(&self) -> bool {
        self.interrupt.load(Relaxed)
    }

    /// The reply to the decoder's last request, once. Taking it moves the run's input frontier to
    /// the window's end.
    pub fn reply(&mut self) -> Option<Window> {
        let w = self.reply.take()?;
        self.taken = Some(w.upto);
        Some(w)
    }

    /// Asks for input. A run has at most one request outstanding, and a turn makes at most one:
    /// the host refuses the turn that makes a second.
    pub fn request(&mut self, r: Request) {
        if self.request.is_some() {
            self.twice = true;
        }
        self.request = Some(r);
    }

    /// The next delivery, while this turn's budget lasts.
    pub fn receive(&mut self) -> Option<Delivery> {
        if self.budget == 0 {
            self.exhausted = !self.inbox.is_empty();
            return None;
        }
        let d = self.inbox.pop_front()?;
        self.budget -= 1;
        match d {
            Delivery::Frontier { start, .. } => self.upstream = Some(start),
            Delivery::Lost { span, .. } => self.received.push(span),
            _ => {}
        }
        Some(d)
    }

    /// The decoder's input was lost from `from` to `to`, after the ops emitted so far this turn.
    /// Subscribers receive it at that place. A loss received this turn must be declared again
    /// this turn, over the same stretch.
    pub fn lost(&mut self, from: Tick, to: Tick) {
        self.lost.push((
            self.out.ops.len(),
            Span {
                start: from,
                end: to,
            },
        ));
    }

    /// The run's start frontier: no item, field span or element this run emits from now on starts
    /// before `t`. It may not pass the input the decoder has taken: its window's end, or the
    /// upstream start frontier it has received.
    pub fn frontier(&mut self, t: Tick) {
        self.frontier = Some(t);
    }

    /// Whether deliveries remain after the budget ran out.
    pub fn more(&self) -> bool {
        !self.inbox.is_empty()
    }

    /// The decoder's input has ended at `end` and it has emitted everything.
    pub fn finish(&mut self, end: Tick) {
        self.end = Some(end);
    }
}

pub trait Decoder: Send {
    fn turn(&mut self, ex: &mut Exchange<'_>) -> Pause;

    /// A copy of the decoder's state for a checkpoint, between turns. Optional: a decoder that
    /// cannot be copied is replayed instead.
    fn save(&self) -> Option<Box<dyn Decoder>> {
        None
    }
}

/// Stands in for a decoder whose turn is under way. The host never runs it: `begin_turn` refuses
/// a second turn while one is out.
struct Taken;

impl Decoder for Taken {
    fn turn(&mut self, _: &mut Exchange<'_>) -> Pause {
        Pause::Input
    }
}

/// A decoder's turn taken out of the host, so it can run apart from it, as on a worker: the
/// decoder, its ready reply and queued deliveries, and once run, what it produced. `Host::end_turn`
/// admits it, or refuses it whole if the run was replaced meanwhile.
pub struct Turn {
    a: usize,
    run: RunId,
    decoder: Box<dyn Decoder>,
    reply: Option<Window>,
    inbox: VecDeque<Delivery>,
    budget: usize,
    interrupt: Arc<AtomicBool>,
    done: Option<Done>,
}

struct Done {
    pause: Pause,
    /// Deliveries remain that the budget kept from the decoder.
    more: bool,
    out: Out,
    taken: Option<Tick>,
    request: Option<Request>,
    twice: bool,
    end: Option<Tick>,
    frontier: Option<Tick>,
    upstream: Option<Tick>,
    lost: Vec<(usize, Span)>,
    received: Vec<Span>,
}

impl Turn {
    pub fn run(&mut self) {
        let mut ex = Exchange {
            run: self.run,
            out: Out::default(),
            reply: self.reply.take(),
            taken: None,
            inbox: &mut self.inbox,
            budget: self.budget,
            request: None,
            twice: false,
            exhausted: false,
            end: None,
            frontier: None,
            upstream: None,
            lost: vec![],
            received: vec![],
            interrupt: &self.interrupt,
        };
        let pause = self.decoder.turn(&mut ex);
        let more = ex.exhausted && ex.more();
        let Exchange {
            out,
            taken,
            request,
            twice,
            end,
            frontier,
            upstream,
            lost,
            received,
            ..
        } = ex;
        self.done = Some(Done {
            pause,
            more,
            out,
            taken,
            request,
            twice,
            end,
            frontier,
            upstream,
            lost,
            received,
        });
    }

    /// How many ops the turn emitted.
    pub fn emitted(&self) -> usize {
        self.done.as_ref().map_or(0, |d| d.out.ops.len())
    }
}

/// Makes a decoder for a run. `local` is where a run starts without earlier context; None starts
/// it at the recording's beginning.
pub type Factory = Box<dyn Fn(Option<Tick>) -> Box<dyn Decoder> + Send + Sync>;

/// Output a run submits after a turn.
#[non_exhaustive]
pub struct Batch {
    pub run: RunId,
    pub seq: u64,
    pub ops: Vec<Op>,
    pub frontier: Option<Tick>,
    /// Lost input, each after the ops before its index.
    pub lost: Vec<(usize, Span)>,
}

impl Batch {
    /// Ops with no frontier and no lost input.
    pub fn new(run: RunId, seq: u64, ops: Vec<Op>) -> Self {
        Self {
            run,
            seq,
            ops,
            frontier: None,
            lost: vec![],
        }
    }
}

/// Why the host did not admit something.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Refusal {
    /// A batch or turn from a run that is no longer current.
    #[error("run {got:?} is no longer current: {current:?} is")]
    StaleRun { got: RunId, current: RunId },
    #[error("batch {got}, where {expected} was next")]
    BatchOrder { expected: u64, got: u64 },
    #[error("op {op}: {why}")]
    Schema { op: usize, why: Mismatch },
    #[error("op {op}: {why}")]
    Lifecycle { op: usize, why: Refused },
    /// A reply that does not answer the current run's outstanding request.
    #[error("reply {got:?} answers no outstanding request")]
    StaleReply {
        got: RequestId,
        outstanding: Option<RequestId>,
    },
    /// A delivery from an upstream run the downstream run is not bound to.
    #[error("delivery from {from:?}, not the bound {bound:?}")]
    StaleDelivery { from: RunId, bound: Option<RunId> },
    /// A run finished with items still open.
    #[error("run {run:?} finished with {open} items open")]
    LeftOpen { run: RunId, open: usize },
    /// A request while one is outstanding.
    #[error("a request while {outstanding:?} is outstanding")]
    RequestOutstanding { outstanding: RequestId },
    /// A turn that made two requests.
    #[error("run {0:?} made two requests in one turn")]
    RequestTwice(RunId),
    /// A turn begun while the run's last is still out.
    #[error("run {0:?} has a turn under way")]
    TurnUnderWay(RunId),
    /// A start frontier moving back.
    #[error("the start frontier moves back from {was} to {got}")]
    FrontierBack { was: Tick, got: Tick },
    /// A start frontier past the input the run has taken.
    #[error("the start frontier {got} is past the input taken, {input}")]
    FrontierAhead { got: Tick, input: Tick },
    /// An item, field span or element starting before the run's start frontier.
    #[error("op {op} starts at {at}, before the start frontier {frontier}")]
    LateStart { op: usize, at: Tick, frontier: Tick },
    /// An item whose id's anchor is before its start: retiring closed ids by the start frontier
    /// relies on every anchor being at or after its item's start.
    #[error("op {op}: an anchor before its item's start")]
    AnchorBeforeStart { op: usize },
    /// More closed ids held than the host allows: a run whose start frontier does not move on.
    #[error("{held} closed ids held, past the host's {at_most}")]
    TooManyRecent { held: usize, at_most: usize },
    /// Lost input that is empty, out of the batch's op order, or not after the run's last.
    #[error("lost input {got:?}, out of order after {was}")]
    LossOrder { was: Tick, got: Span },
    /// Lost input past what the run has taken: its window's end, or the loss it was delivered.
    #[error("lost input to {got}, past the input taken, {input}")]
    LossAhead { got: Tick, input: Tick },
    /// A turn that received a loss and did not declare it again.
    #[error("run {run:?} received lost input {span:?} and did not declare it")]
    LossNotDeclared { run: RunId, span: Span },
    /// A source's read failed; the request stays outstanding.
    #[error("run {run:?}'s read failed: {why}")]
    Read { run: RunId, why: String },
}

/// Why a checkpoint cannot be taken or restored.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CheckpointError {
    /// A decoder in the cut cannot copy its state.
    #[error("{0} cannot copy its state")]
    Unsupported(Name),
    /// A cut must start at an analysis that reads the wire, so that nothing outside it feeds it.
    #[error("{0} does not read the wire")]
    NotARoot(Name),
    /// The checkpoint was taken of other analyses than this host holds.
    #[error("the checkpoint names an analysis this host does not hold")]
    Foreign,
}

/// One run's state in a checkpoint.
struct Snapshot {
    analysis: usize,
    taken: RunId,
    decoder: Box<dyn Decoder>,
    store: Store,
    inbox: VecDeque<Delivery>,
    assembling: HashMap<Id, Completed>,
    recent: BTreeSet<(Tick, Id)>,
    local: Option<Tick>,
    frontier: Tick,
    settled: Tick,
    input: Tick,
    consumed: Tick,
    finished: Option<Tick>,
    lost: Vec<Span>,
    lost_end: Tick,
    lost_total: u64,
    lost_input: Tick,
}

/// A checkpoint of a wire analysis and every analysis stacked on it, taken between turns, so that
/// each stacked state matches exactly the upstream output delivered to it.
#[non_exhaustive]
pub struct Checkpoint {
    pub runs: Vec<RunId>,
    snaps: Vec<Snapshot>,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BindError {
    #[error("decoder {decoder} targets contract {wants}; this host runs {runs}")]
    Contract {
        decoder: Name,
        wants: Contract,
        runs: Contract,
    },
    #[error("{0}")]
    Schema(SchemaError),
    #[error("an analysis named {0} is already added")]
    DuplicateAnalysis(Name),
    #[error("more analyses than a run id numbers")]
    TooManyAnalyses,
    #[error("more wire ports than a window numbers")]
    TooManyPorts,
    #[error("no analysis {0:?} to stack on")]
    NoAnalysis(AnalysisId),
    #[error("{upstream} has no kind {kind}")]
    UnknownKind { upstream: Name, kind: Name },
    #[error("no subscribed kind has a sequence {0}")]
    UnknownSequence(Name),
    /// A completed subscription cannot carry sequences: they have no bound.
    #[error("a completed subscription cannot carry sequence {0}")]
    SequenceInCompleted(Name),
}

/// One line of the host's record, for tests and the trace.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum LogEntry {
    Admitted {
        order: u64,
        run: RunId,
        batch: u64,
        ops: Vec<(char, Id)>,
    },
    Delivered {
        order: u64,
        to: RunId,
        from: RunId,
        what: (char, Option<Id>),
    },
    Request {
        order: u64,
        id: RequestId,
        request: Request,
    },
    Replaced {
        order: u64,
        old: RunId,
        new: RunId,
    },
    Refused {
        order: u64,
        why: Refusal,
    },
}

#[non_exhaustive]
pub enum InputSpec {
    Wire {
        source: Box<dyn Source>,
        local: Option<Tick>,
    },
    Upstream(Subscription),
}

enum Input {
    Wire(Box<dyn Source>),
    Upstream { index: usize, sub: Resolution },
}

/// What a host allows and keeps. Each bound refuses rather than grows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct HostOptions {
    /// Deliveries one turn may take.
    pub turn_deliveries: usize,
    /// The most a stacked analysis's inbox holds before its upstream waits.
    pub inbox_at_most: usize,
    /// The host's record, off where None, else the most entries it keeps, the oldest dropped.
    pub log: Option<usize>,
    /// The most closed ids a run holds to refuse a repeated open; past it, its batch is refused.
    pub recent_at_most: usize,
    /// The largest open-item bound a schema may declare.
    pub open_at_most: usize,
}

impl Default for HostOptions {
    fn default() -> Self {
        Self {
            turn_deliveries: 512,
            inbox_at_most: 4096,
            log: None,
            recent_at_most: 1 << 16,
            open_at_most: 1 << 16,
        }
    }
}

struct Run {
    id: RunId,
    local: Option<Tick>,
    decoder: Box<dyn Decoder>,
    /// Whether the decoder is out for a turn.
    out: bool,
    store: Store,
    next_batch: u64,
    next_request: u64,
    outstanding: Option<(RequestId, Request)>,
    ready: Option<Window>,
    inbox: VecDeque<Delivery>,
    bound: Option<RunId>,
    /// Items being assembled for completed subscribers: fields, links and sequence lengths.
    assembling: HashMap<Id, Completed>,
    started: bool,
    /// The decoder has more to do with input it holds or was kept from by its budget.
    more: bool,
    /// Input arrived since the decoder's last turn: a reply or a delivery.
    fresh: bool,
    finished: Option<Tick>,
    consumed: Tick,
    /// The start frontier this run declared last, and the settlement frontier derived from it.
    frontier: Tick,
    settled: Tick,
    /// For a stacked run, the upstream start frontier its decoder has received.
    input: Tick,
    /// Closed ids whose anchors are not yet behind the start frontier: a repeated open of one is
    /// refused here, and every older one would start too late to repeat.
    recent: BTreeSet<(Tick, Id)>,
    peak_recent: usize,
    /// The input lost, as declared, adjacent stretches joined: those not yet settled or meeting a
    /// kept span. `lost_end` is where the last ends, and `lost_total` counts every one.
    lost: Vec<Span>,
    lost_end: Tick,
    lost_total: u64,
    /// For a stacked run, the end of the last loss delivered.
    lost_input: Tick,
    /// Raised to stop the run's read or turn in progress; the host lowers it once acted on.
    interrupt: Arc<AtomicBool>,
}

struct Slot {
    name: Name,
    def: Arc<Definition>,
    resolved: Resolved,
    input: Input,
    factory: Factory,
    keep: Vec<Span>,
    downstream: Vec<usize>,
    run: Run,
}

pub struct Host {
    options: HostOptions,
    analyses: Vec<Slot>,
    io: VecDeque<(usize, RequestId, Request)>,
    log: VecDeque<LogEntry>,
    order: u64,
    pub admitted: u64,
    pub delivered: u64,
    pub stale: u64,
    pub peak_inbox: usize,
    pub cancelled_reads: u64,
    pub log_dropped: u64,
}

impl Default for Host {
    fn default() -> Self {
        Self::new(HostOptions::default())
    }
}

impl Host {
    pub fn new(options: HostOptions) -> Self {
        Self {
            options,
            analyses: vec![],
            io: VecDeque::new(),
            log: VecDeque::new(),
            order: 0,
            admitted: 0,
            delivered: 0,
            stale: 0,
            peak_inbox: 0,
            cancelled_reads: 0,
            log_dropped: 0,
        }
    }

    pub fn options(&self) -> HostOptions {
        self.options
    }

    fn note(&mut self, entry: impl FnOnce(u64) -> LogEntry) {
        self.order += 1;
        let Some(most) = self.options.log else {
            return;
        };
        if most == 0 {
            self.log_dropped += 1;
            return;
        }
        if self.log.len() >= most {
            self.log.pop_front();
            self.log_dropped += 1;
        }
        self.log.push_back(entry(self.order));
    }

    fn refuse(&mut self, why: Refusal) -> Refusal {
        if matches!(
            why,
            Refusal::StaleRun { .. } | Refusal::StaleReply { .. } | Refusal::StaleDelivery { .. }
        ) {
            self.stale += 1;
        }
        let w = why.clone();
        self.note(|order| LogEntry::Refused { order, why: w });
        why
    }

    /// Adds analysis `name` after those it reads from, and creates its first run, which starts on
    /// the next step. `keep` is where the run's store retains whole items. A decoder that targets
    /// a contract this host does not run is refused here.
    pub fn add(
        &mut self,
        name: Name,
        def: Arc<Definition>,
        input: InputSpec,
        factory: Factory,
        keep: Vec<Span>,
    ) -> Result<AnalysisId, BindError> {
        if !CONTRACT.runs(def.contract) {
            return Err(BindError::Contract {
                decoder: def.name.clone(),
                wants: def.contract,
                runs: CONTRACT,
            });
        }
        if def.ports.len() > usize::from(u8::MAX) + 1 {
            return Err(BindError::TooManyPorts);
        }
        if self.analyses.iter().any(|a| a.name == name) {
            return Err(BindError::DuplicateAnalysis(name));
        }
        let index = self.analyses.len();
        let analysis = u16::try_from(index).map_err(|_| BindError::TooManyAnalyses)?;
        let (input, local, bound, resolved) = match input {
            InputSpec::Wire { source, local } => {
                let r = def
                    .schema
                    .resolve(None, self.options.open_at_most)
                    .map_err(BindError::Schema)?;
                (Input::Wire(source), local, None, r)
            }
            InputSpec::Upstream(sub) => {
                let up = sub.upstream.index();
                let ua = (self.analyses.get(up)).ok_or(BindError::NoAnalysis(sub.upstream))?;
                let us = &ua.def.schema;
                let r = def
                    .schema
                    .resolve(Some(us), self.options.open_at_most)
                    .map_err(BindError::Schema)?;
                let mut kinds = vec![];
                for k in &sub.kinds {
                    kinds.push(us.kind_id(k).ok_or_else(|| BindError::UnknownKind {
                        upstream: ua.name.clone(),
                        kind: k.clone(),
                    })?);
                }
                let mut sequences = vec![];
                for s in &sub.sequences {
                    if sub.mode == Mode::Completed {
                        return Err(BindError::SequenceInCompleted(s.clone()));
                    }
                    let before = sequences.len();
                    for &k in &kinds {
                        let decl = us.kind(k).map(|d| &d.sequences);
                        if let Some(q) = decl.and_then(|q| q.iter().position(|q| q.name == *s)) {
                            sequences.push((k, SeqId(q as u16)));
                        }
                    }
                    if sequences.len() == before {
                        return Err(BindError::UnknownSequence(s.clone()));
                    }
                }
                let sub = Resolution {
                    kinds,
                    mode: sub.mode,
                    sequences,
                };
                let (local, bound) = (ua.run.local, Some(ua.run.id));
                self.analyses[up].downstream.push(index);
                (Input::Upstream { index: up, sub }, local, bound, r)
            }
        };
        let id = RunId {
            analysis: AnalysisId(analysis),
            generation: 1,
        };
        let run = Run::new(
            id,
            local,
            factory(local),
            Store::with_keep(keep.clone()),
            bound,
        );
        self.analyses.push(Slot {
            name,
            def,
            resolved,
            input,
            factory,
            keep,
            downstream: vec![],
            run,
        });
        Ok(AnalysisId(analysis))
    }

    pub fn index(&self, name: &str) -> Option<AnalysisId> {
        let a = self.analyses.iter().position(|a| a.name == name)?;
        Some(AnalysisId(u16::try_from(a).ok()?))
    }

    pub fn name(&self, a: AnalysisId) -> &str {
        let a = a.index();
        &self.analyses[a].name
    }

    pub fn definition(&self, a: AnalysisId) -> &Definition {
        let a = a.index();
        &self.analyses[a].def
    }

    /// An item's semantic id as text, as a citation names it: the analysis, the kind and the
    /// anchor, and the discriminator after a dot where there is one.
    pub fn id_text(&self, a: AnalysisId, id: &Id) -> String {
        let a = a.index();
        let an = &self.analyses[a];
        let kind = an.def.schema.kind(id.kind).map_or("?", |k| &k.name);
        match id.disc {
            0 => format!("{}/{kind}/{}", an.name, id.anchor),
            d => format!("{}/{kind}/{}.{d}", an.name, id.anchor),
        }
    }

    pub fn run_id(&self, a: AnalysisId) -> RunId {
        let a = a.index();
        self.analyses[a].run.id
    }

    pub fn bound(&self, a: AnalysisId) -> Option<RunId> {
        let a = a.index();
        self.analyses[a].run.bound
    }

    pub fn store(&self, a: AnalysisId) -> &Store {
        let a = a.index();
        &self.analyses[a].run.store
    }

    /// How far the run's decoder has taken its wire input.
    pub fn consumed(&self, a: AnalysisId) -> Tick {
        let a = a.index();
        self.analyses[a].run.consumed
    }

    pub fn finished(&self, a: AnalysisId) -> Option<Tick> {
        let a = a.index();
        self.analyses[a].run.finished
    }

    /// Whether the analyses stacked on `a` have nothing waiting.
    pub fn quiet_below(&self, a: AnalysisId) -> bool {
        let a = a.index();
        let mut stack = self.analyses[a].downstream.clone();
        while let Some(d) = stack.pop() {
            let r = &self.analyses[d].run;
            if !r.inbox.is_empty() || r.more {
                return false;
            }
            stack.extend(self.analyses[d].downstream.iter().copied());
        }
        true
    }

    /// The host's record, oldest first, where `HostOptions::log` keeps one.
    pub fn log(&self) -> impl Iterator<Item = &LogEntry> {
        self.log.iter()
    }

    /// No item analysis `a`'s run emits from now on starts before this.
    pub fn start_frontier(&self, a: AnalysisId) -> Tick {
        let a = a.index();
        self.analyses[a].run.frontier
    }

    /// Nothing analysis `a`'s run has emitted before this will change: the view's decoded
    /// coverage.
    pub fn settled(&self, a: AnalysisId) -> Tick {
        let a = a.index();
        self.analyses[a].run.settled
    }

    /// The most closed ids the run has held to refuse a repeated open.
    pub fn peak_recent(&self, a: AnalysisId) -> usize {
        let a = a.index();
        self.analyses[a].run.peak_recent
    }

    /// The flag that stops analysis `a`'s current run's read or turn in progress: another thread
    /// raises it, and the host lowers it once acted on.
    pub fn interrupt(&self, a: AnalysisId) -> Arc<AtomicBool> {
        let a = a.index();
        self.analyses[a].run.interrupt.clone()
    }

    /// The stretches analysis `a`'s run has declared its input lost over that are not yet settled
    /// or meet a kept span, adjacent ones joined.
    pub fn lost(&self, a: AnalysisId) -> &[Span] {
        let a = a.index();
        &self.analyses[a].run.lost
    }

    /// How many losses analysis `a`'s run has declared.
    pub fn lost_total(&self, a: AnalysisId) -> u64 {
        let a = a.index();
        self.analyses[a].run.lost_total
    }

    pub fn outstanding(&self, a: AnalysisId) -> Option<RequestId> {
        let a = a.index();
        self.analyses[a].run.outstanding.map(|o| o.0)
    }

    /// Admits a batch whole or refuses it whole: the run must be current and the batch next, every
    /// op must fit the schema, and the lifecycle must hold. Then fans it out downstream.
    pub fn submit(&mut self, b: Batch) -> Result<(), Refusal> {
        self.admit(b).map_err(|why| self.refuse(why))
    }

    fn admit(&mut self, b: Batch) -> Result<(), Refusal> {
        let a = b.run.analysis.index();
        let recent_at_most = self.options.recent_at_most;
        let Some(an) = self.analyses.get_mut(a) else {
            return Err(Refusal::StaleRun {
                got: b.run,
                current: b.run,
            });
        };
        if b.run != an.run.id {
            return Err(Refusal::StaleRun {
                got: b.run,
                current: an.run.id,
            });
        }
        if b.seq != an.run.next_batch {
            return Err(Refusal::BatchOrder {
                expected: an.run.next_batch,
                got: b.seq,
            });
        }
        for (k, op) in b.ops.iter().enumerate() {
            an.def
                .schema
                .check(&an.resolved, op)
                .map_err(|why| Refusal::Schema { op: k, why })?;
        }
        let wire = matches!(an.input, Input::Wire(_));
        let at_most = an.def.schema.open_at_most;
        let run = &mut an.run;
        if let Some(got) = b.frontier {
            if got < run.frontier {
                return Err(Refusal::FrontierBack {
                    was: run.frontier,
                    got,
                });
            }
            let input = if wire { run.consumed } else { run.input };
            if got > input {
                return Err(Refusal::FrontierAhead { got, input });
            }
        }
        let mut was = run.lost_end;
        let mut place = 0;
        for &(k, got) in &b.lost {
            if k < place || k > b.ops.len() || got.start >= got.end || got.start < was {
                return Err(Refusal::LossOrder { was, got });
            }
            let input = if wire { run.consumed } else { run.lost_input };
            if got.end > input {
                return Err(Refusal::LossAhead {
                    got: got.end,
                    input,
                });
            }
            (was, place) = (got.end, k);
        }
        let mut closing = HashSet::new();
        for (k, op) in b.ops.iter().enumerate() {
            let at = match op {
                Op::Open { id, start, .. } => {
                    if id.anchor < *start {
                        return Err(Refusal::AnchorBeforeStart { op: k });
                    }
                    if run.recent.contains(&(id.anchor, *id)) || closing.contains(id) {
                        return Err(Refusal::Lifecycle {
                            op: k,
                            why: Refused::Reopened,
                        });
                    }
                    Some(*start)
                }
                Op::Set { span, .. } => span.map(|s| s.start),
                Op::Extend { elements, .. } => elements.iter().map(|e| e.cell.start).min(),
                Op::Close { id, .. } => {
                    closing.insert(*id);
                    None
                }
                Op::Link { .. } => None,
            };
            if let Some(at) = at
                && at < run.frontier
            {
                return Err(Refusal::LateStart {
                    op: k,
                    at,
                    frontier: run.frontier,
                });
            }
        }
        let frontier = b.frontier.unwrap_or(run.frontier);
        let floor = (frontier, Id::new(KindId(0), Tick::MIN));
        let held = run.recent.range(floor..).count()
            + closing.iter().filter(|id| id.anchor >= frontier).count();
        if held > recent_at_most {
            return Err(Refusal::TooManyRecent {
                held,
                at_most: recent_at_most,
            });
        }
        run.store
            .apply_batch(&b.ops, at_most)
            .map_err(|(op, why)| Refusal::Lifecycle { op, why })?;
        run.next_batch += 1;
        for id in closing {
            run.recent.insert((id.anchor, id));
        }
        let before = (run.frontier, run.settled);
        if b.frontier.is_some() {
            run.frontier = frontier;
            run.recent = run.recent.split_off(&floor);
        }
        run.peak_recent = run.peak_recent.max(run.recent.len());
        for &(_, span) in &b.lost {
            run.lost_total += 1;
            run.lost_end = span.end;
            match run.lost.last_mut() {
                Some(last) if last.end == span.start => last.end = span.end,
                _ => run.lost.push(span),
            }
        }
        // Only open items can change, and new ones start at or after the start frontier.
        let settled = run
            .store
            .earliest_open()
            .map_or(run.frontier, |o| o.min(run.frontier));
        run.settled = run.settled.max(settled);
        let (settled, keep) = (run.settled, &run.store.keep);
        run.lost
            .retain(|l| l.end > settled || keep.iter().any(|k| l.start < k.end && k.start < l.end));
        let after = (run.frontier, run.settled);
        self.admitted += 1;
        if self.options.log.is_some() {
            let summary = b.ops.iter().map(|op| (op.letter(), *op.id())).collect();
            let (run, batch) = (b.run, b.seq);
            self.note(|order| LogEntry::Admitted {
                order,
                run,
                batch,
                ops: summary,
            });
        }
        self.fan_out(a, b.seq, &b.ops, &b.lost)?;
        if after != before {
            let from = b.run;
            for d in self.analyses[a].downstream.clone() {
                self.deliver(
                    d,
                    Delivery::Frontier {
                        from,
                        start: after.0,
                        settled: after.1,
                    },
                )?;
            }
        }
        Ok(())
    }

    fn fan_out(
        &mut self,
        a: usize,
        batch: u64,
        ops: &[Op],
        lost: &[(usize, Span)],
    ) -> Result<(), Refusal> {
        let from = self.analyses[a].run.id;
        let subs: Vec<(usize, Resolution)> = self.analyses[a]
            .downstream
            .iter()
            .filter_map(|&d| match &self.analyses[d].input {
                Input::Upstream { sub, .. } => Some((d, sub.clone())),
                Input::Wire(_) => None,
            })
            .collect();
        if subs.is_empty() {
            return Ok(());
        }
        let assemble: HashSet<KindId> = subs
            .iter()
            .filter(|(_, s)| s.mode == Mode::Completed)
            .flat_map(|(_, s)| s.kinds.iter().copied())
            .collect();
        let mut out = vec![];
        let mut losses = lost.iter().peekable();
        let mut lose = |at: usize, out: &mut Vec<(usize, Delivery)>| {
            while let Some(&(_, span)) = losses.next_if(|l| l.0 == at) {
                for (d, _) in &subs {
                    out.push((*d, Delivery::Lost { from, batch, span }));
                }
            }
        };
        for (k, op) in ops.iter().enumerate() {
            lose(k, &mut out);
            let id = *op.id();
            for (d, sub) in &subs {
                if sub.mode != Mode::Pieces || !sub.kinds.contains(&id.kind) {
                    continue;
                }
                if let Op::Extend { seq, .. } = op
                    && !sub.sequences.contains(&(id.kind, *seq))
                {
                    continue;
                }
                out.push((
                    *d,
                    Delivery::Piece {
                        from,
                        batch,
                        op: op.clone(),
                    },
                ));
            }
            if !assemble.contains(&id.kind) {
                continue;
            }
            let asm = &mut self.analyses[a].run.assembling;
            // The store admitted every op, so an item being assembled is open.
            match op {
                Op::Open { id, parent, start } => {
                    asm.insert(
                        *id,
                        Completed {
                            id: *id,
                            parent: *parent,
                            start: *start,
                            end: *start,
                            status: Status::Complete,
                            fields: vec![],
                            links: vec![],
                            lengths: vec![],
                        },
                    );
                }
                Op::Set {
                    id,
                    field,
                    value,
                    span,
                    driver,
                } => {
                    if let Some(c) = asm.get_mut(id) {
                        c.fields.push(Field {
                            field: *field,
                            value: value.clone(),
                            span: *span,
                            driver: *driver,
                        });
                    }
                }
                Op::Extend {
                    id, seq, elements, ..
                } => {
                    if let Some(c) = asm.get_mut(id) {
                        let n = elements.len() as u32;
                        match c.lengths.iter_mut().find(|(s, _)| s == seq) {
                            Some((_, len)) => *len = len.saturating_add(n),
                            None => c.lengths.push((*seq, n)),
                        }
                    }
                }
                Op::Link { id, link, other } => {
                    if let Some(c) = asm.get_mut(id) {
                        c.links.push((*link, *other));
                    }
                }
                Op::Close { id, end, status } => {
                    let Some(mut c) = asm.remove(id) else {
                        continue;
                    };
                    c.end = *end;
                    c.status = *status;
                    for (d, sub) in &subs {
                        if sub.mode == Mode::Completed && sub.kinds.contains(&id.kind) {
                            out.push((
                                *d,
                                Delivery::Item {
                                    from,
                                    batch,
                                    item: c.clone(),
                                },
                            ));
                        }
                    }
                }
            }
        }
        lose(ops.len(), &mut out);
        for (d, delivery) in out {
            self.deliver(d, delivery)?;
        }
        Ok(())
    }

    /// Queues a delivery for analysis `to`, if its run is bound to the delivery's upstream run.
    fn deliver(&mut self, to: usize, d: Delivery) -> Result<(), Refusal> {
        let r = &mut self.analyses[to].run;
        if Some(d.from()) != r.bound {
            let why = Refusal::StaleDelivery {
                from: d.from(),
                bound: r.bound,
            };
            return Err(self.refuse(why));
        }
        let to_run = r.id;
        let what = match &d {
            Delivery::Piece { op, .. } => (op.letter(), Some(*op.id())),
            Delivery::Item { item, .. } => ('I', Some(item.id)),
            Delivery::Frontier { .. } => ('F', None),
            Delivery::Lost { .. } => ('G', None),
            Delivery::End { .. } => ('E', None),
        };
        let from = d.from();
        r.inbox.push_back(d);
        r.fresh = true;
        let depth = r.inbox.len();
        self.peak_inbox = self.peak_inbox.max(depth);
        self.delivered += 1;
        if self.options.log.is_some() {
            self.note(|order| LogEntry::Delivered {
                order,
                to: to_run,
                from,
                what,
            });
        }
        Ok(())
    }

    /// Performs the next pending read against its source, outside any decoder's turn. A read for
    /// a run replaced meanwhile still happens: it was already under way. An interrupted read
    /// returns None and stays first in line, still outstanding; so does a failed one.
    pub fn take_io(&mut self) -> Result<Option<Reply>, Refusal> {
        let Some((a, id, req)) = self.io.pop_front() else {
            return Ok(None);
        };
        let interrupt = self.analyses[a].run.interrupt.clone();
        let Input::Wire(source) = &mut self.analyses[a].input else {
            // Only wire analyses request.
            return Ok(None);
        };
        let Request::Window { from, upto } = req;
        match source.window(from, upto, &interrupt) {
            Ok(Some(window)) => Ok(Some(Reply { id, window })),
            Ok(None) => {
                interrupt.store(false, Relaxed);
                self.io.push_front((a, id, req));
                self.cancelled_reads += 1;
                Ok(None)
            }
            Err(e) => {
                self.io.push_front((a, id, req));
                let why = e.to_string();
                Err(self.refuse(Refusal::Read { run: id.run, why }))
            }
        }
    }

    /// Admits a reply if it answers the current run's outstanding request; it is consumed once.
    pub fn reply(&mut self, reply: Reply) -> Result<(), Refusal> {
        let a = reply.id.run.analysis.index();
        let outstanding = self.analyses.get(a).and_then(|an| an.run.outstanding);
        match outstanding {
            Some((id, _)) if id == reply.id => {
                let r = &mut self.analyses[a].run;
                r.outstanding = None;
                r.ready = Some(reply.window);
                r.fresh = true;
                Ok(())
            }
            other => {
                let why = Refusal::StaleReply {
                    got: reply.id,
                    outstanding: other.map(|o| o.0),
                };
                Err(self.refuse(why))
            }
        }
    }

    /// Runs one turn of analysis `a`'s decoder and admits what it produced.
    pub fn turn(&mut self, a: AnalysisId) -> Result<Pause, Refusal> {
        self.turn_at(a.index())
    }

    fn turn_at(&mut self, a: usize) -> Result<Pause, Refusal> {
        let mut t = self.begin_turn_at(a)?;
        t.run();
        self.end_turn(t)
    }

    /// Takes analysis `a`'s decoder out for a turn, with its ready reply and queued deliveries.
    pub fn begin_turn(&mut self, a: AnalysisId) -> Result<Turn, Refusal> {
        self.begin_turn_at(a.index())
    }

    fn begin_turn_at(&mut self, a: usize) -> Result<Turn, Refusal> {
        let r = &mut self.analyses[a].run;
        if r.out {
            let why = Refusal::TurnUnderWay(r.id);
            return Err(self.refuse(why));
        }
        r.started = true;
        r.out = true;
        r.fresh = false;
        Ok(Turn {
            a,
            run: r.id,
            decoder: std::mem::replace(&mut r.decoder, Box::new(Taken)),
            reply: r.ready.take(),
            inbox: std::mem::take(&mut r.inbox),
            budget: self.options.turn_deliveries,
            interrupt: r.interrupt.clone(),
            done: None,
        })
    }

    /// Admits a turn that has run, if its run is still current: its output and frontier, its
    /// request, and its end. A turn from a run replaced meanwhile is refused whole.
    pub fn end_turn(&mut self, mut t: Turn) -> Result<Pause, Refusal> {
        let a = t.a;
        let current = self.analyses[a].run.id;
        if t.run != current {
            let why = Refusal::StaleRun {
                got: t.run,
                current,
            };
            return Err(self.refuse(why));
        }
        let r = &mut self.analyses[a].run;
        r.decoder = t.decoder;
        r.out = false;
        // Deliveries the turn did not take come before any that arrived meanwhile.
        t.inbox.append(&mut r.inbox);
        r.inbox = t.inbox;
        r.interrupt.store(false, Relaxed);
        let Some(Done {
            pause,
            more,
            out,
            taken,
            request,
            twice,
            end,
            frontier,
            upstream,
            lost,
            received,
        }) = t.done.take()
        else {
            // A turn handed back without running: its reply goes back too.
            r.fresh |= t.reply.is_some() || !r.inbox.is_empty();
            r.ready = t.reply;
            return Ok(Pause::Input);
        };
        r.more = pause == Pause::Work || (pause == Pause::Input && more);
        // What arrived during the turn is new to the decoder.
        r.fresh |= r.ready.is_some();
        if let Some(t) = taken {
            r.consumed = t;
        }
        if let Some(u) = upstream {
            r.input = u;
        }
        if let Some(l) = received.last() {
            r.lost_input = l.end;
        }
        let (id, seq) = (r.id, r.next_batch);
        if twice {
            return Err(self.refuse(Refusal::RequestTwice(id)));
        }
        if let Some(&span) = received.iter().find(|s| !lost.iter().any(|l| l.1 == **s)) {
            return Err(self.refuse(Refusal::LossNotDeclared { run: id, span }));
        }
        let moved = frontier.is_some_and(|f| f != r.frontier);
        if !out.ops.is_empty() || moved || !lost.is_empty() {
            self.submit(Batch {
                run: id,
                seq,
                ops: out.ops,
                frontier,
                lost,
            })?;
        }
        if let Some(req) = request {
            let r = &mut self.analyses[a].run;
            if let Some((outstanding, _)) = r.outstanding {
                return Err(self.refuse(Refusal::RequestOutstanding { outstanding }));
            }
            let rid = RequestId {
                run: id,
                seq: r.next_request,
            };
            r.next_request += 1;
            r.outstanding = Some((rid, req));
            self.io.push_back((a, rid, req));
            self.note(|order| LogEntry::Request {
                order,
                id: rid,
                request: req,
            });
        }
        if let Some(end) = end {
            let open = self.analyses[a].run.store.open_count();
            if open > 0 {
                return Err(self.refuse(Refusal::LeftOpen { run: id, open }));
            }
            self.analyses[a].run.finished = Some(end);
            for d in self.analyses[a].downstream.clone() {
                self.deliver(d, Delivery::End { from: id, end })?;
            }
        }
        Ok(pause)
    }

    /// One scheduling step: completes pending reads, then runs one turn, preferring analyses
    /// further downstream so that inboxes drain before their upstream produces more. Returns
    /// whether anything ran.
    pub fn step(&mut self) -> Result<bool, Refusal> {
        let mut read = false;
        while !self.io.is_empty() {
            read = true;
            // An interrupted read stays outstanding and is tried again next step.
            let Some(reply) = self.take_io()? else {
                break;
            };
            match self.reply(reply) {
                Ok(()) | Err(Refusal::StaleReply { .. }) => {}
                Err(e) => return Err(e),
            }
        }
        if let Some(a) = (0..self.analyses.len())
            .rev()
            .find(|&a| self.runnable_at(a))
        {
            self.turn_at(a)?;
            return Ok(true);
        }
        Ok(read)
    }

    /// Whether analysis `a` has something to do and room downstream to put it.
    pub fn runnable(&self, a: AnalysisId) -> bool {
        self.runnable_at(a.index())
    }

    fn runnable_at(&self, a: usize) -> bool {
        let r = &self.analyses[a].run;
        let blocked = (self.analyses[a].downstream.iter())
            .any(|&d| self.analyses[d].run.inbox.len() > self.options.inbox_at_most);
        r.finished.is_none() && !r.out && (!r.started || r.fresh || r.more) && !blocked
    }

    /// Steps until `stop` holds or nothing can run.
    pub fn run_until(&mut self, stop: impl Fn(&Host) -> bool) -> Result<(), Refusal> {
        while !stop(self) {
            if !self.step()? {
                break;
            }
        }
        Ok(())
    }

    fn next_generation(old: RunId) -> RunId {
        RunId {
            analysis: old.analysis,
            generation: old.generation.saturating_add(1),
        }
    }

    /// Replaces analysis `a`'s run with a new generation starting at `local` (None: the
    /// beginning), and every run stacked on it with a new generation bound to its replaced
    /// upstream. Their output, queued input and outstanding requests are dropped; a reply or
    /// batch for an old generation is refused when it arrives. Returns the retired runs.
    pub fn replace(&mut self, a: AnalysisId, local: Option<Tick>) -> Vec<RunId> {
        let a = a.index();
        let mut retired = vec![];
        let mut queue = VecDeque::from([(a, local)]);
        let mut seen = HashSet::new();
        while let Some((x, local)) = queue.pop_front() {
            if !seen.insert(x) {
                continue;
            }
            let bound = match &self.analyses[x].input {
                Input::Upstream { index, .. } => Some(self.analyses[*index].run.id),
                Input::Wire(_) => None,
            };
            let an = &mut self.analyses[x];
            let old = an.run.id;
            let id = Self::next_generation(old);
            let decoder = (an.factory)(local);
            an.run = Run::new(id, local, decoder, Store::with_keep(an.keep.clone()), bound);
            retired.push(old);
            queue.extend(an.downstream.iter().map(|&d| (d, local)));
            self.note(|order| LogEntry::Replaced {
                order,
                old,
                new: id,
            });
        }
        retired
    }

    /// Analysis `a` and every analysis stacked on it, upstream first.
    fn below(&self, a: usize) -> Vec<usize> {
        let mut order = vec![a];
        let mut k = 0;
        while k < order.len() {
            for &d in &self.analyses[order[k]].downstream {
                if !order.contains(&d) {
                    order.push(d);
                }
            }
            k += 1;
        }
        order
    }

    /// Checkpoints wire analysis `a` and everything stacked on it, between turns: decoder states,
    /// stores with their open items and sequence counts, queued deliveries, items being
    /// assembled, and frontiers. Outstanding requests are not kept: a restored decoder asks again.
    pub fn checkpoint(&self, a: AnalysisId) -> Result<Checkpoint, CheckpointError> {
        let a = a.index();
        if !matches!(self.analyses[a].input, Input::Wire(_)) {
            return Err(CheckpointError::NotARoot(self.analyses[a].name.clone()));
        }
        let mut snaps = vec![];
        for x in self.below(a) {
            let an = &self.analyses[x];
            let r = &an.run;
            let decoder = (!r.out)
                .then(|| r.decoder.save())
                .flatten()
                .ok_or_else(|| CheckpointError::Unsupported(an.name.clone()))?;
            snaps.push(Snapshot {
                analysis: x,
                taken: r.id,
                decoder,
                store: r.store.clone(),
                inbox: r.inbox.clone(),
                assembling: r.assembling.clone(),
                recent: r.recent.clone(),
                local: r.local,
                frontier: r.frontier,
                settled: r.settled,
                input: r.input,
                consumed: r.consumed,
                finished: r.finished,
                lost: r.lost.clone(),
                lost_end: r.lost_end,
                lost_total: r.lost_total,
                lost_input: r.lost_input,
            });
        }
        Ok(Checkpoint {
            runs: snaps.iter().map(|s| s.taken).collect(),
            snaps,
        })
    }

    /// Restores a checkpoint as new generations of its runs, each bound to its restored upstream.
    /// The runs it replaces are retired as by `replace`: their late replies, batches and
    /// deliveries are refused. A checkpoint can be restored more than once. Returns the retired
    /// runs; refused, nothing changes.
    pub fn restore(&mut self, cut: &Checkpoint) -> Result<Vec<RunId>, CheckpointError> {
        let mut decoders = vec![];
        for snap in &cut.snaps {
            let an = self
                .analyses
                .get(snap.analysis)
                .ok_or(CheckpointError::Foreign)?;
            let decoder = snap
                .decoder
                .save()
                .ok_or_else(|| CheckpointError::Unsupported(an.name.clone()))?;
            decoders.push(decoder);
        }
        let mut retired = vec![];
        let mut renamed: HashMap<RunId, RunId> = HashMap::new();
        for (snap, decoder) in cut.snaps.iter().zip(decoders) {
            let x = snap.analysis;
            let bound = match &self.analyses[x].input {
                Input::Upstream { index, .. } => Some(self.analyses[*index].run.id),
                Input::Wire(_) => None,
            };
            let an = &mut self.analyses[x];
            let old = an.run.id;
            let id = Self::next_generation(old);
            renamed.insert(snap.taken, id);
            let mut run = Run::new(id, snap.local, decoder, snap.store.clone(), bound);
            run.inbox = snap
                .inbox
                .iter()
                .cloned()
                .map(|mut d| {
                    let from = d.source_mut();
                    *from = renamed.get(from).copied().unwrap_or(*from);
                    d
                })
                .collect();
            run.assembling = snap.assembling.clone();
            run.recent = snap.recent.clone();
            run.frontier = snap.frontier;
            run.settled = snap.settled;
            run.input = snap.input;
            run.consumed = snap.consumed;
            run.finished = snap.finished;
            run.lost = snap.lost.clone();
            run.lost_end = snap.lost_end;
            run.lost_total = snap.lost_total;
            run.lost_input = snap.lost_input;
            run.fresh = !run.inbox.is_empty();
            an.run = run;
            retired.push(old);
            self.note(|order| LogEntry::Replaced {
                order,
                old,
                new: id,
            });
        }
        Ok(retired)
    }
}

impl Run {
    fn new(
        id: RunId,
        local: Option<Tick>,
        decoder: Box<dyn Decoder>,
        store: Store,
        bound: Option<RunId>,
    ) -> Self {
        Self {
            id,
            local,
            decoder,
            out: false,
            store,
            next_batch: 0,
            next_request: 0,
            outstanding: None,
            ready: None,
            inbox: VecDeque::new(),
            bound,
            assembling: HashMap::new(),
            started: false,
            more: false,
            fresh: false,
            finished: None,
            consumed: local.unwrap_or(Tick::MIN),
            frontier: Tick::MIN,
            settled: Tick::MIN,
            input: Tick::MIN,
            recent: BTreeSet::new(),
            peak_recent: 0,
            lost: vec![],
            lost_end: Tick::MIN,
            lost_total: 0,
            lost_input: Tick::MIN,
            interrupt: Arc::new(AtomicBool::new(false)),
        }
    }
}
