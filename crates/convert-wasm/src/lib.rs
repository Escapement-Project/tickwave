//! tickwave's converter for the browser: the CLI's conversion, run in a worker over files the
//! reader picked. JavaScript keeps the files and reads them for it through `tickwaveConvert.read`,
//! a slice at a time, so a Saleae export larger than WASM's memory still converts.

use std::path::PathBuf;
use std::sync::Arc;

use escapement_tickwave_format::decode::MAX_DECODED;
use escapement_tickwave_format::model::{ChannelData, Recording};
use escapement_tickwave_import::convert::{self, Pass, Progress, Settings, Step, select};
use escapement_tickwave_import::input::{Input, read_all};
use escapement_tickwave_import::{saleae, sigrok, vcd};
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

// The worker defines these on `globalThis.tickwaveConvert` before it calls in. Free functions,
// rather than a JS object handed in, keep the readers' iterators `Send`, as the model needs.
#[wasm_bindgen]
extern "C" {
    /// Fills `into` with file `file`'s bytes from `offset`. Throws, ending the run, when the file
    /// cannot be read.
    #[wasm_bindgen(js_namespace = tickwaveConvert)]
    fn read(file: u32, offset: f64, into: &mut [u8]);
    /// A pass starts: `check`, `codes` (with the channel), `encode` or `verify`, over `total` items.
    #[wasm_bindgen(js_namespace = tickwaveConvert)]
    fn start(pass: &str, channel: &str, total: f64);
    /// The pass under way has taken `n` more items.
    #[wasm_bindgen(js_namespace = tickwaveConvert)]
    fn step(n: f64);
    /// A panic's message, before the module traps.
    #[wasm_bindgen(js_namespace = tickwaveConvert)]
    fn panicked(message: &str);
}

/// A file the worker holds, by its index there.
struct JsFile {
    id: u32,
    size: u64,
}

impl Input for JsFile {
    fn size(&self) -> u64 {
        self.size
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) {
        read(self.id, offset as f64, buf);
    }
}

#[derive(Deserialize)]
struct FileRef {
    id: u32,
    name: String,
    size: f64,
}

impl FileRef {
    fn input(&self) -> Arc<dyn Input> {
        Arc::new(JsFile {
            id: self.id,
            size: self.size as u64,
        })
    }
}

#[derive(Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Saleae,
    Sigrok,
    Vcd,
}

#[derive(Serialize)]
struct Described {
    channels: Vec<ChannelInfo>,
    skipped: Vec<SkippedInfo>,
}

#[derive(Serialize)]
struct ChannelInfo {
    /// The source's own name: a Saleae file's stem, a sigrok probe, a VCD scope path.
    name: String,
    analog: bool,
    /// Edges or samples, where the source states them before it is read.
    count: Option<f64>,
    unit: Option<String>,
}

#[derive(Serialize)]
struct SkippedInfo {
    name: String,
    reason: String,
}

fn js(e: impl std::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
}

fn hook() {
    std::panic::set_hook(Box::new(|info| panicked(&info.to_string())));
}

fn single(files: &[FileRef]) -> Result<&FileRef, JsError> {
    match files {
        [one] => Ok(one),
        _ => Err(JsError::new("a sigrok or VCD source is one file")),
    }
}

fn channels_of(recording: &Recording) -> Vec<ChannelInfo> {
    recording
        .channels
        .iter()
        .map(|c| match &c.data {
            ChannelData::Digital(d) => ChannelInfo {
                name: c.name.clone(),
                analog: false,
                count: d.edges.size_hint().1.map(|n| n as f64),
                unit: None,
            },
            ChannelData::Analog(a) => ChannelInfo {
                name: c.name.clone(),
                analog: true,
                count: Some(a.count as f64),
                unit: Some(a.unit.clone()),
            },
        })
        .collect()
}

fn skipped_of(skipped: Vec<sigrok::Skipped>) -> Vec<SkippedInfo> {
    skipped
        .into_iter()
        .map(|s| SkippedInfo {
            name: s.name,
            reason: s.reason,
        })
        .collect()
}

#[derive(Deserialize)]
struct DescribeRequest {
    kind: Kind,
    files: Vec<FileRef>,
}

/// The channels a source offers. `request` is JSON, `{kind, files}`, where `files` is `[{id, name,
/// size}]`: a Saleae export's `.bin` files, or one sigrok or VCD file. The answer is JSON too,
/// `{channels: [{name, analog, count, unit}], skipped: [{name, reason}]}`. A Saleae file that is
/// not one is listed as skipped.
#[wasm_bindgen]
pub fn describe(request: &str) -> Result<String, JsError> {
    hook();
    let DescribeRequest { kind, files } = serde_json::from_str(request).map_err(js)?;
    let described = match kind {
        Kind::Saleae => {
            let mut channels = Vec::new();
            let mut skipped = Vec::new();
            for f in &files {
                let Some(stem) = f.name.strip_suffix(".bin") else {
                    continue;
                };
                match saleae::file_info(PathBuf::from(&f.name), f.input()) {
                    Ok(info) => channels.push(ChannelInfo {
                        name: stem.to_owned(),
                        analog: info.analog,
                        count: Some(info.count as f64),
                        unit: info.analog.then(|| "V".to_owned()),
                    }),
                    Err(e) => skipped.push(SkippedInfo {
                        name: f.name.clone(),
                        reason: e.to_string(),
                    }),
                }
            }
            channels.sort_by_cached_key(|c| natural(&c.name));
            Described { channels, skipped }
        }
        Kind::Sigrok => {
            let f = single(&files)?;
            let import = sigrok::read_bytes(read_all(&*f.input()), &f.name).map_err(js)?;
            Described {
                channels: channels_of(&import.recording),
                skipped: skipped_of(import.skipped),
            }
        }
        Kind::Vcd => {
            let f = single(&files)?;
            let bytes = read_all(&*f.input());
            let import = vcd::read_from(&bytes[..], &f.name).map_err(js)?;
            Described {
                channels: channels_of(&import.recording),
                skipped: skipped_of(import.skipped),
            }
        }
    };
    serde_json::to_string(&described).map_err(js)
}

/// A name's runs of digits as numbers, so `digital_10` sorts after `digital_9`.
fn natural(name: &str) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut digits = String::new();
    for ch in name.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else {
            if !digits.is_empty() {
                out.push((
                    std::mem::take(&mut text),
                    digits.parse().unwrap_or(u64::MAX),
                ));
                digits.clear();
            }
            text.push(ch);
        }
    }
    out.push((text, digits.parse().unwrap_or(0)));
    out
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Request {
    kind: Kind,
    files: Vec<FileRef>,
    /// `[source name, name]`, in the order to store them. A Saleae export needs at least one; for
    /// the others, none takes every channel under its own name.
    channels: Vec<(String, String)>,
    title: Option<String>,
    #[serde(default)]
    meta: Vec<(String, String)>,
    level: i32,
    block_kib: u32,
    #[serde(default)]
    units: Vec<(String, String)>,
    codes: bool,
}

struct Report;

impl Progress for Report {
    fn start(&self, pass: Pass<'_>, total: u64) -> Step {
        match pass {
            Pass::Check => start("check", "", total as f64),
            Pass::Codes(channel) => start("codes", channel, total as f64),
            Pass::Encode => start("encode", "", total as f64),
            Pass::Verify => start("verify", "", total as f64),
        }
        Arc::new(|n| step(n as f64))
    }
}

/// The recording made, and what the conversion found on the way.
#[wasm_bindgen]
pub struct Converted {
    file: Vec<u8>,
    codes: String,
}

#[wasm_bindgen]
impl Converted {
    /// The recording's bytes, copied out of the module.
    #[wasm_bindgen(getter)]
    pub fn file(&self) -> Vec<u8> {
        self.file.clone()
    }

    /// `[[channel, bits or null]]`: each analog channel's exact codes, where it has some.
    #[wasm_bindgen(getter)]
    pub fn codes(&self) -> String {
        self.codes.clone()
    }
}

/// Converts a source as `tickwave convert` does, with the same defaults and checks, so the same
/// source and settings give the same bytes. `request` is `{kind, files, channels, title, meta,
/// level, blockKib, units, codes}`, as JSON.
#[wasm_bindgen]
pub fn convert(request: &str) -> Result<Converted, JsError> {
    hook();
    let r: Request = serde_json::from_str(request).map_err(js)?;
    let settings = Settings {
        title: r.title.filter(|t| !t.is_empty()),
        meta: r.meta.into_iter().collect(),
        level: r.level,
        block_kib: r.block_kib,
        units: r.units,
        codes: r.codes,
    };
    let converted = match r.kind {
        Kind::Saleae => {
            let files =
                r.channels
                    .iter()
                    .map(|(stem, _)| {
                        let name = saleae::file_name(stem);
                        let f =
                            r.files.iter().find(|f| f.name == name).ok_or_else(|| {
                                JsError::new(&format!("no {name} among the files"))
                            })?;
                        Ok((PathBuf::from(name), f.input()))
                    })
                    .collect::<Result<Vec<_>, JsError>>()?;
            let headers = saleae::Headers::read(files, &r.channels).map_err(js)?;
            let export = convert::check_saleae(headers, &Report).map_err(js)?;
            convert::convert(&|| Ok(export.recording()), &settings, MAX_DECODED, &Report)
        }
        Kind::Sigrok => {
            let f = single(&r.files)?;
            let bytes: Arc<[u8]> = read_all(&*f.input()).into();
            let open = || {
                select(
                    sigrok::read_bytes(Arc::clone(&bytes), &f.name)?.recording,
                    &r.channels,
                )
            };
            convert::convert(&open, &settings, MAX_DECODED, &Report)
        }
        Kind::Vcd => {
            let f = single(&r.files)?;
            let bytes = read_all(&*f.input());
            let open = || select(vcd::read_from(&bytes[..], &f.name)?.recording, &r.channels);
            convert::convert(&open, &settings, MAX_DECODED, &Report)
        }
    }
    .map_err(js)?;
    Ok(Converted {
        codes: serde_json::to_string(&converted.codes).map_err(js)?,
        file: converted.file,
    })
}
