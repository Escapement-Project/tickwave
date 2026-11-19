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
    host_adi(options, w, window, local, &[])
}

/// SWD, ADIv5 bound with `adi` and MEM-AP.
fn host_adi(
    options: HostOptions,
    w: Wires,
    window: Tick,
    local: Option<Tick>,
    adi: &[(&str, escapement_tickwave_decode::Setting)],
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
            adi,
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
    let mut items: Vec<Item> = (h.store(a).items().values())
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

/// A memory access's value, its bytes read as a little-endian number, where it is known.
fn mem(i: &Item) -> Option<u64> {
    match value(i, adiv5::MEM_VALUE)? {
        Value::Bits { bytes, .. } => {
            Some(bytes.iter().rev().fold(0, |n, &b| n << 8 | u64::from(b)))
        }
        _ => None,
    }
}

/// A memory access's value as its bytes, lowest address first, and its width in bits.
fn mem_bytes(i: &Item) -> Option<(Vec<u8>, u32)> {
    match value(i, adiv5::MEM_VALUE)? {
        Value::Bits { bytes, bits } => Some((bytes.to_vec(), bits)),
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
    let got: Vec<_> = (of(&h, m, adiv5::MEM_WRITE).iter())
        .map(|i| word(i, adiv5::ADDRESS))
        .collect();
    assert_eq!(
        got,
        [Some(0x2000_0000)],
        "MEM-AP skips the JTAG-AP, and an AP not yet identified writes at no TAR"
    );
}

#[test]
fn the_idr_setting_identifies_aps_from_the_start_until_an_idr_read() {
    let ts = vec![
        dp_write(0x8, 0x0000_0000),
        ap_write(0x4, 0x2000_0000),
        ap_write(0xC, 0x0000_0001),
        dp_write(0x8, 0x0100_0000),
        ap_write(0x4, 0x0000_0001),
        ap_write(0xC, 0x0000_0002),
        dp_write(0x8, 0x0200_0000),
        ap_write(0x0, 0x0000_0000),
        // AP 1's own IDR read: a MEM-AP after all.
        dp_write(0x8, 0x0100_00F0),
        ap_read(0xC, 0),
        dp_read(0xC, IDR),
        dp_write(0x8, 0x0100_0000),
        ap_write(0x4, 0x2000_0010),
    ];
    let (b, _) = bits(&ts);
    let idr = adiv5::idr(&[IDR.into(), JTAG_AP_IDR.into(), adiv5::IDR_NOT_GIVEN]);
    let (mut h, [_, a, m]) = host_adi(HostOptions::default(), wires(&b, P), 100, None, &[idr]);
    run(&mut h);
    let class = |n| Some(Value::Name(n));
    let got: Vec<_> = (of(&h, a, adiv5::AP_WRITE).iter())
        .map(|i| (named(i, adiv5::AP_NAME), value(i, adiv5::AP_CLASS)))
        .collect();
    assert_eq!(
        got,
        [
            (Some(1), class(adiv5::MEM_AP)),
            (Some(3), class(adiv5::MEM_AP)),
            (Some(11), class(adiv5::JTAG_AP)),
            (None, class(adiv5::JTAG_AP)),
            (
                None,
                Some(Value::Unknown {
                    reason: adiv5::NO_IDR,
                    needs: Some(adiv5::NEEDS_IDR),
                })
            ),
            (Some(1), class(adiv5::MEM_AP)),
        ],
        "AP 0 a MEM-AP and AP 1 a JTAG-AP from the start, AP 2 left out, then AP 1's IDR read"
    );
    let mem: Vec<_> = (of(&h, m, adiv5::MEM_WRITE).iter())
        .map(|i| word(i, adiv5::ADDRESS))
        .collect();
    assert_eq!(mem, [Some(0x2000_0000)], "MEM-AP skips AP 1 as a JTAG-AP");
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
        .map(|i| (word(i, adiv5::ADDRESS), mem(i)))
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
    assert_eq!(mem(&w[0]), Some(0x3333_3333));
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

/// MEM-AP's addresses for `ts`'s memory writes.
fn write_addresses(ts: &[T]) -> Vec<Option<Value>> {
    let (b, _) = bits(ts);
    let (mut h, [_, _, m]) = host(wires(&b, P), 100, None);
    run(&mut h);
    (of(&h, m, adiv5::MEM_WRITE).iter())
        .map(|i| value(i, adiv5::ADDRESS))
        .collect()
}

/// Two DRW writes after a CSW write of `csw` and a TAR write of `tar`.
fn two_writes(csw: u32, tar: u32) -> Vec<T> {
    vec![
        dp_write(0x8, 0x0000_0000),
        ap_write(0x0, csw),
        ap_write(0x4, tar),
        ap_write(0xC, 1),
        ap_write(0xC, 2),
    ]
}

fn at(a: u64) -> Option<Value> {
    Some(Value::Word { value: a, bits: 32 })
}

fn no_address(reason: escapement_tickwave_decode::Reason) -> Option<Value> {
    Some(Value::Unknown {
        reason,
        needs: Some(adiv5::NEEDS_TAR),
    })
}

#[test]
fn mem_ap_increments_as_csw_says_singly_or_packed() {
    // CSW's AddrInc (bits 5:4) and Size (bits 2:0), and the second write's address.
    let cases = [
        (0x02, at(0x2000_0000)),
        (0x10, at(0x2000_0001)),
        (0x11, at(0x2000_0002)),
        (0x12, at(0x2000_0004)),
        (0x20, at(0x2000_0004)),
        (0x21, at(0x2000_0004)),
        (0x22, no_address(adiv5::PACKED_WORD)),
        (0x23, no_address(adiv5::PACKED_WIDE)),
        (0x32, no_address(adiv5::CSW_RESERVED)),
        (0x16, no_address(adiv5::CSW_RESERVED)),
    ];
    for (csw, want) in cases {
        let got = write_addresses(&two_writes(0x2300_0000 | csw, 0x2000_0000));
        assert_eq!(got, [at(0x2000_0000), want], "CSW 0x{csw:02X}");
    }
    let packed = |tar| write_addresses(&two_writes(0x2300_0020, tar))[1].clone();
    assert_eq!(
        packed(0x2000_03FA),
        at(0x2000_03FE),
        "from off a word boundary"
    );
    assert_eq!(packed(0x2000_03FE), no_address(adiv5::PAST_1K));
}

#[test]
fn a_csw_read_shows_whether_the_mem_ap_took_packed_word_accesses() {
    // A MEM-AP that takes only words reads AddrInc back as off.
    for (held, second) in [
        (0x2300_0022, at(0x2000_0004)),
        (0x2300_0002, at(0x2000_0000)),
    ] {
        let mut ts = two_writes(0x2300_0022, 0x2000_0000);
        ts.splice(2..2, [ap_read(0x0, 0), dp_read(0xC, held)]);
        assert_eq!(
            write_addresses(&ts),
            [at(0x2000_0000), second],
            "CSW read back as 0x{held:08X}"
        );
    }
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

/// MEM-AP's memory accesses for `ts`, reads and writes in order.
fn accesses(ts: &[T]) -> Vec<Item> {
    let (b, _) = bits(ts);
    let (mut h, [_, _, m]) = host(wires(&b, P), 100, None);
    run(&mut h);
    let mut all = of(&h, m, adiv5::MEM_READ);
    all.extend(of(&h, m, adiv5::MEM_WRITE));
    all.sort_by_key(|i| i.start);
    all
}

/// SELECT AP 0 bank 0, CSW `csw` and TAR `tar`, then `then`.
fn at_csw(csw: u32, tar: u32, then: &[T]) -> Vec<T> {
    let mut ts = vec![
        dp_write(0x8, 0x0000_0000),
        ap_write(0x0, 0x2300_0000 | csw),
        ap_write(0x4, tar),
    ];
    ts.extend_from_slice(then);
    ts
}

fn why(reason: escapement_tickwave_decode::Reason) -> Option<Value> {
    Some(Value::Unknown {
        reason,
        needs: None,
    })
}

#[test]
fn a_narrow_access_takes_the_byte_lanes_its_address_gives() {
    // Table C2-3: a byte at TAR[1:0], a halfword at TAR[1]; little-endian lanes.
    let drw = 0xAABB_CCDD;
    let cases = [
        (0x10, 0x2000_0001, Some((vec![0xCC], 8))),
        (0x10, 0x2000_0003, Some((vec![0xAA], 8))),
        (0x11, 0x2000_0002, Some((vec![0xBB, 0xAA], 16))),
        (0x11, 0x2000_0000, Some((vec![0xDD, 0xCC], 16))),
        (0x12, 0x2000_0000, Some((vec![0xDD, 0xCC, 0xBB, 0xAA], 32))),
        // Packed bytes and halfwords: a word's worth, from TAR up, each in its address's lane.
        (0x20, 0x2000_0002, Some((vec![0xBB, 0xAA, 0xDD, 0xCC], 32))),
        (0x21, 0x2000_0002, Some((vec![0xBB, 0xAA, 0xDD, 0xCC], 32))),
        (0x22, 0x2000_0000, Some((vec![0xDD, 0xCC, 0xBB, 0xAA], 32))),
    ];
    for (csw, tar, want) in cases {
        let got = accesses(&at_csw(csw, tar, &[ap_write(0xC, drw)]));
        assert_eq!(got.len(), 1);
        assert_eq!(mem_bytes(&got[0]), want, "CSW 0x{csw:02X} at 0x{tar:08X}");
    }
}

#[test]
fn an_access_off_its_alignment_or_a_1_kib_boundary_has_no_value() {
    let cases = [
        (0x11, 0x2000_0001, adiv5::UNALIGNED),
        (0x12, 0x2000_0002, adiv5::UNALIGNED),
        (0x21, 0x2000_0003, adiv5::UNALIGNED),
        (0x20, 0x2000_03FE, adiv5::PAST_1K),
    ];
    for (csw, tar, reason) in cases {
        let got = accesses(&at_csw(csw, tar, &[ap_write(0xC, 1)]));
        assert_eq!(
            value(&got[0], adiv5::MEM_VALUE),
            why(reason),
            "CSW 0x{csw:02X} at 0x{tar:08X}"
        );
        assert_eq!(word(&got[0], adiv5::ADDRESS), Some(tar.into()));
    }
}

#[test]
fn without_csw_the_size_is_unknown_and_without_tar_a_narrow_lane_is() {
    let ts = [
        dp_write(0x8, 0x0000_0000),
        ap_write(0x4, 0x2000_0000),
        ap_write(0xC, 1),
    ];
    let got = accesses(&ts);
    assert_eq!(
        value(&got[0], adiv5::MEM_VALUE),
        Some(Value::Unknown {
            reason: adiv5::NO_CSW,
            needs: Some(adiv5::NEEDS_CSW),
        })
    );
    let no_tar = |csw: u32| {
        let ts = [
            dp_write(0x8, 0x0000_0000),
            ap_write(0x0, 0x2300_0000 | csw),
            ap_write(0xC, 0x1122_3344),
        ];
        accesses(&ts)[0].clone()
    };
    assert_eq!(
        mem(&no_tar(0x12)),
        Some(0x1122_3344),
        "a word's lanes need no address"
    );
    assert_eq!(
        value(&no_tar(0x10), adiv5::MEM_VALUE),
        Some(Value::Unknown {
            reason: adiv5::NO_TAR,
            needs: Some(adiv5::NEEDS_TAR),
        })
    );
}

#[test]
fn a_wide_access_is_one_memory_access_over_its_drw_words() {
    // Table C2-20: 64, 128 and 256 bits in 2, 4 and 8 DRW accesses, least significant word
    // first; TAR moves by the whole size after the last.
    for (csw, words) in [(0x13u32, 2u32), (0x14, 4), (0x15, 8)] {
        let mut drw: Vec<T> = (0..2 * words)
            .map(|k| ap_write(0xC, 0x1000_0000 + k))
            .collect();
        drw.push(ap_write(0x0, 0x2300_0002));
        drw.push(ap_write(0xC, 0xAAAA_AAAA));
        let got = accesses(&at_csw(csw, 0x2000_0000, &drw));
        assert_eq!(got.len(), 3, "CSW 0x{csw:02X}");
        let size = 4 * words;
        let want = |first: u32| -> Vec<u8> {
            (first..first + words)
                .flat_map(|k| (0x1000_0000 + k).to_le_bytes())
                .collect()
        };
        assert_eq!(mem_bytes(&got[0]), Some((want(0), 32 * words)));
        assert_eq!(mem_bytes(&got[1]), Some((want(words), 32 * words)));
        let addresses: Vec<_> = got.iter().map(|i| word(i, adiv5::ADDRESS)).collect();
        let base = 0x2000_0000u64;
        assert_eq!(
            addresses,
            [
                Some(base),
                Some(base + u64::from(size)),
                Some(base + 2 * u64::from(size))
            ],
            "CSW 0x{csw:02X}"
        );
        let access = got[0]
            .links
            .iter()
            .filter(|(l, _)| *l == adiv5::ACCESS_LINK)
            .count();
        assert_eq!(
            access, words as usize,
            "one access link for each DRW access"
        );
        assert!(got.iter().all(|i| i.status == Some(Status::Complete)));
    }
    // Auto-increment off: both doublewords at TAR.
    let drw: Vec<T> = (0..4).map(|k| ap_write(0xC, k)).collect();
    let got = accesses(&at_csw(0x03, 0x2000_0008, &drw));
    let addresses: Vec<_> = got.iter().map(|i| word(i, adiv5::ADDRESS)).collect();
    assert_eq!(addresses, [Some(0x2000_0008), Some(0x2000_0008)]);
}

#[test]
fn a_wide_read_takes_its_words_from_posted_reads() {
    let then = [
        ap_read(0xC, 0),
        ap_read(0xC, 0x1111_1111),
        dp_read(0xC, 0x2222_2222),
    ];
    let got = accesses(&at_csw(0x13, 0x2000_0000, &then));
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].id.kind, adiv5::MEM_READ);
    assert_eq!(mem(&got[0]), Some(0x2222_2222_1111_1111));
}

#[test]
fn a_wide_access_cut_short_says_why_and_what_tar_is_after_it() {
    // A CSW access ends the sequence: a written one makes no memory access, and TAR after it is
    // unknown where it increments.
    for (csw, after) in [(0x13, no_address(adiv5::WIDE_CUT)), (0x03, at(0x2000_0000))] {
        let then = [
            ap_write(0xC, 1),
            ap_write(0x0, 0x2300_0002),
            ap_write(0xC, 2),
        ];
        let got = accesses(&at_csw(csw, 0x2000_0000, &then));
        assert_eq!(got[0].status, Some(Status::Incomplete(adiv5::CSW_ENDS)));
        assert_eq!(value(&got[1], adiv5::ADDRESS), after, "CSW 0x{csw:02X}");
        assert_eq!(mem(&got[1]), Some(2));
    }
    // Any other register, or the other direction, is unpredictable: the access that interrupts
    // is dropped, and TAR is unknown until written again.
    for interrupt in [ap_write(0x4, 0x2000_0100), ap_read(0xC, 0)] {
        let then = [
            ap_write(0xC, 1),
            interrupt,
            dp_read(0xC, 0),
            ap_write(0x0, 0x2300_0012),
            ap_write(0xC, 2),
            ap_write(0x4, 0x2000_0200),
            ap_write(0xC, 3),
        ];
        let got = accesses(&at_csw(0x13, 0x2000_0000, &then));
        assert_eq!(got.len(), 3, "{interrupt:?}");
        assert_eq!(got[0].status, Some(Status::Incomplete(adiv5::INTERRUPTED)));
        assert_eq!(
            value(&got[1], adiv5::ADDRESS),
            no_address(adiv5::UNPREDICTABLE)
        );
        assert_eq!(value(&got[2], adiv5::ADDRESS), at(0x2000_0200));
    }
    // The recording ends first.
    let got = accesses(&at_csw(0x14, 0x2000_0000, &[ap_write(0xC, 1)]));
    assert_eq!(got[0].status, Some(Status::Incomplete(adiv5::MEM_ENDS)));
}

#[test]
fn a_gap_cuts_a_wide_access_short() {
    let ts = at_csw(0x13, 0x2000_0000, &[ap_write(0xC, 1), ap_write(0xC, 2)]);
    let (b, starts) = bits(&ts);
    let gap = Span {
        start: rise(starts[3] + 50, P),
        end: rise(starts[4] - 1, P),
    };
    let mut w = wires(&b, P);
    w.lines[1].gaps.push(gap);
    let (mut h, [_, _, m]) = host(w, 64, None);
    run(&mut h);
    let got = of(&h, m, adiv5::MEM_WRITE);
    assert_eq!(got[0].status, Some(Status::Incomplete(adiv5::MEM_GAP_CUT)));
}

#[test]
fn banked_data_accesses_reach_tars_16_byte_block_and_leave_tar() {
    // AP register bank 1 holds BD0 to BD3.
    let then = [
        dp_write(0x8, 0x0000_0010),
        ap_read(0x0, 0),
        dp_read(0xC, 0x1111_1111),
        ap_write(0xC, 0x2222_2222),
        dp_write(0x8, 0x0000_0000),
        ap_write(0xC, 0x3333_3333),
    ];
    let got = accesses(&at_csw(0x12, 0x2000_0014, &then));
    let seen: Vec<_> = got
        .iter()
        .map(|i| (word(i, adiv5::ADDRESS), mem(i)))
        .collect();
    assert_eq!(
        seen,
        [
            (Some(0x2000_0010), Some(0x1111_1111)),
            (Some(0x2000_001C), Some(0x2222_2222)),
            (Some(0x2000_0014), Some(0x3333_3333)),
        ],
        "BD0 and BD3 in TAR's block, then DRW at TAR, unmoved"
    );
    assert!(
        got[0]
            .links
            .iter()
            .all(|(l, _)| *l != adiv5::INCREMENT_FROM)
    );
}

#[test]
fn a_doubleword_through_banked_data_takes_a_register_pair() {
    // Table C2-11: BD2 then BD3 make the doubleword at the block's + 8.
    let then = [
        dp_write(0x8, 0x0000_0010),
        ap_write(0x8, 0x1111_1111),
        ap_write(0xC, 0x2222_2222),
    ];
    let got = accesses(&at_csw(0x13, 0x2000_0004, &then));
    assert_eq!(got.len(), 1);
    assert_eq!(word(&got[0], adiv5::ADDRESS), Some(0x2000_0008));
    assert_eq!(mem(&got[0]), Some(0x2222_2222_1111_1111));
    // The pair's second register first, or a size other than word or doubleword: unpredictable.
    let bank1 = |reg| [dp_write(0x8, 0x0000_0010), ap_write(reg, 1)];
    let got = accesses(&at_csw(0x13, 0x2000_0000, &bank1(0x4)));
    assert_eq!(value(&got[0], adiv5::MEM_VALUE), why(adiv5::BD_ORDER));
    let got = accesses(&at_csw(0x10, 0x2000_0000, &bank1(0x0)));
    assert_eq!(value(&got[0], adiv5::MEM_VALUE), why(adiv5::BD_SIZE));
}

#[test]
fn a_read_never_read_back_still_moves_tar() {
    // The AP write ends the posted read before RDBUFF returns its data.
    let then = [ap_read(0xC, 0), ap_write(0xC, 2)];
    let got = accesses(&at_csw(0x12, 0x2000_0000, &then));
    assert_eq!(value(&got[0], adiv5::MEM_VALUE), why(adiv5::UNREAD));
    assert_eq!(word(&got[1], adiv5::ADDRESS), Some(0x2000_0004));
    // A wide read whose last word never comes back: made, its value unknown, TAR moved.
    let then = [
        ap_read(0xC, 0),
        ap_read(0xC, 0x1111_1111),
        ap_write(0x4, 0x2000_0000),
        ap_write(0x0, 0x2300_0003),
        ap_write(0xC, 2),
    ];
    let got = accesses(&at_csw(0x13, 0x2000_0000, &then));
    assert_eq!(got[0].status, Some(Status::Complete));
    assert_eq!(value(&got[0], adiv5::MEM_VALUE), why(adiv5::UNREAD));
}

#[test]
fn a_parity_failure_drops_a_write_and_leaves_a_reads_value_unknown() {
    let then = [
        ap_write(0xC, 1),
        ap_read(0xC, 0),
        dp_read(0xC, 2),
        ap_write(0xC, 3),
    ];
    let ts = at_csw(0x12, 0x2000_0000, &then);
    let (mut b, starts) = bits(&ts);
    // Each data phase's parity bit: request 8, turnaround 1, ack 3, then a write's turnaround.
    b[starts[3] + 8 + 1 + 3 + 1 + 32] ^= true;
    b[starts[5] + 8 + 1 + 3 + 32] ^= true;
    let (mut h, [_, _, m]) = host(wires(&b, P), 100, None);
    run(&mut h);
    let mut got = of(&h, m, adiv5::MEM_READ);
    got.extend(of(&h, m, adiv5::MEM_WRITE));
    got.sort_by_key(|i| i.start);
    let seen: Vec<_> = got
        .iter()
        .map(|i| (word(i, adiv5::ADDRESS), value(i, adiv5::MEM_VALUE)))
        .collect();
    assert_eq!(
        seen,
        [
            (Some(0x2000_0000), why(adiv5::MEM_PARITY)),
            (
                Some(0x2000_0004),
                Some(Value::Bits {
                    bytes: vec![3, 0, 0, 0].into(),
                    bits: 32
                })
            ),
        ],
        "the dropped write moves nothing; the read does"
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
