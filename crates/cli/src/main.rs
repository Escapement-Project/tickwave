//! `tickwave`: convert captures into tickwave's recording format, describe a recording, and check one
//! against its source.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use indicatif::ProgressStyle;
use tickwave_format::annotations::{self, Included};
use tickwave_format::decode::{Head, verify_limited};
use tickwave_format::encode::{Options, encode};
use tickwave_format::header::{AnalogCoding, ChannelKind, Header};
use tickwave_format::model::{Channel, ChannelData, Recording, Samples};
use tickwave_import::sigrok::Skipped;
use tickwave_import::{codes, saleae, sigrok, vcd};
use tracing::{Span, info, info_span, warn};
use tracing_indicatif::IndicatifLayer;
use tracing_indicatif::span_ext::IndicatifSpanExt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[derive(Parser)]
#[command(
    name = "tickwave",
    version,
    about = "Convert, describe and verify tickwave recordings"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// The most to decode from one block, overview block or head overview, in MiB. A recording
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
        #[command(flatten)]
        common: Common,
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

fn main() -> Result<()> {
    let indicatif = IndicatifLayer::new();
    tracing_subscriber::registry()
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
                    common,
                },
        } => convert(
            &|| Ok(saleae::read(&dir, &channels)?),
            &output,
            &common,
            max,
        ),
        Command::Convert {
            from:
                Source::Sigrok {
                    file,
                    output,
                    channels,
                    common,
                },
        } => {
            report_skipped(&sigrok::read(&file)?.skipped);
            convert(
                &|| select(sigrok::read(&file)?.recording, &channels),
                &output,
                &common,
                max,
            )
        }
        Command::Convert {
            from:
                Source::Vcd {
                    file,
                    output,
                    channels,
                    common,
                },
        } => {
            report_skipped(&vcd::read(&file)?.skipped);
            convert(
                &|| select(vcd::read(&file)?.recording, &channels),
                &output,
                &common,
                max,
            )
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
    }
}

/// Wraps every channel's iterator so the span's bar counts the items taken.
fn counted(recording: Recording, span: &Span) -> Recording {
    const STEP: u64 = 1 << 16;
    let total: u64 = recording
        .channels
        .iter()
        .map(|c| match &c.data {
            ChannelData::Digital(d) => d.edges.size_hint().0 as u64,
            ChannelData::Analog(a) => a.count,
        })
        .sum();
    span.pb_set_style(
        &ProgressStyle::with_template("{span_name} {wide_bar} {pos}/{len} {per_sec} {eta}")
            .expect("a fixed template"),
    );
    span.pb_set_length(total);
    let count = |span: Span, mut n: u64| {
        move || {
            n += 1;
            if n.is_multiple_of(STEP) {
                span.pb_inc(STEP);
            }
        }
    };
    let channels = recording
        .channels
        .into_iter()
        .map(|mut c| {
            c.data = match c.data {
                ChannelData::Digital(mut d) => {
                    let mut tick = count(span.clone(), 0);
                    d.edges = Box::new(d.edges.inspect(move |_| tick()));
                    ChannelData::Digital(d)
                }
                ChannelData::Analog(mut a) => {
                    let mut tick = count(span.clone(), 0);
                    let Samples::F32(samples) = a.samples;
                    a.samples = Samples::F32(Box::new(samples.inspect(move |_| tick())));
                    ChannelData::Analog(a)
                }
            };
            c
        })
        .collect();
    Recording {
        channels,
        ..recording
    }
}

fn report_skipped(skipped: &[Skipped]) {
    for s in skipped {
        warn!("left out {}: {}", s.name, s.reason);
    }
}

/// Keeps the channels named in `wanted`, in its order, renamed; all of them when it is empty.
fn select(recording: Recording, wanted: &[(String, String)]) -> Result<Recording> {
    if wanted.is_empty() {
        return Ok(recording);
    }
    let mut channels: Vec<Option<Channel>> = recording.channels.into_iter().map(Some).collect();
    let mut out = Vec::with_capacity(wanted.len());
    for (from, to) in wanted {
        let found = channels
            .iter_mut()
            .find(|c| c.as_ref().is_some_and(|c| c.name == *from))
            .and_then(Option::take);
        let Some(mut channel) = found else {
            let names: Vec<_> = channels.iter().flatten().map(|c| c.name.as_str()).collect();
            bail!("no channel {from:?}; the source has {names:?}");
        };
        channel.name = to.clone();
        out.push(channel);
    }
    Ok(Recording {
        channels: out,
        ..recording
    })
}

/// Sets analog channels' units, by channel name.
fn set_units(mut recording: Recording, units: &[(String, String)]) -> Result<Recording> {
    for (name, unit) in units {
        let channel = recording
            .channels
            .iter_mut()
            .find(|c| c.name == *name)
            .with_context(|| format!("--unit: no channel {name:?}"))?;
        let ChannelData::Analog(a) = &mut channel.data else {
            bail!("--unit: {name} is digital");
        };
        a.unit = unit.clone();
    }
    Ok(recording)
}

/// Finds exact integer codes for each analog channel, or None where the values have none.
fn recover_codes(recording: Recording) -> Vec<Option<AnalogCoding>> {
    recording
        .channels
        .into_iter()
        .map(|c| match c.data {
            ChannelData::Digital(_) => None,
            ChannelData::Analog(a) => {
                let name = c.name.clone();
                let span = info_span!("codes", channel = %name);
                let _entered = span.enter();
                let one = Recording {
                    timebase: recording.timebase,
                    length: recording.length,
                    channels: vec![Channel {
                        data: ChannelData::Analog(a),
                        ..c
                    }],
                    source: Default::default(),
                };
                let ChannelData::Analog(a) = counted(one, &span).channels.remove(0).data else {
                    unreachable!("the channel was built analog")
                };
                let Samples::F32(samples) = a.samples;
                let codes = codes::recover_codes(samples);
                match &codes {
                    Some(k) => info!("{name}: {}-bit codes", k.bits),
                    None => info!("{name}: no exact codes, stored as f32"),
                }
                codes.map(|k| k.coding())
            }
        })
        .collect()
}

/// Converts what `open` reads. It opens the source three times: to find analog codes, to encode,
/// and to check the output before writing it.
fn convert(
    open: &dyn Fn() -> Result<Recording>,
    output: &Path,
    common: &Common,
    max: usize,
) -> Result<()> {
    let started = Instant::now();
    let open = || set_units(open()?, &common.units);
    let analog = if common.no_codes {
        Vec::new()
    } else {
        recover_codes(open()?)
    };
    let options = Options {
        level: common.level,
        block_bytes: common.block_kib as usize * 1024,
        title: common.title.clone(),
        meta: common.meta.iter().cloned().collect::<BTreeMap<_, _>>(),
        converter: Some(format!(
            "tickwave {} (zstd {}, blocks of {} KiB)",
            env!("CARGO_PKG_VERSION"),
            common.level,
            common.block_kib
        )),
        analog,
    };
    let file = {
        let span = info_span!("encode");
        let _entered = span.enter();
        encode(counted(open()?, &span), &options).context("encoding")?
    };
    {
        let span = info_span!("verify");
        let _entered = span.enter();
        verify_limited(&file, counted(open()?, &span), max)
            .context("the output does not match the source")?;
    }
    write_atomically(output, &file)?;
    let head = Head::parse_file_limited(&file, max)?;
    info!(
        "{}: {:.2} MB, {} blocks, {} in {:.1} s",
        output.display(),
        file.len() as f64 / 1e6,
        head.rows.len(),
        head.header.content_id,
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

/// Writes beside the destination, then renames, so a failed run never leaves half a file.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".partial");
    let tmp = PathBuf::from(tmp);
    let mut file = fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("writing {}", tmp.display()));
    }
    fs::rename(&tmp, path).with_context(|| format!("renaming to {}", path.display()))
}

fn read_head(file: &Path, max: usize) -> Result<(Vec<u8>, Head)> {
    let bytes = fs::read(file).with_context(|| format!("reading {}", file.display()))?;
    let head = Head::parse_file_limited(&bytes, max)
        .with_context(|| format!("parsing {}", file.display()))?;
    Ok((bytes, head))
}

/// A tick as seconds, to the decimal place of one tick and no further: as many digits as the
/// timebase carries. Ticks where the arithmetic would pass u128.
fn seconds(ticks: i64, header: &Header) -> String {
    let [num, den] = header.timebase.map(u128::from);
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
    let (bytes, head) = read_head(file, max)?;
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
            "file_bytes": bytes.len(),
            "head_bytes": head.preamble.head_len,
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
        h.timebase[0],
        h.timebase[1],
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
        "{:.3} MB: head {} bytes ({} overview levels), {} blocks of {}-{} bytes, median {median}",
        bytes.len() as f64 / 1e6,
        head.preamble.head_len,
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
    let (bytes, head) = read_head(file, max)?;
    let h = &head.header;
    let wanted: Vec<(String, String)> = h
        .channels
        .iter()
        .map(|c| (c.source_name.clone(), c.name.clone()))
        .collect();
    let source = match h.source.format.as_str() {
        "saleae-binary-1" => saleae::read(against, &wanted)?,
        "sigrok-sr" => select(sigrok::read(against)?.recording, &wanted)?,
        "vcd" => select(vcd::read(against)?.recording, &wanted)?,
        other => bail!("verifying against {other} sources is not supported"),
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
    let span = info_span!("verify");
    let _entered = span.enter();
    verify_limited(&bytes, counted(source, &span), max)
        .context("the recording does not match its source")?;
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
    let hand =
        fs::read_to_string(sidecar).with_context(|| format!("reading {}", sidecar.display()))?;
    #[derive(serde::Deserialize)]
    struct Includes {
        #[serde(default)]
        include: Vec<String>,
    }
    // A TOML error here is reported by resolve below, with the rest.
    let names = toml::from_str::<Includes>(&hand)
        .map(|i| i.include)
        .unwrap_or_default();
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
    let included: Vec<Included<'_>> = files[1..]
        .iter()
        .map(|(name, text)| Included { name, text })
        .collect();
    let file_name = recording
        .file_name()
        .map(|n| n.to_string_lossy().into_owned());
    match annotations::resolve(&files[0].1, &included, header, file_name.as_deref()) {
        Ok(resolved) => Ok((resolved, files)),
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
    let (_, head) = read_head(recording, max)?;
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
    let (bytes, head) = read_head(recording, max)?;
    let (resolved, files) = load_sidecar(&head.header, recording, sidecar)?;
    let bundled = tickwave_format::bundle::bundle(&bytes, &head, files.into_iter().collect())?;
    let check = Head::parse_file_limited(&bundled, max)?;
    if check.header.content_id != head.header.content_id {
        bail!("bundling changed the content id");
    }
    write_atomically(output, &bundled)?;
    info!(
        "{}: {} with {} annotations",
        output.display(),
        recording.display(),
        resolved.len()
    );
    Ok(())
}
