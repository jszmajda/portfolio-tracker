//! Red-phase TDD tests for the LEDGER-PNL-* EARS specs of `ledger-core`.
//!
//! Each test carries a `// @spec LEDGER-PNL-NNN` comment naming the spec it
//! directly exercises. They are written against the *real* scaffolded API
//! (`replay`, `Snapshot`, `Position`, `OpenLot`, `RealizedGain`, the
//! `LedgerEvent`/`LedgerEventKind` taxonomy) — whose entry-point bodies are
//! stubbed with `unimplemented!()`. The suite therefore COMPILES and FAILS at
//! runtime (RED), as TDD requires; it does not fail-to-compile.
//!
//! All amounts are integer `Cents`; quantities are `MicroShares`
//! (`SHARE_SCALE = 1_000_000`). Prices are cents per *whole* share, so a value
//! is `scale(qty_micro × price_cents / 1_000_000)`, round-half-to-even, once at
//! the `Cents` boundary; fees are already in `Cents` and added after the scale.

use std::collections::BTreeMap;

use ledger_core::{replay, LedgerEvent, LedgerEventKind, LotRef, Marks, Snapshot};
use pt_core::{Cents, Date, MicroShares, Seq};

// ---------------------------------------------------------------------------
// Construction helpers (keep the spec arithmetic in the foreground).
// ---------------------------------------------------------------------------

const SHARE_SCALE: i64 = 1_000_000;

/// `n` whole shares as `MicroShares`.
fn shares(n: i64) -> MicroShares {
    MicroShares(n * SHARE_SCALE)
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
    BTreeMap::new()
}

fn marks(pairs: &[(&str, i64)]) -> Marks {
    pairs
        .iter()
        .map(|(s, c)| (s.to_string(), Cents(*c)))
        .collect()
}

/// Find the single `RealizedGain` for a given lot id (panics if not exactly one).
fn gain_for_lot<'a>(snap: &'a Snapshot, lot_id: &str) -> &'a ledger_core::RealizedGain {
    let matches: Vec<_> = snap
        .realized_gains
        .iter()
        .filter(|g| g.lot_id == lot_id)
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one RealizedGain for lot {lot_id}, found {}",
        matches.len()
    );
    matches[0]
}

// ---------------------------------------------------------------------------
// LEDGER-PNL-001 — gross proceeds = scale(qty × unit_price), net = gross − fees.
// ---------------------------------------------------------------------------

// @spec LEDGER-PNL-001
#[test]
fn sell_computes_gross_and_net_proceeds() {
    // Buy 10 sh @ $1.00 (100c) with no fees: basis = 1000c.
    // Sell 10 sh @ $1.50 (150c) with 25c fees:
    //   gross = scale(10_000_000 × 150 / 1_000_000) = 1500c
    //   net   = 1500 − 25 = 1475c
    let events = vec![
        buy("e1", 1, 100, "L1", "AMZN", shares(10), 100, 0, "fidelity"),
        sell(
            "e2",
            2,
            110,
            "S1",
            "AMZN",
            shares(10),
            150,
            25,
            vec![lot_ref("L1", shares(10))],
            "fidelity",
        ),
    ];
    let snap = replay(&events, &no_marks());

    // Single lot consumed => the lone RealizedGain carries the full net proceeds.
    let g = gain_for_lot(&snap, "L1");
    assert_eq!(
        g.proceeds_cents,
        Cents(1475),
        "net proceeds must be gross (1500) minus fees (25)"
    );
}

// ---------------------------------------------------------------------------
// LEDGER-PNL-002 — multi-lot Sell allocates net proceeds by largest-remainder,
// so per-lot proceeds sum EXACTLY to net proceeds.
// ---------------------------------------------------------------------------

// @spec LEDGER-PNL-002
#[test]
fn multi_lot_sell_allocates_proceeds_by_largest_remainder_summing_exactly() {
    // Three equal lots of 1 share each on the same symbol/platform, FIFO.
    // Sell 3 sh @ price chosen so net proceeds = 100c, which splits 3 ways as
    // 34 + 33 + 33 (largest-remainder hands the +1 residual to the first lot in
    // ascending (acquire_date, open_seq) order).
    //   gross = scale(3_000_000 × 100 / 1_000_000) ... pick price 100 => 300c.
    //   Use fees = 200c so net = 300 − 200 = 100c. 100 / 3 = 33 r 1.
    let events = vec![
        buy("e1", 1, 100, "L1", "AMZN", shares(1), 50, 0, "fidelity"),
        buy("e2", 2, 101, "L2", "AMZN", shares(1), 50, 0, "fidelity"),
        buy("e3", 3, 102, "L3", "AMZN", shares(1), 50, 0, "fidelity"),
        // FIFO sell (empty lot_refs) of all 3 shares.
        sell(
            "e4",
            4,
            200,
            "S1",
            "AMZN",
            shares(3),
            100,
            200,
            vec![],
            "fidelity",
        ),
    ];
    let snap = replay(&events, &no_marks());

    let p1 = gain_for_lot(&snap, "L1").proceeds_cents.0;
    let p2 = gain_for_lot(&snap, "L2").proceeds_cents.0;
    let p3 = gain_for_lot(&snap, "L3").proceeds_cents.0;

    // Sum must be EXACTLY net proceeds (100c) — the headline largest-remainder
    // guarantee.
    assert_eq!(
        p1 + p2 + p3,
        100,
        "per-lot proceeds must sum to net proceeds"
    );
    // The +1 residual goes to the earliest lot (L1) in ascending FIFO order.
    assert_eq!(p1, 34, "earliest lot receives the residual unit");
    assert_eq!(p2, 33);
    assert_eq!(p3, 33);
}

// ---------------------------------------------------------------------------
// LEDGER-PNL-003 — per consumed (lot, qty): gain = allocated_proceeds − basis,
// which MAY be negative.
// ---------------------------------------------------------------------------

// @spec LEDGER-PNL-003
#[test]
fn realized_gain_is_proceeds_minus_basis_and_may_be_negative() {
    // Buy 10 sh @ $2.00 (200c): basis = 2000c.
    // Sell 10 sh @ $1.00 (100c), no fees: net proceeds = 1000c.
    //   gain = 1000 − 2000 = −1000c (a loss).
    let events = vec![
        buy("e1", 1, 100, "L1", "AMZN", shares(10), 200, 0, "fidelity"),
        sell(
            "e2",
            2,
            110,
            "S1",
            "AMZN",
            shares(10),
            100,
            0,
            vec![lot_ref("L1", shares(10))],
            "fidelity",
        ),
    ];
    let snap = replay(&events, &no_marks());

    let g = gain_for_lot(&snap, "L1");
    assert_eq!(
        g.basis_cents,
        Cents(2000),
        "consumed basis is the full lot basis"
    );
    assert_eq!(g.proceeds_cents, Cents(1000));
    assert_eq!(
        g.gain_cents,
        Cents(-1000),
        "gain = proceeds − basis and may be negative"
    );
}

// ---------------------------------------------------------------------------
// LEDGER-PNL-004 — holding_days = sale_date − acquire_date (in days); zero for
// a same-day sale.
// ---------------------------------------------------------------------------

// @spec LEDGER-PNL-004
#[test]
fn holding_days_is_sale_date_minus_acquire_date() {
    // Acquire on day 100, partial sell day 430 (330 days held);
    // remainder sold same day as acquisition (day 100) — holding_days == 0.
    let events = vec![
        buy("e1", 1, 100, "L1", "AMZN", shares(10), 100, 0, "fidelity"),
        // Same-day sell of half the lot: holding_days == 0.
        sell(
            "e2",
            2,
            100,
            "S0",
            "AMZN",
            shares(5),
            100,
            0,
            vec![lot_ref("L1", shares(5))],
            "fidelity",
        ),
        // Later sell of the remainder, 330 days after acquisition.
        sell(
            "e3",
            3,
            430,
            "S1",
            "AMZN",
            shares(5),
            100,
            0,
            vec![lot_ref("L1", shares(5))],
            "fidelity",
        ),
    ];
    let snap = replay(&events, &no_marks());

    let same_day = snap
        .realized_gains
        .iter()
        .find(|g| g.sale_id == "S0")
        .expect("same-day sale gain present");
    assert_eq!(same_day.holding_days, 0, "same-day sale => holding_days 0");

    let later = snap
        .realized_gains
        .iter()
        .find(|g| g.sale_id == "S1")
        .expect("later sale gain present");
    assert_eq!(
        later.holding_days, 330,
        "holding_days = sale_date(430) − acquire_date(100)"
    );
}

// ---------------------------------------------------------------------------
// LEDGER-PNL-005 — RealizedGain reports holding_days and accrues_to_state, but
// does NOT classify LT/ST and does NOT compute tax (those belong to `tax`).
// ---------------------------------------------------------------------------

// @spec LEDGER-PNL-005
#[test]
fn realized_gain_reports_holding_days_and_accrues_to_state_without_tax_classification() {
    // accrues_to_state is stored on the Sell and must surface, untouched, on
    // each RealizedGain it produces. ledger-core does not interpret it and
    // produces no LT/ST flag or tax figure (the RealizedGain struct has no such
    // field — see the API), so we assert the carried-through metadata directly.
    let events = vec![
        buy("e1", 1, 100, "L1", "AMZN", shares(10), 100, 0, "fidelity"),
        LedgerEvent {
            id: "e2".to_string(),
            seq: Seq(2),
            date: Date(900),
            kind: LedgerEventKind::Sell {
                sale_id: "S1".to_string(),
                symbol: "AMZN".to_string(),
                qty: shares(10),
                unit_price_cents: Cents(150),
                fees_cents: Cents(0),
                lot_refs: vec![lot_ref("L1", shares(10))],
                accrues_to_state: Some("NY".to_string()),
                platform: "fidelity".to_string(),
                tracking_code: None,
            },
        },
    ];
    let snap = replay(&events, &no_marks());

    let g = gain_for_lot(&snap, "L1");
    // holding_days is reported (800 = 900 − 100) — well over a year, yet
    // ledger-core does NOT collapse it to a long/short flag.
    assert_eq!(g.holding_days, 800, "holding_days reported, not classified");
    assert_eq!(
        g.accrues_to_state,
        Some("NY".to_string()),
        "accrues_to_state carried through verbatim for the tax leaf"
    );
}

// ---------------------------------------------------------------------------
// LEDGER-PNL-006 — unrealized P&L for a marked symbol =
// Σ over open lots (scale(mark × remaining_qty) − remaining_basis).
// ---------------------------------------------------------------------------

// @spec LEDGER-PNL-006
#[test]
fn unrealized_pnl_sums_per_lot_mark_value_minus_basis() {
    // Two open lots of AMZN:
    //   L1: 10 sh, basis 1000c (bought @100c).
    //   L2:  5 sh, basis  750c (bought @150c).
    // Mark = 200c/share.
    //   L1 value = scale(10_000_000 × 200 / 1e6) = 2000 ; unreal 2000 − 1000 = 1000
    //   L2 value = scale( 5_000_000 × 200 / 1e6) = 1000 ; unreal 1000 −  750 =  250
    //   symbol unrealized = 1000 + 250 = 1250c
    let events = vec![
        buy("e1", 1, 100, "L1", "AMZN", shares(10), 100, 0, "fidelity"),
        buy("e2", 2, 101, "L2", "AMZN", shares(5), 150, 0, "fidelity"),
    ];
    let snap = replay(&events, &marks(&[("AMZN", 200)]));

    let pos = snap.positions.get("AMZN").expect("AMZN position present");
    assert_eq!(
        pos.unrealized_cents,
        Some(Cents(1250)),
        "unrealized = Σ (scale(mark × qty) − basis) over open lots"
    );
}

// ---------------------------------------------------------------------------
// LEDGER-PNL-007 — no mark for a symbol with open lots => unrealized absent
// (degraded), never zero.
// ---------------------------------------------------------------------------

// @spec LEDGER-PNL-007
#[test]
fn unmarked_symbol_with_open_lots_reports_absent_unrealized_not_zero() {
    let events = vec![buy(
        "e1",
        1,
        100,
        "L1",
        "AMZN",
        shares(10),
        100,
        0,
        "fidelity",
    )];
    // No mark supplied for AMZN.
    let snap = replay(&events, &no_marks());

    let pos = snap
        .positions
        .get("AMZN")
        .expect("AMZN position present even when unmarked");
    assert_eq!(
        pos.unrealized_cents, None,
        "missing mark => unrealized None (degraded), never Some(0)"
    );
}

// ---------------------------------------------------------------------------
// LEDGER-PNL-008 — a mark for a symbol with NO open lots is ignored, not an
// error.
// ---------------------------------------------------------------------------

// @spec LEDGER-PNL-008
#[test]
fn mark_for_symbol_with_no_open_lots_is_ignored_without_error() {
    // Buy then fully sell AMZN: no open lots remain. Supply marks for AMZN
    // (now empty) and an unrelated TSLA never held. replay must not panic, and
    // must not invent an open position from the stray mark.
    let events = vec![
        buy("e1", 1, 100, "L1", "AMZN", shares(10), 100, 0, "fidelity"),
        sell(
            "e2",
            2,
            110,
            "S1",
            "AMZN",
            shares(10),
            150,
            0,
            vec![lot_ref("L1", shares(10))],
            "fidelity",
        ),
    ];
    let snap = replay(&events, &marks(&[("AMZN", 999), ("TSLA", 123)]));

    // No OpenLot should exist (the only lot was fully consumed).
    assert!(
        snap.open_lots.is_empty(),
        "fully-consumed symbol leaves no open lots despite a stray mark"
    );
    // TSLA was never held; a stray mark must not fabricate a position.
    assert!(
        !snap.positions.contains_key("TSLA"),
        "a mark for a never-held symbol must be ignored, not create a position"
    );
}

// ---------------------------------------------------------------------------
// LEDGER-PNL-009 — per-symbol Position aggregates open qty, open basis,
// cumulative realized P&L (Σ that symbol's RealizedGain.gain), and unrealized
// (when a mark is available).
// ---------------------------------------------------------------------------

// @spec LEDGER-PNL-009
#[test]
fn position_aggregates_qty_basis_realized_and_unrealized() {
    // L1: buy 10 sh @100c (basis 1000c), L2: buy 10 sh @100c (basis 1000c).
    // Sell 10 sh @150c, no fees, against L1: realized gain = 1500 − 1000 = 500c.
    // Remaining open: only L2 (10 sh, basis 1000c).
    // Mark 200c => unrealized = scale(10_000_000 × 200 / 1e6) − 1000 = 2000−1000 = 1000c.
    let events = vec![
        buy("e1", 1, 100, "L1", "AMZN", shares(10), 100, 0, "fidelity"),
        buy("e2", 2, 101, "L2", "AMZN", shares(10), 100, 0, "fidelity"),
        sell(
            "e3",
            3,
            200,
            "S1",
            "AMZN",
            shares(10),
            150,
            0,
            vec![lot_ref("L1", shares(10))],
            "fidelity",
        ),
    ];
    let snap = replay(&events, &marks(&[("AMZN", 200)]));

    let pos = snap.positions.get("AMZN").expect("AMZN position present");
    assert_eq!(pos.total_qty, shares(10), "only L2's 10 shares remain open");
    assert_eq!(
        pos.total_basis_cents,
        Cents(1000),
        "open basis is L2's remaining basis"
    );
    assert_eq!(
        pos.realized_pnl_cents,
        Cents(500),
        "cumulative realized = Σ RealizedGain.gain for AMZN"
    );
    assert_eq!(
        pos.unrealized_cents,
        Some(Cents(1000)),
        "unrealized present when a mark is supplied"
    );
}

// ---------------------------------------------------------------------------
// LEDGER-PNL-010 — per-open-lot unrealized exposed on the Snapshot
// (scale(mark × remaining_qty) − remaining_basis), or absent when degraded.
// ---------------------------------------------------------------------------

// @spec LEDGER-PNL-010
#[test]
fn open_lots_expose_per_lot_unrealized_or_absent_when_degraded() {
    // AMZN lot is marked => per-lot unrealized present; GOOG lot has no mark =>
    // per-lot unrealized None (degraded), proving the rounding site lives once
    // inside the kernel and degradation is per-symbol on the lot too.
    //   AMZN L1: 10 sh basis 1000c, mark 200c => 2000 − 1000 = 1000c.
    //   GOOG L2:  4 sh basis  800c, no mark    => None.
    let events = vec![
        buy("e1", 1, 100, "L1", "AMZN", shares(10), 100, 0, "fidelity"),
        buy("e2", 2, 101, "L2", "GOOG", shares(4), 200, 0, "fidelity"),
    ];
    let snap = replay(&events, &marks(&[("AMZN", 200)]));

    let amzn_lot = snap
        .open_lots
        .iter()
        .find(|ol| ol.lot.id == "L1")
        .expect("AMZN open lot present");
    assert_eq!(
        amzn_lot.unrealized_cents,
        Some(Cents(1000)),
        "marked lot exposes scale(mark × qty) − basis"
    );

    let goog_lot = snap
        .open_lots
        .iter()
        .find(|ol| ol.lot.id == "L2")
        .expect("GOOG open lot present");
    assert_eq!(
        goog_lot.unrealized_cents, None,
        "degraded (unmarked) lot exposes None, not zero"
    );
}
