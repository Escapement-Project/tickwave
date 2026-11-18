//! The host's rules, on synthetic analyses.

mod common;

use common::*;
use escapement_tickwave_decode::{
    AnalysisId, Batch, BindError, CONTRACT, Cause, CheckpointError, Contract, Decoder, Definition,
    Element, ElementType, Exchange, FieldDecl, FieldId, FieldType, Host, HostOptions, Id,
    InputSpec, KindDecl, KindId, LinkDecl, LinkId, LinkTo, LogEntry, Mismatch, Mode, Name, Need,
    Observation, Op, Pause, Reason, Ref, Refusal, Refused, Reply, Role, Schema, SchemaError,
    SeqDecl, SeqId, Span, Status, Subscription, Tick, Value, field, name,
};
use std::sync::{Arc, atomic::Ordering::Relaxed};

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
            Box::new(move |_| Box::new(script(turns.clone()))),
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
            Box::new(|_| Box::new(script(vec![]))),
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
    assert_eq!(
        add(&mut h, "c", with(CONTRACT)).unwrap(),
        AnalysisId(0),
        "a decoder of the host's own contract is added"
    );
    let message = add(&mut h, "d", with(newer)).unwrap_err().to_string();
    assert!(
        message.contains("1.1") && message.contains("1.0"),
        "{message}"
    );
}

#[test]
fn a_schema_and_its_subscriptions_are_checked_when_added() {
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
            Box::new(|_| Box::new(script(vec![]))),
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
                definition("pairs", pairs_schema()),
                InputSpec::Upstream(
                    Subscription::new(p, mode)
                        .with_kinds(kinds)
                        .with_sequences(sequences),
                ),
                Box::new(|_| Box::new(Pairs::default())),
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
    let missing = h.add(
        name("y"),
        definition("pairs", pairs_schema()),
        InputSpec::Upstream(Subscription::new(AnalysisId(9), Mode::Completed)),
        Box::new(|_| Box::new(Pairs::default())),
        all(),
    );
    assert_eq!(missing.unwrap_err(), BindError::NoAnalysis(AnalysisId(9)));
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
        h.store(a).items[&f].fields.is_empty(),
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
                    bytes: vec![0; 2],
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
    let item = &h.store(a).items[&f];
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
            Box::new(move |_| Box::new(script(turns.clone()))),
            vec![Span { start: 0, end: 15 }],
        )
        .unwrap();
    let sink = h
        .add(
            name("sink"),
            definition("sink", toy_schema()),
            InputSpec::Upstream(Subscription::new(a, Mode::Pieces).with_kinds(vec![name("frame")])),
            Box::new(|_| Box::new(script(vec![]))),
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
            Box::new(move |_| Box::new(script(turns.clone()))),
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
    let pairs = &h.store(q).items;
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
            Box::new(|local| Box::new(Pulses::new(local, 50))),
            all(),
        )
        .unwrap();
    let q = h
        .add(
            name("pairs"),
            definition("pairs", pairs_schema()),
            InputSpec::Upstream(
                Subscription::new(p, Mode::Completed).with_kinds(vec![name("pulse")]),
            ),
            Box::new(|_| Box::new(Swallow)),
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
            definition(n, toy_schema()),
            InputSpec::Upstream(
                Subscription::new(p, Mode::Pieces)
                    .with_kinds(vec![name("pulse")])
                    .with_sequences(sequences),
            ),
            Box::new(|_| Box::new(Swallow)),
            all(),
        )
        .unwrap()
    };
    let bare = subscribe(&mut h, "bare", vec![]);
    let levels = subscribe(&mut h, "levels", vec!["levels"]);
    run_all(&mut h);
    assert_eq!(h.store(p).counts.closes, 6);
    assert_eq!(h.store(q).counts.closes, 3, "three pairs");
    let pair = h.store(q).items.values().next().unwrap();
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
            Box::new(|_| Box::new(Plain)),
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
            Box::new(|_| {
                Box::new(script(vec![
                    vec![Act::Request(0, 10)],
                    vec![Act::TakeReply],
                ]))
            }),
            all(),
        )
        .unwrap();
    h.step().unwrap();
    let id = h.outstanding(a).unwrap();
    h.interrupt(a).store(true, Relaxed);
    assert!(h.take_io().unwrap().is_none());
    assert_eq!(h.cancelled_reads, 1);
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
    assert_eq!(h.stale, 1);
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
        h.admitted
    };
    let options = options(|o| o.log = Some(16));
    let (mut h, _) = pulses_host(line(40, 3, 20, vec![]), 30, None, options);
    run_all(&mut h);
    assert_eq!(h.admitted, quiet);
    assert_eq!(h.log().count(), 16);
    assert!(h.log_dropped > 0);
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
                Box::new(|local| Box::new(Pulses::new(local, 25))),
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
        definition("idle", toy_schema()),
        InputSpec::Upstream(Subscription::new(p, Mode::Completed).with_kinds(vec![name("pulse")])),
        Box::new(move |_| Box::new(Idle(count.clone()))),
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
                Box::new(|_| Box::new(script(vec![]))),
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
    assert!(e.to_string().contains("the file is cut off"));
}
