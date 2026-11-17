//! A recording opened for a graph, and the thread that answers the graph's queries.

use std::fs::File;
use std::io;
use std::ops::Range;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::thread::JoinHandle;

use escapement_tickwave_view::format::annotations::{Colour, Role};
use escapement_tickwave_view::format::model::Ratio;
#[cfg(feature = "live")]
use escapement_tickwave_view::format::snapshot::Snapshot;
use escapement_tickwave_view::{
    Answer, Bytes, Cache, ChannelInfo, Error, MarkerQuery, Query, Recording,
};

use crate::state::Window;

/// Decoded data a viewer keeps by default: one step in on a synthetic hour at 100k pairs a second,
/// `around` decodes about 80 MB.
const CACHE_BYTES: usize = 256 << 20;

/// How a viewer reads its recording. A host builds one with `default` and sets its fields:
///
/// ```
/// # use escapement_tickwave_egui::ViewerOptions;
/// let mut options = ViewerOptions::default();
/// options.cache_bytes = 64 << 20;
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ViewerOptions {
    /// About the most decoded data the viewer keeps, in bytes: index pages, blocks and overview
    /// blocks, least recently used out. 256 MiB unless set.
    pub cache_bytes: usize,
}

impl Default for ViewerOptions {
    fn default() -> Self {
        Self {
            cache_bytes: CACHE_BYTES,
        }
    }
}

/// What a graph asks: a query whose window is resolved against the extent when it runs, so a
/// followed window ends where the recording does then, or at the host's now where that is later.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Request {
    pub window: Window,
    pub now: Option<i64>,
    pub columns: u32,
    pub channels: Vec<usize>,
    pub flags: Vec<(usize, usize)>,
    pub strip: Option<(usize, u32)>,
    /// The lanes' columns over the extent, which a lane draws past `around`.
    pub fallbacks: Vec<(usize, u32)>,
    pub selection: Option<Range<i64>>,
    pub markers: MarkerQuery,
}

impl Request {
    fn follows(&self) -> bool {
        matches!(self.window, Window::Follow { .. })
    }

    /// The query, its window resolved against `extent`. A recording being written, followed,
    /// has its columns at fixed ticks, so the next snapshot's answer extends this one's.
    fn query(&self, extent: Option<&Range<i64>>, live: bool) -> Query {
        let window = self.window.resolve(extent, self.now);
        let mut q = Query::new(window.clone(), self.columns);
        q.statistics = std::iter::once(window)
            .chain(self.selection.clone())
            .collect();
        q.channels = self.channels.clone();
        q.flags = self.flags.clone();
        q.strip = self.strip;
        q.fallbacks = self.fallbacks.clone();
        q.markers = self.markers.clone();
        q.aligned = live && self.follows();
        q
    }
}

/// The latest answer, the request it answers, and the query it was made for.
#[derive(Clone, Debug)]
pub(crate) struct Held {
    pub request: Arc<Request>,
    /// The request's generation, which an extend after a snapshot keeps to.
    #[cfg(feature = "live")]
    pub generation: u64,
    pub query: Arc<Query>,
    pub answer: Arc<Answer>,
}

/// What a frame reads of the recording without taking its lock.
#[derive(Clone, Debug)]
pub(crate) struct Facts {
    pub timebase: Ratio,
    pub channels: Arc<[ChannelInfo]>,
    /// The recording has markers, read or not: the graph keeps room for their labels.
    pub markers: bool,
    /// Made from a writer's snapshot, and not reopened since.
    pub live: bool,
    /// The paint roles the sidecar declares, in order: the graph keeps room for their key
    /// whenever there are any.
    pub roles: Arc<[Role]>,
    /// The trace's colour the sidecar stores (ANNOTATIONS.md, "The stored view").
    pub stored: Option<Colour>,
}

impl Facts {
    /// What a frame reads of `r`. Its roles and stored view come from its bundle alone, read the
    /// first time, no annotation part with them; a bundle that doesn't read declares none.
    fn of(r: &Recording, live: bool) -> Self {
        Self {
            timebase: r.timebase(),
            channels: r.channels().into(),
            markers: r.has_markers(),
            live,
            roles: r.roles().unwrap_or_default().into(),
            stored: r.stored_view().ok().flatten().and_then(|v| v.colour),
        }
    }
}

struct Job {
    generation: u64,
    request: Arc<Request>,
    cancel: Arc<AtomicBool>,
    /// The answer held for the same request, which the job extends where it can.
    held: Option<Held>,
}

struct Slot {
    /// The generation of the newest request: an answer to an older one is never held.
    issued: u64,
    /// The newest request not yet started; a newer one replaces it.
    pending: Option<Job>,
    running: Option<Arc<AtomicBool>>,
    /// The job running is a snapshot's follow-up, which a request cancels.
    following: bool,
    /// A snapshot is being taken.
    #[cfg(feature = "live")]
    applying: bool,
    sent: Option<Arc<Request>>,
    /// The graph's latest now, which a snapshot's follow-up ends a followed window at.
    #[cfg(feature = "live")]
    now: Option<i64>,
    held: Option<Held>,
    /// What a frame draws while no answer is held: the first request's sketch, from the
    /// recording's coarsest levels.
    sketch: Option<Held>,
    query_failure: Option<String>,
    update_failure: Option<String>,
    #[cfg(feature = "live")]
    snapshot: Option<Snapshot>,
    replace: Option<Recording>,
    facts: Facts,
    /// Snapshots taken, for the tests.
    #[cfg(test)]
    snapshots: u64,
    /// Queries run to the end or cancelled, for the tests.
    #[cfg(test)]
    runs: u64,
    /// Each answer held: its request's generation, and the newest then. For the tests.
    #[cfg(test)]
    landed: Vec<(u64, u64)>,
    /// Answers extended from the one held, for the tests.
    #[cfg(test)]
    extends: u64,
    /// Sketches held, for the tests.
    #[cfg(test)]
    sketches: u64,
    /// No sketch is made: for the tests of what a viewer holding nothing draws.
    no_sketch: bool,
    /// The file is let go of until `reopen`: the query thread runs nothing.
    released: bool,
    /// Made from a writer's snapshot and opened finished since: a snapshot given now is one the
    /// host took before the finish, and is dropped.
    #[cfg(feature = "live")]
    finished_live: bool,
    stopped: bool,
}

type Answered = std::thread::Result<Result<(Answer, Query), Error>>;

impl Slot {
    /// After a snapshot, the newest request again, where it follows the recording, ending at the
    /// graph's latest now: from the answer to it where that is the one held.
    #[cfg(feature = "live")]
    fn again(&self) -> Option<Job> {
        let request = self.sent.as_ref().filter(|r| r.follows())?;
        let request = match self.now {
            now if now == request.now => Arc::clone(request),
            now => Arc::new(Request {
                now,
                ..Request::clone(request)
            }),
        };
        Some(Job {
            generation: self.issued,
            request,
            cancel: Arc::new(AtomicBool::new(false)),
            held: self.held.clone().filter(|h| h.generation == self.issued),
        })
    }

    /// Holds what `job` answered, unless it was cancelled or a newer request was issued while it
    /// ran: an answer to an older request, an extend's included, is never held. Whether there
    /// is anything new to draw.
    fn land(&mut self, job: Job, answered: Answered) -> bool {
        if job.cancel.load(Ordering::Relaxed) || job.generation != self.issued {
            return false;
        }
        match answered {
            Ok(Ok((answer, query))) => {
                #[cfg(test)]
                self.landed.push((job.generation, self.issued));
                self.held = Some(Held {
                    request: job.request,
                    #[cfg(feature = "live")]
                    generation: job.generation,
                    query: Arc::new(query),
                    answer: Arc::new(answer),
                });
                self.sketch = None;
                self.query_failure = None;
            }
            Ok(Err(e)) => self.query_failure = Some(e.to_string()),
            Err(_) => self.query_failure = Some("the query stopped on a fault".into()),
        }
        true
    }
}

type Opener = Box<dyn Fn() -> io::Result<Arc<dyn Bytes>> + Send + Sync>;

/// The bytes the opener gave, until `Viewer::release` lets go of them: a read after that fails.
struct Releasable(RwLock<Option<Arc<dyn Bytes>>>);

impl Releasable {
    fn new(bytes: Arc<dyn Bytes>) -> Arc<Self> {
        Arc::new(Self(RwLock::new(Some(bytes))))
    }

    /// Drops the bytes once no read holds them.
    fn release(&self) {
        *self.0.write().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Reads through `bytes` again, after `release`.
    fn take_back(&self, bytes: Arc<dyn Bytes>) {
        *self.0.write().unwrap_or_else(|e| e.into_inner()) = Some(bytes);
    }

    fn with<R>(&self, read: impl FnOnce(&dyn Bytes) -> io::Result<R>) -> io::Result<R> {
        match &*self.0.read().unwrap_or_else(|e| e.into_inner()) {
            Some(bytes) => read(bytes.as_ref()),
            None => Err(io::Error::other("the viewer let go of the file")),
        }
    }
}

impl Bytes for Releasable {
    fn size(&self) -> io::Result<u64> {
        self.with(|b| b.size())
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.with(|b| b.read_at(offset, buf))
    }
}

struct Shared {
    ctx: egui::Context,
    open: Opener,
    /// What the recording reads through, the latest opened. Set and taken only under `slot`'s
    /// lock, so a `release` and a `reopen` on two threads agree on which file is let go of.
    bytes: Mutex<Arc<Releasable>>,
    cache_bytes: usize,
    /// The cache of the latest recording.
    #[cfg(test)]
    cache: Mutex<Cache>,
    recording: RwLock<Recording>,
    slot: Mutex<Slot>,
    wake: Condvar,
}

impl Shared {
    fn slot(&self) -> MutexGuard<'_, Slot> {
        // Nothing that can panic runs under the lock: a query runs outside it, its panic caught.
        self.slot.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn read(&self) -> RwLockReadGuard<'_, Recording> {
        self.recording.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> RwLockWriteGuard<'_, Recording> {
        self.recording.write().unwrap_or_else(|e| e.into_inner())
    }
}

/// A recording a graph shows, and the one thread that answers its queries off the UI thread. A
/// newer query replaces one not yet started and cancels one running; only the newest one's answer
/// is kept, and its arrival asks egui for a frame. It is `Send`, so a host may open it off the UI
/// thread and hand it over.
pub struct Viewer {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

const _: fn() = || {
    fn send<T: Send>() {}
    send::<Viewer>();
};

fn file(path: &Path) -> Opener {
    let path: PathBuf = path.to_owned();
    Box::new(move || Ok(Arc::new(File::open(&path)?) as Arc<dyn Bytes>))
}

impl Viewer {
    /// The finished recording at `path`. `Error::Unfinished` for one with no footer, which the
    /// format crate's Recutter repairs.
    pub fn open(ctx: &egui::Context, path: &Path) -> Result<Self, Error> {
        Self::open_with(ctx, path, ViewerOptions::default())
    }

    /// `open`, reading as `options` says.
    pub fn open_with(
        ctx: &egui::Context,
        path: &Path,
        options: ViewerOptions,
    ) -> Result<Self, Error> {
        Self::opened(ctx, file(path), options.cache_bytes)
    }

    fn opened(ctx: &egui::Context, open: Opener, cache_bytes: usize) -> Result<Self, Error> {
        let bytes = Releasable::new(open()?);
        let cache = Cache::new(cache_bytes);
        let recording = Recording::open(bytes.clone(), cache.clone())?;
        Ok(Self::start(
            ctx,
            open,
            bytes,
            cache,
            cache_bytes,
            recording,
            false,
        ))
    }

    /// The recording at `path` that a writer in this process is writing. `snapshot` is the
    /// writer's, taken after the bytes it covers are in the file; give each newer one to `update`.
    #[cfg(feature = "live")]
    pub fn live(ctx: &egui::Context, path: &Path, snapshot: Snapshot) -> Result<Self, Error> {
        Self::live_with(ctx, path, snapshot, ViewerOptions::default())
    }

    /// `live`, reading as `options` says.
    #[cfg(feature = "live")]
    pub fn live_with(
        ctx: &egui::Context,
        path: &Path,
        snapshot: Snapshot,
        options: ViewerOptions,
    ) -> Result<Self, Error> {
        let open = file(path);
        let bytes = Releasable::new(open()?);
        let cache = Cache::new(options.cache_bytes);
        let recording = Recording::live(snapshot, bytes.clone(), cache.clone())?;
        Ok(Self::start(
            ctx,
            open,
            bytes,
            cache,
            options.cache_bytes,
            recording,
            true,
        ))
    }

    fn start(
        ctx: &egui::Context,
        open: Opener,
        bytes: Arc<Releasable>,
        #[cfg_attr(not(test), allow(unused_variables))] cache: Cache,
        cache_bytes: usize,
        recording: Recording,
        live: bool,
    ) -> Self {
        let shared = Arc::new(Shared {
            ctx: ctx.clone(),
            open,
            bytes: Mutex::new(bytes),
            cache_bytes,
            #[cfg(test)]
            cache: Mutex::new(cache),
            slot: Mutex::new(Slot {
                issued: 0,
                pending: None,
                running: None,
                following: false,
                #[cfg(feature = "live")]
                applying: false,
                sent: None,
                #[cfg(feature = "live")]
                now: None,
                held: None,
                sketch: None,
                query_failure: None,
                update_failure: None,
                #[cfg(feature = "live")]
                snapshot: None,
                replace: None,
                facts: Facts::of(&recording, live),
                #[cfg(test)]
                snapshots: 0,
                #[cfg(test)]
                runs: 0,
                #[cfg(test)]
                landed: Vec::new(),
                #[cfg(test)]
                extends: 0,
                #[cfg(test)]
                sketches: 0,
                no_sketch: false,
                released: false,
                #[cfg(feature = "live")]
                finished_live: false,
                stopped: false,
            }),
            recording: RwLock::new(recording),
            wake: Condvar::new(),
        });
        let work = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("tickwave-query".into())
            .spawn(move || answer(&work))
            .expect("a thread for the queries");
        Self {
            shared,
            thread: Some(thread),
        }
    }

    /// A newer snapshot of the same writer. The query thread takes it before its next query; a
    /// newer one given before then replaces it. Its error, if any, is the `failure`. Once a
    /// `reopen` has opened the finished file, a snapshot is one taken before the finish, and is
    /// dropped.
    #[cfg(feature = "live")]
    pub fn update(&self, snapshot: Snapshot) {
        let mut slot = self.shared.slot();
        if slot.finished_live {
            return;
        }
        slot.snapshot = Some(snapshot);
        drop(slot);
        self.shared.wake.notify_all();
    }

    /// Opens the file at the path again, finished: once its writer finished it, or after it was
    /// replaced by a compacted copy. The recording and its cache are new; the query under way is
    /// cancelled, and the graph asks again for the same view. A live viewer's file that its writer
    /// hasn't finished is `Error::Unfinished`: the viewer reads it again where `release` let go of
    /// it, and carries on live. On another error the viewer keeps the recording it had, let go of
    /// where `release` let go of it.
    pub fn reopen(&self) -> Result<(), Error> {
        let opened = (self.shared.open)()?;
        let bytes = Releasable::new(Arc::clone(&opened));
        let cache = Cache::new(self.shared.cache_bytes);
        let recording = match Recording::open(bytes.clone(), cache.clone()) {
            Ok(recording) => recording,
            Err(Error::Unfinished) => {
                let mut slot = self.shared.slot();
                if slot.facts.live && slot.released {
                    let live = self.shared.bytes.lock().unwrap_or_else(|e| e.into_inner());
                    live.take_back(opened);
                    slot.released = false;
                    drop(live);
                    drop(slot);
                    self.shared.wake.notify_all();
                    self.shared.ctx.request_repaint();
                }
                return Err(Error::Unfinished);
            }
            Err(e) => return Err(e),
        };
        let mut slot = self.shared.slot();
        *self.shared.bytes.lock().unwrap_or_else(|e| e.into_inner()) = bytes;
        #[cfg(test)]
        {
            *self.shared.cache.lock().unwrap_or_else(|e| e.into_inner()) = cache;
        }
        slot.replace = Some(recording);
        slot.released = false;
        #[cfg(feature = "live")]
        {
            slot.finished_live |= slot.facts.live;
        }
        #[cfg(feature = "live")]
        {
            slot.snapshot = None;
        }
        if let Some(cancel) = &slot.running {
            cancel.store(true, Ordering::Relaxed);
        }
        slot.sent = None;
        drop(slot);
        self.shared.wake.notify_all();
        self.shared.ctx.request_repaint();
        Ok(())
    }

    /// Lets go of the file, as a compaction needs before it renames a copy over it: Windows
    /// refuses to rename over a file held open. The query under way is cancelled and none runs
    /// until `reopen`, which opens the file again; meanwhile the graph draws the answer it holds,
    /// moved to the window on screen, and a snapshot given to `update` waits too. What `recording`
    /// reads of the file fails until then.
    pub fn release(&self) {
        let bytes = {
            let mut slot = self.shared.slot();
            slot.released = true;
            if let Some(cancel) = &slot.running {
                cancel.store(true, Ordering::Relaxed);
            }
            Arc::clone(&self.shared.bytes.lock().unwrap_or_else(|e| e.into_inner()))
        };
        // Waits for a read under way, then drops the opener's bytes: the file closes once
        // nothing else holds it.
        bytes.release();
    }

    /// The latest answer: the window's and the selection's statistics for a host's panel, and the
    /// marker pages its `Extras` asked for.
    pub fn answer(&self) -> Option<Arc<Answer>> {
        self.shared
            .slot()
            .held
            .as_ref()
            .map(|h| Arc::clone(&h.answer))
    }

    /// Why the last snapshot could not be taken, or else why the last query failed, until one
    /// succeeds.
    pub fn failure(&self) -> Option<String> {
        let slot = self.shared.slot();
        slot.update_failure
            .clone()
            .or_else(|| slot.query_failure.clone())
    }

    /// Reads the recording, for what it says of the whole capture: `lost`, `gaps`, `sources`,
    /// `marker_count`. It waits while the query thread takes a snapshot, never for a query.
    pub fn recording<R>(&self, read: impl FnOnce(&Recording) -> R) -> R {
        read(&self.shared.read())
    }

    /// Asks `request`, replacing the one not yet started and cancelling the one running, a
    /// snapshot's follow-up or a request.
    pub(crate) fn request(&self, request: Request) -> u64 {
        let mut slot = self.shared.slot();
        slot.issued += 1;
        if let Some(cancel) = &slot.running {
            cancel.store(true, Ordering::Relaxed);
        }
        let request = Arc::new(request);
        slot.sent = Some(Arc::clone(&request));
        slot.pending = Some(Job {
            generation: slot.issued,
            request,
            cancel: Arc::new(AtomicBool::new(false)),
            held: None,
        });
        let generation = slot.issued;
        drop(slot);
        self.shared.wake.notify_all();
        generation
    }

    /// Snapshots taken so far.
    #[cfg(test)]
    pub(crate) fn snapshots(&self) -> u64 {
        self.shared.slot().snapshots
    }

    /// Each answer held: its request's generation, and the newest then.
    #[cfg(test)]
    pub(crate) fn landed(&self) -> Vec<(u64, u64)> {
        self.shared.slot().landed.clone()
    }

    /// Answers the query thread extended.
    #[cfg(test)]
    pub(crate) fn extends(&self) -> u64 {
        self.shared.slot().extends
    }

    /// A request or a snapshot is waiting, a snapshot is being taken, or a query runs, a
    /// snapshot's follow-up included: the answer drawn may be older than what the graph asks.
    /// Once it is false, a followed window's answer is of the last snapshot given, unless
    /// `failure` says why not; a window of its own is not asked again after a snapshot.
    pub fn busy(&self) -> bool {
        let slot = self.shared.slot();
        #[cfg(feature = "live")]
        if slot.snapshot.is_some() || slot.applying {
            return true;
        }
        slot.pending.is_some() || slot.running.is_some()
    }

    /// A request is waiting or runs, not a snapshot's follow-up: what the graph waits on before
    /// it asks again, so a pan doesn't cancel its own queries.
    pub(crate) fn asking(&self) -> bool {
        let slot = self.shared.slot();
        slot.pending.is_some() || (slot.running.is_some() && !slot.following)
    }

    /// The graph's now, each frame, which a snapshot's follow-up ends a followed window at. It
    /// asks nothing.
    #[cfg(feature = "live")]
    pub(crate) fn set_now(&self, now: Option<i64>) {
        self.shared.slot().now = now;
    }

    /// The last request asked; None after a reopen, so the graph asks again.
    pub(crate) fn sent(&self) -> Option<Arc<Request>> {
        self.shared.slot().sent.clone()
    }

    /// The answer held, or before the first lands, the sketch drawn meanwhile.
    pub(crate) fn held(&self) -> Option<Held> {
        let slot = self.shared.slot();
        slot.held.clone().or_else(|| slot.sketch.clone())
    }

    /// Sketches held so far.
    #[cfg(test)]
    pub(crate) fn sketches(&self) -> u64 {
        self.shared.slot().sketches
    }

    /// Makes no sketch from now on, so the viewer holds nothing until an answer lands.
    #[cfg(test)]
    pub(crate) fn no_sketch(&self) {
        self.shared.slot().no_sketch = true;
    }

    pub(crate) fn facts(&self) -> Facts {
        self.shared.slot().facts.clone()
    }

    #[cfg(test)]
    pub(crate) fn with_opener(
        ctx: &egui::Context,
        open: Opener,
        cache_bytes: usize,
    ) -> Result<Self, Error> {
        Self::opened(ctx, open, cache_bytes)
    }

    #[cfg(all(test, feature = "live"))]
    pub(crate) fn live_with_opener(
        ctx: &egui::Context,
        open: Opener,
        snapshot: Snapshot,
        cache_bytes: usize,
    ) -> Result<Self, Error> {
        let bytes = Releasable::new(open()?);
        let cache = Cache::new(cache_bytes);
        let recording = Recording::live(snapshot, bytes.clone(), cache.clone())?;
        Ok(Self::start(
            ctx,
            open,
            bytes,
            cache,
            cache_bytes,
            recording,
            true,
        ))
    }

    /// About the bytes the recording's cache holds.
    #[cfg(test)]
    pub(crate) fn cached(&self) -> usize {
        self.shared
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .bytes()
    }

    #[cfg(test)]
    pub(crate) fn runs(&self) -> u64 {
        self.shared.slot().runs
    }

    /// A query is running.
    #[cfg(test)]
    pub(crate) fn started(&self) -> bool {
        self.shared.slot().running.is_some()
    }

    /// `release` has let go of the file, its query under way cancelled.
    #[cfg(test)]
    pub(crate) fn released(&self) -> bool {
        self.shared.slot().released
    }

    /// Requests asked so far.
    #[cfg(test)]
    pub(crate) fn issued(&self) -> u64 {
        self.shared.slot().issued
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        let mut slot = self.shared.slot();
        slot.stopped = true;
        if let Some(cancel) = &slot.running {
            cancel.store(true, Ordering::Relaxed);
        }
        drop(slot);
        self.shared.wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The query thread: a new recording or snapshot first, then the newest request. After a
/// snapshot it answers a request that follows the recording again at once, with no frame's
/// asking, extending the answer held for it.
fn answer(shared: &Shared) {
    loop {
        let (job, live) = {
            let mut slot = shared.slot();
            loop {
                if slot.stopped {
                    return;
                }
                if slot.released && slot.replace.is_none() {
                    slot = shared.wake.wait(slot).unwrap_or_else(|e| e.into_inner());
                    continue;
                }
                if let Some(recording) = slot.replace.take() {
                    drop(slot);
                    let facts = Facts::of(&recording, false);
                    *shared.write() = recording;
                    slot = shared.slot();
                    slot.facts = facts;
                    slot.update_failure = None;
                    continue;
                }
                #[cfg(feature = "live")]
                if let Some(snapshot) = slot.snapshot.take() {
                    slot.applying = true;
                    drop(slot);
                    let updated = {
                        let _span = tracing::debug_span!("snapshot").entered();
                        let mut recording = shared.write();
                        recording
                            .update(snapshot)
                            .map(|()| Facts::of(&recording, true))
                    };
                    slot = shared.slot();
                    slot.applying = false;
                    match updated {
                        Ok(facts) => {
                            slot.facts = facts;
                            #[cfg(test)]
                            {
                                slot.snapshots += 1;
                            }
                            slot.update_failure = None;
                            // A request waiting runs next, of the new snapshot.
                            if slot.pending.is_none()
                                && let Some(job) = slot.again()
                            {
                                slot.running = Some(Arc::clone(&job.cancel));
                                slot.following = true;
                                break (job, true);
                            }
                        }
                        Err(e) => slot.update_failure = Some(e.to_string()),
                    }
                    continue;
                }
                if let Some(job) = slot.pending.take() {
                    slot.running = Some(Arc::clone(&job.cancel));
                    break (job, slot.facts.live);
                }
                slot = shared.wake.wait(slot).unwrap_or_else(|e| e.into_inner());
            }
        };
        // Nothing held yet for this recording: a sketch from its coarsest levels first, which a
        // finished file's footer holds, so a frame draws while the query reads what it needs.
        let unsketched = {
            let slot = shared.slot();
            slot.held.is_none() && !slot.no_sketch
        };
        if unsketched {
            sketch(shared, &job, live);
        }
        let answered = {
            let _span = tracing::debug_span!("query job", generation = job.generation).entered();
            let recording = shared.read();
            let query = job.request.query(recording.extent().as_ref(), live);
            catch_unwind(AssertUnwindSafe(|| {
                let extended = match &job.held {
                    Some(h) => recording.extend(&h.answer, &h.query, &query, &job.cancel)?,
                    None => None,
                };
                let from_held = extended.is_some();
                match extended {
                    Some(answer) => Ok(answer),
                    None => recording.query(&query, &job.cancel),
                }
                .map(|answer| (answer, query, from_held))
            }))
        };
        let mut slot = shared.slot();
        slot.running = None;
        slot.following = false;
        #[cfg(test)]
        {
            slot.runs += 1;
            slot.extends += u64::from(matches!(answered, Ok(Ok((_, _, true)))));
        }
        let answered = answered.map(|a| a.map(|(answer, query, _)| (answer, query)));
        if slot.land(job, answered) {
            drop(slot);
            shared.ctx.request_repaint();
        }
    }
}

/// The sketch of `job`'s request, held for frames to draw until an answer lands, unless the job
/// was cancelled or a newer request issued meanwhile. Its window is resolved against the data's
/// extent, which the sketch reads without the markers.
fn sketch(shared: &Shared, job: &Job, live: bool) {
    let sketched = {
        let _span = tracing::debug_span!("sketch job", generation = job.generation).entered();
        let recording = shared.read();
        catch_unwind(AssertUnwindSafe(|| {
            let extent = recording.sketch(&Query::default(), &job.cancel)?.extent;
            let query = job.request.query(extent.as_ref(), live);
            recording
                .sketch(&query, &job.cancel)
                .map(|answer| (answer, query))
        }))
    };
    let Ok(Ok((answer, query))) = sketched else {
        return;
    };
    let mut slot = shared.slot();
    if job.cancel.load(Ordering::Relaxed) || job.generation != slot.issued || slot.held.is_some() {
        return;
    }
    slot.sketch = Some(Held {
        request: Arc::clone(&job.request),
        #[cfg(feature = "live")]
        generation: job.generation,
        query: Arc::new(query),
        answer: Arc::new(answer),
    });
    #[cfg(test)]
    {
        slot.sketches += 1;
    }
    drop(slot);
    shared.ctx.request_repaint();
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use escapement_tickwave_view::format::model::Ratio;
    use escapement_tickwave_view::{Cache, MarkerQuery, Recording};

    use super::{Facts, Held, Job, Request, Slot};
    use crate::Window;

    fn slot(issued: u64, held: Option<Held>) -> Slot {
        Slot {
            issued,
            pending: None,
            running: None,
            following: false,
            #[cfg(feature = "live")]
            applying: false,
            sent: None,
            #[cfg(feature = "live")]
            now: None,
            held,
            sketch: None,
            query_failure: None,
            update_failure: None,
            #[cfg(feature = "live")]
            snapshot: None,
            replace: None,
            facts: Facts {
                timebase: Ratio::ONE,
                channels: Arc::new([]),
                markers: false,
                live: true,
                roles: Arc::new([]),
                stored: None,
            },
            snapshots: 0,
            runs: 0,
            landed: Vec::new(),
            extends: 0,
            sketches: 0,
            no_sketch: false,
            released: false,
            #[cfg(feature = "live")]
            finished_live: false,
            stopped: false,
        }
    }

    /// An extend of the answer held for generation 1 that finishes, not cancelled, once
    /// generation 2 has been asked: what it answers is not held, and the next answer for 2 is.
    #[test]
    fn an_extend_never_overwrites_the_answer_to_a_newer_request() {
        let rec = Recording::open(
            Arc::new(crate::tests::fixture().write()),
            Cache::new(1 << 20),
        )
        .unwrap();
        let request = |window| Request {
            window,
            now: None,
            columns: 16,
            channels: vec![crate::tests::I],
            flags: Vec::new(),
            strip: None,
            fallbacks: Vec::new(),
            selection: None,
            markers: MarkerQuery::default(),
        };
        let answer = |r: &Request| {
            let q = r.query(rec.extent().as_ref(), true);
            let a = rec.query(&q, &AtomicBool::new(false)).unwrap();
            (a, q)
        };
        let follow = Arc::new(request(Window::Follow { span: 1_000_000 }));
        let (a, q) = answer(&follow);
        let held = Held {
            request: Arc::clone(&follow),
            generation: 1,
            query: Arc::new(q.clone()),
            answer: Arc::new(a.clone()),
        };
        let mut s = slot(2, Some(held.clone()));
        let extend = Job {
            generation: 1,
            request: Arc::clone(&follow),
            cancel: Arc::new(AtomicBool::new(false)),
            held: Some(held),
        };
        assert!(!s.land(extend, Ok(Ok((a, q)))));
        assert_eq!(s.held.as_ref().map(|h| h.generation), Some(1));
        assert!(s.landed.is_empty());
        let pan = Arc::new(request(Window::At(0..1_000_000)));
        let (b, r) = answer(&pan);
        let job = Job {
            generation: 2,
            request: Arc::clone(&pan),
            cancel: Arc::new(AtomicBool::new(false)),
            held: None,
        };
        assert!(s.land(job, Ok(Ok((b, r)))));
        assert_eq!(s.held.as_ref().map(|h| h.generation), Some(2));
        assert_eq!(s.landed, [(2, 2)]);
    }
}
