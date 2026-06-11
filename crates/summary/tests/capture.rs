//! Daily Capture & Locking: SUMMARY-CAP-001, SUMMARY-CAP-002.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use common::*;
use pt_core::Date;
use reports::{read_history, TradingDayKey};
use summary::testkit::{FakeLockProbe, InMemoryHistory, NoopLock};
use summary::{build_report, capture, compute_delta, CaptureOutcome, StaleReason};

// @spec SUMMARY-CAP-001
#[test]
fn capture_triggers_append_snapshot_for_the_current_trading_day() {
    let mut client = InMemoryHistory::new();
    let lock = NoopLock::new();
    let probe = FakeLockProbe::free();
    let inputs = inputs_at(19_490, 200_00);

    let outcome = capture(&mut client, &lock, &probe, &inputs).expect("a point to capture");

    // A point was durably appended for the current trading day. (SUMMARY-CAP-001)
    assert!(matches!(outcome, CaptureOutcome::Appended(_)));
    let stored = read_history(&client).expect("integrity-clean read");
    assert_eq!(stored.len(), 1, "exactly one point appended");
    assert_eq!(stored[0].key, TradingDayKey(Date(19_490)));
    // The lock was acquired INSIDE reports::append_snapshot (a TUI + a cron capture
    // cannot race). (SUMMARY-CAP-001)
    assert!(
        lock.acquire_count() >= 1,
        "capture must go through the locked primitive"
    );
}

// @spec SUMMARY-CAP-001
#[test]
fn capture_is_last_wins_per_trading_day_no_duplicate_on_rerun() {
    let mut client = InMemoryHistory::new();
    let lock = NoopLock::new();
    let probe = FakeLockProbe::free();

    // First run of the day.
    capture(&mut client, &lock, &probe, &inputs_at(19_490, 200_00)).expect("point");
    // A second run the SAME trading day with a fresh value: last-wins, no duplicate.
    capture(&mut client, &lock, &probe, &inputs_at(19_490, 210_00)).expect("point");

    let stored = read_history(&client).expect("integrity-clean read");
    assert_eq!(
        stored.len(),
        1,
        "re-run the same trading day must not duplicate"
    );
    assert_eq!(stored[0].total_market_value_cents.0, 210_00 * 3 + 120_00); // last value wins
}

// @spec SUMMARY-CAP-002
#[test]
fn lock_held_runs_read_only_skips_the_capture() {
    let mut client = InMemoryHistory::new();
    let lock = NoopLock::new();
    // An open interactive TUI holds the advisory write-lock.
    let probe = FakeLockProbe::held();
    let inputs = inputs_at(19_490, 200_00);

    let outcome = capture(&mut client, &lock, &probe, &inputs).expect("a point built");

    // Read-only: the capture is skipped, nothing is written, the run is flagged
    // uncaptured (the uncaptured-delta path). (SUMMARY-CAP-002)
    assert!(matches!(outcome, CaptureOutcome::SkippedLockHeld(_)));
    assert!(outcome.is_uncaptured(), "lock-held run is uncaptured");
    let stored = read_history(&client).expect("integrity-clean read");
    assert!(stored.is_empty(), "no point written while the lock is held");
}

// @spec SUMMARY-CAP-003
#[test]
fn uncaptured_note_distinguishes_lock_held_from_offline_append_failed() {
    // The uncaptured-run note must distinguish its reason: a lock-held read-only run
    // (a concurrent writer) is noted DISTINCTLY from an offline / append-failed run
    // (no point could be durably appended) — never conflated in one note.
    // (SUMMARY-CAP-003)
    let inputs = inputs_at(19_490, 200_00);
    let live = summary::build_current_point(&inputs).expect("a priced point");
    let stored = vec![point_at(19_485, 190_00)];

    // Lock-held read-only path.
    let held = CaptureOutcome::SkippedLockHeld(live.clone());
    let held_delta = compute_delta(&held, &stored, &[]);
    let held_report = build_report(&inputs, &held, held_delta, stored.first());

    // Offline / append-failed path.
    let failed = CaptureOutcome::AppendFailed(live);
    let failed_delta = compute_delta(&failed, &stored, &[]);
    let failed_report = build_report(&inputs, &failed, failed_delta, stored.first());

    // Both are stale (uncaptured), but the model carries a DISTINCT reason. (SUMMARY-CAP-003)
    assert!(
        held_report.stale && failed_report.stale,
        "both uncaptured runs are stale"
    );
    assert_eq!(held_report.stale_reason, Some(StaleReason::LockHeld));
    assert_eq!(
        failed_report.stale_reason,
        Some(StaleReason::OfflineOrAppendFailed)
    );
    assert_ne!(
        held_report.stale_reason, failed_report.stale_reason,
        "the two reasons are not conflated"
    );

    // The rendered text note differs by reason and does not conflate the two — the
    // lock-held note names the lock, the offline note names the failed append, and
    // neither uses the old conflated "offline or lock held" phrasing. (SUMMARY-CAP-003)
    let held_text = summary::render_text(&held_report).to_lowercase();
    let failed_text = summary::render_text(&failed_report).to_lowercase();
    assert!(held_text.contains("lock"), "lock-held note names the lock");
    assert!(
        failed_text.contains("offline") || failed_text.contains("append"),
        "offline note names the failed append"
    );
    assert!(
        !held_text.contains("offline or lock held")
            && !failed_text.contains("offline or lock held"),
        "the conflated note is gone"
    );
    // A fresh (non-stale) run carries no reason.
    let fresh = CaptureOutcome::Appended(summary::build_current_point(&inputs).unwrap());
    let fresh_delta = compute_delta(&fresh, &stored, &[]);
    let fresh_report = build_report(&inputs, &fresh, fresh_delta, stored.first());
    assert!(!fresh_report.stale);
    assert_eq!(
        fresh_report.stale_reason, None,
        "a fresh run carries no stale reason"
    );
}

// @spec SUMMARY-CAP-001
#[test]
fn no_priced_symbol_yields_no_point_to_capture() {
    let mut client = InMemoryHistory::new();
    let lock = NoopLock::new();
    let probe = FakeLockProbe::free();
    // A degraded-only cycle keys nothing (no trading-day key → no point).
    let mut inputs = inputs_at(19_490, 200_00);
    inputs.trading_day_key = None;

    assert!(capture(&mut client, &lock, &probe, &inputs).is_none());
}
