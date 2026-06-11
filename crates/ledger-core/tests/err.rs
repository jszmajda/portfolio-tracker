//! RED-phase TDD tests for the LEDGER-ERR-* append-time validation specs of
//! `ledger-core` (see `docs/intent/ledger-core/ledger-core-specs.md` →
//! "Validation & Errors").
//!
//! Every LEDGER-ERR-NNN spec is exercised by calling the real public entry
//! point `ledger_core::validate(accepted, candidate)` and asserting that the
//! returned `Err` carries the exact `LedgerError` variant the spec names. The
//! scaffold stubs `validate` with `unimplemented!()`, so each test currently
//! PANICS (RED). Once the kernel lands the assertions pin the right rejection.
//!
//! These tests target the *append-time* validator (the filtered-prefix check),
//! never `replay` — by design a stored log is valid by construction, so replay
//! never sees an event it must reject.

use pt_core::{Cents, Date, MicroShares, Seq, MONEY_CAP, SHARE_SCALE};

use ledger_core::{validate, LedgerError, LedgerEvent, LedgerEventKind, LotRef};

// ---------------------------------------------------------------------------
// Event constructors. Keep them total and explicit so each test reads as a
// minimal scenario against the real API surface.
// ---------------------------------------------------------------------------

fn buy(
    id: &str,
    seq: u64,
    date: i32,
    lot_id: &str,
    symbol: &str,
    qty: i64,
    unit_price_cents: i64,
    platform: &str,
) -> LedgerEvent {
    LedgerEvent {
        id: id.to_string(),
        seq: Seq(seq),
        date: Date(date),
        kind: LedgerEventKind::Buy {
            lot_id: lot_id.to_string(),
            symbol: symbol.to_string(),
            qty: MicroShares(qty),
            unit_price_cents: Cents(unit_price_cents),
            fees_cents: Cents(0),
            platform: platform.to_string(),
            tracking_code: None,
        },
    }
}

fn vest(
    id: &str,
    seq: u64,
    date: i32,
    lot_id: &str,
    symbol: &str,
    qty: i64,
    fmv_per_share_cents: i64,
    platform: &str,
) -> LedgerEvent {
    LedgerEvent {
        id: id.to_string(),
        seq: Seq(seq),
        date: Date(date),
        kind: LedgerEventKind::Vest {
            lot_id: lot_id.to_string(),
            symbol: symbol.to_string(),
            qty: MicroShares(qty),
            fmv_per_share_cents: Cents(fmv_per_share_cents),
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
    symbol: &str,
    qty: i64,
    unit_price_cents: i64,
    lot_refs: Vec<LotRef>,
    platform: &str,
) -> LedgerEvent {
    LedgerEvent {
        id: id.to_string(),
        seq: Seq(seq),
        date: Date(date),
        kind: LedgerEventKind::Sell {
            sale_id: id.to_string(),
            symbol: symbol.to_string(),
            qty: MicroShares(qty),
            unit_price_cents: Cents(unit_price_cents),
            fees_cents: Cents(0),
            lot_refs,
            accrues_to_state: None,
            platform: platform.to_string(),
            tracking_code: None,
        },
    }
}

fn split(id: &str, seq: u64, date: i32, symbol: &str, num: i64, den: i64) -> LedgerEvent {
    LedgerEvent {
        id: id.to_string(),
        seq: Seq(seq),
        date: Date(date),
        kind: LedgerEventKind::Split {
            symbol: symbol.to_string(),
            ratio_num: num,
            ratio_den: den,
        },
    }
}

fn reversal(id: &str, seq: u64, date: i32, target: &str) -> LedgerEvent {
    LedgerEvent {
        id: id.to_string(),
        seq: Seq(seq),
        date: Date(date),
        kind: LedgerEventKind::Reversal {
            target_event_id: target.to_string(),
        },
    }
}

fn lot_ref(lot_id: &str, qty: i64) -> LotRef {
    LotRef {
        lot_id: lot_id.to_string(),
        qty: MicroShares(qty),
    }
}

const ONE_SHARE: i64 = SHARE_SCALE; // 1_000_000 micro-shares = 1 whole share

// ===========================================================================
// LEDGER-ERR-001: validate against the replayed-so-far state, with reversed
// targets filtered out, before appending.
// ===========================================================================

// A Sell that names a lot whose opening Buy was reversed must be rejected: the
// reversed target is filtered out of the replayed-so-far state, so the lot is
// not present. This directly exercises "validate against the filtered prefix".
#[test]
// @spec LEDGER-ERR-001
fn err_001_validates_against_filtered_replayed_prefix() {
    let accepted = vec![
        buy("e1", 1, 100, "L1", "AMZN", 10 * ONE_SHARE, 5000, "fidelity"),
        reversal("e2", 2, 100, "e1"), // L1's opening Buy is filtered out
    ];
    // The candidate Sell designates L1, which no longer exists in the filtered
    // prefix — append-time validation must reject it.
    let candidate = sell(
        "s1",
        3,
        101,
        "AMZN",
        ONE_SHARE,
        6000,
        vec![lot_ref("L1", ONE_SHARE)],
        "fidelity",
    );

    let result = validate(&accepted, &candidate);
    assert!(
        result.is_err(),
        "ERR-001: a candidate referencing a reversed (filtered-out) lot must be \
         rejected when validated against the replayed-so-far state, got {result:?}"
    );
}

// ===========================================================================
// LEDGER-ERR-002: Buy/Vest/Sell with qty ≤ 0 → NonPositiveQty.
// ===========================================================================

#[test]
// @spec LEDGER-ERR-002
fn err_002_buy_with_zero_qty_is_non_positive() {
    let accepted: Vec<LedgerEvent> = vec![];
    let candidate = buy("b0", 1, 100, "L1", "AMZN", 0, 5000, "fidelity");

    assert_eq!(
        validate(&accepted, &candidate),
        Err(LedgerError::NonPositiveQty),
        "ERR-002: a Buy with qty == 0 must be rejected as NonPositiveQty"
    );
}

#[test]
// @spec LEDGER-ERR-002
fn err_002_vest_with_negative_qty_is_non_positive() {
    let accepted: Vec<LedgerEvent> = vec![];
    let candidate = vest("v0", 1, 100, "L1", "AMZN", -1, 5000, "fidelity");

    assert_eq!(
        validate(&accepted, &candidate),
        Err(LedgerError::NonPositiveQty),
        "ERR-002: a Vest with qty < 0 must be rejected as NonPositiveQty"
    );
}

#[test]
// @spec LEDGER-ERR-002
fn err_002_sell_with_zero_qty_is_non_positive() {
    let accepted = vec![buy(
        "b1",
        1,
        100,
        "L1",
        "AMZN",
        10 * ONE_SHARE,
        5000,
        "fidelity",
    )];
    let candidate = sell("s0", 2, 101, "AMZN", 0, 6000, vec![], "fidelity");

    assert_eq!(
        validate(&accepted, &candidate),
        Err(LedgerError::NonPositiveQty),
        "ERR-002: a Sell with qty == 0 must be rejected as NonPositiveQty"
    );
}

// ===========================================================================
// LEDGER-ERR-003: Buy/Vest reusing an existing LotId → DuplicateLotId.
// ===========================================================================

#[test]
// @spec LEDGER-ERR-003
fn err_003_buy_reusing_lot_id_is_duplicate() {
    let accepted = vec![buy(
        "b1",
        1,
        100,
        "L1",
        "AMZN",
        10 * ONE_SHARE,
        5000,
        "fidelity",
    )];
    // Reuses LotId "L1".
    let candidate = buy("b2", 2, 101, "L1", "AMZN", 5 * ONE_SHARE, 5100, "fidelity");

    assert_eq!(
        validate(&accepted, &candidate),
        Err(LedgerError::DuplicateLotId),
        "ERR-003: a Buy reusing an existing LotId must be rejected as DuplicateLotId"
    );
}

#[test]
// @spec LEDGER-ERR-003
fn err_003_vest_reusing_lot_id_is_duplicate() {
    let accepted = vec![buy(
        "b1",
        1,
        100,
        "L1",
        "AMZN",
        10 * ONE_SHARE,
        5000,
        "fidelity",
    )];
    // A Vest reusing the same LotId "L1".
    let candidate = vest("v2", 2, 101, "L1", "AMZN", 5 * ONE_SHARE, 5100, "fidelity");

    assert_eq!(
        validate(&accepted, &candidate),
        Err(LedgerError::DuplicateLotId),
        "ERR-003: a Vest reusing an existing LotId must be rejected as DuplicateLotId"
    );
}

// ===========================================================================
// LEDGER-ERR-004: a Sell naming a missing lot → UnknownLot; or a lot of
// another symbol → WrongSymbolLot.
// ===========================================================================

#[test]
// @spec LEDGER-ERR-004
fn err_004_sell_naming_missing_lot_is_unknown() {
    let accepted = vec![buy(
        "b1",
        1,
        100,
        "L1",
        "AMZN",
        10 * ONE_SHARE,
        5000,
        "fidelity",
    )];
    // Names "L_NOPE", which was never opened.
    let candidate = sell(
        "s1",
        2,
        101,
        "AMZN",
        ONE_SHARE,
        6000,
        vec![lot_ref("L_NOPE", ONE_SHARE)],
        "fidelity",
    );

    assert_eq!(
        validate(&accepted, &candidate),
        Err(LedgerError::UnknownLot),
        "ERR-004: a Sell naming a non-existent lot must be rejected as UnknownLot"
    );
}

#[test]
// @spec LEDGER-ERR-004
fn err_004_sell_naming_other_symbols_lot_is_wrong_symbol() {
    let accepted = vec![buy(
        "b1",
        1,
        100,
        "L1",
        "AMZN",
        10 * ONE_SHARE,
        5000,
        "fidelity",
    )];
    // The Sell is for GOOG but designates lot L1, which belongs to AMZN.
    let candidate = sell(
        "s1",
        2,
        101,
        "GOOG",
        ONE_SHARE,
        6000,
        vec![lot_ref("L1", ONE_SHARE)],
        "fidelity",
    );

    assert_eq!(
        validate(&accepted, &candidate),
        Err(LedgerError::WrongSymbolLot),
        "ERR-004: a Sell naming a lot of another symbol must be rejected as WrongSymbolLot"
    );
}

// ===========================================================================
// LEDGER-ERR-005: designated quantities don't sum to the sale qty →
// LotRefsMismatch.
// ===========================================================================

#[test]
// @spec LEDGER-ERR-005
fn err_005_lot_refs_not_summing_to_sale_qty() {
    let accepted = vec![buy(
        "b1",
        1,
        100,
        "L1",
        "AMZN",
        10 * ONE_SHARE,
        5000,
        "fidelity",
    )];
    // Sale qty is 5 shares but the single lot_ref designates only 3.
    let candidate = sell(
        "s1",
        2,
        101,
        "AMZN",
        5 * ONE_SHARE,
        6000,
        vec![lot_ref("L1", 3 * ONE_SHARE)],
        "fidelity",
    );

    assert_eq!(
        validate(&accepted, &candidate),
        Err(LedgerError::LotRefsMismatch),
        "ERR-005: designated quantities not summing to the sale qty must be \
         rejected as LotRefsMismatch"
    );
}

// ===========================================================================
// LEDGER-ERR-006: the same lot_id named twice within one Sell → DuplicateLotRef.
// ===========================================================================

#[test]
// @spec LEDGER-ERR-006
fn err_006_same_lot_named_twice() {
    let accepted = vec![buy(
        "b1",
        1,
        100,
        "L1",
        "AMZN",
        10 * ONE_SHARE,
        5000,
        "fidelity",
    )];
    // L1 appears twice in lot_refs (3 + 2 = 5 shares — sums correctly, so the
    // rejection must be the duplicate, not a mismatch).
    let candidate = sell(
        "s1",
        2,
        101,
        "AMZN",
        5 * ONE_SHARE,
        6000,
        vec![lot_ref("L1", 3 * ONE_SHARE), lot_ref("L1", 2 * ONE_SHARE)],
        "fidelity",
    );

    assert_eq!(
        validate(&accepted, &candidate),
        Err(LedgerError::DuplicateLotRef),
        "ERR-006: naming the same lot twice in one Sell must be rejected as DuplicateLotRef"
    );
}

// ===========================================================================
// LEDGER-ERR-007: a designated/FIFO lot not on the sale's platform →
// WrongPlatform.
// ===========================================================================

#[test]
// @spec LEDGER-ERR-007
fn err_007_designated_lot_on_other_platform() {
    let accepted = vec![buy(
        "b1",
        1,
        100,
        "L1",
        "AMZN",
        10 * ONE_SHARE,
        5000,
        "fidelity",
    )];
    // Lot L1 lives on "fidelity"; the Sell is on "schwab" and designates L1.
    let candidate = sell(
        "s1",
        2,
        101,
        "AMZN",
        ONE_SHARE,
        6000,
        vec![lot_ref("L1", ONE_SHARE)],
        "schwab",
    );

    assert_eq!(
        validate(&accepted, &candidate),
        Err(LedgerError::WrongPlatform),
        "ERR-007: designating a lot that is not on the sale's platform must be \
         rejected as WrongPlatform"
    );
}

// ===========================================================================
// LEDGER-ERR-008: specific-ID or FIFO selection can't cover the sale qty →
// InsufficientShares (no short sales).
// ===========================================================================

#[test]
// @spec LEDGER-ERR-008
fn err_008_fifo_cannot_cover_sale_qty() {
    // Only 2 shares open, FIFO fallback (no lot_refs), selling 5.
    let accepted = vec![buy(
        "b1",
        1,
        100,
        "L1",
        "AMZN",
        2 * ONE_SHARE,
        5000,
        "fidelity",
    )];
    let candidate = sell(
        "s1",
        2,
        101,
        "AMZN",
        5 * ONE_SHARE,
        6000,
        vec![],
        "fidelity",
    );

    assert_eq!(
        validate(&accepted, &candidate),
        Err(LedgerError::InsufficientShares),
        "ERR-008: a FIFO Sell exceeding total open shares must be rejected as \
         InsufficientShares (no short sales)"
    );
}

#[test]
// @spec LEDGER-ERR-008
fn err_008_specific_id_exceeds_lot_remaining() {
    // Lot L1 has 2 shares; the Sell designates 5 from it (sums to sale qty, so
    // the rejection is under-supply, not a refs mismatch).
    let accepted = vec![buy(
        "b1",
        1,
        100,
        "L1",
        "AMZN",
        2 * ONE_SHARE,
        5000,
        "fidelity",
    )];
    let candidate = sell(
        "s1",
        2,
        101,
        "AMZN",
        5 * ONE_SHARE,
        6000,
        vec![lot_ref("L1", 5 * ONE_SHARE)],
        "fidelity",
    );

    assert_eq!(
        validate(&accepted, &candidate),
        Err(LedgerError::InsufficientShares),
        "ERR-008: a specific-ID Sell exceeding a lot's remaining_qty must be \
         rejected as InsufficientShares"
    );
}

// ===========================================================================
// LEDGER-ERR-009: a Split with ratio_num < 1 or ratio_den < 1 → BadSplitRatio.
// ===========================================================================

#[test]
// @spec LEDGER-ERR-009
fn err_009_split_zero_numerator() {
    let accepted = vec![buy(
        "b1",
        1,
        100,
        "L1",
        "AMZN",
        10 * ONE_SHARE,
        5000,
        "fidelity",
    )];
    let candidate = split("sp1", 2, 101, "AMZN", 0, 1);

    assert_eq!(
        validate(&accepted, &candidate),
        Err(LedgerError::BadSplitRatio),
        "ERR-009: a Split with ratio_num < 1 must be rejected as BadSplitRatio"
    );
}

#[test]
// @spec LEDGER-ERR-009
fn err_009_split_zero_denominator() {
    let accepted = vec![buy(
        "b1",
        1,
        100,
        "L1",
        "AMZN",
        10 * ONE_SHARE,
        5000,
        "fidelity",
    )];
    let candidate = split("sp1", 2, 101, "AMZN", 2, 0);

    assert_eq!(
        validate(&accepted, &candidate),
        Err(LedgerError::BadSplitRatio),
        "ERR-009: a Split with ratio_den < 1 must be rejected as BadSplitRatio"
    );
}

// ===========================================================================
// LEDGER-ERR-010: a Reversal targeting an unknown event, an already-reversed
// event, or an event a surviving lower-Seq event depends on → BadReversal.
// ===========================================================================

#[test]
// @spec LEDGER-ERR-010
fn err_010_reversal_of_unknown_event() {
    let accepted = vec![buy(
        "b1",
        1,
        100,
        "L1",
        "AMZN",
        10 * ONE_SHARE,
        5000,
        "fidelity",
    )];
    // Targets "ghost", an event that does not exist.
    let candidate = reversal("r1", 2, 100, "ghost");

    assert_eq!(
        validate(&accepted, &candidate),
        Err(LedgerError::BadReversal),
        "ERR-010: a Reversal of an unknown event must be rejected as BadReversal"
    );
}

#[test]
// @spec LEDGER-ERR-010
fn err_010_reversal_of_already_reversed_event() {
    let accepted = vec![
        buy("b1", 1, 100, "L1", "AMZN", 10 * ONE_SHARE, 5000, "fidelity"),
        reversal("r1", 2, 100, "b1"), // b1 is already reversed
    ];
    // A second Reversal of the same target b1.
    let candidate = reversal("r2", 3, 100, "b1");

    assert_eq!(
        validate(&accepted, &candidate),
        Err(LedgerError::BadReversal),
        "ERR-010: a Reversal of an already-reversed event must be rejected as BadReversal"
    );
}

#[test]
// @spec LEDGER-ERR-010
fn err_010_reversal_of_depended_upon_event() {
    // A surviving Sell (s1) consumed lot L1 opened by b1; reversing b1 while
    // that dependent Sell survives must be rejected (dependents reverse first).
    let accepted = vec![
        buy("b1", 1, 100, "L1", "AMZN", 10 * ONE_SHARE, 5000, "fidelity"),
        sell(
            "s1",
            2,
            101,
            "AMZN",
            ONE_SHARE,
            6000,
            vec![lot_ref("L1", ONE_SHARE)],
            "fidelity",
        ),
    ];
    let candidate = reversal("r1", 3, 102, "b1");

    assert_eq!(
        validate(&accepted, &candidate),
        Err(LedgerError::BadReversal),
        "ERR-010: a Reversal of an event a surviving event depends on must be \
         rejected as BadReversal"
    );
}

// ===========================================================================
// LEDGER-ERR-011: a scaled Cents/MicroShares result exceeding MONEY_CAP →
// AmountOutOfRange.
// ===========================================================================

#[test]
// @spec LEDGER-ERR-011
fn err_011_buy_basis_exceeds_money_cap() {
    let accepted: Vec<LedgerEvent> = vec![];
    // scale(qty × unit_price) = qty_whole_shares × unit_price_cents. Choose a
    // whole-share count and a price whose product blows past MONEY_CAP (2^40).
    // 2_000_000 whole shares (in micro) × 1_000_000 cents/share = 2e12 cents,
    // well above 2^40 ≈ 1.0995e12.
    let huge_qty = 2_000_000 * ONE_SHARE;
    let candidate = buy("b1", 1, 100, "L1", "AMZN", huge_qty, 1_000_000, "fidelity");

    // Sanity: the scaled basis really does exceed the cap.
    let scaled_basis: i128 = (huge_qty as i128) * 1_000_000i128 / (SHARE_SCALE as i128);
    assert!(
        scaled_basis > MONEY_CAP as i128,
        "test setup: scaled basis {scaled_basis} should exceed MONEY_CAP {}",
        MONEY_CAP
    );

    assert_eq!(
        validate(&accepted, &candidate),
        Err(LedgerError::AmountOutOfRange),
        "ERR-011: a Buy whose scaled basis exceeds MONEY_CAP must be rejected as \
         AmountOutOfRange"
    );
}
