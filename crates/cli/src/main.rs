//! `tickwave`: convert captures into tickwave's recording format, describe a recording, check one
//! against its source, repair one cut off before its end, and open one in the browser.

mod open;

use std::cell::RefCell;
use std::fs;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use escapement_tickwave_format::annotations::{self, Included};
use escapement_tickwave_format::decode::{DecodeError, FIRST_READ, Footer, inflate_limited};
use escapement_tickwave_format::header::{ChannelKind, Header};
use escapement_tickwave_format::model::{RateOrigin, Recording, SourceFormat};
use escapement_tickwave_format::repair::{RepairError, Repairer};
use escapement_tickwave_format::verify::verify_limited;
use escapement_tickwave_format::writer::generator_file;
use escapement_tickwave_import::convert::{
    self, Keep, Pass, Progress, Settings, Step, counted, select, set_units,
};
use escapement_tickwave_import::saleae::SampleRate;
use escapement_tickwave_import::shape::check_time;
use escapement_tickwave_import::sigrok::Skipped;
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
    about = "Convert, describe, verify, repair and open tickwave recordings"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// The most to decode from one block, overview block or footer overview, in MiB. A recording
    /// that needs more is refused, as a page's reader refuses it unless configured.
    #[arg(long, global = true, default_value_t = 64, value_name = "MIB")]
    max_decoded_mib: u16,
}

#[derive(Subcommand)]
enum Command {
    /// Convert a capture into a tickwave recording. The output is checked against the source
    /// before it is written.
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
    },
    /// Check a recording against the source it was converted from, edge by edge and sample by
    /// sample.
    Verify {
        file: PathBuf,
        /// The source: for a Saleae export, its directory.
        #[arg(long)]
        against: PathBuf,
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
    /// Open the recording in the browser once it is written and checked, as `tickwave open`
    /// does, and serve it until Ctrl-C.
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
    fn serve(
        &self,
        viewer: Option<Box<dyn open::Site>>,
        recording: &Path,
        max: usize,
    ) -> Result<()> {
        match viewer {
            Some(site) => open_file(
                site,
                recording,
                None,
                self.port.unwrap_or(0),
                !self.no_browser,
                max,
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

fn main() -> Result<()> {
    let indicatif = IndicatifLayer::new();
    tracing_subscriber::registry()
        .with(LevelFilter::INFO)
        .with(tracing_subscriber::fmt::layer().with_writer(indicatif.get_stderr_writer()))
        .with(indicatif)
        .init();
    let cli = Cli::parse();
    let max = usize::from(cli.max_decoded_mib) << 20;
    match cli.command {
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
            if recording.source.rate == Some(RateOrigin::Inferred) {
                info!(
                    "sample rate {} Hz, inferred from the edges",
                    recording.timebase.num()
                );
            }
            convert(&|| Ok(export.recording()), &output, &common, max)?;
            and_open.serve(viewer, &output, max)
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
                max,
            )?;
            and_open.serve(viewer, &output, max)
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
                max,
            )?;
            and_open.serve(viewer, &output, max)
        }
        Command::Info { file, json } => info(&file, json, max),
        Command::Verify { file, against } => verify_file(&file, &against, max),
        Command::Bundle {
            recording,
            sidecar,
            output,
        } => bundle_file(&recording, &sidecar, &output, max),
        Command::Annotations {
            command:
                AnnotationsCommand::Check {
                    recording,
                    sidecar,
                    json,
                },
        } => check_annotations(&recording, &sidecar, json, max),
        Command::Repair {
            file,
            output,
            level,
        } => repair_file(&file, &output, level),
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
            max,
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

/// Converts what `open` reads, checks the output against the source, and writes it.
fn convert(
    open: &dyn Fn() -> Result<Recording, convert::Error>,
    output: &Path,
    common: &Common,
    max: usize,
) -> Result<()> {
    let started = Instant::now();
    let settings = common.settings();
    let converted = write_atomically(output, |file| {
        Ok(convert::convert(
            open,
            &settings,
            max,
            &Bars::default(),
            file,
        )?)
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
        converted.header.blocks,
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

/// The recording at `path`, open, its length, and its footer, read from its end.
fn read_footer(path: &Path, max: usize) -> Result<(fs::File, u64, Footer)> {
    let mut file = fs::File::open(path).with_context(|| format!("reading {}", path.display()))?;
    let len = file.metadata()?.len();
    let mut from = len.saturating_sub(FIRST_READ as u64);
    loop {
        file.seek(SeekFrom::Start(from))?;
        let mut tail = Vec::new();
        file.read_to_end(&mut tail)?;
        match Footer::parse_with(&tail, len, |f, n| inflate_limited(f, n, max)) {
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

fn info(file: &Path, json: bool, max: usize) -> Result<()> {
    let (_, len, head) = read_footer(file, max)?;
    let h = &head.header;
    let mut lengths: Vec<u32> = head.rows.iter().map(|r| r.length).collect();
    lengths.sort_unstable();
    let median = lengths.get(lengths.len() / 2).copied().unwrap_or(0);
    let blocks_total: u64 = lengths.iter().map(|&l| u64::from(l)).sum();
    let overview_rows = head.overview_rows.iter().flatten();
    let overview_count = overview_rows.clone().count();
    let overview_total: u64 = overview_rows.map(|r| u64::from(r.length)).sum();
    if json {
        let out = serde_json::json!({
            "header": h,
            "file_bytes": len,
            "footer_bytes": head.trailer.footer_len,
            "block_bytes": { "total": blocks_total, "min": lengths.first(), "median": median, "max": lengths.last() },
            "overview_block_bytes": { "count": overview_count, "total": overview_total },
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    if let Some(title) = &h.title {
        println!("{title}");
    }
    println!(
        "{} at {}/{} ticks a second, {} channels, {}",
        seconds(h.length, h),
        h.timebase.num(),
        h.timebase.den(),
        h.channels.len(),
        h.content_id
    );
    for c in &h.channels {
        match &c.data {
            ChannelKind::Digital {
                initial,
                end,
                edges,
                ..
            } => println!(
                "  {:<12} digital, {edges} edges, starts {}, known to {} ({})",
                c.name,
                u8::from(*initial),
                seconds(*end, h),
                c.source_name
            ),
            ChannelKind::Analog {
                unit,
                count,
                coding,
                ..
            } => println!(
                "  {:<12} analog, {count} samples in {unit}, {coding:?} ({})",
                c.name, c.source_name
            ),
        }
    }
    println!(
        "{:.3} MB: footer {} bytes ({} overview levels), {} blocks of {}-{} bytes, median {median}",
        len as f64 / 1e6,
        head.trailer.footer_len,
        h.overview.len(),
        lengths.len(),
        lengths.first().unwrap_or(&0),
        lengths.last().unwrap_or(&0),
    );
    if overview_count > 0 {
        let shifts: Vec<String> = h
            .overview_blocks
            .iter()
            .map(|l| format!("2^{}", l.shift))
            .collect();
        println!(
            "  overview levels {} in {overview_count} blocks, {overview_total} bytes",
            shifts.join(", ")
        );
    }
    for (k, v) in &h.meta {
        println!("  {k} = {v}");
    }
    Ok(())
}

fn verify_file(file: &Path, against: &Path, max: usize) -> Result<()> {
    let (mut opened, _, head) = read_footer(file, max)?;
    let h = &head.header;
    // Where the recording starts in the source is not in the file, so the moves and cuts cannot
    // be made again.
    if let Some(c) = h.channels.iter().find(|c| !c.shaping.is_whole()) {
        bail!(
            "{}: channel {} was moved or cut when it was converted; verify checks a recording \
             converted whole",
            file.display(),
            c.name
        );
    }
    let wanted: Vec<(String, String)> = h
        .channels
        .iter()
        .map(|c| (c.source_name.clone(), c.name.clone()))
        .collect();
    let saleae = |rate| -> Result<Recording> {
        Ok(
            convert::check_saleae(saleae_headers(against, &wanted, rate)?, &Bars::default())?
                .recording(),
        )
    };
    let source = match &h.source.format {
        SourceFormat::SaleaeBinary1 => saleae(None)?,
        // A version 0 export's digital files don't record the rate the recording was made at.
        SourceFormat::SaleaeBinary0 => saleae(Some(
            SampleRate::of_timebase(h.timebase)
                .context("the recording's timebase is not a whole number of hertz")?,
        ))?,
        SourceFormat::SigrokSr => select(sigrok::read(against)?.recording, &wanted)?,
        SourceFormat::Vcd => select(vcd::read(against)?.recording, &wanted)?,
        SourceFormat::Other(name) => bail!("verifying against {name} sources is not supported"),
    };
    let units: Vec<(String, String)> = h
        .channels
        .iter()
        .filter_map(|c| match &c.data {
            ChannelKind::Analog { unit, .. } => Some((c.name.clone(), unit.clone())),
            ChannelKind::Digital { .. } => None,
        })
        .collect();
    let source = set_units(source, &units)?;
    let bars = Bars::default();
    opened.seek(SeekFrom::Start(0))?;
    verify_limited(
        BufReader::with_capacity(1 << 20, opened),
        counted(source, &bars, Pass::Verify),
        max,
    )
    .context("the recording does not match its source")?;
    bars.end(Pass::Verify);
    info!("{}: matches {}", file.display(), against.display());
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

fn check_annotations(recording: &Path, sidecar: &Path, json: bool, max: usize) -> Result<()> {
    let (_, _, head) = read_footer(recording, max)?;
    let (resolved, _) = load_sidecar(&head.header, recording, sidecar)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&resolved)?);
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

fn bundle_file(recording: &Path, sidecar: &Path, output: &Path, max: usize) -> Result<()> {
    let (mut file, _, head) = read_footer(recording, max)?;
    let (resolved, files) = load_sidecar(&head.header, recording, sidecar)?;
    let (keep, tail) =
        escapement_tickwave_format::bundle::bundled_tail(&head, files.into_iter().collect())?;
    write_atomically(output, |out| copy_then(&mut file, keep, &tail, out))?;
    let (_, _, check) = read_footer(output, max)?;
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
    let repaired = match repairer.finish() {
        Ok(repaired) => repaired,
        Err(RepairError::Shaped(_)) => bail!(
            "{} was moved or trimmed when it was converted, so it isn't repaired: convert it again \
             from its source",
            file.display()
        ),
        Err(e) => return Err(e).with_context(|| format!("repairing {}", file.display())),
    };
    write_atomically(output, |out| {
        copy_then(&mut opened, repaired.keep, &repaired.tail, out)
    })?;
    let stem = output
        .file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    let dir = output.parent().unwrap_or(Path::new(""));
    for (source, lines) in &repaired.generated {
        let path = dir.join(format!("{stem}.{source}.jsonl"));
        let text = generator_file(source, None, repaired.header.content_id, lines);
        write_atomically(&path, |f| Ok(f.write_all(text.as_bytes())?))?;
        info!("{}: {} annotations", path.display(), lines.len());
    }
    info!(
        "{}: {} of {} bytes kept, {} blocks to {}, {}",
        output.display(),
        repaired.keep,
        len,
        repaired.header.blocks,
        seconds(repaired.header.length, &repaired.header),
        repaired.header.content_id
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
    max: usize,
) -> Result<()> {
    let (_, _, head) = read_footer(recording, max)?;
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
            data: ChannelKind::Digital {
                initial: true,
                end: 1_000_000,
                edges: 10,
                gaps: Vec::new(),
            },
            shaping: Default::default(),
        };
        Header {
            timebase: escapement_tickwave_format::model::Ratio::integer(1_000_000_000).unwrap(),
            length: 1_000_000,
            channels: vec![digital("SWCLK"), digital("SWDIO")],
            source: SourceHeader::new(SourceFormat::Vcd),
            title: None,
            meta: Default::default(),
            content_id: "blake3:00ff000000000000000000000000000000000000000000000000000000000000"
                .parse()
                .unwrap(),
            blocks: 1,
            overview_shift: 12,
            overview: vec![12],
            overview_blocks: vec![],
            annotations: None,
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
            convert(&open, &output, &common, 64 << 20)
        };
        let err = run(&["--offset", "D0=5", "--offset", "D0=6"]).expect_err("refused");
        assert_eq!(err.to_string(), r#""D0" is given an offset twice"#);
        assert!(!output.exists());

        run(&["--offset", "D0=-5", "--keep", "D1=10..100"]).unwrap();
        let (_, _, head) = read_footer(&output, 64 << 20).unwrap();
        let shaping: Vec<_> = head.header.channels[..2]
            .iter()
            .map(|c| (c.shaping.offset.as_ref().map(|o| o.ticks), c.shaping.kept))
            .collect();
        use escapement_tickwave_format::header::Kept;
        // D0, moved 5 earlier, starts the recording and is kept whole: its 5,000 samples.
        assert_eq!(
            shaping,
            [
                (Some(-5), Some(Kept { from: 0, to: 5000 })),
                (None, Some(Kept { from: 15, to: 105 }))
            ]
        );
        let err = verify_file(&output, &source, 64 << 20).expect_err("refused");
        assert_eq!(
            err.to_string(),
            format!(
                "{}: channel D0 was moved or cut when it was converted; verify checks a recording \
                 converted whole",
                output.display()
            )
        );
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
            convert(&open, &path, &common, 64 << 20).unwrap();
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
                    "{} was moved or trimmed when it was converted, so it isn't repaired: convert \
                     it again from its source",
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
            let (_, _, head) = read_footer(&output, 64 << 20).unwrap();
            assert!(head.header.channels.iter().all(|c| c.shaping.is_whole()));
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
}
