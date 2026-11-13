//! Where a recording's bytes come from.

use std::io;
use std::sync::Arc;

/// Bytes of a recording at any offset. Fallible: a live file can go away under a view.
pub trait Bytes: Send + Sync {
    fn size(&self) -> io::Result<u64>;
    /// Fills `buf` from `offset`, all of it, or fails.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()>;
}

#[cfg(unix)]
impl Bytes for std::fs::File {
    fn size(&self) -> io::Result<u64> {
        Ok(self.metadata()?.len())
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        std::os::unix::fs::FileExt::read_exact_at(self, buf, offset)
    }
}

#[cfg(windows)]
impl Bytes for std::fs::File {
    fn size(&self) -> io::Result<u64> {
        Ok(self.metadata()?.len())
    }

    fn read_at(&self, offset: u64, mut buf: &mut [u8]) -> io::Result<()> {
        use std::os::windows::fs::FileExt;
        // `seek_read` moves the file's cursor, which no other reader here uses.
        let mut at = offset;
        while !buf.is_empty() {
            match self.seek_read(buf, at) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => {
                    buf = &mut buf[n..];
                    at += n as u64;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

fn read_slice(bytes: &[u8], offset: u64, buf: &mut [u8]) -> io::Result<()> {
    let slice = usize::try_from(offset)
        .ok()
        .and_then(|at| bytes.get(at..at.checked_add(buf.len())?))
        .ok_or(io::ErrorKind::UnexpectedEof)?;
    buf.copy_from_slice(slice);
    Ok(())
}

impl Bytes for Vec<u8> {
    fn size(&self) -> io::Result<u64> {
        Ok(self.len() as u64)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        read_slice(self, offset, buf)
    }
}

impl Bytes for Arc<[u8]> {
    fn size(&self) -> io::Result<u64> {
        Ok(self.len() as u64)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        read_slice(self, offset, buf)
    }
}

/// `len` bytes from `offset`.
pub(crate) fn read(bytes: &dyn Bytes, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    let mut buf = vec![0; len];
    bytes.read_at(offset, &mut buf)?;
    Ok(buf)
}
