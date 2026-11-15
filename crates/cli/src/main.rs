//! `tickwave`: convert captures into tickwave's recording format, describe a recording, check one
//! against its source, repair one cut off before its end, re-cut one into full-size blocks, and
//! open one in the browser.

mod open;

use std::cell::RefCell;
use std::fs;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use escapement_tickwave_format::annotations::{self, Included};
use escapement_tickwave_format::decode::{DecodeError, FIRST_READ, Footer, Index};
use escapement_tickwave_format::header::{AnalogCoding, ChannelKind, Header, MAGIC};
use escapement_tickwave_format::model::{RateOrigin, Recording, SourceFormat};
use escapement_tickwave_format::recut::{RecutError, Recutter};
use escapement_tickwave_format::repair::{RepairError, Repairer};
use escapement_tickwave_format::samples::coding_name;
use escapement_tickwave_format::verify::verify_limited;
use escapement_tickwave_format::writer::{Compression, WriteOptions, generator_file};
use escapement_tickwave_import::convert::{
    self, Keep, Pass, Progress, Settings, Step, counted, select, set_units,
};
use escapement_tickwave_import::input::map;
use escapement_tickwave_import::saleae::SampleRate;
use escapement_tickwave_import::shape::{Plan, check_time};
use escapement_tickwave_import::sigrok::Skipped;
use escapement_tickwave_import::{drift, merge};
use escapement_tickwave_import::{saleae, sigrok, vcd};
use indicatif::ProgressStyle;
use tracing::span::EnteredSpan;
use tracing::{info, info_span, warn};
use tracing_indicatif::IndicatifLayer;
use tracing_indicatif::span_ext::IndicatifSpanExt;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[derive(Parser)]
#[command(
    name = "tickwave",
    version,
    about = "Convert, describe, verify, repair, re-cut and open tickwave recordings"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// The most to decode from one block, overview block or strip, in MiB. A recording that needs
    /// more is refused, as a page's reader refuses it unless configured.
    #[arg(long, global = true, default_value_t = 64, value_name = "MIB")]
    max_decoded_mib: u16,
}

#[derive(Subcommand)]
enum Command {
    /// Convert a capture into a tickwave recording. `tickwave verify` checks it against the
    /// capture.
    Convert {
        #[command(subcommand)]
        from: Source,
    },
    /// Describe a recording.
    Info {
        file: PathBuf,
        /// Print the header and the block statistics as JSON.
        #[arg(long)]
        json: bool,
        /// Measure the drift between sources that recorded one periodic signal. It decodes every
        /// block; without it, `info` reads the footer alone.
        #[arg(long)]
        drift: bool,
    },
    /// Check a recording against the source it was converted from, edge by edge and sample by
    /// sample. A moved or trimmed channel is checked through its offset and kept span, and a
    /// merged recording through each source's origin and alignment. Without --against, the
    /// recording is checked on its own, as a recorder's file has no other source: every record's
    /// check, every block decoded and held to the blocks its index places, the overview blocks
    /// and strips against the content, and the content id.
    Verify {
        file: PathBuf,
        /// The source: a capture (for a Saleae export, its directory) or a .tickwave recording.
        /// A merged recording takes one for each recording merged into it, in the merge's order.
        #[arg(long)]
        against: Vec<PathBuf>,
    },
    /// Merge recordings onto one time axis, at the least common multiple of their tick rates, or
    /// at the fastest of them where the multiple has a term past 2^53. A capture goes through
    /// `tickwave convert` first. `tickwave verify` checks the merge against its inputs.
    Merge {
        /// Two or more .tickwave recordings, in order.
        #[arg(required = true, num_args = 2..)]
        inputs: Vec<PathBuf>,
        #[arg(short, long)]
        output: PathBuf,
        /// A channel to take, as `<input>:<name>` or `<input>:<name>=<new name>`, inputs counted
        /// from 1. An input with none named gives every channel under its own name; a name
        /// another input took is then `<file stem>/<name>`.
        #[arg(long = "channel", value_name = "N:NAME[=NEW]", value_parser = taken)]
        channels: Vec<(usize, String, Option<String>)>,
        /// Move an input's channels together, as `<input>=<time>`, at the merged rate: a whole
        /// number of merged ticks, or a decimal with a unit, within 0.01 of a merged tick.
        /// Positive moves it later. Or place it by two marks, `<input>=<x>@<y>,<x>@<y>`: each
        /// an event's time in the input's own time and the merged time it takes, which give the
        /// input's move and its stretch.
        #[arg(long = "align", value_name = "N=TIME", value_parser = align)]
        aligns: Vec<(usize, String)>,
        /// Stretch an input's ticks, as `<input>=<a>/<b>` or `<input>=<±N>ppm`: above 1, or a
        /// positive change, says its clock ran slow, so each of its ticks takes longer.
        #[arg(long = "stretch", value_name = "N=RATIO", value_parser = stretch)]
        stretches: Vec<(usize, String)>,
        /// Move an output channel, as `<name>=<time>`, in its source's own ticks: a whole number
        /// of them, or a decimal with a unit, within 0.01 of one. Positive is later.
        #[arg(long = "offset", value_name = "NAME=TIME", value_parser = offset)]
        offsets: Vec<(String, String)>,
        /// Keep only part of an output channel, as `<name>=<from>..<to>`, in its input's own
        /// time, before any alignment or --offset moves it.
        #[arg(long = "keep", value_name = "NAME=FROM..TO", value_parser = keep)]
        keeps: Vec<Keep>,
        /// An analog output channel's unit, as `<name>=<unit>`.
        #[arg(long = "unit", value_parser = pair)]
        units: Vec<(String, String)>,
        #[arg(long)]
        title: Option<String>,
        /// Free metadata, as `<key>=<value>`.
        #[arg(long = "meta", value_parser = pair)]
        meta: Vec<(String, String)>,
        /// The zstd level.
        #[arg(long, default_value_t = 19)]
        level: i32,
        /// The decoded size each block aims for, in KiB: at most 65,536, what a reader holds by
        /// default.
        #[arg(long, default_value_t = 1024, value_parser = clap::value_parser!(u32).range(1..=65_536))]
        block_kib: u32,
        #[command(flatten)]
        and_open: AndOpen,
    },
    /// Write a copy of a recording with its sidecar's files inside, for a single-file share. The
    /// sidecar is checked first.
    Bundle {
        recording: PathBuf,
        /// The hand-written TOML file. Its includes are read from beside it.
        sidecar: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Work with a recording's sidecar annotations.
    Annotations {
        #[command(subcommand)]
        command: AnnotationsCommand,
    },
    /// Finish a recording cut off before its footer, by a crash, a power cut or a full disk: a
    /// copy with every whole record and a footer for them. Each annotation generator's file goes
    /// beside the copy, as `<name>.<source>.jsonl`.
    Repair {
        file: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// The zstd level for the overview repair writes.
        #[arg(long, default_value_t = 19)]
        level: i32,
    },
    /// Write a recording again with blocks of the usual size, as one a recorder flushed often
    /// needs. The copy holds the same content, with the same id, and is checked before it is
    /// written. A recording cut off before its end is finished as `repair` would finish it, and
    /// each annotation generator's file goes beside the copy.
    Recut {
        input: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// The decoded size each block aims for, in KiB: at most 65,536, what a reader holds by
        /// default.
        #[arg(long, default_value_t = 1024, value_parser = clap::value_parser!(u32).range(1..=65_536))]
        block_kib: u32,
        /// The zstd level.
        #[arg(long, default_value_t = 19)]
        level: i32,
    },
    /// Open a recording in the browser, with its sidecar's annotations. The viewer and the files
    /// are served on 127.0.0.1 until Ctrl-C, since the viewer reads the recording as it moves. The
    /// page can save the sidecar's annotations in place.
    Open {
        recording: PathBuf,
        /// The sidecar's hand-written TOML file. Without it, `<name>.annotations.toml` beside the
        /// recording is taken if there is one.
        #[arg(long)]
        annotations: Option<PathBuf>,
        #[command(flatten)]
        serve: Serve,
    },
}

#[derive(Args)]
struct Serve {
    /// The port to serve on. 0 lets the system choose one.
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// Print the viewer's address without starting a browser.
    #[arg(long)]
    no_browser: bool,
}

#[derive(Args)]
struct AndOpen {
    /// Open the recording in the browser once it is written, as `tickwave open` does, and serve
    /// it until Ctrl-C.
    #[arg(long)]
    open: bool,
    /// With --open: the port to serve on. Without it, the system chooses one.
    #[arg(long, requires = "open")]
    port: Option<u16>,
    /// With --open: print the viewer's address without starting a browser.
    #[arg(long, requires = "open")]
    no_browser: bool,
}

impl AndOpen {
    /// The viewer, if the recording is to be opened: had before a conversion starts, so a build
    /// without one is refused before the work rather than after it.
    fn viewer(&self) -> Result<Option<Box<dyn open::Site>>> {
        self.open.then(open::viewer).transpose()
    }

    /// Opens `recording` with `viewer`, if there is one.
    fn serve(&self, viewer: Option<Box<dyn open::Site>>, recording: &Path) -> Result<()> {
        match viewer {
            Some(site) => open_file(
                site,
                recording,
                None,
                self.port.unwrap_or(0),
                !self.no_browser,
            ),
            None => Ok(()),
        }
    }
}

#[derive(Subcommand)]
enum AnnotationsCommand {
    /// Check a sidecar against its recording: ids, refs, includes, channels, times and the
    /// recording id. Exits non-zero, listing every problem, if any is found.
    Check {
        recording: PathBuf,
        /// The hand-written TOML file. Its includes are read from beside it.
        sidecar: PathBuf,
        /// Print the resolved annotations as JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum Source {
    /// Saleae Logic 2's binary export: a directory with one file a channel.
    Saleae {
        dir: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// A channel to take, as `<file stem>=<name>`, such as `digital_3_0=SWCLK`. Channels not
        /// named are left out.
        #[arg(long = "channel", value_parser = pair, required = true)]
        channels: Vec<(String, String)>,
        /// The sample rate, in hertz, with an optional k, M or G, such as 50M. A version 0
        /// export (from a Logic 8, Logic Pro 8 or Logic Pro 16) doesn't record its digital
        /// channels' rate: without this, it is inferred from the export's edges.
        #[arg(long, value_name = "HZ")]
        rate: Option<SampleRate>,
        #[command(flatten)]
        common: Common,
        #[command(flatten)]
        and_open: AndOpen,
    },
    /// A sigrok session file (`.sr`), as PulseView, sigrok-cli and verge write them.
    Sigrok {
        file: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// A channel to take, as `<probe name>=<name>`. Without any, every enabled channel goes
        /// in under its own name.
        #[arg(long = "channel", value_parser = pair)]
        channels: Vec<(String, String)>,
        #[command(flatten)]
        common: Common,
        #[command(flatten)]
        and_open: AndOpen,
    },
    /// A value change dump (`.vcd`). One-bit signals only; the rest are listed as skipped.
    Vcd {
        file: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// A signal to take, as `<scope path>=<name>`, such as `top.dut.clk=CLK`. Without any,
        /// every one-bit signal goes in under its scope path.
        #[arg(long = "channel", value_parser = pair)]
        channels: Vec<(String, String)>,
        #[command(flatten)]
        common: Common,
        #[command(flatten)]
        and_open: AndOpen,
    },
}

#[derive(Args)]
struct Common {
    #[arg(long)]
    title: Option<String>,
    /// Free metadata, as `<key>=<value>`, such as `probe=verge`.
    #[arg(long = "meta", value_parser = pair)]
    meta: Vec<(String, String)>,
    /// The zstd level.
    #[arg(long, default_value_t = 19)]
    level: i32,
    /// The decoded size each block aims for, in KiB: at most 65,536, what a reader holds by default.
    #[arg(long, default_value_t = 1024, value_parser = clap::value_parser!(u32).range(1..=65_536))]
    block_kib: u32,
    /// An analog channel's unit, as `<name>=<unit>`, where the source does not state one.
    #[arg(long = "unit", value_parser = pair)]
    units: Vec<(String, String)>,
    /// Store analog values as f32 even where exact integer codes reproduce them.
    #[arg(long)]
    no_codes: bool,
    /// Move a channel along the time axis, as `<name>=<time>`, such as `SWCLK=-40ns`. A positive
    /// time moves it later. The name is the one stored, after --channel. The time is the source's:
    /// a whole number of its ticks, or a decimal with a unit (s, ms, us, ns, ps), within 0.01 of a
    /// tick.
    #[arg(long = "offset", value_name = "NAME=TIME", value_parser = offset)]
    offsets: Vec<(String, String)>,
    /// Keep only part of a channel, as `<name>=<from>..<to>`, such as `SWCLK=1.2ms..2ms`; either
    /// end may be left open, as in `SWCLK=1.2ms..`. The times are the channel's own in the
    /// source, before any --offset moves it, and round to the nearest tick.
    #[arg(long = "keep", value_name = "NAME=FROM..TO", value_parser = keep)]
    keeps: Vec<Keep>,
}

fn pair(s: &str) -> Result<(String, String), String> {
    let (k, v) = s
        .split_once('=')
        .ok_or_else(|| format!("expected <key>=<value>, got {s:?}"))?;
    if k.is_empty() || v.is_empty() {
        return Err(format!("expected <key>=<value>, got {s:?}"));
    }
    Ok((k.to_owned(), v.to_owned()))
}

/// `<input>:<name>` or `<input>:<name>=<new>`, the input counted from 1.
fn taken(s: &str) -> Result<(usize, String, Option<String>), String> {
    let wrong = || format!("expected <input>:<name> or <input>:<name>=<new name>, got {s:?}");
    let (input, rest) = s.split_once(':').ok_or_else(wrong)?;
    let input: usize = input.parse().ok().filter(|&n| n >= 1).ok_or_else(wrong)?;
    let (name, new) = match rest.split_once('=') {
        Some((name, new)) => (name, Some(new)),
        None => (rest, None),
    };
    if name.is_empty() || new.is_some_and(str::is_empty) {
        return Err(wrong());
    }
    Ok((input, name.to_owned(), new.map(str::to_owned)))
}

/// `<input>=<time>` or `<input>=<x>@<y>,<x>@<y>`, the input counted from 1.
fn align(s: &str) -> Result<(usize, String), String> {
    let wrong = || format!("expected <input>=<time> or <input>=<x>@<y>,<x>@<y>, got {s:?}");
    let (input, time) = s.split_once('=').ok_or_else(wrong)?;
    let input: usize = input.parse().ok().filter(|&n| n >= 1).ok_or_else(wrong)?;
    match time.split_once(',') {
        None => check_time(time)?,
        Some((a, b)) => {
            for mark in [a, b] {
                let (x, y) = mark.split_once('@').ok_or_else(wrong)?;
                check_time(x)?;
                check_time(y)?;
            }
        }
    }
    Ok((input, time.to_owned()))
}

/// `<input>=<a>/<b>` or `<input>=<±N>ppm`, the input counted from 1.
fn stretch(s: &str) -> Result<(usize, String), String> {
    let wrong = || format!("expected <input>=<a>/<b> or <input>=<±N>ppm, got {s:?}");
    let (input, ratio) = s.split_once('=').ok_or_else(wrong)?;
    let input: usize = input.parse().ok().filter(|&n| n >= 1).ok_or_else(wrong)?;
    merge::parse_stretch(ratio)?;
    Ok((input, ratio.to_owned()))
}

fn offset(s: &str) -> Result<(String, String), String> {
    let (name, time) = pair(s).map_err(|_| format!("expected <name>=<time>, got {s:?}"))?;
    check_time(&time)?;
    Ok((name, time))
}

fn keep(s: &str) -> Result<Keep, String> {
    let wrong = || format!("expected <name>=<from>..<to>, got {s:?}");
    let (name, span) = s
        .split_once('=')
        .filter(|(name, _)| !name.is_empty())
        .ok_or_else(wrong)?;
    let (from, to) = span.split_once("..").ok_or_else(wrong)?;
    let end = |time: &str| -> Result<Option<String>, String> {
        if time.trim().is_empty() {
            return Ok(None);
        }
        check_time(time)?;
        Ok(Some(time.to_owned()))
    };
    Ok((name.to_owned(), end(from)?, end(to)?))
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        // A reader that stops early, as `head` does, closes the pipe: it has what it wanted.
        Err(e)
            if e.chain().any(|c| {
                c.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe)
            }) =>
        {
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Error: {e:?}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let indicatif = IndicatifLayer::new();
    tracing_subscriber::registry()
        .with(LevelFilter::INFO)
        .with(tracing_subscriber::fmt::layer().with_writer(indicatif.get_stderr_writer()))
        .with(indicatif)
        .init();
    let cli = Cli::parse();
    execute(cli.command, usize::from(cli.max_decoded_mib) << 20)
}

/// Runs `command`, decoding at most `max` bytes from one frame.
fn execute(command: Command, max: usize) -> Result<()> {
    match command {
        Command::Convert {
            from:
                Source::Saleae {
                    dir,
                    output,
                    channels,
                    rate,
                    common,
                    and_open,
                },
        } => {
            let viewer = and_open.viewer()?;
            let export =
                convert::check_saleae(saleae_headers(&dir, &channels, rate)?, &Bars::default())
                    .map_err(|e| match e {
                        convert::Error::Saleae(saleae::Error::NoRate(_)) => {
                            anyhow::anyhow!("{e}, with --rate")
                        }
                        e => e.into(),
                    })?;
            let recording = export.recording();
            if recording.sources[0].rate == Some(RateOrigin::Inferred) {
                info!(
                    "sample rate {} Hz, inferred from the edges",
                    recording.timebase.num()
                );
            }
            convert(&|| Ok(export.recording()), &output, &common)?;
            and_open.serve(viewer, &output)
        }
        Command::Convert {
            from:
                Source::Sigrok {
                    file,
                    output,
                    channels,
                    common,
                    and_open,
                },
        } => {
            let viewer = and_open.viewer()?;
            report_skipped(&sigrok::read(&file)?.skipped);
            convert(
                &|| select(sigrok::read(&file)?.recording, &channels),
                &output,
                &common,
            )?;
            and_open.serve(viewer, &output)
        }
        Command::Convert {
            from:
                Source::Vcd {
                    file,
                    output,
                    channels,
                    common,
                    and_open,
                },
        } => {
            let viewer = and_open.viewer()?;
            report_skipped(&vcd::read(&file)?.skipped);
            convert(
                &|| select(vcd::read(&file)?.recording, &channels),
                &output,
                &common,
            )?;
            and_open.serve(viewer, &output)
        }
        Command::Info { file, json, drift } => {
            let (_, len, head) = read_footer(&file)?;
            write_info(
                &mut std::io::stdout().lock(),
                head,
                len,
                json,
                &*map(&file)?,
                drift,
            )
        }
        Command::Verify { file, against } => verify_file(&file, &against, max),
        Command::Merge {
            inputs,
            output,
            channels,
            aligns,
            stretches,
            offsets,
            keeps,
            units,
            title,
            meta,
            level,
            block_kib,
            and_open,
        } => {
            let viewer = and_open.viewer()?;
            let mut settings = merge::Settings {
                inputs: vec![merge::InputSettings::default(); inputs.len()],
                offsets,
                keeps,
                units,
                title,
                meta: meta.into_iter().collect(),
                level,
                block_kib,
                stem: stem_of(&output),
            };
            for (n, name, new) in channels {
                settings
                    .inputs
                    .get_mut(n - 1)
                    .with_context(|| {
                        format!("--channel {n}:{name}: there are {} inputs", inputs.len())
                    })?
                    .channels
                    .push((name, new));
            }
            for (n, time) in aligns {
                let input = settings.inputs.get_mut(n - 1).with_context(|| {
                    format!("--align {n}={time}: there are {} inputs", inputs.len())
                })?;
                if input.align.replace(time).is_some() {
                    bail!("input {n} is given an alignment twice");
                }
            }
            for (n, ratio) in stretches {
                let input = settings.inputs.get_mut(n - 1).with_context(|| {
                    format!("--stretch {n}={ratio}: there are {} inputs", inputs.len())
                })?;
                if input.stretch.replace(ratio).is_some() {
                    bail!("input {n} is given a stretch twice");
                }
            }
            merge_files(&inputs, &output, &settings)?;
            and_open.serve(viewer, &output)
        }
        Command::Bundle {
            recording,
            sidecar,
            output,
        } => bundle_file(&recording, &sidecar, &output),
        Command::Annotations {
            command:
                AnnotationsCommand::Check {
                    recording,
                    sidecar,
                    json,
                },
        } => check_annotations(&recording, &sidecar, json),
        Command::Repair {
            file,
            output,
            level,
        } => repair_file(&file, &output, level),
        Command::Recut {
            input,
            output,
            block_kib,
            level,
        } => recut_file(
            &input,
            &output,
            WriteOptions {
                level,
                block_bytes: block_kib as usize * 1024,
                compression: Compression::Offload,
                ..WriteOptions::default()
            },
        ),
        Command::Open {
            recording,
            annotations,
            serve,
        } => open_file(
            open::viewer()?,
            &recording,
            annotations.as_deref(),
            serve.port,
            !serve.no_browser,
        ),
    }
}

/// Reads the headers of the named channels' files in a Saleae export's directory.
fn saleae_headers(
    dir: &Path,
    channels: &[(String, String)],
    rate: Option<SampleRate>,
) -> Result<saleae::Headers> {
    Ok(saleae::Headers::read(
        saleae::export_files(dir)?,
        channels,
        rate,
    )?)
}

/// Draws each pass as a span with a bar, and logs inside it.
#[derive(Default)]
struct Bars {
    entered: RefCell<Option<EnteredSpan>>,
}

impl Progress for Bars {
    fn start(&self, pass: Pass<'_>, total: u64) -> Step {
        let span = match pass {
            Pass::Check => info_span!("check"),
            Pass::Codes(channel) => info_span!("codes", channel = %channel),
            Pass::Encode => info_span!("encode"),
            Pass::Verify => info_span!("verify"),
        };
        span.pb_set_style(
            &ProgressStyle::with_template("{span_name} {wide_bar} {pos}/{len} {per_sec} {eta}")
                .expect("a fixed template"),
        );
        span.pb_set_length(total);
        *self.entered.borrow_mut() = Some(span.clone().entered());
        Arc::new(move |n| span.pb_inc(n))
    }

    fn end(&self, _: Pass<'_>) {
        self.entered.borrow_mut().take();
    }
}

fn report_skipped(skipped: &[Skipped]) {
    for s in skipped {
        warn!("left out {}: {}", s.name, s.reason);
    }
}

impl Common {
    fn settings(&self) -> Settings {
        Settings {
            title: self.title.clone(),
            meta: self.meta.iter().cloned().collect(),
            level: self.level,
            block_kib: self.block_kib,
            units: self.units.clone(),
            codes: !self.no_codes,
            offsets: self.offsets.clone(),
            keeps: self.keeps.clone(),
        }
    }
}

/// Converts what `open` reads and writes it.
fn convert(
    open: &dyn Fn() -> Result<Recording, convert::Error>,
    output: &Path,
    common: &Common,
) -> Result<()> {
    let started = Instant::now();
    let settings = common.settings();
    let converted = write_atomically(output, |file| {
        Ok(convert::convert(open, &settings, &Bars::default(), file)?)
    })?;
    for (name, bits) in &converted.codes {
        match bits {
            Some(bits) => info!("{name}: {bits}-bit codes"),
            None => info!("{name}: no exact codes, stored as f32"),
        }
    }
    let len = fs::metadata(output)?.len();
    info!(
        "{}: {:.2} MB, {} blocks, {} in {:.1} s",
        output.display(),
        len as f64 / 1e6,
        blocks(&converted.header),
        converted.header.content_id,
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

/// Writes through `write` beside the destination, then syncs and renames, so a failed run never
/// leaves half a file.
fn write_atomically<T>(path: &Path, write: impl FnOnce(&mut fs::File) -> Result<T>) -> Result<T> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".partial");
    let tmp = PathBuf::from(tmp);
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)
        .with_context(|| format!("creating {}", tmp.display()))?;
    let written = write(&mut file).and_then(|t| {
        file.sync_all()
            .with_context(|| format!("writing {}", tmp.display()))?;
        Ok(t)
    });
    match written {
        Ok(t) => {
            fs::rename(&tmp, path).with_context(|| format!("renaming to {}", path.display()))?;
            Ok(t)
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Every channel's blocks.
fn blocks(h: &Header) -> u64 {
    h.channels.iter().map(|c| c.blocks).sum()
}

/// The recording at `path`, open, its length, and its footer, read from its end.
fn read_footer(path: &Path) -> Result<(fs::File, u64, Footer)> {
    let mut file = fs::File::open(path).with_context(|| format!("reading {}", path.display()))?;
    let len = file.metadata()?.len();
    let mut from = len.saturating_sub(FIRST_READ as u64);
    loop {
        file.seek(SeekFrom::Start(from))?;
        let mut tail = Vec::new();
        file.read_to_end(&mut tail)?;
        match Footer::parse(&tail, len) {
            Ok(footer) => return Ok((file, len, footer)),
            Err(DecodeError::NeedFrom(at)) if at < from => from = at,
            Err(DecodeError::Unfinished) => bail!(
                "{}: {}; `tickwave repair` finishes a recording cut off before its end",
                path.display(),
                DecodeError::Unfinished
            ),
            Err(e) => return Err(e).with_context(|| format!("parsing {}", path.display())),
        }
    }
}

/// Copies the first `keep` bytes of `from` into `to`, then `tail`.
fn copy_then(from: &mut fs::File, keep: u64, tail: &[u8], to: &mut fs::File) -> Result<()> {
    from.seek(SeekFrom::Start(0))?;
    let copied = std::io::copy(&mut from.take(keep), to)?;
    if copied != keep {
        bail!("the file changed while it was read");
    }
    to.write_all(tail)?;
    Ok(())
}

/// A tick as seconds, to the decimal place of one tick and no further: as many digits as the
/// timebase carries. Ticks where the arithmetic would pass u128.
fn seconds(ticks: i64, header: &Header) -> String {
    let (num, den) = (
        u128::from(header.timebase.num()),
        u128::from(header.timebase.den()),
    );
    let ticks = ticks.max(0) as u128;
    let mut places = 0;
    while 10u128.pow(places) * den < num {
        places += 1;
    }
    let unit = 10u128.pow(places);
    let Some(scaled) = ticks
        .checked_mul(den)
        .and_then(|t| t.checked_mul(unit))
        .and_then(|t| t.checked_mul(2))
        .map(|t| (t + num) / (2 * num))
    else {
        return format!("{ticks} ticks");
    };
    match places {
        0 => format!("{scaled} s"),
        p => format!(
            "{}.{:0width$} s",
            scaled / unit,
            scaled % unit,
            width = p as usize
        ),
    }
}

/// Frames below this many bytes are counted by length for `info`'s median; one this long or
/// longer takes as many bytes of the file, so the longer ones held one by one are few.
const SHORT_FRAME: usize = 1 << 16;

/// The frames `info` describes: every block's length for the median, each channel's blocks and
/// their bytes, and each overview channel's rows in overview blocks and their bytes.
struct Sizes {
    short: Vec<u64>,
    long: Vec<u32>,
    blocks: Vec<(u64, u64)>,
    overview: Vec<(u64, u64)>,
}

impl Sizes {
    fn new(footer: &Footer) -> Self {
        Self {
            short: vec![0; SHORT_FRAME],
            long: Vec::new(),
            blocks: vec![(0, 0); footer.header.channels.len()],
            overview: vec![(0, 0); footer.channels.len()],
        }
    }

    fn block(&mut self, c: usize, length: u32) {
        match self.short.get_mut(length as usize) {
            Some(n) => *n += 1,
            None => self.long.push(length),
        }
        self.blocks[c].0 += 1;
        self.blocks[c].1 += u64::from(length);
    }

    fn row(&mut self, o: usize, length: u32) {
        self.overview[o].0 += 1;
        self.overview[o].1 += u64::from(length);
    }

    /// Every channel's blocks a page at a time, each page checked as a whole index read is and
    /// dropped once counted, so a recording of any length is described in bounded memory.
    fn read(footer: &Footer, input: &dyn escapement_tickwave_import::input::Input) -> Result<Self> {
        let mut sizes = Self::new(footer);
        let mut fetch = escapement_tickwave_import::recording::fetch(input);
        for (c, ch) in footer.channels.iter().enumerate() {
            if let Some(table) = &ch.blocks {
                table.visit(&mut fetch, |at, page| {
                    for b in footer.block_page(c, &at, &page, None)? {
                        sizes.block(c, b.row.length);
                    }
                    Ok::<_, DecodeError>(ControlFlow::Continue(()))
                })?;
            }
            for (li, (_, table)) in ch.levels.iter().enumerate() {
                table.visit(&mut fetch, |at, page| {
                    for r in footer.level_page(c, li, &at, &page)? {
                        sizes.row(c, r.row.length);
                    }
                    Ok::<_, DecodeError>(ControlFlow::Continue(()))
                })?;
            }
        }
        Ok(sizes)
    }

    fn of_index(index: &Index) -> Self {
        let mut sizes = Self::new(&index.footer);
        for (c, blocks) in index.blocks.iter().enumerate() {
            for b in blocks {
                sizes.block(c, b.row.length);
            }
        }
        for (o, levels) in index.levels.iter().enumerate() {
            for r in levels.iter().flatten() {
                sizes.row(o, r.row.length);
            }
        }
        sizes
    }

    fn count(&self) -> u64 {
        self.blocks.iter().map(|b| b.0).sum()
    }

    fn total(&self) -> u64 {
        self.blocks.iter().map(|b| b.1).sum()
    }

    fn min(&self) -> Option<u32> {
        let short = self.short.iter().position(|&n| n > 0).map(|l| l as u32);
        short.or_else(|| self.long.iter().min().copied())
    }

    fn max(&self) -> Option<u32> {
        let long = self.long.iter().max().copied();
        long.or_else(|| self.short.iter().rposition(|&n| n > 0).map(|l| l as u32))
    }

    /// The length at place n / 2 of every block's in order, 0 for none.
    fn median(&mut self) -> u32 {
        let mut rank = self.count() / 2;
        for (length, &n) in self.short.iter().enumerate() {
            if rank < n {
                return length as u32;
            }
            rank -= n;
        }
        self.long.sort_unstable();
        self.long.get(rank as usize).copied().unwrap_or(0)
    }
}

/// What `info` prints of the recording `head` ends, `len` bytes long. It reads the index from
/// `input` a page at a time, and only `drift` reads blocks, holding the index whole to do so.
fn write_info(
    out: &mut impl Write,
    mut head: Footer,
    len: u64,
    json: bool,
    input: &dyn escapement_tickwave_import::input::Input,
    drift: bool,
) -> Result<()> {
    let fetch = || escapement_tickwave_import::recording::fetch(input);
    let bundled = head.read_bundle(fetch())?;
    let (head, drifts, mut sizes) = if drift {
        let index = Index::read(head, fetch())?;
        let drifts = drift::of_index(input, &index)?;
        let sizes = Sizes::of_index(&index);
        (index.footer, Some(drifts), sizes)
    } else {
        head.load_lists(fetch())?;
        let sizes = Sizes::read(&head, input)?;
        (head, None, sizes)
    };
    let (head, h) = (&head, &head.header);
    let candidates = drift::candidates_in(h, &sizes.blocks);
    let (median, blocks_total, blocks, min, max) = (
        sizes.median(),
        sizes.total(),
        sizes.count(),
        sizes.min(),
        sizes.max(),
    );
    let overview_count: u64 = sizes.overview.iter().map(|r| r.0).sum();
    let overview_total: u64 = sizes.overview.iter().map(|r| r.1).sum();
    if json {
        let report = serde_json::json!({
            "header": h,
            "file_bytes": len,
            "footer_bytes": head.trailer.footer_len,
            "block_bytes": { "total": blocks_total, "min": min, "median": median, "max": max },
            "overview_block_bytes": { "count": overview_count, "total": overview_total },
            "drift_candidates": candidates.map(|c| serde_json::json!({
                "channels": c.channels,
                "blocks": c.blocks,
                "bytes": c.bytes,
            })),
            "drift": drifts.as_ref().map(|drifts| drifts.iter().map(|d| serde_json::json!({
                "channels": [&h.channels[d.a.channel].name, &h.channels[d.b.channel].name],
                "ppm": d.ppm,
                "ppm_se": d.ppm_se,
                "periods_s": [d.a.period, d.b.period],
                "rises": [d.a.rises, d.b.rises],
                "rms_s": [d.a.rms, d.b.rms],
            })).collect::<Vec<_>>()),
        });
        writeln!(out, "{}", serde_json::to_string_pretty(&report)?)?;
        return Ok(());
    }
    if let Some(title) = &h.title {
        writeln!(out, "{title}")?;
    }
    writeln!(
        out,
        "{} at {}/{} ticks a second, {} channels, {}",
        seconds(h.length, h),
        h.timebase.num(),
        h.timebase.den(),
        h.channels.len(),
        h.content_id
    )?;
    for (i, s) in h.sources.iter().enumerate() {
        let align = s
            .align
            .map_or(String::new(), |a| format!(", aligned by {a}"));
        let files = if s.files.is_empty() {
            String::new()
        } else {
            format!(": {}", s.files.join(", "))
        };
        // A merged recording's source ticks slower than the recording: each of its ticks is m of
        // the recording's.
        let m = h
            .m(i)
            .filter(|m| m.num() != m.den())
            .map_or(String::new(), |m| match m.den() {
                1 => format!(" ({} of the recording's ticks each)", m.num()),
                d => format!(" ({}/{d} of the recording's ticks each)", m.num()),
            });
        let stretch = s.stretch.map_or(String::new(), |st| {
            let ppm = (st.num() as f64 / st.den() as f64 - 1.0) * 1e6;
            format!(", stretched {}/{} ({ppm:+.1} ppm)", st.num(), st.den())
        });
        writeln!(
            out,
            "  source {i}: {} at {}/{} ticks a second{m}, its tick 0 at {}{align}{stretch}{files}",
            s.format,
            s.timebase.num(),
            s.timebase.den(),
            s.origin
        )?;
        // A merged recording's source keeps the title and meta of the recording it came from.
        if let Some(title) = &s.title {
            writeln!(out, "    from {title}")?;
        }
        for (k, v) in &s.meta {
            writeln!(out, "    {k} = {v}")?;
        }
    }
    let several = h.sources.len() > 1;
    for (i, c) in h.channels.iter().enumerate() {
        let from = if several {
            format!(", source {}", c.source)
        } else {
            String::new()
        };
        match &c.data {
            ChannelKind::Digital {
                initial,
                end,
                edges,
                ..
            } => writeln!(
                out,
                "  {:<12} digital, {edges} edges, starts {}, known to {} ({}{from})",
                c.name,
                u8::from(*initial),
                h.mapping(i)
                    .and_then(|m| m.axis(*end))
                    .map_or_else(|| format!("source tick {end}"), |t| seconds(t, h)),
                c.source_name
            )?,
            ChannelKind::Analog {
                unit,
                count,
                coding,
                breaks,
                ..
            } => {
                let lost: u64 = breaks.iter().map(|b| b.lost).sum();
                let breaks = match (breaks.len(), lost) {
                    (0, _) => String::new(),
                    (n, 0) => format!(", {n} breaks"),
                    (n, lost) => format!(", {n} breaks, {lost} samples lost"),
                };
                let unit = if unit.is_empty() {
                    String::new()
                } else {
                    format!(" in {unit}")
                };
                writeln!(
                    out,
                    "  {:<12} analog, {count} samples{unit}{breaks}, {} ({}{from})",
                    c.name,
                    describe_coding(*coding),
                    c.source_name
                )?
            }
        }
    }
    for d in &h.derived {
        writeln!(
            out,
            "  {:<12} derived, {} × {} in {}",
            d.name, d.product[0], d.product[1], d.unit
        )?;
    }
    writeln!(
        out,
        "{:.3} MB: footer {} bytes, {blocks} blocks of {}-{} bytes, median {median}",
        len as f64 / 1e6,
        head.trailer.footer_len,
        min.unwrap_or(0),
        max.unwrap_or(0),
    )?;
    let names = h
        .channels
        .iter()
        .map(|c| &c.name)
        .chain(h.derived.iter().map(|d| &d.name));
    for (o, name) in names.enumerate() {
        let strip = head.channels[o].strip_shift;
        let levels = h.levels(o).unwrap_or_default();
        if levels.is_empty() {
            writeln!(out, "  {name:<12} overview strip at 2^{strip}")?;
            continue;
        }
        let shifts: Vec<String> = levels.iter().map(|l| format!("2^{}", l.shift)).collect();
        let (rows, bytes) = sizes.overview[o];
        writeln!(
            out,
            "  {name:<12} overview levels {} in {rows} blocks, {bytes} bytes, strip at 2^{strip}",
            shifts.join(", "),
        )?;
    }
    for (k, v) in &h.meta {
        writeln!(out, "  {k} = {v}")?;
    }
    for (name, text) in bundled.iter().flatten() {
        let lines = text.lines().filter(|l| !l.trim().is_empty()).count();
        writeln!(out, "  bundled {name}, {lines} lines")?;
    }
    match (&drifts, candidates) {
        (Some(drifts), _) => {
            for d in drifts {
                writeln!(out, "{}", describe_drift(h, d))?;
            }
        }
        (None, Some(c)) => writeln!(
            out,
            "drift: {} digital channels from two or more sources may share a signal; `tickwave \
             info --drift` measures it, decoding their {} blocks, {:.3} MB",
            c.channels,
            c.blocks,
            c.bytes as f64 / 1e6
        )?,
        (None, None) => {}
    }
    Ok(())
}

/// A drift as `info` and `verify` state it.
fn describe_drift(h: &Header, d: &drift::Drift) -> String {
    let name = |p: &drift::Periodic| &h.channels[p.channel].name;
    format!(
        "drift: {} (source {}) against {} (source {}): {:+.1} ± {:.1} ppm, the stretch source {} \
         would take to tick as source {} does; periods {:.6} ms and {:.6} ms over {} and {} rises, \
         {:.1} µs and {:.1} µs rms off a straight line",
        name(&d.a),
        d.a.source,
        name(&d.b),
        d.b.source,
        d.ppm,
        d.ppm_se,
        d.a.source,
        d.b.source,
        d.a.period * 1e3,
        d.b.period * 1e3,
        d.a.rises,
        d.b.rises,
        d.a.rms * 1e6,
        d.b.rms * 1e6,
    )
}

/// A coding as `info` shows it.
fn describe_coding(coding: AnalogCoding) -> String {
    match coding {
        AnalogCoding::Int(int) => {
            let ratio =
                |r: escapement_tickwave_format::header::Exact| format!("{}/{}", r.num(), r.den());
            let scale = int
                .scale
                .map_or(String::new(), |s| format!(", scale {}", ratio(s)));
            let offset = int
                .offset
                .map_or(String::new(), |o| format!(", offset {}", ratio(o)));
            format!("{}{scale}{offset}", coding_name(coding))
        }
        other => coding_name(other),
    }
}

/// Whether `path` is a tickwave recording: a file that starts with the magic.
fn is_recording(path: &Path) -> bool {
    let mut magic = [0; 8];
    fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .is_ok_and(|()| magic == MAGIC)
}

/// The capture at `against` imported as the source `source` of the recording `h` says it was: its
/// channels of that source by their source names, under the recording's names and units.
fn capture(h: &Header, source: usize, against: &Path) -> Result<Recording> {
    let wanted: Vec<(String, String)> = h
        .channels
        .iter()
        .filter(|c| c.source as usize == source)
        .map(|c| (c.source_name.clone(), c.name.clone()))
        .collect();
    let stated = &h.sources[source];
    let saleae = |rate| -> Result<Recording> {
        Ok(
            convert::check_saleae(saleae_headers(against, &wanted, rate)?, &Bars::default())?
                .recording(),
        )
    };
    let recording = match &stated.format {
        SourceFormat::SaleaeBinary1 => saleae(None)?,
        // A version 0 export's digital files don't record the rate the recording was made at.
        SourceFormat::SaleaeBinary0 => saleae(Some(
            SampleRate::of_timebase(stated.timebase)
                .context("the source's timebase is not a whole number of hertz")?,
        ))?,
        SourceFormat::SigrokSr => select(sigrok::read(against)?.recording, &wanted)?,
        SourceFormat::Vcd => select(vcd::read(against)?.recording, &wanted)?,
        SourceFormat::Other(name) => bail!(
            "{}: verifying against {name} sources is not supported; give its .tickwave",
            against.display()
        ),
    };
    let units: Vec<(String, String)> = h
        .channels
        .iter()
        .filter(|c| c.source as usize == source)
        .filter_map(|c| match &c.data {
            ChannelKind::Analog { unit, .. } => Some((c.name.clone(), unit.clone())),
            ChannelKind::Digital { .. } => None,
        })
        .collect();
    Ok(set_units(recording, &units)?)
}

fn verify_file(file: &Path, against: &[PathBuf], max: usize) -> Result<()> {
    let (mut opened, _, head) = read_footer(file)?;
    let h = &head.header;
    let one_capture = h.sources.len() == 1 && against.len() == 1 && !is_recording(&against[0]);
    let source = if against.is_empty() {
        // The recording as its index places its blocks, against its records read in order.
        let input = map(file).with_context(|| format!("reading {}", file.display()))?;
        escapement_tickwave_import::recording::read(input)
            .with_context(|| format!("reading {}", file.display()))?
            .1
    } else if one_capture {
        // A conversion: its moves and cuts from the header, its origin among them.
        let source = capture(h, 0, &against[0])?;
        let mut plan = Plan::of_header(&source, h)
            .context("the recording's moves and cuts do not fit its source")?;
        if plan.cuts_edges() {
            plan.count(capture(h, 0, &against[0])?);
        }
        plan.apply(source)
    } else {
        let mut recordings = Vec::with_capacity(against.len());
        let mut at = 0;
        for path in against {
            let recording = if is_recording(path) {
                let input = map(path).with_context(|| format!("reading {}", path.display()))?;
                escapement_tickwave_import::recording::read(input)
                    .with_context(|| format!("reading {}", path.display()))?
                    .1
            } else {
                if at >= h.sources.len() {
                    bail!(
                        "{}: the recording has {} sources, and the recordings before cover them",
                        path.display(),
                        h.sources.len()
                    );
                }
                capture(h, at, path)?
            };
            at += recording.sources.len();
            recordings.push(recording);
        }
        merge::expected(h, recordings).context("the recording does not follow from its inputs")?
    };
    let bars = Bars::default();
    opened.seek(SeekFrom::Start(0))?;
    verify_limited(
        BufReader::with_capacity(1 << 20, opened),
        counted(source, &bars, Pass::Verify),
        max,
    )
    .context("the recording does not match its source")?;
    bars.end(Pass::Verify);
    if against.is_empty() {
        info!(
            "{}: checks on its own: its records, blocks, overview and content id",
            file.display()
        );
    } else {
        let names: Vec<String> = against.iter().map(|p| p.display().to_string()).collect();
        info!("{}: matches {}", file.display(), names.join(", "));
    }
    let (h, drifts) = drift::of_file(map(file)?)?;
    for d in &drifts {
        info!("{}", describe_drift(&h, d));
    }
    Ok(())
}

/// A path's file name without its extension.
fn stem_of(path: &Path) -> String {
    path.file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned())
}

/// The input at `path` for a merge: its bundled sidecar, or else the one beside it, as `tickwave
/// open` finds one.
/// An input's recording and its sidecar; the caller says which file a failure is about.
fn merge_input(path: &Path) -> Result<merge::Input> {
    if !is_recording(path) {
        bail!("it isn't a tickwave recording: convert a capture with `tickwave convert` first");
    }
    let mut input = merge::Input::read(map(path)?, stem_of(path))?;
    if input.bundled.is_none() {
        let mut name = path.file_stem().unwrap_or_default().to_owned();
        name.push(".annotations.toml");
        let beside = path.with_file_name(name);
        if beside.is_file() {
            input.bundled = Some(read_sidecar(&beside)?.into_iter().collect());
        }
    }
    Ok(input)
}

fn merge_files(inputs: &[PathBuf], output: &Path, settings: &merge::Settings) -> Result<()> {
    let started = Instant::now();
    let open = || -> Result<Vec<merge::Input>, merge::Error> {
        inputs
            .iter()
            .map(|p| {
                merge_input(p).map_err(|e| {
                    merge::Error::Io(
                        p.display().to_string(),
                        std::io::Error::other(format!("{e:#}")),
                    )
                })
            })
            .collect()
    };
    let merged = write_atomically(output, |file| {
        Ok(merge::merge(&open, settings, &Bars::default(), file)?)
    })?;
    for note in &merged.notes {
        if note.warns() {
            warn!("{note}");
        } else {
            info!("{note}");
        }
    }
    let len = fs::metadata(output)?.len();
    info!(
        "{}: {:.2} MB, {} blocks, {} in {:.1} s",
        output.display(),
        len as f64 / 1e6,
        blocks(&merged.header),
        merged.header.content_id,
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

/// A sidecar's files: the hand-written TOML file's name and text, then each file it includes.
type SidecarFiles = Vec<(String, String)>;

/// Reads a sidecar and checks it against the recording whose header is `header`. Prints every
/// problem and fails if there is any.
fn load_sidecar(
    header: &Header,
    recording: &Path,
    sidecar: &Path,
) -> Result<(Vec<annotations::Annotation>, SidecarFiles)> {
    let files = read_sidecar(sidecar)?;
    let resolved = resolve_sidecar(header, recording, sidecar, &files)?;
    Ok((resolved, files))
}

/// A sidecar's hand-written file and the included files that can be read.
fn read_sidecar(sidecar: &Path) -> Result<SidecarFiles> {
    let hand =
        fs::read_to_string(sidecar).with_context(|| format!("reading {}", sidecar.display()))?;
    Ok(with_includes(sidecar, hand))
}

/// The files a hand-written file's text includes, or None when its TOML doesn't parse.
fn include_names(hand: &str) -> Option<Vec<String>> {
    #[derive(serde::Deserialize)]
    struct Includes {
        #[serde(default)]
        include: Vec<String>,
    }
    toml::from_str::<Includes>(hand).ok().map(|i| i.include)
}

/// `hand` as the text of the sidecar at `sidecar`, with the included files that can be read from
/// beside it.
fn with_includes(sidecar: &Path, hand: String) -> SidecarFiles {
    // A TOML error here is reported by resolve, with the rest.
    let names = include_names(&hand).unwrap_or_default();
    let dir = sidecar.parent().unwrap_or(Path::new("."));
    let mut files = vec![(
        sidecar
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
        hand,
    )];
    // What cannot be read is left out, and resolve reports it with every other problem; a name
    // with a path is never read, so a bundle carries nothing from outside the sidecar's directory.
    for name in names {
        if name.contains(['/', '\\']) || name == ".." || name == "." {
            continue;
        }
        match fs::read_to_string(dir.join(&name)) {
            Ok(text) => files.push((name, text)),
            Err(e) => eprintln!("{}: reading the included {name}: {e}", sidecar.display()),
        }
    }
    files
}

/// Checks a sidecar's `files` against the recording whose header is `header`: the annotations,
/// or every problem.
fn resolve_files(
    header: &Header,
    recording: &Path,
    files: &SidecarFiles,
) -> Result<Vec<annotations::Annotation>, Vec<String>> {
    let included: Vec<Included<'_>> = files[1..]
        .iter()
        .map(|(name, text)| Included { name, text })
        .collect();
    let file_name = recording
        .file_name()
        .map(|n| n.to_string_lossy().into_owned());
    annotations::resolve(&files[0].1, &included, header, file_name.as_deref())
}

/// Checks a sidecar's `files` against the recording whose header is `header`. Prints every
/// problem and fails if there is any.
fn resolve_sidecar(
    header: &Header,
    recording: &Path,
    sidecar: &Path,
    files: &SidecarFiles,
) -> Result<Vec<annotations::Annotation>> {
    match resolve_files(header, recording, files) {
        Ok(resolved) => Ok(resolved),
        Err(problems) => {
            for problem in &problems {
                eprintln!("{}: {problem}", sidecar.display());
            }
            match problems.len() {
                1 => bail!("1 problem"),
                n => bail!("{n} problems"),
            }
        }
    }
}

fn check_annotations(recording: &Path, sidecar: &Path, json: bool) -> Result<()> {
    let (_, _, head) = read_footer(recording)?;
    let (resolved, _) = load_sidecar(&head.header, recording, sidecar)?;
    if json {
        writeln!(
            std::io::stdout(),
            "{}",
            serde_json::to_string_pretty(&resolved)?
        )?;
    } else {
        let generated = resolved.iter().filter(|a| a.source.is_some()).count();
        info!(
            "{}: {} annotations, {} hand-written and {generated} generated, all check",
            sidecar.display(),
            resolved.len(),
            resolved.len() - generated
        );
    }
    Ok(())
}

fn bundle_file(recording: &Path, sidecar: &Path, output: &Path) -> Result<()> {
    let (mut file, _, head) = read_footer(recording)?;
    let (resolved, files) = load_sidecar(&head.header, recording, sidecar)?;
    let mut record = vec![0; head.trailer.footer_len as usize];
    file.seek(SeekFrom::Start(head.trailer.footer_offset))?;
    file.read_exact(&mut record)?;
    // At the converter's level: a sidecar is small, and read once.
    let level = Settings::default().level;
    let (keep, tail) = escapement_tickwave_format::bundle::bundled_tail(
        &record,
        &head,
        files.into_iter().collect(),
        level,
    )?;
    write_atomically(output, |out| copy_then(&mut file, keep, &tail, out))?;
    let (_, _, check) = read_footer(output)?;
    if check.header.content_id != head.header.content_id {
        bail!("bundling changed the content id");
    }
    info!(
        "{}: {} with {} annotations",
        output.display(),
        recording.display(),
        resolved.len()
    );
    Ok(())
}

fn repair_file(file: &Path, output: &Path, level: i32) -> Result<()> {
    let mut opened = fs::File::open(file).with_context(|| format!("reading {}", file.display()))?;
    let len = opened.metadata()?.len();
    let mut repairer = Repairer::new(level);
    let mut chunk = vec![0; 1 << 20];
    loop {
        let n = opened.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        repairer.feed(&chunk[..n])?;
    }
    let left_out = repairer.left_out().to_vec();
    let repaired = match repairer.finish() {
        Ok(repaired) => repaired,
        Err(RepairError::Shaped(_)) => bail!(
            "{} states the spans its channels keep, as a moved, trimmed or merged recording does, \
             so it isn't repaired: make it again from its sources",
            file.display()
        ),
        Err(e) => return Err(e).with_context(|| format!("repairing {}", file.display())),
    };
    write_atomically(output, |out| {
        copy_then(&mut opened, repaired.keep, &repaired.tail, out)
    })?;
    write_generated(output, &repaired.header, &repaired.generated)?;
    warn_left_out(&left_out);
    info!(
        "{}: {} of {} bytes kept, {} blocks to {}, {}",
        output.display(),
        repaired.keep,
        len,
        blocks(&repaired.header),
        seconds(repaired.header.length, &repaired.header),
        repaired.header.content_id
    );
    Ok(())
}

/// The annotation lines repair or recut left out, as they would not resolve.
fn warn_left_out(left_out: &[String]) {
    for why in left_out {
        warn!("left out an annotation that would not resolve: {why}");
    }
}

/// Each annotation generator's file beside `output`, as `<stem>.<source>.jsonl`.
fn write_generated(
    output: &Path,
    header: &Header,
    generated: &std::collections::BTreeMap<String, Vec<String>>,
) -> Result<()> {
    let stem = output
        .file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    let dir = output.parent().unwrap_or(Path::new(""));
    for (source, lines) in generated {
        let path = dir.join(format!("{stem}.{source}.jsonl"));
        let text = generator_file(source, None, header.content_id, lines);
        write_atomically(&path, |f| Ok(f.write_all(text.as_bytes())?))?;
        info!("{}: {} annotations", path.display(), lines.len());
    }
    Ok(())
}

fn recut_file(input: &Path, output: &Path, options: WriteOptions) -> Result<()> {
    let mut opened =
        fs::File::open(input).with_context(|| format!("reading {}", input.display()))?;
    let len = opened.metadata()?.len();
    let started = Instant::now();
    let mut recutter = Recutter::new(options);
    let (finished, left_out) = write_atomically(output, |out| {
        let mut chunk = vec![0; 1 << 20];
        loop {
            let n = opened.read(&mut chunk)?;
            if n == 0 {
                break;
            }
            recutter
                .feed(&chunk[..n])
                .with_context(|| format!("reading {}", input.display()))?;
            out.write_all(recutter.output())?;
            let written = recutter.output().len();
            recutter.consume(written);
        }
        // What the last `consume` compressed waits in the output, and the finish's tail holds it.
        let left_out = recutter.left_out().to_vec();
        let finished = match recutter.finish() {
            Ok(finished) => finished,
            Err(RecutError::Shaped(_)) => bail!(
                "{} is cut off, and it states the spans its channels keep, as a moved, trimmed or \
                 merged recording does, so it isn't finished: make it again from its sources",
                input.display()
            ),
            Err(e) => return Err(e).with_context(|| format!("re-cutting {}", input.display())),
        };
        out.write_all(&finished.tail)?;
        Ok((finished, left_out))
    })?;
    let check = match read_footer(output) {
        Ok((_, _, check)) => check,
        Err(e) => {
            let _ = fs::remove_file(output);
            return Err(e.context("the copy does not read back"));
        }
    };
    if check.header.content_id != finished.header.content_id {
        let _ = fs::remove_file(output);
        bail!("the copy does not read back with its content id");
    }
    if check.bundle.is_none() {
        write_generated(output, &finished.header, &finished.generated)?;
    }
    warn_left_out(&left_out);
    info!(
        "{}: {:.2} MB from {:.2} MB, {} blocks, {} in {:.1} s",
        output.display(),
        fs::metadata(output)?.len() as f64 / 1e6,
        len as f64 / 1e6,
        blocks(&finished.header),
        finished.header.content_id,
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

/// A file's name, as the viewer is given it.
fn file_name(path: &Path) -> Result<String> {
    Ok(path
        .file_name()
        .with_context(|| format!("{} names no file", path.display()))?
        .to_string_lossy()
        .into_owned())
}

/// Serves `recording` and its sidecar with `site` until the process is stopped: the sidecar given,
/// or the one beside the recording by its name.
fn open_file(
    site: Box<dyn open::Site>,
    recording: &Path,
    sidecar: Option<&Path>,
    port: u16,
    browser: bool,
) -> Result<()> {
    let (_, _, head) = read_footer(recording)?;
    let files = open::Files {
        recording: open::Named {
            name: file_name(recording)?,
            path: recording.to_owned(),
        },
        sidecar: sidecar_for(&head.header, recording, sidecar)?,
    };
    open::serve(site, files, port, browser)
}

/// The sidecar `open` serves with `recording`: the one `given`, or `<stem>.annotations.toml`
/// beside the recording. The page may save it when it checks, or when there is none beside the
/// recording, which the page then starts.
fn sidecar_for(
    header: &Header,
    recording: &Path,
    given: Option<&Path>,
) -> Result<Option<open::Sidecar>> {
    let (path, exists) = match given {
        Some(given) => (given.to_owned(), true),
        None => {
            let mut name = recording.file_stem().unwrap_or_default().to_owned();
            name.push(".annotations.toml");
            let beside = recording.with_file_name(name);
            match fs::metadata(&beside) {
                Ok(m) if m.is_file() => (beside, true),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (beside, false),
                _ => return Ok(None),
            }
        }
    };
    let hand = open::Named {
        name: file_name(&path)?,
        path: path.clone(),
    };
    if !exists {
        info!(
            "{}: no sidecar, so the page starts {} when it saves annotations",
            recording.display(),
            path.display()
        );
        let check = save_check(header.clone(), recording.to_owned(), path, Vec::new());
        return Ok(Some(open::Sidecar {
            hand,
            included: Vec::new(),
            exists,
            check: Some(check),
        }));
    }
    let read = read_sidecar(&path)?;
    let checks = match resolve_sidecar(header, recording, &path, &read) {
        Ok(resolved) => {
            info!(
                "{}: with its sidecar {}, {} annotations",
                recording.display(),
                path.display(),
                resolved.len()
            );
            true
        }
        Err(e) => {
            warn!(
                "{}: {e}, so the viewer shows the recording without its annotations and the \
                 page can't save over them",
                path.display()
            );
            false
        }
    };
    let dir = path.parent().unwrap_or(Path::new(""));
    let included: Vec<open::Named> = read[1..]
        .iter()
        .map(|(name, _)| open::Named {
            name: name.clone(),
            path: dir.join(name),
        })
        .collect();
    let served = included.iter().map(|n| n.name.clone()).collect();
    Ok(Some(open::Sidecar {
        hand,
        included,
        exists,
        check: checks.then(|| save_check(header.clone(), recording.to_owned(), path, served)),
    }))
}

/// Checks a text the page saves as the sidecar at `sidecar` as `annotations check` does, with its
/// includes read from disk. It must include the files in `served`, which the page reads from this
/// server, and no others.
fn save_check(
    header: Header,
    recording: PathBuf,
    sidecar: PathBuf,
    mut served: Vec<String>,
) -> open::Check {
    served.sort();
    served.dedup();
    Box::new(move |text| {
        if let Some(mut names) = include_names(text) {
            names.sort();
            names.dedup();
            if names != served {
                return Err(vec![format!(
                    "the sidecar includes {names:?}, but tickwave open serves {served:?}: \
                     restart it to serve other files"
                )]);
            }
        }
        let files = with_includes(&sidecar, text.to_owned());
        resolve_files(&header, &recording, &files).map(drop)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use escapement_tickwave_format::header::{ChannelHeader, SourceHeader};

    /// A recording's header at 1 GS/s, 1 ms long, with digital SWCLK and SWDIO.
    pub(crate) fn header() -> Header {
        let digital = |name: &str| ChannelHeader {
            name: name.into(),
            source_name: name.into(),
            source: 0,
            data: ChannelKind::Digital {
                initial: true,
                end: 1_000_000,
                edges: 10,
                gaps: Vec::new(),
                gap_count: None,
            },
            shaping: Default::default(),
            overview_shift: None,
            blocks: 1,
            overview_blocks: Vec::new(),
        };
        Header {
            timebase: escapement_tickwave_format::model::Ratio::integer(1_000_000_000).unwrap(),
            length: 1_000_000,
            channels: vec![digital("SWCLK"), digital("SWDIO")],
            derived: Vec::new(),
            sources: vec![SourceHeader::new(
                SourceFormat::Vcd,
                escapement_tickwave_format::model::Ratio::integer(1_000_000_000).unwrap(),
            )],
            title: None,
            meta: Default::default(),
            content_id: "blake3:00ff000000000000000000000000000000000000000000000000000000000000"
                .parse()
                .unwrap(),
            overview_shift: 12,
        }
    }

    const HAND: &str = "format = \"tickwave-annotations/1\"\nrecording_id = \
                        \"blake3:00ff000000000000000000000000000000000000000000000000000000000000\"\n";

    #[test]
    fn the_page_may_save_a_sidecar_that_checks() {
        let dir = tempfile::tempdir().unwrap();
        let recording = dir.path().join("rec.tickwave");
        fs::write(dir.path().join("rec.annotations.toml"), HAND).unwrap();
        let sidecar = sidecar_for(&header(), &recording, None).unwrap().unwrap();
        assert!(sidecar.exists);
        assert_eq!(sidecar.hand.name, "rec.annotations.toml");
        let check = sidecar.check.expect("a sidecar that checks may be saved");
        assert_eq!(check(HAND), Ok(()));
    }

    #[test]
    fn the_page_may_not_save_over_a_sidecar_with_problems() {
        let dir = tempfile::tempdir().unwrap();
        let recording = dir.path().join("rec.tickwave");
        let given = dir.path().join("other.toml");
        fs::write(&given, HAND.replace("00ff", "11ff")).unwrap();
        let sidecar = sidecar_for(&header(), &recording, Some(&given))
            .unwrap()
            .unwrap();
        assert!(sidecar.exists);
        assert_eq!(sidecar.hand.name, "other.toml");
        assert!(sidecar.check.is_none());
    }

    #[test]
    fn the_page_starts_a_sidecar_beside_the_recording_when_there_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let recording = dir.path().join("rec.tickwave");
        let sidecar = sidecar_for(&header(), &recording, None).unwrap().unwrap();
        assert!(!sidecar.exists);
        assert_eq!(sidecar.hand.path, dir.path().join("rec.annotations.toml"));
        assert!(sidecar.included.is_empty());
        let check = sidecar.check.expect("the page may start one");
        assert_eq!(check(HAND), Ok(()));
        // Something else by that name: nothing to show, and nowhere to save.
        fs::create_dir(dir.path().join("rec.annotations.toml")).unwrap();
        assert!(sidecar_for(&header(), &recording, None).unwrap().is_none());
    }

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from([&["tickwave", "convert"], args].concat())
    }

    fn common(args: &[&str]) -> Common {
        match parse(args)
            .unwrap_or_else(|e| panic!("{args:?}: {e}"))
            .command
        {
            Command::Convert {
                from:
                    Source::Saleae { common, .. }
                    | Source::Sigrok { common, .. }
                    | Source::Vcd { common, .. },
            } => common,
            _ => unreachable!("a convert command"),
        }
    }

    const SHAPING: [&str; 10] = [
        "--offset",
        "CLK=-40ns",
        "--offset",
        "D=12",
        "--keep",
        "CLK=1.2ms..",
        "--keep",
        "D=..2ms",
        "--keep",
        "E=1 us..3us",
    ];

    #[test]
    fn every_convert_source_takes_offsets_and_keeps_into_its_settings() {
        for source in [
            &["saleae", "dir", "-o", "x.tickwave", "--channel", "a=CLK"][..],
            &["sigrok", "x.sr", "-o", "x.tickwave"],
            &["vcd", "x.vcd", "-o", "x.tickwave", "--open"],
        ] {
            let settings = common(&[source, &SHAPING].concat()).settings();
            let owned = |s: &str| s.to_owned();
            assert_eq!(
                settings.offsets,
                [("CLK", "-40ns"), ("D", "12")].map(|(n, t)| (owned(n), owned(t)))
            );
            assert_eq!(
                settings.keeps,
                [
                    (owned("CLK"), Some(owned("1.2ms")), None),
                    (owned("D"), None, Some(owned("2ms"))),
                    (owned("E"), Some(owned("1 us")), Some(owned("3us"))),
                ]
            );
        }
        let settings = common(&["vcd", "x.vcd", "-o", "x.tickwave"]).settings();
        assert!(settings.offsets.is_empty() && settings.keeps.is_empty());
    }

    #[test]
    fn an_offset_or_a_keep_that_does_not_read_is_refused() {
        for (flag, why) in [
            ("--offset=CLK", "expected <name>=<time>"),
            ("--offset==40ns", "expected <name>=<time>"),
            ("--offset=CLK=", "expected <name>=<time>"),
            (
                "--offset=CLK=40 parsecs",
                r#""40 parsecs": unknown unit "parsecs""#,
            ),
            ("--offset=CLK=1.5", r#""1.5": no unit"#),
            ("--keep=CLK=1ms", "expected <name>=<from>..<to>"),
            ("--keep==1ms..", "expected <name>=<from>..<to>"),
            ("--keep=CLK", "expected <name>=<from>..<to>"),
            (
                "--keep=CLK=1ms..2 fortnights",
                r#"unknown unit "fortnights""#,
            ),
        ] {
            let err = parse(&["vcd", "x.vcd", "-o", "x.tickwave", flag])
                .err()
                .unwrap_or_else(|| panic!("{flag} parsed"))
                .to_string();
            assert!(err.contains(why), "{flag}: {err}");
        }
    }

    #[test]
    fn a_conversion_moves_and_cuts_what_its_flags_say_and_refuses_what_it_cannot() {
        let dir = tempfile::tempdir().unwrap();
        let source =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/import/demo-logic.sr");
        let output = dir.path().join("out.tickwave");
        let open = || Ok(sigrok::read(&source)?.recording);
        let run = |flags: &[&str]| {
            let common = common(&[&["sigrok", "x.sr", "-o", "x.tickwave"], flags].concat());
            convert(&open, &output, &common)
        };
        let err = run(&["--offset", "D0=5", "--offset", "D0=6"]).expect_err("refused");
        assert_eq!(err.to_string(), r#""D0" is given an offset twice"#);
        assert!(!output.exists());

        run(&[
            "--offset",
            "D0=-5",
            "--offset",
            "D2=3",
            "--keep",
            "D1=10..100",
        ])
        .unwrap();
        let (_, _, head) = read_footer(&output).unwrap();
        let shaping: Vec<_> = head.header.channels[..3]
            .iter()
            .map(|c| (c.shaping.offset.as_ref().map(|o| o.ticks), c.shaping.kept))
            .collect();
        use escapement_tickwave_format::header::Kept;
        // D0, moved 5 earlier, starts the recording and is kept whole: its 5,000 samples. The
        // source's tick 0 is then the recording's tick 5. Every span is in the source's ticks.
        assert_eq!(
            shaping,
            [
                (Some(-5), Some(Kept { from: 0, to: 5000 })),
                (None, Some(Kept { from: 10, to: 100 })),
                (Some(3), Some(Kept { from: 0, to: 5000 })),
            ]
        );
        assert_eq!(head.header.sources[0].origin, 5);
        // Checked through the origin, the offsets and the kept spans.
        verify_file(&output, std::slice::from_ref(&source), 64 << 20).unwrap();

        // The same file stating D2 moved one tick less, in its declaration and its footer. Its
        // moves and cuts then end the recording a tick sooner than the file does. One tick more
        // would put its end past the length, which the reader refuses.
        let doctored = dir.path().join("doctored.tickwave");
        fs::write(
            &doctored,
            rewritten(
                &fs::read(&output).unwrap(),
                r#""offset":3,"#,
                r#""offset":2,"#,
            ),
        )
        .unwrap();
        read_footer(&doctored).unwrap();
        let err =
            verify_file(&doctored, std::slice::from_ref(&source), 64 << 20).expect_err("refused");
        assert_eq!(
            format!("{err:#}"),
            "the recording does not match its source: channel D0: a block ends at tick 5008, past \
             the source's length, 5007"
        );
        // Or its source's tick 0 one tick later than the moves and cuts put it: no channel then
        // starts the recording, which the reader refuses.
        fs::write(
            &doctored,
            rewritten(
                &fs::read(&output).unwrap(),
                r#""origin":5"#,
                r#""origin":6"#,
            ),
        )
        .unwrap();
        let err =
            verify_file(&doctored, std::slice::from_ref(&source), 64 << 20).expect_err("refused");
        assert!(
            format!("{err:#}").ends_with(
                "no channel's kept span starts at the axis's tick 0, where the recording starts"
            ),
            "{err:#}"
        );
    }

    #[test]
    fn a_merge_takes_a_stretch_or_two_marks_for_an_input() {
        let dir = tempfile::tempdir().unwrap();
        let testdata = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/import");
        let (a, b, m) = (
            dir.path().join("a.tickwave"),
            dir.path().join("b.tickwave"),
            dir.path().join("m.tickwave"),
        );
        let logic = testdata.join("demo-logic.sr");
        let mixed = testdata.join("demo-mixed.vcd");
        convert(
            &|| Ok(sigrok::read(&logic)?.recording),
            &a,
            &common(&["sigrok", "x.sr", "-o", "x"]),
        )
        .unwrap();
        convert(
            &|| Ok(vcd::read(&mixed)?.recording),
            &b,
            &common(&["vcd", "x.vcd", "-o", "x"]),
        )
        .unwrap();
        let merge = |flags: &[&str]| {
            let mut args = vec![
                std::ffi::OsString::from("merge"),
                a.clone().into(),
                b.clone().into(),
                "-o".into(),
                m.clone().into(),
            ];
            args.extend(flags.iter().map(std::ffi::OsString::from));
            let args: Vec<&std::ffi::OsStr> = args.iter().map(|a| a.as_os_str()).collect();
            cli(&args).map_err(|e| format!("{e:#}"))
        };
        use escapement_tickwave_format::model::Ratio;
        merge(&["--stretch", "2=+1000ppm"]).unwrap();
        let (_, _, head) = read_footer(&m).unwrap();
        let stretches: Vec<_> = head.header.sources.iter().map(|s| s.stretch).collect();
        assert_eq!(stretches, [None, Ratio::new(1001, 1000)]);
        verify_file(&m, &[a.clone(), b.clone()], 64 << 20).unwrap();
        // Two marks: the VCD's 0 at 10 us and its 1 s at 1.00201 s, a stretch of 1.002.
        merge(&["--align", "2=0@10us,1s@1.00201s"]).unwrap();
        let (_, _, head) = read_footer(&m).unwrap();
        let s = &head.header.sources[1];
        assert_eq!(
            (s.stretch, s.origin, s.align),
            (Ratio::new(501, 500), 100, Some(100))
        );
        verify_file(&m, &[a.clone(), b.clone()], 64 << 20).unwrap();
        for (flags, why) in [
            (
                &["--stretch", "2=+1ppm", "--align", "2=0@0,1ms@1ms"][..],
                "input 2 is given a stretch and two marks, which give one each",
            ),
            (
                &["--stretch", "3=+1ppm"],
                "--stretch 3=+1ppm: there are 2 inputs",
            ),
            (
                &["--stretch", "2=+1ppm", "--stretch", "2=-1ppm"],
                "input 2 is given a stretch twice",
            ),
            (
                &["--stretch", "2=fast"],
                "\"fast\": a stretch is a ratio a/b, or a change such as +20ppm",
            ),
            (
                &["--align", "2=0@0,1ms"],
                "expected <input>=<time> or <input>=<x>@<y>,<x>@<y>, got \"2=0@0,1ms\"",
            ),
        ] {
            let err = merge(flags).unwrap_err();
            assert!(err.contains(why), "{flags:?}: {err}");
        }
    }

    /// A recording's bytes as an input that keeps the spans it reads.
    struct Counted {
        bytes: Vec<u8>,
        reads: std::sync::Mutex<Vec<std::ops::Range<u64>>>,
    }

    impl escapement_tickwave_import::input::Input for Counted {
        fn size(&self) -> u64 {
            self.bytes.len() as u64
        }
        fn read_at(&self, offset: u64, buf: &mut [u8]) {
            self.reads
                .lock()
                .unwrap()
                .push(offset..offset + buf.len() as u64);
            buf.copy_from_slice(&self.bytes[offset as usize..][..buf.len()]);
        }
    }

    /// A marker high for 2 ms every 10 ms, 200 times, as a source at `rate` Hz whose clock reads
    /// `ppm` fast sees it.
    fn marker(rate: u64, ppm: f64) -> Vec<u8> {
        use escapement_tickwave_format::model::{Channel, ChannelData, Digital, Ratio, Shaping};
        let scale = rate as f64 * 1e-3 * (1.0 + ppm / 1e6);
        let edges: Vec<i64> = (1..=200i64)
            .flat_map(|k| {
                let rise = (10.0 * k as f64 * scale).round() as i64;
                [rise, rise + (2.0 * scale).round() as i64]
            })
            .collect();
        let end = (2_020.0 * scale) as i64;
        let tb = Ratio::integer(rate).unwrap();
        escapement_tickwave_format::encode::encode(
            Recording {
                timebase: tb,
                length: end,
                channels: vec![Channel {
                    name: "M".into(),
                    source_name: "m".into(),
                    source: 0,
                    data: ChannelData::Digital(Digital {
                        initial: false,
                        end,
                        count: edges.len() as u64,
                        edges: Box::new(edges.into_iter()),
                        gaps: Vec::new(),
                    }),
                    shaping: Shaping::default(),
                }],
                derived: Vec::new(),
                sources: vec![escapement_tickwave_format::model::Source::new(
                    SourceFormat::Other("marker".into()),
                    tb,
                )],
            },
            &escapement_tickwave_format::encode::Options {
                level: 3,
                block_bytes: 1 << 10,
                ..Default::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn info_reads_the_index_alone_and_measures_the_drift_only_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, m) = (
            dir.path().join("a.tickwave"),
            dir.path().join("b.tickwave"),
            dir.path().join("m.tickwave"),
        );
        // B's clock 1,000 ppm fast: A takes a stretch of +1,000 ppm to tick as B does.
        fs::write(&a, marker(1_000_000, 0.0)).unwrap();
        fs::write(&b, marker(2_000_000, 1_000.0)).unwrap();
        merge_files(
            &[a, b],
            &m,
            &merge::Settings {
                inputs: vec![merge::InputSettings::default(); 2],
                level: 3,
                block_kib: 1,
                ..merge::Settings::default()
            },
        )
        .unwrap();
        let input = Counted {
            bytes: fs::read(&m).unwrap(),
            reads: Default::default(),
        };
        let index = Index::of_file(&input.bytes).unwrap();
        let frames: Vec<std::ops::Range<u64>> = index
            .blocks
            .iter()
            .flatten()
            .map(|b| b.row.offset..b.row.offset + u64::from(b.row.length))
            .collect();
        // Whether any read so far took bytes of a block.
        let read_a_block = || {
            let reads = input.reads.lock().unwrap();
            reads
                .iter()
                .any(|r| frames.iter().any(|f| r.start < f.end && f.start < r.end))
        };
        let len = input.bytes.len() as u64;
        for json in [false, true] {
            let mut out = Vec::new();
            let head = Footer::parse_file(&input.bytes).unwrap();
            write_info(&mut out, head, len, json, &input, false).unwrap();
            let out = String::from_utf8(out).unwrap();
            assert!(!input.reads.lock().unwrap().is_empty(), "the index is read");
            assert!(!read_a_block(), "{out}");
            if !json {
                let hint = format!(
                    "drift: 2 digital channels from two or more sources may share a signal; \
                     `tickwave info --drift` measures it, decoding their {} blocks",
                    frames.len()
                );
                assert!(out.contains(&hint), "{out}");
            }
        }
        let mut out = Vec::new();
        let head = Footer::parse_file(&input.bytes).unwrap();
        write_info(&mut out, head, len, false, &input, true).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(read_a_block());
        assert!(
            out.contains("drift: M (source 0) against b/M (source 1): +1000.0 ± "),
            "{out}"
        );
        assert!(!out.contains("--drift"), "{out}");
    }

    /// The median, least and greatest of lengths below and above the counted ones, against the
    /// lengths sorted: each count of a short length is a rank, and the long ones are held.
    #[test]
    fn info_takes_the_median_of_every_block_length_counted_or_held() {
        let energy = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../web/fixtures/energy.tickwave"
        );
        let footer = Footer::parse_file(&fs::read(energy).unwrap()).unwrap();
        let mut x = 0x2545_f491_4f6c_dd1du64;
        for n in [1usize, 2, 3, 10, 1001] {
            for long in [0u64, 1, 2, 3] {
                let mut sizes = Sizes::new(&footer);
                let mut all = Vec::new();
                for i in 0..n {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    // Every long-th length past the counted ones, the rest below them.
                    let l = if long > 0 && x % 4 < long {
                        SHORT_FRAME as u32 + (x >> 40) as u32
                    } else {
                        (x >> 48) as u32
                    };
                    sizes.block(i % 2, l);
                    all.push(l);
                }
                all.sort_unstable();
                let at = format!("{n} lengths, {long} in 4 long");
                assert_eq!(sizes.min(), all.first().copied(), "{at}");
                assert_eq!(sizes.max(), all.last().copied(), "{at}");
                assert_eq!(sizes.median(), all[n / 2], "{at}");
                assert_eq!(sizes.count(), n as u64, "{at}");
            }
        }
    }

    /// A channel's 70,000 blocks, in 18 pages under a directory, and its overview level's rows
    /// in two pages: `info` counts them a page at a time as the whole index gives them.
    #[test]
    fn info_counts_an_index_a_page_at_a_time_as_the_whole_index_gives_it() {
        use escapement_tickwave_format::header::{Declaration, DeclaredChannel, DeclaredKind};
        use escapement_tickwave_format::model::Ratio;
        use escapement_tickwave_format::writer::{Finish, Writer};
        let timebase = Ratio::integer(1_000_000_000).unwrap();
        let digital = |name: &str, shift| DeclaredChannel {
            name: name.into(),
            source_name: name.into(),
            source: 0,
            data: DeclaredKind::Digital { initial: false },
            shaping: Default::default(),
            overview_shift: shift,
        };
        let d = Declaration {
            timebase,
            channels: vec![digital("D", None), digital("L", Some(0))],
            derived: Vec::new(),
            sources: vec![SourceHeader::new(SourceFormat::Other("t".into()), timebase)],
            title: None,
            meta: Default::default(),
            overview_shift: 40,
        };
        let options = WriteOptions {
            block_bytes: 1,
            promise_with_items: true,
            level: 1,
            ..WriteOptions::default()
        };
        let mut w = Writer::new(d, options).unwrap();
        let mut file = w.take_output();
        // D's edges a block each; L's one in each 4,096-tick overview block, each in another
        // place, so its 4,100 rows are all apart.
        let mut edges: Vec<(i64, usize)> = (0..70_000i64).map(|k| (10 * k, 0)).collect();
        edges.extend((0..4_100i64).map(|k| (k * 4096 + 1 + (k * 37) % 4000, 1)));
        edges.sort();
        for (t, c) in edges {
            w.edge(c, t).unwrap();
            file.extend(w.take_output());
        }
        let length = 4_101 * 4096;
        file.extend(
            &w.finish(length, &[length, length], Finish::default())
                .unwrap()
                .tail,
        );
        let len = file.len() as u64;
        let info = |drift: bool, json: bool| {
            let mut out = Vec::new();
            let head = Footer::parse_file(&file).unwrap();
            write_info(&mut out, head, len, json, &file, drift).unwrap();
            String::from_utf8(out).unwrap()
        };
        let report =
            |drift| -> serde_json::Value { serde_json::from_str(&info(drift, true)).unwrap() };
        let (paged, whole) = (report(false), report(true));
        assert_eq!(paged["block_bytes"], whole["block_bytes"]);
        assert_eq!(paged["overview_block_bytes"], whole["overview_block_bytes"]);
        // At shifts 0, 4, 8 and 12, the last of 2^0's 4,101 blocks holding no edge.
        assert_eq!(whole["overview_block_bytes"]["count"], 4_101 + 257 + 17 + 2);
        let lines = |text: String| -> Vec<String> {
            text.lines()
                .filter(|l| l.contains("blocks"))
                .map(str::to_owned)
                .collect()
        };
        let text = lines(info(false, false));
        assert!(text.iter().any(|l| l.contains("74101 blocks")), "{text:?}");
        assert_eq!(text, lines(info(true, false)));
    }

    /// Runs `tickwave` as a user would, with `args`.
    fn cli(args: &[&std::ffi::OsStr]) -> Result<()> {
        let cli = Cli::try_parse_from(
            std::iter::once(std::ffi::OsStr::new("tickwave")).chain(args.iter().copied()),
        )?;
        execute(cli.command, 64 << 20)
    }

    #[test]
    fn a_merge_of_conversions_verifies_against_their_captures_and_their_recordings() {
        let dir = tempfile::tempdir().unwrap();
        let testdata = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/import");
        let (logic, mixed) = (
            testdata.join("demo-logic.sr"),
            testdata.join("demo-mixed.vcd"),
        );
        let (a, b, m) = (
            dir.path().join("a.tickwave"),
            dir.path().join("b.tickwave"),
            dir.path().join("m.tickwave"),
        );
        // A conversion moved and cut, and one whole.
        let open_logic = || Ok(sigrok::read(&logic)?.recording);
        let common_a = common(&[
            "sigrok",
            "x.sr",
            "-o",
            "x",
            "--offset",
            "D0=-5",
            "--keep",
            "D1=10..100",
        ]);
        convert(&open_logic, &a, &common_a).unwrap();
        let open_mixed = || Ok(vcd::read(&mixed)?.recording);
        convert(&open_mixed, &b, &common(&["vcd", "x.vcd", "-o", "x"])).unwrap();
        let os = |s: &Path| s.as_os_str().to_owned();
        let arg = |s: &str| std::ffi::OsString::from(s);
        cli(&[
            &arg("merge"),
            &os(&a),
            &os(&b),
            &arg("-o"),
            &os(&m),
            &arg("--align"),
            &arg("2=100"),
        ])
        .unwrap();
        let (_, _, head) = read_footer(&m).unwrap();
        let h = &head.header;
        // 2.5 MHz and 10 MHz: an m of 4 and of 1. The conversion's source tick 0, 5 of its ticks
        // in, is 20 merged ticks in; the VCD's, aligned by 100, at 100.
        assert_eq!(
            h.timebase,
            escapement_tickwave_format::model::Ratio::integer(10_000_000).unwrap()
        );
        assert_eq!(
            h.sources
                .iter()
                .map(|s| (s.origin, s.align))
                .collect::<Vec<_>>(),
            [(20, None), (100, Some(100))]
        );
        let verify = |against: &[&Path]| {
            let mut args = vec![arg("verify"), os(&m)];
            for path in against {
                args.push(arg("--against"));
                args.push(os(path));
            }
            let args: Vec<&std::ffi::OsStr> = args.iter().map(|a| a.as_os_str()).collect();
            cli(&args)
        };
        // Against the captures themselves: the conversion's offset and cut fold into the merge's.
        verify(&[&logic, &mixed]).unwrap();
        verify(&[&a, &b]).unwrap();
        verify(&[&a, &mixed]).unwrap();
        // Swapped: the order check names both.
        let err = format!("{:#}", verify(&[&b, &a]).unwrap_err());
        assert!(
            err.ends_with(
                "input 1's source 0 is vcd [demo-mixed.vcd], its tick 0 at 0 s, but the merged \
                 recording's source 0 is sigrok-sr [demo-logic.sr]: the recordings are given in \
                 another order than the merge's, or another recording"
            ),
            "{err}"
        );
        // Tampered, in its declaration and its footer: an origin, an align, a kept span.
        let bytes = fs::read(&m).unwrap();
        let tampered = dir.path().join("t.tickwave");
        for (from, to, why) in [
            // The VCD's channels end at the length: a tick later, they pass it.
            (
                r#""origin":100,"align":100"#,
                r#""origin":101,"align":100"#,
                "channel libsigrok.D0 is kept from source tick 0 to 952381, which is empty or \
                 maps outside the axis's 0 to 952481",
            ),
            (
                r#""origin":100,"align":100"#,
                r#""origin":100,"align":101"#,
                "the content id does not match the content",
            ),
            // A kept span the reader takes and the inputs rebuild: the id pins it.
            (
                r#""kept":[10,100]"#,
                r#""kept":[10,101]"#,
                "the content id does not match the content",
            ),
        ] {
            fs::write(&tampered, rewritten(&bytes, from, to)).unwrap();
            let args = [
                arg("verify"),
                os(&tampered),
                arg("--against"),
                os(&a),
                arg("--against"),
                os(&b),
            ];
            let args: Vec<&std::ffi::OsStr> = args.iter().map(|a| a.as_os_str()).collect();
            let err = format!("{:#}", cli(&args).unwrap_err());
            assert!(err.contains(why), "{to}: {err}");
        }
        // More recordings than the merged one has sources.
        let err = format!("{:#}", verify(&[&logic, &mixed, &mixed]).unwrap_err());
        assert!(
            err.ends_with("the recording has 2 sources, and the recordings before cover them"),
            "{err}"
        );
        // A recording of one source checks against a .tickwave too, as a merge of one.
        let args = [arg("verify"), os(&a), arg("--against"), os(&a)];
        let args: Vec<&std::ffi::OsStr> = args.iter().map(|a| a.as_os_str()).collect();
        cli(&args).unwrap();
        // Inputs count from 1: input 1's D1, renamed, and nothing else of it.
        let m2 = dir.path().join("m2.tickwave");
        cli(&[
            &arg("merge"),
            &os(&a),
            &os(&b),
            &arg("-o"),
            &os(&m2),
            &arg("--channel"),
            &arg("1:D1=clock"),
        ])
        .unwrap();
        let (_, _, head) = read_footer(&m2).unwrap();
        assert_eq!(head.header.channels[0].name, "clock");
        verify_file(&m2, &[a.clone(), b.clone()], 64 << 20).unwrap();
        assert_eq!(
            head.header
                .channels
                .iter()
                .filter(|c| c.source == 0)
                .count(),
            1
        );
        let err = cli(&[
            &arg("merge"),
            &os(&a),
            &os(&b),
            &arg("-o"),
            &os(&m2),
            &arg("--align"),
            &arg("2=1"),
            &arg("--align"),
            &arg("2=2"),
        ])
        .unwrap_err();
        assert_eq!(err.to_string(), "input 2 is given an alignment twice");
        for (flag, value) in [("--channel", "0:D1"), ("--align", "0=1")] {
            let parsed = Cli::try_parse_from([
                "tickwave",
                "merge",
                "a.tickwave",
                "b.tickwave",
                "-o",
                "m.tickwave",
                flag,
                value,
            ]);
            assert!(parsed.is_err(), "{flag} {value}");
        }
        // A merge takes two inputs at least.
        let one = Cli::try_parse_from(["tickwave", "merge", "a.tickwave", "-o", "m.tickwave"]);
        assert!(one.is_err());
    }

    #[test]
    fn a_merge_carries_the_sidecar_beside_an_input_and_each_source_has_its_m() {
        let dir = tempfile::tempdir().unwrap();
        let testdata = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/import");
        let (a, b, m) = (
            dir.path().join("a.tickwave"),
            dir.path().join("b b.tickwave"),
            dir.path().join("m.tickwave"),
        );
        let logic = testdata.join("demo-logic.sr");
        let open_logic = || Ok(sigrok::read(&logic)?.recording);
        convert(&open_logic, &a, &common(&["sigrok", "x.sr", "-o", "x"])).unwrap();
        let mixed = testdata.join("demo-mixed.vcd");
        let open_mixed = || Ok(vcd::read(&mixed)?.recording);
        convert(&open_mixed, &b, &common(&["vcd", "x.vcd", "-o", "x"])).unwrap();
        let (_, _, head) = read_footer(&b).unwrap();
        let id = head.header.content_id;
        // B's sidecar beside it: a generator's file, and a hand-written annotation of its own.
        fs::write(
            dir.path().join("b b.annotations.toml"),
            format!(
                "format = \"tickwave-annotations/1\"\nrecording_id = \"{id}\"\n\
                 include = [\"b b.dec.jsonl\"]\n\n[[annotation]]\nid = \"mine\"\nstart = 3\n"
            ),
        )
        .unwrap();
        fs::write(
            dir.path().join("b b.dec.jsonl"),
            generator_file(
                "dec",
                None,
                id,
                &[r#"{"id":"x/7","start":7,"channels":["libsigrok.D0"]}"#.to_owned()],
            ),
        )
        .unwrap();
        let args = [
            std::ffi::OsString::from("merge"),
            a.clone().into_os_string(),
            b.clone().into_os_string(),
            "-o".into(),
            m.clone().into_os_string(),
        ];
        let args: Vec<&std::ffi::OsStr> = args.iter().map(|a| a.as_os_str()).collect();
        cli(&args).unwrap();
        verify_file(&m, &[a, b], 64 << 20).unwrap();
        let (_, _, head) = read_footer(&m).unwrap();
        let h = &head.header;
        let bytes = fs::read(&m).unwrap();
        let files = head
            .read_bundle(escapement_tickwave_format::decode::from_memory(&bytes))
            .unwrap();
        let files = files.as_ref().expect("bundled");
        let jsonl = &files["m.dec.jsonl"];
        // At 10 MHz the VCD's tick is the recording's: 7 stays 7.
        assert!(
            jsonl.contains(r#""channels":["libsigrok.D0"],"id":"x/7","start":7"#),
            "{jsonl}"
        );
        // `annotations check` passes on the merged bundle.
        let included: Vec<Included<'_>> = files
            .iter()
            .filter(|(n, _)| n.ends_with(".jsonl"))
            .map(|(name, text)| Included { name, text })
            .collect();
        annotations::resolve(
            &files["m.annotations.toml"],
            &included,
            h,
            Some("m.tickwave"),
        )
        .unwrap();
        // Each source's m, which info states.
        let ms: Vec<_> = (0..h.sources.len()).map(|i| h.m(i)).collect();
        assert_eq!(
            ms,
            [
                escapement_tickwave_format::model::Ratio::new(4, 1),
                Some(escapement_tickwave_format::model::Ratio::ONE)
            ]
        );
    }

    /// `file` with `from` replaced by `to`, of the same length, in every record that holds it, and
    /// each such record's check made again.
    fn rewritten(file: &[u8], from: &str, to: &str) -> Vec<u8> {
        use escapement_tickwave_format::record::{self, Kind, Next};
        assert_eq!(from.len(), to.len());
        let mut out = file[..12].to_vec();
        let mut pos = 12;
        let mut changed = 0;
        while let Next::Record { kind, payload, len } = record::next(&file[pos..], usize::MAX) {
            match payload
                .windows(from.len())
                .position(|w| w == from.as_bytes())
            {
                Some(at) => {
                    let mut payload = payload.to_vec();
                    payload[at..at + to.len()].copy_from_slice(to.as_bytes());
                    record::write(&mut out, Kind::from_byte(kind).unwrap(), &[&payload]);
                    changed += 1;
                }
                None => out.extend_from_slice(&file[pos..pos + len]),
            }
            pos += len;
        }
        assert_eq!(changed, 2, "the declaration and the footer");
        out.extend_from_slice(&file[pos..]);
        out
    }

    #[test]
    fn a_moved_or_trimmed_recording_cut_off_is_refused_and_a_whole_one_repairs() {
        let dir = tempfile::tempdir().unwrap();
        let source =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/import/demo-mixed.sr");
        let open = || Ok(sigrok::read(&source)?.recording);
        let converted = |name: &str, flags: &[&str]| {
            let path = dir.path().join(name);
            // Small blocks, so a file cut off part way still holds whole ones.
            let common = common(
                &[
                    &["sigrok", "x.sr", "-o", "x.tickwave", "--block-kib", "1"],
                    flags,
                ]
                .concat(),
            );
            convert(&open, &path, &common).unwrap();
            fs::read(path).unwrap()
        };
        let shaped = converted(
            "shaped.tickwave",
            &["--offset", "D0=-5", "--keep", "A0=100.."],
        );
        let whole = converted("whole.tickwave", &[]);
        for percent in [40, 70, 99] {
            let cut = |file: &[u8], name: &str| {
                let path = dir.path().join(format!("{name}-{percent}.tickwave"));
                fs::write(&path, &file[..file.len() * percent / 100]).unwrap();
                path
            };
            let input = cut(&shaped, "shaped");
            let output = dir
                .path()
                .join(format!("shaped-{percent}-repaired.tickwave"));
            let err = repair_file(&input, &output, 3).expect_err("refused");
            assert_eq!(
                err.to_string(),
                format!(
                    "{} states the spans its channels keep, as a moved, trimmed or merged \
                     recording does, so it isn't repaired: make it again from its sources",
                    input.display()
                )
            );
            let mut partial = output.clone().into_os_string();
            partial.push(".partial");
            assert!(!output.exists() && !Path::new(&partial).exists());

            let input = cut(&whole, "whole");
            let output = dir
                .path()
                .join(format!("whole-{percent}-repaired.tickwave"));
            repair_file(&input, &output, 3).unwrap();
            let (_, _, head) = read_footer(&output).unwrap();
            assert!(head.header.channels.iter().all(|c| c.shaping.is_whole()));
            // The converter closes a sparse channel's one block at the finish. D0 and D7, high
            // at their start, have none before the 40 % cut: never known there, their declared
            // level kept.
            if percent == 40 {
                for name in ["D0", "D7"] {
                    let c = head
                        .header
                        .channels
                        .iter()
                        .find(|c| c.name == name)
                        .unwrap();
                    let ChannelKind::Digital { initial, end, .. } = c.data else {
                        panic!("{name} is digital")
                    };
                    assert!(initial, "{name}");
                    assert_eq!(Some(end), head.header.start(0), "{name}");
                }
            }
        }
    }

    /// A cut-off file whose annotation record names the generator `x/../../escaped`, beside a
    /// folder `rec.x`: its file, `rec.x/../../escaped.jsonl`, would lie outside the output's
    /// folder. Repair refuses it and writes nothing; a plain name repairs.
    #[test]
    fn a_generator_name_from_the_file_names_nothing_outside_the_output_s_folder() {
        use escapement_tickwave_format::decode::Footer;
        use escapement_tickwave_format::record::{self, Kind};
        let dir = tempfile::tempdir().unwrap();
        let source =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/import/demo-mixed.sr");
        let open = || Ok(sigrok::read(&source)?.recording);
        let whole = dir.path().join("whole.tickwave");
        let common = common(&["sigrok", "x.sr", "-o", "x.tickwave"]);
        convert(&open, &whole, &common).unwrap();
        let mut file = fs::read(&whole).unwrap();
        let footer = Footer::parse_file(&file).unwrap();
        file.truncate(footer.trailer.footer_offset as usize);
        let out = dir.path().join("a").join("b");
        fs::create_dir_all(out.join("rec.x")).unwrap();
        let escaped = dir.path().join("a").join("escaped.jsonl");
        let output = out.join("rec.tickwave");
        for source in ["x", "x/../../escaped"] {
            let mut cut = file.clone();
            let a = serde_json::json!({"source": source, "annotation": {"id": "m", "start": 1}});
            record::write(&mut cut, Kind::Annotation, &[a.to_string().as_bytes()]);
            let input = dir.path().join("cut.tickwave");
            fs::write(&input, &cut).unwrap();
            let got = repair_file(&input, &output, 3);
            assert!(!escaped.exists(), "{source}: wrote {}", escaped.display());
            if source == "x" {
                got.unwrap();
                assert!(out.join("rec.x.jsonl").exists());
                fs::remove_file(&output).unwrap();
            } else {
                let err = format!("{:#}", got.expect_err("refused"));
                assert!(err.contains("generator name"), "{err}");
                assert!(!output.exists());
            }
        }
    }

    #[test]
    fn a_sidecar_named_that_is_not_there_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let recording = dir.path().join("rec.tickwave");
        let given = dir.path().join("missing.toml");
        let e = sidecar_for(&header(), &recording, Some(&given))
            .err()
            .expect("refused");
        assert!(e.to_string().starts_with("reading "), "{e}");
    }

    /// A recut whose last `consume` leaves bytes in the output: the finish's tail holds them, so
    /// the copy holds them once and reads back.
    #[test]
    fn a_recut_writes_what_its_last_feed_left_waiting_once() {
        use escapement_tickwave_format::model::{Analog, Channel, ChannelData, Ratio, Samples};
        let dir = tempfile::tempdir().unwrap();
        let mut x = 0x2545_f491_4f6c_dd1du64;
        // Blocks of 1,024 samples in and out: each output block one slice, so the one the last
        // input block's promise closes is still to compress when the last feed ends.
        let n = 200 * 1024u64;
        let samples: Vec<f32> = (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x % 4096) as f32
            })
            .collect();
        let tb = escapement_tickwave_format::model::Ratio::integer(1_000_000).unwrap();
        let bytes = escapement_tickwave_format::encode::encode(
            Recording {
                timebase: tb,
                length: n as i64,
                channels: vec![Channel {
                    name: "A".into(),
                    source_name: "a".into(),
                    source: 0,
                    data: ChannelData::Analog(Analog {
                        unit: "V".into(),
                        first: 0,
                        period: Ratio::ONE,
                        count: n,
                        breaks: Vec::new(),
                        significant: None,
                        samples: Samples::F32(Box::new(samples.into_iter())),
                        lost_before: 0,
                        lost_after: 0,
                    }),
                    shaping: Default::default(),
                }],
                derived: Vec::new(),
                sources: vec![escapement_tickwave_format::model::Source::new(
                    SourceFormat::Other("probe".into()),
                    tb,
                )],
            },
            &escapement_tickwave_format::encode::Options {
                level: 1,
                block_bytes: 4 << 10,
                ..Default::default()
            },
        )
        .unwrap();
        let input = dir.path().join("in.tickwave");
        fs::write(&input, &bytes).unwrap();
        let options = WriteOptions {
            level: 1,
            block_bytes: 4 << 10,
            compression: Compression::Spread,
            ..WriteOptions::default()
        };
        // The case: recut_file's loop, as its reads come, leaves bytes waiting.
        let mut r = Recutter::new(options.clone());
        for chunk in bytes.chunks(1 << 20) {
            r.feed(chunk).unwrap();
            let n = r.output().len();
            r.consume(n);
        }
        assert!(
            !r.output().is_empty(),
            "nothing waits after the last consume"
        );
        let output = dir.path().join("out.tickwave");
        recut_file(&input, &output, options).unwrap();
        let (_, _, head) = read_footer(&output).unwrap();
        let (_, _, was) = read_footer(&input).unwrap();
        assert_eq!(head.header.content_id, was.header.content_id);
    }
}
