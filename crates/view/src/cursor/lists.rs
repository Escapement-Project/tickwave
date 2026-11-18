//! A channel's gaps or breaks as a cursor reads them, a window at a time: from a finished file's
//! list pages, from a list the footer holds whole, or from a live recording's writer.

use std::sync::atomic::AtomicBool;

use escapement_tickwave_format::decode::List;
use escapement_tickwave_format::index::PAGE;
use escapement_tickwave_format::mapping::Mapping;
use escapement_tickwave_format::model::{Break, Gap};

use super::corrupt;
use crate::cache::Reading;
#[cfg(feature = "live")]
use crate::recording::Place;
use crate::recording::{Error, Finished, Recording, Store};

/// The most entries a window copies from a list held whole: a list page's.
pub(super) const WINDOW: usize = PAGE;

/// Where the entries after a window come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum More {
    /// The window holds the list's last entry.
    Done,
    /// Page `j` of a finished file's list, whose first entry's key is `key`.
    Page { key: i64, j: u64 },
    /// Entry `i` on, of a list held whole or a live recording's, which can grow.
    Entry(u64),
}

impl More {
    fn after(next: Option<(i64, u64)>) -> Self {
        next.map_or(More::Done, |(key, j)| More::Page { key, j })
    }
}

/// Entries of a list in order, whether the first is the list's first, and what follows them.
pub(super) struct Window<T> {
    pub list: Vec<T>,
    pub first: bool,
    pub more: More,
}

/// A finished file whose channel `c`'s list is in pages not read whole.
fn paged(rec: &Recording, c: usize) -> Option<&Finished> {
    rec.finished().filter(|_| rec.paged(c))
}

/// Entries `i..` of a list held whole, at most a window's.
fn window_of<T: Copy>(list: &[T], i: u64, live: bool) -> Window<T> {
    let from = usize::try_from(i).unwrap_or(usize::MAX).min(list.len());
    let to = from.saturating_add(WINDOW).min(list.len());
    let more = if live || to < list.len() {
        More::Entry(to as u64)
    } else {
        More::Done
    };
    Window {
        list: list[from..to].to_vec(),
        first: from == 0,
        more,
    }
}

/// Page `j` of finished channel `c`'s list, which starts at key `key`, and the page after it.
fn page(
    rec: &Recording,
    f: &Finished,
    c: usize,
    key: i64,
    j: u64,
    cancel: &AtomicBool,
) -> Result<(List, More), Error> {
    let table = f
        .footer()
        .channels
        .get(c)
        .and_then(|ch| ch.list.clone())
        .ok_or(Error::NoChannel(c))?;
    let r = Reading::new(cancel);
    let at = rec
        .pages(&r, &table, c, key..key.saturating_add(1))?
        .into_iter()
        .find(|at| at.j == j)
        .ok_or_else(|| {
            corrupt(format!(
                "channel {c}: list page {j} doesn't start where the page before says"
            ))
        })?;
    let list = rec.list_page(&r, f, &table, c, &at)?;
    let next = at.next.and_then(|n| Some((n.0, j.checked_add(1)?)));
    Ok(((*list).clone(), More::after(next)))
}

/// The gaps a cursor seeking axis tick `t` of digital channel `c` needs: from the last that
/// starts before `t`, or from the first where none does.
pub(super) fn gaps_at(
    rec: &Recording,
    c: usize,
    t: i64,
    map: &Mapping,
    cancel: &AtomicBool,
) -> Result<Window<Gap>, Error> {
    if let Some(f) = paged(rec, c) {
        let slice = rec.list_over(&Reading::new(cancel), f, c, t..t.saturating_add(1))?;
        let List::Gaps(list) = slice.list else {
            return Err(Error::NoChannel(c));
        };
        return Ok(Window {
            list,
            first: slice.first,
            more: More::after(slice.next),
        });
    }
    match &rec.store {
        Store::Finished(f) => {
            let footer = f.footer();
            let gaps = footer.gaps(c).unwrap_or_default();
            let p = gaps.partition_point(|g| map.axis_saturating(g.from) < t);
            Ok(window_of(gaps, p.saturating_sub(1) as u64, false))
        }
        #[cfg(feature = "live")]
        Store::Live(live) => {
            // The place holds every gap on the axis, in the writer's order.
            let Some(Place::Digital { leading, gaps, .. }) = rec.places.get(c) else {
                return Err(Error::NoChannel(c));
            };
            let p = match leading {
                Some(g) if g.from >= t => 0,
                Some(_) => 1 + gaps.partition_point(|g| g.from < t),
                None => gaps.partition_point(|g| g.from < t),
            };
            live_gaps(rec, live, c, p.saturating_sub(1) as u64)
        }
    }
}

/// The gaps after a window, as `more` says.
pub(super) fn gaps_after(
    rec: &Recording,
    c: usize,
    more: More,
    cancel: &AtomicBool,
) -> Result<Window<Gap>, Error> {
    match (more, &rec.store) {
        (More::Done, _) => Ok(Window {
            list: Vec::new(),
            first: false,
            more,
        }),
        (More::Page { key, j }, Store::Finished(f)) => match page(rec, f, c, key, j, cancel)? {
            (List::Gaps(list), more) => Ok(Window {
                list,
                first: j == 0,
                more,
            }),
            (List::Breaks(_), _) => Err(Error::NoChannel(c)),
        },
        (More::Entry(i), Store::Finished(f)) => {
            Ok(window_of(f.footer().gaps(c).unwrap_or_default(), i, false))
        }
        #[cfg(feature = "live")]
        (More::Entry(i), Store::Live(live)) => live_gaps(rec, live, c, i),
        #[cfg(feature = "live")]
        (More::Page { .. }, Store::Live(_)) => Err(Error::NoChannel(c)),
    }
}

/// What analog channel `c`'s seek of axis tick `t` takes of its breaks: from a finished file's
/// list pages, the slice `t` needs, read for `search`; else the list held whole, handed to `f`.
pub(super) enum BreaksAt {
    Slice(Window<Break>),
    Whole,
}

pub(super) fn breaks_at(
    rec: &Recording,
    c: usize,
    t: i64,
    cancel: &AtomicBool,
) -> Result<BreaksAt, Error> {
    let Some(f) = paged(rec, c) else {
        return Ok(BreaksAt::Whole);
    };
    let slice = rec.list_over(&Reading::new(cancel), f, c, t..t.saturating_add(1))?;
    let List::Breaks(list) = slice.list else {
        return Err(Error::NoChannel(c));
    };
    Ok(BreaksAt::Slice(Window {
        list,
        first: slice.first,
        more: More::after(slice.next),
    }))
}

/// Analog channel `c`'s breaks held whole, and whether the list can grow: a finished file's
/// footer's where it holds them, a live recording's place's.
pub(super) fn whole_breaks<R>(
    rec: &Recording,
    c: usize,
    f: impl FnOnce(&[Break], bool) -> R,
) -> Result<R, Error> {
    match &rec.store {
        Store::Finished(fin) => {
            let footer = fin.footer();
            let breaks = footer.breaks(c).ok_or(Error::NoChannel(c))?;
            Ok(f(breaks, false))
        }
        #[cfg(feature = "live")]
        Store::Live(_) => match rec.places.get(c) {
            Some(Place::Analog { breaks, .. }) => Ok(f(breaks, true)),
            _ => Err(Error::NoChannel(c)),
        },
    }
}

/// The breaks after a window, as `more` says.
pub(super) fn breaks_after(
    rec: &Recording,
    c: usize,
    more: More,
    cancel: &AtomicBool,
) -> Result<Window<Break>, Error> {
    match (more, &rec.store) {
        (More::Done, _) => Ok(Window {
            list: Vec::new(),
            first: false,
            more,
        }),
        (More::Page { key, j }, Store::Finished(f)) => match page(rec, f, c, key, j, cancel)? {
            (List::Breaks(list), more) => Ok(Window {
                list,
                first: j == 0,
                more,
            }),
            (List::Gaps(_), _) => Err(Error::NoChannel(c)),
        },
        (More::Entry(i), _) => whole_breaks(rec, c, |breaks, live| window_of(breaks, i, live)),
        #[cfg(feature = "live")]
        (More::Page { .. }, Store::Live(_)) => Err(Error::NoChannel(c)),
    }
}

/// Entries `i..` of live digital channel `c`'s gaps, at most a page's: from the one list page in
/// the file that holds entry `i`, or from the entries no page holds yet.
#[cfg(feature = "live")]
fn live_gaps(
    rec: &Recording,
    live: &crate::recording::Live,
    c: usize,
    i: u64,
) -> Result<Window<Gap>, Error> {
    use escapement_tickwave_format::index::Page;
    let list = &live.snapshot.channel(c).ok_or(Error::NoChannel(c))?.gaps;
    let paged = list.table.as_ref().map_or(0, |t| t.rows);
    let mut out = Vec::new();
    if i < paged {
        let table = list.table.as_ref().ok_or(Error::NoChannel(c))?;
        let mut fetch = |offset: u64, len: u32| -> Result<Vec<u8>, Error> {
            Ok(crate::bytes::read(&*rec.bytes, offset, len as usize)?)
        };
        let j = i / PAGE as u64;
        let entries = table.pages(0, &mut fetch)?;
        let e = usize::try_from(j)
            .ok()
            .and_then(|j| entries.get(j))
            .ok_or_else(|| corrupt(format!("channel {c}: no list page holds gap {i}")))?;
        let bytes = fetch(e.offset, e.length)?;
        let Page::Gaps(gaps) = table.parse_page(0, j, e, &bytes)? else {
            return Err(corrupt(format!("channel {c}: a gap page holds no gaps")));
        };
        let skip = usize::try_from(i - j * PAGE as u64).unwrap_or(usize::MAX);
        out.extend(gaps.iter().skip(skip).copied());
    } else {
        let k = usize::try_from(i - paged).unwrap_or(usize::MAX);
        out.extend(list.open.iter_from(k).take(WINDOW).copied());
        let w = k.saturating_sub(list.open.len());
        let room = WINDOW - out.len();
        out.extend(list.waiting.iter().skip(w).take(room).copied());
    }
    Ok(Window {
        first: i == 0,
        more: More::Entry(i + out.len() as u64),
        list: out,
    })
}

/// Live digital channel `c`'s last gap.
#[cfg(feature = "live")]
pub(super) fn live_last_gap(
    rec: &Recording,
    live: &crate::recording::Live,
    c: usize,
) -> Result<Option<Gap>, Error> {
    let list = &live.snapshot.channel(c).ok_or(Error::NoChannel(c))?.gaps;
    if let Some(g) = list.waiting.last().or_else(|| list.open.last()) {
        return Ok(Some(*g));
    }
    match list.table.as_ref().map_or(0, |t| t.rows) {
        0 => Ok(None),
        n => Ok(live_gaps(rec, live, c, n - 1)?.list.first().copied()),
    }
}
