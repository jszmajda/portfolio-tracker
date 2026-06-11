//! RED tests for the LEDGER-SPLIT-* EARS specs of `ledger-core`.
//!
//! These exercise Split mechanics through the public `replay` entry point: open
//! lots via Buy/Vest, apply a `Split`, and inspect the resulting `Snapshot`'s
//! open lots (`remaining_qty`, `remaining_basis_cents`, `acquire_date`). The
//! scaffold's `replay` is `unimplemented!()`, so every test compiles and FAILS
//! at runtime (RED) until the kernel fold lands.
//!
//! Specs covered (ledger-core-specs.md → "Splits"):
//!   LEDGER-SPLIT-001 — rescale remaining_qty by round_half_to_even(qty*num/den)
//!   LEDGER-SPLIT-002 — leave remaining_basis and acquire_date unchanged
//!   LEDGER-SPLIT-003 — affect only OPEN lots of the split's symbol

use ledger_core::{replay, LedgerEvent, LedgerEventKind, LotRef, Marks, OpenLot};
use pt_core::{Cents, Date, MicroShares, Seq};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// One whole share in MicroShares (1e-6 granularity → 1_000_000 micro/share).
const SHARE: i64 = 1_000_000;

fn buy(
    seq: u64,
    date: i32,
    lot_id: &str,
    symbol: &str,
    qty_micro: i64,
    unit_price_cents: i64,
    fees_cents: i64,
) -> LedgerEvent {
    LedgerEvent {
        id: format!("evt-{seq}"),
        seq: Seq(seq),
        date: Date(date),
        kind: LedgerEventKind::Buy {
            lot_id: lot_id.to_string(),
            symbol: symbol.to_string(),
            qty: MicroShares(qty_micro),
            unit_price_cents: Cents(unit_price_cents),
            fees_cents: Cents(fees_cents),
            platform: "BROKER".to_string(),
            tracking_code: None,
        },
    }
}

fn split(seq: u64, date: i32, symbol: &str, ratio_num: i64, ratio_den: i64) -> LedgerEvent {
    LedgerEvent {
        id: format!("evt-{seq}"),
        seq: Seq(seq),
        date: Date(date),
        kind: LedgerEventKind::Split {
            symbol: symbol.to_string(),
            ratio_num,
            ratio_den,
        },
    }
}

fn sell_fifo(
    seq: u64,
    date: i32,
    sale_id: &str,
    symbol: &str,
    qty_micro: i64,
    unit_price_cents: i64,
) -> LedgerEvent {
    LedgerEvent {
        id: format!("evt-{seq}"),
        seq: Seq(seq),
        date: Date(date),
        kind: LedgerEventKind::Sell {
            sale_id: sale_id.to_string(),
            symbol: symbol.to_string(),
            qty: MicroShares(qty_micro),
            unit_price_cents: Cents(unit_price_cents),
            fees_cents: Cents(0),
            lot_refs: Vec::<LotRef>::new(),
            accrues_to_state: None,
            platform: "BROKER".to_string(),
            tracking_code: None,
        },
    }
}

fn no_marks() -> Marks {
    Marks::new()
}

/// Find the open lot with the given id in a snapshot, panicking if absent.
fn find_lot<'a>(open_lots: &'a [OpenLot], lot_id: &str) -> &'a OpenLot {
    open_lots
        .iter()
        .find(|ol| ol.lot.id == lot_id)
        .unwrap_or_else(|| panic!("expected open lot {lot_id} in snapshot"))
}

// ---------------------------------------------------------------------------
// LEDGER-SPLIT-001 — rescale remaining_qty to round_half_to_even(qty*num/den)
// ---------------------------------------------------------------------------

// @spec LEDGER-SPLIT-001
#[test]
fn split_2_for_1_doubles_remaining_qty() {
    // Buy 10 shares of AMZN, then a 2:1 split → remaining_qty = 20 shares.
    let events = vec![
        buy(1, 100, "lot-A", "AMZN", 10 * SHARE, 5000, 0),
        split(2, 110, "AMZN", 2, 1),
    ];
    let snap = replay(&events, &no_marks());
    let lot = find_lot(&snap.open_lots, "lot-A");
    assert_eq!(
        lot.lot.remaining_qty,
        MicroShares(20 * SHARE),
        "2:1 split should double 10 shares to 20"
    );
}

// @spec LEDGER-SPLIT-001
#[test]
fn split_3_for_2_uses_round_half_to_even() {
    // Buy 5 shares, apply a 3:2 split. 5 * 3 / 2 = 7.5 shares exactly.
    // round_half_to_even(7_500_000) at micro-share granularity: 7_500_000 is an
    // exact integer in MicroShares, so the result is 7_500_000 micro (7.5 sh).
    // To force a true tie in the rounding site, use a qty whose scaled product
    // lands exactly on a .5 micro-share: qty = 1 micro-share, ratio 3:2 →
    // 1 * 3 / 2 = 1.5 → round_half_to_even → 2 (nearest even).
    let events = vec![
        buy(1, 100, "lot-A", "AMZN", 5 * SHARE, 5000, 0),
        split(2, 110, "AMZN", 3, 2),
    ];
    let snap = replay(&events, &no_marks());
    let lot = find_lot(&snap.open_lots, "lot-A");
    assert_eq!(
        lot.lot.remaining_qty,
        MicroShares(7_500_000),
        "3:2 split of 5 shares = 7.5 shares (7_500_000 micro)"
    );
}

// @spec LEDGER-SPLIT-001
#[test]
fn split_rounds_half_to_even_at_micro_granularity() {
    // qty = 1 micro-share; a 3:2 split → 1 * 3 / 2 = 1.5 micro → ties to the
    // nearest EVEN integer = 2 micro (banker's rounding, NOT 1 and NOT 2 via
    // round-half-up of an odd source). 3 micro * 3 / 2 = 4.5 → 4 (even).
    let to_even_up = vec![
        buy(1, 100, "lot-A", "AMZN", 1, 5000, 0),
        split(2, 110, "AMZN", 3, 2),
    ];
    let snap = replay(&to_even_up, &no_marks());
    let lot = find_lot(&snap.open_lots, "lot-A");
    assert_eq!(
        lot.lot.remaining_qty,
        MicroShares(2),
        "1 micro * 3/2 = 1.5 → round half to EVEN = 2"
    );

    let to_even_down = vec![
        buy(1, 100, "lot-B", "AMZN", 3, 5000, 0),
        split(2, 110, "AMZN", 3, 2),
    ];
    let snap2 = replay(&to_even_down, &no_marks());
    let lot2 = find_lot(&snap2.open_lots, "lot-B");
    assert_eq!(
        lot2.lot.remaining_qty,
        MicroShares(4),
        "3 micro * 3/2 = 4.5 → round half to EVEN = 4"
    );
}

// @spec LEDGER-SPLIT-001
#[test]
fn reverse_split_shrinks_remaining_qty() {
    // A 1:10 reverse split of 100 shares → 10 shares.
    let events = vec![
        buy(1, 100, "lot-A", "AMZN", 100 * SHARE, 5000, 0),
        split(2, 110, "AMZN", 1, 10),
    ];
    let snap = replay(&events, &no_marks());
    let lot = find_lot(&snap.open_lots, "lot-A");
    assert_eq!(
        lot.lot.remaining_qty,
        MicroShares(10 * SHARE),
        "1:10 reverse split should shrink 100 shares to 10"
    );
}

// ---------------------------------------------------------------------------
// LEDGER-SPLIT-002 — leave remaining_basis and acquisition date unchanged
// ---------------------------------------------------------------------------

// @spec LEDGER-SPLIT-002
#[test]
fn split_leaves_remaining_basis_unchanged() {
    // Buy 10 shares @ 5000c = 50000c basis (no fees). After a 2:1 split, qty
    // doubles but the basis must be byte-identical (per-share basis halves).
    let events = vec![
        buy(1, 100, "lot-A", "AMZN", 10 * SHARE, 5000, 0),
        split(2, 110, "AMZN", 2, 1),
    ];
    let snap = replay(&events, &no_marks());
    let lot = find_lot(&snap.open_lots, "lot-A");
    assert_eq!(
        lot.lot.remaining_basis_cents,
        Cents(50_000),
        "split must NOT rescale remaining_basis (basis conserved)"
    );
}

// @spec LEDGER-SPLIT-002
#[test]
fn split_leaves_acquire_date_unchanged() {
    // The holding clock is unbroken: acquire_date survives the split.
    let acquire = 100;
    let events = vec![
        buy(1, acquire, "lot-A", "AMZN", 10 * SHARE, 5000, 0),
        split(2, 110, "AMZN", 3, 1),
    ];
    let snap = replay(&events, &no_marks());
    let lot = find_lot(&snap.open_lots, "lot-A");
    assert_eq!(
        lot.lot.acquire_date,
        Date(acquire),
        "split must preserve the lot's acquisition date (holding period)"
    );
}

// @spec LEDGER-SPLIT-002
#[test]
fn split_preserves_total_symbol_basis_across_multiple_lots() {
    // Two lots of AMZN; a split rescales both quantities but the SUM of basis
    // is conserved exactly (split basis-neutrality).
    let events = vec![
        buy(1, 100, "lot-A", "AMZN", 4 * SHARE, 2500, 0), // basis 10_000c
        buy(2, 101, "lot-B", "AMZN", 6 * SHARE, 3000, 0), // basis 18_000c
        split(3, 110, "AMZN", 2, 1),
    ];
    let snap = replay(&events, &no_marks());
    let a = find_lot(&snap.open_lots, "lot-A");
    let b = find_lot(&snap.open_lots, "lot-B");
    assert_eq!(a.lot.remaining_basis_cents, Cents(10_000));
    assert_eq!(b.lot.remaining_basis_cents, Cents(18_000));
    let total: i64 = a.lot.remaining_basis_cents.0 + b.lot.remaining_basis_cents.0;
    assert_eq!(total, 28_000, "total symbol basis unchanged by split");
}

// ---------------------------------------------------------------------------
// LEDGER-SPLIT-003 — affect only OPEN lots of the split's symbol
// ---------------------------------------------------------------------------

// @spec LEDGER-SPLIT-003
#[test]
fn split_leaves_other_symbols_untouched() {
    // A split on AMZN must not touch a GOOG lot's remaining_qty.
    let events = vec![
        buy(1, 100, "lot-AMZN", "AMZN", 10 * SHARE, 5000, 0),
        buy(2, 100, "lot-GOOG", "GOOG", 8 * SHARE, 4000, 0),
        split(3, 110, "AMZN", 2, 1),
    ];
    let snap = replay(&events, &no_marks());
    let amzn = find_lot(&snap.open_lots, "lot-AMZN");
    let goog = find_lot(&snap.open_lots, "lot-GOOG");
    assert_eq!(
        amzn.lot.remaining_qty,
        MicroShares(20 * SHARE),
        "AMZN lot should be doubled by its 2:1 split"
    );
    assert_eq!(
        goog.lot.remaining_qty,
        MicroShares(8 * SHARE),
        "GOOG lot must be untouched by an AMZN split"
    );
}

// @spec LEDGER-SPLIT-003
#[test]
fn split_affects_all_open_lots_of_the_symbol() {
    // Every OPEN lot of the split's symbol is rescaled, not just the first.
    let events = vec![
        buy(1, 100, "lot-A", "AMZN", 10 * SHARE, 5000, 0),
        buy(2, 101, "lot-B", "AMZN", 4 * SHARE, 5200, 0),
        split(3, 110, "AMZN", 2, 1),
    ];
    let snap = replay(&events, &no_marks());
    let a = find_lot(&snap.open_lots, "lot-A");
    let b = find_lot(&snap.open_lots, "lot-B");
    assert_eq!(a.lot.remaining_qty, MicroShares(20 * SHARE));
    assert_eq!(b.lot.remaining_qty, MicroShares(8 * SHARE));
}

// @spec LEDGER-SPLIT-003
#[test]
fn split_leaves_fully_consumed_lots_untouched() {
    // lot-A is fully sold (closed) BEFORE the split; lot-B remains open. The
    // split must rescale only lot-B and never resurrect or rescale closed lot-A.
    let events = vec![
        buy(1, 100, "lot-A", "AMZN", 10 * SHARE, 5000, 0),
        buy(2, 101, "lot-B", "AMZN", 10 * SHARE, 5000, 0),
        // FIFO sells the whole of lot-A (10 shares) — lot-A closes.
        sell_fifo(3, 105, "sale-1", "AMZN", 10 * SHARE, 6000),
        split(4, 110, "AMZN", 2, 1),
    ];
    let snap = replay(&events, &no_marks());

    // lot-A must NOT appear among open lots (fully consumed).
    assert!(
        snap.open_lots.iter().all(|ol| ol.lot.id != "lot-A"),
        "fully-consumed lot-A must not be an open lot"
    );

    // lot-B is the only open lot and is doubled by the 2:1 split.
    let b = find_lot(&snap.open_lots, "lot-B");
    assert_eq!(
        b.lot.remaining_qty,
        MicroShares(20 * SHARE),
        "open lot-B should be doubled by the post-sale 2:1 split"
    );
}

// @spec LEDGER-SPLIT-003
#[test]
fn split_rescales_only_remaining_qty_of_partially_consumed_lot() {
    // A lot partially consumed before the split is rescaled on its REMAINING
    // quantity only (already-disposed shares are frame-relative, not rescaled).
    // Buy 10 sh; sell 4 sh (remaining 6 sh); 2:1 split → remaining 12 sh.
    let events = vec![
        buy(1, 100, "lot-A", "AMZN", 10 * SHARE, 5000, 0),
        sell_fifo(2, 105, "sale-1", "AMZN", 4 * SHARE, 6000),
        split(3, 110, "AMZN", 2, 1),
    ];
    let snap = replay(&events, &no_marks());
    let a = find_lot(&snap.open_lots, "lot-A");
    assert_eq!(
        a.lot.remaining_qty,
        MicroShares(12 * SHARE),
        "2:1 split should double the 6 remaining shares to 12"
    );
}
