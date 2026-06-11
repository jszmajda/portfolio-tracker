//! RED-phase TDD tests for the LEDGER-EVENT-* EARS specs of `ledger-core`.
//!
//! These tests exercise the *event taxonomy & ordering* behaviour described in
//! `docs/intent/ledger-core/ledger-core-specs.md` (LEDGER-EVENT-001..007) and
//! `-design.md`. The scaffold's `replay`/`validate` bodies are stubbed with
//! `unimplemented!()`, so every test here compiles against the real public API
//! and FAILS at runtime (RED) until the kernel fold is implemented.
//!
//! Each test carries a `// @spec LEDGER-EVENT-NNN` annotation on the test that
//! directly exercises that spec (per CLAUDE.md "Code annotations").

use ledger_core::{
    replay, validate, LedgerError, LedgerEvent, LedgerEventKind, LotRef, Marks, Snapshot,
};
use pt_core::{Cents, Date, MicroShares, Seq};

use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Construction helpers — keep the tests readable; all amounts are exact
// integers (Cents / MicroShares) per the design.
// ---------------------------------------------------------------------------

const PLATFORM: &str = "schwab";

fn ev(id: &str, seq: u64, date: i32, kind: LedgerEventKind) -> LedgerEvent {
    LedgerEvent {
        id: id.to_string(),
        seq: Seq(seq),
        date: Date(date),
        kind,
    }
}

fn buy(
    lot_id: &str,
    symbol: &str,
    qty: i64,
    unit_price_cents: i64,
    fees_cents: i64,
) -> LedgerEventKind {
    LedgerEventKind::Buy {
        lot_id: lot_id.to_string(),
        symbol: symbol.to_string(),
        qty: MicroShares(qty),
        unit_price_cents: Cents(unit_price_cents),
        fees_cents: Cents(fees_cents),
        platform: PLATFORM.to_string(),
        tracking_code: None,
    }
}

fn buy_meta(
    lot_id: &str,
    symbol: &str,
    qty: i64,
    unit_price_cents: i64,
    fees_cents: i64,
    platform: &str,
    tracking_code: Option<&str>,
) -> LedgerEventKind {
    LedgerEventKind::Buy {
        lot_id: lot_id.to_string(),
        symbol: symbol.to_string(),
        qty: MicroShares(qty),
        unit_price_cents: Cents(unit_price_cents),
        fees_cents: Cents(fees_cents),
        platform: platform.to_string(),
        tracking_code: tracking_code.map(str::to_string),
    }
}

fn vest(lot_id: &str, symbol: &str, qty: i64, fmv_per_share_cents: i64) -> LedgerEventKind {
    LedgerEventKind::Vest {
        lot_id: lot_id.to_string(),
        symbol: symbol.to_string(),
        qty: MicroShares(qty),
        fmv_per_share_cents: Cents(fmv_per_share_cents),
        platform: PLATFORM.to_string(),
        tracking_code: None,
    }
}

fn sell(
    sale_id: &str,
    symbol: &str,
    qty: i64,
    unit_price_cents: i64,
    fees_cents: i64,
    lot_refs: Vec<LotRef>,
) -> LedgerEventKind {
    LedgerEventKind::Sell {
        sale_id: sale_id.to_string(),
        symbol: symbol.to_string(),
        qty: MicroShares(qty),
        unit_price_cents: Cents(unit_price_cents),
        fees_cents: Cents(fees_cents),
        lot_refs,
        accrues_to_state: None,
        platform: PLATFORM.to_string(),
        tracking_code: None,
    }
}

fn split(symbol: &str, ratio_num: i64, ratio_den: i64) -> LedgerEventKind {
    LedgerEventKind::Split {
        symbol: symbol.to_string(),
        ratio_num,
        ratio_den,
    }
}

fn reversal(target_event_id: &str) -> LedgerEventKind {
    LedgerEventKind::Reversal {
        target_event_id: target_event_id.to_string(),
    }
}

fn no_marks() -> Marks {
    BTreeMap::new()
}

const SHARE: i64 = 1_000_000; // one whole share in MicroShares.

// ---------------------------------------------------------------------------
// LEDGER-EVENT-001 — fold in ascending `Seq` order; `Date` is data, not the
// sort key. We feed events whose `Date` order is the REVERSE of their `Seq`
// order: a Buy (seq 1, later date) then a Sell (seq 2, earlier date) consuming
// that lot. If the fold sorted by `Date`, the Sell would precede its Buy and be
// nonsensical; folding by `Seq` makes the Sell follow the Buy and consume it.
// ---------------------------------------------------------------------------

// @spec LEDGER-EVENT-001
#[test]
fn folds_in_seq_order_treating_date_as_data() {
    let events = vec![
        // Higher (later) date, but LOWER seq → must be applied first.
        ev("e-buy", 1, 200, buy("L1", "AMZN", 10 * SHARE, 1000, 0)),
        // Lower (earlier) date, but HIGHER seq → must be applied second.
        ev(
            "e-sell",
            2,
            100,
            sell("S1", "AMZN", 4 * SHARE, 1500, 0, vec![]),
        ),
    ];

    let snap: Snapshot = replay(&events, &no_marks());

    // The Sell (seq 2) folded after the Buy (seq 1) and consumed 4 shares,
    // leaving 6 open and producing exactly one realized gain. A Date-sorted
    // fold would have applied the Sell first against an empty book.
    assert_eq!(
        snap.realized_gains.len(),
        1,
        "Sell (higher Seq) must fold after Buy (lower Seq), consuming the lot"
    );
    let pos = snap
        .positions
        .get("AMZN")
        .expect("AMZN position present after fold");
    assert_eq!(
        pos.total_qty,
        MicroShares(6 * SHARE),
        "6 shares remain open after a Seq-ordered Buy-then-Sell"
    );
}

// ---------------------------------------------------------------------------
// LEDGER-EVENT-002 — a Buy opens a lot with
// `total_basis = scale(qty × unit_price) + fees`, acquisition date = event date.
// 10 shares @ $1.00 (100 cents) + $0.50 fees = scale(10e6 × 100 / 1e6) + 50
//   = 1000 + 50 = 1050 cents.
// ---------------------------------------------------------------------------

// @spec LEDGER-EVENT-002
#[test]
fn buy_opens_lot_with_scaled_basis_plus_fees_and_event_date() {
    let events = vec![ev(
        "e-buy",
        1,
        18_000, // event date (days since epoch)
        buy("L1", "AMZN", 10 * SHARE, 100, 50),
    )];

    let snap = replay(&events, &no_marks());

    assert_eq!(snap.open_lots.len(), 1, "Buy opens exactly one lot");
    let lot = &snap.open_lots[0].lot;
    assert_eq!(lot.id, "L1");
    assert_eq!(lot.symbol, "AMZN");
    assert_eq!(
        lot.remaining_qty,
        MicroShares(10 * SHARE),
        "lot holds the bought quantity"
    );
    assert_eq!(
        lot.remaining_basis_cents,
        Cents(1050),
        "total_basis = scale(10 × 100) + 50 fees = 1050 cents"
    );
    assert_eq!(
        lot.acquire_date,
        Date(18_000),
        "acquisition date equals the Buy event's date"
    );
}

// ---------------------------------------------------------------------------
// LEDGER-EVENT-003 — a Vest opens a lot with
// `total_basis = scale(qty × fmv_per_share)` (no fee field), acquisition date =
// vest date. 4 shares @ $25.00 (2500 cents) FMV = scale(4e6 × 2500 / 1e6)
//   = 10000 cents.
// ---------------------------------------------------------------------------

// @spec LEDGER-EVENT-003
#[test]
fn vest_opens_lot_with_fmv_basis_and_vest_date() {
    let events = vec![ev(
        "e-vest",
        1,
        19_000, // vest date
        vest("V1", "AMZN", 4 * SHARE, 2500),
    )];

    let snap = replay(&events, &no_marks());

    assert_eq!(snap.open_lots.len(), 1, "Vest opens exactly one lot");
    let lot = &snap.open_lots[0].lot;
    assert_eq!(lot.id, "V1");
    assert_eq!(
        lot.remaining_basis_cents,
        Cents(10_000),
        "total_basis = scale(4 × 2500) = 10000 cents (FMV, no fees)"
    );
    assert_eq!(
        lot.acquire_date,
        Date(19_000),
        "acquisition date equals the vest date"
    );
}

// ---------------------------------------------------------------------------
// LEDGER-EVENT-004 — a Sell disposes the sale quantity from the selected lots
// and emits one RealizedGain per consumed (lot, qty) pair, each keyed by
// `(sale_id, lot_id)` and carrying the Sell's `sale_seq`. Here a single Sell
// spans TWO lots (specific-id), so it must emit exactly two RealizedGains, both
// with the Sell's sale_id and sale_seq, one per consumed lot.
// ---------------------------------------------------------------------------

// @spec LEDGER-EVENT-004
#[test]
fn sell_emits_one_realized_gain_per_consumed_lot_with_stable_identity() {
    let events = vec![
        ev("e-b1", 1, 100, buy("L1", "AMZN", 5 * SHARE, 1000, 0)),
        ev("e-b2", 2, 101, buy("L2", "AMZN", 5 * SHARE, 2000, 0)),
        ev(
            "e-sell",
            7,
            200,
            sell(
                "S1",
                "AMZN",
                6 * SHARE,
                3000,
                0,
                vec![
                    LotRef {
                        lot_id: "L1".to_string(),
                        qty: MicroShares(5 * SHARE),
                    },
                    LotRef {
                        lot_id: "L2".to_string(),
                        qty: MicroShares(1 * SHARE),
                    },
                ],
            ),
        ),
    ];

    let snap = replay(&events, &no_marks());

    assert_eq!(
        snap.realized_gains.len(),
        2,
        "one RealizedGain per consumed (lot, qty) pair across the two lots"
    );
    for rg in &snap.realized_gains {
        assert_eq!(rg.sale_id, "S1", "each gain carries the Sell's sale_id");
        assert_eq!(
            rg.sale_seq,
            Seq(7),
            "each gain carries the Sell's sale_seq for downstream ordering"
        );
    }
    let mut lot_ids: Vec<&str> = snap
        .realized_gains
        .iter()
        .map(|rg| rg.lot_id.as_str())
        .collect();
    lot_ids.sort_unstable();
    assert_eq!(
        lot_ids,
        vec!["L1", "L2"],
        "the (sale_id, lot_id) keys identify both consumed lots"
    );
}

// ---------------------------------------------------------------------------
// LEDGER-EVENT-005 — a Split rescales the open lots of the split's symbol. A
// 2-for-1 split (ratio 2:1) on a 10-share lot should leave a 20-share lot with
// basis unchanged (split mechanics; LEDGER-SPLIT-*).
// ---------------------------------------------------------------------------

// @spec LEDGER-EVENT-005
#[test]
fn split_rescales_open_lots_of_its_symbol() {
    let events = vec![
        ev("e-buy", 1, 100, buy("L1", "AMZN", 10 * SHARE, 1000, 0)),
        ev("e-split", 2, 150, split("AMZN", 2, 1)),
    ];

    let snap = replay(&events, &no_marks());

    assert_eq!(snap.open_lots.len(), 1, "the lot survives the split");
    let lot = &snap.open_lots[0].lot;
    assert_eq!(
        lot.remaining_qty,
        MicroShares(20 * SHARE),
        "2-for-1 split rescales 10 shares → 20 shares"
    );
    assert_eq!(
        lot.remaining_basis_cents,
        Cents(10_000),
        "basis is untouched by a split: scale(10 × 1000) = 10000 cents"
    );
}

// ---------------------------------------------------------------------------
// LEDGER-EVENT-006 — a Reversal replays as though its target had never occurred
// (the target filtered out of the fold). Reversing the Buy must remove its lot
// from the snapshot entirely (and, since the Sell depended on it, this scenario
// reverses only a standalone Buy with no dependents).
// ---------------------------------------------------------------------------

// @spec LEDGER-EVENT-006
#[test]
fn reversal_replays_as_if_target_never_occurred() {
    let events = vec![
        ev("e-b1", 1, 100, buy("L1", "AMZN", 10 * SHARE, 1000, 0)),
        ev("e-b2", 2, 101, buy("L2", "AMZN", 4 * SHARE, 2000, 0)),
        // Reverse the second Buy: lot L2 must vanish from the fold.
        ev("e-rev", 3, 102, reversal("e-b2")),
    ];

    let snap = replay(&events, &no_marks());

    assert_eq!(
        snap.open_lots.len(),
        1,
        "reversing e-b2 leaves only lot L1 in the fold"
    );
    assert_eq!(
        snap.open_lots[0].lot.id, "L1",
        "the reversed target's lot (L2) is filtered out of replay"
    );
    let pos = snap.positions.get("AMZN").expect("AMZN position present");
    assert_eq!(
        pos.total_qty,
        MicroShares(10 * SHARE),
        "only L1's 10 shares remain; L2's 4 are filtered out"
    );
}

// ---------------------------------------------------------------------------
// LEDGER-EVENT-007 — on Buy, Vest, and Sell, the system records `platform` and
// the optional `tracking_code` as tranche metadata, WITHOUT using them in any
// accounting computation. We assert the lot carries the recorded platform &
// tracking_code (recorded), and that two otherwise-identical Buys differing only
// in metadata produce identical basis/quantity (not used in computation).
// ---------------------------------------------------------------------------

// @spec LEDGER-EVENT-007
#[test]
fn buy_records_platform_and_tracking_code_as_inert_metadata() {
    let with_meta = vec![ev(
        "e-buy",
        1,
        100,
        buy_meta(
            "L1",
            "AMZN",
            10 * SHARE,
            1000,
            0,
            "fidelity",
            Some("RSU-2024-a"),
        ),
    )];
    let without_meta = vec![ev(
        "e-buy",
        1,
        100,
        buy_meta("L1", "AMZN", 10 * SHARE, 1000, 0, "schwab", None),
    )];

    let snap_meta = replay(&with_meta, &no_marks());
    let snap_plain = replay(&without_meta, &no_marks());

    // Metadata is RECORDED on the lot.
    let lot = &snap_meta.open_lots[0].lot;
    assert_eq!(lot.platform, "fidelity", "platform recorded as metadata");
    assert_eq!(
        lot.tracking_code,
        Some("RSU-2024-a".to_string()),
        "tracking_code recorded as metadata"
    );

    // Metadata is NOT used in any accounting computation: differing platform /
    // tracking_code yields identical basis and quantity.
    assert_eq!(
        snap_meta.open_lots[0].lot.remaining_basis_cents,
        snap_plain.open_lots[0].lot.remaining_basis_cents,
        "platform/tracking_code do not affect computed basis"
    );
    assert_eq!(
        snap_meta.open_lots[0].lot.remaining_qty, snap_plain.open_lots[0].lot.remaining_qty,
        "platform/tracking_code do not affect quantity"
    );
}

// ---------------------------------------------------------------------------
// LEDGER-EVENT-007 (Sell facet) — the Sell branch must likewise carry platform
// and tracking_code as inert metadata. We validate that a well-formed Sell on
// the lot's platform is accepted (metadata recorded, not blocking accounting),
// exercised through the append-time `validate` entry point.
// ---------------------------------------------------------------------------

// @spec LEDGER-EVENT-007
#[test]
fn sell_carries_metadata_without_affecting_accounting() {
    let accepted = vec![ev("e-buy", 1, 100, buy("L1", "AMZN", 10 * SHARE, 1000, 0))];
    let candidate = ev(
        "e-sell",
        2,
        200,
        LedgerEventKind::Sell {
            sale_id: "S1".to_string(),
            symbol: "AMZN".to_string(),
            qty: MicroShares(4 * SHARE),
            unit_price_cents: Cents(1500),
            fees_cents: Cents(0),
            lot_refs: vec![],
            accrues_to_state: Some("VA".to_string()),
            platform: PLATFORM.to_string(),
            tracking_code: Some("trk-1".to_string()),
        },
    );

    // A Sell with recorded metadata on the lot's platform is well-formed; the
    // metadata is inert and must not cause a rejection.
    assert_eq!(
        validate(&accepted, &candidate),
        Ok(()),
        "metadata-bearing Sell on the lot's platform validates (metadata inert)"
    );
}

// ---------------------------------------------------------------------------
// LEDGER-EVENT-001 (validate facet) — append-time validation runs against the
// replayed-so-far prefix in Seq order. A Reversal whose target is filtered must
// be evaluated against the post-filter prefix. This guards that validate also
// honours the Seq fold (not a Date ordering) when checking the candidate.
// ---------------------------------------------------------------------------

// @spec LEDGER-EVENT-006
#[test]
fn reversal_of_unknown_target_is_rejected() {
    let accepted = vec![ev("e-buy", 1, 100, buy("L1", "AMZN", 10 * SHARE, 1000, 0))];
    // Reversal targeting an event that does not exist → BadReversal.
    let candidate = ev("e-rev", 2, 101, reversal("does-not-exist"));

    assert_eq!(
        validate(&accepted, &candidate),
        Err(LedgerError::BadReversal),
        "a Reversal of an unknown target is rejected (its effect cannot be filtered)"
    );
}
