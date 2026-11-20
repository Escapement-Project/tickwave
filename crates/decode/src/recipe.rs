//! Recipes: the analyses to run over a recording, as `[[decode]]` tables name them, bound through
//! a registry of decoders.
//!
//! ```toml
//! [[decode]]
//! id = "swd"
//! decoder = "swd"
//! version = 1
//! inputs = { SWCLK = "SWCLK", SWDIO = "SWDIO" }
//!
//! [[decode]]
//! id = "adi"
//! decoder = "adiv5"
//! version = 1
//! inputs = { transfers = { analysis = "swd" } }
//! ```
//!
//! Each table is one analysis: `id` names it, as citations do; `decoder` and `version` (the major)
//! name its definition; `label`, if given, names its track. `inputs` gives each port it binds by
//! the definition's name for it: a channel by its name for a wire port, which an optional port may
//! leave out, or `{ analysis = "<id>" }` for another analysis's output. `settings`, if given, holds
//! the definition's settings by name, a choice by its name; one left out takes its default. The
//! host checks the settings against the definition when it binds them.

use crate::wire::{BindWire, WireSource};
use crate::{
    AnalysisId, BindError, Definition, Factory, Host, HostOptions, InputDecl, InputSpec, Name,
    Setting, Span, Tick,
};
use escapement_tickwave_view::Recording;
use escapement_tickwave_view::cursor::timeline_map;
use escapement_tickwave_view::format::mapping::Mapping;
use std::sync::Arc;

/// The most analyses a recipe holds.
pub const ANALYSES_AT_MOST: usize = 64;

/// The decoders a host binds recipes through, and the recipes it offers whole.
#[derive(Clone, Debug, Default)]
pub struct Registry {
    pub decoders: Vec<Entry>,
    pub presets: Vec<Preset>,
}

impl Registry {
    /// The decoder of this name and major version.
    pub fn find(&self, name: &str, version: u32) -> Option<&Entry> {
        (self.decoders.iter())
            .find(|e| e.definition.name == name && e.definition.version == version)
    }
}

/// A decoder a registry holds: its definition, the name its track takes where a recipe gives none,
/// and the factory of its runs.
#[derive(Clone)]
pub struct Entry {
    pub definition: Arc<Definition>,
    pub label: String,
    factory: Arc<dyn Fn(Tick) -> Factory + Send + Sync>,
}

impl Entry {
    /// `factory` makes the factory of a run; a wire decoder's reads windows of the ticks it is
    /// given.
    pub fn new(
        definition: Arc<Definition>,
        label: impl Into<String>,
        factory: impl Fn(Tick) -> Factory + Send + Sync + 'static,
    ) -> Self {
        Self {
            definition,
            label: label.into(),
            factory: Arc::new(factory),
        }
    }

    /// The factory of its runs; a wire decoder's reads windows of `window` ticks of input.
    pub fn factory(&self, window: Tick) -> Factory {
        (self.factory)(window)
    }
}

impl std::fmt::Debug for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        (f.debug_struct("Entry"))
            .field("decoder", &self.definition.name)
            .field("version", &self.definition.version)
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

/// A recipe a registry offers whole, by its name: "SWD, ADIv5, MEM-AP".
#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    pub name: String,
    pub recipe: Recipe,
}

/// About 100 µs of `recording`'s input, in ticks: a wire decoder's window that keeps one step of
/// a host well under a millisecond.
pub fn window(recording: &Recording) -> Tick {
    let tb = recording.timebase();
    let per_s = tb.num() as f64 / tb.den() as f64;
    ((per_s * 1e-4) as Tick).max(1)
}

/// The analyses to run over a recording, each a `[[decode]]` table.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Recipe {
    pub analyses: Vec<Analysis>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Analysis {
    pub id: String,
    pub decoder: String,
    pub version: u32,
    /// The track's name; the registry's label for the decoder where none is given.
    pub label: Option<String>,
    /// Each port it binds, by the definition's name for it.
    pub inputs: Vec<(String, Input)>,
    pub settings: Vec<(String, Setting)>,
}

/// A recipe bound over a recording: the host, and what a view draws of each analysis.
pub struct Binding {
    pub host: Host,
    /// Each analysis, in the order the host holds them.
    pub analyses: Vec<Bound>,
}

/// An analysis as a recipe bound it. The host knows it by its recipe id (`Host::name`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bound {
    pub id: AnalysisId,
    /// Its track's name: the recipe's label, else the registry's.
    pub label: String,
    /// The channel whose source's timeline its items are on: its wire analysis's first port given
    /// a channel; and that timeline on the recording's axis.
    pub timeline: usize,
    pub map: Mapping,
}

/// What a port reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    /// A channel of the recording, by its name.
    Channel(String),
    /// Another analysis's output, by its id.
    Analysis(String),
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RecipeError {
    #[error("the recipe is not TOML: {0}")]
    Toml(String),
    #[error("{0}")]
    Table(String),
    #[error("{analysis}: no decoder {decoder} of major version {version}")]
    UnknownDecoder {
        analysis: String,
        decoder: String,
        version: u32,
    },
    #[error("{analysis}: no analysis {upstream} to read")]
    UnknownAnalysis { analysis: String, upstream: String },
    #[error("the analyses read each other in a cycle")]
    Cycle,
    #[error("{analysis}: {decoder} has no port {port}")]
    UnknownPort {
        analysis: String,
        decoder: String,
        port: String,
    },
    #[error("{analysis}: port {port} needs a channel")]
    MissingPort { analysis: String, port: String },
    #[error("{analysis}: the recording has no channel {channel}")]
    NoChannel { analysis: String, channel: String },
    #[error("{analysis}: reads one analysis, through one port")]
    OneUpstream { analysis: String },
    #[error("{analysis}: {source}")]
    Wire {
        analysis: String,
        #[source]
        source: BindWire,
    },
    #[error("{analysis}: {source}")]
    Bind {
        analysis: String,
        #[source]
        source: BindError,
    },
}

fn table(why: impl Into<String>) -> RecipeError {
    RecipeError::Table(why.into())
}

impl Recipe {
    /// Reads a recipe: a file of `[[decode]]` tables and nothing else.
    pub fn parse(text: &str) -> Result<Self, RecipeError> {
        let doc: toml::Table = text
            .parse()
            .map_err(|e| RecipeError::Toml(format!("{e}")))?;
        if let Some(k) = doc.keys().find(|k| *k != "decode") {
            return Err(table(format!(
                "a recipe holds only [[decode]] tables, not {k}"
            )));
        }
        let tables = match doc.get("decode") {
            None => &vec![],
            Some(toml::Value::Array(a)) => a,
            Some(_) => return Err(table("decode is an array of tables, [[decode]]")),
        };
        if tables.len() > ANALYSES_AT_MOST {
            return Err(table(format!("more than {ANALYSES_AT_MOST} analyses")));
        }
        let mut analyses: Vec<Analysis> = Vec::with_capacity(tables.len());
        for t in tables {
            let a = Analysis::parse(t)?;
            if analyses.iter().any(|b| b.id == a.id) {
                return Err(table(format!("analysis {} is named twice", a.id)));
            }
            analyses.push(a);
        }
        Ok(Self { analyses })
    }

    /// Binds its analyses over `recording`, each after those it reads, through `registry`; each
    /// wire decoder reads windows of `window` ticks, and each analysis keeps whole items in
    /// `keep`.
    pub fn bind(
        &self,
        registry: &Registry,
        recording: &Arc<Recording>,
        window: Tick,
        keep: &[Span],
        options: HostOptions,
    ) -> Result<Binding, RecipeError> {
        let mut h = Host::new(options);
        let mut analyses: Vec<Bound> = Vec::with_capacity(self.analyses.len());
        let mut left: Vec<&Analysis> = self.analyses.iter().collect();
        while !left.is_empty() {
            let ready = left.iter().position(|a| {
                (a.inputs.iter()).all(|(_, i)| match i {
                    Input::Analysis(up) => analyses.iter().any(|b| h.name(b.id) == up),
                    Input::Channel(_) => true,
                })
            });
            let Some(k) = ready else {
                let unknown = left.iter().find_map(|a| {
                    (a.inputs.iter()).find_map(|(_, i)| match i {
                        Input::Analysis(up) if !self.analyses.iter().any(|b| b.id == *up) => {
                            Some((a.id.clone(), up.clone()))
                        }
                        _ => None,
                    })
                });
                return Err(match unknown {
                    Some((analysis, upstream)) => {
                        RecipeError::UnknownAnalysis { analysis, upstream }
                    }
                    None => RecipeError::Cycle,
                });
            };
            let a = left.remove(k);
            let b = a.add(&mut h, registry, recording, &analyses, window, keep)?;
            analyses.push(b);
        }
        Ok(Binding { host: h, analyses })
    }

    /// The recipe with each wire port given one of `channels`: the channel it names where that is
    /// one of them, else the one named as the port is, letters' case aside, else none, as an
    /// optional port may be left. A form that binds it starts from this.
    pub fn on_channels(&self, registry: &Registry, channels: &[String]) -> Recipe {
        let mut recipe = self.clone();
        for a in &mut recipe.analyses {
            let Some(entry) = registry.find(&a.decoder, a.version) else {
                continue;
            };
            if a.inputs
                .iter()
                .any(|(_, i)| matches!(i, Input::Analysis(_)))
            {
                continue;
            }
            let mut inputs = vec![];
            for p in &entry.definition.ports {
                let named = (a.inputs.iter())
                    .find(|(n, _)| p.name == n.as_str())
                    .and_then(|(_, i)| match i {
                        Input::Channel(c) if channels.contains(c) => Some(c.clone()),
                        _ => None,
                    });
                let by_port = || {
                    (channels.iter())
                        .find(|c| c.eq_ignore_ascii_case(&p.name))
                        .cloned()
                };
                if let Some(c) = named.or_else(by_port) {
                    inputs.push((p.name.to_string(), Input::Channel(c)));
                }
            }
            a.inputs = inputs;
        }
        recipe
    }

    /// The recipe without analysis `id` and every analysis that reads it, near or far.
    pub fn without(&self, id: &str) -> Recipe {
        let mut gone = vec![id];
        let reads = |a: &Analysis, gone: &[&str]| {
            (a.inputs.iter())
                .any(|(_, i)| matches!(i, Input::Analysis(up) if gone.contains(&up.as_str())))
        };
        while let Some(a) =
            (self.analyses.iter()).find(|a| !gone.contains(&a.id.as_str()) && reads(a, &gone))
        {
            gone.push(&a.id);
        }
        let analyses = (self.analyses.iter())
            .filter(|a| !gone.contains(&a.id.as_str()))
            .cloned()
            .collect();
        Recipe { analyses }
    }

    /// The recipe with each id `taken` says is taken renamed, and what reads it: `swd` to `swd-2`,
    /// or the first number past that is free. For adding it beside analyses already bound.
    pub fn fresh(&self, taken: impl Fn(&str) -> bool) -> Recipe {
        let mut names: Vec<(String, String)> = vec![];
        for a in &self.analyses {
            let free = |n: &str, names: &[(String, String)]| {
                !taken(n)
                    && !names.iter().any(|(_, to)| to == n)
                    && !(self.analyses.iter()).any(|b| b.id == n && b.id != a.id)
            };
            let mut id = a.id.clone();
            let mut k = 2;
            while !free(&id, &names) {
                id = format!("{}-{k}", a.id);
                k += 1;
            }
            names.push((a.id.clone(), id));
        }
        let renamed = |id: &str| {
            (names.iter())
                .find(|(from, _)| from == id)
                .map_or_else(|| id.to_owned(), |(_, to)| to.clone())
        };
        let analyses = (self.analyses.iter())
            .map(|a| Analysis {
                id: renamed(&a.id),
                inputs: (a.inputs.iter())
                    .map(|(port, i)| {
                        let i = match i {
                            Input::Analysis(up) => Input::Analysis(renamed(up)),
                            c => c.clone(),
                        };
                        (port.clone(), i)
                    })
                    .collect(),
                ..a.clone()
            })
            .collect();
        Recipe { analyses }
    }
}

impl Analysis {
    fn parse(v: &toml::Value) -> Result<Self, RecipeError> {
        let toml::Value::Table(t) = v else {
            return Err(table("each [[decode]] is a table"));
        };
        const KEYS: [&str; 6] = ["id", "decoder", "version", "label", "inputs", "settings"];
        if let Some(k) = t.keys().find(|k| !KEYS.contains(&k.as_str())) {
            return Err(table(format!("a [[decode]] table takes no {k}")));
        }
        let text = |k: &str| match t.get(k) {
            Some(toml::Value::String(s)) => Ok(Some(s.clone())),
            None => Ok(None),
            Some(_) => Err(table(format!("{k} is a string"))),
        };
        let id = text("id")?.ok_or_else(|| table("a [[decode]] table needs an id"))?;
        let named =
            !id.is_empty() && !id.contains(|c: char| c.is_whitespace() || c == '/' || c == '\\');
        if !named {
            return Err(table(format!(
                "id {id:?} is empty or holds whitespace, / or \\"
            )));
        }
        let decoder = text("decoder")?.ok_or_else(|| table(format!("{id}: no decoder")))?;
        let version = match t.get("version") {
            Some(toml::Value::Integer(v)) => {
                u32::try_from(*v).map_err(|_| table(format!("{id}: version {v}")))?
            }
            _ => return Err(table(format!("{id}: version is a whole number"))),
        };
        let label = text("label")?;
        let mut inputs = vec![];
        match t.get("inputs") {
            Some(toml::Value::Table(ins)) => {
                for (port, v) in ins {
                    let input = match v {
                        toml::Value::String(c) => Some(Input::Channel(c.clone())),
                        toml::Value::Table(u) if u.len() == 1 => (u.get("analysis"))
                            .and_then(toml::Value::as_str)
                            .map(|up| Input::Analysis(up.to_owned())),
                        _ => None,
                    }
                    .ok_or_else(|| {
                        table(format!(
                            "{id}: input {port} is a channel's name or {{ analysis = \"<id>\" }}"
                        ))
                    })?;
                    inputs.push((port.clone(), input));
                }
            }
            None => {}
            Some(_) => return Err(table(format!("{id}: inputs is a table"))),
        }
        let mut settings = vec![];
        match t.get("settings") {
            Some(toml::Value::Table(ss)) => {
                for (name, v) in ss {
                    settings.push((name.clone(), setting(&id, name, v)?));
                }
            }
            None => {}
            Some(_) => return Err(table(format!("{id}: settings is a table"))),
        }
        Ok(Self {
            id,
            decoder,
            version,
            label,
            inputs,
            settings,
        })
    }

    /// Adds it to `h` through `registry`, reading the analyses `bound` holds by their ids; a wire
    /// decoder reads windows of `window` ticks, and the analysis keeps whole items in `keep`.
    /// Stacked on an analysis whose chain has run, it decodes the chain again (`Host::add`).
    pub fn add(
        &self,
        h: &mut Host,
        registry: &Registry,
        recording: &Arc<Recording>,
        bound: &[Bound],
        window: Tick,
        keep: &[Span],
    ) -> Result<Bound, RecipeError> {
        let analysis = || self.id.clone();
        let entry = (registry.find(&self.decoder, self.version)).ok_or_else(|| {
            RecipeError::UnknownDecoder {
                analysis: analysis(),
                decoder: self.decoder.clone(),
                version: self.version,
            }
        })?;
        let def = entry.definition.clone();
        let unknown_port = |port: &str| RecipeError::UnknownPort {
            analysis: analysis(),
            decoder: self.decoder.clone(),
            port: port.to_owned(),
        };
        let upstreams: Vec<_> = (self.inputs.iter())
            .filter_map(|(port, i)| match i {
                Input::Analysis(up) => Some((port, up)),
                Input::Channel(_) => None,
            })
            .collect();
        let (input, timeline, map) = match upstreams[..] {
            [] => {
                for (port, _) in &self.inputs {
                    if !def.ports.iter().any(|p| p.name == port.as_str()) {
                        return Err(unknown_port(port));
                    }
                }
                let mut ports = Vec::with_capacity(def.ports.len());
                for p in &def.ports {
                    let given = (self.inputs.iter()).find(|(port, _)| p.name == port.as_str());
                    ports.push(match given {
                        Some((_, Input::Channel(c))) => {
                            Some(recording.channel(c).ok_or_else(|| RecipeError::NoChannel {
                                analysis: analysis(),
                                channel: c.clone(),
                            })?)
                        }
                        _ if p.optional => None,
                        _ => {
                            return Err(RecipeError::MissingPort {
                                analysis: analysis(),
                                port: p.name.to_string(),
                            });
                        }
                    });
                }
                // A wire source refuses a binding of no channel at all.
                let timeline = ports.iter().flatten().copied().next().unwrap_or(0);
                let source =
                    WireSource::with_ports(recording.clone(), ports).map_err(|source| {
                        RecipeError::Wire {
                            analysis: analysis(),
                            source,
                        }
                    })?;
                let map = timeline_map(recording, timeline).map_err(|e| RecipeError::Wire {
                    analysis: analysis(),
                    source: BindWire::Read(e),
                })?;
                let wire = InputSpec::Wire {
                    source: Box::new(source),
                    local: None,
                };
                (wire, timeline, map)
            }
            [(port, up)] if self.inputs.len() == 1 => {
                let declared = (def.inputs.iter())
                    .any(|i| matches!(i, InputDecl::Upstream(u) if u.port == port.as_str()));
                if !declared {
                    return Err(unknown_port(port));
                }
                let up = (bound.iter().find(|b| h.name(b.id) == up.as_str())).ok_or_else(|| {
                    RecipeError::UnknownAnalysis {
                        analysis: analysis(),
                        upstream: up.clone(),
                    }
                })?;
                (InputSpec::Upstream(up.id), up.timeline, up.map)
            }
            _ => {
                return Err(RecipeError::OneUpstream {
                    analysis: analysis(),
                });
            }
        };
        let settings: Vec<(&str, Setting)> = (self.settings.iter())
            .map(|(n, s)| (n.as_str(), s.clone()))
            .collect();
        let label = (self.label.clone()).unwrap_or_else(|| entry.label.clone());
        let id = h
            .add(
                Name::Owned(self.id.clone()),
                def,
                input,
                entry.factory(window),
                &settings,
                keep.to_vec(),
            )
            .map_err(|source| RecipeError::Bind {
                analysis: analysis(),
                source,
            })?;
        Ok(Bound {
            id,
            label,
            timeline,
            map,
        })
    }
}

/// A setting's value as TOML gives it: a flag, a whole number, a choice by its name, or a list of
/// whole numbers.
fn setting(id: &str, name: &str, v: &toml::Value) -> Result<Setting, RecipeError> {
    Ok(match v {
        toml::Value::Boolean(b) => Setting::Flag(*b),
        toml::Value::Integer(i) => Setting::Int(*i),
        toml::Value::String(s) => Setting::Name(Name::Owned(s.clone())),
        toml::Value::Array(a) => Setting::List(
            (a.iter())
                .map(|x| x.as_integer())
                .collect::<Option<_>>()
                .ok_or_else(|| table(format!("{id}: setting {name} lists whole numbers only")))?,
        ),
        _ => {
            return Err(table(format!(
                "{id}: setting {name} is a flag, a whole number, a name or a list of whole numbers"
            )));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{KindDecl, Mode, PortDecl, Schema, UpstreamDecl};

    const STACK: &str = r#"
[[decode]]
id = "swd"
decoder = "swd"
version = 1
inputs = { SWCLK = "SWCLK", SWDIO = "D1" }

[[decode]]
id = "adi"
decoder = "adiv5"
version = 1
inputs = { transfers = { analysis = "swd" } }

[[decode]]
id = "mem"
decoder = "mem-ap"
version = 1
inputs = { accesses = { analysis = "adi" } }

[[decode]]
id = "jtag"
decoder = "jtag"
version = 1
inputs = { TCK = "TCK" }
"#;

    fn ids(r: &Recipe) -> Vec<&str> {
        r.analyses.iter().map(|a| a.id.as_str()).collect()
    }

    #[test]
    fn leaving_an_analysis_out_leaves_out_what_reads_it() {
        let r = Recipe::parse(STACK).unwrap();
        assert_eq!(ids(&r.without("adi")), ["swd", "jtag"]);
        assert_eq!(ids(&r.without("swd")), ["jtag"]);
        assert_eq!(ids(&r.without("mem")), ["swd", "adi", "jtag"]);
        assert_eq!(r.without("none"), r);
    }

    #[test]
    fn fresh_ids_rename_what_is_taken_and_what_reads_it() {
        let r = Recipe::parse(STACK).unwrap();
        let taken = ["swd", "swd-2", "mem"];
        let f = r.fresh(|id| taken.contains(&id));
        assert_eq!(ids(&f), ["swd-3", "adi", "mem-2", "jtag"]);
        let reads = |a: &Analysis| a.inputs[0].1.clone();
        assert_eq!(reads(&f.analyses[1]), Input::Analysis("swd-3".into()));
        assert_eq!(reads(&f.analyses[2]), Input::Analysis("adi".into()));
        assert_eq!(r.fresh(|_| false), r, "nothing taken, nothing renamed");
    }

    #[test]
    fn ports_take_the_channel_named_else_one_named_as_the_port() {
        let wire = |name: &'static str, ports: Vec<PortDecl>| {
            let def =
                Definition::new(name, 1, Schema::new(vec![KindDecl::new("k")])).with_ports(ports);
            Entry::new(Arc::new(def), name, |_| Box::new(|_, _| unreachable!()))
        };
        let mut tdo = PortDecl::new("TDO");
        tdo.optional = true;
        let adi = Definition::new("adiv5", 1, Schema::new(vec![KindDecl::new("k")]))
            .with_inputs([UpstreamDecl::new("transfers", Mode::Pieces)]);
        let registry = Registry {
            decoders: vec![
                wire("swd", vec!["SWCLK".into(), "SWDIO".into()]),
                wire("jtag", vec!["TCK".into(), "TMS".into(), tdo]),
                Entry::new(Arc::new(adi), "ADIv5", |_| Box::new(|_, _| unreachable!())),
            ],
            presets: vec![],
        };
        let channels = ["swclk", "D1", "TMS", "SWDIO"].map(String::from);
        let r = Recipe::parse(STACK)
            .unwrap()
            .on_channels(&registry, &channels);
        let channel = |a: &Analysis| -> Vec<(String, String)> {
            (a.inputs.iter())
                .map(|(p, i)| match i {
                    Input::Channel(c) => (p.clone(), c.clone()),
                    Input::Analysis(up) => (p.clone(), format!("<{up}>")),
                })
                .collect()
        };
        let pairs = |v: &[(&str, &str)]| -> Vec<(String, String)> {
            v.iter().map(|&(a, b)| (a.into(), b.into())).collect()
        };
        assert_eq!(
            channel(&r.analyses[0]),
            pairs(&[("SWCLK", "swclk"), ("SWDIO", "D1")]),
            "SWCLK is not a channel, so the port's name finds swclk; D1 is"
        );
        assert_eq!(channel(&r.analyses[1]), pairs(&[("transfers", "<swd>")]));
        assert_eq!(
            channel(&r.analyses[3]),
            pairs(&[("TMS", "TMS")]),
            "TCK and TDO left without a channel the recording has"
        );
    }
}
