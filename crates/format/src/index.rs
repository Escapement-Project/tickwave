//! The index (FORMAT.md, "Index"): each channel's blocks, each level of its overview in overview
//! blocks, and each long gap or break list is a table, its entries in pages of 4,096, the pages
//! listed in directory pages once there are more than 16, and the footer's top index listing at
//! most 16 entries a table. Pages are records: a writer writes one right after the record of its
//! last entry, a directory right after its last page, so a reader in order meets each where the
//! writer wrote it.

use std::ops::{ControlFlow, Range};

use crate::header::{BlockRow, JSON_SAFE, LEVEL_BIT, OverviewRow};
use crate::model::{Break, Gap};
use crate::parts::PartRow;
use crate::record::{self, Kind};
use crate::shared::Chunks;

/// Entries a page holds, and directory entries a directory page.
pub const PAGE: usize = 4096;
/// The most entries the top index holds for one table.
pub const TOP: usize = 16;
/// A page record's payload before its entries.
pub const PAGE_HEAD: usize = 19;

/// A directory entry, 28 bytes, and a top-index entry: where a page starts, and its record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    /// For blocks, the first block's start tick; for an overview level, the first row's `k`;
    /// for gaps, the first gap's `from`; for breaks, the first break's `tick`; for annotation
    /// parts, the earliest `start` of the rows under it.
    pub first: i64,
    /// For blocks, the first block's first item's index in the channel; for an overview level
    /// and for gaps, the first entry's place; for breaks, the first break's `index`; for
    /// annotation parts, the latest `end` of the rows under it.
    pub number: u64,
    /// The page record's offset in the file, and its length, its framing included.
    pub offset: u64,
    pub length: u32,
}

impl Entry {
    pub const SIZE: usize = 28;

    pub fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.first.to_le_bytes());
        out.extend_from_slice(&self.number.to_le_bytes());
        out.extend_from_slice(&self.offset.to_le_bytes());
        out.extend_from_slice(&self.length.to_le_bytes());
    }

    /// `bytes` is exactly one entry.
    pub fn parse(bytes: &[u8]) -> Self {
        Self {
            first: i64::from_le_bytes(bytes[0..8].try_into().unwrap()),
            number: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            offset: u64::from_le_bytes(bytes[16..24].try_into().unwrap()),
            length: u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
        }
    }
}

/// Which table a page belongs to: its code in a page's head, and an overview level's shift.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Table {
    Blocks,
    Level(u8),
    Gaps,
    Breaks,
    /// The recording's annotation parts, channel 0.
    Parts,
}

impl Table {
    pub fn code(self) -> u8 {
        match self {
            Self::Blocks => 0,
            Self::Level(_) => 1,
            Self::Gaps => 2,
            Self::Breaks => 3,
            Self::Parts => 4,
        }
    }

    pub fn shift(self) -> u8 {
        match self {
            Self::Level(s) => s,
            _ => 0,
        }
    }

    /// The record kind of its pages at `depth`: a list's own pages are list pages, every other
    /// page and every directory an index page.
    pub fn kind(self, depth: u8) -> Kind {
        match self {
            Self::Gaps | Self::Breaks if depth == 0 => Kind::ListPage,
            _ => Kind::IndexPage,
        }
    }

    /// The bytes one of its entries takes at depth 0.
    pub fn row_size(self) -> usize {
        match self {
            Self::Blocks => BlockRow::SIZE,
            Self::Level(_) => OverviewRow::SIZE,
            Self::Gaps => GAP_SIZE,
            Self::Breaks => BREAK_SIZE,
            Self::Parts => PartRow::SIZE,
        }
    }

    /// Whether its keys are spans, not places: the annotation parts' rows go in the order the
    /// parts were written, not in order of time, so a directory entry's `first` and `number` are
    /// the earliest start and latest end under it, and a reader prunes by span.
    pub fn spans(self) -> bool {
        self == Self::Parts
    }
}

const GAP_SIZE: usize = 17;
const BREAK_SIZE: usize = 28;

/// An entry of a table at depth 0.
pub trait Row: Copy + std::fmt::Debug + Send + Sync + 'static {
    fn write(&self, out: &mut Vec<u8>);
    /// What the entry adds to the count a directory's `number` carries: a block's items, an
    /// overview row's blocks. `digital` says how a block's word counts.
    fn weight(&self, digital: bool) -> u64;
    /// Its `first` and `number`, `sum` being the weights before it and `place` its place.
    fn key(&self, sum: u64, place: u64) -> (i64, u64);
}

/// The `first` and `number` of entries whose keys are spans (`Table::spans`): the earliest start
/// and the latest end.
fn span_key(a: (i64, u64), b: (i64, u64)) -> (i64, u64) {
    (a.0.min(b.0), a.1.max(b.1))
}

impl Row for BlockRow {
    fn write(&self, out: &mut Vec<u8>) {
        BlockRow::write(self, out)
    }
    fn weight(&self, digital: bool) -> u64 {
        u64::from(if digital {
            self.word & !LEVEL_BIT
        } else {
            self.word
        })
    }
    fn key(&self, sum: u64, _: u64) -> (i64, u64) {
        (self.start, sum)
    }
}

impl Row for OverviewRow {
    fn write(&self, out: &mut Vec<u8>) {
        OverviewRow::write(self, out)
    }
    fn weight(&self, _: bool) -> u64 {
        u64::from(self.n)
    }
    fn key(&self, sum: u64, place: u64) -> (i64, u64) {
        (sum as i64, place)
    }
}

impl Row for Gap {
    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.from.to_le_bytes());
        out.extend_from_slice(&self.to.to_le_bytes());
        out.push(u8::from(self.level));
    }
    fn weight(&self, _: bool) -> u64 {
        1
    }
    fn key(&self, _: u64, place: u64) -> (i64, u64) {
        (self.from, place)
    }
}

impl Row for PartRow {
    fn write(&self, out: &mut Vec<u8>) {
        PartRow::write(self, out)
    }
    fn weight(&self, _: bool) -> u64 {
        1
    }
    fn key(&self, _: u64, _: u64) -> (i64, u64) {
        (self.start, self.end as u64)
    }
}

impl Row for Break {
    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.index.to_le_bytes());
        out.extend_from_slice(&self.tick.to_le_bytes());
        out.extend_from_slice(&self.lost.to_le_bytes());
        out.extend_from_slice(&self.phase.to_le_bytes());
    }
    fn weight(&self, _: bool) -> u64 {
        1
    }
    fn key(&self, _: u64, _: u64) -> (i64, u64) {
        (self.tick, self.index)
    }
}

/// The pages a table of `rows` entries has at `depth`: `ceil(rows / 4,096)` at depth 0, and
/// `ceil(pages / 4,096)` of the depth below at each depth above.
pub fn pages_at(rows: u64, depth: u8) -> u64 {
    let mut n = rows.div_ceil(PAGE as u64);
    for _ in 0..depth {
        n = n.div_ceil(PAGE as u64);
    }
    n
}

/// The depth a table of `rows` entries has (FORMAT.md, "Index"): the least at which its pages
/// number at most 16, which the top index lists.
pub fn depth_of(rows: u64) -> u8 {
    let mut depth = 0;
    while pages_at(rows, depth) > TOP as u64 {
        depth += 1;
    }
    depth
}

/// A page's head, as its record has it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageHead {
    pub channel: u32,
    pub table: u8,
    pub shift: u8,
    pub depth: u8,
    /// Its first entry's place among the entries at its depth.
    pub place: u64,
    pub count: u32,
}

impl PageHead {
    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.channel.to_le_bytes());
        out.push(self.table);
        out.push(self.shift);
        out.push(self.depth);
        out.extend_from_slice(&self.place.to_le_bytes());
        out.extend_from_slice(&self.count.to_le_bytes());
    }

    /// None if the payload is shorter than a head.
    pub fn parse(payload: &[u8]) -> Option<(Self, &[u8])> {
        let head = payload.get(..PAGE_HEAD)?;
        Some((
            Self {
                channel: u32::from_le_bytes(head[0..4].try_into().unwrap()),
                table: head[4],
                shift: head[5],
                depth: head[6],
                place: u64::from_le_bytes(head[7..15].try_into().unwrap()),
                count: u32::from_le_bytes(head[15..19].try_into().unwrap()),
            },
            &payload[PAGE_HEAD..],
        ))
    }
}

/// A page record whose place in the file is not known yet, and the entry a directory will hold
/// for it once it is.
#[derive(Debug, PartialEq, Eq)]
pub struct Due {
    pub depth: u8,
    pub record: Vec<u8>,
    first: i64,
    number: u64,
}

/// One table as a writer builds it, and as a reader in order follows it: its entries come in
/// order, each once its record has a place, and it says which page records follow from them.
/// It holds the open page's entries and, at each depth, the entries of the pages written that no
/// directory lists yet: at most 4,095 each, however long the table.
#[derive(Clone, Debug)]
pub struct TableWriter<R: Row> {
    channel: u32,
    table: Table,
    digital: bool,
    /// Entries taken.
    count: u64,
    /// Their weights, summed, and those of the open page's alone.
    sum: u64,
    open_sum: u64,
    /// The open page: entries no page holds yet, from entry `count - open.len()`.
    open: Chunks<R>,
    /// The open page's first entry's `first` and `number`.
    open_key: (i64, u64),
    /// At each depth, the pages written that no directory lists.
    pending: Vec<Chunks<Entry>>,
    /// At each depth, the pages written.
    pages: Vec<u64>,
    finished: bool,
}

impl<R: Row> TableWriter<R> {
    pub fn new(channel: u32, table: Table, digital: bool) -> Self {
        Self {
            channel,
            table,
            digital,
            count: 0,
            sum: 0,
            open_sum: 0,
            open: Chunks::new(),
            open_key: (0, 0),
            pending: Vec::new(),
            pages: Vec::new(),
            finished: false,
        }
    }

    pub fn table(&self) -> Table {
        self.table
    }

    /// Entries taken so far.
    pub fn count(&self) -> u64 {
        self.count
    }

    /// The entries' weights so far: a block table's items, an overview level's blocks.
    pub fn sum(&self) -> u64 {
        self.sum
    }

    /// The entries no page holds yet.
    pub fn open(&self) -> &Chunks<R> {
        &self.open
    }

    /// The entries in pages written: every entry before the open page's.
    pub fn paged(&self) -> u64 {
        self.count - self.open.len() as u64
    }

    /// The weights of the entries in pages written: the open page's first entry's items before
    /// it, or its first block.
    pub fn paged_sum(&self) -> u64 {
        self.sum - self.open_sum
    }

    /// The entries of the pages written that no directory lists, depth by depth.
    pub fn pending(&self) -> &[Chunks<Entry>] {
        &self.pending
    }

    /// The next entry. When it fills a page, the page record that follows it, for the caller to
    /// place and pass to `placed`.
    pub fn push(&mut self, row: R) -> Option<Due> {
        debug_assert!(!self.finished, "an entry after the table's last page");
        let key = row.key(self.sum, self.count);
        if self.open.is_empty() {
            self.open_key = key;
        } else if self.table.spans() {
            self.open_key = span_key(self.open_key, key);
        }
        self.count += 1;
        let w = row.weight(self.digital);
        self.sum += w;
        self.open_sum += w;
        self.open.push(row);
        (self.open.len() == PAGE).then(|| self.page())
    }

    /// The open page as a record, its entries handed over.
    fn page(&mut self) -> Due {
        let rows = std::mem::take(&mut self.open);
        self.open_sum = 0;
        let n = rows.len();
        let mut record =
            Vec::with_capacity(record::FRAMING + PAGE_HEAD + n * self.table.row_size());
        let mut payload = Vec::with_capacity(PAGE_HEAD + n * self.table.row_size());
        PageHead {
            channel: self.channel,
            table: self.table.code(),
            shift: self.table.shift(),
            depth: 0,
            place: self.count - n as u64,
            count: n as u32,
        }
        .write(&mut payload);
        for row in rows.iter() {
            row.write(&mut payload);
        }
        record::write(&mut record, self.table.kind(0), &[&payload]);
        Due {
            depth: 0,
            record,
            first: self.open_key.0,
            number: self.open_key.1,
        }
    }

    /// The directory page of depth `depth + 1` that lists the pending pages of `depth`.
    fn directory(&mut self, depth: u8) -> Due {
        let d = usize::from(depth);
        let entries = std::mem::take(&mut self.pending[d]);
        let n = entries.len();
        let mut first = *entries.get(0).expect("a directory lists a page");
        if self.table.spans() {
            (first.first, first.number) =
                entries.iter().fold((first.first, first.number), |k, e| {
                    span_key(k, (e.first, e.number))
                });
        }
        let listed_before = self.pages[d] - n as u64;
        let mut payload = Vec::with_capacity(PAGE_HEAD + n * Entry::SIZE);
        PageHead {
            channel: self.channel,
            table: self.table.code(),
            shift: self.table.shift(),
            depth: depth + 1,
            place: listed_before,
            count: n as u32,
        }
        .write(&mut payload);
        for e in entries.iter() {
            e.write(&mut payload);
        }
        let mut record = Vec::with_capacity(record::FRAMING + payload.len());
        record::write(&mut record, self.table.kind(depth + 1), &[&payload]);
        Due {
            depth: depth + 1,
            record,
            first: first.first,
            number: first.number,
        }
    }

    /// `due` is in the file at `offset`. When it fills a directory page, the directory record
    /// that follows it.
    pub fn placed(&mut self, due: &Due, offset: u64) -> Option<Due> {
        let d = usize::from(due.depth);
        if self.pending.len() <= d {
            self.pending.resize_with(d + 1, Chunks::new);
            self.pages.resize(d + 1, 0);
        }
        self.pending[d].push(Entry {
            first: due.first,
            number: due.number,
            offset,
            length: due.record.len() as u32,
        });
        self.pages[d] += 1;
        (!self.finished && self.pending[d].len() == PAGE).then(|| self.directory(due.depth))
    }

    /// Drops the open page's last `n` entries: a gap list's that its channel's end leaves out.
    /// None if fewer are open, so some are in a page written already.
    pub fn drop_last(&mut self, n: u64) -> Option<()> {
        if n > self.open.len() as u64 {
            return None;
        }
        let mut kept = Chunks::new();
        let keep = self.open.len() - n as usize;
        for row in self.open.iter().take(keep) {
            kept.push(*row);
        }
        for row in self.open.iter().skip(keep) {
            let w = row.weight(self.digital);
            self.sum -= w;
            self.open_sum -= w;
        }
        self.open = kept;
        self.count -= n;
        Some(())
    }

    /// The next of the table's last pages: the open page, then the directories its shape needs,
    /// each placed before the next is asked for. None once the top index can list the pages.
    /// A table of `inline` entries or fewer needs no page: its entries go in the header.
    pub fn finish_next(&mut self, inline: u64) -> Option<Due> {
        if self.count <= inline && self.paged() == 0 {
            self.finished = true;
            return None;
        }
        if !self.open.is_empty() {
            return Some(self.page());
        }
        self.finished = true;
        let mut d = 0;
        loop {
            let pages = self.pages.get(d).copied().unwrap_or(0);
            if pages <= TOP as u64 {
                return None;
            }
            if !self.pending[d].is_empty() {
                return Some(self.directory(d as u8));
            }
            d += 1;
        }
    }

    /// The top index's entries for the table, once `finish_next` has given its last page: its
    /// depth and its entries. None for a table whose entries go in the header.
    pub fn top(&self) -> Option<(u8, Vec<Entry>)> {
        let d = self.pending.iter().rposition(|p| !p.is_empty())?;
        debug_assert_eq!(d as u8, depth_of(self.count));
        Some((d as u8, self.pending[d].to_vec()))
    }

    /// The open page's entries, for a header's inline list.
    pub fn inline(&self) -> Vec<R> {
        self.open.to_vec()
    }
}

/// A page of a table at depth 0 as `TableIndex::locate` finds it: its place, the entry that lists
/// it, and the `first` and `number` of the page after it, None for the table's last page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Located {
    pub j: u64,
    pub entry: Entry,
    pub next: Option<(i64, u64)>,
}

/// A table as a reader finds it: its entries in pages from the top index's, or, for a writer's
/// snapshot, from the pages no directory lists yet at each depth.
#[derive(Clone, Debug)]
pub struct TableIndex {
    pub channel: u32,
    pub table: Table,
    /// Entries in pages.
    pub rows: u64,
    /// At each depth, page entries no directory in the table lists: for a finished table only
    /// its depth's, the top index's.
    pub levels: Vec<Vec<Entry>>,
}

impl TableIndex {
    /// A finished table: `rows` entries listed by `top` at `depth`.
    pub fn finished(channel: u32, table: Table, rows: u64, depth: u8, top: Vec<Entry>) -> Self {
        let mut levels = vec![Vec::new(); usize::from(depth) + 1];
        levels[usize::from(depth)] = top;
        Self {
            channel,
            table,
            rows,
            levels,
        }
    }

    /// The table a writer's `TableWriter` holds in pages so far.
    pub fn of<R: Row>(t: &TableWriter<R>) -> Self {
        Self {
            channel: t.channel,
            table: t.table,
            rows: t.paged(),
            levels: t.pending.iter().map(|p| p.to_vec()).collect(),
        }
    }

    /// The pages at `depth` in order, `entries` of each, through `fetch` for the directories
    /// above: what a reader walks to read every entry. Each page is checked against the entry
    /// that lists it.
    pub fn pages<E: From<crate::decode::DecodeError>>(
        &self,
        depth: u8,
        fetch: &mut impl FnMut(u64, u32) -> Result<Vec<u8>, E>,
    ) -> Result<Vec<Entry>, E> {
        let d = usize::from(depth);
        let mut out = Vec::new();
        if d + 1 < self.levels.len() {
            for (j, dir) in self.pages(depth + 1, fetch)?.into_iter().enumerate() {
                self.check_len(depth + 1, j as u64, &dir)?;
                let bytes = fetch(dir.offset, dir.length)?;
                let page = self.parse_page(depth + 1, j as u64, &dir, &bytes)?;
                match page {
                    Page::Directory(entries) => out.extend(entries),
                    _ => unreachable!("a page above depth 0 is a directory"),
                }
            }
        }
        if let Some(level) = self.levels.get(d) {
            out.extend_from_slice(level);
        }
        Ok(out)
    }

    /// The entries at depth 0, every page read through `fetch`.
    pub fn read_all<E: From<crate::decode::DecodeError>>(
        &self,
        fetch: &mut impl FnMut(u64, u32) -> Result<Vec<u8>, E>,
    ) -> Result<Vec<Page>, E> {
        let mut out = Vec::new();
        for (j, e) in self.pages(0, fetch)?.into_iter().enumerate() {
            self.check_len(0, j as u64, &e)?;
            let bytes = fetch(e.offset, e.length)?;
            out.push(self.parse_page(0, j as u64, &e, &bytes)?);
        }
        Ok(out)
    }

    /// Each page at depth 0 that can hold an entry whose `first` lies in `keys`, in order, given to
    /// `f`; `f` ends the walk early with `Break`. A page holds the entries from its own `first` to
    /// the next page's. The directory pages on the way are read through `fetch`, one a depth at a
    /// time, each checked as `pages` checks it; the pages at depth 0 are not read.
    pub fn locate<E: From<crate::decode::DecodeError>>(
        &self,
        keys: Range<i64>,
        fetch: &mut impl FnMut(u64, u32) -> Result<Vec<u8>, E>,
        mut f: impl FnMut(Located) -> Result<ControlFlow<()>, E>,
    ) -> Result<(), E> {
        self.walk(&keys, fetch, &mut |at, _| f(at))
    }

    /// Every page at depth 0 in order, each read through `fetch` and checked as `read_all` checks
    /// it, given to `f` with where it lies; `f` ends the walk early with `Break`. It holds one
    /// page a depth at a time, so a reader can count a table of any length.
    pub fn visit<E: From<crate::decode::DecodeError>>(
        &self,
        fetch: &mut impl FnMut(u64, u32) -> Result<Vec<u8>, E>,
        f: impl FnMut(Located, Page) -> Result<ControlFlow<()>, E>,
    ) -> Result<(), E> {
        self.visit_in(i64::MIN..i64::MAX, fetch, f)
    }

    /// As `visit`, for the pages at depth 0 that `keys` reaches, as `locate` finds them: for a
    /// table whose keys are spans (`Table::spans`), those whose span meets `keys`.
    pub fn visit_in<E: From<crate::decode::DecodeError>>(
        &self,
        keys: Range<i64>,
        fetch: &mut impl FnMut(u64, u32) -> Result<Vec<u8>, E>,
        mut f: impl FnMut(Located, Page) -> Result<ControlFlow<()>, E>,
    ) -> Result<(), E> {
        self.walk(&keys, fetch, &mut |at, fetch| {
            let bytes = fetch(at.entry.offset, at.entry.length)?;
            let page = self.parse_page(0, at.j, &at.entry, &bytes)?;
            f(at, page)
        })
    }

    /// The walk `locate` and `visit` share: the entries no directory lists, deepest first, each
    /// directory's pages before the pages written after it, and `leaf` for each page at depth 0
    /// that `keys` reaches.
    fn walk<E, F>(
        &self,
        keys: &Range<i64>,
        fetch: &mut F,
        leaf: &mut impl FnMut(Located, &mut F) -> Result<ControlFlow<()>, E>,
    ) -> Result<(), E>
    where
        E: From<crate::decode::DecodeError>,
        F: FnMut(u64, u32) -> Result<Vec<u8>, E>,
    {
        // The entries at each depth that no directory lists follow, in the file, those every
        // directory above them lists: a writer's directories list only full pages, so the first
        // of them is page 4,096 × (the pages at the depth above) of its depth.
        let mut roots = Vec::new();
        let mut above = 0u64;
        for d in (0..self.levels.len()).rev() {
            let base = above.saturating_mul(PAGE as u64);
            for (i, e) in self.levels[d].iter().enumerate() {
                roots.push((d as u8, base + i as u64, *e));
            }
            above = base + self.levels[d].len() as u64;
        }
        for (i, &(depth, j, entry)) in roots.iter().enumerate() {
            let after = roots.get(i + 1).map(|r| (r.2.first, r.2.number));
            if self
                .descend(keys, depth, j, entry, after, fetch, leaf)?
                .is_break()
            {
                break;
            }
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "a page's place, its bounds, and the walk's keys, fetch and leaf"
    )]
    fn descend<E, F>(
        &self,
        keys: &Range<i64>,
        depth: u8,
        j: u64,
        entry: Entry,
        after: Option<(i64, u64)>,
        fetch: &mut F,
        leaf: &mut impl FnMut(Located, &mut F) -> Result<ControlFlow<()>, E>,
    ) -> Result<ControlFlow<()>, E>
    where
        E: From<crate::decode::DecodeError>,
        F: FnMut(u64, u32) -> Result<Vec<u8>, E>,
    {
        if self.table.spans() {
            // Rows keep no order of time: a page is passed over where its span misses the keys,
            // and the walk goes on past it.
            let end = i64::try_from(entry.number).unwrap_or(i64::MAX);
            if entry.first >= keys.end || end < keys.start {
                return Ok(ControlFlow::Continue(()));
            }
        } else {
            if entry.first >= keys.end {
                return Ok(ControlFlow::Break(()));
            }
            if after.is_some_and(|a| a.0 <= keys.start) {
                return Ok(ControlFlow::Continue(()));
            }
        }
        self.check_len(depth, j, &entry)?;
        if depth == 0 {
            let at = Located {
                j,
                entry,
                next: after,
            };
            return leaf(at, fetch);
        }
        let bytes = fetch(entry.offset, entry.length)?;
        let Page::Directory(entries) = self.parse_page(depth, j, &entry, &bytes)? else {
            unreachable!("a page above depth 0 is a directory")
        };
        for (i, e) in entries.iter().enumerate() {
            let next = entries.get(i + 1).map(|n| (n.first, n.number)).or(after);
            let place = j.saturating_mul(PAGE as u64) + i as u64;
            if self
                .descend(keys, depth - 1, place, *e, next, fetch, leaf)?
                .is_break()
            {
                return Ok(ControlFlow::Break(()));
            }
        }
        Ok(ControlFlow::Continue(()))
    }

    /// Refuses `entry`, which lists page `j` at `depth`, where its length isn't the one the
    /// table's shape gives: checked before the page is fetched.
    fn check_len(
        &self,
        depth: u8,
        j: u64,
        entry: &Entry,
    ) -> Result<(), crate::decode::DecodeError> {
        let want = self.page_len(depth, j);
        if u64::from(entry.length) != want {
            return Err(crate::decode::corrupt(format!(
                "channel {}: page {j} at depth {depth} of its {} is {} bytes, where its entries \
                 take {want}",
                self.channel,
                table_name(self.table),
                entry.length
            )));
        }
        Ok(())
    }

    /// The length page `j` at `depth` of the table's shape has as a record, its framing
    /// included: 32 bytes and its entries, each its table's own size at depth 0 and a directory
    /// entry's above. 0 for a page past the table's end.
    pub fn page_len(&self, depth: u8, j: u64) -> u64 {
        let below = if depth == 0 {
            self.rows
        } else {
            pages_at(self.rows, depth - 1)
        };
        let count = below
            .saturating_sub(j.saturating_mul(PAGE as u64))
            .min(PAGE as u64);
        if count == 0 {
            return 0;
        }
        let size = if depth == 0 {
            self.table.row_size()
        } else {
            Entry::SIZE
        };
        (record::FRAMING + PAGE_HEAD) as u64 + count * size as u64
    }

    /// Page `j` of `depth`, listed by `entry`, from exactly its record's bytes. It must be the
    /// page the table's shape puts there, with its entries in order, and start where the entry
    /// says.
    pub fn parse_page(
        &self,
        depth: u8,
        j: u64,
        entry: &Entry,
        bytes: &[u8],
    ) -> Result<Page, crate::decode::DecodeError> {
        use crate::decode::corrupt;
        let bad = |what: &str| {
            corrupt(format!(
                "channel {}: page {j} at depth {depth} of its {}: {what}",
                self.channel,
                table_name(self.table)
            ))
        };
        let payload = match record::next(bytes, bytes.len()) {
            record::Next::Record { kind, payload, len }
                if kind == self.table.kind(depth) as u8 && len == bytes.len() =>
            {
                payload
            }
            _ => return Err(bad("the record is not a whole page of its kind")),
        };
        let (head, body) =
            PageHead::parse(payload).ok_or_else(|| bad("the record is too short"))?;
        let below = if depth == 0 {
            self.rows
        } else {
            pages_at(self.rows, depth - 1)
        };
        let place = j.saturating_mul(PAGE as u64);
        let count = below.saturating_sub(place).min(PAGE as u64);
        if head.channel != self.channel
            || head.table != self.table.code()
            || head.shift != self.table.shift()
            || head.depth != depth
            || head.place != place
            || u64::from(head.count) != count
            || count == 0
        {
            return Err(bad("its head differs from where the table's shape puts it"));
        }
        let size = if depth == 0 {
            self.table.row_size()
        } else {
            Entry::SIZE
        };
        if body.len() as u64 != count * size as u64 {
            return Err(bad("its entries do not fill it"));
        }
        let chunks = body.chunks_exact(size);
        let page = if depth > 0 {
            let entries: Vec<Entry> = chunks.map(Entry::parse).collect();
            if self.table.spans() {
                if !entries.windows(2).all(|w| w[0].offset < w[1].offset) {
                    return Err(bad("its entries are out of order"));
                }
                if entries.iter().any(|e| !span_in_range(e.first, e.number)) {
                    return Err(bad("an entry's span lies outside 0 to 2^53"));
                }
            } else if !entries
                .windows(2)
                .all(|w| w[0].first <= w[1].first && w[0].number <= w[1].number)
            {
                return Err(bad("its entries are out of order"));
            }
            if entries.iter().any(|e| e.offset >= entry.offset) {
                return Err(bad("it lists a page that does not come before it"));
            }
            let e = entries[0];
            let key = if self.table.spans() {
                entries
                    .iter()
                    .fold((e.first, e.number), |k, e| span_key(k, (e.first, e.number)))
            } else {
                (e.first, e.number)
            };
            if key != (entry.first, entry.number) {
                return Err(bad("it starts where its directory says it doesn't"));
            }
            Page::Directory(entries)
        } else {
            match self.table {
                Table::Blocks => {
                    let rows: Vec<BlockRow> = chunks.map(BlockRow::parse).collect();
                    let ordered = rows
                        .windows(2)
                        .all(|w| w[0].start < w[1].start && w[0].offset < w[1].offset);
                    if !ordered {
                        return Err(bad("its blocks are out of order"));
                    }
                    if rows.iter().any(|r| {
                        r.offset < crate::header::PREAMBLE_LEN as u64
                            || r.offset.saturating_add(u64::from(r.length)) > entry.offset
                    }) {
                        return Err(bad("a block's frame does not lie before the page"));
                    }
                    if rows[0].start != entry.first {
                        return Err(bad("it starts where its directory says it doesn't"));
                    }
                    Page::Blocks {
                        number: entry.number,
                        rows,
                    }
                }
                Table::Level(_) => {
                    let rows: Vec<OverviewRow> = chunks.map(OverviewRow::parse).collect();
                    if rows.iter().any(|r| r.n == 0) {
                        return Err(bad("a row stands for no block"));
                    }
                    if !rows.windows(2).all(|w| w[0].offset < w[1].offset) {
                        return Err(bad("its rows are out of order"));
                    }
                    if rows.iter().any(|r| {
                        r.offset < crate::header::PREAMBLE_LEN as u64
                            || r.offset.saturating_add(u64::from(r.length)) > entry.offset
                    }) {
                        return Err(bad("a row's frame does not lie before the page"));
                    }
                    if entry.number != place {
                        return Err(bad("it starts where its directory says it doesn't"));
                    }
                    Page::Level {
                        k: u64::try_from(entry.first).map_err(|_| bad("a negative k"))?,
                        rows,
                    }
                }
                Table::Gaps => {
                    let mut gaps = Vec::with_capacity(count as usize);
                    for b in chunks {
                        if b[16] > 1 {
                            return Err(bad("a gap's level is not 0 or 1"));
                        }
                        gaps.push(Gap {
                            from: i64::from_le_bytes(b[0..8].try_into().unwrap()),
                            to: i64::from_le_bytes(b[8..16].try_into().unwrap()),
                            level: b[16] == 1,
                        });
                    }
                    if !gaps.iter().all(|g| g.from < g.to)
                        || !gaps.windows(2).all(|w| w[0].to < w[1].from)
                    {
                        return Err(bad("its gaps are out of order"));
                    }
                    if gaps[0].from != entry.first || entry.number != place {
                        return Err(bad("it starts where its directory says it doesn't"));
                    }
                    Page::Gaps(gaps)
                }
                Table::Breaks => {
                    let breaks: Vec<Break> = chunks
                        .map(|b| Break {
                            index: u64::from_le_bytes(b[0..8].try_into().unwrap()),
                            tick: i64::from_le_bytes(b[8..16].try_into().unwrap()),
                            lost: u64::from_le_bytes(b[16..24].try_into().unwrap()),
                            phase: u32::from_le_bytes(b[24..28].try_into().unwrap()),
                        })
                        .collect();
                    if !breaks
                        .windows(2)
                        .all(|w| w[0].index < w[1].index && w[0].tick <= w[1].tick)
                    {
                        return Err(bad("its breaks are out of order"));
                    }
                    if (breaks[0].tick, breaks[0].index) != (entry.first, entry.number) {
                        return Err(bad("it starts where its directory says it doesn't"));
                    }
                    Page::Breaks(breaks)
                }
                Table::Parts => {
                    let rows: Vec<PartRow> = chunks.map(PartRow::parse).collect();
                    if !rows.windows(2).all(|w| w[0].offset < w[1].offset) {
                        return Err(bad("its parts are out of order"));
                    }
                    if rows.iter().any(|r| {
                        r.offset < crate::header::PREAMBLE_LEN as u64
                            || r.offset.saturating_add(u64::from(r.length)) > entry.offset
                    }) {
                        return Err(bad("a part's frame does not lie before the page"));
                    }
                    if rows
                        .iter()
                        .any(|r| r.start > r.end || !span_in_range(r.start, r.end as u64))
                    {
                        return Err(bad("a part's span lies outside 0 to 2^53"));
                    }
                    if rows.iter().any(|r| {
                        r.decoded_length as usize > record::MAX_PART
                            || r.length as usize > record::compress_bound(r.decoded_length as usize)
                    }) {
                        return Err(bad("a part is past the format's limits"));
                    }
                    let key = rows
                        .iter()
                        .fold(rows[0].key(0, 0), |k, r| span_key(k, r.key(0, 0)));
                    if key != (entry.first, entry.number) {
                        return Err(bad("its span differs from the one its directory states"));
                    }
                    Page::Parts(rows)
                }
            }
        };
        Ok(page)
    }
}

/// Whether a span's start and end, as a part row or a parts directory entry states them, lie in
/// `0..2^53`, the start at most the end.
pub(crate) fn span_in_range(first: i64, number: u64) -> bool {
    first >= 0 && (first as u64) <= number && number < JSON_SAFE
}

/// A table's name, for a message.
pub fn table_name(table: Table) -> String {
    match table {
        Table::Blocks => "blocks".into(),
        Table::Level(s) => format!("overview level 2^{s}"),
        Table::Gaps => "gaps".into(),
        Table::Breaks => "breaks".into(),
        Table::Parts => "annotation parts".into(),
    }
}

/// A page, parsed.
#[derive(Clone, Debug, PartialEq)]
pub enum Page {
    /// Block rows, the first block's first item at index `number` of the channel.
    Blocks {
        number: u64,
        rows: Vec<BlockRow>,
    },
    /// Overview rows of one level, the first standing for blocks from `k`.
    Level {
        k: u64,
        rows: Vec<OverviewRow>,
    },
    Gaps(Vec<Gap>),
    Breaks(Vec<Break>),
    /// Annotation part rows, in the order the parts were written.
    Parts(Vec<PartRow>),
    Directory(Vec<Entry>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tables_depth_is_the_least_at_which_16_pages_list_it() {
        // 16 pages of 4,096: the top index lists them. One row more needs a directory.
        assert_eq!(depth_of(1), 0);
        assert_eq!(depth_of(16 * 4096), 0);
        assert_eq!(depth_of(16 * 4096 + 1), 1);
        assert_eq!(pages_at(16 * 4096 + 1, 0), 17);
        assert_eq!(pages_at(16 * 4096 + 1, 1), 1);
        // One directory level lists 16 x 4,096 pages of 4,096 rows: 2^28.
        assert_eq!(depth_of(1 << 28), 1);
        assert_eq!(depth_of((1 << 28) + 1), 2);
        assert_eq!(depth_of((1 << 40) + 1), 3);
        // Three list 2^52, and counts are below 2^53: four directory levels at most.
        assert_eq!(depth_of(1 << 52), 3);
        assert_eq!(depth_of((1 << 52) + 1), 4);
        assert_eq!(depth_of((1 << 53) - 1), 4);
    }

    /// Writes a block table of `n` rows as a writer would, each page placed right after it is
    /// due, and returns the records in order and the top.
    fn written(n: u64) -> (Vec<Due>, Option<(u8, Vec<Entry>)>) {
        let mut t = TableWriter::<BlockRow>::new(3, Table::Blocks, false);
        let mut out = Vec::new();
        let mut at = 1 << 20;
        let mut place =
            |t: &mut TableWriter<BlockRow>, mut due: Option<Due>, out: &mut Vec<Due>| {
                while let Some(d) = due {
                    due = t.placed(&d, at);
                    at += d.record.len() as u64;
                    out.push(d);
                }
            };
        for i in 0..n {
            let due = t.push(BlockRow {
                start: i as i64,
                offset: 12 + i,
                length: 1,
                decoded_length: 0,
                word: 2,
            });
            place(&mut t, due, &mut out);
        }
        loop {
            let due = t.finish_next(0);
            if due.is_none() {
                break;
            }
            place(&mut t, due, &mut out);
        }
        (out, t.top())
    }

    #[test]
    fn a_writer_pages_a_table_as_its_shape_says() {
        for n in [1, 4095, 4096, 4097, 16 * 4096, 16 * 4096 + 1, 17 * 4096 + 5] {
            let (records, top) = written(n);
            let (depth, entries) = top.expect("a block table has a top");
            assert_eq!(depth, depth_of(n), "{n} rows");
            assert_eq!(entries.len() as u64, pages_at(n, depth), "{n} rows");
            let pages = records.iter().filter(|r| r.depth == 0).count() as u64;
            let directories = records.iter().filter(|r| r.depth == 1).count() as u64;
            assert_eq!(pages, pages_at(n, 0), "{n} rows");
            assert_eq!(
                directories,
                if depth == 0 { 0 } else { pages_at(n, 1) },
                "{n} rows"
            );
            // The top's entries start where the pages do: block i at tick i, two items each.
            for (j, e) in entries.iter().enumerate() {
                let per = PAGE.pow(u32::from(depth)) as u64 * PAGE as u64;
                assert_eq!(
                    (e.first, e.number),
                    ((j as u64 * per) as i64, 2 * j as u64 * per)
                );
            }
        }
    }

    #[test]
    fn a_reader_reads_back_every_row_and_refuses_a_page_out_of_place() {
        let n = 17 * 4096 + 5;
        let (records, top) = written(n);
        let mut file = vec![0u8; 1 << 20];
        for r in &records {
            file.extend_from_slice(&r.record);
        }
        let (depth, entries) = top.unwrap();
        let index = TableIndex::finished(3, Table::Blocks, n, depth, entries);
        let mut fetch = |offset: u64, len: u32| -> Result<Vec<u8>, crate::decode::DecodeError> {
            Ok(file[offset as usize..][..len as usize].to_vec())
        };
        let pages = index.read_all(&mut fetch).unwrap();
        let rows: Vec<BlockRow> = pages
            .iter()
            .flat_map(|p| match p {
                Page::Blocks { rows, .. } => rows.clone(),
                _ => panic!("block pages"),
            })
            .collect();
        assert_eq!(rows.len() as u64, n);
        assert!(rows.iter().enumerate().all(|(i, r)| r.start == i as i64));
        // The same table read as one of a count it isn't: the last page's entry is longer than
        // that count gives it, which is refused before the page is fetched.
        let wrong = TableIndex {
            rows: n - 1,
            ..index.clone()
        };
        let err = wrong.read_all(&mut fetch).unwrap_err().to_string();
        assert!(
            // 32 bytes of framing and head, and 28 a row: five rows against four.
            err.contains(
                "page 17 at depth 0 of its blocks is 172 bytes, where its entries take 144"
            ),
            "{err}"
        );
        // As another channel's.
        let other = TableIndex {
            channel: 4,
            ..index
        };
        let err = other.read_all(&mut fetch).unwrap_err().to_string();
        assert!(err.contains("its head differs"), "{err}");
    }

    /// The rows of block table pages, flattened.
    fn rows_of<'a>(pages: impl IntoIterator<Item = &'a Page>) -> Vec<BlockRow> {
        pages
            .into_iter()
            .flat_map(|p| match p {
                Page::Blocks { rows, .. } => rows.clone(),
                _ => panic!("block pages"),
            })
            .collect()
    }

    #[test]
    fn a_walk_gives_each_page_where_the_next_starts_and_reads_only_what_its_keys_reach() {
        for n in [4097, 17 * 4096 + 5] {
            let (records, top) = written(n);
            let mut file = vec![0u8; 1 << 20];
            for r in &records {
                file.extend_from_slice(&r.record);
            }
            let (depth, entries) = top.unwrap();
            let index = TableIndex::finished(3, Table::Blocks, n, depth, entries);
            let directories = u64::from(depth) * pages_at(n, 1);
            let fetched = std::cell::Cell::new(0u64);
            let mut fetch =
                |offset: u64, len: u32| -> Result<Vec<u8>, crate::decode::DecodeError> {
                    fetched.set(fetched.get() + 1);
                    Ok(file[offset as usize..][..len as usize].to_vec())
                };
            let all = index.read_all(&mut fetch).unwrap();
            let pages = pages_at(n, 0);
            // Every page in order, each with where the next starts: block i at tick i, two items
            // each, and the last with none after it.
            fetched.set(0);
            let mut seen = Vec::new();
            index
                .visit(&mut fetch, |at, page| {
                    seen.push((at, page));
                    Ok(ControlFlow::Continue(()))
                })
                .unwrap();
            assert_eq!(
                fetched.get(),
                pages + directories,
                "{n} rows: one read a page"
            );
            assert_eq!(seen.len() as u64, pages, "{n} rows");
            for (j, (at, _)) in seen.iter().enumerate() {
                let j = j as u64;
                let start = (j * PAGE as u64) as i64;
                assert_eq!((at.j, at.entry.first), (j, start), "{n} rows, page {j}");
                let next =
                    (j + 1 < pages).then(|| (start + PAGE as i64, 2 * (j + 1) * PAGE as u64));
                assert_eq!(at.next, next, "{n} rows, page {j}");
            }
            assert_eq!(
                rows_of(seen.iter().map(|s| &s.1)),
                rows_of(&all),
                "{n} rows"
            );
            // A span inside page 5, one from page 3 to 6, and one from before the table to past
            // its end: only the pages that hold them, the last holding every later tick, and
            // only the directory above them read.
            let last = pages - 1;
            for (keys, want) in [
                (5 * 4096 + 10..5 * 4096 + 20, 5.min(last)..6.min(pages)),
                (3 * 4096 + 4095..6 * 4096 + 1, 3.min(last)..7.min(pages)),
                (-10..i64::MAX, 0..pages),
            ] {
                fetched.set(0);
                let mut got = Vec::new();
                index
                    .locate(keys.clone(), &mut fetch, |at| {
                        got.push(at.j);
                        Ok(ControlFlow::Continue(()))
                    })
                    .unwrap();
                assert_eq!(got, want.collect::<Vec<_>>(), "{n} rows, {keys:?}");
                assert_eq!(
                    fetched.get(),
                    directories,
                    "{n} rows, {keys:?}: no page read"
                );
            }
            // `Break` ends the walk at once.
            let mut got = 0;
            index
                .locate(0..i64::MAX, &mut fetch, |_| {
                    got += 1;
                    Ok(ControlFlow::Break(()))
                })
                .unwrap();
            assert_eq!(got, 1, "{n} rows");
        }
    }

    /// A full page of 4,096 block rows, framing included.
    const FULL: u32 = (record::FRAMING + PAGE_HEAD + PAGE * BlockRow::SIZE) as u32;

    /// Directory page `place` of depth 1 listing pages `pages` of a block table whose page j
    /// starts at tick 4,096 j: its record, and its entry.
    fn directory(file: &mut Vec<u8>, place: u64, pages: Range<u64>) -> Entry {
        // A full page of block rows, as the table's shape gives each of these.
        let entry = |j: u64| Entry {
            first: (j * PAGE as u64) as i64,
            number: 2 * j * PAGE as u64,
            offset: 12 + j,
            length: FULL,
        };
        let mut payload = Vec::new();
        PageHead {
            channel: 3,
            table: Table::Blocks.code(),
            shift: 0,
            depth: 1,
            place,
            count: pages.clone().count() as u32,
        }
        .write(&mut payload);
        for j in pages.clone() {
            entry(j).write(&mut payload);
        }
        let offset = file.len() as u64;
        record::write(file, Kind::IndexPage, &[&payload]);
        Entry {
            offset,
            length: (file.len() as u64 - offset) as u32,
            ..entry(pages.start)
        }
    }

    /// Past one directory page, a page's next is the first under the next directory, or the
    /// first of the pages a writer has written since; and those pages take their places after
    /// every page the directories list. Only the directories are read: the pages they list here
    /// are not there.
    #[test]
    fn a_walk_carries_where_the_next_page_starts_across_directories() {
        let per = PAGE as u64;
        let mut file = vec![0u8; 1 << 20];
        let first = directory(&mut file, 0, 0..per);
        let second = directory(&mut file, per, per..per + 1);
        let finished =
            TableIndex::finished(3, Table::Blocks, (per + 1) * per, 1, vec![first, second]);
        // A writer's: one directory written, two pages since.
        let pending = |j: u64| Entry {
            first: (j * per) as i64,
            number: 2 * j * per,
            offset: file.len() as u64 + j,
            length: FULL,
        };
        let live = TableIndex {
            channel: 3,
            table: Table::Blocks,
            rows: (per + 2) * per,
            levels: vec![vec![pending(per), pending(per + 1)], vec![first]],
        };
        let mut fetch = |offset: u64, len: u32| -> Result<Vec<u8>, crate::decode::DecodeError> {
            Ok(file[offset as usize..][..len as usize].to_vec())
        };
        for (index, pages) in [(finished, per + 1), (live, per + 2)] {
            let mut got = Vec::new();
            index
                .locate(i64::MIN..i64::MAX, &mut fetch, |at| {
                    got.push(at);
                    Ok(ControlFlow::Continue(()))
                })
                .unwrap();
            assert_eq!(got.len() as u64, pages);
            for (j, at) in got.iter().enumerate() {
                let j = j as u64;
                let next = (j + 1 < pages).then(|| (((j + 1) * per) as i64, 2 * (j + 1) * per));
                assert_eq!((at.j, at.entry.first, at.next), (j, (j * per) as i64, next));
            }
        }
    }

    /// A writer's table before its finish, as a snapshot lists it: the pages written, with the
    /// rows of its open page left to the snapshot's own list.
    #[test]
    fn a_walk_of_a_table_still_written_gives_the_pages_it_has() {
        let mut t = TableWriter::<BlockRow>::new(3, Table::Blocks, false);
        let mut file = vec![0u8; 1 << 20];
        let n = 5 * 4096 + 7;
        for i in 0..n {
            let mut due = t.push(BlockRow {
                start: i as i64,
                offset: 12 + i,
                length: 1,
                decoded_length: 0,
                word: 2,
            });
            while let Some(d) = due {
                due = t.placed(&d, file.len() as u64);
                file.extend_from_slice(&d.record);
            }
        }
        let index = TableIndex::of(&t);
        let mut fetch = |offset: u64, len: u32| -> Result<Vec<u8>, crate::decode::DecodeError> {
            Ok(file[offset as usize..][..len as usize].to_vec())
        };
        let mut seen = Vec::new();
        index
            .visit(&mut fetch, |at, page| {
                seen.push((at.j, at.next, page));
                Ok(ControlFlow::Continue(()))
            })
            .unwrap();
        let js: Vec<(u64, Option<(i64, u64)>)> = seen.iter().map(|s| (s.0, s.1)).collect();
        let want: Vec<(u64, Option<(i64, u64)>)> = (0..5u64)
            .map(|j| {
                (
                    j,
                    (j < 4).then(|| (((j + 1) * 4096) as i64, 2 * (j + 1) * 4096)),
                )
            })
            .collect();
        assert_eq!(js, want);
        let pages: Vec<Page> = seen.into_iter().map(|s| s.2).collect();
        assert_eq!(
            rows_of(&pages),
            rows_of(&index.read_all(&mut fetch).unwrap())
        );
    }
}
