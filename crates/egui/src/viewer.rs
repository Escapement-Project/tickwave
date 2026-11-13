//! A recording opened for a graph, and the thread that answers the graph's queries.

use std::fs::File;
use std::io;
use std::ops::Range;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::thread::JoinHandle;

use escapement_tickwave_view::format::model::Ratio;
#[cfg(feature = "live")]
use escapement_tickwave_view::format::snapshot::Snapshot;
use escapement_tickwave_view::{
    Answer, Bytes, Cache, ChannelInfo, Error, MarkerQuery, Query, Recording,
};

use crate::state::Window;

/// Decoded data a viewer keeps: one step in on a synthetic hour at 100k pairs a second, `around`
/// decodes about 80 MB.
const CACHE_BYTES: usize = 256 << 20;

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
    pub selection: Option<Range<i64>>,
    pub markers: MarkerQuery,
}

impl Request {
    fn query(&self, extent: Option<&Range<i64>>) -> Query {
        let window = self.window.resolve(extent, self.now);
        Query {
            statistics: std::iter::once(window.clone())
                .chain(self.selection.clone())
                .collect(),
            window,
            columns: self.columns,
            channels: self.channels.clone(),
            flags: self.flags.clone(),
            strip: self.strip,
            markers: self.markers.clone(),
        }
    }
}

/// The latest answer, and the request it answers.
#[derive(Clone, Debug)]
pub(crate) struct Held {
    pub request: Arc<Request>,
    pub answer: Arc<Answer>,
}

/// What a frame reads of the recording without taking its lock.
#[derive(Clone, Debug)]
pub(crate) struct Facts {
    pub timebase: Ratio,
    pub channels: Arc<[ChannelInfo]>,
    pub marker_count: usize,
    /// Made from a writer's snapshot, and not reopened since.
    pub live: bool,
}

impl Facts {
    fn of(r: &Recording, live: bool) -> Self {
        Self {
            timebase: r.timebase(),
            channels: r.channels().into(),
            marker_count: r.marker_count(),
            live,
        }
    }
}

struct Job {
    generation: u64,
    request: Arc<Request>,
    cancel: Arc<AtomicBool>,
}

struct Slot {
    /// The generation of the newest request: an answer to an older one is never held.
    issued: u64,
    /// The newest request not yet started; a newer one replaces it.
    pending: Option<Job>,
    running: Option<Arc<AtomicBool>>,
    sent: Option<Arc<Request>>,
    held: Option<Held>,
    query_failure: Option<String>,
    update_failure: Option<String>,
    #[cfg(feature = "live")]
    snapshot: Option<Snapshot>,
    replace: Option<Recording>,
    facts: Facts,
    /// Snapshots taken, so a graph following the recording can tell when to ask again.
    snapshots: u64,
    /// Queries run to the end or cancelled, for the tests.
    #[cfg(test)]
    runs: u64,
    stopped: bool,
}

type Opener = Box<dyn Fn() -> io::Result<Arc<dyn Bytes>> + Send + Sync>;

struct Shared {
    ctx: egui::Context,
    open: Opener,
    cache_bytes: usize,
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
/// is kept, and its arrival asks egui for a frame.
pub struct Viewer {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

fn file(path: &Path) -> Opener {
    let path: PathBuf = path.to_owned();
    Box::new(move || Ok(Arc::new(File::open(&path)?) as Arc<dyn Bytes>))
}

impl Viewer {
    /// The finished recording at `path`. `Error::Unfinished` for one with no footer, which the
    /// format crate's Recutter repairs.
    pub fn open(ctx: &egui::Context, path: &Path) -> Result<Self, Error> {
        Self::opened(ctx, file(path), CACHE_BYTES)
    }

    fn opened(ctx: &egui::Context, open: Opener, cache_bytes: usize) -> Result<Self, Error> {
        let recording = Recording::open(open()?, Cache::new(cache_bytes))?;
        Ok(Self::start(ctx, open, cache_bytes, recording, false))
    }

    /// The recording at `path` that a writer in this process is writing. `snapshot` is the
    /// writer's, taken after the bytes it covers are in the file; give each newer one to `update`.
    #[cfg(feature = "live")]
    pub fn live(ctx: &egui::Context, path: &Path, snapshot: Snapshot) -> Result<Self, Error> {
        let open = file(path);
        let recording = Recording::live(snapshot, open()?, Cache::new(CACHE_BYTES))?;
        Ok(Self::start(ctx, open, CACHE_BYTES, recording, true))
    }

    fn start(
        ctx: &egui::Context,
        open: Opener,
        cache_bytes: usize,
        recording: Recording,
        live: bool,
    ) -> Self {
        let shared = Arc::new(Shared {
            ctx: ctx.clone(),
            open,
            cache_bytes,
            slot: Mutex::new(Slot {
                issued: 0,
                pending: None,
                running: None,
                sent: None,
                held: None,
                query_failure: None,
                update_failure: None,
                #[cfg(feature = "live")]
                snapshot: None,
                replace: None,
                facts: Facts::of(&recording, live),
                snapshots: 0,
                #[cfg(test)]
                runs: 0,
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
    /// newer one given before then replaces it. Its error, if any, is the `failure`.
    #[cfg(feature = "live")]
    pub fn update(&self, snapshot: Snapshot) {
        self.shared.slot().snapshot = Some(snapshot);
        self.shared.wake.notify_all();
    }

    /// Opens the file at the path again, finished: once its writer finished it, or after it was
    /// replaced by a compacted copy. The recording and its cache are new; the query under way is
    /// cancelled, and the graph asks again for the same view. On an error the viewer keeps the
    /// recording it had.
    pub fn reopen(&self) -> Result<(), Error> {
        let recording =
            Recording::open((self.shared.open)()?, Cache::new(self.shared.cache_bytes))?;
        let mut slot = self.shared.slot();
        slot.replace = Some(recording);
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

    /// Asks `request`, replacing the one not yet started and cancelling the one running.
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
        });
        let generation = slot.issued;
        drop(slot);
        self.shared.wake.notify_all();
        generation
    }

    /// Snapshots taken so far.
    pub(crate) fn snapshots(&self) -> u64 {
        self.shared.slot().snapshots
    }

    /// A query is waiting or running.
    pub(crate) fn busy(&self) -> bool {
        let slot = self.shared.slot();
        slot.pending.is_some() || slot.running.is_some()
    }

    /// The last request asked; None after a reopen, so the graph asks again.
    pub(crate) fn sent(&self) -> Option<Arc<Request>> {
        self.shared.slot().sent.clone()
    }

    pub(crate) fn held(&self) -> Option<Held> {
        self.shared.slot().held.clone()
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
        let recording = Recording::live(snapshot, open()?, Cache::new(cache_bytes))?;
        Ok(Self::start(ctx, open, cache_bytes, recording, true))
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

/// The query thread: a new recording or snapshot first, then the newest request.
fn answer(shared: &Shared) {
    loop {
        let job = {
            let mut slot = shared.slot();
            loop {
                if slot.stopped {
                    return;
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
                    drop(slot);
                    let updated = {
                        let _span = tracing::debug_span!("snapshot").entered();
                        let mut recording = shared.write();
                        recording
                            .update(snapshot)
                            .map(|()| Facts::of(&recording, true))
                    };
                    slot = shared.slot();
                    match updated {
                        Ok(facts) => {
                            slot.facts = facts;
                            slot.snapshots += 1;
                            slot.update_failure = None;
                        }
                        Err(e) => slot.update_failure = Some(e.to_string()),
                    }
                    continue;
                }
                if let Some(job) = slot.pending.take() {
                    slot.running = Some(Arc::clone(&job.cancel));
                    break job;
                }
                slot = shared.wake.wait(slot).unwrap_or_else(|e| e.into_inner());
            }
        };
        let answered = {
            let _span = tracing::debug_span!("query job", generation = job.generation).entered();
            let recording = shared.read();
            let query = job.request.query(recording.extent().as_ref());
            catch_unwind(AssertUnwindSafe(|| recording.query(&query, &job.cancel)))
        };
        let mut slot = shared.slot();
        slot.running = None;
        #[cfg(test)]
        {
            slot.runs += 1;
        }
        if job.cancel.load(Ordering::Relaxed) || job.generation != slot.issued {
            continue;
        }
        match answered {
            Ok(Ok(answer)) => {
                slot.held = Some(Held {
                    request: job.request,
                    answer: Arc::new(answer),
                });
                slot.query_failure = None;
            }
            Ok(Err(e)) => slot.query_failure = Some(e.to_string()),
            Err(_) => slot.query_failure = Some("the query stopped on a fault".into()),
        }
        drop(slot);
        shared.ctx.request_repaint();
    }
}
