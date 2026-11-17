//! Paint roles (ANNOTATIONS.md, "Roles"): each role's look, field by field, from the host's own
//! setting, then the recording's declaration, and for its colour the role's place among the
//! declared roles; which role draws each stretch of a row; and where a dash's phase starts. Ports
//! `web/src/roles.ts`, and both are held to `testdata/view/roles.json`, so a native renderer and
//! the browser draw a role alike.

use escapement_tickwave_format::annotations::Role;
use escapement_tickwave_format::annotations::roles::{dash_fits, weight_fits};

/// How a role's lines run: solid, or dashed.
#[derive(Clone, Debug, PartialEq)]
pub enum Dash {
    Solid,
    /// (dash, gap) pairs, in the role's line widths, its weight included.
    Pattern(Vec<f64>),
}

/// What a host's own setting says of a role's look, `C` its colour type: None for each field it
/// leaves to the recording. A weight or a dash past the bounds a declaration's are held to
/// (`weight_fits`, `dash_fits`) is left to the recording too, as a page's is.
#[derive(Clone, Debug, PartialEq)]
pub struct Host<C> {
    pub colour: Option<C>,
    /// Added to the trace's line weight.
    pub weight: Option<f64>,
    /// `Dash::Solid` makes a role the declaration dashes solid.
    pub dash: Option<Dash>,
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
    /// The declaration's on the host's face: red, green, blue.
    Stored([u8; 3]),
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
    pub dash: Dash,
}

/// `#rrggbb` as red, green and blue; None for anything else. A declaration's colours, and a stored
/// view's, are held to that form when they are read.
pub fn rgb(text: &str) -> Option<[u8; 3]> {
    let hex = (text.strip_prefix('#'))
        .filter(|h| h.len() == 6 && h.bytes().all(|b| b.is_ascii_hexdigit()))?;
    let at = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some([at(0)?, at(2)?, at(4)?])
}

/// Role `name`'s look among `roles`, the order the sidecar declares them in, each field from the
/// first that gives it: `host`, then the declaration. A colour neither gives is the place's: the
/// first role takes the trace's colour and the rest ink. A dash neither gives is solid, and a
/// weight the declaration's. `night` picks a declared colour's night face. None for a role
/// `roles` doesn't hold: a paint naming one draws as if it painted nothing.
pub fn look<C>(roles: &[Role], name: &str, host: Host<C>, night: bool) -> Option<Look<C>> {
    let at = roles.iter().position(|r| r.name == name)?;
    let declared = &roles[at];
    let stored =
        (declared.colour.as_ref()).and_then(|c| rgb(if night { &c.night } else { &c.day }));
    let colour = match (host.colour, stored) {
        (Some(c), _) => LookColour::Host(c),
        (None, Some(c)) => LookColour::Stored(c),
        (None, None) if at == 0 => LookColour::Trace,
        (None, None) => LookColour::Ink,
    };
    let dash = match host.dash {
        Some(Dash::Pattern(p)) if dash_fits(&p) => Dash::Pattern(p),
        Some(Dash::Solid) => Dash::Solid,
        _ => declared.dash.clone().map_or(Dash::Solid, Dash::Pattern),
    };
    Some(Look {
        label: declared.label.clone(),
        colour,
        weight: host
            .weight
            .filter(|&w| weight_fits(w))
            .unwrap_or(declared.weight),
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
/// adds nothing, so between two paths the length is the first's at its end. A path with no whole
/// point is passed over, and a number past a path's last whole point is not read. 0 where no path
/// has a point.
///
/// The paths, and the points in each, are found by halving: a row's run left to right. Out of
/// order, the length is unreliable, but every read stays in the paths given.
pub fn length_at<P: AsRef<[f64]>>(paths: &[P], x: f64) -> f64 {
    let whole = |p: &&P| p.as_ref().len() >= 2;
    // The last path whose first point is at or before `x`, else the first with a point. A path
    // with no point counts as at or before, so the paths up to the halving's end hold the one.
    let starts = paths.partition_point(|p| p.as_ref().first().is_none_or(|&first| first <= x));
    let chosen = (paths[..starts].iter().rev())
        .find(|p| whole(p) && p.as_ref()[0] <= x)
        .or_else(|| paths.iter().find(whole));
    let Some(points) = chosen.map(AsRef::as_ref) else {
        return 0.0;
    };
    let points = &points[..points.len() / 2 * 2];
    // The last point at or before `x`, or the first.
    let (mut lo, mut hi) = (0, points.len() / 2);
    while lo < hi {
        let mid = (lo + hi) / 2;
        if points[2 * mid] <= x {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    let k = 2 * lo.saturating_sub(1);
    if k + 2 >= points.len() || points[k] > x {
        return points[k + 1];
    }
    points[k + 1] + (x - points[k]) * (points[k + 3] - points[k + 1]) / (points[k + 2] - points[k])
}

#[cfg(test)]
mod tests {
    use super::rgb;

    #[test]
    fn rgb_takes_hash_and_six_hex_digits_alone() {
        assert_eq!(rgb("#d9480f"), Some([0xd9, 0x48, 0x0f]));
        assert_eq!(rgb("#D9480F"), Some([0xd9, 0x48, 0x0f]));
        // A sign parses as a number's, but is no hex digit.
        for text in ["#+1+2+3", "d9480f", "#d9480", "#d9480f0", "#g9480f", ""] {
            assert_eq!(rgb(text), None, "{text:?}");
        }
    }
}
