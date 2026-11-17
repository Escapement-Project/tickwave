//! Paint roles and the stored view (ANNOTATIONS.md, "Roles", "The stored view") in the graph:
//! a painted stretch of a lane or the trace in its role's look, one dash phase along a stretch,
//! the key of every declared role, the trace's stored colour, and the host's own looks over the
//! recording's. The fixture's DIO lanes stay low, so each lane is one level line.

use std::collections::BTreeMap;
use std::time::Duration;

use egui::epaint::ColorMode;
use egui::{Color32, Pos2, Rect, Shape};
use escapement_tickwave_format::decode::Footer;
use escapement_tickwave_format::writer::{Bundle, Finish};
use escapement_tickwave_view::roles::{Dash, Host as RoleHost};

use super::*;
use crate::Window;

const GREEN: Color32 = Color32::from_rgb(0, 200, 0);
const BLUE: Color32 = Color32::from_rgb(0, 0, 230);
const MAGENTA: Color32 = Color32::from_rgb(230, 0, 230);

/// Probe, green and a point heavier; neither, blue by day and magenta by night, dashed 2 and 1;
/// target, with nothing of its own.
const ROLES: &str = "[[role]]\nname = \"probe\"\ncolour = \"#00c800\"\nweight = 1\n\n\
                     [[role]]\nname = \"neither\"\ncolour = [\"#0000e6\", \"#e600e6\"]\ndash = [2, 1]\n\n\
                     [[role]]\nname = \"target\"\nlabel = \"target drives\"\n\n";

/// `capture` written finished with a hand-written sidecar bundled whose body, after its head, is
/// `body`, unchecked, as a writer bundles the files it is handed.
fn painted(capture: &Capture, body: &str) -> Vec<u8> {
    let write = |files: Option<BTreeMap<String, String>>| {
        let (mut w, mut out) = capture.writer();
        capture.items(&mut w, 0..capture.count);
        out.extend(w.take_output());
        let length = capture.length();
        let finish = Finish {
            bundle: files.map(Bundle::Files),
            ..Finish::default()
        };
        out.extend(w.finish(length, &[length; 5], finish).unwrap().tail);
        out
    };
    // The content id is the data's alone: a first pass gives the one the sidecar names.
    let id = Footer::parse_file(&write(None)).unwrap().header.content_id;
    let toml = format!("format = \"tickwave-annotations/1\"\nrecording_id = \"{id}\"\n\n{body}");
    write(Some(BTreeMap::from([(
        "capture.annotations.toml".to_owned(),
        toml,
    )])))
}

/// A span painting `channel` from `from` to `to` seconds in `role`.
fn paint(id: &str, from: f64, to: f64, channel: &str, role: &str) -> String {
    let tick = |s: f64| (s * SECOND as f64).round() as i64;
    format!(
        "[[annotation]]\nid = \"{id}\"\nstart = {}\nend = {}\nchannels = [\"{channel}\"]\npaint = \"{role}\"\n\n",
        tick(from),
        tick(to)
    )
}

/// A harness over the first 10 s of verge's fixture with `body` bundled, its answer landed and
/// drawn, with `palette` changed by `set`.
fn shown(body: &str, set: impl FnOnce(&mut Palette)) -> Harness<'static, Host> {
    shown_in(&fixture(), body, set)
}

/// `shown`, of `capture`.
fn shown_in(
    capture: &Capture,
    body: &str,
    set: impl FnOnce(&mut Palette),
) -> Harness<'static, Host> {
    let state = ViewState {
        window: Window::At(0..10 * SECOND),
        ..ViewState::default()
    };
    let (mut h, gate) = showing(painted(capture, body), state, true);
    gate.open();
    set(&mut h.state_mut().palette);
    wait(&mut h);
    // The answer with its paints, drawn.
    for _ in 0..4 {
        h.step();
        std::thread::sleep(Duration::from_millis(2));
    }
    h
}

/// What shows of `colour` in the last frame's meshes: in each triangle, the bounds of its
/// corners of that colour cut to its clip, where some of them is left and its middle lies in
/// `within`; with the clip. Feathering's corners fade out, so this is a line's core.
fn seen(h: &Harness<'_, Host>, colour: Color32, within: Rect) -> Vec<(Rect, Rect)> {
    let mut out = Vec::new();
    for s in shapes(h) {
        let Shape::Mesh(mesh) = &s.shape else {
            continue;
        };
        for t in mesh.indices.as_chunks::<3>().0 {
            let v = t.map(|i| mesh.vertices[i as usize]);
            let corners: Vec<Pos2> = (v.iter().filter(|v| v.color == colour))
                .map(|v| v.pos)
                .collect();
            if corners.is_empty() {
                continue;
            }
            let r = Rect::from_points(&corners).intersect(s.clip_rect);
            if r.min.x < r.max.x && r.min.y <= r.max.y && within.contains(r.center()) {
                out.push((s.clip_rect, r));
            }
        }
    }
    out
}

/// The rects of `seen`.
fn shows(h: &Harness<'_, Host>, colour: Color32, within: Rect) -> Vec<Rect> {
    seen(h, colour, within)
        .into_iter()
        .map(|(_, r)| r)
        .collect()
}

/// The runs `rects` cover along x, each its left and right, where a gap wider than `apart`
/// parts two.
fn runs(rects: &[Rect], apart: f32) -> Vec<(f32, f32)> {
    let mut xs: Vec<(f32, f32)> = rects.iter().map(|r| (r.left(), r.right())).collect();
    xs.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out: Vec<(f32, f32)> = Vec::new();
    for (a, b) in xs {
        match out.last_mut() {
            Some(r) if a - r.1 <= apart => r.1 = r.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// How tall `rects` stand together.
fn height(rects: &[Rect]) -> f32 {
    let top = rects.iter().map(|r| r.top()).fold(f32::INFINITY, f32::min);
    rects
        .iter()
        .map(|r| r.bottom())
        .fold(f32::NEG_INFINITY, f32::max)
        - top
}

/// The paths stroked in `colour` whose middles lie in `within`, each its points and width.
fn strokes(h: &Harness<'_, Host>, colour: Color32, within: Rect) -> Vec<(Vec<Pos2>, f32)> {
    shapes(h)
        .iter()
        .filter_map(|s| match &s.shape {
            Shape::Path(p) if p.stroke.color == ColorMode::Solid(colour) => {
                let r = Rect::from_points(&p.points);
                within
                    .contains(r.center())
                    .then(|| (p.points.clone(), p.stroke.width))
            }
            _ => None,
        })
        .collect()
}

/// Where tick `t` sits across the plot as last drawn.
fn x_of(h: &Harness<'_, Host>, t: i64) -> f32 {
    let s = drawn(h);
    let (plot, w) = (s.layout.plot, s.window);
    plot.left() + ((t - w.start) as f64 / (w.end - w.start) as f64) as f32 * plot.width()
}

/// Within a device pixel of each other, at the tests' one pixel a point.
fn near(a: (f32, f32), b: (f32, f32)) -> bool {
    (a.0 - b.0).abs() <= 1.0 && (a.1 - b.1).abs() <= 1.0
}

#[test]
fn a_painted_stretch_of_a_lane_draws_in_its_roles_colour_and_width_and_the_rest_in_the_traces() {
    let body = format!("{ROLES}{}", paint("p", 2.0, 4.0, "DIO3", "probe"));
    let h = shown(&body, |_| {});
    let lane = drawn(&h).layout.lanes[3];
    let green = shows(&h, GREEN, lane);
    let along = runs(&green, 0.0);
    let span = (x_of(&h, 2 * SECOND), x_of(&h, 4 * SECOND));
    assert_eq!(along.len(), 1, "{along:?}");
    assert!(near(along[0], span), "{along:?} against {span:?}");
    // The trace's colour on either side of it, and none inside.
    let plain = shows(&h, palette().trace.unwrap(), lane);
    let around = runs(&plain, 0.0);
    assert_eq!(around.len(), 2, "{around:?}");
    assert!((around[0].1 - span.0).abs() <= 1.0 && (around[1].0 - span.1).abs() <= 1.0);
    // A point heavier.
    let (painted, plain) = (height(&green), height(&plain));
    assert!(
        (painted - plain - 1.0).abs() < 0.01,
        "{painted} against {plain}"
    );
    // The other lanes are the trace's alone.
    assert!(shows(&h, GREEN, drawn(&h).layout.lanes[2]).is_empty());
}

/// Asserts `rects` are dashes `dash` long and `gap` apart from `start` to `end`, the first whole.
fn dashed(rects: &[Rect], (start, end): (f32, f32), dash: f32, gap: f32) {
    let dashes = runs(rects, 0.0);
    let period = dash + gap;
    let count = ((end - start) / period).ceil() as usize;
    assert_eq!(dashes.len(), count, "{dashes:?} over {start}..{end}");
    for (k, d) in dashes.iter().enumerate() {
        let a = start + k as f32 * period;
        let b = (a + dash).min(end);
        assert!(
            (d.0 - a).abs() < 0.01 && (d.1 - b).abs() < 0.01,
            "{dashes:?} from {start}"
        );
    }
}

#[test]
fn a_dashed_role_draws_its_dash_in_its_line_widths_from_where_each_stretch_starts() {
    // Two stretches of neither, each starting part way through a period of the pattern as
    // counted from the line's start, as declared and a point heavier.
    let body = format!(
        "{ROLES}{}{}",
        paint("n1", 6.02, 7.5, "DIO3", "neither"),
        paint("n2", 8.31, 9.7, "DIO3", "neither")
    );
    for weight in [None, Some(1.0)] {
        let h = shown(&body, |p| {
            let neither = RoleHost {
                weight,
                ..RoleHost::default()
            };
            p.roles.insert("neither".into(), neither);
        });
        let lane = drawn(&h).layout.lanes[3];
        let blue = seen(&h, BLUE, lane);
        let mut clips: Vec<Rect> = blue.iter().map(|(c, _)| *c).collect();
        clips.dedup();
        assert_eq!(clips.len(), 2, "{clips:?}");
        // Dash 2 and gap 1 in line widths.
        let width = 1.0 + weight.unwrap_or(0.0) as f32;
        let (dash, gap) = (2.0 * width, width);
        for clip in clips {
            let into = (clip.left() - lane.left()).rem_euclid(dash + gap);
            assert!(
                into > 0.2 && into < dash + gap - 0.2,
                "the stretch starts {into} into a period"
            );
            let rects: Vec<Rect> = (blue.iter())
                .filter(|(c, _)| *c == clip)
                .map(|(_, r)| *r)
                .collect();
            dashed(&rects, (clip.left(), clip.right()), dash, gap);
        }
    }
}

#[test]
fn a_dash_starts_again_where_the_data_does_after_a_gap() {
    let mut capture = fixture();
    capture.dio_gaps = std::iter::once(7 * SECOND..7 * SECOND + 400 * MS).collect();
    let body = format!("{ROLES}{}", paint("n", 6.02, 9.7, "DIO3", "neither"));
    let h = shown_in(&capture, &body, |_| {});
    let lane = drawn(&h).layout.lanes[3];
    let blue = seen(&h, BLUE, lane);
    let clip = blue[0].0;
    let (gap0, gap1) = (x_of(&h, 7 * SECOND), x_of(&h, 7 * SECOND + 400 * MS));
    let before: Vec<Rect> = blue
        .iter()
        .map(|(_, r)| *r)
        .filter(|r| r.right() <= gap0 + 1.0)
        .collect();
    let after: Vec<Rect> = blue
        .iter()
        .map(|(_, r)| *r)
        .filter(|r| r.left() >= gap1 - 1.0)
        .collect();
    assert_eq!(
        before.len() + after.len(),
        blue.len(),
        "blue across the gap"
    );
    let ends = |rs: &[Rect]| {
        runs(rs, 0.0)
            .iter()
            .fold((f32::INFINITY, 0.0f32), |a, r| (a.0.min(r.0), a.1.max(r.1)))
    };
    dashed(&before, (clip.left(), ends(&before).1), 2.0, 1.0);
    let restart = ends(&after).0;
    assert!(
        (restart - gap1).abs() <= 1.0,
        "the line again at {restart}, the gap ending at {gap1}"
    );
    dashed(&after, (restart, clip.right()), 2.0, 1.0);
}

#[test]
fn a_painted_stretch_of_the_trace_draws_its_band_and_lines_in_the_role() {
    let body = format!("{ROLES}{}", paint("t", 3.0, 3.5, "I", "probe"));
    let h = shown(&body, |_| {});
    let plot = drawn(&h).layout.plot;
    let p = palette();
    let span = (x_of(&h, 3 * SECOND), x_of(&h, 3 * SECOND + SECOND / 2));
    let green = runs(&shows(&h, GREEN, plot), 0.0);
    assert_eq!(green.len(), 1, "{green:?}");
    assert!(near(green[0], span), "{green:?} against {span:?}");
    // Its band's fill is the role's colour at the fill's share.
    let fill = runs(&shows(&h, GREEN.gamma_multiply(p.fill), plot), 0.0);
    assert!(
        !fill.is_empty()
            && fill
                .iter()
                .all(|r| r.0 >= span.0 - 1.0 && r.1 <= span.1 + 1.0)
    );
    // And none of the trace's own inside it.
    let plain = runs(&shows(&h, p.trace.unwrap(), plot), 0.0);
    assert!(
        plain
            .iter()
            .all(|r| r.1 <= span.0 + 1.0 || r.0 >= span.1 - 1.0),
        "{plain:?}"
    );
}

#[test]
fn night_takes_a_declared_colour_s_night_face_and_the_traces_stored_one() {
    let body = format!(
        "[view]\ncolour = [\"#d9480f\", \"#ff8a50\"]\n\n{ROLES}{}",
        paint("n", 6.0, 8.0, "DIO3", "neither")
    );
    for dark in [false, true] {
        let h = shown(&body, |p| {
            p.trace = None;
            p.dark = dark;
        });
        let l = drawn(&h).layout;
        let (role, other, stored) = match dark {
            false => (BLUE, MAGENTA, Color32::from_rgb(0xd9, 0x48, 0x0f)),
            true => (MAGENTA, BLUE, Color32::from_rgb(0xff, 0x8a, 0x50)),
        };
        assert!(!shows(&h, role, l.lanes[3]).is_empty(), "dark {dark}");
        assert!(shows(&h, other, l.lanes[3]).is_empty(), "dark {dark}");
        // The trace and the lanes' unpainted levels in the stored colour on this face.
        assert!(!shows(&h, stored, l.lanes[3]).is_empty(), "dark {dark}");
        assert!(!shows(&h, stored, l.plot).is_empty(), "dark {dark}");
    }
}

#[test]
fn the_hosts_colour_is_the_traces_over_the_stored_one_and_ink_where_neither_is() {
    let stored = "[view]\ncolour = \"#d9480f\"\n\n";
    let red = Color32::from_rgb(0xd9, 0x48, 0x0f);
    let h = shown(stored, |p| p.trace = Some(GREEN));
    let plot = drawn(&h).layout.plot;
    assert!(!shows(&h, GREEN, plot).is_empty());
    assert!(shows(&h, red, plot).is_empty());
    let h = shown("", |p| p.trace = None);
    let plot = drawn(&h).layout.plot;
    assert!(!shows(&h, palette().ink, plot).is_empty());
}

#[test]
fn a_hosts_role_look_wins_field_by_field_and_one_past_the_bounds_is_left_to_the_declaration() {
    let body = format!(
        "{ROLES}{}{}",
        paint("p", 2.0, 4.0, "DIO3", "probe"),
        paint("n", 6.0, 8.0, "DIO3", "neither")
    );
    let h = shown(&body, |p| {
        p.roles.insert(
            "probe".into(),
            RoleHost {
                colour: Some(MAGENTA),
                weight: Some(3.0),
                dash: None,
            },
        );
        // A weight past 4 is left to the declaration; Solid makes neither solid.
        let neither = RoleHost {
            colour: None,
            weight: Some(9.0),
            dash: Some(Dash::Solid),
        };
        p.roles.insert("neither".into(), neither);
    });
    let lane = drawn(&h).layout.lanes[3];
    let probe = shows(&h, MAGENTA, lane);
    let plain = shows(&h, palette().trace.unwrap(), lane);
    assert!(
        shows(&h, GREEN, lane).is_empty(),
        "the declared colour under the host's"
    );
    let (heavier, plain_height) = (height(&probe), height(&plain));
    assert!(
        (heavier - plain_height - 3.0).abs() < 0.01,
        "{heavier} against {plain_height}"
    );
    // Its declared weight, none, and solid: one run across its stretch.
    let neither = shows(&h, BLUE, lane);
    assert!((height(&neither) - plain_height).abs() < 0.01);
    let along = runs(&neither, 0.0);
    let span = (x_of(&h, 6 * SECOND), x_of(&h, 8 * SECOND));
    assert_eq!(along.len(), 1, "{along:?}");
    assert!(near(along[0], span), "{along:?} against {span:?}");
}

#[test]
fn a_paint_naming_no_declared_role_draws_as_none_and_shows_what_lies_under_it() {
    let body = format!(
        "{ROLES}{}{}",
        paint("p", 1.0, 5.0, "DIO3", "probe"),
        paint("s", 2.0, 4.0, "DIO3", "stray")
    );
    let h = shown(&body, |_| {});
    let lane = drawn(&h).layout.lanes[3];
    let green = runs(&shows(&h, GREEN, lane), 0.0);
    let span = (x_of(&h, SECOND), x_of(&h, 5 * SECOND));
    assert_eq!(
        green.len(),
        1,
        "probe broken by a paint naming no declared role: {green:?}"
    );
    assert!(near(green[0], span), "{green:?} against {span:?}");
    // The key names the declared roles alone.
    let key = drawn(&h).layout.key.expect("a key");
    let labels: Vec<String> = (texts(&h).into_iter())
        .filter(|(_, r)| key.contains(r.center()))
        .map(|(t, _)| t)
        .collect();
    assert_eq!(labels.len(), 3, "{labels:?}");
    assert!(!labels.iter().any(|l| l.contains("stray")), "{labels:?}");
}

#[test]
fn every_paint_over_the_window_draws_however_many_markers_it_holds() {
    // 520 painted spans in the first 5.2 s, then one at 8 s: the window's markers stop at 512,
    // before the last.
    let mut body = ROLES.to_owned();
    for k in 0..520 {
        let at = k as f64 * 0.01;
        body.push_str(&paint(&format!("e{k}"), at, at + 0.005, "DIO0", "probe"));
    }
    body.push_str(&paint("late", 8.0, 9.0, "DIO3", "neither"));
    let h = shown(&body, |_| {});
    let answer = viewer(&h).answer().expect("an answer");
    assert_eq!(answer.markers.window.entries.len(), 512);
    assert!(answer.markers.window.next.is_some());
    assert_eq!(answer.markers.paints.len(), 521);
    let late = runs(&shows(&h, BLUE, drawn(&h).layout.lanes[3]), 0.0);
    let span = (x_of(&h, 8 * SECOND), x_of(&h, 9 * SECOND));
    assert!(
        near((late[0].0, late.last().unwrap().1), span),
        "{late:?} against {span:?}"
    );
}

#[test]
fn the_key_lists_every_declared_role_in_order_right_aligned_and_keeps_its_room_across_a_pan() {
    let mut h = shown(ROLES, |_| {});
    let before = drawn(&h);
    let key = before.layout.key.expect("a key while roles are declared");
    let mut in_key: Vec<(String, Rect)> = (texts(&h).into_iter())
        .filter(|(_, r)| key.contains(r.center()))
        .collect();
    in_key.sort_by(|a, b| a.1.left().total_cmp(&b.1.left()));
    let labels: Vec<&str> = in_key.iter().map(|(t, _)| t.as_str()).collect();
    // In the order declared; a role without a label goes by its name.
    assert_eq!(labels, ["probe", "neither", "target drives"]);
    assert!(
        (in_key[2].1.right() - key.right()).abs() < 0.01,
        "right-aligned"
    );
    // Each swatch just left of its label and drawn as the role's lines are: green a point
    // heavier, blue dashed 2 and 1, and the third place's ink.
    let swatches = [(GREEN, 2.0), (BLUE, 1.0), (palette().ink, 1.0)];
    for (i, (colour, width)) in swatches.into_iter().enumerate() {
        let left = in_key[i].1.left();
        let pieces = strokes(&h, colour, key);
        assert!(!pieces.is_empty(), "no swatch for {}", labels[i]);
        // 18 points long, ending 5 before the label; a dash's ends where its pattern says.
        let ends: Vec<(f32, f32)> = pieces
            .iter()
            .map(|(q, _)| (q[0].x, q[q.len() - 1].x))
            .collect();
        let starts = if colour == BLUE {
            (0..6).map(|k| left - 23.0 + 3.0 * k as f32).collect()
        } else {
            vec![left - 23.0]
        };
        assert_eq!(ends.len(), starts.len(), "{ends:?}");
        for ((a, b), start) in ends.iter().zip(starts) {
            let long = if colour == BLUE { 2.0 } else { 18.0 };
            assert!(
                (a - start).abs() < 0.01 && (b - a - long).abs() < 0.01,
                "{ends:?}"
            );
        }
        assert!(pieces.iter().all(|(_, w)| *w == width), "{pieces:?}");
    }
    // A pan moves nothing of the layout: the plot keeps its height, the key its place.
    let plot = before.layout.plot;
    drag(&mut h, plot.center(), egui::vec2(-200.0, 0.0), 4);
    for _ in 0..4 {
        h.step();
    }
    let after = drawn(&h);
    assert_ne!(after.window, before.window);
    assert_eq!(after.layout.plot, plot);
    assert_eq!(after.layout.key, Some(key));
    // Without roles, no key and no room for one: the plot is taller by the key's height.
    let none = shown("", |_| {});
    let l = drawn(&none).layout;
    assert_eq!(l.key, None);
    assert!((l.plot.height() - plot.height() - key.height()).abs() < 0.01);
}

#[test]
fn a_role_without_a_colour_takes_the_traces_in_first_place_and_ink_after() {
    let body = "[[role]]\nname = \"first\"\nweight = 2\n\n[[role]]\nname = \"second\"\n\n";
    let stored = Color32::from_rgb(0xd9, 0x48, 0x0f);
    let cases = [
        ("", palette().trace.unwrap(), Some(palette().trace.unwrap())),
        ("[view]\ncolour = \"#d9480f\"\n\n", stored, None),
    ];
    for (view, trace, host) in cases {
        let h = shown(&format!("{view}{body}"), |p| p.trace = host);
        let key = drawn(&h).layout.key.expect("a key");
        let first = strokes(&h, trace, key);
        assert_eq!(first.len(), 1, "{first:?}");
        assert_eq!(first[0].1, 3.0);
        let second = strokes(&h, palette().ink, key);
        assert_eq!(second.len(), 1, "{second:?}");
        assert_eq!(second[0].1, 1.0);
        assert!(first[0].0[0].x < second[0].0[0].x, "in declaration order");
    }
}
