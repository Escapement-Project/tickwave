//! The project's design tokens for the demo, read from `site/tokens.css`: each face's colours
//! from its `[data-face="..."]` block, mapped to a `Palette` as verge maps its `Face`.

use std::collections::HashMap;

use egui::{Color32, FontId};
use escapement_tickwave_egui::Palette;

pub const CSS: &str = include_str!("../../../../site/tokens.css");

/// The colours a host needs around the graph: its page, and the graph's palette.
#[derive(Clone, Debug)]
pub struct Face {
    pub page: Color32,
    pub ink: Color32,
    pub night: bool,
    pub palette: Palette,
}

/// The custom properties the `[data-face="<face>"]` block sets.
fn block<'a>(css: &'a str, face: &str) -> Result<HashMap<&'a str, &'a str>, String> {
    let head = format!("[data-face=\"{face}\"] {{");
    let start = css.find(&head).ok_or(format!("no {head} block"))? + head.len();
    let end = start + css[start..].find('}').ok_or("an unclosed block")?;
    Ok(css[start..end]
        .split(';')
        .filter_map(|d| d.split_once(':'))
        .map(|(k, v)| (k.trim(), v.trim()))
        .filter(|(k, _)| k.starts_with("--"))
        .collect())
}

/// Red, green, blue and alpha from 0 to 1, the colour channels premultiplied by alpha.
type Premultiplied = [f32; 4];

/// The colour a property's value names: `#rrggbb`, `transparent`, `var(--name)`, or
/// `color-mix(in srgb, <colour> <p>%, <colour>)`.
fn colour(props: &HashMap<&str, &str>, value: &str) -> Result<Premultiplied, String> {
    let value = value.trim();
    if value == "transparent" {
        return Ok([0.0; 4]);
    }
    if let Some(hex) = value.strip_prefix('#') {
        let byte = |i: usize| {
            u8::from_str_radix(hex.get(i..i + 2).ok_or(format!("#{hex}"))?, 16)
                .map(|b| f32::from(b) / 255.0)
                .map_err(|e| format!("#{hex}: {e}"))
        };
        let a = if hex.len() == 8 { byte(6)? } else { 1.0 };
        return Ok([byte(0)? * a, byte(2)? * a, byte(4)? * a, a]);
    }
    if let Some(name) = value.strip_prefix("var(").and_then(|v| v.strip_suffix(')')) {
        let v = props.get(name.trim()).ok_or(format!("no {name}"))?;
        return colour(props, v);
    }
    if let Some(args) = value
        .strip_prefix("color-mix(")
        .and_then(|v| v.strip_suffix(')'))
    {
        let mut parts = args.split(',').map(str::trim);
        if parts.next() != Some("in srgb") {
            return Err(format!("{value}: only srgb mixes"));
        }
        let side = |part: Option<&str>| -> Result<(Premultiplied, Option<f32>), String> {
            let part = part.ok_or(format!("{value}: two colours"))?;
            match part.rsplit_once(' ') {
                Some((c, p)) if p.ends_with('%') => {
                    let p: f32 = p[..p.len() - 1].parse().map_err(|e| format!("{p}: {e}"))?;
                    Ok((colour(props, c)?, Some(p / 100.0)))
                }
                _ => Ok((colour(props, part)?, None)),
            }
        };
        let (a, pa) = side(parts.next())?;
        let (b, pb) = side(parts.next())?;
        let p = match (pa, pb) {
            (Some(p), _) => p,
            (None, Some(q)) => 1.0 - q,
            (None, None) => 0.5,
        };
        return Ok(std::array::from_fn(|i| a[i] * p + b[i] * (1.0 - p)));
    }
    Err(format!("{value}: not a colour this reads"))
}

fn color32(c: Premultiplied) -> Color32 {
    let [r, g, b, a] = c.map(|x| (x * 255.0).round().clamp(0.0, 255.0) as u8);
    Color32::from_rgba_premultiplied(r, g, b, a)
}

/// The opacities verge's bench draws with (verge gui/src/bench.rs): a column that touches a
/// loss and a sample read under range, a column's band, lost time, and a busy DIO column. They
/// are not tokens.
const PALE: f32 = 0.35;
const FILL: f32 = 0.45;
const SHADE: f32 = 0.2;
const BUSY: f32 = 0.6;

/// The day or night face, as verge fills its graph from tokens.rs: the trace in
/// `--series-verge`, the selection in `--wash`, figures and labels in monospace at 11 points.
pub fn face(night: bool) -> Result<Face, String> {
    let props = block(CSS, if night { "night" } else { "day" })?;
    let get = |name: &str| -> Result<Color32, String> {
        let value = props.get(name).ok_or(format!("no {name}"))?;
        Ok(color32(colour(&props, value)?))
    };
    Ok(Face {
        page: get("--page")?,
        ink: get("--ink")?,
        night,
        palette: Palette {
            trace: get("--series-verge")?,
            surface: get("--surface")?,
            hair: get("--hair")?,
            ink: get("--ink")?,
            ink_2: get("--ink-2")?,
            attention_fill: get("--attention-fill")?,
            attention_ink: get("--attention-ink")?,
            wash: get("--wash")?,
            figures: FontId::monospace(11.0),
            labels: FontId::monospace(11.0),
            pale: PALE,
            fill: FILL,
            shade: SHADE,
            busy: BUSY,
        },
    })
}

/// egui's own widgets in the face's page and ink.
pub fn visuals(face: &Face) -> egui::Visuals {
    let mut v = if face.night {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    v.panel_fill = face.page;
    v.window_fill = face.palette.surface;
    v.extreme_bg_color = face.palette.surface;
    v.override_text_color = Some(face.ink);
    v
}
