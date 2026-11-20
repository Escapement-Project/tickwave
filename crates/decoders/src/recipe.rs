//! Recipes: the analyses to run over a recording, as `[[decode]]` tables name them, and the
//! registry of these decoders that binds them.
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

use crate::{adiv5, cjtag, jtag, jtagdp, swd};
use escapement_tickwave_decode::{
    AnalysisId, BindError, Definition, Factory, Host, HostOptions, InputDecl, InputSpec, Name,
    Setting, Span, Tick,
    wire::{BindWire, WireSource},
};
use escapement_tickwave_view::Recording;
use foldhash::HashMap;
use std::sync::Arc;

/// The most analyses a recipe holds.
pub const ANALYSES_AT_MOST: usize = 64;

/// A decoder the registry knows: its definition, and the factory of its runs.
pub struct Entry {
    pub definition: Arc<Definition>,
    factory: fn(Tick) -> Factory,
}

impl Entry {
    /// The factory of its runs; a wire decoder's reads windows of `window` ticks of input.
    pub fn factory(&self, window: Tick) -> Factory {
        (self.factory)(window)
    }
}

/// This crate's decoders.
pub fn registry() -> Vec<Entry> {
    vec![
        Entry {
            definition: swd::definition(),
            factory: swd::Swd::factory,
        },
        Entry {
            definition: jtag::definition(),
            factory: jtag::Jtag::factory,
        },
        Entry {
            definition: cjtag::definition(),
            factory: cjtag::Cjtag::factory,
        },
        Entry {
            definition: jtagdp::definition(),
            factory: |_| jtagdp::JtagDp::factory(),
        },
        Entry {
            definition: adiv5::adi_definition(),
            factory: |_| adiv5::Adi::factory(),
        },
        Entry {
            definition: adiv5::mem_definition(),
            factory: |_| adiv5::Mem::factory(),
        },
    ]
}

/// The decoder of this name and major version.
pub fn find(name: &str, version: u32) -> Option<Entry> {
    (registry().into_iter()).find(|e| e.definition.name == name && e.definition.version == version)
}

/// About 100 µs of `recording`'s input, in ticks: a wire decoder's window that keeps one step of
/// a host well under a millisecond.
pub fn window(recording: &Recording) -> Tick {
    let tb = recording.timebase();
    let per_s = tb.num() as f64 / tb.den() as f64;
    ((per_s * 1e-4) as Tick).max(1)
}

/// The analyses to run over a recording, each a `[[decode]]` table.
#[derive(Clone, Debug, PartialEq)]
pub struct Recipe {
    pub analyses: Vec<Analysis>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Analysis {
    pub id: String,
    pub decoder: String,
    pub version: u32,
    /// The track's name; the decoder's where none is given.
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

/// An analysis as a recipe bound it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bound {
    pub id: AnalysisId,
    /// Its track's name: the recipe's label, else the decoder's name.
    pub label: String,
    /// The channel whose source's timeline its items are on: its wire analysis's first port given
    /// a channel.
    pub timeline: usize,
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

    /// Binds its analyses over `recording`, each after those it reads, through this crate's
    /// registry; each wire decoder reads windows of `window` ticks, and each analysis keeps
    /// whole items in `keep`.
    pub fn bind(
        &self,
        recording: &Arc<Recording>,
        window: Tick,
        keep: &[Span],
        options: HostOptions,
    ) -> Result<Binding, RecipeError> {
        let mut h = Host::new(options);
        let mut bound: HashMap<&str, Bound> = HashMap::default();
        let mut analyses = Vec::with_capacity(self.analyses.len());
        let mut left: Vec<&Analysis> = self.analyses.iter().collect();
        while !left.is_empty() {
            let ready = left.iter().position(|a| {
                (a.inputs.iter()).all(|(_, i)| match i {
                    Input::Analysis(up) => bound.contains_key(up.as_str()),
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
            let b = a.bind(&mut h, recording, &bound, window, keep)?;
            analyses.push(b.clone());
            bound.insert(a.id.as_str(), b);
        }
        Ok(Binding { host: h, analyses })
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

    /// Adds it to `h`, reading the analyses `bound` holds by their ids.
    fn bind(
        &self,
        h: &mut Host,
        recording: &Arc<Recording>,
        bound: &HashMap<&str, Bound>,
        window: Tick,
        keep: &[Span],
    ) -> Result<Bound, RecipeError> {
        let analysis = || self.id.clone();
        let entry =
            find(&self.decoder, self.version).ok_or_else(|| RecipeError::UnknownDecoder {
                analysis: analysis(),
                decoder: self.decoder.clone(),
                version: self.version,
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
        let (input, timeline) = match upstreams[..] {
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
                let wire = InputSpec::Wire {
                    source: Box::new(source),
                    local: None,
                };
                (wire, timeline)
            }
            [(port, up)] if self.inputs.len() == 1 => {
                let declared = (def.inputs.iter())
                    .any(|i| matches!(i, InputDecl::Upstream(u) if u.port == port.as_str()));
                if !declared {
                    return Err(unknown_port(port));
                }
                let up = &bound[up.as_str()];
                (InputSpec::Upstream(up.id), up.timeline)
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
        let label = (self.label.clone()).unwrap_or_else(|| def.name.to_string());
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
