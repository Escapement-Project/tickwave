//! Prints what a structured input holds and what the readers make of the file it builds:
//! `cargo run --no-default-features --bin show -- <input>`.

use escapement_tickwave_format::reader::{Event, Reader};
use escapement_tickwave_format::repair::repair;

fn main() {
    let data = std::fs::read(std::env::args().nth(1).unwrap()).unwrap();
    let file = escapement_tickwave_fuzz::file(&data);
    println!("{} bytes structured, {} built", data.len(), file.len());
    let mut r = Reader::default();
    r.feed(&file);
    loop {
        let at = r.position();
        match r.next_event() {
            Ok(Some(e)) => println!(
                "{at}: {}",
                match e {
                    Event::Declaration(d) =>
                        format!("declaration {}", serde_json::to_string(&d).unwrap()),
                    Event::Block(b) => format!(
                        "block {:?} frame {}+{}",
                        b.head, b.frame_offset, b.frame_len
                    ),
                    Event::OverviewBlock {
                        shift,
                        k,
                        frame_offset,
                        frame,
                    } => format!(
                        "overview block shift {shift} k {k} at {frame_offset}+{}",
                        frame.len()
                    ),
                    Event::Footer(f) =>
                        format!("footer {}", serde_json::to_string(&f.header).unwrap()),
                    other => format!("{other:?}"),
                }
            ),
            Ok(None) => break,
            Err(e) => {
                println!("{at}: error {e}");
                break;
            }
        }
    }
    println!("ending {:?}", r.end());
    match repair(&file, 1) {
        Ok(rep) => {
            let mut out = file[..rep.keep as usize].to_vec();
            out.extend(&rep.tail);
            println!("repaired: keep {} tail {}", rep.keep, rep.tail.len());
            let mut r = Reader::default();
            r.feed(&out);
            loop {
                match r.next_event() {
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(e) => {
                        println!("repaired file: error {e}");
                        break;
                    }
                }
            }
        }
        Err(e) => println!("repair: {e}"),
    }
}
