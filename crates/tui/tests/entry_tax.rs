//! Tax-accrual action tests (TUI-ENTRY-TAX-*).

mod common;

use config::{Jurisdiction, TaxYear};
use pt_core::{Cents, Date};
use tax::{AccrualKey, Quarter, TaxEventKind};
use tui::entry::{
    self, revalidate_batch, submit_tax, BatchDelta, BatchSnapshot, PayAdvisory, Phase,
};
use tui::testkit::{flat_federal_ctx, FakeRuntime, SubmitBehavior, ViewBuilder};

fn key(sale: &str, lot: &str) -> AccrualKey {
    AccrualKey {
        sale_id: sale.to_string(),
        lot_id: lot.to_string(),
        jurisdiction: Jurisdiction::Federal,
        tax_year: TaxYear(common::YEAR),
    }
}

/// 2026-04-15 in days-since-epoch — a `sale_date` whose `tax_year` is 2026, so its
/// derived `AccrualKey.tax_year` matches `key(..)`'s `TaxYear(2026)`.
const SALE_2026: Date = Date(20_558);

/// A snapshot carrying one realized gain for `(sale, lot)` in tax-year 2026, so its
/// federal accrual is a real key the Allocate → Move → Pay lifecycle can advance.
fn snapshot_with_realized_gain(sale: &str, lot: &str, gain: i64) -> ledger_core::Snapshot {
    let mut snap = ledger_core::Snapshot::default();
    snap.realized_gains.push(ledger_core::RealizedGain {
        sale_id: sale.to_string(),
        sale_seq: pt_core::Seq(1),
        lot_id: lot.to_string(),
        symbol: "AMZN".to_string(),
        sale_date: SALE_2026,
        proceeds_cents: Cents(gain),
        basis_cents: Cents(0),
        gain_cents: Cents(gain),
        acquire_date: Date(15_000), // long-term
        holding_days: 5_558,
        accrues_to_state: None,
    });
    snap
}

/// A fake runtime over a realized-gain snapshot, federal-flat brackets — the live
/// write path for the tax-accrual lifecycle tests.
fn tax_runtime(sale: &str, lot: &str, gain: i64) -> FakeRuntime {
    let view = ViewBuilder::new(snapshot_with_realized_gain(sale, lot, gain)).build();
    FakeRuntime::new(view, flat_federal_ctx(common::YEAR, 220_000))
}

fn accrual(sale: &str, lot: &str, applied: i64, state: tax::AccrualState) -> tax::Accrual {
    tax::Accrual {
        key: key(sale, lot),
        term: tax::Term::LongTerm,
        gain_cents: Cents(10_000),
        derived_cents: Some(Cents(applied)),
        applied_cents: Some(Cents(applied)),
        override_cents: None,
        state,
        de_minimis: false,
        superseded: false,
        bracket_state: config::BracketState::Verified,
    }
}

// @spec TUI-ENTRY-TAX-001
#[test]
fn allocate_assigns_an_accrual_to_a_reserve_account() {
    let ev = entry::compose_allocate(key("S1", "L1"), "Reserve-Fed".to_string());
    match ev.kind {
        TaxEventKind::Allocate { accrual_key, account_label } => {
            assert_eq!(accrual_key, key("S1", "L1"));
            assert_eq!(account_label, "Reserve-Fed");
        }
        other => panic!("expected Allocate, got {other:?}"),
    }
}

// @spec TUI-ENTRY-TAX-002
#[test]
fn move_records_the_actual_amount_and_date_which_may_differ_from_the_estimate() {
    // The actual moved ($2,400) differs from the computed estimate; Move records the
    // actual + date (the shortfall surfaces elsewhere, not a rejection). (TUI-ENTRY-TAX-002)
    let ev = entry::compose_move(key("S1", "L1"), Cents(240_000), Date(20_100));
    match ev.kind {
        TaxEventKind::Move { accrual_key, amount_cents, date } => {
            assert_eq!(accrual_key, key("S1", "L1"));
            assert_eq!(amount_cents, Cents(240_000));
            assert_eq!(date, Date(20_100));
        }
        other => panic!("expected Move, got {other:?}"),
    }
}

// @spec TUI-ENTRY-TAX-003
#[test]
fn pay_covers_moved_accruals_with_amount_independent_and_advisory_balance() {
    let covers = vec![key("S1", "L1"), key("S1", "L2")];
    let ev = entry::compose_pay(
        Jurisdiction::Federal,
        TaxYear(common::YEAR),
        Quarter::Q2,
        Cents(1_210_000),
        Date(20_150),
        covers.clone(),
    );
    match ev.kind {
        TaxEventKind::Pay { jurisdiction, tax_year, period, amount_cents, covers: c, .. } => {
            assert_eq!(jurisdiction, Jurisdiction::Federal);
            assert_eq!(tax_year, TaxYear(common::YEAR));
            assert_eq!(period, Quarter::Q2);
            assert_eq!(amount_cents, Cents(1_210_000));
            assert_eq!(c, covers);
        }
        other => panic!("expected Pay, got {other:?}"),
    }

    // The covered $X / paid $Y ✓ line is ADVISORY; a partial/over-payment is allowed
    // (surfacing the delta), never a submit gate. (TUI-ENTRY-TAX-003)
    let balanced = PayAdvisory { covered_cents: Cents(1_210_000), paid_cents: Cents(1_210_000) };
    assert!(balanced.balanced());
    assert!(balanced.text().contains('\u{2713}'), "✓ on balance");

    let partial = PayAdvisory { covered_cents: Cents(1_210_000), paid_cents: Cents(1_000_000) };
    assert!(!partial.balanced(), "a partial payment is allowed");
    assert_eq!(partial.delta(), Cents(-210_000), "the delta surfaces");
    assert!(partial.text().contains('\u{0394}'), "the delta is shown, not gated");
}

// @spec TUI-ENTRY-TAX-004
#[test]
fn override_replaces_the_computed_amount_with_an_absolute_amount_and_reason() {
    let ev = entry::compose_override(key("S1", "L1"), Cents(5_000), "negotiated settlement".to_string());
    match ev.kind {
        TaxEventKind::AmountOverride { accrual_key, applied_amount_cents, reason } => {
            assert_eq!(accrual_key, key("S1", "L1"));
            assert_eq!(applied_amount_cents, Cents(5_000));
            assert_eq!(reason, "negotiated settlement");
        }
        other => panic!("expected AmountOverride, got {other:?}"),
    }
}

// @spec TUI-ENTRY-TAX-005
#[test]
fn batch_snapshots_at_confirm_and_revalidates_at_submit_flagging_vanished_or_repriced() {
    use tax::AccrualState;
    // At confirm, two accruals selected at their applied amounts.
    let snapshot = BatchSnapshot {
        selected: vec![
            (key("S1", "L1"), Some(Cents(240_000))),
            (key("S1", "L2"), Some(Cents(96_000))),
        ],
    };
    // At submit, L1 repriced (a config edit) and L2 vanished (its sale was reversed).
    let current = vec![accrual("S1", "L1", 210_000, AccrualState::Allocated { account_label: "R".to_string() })];
    let deltas = revalidate_batch(&snapshot, &current);
    assert_eq!(deltas.len(), 2);
    assert!(deltas.iter().any(|d| matches!(d, BatchDelta::Repriced { key: k, .. } if k == &key("S1", "L1"))));
    assert!(deltas.iter().any(|d| matches!(d, BatchDelta::Vanished(k) if k == &key("S1", "L2"))));

    // A stable batch (nothing changed) flags nothing — safe to submit.
    let stable_now = vec![
        accrual("S1", "L1", 240_000, AccrualState::Allocated { account_label: "R".to_string() }),
        accrual("S1", "L2", 96_000, AccrualState::Allocated { account_label: "R".to_string() }),
    ];
    assert!(revalidate_batch(&snapshot, &stable_now).is_empty());
}

// @spec TUI-ENTRY-TAX-001
// @spec TUI-ENTRY-TAX-002
// @spec TUI-ENTRY-TAX-003
#[test]
fn tax_accrual_lifecycle_allocate_then_move_then_pay_confirms_durable_through_the_write_loop() {
    // Drive the full tax write-path loop: compose → validate → submit → confirmed,
    // for Allocate → Move → Pay, each appending a TaxEvent that the next step's live
    // re-validation sees (the lifecycle states are in scope). A Move on an Allocated
    // accrual validates; a Pay on the Moved accrual validates and confirms durable.
    // (TUI-ENTRY-TAX-001/002/003, TUI-ENTRY-FLOW-003)
    let mut rt = tax_runtime("S1", "L1", 10_000_00);
    let k = key("S1", "L1");

    // A Move BEFORE any Allocate is rejected live (the accrual is still Accrued) —
    // proving the harness models the real lifecycle, not an always-Accrued stub.
    let premature_move = entry::compose_move(k.clone(), Cents(240_000), SALE_2026);
    assert!(
        matches!(submit_tax(&mut rt, &premature_move), Phase::Rejected(_)),
        "a Move before Allocate is rejected live (MoveOnUnallocated)"
    );
    assert!(rt.tax_log.is_empty(), "a rejected submit writes nothing");

    // Allocate → Confirmed durable.
    let allocate = entry::compose_allocate(k.clone(), "Reserve-Fed".to_string());
    assert_eq!(submit_tax(&mut rt, &allocate), Phase::Confirmed, "Allocate confirms durable");
    assert_eq!(rt.tax_log.len(), 1, "the Allocate landed in the accepted log");

    // Move (actual amount, may differ from estimate) on the now-Allocated accrual.
    let mv = entry::compose_move(k.clone(), Cents(240_000), SALE_2026);
    assert_eq!(submit_tax(&mut rt, &mv), Phase::Confirmed, "Move on Allocated confirms durable");
    assert_eq!(rt.tax_log.len(), 2);

    // Pay covering the now-Moved accrual (jurisdiction/year match) → Confirmed.
    let pay = entry::compose_pay(
        Jurisdiction::Federal,
        TaxYear(common::YEAR),
        Quarter::Q2,
        Cents(240_000),
        SALE_2026,
        vec![k.clone()],
    );
    assert_eq!(submit_tax(&mut rt, &pay), Phase::Confirmed, "Pay on a Moved accrual confirms durable");
    assert_eq!(rt.tax_log.len(), 3);
    assert!(rt.last_event_id.is_some(), "read-back-verify confirmed a stored id");

    // A second Pay covering the now-Paid accrual is rejected live (DoublePay).
    let double = entry::compose_pay(
        Jurisdiction::Federal,
        TaxYear(common::YEAR),
        Quarter::Q2,
        Cents(240_000),
        SALE_2026,
        vec![k.clone()],
    );
    assert!(matches!(submit_tax(&mut rt, &double), Phase::Rejected(_)), "a double Pay is rejected live");
}

// @spec TUI-ENTRY-TAX-003
#[test]
fn pay_covering_a_mismatched_jurisdiction_or_year_is_rejected_live() {
    // A Pay whose covered accrual key's jurisdiction/year differs from the Pay's is
    // rejected by the kernel (PayCoverMismatch). Allocate + Move a Federal-2026
    // accrual, then attempt to cover it with a 2025 Pay. (TUI-ENTRY-TAX-003)
    let mut rt = tax_runtime("S1", "L1", 10_000_00);
    let k = key("S1", "L1");
    assert_eq!(submit_tax(&mut rt, &entry::compose_allocate(k.clone(), "R".to_string())), Phase::Confirmed);
    assert_eq!(submit_tax(&mut rt, &entry::compose_move(k.clone(), Cents(240_000), SALE_2026)), Phase::Confirmed);

    // The Pay declares year 2025 but covers the 2026 accrual → mismatch.
    let mismatched = entry::compose_pay(
        Jurisdiction::Federal,
        TaxYear(common::YEAR - 1),
        Quarter::Q2,
        Cents(240_000),
        SALE_2026,
        vec![k.clone()],
    );
    match submit_tax(&mut rt, &mismatched) {
        Phase::Rejected(entry::InlineError::Tax(e)) => {
            assert_eq!(e, tax::TaxError::PayCoverMismatch, "the jurisdiction/year mismatch is the live rejection");
        }
        other => panic!("expected a PayCoverMismatch inline rejection, got {other:?}"),
    }
}

// @spec TUI-ENTRY-TAX-005
#[test]
fn batch_submit_revalidates_live_at_submit_and_a_vanished_selection_prevents_the_commit() {
    // The batch-submit driver re-validates the confirm-time snapshot against the
    // LIVE accruals immediately before submit. A clean snapshot submits the events;
    // a vanished (reversed) or repriced (config-edited) selection returns to the form
    // with the delta flagged — nothing committed. (TUI-ENTRY-TAX-005)
    use tax::AccrualState;
    use tui::entry::{submit_tax_batch, BatchSubmit};

    let k1 = key("S1", "L1");
    let k2 = key("S1", "L2");

    // Confirm-time snapshot: two Allocated accruals at their applied amounts.
    let snapshot = BatchSnapshot {
        selected: vec![(k1.clone(), Some(Cents(240_000))), (k2.clone(), Some(Cents(96_000)))],
    };
    let allocate1 = entry::compose_allocate(k1.clone(), "R".to_string());
    let allocate2 = entry::compose_allocate(k2.clone(), "R".to_string());

    // STALE: at submit, L2 vanished (reversed) and L1 repriced. The live view carries
    // only a repriced L1. The batch returns Stale, committing nothing.
    let live_changed = ViewBuilder::new(ledger_core::Snapshot::default())
        .accruals(vec![accrual("S1", "L1", 210_000, AccrualState::Allocated { account_label: "R".to_string() })])
        .build();
    let mut stale_rt = FakeRuntime::new(live_changed, flat_federal_ctx(common::YEAR, 220_000));
    match submit_tax_batch(&mut stale_rt, &snapshot, &[allocate1.clone(), allocate2.clone()]) {
        BatchSubmit::Stale(deltas) => {
            assert!(deltas.iter().any(|d| matches!(d, BatchDelta::Repriced { key: k, .. } if k == &k1)));
            assert!(deltas.iter().any(|d| matches!(d, BatchDelta::Vanished(k) if k == &k2)));
        }
        other => panic!("a changed selection must not commit, got {other:?}"),
    }
    assert_eq!(stale_rt.tax_log.len(), 0, "a stale batch commits nothing");

    // CLEAN: the live accruals match the snapshot; the batch submits each event.
    let live_stable = ViewBuilder::new(ledger_core::Snapshot::default())
        .accruals(vec![
            accrual("S1", "L1", 240_000, AccrualState::Allocated { account_label: "R".to_string() }),
            accrual("S1", "L2", 96_000, AccrualState::Allocated { account_label: "R".to_string() }),
        ])
        .build();
    let mut clean_rt = FakeRuntime::new(live_stable, flat_federal_ctx(common::YEAR, 220_000));
    match submit_tax_batch(&mut clean_rt, &snapshot, &[allocate1, allocate2]) {
        BatchSubmit::Submitted(phases) => {
            assert_eq!(phases.len(), 2);
            assert!(phases.iter().all(|p| *p == Phase::Confirmed), "a clean batch submits each event durably");
        }
        other => panic!("a clean batch must submit, got {other:?}"),
    }
}

// @spec TUI-ENTRY-TAX-001
// @spec TUI-ENTRY-FLOW-005
#[test]
fn tax_submit_lock_held_and_write_failed_branches_preserve_the_entry() {
    // A tax-accrual submit hits the LockHeld + WriteFailed branches non-destructively
    // (entry preserved, retry) — the same write loop the ledger flows ride.
    let k = key("S1", "L1");
    let allocate = entry::compose_allocate(k.clone(), "R".to_string());

    let mut locked = tax_runtime("S1", "L1", 10_000_00)
        .with_behavior(SubmitBehavior::LockHeld { holder: "summary-cron".to_string() });
    match submit_tax(&mut locked, &allocate) {
        Phase::Retry(entry::RetryReason::LockHeld { holder }) => assert_eq!(holder, "summary-cron"),
        other => panic!("expected a lock-held retry, got {other:?}"),
    }
    assert!(locked.tax_log.is_empty(), "lock-held writes nothing");

    let mut failed = tax_runtime("S1", "L1", 10_000_00).with_behavior(SubmitBehavior::VerifyMismatch);
    assert_eq!(
        submit_tax(&mut failed, &allocate),
        Phase::Retry(entry::RetryReason::VerifyMismatch),
        "a write-verify mismatch returns control with [r]etry"
    );
    assert!(failed.tax_log.is_empty(), "a write failure leaves no partial write");
}
