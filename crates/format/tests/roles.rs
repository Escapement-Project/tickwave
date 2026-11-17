//! Paint roles (ANNOTATIONS.md, "Roles"): the schema's bounds, each problem naming the role, and
//! one name's declarations across a sidecar's files held to agree once their defaults are in.

use escapement_tickwave_format::annotations::{
    Colour, Included, Role, resolve, resolve_lenient, roles,
};

const ID: &str = "blake3:00ff000000000000000000000000000000000000000000000000000000000000";

/// A hand-written file with these lines after its head, including `rec.swd.jsonl`.
fn hand(body: &str) -> String {
    format!(
        "format = \"tickwave-annotations/1\"\nrecording_id = \"{ID}\"\ninclude = [\"rec.swd.jsonl\"]\n\n{body}"
    )
}

/// A generator's file whose first line declares `roles` (JSON), with one painted line.
fn generated(roles: &str) -> String {
    format!(
        "{{\"format\": \"tickwave-annotations/1\", \"source\": \"swd\", \"recording_id\": \"{ID}\", \"role\": {roles}}}\n\
         {{\"id\": \"a/1\", \"start\": 10, \"end\": 20, \"channels\": [\"SWDIO\"], \"paint\": \"probe\"}}\n"
    )
}

fn roles_of(hand_body: &str, gen_roles: &str) -> (Vec<Role>, Vec<String>) {
    let g = generated(gen_roles);
    roles(
        &hand(hand_body),
        &[Included {
            name: "rec.swd.jsonl",
            text: &g,
        }],
    )
}

#[test]
fn a_declaration_takes_its_defaults_and_a_colour_by_face() {
    let (got, problems) = roles_of(
        "[[role]]\nname = \"neither\"\nlabel = \"neither drives\"\ncolour = [\"#8A8A8A\", \"#5c5c5c\"]\ndash = [2, 1]\n\n[[role]]\nname = \"bare\"\ncolour = \"#112233\"\n",
        r#"[{"name": "probe", "label": "probe drives"}, {"name": "target", "label": "target drives", "weight": 1}]"#,
    );
    assert_eq!(problems, Vec::<String>::new());
    let names: Vec<&str> = got.iter().map(|r| r.name.as_str()).collect();
    // The hand-written file's first, then the generator's.
    assert_eq!(names, ["neither", "bare", "probe", "target"]);
    assert_eq!(
        got[0],
        Role {
            name: "neither".into(),
            label: "neither drives".into(),
            colour: Some(Colour {
                day: "#8a8a8a".into(),
                night: "#5c5c5c".into()
            }),
            weight: 0.0,
            dash: Some(vec![2.0, 1.0]),
        }
    );
    // One colour is both faces'; a label left out is the name.
    assert_eq!(
        got[1].colour,
        Some(Colour {
            day: "#112233".into(),
            night: "#112233".into()
        })
    );
    assert_eq!(got[1].label, "bare");
    assert_eq!((got[3].weight, &got[3].dash), (1.0, &None));
}

#[test]
fn each_bound_is_a_problem_naming_the_role() {
    let one = |fields: &str| roles_of(&format!("[[role]]\n{fields}\n"), "[]").1;
    let long = "x".repeat(201);
    for (fields, says) in [
        ("name = \"Probe\"", r#"role "Probe": a name is lower-case"#),
        (
            "name = \"probe-x\"",
            r#"role "probe-x": a name is lower-case"#,
        ),
        (
            "name = \"9lives\"",
            r#"role "9lives": a name is lower-case"#,
        ),
        (
            &format!("name = \"{}\"", "a".repeat(33)),
            "a name is at most 32 bytes",
        ),
        ("label = \"x\"", "role 1 has no name"),
        (
            &format!("name = \"r\"\nlabel = \"{long}\""),
            r#"role "r": label is longer than 200 bytes"#,
        ),
        (
            "name = \"r\"\ncolour = \"#12345\"",
            r##"role "r": colour is "#rrggbb""##,
        ),
        (
            "name = \"r\"\ncolour = \"red\"",
            r##"role "r": colour is "#rrggbb""##,
        ),
        (
            "name = \"r\"\ncolour = \"#12345g\"",
            r##"role "r": colour is "#rrggbb""##,
        ),
        (
            "name = \"r\"\ncolour = [\"#123456\"]",
            r##"role "r": colour is "#rrggbb""##,
        ),
        (
            "name = \"r\"\ncolour = [\"#123456\", \"blue\"]",
            r#"role "r": colour: each of a pair"#,
        ),
        (
            "name = \"r\"\nweight = 4.5",
            r#"role "r": weight is a number from 0 to 4"#,
        ),
        (
            "name = \"r\"\nweight = -1",
            r#"role "r": weight is a number from 0 to 4"#,
        ),
        (
            "name = \"r\"\ndash = [1, 2, 3]",
            r#"role "r": dash is (dash, gap) pairs"#,
        ),
        (
            "name = \"r\"\ndash = []",
            r#"role "r": dash is (dash, gap) pairs"#,
        ),
        (
            "name = \"r\"\ndash = [0, 0]",
            r#"role "r": dash is (dash, gap) pairs"#,
        ),
        (
            "name = \"r\"\ndash = [1, 65]",
            r#"role "r": dash is (dash, gap) pairs"#,
        ),
        // Under a line width together: a renderer would cut a row into about 10^30 pieces a
        // line width.
        (
            "name = \"r\"\ndash = [1e-30, 1e-30]",
            r#"role "r": dash is (dash, gap) pairs, at most 8 numbers, each from 0 to 64 line widths, together at least 1"#,
        ),
        (
            "name = \"r\"\ndash = [0.5, 0.25]",
            r#"role "r": dash is (dash, gap) pairs"#,
        ),
        (
            "name = \"r\"\ndash = [1, -1]",
            r#"role "r": dash is (dash, gap) pairs"#,
        ),
        (
            "name = \"r\"\ndash = [1, 1, 1, 1, 1, 1, 1, 1, 1, 1]",
            r#"role "r": dash is (dash, gap) pairs"#,
        ),
        (
            "name = \"r\"\nstyle = \"bold\"",
            r#"role "r": unknown field "style""#,
        ),
    ] {
        let problems = one(fields);
        assert!(
            problems.iter().any(|p| p.contains(says)),
            "{fields}: {problems:?}"
        );
    }
    // At the bounds, fine: 32 bytes, 200 bytes, weight 4, eight numbers each up to 64.
    for fields in [
        format!("name = \"{}\"", "a".repeat(32)),
        format!("name = \"r\"\nlabel = \"{}\"", "x".repeat(200)),
        "name = \"r\"\nweight = 4".into(),
        "name = \"r\"\nweight = 0".into(),
        "name = \"r\"\ndash = [64, 0, 0, 64, 1, 1, 0.5, 0.25]".into(),
        "name = \"r\"\ndash = [0.5, 0.5]".into(),
        "name = \"r_2\"\ncolour = \"#ABCDEF\"".into(),
    ] {
        assert_eq!(one(&fields), Vec::<String>::new(), "{fields}");
    }
    // In a generator's first line, the same.
    for dash in ["[1]", "[1e-30, 1e-30]"] {
        let (_, problems) = roles_of("", &format!(r#"[{{"name": "probe", "dash": {dash}}}]"#));
        assert!(
            problems
                .iter()
                .any(|p| p.starts_with(r#"rec.swd.jsonl: role "probe": dash"#)),
            "{dash}: {problems:?}"
        );
    }
}

#[test]
fn more_than_64_roles_in_a_file_or_between_them_is_one_problem_and_the_rest_are_not_read() {
    let tables = |n: usize, from: usize| {
        (from..from + n)
            .map(|k| format!("[[role]]\nname = \"r{k}\"\n"))
            .collect::<String>()
    };
    assert_eq!(roles_of(&tables(64, 0), "[]").1, Vec::<String>::new());
    // A file's first 64 are read, and the file is one problem.
    let (got, problems) = roles_of(&tables(65, 0), "[]");
    assert_eq!(got.len(), 64);
    assert_eq!(got[63].name, "r63");
    assert_eq!(problems, ["the TOML file: declares 65 roles, more than 64"]);
    // 40 here and 40 there, 80 between them: the first 64 names, and one problem.
    let json: Vec<String> = (100..140)
        .map(|k| format!("{{\"name\": \"r{k}\"}}"))
        .collect();
    let (got, problems) = roles_of(&tables(40, 0), &format!("[{}]", json.join(", ")));
    assert_eq!(got.len(), 64);
    assert_eq!(got[63].name, "r123");
    assert_eq!(problems, ["the sidecar's files declare more than 64 roles"]);
}

#[test]
fn a_file_of_10000_roles_is_refused_with_one_problem_reading_no_more_than_the_bound() {
    // Every table is a role of its own that would conflict with nothing: a reader that went on
    // past the bound would collect all of them, each looked up among those before.
    let many: String = (0..10_000)
        .map(|k| format!("[[role]]\nname = \"r{k}\"\nlabel = \"role {k}\"\n"))
        .collect();
    let (got, problems) = roles_of(&many, "[]");
    assert_eq!(
        problems,
        ["the TOML file: declares 10000 roles, more than 64"]
    );
    assert_eq!(got.len(), 64);
    // So does a generator's first line, and the check says the same of each.
    let json: Vec<String> = (0..10_000)
        .map(|k| format!("{{\"name\": \"g{k}\"}}"))
        .collect();
    let (got, problems) = roles_of("", &format!("[{}]", json.join(", ")));
    assert_eq!(
        problems,
        ["rec.swd.jsonl: declares 10000 roles, more than 64"]
    );
    assert_eq!(got.len(), 64);
}

#[test]
fn a_name_twice_in_a_file_is_a_problem_and_twice_across_files_only_where_they_differ() {
    let (_, problems) = roles_of(
        "[[role]]\nname = \"probe\"\n[[role]]\nname = \"probe\"\n",
        "[]",
    );
    assert_eq!(
        problems,
        [r#"the TOML file: role "probe" is declared twice"#]
    );
    // The same in both files once defaults are in: a label of the name, weight 0, one colour as
    // a pair of it, fields in any order.
    for (here, there) in [
        ("name = \"probe\"", r#"{"name": "probe"}"#),
        (
            "name = \"probe\"\nlabel = \"probe\"\nweight = 0",
            r#"{"name": "probe"}"#,
        ),
        (
            "name = \"probe\"\ncolour = \"#aabbcc\"",
            r##"{"name": "probe", "colour": ["#AABBCC", "#aabbcc"]}"##,
        ),
        (
            "dash = [2, 1]\nname = \"probe\"",
            r#"{"dash": [2, 1], "name": "probe"}"#,
        ),
    ] {
        let (got, problems) = roles_of(&format!("[[role]]\n{here}\n"), &format!("[{there}]"));
        assert_eq!(problems, Vec::<String>::new(), "{here} / {there}");
        assert_eq!(got.len(), 1);
    }
    for (here, there, fields) in [
        (
            "name = \"probe\"\nlabel = \"probe drives\"",
            r#"{"name": "probe"}"#,
            "label",
        ),
        (
            "name = \"probe\"\nweight = 1",
            r#"{"name": "probe"}"#,
            "weight",
        ),
        // An absent colour or dash equals only an absent one.
        (
            "name = \"probe\"\ncolour = \"#000000\"",
            r#"{"name": "probe"}"#,
            "colour",
        ),
        (
            "name = \"probe\"\ndash = [2, 1]",
            r#"{"name": "probe", "dash": [2, 1, 2, 1]}"#,
            "dash",
        ),
        (
            "name = \"probe\"\ncolour = [\"#000000\", \"#ffffff\"]\nweight = 2",
            r##"{"name": "probe", "colour": "#000000"}"##,
            "colour, weight",
        ),
    ] {
        let (got, problems) = roles_of(&format!("[[role]]\n{here}\n"), &format!("[{there}]"));
        assert_eq!(
            problems,
            [format!(
                "role \"probe\": the TOML file and rec.swd.jsonl declare it differently ({fields})"
            )],
            "{here} / {there}"
        );
        // The first declaration stays.
        assert_eq!(got.len(), 1);
    }
}

#[test]
fn a_role_problem_fails_the_check_and_a_paint_names_a_role() {
    // The generator's line paints probe, which its refused declaration still names: one problem,
    // not that one and an undeclared paint as well.
    let header = common::header();
    let g = generated(r#"[{"name": "probe", "weight": 9}]"#);
    let got = resolve(
        &hand(""),
        &[Included {
            name: "rec.swd.jsonl",
            text: &g,
        }],
        &header,
        None,
    );
    assert_eq!(
        got.unwrap_err(),
        [r#"rec.swd.jsonl: role "probe": weight is a number from 0 to 4"#]
    );
    let g = generated("[]").replace("\"paint\": \"probe\"", "\"paint\": \"Probe\"");
    let got = resolve(
        &hand(""),
        &[Included {
            name: "rec.swd.jsonl",
            text: &g,
        }],
        &header,
        None,
    );
    assert_eq!(
        got.unwrap_err(),
        [
            r#"rec.swd.jsonl, line 2: paint: role "Probe": a name is lower-case letters, digits and '_', starting with a letter"#
        ]
    );
}

#[test]
fn a_paint_names_a_declared_role_and_the_check_says_so_once_a_name_where_it_doesnt() {
    let header = common::header();
    // The generator declares target; its lines paint target, late (twice) and early.
    let g = format!(
        "{{\"format\": \"tickwave-annotations/1\", \"source\": \"swd\", \"recording_id\": \"{ID}\", \"role\": [{{\"name\": \"target\"}}]}}\n\
         {{\"id\": \"a/1\", \"start\": 900, \"end\": 950, \"paint\": \"late\"}}\n\
         {{\"id\": \"a/2\", \"start\": 10, \"end\": 20, \"paint\": \"early\"}}\n\
         {{\"id\": \"a/3\", \"start\": 30, \"end\": 40, \"paint\": \"target\"}}\n\
         {{\"id\": \"a/4\", \"start\": 50, \"end\": 60, \"paint\": \"late\"}}\n"
    );
    let inc = [Included {
        name: "rec.swd.jsonl",
        text: &g,
    }];
    // The hand-written file declares probe, and paints with it, with target, which the generator
    // declares, and with handy, which nothing does.
    let h = hand(
        "[[role]]\nname = \"probe\"\n\n\
         [[annotation]]\nid = \"mine\"\nstart = 500\nend = 600\npaint = \"handy\"\n\n\
         [[annotation]]\nid = \"theirs\"\nstart = 600\nend = 700\npaint = \"target\"\n\n\
         [[annotation]]\nid = \"ours\"\nstart = 700\nend = 800\npaint = \"probe\"\n",
    );
    assert_eq!(
        resolve(&h, &inc, &header, None).unwrap_err(),
        [
            r#"annotation 1: paint "handy" names no role the sidecar declares"#,
            r#"rec.swd.jsonl, line 2: paint "late" names no role the sidecar declares"#,
            r#"rec.swd.jsonl, line 3: paint "early" names no role the sidecar declares"#,
        ]
    );
    // A reader keeps each annotation, its paint as written, and says why: it draws the paint as
    // none (the view core's `paints`, the web's `Plot`).
    let lenient = resolve_lenient(&h, &inc, &header, None);
    assert_eq!(lenient.annotations.len(), 7);
    assert_eq!(lenient.annotations[0].paint.as_deref(), Some("handy"));
    assert_eq!(lenient.problems.len(), 3);
    // The declarations' order: the hand-written file's, then the generator's.
    let (declared, problems) = roles(&h, &inc);
    assert_eq!(problems, Vec::<String>::new());
    let names: Vec<&str> = declared.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["probe", "target"]);
}

#[test]
fn two_generators_declaring_a_role_alike_keep_its_first_place() {
    let head = |source: &str, roles: &str| {
        format!(
            "{{\"format\": \"tickwave-annotations/1\", \"source\": \"{source}\", \"recording_id\": \"{ID}\", \"role\": {roles}}}\n"
        )
    };
    let (a, b) = (
        head(
            "one",
            r#"[{"name": "probe"}, {"name": "target", "weight": 1}]"#,
        ),
        head(
            "two",
            r#"[{"name": "neither"}, {"name": "target", "weight": 1}]"#,
        ),
    );
    // Given in the other order: include order sets the place.
    let inc = [
        Included {
            name: "rec.two.jsonl",
            text: &b,
        },
        Included {
            name: "rec.one.jsonl",
            text: &a,
        },
    ];
    let h = "format = \"tickwave-annotations/1\"\nrecording_id = \"blake3:00ff000000000000000000000000000000000000000000000000000000000000\"\ninclude = [\"rec.one.jsonl\", \"rec.two.jsonl\"]\n";
    let (got, problems) = roles(h, &inc);
    assert_eq!(problems, Vec::<String>::new());
    let names: Vec<&str> = got.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["probe", "target", "neither"]);
}

mod common {
    use escapement_tickwave_format::header::{
        ChannelHeader, ChannelKind, Header, SourceFormat, SourceHeader,
    };
    use escapement_tickwave_format::model::Ratio;

    pub fn header() -> Header {
        let digital = |name: &str| ChannelHeader {
            name: name.into(),
            source_name: name.to_lowercase(),
            source: 0,
            data: ChannelKind::Digital {
                initial: true,
                end: 1_000,
                edges: 10,
                gaps: Vec::new(),
                gap_count: None,
            },
            shaping: Default::default(),
            overview_shift: None,
            blocks: 1,
            overview_blocks: Vec::new(),
            codec: None,
            requires: Vec::new(),
        };
        Header {
            timebase: Ratio::integer(1_000_000_000).unwrap(),
            length: 1_000,
            channels: vec![digital("SWCLK"), digital("SWDIO")],
            derived: Vec::new(),
            sources: vec![SourceHeader::new(
                SourceFormat::Vcd,
                Ratio::integer(1_000_000_000).unwrap(),
            )],
            title: None,
            meta: Default::default(),
            content_id: super::ID.parse().unwrap(),
            overview_shift: 12,
            annotation_parts: None,
            requires: Vec::new(),
        }
    }
}
