//! The host's rules, on synthetic analyses.

mod common;

use common::*;
use escapement_tickwave_decode::{
    AnalysisId, Batch, BindError, Bound, CONTRACT, Cause, CheckpointError, Contract, Decoder,
    Definition, Delivery, Element, ElementType, Exchange, FieldDecl, FieldId, FieldType, Host,
    HostOptions, Id, InputDecl, InputSpec, Keeping, KindDecl, KindId, LinkDecl, LinkId, LinkTo,
    LogEntry, Mismatch, Mode, Name, Need, Observation, Op, Pause, Reason, Ref, Refusal, Refused,
    Reply, Role, RunView, Schema, SchemaError, Seek, SeqDecl, SeqId, Setting, SettingDecl,
    SettingId, Source, SourceError, Span, Status, Tick, UpstreamDecl, Value, Window, WindowId,
    field, name, seq,
};
use std::sync::{Arc, Mutex, atomic::Ordering::Relaxed};

fn toy_host(turns: Vec<Vec<Act>>, options: HostOptions) -> (Host, AnalysisId) {
    let mut h = Host::new(options);
    let a = h
        .add(
            name("toy"),
            toy(),
            InputSpec::Wire {
                source: Box::new(Nothing::default()),
                local: None,
            },
            Box::new(move |_, _| Box::new(script(turns.clone()))),
            &[],
            all(),
        )
        .unwrap();
    (h, a)
}

fn batch(h: &Host, a: AnalysisId, seq: u64, ops: Vec<Op>) -> Batch {
    Batch::new(h.run_id(a), seq, ops)
}

fn options(f: impl FnOnce(&mut HostOptions)) -> HostOptions {
    let mut o = HostOptions::default();
    f(&mut o);
    o
}

/// Analysis `a`'s run in window chain `w`, which is live.
fn win(h: &Host, w: WindowId, a: AnalysisId) -> RunView<'_> {
    h.window_run(w, a).unwrap()
}

#[test]
fn a_decoder_for_a_contract_the_host_does_not_run_is_refused_when_added() {
    let mut h = Host::default();
    let with = |contract| Arc::new((*toy()).clone().with_contract(contract));
    let add = |h: &mut Host, n: &'static str, def| {
        h.add(
            name(n),
            def,
            InputSpec::Wire {
                source: Box::new(Nothing::default()),
                local: None,
            },
            Box::new(|_, _| Box::new(script(vec![]))),
            &[],
            all(),
        )
    };
    let newer = Contract {
        minor: CONTRACT.minor + 1,
        ..CONTRACT
    };
    assert_eq!(
        add(&mut h, "a", with(newer)).unwrap_err(),
        BindError::Contract {
            decoder: name("toy"),
            wants: newer,
            runs: CONTRACT,
        }
    );
    let other = Contract {
        major: CONTRACT.major + 1,
        minor: 0,
    };
    assert!(matches!(
        add(&mut h, "b", with(other)),
        Err(BindError::Contract { .. })
    ));
    assert!(
        add(&mut h, "c", with(CONTRACT)).is_ok(),
        "a decoder of the host's own contract is added"
    );
    let message = add(&mut h, "d", with(newer)).unwrap_err().to_string();
    assert!(
        message.contains("1.1") && message.contains("1.0"),
        "{message}"
    );
}

#[test]
fn a_schema_and_its_inputs_are_checked_when_added() {
    let kind = |name_: &'static str, parents: Vec<Name>, fields, links| {
        KindDecl::new(name_)
            .with_parents(parents)
            .with_fields(fields)
            .with_links(links)
    };
    let schema = |kinds: Vec<KindDecl>| {
        let mut s = toy_schema();
        s.kinds = kinds;
        s
    };
    let to = |l: LinkTo| vec![LinkDecl::new("l", vec![l])];
    let refused = [
        (
            schema(vec![
                kind("k", vec![], vec![], vec![]),
                kind("k", vec![], vec![], vec![]),
            ]),
            "declared twice",
        ),
        (
            schema(vec![kind("k", vec![name("nobody")], vec![], vec![])]),
            "parent",
        ),
        (
            schema(vec![kind(
                "k",
                vec![],
                vec![field("w", FieldType::Word { bits: 0 })],
                vec![],
            )]),
            "bounds",
        ),
        (
            schema(vec![kind(
                "k",
                vec![],
                vec![field("w", FieldType::Name(vec![]))],
                vec![],
            )]),
            "no names",
        ),
        (
            schema(vec![kind(
                "k",
                vec![],
                vec![],
                to(LinkTo::Own(name("elsewhere"))),
            )]),
            "points to no kind",
        ),
        (
            schema(vec![kind(
                "k",
                vec![],
                vec![],
                to(LinkTo::Upstream(name("pulse"))),
            )]),
            "points to no kind",
        ),
        (
            schema(vec![kind("", vec![], vec![], vec![])]),
            "name is empty",
        ),
        (toy_schema().with_open_at_most(0), "open-item bound"),
        (
            schema(vec![kind(
                "k",
                vec![],
                vec![field("w", FieldType::Name(vec![name("a"), name("")]))],
                vec![],
            )]),
            "name is empty",
        ),
        (
            schema(vec![kind(
                "k",
                vec![],
                vec![field("w", FieldType::Name(vec![name("a"), name("a")]))],
                vec![],
            )]),
            "kind k: a is declared twice",
        ),
        (
            schema(vec![KindDecl::new("k").with_sequences(vec![seq(
                "s",
                ElementType::Name((0..=256).map(|n| Name::from(n.to_string())).collect()),
            )])]),
            "kind k: s is past the host's bounds",
        ),
        (
            schema(vec![
                KindDecl::new("k").with_sequences(vec![seq("s", ElementType::Name(vec![]))]),
            ]),
            "kind k: s declares no names",
        ),
        (
            schema(vec![KindDecl::new("k").with_sequences(vec![seq(
                "s",
                ElementType::Name(vec![name("x"); 2]),
            )])]),
            "kind k: x is declared twice",
        ),
        (
            toy_schema().with_roles(["probe", "target", "probe"]),
            "role probe is declared twice",
        ),
        (
            Schema::new(vec![
                KindDecl::new("a"),
                KindDecl::new("b").with_row(escapement_tickwave_decode::MAX_LANES),
            ]),
            "row 8, not below 8",
        ),
        (
            Schema::new(vec![
                KindDecl::new("a").with_lanes(8),
                KindDecl::new("b").with_row(1),
            ]),
            "take 9 lanes together",
        ),
    ];
    for (k, (s, why)) in refused.into_iter().enumerate() {
        let mut h = Host::default();
        let mut def = (*toy()).clone();
        def.schema = s;
        let def = Arc::new(def);
        let got = h.add(
            name("toy"),
            def,
            InputSpec::Wire {
                source: Box::new(Nothing::default()),
                local: None,
            },
            Box::new(|_, _| Box::new(script(vec![]))),
            &[],
            all(),
        );
        let message = got.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(message.contains(why), "schema {k}: {message:?}");
    }

    let (mut h, [p, _]) = pulses_host(line(1, 2, 10, vec![]), 100, None, HostOptions::default());
    let mut sub =
        |kinds: Vec<&'static str>, mode, sequences: Vec<&'static str>, n: &'static str| {
            h.add(
                name(n),
                stacked(
                    "pairs",
                    pairs_schema(),
                    UpstreamDecl::new("in", mode)
                        .with_kinds(kinds)
                        .with_sequences(sequences),
                ),
                InputSpec::Upstream(p),
                Box::new(|_, _| Box::new(Pairs::default())),
                &[],
                all(),
            )
        };
    assert!(matches!(
        sub(vec!["frame"], Mode::Pieces, vec![], "x"),
        Err(BindError::UnknownKind { .. })
    ));
    assert!(matches!(
        sub(vec!["pulse"], Mode::Completed, vec!["levels"], "x"),
        Err(BindError::SequenceInCompleted(_))
    ));
    assert!(matches!(
        sub(vec!["pulse"], Mode::Pieces, vec!["bits"], "x"),
        Err(BindError::UnknownSequence(_))
    ));
    assert!(matches!(
        sub(vec!["pulse"], Mode::Pieces, vec!["levels"], "pairs"),
        Err(BindError::DuplicateAnalysis(_))
    ));
    assert!(sub(vec!["pulse"], Mode::Pieces, vec!["levels"], "x").is_ok());
    // An id from a host with more analyses than this one.
    let mut other = Host::default();
    let ids: Vec<AnalysisId> = (0..10)
        .map(|k| {
            let n = format!("t{k}");
            let source = Box::new(Nothing::default());
            let input = InputSpec::Wire {
                source,
                local: None,
            };
            other.add(
                n.into(),
                toy(),
                input,
                Box::new(|_, _| Box::new(Swallow)),
                &[],
                all(),
            )
        })
        .collect::<Result<_, _>>()
        .unwrap();
    let foreign = ids[9];
    let missing = h.add(
        name("y"),
        stacked(
            "pairs",
            pairs_schema(),
            UpstreamDecl::new("in", Mode::Completed),
        ),
        InputSpec::Upstream(foreign),
        Box::new(|_, _| Box::new(Pairs::default())),
        &[],
        all(),
    );
    assert_eq!(missing.unwrap_err(), BindError::NoAnalysis(foreign));
}

#[test]
fn every_operation_is_checked_and_a_batch_commits_whole() {
    let (mut h, a) = toy_host(vec![], HostOptions::default());
    let f = frame(10);
    h.submit(batch(&h, a, 0, vec![open(f, 10)])).unwrap();
    let el = |value| Element::new(value, Observation::Wire(12), Span { start: 11, end: 13 });
    let span = Some(Span { start: 10, end: 12 });
    let word = |value, bits| Value::Word { value, bits };
    let refused = [
        (
            set(f, FieldId(7), Value::Bool(true)),
            Mismatch::Field {
                kind: FRAME,
                field: 7,
            },
        ),
        (
            set(f, V, word(1, 16)),
            Mismatch::Type {
                kind: FRAME,
                field: 0,
            },
        ),
        (
            set(f, V, word(256, 8)),
            Mismatch::Type {
                kind: FRAME,
                field: 0,
            },
        ),
        (
            set(f, N, Value::Name(2)),
            Mismatch::Type {
                kind: FRAME,
                field: 1,
            },
        ),
        (
            set(f, N, Value::Bool(true)),
            Mismatch::Type {
                kind: FRAME,
                field: 1,
            },
        ),
        (
            set(f, P, Value::Phase),
            Mismatch::Unspanned {
                kind: FRAME,
                field: 2,
            },
        ),
        (
            Op::Set {
                id: f,
                field: P,
                value: Value::Phase,
                span,
                driver: Some(Role(1)),
            },
            Mismatch::Role(1),
        ),
        (
            set(
                f,
                V,
                Value::Unknown {
                    reason: Reason(1),
                    needs: None,
                },
            ),
            Mismatch::Reason(1),
        ),
        (
            set(
                f,
                V,
                Value::Unknown {
                    reason: Reason(0),
                    needs: Some(Need(3)),
                },
            ),
            Mismatch::Need(3),
        ),
        (
            Op::Extend {
                id: f,
                seq: BITS,
                offset: 0,
                elements: vec![el(2)],
            },
            Mismatch::Element {
                kind: FRAME,
                seq: 0,
                value: 2,
            },
        ),
        (
            Op::Extend {
                id: f,
                seq: SeqId(1),
                offset: 0,
                elements: vec![el(1)],
            },
            Mismatch::Sequence {
                kind: FRAME,
                seq: 1,
            },
        ),
        (
            Op::Link {
                id: f,
                link: LinkId(1),
                other: Ref::Own(f),
            },
            Mismatch::Link {
                kind: FRAME,
                link: 1,
            },
        ),
        (
            Op::Link {
                id: f,
                link: LinkId(0),
                other: Ref::Own(Id::new(CHILD, 11)),
            },
            Mismatch::LinkTarget {
                kind: FRAME,
                link: 0,
                to: Ref::Own(Id::new(CHILD, 11)),
            },
        ),
        (
            Op::Link {
                id: f,
                link: LinkId(0),
                other: Ref::Upstream(f),
            },
            Mismatch::LinkTarget {
                kind: FRAME,
                link: 0,
                to: Ref::Upstream(f),
            },
        ),
        (
            Op::Open {
                id: Id::new(FRAME, 11),
                parent: Some(f),
                start: 11,
            },
            Mismatch::Parent {
                kind: FRAME,
                parent: FRAME,
            },
        ),
        (
            Op::Close {
                id: f,
                end: 20,
                status: Status::Incomplete(Cause(4)),
            },
            Mismatch::Cause(4),
        ),
        (open(Id::new(KindId(9), 11), 11), Mismatch::Kind(KindId(9))),
    ];
    for (k, (op, why)) in refused.into_iter().enumerate() {
        let good = set(f, V, word(1, 8));
        let got = h.submit(batch(&h, a, 1, vec![good, op]));
        assert_eq!(got, Err(Refusal::Schema { op: 1, why }), "case {k}");
    }
    assert!(
        h.store(a).items()[&f].fields.is_empty(),
        "a refused batch commits nothing"
    );

    let lifecycle = [
        (vec![open(f, 10)], Refused::Reopened),
        (vec![set(frame(30), V, word(1, 8))], Refused::NotOpen),
        (
            vec![set(f, V, word(1, 8)), set(f, V, word(2, 8))],
            Refused::FieldTwice,
        ),
        (
            vec![Op::Set {
                id: f,
                field: P,
                value: Value::Phase,
                span: Some(Span { start: 9, end: 12 }),
                driver: None,
            }],
            Refused::BeforeStart,
        ),
        (
            vec![Op::Extend {
                id: f,
                seq: BITS,
                offset: 1,
                elements: vec![el(1)],
            }],
            Refused::OffsetGap {
                expected: 0,
                got: 1,
            },
        ),
        (
            vec![Op::Extend {
                id: f,
                seq: BITS,
                offset: 0,
                elements: vec![el(1); 65],
            }],
            Refused::ChunkTooLong,
        ),
        (
            vec![
                Op::Extend {
                    id: f,
                    seq: BITS,
                    offset: 0,
                    elements: vec![el(1)],
                },
                close(f, 12),
            ],
            Refused::PastEnd,
        ),
        (vec![close(f, 9)], Refused::EndBeforeStart),
        (
            vec![Op::Open {
                id: Id::new(CHILD, 12),
                parent: Some(frame(40)),
                start: 12,
            }],
            Refused::ParentNotOpen,
        ),
        (
            vec![Op::Open {
                id: Id::new(CHILD, 9),
                parent: Some(f),
                start: 9,
            }],
            Refused::OutsideParent,
        ),
        (
            vec![
                Op::Open {
                    id: Id::new(CHILD, 12),
                    parent: Some(f),
                    start: 12,
                },
                close(f, 20),
            ],
            Refused::ChildOpen,
        ),
        (
            vec![set(
                f,
                B,
                Value::Bits {
                    bytes: vec![0; 2].into(),
                    bits: 4,
                },
            )],
            Refused::ValueTooLong,
        ),
    ];
    for (k, (ops, why)) in lifecycle.into_iter().enumerate() {
        let n = ops.len() - 1;
        let got = h.submit(batch(&h, a, 1, ops));
        assert_eq!(got, Err(Refusal::Lifecycle { op: n, why }), "case {k}");
    }
    let counts = h.store(a).counts;
    assert_eq!(
        (counts.ops, counts.opens),
        (1, 1),
        "refused batches count nothing"
    );

    assert_eq!(
        h.submit(batch(&h, a, 3, vec![])),
        Err(Refusal::BatchOrder {
            expected: 1,
            got: 3
        })
    );
    let child = Id::new(CHILD, 12);
    let ops = vec![
        Op::Open {
            id: child,
            parent: Some(f),
            start: 12,
        },
        close(child, 18),
        Op::Link {
            id: f,
            link: LinkId(0),
            other: Ref::Own(frame(3)),
        },
        Op::Extend {
            id: f,
            seq: BITS,
            offset: 0,
            elements: vec![el(1), el(0)],
        },
        close(f, 19),
    ];
    h.submit(batch(&h, a, 1, ops)).unwrap();
    let item = &h.store(a).items()[&f];
    assert_eq!((item.end, item.seq(BITS).len()), (Some(19), 2));
    assert_eq!(h.id_text(a, &f), "toy/frame/10");
    assert_eq!(h.id_text(a, &Id { disc: 2, ..child }), "toy/child/12.2");
}

#[test]
fn a_batch_past_the_declared_open_bound_is_refused_where_it_passes_it() {
    let (mut h, a) = toy_host(vec![], HostOptions::default());
    let ops: Vec<Op> = (0..5).map(|k| open(frame(k), k)).collect();
    assert_eq!(
        h.submit(batch(&h, a, 0, ops.clone())),
        Err(Refusal::Lifecycle {
            op: 4,
            why: Refused::TooManyOpen {
                open: 5,
                at_most: 4
            },
        })
    );
    assert_eq!(h.store(a).open_count(), 0);
    let mut ops = ops;
    ops.insert(4, close(frame(0), 4));
    h.submit(batch(&h, a, 0, ops)).unwrap();
    assert_eq!(h.store(a).counts.peak_open, 4);
}

#[test]
fn frontiers_bound_new_starts_and_retire_closed_ids() {
    let options = options(|o| o.recent_at_most = 3);
    let (mut h, a) = toy_host(vec![], options);
    let at = |h: &Host, seq, ops, frontier| {
        let mut b = batch(h, a, seq, ops);
        b.frontier = Some(frontier);
        b
    };
    assert_eq!(
        h.submit(at(&h, 0, vec![], 5)),
        Err(Refusal::FrontierAhead {
            got: 5,
            input: Tick::MIN
        }),
        "nothing taken yet"
    );
    // Take input to 100 through a turn.
    let mut h = {
        let turns = vec![vec![Act::Request(0, 100)], vec![Act::TakeReply]];
        toy_host(turns, options).0
    };
    h.step().unwrap();
    h.step().unwrap();
    assert_eq!(h.consumed(a), 100);
    let ids: Vec<Id> = (0..4).map(|k| frame(10 + k)).collect();
    let mut ops = vec![];
    for &id in &ids[..3] {
        ops.push(open(id, id.anchor));
        ops.push(close(id, id.anchor + 1));
    }
    h.submit(at(&h, 0, ops, 10)).unwrap();
    assert_eq!(h.peak_recent(a), 3);
    assert_eq!(
        h.submit(at(&h, 1, vec![open(ids[3], 13), close(ids[3], 14)], 10)),
        Err(Refusal::TooManyRecent {
            held: 4,
            at_most: 3
        }),
        "a run whose frontier holds still cannot hold every closed id"
    );
    assert_eq!(
        h.submit(at(&h, 1, vec![open(ids[0], 10)], 10)),
        Err(Refusal::Lifecycle {
            op: 0,
            why: Refused::Reopened
        }),
        "a closed id ahead of the frontier is held"
    );
    assert_eq!(
        h.submit(at(&h, 1, vec![], 9)),
        Err(Refusal::FrontierBack { was: 10, got: 9 })
    );
    h.submit(at(&h, 1, vec![open(ids[3], 13), close(ids[3], 14)], 13))
        .unwrap();
    assert_eq!(
        h.peak_recent(a),
        3,
        "the frontier retired the ids behind it"
    );
    assert_eq!(
        h.submit(at(&h, 2, vec![open(frame(12), 12)], 13)),
        Err(Refusal::LateStart {
            op: 0,
            at: 12,
            frontier: 13
        })
    );
    assert_eq!(
        h.submit(at(&h, 2, vec![open(frame(13), 14)], 13)),
        Err(Refusal::AnchorBeforeStart { op: 0 })
    );
    assert_eq!(h.settled(a), 13);
}

#[test]
fn replies_carry_their_run_and_are_taken_once() {
    let turns = vec![
        vec![Act::Request(0, 10)],
        vec![Act::TakeReply, Act::Request(10, 20)],
    ];
    let (mut h, a) = toy_host(turns, HostOptions::default());
    h.step().unwrap();
    let first = h.outstanding(a).unwrap();
    let reply = h.take_io().unwrap().unwrap();
    h.replace(a, None);
    assert_eq!(
        h.reply(reply),
        Err(Refusal::StaleReply {
            got: first,
            outstanding: None
        }),
        "a reply to a retired run is refused"
    );
    h.step().unwrap();
    let id = h.outstanding(a).unwrap();
    assert_ne!(id.run, first.run);
    let reply = h.take_io().unwrap().unwrap();
    h.reply(reply).unwrap();
    let again = Reply::new(id, Default::default());
    assert!(
        matches!(h.reply(again), Err(Refusal::StaleReply { .. })),
        "a reply is consumed once"
    );
    h.step().unwrap();
    assert_eq!(h.consumed(a), 10);
}

#[test]
fn a_decoder_that_breaks_the_rules_is_refused_never_a_panic() {
    let twice = vec![vec![Act::Request(0, 10), Act::Request(10, 20)]];
    let (mut h, a) = toy_host(twice, HostOptions::default());
    assert_eq!(h.turn(a), Err(Refusal::RequestTwice(h.run_id(a))));

    let left_open = vec![vec![Act::Emit(open(frame(0), 0)), Act::Finish(5)]];
    let (mut h, a) = toy_host(left_open, HostOptions::default());
    assert_eq!(
        h.turn(a),
        Err(Refusal::LeftOpen {
            run: h.run_id(a),
            open: 1
        })
    );

    let (mut h, a) = toy_host(vec![vec![]], HostOptions::default());
    let t = h.begin_turn(a).unwrap();
    assert_eq!(
        h.begin_turn(a).err(),
        Some(Refusal::TurnUnderWay(h.run_id(a)))
    );
    assert!(!h.runnable(a), "a run whose turn is out does not run again");
    assert!(h.checkpoint(a).is_err(), "nor is it checkpointed");
    assert_eq!(h.end_turn(t), Ok(Pause::Input), "a turn handed back unrun");
    assert!(h.begin_turn(a).is_ok());
}

/// Every frame open and closed in turn, one at each tick of `at`.
fn frames(at: &[Tick]) -> Vec<Op> {
    (at.iter())
        .flat_map(|&t| [open(frame(t), t), close(frame(t), t + 1)])
        .collect()
}

/// The toy decoder on a wire, and a sink on its frames' pieces that records each piece's op.
fn toy_and_sink(options: HostOptions) -> (Host, AnalysisId, AnalysisId, Arc<Mutex<Vec<Op>>>) {
    struct Record(Arc<Mutex<Vec<Op>>>);
    impl Decoder for Record {
        fn turn(&mut self, ex: &mut Exchange<'_>) -> Pause {
            while let Some(d) = ex.receive() {
                if let Delivery::Piece { piece, .. } = d {
                    self.0.lock().unwrap().push(piece.op().clone());
                }
            }
            if ex.more() { Pause::Work } else { Pause::Input }
        }
    }
    let mut h = Host::new(options);
    let a = h
        .add(
            name("toy"),
            toy(),
            InputSpec::Wire {
                source: Box::new(Nothing::default()),
                local: None,
            },
            Box::new(|_, _| Box::new(script(vec![]))),
            &[],
            all(),
        )
        .unwrap();
    let got = Arc::new(Mutex::new(vec![]));
    let g = got.clone();
    let sink = h
        .add(
            name("sink"),
            stacked(
                "sink",
                toy_schema(),
                UpstreamDecl::new("in", Mode::Pieces).with_kinds(["frame"]),
            ),
            InputSpec::Upstream(a),
            Box::new(move |_, _| Box::new(Record(g.clone()))),
            &[],
            all(),
        )
        .unwrap();
    (h, a, sink, got)
}

#[test]
fn a_batch_past_the_bound_is_refused() {
    let (mut h, a, _, _) = toy_and_sink(options(|o| o.batch_ops = 4));
    assert_eq!(
        h.submit(batch(&h, a, 0, frames(&[10, 20, 30]))),
        Err(Refusal::TooManyOps { got: 6, at_most: 4 })
    );
    assert!(h.submit(batch(&h, a, 0, frames(&[10, 20]))).is_ok());
}

#[test]
fn a_batch_counts_its_losses_against_the_bound() {
    let losses: Vec<Act> = (0..100).map(|t| Act::Lost(2 * t, 2 * t + 1)).collect();
    let turns = vec![
        vec![Act::Request(0, 1000)],
        [vec![Act::TakeReply], losses].concat(),
    ];
    let mut h = Host::new(options(|o| o.batch_ops = 4));
    let a = h
        .add(
            name("toy"),
            toy(),
            InputSpec::Wire {
                source: Box::new(Nothing::default()),
                local: None,
            },
            Box::new(move |_, _| Box::new(script(turns.clone()))),
            &[],
            all(),
        )
        .unwrap();
    h.turn(a).unwrap();
    let reply = h.take_io().unwrap().unwrap();
    h.reply(reply).unwrap();
    assert_eq!(
        h.turn(a),
        Err(Refusal::TooManyOps {
            got: 100,
            at_most: 4
        }),
        "each loss is a delivery to every analysis stacked on it"
    );
}

#[test]
fn a_run_whose_frontier_stays_holds_no_more_losses_than_the_bound() {
    let mut turns = vec![vec![Act::Request(0, 1000)], vec![Act::TakeReply]];
    turns.extend((0..10).map(|t| vec![Act::Lost(2 * t, 2 * t + 1)]));
    let mut h = Host::new(options(|o| o.recent_at_most = 5));
    let a = h
        .add(
            name("toy"),
            toy(),
            InputSpec::Wire {
                source: Box::new(Nothing::default()),
                local: None,
            },
            Box::new(move |_, _| Box::new(script(turns.clone()))),
            &[],
            vec![],
        )
        .unwrap();
    h.turn(a).unwrap();
    let reply = h.take_io().unwrap().unwrap();
    h.reply(reply).unwrap();
    let got: Vec<_> = (0..7).map(|_| h.turn(a)).collect();
    assert!(got[..6].iter().all(Result::is_ok), "{got:?}");
    assert_eq!(
        got[6],
        Err(Refusal::TooManyLost {
            held: 6,
            at_most: 5
        })
    );
}

#[test]
fn pieces_of_a_batch_longer_than_a_chunk_arrive_whole_and_in_order() {
    let (mut h, a, sink, got) = toy_and_sink(HostOptions::default());
    let at: Vec<Tick> = (0..400).map(|k| 10 * k).collect();
    let ops = frames(&at);
    assert!(ops.len() > 3 * escapement_tickwave_decode::PIECE_CHUNK);
    h.submit(batch(&h, a, 0, ops.clone())).unwrap();
    while h.runnable(sink) {
        h.turn(sink).unwrap();
    }
    assert_eq!(*got.lock().unwrap(), ops);
}

#[test]
fn lost_input_is_checked_joined_and_delivered_at_its_place() {
    let turns = vec![
        vec![Act::Request(0, 100)],
        vec![Act::TakeReply],
        vec![
            Act::Lost(10, 20),
            Act::Emit(open(frame(30), 30)),
            Act::Lost(20, 25),
        ],
        vec![Act::Emit(close(frame(30), 31)), Act::Frontier(40)],
    ];
    let mut h = Host::new(options(|o| o.log = Some(1 << 10)));
    let a = h
        .add(
            name("toy"),
            toy(),
            InputSpec::Wire {
                source: Box::new(Nothing::default()),
                local: None,
            },
            Box::new(move |_, _| Box::new(script(turns.clone()))),
            &[],
            vec![Span { start: 0, end: 15 }],
        )
        .unwrap();
    let sink = h
        .add(
            name("sink"),
            stacked(
                "sink",
                toy_schema(),
                UpstreamDecl::new("in", Mode::Pieces).with_kinds(["frame"]),
            ),
            InputSpec::Upstream(a),
            Box::new(|_, _| Box::new(script(vec![]))),
            &[],
            all(),
        )
        .unwrap();
    let lost = |h: &Host, seq, lost: Vec<(usize, Span)>| {
        let mut b = batch(h, a, seq, vec![]);
        b.lost = lost;
        b
    };
    assert_eq!(
        h.submit(lost(&h, 0, vec![(0, Span { start: 0, end: 5 })])),
        Err(Refusal::LossAhead {
            got: 5,
            input: Tick::MIN
        }),
        "nothing taken yet"
    );
    h.turn(a).unwrap();
    let reply = h.take_io().unwrap().unwrap();
    h.reply(reply).unwrap();
    h.turn(a).unwrap();
    assert_eq!(h.consumed(a), 100);
    let bad = [
        vec![(0, Span { start: 5, end: 5 })],
        vec![
            (0, Span { start: 5, end: 9 }),
            (0, Span { start: 7, end: 9 }),
        ],
        vec![(1, Span { start: 5, end: 9 })],
    ];
    for (k, l) in bad.into_iter().enumerate() {
        assert!(
            matches!(h.submit(lost(&h, 0, l)), Err(Refusal::LossOrder { .. })),
            "case {k}"
        );
    }
    h.turn(a).unwrap();
    assert_eq!(
        h.lost(a),
        &[Span { start: 10, end: 25 }],
        "adjacent losses join"
    );
    assert_eq!(h.lost_total(a), 2);
    let to_sink: Vec<char> = h
        .log()
        .filter_map(|e| match e {
            LogEntry::Delivered { to, what, .. } if *to == h.run_id(sink) => Some(what.0),
            _ => None,
        })
        .collect();
    assert_eq!(
        to_sink,
        ['G', 'O', 'G'],
        "each loss in its place among the output"
    );
    h.turn(a).unwrap();
    assert_eq!(h.settled(a), 40);
    assert_eq!(
        h.lost(a),
        &[Span { start: 10, end: 25 }],
        "a settled loss that meets a kept span stays"
    );

    // A settled loss nothing keeps is let go; the count stays.
    let turns = vec![
        vec![Act::Request(0, 100)],
        vec![Act::TakeReply, Act::Lost(10, 20), Act::Frontier(5)],
        vec![Act::Frontier(30)],
    ];
    let mut h = Host::default();
    let a = h
        .add(
            name("toy"),
            toy(),
            InputSpec::Wire {
                source: Box::new(Nothing::default()),
                local: None,
            },
            Box::new(move |_, _| Box::new(script(turns.clone()))),
            &[],
            vec![],
        )
        .unwrap();
    run_all(&mut h);
    assert_eq!((h.lost(a), h.lost_total(a)), (&[][..], 1));
}

#[test]
fn a_stacked_analysis_receives_each_loss_in_its_place_and_must_declare_it() {
    let gap = Span {
        start: 95,
        end: 130,
    };
    let (mut h, [p, q]) = pulses_host(line(20, 3, 20, vec![gap]), 50, None, HostOptions::default());
    run_all(&mut h);
    assert_eq!(h.lost(p), &[gap]);
    assert_eq!(h.lost(q), &[gap], "the stacked analysis declared it again");
    let pairs = &h.store(q).items();
    assert!(
        pairs
            .values()
            .all(|i| i.start >= gap.end || i.end.unwrap() <= gap.start),
        "no pair spans the gap"
    );

    // One that takes a loss and does not declare it again is refused.
    let mut h = Host::default();
    let p = h
        .add(
            name("pulses"),
            definition("pulses", pulses_schema()),
            InputSpec::Wire {
                source: Box::new(line(20, 3, 20, vec![gap])),
                local: None,
            },
            Box::new(|_, local| Box::new(Pulses::new(local, 50))),
            &[],
            all(),
        )
        .unwrap();
    let q = h
        .add(
            name("pairs"),
            stacked(
                "pairs",
                pairs_schema(),
                UpstreamDecl::new("in", Mode::Completed).with_kinds(["pulse"]),
            ),
            InputSpec::Upstream(p),
            Box::new(|_, _| Box::new(Swallow)),
            &[],
            all(),
        )
        .unwrap();
    let got = h.run_until(|_| false);
    assert_eq!(
        got,
        Err(Refusal::LossNotDeclared {
            run: h.run_id(q),
            span: gap
        })
    );
}

#[test]
fn completed_items_and_pieces_reach_stacked_analyses_as_subscribed() {
    let options = options(|o| o.log = Some(1 << 12));
    let (mut h, [p, q]) = pulses_host(line(6, 3, 20, vec![]), 35, None, options);
    let subscribe = |h: &mut Host, n: &'static str, sequences: Vec<&'static str>| {
        h.add(
            name(n),
            stacked(
                n,
                toy_schema(),
                UpstreamDecl::new("in", Mode::Pieces)
                    .with_kinds(["pulse"])
                    .with_sequences(sequences),
            ),
            InputSpec::Upstream(p),
            Box::new(|_, _| Box::new(Swallow)),
            &[],
            all(),
        )
        .unwrap()
    };
    let bare = subscribe(&mut h, "bare", vec![]);
    let levels = subscribe(&mut h, "levels", vec!["levels"]);
    run_all(&mut h);
    assert_eq!(h.store(p).counts.closes, 6);
    assert_eq!(h.store(q).counts.closes, 3, "three pairs");
    let pair = h.store(q).items().values().next().unwrap();
    assert_eq!(pair.links, vec![(FIRST, Ref::Upstream(Id::new(PULSE, 10)))]);
    assert_eq!(
        pair.field(WIDTH).unwrap().value,
        Value::Word { value: 6, bits: 32 }
    );
    assert_eq!(h.finished(q), Some(130));
    let got = |a: AnalysisId| -> String {
        let run = h.run_id(a);
        h.log()
            .filter_map(|e| match e {
                LogEntry::Delivered { to, what, .. } if *to == run && what.0 != 'F' => Some(what.0),
                _ => None,
            })
            .collect()
    };
    assert_eq!(
        got(bare),
        "OSC".repeat(6) + "E",
        "no sequence named, no extends"
    );
    assert_eq!(got(levels), "OXSC".repeat(6) + "E");
    assert_eq!(got(q), "I".repeat(6) + "E");
}

#[test]
fn a_checkpoint_needs_a_root_and_decoders_that_save_and_restores_as_new_generations() {
    let full = {
        let (mut h, [p, q]) =
            pulses_host(line(30, 3, 20, vec![]), 45, None, HostOptions::default());
        run_all(&mut h);
        (h.store(p).counts, h.store(q).counts)
    };
    let (mut h, [p, q]) = pulses_host(line(30, 3, 20, vec![]), 45, None, HostOptions::default());
    assert!(matches!(h.checkpoint(q), Err(CheckpointError::NotARoot(_))));
    while h.consumed(p) < 200 {
        h.step().unwrap();
    }
    let cut = h.checkpoint(p).unwrap();
    run_all(&mut h);
    assert_eq!((h.store(p).counts, h.store(q).counts), full);
    let retired = h.restore(&cut).unwrap();
    assert_eq!(retired.len(), 2);
    assert_ne!(h.run_id(p), cut.runs[0], "a restore is a new generation");
    run_all(&mut h);
    assert_eq!(
        (h.store(p).counts, h.store(q).counts),
        full,
        "a restored chain ends where the whole run did"
    );
    h.restore(&cut).unwrap();
    run_all(&mut h);
    assert_eq!(
        (h.store(p).counts, h.store(q).counts),
        full,
        "restored twice"
    );

    struct Plain;
    impl Decoder for Plain {
        fn turn(&mut self, _: &mut Exchange<'_>) -> Pause {
            Pause::Input
        }
    }
    let mut h = Host::default();
    let a = h
        .add(
            name("plain"),
            toy(),
            InputSpec::Wire {
                source: Box::new(Nothing::default()),
                local: None,
            },
            Box::new(|_, _| Box::new(Plain)),
            &[],
            all(),
        )
        .unwrap();
    assert!(matches!(
        h.checkpoint(a),
        Err(CheckpointError::Unsupported(_))
    ));
}

#[test]
fn an_interrupted_read_stays_outstanding_with_its_source_unmoved() {
    let reads = Nothing::default();
    let count = reads.0.clone();
    let mut h = Host::default();
    let a = h
        .add(
            name("toy"),
            toy(),
            InputSpec::Wire {
                source: Box::new(reads),
                local: None,
            },
            Box::new(|_, _| {
                Box::new(script(vec![
                    vec![Act::Request(0, 10)],
                    vec![Act::TakeReply],
                ]))
            }),
            &[],
            all(),
        )
        .unwrap();
    h.step().unwrap();
    let id = h.outstanding(a).unwrap();
    h.interrupt(a).store(true, Relaxed);
    assert!(h.take_io().unwrap().is_none());
    assert_eq!(h.stats().cancelled_reads, 1);
    assert!(
        !h.interrupt(a).load(Relaxed),
        "the host lowers it once acted on"
    );
    assert_eq!(h.outstanding(a), Some(id), "still outstanding");
    assert_eq!(count.load(Relaxed), 0, "the source did not read");
    h.step().unwrap();
    assert_eq!(count.load(Relaxed), 1);
    h.step().unwrap();
    assert_eq!(h.consumed(a), 10);
}

#[test]
fn a_turn_whose_run_is_replaced_meanwhile_is_refused_whole() {
    let turns = vec![vec![
        Act::Emit(open(frame(0), 0)),
        Act::Emit(close(frame(0), 1)),
    ]];
    let (mut h, a) = toy_host(turns, HostOptions::default());
    let mut t = h.begin_turn(a).unwrap();
    t.run();
    assert_eq!(t.emitted(), 2);
    let old = h.run_id(a);
    h.replace(a, None);
    assert_eq!(
        h.end_turn(t),
        Err(Refusal::StaleRun {
            got: old,
            current: h.run_id(a)
        })
    );
    assert_eq!(h.store(a).counts.ops, 0);
    assert_eq!(h.stats().stale, 1);
}

#[test]
fn deliveries_that_arrive_during_a_turn_queue_behind_those_it_left() {
    let full = {
        let (mut h, [_, q]) = pulses_host(line(8, 3, 20, vec![]), 30, None, HostOptions::default());
        run_all(&mut h);
        h.store(q).counts
    };
    let options = options(|o| o.turn_deliveries = 1);
    let (mut h, [p, q]) = pulses_host(line(8, 3, 20, vec![]), 30, None, options);
    let pump = |h: &mut Host| {
        if let Some(r) = h.take_io().unwrap() {
            h.reply(r).unwrap();
        }
        h.turn(p).unwrap();
    };
    while h.store(p).counts.closes < 2 {
        pump(&mut h);
    }
    // The turn takes one delivery of those queued; the upstream finishes while it is out.
    let mut t = h.begin_turn(q).unwrap();
    while h.finished(p).is_none() {
        pump(&mut h);
    }
    t.run();
    h.end_turn(t).unwrap();
    run_all(&mut h);
    assert_eq!(h.store(q).counts, full);
    assert_eq!(h.finished(q), h.finished(p));
}

#[test]
fn the_log_is_off_by_default_and_bounded_when_on() {
    let quiet = {
        let (mut h, _) = pulses_host(line(40, 3, 20, vec![]), 30, None, HostOptions::default());
        run_all(&mut h);
        assert_eq!(h.log().count(), 0);
        h.stats().admitted
    };
    let options = options(|o| o.log = Some(16));
    let (mut h, _) = pulses_host(line(40, 3, 20, vec![]), 30, None, options);
    run_all(&mut h);
    assert_eq!(h.stats().admitted, quiet);
    assert_eq!(h.log().count(), 16);
    assert!(h.stats().log_dropped > 0);
}

#[test]
fn a_definition_from_data_runs_as_a_static_one_does() {
    let owned = Schema::new(vec![
        KindDecl::new("pulse".to_string())
            .with_fields(vec![FieldDecl::new(
                "width".to_string(),
                FieldType::Word { bits: 32 },
            )])
            .with_sequences(vec![SeqDecl::new("levels".to_string(), ElementType::Bit)]),
    ])
    .with_causes(vec!["a gap cuts it".to_string()]);
    let run = |schema: Schema| {
        let mut h = Host::default();
        let p = h
            .add(
                name("pulses"),
                Arc::new(Definition::new("pulses", 1, schema).with_ports(["line"])),
                InputSpec::Wire {
                    source: Box::new(line(10, 3, 20, vec![Span { start: 70, end: 90 }])),
                    local: None,
                },
                Box::new(|_, local| Box::new(Pulses::new(local, 25))),
                &[],
                all(),
            )
            .unwrap();
        run_all(&mut h);
        h.store(p).counts
    };
    let (a, b) = (run(owned), run(pulses_schema()));
    assert_eq!(a, b);
    assert!(a.ops > 0 && a.incomplete == 0);
}

#[test]
fn kept_checkpoints_spread_within_their_budget_and_a_seek_replays_from_the_nearest() {
    let run = || {
        let (mut h, [p, q]) =
            pulses_host(line(60, 3, 20, vec![]), 15, None, HostOptions::default());
        (
            h.keep_checkpoints(p, Keeping::new(50, usize::MAX)).unwrap(),
            (h, p, q),
        )
            .1
    };
    let (mut h, p, q) = run();
    run_all(&mut h);
    let full = (
        h.store(p).counts,
        h.store(q).counts,
        h.store(q).items().len(),
    );
    let forward = h.store(q).items().clone();
    let all = h.kept(p).unwrap();
    assert!(all.count > 10, "{all:?}");
    assert!(
        all.widest >= 50 && all.widest < 50 + 15,
        "spaced by the keeping: {all:?}"
    );
    assert!(matches!(
        h.keep_checkpoints(q, Keeping::new(50, 0)),
        Err(CheckpointError::NotARoot(_))
    ));

    let Seek::Restored(at) = h.seek(p, 700).unwrap() else {
        panic!("a checkpoint at or before 700 is held");
    };
    assert!(at <= 700 && 700 - at < all.widest, "the nearest: {at}");
    assert!(h.consumed(p) == at);
    run_all(&mut h);
    assert_eq!(
        (h.store(p).counts, h.store(q).counts),
        (full.0, full.1),
        "a replay from the nearest ends where the forward pass did"
    );
    let replayed = &h.store(q).items();
    assert!(replayed.keys().all(|id| forward.contains_key(id)));
    assert!(
        (forward.iter())
            .filter(|(_, i)| i.start >= at)
            .all(|(id, i)| replayed.get(id).is_some_and(|r| r.end == i.end)),
        "everything from the checkpoint on is kept again"
    );
    assert!(
        replayed.len() < forward.len(),
        "a kept checkpoint holds no closed items"
    );
    assert_eq!(h.kept(p).unwrap(), all, "a replay takes no checkpoints");

    // A window starting just before a checkpoint is re-derived from the one before it.
    let window = Span {
        start: at - 1,
        end: at + 100,
    };
    let meets = |i: &escapement_tickwave_decode::Item| {
        i.start < window.end && i.end.is_none_or(|e| e > window.start)
    };
    h.set_keep(q, vec![window]);
    let Seek::Restored(before) = h.seek(p, window.start).unwrap() else {
        panic!("a checkpoint before {at} is held");
    };
    assert!(before < at);
    run_all(&mut h);
    let got: Vec<_> = (h.store(q).items().iter())
        .filter(|(_, i)| meets(i))
        .map(|(id, i)| (*id, i.end))
        .collect();
    let want: Vec<_> = (forward.iter())
        .filter(|(_, i)| meets(i))
        .map(|(id, i)| (*id, i.end))
        .collect();
    assert!(!want.is_empty());
    assert_eq!(got, want, "the window's items are the forward pass's");
    h.set_keep(q, common::all());
    assert_eq!(h.seek(p, Tick::MIN).unwrap(), Seek::FromStart);
    run_all(&mut h);
    assert_eq!(
        (
            h.store(p).counts,
            h.store(q).counts,
            h.store(q).items().len()
        ),
        full
    );

    // Room for ten: the first and the latest stay, and no gap is twice an even spread's.
    let (mut h, p, _) = run();
    run_all(&mut h);
    let one = h.kept(p).unwrap().bytes / all.count;
    let (mut small, sp, _) = run();
    let budget = one * 10 + one / 2;
    small
        .keep_checkpoints(sp, Keeping::new(50, budget))
        .unwrap();
    run_all(&mut small);
    let few = small.kept(sp).unwrap();
    assert!(few.count == 10 && few.bytes <= budget, "{few:?}");
    assert_eq!((few.first, few.latest), (all.first, all.latest));
    let even = (all.latest.unwrap() - all.first.unwrap()) / 9;
    assert!(few.widest < 2 * even, "{few:?} against {even}");

    // A checkpoint restores only onto the decoders it was taken of.
    let cut = h.checkpoint(p).unwrap();
    let (mut other, _) = toy_host(vec![], HostOptions::default());
    assert!(matches!(
        other.restore(&cut),
        Err(CheckpointError::Incompatible)
    ));
}

#[test]
fn kept_checkpoints_can_be_spaced_by_the_observations_taken() {
    let kept = |keeping: Keeping| {
        let (mut h, [p, _]) =
            pulses_host(line(60, 3, 20, vec![]), 15, None, HostOptions::default());
        h.keep_checkpoints(p, keeping).unwrap();
        run_all(&mut h);
        (h.kept(p).unwrap().count as u64, h.observed(p))
    };
    let (by_ticks, observed) = kept(Keeping::new(Tick::MAX, usize::MAX));
    assert_eq!(by_ticks, 1, "only the first, ticks alone never due again");
    assert!(observed >= 100, "{observed}");
    let (by_events, _) = kept(Keeping::new(Tick::MAX, usize::MAX).with_events(10));
    assert!(
        by_events + 2 >= observed / 10 && by_events <= observed / 10 + 1,
        "{by_events} for {observed} observations"
    );
}

#[test]
fn checkpoints_spaced_by_observations_are_evicted_by_observations() {
    // Sparse pulses, then ten times the edges in under half the ticks: checkpoints spaced by
    // observations crowd together in ticks where the edges do.
    let (mut edges, mut at) = (vec![], 10);
    for period in [100; 50].into_iter().chain([4; 500]) {
        edges.extend([at, at + 2]);
        at += period;
    }
    let host = |keeping: Keeping| {
        let line = common::Line {
            initial: false,
            edges: edges.clone(),
            gaps: vec![],
            end: at,
        };
        let (mut h, [p, _]) = pulses_host(line, 15, None, HostOptions::default());
        h.keep_checkpoints(p, keeping).unwrap();
        run_all(&mut h);
        (h.kept(p).unwrap(), h.observed(p))
    };
    let keeping = |budget| Keeping::new(Tick::MAX, budget).with_events(20);
    let (all, observed) = host(keeping(usize::MAX));
    assert!(all.count > 40, "{all:?}");

    // Room for about ten, as checkpoints differ in size: no gap holds twice the observations of
    // an even spread.
    let one = all.bytes / all.count;
    let budget = one * 10 + one / 2;
    let (few, _) = host(keeping(budget));
    assert!(
        (9..=11).contains(&few.count) && few.bytes <= budget,
        "{few:?}"
    );
    assert_eq!((few.first, few.latest), (all.first, all.latest));
    let even = observed / (few.count as u64 - 1);
    assert!(few.widest_observed < 2 * even, "{few:?} against {even}");
}

#[test]
fn a_window_chain_beside_the_forward_pass_re_derives_its_span_and_leaves_the_pass_unchanged() {
    let keeping = Keeping::new(50, usize::MAX);
    let alone = {
        let (mut h, [p, q]) =
            pulses_host(line(80, 3, 20, vec![]), 15, None, HostOptions::default());
        h.keep_checkpoints(p, keeping).unwrap();
        run_all(&mut h);
        (
            h.store(p).counts,
            h.store(q).counts,
            h.store(q).items().clone(),
            h.kept(p).unwrap(),
        )
    };
    let (mut h, [p, q]) = pulses_host(line(80, 3, 20, vec![]), 15, None, HostOptions::default());
    h.keep_checkpoints(p, keeping).unwrap();
    while h.consumed(p) < 1200 {
        h.step().unwrap();
    }
    assert!(h.finished(p).is_none(), "the forward pass is under way");
    let window = Span {
        start: 700,
        end: 800,
    };
    let w = h.derive(p, window).unwrap();
    assert_eq!(w.window.root(), p);
    assert!(
        w.restored
            .is_some_and(|t| t <= window.start && window.start - t < 50 + 15)
    );
    let id = w.window;
    assert_ne!(win(&h, id, p).id(), h.run_id(p), "a run of its own");
    assert!(win(&h, id, p).runnable(), "the window chain runs first");
    h.run_until(|h| win(h, id, p).consumed() >= window.end && win(h, id, p).quiet_below())
        .unwrap();
    assert!(
        h.finished(p).is_none(),
        "the forward pass waits behind the window"
    );
    let meets = |i: &escapement_tickwave_decode::Item| {
        i.start < window.end && i.end.is_none_or(|e| e > window.start)
    };
    let span =
        |items: &std::collections::BTreeMap<Id, escapement_tickwave_decode::Item>| -> Vec<_> {
            (items.iter())
                .filter(|(_, i)| meets(i))
                .map(|(id, i)| (*id, i.end))
                .collect()
        };
    let want = span(&alone.2);
    assert!(!want.is_empty());
    assert_eq!(
        span(win(&h, id, q).store().items()),
        want,
        "the window's items are the forward pass's"
    );
    h.retire(id);
    assert!(h.window_run(id, p).is_none() && h.window_run(id, q).is_none());
    run_all(&mut h);
    assert_eq!(
        (
            h.store(p).counts,
            h.store(q).counts,
            h.store(q).items().len(),
            h.kept(p).unwrap()
        ),
        (alone.0, alone.1, alone.2.len(), alone.3),
        "the forward pass's output and checkpoints as without the window"
    );
}

#[test]
fn a_window_keeps_an_item_open_across_its_checkpoint_when_the_forward_pass_kept_nothing() {
    let (mut all, [ap, aq]) =
        pulses_host(line(80, 3, 20, vec![]), 15, None, HostOptions::default());
    run_all(&mut all);
    // A checkpoint every tick, so that the nearest one falls inside a pulse, open from its rise to
    // its fall.
    let (mut h, [p, q]) = pulses_host(line(80, 3, 20, vec![]), 15, None, HostOptions::default());
    h.set_keep(p, vec![]);
    h.set_keep(q, vec![]);
    h.keep_checkpoints(p, Keeping::new(1, usize::MAX)).unwrap();
    run_all(&mut h);
    assert!(
        h.store(q).items().is_empty(),
        "the forward pass keeps nothing closed"
    );
    for start in [695, 700, 705, 711, 712] {
        let window = Span {
            start,
            end: start + 60,
        };
        let meets = |i: &escapement_tickwave_decode::Item| {
            i.start < window.end && i.end.is_none_or(|e| e > window.start)
        };
        let span =
            |items: &std::collections::BTreeMap<Id, escapement_tickwave_decode::Item>| -> Vec<_> {
                (items.iter())
                    .filter(|(_, i)| meets(i))
                    .map(|(id, i)| (*id, i.end, i.fields.len()))
                    .collect()
            };
        let w = h.derive(p, window).unwrap().window;
        h.run_until(|h| h.window_run(w, q).unwrap().settled() >= window.end)
            .unwrap();
        for (a, x) in [(ap, p), (aq, q)] {
            let want = span(all.store(a).items());
            let got = span(h.window_run(w, x).unwrap().store().items());
            assert_eq!(got, want, "the window at {start}");
        }
        if start == 711 {
            let pulses = span(all.store(ap).items());
            assert!(
                pulses.iter().any(|(id, ..)| id.anchor < start),
                "a pulse open at {start}"
            );
        }
        h.retire(w);
    }
}

#[test]
fn an_item_ending_past_a_checkpoints_input_is_kept_by_a_window_restored_from_it() {
    // Pulses fall at 13 past every 20 and end 5 later: the one falling at 73, read up to 75,
    // ends at 78, past the input the checkpoint at 75 had taken.
    let host = || pulses_host_tail(line(80, 3, 20, vec![]), 15, None, HostOptions::default(), 5);
    let (mut all, [ap, aq]) = host();
    run_all(&mut all);
    let (mut h, [p, q]) = host();
    h.set_keep(p, vec![]);
    h.set_keep(q, vec![]);
    h.keep_checkpoints(p, Keeping::new(1, usize::MAX)).unwrap();
    run_all(&mut h);
    let span = |items: &std::collections::BTreeMap<Id, escapement_tickwave_decode::Item>,
                w: Span|
     -> Vec<_> {
        (items.iter())
            .filter(|(_, i)| i.start < w.end && i.end.is_none_or(|e| e > w.start))
            .map(|(id, i)| (*id, i.end, i.fields.len()))
            .collect()
    };
    for start in 60..100 {
        let window = Span {
            start,
            end: start + 40,
        };
        let w = h.derive(p, window).unwrap();
        if start == 76 {
            assert_eq!(w.restored, Some(75));
        }
        let w = w.window;
        h.run_until(|h| win(h, w, q).settled() >= window.end)
            .unwrap();
        for (f, x) in [(ap, p), (aq, q)] {
            let want = span(all.store(f).items(), window);
            assert_eq!(
                span(win(&h, w, x).store().items(), window),
                want,
                "the window at {start}"
            );
        }
        h.retire(w);
    }
    assert!(
        h.store(p).items().is_empty() && h.store(q).items().is_empty(),
        "finished, the forward pass holds nothing past its spans"
    );
}

#[test]
fn a_read_past_the_hosts_bound_ends_early_and_the_output_is_the_same() {
    let gaps = vec![Span {
        start: 300,
        end: 420,
    }];
    let decoded = |options: HostOptions| {
        let (mut h, [p, q]) = pulses_host(line(80, 3, 20, gaps.clone()), 1 << 20, None, options);
        run_all(&mut h);
        (
            h.store(p).counts,
            h.store(q).counts,
            h.lost(p).to_vec(),
            h.lost(q).to_vec(),
        )
    };
    let want = decoded(HostOptions::default());
    for ticks in [1, 2, 3, 50] {
        assert_eq!(
            decoded(options(|o| o.window_ticks = ticks)),
            want,
            "reads of {ticks} ticks"
        );
    }
}

#[test]
fn two_live_window_chains_each_read_their_own_window() {
    let (mut all, [ap, aq]) =
        pulses_host(line(80, 3, 20, vec![]), 15, None, HostOptions::default());
    run_all(&mut all);
    let (mut h, [p, q]) = pulses_host(line(80, 3, 20, vec![]), 15, None, HostOptions::default());
    h.keep_checkpoints(p, Keeping::new(50, usize::MAX)).unwrap();
    run_all(&mut h);
    let windows = [(300, 360), (900, 960)].map(|(start, end)| Span { start, end });
    let span = |items: &std::collections::BTreeMap<Id, escapement_tickwave_decode::Item>,
                w: Span|
     -> Vec<_> {
        (items.iter())
            .filter(|(_, i)| i.start < w.end && i.end.is_none_or(|e| e > w.start))
            .map(|(id, i)| (*id, i.end, i.fields.len()))
            .collect()
    };
    let [a, b] = windows.map(|w| h.derive(p, w).unwrap().window);
    h.run_until(|h| {
        win(h, a, q).settled() >= windows[0].end && win(h, b, q).settled() >= windows[1].end
    })
    .unwrap();
    for (w, window) in [(a, windows[0]), (b, windows[1])] {
        for (f, x) in [(ap, p), (aq, q)] {
            let want = span(all.store(f).items(), window);
            assert!(!want.is_empty());
            assert_eq!(span(win(&h, w, x).store().items(), window), want);
        }
    }
    h.retire(a);
    assert!(
        h.window_run(a, q).is_none(),
        "the retired chain reads nothing"
    );
    assert_eq!(
        span(win(&h, b, q).store().items(), windows[1]),
        span(all.store(aq).items(), windows[1]),
        "the other chain is untouched"
    );
    // A third chain while the second is live: neither handle reads the other's runs.
    let c = h.derive(p, windows[0]).unwrap().window;
    h.run_until(|h| win(h, c, q).settled() >= windows[0].end)
        .unwrap();
    assert!(h.window_run(a, q).is_none());
    assert_ne!(win(&h, b, q).id(), win(&h, c, q).id());
    assert_eq!(
        span(win(&h, c, q).store().items(), windows[0]),
        span(all.store(aq).items(), windows[0])
    );
}

#[test]
fn retired_window_chains_leave_nothing_behind() {
    let (mut h, [p, q]) = pulses_host(line(80, 3, 20, vec![]), 15, None, HostOptions::default());
    h.keep_checkpoints(p, Keeping::new(50, usize::MAX)).unwrap();
    run_all(&mut h);
    let first = h
        .derive(
            p,
            Span {
                start: 300,
                end: 400,
            },
        )
        .unwrap();
    let first = first.window;
    let mut runs = vec![h.window_run(first, p).unwrap().id()];
    h.retire(first);
    for k in 0..300 {
        let start = 100 + (k * 7) % 1200;
        let w = h
            .derive(
                p,
                Span {
                    start,
                    end: start + 50,
                },
            )
            .unwrap()
            .window;
        assert!(
            h.window_run(first, q).is_none(),
            "a retired chain's handle reads nothing, window {k}"
        );
        runs.push(h.window_run(w, p).unwrap().id());
        h.run_until(|h| h.window_run(w, q).unwrap().settled() >= start + 50)
            .unwrap();
        h.retire(w);
    }
    let distinct: std::collections::HashSet<_> = runs.iter().copied().collect();
    assert_eq!(distinct.len(), 301, "no two chains share a run id");
    assert!(
        !h.store(q).items().is_empty(),
        "the forward chain is untouched"
    );
}

#[test]
fn the_forward_pass_takes_a_turn_after_a_run_of_window_turns() {
    let alone = {
        let (mut h, [p, q]) =
            pulses_host(line(80, 3, 20, vec![]), 15, None, HostOptions::default());
        run_all(&mut h);
        (h.store(p).counts, h.store(q).counts)
    };
    let run = |window_turns| {
        let opts = options(|o| o.window_turns = window_turns);
        let (mut h, [p, q]) = pulses_host(line(80, 3, 20, vec![]), 15, None, opts);
        h.keep_checkpoints(p, Keeping::new(50, usize::MAX)).unwrap();
        while h.consumed(p) < 1200 {
            h.step().unwrap();
        }
        let before = h.consumed(p);
        let window = Span {
            start: 300,
            end: 700,
        };
        let w = h.derive(p, window).unwrap().window;
        h.run_until(|h| h.window_run(w, q).unwrap().settled() >= window.end)
            .unwrap();
        let moved = h.consumed(p) - before;
        let items: Vec<_> = (h.window_run(w, q).unwrap().store().items().iter())
            .map(|(id, i)| (*id, i.end, i.fields.len()))
            .collect();
        h.retire(w);
        run_all(&mut h);
        (moved, items, (h.store(p).counts, h.store(q).counts))
    };
    let (strict, strict_items, strict_end) = run(usize::MAX);
    let (shared, shared_items, shared_end) = run(2);
    assert_eq!(strict, 0, "windows only, the forward pass waits");
    assert!(shared > 0, "it moves while the window runs");
    assert!(strict_items.len() > 3);
    assert_eq!(
        shared_items, strict_items,
        "the window's output whichever way"
    );
    assert_eq!(strict_end, alone);
    assert_eq!(
        shared_end, alone,
        "the forward pass's output as without the window"
    );
}

#[test]
fn a_window_ahead_of_the_forward_pass_is_provisional_until_the_pass_reaches_it() {
    let (mut all, [_, aq]) = pulses_host(line(80, 3, 20, vec![]), 15, None, HostOptions::default());
    run_all(&mut all);
    let (mut h, [p, q]) = pulses_host(line(80, 3, 20, vec![]), 15, None, HostOptions::default());
    h.keep_checkpoints(p, Keeping::new(50, usize::MAX)).unwrap();
    while h.consumed(p) < 300 {
        h.step().unwrap();
    }
    let window = Span {
        start: 1000,
        end: 1100,
    };
    let span =
        |items: &std::collections::BTreeMap<Id, escapement_tickwave_decode::Item>| -> Vec<_> {
            (items.iter())
                .filter(|(_, i)| i.start < window.end && i.end.is_none_or(|e| e > window.start))
                .map(|(id, i)| (*id, i.end, i.fields.len()))
                .collect()
        };
    let ahead = h.derive(p, window).unwrap();
    assert!(ahead.provisional && ahead.restored.is_none());
    let w = ahead.window;
    assert_eq!(
        win(&h, w, p).consumed(),
        window.start,
        "it starts at the window"
    );
    h.run_until(|h| win(h, w, q).settled() >= window.end)
        .unwrap();
    assert!(
        h.consumed(p) < window.start,
        "the forward pass has not reached it"
    );
    assert!(!win(&h, w, p).store().items().is_empty());
    h.retire(w);
    h.run_until(|h| h.consumed(p) >= window.end).unwrap();
    let again = h.derive(p, window).unwrap();
    assert!(!again.provisional && again.restored.is_some());
    let w = again.window;
    h.run_until(|h| win(h, w, q).settled() >= window.end)
        .unwrap();
    assert_eq!(
        span(win(&h, w, q).store().items()),
        span(all.store(aq).items()),
        "exact once the pass is past"
    );
}

#[test]
fn a_kind_declares_between_one_and_eight_lanes() {
    let add = |lanes| {
        let mut schema = toy_schema();
        schema.kinds[0] = schema.kinds[0].clone().with_lanes(lanes);
        let mut h = Host::default();
        h.add(
            name("toy"),
            definition("toy", schema),
            InputSpec::Wire {
                source: Box::new(Nothing::default()),
                local: None,
            },
            Box::new(|_, _| Box::new(Swallow)),
            &[],
            all(),
        )
        .map(|_| ())
    };
    assert!(add(1).is_ok() && add(escapement_tickwave_decode::MAX_LANES).is_ok());
    for lanes in [0, escapement_tickwave_decode::MAX_LANES + 1] {
        assert!(
            matches!(
                add(lanes),
                Err(BindError::Schema(SchemaError::Lanes { .. }))
            ),
            "{lanes} lanes"
        );
    }
}

#[test]
fn window_size_does_not_change_the_output() {
    let gaps = vec![
        Span {
            start: 101,
            end: 130,
        },
        Span {
            start: 300,
            end: 301,
        },
    ];
    let digest = |step| {
        let (mut h, [p, q]) = pulses_host(
            line(30, 3, 20, gaps.clone()),
            step,
            None,
            HostOptions::default(),
        );
        run_all(&mut h);
        (h.store(p).counts, h.store(q).counts, h.lost(p).to_vec())
    };
    let want = digest(1000);
    for step in [1, 7, 20, 64] {
        assert_eq!(digest(step), want, "windows of {step}");
    }
}

#[test]
fn a_decoder_that_leaves_its_deliveries_waits_for_new_input_instead_of_spinning() {
    struct Idle(Arc<std::sync::atomic::AtomicUsize>);
    impl Decoder for Idle {
        fn turn(&mut self, _: &mut Exchange<'_>) -> Pause {
            self.0.fetch_add(1, Relaxed);
            Pause::Input
        }
    }
    let (mut h, [p, _]) = pulses_host(line(6, 3, 20, vec![]), 35, None, HostOptions::default());
    let turns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = turns.clone();
    h.add(
        name("idle"),
        stacked(
            "idle",
            toy_schema(),
            UpstreamDecl::new("in", Mode::Completed).with_kinds(["pulse"]),
        ),
        InputSpec::Upstream(p),
        Box::new(move |_, _| Box::new(Idle(count.clone()))),
        &[],
        all(),
    )
    .unwrap();
    let mut steps = 0;
    while h.step().unwrap() {
        steps += 1;
        assert!(
            steps < 1000,
            "the host keeps running a decoder that took nothing"
        );
    }
    assert_eq!(h.finished(p), Some(130));
    let upstream = h.store(p).counts;
    assert!(
        turns.load(Relaxed) as u64 <= upstream.closes + 8,
        "one turn per arrival, not per step: {} turns",
        turns.load(Relaxed)
    );
}

#[test]
fn names_and_phrases_have_bounds_of_their_own() {
    let add = |schema: Schema| {
        let mut def = (*toy()).clone();
        def.schema = schema;
        Host::default()
            .add(
                name("toy"),
                Arc::new(def),
                InputSpec::Wire {
                    source: Box::new(Nothing::default()),
                    local: None,
                },
                Box::new(|_, _| Box::new(script(vec![]))),
                &[],
                all(),
            )
            .err()
    };
    let long = |n: usize| "x".repeat(n);
    let mut s = toy_schema();
    s.reasons = vec![long(200).into()];
    assert_eq!(add(s), None, "a reason is a sentence");
    let mut s = toy_schema();
    s.causes = vec![long(257).into()];
    assert_eq!(add(s), Some(BindError::Schema(SchemaError::BadPhrase)));
    let mut s = toy_schema();
    s.roles = vec![long(65).into()];
    assert_eq!(add(s), Some(BindError::Schema(SchemaError::BadName)));
}

#[test]
fn a_source_error_keeps_the_recordings_error_as_its_source() {
    use std::error::Error;
    let inner = std::io::Error::other("the file is cut off");
    let e = escapement_tickwave_decode::SourceError::new(inner);
    let source = e.source().map(|s| s.to_string());
    assert_eq!(source.as_deref(), Some("the file is cut off"));
    assert!(
        !e.to_string().contains("cut off"),
        "a reporter walking the chain shows the recording's error once"
    );
}

/// A source with nothing on it and `.0` its ports, true where one has a channel.
struct Ports(Vec<bool>);

impl Source for Ports {
    fn window(
        &mut self,
        from: Tick,
        upto: Tick,
        _: usize,
        _: &std::sync::atomic::AtomicBool,
    ) -> Result<Option<Window>, SourceError> {
        Ok(Some(Window::new(from, upto)))
    }

    fn binding(&self) -> u64 {
        0
    }

    fn ports(&self) -> Vec<bool> {
        self.0.clone()
    }
}

fn configured(settings: Vec<SettingDecl>) -> Arc<Definition> {
    Arc::new(
        Definition::new("set", 1, pulses_schema())
            .with_ports(["clock"])
            .with_optional_ports(["data"])
            .with_settings(settings),
    )
}

fn three() -> Vec<SettingDecl> {
    vec![
        SettingDecl::int("cycles", 1..=4, 1),
        SettingDecl::choice("edge", ["rise", "fall"], 0),
        SettingDecl::flag("assume", true),
        SettingDecl::list("lengths", 1..=8, 3, vec![]),
    ]
}

/// The bound a decoder of `def` is made with, given `settings` on a source of `ports`.
fn bound(
    def: Arc<Definition>,
    ports: Vec<bool>,
    settings: &[(&str, Setting)],
) -> Result<Bound, BindError> {
    let seen = Arc::new(Mutex::new(None));
    let s = seen.clone();
    Host::default().add(
        name("set"),
        def,
        InputSpec::Wire {
            source: Box::new(Ports(ports)),
            local: None,
        },
        Box::new(move |b, _| {
            *s.lock().unwrap() = Some(b.clone());
            Box::new(Swallow)
        }),
        settings,
        all(),
    )?;
    Ok(seen.lock().unwrap().take().expect("the decoder was made"))
}

#[test]
fn a_decoder_is_made_with_the_settings_given_by_name_and_the_rest_at_their_defaults() {
    let (cycles, edge, assume) = (SettingId(0), SettingId(1), SettingId(2));
    let given = [
        ("edge", Setting::Name(name("fall"))),
        ("cycles", Setting::Int(3)),
    ];
    let b = bound(configured(three()), vec![true, true], &given).unwrap();
    assert_eq!(
        (b.int(cycles), b.choice(edge), b.flag(assume)),
        (Some(3), Some(1), Some(true))
    );
    assert_eq!(
        (b.flag(cycles), b.int(edge), b.int(SettingId(4))),
        (None, None, None)
    );
    let lengths = SettingId(3);
    assert_eq!(b.list(lengths), Some(&[][..]));
    assert_eq!(b.list(cycles), None);
    let b = bound(
        configured(three()),
        vec![true, true],
        &[("lengths", Setting::List(vec![4, 6]))],
    )
    .unwrap();
    assert_eq!(b.list(lengths), Some(&[4, 6][..]));
    assert!(b.port(0) && b.port(1) && !b.port(2));
    let b = bound(configured(three()), vec![true, true], &[]).unwrap();
    assert_eq!((b.int(cycles), b.choice(edge)), (Some(1), Some(0)));

    let refused = |given: &[(&str, Setting)]| bound(configured(three()), vec![true, true], given);
    let value = |n: &'static str| Err(BindError::SettingValue(name(n)));
    assert_eq!(refused(&[("cycles", Setting::Int(5))]), value("cycles"));
    assert_eq!(refused(&[("cycles", Setting::Flag(true))]), value("cycles"));
    // A list's entries each in its range, and no more of them than it takes.
    for list in [vec![0], vec![9], vec![1, 2, 3, 4]] {
        assert_eq!(
            refused(&[("lengths", Setting::List(list))]),
            value("lengths")
        );
    }
    assert!(refused(&[("lengths", Setting::List(vec![1, 8, 8]))]).is_ok());
    assert_eq!(
        refused(&[("edge", Setting::Name(name("both")))]),
        value("edge")
    );
    assert_eq!(
        refused(&[("speed", Setting::Int(1))]),
        Err(BindError::UnknownSetting(name("speed")))
    );
    assert_eq!(
        refused(&[
            ("assume", Setting::Flag(false)),
            ("assume", Setting::Flag(true))
        ]),
        Err(BindError::SettingTwice(name("assume")))
    );
}

#[test]
fn a_binding_may_leave_out_an_optional_port_and_no_other() {
    let b = bound(configured(vec![]), vec![true, false], &[]).unwrap();
    assert!(b.port(0) && !b.port(1));
    assert_eq!(
        bound(configured(vec![]), vec![false, true], &[]),
        Err(BindError::Unbound(name("clock")))
    );
    assert_eq!(
        bound(configured(vec![]), vec![true], &[]),
        Err(BindError::Ports {
            declared: 2,
            given: 1
        })
    );
}

#[test]
fn a_definition_whose_settings_or_ports_clash_is_refused() {
    let refused = |settings: Vec<SettingDecl>| bound(configured(settings), vec![true, true], &[]);
    let schema = |e| Err(BindError::Schema(e));
    assert_eq!(
        refused(vec![SettingDecl::int("n", 1..=4, 0)]),
        schema(SchemaError::SettingDefault(name("n")))
    );
    assert_eq!(
        refused(vec![SettingDecl::choice("n", ["a", "b"], 2)]),
        schema(SchemaError::SettingDefault(name("n")))
    );
    assert_eq!(
        refused(vec![SettingDecl::choice("n", Vec::<Name>::new(), 0)]),
        schema(SchemaError::SettingDefault(name("n")))
    );
    // A list's default out of its range or longer than it takes, and one taking more than the
    // contract's lists hold.
    for default in [vec![0], vec![1, 1, 1]] {
        assert_eq!(
            refused(vec![SettingDecl::list("n", 1..=4, 2, default)]),
            schema(SchemaError::SettingDefault(name("n")))
        );
    }
    assert!(matches!(
        refused(vec![SettingDecl::list("n", 1..=4, 257, vec![])]),
        Err(BindError::Schema(SchemaError::TooMany { .. }))
    ));
    assert_eq!(
        refused(vec![SettingDecl::choice("n", ["a", "a"], 0)]),
        schema(SchemaError::DuplicateChoice {
            setting: name("n"),
            name: name("a")
        })
    );
    assert_eq!(
        refused(vec![
            SettingDecl::flag("n", false),
            SettingDecl::flag("n", true)
        ]),
        schema(SchemaError::DuplicateSetting(name("n")))
    );
    assert_eq!(
        refused(vec![SettingDecl::flag("", false)]),
        schema(SchemaError::BadName)
    );
    let twice = Arc::new(
        Definition::new("set", 1, pulses_schema())
            .with_ports(["clock"])
            .with_optional_ports(["clock"]),
    );
    assert_eq!(
        bound(twice, vec![true, true], &[]),
        schema(SchemaError::DuplicatePort(name("clock")))
    );
    let inputs = |inputs: Vec<InputDecl>| {
        let def = Definition::new("set", 1, pulses_schema()).with_ports(["clock", "data"]);
        bound(Arc::new(def.with_inputs(inputs)), vec![true, true], &[])
    };
    let up = |port| InputDecl::Upstream(UpstreamDecl::new(port, Mode::Completed));
    assert_eq!(inputs(vec![]), schema(SchemaError::NoInputs));
    assert_eq!(
        inputs(vec![InputDecl::Wire, InputDecl::Wire]),
        schema(SchemaError::WireTwice)
    );
    assert_eq!(
        inputs(vec![InputDecl::Wire, up("in"), up("other")]),
        schema(SchemaError::AnyTwice)
    );
    let from = |port, f: &'static str| {
        InputDecl::Upstream(UpstreamDecl::new(port, Mode::Completed).from([f]))
    };
    assert_eq!(
        inputs(vec![from("in", "swd"), from("other", "swd")]),
        schema(SchemaError::TakenTwice(name("swd")))
    );
    // One port name over several upstreams, each its own alternative.
    let alternatives = vec![
        InputDecl::Wire,
        from("in", "swd"),
        from("in", "jtag-dp"),
        up("in"),
    ];
    assert!(inputs(alternatives).is_ok());
    assert_eq!(inputs(vec![up("")]), schema(SchemaError::BadName));
}

#[test]
fn an_upstream_binds_to_the_input_naming_its_definition_and_the_decoder_is_told_which() {
    let (mut h, [p, _]) = pulses_host(line(1, 2, 10, vec![]), 100, None, HostOptions::default());
    let picked = Arc::new(Mutex::new(vec![]));
    let mut add = |n: &'static str, inputs: Vec<InputDecl>| {
        let def = Definition::new("pairs", 1, pairs_schema()).with_inputs(inputs);
        let got = picked.clone();
        let factory: escapement_tickwave_decode::Factory = Box::new(move |b: &Bound, _| {
            got.lock().unwrap().push(b.input());
            Box::new(Swallow)
        });
        h.add(
            name(n),
            Arc::new(def),
            InputSpec::Upstream(p),
            factory,
            &[],
            all(),
        )
    };
    let named = |port: &'static str, from: &'static str| {
        InputDecl::Upstream(
            UpstreamDecl::new(port, Mode::Completed)
                .from([from])
                .with_kinds(["pulse"]),
        )
    };
    let any =
        || InputDecl::Upstream(UpstreamDecl::new("any", Mode::Completed).with_kinds(["pulse"]));
    // The input naming "pulses" wins over one taking any, wherever each sits.
    add(
        "a",
        vec![
            InputDecl::Wire,
            any(),
            named("other", "jtag"),
            named("in", "pulses"),
        ],
    )
    .unwrap();
    // With none naming it, the first taking any.
    add("b", vec![named("other", "jtag"), any()]).unwrap();
    assert_eq!(*picked.lock().unwrap(), vec![3, 1]);
    assert_eq!(
        add("c", vec![InputDecl::Wire, named("other", "jtag")]).unwrap_err(),
        BindError::NotAnInput {
            upstream: name("pulses"),
            definition: name("pulses")
        }
    );
    // A wire binding of a decoder that only stacks.
    let stacked_only = Definition::new("pairs", 1, pairs_schema()).with_inputs([any()]);
    let wire = h.add(
        name("d"),
        Arc::new(stacked_only),
        InputSpec::Wire {
            source: Box::new(Nothing::default()),
            local: None,
        },
        Box::new(|_, _| Box::new(Swallow)),
        &[],
        all(),
    );
    assert_eq!(wire.unwrap_err(), BindError::NoWireInput);
}

/// A stacked binding refused for its settings leaves the host as it was, and the host runs on.
#[test]
fn a_refused_stacked_binding_leaves_its_upstream_as_it_was() {
    let (mut h, [p, q]) = pulses_host(line(1, 2, 10, vec![]), 100, None, HostOptions::default());
    let def = Definition::new("set", 1, pulses_schema()).with_inputs([UpstreamDecl::new(
        "in",
        Mode::Completed,
    )
    .with_kinds(["pulse"])]);
    let refused = h.add(
        name("stacked"),
        Arc::new(def),
        InputSpec::Upstream(p),
        Box::new(|_, _| Box::new(Swallow)),
        &[("none", Setting::Flag(true))],
        all(),
    );
    assert_eq!(
        refused.unwrap_err(),
        BindError::UnknownSetting(name("none"))
    );
    h.run_until(|_| false).unwrap();
    assert!(h.finished(p).is_some() && h.finished(q).is_some());
}

#[test]
fn a_checkpoint_restores_only_where_the_settings_are_the_same() {
    let host = |cycles: i64| {
        let def = Arc::new(
            Definition::new("pulses", 1, pulses_schema())
                .with_ports(["line"])
                .with_settings(vec![SettingDecl::int("cycles", 1..=4, 1)]),
        );
        let mut h = Host::default();
        let p = h
            .add(
                name("pulses"),
                def,
                InputSpec::Wire {
                    source: Box::new(line(20, 3, 20, vec![])),
                    local: None,
                },
                Box::new(|_, local| Box::new(Pulses::new(local, 15))),
                &[("cycles", Setting::Int(cycles))],
                all(),
            )
            .unwrap();
        (h, p)
    };
    let (mut h, p) = host(2);
    h.run_until(|h| h.consumed(p) >= 100).unwrap();
    let cut = h.checkpoint(p).unwrap();
    assert!(host(2).0.restore(&cut).is_ok());
    assert!(matches!(
        host(3).0.restore(&cut),
        Err(CheckpointError::Incompatible)
    ));
}

#[test]
fn a_label_that_shows_a_field_its_kind_does_not_declare_is_refused() {
    let schema = |labels: Vec<escapement_tickwave_decode::Label>| {
        Schema::new(vec![
            KindDecl::new("a")
                .with_fields(vec![
                    field("n", FieldType::Word { bits: 8 }).with_short("N"),
                ])
                .with_labels(labels),
        ])
    };
    let add = |schema: Schema| {
        Host::default().add(
            name("a"),
            Arc::new(Definition::new("a", 1, schema).with_ports(["line"])),
            InputSpec::Wire {
                source: Box::new(Nothing::default()),
                local: None,
            },
            Box::new(|_, _| Box::new(Swallow)),
            &[],
            all(),
        )
    };
    let label = escapement_tickwave_decode::Label::default;
    assert!(add(schema(vec![label().text("n ").field(FieldId(0))])).is_ok());
    assert!(matches!(
        add(schema(vec![label().field(FieldId(1))])),
        Err(BindError::Schema(SchemaError::LabelField(k))) if k == "a"
    ));
    assert!(matches!(
        add(schema(vec![label().text("")])),
        Err(BindError::Schema(SchemaError::BadName))
    ));
    assert!(matches!(
        add(schema(vec![
            label();
            escapement_tickwave_decode::MAX_LABELS + 1
        ])),
        Err(BindError::Schema(SchemaError::TooMany { .. }))
    ));
}

#[test]
fn a_kind_reads_as_text_from_a_field_of_bytes_it_declares() {
    let schema = |ty: FieldType, text: u16| {
        Schema::new(vec![
            KindDecl::new("a")
                .with_fields(vec![field("data", ty)])
                .with_text(FieldId(text)),
        ])
    };
    let add = |schema: Schema| {
        Host::default().add(
            name("a"),
            Arc::new(Definition::new("a", 1, schema).with_ports(["line"])),
            InputSpec::Wire {
                source: Box::new(Nothing::default()),
                local: None,
            },
            Box::new(|_, _| Box::new(Swallow)),
            &[],
            all(),
        )
    };
    assert!(add(schema(FieldType::Word { bits: 8 }, 0)).is_ok());
    assert!(add(schema(FieldType::Bits { max: 64 }, 0)).is_ok());
    for (ty, at) in [
        (FieldType::Word { bits: 16 }, 0),
        (FieldType::Bool, 0),
        (FieldType::Word { bits: 8 }, 1),
    ] {
        assert!(matches!(
            add(schema(ty, at)),
            Err(BindError::Schema(SchemaError::TextField(k))) if k == "a"
        ));
    }
}
