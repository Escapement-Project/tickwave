//! Paint roles (ANNOTATIONS.md, "Roles"): each role's look, field by field, from the host's own
//! setting, then the recording's declaration, then the built-in look of its place among the
//! declared roles; which role draws each stretch of a row; and where a dash's phase starts. Ports
//! `web/src/roles.ts`, and both are held to `testdata/view/roles.json`, so a native renderer and
//! the browser draw a role alike.

use escapement_tickwave_format::annotations::Role;

/// The dashes the third declared role on takes where neither the host nor the declaration gives
/// one, in turn, in line widths.
pub const DASHES: [&[f64]; 4] = [&[2.0, 2.0], &[1.0, 1.0], &[5.0, 2.0, 1.0, 2.0], &[8.0, 2.0]];

/// What a host's own setting says of a role's look, `C` its colour type: None for each field it
/// leaves to the recording.
#[derive(Clone, Debug, PartialEq)]
pub struct Host<C> {
    pub colour: Option<C>,
    /// Added to the trace's line weight.
    pub weight: Option<f64>,
    /// (dash, gap) pairs in the role's line widths; `Some(None)` for a solid line over a dash the
    /// declaration or the place gives.
    pub dash: Option<Option<Vec<f64>>>,
}

impl<C> Default for Host<C> {
    fn default() -> Self {
        Self {
            colour: None,
            weight: None,
            dash: None,
        }
    }
}

/// Where a look's colour comes from.
#[derive(Clone, Debug, PartialEq)]
pub enum LookColour<C> {
    /// The host's own.
    Host(C),
    /// The declaration's on the host's face, `#rrggbb`.
    Stored(String),
    /// The trace's, as the first declared role's place gives it.
    Trace,
    /// Ink, as every later place gives it.
    Ink,
}

/// How a role's lines draw, and what its key says.
#[derive(Clone, Debug, PartialEq)]
pub struct Look<C> {
    pub label: String,
    pub colour: LookColour<C>,
    /// Added to the trace's line weight.
    pub weight: f64,
    /// (dash, gap) pairs in the role's line widths; None for a solid line.
    pub dash: Option<Vec<f64>>,
}

/// Role `name`'s look among `roles`, the order the sidecar declares them in, each field from the
/// first that gives it: `host`, the declaration, then the role's place. The first place takes the
/// trace's colour and the rest ink; the third on are dashed in turn (`DASHES`). A weight is the
/// declaration's. `night` picks a declared colour's night face. None for a role `roles` doesn't
/// hold: a paint naming one draws as if it painted nothing.
pub fn look<C>(roles: &[Role], name: &str, host: Host<C>, night: bool) -> Option<Look<C>> {
    let at = roles.iter().position(|r| r.name == name)?;
    let declared = &roles[at];
    let colour = match (host.colour, &declared.colour) {
        (Some(c), _) => LookColour::Host(c),
        (None, Some(c)) => LookColour::Stored(if night { &c.night } else { &c.day }.clone()),
        (None, None) if at == 0 => LookColour::Trace,
        (None, None) => LookColour::Ink,
    };
    let dash = match host.dash {
        Some(dash) => dash,
        None => declared
            .dash
            .clone()
            .or_else(|| at.checked_sub(2).map(|k| DASHES[k % DASHES.len()].to_vec())),
    };
    Some(Look {
        label: declared.label.clone(),
        colour,
        weight: host.weight.unwrap_or(declared.weight),
        dash,
    })
}

/// A stretch of a row's columns, `px0` to `px1`, and the role that draws it: its place in the
/// roles given to `stretches`, None for the trace's own look.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stretch {
    pub px0: i64,
    pub px1: i64,
    pub role: Option<usize>,
}

/// Columns `px0` to `px1` of a row cut where the role drawing them changes. `spans` are the paints
/// over the row in sidecar order, each its first column, the column past its last, and its role;
/// where several cover a column, the latest draws it. A span naming a role `roles` doesn't hold
/// draws nothing, so what lies under it shows. Neighbours in one role are one stretch.
pub fn stretches(spans: &[(i64, i64, &str)], roles: &[Role], px0: i64, px1: i64) -> Vec<Stretch> {
    // (column, span, whether it begins there); a span is its place in sidecar order and its role's.
    let mut marks: Vec<(i64, usize, bool)> = Vec::new();
    let mut kept: Vec<usize> = Vec::new();
    for &(from, to, role) in spans {
        let (from, to) = (from.max(px0), to.min(px1));
        let Some(r) = roles.iter().position(|r| r.name == role) else {
            continue;
        };
        if to <= from {
            continue;
        }
        marks.push((from, kept.len(), true));
        marks.push((to, kept.len(), false));
        kept.push(r);
    }
    marks.sort_by_key(|&(at, ..)| at);
    let mut out: Vec<Stretch> = Vec::new();
    let mut active = std::collections::BTreeSet::new();
    let mut stretch = |from: i64, to: i64, active: &std::collections::BTreeSet<usize>| {
        let role = active.last().map(|&i| kept[i]);
        match out.last_mut() {
            Some(last) if last.role == role && last.px1 == from => last.px1 = to,
            _ => out.push(Stretch {
                px0: from,
                px1: to,
                role,
            }),
        }
    };
    let mut x = px0;
    for (at, i, on) in marks {
        if at > x {
            stretch(x, at, &active);
            x = at;
        }
        if on {
            active.insert(i);
        } else {
            active.remove(&i);
        }
    }
    if x < px1 {
        stretch(x, px1, &active);
    }
    out
}

/// The length of a row's line at column `x`, where a dashed stretch starting there starts its
/// pattern: the last point at or before `x`, and the way along the segment after it. `paths` are
/// the line's paths in order, each its points' x and the line's length there, flat. A lifted pen
/// adds nothing, so between two paths the length is the first's at its end.
pub fn length_at<P: AsRef<[f64]>>(paths: &[P], x: f64) -> f64 {
    let at = paths.partition_point(|p| p.as_ref().first().is_some_and(|&x0| x0 <= x));
    let Some(points) = paths.get(at.saturating_sub(1)).map(AsRef::as_ref) else {
        return 0.0;
    };
    let mut k = 0;
    while k + 2 < points.len() && points[k + 2] <= x {
        k += 2;
    }
    if k + 2 >= points.len() || points[k] > x {
        return points[k + 1];
    }
    points[k + 1] + (x - points[k]) * (points[k + 3] - points[k + 1]) / (points[k + 2] - points[k])
}
