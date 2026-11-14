//! The in-order `Reader` fed a file in chunks of any size: it never panics, and it comes to the
//! same verdict, and for a file it reads to its end the same events, as when fed the file whole.
#![no_main]

use escapement_tickwave_format::reader::Reader;
use libfuzzer_sys::fuzz_target;

/// The events in order, by their Debug form and where the reader was, then how it ended.
fn read<'a>(pieces: impl Iterator<Item = &'a [u8]>) -> (Vec<String>, Result<String, String>) {
    let mut r = Reader::new(1 << 20);
    let mut events = Vec::new();
    for piece in pieces {
        r.feed(piece);
        loop {
            match r.next_event() {
                Ok(Some(e)) => events.push(format!("{} {e:?}", r.position())),
                Ok(None) => break,
                Err(e) => return (events, Err(e.to_string())),
            }
        }
    }
    let ending = r.end().map(|e| format!("{e:?}")).map_err(|e| e.to_string());
    (events, ending)
}

fuzz_target!(|data: &[u8]| {
    let file = escapement_tickwave_fuzz::file(data);
    let seed = data
        .iter()
        .fold(0u64, |h, &b| h.rotate_left(5) ^ u64::from(b));
    let whole = read(std::iter::once(&file[..]));
    let pieces = read(escapement_tickwave_fuzz::chunks(&file, seed));
    assert_eq!(
        whole.1.is_ok(),
        pieces.1.is_ok(),
        "{:?} / {:?}",
        whole.1,
        pieces.1
    );
    if whole.1.is_ok() {
        assert_eq!(whole, pieces);
    }
});
