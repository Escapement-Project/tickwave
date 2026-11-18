//! SWD, ADIv5 and MEM-AP on synthetic wires.

mod common;

use common::*;
use escapement_tickwave_decode::{
    AnalysisId, BindError, Counts, FieldId, Host, HostOptions, InputSpec, Item, KindId, Ref, Span,
    Status, Tick, Value, name,
};
use escapement_tickwave_decoders::{adiv5, swd};

const P: Tick = 10;

fn all() -> Vec<Span> {
    vec![Span {
        start: Tick::MIN,
        end: Tick::MAX,
    }]
}

fn host(w: Wires, window: Tick, local: Option<Tick>) -> (Host, [AnalysisId; 3]) {
    host_with(HostOptions::default(), w, window, local)
}

fn host_with(
    options: HostOptions,
    w: Wires,
    window: Tick,
    local: Option<Tick>,
) -> (Host, [AnalysisId; 3]) {
    let mut h = Host::new(options);
    let s = h
        .add(
            name("swd"),
            swd::definition(),
            InputSpec::Wire {
                source: Box::new(w),
                local,
            },
            swd::Swd::factory(window),
            &[],
            all(),
        )
        .unwrap();
    let a = h
        .add(
            name("adi"),
            adiv5::adi_definition(),
            InputSpec::Upstream(adiv5::adi_subscription(s)),
            adiv5::Adi::factory(),
            &[],
            all(),
        )
        .unwrap();
    let m = h
        .add(
            name("mem"),
            adiv5::mem_definition(),
            InputSpec::Upstream(adiv5::mem_subscription(a)),
            adiv5::Mem::factory(),
            &[],
            all(),
        )
        .unwrap();
    (h, [s, a, m])
}

fn run(h: &mut Host) {
    h.run_until(|_| false).unwrap();
}

fn decoded(w: Wires, window: Tick) -> [Counts; 3] {
    decoded_with(HostOptions::default(), w, window)
}

fn decoded_with(options: HostOptions, w: Wires, window: Tick) -> [Counts; 3] {
    let (mut h, a) = host_with(options, w, window, None);
    run(&mut h);
    a.map(|a| h.store(a).counts)
}

/// Host options that bound each read to `ticks` ticks of observations.
fn reads_of(ticks: usize) -> HostOptions {
    let mut o = HostOptions::default();
    o.window_ticks = ticks;
    o
}

fn of(h: &Host, a: AnalysisId, kind: KindId) -> Vec<Item> {
    let mut items: Vec<Item> = (h.store(a).items.values())
        .filter(|i| i.id.kind == kind)
        .cloned()
        .collect();
    items.sort_by_key(|i| i.start);
    items
}

fn value(i: &Item, f: FieldId) -> Option<Value> {
    i.field(f).map(|f| f.value.clone())
}

fn word(i: &Item, f: FieldId) -> Option<u64> {
    match value(i, f)? {
        Value::Word { value, .. } => Some(value),
        _ => None,
    }
}

fn named(i: &Item, f: FieldId) -> Option<u16> {
    match value(i, f)? {
        Value::Name(n) => Some(n),
        _ => None,
    }
}

const DPIDR: u32 = 0x0BC1_1477;
const IDR: u32 = 0x2477_0011;

fn session() -> Vec<T> {
    let wait = T {
        ack: 2,
        ..ap_read(0xC, 0)
    };
    vec![
        dp_read(0x0, DPIDR),
        dp_write(0x8, 0x0000_00F0),
        ap_read(0xC, 0),
        dp_read(0xC, IDR),
        wait,
        ap_read(0xC, 0),
    ]
}

#[test]
fn transfers_decode_as_encoded() {
    let ts = session();
    let (b, _) = bits(&ts);
    let (mut h, [s, a, m]) = host(wires(&b, P), 100, None);
    run(&mut h);
    for x in [s, a, m] {
        fits_its_rows(&h, x);
    }
    let got = of(&h, s, swd::TRANSFER);
    assert_eq!(got.len(), ts.len());
    for (i, t) in got.iter().zip(&ts) {
        let port = if t.ap { swd::AP } else { swd::DP };
        let dir = if t.read { swd::READ } else { swd::WRITE };
        assert_eq!(named(i, swd::PORT), Some(port));
        assert_eq!(named(i, swd::DIR), Some(dir));
        assert_eq!(word(i, swd::A), Some(u64::from(t.addr)));
        let ack = if t.ack == 1 { 0 } else { 1 };
        assert_eq!(named(i, swd::ACK), Some(ack));
        if t.ack == 1 {
            assert_eq!(word(i, swd::DATA), Some(u64::from(t.data)));
            assert_eq!(value(i, swd::PARITY), Some(Value::Name(swd::PARITY_OK)));
            assert_eq!(i.seq(swd::DATA_BITS).len(), 32);
        }
        assert_eq!(i.status, Some(Status::Complete));
    }
    assert_eq!(
        got[5].links,
        vec![(swd::RETRY_OF, Ref::Own(got[4].id))],
        "the retry links to the WAIT it repeats"
    );
    let dp = of(&h, a, adiv5::DP_READ);
    assert_eq!(named(&dp[0], adiv5::DP_REG), Some(0), "DPIDR");
    assert_eq!(word(&dp[0], adiv5::DP_VALUE), Some(u64::from(DPIDR)));
    let reads = of(&h, a, adiv5::AP_READ);
    assert_eq!(reads.len(), 2);
    let idr = &reads[0];
    assert_eq!(word(idr, adiv5::AP_REG), Some(0xFC));
    assert_eq!(named(idr, adiv5::AP_NAME), Some(10), "IDR");
    assert_eq!(
        word(idr, adiv5::AP_VALUE),
        Some(u64::from(IDR)),
        "from RDBUFF"
    );
    assert_eq!(
        idr.links,
        vec![
            (adiv5::R_REQUEST, Ref::Upstream(got[2].id)),
            (adiv5::R_CONTEXT, Ref::Own(of(&h, a, adiv5::DP_WRITE)[0].id)),
            (adiv5::R_DATA, Ref::Upstream(got[3].id)),
        ]
    );
    assert_eq!(
        named(idr, adiv5::AP_CLASS),
        Some(adiv5::MEM_AP),
        "its own IDR"
    );
    assert_eq!(
        reads[1].status,
        Some(Status::Incomplete(adiv5::ENDS_UNREAD)),
        "the last read is never read back"
    );
    assert_eq!(
        named(&reads[1], adiv5::AP_CLASS),
        Some(adiv5::MEM_AP),
        "an IDR read never read back has the class known before it"
    );
}

/// An Arm JTAG-AP's IDR: revision 2, Arm, no class, variant 1, type 0.
const JTAG_AP_IDR: u32 = 0x2476_0010;

#[test]
fn ap_registers_are_named_by_the_class_an_idr_read_gives() {
    let ts = vec![
        dp_write(0x8, 0x0000_0000),
        ap_write(0x4, 0x0000_0001),
        dp_write(0x8, 0x0000_00F0),
        ap_read(0xC, 0),
        dp_read(0xC, JTAG_AP_IDR),
        dp_write(0x8, 0x0000_0000),
        ap_write(0x4, 0x0000_0001),
        ap_write(0xC, 0x0000_0002),
        dp_write(0x8, 0x0100_00F0),
        ap_read(0xC, 0),
        dp_read(0xC, IDR),
        dp_write(0x8, 0x0100_0000),
        ap_write(0x4, 0x2000_0000),
        ap_write(0xC, 0x0000_0003),
        dp_write(0x8, 0x0200_00F0),
        ap_read(0xC, 0),
        dp_read(0xC, 0x2477_0111),
    ];
    let (b, _) = bits(&ts);
    let (mut h, [_, a, m]) = host(wires(&b, P), 100, None);
    run(&mut h);
    let writes = of(&h, a, adiv5::AP_WRITE);
    let got: Vec<_> = (writes.iter())
        .map(|i| (named(i, adiv5::AP_NAME), value(i, adiv5::AP_CLASS)))
        .collect();
    let class = |n| Some(Value::Name(n));
    assert_eq!(
        got,
        [
            (
                None,
                Some(Value::Unknown {
                    reason: adiv5::NO_IDR,
                    needs: Some(adiv5::NEEDS_IDR),
                })
            ),
            (Some(11), class(adiv5::JTAG_AP)),
            (None, class(adiv5::JTAG_AP)),
            (Some(1), class(adiv5::MEM_AP)),
            (Some(3), class(adiv5::MEM_AP)),
        ],
        "PSEL, a JTAG-AP's reserved 0x0C, then a MEM-AP's TAR and DRW"
    );
    let reads = of(&h, a, adiv5::AP_READ);
    let classes: Vec<_> = reads.iter().map(|i| value(i, adiv5::AP_CLASS)).collect();
    let unidentifiable = Some(Value::Unknown {
        reason: adiv5::UNIDENTIFIABLE,
        needs: None,
    });
    assert_eq!(
        classes,
        [class(adiv5::JTAG_AP), class(adiv5::MEM_AP), unidentifiable],
        "an IDR with reserved bits set leaves AP 2 unidentifiable"
    );
    let mem = of(&h, m, adiv5::MEM_WRITE);
    let got: Vec<_> = (mem.iter())
        .map(|i| (word(i, adiv5::ADDRESS), word(i, adiv5::MEM_VALUE)))
        .collect();
    assert_eq!(
        got,
        [(Some(0x2000_0000), Some(3))],
        "MEM-AP skips the JTAG-AP, and an AP not yet identified writes at no TAR"
    );
}

#[test]
fn an_aps_class_outlasts_a_gap() {
    let ts = vec![
        dp_write(0x8, 0x0000_00F0),
        ap_read(0xC, 0),
        dp_read(0xC, JTAG_AP_IDR),
        dp_write(0x8, 0x0000_0000),
        dp_write(0x8, 0x0000_0000),
        ap_write(0x4, 0x0000_0001),
    ];
    let (b, starts) = bits(&ts);
    // Through the first SELECT write after the IDR comes back.
    let gap = Span {
        start: rise(starts[3] + 20, P),
        end: rise(starts[4] - 1, P),
    };
    let mut w = wires(&b, P);
    w.lines[1].gaps.push(gap);
    let (mut h, [_, a, _]) = host(w, 64, None);
    run(&mut h);
    let writes = of(&h, a, adiv5::AP_WRITE);
    assert_eq!(writes.len(), 1);
    assert_eq!(named(&writes[0], adiv5::AP_CLASS), Some(adiv5::JTAG_AP));
    assert_eq!(named(&writes[0], adiv5::AP_NAME), Some(11), "PSEL");
}

#[test]
fn an_idr_gives_the_class_table_c1_3_does() {
    let cases = [
        (0x0000_0000, Ok(adiv5::ABSENT)),
        (IDR, Ok(adiv5::MEM_AP)),
        (0x8477_0001, Ok(adiv5::MEM_AP)),
        (JTAG_AP_IDR, Ok(adiv5::JTAG_AP)),
        (0x2476_2000, Ok(adiv5::COM_AP)),
        // Arm, no class, but variant zero: not a JTAG-AP.
        (0x2476_0000, Ok(adiv5::OTHER)),
        // Another designer's AP of no class.
        (0x002E_0010, Ok(adiv5::OTHER)),
        // An early Arm DAP: no designer, CLASS reads as zero, TYPE says AHB or JTAG.
        (0x0000_0001, Ok(adiv5::MEM_AP)),
        (0x0000_0010, Ok(adiv5::JTAG_AP)),
        // Reserved bits 12:8 set.
        (0x2477_0111, Err(adiv5::UNIDENTIFIABLE)),
    ];
    for (idr, want) in cases {
        assert_eq!(adiv5::ap_class(idr), want, "IDR 0x{idr:08X}");
    }
}

#[test]
fn window_size_does_not_change_the_output() {
    let (b, _) = bits(&session());
    let want = decoded(wires(&b, P), 1 << 20);
    for window in [1, 7, 64, 333, 5000] {
        assert_eq!(decoded(wires(&b, P), window), want, "windows of {window}");
    }
    for ticks in [1, 2, 5, 64] {
        let got = decoded_with(reads_of(ticks), wires(&b, P), 1 << 20);
        assert_eq!(got, want, "reads of {ticks} ticks");
    }
}

#[test]
fn a_data_change_at_a_rise_counts_after_it_in_either_order() {
    let ts = session();
    let (b, starts) = bits(&ts);
    // A data bit of the IDR read that differs from the bit before it.
    let first = starts[3] + 12;
    let k = (first + 1..first + 32).find(|&k| b[k] != b[k - 1]).unwrap();
    let mut moved = wires(&b, P);
    let at = rise(k, P) - P / 2;
    let e = moved.lines[1].edges.iter().position(|&x| x == at).unwrap();
    moved.lines[1].edges[e] = rise(k, P);
    let mut held = b.clone();
    held[k] = b[k - 1];
    let want = decoded(wires(&held, P), 100);
    assert_eq!(decoded(moved.clone(), 100), want);
    moved.reversed = true;
    assert_eq!(
        decoded(moved, 100),
        want,
        "the order of coincident changes does not matter"
    );
}

#[test]
fn a_gap_is_lost_input_that_every_stacked_analysis_declares_again() {
    let mut ts = session();
    ts[2].data = 0;
    let (b, starts) = bits(&ts);
    let gap = Span {
        start: rise(starts[2] + 20, P),
        end: rise(starts[3] - 1, P),
    };
    let mut w = wires(&b, P);
    w.lines[1].gaps.push(gap);
    let (mut h, [s, a, m]) = host(w, 64, None);
    run(&mut h);
    for x in [s, a, m] {
        assert_eq!(h.lost(x), &[gap], "{}", h.name(x));
    }
    let got = of(&h, s, swd::TRANSFER);
    assert_eq!(
        got.len(),
        ts.len() - 1,
        "the transfer the gap cuts is not framed"
    );
    assert!(
        got.iter()
            .all(|i| i.end.unwrap() <= gap.start || i.start >= gap.end)
    );
    let contexts = of(&h, a, adiv5::CONTEXT);
    let after = contexts.iter().find(|c| c.start == gap.end).unwrap();
    assert_eq!(
        value(after, adiv5::SELECT),
        Some(Value::Unknown {
            reason: adiv5::LOST_SELECT,
            needs: Some(adiv5::NEEDS_SELECT),
        })
    );
}

#[test]
fn mem_ap_follows_tar_and_csw_through_posted_reads() {
    let ts = vec![
        dp_write(0x8, 0x0000_0000),
        ap_write(0x0, 0x2300_0012),
        ap_write(0x4, 0x2000_0000),
        ap_read(0xC, 0),
        ap_read(0xC, 0x1111_1111),
        dp_read(0xC, 0x2222_2222),
        ap_write(0xC, 0x3333_3333),
    ];
    let (b, _) = bits(&ts);
    let (mut h, [_, a, m]) = host(wires(&b, P), 100, None);
    run(&mut h);
    let writes = of(&h, a, adiv5::AP_WRITE);
    let (csw, tar) = (writes[0].id, writes[1].id);
    let reads = of(&h, m, adiv5::MEM_READ);
    let got: Vec<_> = (reads.iter())
        .map(|i| (word(i, adiv5::ADDRESS), word(i, adiv5::MEM_VALUE)))
        .collect();
    assert_eq!(
        got,
        [
            (Some(0x2000_0000), Some(0x1111_1111)),
            (Some(0x2000_0004), Some(0x2222_2222)),
        ]
    );
    assert!(
        reads[0]
            .links
            .contains(&(adiv5::ADDRESS_FROM, Ref::Upstream(tar)))
    );
    assert!(
        reads[1]
            .links
            .contains(&(adiv5::ADDRESS_FROM, Ref::Own(reads[0].id)))
    );
    assert!(
        reads[1]
            .links
            .contains(&(adiv5::INCREMENT_FROM, Ref::Upstream(csw)))
    );
    let w = of(&h, m, adiv5::MEM_WRITE);
    assert_eq!(word(&w[0], adiv5::ADDRESS), Some(0x2000_0008));
    assert_eq!(word(&w[0], adiv5::MEM_VALUE), Some(0x3333_3333));
}

#[test]
fn a_restored_chain_ends_where_the_whole_run_does() {
    let (b, _) = bits(&session());
    let want = decoded(wires(&b, P), 50);
    let (mut h, [s, a, m]) = host(wires(&b, P), 50, None);
    while h.consumed(s) < rise(40, P) {
        h.step().unwrap();
    }
    let cut = h.checkpoint(s).unwrap();
    run(&mut h);
    assert_eq!([s, a, m].map(|x| h.store(x).counts), want);
    h.restore(&cut).unwrap();
    run(&mut h);
    assert_eq!([s, a, m].map(|x| h.store(x).counts), want);
}

#[test]
fn a_local_start_says_what_it_cannot_know_yet() {
    let ts = vec![
        dp_write(0x8, 0x0000_0000),
        ap_write(0x4, 0x2000_0000),
        ap_read(0xC, 0),
        dp_read(0xC, 0x1111_1111),
    ];
    let (b, starts) = bits(&ts);
    let local = rise(starts[1], P) - P;
    let (mut h, [s, a, m]) = host(wires(&b, P), 64, Some(local));
    run(&mut h);
    assert_eq!(
        of(&h, s, swd::TRANSFER).len(),
        3,
        "the SELECT write is before the start"
    );
    let context = &of(&h, a, adiv5::CONTEXT)[0];
    assert_eq!(context.start, local);
    assert_eq!(
        value(context, adiv5::SELECT),
        Some(Value::Unknown {
            reason: adiv5::NO_SELECT,
            needs: Some(adiv5::NEEDS_SELECT),
        })
    );
    let write = &of(&h, a, adiv5::AP_WRITE)[0];
    assert!(matches!(
        value(write, adiv5::AP),
        Some(Value::Unknown { .. })
    ));
    assert_eq!(
        value(write, adiv5::AP_CLASS),
        Some(Value::Unknown {
            reason: adiv5::NO_SELECT,
            needs: Some(adiv5::NEEDS_SELECT),
        }),
        "no AP, so no class"
    );
    let mem = of(&h, m, adiv5::MEM_CONTEXT);
    assert_eq!(mem.len(), 1, "MEM-AP says the AP register is unknown");
    assert_eq!(
        value(&mem[0], adiv5::ACCESS),
        Some(Value::Unknown {
            reason: adiv5::AP_UNKNOWN,
            needs: Some(adiv5::MEM_NEEDS_SELECT),
        })
    );
}

#[test]
fn line_resets_and_selection_sequences_are_masked() {
    let ts = session();
    let (mut b, _) = bits(&ts);
    let reset = || {
        let mut r = vec![true; 56];
        r.extend([false; 2]);
        r
    };
    let lsb = |v: u128, n: usize| -> Vec<bool> { (0..n).map(|k| v >> k & 1 != 0).collect() };
    // Dormant to SWD: the selection alert, four idle bits and SWD's activation code, then a line
    // reset; and JTAG to SWD after another.
    let mut head = vec![true; 8];
    head.extend(lsb(0x19BC_0EA2_E3DD_AFE9_8685_2D95_6209_F392, 128));
    head.extend([false; 4]);
    head.extend(lsb(0x1A, 8));
    head.extend(reset());
    head.extend(lsb(0xE79E, 16));
    head.extend(reset());
    head.append(&mut b);
    let (mut h, [s, _, _]) = host(wires(&head, P), 100, None);
    run(&mut h);
    assert_eq!(of(&h, s, swd::TRANSFER).len(), ts.len());
}

#[test]
fn mem_ap_does_not_increment_past_a_1_kib_boundary() {
    let ts = vec![
        dp_write(0x8, 0x0000_0000),
        ap_write(0x0, 0x2300_0012),
        ap_write(0x4, 0x2000_03FC),
        ap_write(0xC, 1),
        ap_write(0xC, 2),
    ];
    let (b, _) = bits(&ts);
    let (mut h, [_, _, m]) = host(wires(&b, P), 100, None);
    run(&mut h);
    let w = of(&h, m, adiv5::MEM_WRITE);
    assert_eq!(word(&w[0], adiv5::ADDRESS), Some(0x2000_03FC));
    assert_eq!(
        value(&w[1], adiv5::ADDRESS),
        Some(Value::Unknown {
            reason: adiv5::PAST_1K,
            needs: Some(adiv5::NEEDS_TAR),
        })
    );
}

#[test]
fn mem_ap_forgets_tar_across_a_gap() {
    let ts = vec![
        dp_write(0x8, 0x0000_0000),
        ap_write(0x4, 0x2000_0000),
        ap_read(0xC, 0),
        dp_write(0x8, 0x0000_0000),
        ap_read(0xC, 0),
        dp_read(0xC, 0x1111_1111),
    ];
    let (b, starts) = bits(&ts);
    let gap = Span {
        start: rise(starts[2] + 20, P),
        end: rise(starts[3] - 1, P),
    };
    let mut w = wires(&b, P);
    w.lines[1].gaps.push(gap);
    let (mut h, [_, _, m]) = host(w, 64, None);
    run(&mut h);
    let reads = of(&h, m, adiv5::MEM_READ);
    assert_eq!(reads.len(), 1);
    assert_eq!(
        value(&reads[0], adiv5::ADDRESS),
        Some(Value::Unknown {
            reason: adiv5::LOST_TAR,
            needs: Some(adiv5::NEEDS_TAR),
        })
    );
}

/// `bits` with a turnaround of `trn` cycles.
fn bits_turnaround(transfers: &[T], trn: usize) -> Vec<bool> {
    let mut out = vec![false; 8];
    for t in transfers {
        let a2 = t.addr & 4 != 0;
        let a3 = t.addr & 8 != 0;
        let parity = t.ap ^ t.read ^ a2 ^ a3;
        out.extend([true, t.ap, t.read, a2, a3, parity, false, true]);
        out.extend(vec![true; trn]);
        out.extend((0..3).map(|k| t.ack >> k & 1 != 0));
        if t.ack == 1 {
            if !t.read {
                out.extend(vec![true; trn]);
            }
            out.extend((0..32).map(|k| t.data >> k & 1 != 0));
            out.push(t.data.count_ones() % 2 == 1);
            if t.read {
                out.extend(vec![true; trn]);
            }
        } else {
            out.extend(vec![true; trn]);
        }
        out.extend([false, false]);
    }
    out.extend([false; 8]);
    out
}

#[test]
fn the_turnaround_is_a_setting_of_one_to_four_cycles() {
    let ts = session();
    for trn in 1..=4u8 {
        let b = bits_turnaround(&ts, usize::from(trn));
        let mut h = Host::default();
        let s = h
            .add(
                name("swd"),
                swd::definition(),
                InputSpec::Wire {
                    source: Box::new(wires(&b, P)),
                    local: None,
                },
                swd::Swd::factory(64),
                &[swd::turnaround(trn)],
                all(),
            )
            .unwrap();
        run(&mut h);
        let got = of(&h, s, swd::TRANSFER);
        assert_eq!(got.len(), ts.len(), "turnaround {trn}");
        let data: Vec<_> = got.iter().map(|i| word(i, swd::DATA)).collect();
        let want: Vec<_> = (ts.iter())
            .map(|t| (t.ack == 1).then_some(u64::from(t.data)))
            .collect();
        assert_eq!(data, want, "turnaround {trn}");
        let trn2 = got[1].field(swd::TRN2).and_then(|f| f.span).unwrap();
        let cells = (trn2.end - trn2.start) / P;
        assert_eq!(
            cells,
            Tick::from(trn),
            "a write's second turnaround spans its cycles"
        );
        let data = got[1].field(swd::DATA).and_then(|f| f.span).unwrap();
        assert_eq!(
            data.start, trn2.end,
            "a write's data follows its second turnaround"
        );
        let bits = got[1].seq(swd::DATA_BITS);
        assert_eq!(bits.first().map(|e| e.cell.start), Some(data.start));
        let read = got[0].field(swd::TRN2).and_then(|f| f.span).unwrap();
        let parity = got[0].field(swd::PARITY).and_then(|f| f.span).unwrap();
        assert_eq!(
            read.start, parity.end,
            "a read's second turnaround follows its parity"
        );
    }
    for bad in [0, 5] {
        let refused = Host::default().add(
            name("swd"),
            swd::definition(),
            InputSpec::Wire {
                source: Box::new(wires(&[], P)),
                local: None,
            },
            swd::Swd::factory(64),
            &[swd::turnaround(bad)],
            all(),
        );
        assert_eq!(
            refused.err(),
            Some(BindError::SettingValue(name("turnaround")))
        );
    }
}

#[test]
fn a_transfer_whose_rises_are_not_held_closes_incomplete_rather_than_going_missing() {
    let t = swd::Transfer {
        anchor: 1_000,
        ap: false,
        read: true,
        address: 0,
        ack: 1,
        word: Some(0x0BC1_1477),
        parity: Some(true),
        turnaround: 1,
        data_phase: true,
    };
    let mut track = swd::Track::default();
    let mut out = escapement_tickwave_decode::Out::default();
    let rises: Vec<Tick> = (0..10).map(|k| 1_000 + k * P).collect();
    let levels = vec![false; rises.len()];
    track.emit(&t, None, 0, &rises, &levels, &mut out);
    let id = escapement_tickwave_decode::Id::new(swd::TRANSFER, 1_000);
    assert_eq!(
        out.ops,
        vec![
            escapement_tickwave_decode::Op::Open {
                id,
                parent: None,
                start: 1_000
            },
            escapement_tickwave_decode::Op::Close {
                id,
                end: 1_001,
                status: Status::Incomplete(swd::DROPPED)
            },
        ],
        "fewer rises held than a transfer spans"
    );
}

/// `transfers` onto `out` with a turnaround of `trn` cycles, a WAIT or FAULT carrying a data
/// phase where `orun` (ADIv5.2, B4.2.6): a read's 33 cycles undriven, here alternating.
fn link_bits(out: &mut Vec<bool>, transfers: &[T], trn: usize, orun: bool) {
    for t in transfers {
        let a2 = t.addr & 4 != 0;
        let a3 = t.addr & 8 != 0;
        let parity = t.ap ^ t.read ^ a2 ^ a3;
        out.extend([true, t.ap, t.read, a2, a3, parity, false, true]);
        out.extend(vec![true; trn]);
        out.extend((0..3).map(|k| t.ack >> k & 1 != 0));
        if t.ack == 1 || orun {
            if !t.read {
                out.extend(vec![true; trn]);
            }
            if t.ack == 1 || !t.read {
                out.extend((0..32).map(|k| t.data >> k & 1 != 0));
                out.push(t.data.count_ones() % 2 == 1);
            } else {
                out.extend((0..33).map(|k| k % 2 == 0));
            }
            if t.read {
                out.extend(vec![true; trn]);
            }
        } else {
            out.extend(vec![true; trn]);
        }
        out.extend([false, false]);
    }
}

fn swd_of(b: &[bool], settings: &[(&str, escapement_tickwave_decode::Setting)]) -> Vec<Item> {
    let mut h = Host::default();
    let s = h
        .add(
            name("swd"),
            swd::definition(),
            InputSpec::Wire {
                source: Box::new(wires(b, P)),
                local: None,
            },
            swd::Swd::factory(64),
            settings,
            all(),
        )
        .unwrap();
    run(&mut h);
    of(&h, s, swd::TRANSFER)
}

/// Each transfer's first turnaround, in cycles.
fn turnarounds(ts: &[Item]) -> Vec<Tick> {
    (ts.iter())
        .map(|t| {
            t.field(swd::TRN)
                .and_then(|f| f.span)
                .map_or(0, |s| (s.end - s.start) / P)
        })
        .collect()
}

/// A DLCR write sets the turnaround of the transfers after it, under SELECT's bank 1; a line
/// reset sets it back to one cycle.
#[test]
fn a_dlcr_write_sets_the_turnaround_and_a_line_reset_sets_it_back() {
    let wait = T {
        ack: 2,
        ..ap_read(0xC, 0)
    };
    let mut b = vec![false; 8];
    // Bank 1, then DLCR: two cycles, its bit 0 set as CTRL/STAT's ORUNDETECT would be.
    link_bits(
        &mut b,
        &[dp_write(0x8, 1), dp_write(0x4, 1 << 8 | 1)],
        1,
        false,
    );
    link_bits(
        &mut b,
        &[dp_read(0x0, DPIDR), wait, ap_read(0xC, IDR)],
        2,
        false,
    );
    b.extend([true; 60]);
    b.extend([false; 2]);
    link_bits(&mut b, &[dp_read(0x0, DPIDR)], 1, false);
    // Still bank 1: a DLCR write whose data's parity fails changes nothing.
    let mut bad = vec![];
    link_bits(&mut bad, &[dp_write(0x4, 3 << 8)], 1, false);
    bad[8 + 1 + 3 + 1 + 32] ^= true;
    b.extend(bad);
    link_bits(&mut b, &[dp_read(0x0, DPIDR)], 1, false);
    b.extend([false; 8]);
    let ts = swd_of(&b, &[]);
    assert_eq!(turnarounds(&ts), [1, 1, 2, 2, 2, 1, 1, 1]);
    let data: Vec<_> = ts.iter().map(|t| word(t, swd::DATA)).collect();
    let (dpidr, idr) = (Some(u64::from(DPIDR)), Some(u64::from(IDR)));
    assert_eq!(
        data,
        [
            Some(1),
            Some(0x101),
            dpidr,
            None,
            idr,
            dpidr,
            Some(0x300),
            dpidr
        ]
    );
}

/// With CTRL/STAT.ORUNDETECT written under bank 0, or set at the start, a WAIT or FAULT keeps an
/// OK transfer's timing: a read's data cycles undriven, a write's data the target ignores.
#[test]
fn with_orundetect_a_wait_or_fault_carries_a_data_phase() {
    let wait = T {
        ack: 2,
        ..ap_read(0xC, 0)
    };
    let fault = T {
        ack: 4,
        ..ap_write(0x4, 0x1234)
    };
    let after = [wait, fault, dp_read(0x4, 0xF000_0001)];
    let mut b = vec![false; 8];
    link_bits(&mut b, &[dp_write(0x8, 0), dp_write(0x4, 1)], 1, false);
    link_bits(&mut b, &after, 1, true);
    b.extend([false; 8]);
    let ts = swd_of(&b, &[]);
    assert_eq!(ts.len(), 5);
    let undriven = Some(Value::Unknown {
        reason: swd::UNDRIVEN,
        needs: None,
    });
    assert_eq!(value(&ts[2], swd::DATA), undriven);
    // The read's second turnaround follows its 33 undriven cycles.
    let span = |t: &Item, f| t.field(f).and_then(|f| f.span).unwrap();
    assert_eq!(span(&ts[2], swd::TRN2).start, span(&ts[2], swd::DATA).end);
    assert_eq!(
        span(&ts[2], swd::DATA).end - span(&ts[2], swd::DATA).start,
        33 * P
    );
    assert_eq!(word(&ts[3], swd::DATA), Some(0x1234));
    assert_eq!(word(&ts[4], swd::DATA), Some(0xF000_0001));
    // Set at the start instead, without the writes.
    let mut b = vec![false; 8];
    link_bits(&mut b, &after, 1, true);
    b.extend([false; 8]);
    let ts = swd_of(&b, &[swd::orundetect(true)]);
    assert_eq!(ts.len(), 3);
    assert_eq!(value(&ts[0], swd::DATA), undriven);
    assert_eq!(word(&ts[2], swd::DATA), Some(0xF000_0001));
}
