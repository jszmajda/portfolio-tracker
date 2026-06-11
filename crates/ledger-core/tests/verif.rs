//! Red (failing) tests for the LEDGER-VERIF-* verification invariants of
//! `ledger-core`. TDD: written BEFORE the kernel is implemented. The scaffold
//! compiles (signatures complete, bodies `unimplemented!()`), so these tests
//! compile and FAIL AT RUNTIME (panic) — the RED phase.
//!
//! Two layers cover each invariant:
//!   1. `#[cfg(kani)]` `#[kani::proof]` harness stubs — bounded model checking
//!      under `cargo kani` (compiled out under plain `cargo test`, so Kani is
//!      not a hard dependency).
//!   2. Runnable property/unit tests over bounded inputs — these gate under
//!      `cargo test -p ledger-core --test verif` even without Kani installed,
//!      and fail RED until `replay`/`validate` are implemented.
//!
//! Each test carries a `// @spec LEDGER-VERIF-NNN` comment on the test that
//! directly exercises that spec. Covers LEDGER-VERIF-001..008.

use std::collections::BTreeMap;

use ledger_core::{
    replay, validate, LedgerError, LedgerEvent, LedgerEventKind, LotRef, Marks, Snapshot,
};
use pt_core::{Cents, Date, MicroShares, Seq, SHARE_SCALE};

// ===========================================================================
// Test fixtures — small, bounded logs built from the real event taxonomy.
// All amounts are tiny so the verified MONEY_CAP bound is never approached.
// ===========================================================================

/// One whole share in MicroShares.
const SHARE: i64 = SHARE_SCALE; // 1_000_000

fn buy(
    id: &str,
    seq: u64,
    date: i32,
    lot_id: &str,
    symbol: &str,
    qty: i64,
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
            qty: MicroShares(qty),
            unit_price_cents: Cents(unit_price_cents),
            fees_cents: Cents(fees_cents),
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
    sale_id: &str,
    symbol: &str,
    qty: i64,
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
            qty: MicroShares(qty),
            unit_price_cents: Cents(unit_price_cents),
            fees_cents: Cents(fees_cents),
            lot_refs,
            accrues_to_state: None,
            platform: platform.to_string(),
            tracking_code: None,
        },
    }
}

fn split(id: &str, seq: u64, date: i32, symbol: &str, ratio_num: i64, ratio_den: i64) -> LedgerEvent {
    LedgerEvent {
        id: id.to_string(),
        seq: Seq(seq),
        date: Date(date),
        kind: LedgerEventKind::Split {
            symbol: symbol.to_string(),
            ratio_num,
            ratio_den,
        },
    }
}

fn reversal(id: &str, seq: u64, date: i32, target_event_id: &str) -> LedgerEvent {
    LedgerEvent {
        id: id.to_string(),
        seq: Seq(seq),
        date: Date(date),
        kind: LedgerEventKind::Reversal {
            target_event_id: target_event_id.to_string(),
        },
    }
}

fn lot_ref(lot_id: &str, qty: i64) -> LotRef {
    LotRef {
        lot_id: lot_id.to_string(),
        qty: MicroShares(qty),
    }
}

fn no_marks() -> Marks {
    BTreeMap::new()
}

/// Total acquired basis across all open lots + all realized gains' basis, per
/// the snapshot, for the named symbol.
fn observed_basis_for(snap: &Snapshot, symbol: &str) -> i64 {
    let open: i64 = snap
        .open_lots
        .iter()
        .filter(|ol| ol.lot.symbol == symbol)
        .map(|ol| ol.lot.remaining_basis_cents.0)
        .sum();
    let realized: i64 = snap
        .realized_gains
        .iter()
        .filter(|rg| rg.symbol == symbol)
        .map(|rg| rg.basis_cents.0)
        .sum();
    open + realized
}

// ===========================================================================
// LEDGER-VERIF-001 — basis conservation (headline).
// Σ remaining_basis + Σ realized_basis ≡ Σ acquired total_basis, per symbol,
// invariant ACROSS splits.
// ===========================================================================

// @spec LEDGER-VERIF-001
#[test]
fn verif_001_basis_conservation_across_buy_sell_split() {
    // Buy 10 @ $1.00 + $5 fees → total_basis = scale(10e6 * 100 / 1e6) + 500
    //   = 1000 + 500 = 1500 cents.
    // Split 2:1 (basis untouched), then sell 4 shares (some basis moves to
    // realized). Basis conservation must hold across the split and the sell.
    let acquired_basis = scale_i128(10 * SHARE as i128 * 100) as i64 + 500; // 1500

    let log = vec![
        buy("e1", 1, 100, "lotA", "AMZN", 10 * SHARE, 100, 500, "fidelity"),
        split("e2", 2, 110, "AMZN", 2, 1),
        sell("e3", 3, 120, "s1", "AMZN", 4 * SHARE, 110, 0, vec![], "fidelity"),
    ];

    let snap = replay(&log, &no_marks());

    let observed = observed_basis_for(&snap, "AMZN");
    assert_eq!(
        observed, acquired_basis,
        "Σ remaining_basis + Σ realized_basis must equal Σ acquired total_basis"
    );
}

// @spec LEDGER-VERIF-001
#[test]
fn verif_001_basis_conservation_full_consumption_zero_residual() {
    // Selling a lot's entire quantity must sweep its whole remaining_basis into
    // realized with zero residual (no rounding loss at closure).
    let acquired_basis = scale_i128(3 * SHARE as i128 * 333) as i64; // vest, no fees

    let log = vec![
        vest("e1", 1, 100, "lotV", "GOOG", 3 * SHARE, 333, "schwab"),
        sell("e2", 2, 130, "s1", "GOOG", 3 * SHARE, 333, 0, vec![], "schwab"),
    ];

    let snap = replay(&log, &no_marks());

    // After full consumption there should be no open GOOG lot.
    let open_qty: i64 = snap
        .open_lots
        .iter()
        .filter(|ol| ol.lot.symbol == "GOOG")
        .map(|ol| ol.lot.remaining_qty.0)
        .sum();
    assert_eq!(open_qty, 0, "fully-consumed lot must leave zero open quantity");

    let observed = observed_basis_for(&snap, "GOOG");
    assert_eq!(
        observed, acquired_basis,
        "all acquired basis must be conserved into the realized gain at closure"
    );
}

// ===========================================================================
// LEDGER-VERIF-002 — share accounting within a split epoch.
// Σ remaining_qty + Σ disposed_qty ≡ Σ acquired_qty, with a Split rescaling
// the aggregate to round(Σ × ratio_num/ratio_den).
// ===========================================================================

// @spec LEDGER-VERIF-002
#[test]
fn verif_002_share_accounting_within_epoch_no_split() {
    // Before any split: acquired 10 shares, sell 4 → 6 open + 4 disposed = 10.
    let log = vec![
        buy("e1", 1, 100, "lotA", "MSFT", 10 * SHARE, 200, 0, "fidelity"),
        sell("e2", 2, 110, "s1", "MSFT", 4 * SHARE, 210, 0, vec![], "fidelity"),
    ];

    let snap = replay(&log, &no_marks());

    let open_qty: i64 = snap
        .open_lots
        .iter()
        .filter(|ol| ol.lot.symbol == "MSFT")
        .map(|ol| ol.lot.remaining_qty.0)
        .sum();
    // Disposed = the quantity attributable to the realized gains. We reconstruct
    // disposed from the difference; share accounting requires open == 6 shares.
    assert_eq!(
        open_qty,
        6 * SHARE,
        "remaining open quantity must be acquired minus disposed (10 - 4 = 6)"
    );
}

// @spec LEDGER-VERIF-002
#[test]
fn verif_002_split_rescales_aggregate_quantity() {
    // 3-for-2 split of 10 shares → round(10 * 3 / 2) = 15 shares open, basis
    // unchanged. Tests the split's aggregate quantity rescale.
    let log = vec![
        buy("e1", 1, 100, "lotA", "TSLA", 10 * SHARE, 100, 0, "schwab"),
        split("e2", 2, 110, "TSLA", 3, 2),
    ];

    let snap = replay(&log, &no_marks());

    let open_qty: i64 = snap
        .open_lots
        .iter()
        .filter(|ol| ol.lot.symbol == "TSLA")
        .map(|ol| ol.lot.remaining_qty.0)
        .sum();
    assert_eq!(
        open_qty,
        15 * SHARE,
        "3-for-2 split must rescale 10 shares to round(10*3/2) = 15"
    );
}

// ===========================================================================
// LEDGER-VERIF-003 — non-negativity.
// Every remaining_qty ≥ 0 and every remaining_basis_cents ≥ 0 at all times.
// ===========================================================================

// @spec LEDGER-VERIF-003
#[test]
fn verif_003_non_negative_qty_and_basis() {
    let log = vec![
        buy("e1", 1, 100, "lotA", "NFLX", 5 * SHARE, 150, 25, "fidelity"),
        sell("e2", 2, 120, "s1", "NFLX", 5 * SHARE, 160, 10, vec![], "fidelity"),
        buy("e3", 3, 130, "lotB", "NFLX", 2 * SHARE, 170, 0, "fidelity"),
    ];

    let snap = replay(&log, &no_marks());

    for ol in &snap.open_lots {
        assert!(
            ol.lot.remaining_qty.0 >= 0,
            "remaining_qty must be non-negative, got {}",
            ol.lot.remaining_qty.0
        );
        assert!(
            ol.lot.remaining_basis_cents.0 >= 0,
            "remaining_basis_cents must be non-negative, got {}",
            ol.lot.remaining_basis_cents.0
        );
    }
}

// ===========================================================================
// LEDGER-VERIF-004 — no partial mutation on reject.
// A rejected candidate event leaves the log and state byte-identical.
// ===========================================================================

// @spec LEDGER-VERIF-004
#[test]
fn verif_004_reject_leaves_state_byte_identical() {
    // A valid accepted prefix.
    let accepted = vec![buy("e1", 1, 100, "lotA", "AMZN", 5 * SHARE, 100, 0, "fidelity")];

    // Snapshot of the accepted prefix before attempting the bad candidate.
    let before = replay(&accepted, &no_marks());

    // A candidate that must be rejected: a Sell of 100 shares against a lot that
    // only has 5 (InsufficientShares).
    let bad = sell("e2", 2, 110, "s1", "AMZN", 100 * SHARE, 100, 0, vec![], "fidelity");

    let result = validate(&accepted, &bad);
    assert!(result.is_err(), "over-large Sell must be rejected");

    // The replayed state must be byte-identical to before — rejection mutated
    // nothing. (Re-replay the same accepted prefix and compare.)
    let after = replay(&accepted, &no_marks());
    assert_eq!(
        before, after,
        "rejecting a candidate must leave replayed state byte-identical"
    );
}

// ===========================================================================
// LEDGER-VERIF-005 — replay determinism.
// replay(log, marks) is a pure function of (log, marks); identical input yields
// identical output.
// ===========================================================================

// @spec LEDGER-VERIF-005
#[test]
fn verif_005_replay_is_deterministic() {
    let log = vec![
        buy("e1", 1, 100, "lotA", "AMZN", 10 * SHARE, 100, 0, "fidelity"),
        buy("e2", 2, 100, "lotB", "AMZN", 10 * SHARE, 110, 0, "fidelity"),
        sell("e3", 3, 120, "s1", "AMZN", 15 * SHARE, 130, 0, vec![], "fidelity"),
    ];
    let mut marks: Marks = BTreeMap::new();
    marks.insert("AMZN".to_string(), Cents(140));

    let first = replay(&log, &marks);
    let second = replay(&log, &marks);

    assert_eq!(
        first, second,
        "replay over identical (log, marks) must yield identical Snapshots"
    );
}

// @spec LEDGER-VERIF-005
#[test]
fn verif_005_fifo_order_is_by_acquire_date_then_open_seq() {
    // Two same-date lots; FIFO must break the tie by open_seq (e1 before e2).
    // A 5-share sell with no lot_refs must consume lotA (open_seq 1) first.
    let log = vec![
        buy("e1", 1, 100, "lotA", "AMZN", 5 * SHARE, 100, 0, "fidelity"),
        buy("e2", 2, 100, "lotB", "AMZN", 5 * SHARE, 200, 0, "fidelity"),
        sell("e3", 3, 120, "s1", "AMZN", 5 * SHARE, 150, 0, vec![], "fidelity"),
    ];

    let snap = replay(&log, &no_marks());

    // FIFO consumed lotA fully; lotB remains open with 5 shares.
    let remaining_lot_ids: Vec<&str> = snap
        .open_lots
        .iter()
        .filter(|ol| ol.lot.remaining_qty.0 > 0)
        .map(|ol| ol.lot.id.as_str())
        .collect();
    assert_eq!(
        remaining_lot_ids,
        vec!["lotB"],
        "FIFO must consume lotA (lower open_seq) first, leaving lotB open"
    );
}

// ===========================================================================
// LEDGER-VERIF-006 — realized-gain correctness.
// For a Sell: Σ RealizedGain.gain ≡ net_proceeds − Σ consumed_basis; consumed
// quantities sum to the sale quantity; per-lot proceeds sum exactly to net
// proceeds.
// ===========================================================================

// @spec LEDGER-VERIF-006
#[test]
fn verif_006_realized_gain_sum_equals_net_minus_basis() {
    // Two lots, multi-lot FIFO sell crossing both. Net proceeds = gross - fees.
    // gross = scale(qty * unit_price). qty = 15 shares @ 130, fees 7.
    let qty = 15i128 * SHARE as i128;
    let gross = scale_i128(qty * 130) as i64;
    let fees = 7i64;
    let net_proceeds = gross - fees;

    let log = vec![
        buy("e1", 1, 100, "lotA", "AMZN", 10 * SHARE, 100, 0, "fidelity"),
        buy("e2", 2, 105, "lotB", "AMZN", 10 * SHARE, 120, 0, "fidelity"),
        sell("e3", 3, 120, "s1", "AMZN", 15 * SHARE, 130, fees, vec![], "fidelity"),
    ];

    let snap = replay(&log, &no_marks());

    let gains: Vec<_> = snap
        .realized_gains
        .iter()
        .filter(|rg| rg.sale_id == "s1")
        .collect();
    assert!(!gains.is_empty(), "the sell must produce realized gains");

    let sum_gain: i64 = gains.iter().map(|rg| rg.gain_cents.0).sum();
    let sum_basis: i64 = gains.iter().map(|rg| rg.basis_cents.0).sum();
    let sum_proceeds: i64 = gains.iter().map(|rg| rg.proceeds_cents.0).sum();

    assert_eq!(
        sum_proceeds, net_proceeds,
        "per-lot proceeds must sum exactly to net proceeds"
    );
    assert_eq!(
        sum_gain,
        net_proceeds - sum_basis,
        "Σ gain must equal net_proceeds − Σ consumed_basis"
    );
    // gain == proceeds - basis per lot.
    for rg in &gains {
        assert_eq!(
            rg.gain_cents.0,
            rg.proceeds_cents.0 - rg.basis_cents.0,
            "each RealizedGain.gain must equal proceeds − basis"
        );
    }
}

// @spec LEDGER-VERIF-006
#[test]
fn verif_006_consumed_quantities_sum_to_sale_quantity() {
    // Per-lot RealizedGains do not carry qty directly, but the sale must have
    // consumed exactly the sale quantity: pre-sale open qty − post-sale open qty
    // == sale qty. Acquired 10 + 10 = 20; sell 15 → 5 open remaining.
    let log = vec![
        buy("e1", 1, 100, "lotA", "AMZN", 10 * SHARE, 100, 0, "fidelity"),
        buy("e2", 2, 105, "lotB", "AMZN", 10 * SHARE, 120, 0, "fidelity"),
        sell("e3", 3, 120, "s1", "AMZN", 15 * SHARE, 130, 0, vec![], "fidelity"),
    ];

    let snap = replay(&log, &no_marks());

    let open_qty: i64 = snap
        .open_lots
        .iter()
        .filter(|ol| ol.lot.symbol == "AMZN")
        .map(|ol| ol.lot.remaining_qty.0)
        .sum();
    assert_eq!(
        open_qty,
        5 * SHARE,
        "consumed qty (15) must reduce 20 open shares to 5 (sale qty conserved)"
    );
}

// ===========================================================================
// LEDGER-VERIF-007 — split basis-neutrality.
// A Split changes no lot's remaining_basis and changes total symbol basis by 0.
// ===========================================================================

// @spec LEDGER-VERIF-007
#[test]
fn verif_007_split_is_basis_neutral() {
    let pre_log = vec![buy("e1", 1, 100, "lotA", "AAPL", 10 * SHARE, 100, 0, "fidelity")];
    let pre = replay(&pre_log, &no_marks());
    let pre_basis = observed_basis_for(&pre, "AAPL");

    let post_log = vec![
        buy("e1", 1, 100, "lotA", "AAPL", 10 * SHARE, 100, 0, "fidelity"),
        split("e2", 2, 110, "AAPL", 4, 1),
    ];
    let post = replay(&post_log, &no_marks());
    let post_basis = observed_basis_for(&post, "AAPL");

    assert_eq!(
        post_basis, pre_basis,
        "a Split must change total symbol basis by zero"
    );

    // And per-lot basis must be unchanged after the split.
    let lot_basis: i64 = post
        .open_lots
        .iter()
        .find(|ol| ol.lot.id == "lotA")
        .map(|ol| ol.lot.remaining_basis_cents.0)
        .expect("lotA must still be open after the split");
    assert_eq!(
        lot_basis,
        scale_i128(10 * SHARE as i128 * 100) as i64,
        "the split must leave the lot's remaining_basis untouched"
    );
}

// ===========================================================================
// LEDGER-VERIF-008 — Reversal totality.
// Re-folding with the target filtered out preserves every surviving event's
// precondition; replay stays total, shares and basis conserved.
// ===========================================================================

// @spec LEDGER-VERIF-008
#[test]
fn verif_008_reversal_of_buy_refolds_total() {
    // Buy lotA, Buy lotB, then Reverse the SECOND buy (no surviving dependent).
    // Re-fold must behave as though lotB never opened: only lotA's basis/qty.
    let acquired_basis_lot_a = scale_i128(10 * SHARE as i128 * 100) as i64;

    let log = vec![
        buy("e1", 1, 100, "lotA", "ORCL", 10 * SHARE, 100, 0, "fidelity"),
        buy("e2", 2, 105, "lotB", "ORCL", 7 * SHARE, 120, 0, "fidelity"),
        reversal("e3", 3, 110, "e2"),
    ];

    let snap = replay(&log, &no_marks());

    // lotB must be absent (filtered out); lotA conserved.
    let lot_ids: Vec<&str> = snap.open_lots.iter().map(|ol| ol.lot.id.as_str()).collect();
    assert!(
        !lot_ids.contains(&"lotB"),
        "the reversed Buy's lot must be filtered out of the re-fold"
    );
    let observed = observed_basis_for(&snap, "ORCL");
    assert_eq!(
        observed, acquired_basis_lot_a,
        "after reversing lotB's Buy, only lotA's basis survives (conserved)"
    );
}

// @spec LEDGER-VERIF-008
#[test]
fn verif_008_reversal_of_split_refolds_total() {
    // Buy lotA, Split 2:1, then Reverse the Split. Re-fold without the split
    // must leave the original (un-split) quantity, basis unchanged throughout.
    let log = vec![
        buy("e1", 1, 100, "lotA", "IBM", 10 * SHARE, 100, 0, "fidelity"),
        split("e2", 2, 110, "IBM", 2, 1),
        reversal("e3", 3, 120, "e2"),
    ];

    let snap = replay(&log, &no_marks());

    let open_qty: i64 = snap
        .open_lots
        .iter()
        .filter(|ol| ol.lot.symbol == "IBM")
        .map(|ol| ol.lot.remaining_qty.0)
        .sum();
    assert_eq!(
        open_qty,
        10 * SHARE,
        "reversing the Split must re-fold to the original 10 shares (not 20)"
    );

    let observed = observed_basis_for(&snap, "IBM");
    assert_eq!(
        observed,
        scale_i128(10 * SHARE as i128 * 100) as i64,
        "basis stays conserved through a reversed split"
    );
}

// @spec LEDGER-VERIF-008
#[test]
fn verif_008_reversal_rejected_when_surviving_event_depends() {
    // Buy lotA, Sell consuming lotA (a surviving dependent), then a Reversal of
    // the Buy must be REJECTED (BadReversal) — the dependent must be reversed
    // first. validate() against the accepted prefix must reject the Reversal.
    let accepted = vec![
        buy("e1", 1, 100, "lotA", "AMZN", 10 * SHARE, 100, 0, "fidelity"),
        sell("e2", 2, 110, "s1", "AMZN", 4 * SHARE, 120, 0, vec![lot_ref("lotA", 4 * SHARE)], "fidelity"),
    ];
    let candidate = reversal("e3", 3, 120, "e1");

    let result = validate(&accepted, &candidate);
    assert_eq!(
        result,
        Err(LedgerError::BadReversal),
        "reversing a Buy that a surviving Sell consumed must be rejected (BadReversal)"
    );
}

// ===========================================================================
// Local mirror of pt_core::scale for computing EXPECTED values in tests.
// We do NOT call pt_core::scale (it is itself a stubbed todo!()); instead we
// reproduce the documented rounding here so the test's *expected* side is
// independent of the code-under-test. This is round_half_to_even(micro / 1e6).
// ===========================================================================

fn scale_i128(micro: i128) -> i128 {
    round_half_to_even_i128(micro, SHARE_SCALE as i128)
}

/// Reference banker's rounding of `num / den` (den > 0), for expected values.
fn round_half_to_even_i128(num: i128, den: i128) -> i128 {
    assert!(den > 0, "denominator must be positive");
    let q = num.div_euclid(den);
    let r = num.rem_euclid(den); // 0 <= r < den
    let twice = 2 * r;
    if twice < den {
        q
    } else if twice > den {
        q + 1
    } else {
        // exactly halfway → round to even
        if q % 2 == 0 {
            q
        } else {
            q + 1
        }
    }
}

// ===========================================================================
// Kani bounded-model-checking harness STUBS (one per VERIF invariant).
//
// Compiled out under plain `cargo test` (Kani not a hard dependency); run with
// `cargo kani` when the Kani toolchain is installed. The crate already carries
// src/kani_proofs.rs with VERIF-001/004/006/008; these mirror the full set so
// each LEDGER-VERIF-* invariant has a proof obligation reachable from the test
// suite as well. Bodies are stubs (unimplemented!) so they fail until the
// kernel and its contracts land.
// ===========================================================================

#[cfg(kani)]
mod kani_harnesses {
    // @spec LEDGER-VERIF-001
    #[kani::proof]
    #[kani::unwind(4)]
    fn kani_verif_001_basis_conservation() {
        unimplemented!("construct a bounded log, replay, assert basis conservation")
    }

    // @spec LEDGER-VERIF-002
    #[kani::proof]
    #[kani::unwind(4)]
    fn kani_verif_002_share_accounting() {
        unimplemented!("bounded log incl. a Split; assert Σ qty within the split epoch")
    }

    // @spec LEDGER-VERIF-003
    #[kani::proof]
    #[kani::unwind(4)]
    fn kani_verif_003_non_negativity() {
        unimplemented!("assert remaining_qty >= 0 and remaining_basis_cents >= 0")
    }

    // @spec LEDGER-VERIF-004
    #[kani::proof]
    #[kani::unwind(4)]
    fn kani_verif_004_no_partial_mutation_on_reject() {
        unimplemented!("a rejected candidate leaves log/state byte-identical")
    }

    // @spec LEDGER-VERIF-005
    #[kani::proof]
    #[kani::unwind(4)]
    fn kani_verif_005_replay_determinism() {
        unimplemented!("replay(log, marks) twice yields identical Snapshots")
    }

    // @spec LEDGER-VERIF-006
    #[kani::proof]
    #[kani::unwind(4)]
    fn kani_verif_006_realized_gain_correctness() {
        unimplemented!("Σ gain ≡ net_proceeds − Σ consumed_basis; proceeds sum exact")
    }

    // @spec LEDGER-VERIF-007
    #[kani::proof]
    #[kani::unwind(4)]
    fn kani_verif_007_split_basis_neutrality() {
        unimplemented!("a Split changes no lot's remaining_basis; total basis Δ = 0")
    }

    // @spec LEDGER-VERIF-008
    #[kani::proof]
    #[kani::unwind(4)]
    fn kani_verif_008_reversal_totality() {
        unimplemented!("re-fold with target filtered preserves preconditions; total")
    }
}
