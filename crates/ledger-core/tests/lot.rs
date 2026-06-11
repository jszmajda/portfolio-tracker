//! Red-phase TDD tests for the LEDGER-LOT-* EARS specs of `ledger-core`.
//!
//! These exercise the public `replay` entry point (the verified accounting
//! fold). The scaffold stubs `replay` with `unimplemented!()`, so every test
//! here COMPILES and FAILS at runtime (panics on the stub) — RED, not
//! fail-to-compile. Each test carries a `// @spec LEDGER-LOT-NNN` comment on the
//! spec it directly exercises.
//!
//! Specs covered (see docs/intent/ledger-core/ledger-core-specs.md):
//!   LEDGER-LOT-001 — open lot tracks non-negative remaining_qty / remaining_basis
//!   LEDGER-LOT-002 — partial consume: rounded proportional consumed_basis
//!   LEDGER-LOT-003 — final-share consume: residual swept, basis to zero
//!   LEDGER-LOT-004 — explicit lot_refs consume exactly the named (lot, qty) pairs
//!   LEDGER-LOT-005 — FIFO fallback in ascending (acquire_date, open_seq), same platform
//!   LEDGER-LOT-006 — every lot gets a stable, log-unique LotId a later Sell can name

use ledger_core::{
    replay, LedgerEvent, LedgerEventKind, LotRef, Marks, OpenLot, Snapshot,
};
use pt_core::{Cents, Date, MicroShares, Seq};

// ---------------------------------------------------------------------------
// Test helpers — build LedgerEvents tersely. SHARE_SCALE = 1_000_000, so a
// whole share is 1_000_000 MicroShares and a price is Cents per whole share.
// ---------------------------------------------------------------------------

const WHOLE: i64 = 1_000_000; // one whole share, in MicroShares

fn shares(n: i64) -> MicroShares {
    MicroShares(n * WHOLE)
}

fn buy(
    id: &str,
    seq: u64,
    date: i32,
    lot_id: &str,
    symbol: &str,
    qty: MicroShares,
    unit_price_cents: i64,
    fees_cents: i64,
    platform: &str,
) -> LedgerEvent {
    LedgerEvent {
        id: id.to_string(),
        seq: Seq(seq),
        date: Date(date),
        kind: LedgerEventKind::Buy {
            lot_id: lot_id.to_string(),
            symbol: symbol.to_string(),
            qty,
            unit_price_cents: Cents(unit_price_cents),
            fees_cents: Cents(fees_cents),
            platform: platform.to_string(),
            tracking_code: None,
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn sell(
    id: &str,
    seq: u64,
    date: i32,
    sale_id: &str,
    symbol: &str,
    qty: MicroShares,
    unit_price_cents: i64,
    fees_cents: i64,
    lot_refs: Vec<LotRef>,
    platform: &str,
) -> LedgerEvent {
    LedgerEvent {
        id: id.to_string(),
        seq: Seq(seq),
        date: Date(date),
        kind: LedgerEventKind::Sell {
            sale_id: sale_id.to_string(),
            symbol: symbol.to_string(),
            qty,
            unit_price_cents: Cents(unit_price_cents),
            fees_cents: Cents(fees_cents),
            lot_refs,
            accrues_to_state: None,
            platform: platform.to_string(),
            tracking_code: None,
        },
    }
}

fn lot_ref(lot_id: &str, qty: MicroShares) -> LotRef {
    LotRef {
        lot_id: lot_id.to_string(),
        qty,
    }
}

fn no_marks() -> Marks {
    Marks::new()
}

/// Find the (single) open lot in a snapshot by its LotId.
fn open_lot<'a>(snap: &'a Snapshot, lot_id: &str) -> Option<&'a OpenLot> {
    snap.open_lots.iter().find(|ol| ol.lot.id == lot_id)
}

// ---------------------------------------------------------------------------
// LEDGER-LOT-001 — While a lot is open, the system shall track its
// `remaining_qty` (MicroShares) and `remaining_basis_cents`, holding both
// non-negative.
// ---------------------------------------------------------------------------

// @spec LEDGER-LOT-001
#[test]
fn open_lot_tracks_remaining_qty_and_basis_non_negative() {
    // Buy 10 whole shares @ 500 cents + 30 cents fees.
    // total_basis = scale(10e6 × 500 / 1e6) + 30 = 5000 + 30 = 5030 cents.
    let log = vec![buy(
        "e1", 1, 100, "lotA", "AMZN", shares(10), 500, 30, "fidelity",
    )];

    let snap = replay(&log, &no_marks());

    let ol = open_lot(&snap, "lotA").expect("lotA should be open");
    assert_eq!(
        ol.lot.remaining_qty,
        shares(10),
        "remaining_qty should equal the bought quantity"
    );
    assert_eq!(
        ol.lot.remaining_basis_cents,
        Cents(5030),
        "remaining_basis_cents = scale(qty × price) + fees"
    );
    // Non-negativity invariant (LEDGER-LOT-001).
    assert!(ol.lot.remaining_qty.0 >= 0, "remaining_qty must be non-negative");
    assert!(
        ol.lot.remaining_basis_cents.0 >= 0,
        "remaining_basis_cents must be non-negative"
    );
}

// ---------------------------------------------------------------------------
// LEDGER-LOT-002 — When a Sell consumes `c` shares from a lot whose remaining
// quantity exceeds `c`, the system shall realize
// `consumed_basis = round_half_to_even(remaining_basis × c / remaining_qty)`
// and then decrement `remaining_basis` and `remaining_qty` by the consumed
// amounts.
// ---------------------------------------------------------------------------

// @spec LEDGER-LOT-002
#[test]
fn partial_consume_realizes_rounded_proportional_basis_and_decrements() {
    // Buy 3 whole shares, basis 1000 cents (price 333.33.. -> use direct).
    // total_basis = scale(3e6 × 1000 / 1e6) = 3000 cents over 3 shares.
    // Sell 1 whole share via FIFO: c = 1e6, remaining = 3e6, basis = 3000.
    // consumed_basis = round_half_to_even(3000 × 1e6 / 3e6) = 1000.
    // After: remaining_qty = 2e6, remaining_basis = 2000.
    let log = vec![
        buy("e1", 1, 100, "lotA", "AMZN", shares(3), 1000, 0, "fidelity"),
        sell(
            "e2",
            2,
            150,
            "s1",
            "AMZN",
            shares(1),
            1200,
            0,
            vec![], // FIFO
            "fidelity",
        ),
    ];

    let snap = replay(&log, &no_marks());

    // The lot is partially consumed and still open.
    let ol = open_lot(&snap, "lotA").expect("lotA should still be open");
    assert_eq!(
        ol.lot.remaining_qty,
        shares(2),
        "remaining_qty decremented by consumed c"
    );
    assert_eq!(
        ol.lot.remaining_basis_cents,
        Cents(2000),
        "remaining_basis decremented by consumed_basis (3000 - 1000)"
    );

    // The realized gain records consumed_basis = 1000.
    let rg = snap
        .realized_gains
        .iter()
        .find(|g| g.lot_id == "lotA" && g.sale_id == "s1")
        .expect("a RealizedGain for (s1, lotA)");
    assert_eq!(
        rg.basis_cents,
        Cents(1000),
        "consumed_basis = round_half_to_even(3000 × 1 / 3)"
    );
}

// @spec LEDGER-LOT-002
#[test]
fn partial_consume_uses_banker_rounding_for_consumed_basis() {
    // Construct a case where remaining_basis × c / remaining_qty has a .5 tie
    // so round_half_to_even is observable.
    // Buy 4 whole shares with total_basis 10 cents (price 2.5 -> use unit price
    // that scales: scale(4e6 × ? /1e6). We instead pick a basis directly via a
    // single buy whose scaled basis is 10: scale(4e6 × 250 / 1e6) = 1000. Not 10.
    // Use 2 shares, basis 5 cents: need scale(2e6 × p /1e6) = 5 -> impossible
    // with integer price (price 2 -> 4, price 3 -> 6). Instead, sell 1 of a
    // 2-share lot whose basis is 5 via fractional micro to force the tie.
    //
    // Simpler tie: basis 5 over qty 2, consume 1 -> 5×1/2 = 2.5 -> ties to
    // even -> 2. We get basis 5 by a price giving an odd scaled basis: buy
    // 2 shares with two fractional micro? Keep it concrete: buy qty 2_000_000
    // (=2 shares) at price 250 with fee 1: basis = scale(2e6×250/1e6)+1 = 500+1
    // = 501 over 2 shares. consume 1 -> 501×1/2 = 250.5 -> ties to even -> 250.
    let log = vec![
        buy("e1", 1, 100, "lotA", "AMZN", shares(2), 250, 1, "fidelity"),
        sell(
            "e2", 2, 150, "s1", "AMZN", shares(1), 300, 0, vec![], "fidelity",
        ),
    ];

    let snap = replay(&log, &no_marks());

    let rg = snap
        .realized_gains
        .iter()
        .find(|g| g.lot_id == "lotA" && g.sale_id == "s1")
        .expect("a RealizedGain for (s1, lotA)");
    assert_eq!(
        rg.basis_cents,
        Cents(250),
        "501 × 1 / 2 = 250.5 ties to even (250) under round_half_to_even"
    );
    // And the lot retains the complementary basis (501 - 250 = 251).
    let ol = open_lot(&snap, "lotA").expect("lotA should still be open");
    assert_eq!(ol.lot.remaining_basis_cents, Cents(251));
    assert_eq!(ol.lot.remaining_qty, shares(1));
}

// ---------------------------------------------------------------------------
// LEDGER-LOT-003 — When a Sell consumes a lot's final shares (`c` equals
// `remaining_qty`), the system shall realize `consumed_basis` equal to the
// lot's entire `remaining_basis` (residual swept), leaving `remaining_basis`
// zero.
// ---------------------------------------------------------------------------

// @spec LEDGER-LOT-003
#[test]
fn final_share_consume_sweeps_entire_basis_and_closes_lot() {
    // Buy 3 shares, then partial-sell 1 (leaves an odd residual basis), then
    // sell the final 2 — the final consume must sweep the WHOLE remaining
    // basis exactly (no rounding loss), leaving the lot with zero basis/qty.
    //
    // Buy 3 shares @ price giving basis 100 cents: scale(3e6 × ? /1e6) — use a
    // direct odd basis to make the sweep observable: price 1, fee 100 ->
    // basis = scale(3e6 × 1/1e6) + 100 = 3 + 100 = 103 over 3 shares.
    let log = vec![
        buy("e1", 1, 100, "lotA", "AMZN", shares(3), 1, 100, "fidelity"),
        // Partial sell of 1 share -> consumed_basis = round(103×1/3) = round(34.33) = 34.
        // remaining_basis = 103 - 34 = 69, remaining_qty = 2.
        sell(
            "e2", 2, 150, "s1", "AMZN", shares(1), 200, 0, vec![], "fidelity",
        ),
        // Final sell of the remaining 2 shares -> c == remaining_qty -> sweep.
        sell(
            "e3", 3, 160, "s2", "AMZN", shares(2), 200, 0, vec![], "fidelity",
        ),
    ];

    let snap = replay(&log, &no_marks());

    // The lot is fully consumed: it must NOT appear among open lots (or, if it
    // does, must carry zero remaining). We require it absent from open_lots.
    assert!(
        open_lot(&snap, "lotA").is_none(),
        "a fully-consumed lot should not be reported as open"
    );

    // The final RealizedGain (s2, lotA) sweeps the entire residual basis (69),
    // NOT the proportional rounded amount.
    let rg_final = snap
        .realized_gains
        .iter()
        .find(|g| g.lot_id == "lotA" && g.sale_id == "s2")
        .expect("a RealizedGain for (s2, lotA)");
    assert_eq!(
        rg_final.basis_cents,
        Cents(69),
        "final consume sweeps the entire residual basis (103 - 34 = 69)"
    );

    // Conservation: the two consumed_basis amounts sum to the full 103.
    let total_consumed: i64 = snap
        .realized_gains
        .iter()
        .filter(|g| g.lot_id == "lotA")
        .map(|g| g.basis_cents.0)
        .sum();
    assert_eq!(
        total_consumed, 103,
        "sum of consumed basis equals the lot's acquired total basis"
    );
}

// ---------------------------------------------------------------------------
// LEDGER-LOT-004 — When a Sell supplies explicit `lot_refs`
// (specific-identification), the system shall consume exactly the named
// `(lot, quantity)` pairs.
// ---------------------------------------------------------------------------

// @spec LEDGER-LOT-004
#[test]
fn specific_identification_consumes_exactly_the_named_lots() {
    // Two lots of the same symbol/platform. A Sell names ONLY the SECOND lot
    // (lotB), even though lotA is the FIFO-earlier lot. Specific-ID must
    // consume lotB exactly and leave lotA wholly untouched (defeating FIFO).
    let log = vec![
        buy("e1", 1, 100, "lotA", "AMZN", shares(5), 1000, 0, "fidelity"),
        buy("e2", 2, 110, "lotB", "AMZN", shares(5), 2000, 0, "fidelity"),
        sell(
            "e3",
            3,
            150,
            "s1",
            "AMZN",
            shares(2),
            3000,
            0,
            vec![lot_ref("lotB", shares(2))], // name lotB specifically
            "fidelity",
        ),
    ];

    let snap = replay(&log, &no_marks());

    // lotA is the FIFO-earlier lot but was NOT named: it must be fully intact.
    let ol_a = open_lot(&snap, "lotA").expect("lotA should be untouched");
    assert_eq!(
        ol_a.lot.remaining_qty,
        shares(5),
        "specific-ID must NOT consume the un-named (FIFO-earlier) lotA"
    );

    // lotB was named: it gives up exactly 2 shares.
    let ol_b = open_lot(&snap, "lotB").expect("lotB should still be open");
    assert_eq!(
        ol_b.lot.remaining_qty,
        shares(3),
        "named lotB consumed exactly the designated 2 shares"
    );

    // Exactly one RealizedGain, against lotB.
    let gains_for_sale: Vec<_> = snap
        .realized_gains
        .iter()
        .filter(|g| g.sale_id == "s1")
        .collect();
    assert_eq!(
        gains_for_sale.len(),
        1,
        "exactly one consumed (lot, qty) pair was named"
    );
    assert_eq!(
        gains_for_sale[0].lot_id, "lotB",
        "the named lot is the one consumed"
    );
}

// @spec LEDGER-LOT-004
#[test]
fn specific_identification_consumes_multiple_named_pairs_exactly() {
    // Name two pairs across two lots; each pair must be consumed exactly.
    let log = vec![
        buy("e1", 1, 100, "lotA", "AMZN", shares(5), 1000, 0, "fidelity"),
        buy("e2", 2, 110, "lotB", "AMZN", shares(5), 2000, 0, "fidelity"),
        sell(
            "e3",
            3,
            150,
            "s1",
            "AMZN",
            shares(3), // 1 from lotA + 2 from lotB
            3000,
            0,
            vec![lot_ref("lotA", shares(1)), lot_ref("lotB", shares(2))],
            "fidelity",
        ),
    ];

    let snap = replay(&log, &no_marks());

    let ol_a = open_lot(&snap, "lotA").expect("lotA still open");
    let ol_b = open_lot(&snap, "lotB").expect("lotB still open");
    assert_eq!(ol_a.lot.remaining_qty, shares(4), "lotA gave up exactly 1");
    assert_eq!(ol_b.lot.remaining_qty, shares(3), "lotB gave up exactly 2");

    // One RealizedGain per named pair.
    let mut gains: Vec<_> = snap
        .realized_gains
        .iter()
        .filter(|g| g.sale_id == "s1")
        .map(|g| g.lot_id.clone())
        .collect();
    gains.sort();
    assert_eq!(
        gains,
        vec!["lotA".to_string(), "lotB".to_string()],
        "one RealizedGain per named (lot, qty) pair"
    );
}

// ---------------------------------------------------------------------------
// LEDGER-LOT-005 — When a Sell supplies no `lot_refs`, the system shall consume
// open lots of the sale's symbol on the sale's platform in ascending
// `(acquire_date, open_seq)` order (FIFO) until the sale quantity is satisfied.
// ---------------------------------------------------------------------------

// @spec LEDGER-LOT-005
#[test]
fn fifo_consumes_in_ascending_acquire_date_then_open_seq() {
    // Three lots: lotEarly (date 100), lotMidA & lotMidB (both date 110, seq 2
    // and 3 -> open_seq breaks the tie), all fidelity/AMZN. A FIFO Sell of 8
    // shares must drain lotEarly (5) fully, then lotMidA (3 of 5), leaving
    // lotMidB wholly untouched.
    let log = vec![
        buy("e1", 1, 100, "lotEarly", "AMZN", shares(5), 1000, 0, "fidelity"),
        buy("e2", 2, 110, "lotMidA", "AMZN", shares(5), 1000, 0, "fidelity"),
        buy("e3", 3, 110, "lotMidB", "AMZN", shares(5), 1000, 0, "fidelity"),
        sell(
            "e4", 4, 200, "s1", "AMZN", shares(8), 1500, 0, vec![], "fidelity",
        ),
    ];

    let snap = replay(&log, &no_marks());

    // lotEarly fully consumed (earliest acquire_date).
    assert!(
        open_lot(&snap, "lotEarly").is_none(),
        "FIFO drains the earliest-acquired lot first"
    );
    // lotMidA (open_seq 2) consumed before lotMidB (open_seq 3): 3 of 5 gone.
    let ol_mid_a = open_lot(&snap, "lotMidA").expect("lotMidA partly open");
    assert_eq!(
        ol_mid_a.lot.remaining_qty,
        shares(2),
        "same-date tie broken by ascending open_seq: lotMidA next"
    );
    // lotMidB untouched (latest open_seq).
    let ol_mid_b = open_lot(&snap, "lotMidB").expect("lotMidB untouched");
    assert_eq!(
        ol_mid_b.lot.remaining_qty,
        shares(5),
        "lotMidB (later open_seq) not yet reached by FIFO"
    );
}

// @spec LEDGER-LOT-005
#[test]
fn fifo_selects_only_lots_on_the_sales_platform() {
    // Two lots, same symbol, DIFFERENT platforms. A FIFO Sell on "schwab" must
    // consume only the schwab lot, leaving the (FIFO-earlier) fidelity lot
    // untouched — platform binds lot selection.
    let log = vec![
        buy("e1", 1, 100, "lotFid", "AMZN", shares(5), 1000, 0, "fidelity"),
        buy("e2", 2, 110, "lotSch", "AMZN", shares(5), 1000, 0, "schwab"),
        sell(
            "e3", 3, 200, "s1", "AMZN", shares(2), 1500, 0, vec![], "schwab",
        ),
    ];

    let snap = replay(&log, &no_marks());

    // The earlier fidelity lot is on a different platform: untouched.
    let ol_fid = open_lot(&snap, "lotFid").expect("lotFid untouched");
    assert_eq!(
        ol_fid.lot.remaining_qty,
        shares(5),
        "FIFO must not cross platforms: fidelity lot untouched on a schwab sell"
    );
    // The schwab lot gives up the 2 shares.
    let ol_sch = open_lot(&snap, "lotSch").expect("lotSch partly open");
    assert_eq!(
        ol_sch.lot.remaining_qty,
        shares(3),
        "FIFO consumed the same-platform (schwab) lot"
    );
}

// ---------------------------------------------------------------------------
// LEDGER-LOT-006 — The system shall assign every lot a stable, log-unique
// `LotId` by which a later Sell may designate it.
// ---------------------------------------------------------------------------

// @spec LEDGER-LOT-006
#[test]
fn lots_carry_stable_log_unique_ids_designatable_by_later_sell() {
    // Open two lots with distinct LotIds. (1) Their ids must be preserved and
    // unique in the snapshot; (2) a later Sell must be able to designate one by
    // that exact id (specific-ID), proving the id is a stable handle.
    let log = vec![
        buy("e1", 1, 100, "lotA", "AMZN", shares(5), 1000, 0, "fidelity"),
        buy("e2", 2, 110, "lotB", "AMZN", shares(5), 2000, 0, "fidelity"),
        sell(
            "e3",
            3,
            150,
            "s1",
            "AMZN",
            shares(1),
            3000,
            0,
            vec![lot_ref("lotA", shares(1))], // designate by the stable id
            "fidelity",
        ),
    ];

    let snap = replay(&log, &no_marks());

    // Open lot ids are exactly the opened ids and are log-unique.
    let mut ids: Vec<String> =
        snap.open_lots.iter().map(|ol| ol.lot.id.clone()).collect();
    ids.sort();
    assert_eq!(
        ids,
        vec!["lotA".to_string(), "lotB".to_string()],
        "every opened lot is reported under its stable, log-unique LotId"
    );

    // The Sell designated lotA by its id, so the realized gain references it.
    let rg = snap
        .realized_gains
        .iter()
        .find(|g| g.sale_id == "s1")
        .expect("a RealizedGain for s1");
    assert_eq!(
        rg.lot_id, "lotA",
        "a later Sell designates the lot by its stable LotId"
    );
    // And lotA reflects the designated consumption.
    let ol_a = open_lot(&snap, "lotA").expect("lotA still open");
    assert_eq!(ol_a.lot.remaining_qty, shares(4));
}
