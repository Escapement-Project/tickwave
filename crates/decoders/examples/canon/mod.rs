//! The rendering `swd_canon` and `jtag_canon` share: every item of an analysis by name, sorted by
//! anchor, kind and discriminator, hashed with FNV-1a.

#![allow(dead_code)]

use escapement_tickwave_decode::{
    AnalysisId, Definition, FieldType, Host, Id, Item, Observation, Ref, Status, Tick, Value,
};

pub fn fnv(lines: &[String]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for l in lines {
        for &b in l.as_bytes().iter().chain(b"\n") {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100_0000_01b3);
        }
    }
    h
}

/// Renders an analysis's items by name, an id as `track/kind/anchor.disc`.
pub struct Names<'a> {
    pub track: &'a str,
    pub def: &'a Definition,
    pub up: Option<(&'a str, &'a Definition)>,
}

impl Names<'_> {
    fn id(&self, track: &str, def: &Definition, i: &Id) -> String {
        let kind = def.schema.kind(i.kind).map_or("?", |k| &k.name);
        format!("{track}/{kind}/{}.{}", i.anchor, i.disc)
    }

    fn own(&self, i: &Id) -> String {
        self.id(self.track, self.def, i)
    }

    fn other(&self, r: &Ref) -> String {
        match r {
            Ref::Own(i) => self.own(i),
            Ref::Upstream(i) => match self.up {
                Some((t, d)) => self.id(t, d, i),
                None => "?".into(),
            },
            _ => "?".into(),
        }
    }

    fn value(&self, ty: Option<&FieldType>, v: &Value) -> String {
        let s = &self.def.schema;
        let at = |list: &[escapement_tickwave_decode::Name], n: u16| {
            list.get(usize::from(n))
                .map_or("?".to_string(), |x| x.to_string())
        };
        match v {
            Value::Phase => "phase".into(),
            Value::Bool(b) => b.to_string(),
            Value::Word { value, bits } => format!("{value}/{bits}"),
            Value::Name(n) => match ty {
                Some(FieldType::Name(list)) => at(list, *n),
                _ => "?".into(),
            },
            Value::Text(t) => format!("t:{t}"),
            Value::Bits { bytes, bits } => {
                let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
                format!("b:{bits}:{hex}")
            }
            Value::Unknown { reason, needs } => format!(
                "?{}|{}",
                at(&s.reasons, reason.0),
                needs.map_or("-".into(), |n| at(&s.needs, n.0))
            ),
            _ => "?".into(),
        }
    }

    pub fn render(&self, i: &Item) -> String {
        let s = &self.def.schema;
        let kind = s.kind(i.id.kind);
        let status = match &i.status {
            None => "open".to_string(),
            Some(Status::Complete) => "complete".into(),
            Some(Status::Incomplete(c)) => format!(
                "incomplete:{}",
                s.causes.get(usize::from(c.0)).map_or("?", |x| x)
            ),
            Some(_) => "?".into(),
        };
        let mut out = format!(
            "{} parent={} start={} end={} status={status}",
            self.own(&i.id),
            i.parent.as_ref().map_or("-".into(), |p| self.own(p)),
            i.start,
            i.end.map_or("-".into(), |e| e.to_string()),
        );
        for f in &i.fields {
            let decl = kind.and_then(|k| k.fields.get(usize::from(f.field.0)));
            out += &format!(
                " {}={}{}{}",
                decl.map_or("?", |d| &d.name),
                self.value(decl.map(|d| &d.ty), &f.value),
                f.span
                    .map_or(String::new(), |s| format!("@{}..{}", s.start, s.end)),
                f.driver.map_or(String::new(), |d| format!(
                    "~{}",
                    s.roles.get(usize::from(d.0)).map_or("?", |x| x)
                ))
            );
        }
        for (seq, els) in &i.seqs {
            let n = kind
                .and_then(|k| k.sequences.get(usize::from(seq.0)))
                .map_or("?", |q| &q.name);
            out += &format!(" {n}:[");
            for e in els {
                let at = match e.at {
                    Observation::Wire(t) => format!("w{t}"),
                    Observation::Upstream { id, seq, index } => {
                        format!("u{}/{}/{index}", self.other(&Ref::Upstream(id)), seq.0)
                    }
                    _ => "?".into(),
                };
                out += &format!("{}{at}|{}..{},", e.value, e.cell.start, e.cell.end);
            }
            out += "]";
        }
        for (link, other) in &i.links {
            let role = kind
                .and_then(|k| k.links.get(usize::from(link.0)))
                .map_or("?", |l| &l.role);
            out += &format!(" {role}->{}", self.other(other));
        }
        out
    }
}

/// Prints each analysis's counts and the digest of its items rendered by name; `up` names the
/// analysis each is stacked on.
pub fn print(h: &Host, analyses: &[(AnalysisId, Option<AnalysisId>)]) {
    for &(x, u) in analyses {
        let names = Names {
            track: h.name(x),
            def: h.definition(x),
            up: u.map(|u| (h.name(u), h.definition(u))),
        };
        let store = h.store(x);
        let mut lines: Vec<(Tick, String, u32, String)> = (store.items().values())
            .map(|i| {
                let kind = names.def.schema.kind(i.id.kind).map_or("?", |k| &k.name);
                (i.id.anchor, kind.to_string(), i.id.disc, names.render(i))
            })
            .collect();
        lines.sort();
        let lines: Vec<String> = lines.into_iter().map(|l| l.3).collect();
        let c = store.counts;
        println!(
            "{}: items {} ops {} opens {} sets {} spanned {} extends {} elements {} links {} closes {} incomplete {} peak_open {} lost {} canon {:016x}",
            h.name(x),
            lines.len(),
            c.ops,
            c.opens,
            c.sets,
            c.spanned_sets,
            c.extends,
            c.elements,
            c.links,
            c.closes,
            c.incomplete,
            c.peak_open,
            h.lost(x).len(),
            fnv(&lines)
        );
    }
}
