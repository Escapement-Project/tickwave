//! A zip of files to open, as GitHub takes a capture attached to an issue or a discussion only
//! zipped. Its entries flatten to their file names, as a dropped folder's do. Nothing is written
//! out, so a name has nowhere to escape to.

use std::collections::HashMap;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::sync::Arc;

use zip::{CompressionMethod, ZipArchive};

use crate::input::Input;

/// How much a zip may hold before it is refused.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Entries in the whole archive, taken or not.
    pub entries: usize,
    /// Bytes the deflated entries taken inflate to, together.
    pub inflated: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            entries: 4096,
            inflated: 1 << 30,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("it isn't a zip archive tickwave can read: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("it holds {0} entries, more than the {1} tickwave reads")]
    Entries(usize, usize),
    #[error("{0} is encrypted")]
    Encrypted(String),
    #[error("{0} is compressed with {1}, and tickwave reads only stored and deflated files")]
    Method(String, CompressionMethod),
    #[error("{0} and {1} share a name, so either could be the one meant")]
    Duplicate(String, String),
    #[error("its files would take {0} bytes unzipped, more than the {1} tickwave unzips")]
    Inflated(u64, u64),
    #[error("{0} is cut short: the zip ends inside it")]
    Short(String),
}

/// Where an entry's bytes are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Data {
    /// As they are, from this offset in the zip.
    Stored { start: u64 },
    /// Deflated: [`Zipped::inflate`] gives them.
    Deflated,
}

/// A file in the zip that a reader takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Its file name, out of its folders.
    pub name: String,
    /// Its path in the zip.
    pub path: String,
    /// Its size unzipped.
    pub size: u64,
    pub data: Data,
    index: usize,
}

/// An input read through `Read` and `Seek`, for the zip crate.
struct At {
    input: Arc<dyn Input>,
    at: u64,
}

impl Read for At {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.input.size().saturating_sub(self.at);
        let n = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
        self.input.read_at(self.at, &mut buf[..n]);
        self.at += n as u64;
        Ok(n)
    }
}

impl Seek for At {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let at = match to {
            SeekFrom::Start(at) => Some(at),
            SeekFrom::End(by) => self.input.size().checked_add_signed(by),
            SeekFrom::Current(by) => self.at.checked_add_signed(by),
        };
        self.at = at.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "a seek before the start")
        })?;
        Ok(self.at)
    }
}

/// A file's name out of its folders. Some Windows tools write `\` between them.
fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// macOS's Compress keeps a file's resource fork beside it, as `__MACOSX/<path>/._<name>`.
fn resource_fork(path: &str) -> bool {
    path.starts_with("__MACOSX/") || file_name(path).starts_with("._")
}

pub struct Zipped {
    archive: ZipArchive<BufReader<At>>,
    entries: Vec<Entry>,
}

impl Zipped {
    /// The zip in `input`, with the files whose name `wanted` takes. Folders, macOS's resource
    /// forks and other files are passed over. A file taken that tickwave can't unzip, or that
    /// shares its name with another, refuses the whole zip.
    pub fn open(
        input: Arc<dyn Input>,
        wanted: &dyn Fn(&str) -> bool,
        limits: Limits,
    ) -> Result<Self, Error> {
        let size = input.size();
        let reader = BufReader::with_capacity(1 << 16, At { input, at: 0 });
        let mut archive = ZipArchive::new(reader)?;
        if archive.len() > limits.entries {
            return Err(Error::Entries(archive.len(), limits.entries));
        }
        let mut entries = Vec::new();
        let mut seen: HashMap<String, String> = HashMap::new();
        let mut inflated = 0u64;
        for index in 0..archive.len() {
            let file = archive.by_index_raw(index)?;
            let path = file.name()?.into_owned();
            let name = file_name(&path).to_owned();
            if file.is_dir() || resource_fork(&path) || name.is_empty() || !wanted(&name) {
                continue;
            }
            if file.encrypted() {
                return Err(Error::Encrypted(path));
            }
            if let Some(first) = seen.insert(name.clone(), path.clone()) {
                return Err(Error::Duplicate(first, path));
            }
            let data = match file.compression() {
                CompressionMethod::Stored => {
                    let start = file.data_start().ok_or_else(|| {
                        zip::result::ZipError::InvalidArchive("a file's data has no start".into())
                    })?;
                    if file.compressed_size() != file.size() {
                        return Err(zip::result::ZipError::InvalidArchive(
                            "a stored file states two sizes".into(),
                        )
                        .into());
                    }
                    if start.checked_add(file.size()).is_none_or(|end| end > size) {
                        return Err(Error::Short(path));
                    }
                    Data::Stored { start }
                }
                CompressionMethod::Deflated => {
                    inflated = inflated.saturating_add(file.size());
                    Data::Deflated
                }
                method => return Err(Error::Method(path, method)),
            };
            entries.push(Entry {
                name,
                path,
                size: file.size(),
                data,
                index,
            });
        }
        if inflated > limits.inflated {
            return Err(Error::Inflated(inflated, limits.inflated));
        }
        Ok(Self { archive, entries })
    }

    /// The files taken, in the zip's order.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Unzips a deflated entry, handing its bytes to `emit` a chunk at a time. The zip crate checks
    /// them against the CRC the zip states, and stops at the size it states.
    pub fn inflate(&mut self, entry: &Entry, emit: &mut dyn FnMut(&[u8])) -> Result<(), Error> {
        let mut file = self.archive.by_index(entry.index)?;
        let mut buf = vec![0; 1 << 16];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                return Ok(());
            }
            emit(&buf[..n]);
        }
    }
}
