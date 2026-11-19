//! What a decoder declares: its name and versions, and the schema its output keeps to. A schema is
//! data: a decoder built into tickwave declares one statically through borrowed names, and one
//! loaded at run time hands one over owned. The host checks it when the decoder is added, then
//! every operation against it.

use crate::Mode;
use crate::output::{FieldId, Id, KindId, Op, Ref, Status, Value};
use std::{borrow::Cow, collections::HashSet, fmt};

pub type Name = Cow<'static, str>;

/// A version of the decoder contract. A host whose contract is `M.h` runs a decoder that targets
/// `M.m` for every `m <= h`: a minor version only adds to what a decoder may use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Contract {
    pub major: u16,
    pub minor: u16,
}

/// The contract this host implements.
pub const CONTRACT: Contract = Contract { major: 1, minor: 0 };

impl Contract {
    /// Whether a host of this contract runs a decoder that targets `wanted`.
    pub fn runs(self, wanted: Contract) -> bool {
        wanted.major == self.major && wanted.minor <= self.minor
    }
}

impl fmt::Display for Contract {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// A decoder as the host adds it.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Definition {
    /// The decoder's name, which a recipe names.
    pub name: Name,
    /// Its major version, which is its output schema's: kinds and fields keep their meaning within
    /// it.
    pub version: u32,
    /// The decoder contract it targets.
    pub contract: Contract,
    /// The inputs it takes, each a binding may pick: the wire, or another analysis's output. The
    /// decoder's factory is told which one its binding picked (`Bound::input`).
    pub inputs: Vec<InputDecl>,
    /// The wire ports it reads through its wire input: a window's port numbers index this list,
    /// and a binding names a channel for each, or leaves out one declared optional.
    pub ports: Vec<PortDecl>,
    /// What a binding may set, each with its default. The host hands a run's decoder the values
    /// it bound, and keys checkpoints on them.
    pub settings: Vec<SettingDecl>,
    pub schema: Schema,
}

/// An input a decoder takes.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum InputDecl {
    /// The wire, through the definition's ports.
    Wire,
    /// Another analysis's output.
    Upstream(UpstreamDecl),
}

/// Another analysis's output as an input: the analyses that may give it, by their definition's
/// name, and the kinds it reads of them, delivered in one mode. A pieces input names the
/// sequences it reads; others are not delivered.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct UpstreamDecl {
    /// The input's name, which a recipe binds an analysis to. Alternatives may share one.
    pub port: Name,
    /// The definitions whose analyses it takes; any analysis's where empty.
    pub from: Vec<Name>,
    pub kinds: Vec<Name>,
    pub mode: Mode,
    pub sequences: Vec<Name>,
}

impl UpstreamDecl {
    /// An input named `port` taking any analysis's output in `mode`, no kinds yet.
    pub fn new(port: impl Into<Name>, mode: Mode) -> Self {
        Self {
            port: port.into(),
            from: vec![],
            kinds: vec![],
            mode,
            sequences: vec![],
        }
    }

    /// Takes only analyses of these definitions.
    pub fn from<N: Into<Name>>(mut self, definitions: impl IntoIterator<Item = N>) -> Self {
        self.from = names(definitions);
        self
    }

    pub fn with_kinds<N: Into<Name>>(mut self, kinds: impl IntoIterator<Item = N>) -> Self {
        self.kinds = names(kinds);
        self
    }

    pub fn with_sequences<N: Into<Name>>(mut self, sequences: impl IntoIterator<Item = N>) -> Self {
        self.sequences = names(sequences);
        self
    }
}

impl From<UpstreamDecl> for InputDecl {
    fn from(u: UpstreamDecl) -> Self {
        InputDecl::Upstream(u)
    }
}

/// A wire port, by name. A binding may leave out an optional one, and the decoder is told which
/// ports it has.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct PortDecl {
    pub name: Name,
    pub optional: bool,
}

/// A setting, which a recipe gives by name. A binding that leaves it out takes its default.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct SettingDecl {
    pub name: Name,
    pub ty: SettingType,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SettingType {
    Flag {
        default: bool,
    },
    /// A whole number from `min` to `max`.
    Int {
        min: i64,
        max: i64,
        default: i64,
    },
    /// One of `names`, given by name; `default` indexes them.
    Choice {
        names: Vec<Name>,
        default: usize,
    },
    /// Whole numbers, each from `min` to `max`, at most `at_most` of them.
    List {
        min: i64,
        max: i64,
        at_most: usize,
        default: Vec<i64>,
    },
}

/// A setting's value as a binding gives it, as a recipe's TOML holds it: a choice by its name.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Setting {
    Flag(bool),
    Int(i64),
    Name(Name),
    List(Vec<i64>),
}

/// A setting, by its index in the definition's settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SettingId(pub u16);

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FieldType {
    /// A spanned part with no value, such as a turnaround.
    Phase,
    Bool,
    Word {
        bits: u8,
    },
    Name(Vec<Name>),
    Text {
        max: usize,
    },
    Bits {
        max: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ElementType {
    Bit,
    /// An index into a declared list of at most 256 names, such as TAP states.
    Name(Vec<Name>),
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct FieldDecl {
    pub name: Name,
    pub ty: FieldType,
    /// What a part too narrow for the field's name shows; None for the name's first three
    /// letters.
    pub short: Option<Name>,
}

/// A piece of a label: text, or one of the item's fields, by its index in its kind's fields.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LabelPiece {
    Text(Name),
    Field(FieldId),
}

/// A label over an item's typed values. A field the item does not hold is left out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Label(pub Vec<LabelPiece>);

impl Label {
    pub fn text(mut self, t: impl Into<Name>) -> Self {
        self.0.push(LabelPiece::Text(t.into()));
        self
    }

    pub fn field(mut self, f: FieldId) -> Self {
        self.0.push(LabelPiece::Field(f));
        self
    }
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct SeqDecl {
    pub name: Name,
    pub element: ElementType,
}

/// A kind a link may point to, by name: one of this analysis's, or of the analysis it is stacked
/// on.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LinkTo {
    Own(Name),
    Upstream(Name),
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct LinkDecl {
    pub role: Name,
    pub to: Vec<LinkTo>,
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct KindDecl {
    pub name: Name,
    pub parents: Vec<Name>,
    pub fields: Vec<FieldDecl>,
    pub sequences: Vec<SeqDecl>,
    pub links: Vec<LinkDecl>,
    /// How many of its items may overlap, each drawn in a lane of its own, 1 to `MAX_LANES`.
    pub lanes: usize,
    /// The row its items draw in, below `MAX_LANES`. A track stacks its rows in order of their
    /// numbers, each as many lanes high as its kinds' most, and only kinds of one row share
    /// lanes. A child kind goes in a row under its parents', since its items lie inside theirs.
    pub row: usize,
    /// Its labels, fullest first: a view shows the first that fits an item, else none.
    pub labels: Vec<Label>,
    /// The field whose value reads as text, a byte as a word of 8 bits or bytes as bits: a view
    /// joins an analysis's text into lines, each character from its item.
    pub text: Option<FieldId>,
}

/// What an analysis may emit.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Schema {
    pub kinds: Vec<KindDecl>,
    /// Driver roles, in phase 7d's sense; a spanned field names one.
    pub roles: Vec<Name>,
    /// Why a value may be unknown.
    pub reasons: Vec<Name>,
    /// What an unknown value may need to be known.
    pub needs: Vec<Name>,
    /// Why an item may close incomplete.
    pub causes: Vec<Name>,
    /// The most items the analysis holds open at once, its declared bound on protocol state. The
    /// host refuses a batch that leaves more open.
    pub open_at_most: usize,
}

fn names<N: Into<Name>>(list: impl IntoIterator<Item = N>) -> Vec<Name> {
    list.into_iter().map(Into::into).collect()
}

impl Definition {
    /// A decoder of the host's own contract that reads the wire, with no ports yet.
    pub fn new(name: impl Into<Name>, version: u32, schema: Schema) -> Self {
        Self {
            name: name.into(),
            version,
            contract: CONTRACT,
            inputs: vec![InputDecl::Wire],
            ports: vec![],
            settings: vec![],
            schema,
        }
    }

    pub fn with_contract(mut self, contract: Contract) -> Self {
        self.contract = contract;
        self
    }

    /// Its ports, each one a binding must give a channel.
    pub fn with_ports<N: Into<Name>>(mut self, ports: impl IntoIterator<Item = N>) -> Self {
        self.ports = (names(ports).into_iter())
            .map(|name| PortDecl {
                name,
                optional: false,
            })
            .collect();
        self
    }

    /// Adds ports after those it has, each one a binding may leave out.
    pub fn with_optional_ports<N: Into<Name>>(
        mut self,
        ports: impl IntoIterator<Item = N>,
    ) -> Self {
        let optional = names(ports).into_iter().map(|name| PortDecl {
            name,
            optional: true,
        });
        self.ports.extend(optional);
        self
    }

    pub fn with_settings(mut self, settings: Vec<SettingDecl>) -> Self {
        self.settings = settings;
        self
    }

    /// Its inputs, in place of the wire alone.
    pub fn with_inputs<I: Into<InputDecl>>(mut self, inputs: impl IntoIterator<Item = I>) -> Self {
        self.inputs = inputs.into_iter().map(Into::into).collect();
        self
    }

    /// The input an analysis of definition `upstream` binds to: the first that names it, else the
    /// first that takes any.
    pub fn upstream_input(&self, upstream: &str) -> Option<(usize, &UpstreamDecl)> {
        let ups = || {
            (self.inputs.iter().enumerate()).filter_map(|(k, i)| match i {
                InputDecl::Upstream(u) => Some((k, u)),
                InputDecl::Wire => None,
            })
        };
        (ups().find(|(_, u)| u.from.iter().any(|f| f == upstream)))
            .or_else(|| ups().find(|(_, u)| u.from.is_empty()))
    }

    /// The index of its wire input, if it takes the wire.
    pub fn wire_input(&self) -> Option<usize> {
        self.inputs.iter().position(|i| *i == InputDecl::Wire)
    }

    /// Checks its inputs, ports and settings: their names, none twice, no upstream two inputs
    /// take, and each default among the values its setting takes.
    pub(crate) fn check(&self) -> Result<(), SchemaError> {
        at_most(self.inputs.len(), MAX_LIST, "inputs")?;
        if self.inputs.is_empty() {
            return Err(SchemaError::NoInputs);
        }
        // Alternatives may share a port name, so that one binding reads whichever upstream it is
        // given; but a definition two inputs take would leave the second unreachable.
        let (mut wires, mut any) = (0, 0);
        let mut taken = HashSet::new();
        for i in &self.inputs {
            let InputDecl::Upstream(u) = i else {
                wires += 1;
                continue;
            };
            named(&u.port)?;
            at_most(u.from.len(), MAX_LIST, "definitions an input takes")?;
            at_most(u.kinds.len(), MAX_LIST, "kinds an input reads")?;
            at_most(u.sequences.len(), MAX_LIST, "sequences an input reads")?;
            for n in u.from.iter().chain(&u.kinds).chain(&u.sequences) {
                named(n)?;
            }
            any += usize::from(u.from.is_empty());
            if let Some(f) = u.from.iter().find(|f| !taken.insert(*f)) {
                return Err(SchemaError::TakenTwice(f.clone()));
            }
        }
        if wires > 1 {
            return Err(SchemaError::WireTwice);
        }
        if any > 1 {
            return Err(SchemaError::AnyTwice);
        }
        let mut seen = HashSet::new();
        for p in &self.ports {
            named(&p.name)?;
            if !seen.insert(&p.name) {
                return Err(SchemaError::DuplicatePort(p.name.clone()));
            }
        }
        at_most(self.settings.len(), MAX_LIST, "settings")?;
        let mut seen = HashSet::new();
        for s in &self.settings {
            named(&s.name)?;
            if !seen.insert(&s.name) {
                return Err(SchemaError::DuplicateSetting(s.name.clone()));
            }
            let fits = match &s.ty {
                SettingType::Flag { .. } => true,
                SettingType::Int { min, max, default } => (min..=max).contains(&default),
                SettingType::Choice { names, default } => {
                    at_most(names.len(), MAX_LIST, "choices")?;
                    let mut choices = HashSet::new();
                    for n in names {
                        named(n)?;
                        if !choices.insert(n) {
                            return Err(SchemaError::DuplicateChoice {
                                setting: s.name.clone(),
                                name: n.clone(),
                            });
                        }
                    }
                    *default < names.len()
                }
                SettingType::List {
                    min,
                    max,
                    at_most: most,
                    default,
                } => {
                    at_most(*most, MAX_LIST, "list entries")?;
                    default.len() <= *most && default.iter().all(|d| (min..=max).contains(&d))
                }
            };
            if !fits {
                return Err(SchemaError::SettingDefault(s.name.clone()));
            }
        }
        Ok(())
    }
}

impl SettingDecl {
    pub fn flag(name: impl Into<Name>, default: bool) -> Self {
        Self {
            name: name.into(),
            ty: SettingType::Flag { default },
        }
    }

    pub fn int(name: impl Into<Name>, range: std::ops::RangeInclusive<i64>, default: i64) -> Self {
        Self {
            name: name.into(),
            ty: SettingType::Int {
                min: *range.start(),
                max: *range.end(),
                default,
            },
        }
    }

    /// Whole numbers in `range`, at most `at_most` of them; `default` when a binding leaves it out.
    pub fn list(
        name: impl Into<Name>,
        range: std::ops::RangeInclusive<i64>,
        at_most: usize,
        default: Vec<i64>,
    ) -> Self {
        Self {
            name: name.into(),
            ty: SettingType::List {
                min: *range.start(),
                max: *range.end(),
                at_most,
                default,
            },
        }
    }

    pub fn choice<N: Into<Name>>(
        name: impl Into<Name>,
        choices: impl IntoIterator<Item = N>,
        default: usize,
    ) -> Self {
        Self {
            name: name.into(),
            ty: SettingType::Choice {
                names: names(choices),
                default,
            },
        }
    }
}

impl Schema {
    /// A schema of these kinds, with no roles, reasons, needs or causes, holding one item open.
    pub fn new(kinds: Vec<KindDecl>) -> Self {
        Self {
            kinds,
            roles: vec![],
            reasons: vec![],
            needs: vec![],
            causes: vec![],
            open_at_most: 1,
        }
    }

    pub fn with_roles<N: Into<Name>>(mut self, roles: impl IntoIterator<Item = N>) -> Self {
        self.roles = names(roles);
        self
    }

    pub fn with_reasons<N: Into<Name>>(mut self, reasons: impl IntoIterator<Item = N>) -> Self {
        self.reasons = names(reasons);
        self
    }

    pub fn with_needs<N: Into<Name>>(mut self, needs: impl IntoIterator<Item = N>) -> Self {
        self.needs = names(needs);
        self
    }

    pub fn with_causes<N: Into<Name>>(mut self, causes: impl IntoIterator<Item = N>) -> Self {
        self.causes = names(causes);
        self
    }

    pub fn with_open_at_most(mut self, n: usize) -> Self {
        self.open_at_most = n;
        self
    }
}

impl KindDecl {
    /// A kind with no parents, fields, sequences or links.
    pub fn new(name: impl Into<Name>) -> Self {
        Self {
            name: name.into(),
            parents: vec![],
            fields: vec![],
            sequences: vec![],
            links: vec![],
            lanes: 1,
            row: 0,
            labels: vec![],
            text: None,
        }
    }

    pub fn with_lanes(mut self, lanes: usize) -> Self {
        self.lanes = lanes;
        self
    }

    pub fn with_row(mut self, row: usize) -> Self {
        self.row = row;
        self
    }

    pub fn with_parents<N: Into<Name>>(mut self, parents: impl IntoIterator<Item = N>) -> Self {
        self.parents = names(parents);
        self
    }

    pub fn with_fields(mut self, fields: Vec<FieldDecl>) -> Self {
        self.fields = fields;
        self
    }

    pub fn with_sequences(mut self, sequences: Vec<SeqDecl>) -> Self {
        self.sequences = sequences;
        self
    }

    pub fn with_links(mut self, links: Vec<LinkDecl>) -> Self {
        self.links = links;
        self
    }

    pub fn with_labels(mut self, labels: Vec<Label>) -> Self {
        self.labels = labels;
        self
    }

    pub fn with_text(mut self, field: FieldId) -> Self {
        self.text = Some(field);
        self
    }
}

impl FieldDecl {
    pub fn new(name: impl Into<Name>, ty: FieldType) -> Self {
        Self {
            name: name.into(),
            ty,
            short: None,
        }
    }

    pub fn with_short(mut self, short: impl Into<Name>) -> Self {
        self.short = Some(short.into());
        self
    }
}

impl SeqDecl {
    pub fn new(name: impl Into<Name>, element: ElementType) -> Self {
        Self {
            name: name.into(),
            element,
        }
    }
}

impl LinkDecl {
    pub fn new(role: impl Into<Name>, to: Vec<LinkTo>) -> Self {
        Self {
            role: role.into(),
            to,
        }
    }
}

pub const fn name(s: &'static str) -> Name {
    Cow::Borrowed(s)
}
pub const fn field(name: &'static str, ty: FieldType) -> FieldDecl {
    FieldDecl {
        name: Cow::Borrowed(name),
        ty,
        short: None,
    }
}
pub const fn seq(name: &'static str, element: ElementType) -> SeqDecl {
    SeqDecl {
        name: Cow::Borrowed(name),
        element,
    }
}

/// Bounds on a schema, so one handed over as data cannot make the host's checks unbounded.
pub const MAX_KINDS: usize = 256;
pub const MAX_SEQUENCES: usize = 16;
/// The most lanes a kind may declare, and the most a schema's rows may take together.
pub const MAX_LANES: usize = 8;
pub const MAX_LINK_ROLES: usize = 16;
pub const MAX_LINK_TARGETS: usize = 16;
pub const MAX_ROLES: usize = 64;
pub const MAX_LIST: usize = 256;
/// The most labels a kind declares, and the most pieces in one.
pub const MAX_LABELS: usize = 8;
pub const MAX_LABEL_PIECES: usize = 32;
pub const MAX_FIELD_NAMES: usize = 4096;
pub const MAX_NAME: usize = 64;
/// The longest reason, need or cause: display text a reader is shown, not an identifier.
pub const MAX_PHRASE: usize = crate::MAX_TEXT;

/// A schema that cannot be loaded.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SchemaError {
    #[error("kind {kind}: {name} is declared twice")]
    Duplicate { kind: Name, name: Name },
    #[error("kind {kind}: no kind {parent} to be its parent")]
    UnknownParent { kind: Name, parent: Name },
    #[error("kind {kind}: {name} declares no names")]
    EmptyNames { kind: Name, name: Name },
    #[error("kind {kind}: {name} is past the host's bounds")]
    OverBound { kind: Name, name: Name },
    #[error("too many {what}")]
    TooMany { what: &'static str },
    #[error("a name is empty or longer than {MAX_NAME} bytes")]
    BadName,
    #[error("a reason, need or cause is empty or longer than {MAX_PHRASE} bytes")]
    BadPhrase,
    #[error("kind {kind}: link {role} points to no kind {to}")]
    UnknownLinkTarget { kind: Name, role: Name, to: Name },
    #[error("the open-item bound {0} is not between 1 and the host's")]
    OpenBound(usize),
    #[error("kind {kind}: {lanes} lanes, not between 1 and {MAX_LANES}")]
    Lanes { kind: Name, lanes: usize },
    #[error("kind {kind}: row {row}, not below {MAX_LANES}")]
    Row { kind: Name, row: usize },
    #[error("its rows take {0} lanes together, more than {MAX_LANES}")]
    RowLanes(usize),
    #[error("role {0} is declared twice")]
    DuplicateRole(Name),
    #[error("port {0} is declared twice")]
    DuplicatePort(Name),
    #[error("it declares no inputs")]
    NoInputs,
    #[error("two inputs take {0}")]
    TakenTwice(Name),
    #[error("two inputs take any analysis")]
    AnyTwice,
    #[error("the wire input is declared twice")]
    WireTwice,
    #[error("setting {0} is declared twice")]
    DuplicateSetting(Name),
    #[error("setting {setting}: {name} is declared twice")]
    DuplicateChoice { setting: Name, name: Name },
    #[error("setting {0}'s default is not a value it takes")]
    SettingDefault(Name),
    #[error("kind {0}: a label shows a field it does not declare")]
    LabelField(Name),
    #[error("kind {0}: its text is not a field of 8-bit words or of bits it declares")]
    TextField(Name),
}

/// An operation its analysis's schema does not allow.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Mismatch {
    #[error("no kind {0:?}")]
    Kind(KindId),
    #[error("{kind:?} cannot have a {parent:?} for parent")]
    Parent { kind: KindId, parent: KindId },
    #[error("{kind:?} has no field {field}")]
    Field { kind: KindId, field: u16 },
    #[error("{kind:?}'s field {field} does not take that value")]
    Type { kind: KindId, field: u16 },
    #[error("{kind:?} has no sequence {seq}")]
    Sequence { kind: KindId, seq: u16 },
    #[error("{kind:?}'s sequence {seq} has no element {value}")]
    Element { kind: KindId, seq: u16, value: u8 },
    #[error("no role {0}")]
    Role(u16),
    #[error("no reason {0}")]
    Reason(u16),
    #[error("no need {0}")]
    Need(u16),
    #[error("no cause {0}")]
    Cause(u16),
    #[error("{kind:?} has no link {link}")]
    Link { kind: KindId, link: u16 },
    #[error("{kind:?}'s link {link} cannot point to {to:?}")]
    LinkTarget { kind: KindId, link: u16, to: Ref },
    /// A phase, and any value with a driver, needs a span.
    #[error("{kind:?}'s field {field} needs a span")]
    Unspanned { kind: KindId, field: u16 },
}

fn named(n: &Name) -> Result<(), SchemaError> {
    if n.is_empty() || n.len() > MAX_NAME {
        return Err(SchemaError::BadName);
    }
    Ok(())
}

/// Checks the names a value or element of kind `kind`'s `name` indexes: some, at most `most`,
/// each a name, none twice, since a recipe or citation matches them by name.
fn name_list(list: &[Name], most: usize, kind: &Name, name: &Name) -> Result<(), SchemaError> {
    let err = |e: fn(Name, Name) -> SchemaError| e(kind.clone(), name.clone());
    if list.is_empty() {
        return Err(err(|kind, name| SchemaError::EmptyNames { kind, name }));
    }
    if list.len() > most {
        return Err(err(|kind, name| SchemaError::OverBound { kind, name }));
    }
    let mut seen = HashSet::new();
    for n in list {
        named(n)?;
        if !seen.insert(n) {
            return Err(SchemaError::Duplicate {
                kind: kind.clone(),
                name: n.clone(),
            });
        }
    }
    Ok(())
}

fn at_most(len: usize, most: usize, what: &'static str) -> Result<(), SchemaError> {
    if len > most {
        return Err(SchemaError::TooMany { what });
    }
    Ok(())
}

/// A schema's names resolved to indices, for checking operations without comparing strings.
#[derive(Clone, Debug, Default)]
pub(crate) struct Resolved {
    /// Each kind's allowed parents.
    parents: Vec<Vec<KindId>>,
    /// Each kind's link roles, each with the kinds it may point to: (upstream, kind).
    links: Vec<Vec<Vec<(bool, KindId)>>>,
}

impl Schema {
    pub fn kind(&self, k: KindId) -> Option<&KindDecl> {
        self.kinds.get(usize::from(k.0))
    }

    /// Where its kinds' items draw: the lanes its rows take together, and the range of them each
    /// kind's row takes, by kind. A row is as many lanes as the most any of its kinds declares.
    pub fn layout(&self) -> (usize, Vec<std::ops::Range<usize>>) {
        let lanes = |k: &KindDecl| k.lanes.clamp(1, MAX_LANES);
        let mut rows: Vec<(usize, usize)> = vec![];
        for k in &self.kinds {
            match rows.iter_mut().find(|(r, _)| *r == k.row) {
                Some((_, n)) => *n = (*n).max(lanes(k)),
                None => rows.push((k.row, lanes(k))),
            }
        }
        rows.sort_unstable();
        let mut at = 0;
        let mut starts = Vec::with_capacity(rows.len());
        for (row, n) in rows {
            starts.push((row, at..at + n));
            at += n;
        }
        let of = |k: &KindDecl| {
            (starts.iter())
                .find(|(r, _)| *r == k.row)
                .map_or(0..1, |(_, l)| l.clone())
        };
        (at.max(1), self.kinds.iter().map(of).collect())
    }

    pub fn kind_id(&self, name: &str) -> Option<KindId> {
        let k = self.kinds.iter().position(|k| k.name == name)?;
        Some(KindId(u16::try_from(k).ok()?))
    }

    /// Checks the schema on its own: unique names, bounds within the host's, and parents and links
    /// to its own kinds that resolve. `upstream` resolves links to the analysis it is stacked on;
    /// `open_most` is the host's bound on open items.
    pub(crate) fn resolve(
        &self,
        upstream: Option<&Schema>,
        open_most: usize,
    ) -> Result<Resolved, SchemaError> {
        at_most(self.kinds.len(), MAX_KINDS, "kinds")?;
        at_most(self.roles.len(), MAX_ROLES, "roles")?;
        at_most(self.reasons.len(), MAX_LIST, "reasons")?;
        at_most(self.needs.len(), MAX_LIST, "needs")?;
        at_most(self.causes.len(), MAX_LIST, "causes")?;
        if self.open_at_most == 0 || self.open_at_most > open_most {
            return Err(SchemaError::OpenBound(self.open_at_most));
        }
        let mut roles = HashSet::new();
        for n in self.roles.iter() {
            named(n)?;
            if !roles.insert(n) {
                return Err(SchemaError::DuplicateRole(n.clone()));
            }
        }
        for n in self.reasons.iter().chain(&self.needs).chain(&self.causes) {
            if n.is_empty() || n.len() > MAX_PHRASE {
                return Err(SchemaError::BadPhrase);
            }
        }
        let mut kinds = HashSet::new();
        for k in self.kinds.iter() {
            named(&k.name)?;
            if k.lanes == 0 || k.lanes > MAX_LANES {
                return Err(SchemaError::Lanes {
                    kind: k.name.clone(),
                    lanes: k.lanes,
                });
            }
            if k.row >= MAX_LANES {
                return Err(SchemaError::Row {
                    kind: k.name.clone(),
                    row: k.row,
                });
            }
            if !kinds.insert(&k.name) {
                return Err(SchemaError::Duplicate {
                    kind: k.name.clone(),
                    name: k.name.clone(),
                });
            }
        }
        let (lanes, _) = self.layout();
        if lanes > MAX_LANES {
            return Err(SchemaError::RowLanes(lanes));
        }
        let mut out = Resolved::default();
        for k in self.kinds.iter() {
            let dup = |name: &Name| SchemaError::Duplicate {
                kind: k.name.clone(),
                name: name.clone(),
            };
            let over = |name: &Name| SchemaError::OverBound {
                kind: k.name.clone(),
                name: name.clone(),
            };
            at_most(k.fields.len(), crate::MAX_FIELDS, "fields")?;
            at_most(k.sequences.len(), MAX_SEQUENCES, "sequences")?;
            at_most(k.links.len(), MAX_LINK_ROLES, "link roles")?;
            at_most(k.parents.len(), MAX_KINDS, "parents")?;
            let mut names = HashSet::new();
            for f in k.fields.iter() {
                named(&f.name)?;
                if let Some(short) = &f.short {
                    named(short)?;
                }
                if !names.insert(&f.name) {
                    return Err(dup(&f.name));
                }
                match &f.ty {
                    FieldType::Name(list) => name_list(list, MAX_FIELD_NAMES, &k.name, &f.name)?,
                    FieldType::Word { bits } if *bits == 0 || *bits > 64 => {
                        return Err(over(&f.name));
                    }
                    FieldType::Text { max } if *max > crate::MAX_TEXT => {
                        return Err(over(&f.name));
                    }
                    FieldType::Bits { max } if *max > crate::MAX_BITS => {
                        return Err(over(&f.name));
                    }
                    _ => {}
                }
            }
            at_most(k.labels.len(), MAX_LABELS, "labels")?;
            for l in &k.labels {
                at_most(l.0.len(), MAX_LABEL_PIECES, "label pieces")?;
                for piece in &l.0 {
                    match piece {
                        LabelPiece::Text(t) => named(t)?,
                        LabelPiece::Field(f) if usize::from(f.0) >= k.fields.len() => {
                            return Err(SchemaError::LabelField(k.name.clone()));
                        }
                        LabelPiece::Field(_) => {}
                    }
                }
            }
            if let Some(f) = k.text {
                match k.fields.get(usize::from(f.0)).map(|d| &d.ty) {
                    Some(FieldType::Word { bits: 8 } | FieldType::Bits { .. }) => {}
                    _ => return Err(SchemaError::TextField(k.name.clone())),
                }
            }
            // A sequence may share its name with a field: a part and its bits.
            let mut seqs = HashSet::new();
            for s in k.sequences.iter() {
                named(&s.name)?;
                if !seqs.insert(&s.name) {
                    return Err(dup(&s.name));
                }
                if let ElementType::Name(list) = &s.element {
                    name_list(list, MAX_LIST, &k.name, &s.name)?;
                }
            }
            let mut parents = vec![];
            for p in k.parents.iter() {
                let id = self.kind_id(p).ok_or_else(|| SchemaError::UnknownParent {
                    kind: k.name.clone(),
                    parent: p.clone(),
                })?;
                parents.push(id);
            }
            let mut roles = HashSet::new();
            let mut links = vec![];
            for l in k.links.iter() {
                named(&l.role)?;
                if !roles.insert(&l.role) {
                    return Err(dup(&l.role));
                }
                at_most(l.to.len(), MAX_LINK_TARGETS, "link targets")?;
                let mut to = vec![];
                for t in l.to.iter() {
                    let (up, kind) = match t {
                        LinkTo::Own(n) => (false, self.kind_id(n).ok_or(n)),
                        LinkTo::Upstream(n) => (true, upstream.and_then(|u| u.kind_id(n)).ok_or(n)),
                    };
                    let kind = kind.map_err(|n| SchemaError::UnknownLinkTarget {
                        kind: k.name.clone(),
                        role: l.role.clone(),
                        to: n.clone(),
                    })?;
                    to.push((up, kind));
                }
                links.push(to);
            }
            out.parents.push(parents);
            out.links.push(links);
        }
        Ok(out)
    }

    fn check_value(kind: &KindDecl, field: u16, value: &Value) -> bool {
        let Some(f) = kind.fields.get(usize::from(field)) else {
            return false;
        };
        match (value, &f.ty) {
            (Value::Unknown { .. }, _) => true,
            (Value::Phase, FieldType::Phase) | (Value::Bool(_), FieldType::Bool) => true,
            (Value::Word { value, bits }, FieldType::Word { bits: b }) => {
                *bits == *b && (*b >= 64 || value >> b == 0)
            }
            (Value::Name(n), FieldType::Name(list)) => usize::from(*n) < list.len(),
            (Value::Text(t), FieldType::Text { max }) => t.len() <= *max,
            (Value::Bits { bits, .. }, FieldType::Bits { max }) => bits <= max,
            _ => false,
        }
    }

    /// Checks one operation against the schema. It needs nothing but the operation: the kind
    /// travels in the item's id.
    pub(crate) fn check(&self, r: &Resolved, op: &Op) -> Result<(), Mismatch> {
        let id: &Id = op.id();
        let k = id.kind;
        let kind = self.kind(k).ok_or(Mismatch::Kind(k))?;
        let ki = usize::from(k.0);
        match op {
            Op::Open { parent, .. } => {
                if let Some(p) = parent
                    && !r.parents.get(ki).is_some_and(|ps| ps.contains(&p.kind))
                {
                    return Err(Mismatch::Parent {
                        kind: k,
                        parent: p.kind,
                    });
                }
            }
            Op::Set {
                field,
                value,
                span,
                driver,
                ..
            } => {
                let f = field.0;
                if usize::from(f) >= kind.fields.len() {
                    return Err(Mismatch::Field { kind: k, field: f });
                }
                if !Self::check_value(kind, f, value) {
                    return Err(Mismatch::Type { kind: k, field: f });
                }
                if let Value::Unknown { reason, needs } = value {
                    if usize::from(reason.0) >= self.reasons.len() {
                        return Err(Mismatch::Reason(reason.0));
                    }
                    if let Some(n) = needs
                        && usize::from(n.0) >= self.needs.len()
                    {
                        return Err(Mismatch::Need(n.0));
                    }
                }
                if (matches!(value, Value::Phase) || driver.is_some()) && span.is_none() {
                    return Err(Mismatch::Unspanned { kind: k, field: f });
                }
                if let Some(role) = driver
                    && usize::from(role.0) >= self.roles.len()
                {
                    return Err(Mismatch::Role(role.0));
                }
            }
            Op::Extend { seq, elements, .. } => {
                let s = kind
                    .sequences
                    .get(usize::from(seq.0))
                    .ok_or(Mismatch::Sequence {
                        kind: k,
                        seq: seq.0,
                    })?;
                let most = match &s.element {
                    ElementType::Bit => 1,
                    ElementType::Name(list) => list.len().saturating_sub(1),
                };
                if let Some(e) = elements.iter().find(|e| usize::from(e.value) > most) {
                    return Err(Mismatch::Element {
                        kind: k,
                        seq: seq.0,
                        value: e.value,
                    });
                }
            }
            Op::Link { link, other, .. } => {
                let to = r
                    .links
                    .get(ki)
                    .and_then(|ls| ls.get(usize::from(link.0)))
                    .ok_or(Mismatch::Link {
                        kind: k,
                        link: link.0,
                    })?;
                let target = match other {
                    Ref::Own(o) => (false, o.kind),
                    Ref::Upstream(o) => (true, o.kind),
                };
                if !to.contains(&target) {
                    return Err(Mismatch::LinkTarget {
                        kind: k,
                        link: link.0,
                        to: *other,
                    });
                }
            }
            Op::Close { status, .. } => {
                if let Status::Incomplete(c) = status
                    && usize::from(c.0) >= self.causes.len()
                {
                    return Err(Mismatch::Cause(c.0));
                }
            }
        }
        Ok(())
    }
}
