//! A recording's annotations as markers: in order of their start tick, then of their arrival,
//! with a cursor that names one in that order.

use std::ops::Range;

use escapement_tickwave_format::annotations::Annotation;

use crate::recording::Recording;

/// Where a marker sits in the order markers are listed: its start tick, then its arrival, the
/// order the writer gave it or the sidecar lists it in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MarkerCursor {
    pub tick: i64,
    pub arrival: usize,
}

/// An annotation and its cursor. A host reads the annotation's `kind`, `label` and `values`.
#[derive(Clone, Debug, PartialEq)]
pub struct Marker {
    pub cursor: MarkerCursor,
    pub annotation: Annotation,
}

/// Markers in order from a cursor, at most a page of them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Page {
    pub entries: Vec<Marker>,
    /// The cursor to ask from for the next page; None when this page has the last.
    pub next: Option<MarkerCursor>,
    /// How many markers the interval holds in all.
    pub total: usize,
}

/// A writer's annotation record, `annotation` one line of generator `source`'s file with its
/// times in ticks, resolved as the sidecar would resolve it: id `<source>/<id>`. A record with no
/// id is named by its arrival.
#[cfg(feature = "live")]
pub(crate) fn generated(
    source: &str,
    annotation: &serde_json::Map<String, serde_json::Value>,
    arrival: usize,
) -> Annotation {
    use serde_json::Value;
    let text = |key: &str| {
        annotation
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let id = text("id").unwrap_or_else(|| arrival.to_string());
    Annotation {
        id: format!("{source}/{id}"),
        source: Some(source.to_owned()),
        target: None,
        start: annotation.get("start").and_then(Value::as_i64).unwrap_or(0),
        end: annotation.get("end").and_then(Value::as_i64),
        channels: annotation
            .get("channels")
            .and_then(Value::as_array)
            .map(|cs| {
                cs.iter()
                    .filter_map(|c| c.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
        label: text("label"),
        kind: text("kind"),
        text: text("text"),
        paint: text("paint"),
        measure: annotation
            .get("measure")
            .and_then(|m| serde_json::from_value(m.clone()).ok()),
        values: annotation
            .get("values")
            .and_then(Value::as_object)
            .map(|v| {
                v.iter()
                    .filter_map(|(k, x)| Some((k.clone(), x.as_str()?.to_owned())))
                    .collect::<std::collections::BTreeMap<_, _>>()
            }),
    }
}

impl Recording {
    /// The markers that start in `interval`, in order, after `after` where it is given, at most
    /// `limit` of them; none where they don't read (a query asking for markers says why).
    pub fn markers(&self, interval: Range<i64>, after: Option<MarkerCursor>, limit: usize) -> Page {
        let m = self.read_markers().map_or(&[][..], |m| &m.list);
        let lo = m.partition_point(|x| x.cursor.tick < interval.start);
        let hi = m.partition_point(|x| x.cursor.tick < interval.end).max(lo);
        let from = match after {
            Some(c) => m[lo..hi].partition_point(|x| x.cursor <= c) + lo,
            None => lo,
        };
        let to = hi.min(from.saturating_add(limit));
        Page {
            entries: m[from..to].to_vec(),
            next: (to < hi && to > from).then(|| m[to - 1].cursor),
            total: hi - lo,
        }
    }

    /// The marker after `cursor` in order, wherever it starts.
    pub fn marker_after(&self, cursor: MarkerCursor) -> Option<Marker> {
        let m = self.read_markers().map_or(&[][..], |m| &m.list);
        let i = m.partition_point(|x| x.cursor <= cursor);
        m.get(i).cloned()
    }
}
