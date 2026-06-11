//! Post-rebuild re-validation handshake: RUNTIME-REVALIDATE-001. When a store-side
//! append triggers a cache rebuild from the workbook, runtime re-runs the kernel
//! validation the original event passed — against the REFRESHED post-rebuild log,
//! before Seq assignment — so an event that conflicts with rows appended
//! out-of-band since it was composed is rejected rather than appended on stale
//! assumptions; on success the refreshed view is handed back to the writer.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use common::{buy, ctx, sell};

use ledger_core::LedgerError;
use runtime::{revalidate_ledger, revalidate_tax, RevalidateError};
use store::EventLogs;

const AS_OF: i32 = 19_200;

// @spec RUNTIME-REVALIDATE-001
#[test]
fn a_pending_ledger_event_valid_against_the_refreshed_log_is_accepted_and_hands_back_the_view() {
    // The event the writer composed still validates against the refreshed log: the
    // handshake re-runs the kernel gate and hands the SAME refreshed view back, so
    // the writer assigns Seq and appends against exactly this view (no second read
    // window). (RUNTIME-REVALIDATE-001)
    let refreshed = EventLogs {
        ledger: vec![buy(1, 19_000, "lot-amzn", "AMZN", 3_000_000, 150_00)],
        tax: vec![],
    };
    // A new Buy of a fresh lot is independent of the existing rows — still valid.
    let candidate = buy(2, 19_050, "lot-msft", "MSFT", 1_000_000, 300_00);

    let view =
        revalidate_ledger(&refreshed, &candidate).expect("still valid against the refreshed log");
    // The hand-back is the refreshed view itself (same rows), so the writer proceeds
    // against exactly what was re-validated.
    assert_eq!(
        view.ledger.len(),
        1,
        "the refreshed view is handed back to the writer"
    );
    assert_eq!(view.ledger[0].id, "e1");
}

// @spec RUNTIME-REVALIDATE-001
#[test]
fn a_pending_sell_conflicting_with_an_out_of_band_sell_is_rejected_against_the_refreshed_log() {
    // The pending Sell was composed when the lot still had shares. Since then an
    // out-of-band Sell consumed the whole lot (now in the refreshed log). Re-running
    // the kernel validation against the REFRESHED log rejects the pending Sell
    // (InsufficientShares) rather than appending it on the stale assumption that the
    // lot still had shares. (RUNTIME-REVALIDATE-001)
    let refreshed = EventLogs {
        ledger: vec![
            buy(1, 19_000, "lot-goog", "GOOG", 2_000_000, 100_00),
            // The out-of-band Sell that drained the lot since the pending event was
            // composed (sold all 2 shares).
            sell(2, 19_100, "sale-oob", "GOOG", 2_000_000, 130_00, None),
        ],
        tax: vec![],
    };
    // The pending Sell — valid against the PRE-rebuild log (when the lot was full),
    // now conflicts with the refreshed log (the lot is empty).
    let pending = sell(3, 19_150, "sale-pending", "GOOG", 1_000_000, 140_00, None);

    match revalidate_ledger(&refreshed, &pending) {
        Err(RevalidateError::Ledger(LedgerError::InsufficientShares)) => {}
        other => panic!(
            "a pending Sell conflicting with the refreshed log must be rejected (InsufficientShares), got {other:?}"
        ),
    }
}

// @spec RUNTIME-REVALIDATE-001
#[test]
fn a_pending_buy_reusing_an_out_of_band_lot_id_is_rejected_against_the_refreshed_log() {
    // The pending Buy uses a lot id that, since it was composed, was claimed by an
    // out-of-band Buy now in the refreshed log. Re-validation rejects the duplicate
    // lot id rather than appending it. (RUNTIME-REVALIDATE-001)
    let refreshed = EventLogs {
        ledger: vec![buy(1, 19_000, "lot-dup", "AMZN", 1_000_000, 150_00)],
        tax: vec![],
    };
    let pending = buy(2, 19_050, "lot-dup", "AMZN", 1_000_000, 160_00); // same lot id.

    match revalidate_ledger(&refreshed, &pending) {
        Err(RevalidateError::Ledger(LedgerError::DuplicateLotId)) => {}
        other => {
            panic!("a pending Buy reusing an out-of-band lot id must be rejected, got {other:?}")
        }
    }
}

// @spec RUNTIME-REVALIDATE-001
#[test]
fn a_pending_tax_move_conflicting_with_the_refreshed_state_is_rejected() {
    // A pending tax Move targets an accrual that, in the refreshed state, is NOT
    // Allocated (no Allocate event precedes it). Re-running the tax kernel validation
    // against the refreshed realized-gains + tax log rejects the Move
    // (MoveOnUnallocated) rather than appending it on stale assumptions.
    // (RUNTIME-REVALIDATE-001)
    use config::{Jurisdiction, TaxYear};
    use pt_core::{Cents, Date, Seq};
    use tax::{AccrualKey, TaxEvent, TaxEventKind};

    let ctx = ctx(2022);
    // The refreshed ledger realizes a GOOG gain (so a backing accrual exists), but the
    // refreshed tax log has NO Allocate for it — so a Move is invalid.
    let refreshed = EventLogs {
        ledger: vec![
            buy(1, 19_000, "lot-goog", "GOOG", 2_000_000, 100_00),
            sell(2, 19_100, "sale-1", "GOOG", 1_000_000, 130_00, Some("NJ")),
        ],
        tax: vec![], // no Allocate — the accrual is not Allocated in the refreshed state.
    };
    let pending = TaxEvent {
        seq: Seq(1),
        kind: TaxEventKind::Move {
            accrual_key: AccrualKey {
                sale_id: "sale-1".to_string(),
                lot_id: "lot-goog".to_string(),
                jurisdiction: Jurisdiction::Federal,
                tax_year: TaxYear(2022),
            },
            amount_cents: Cents(50_00),
            date: Date(19_150),
        },
    };

    match revalidate_tax(&refreshed, &pending, &ctx) {
        Err(RevalidateError::Tax(tax::TaxError::MoveOnUnallocated)) => {}
        other => panic!("a pending Move on an unallocated accrual must be rejected, got {other:?}"),
    }
}

// (silence the unused AS_OF if a future test does not reference it.)
const _: i32 = AS_OF;
