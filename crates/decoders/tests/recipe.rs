//! Recipes: `[[decode]]` tables read and bound over a recording through the registry, against the
//! same analyses bound by hand, and every refusal.

#![cfg(feature = "recipe")]

use escapement_tickwave_decode::{
    Host, HostOptions, InputSpec, Span, Tick, name, wire::WireSource,
};
use escapement_tickwave_decoders::recipe::{self, Input, Recipe, RecipeError};
use escapement_tickwave_decoders::{adiv5, swd};
use escapement_tickwave_view::{Cache, Recording};
use std::sync::Arc;

fn all() -> Vec<Span> {
    vec![Span {
        start: Tick::MIN,
        end: Tick::MAX,
    }]
}

fn fixture() -> Arc<Recording> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../web/fixtures/identify-swd.tickwave");
    let file = std::fs::File::open(path).unwrap();
    Arc::new(Recording::open(Arc::new(file), Cache::new(16 << 20)).unwrap())
}

const SWD: &str = r#"
[[decode]]
id = "swd"
decoder = "swd"
version = 1
label = "SWD"
inputs = { SWCLK = "SWCLK", SWDIO = "SWDIO" }
settings = { turnaround = 1 }

[[decode]]
id = "mem"
decoder = "mem-ap"
version = 1
inputs = { accesses = { analysis = "adi" } }

[[decode]]
id = "adi"
decoder = "adiv5"
version = 1
inputs = { transfers = { analysis = "swd" } }
"#;

#[test]
fn a_recipe_binds_what_code_binds_by_hand() {
    let rec = fixture();
    let window = recipe::window(&rec);
    assert_eq!(window, 100_000, "100 µs at the fixture's 1 GS/s");
    let r = Recipe::parse(SWD).unwrap();
    assert_eq!(r.analyses[0].label.as_deref(), Some("SWD"));
    assert_eq!(
        r.analyses[2].inputs,
        [("transfers".to_owned(), Input::Analysis("swd".to_owned()))]
    );
    let bound = r
        .bind(&rec, window, &all(), HostOptions::default())
        .unwrap();
    let clock = rec.channel("SWCLK").unwrap();
    let names: Vec<_> = (bound.analyses.iter())
        .map(|a| {
            (
                bound.host.name(a.id).to_owned(),
                a.label.as_str(),
                a.timeline,
            )
        })
        .collect();
    assert_eq!(
        names,
        [
            ("swd".to_owned(), "SWD", clock),
            ("adi".to_owned(), "adiv5", clock),
            ("mem".to_owned(), "mem-ap", clock),
        ],
        "in the order bound, each upstream first; on the wire analysis's first channel's timeline"
    );
    let mut h = bound.host;
    h.run_until(|_| false).unwrap();
    let mut by_hand = Host::default();
    let data = rec.channel("SWDIO").unwrap();
    let s = by_hand
        .add(
            name("swd"),
            swd::definition(),
            InputSpec::Wire {
                source: Box::new(WireSource::new(rec.clone(), vec![clock, data]).unwrap()),
                local: None,
            },
            swd::Swd::factory(window),
            &[swd::turnaround(1)],
            all(),
        )
        .unwrap();
    let a = by_hand
        .add(
            name("adi"),
            adiv5::adi_definition(),
            InputSpec::Upstream(s),
            adiv5::Adi::factory(),
            &[],
            all(),
        )
        .unwrap();
    let m = by_hand
        .add(
            name("mem"),
            adiv5::mem_definition(),
            InputSpec::Upstream(a),
            adiv5::Mem::factory(),
            &[],
            all(),
        )
        .unwrap();
    by_hand.run_until(|_| false).unwrap();
    for (id, hand) in [("swd", s), ("adi", a), ("mem", m)] {
        let ours = h.index(id).unwrap();
        assert_eq!(h.store(ours).counts, by_hand.store(hand).counts, "{id}");
        assert!(h.store(ours).counts.closes > 0, "{id} decoded something");
    }
}

#[test]
fn every_rule_of_the_table_is_held_to() {
    let rec = fixture();
    let bind = |text: &str| -> Result<Host, RecipeError> {
        Ok(Recipe::parse(text)?
            .bind(&rec, 100_000, &[], HostOptions::default())?
            .host)
    };
    let wire = |extra: &str| {
        format!(
            "[[decode]]\nid = \"swd\"\ndecoder = \"swd\"\nversion = 1\ninputs = {{ SWCLK = \"SWCLK\", SWDIO = \"SWDIO\" }}\n{extra}"
        )
    };
    assert!(bind(&wire("")).is_ok());
    let refused = |text: &str| bind(text).err().map(|e| e.to_string()).unwrap_or_default();
    assert!(matches!(bind("decode = 3"), Err(RecipeError::Table(_))));
    assert!(matches!(bind("[view]\n"), Err(RecipeError::Table(_))));
    assert!(matches!(bind("[[decode]"), Err(RecipeError::Toml(_))));
    assert!(refused(&wire("colour = \"red\"")).contains("takes no colour"));
    assert!(
        refused(&wire("").replace("\"swd\"\ndecoder", "\"s w\"\ndecoder")).contains("whitespace")
    );
    assert!(refused(&format!("{}{}", wire(""), wire(""))).contains("named twice"));
    assert!(matches!(
        bind(&wire("").replace("version = 1", "version = 2")),
        Err(RecipeError::UnknownDecoder { .. })
    ));
    assert!(matches!(
        bind(&wire("").replace("SWDIO = \"SWDIO\"", "SWDIO = \"D9\"")),
        Err(RecipeError::NoChannel { .. })
    ));
    assert!(matches!(
        bind(&wire("").replace(", SWDIO = \"SWDIO\"", "")),
        Err(RecipeError::MissingPort { .. })
    ));
    assert!(matches!(
        bind(&wire("").replace("SWDIO = ", "TDO = \"SWDIO\", SWDIO = ")),
        Err(RecipeError::UnknownPort { .. })
    ));
    assert!(matches!(
        bind(&wire("settings = { turnaround = 9 }")),
        Err(RecipeError::Bind { .. })
    ));
    assert!(refused(&wire("settings = { turnaround = 1.5 }")).contains("setting turnaround"));
    let stacked = |inputs: &str| {
        format!(
            "{}[[decode]]\nid = \"adi\"\ndecoder = \"adiv5\"\nversion = 1\ninputs = {inputs}\n",
            wire("")
        )
    };
    assert!(bind(&stacked("{ transfers = { analysis = \"swd\" } }")).is_ok());
    assert!(matches!(
        bind(&stacked("{ transfers = { analysis = \"jtag\" } }")),
        Err(RecipeError::UnknownAnalysis { .. })
    ));
    assert!(matches!(
        bind(&stacked("{ scans = { analysis = \"swd\" } }")),
        Err(RecipeError::UnknownPort { .. })
    ));
    assert!(matches!(
        bind(&stacked(
            "{ transfers = { analysis = \"swd\" }, SWCLK = \"SWCLK\" }"
        )),
        Err(RecipeError::OneUpstream { .. })
    ));
    let cycle = "[[decode]]\nid = \"a\"\ndecoder = \"adiv5\"\nversion = 1\ninputs = { transfers = { analysis = \"b\" } }\n\
                 [[decode]]\nid = \"b\"\ndecoder = \"adiv5\"\nversion = 1\ninputs = { transfers = { analysis = \"a\" } }\n";
    assert!(matches!(bind(cycle), Err(RecipeError::Cycle)));
    let missing = cycle.replace("{ analysis = \"a\" }", "{ analysis = \"x\" }");
    assert!(
        matches!(bind(&missing), Err(RecipeError::UnknownAnalysis { analysis, upstream }) if analysis == "b" && upstream == "x"),
        "the analysis that names one missing, though another is first"
    );
    let many: String = (0..=recipe::ANALYSES_AT_MOST)
        .map(|k| wire("").replace("id = \"swd\"", &format!("id = \"s{k}\"")))
        .collect();
    assert!(refused(&many).contains("more than"));
}

#[test]
fn an_optional_port_may_be_left_out() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../web/fixtures/identify-jtag.tickwave");
    let file = std::fs::File::open(path).unwrap();
    let rec = Arc::new(Recording::open(Arc::new(file), Cache::new(16 << 20)).unwrap());
    let text = "[[decode]]\nid = \"jtag\"\ndecoder = \"jtag\"\nversion = 1\n\
                inputs = { TCK = \"TCK\", TMS = \"TMS\", TDI = \"TDI\" }\n\
                settings = { tdo_edge = \"fall\", chain = [4] }\n";
    let mut h = (Recipe::parse(text).unwrap())
        .bind(&rec, recipe::window(&rec), &all(), HostOptions::default())
        .unwrap()
        .host;
    h.run_until(|_| false).unwrap();
    let j = h.index("jtag").unwrap();
    assert!(h.store(j).counts.closes > 0);
}
