//! The shared tables in testdata/view/: the web viewer's TypeScript tests read the same rows, so
//! both implementations are held to one set of cases. testdata/view/README.md gives a row's
//! layout and how a result is matched.

use std::path::PathBuf;

use escapement_tickwave_format::annotations::roles::dash_fits;
use escapement_tickwave_format::mapping::Mapping;
use escapement_tickwave_format::model::{Break, Ratio};
use escapement_tickwave_view::columns::{
    self, AnalogOverview, DigitalOverview, Edges, Grid, SampleRuns, Samples, State,
};
use escapement_tickwave_view::probe::{self, AnalogAt, EdgeAt, Known, Pulse};
use escapement_tickwave_view::readouts::{self, Measure, PeriodStats};
use escapement_tickwave_view::roles::{self, Dash, LookColour};
use escapement_tickwave_view::runs::Runs;
use escapement_tickwave_view::scale::{self, ScaleLimits, ValueRange};
use escapement_tickwave_view::{axis, shade, time};
use serde_json::{Map, Value, json};

fn table(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/view")
        .join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path:?}: {e}"))
}

/// A number as the tables write it: a JSON number, or "Infinity", "-Infinity" or "NaN".
fn f(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap(),
        Value::String(s) if s == "Infinity" => f64::INFINITY,
        Value::String(s) if s == "-Infinity" => f64::NEG_INFINITY,
        Value::String(s) if s == "NaN" => f64::NAN,
        other => panic!("not a number: {other}"),
    }
}

fn num(x: f64) -> Value {
    match serde_json::Number::from_f64(x) {
        Some(n) => Value::Number(n),
        None if x.is_nan() => json!("NaN"),
        None if x > 0.0 => json!("Infinity"),
        None => json!("-Infinity"),
    }
}

fn int(v: &Value) -> i64 {
    v.as_i64().unwrap_or_else(|| panic!("not an integer: {v}"))
}

fn uint(v: &Value) -> u64 {
    v.as_u64().unwrap_or_else(|| panic!("not a count: {v}"))
}

/// A count that the tables may give as "Infinity", for a tile whose channel's count doesn't
/// matter.
fn count(v: &Value) -> u64 {
    if f(v) == f64::INFINITY {
        u64::MAX
    } else {
        uint(v)
    }
}

fn ratio(v: &Value) -> Ratio {
    Ratio::new(uint(&v[0]), uint(&v[1])).unwrap()
}

fn opt<T>(v: &Value, read: impl Fn(&Value) -> T) -> Option<T> {
    (!v.is_null()).then(|| read(v))
}

/// The ticks one tick of a reading's source spans, `[num, den]`; 1 where a row gives none.
fn step(args: &Value) -> Ratio {
    opt(&args["step"], ratio).unwrap_or(Ratio::ONE)
}

fn ticks(v: &Value) -> Vec<i64> {
    v.as_array().unwrap().iter().map(int).collect()
}

fn floats(v: &Value) -> Vec<f64> {
    v.as_array().unwrap().iter().map(f).collect()
}

fn floats_out(xs: &[f64]) -> Value {
    Value::Array(xs.iter().map(|&x| num(x)).collect())
}

fn bit(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        other => int(other) != 0,
    }
}

/// `actual` holds what `expect` states, numbers equal or within `close`. An object expected of
/// an object names the keys checked; one expected of an array names its indices.
fn check(actual: &Value, expect: &Value, close: Option<f64>, at: &str, out: &mut Vec<String>) {
    let numeric = |v: &Value| {
        v.is_number()
            || matches!(v, Value::String(s) if ["Infinity", "-Infinity", "NaN"].contains(&s.as_str()))
    };
    match (actual, expect) {
        (a, e) if numeric(e) && numeric(a) => {
            let (a, e) = (f(a), f(e));
            let same = match close {
                Some(tolerance) => (a - e).abs() < tolerance,
                // As the web's deepStrictEqual: NaN is NaN, and −0 is not +0.
                None => a.to_bits() == e.to_bits() || (a.is_nan() && e.is_nan()),
            };
            if !same {
                out.push(format!("{at}: {a:?}, expected {e:?}"));
            }
        }
        (Value::Array(a), Value::Array(e)) => {
            if a.len() != e.len() {
                out.push(format!(
                    "{at}: {} items, expected {}: {actual}",
                    a.len(),
                    e.len()
                ));
                return;
            }
            for (i, (a, e)) in a.iter().zip(e).enumerate() {
                check(a, e, close, &format!("{at}[{i}]"), out);
            }
        }
        (Value::Array(a), Value::Object(e)) => {
            for (k, e) in e {
                match k.parse::<usize>().ok().and_then(|i| a.get(i)) {
                    Some(a) => check(a, e, close, &format!("{at}[{k}]"), out),
                    None => out.push(format!("{at}: no item {k}")),
                }
            }
        }
        (Value::Object(a), Value::Object(e)) => {
            for (k, e) in e {
                match a.get(k) {
                    Some(a) => check(a, e, close, &format!("{at}.{k}"), out),
                    None => out.push(format!("{at}: no {k} in {actual}")),
                }
            }
        }
        (a, e) if a == e => {}
        (a, e) => out.push(format!("{at}: {a}, expected {e}")),
    }
}

/// Runs every row of `name`'s table through `call`, which gets a row's function, its arguments
/// and, for a row's `then`, the result before it.
fn run(name: &str, call: impl Fn(&str, &Value, Option<&Value>) -> Value) {
    let t = table(name);
    let mut failures = Vec::new();
    let mut rows = 0;
    for group in t["tests"].as_array().unwrap() {
        let test = group["name"].as_str().unwrap();
        for (i, row) in group["rows"].as_array().unwrap().iter().enumerate() {
            rows += 1;
            let at = format!("{test} [{i}] {}", row["fn"].as_str().unwrap());
            let close = row["close"].as_f64();
            let result = call(row["fn"].as_str().unwrap(), &row["args"], None);
            let viewed = match row["view"].as_str() {
                None => result.clone(),
                Some("span_and_place") => {
                    let (lo, hi) = (f(&result["lo"]), f(&result["hi"]));
                    json!({"span": num(hi - lo), "place": num((f(&row["args"]["at"]) - lo) / (hi - lo))})
                }
                Some(other) => panic!("{at}: no view {other}"),
            };
            check(&viewed, &row["expect"], close, &at, &mut failures);
            for then in row["then"].as_array().into_iter().flatten() {
                let f = then["fn"].as_str().unwrap();
                let got = call(f, &then["args"], Some(&result));
                check(
                    &got,
                    &then["expect"],
                    then["close"].as_f64(),
                    &format!("{at} then {f}"),
                    &mut failures,
                );
            }
        }
    }
    assert!(rows > 0, "{name}: no rows");
    assert!(failures.is_empty(), "{name}:\n{}", failures.join("\n"));
}

fn stats_in(v: &Value) -> PeriodStats {
    PeriodStats {
        count: uint(&v["count"]),
        median: int(&v["median"]),
        min: int(&v["min"]),
        max: int(&v["max"]),
        used: uint(&v["used"]),
        runs: uint(&v["runs"]),
        span: int(&v["span"]),
    }
}

fn stats_out(s: &PeriodStats) -> Value {
    json!({"count": s.count, "median": s.median, "min": s.min, "max": s.max, "used": s.used, "runs": s.runs, "span": s.span})
}

/// The stats a `then` row reads: the row before's result, or its own `stats`.
fn stats(args: &Value, before: Option<&Value>) -> PeriodStats {
    stats_in(before.unwrap_or(&args["stats"]))
}

#[test]
fn readouts() {
    run("readouts", |name, a, before| match name {
        "tick_place" => json!(readouts::tick_place(ratio(&a["timebase"]), step(a))),
        "format_interval" => {
            json!(readouts::format_interval(
                int(&a["ticks"]),
                ratio(&a["timebase"]),
                step(a)
            ))
        }
        "format_brief" => {
            json!(readouts::format_brief(
                int(&a["ticks"]),
                ratio(&a["timebase"]),
                step(a)
            ))
        }
        "format_rate" => json!(readouts::format_rate(
            uint(&a["periods"]),
            int(&a["span"]),
            ratio(&a["timebase"]),
            opt(&a["runs"], uint).unwrap_or(1),
            step(a)
        )),
        "period_stats" => readouts::period_stats(bit(&a["level"]), &ticks(&a["ticks"]))
            .map_or(Value::Null, |s| stats_out(&s)),
        "format_clock_rate" => json!(readouts::format_clock_rate(
            &stats(a, before),
            ratio(&a["timebase"]),
            step(a)
        )),
        "format_clock_period" => json!(readouts::format_clock_period(
            &stats(a, before),
            ratio(&a["timebase"]),
            step(a)
        )),
        "clock_note" => json!(readouts::clock_note(&stats(a, before))),
        "format_value" => json!(readouts::format_value(
            f(&a["value"]),
            opt(&a["resolution"], f),
            a["unit"].as_str().unwrap(),
            opt(&a["significant"], |v| uint(v) as u32)
        )),
        "count_edges" => json!(readouts::count_edges(
            &ticks(&a["ticks"]),
            int(&a["start"]),
            int(&a["end"])
        )),
        "format_count" => json!(readouts::format_count(int(&a["n"]))),
        "format_marks" => readouts::format_marks(
            f(&a["top"]),
            f(&a["bottom"]),
            int(&a["place"]) as i32,
            a["unit"].as_str().unwrap(),
        )
        .map_or(Value::Null, |(top, bottom)| json!([top, bottom])),
        "measure_reading" => {
            let what = match a["what"].as_str().unwrap() {
                "rate" => Measure::Rate,
                "period" => Measure::Period,
                "duration" => Measure::Duration,
                other => panic!("no measure {other}"),
            };
            readouts::measure_reading(
                what,
                bit(&a["level"]),
                &ticks(&a["ticks"]),
                ratio(&a["timebase"]),
                step(a),
            )
            .map_or(Value::Null, |r| json!({"value": r.value, "note": r.note}))
        }
        other => panic!("readouts has no {other}"),
    });
}

#[test]
fn time() {
    run("time", |name, a, _| match name {
        "to_ticks" => {
            let text = a["text"].as_str().unwrap();
            let spec = time::parse_time(text).unwrap_or_else(|| panic!("{text} parses"));
            json!(time::to_ticks(&spec, ratio(&a["timebase"])))
        }
        "parse_time" => match time::parse_time(a["text"].as_str().unwrap()) {
            None => Value::Null,
            Some(time::TimeSpec::Ticks(t)) => json!({"ticks": t}),
            Some(time::TimeSpec::Seconds(s)) => {
                json!({"seconds": {"mantissa": s.mantissa.to_string(), "exp": s.exp}})
            }
        },
        other => panic!("time has no {other}"),
    });
}

fn range_in(v: &Value) -> ValueRange {
    ValueRange {
        lo: f(&v["lo"]),
        hi: f(&v["hi"]),
    }
}

fn range_out(r: ValueRange) -> Value {
    json!({"lo": num(r.lo), "hi": num(r.hi)})
}

/// The limits `scale_limits` gives for a row's `whole` and `resolution`.
fn limits(v: &Value) -> ScaleLimits {
    scale::scale_limits(range_in(&v["whole"]), opt(&v["resolution"], f))
}

#[test]
fn scale() {
    run("scale", |name, a, _| match name {
        "scale_limits" => {
            let l = limits(a);
            json!({"whole": range_out(l.whole), "min_span": num(l.min_span), "max_span": num(l.max_span)})
        }
        "zoom_range" => range_out(scale::zoom_range(
            range_in(&a["range"]),
            f(&a["at"]),
            f(&a["factor"]),
            &limits(&a["limits"]),
        )),
        "move_range" => range_out(scale::move_range(
            range_in(&a["range"]),
            f(&a["by"]),
            &limits(&a["limits"]),
        )),
        "clamp_range" => range_out(scale::clamp_range(
            range_in(&a["range"]),
            &limits(&a["limits"]),
        )),
        "fit_range" => scale::fit_range(f(&a["min"]), f(&a["max"]), &limits(&a["limits"]))
            .map_or(Value::Null, range_out),
        "range_marks" => scale::range_marks(range_in(&a["range"])).map_or(
            Value::Null,
            |m| json!({"top": num(m.top), "bottom": num(m.bottom), "place": m.place}),
        ),
        other => panic!("scale has no {other}"),
    });
}

#[test]
fn axis() {
    run("axis", |name, a, _| match name {
        "time_axis" => {
            let per_char = f(&a["measure_per_char"]) as f32;
            let axis = axis::time_axis(
                f(&a["start"]),
                f(&a["end"]),
                f(&a["width"]),
                ratio(&a["timebase"]),
                |label: &str| label.chars().count() as f32 * per_char,
                opt(&a["gap"], f).unwrap_or(16.0),
            );
            let ticks: Vec<Value> = axis
                .ticks
                .iter()
                .map(|t| json!({"tick": num(t.tick), "x": num(t.x), "label": t.label, "width": num(f64::from(t.width))}))
                .collect();
            json!({"ticks": ticks, "unit": axis.unit.map(|u| json!({"exp": u.exp, "name": u.name}))})
        }
        other => panic!("axis has no {other}"),
    });
}

fn grid(v: &Value) -> Grid {
    Grid {
        x0: f(&v["x0"]),
        per: f(&v["per"]),
        count: uint(&v["count"]) as usize,
    }
}

fn state_name(s: State) -> &'static str {
    match s {
        State::None => "NONE",
        State::Low => "LOW",
        State::High => "HIGH",
        State::Busy => "BUSY",
        State::Rise => "RISE",
        State::Fall => "FALL",
    }
}

fn state_of(name: &str) -> State {
    match name {
        "NONE" => State::None,
        "LOW" => State::Low,
        "HIGH" => State::High,
        "BUSY" => State::Busy,
        "RISE" => State::Rise,
        "FALL" => State::Fall,
        other => panic!("no state {other}"),
    }
}

fn digital_out(c: &columns::DigitalColumns) -> Value {
    json!({
        "state": c.state.iter().map(|&s| state_name(s)).collect::<Vec<_>>(),
        "at": floats_out(&c.at),
        "edges": floats_out(&c.edges),
        "high": floats_out(&c.high),
        "known": floats_out(&c.known),
    })
}

fn analog_out(c: &columns::AnalogColumns) -> Value {
    json!({"min": floats_out(&c.min), "max": floats_out(&c.max)})
}

fn breaks(v: &Value) -> Vec<Break> {
    v.as_array()
        .into_iter()
        .flatten()
        .map(|b| Break {
            index: uint(&b["index"]),
            tick: int(&b["tick"]),
            lost: uint(&b["lost"]),
            phase: 0,
        })
        .collect()
}

/// Calls `then` with a row's samples and runs, as the borrowed types the functions take.
fn with_samples<R>(a: &Value, then: impl FnOnce(&Samples<f64>, &Runs) -> R) -> R {
    let values = floats(&a["samples"]["values"]);
    let (run_min, run_max) = match &a["samples"]["runs"] {
        Value::Null => (Vec::new(), Vec::new()),
        r => (floats(&r["min"]), floats(&r["max"])),
    };
    let samples = Samples {
        first: uint(&a["samples"]["first"]),
        values: &values,
        runs: (!a["samples"]["runs"].is_null()).then_some(SampleRuns {
            min: &run_min,
            max: &run_max,
        }),
    };
    let b = breaks(&a["runs"]["breaks"]);
    let runs = Runs {
        first: int(&a["runs"]["first"]),
        grid: escapement_tickwave_format::model::Grid::new(ratio(&a["runs"]["period"]), None)
            .unwrap(),
        count: count(&a["runs"]["count"]),
        breaks: &b,
        map: Mapping::IDENTITY,
    };
    then(&samples, &runs)
}

fn analog_overview<R>(v: &Value, then: impl FnOnce(&AnalogOverview<f64>) -> R) -> R {
    let (min, max) = (floats(&v["min"]), floats(&v["max"]));
    then(&AnalogOverview {
        bucket: int(&v["bucket"]),
        start: int(&v["start"]),
        min: &min,
        max: &max,
    })
}

#[test]
fn columns() {
    run("columns", |name, a, _| match name {
        "digital_from_edges" => {
            let t = ticks(&a["edges"]["ticks"]);
            let edges = Edges {
                level: bit(&a["edges"]["level"]),
                ticks: &t,
            };
            digital_out(&columns::digital_from_edges(
                &grid(&a["grid"]),
                &edges,
                f(&a["from"]),
                f(&a["to"]),
            ))
        }
        "digital_from_overview" => {
            let bytes = |v: &Value| {
                (v.as_array().unwrap().iter())
                    .map(|x| u8::try_from(uint(x)).unwrap())
                    .collect::<Vec<_>>()
            };
            let (density, duty) = (
                bytes(&a["overview"]["density"]),
                bytes(&a["overview"]["duty"]),
            );
            let ov = DigitalOverview {
                bucket: int(&a["overview"]["bucket"]),
                start: int(&a["overview"]["start"]),
                density: &density,
                duty: &duty,
            };
            digital_out(&columns::digital_from_overview(
                &grid(&a["grid"]),
                &ov,
                f(&a["from"]),
                f(&a["to"]),
            ))
        }
        "analog_from_samples" => with_samples(a, |s, runs| {
            analog_out(&columns::analog_from_samples(
                &grid(&a["grid"]),
                s,
                runs,
                f(&a["from"]),
                f(&a["to"]),
            ))
        }),
        "analog_from_overview" => analog_overview(&a["overview"], |ov| {
            analog_out(&columns::analog_from_overview(
                &grid(&a["grid"]),
                ov,
                f(&a["from"]),
                f(&a["to"]),
            ))
        }),
        "line_samples" => with_samples(a, |s, runs| {
            let (k0, k1) = columns::line_samples(
                &grid(&a["grid"]),
                s,
                runs,
                f(&a["from"]),
                f(&a["to"]),
                f(&a["pad"]),
            );
            json!([k0, k1])
        }),
        other => panic!("columns has no {other}"),
    });
}

#[test]
fn shade() {
    let byte = |v: &Value| u8::try_from(uint(v)).unwrap();
    run("shade", |name, a, _| match name {
        "counts" => match shade::counts(byte(&a["density"])) {
            Some((lo, hi)) => json!([lo, hi]),
            None => Value::Null,
        },
        "count" => num(shade::count(byte(&a["density"]))),
        "rate" => num(shade::rate(
            f(&a["edges"]),
            f(&a["ticks"]),
            ratio(&a["timebase"]),
        )),
        "band" => num(shade::band(f(&a["rate"]))),
        "stroke" => num(shade::stroke(
            f(&a["band"]),
            f(&a["spacing"]),
            f(&a["width"]),
        )),
        "rails" => {
            let (h, l) = shade::rails(f(&a["high"]), bit(&a["edges"]), f(&a["band"]));
            json!([num(h), num(l)])
        }
        "looks" => {
            let c = &a["columns"];
            let cols = columns::DigitalColumns {
                state: (c["state"].as_array().unwrap().iter())
                    .map(|s| state_of(s.as_str().unwrap()))
                    .collect(),
                at: floats(&c["at"]),
                edges: floats(&c["edges"]),
                high: floats(&c["high"]),
                known: floats(&c["known"]),
            };
            let shown = uint(&a["shown"][0]) as usize..uint(&a["shown"][1]) as usize;
            let looks = shade::looks(
                &cols,
                shown,
                f(&a["width"]),
                f(&a["per"]),
                ratio(&a["timebase"]),
            );
            Value::Array(
                looks
                    .into_iter()
                    .map(|l| match l {
                        shade::Look::Flat => json!("flat"),
                        shade::Look::Stroke(a) => json!(["stroke", num(a)]),
                        shade::Look::Pulse { alpha, at } => json!(["pulse", num(alpha), num(at)]),
                        shade::Look::Shade { band, high, low } => {
                            json!(["shade", num(band), num(high), num(low)])
                        }
                        shade::Look::Held(a) => json!(["held", num(a)]),
                    })
                    .collect(),
            )
        }
        "duty_share" => match shade::duty_share(byte(&a["duty"])) {
            Some((mid, lo, hi)) => json!([num(mid), num(lo), num(hi)]),
            None => Value::Null,
        },
        other => panic!("no function {other}"),
    });
}

fn edge_in(v: &Value) -> EdgeAt {
    match v {
        Value::Null => EdgeAt::Open,
        Value::String(s) if s == "unread" => EdgeAt::Unread,
        other => EdgeAt::Tick(int(other)),
    }
}

fn edge_out(e: EdgeAt) -> Value {
    match e {
        EdgeAt::Tick(t) => json!(t),
        EdgeAt::Open => Value::Null,
        EdgeAt::Unread => json!("unread"),
    }
}

fn known(v: &Value) -> Known {
    Known {
        start: int(&v["start"]),
        end: int(&v["end"]),
    }
}

fn analog_at(r: AnalogAt) -> Value {
    match r {
        AnalogAt::Value(v) => json!({"kind": "value", "value": num(v)}),
        AnalogAt::Range { min, max } => json!({"kind": "range", "min": num(min), "max": num(max)}),
        AnalogAt::None => json!({"kind": "none"}),
    }
}

#[test]
fn probe() {
    run("probe", |name, a, _| match name {
        "pulse_from_edges" => {
            let t = ticks(&a["edges"]["ticks"]);
            let edges = Edges {
                level: bit(&a["edges"]["level"]),
                ticks: &t,
            };
            let p = probe::pulse_from_edges(
                &edges,
                int(&a["from"]),
                int(&a["to"]),
                known(&a["known"]),
                int(&a["tick"]),
            );
            json!({"level": u8::from(p.level), "start": edge_out(p.start), "end": edge_out(p.end), "next": edge_out(p.next)})
        }
        "pulse_figures" => {
            let p = &a["pulse"];
            let pulse = Pulse {
                level: bit(&p["level"]),
                start: edge_in(&p["start"]),
                end: edge_in(&p["end"]),
                next: edge_in(&p["next"]),
            };
            let fig =
                probe::pulse_figures(&pulse, ratio(&a["timebase"]), known(&a["known"]), step(a));
            let mut out = Map::new();
            out.insert("level".into(), json!(fig.level));
            out.insert("width".into(), json!(fig.width));
            out.insert("inverse_width".into(), json!(fig.inverse_width));
            out.insert("period".into(), json!(fig.period));
            out.insert("frequency".into(), json!(fig.frequency));
            out.insert("duty".into(), json!(fig.duty));
            Value::Object(out)
        }
        "format_duty" => json!(probe::format_duty(
            int(&a["high"]),
            int(&a["period"]),
            step(a)
        )),
        "analog_from_samples" => with_samples(a, |s, runs| {
            analog_at(probe::analog_from_samples(
                runs,
                int(&a["end"]),
                s,
                f(&a["t0"]),
                f(&a["t1"]),
                f(&a["tick"]),
            ))
        }),
        "analog_from_overview" => analog_overview(&a["overview"], |ov| {
            analog_at(probe::analog_from_overview(ov, f(&a["t0"]), f(&a["t1"])))
        }),
        other => panic!("probe has no {other}"),
    });
}

fn role_in(v: &Value) -> escapement_tickwave_format::annotations::Role {
    escapement_tickwave_format::annotations::Role {
        name: v["name"].as_str().unwrap().into(),
        label: v["label"].as_str().unwrap().into(),
        colour: opt(&v["colour"], |c| {
            escapement_tickwave_format::annotations::Colour {
                day: c["day"].as_str().unwrap().into(),
                night: c["night"].as_str().unwrap().into(),
            }
        }),
        weight: f(&v["weight"]),
        dash: opt(&v["dash"], floats),
    }
}

#[test]
fn roles() {
    run("roles", |name, a, _| match name {
        "look" => {
            let roles: Vec<_> = a["roles"].as_array().unwrap().iter().map(role_in).collect();
            let h = &a["host"];
            let host = roles::Host {
                colour: h["colour"].as_str().map(str::to_owned),
                weight: h.get("weight").map(f),
                dash: h.get("dash").map(|d| match d.as_str() {
                    Some("none") => Dash::Solid,
                    _ => Dash::Pattern(floats(d)),
                }),
            };
            match roles::look(&roles, a["name"].as_str().unwrap(), host, bit(&a["night"])) {
                None => Value::Null,
                Some(l) => json!({
                    "label": l.label,
                    "colour": match l.colour {
                        LookColour::Host(c) => c,
                        LookColour::Stored([r, g, b]) => format!("#{r:02x}{g:02x}{b:02x}"),
                        LookColour::Trace => "trace".into(),
                        LookColour::Ink => "ink".into(),
                    },
                    "weight": num(l.weight),
                    "dash": match &l.dash {
                        Dash::Solid => Value::Null,
                        Dash::Pattern(p) => floats_out(p),
                    },
                }),
            }
        }
        "stretches" => {
            let names: Vec<&str> = a["roles"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n.as_str().unwrap())
                .collect();
            let roles: Vec<_> = names
                .iter()
                .map(|&n| {
                    role_in(
                        &json!({"name": n, "label": n, "colour": null, "weight": 0, "dash": null}),
                    )
                })
                .collect();
            let spans: Vec<(i64, i64, &str)> = (a["spans"].as_array().unwrap().iter())
                .map(|s| (int(&s[0]), int(&s[1]), s[2].as_str().unwrap()))
                .collect();
            Value::Array(
                roles::stretches(&spans, &roles, int(&a["px0"]), int(&a["px1"]))
                    .into_iter()
                    .map(|s| json!([s.px0, s.px1, s.role.map(|r| names[r])]))
                    .collect(),
            )
        }
        "length_at" => {
            let paths: Vec<Vec<f64>> = a["paths"].as_array().unwrap().iter().map(floats).collect();
            num(roles::length_at(&paths, f(&a["x"])))
        }
        "dash_fits" => json!(dash_fits(&floats(&a["numbers"]))),
        other => panic!("no function {other}"),
    });
}
