//! A wire analysis's input from a recording: the channels bound to its ports, read through the
//! view's exact cursors and merged in exact order on their source's timeline. Blocks come through
//! the recording's cache, so the waveform and a decoder share what either decoded.

use crate::{
    host::{Observed, Source, SourceError, Window},
    output::Tick,
};
use escapement_tickwave_view::{
    Error, Recording,
    cursor::{Budget, DigitalCursor, Groups, Merge, Seen, Until},
};
use std::sync::{Arc, atomic::AtomicBool};

/// Why channels cannot be bound to a wire analysis's ports.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BindWire {
    #[error("{0}")]
    Read(#[from] Error),
    /// A decoder reads one source's timeline; ports on several sources need exact axis time.
    #[error("channels {0} and {1} come from different sources")]
    Sources(usize, usize),
    #[error("more channels than a window numbers ports")]
    TooMany,
}

/// Channels of one source as a wire analysis's ports, in port order.
pub struct WireSource {
    recording: Arc<Recording>,
    channels: Vec<usize>,
    merge: Option<Merge>,
    /// Where the merge stands: a window that starts elsewhere seeks.
    at: Tick,
    /// Where the first channel to finish ends, on the timeline; None while live.
    end: Option<Tick>,
    groups: Groups,
    pub seeks: u64,
    pub reads: u64,
}

impl WireSource {
    pub fn new(recording: Arc<Recording>, channels: Vec<usize>) -> Result<Self, BindWire> {
        if channels.len() > usize::from(u8::MAX) + 1 {
            return Err(BindWire::TooMany);
        }
        let never = AtomicBool::new(false);
        let mut end: Option<Tick> = None;
        let mut first: Option<(usize, u32)> = None;
        for &c in &channels {
            let cur = DigitalCursor::seek(&recording, c, 0, &Budget::LARGE, &never)?;
            match first {
                Some((f, source)) if source != cur.source() => {
                    return Err(BindWire::Sources(f, c));
                }
                None => first = Some((c, cur.source())),
                _ => {}
            }
            if let Some(e) = cur.end() {
                end = Some(end.map_or(e, |x| x.min(e)));
            }
        }
        Ok(Self {
            recording,
            channels,
            merge: None,
            at: Tick::MIN,
            end,
            groups: Groups::new(),
            seeks: 0,
            reads: 0,
        })
    }

    /// Where its input ends, for a finished recording.
    pub fn end(&self) -> Option<Tick> {
        self.end
    }
}

fn read<T>(r: Result<T, Error>) -> Result<Option<T>, SourceError> {
    match r {
        Ok(t) => Ok(Some(t)),
        Err(Error::Cancelled) => Ok(None),
        Err(e) => Err(SourceError::new(e)),
    }
}

impl Source for WireSource {
    fn window(
        &mut self,
        from: Tick,
        upto: Tick,
        cancel: &AtomicBool,
    ) -> Result<Option<Window>, SourceError> {
        let rec = &*self.recording;
        if self.merge.is_none() || self.at != from {
            let mut cursors = Vec::with_capacity(self.channels.len());
            for &c in &self.channels {
                let cur = DigitalCursor::seek_timeline(rec, c, from, &Budget::LARGE, cancel);
                let Some(cur) = read(cur)? else {
                    return Ok(None);
                };
                cursors.push(cur);
            }
            self.merge = Some(Merge::new(cursors));
            self.at = from;
            self.seeks += 1;
        }
        let Some(merge) = self.merge.as_mut() else {
            return Ok(None);
        };
        // Nothing after the first channel's end: the analysis's input ends there.
        let limit = self.end.map_or(upto, |e| upto.min(e.saturating_add(1)));
        let mut w = Window::new(from, upto);
        for p in 0..merge.ports() {
            let level = merge.level(p);
            w.events.push(Observed::Start {
                port: p as u8,
                level,
            });
        }
        self.groups.clear();
        let b = merge.next_in(
            rec,
            Until::Source(limit),
            &Budget::LARGE,
            cancel,
            &mut self.groups,
        );
        let Some(b) = read(b)? else {
            return Ok(None);
        };
        self.reads += 1;
        for g in self.groups.iter() {
            let Some(at) = merge.timeline(&g) else {
                continue;
            };
            for &(port, seen) in g.seen {
                let port = port as u8;
                w.events.push(match seen {
                    Seen::Edge(e) => Observed::Edge {
                        at,
                        port,
                        level: e.level,
                    },
                    Seen::Known { level, .. } => Observed::Known { at, port, level },
                    Seen::Unknown { .. } => Observed::Unknown { at, port },
                    _ => continue,
                });
            }
        }
        // A live recording settled short of the window: the window ends where it is.
        if b.reached < limit {
            w.upto = b.reached.max(from);
        }
        self.at = w.upto;
        w.end = self.end.filter(|&e| e < upto);
        Ok(Some(w))
    }
}
