//! Exit Codes & top-level orchestration: SUMMARY-EXIT-001, SUMMARY-EXIT-002,
//! SUMMARY-EXIT-003.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use common::*;
use pt_core::Date;
use reports::{read_history, HistoryError, TradingDayKey};
use summary::testkit::{FakeLockProbe, InMemoryHistory, NoopLock};
use summary::{run_summary, Delta, ExitCode, FatalError, SummaryRun, TrustState};

// @spec SUMMARY-DELTA-001
#[test]
fn run_summary_with_a_prior_stored_point_reads_before_append_and_reconciles() {
    // The full run path with a strictly-prior stored point: read_history runs BEFORE
    // the append (so the baseline is the most recent strictly-prior point, captured
    // BEFORE today's append lands), the produced delta is the reconciling
    // point-to-point form, and exactly TWO points are stored afterward.
    // (SUMMARY-DELTA-001, SUMMARY-CAP-001)
    let mut client = InMemoryHistory::new();
    let prior = point_at(19_485, 190_00);
    client.set_rows(vec![row(&prior)]);
    let lock = NoopLock::new();
    let probe = FakeLockProbe::free();
    let inputs = inputs_at(19_490, 200_00);

    let run = run_summary(&mut client, &lock, &probe, TrustState::Ok, &inputs);

    match run {
        SummaryRun::Produced(report) => {
            assert!(!report.stale, "a fresh captured run is not stale");
            match report.delta {
                Delta::PointToPoint { baseline_key, current_key, total_delta_cents, .. } => {
                    // Baseline = the prior stored point (read BEFORE the append), not
                    // today's just-appended point. (SUMMARY-DELTA-001)
                    assert_eq!(baseline_key, TradingDayKey(Date(19_485)));
                    assert_eq!(current_key, TradingDayKey(Date(19_490)));
                    assert_eq!(report.baseline_day, Some(TradingDayKey(Date(19_485))));
                    // header value − delta = baseline value. (SUMMARY-DELTA-001)
                    let header = report.header.total_value_cents.0;
                    assert_eq!(
                        header - total_delta_cents.0,
                        prior.total_market_value_cents.0,
                        "header − delta = baseline"
                    );
                }
                other => panic!("expected PointToPoint, got {other:?}"),
            }
        }
        other => panic!("expected Produced, got {other:?}"),
    }

    // The append landed: the prior point plus today's = exactly two stored points.
    let stored = read_history(&client).expect("integrity-clean read");
    assert_eq!(stored.len(), 2, "prior + today's appended point");
}

// @spec SUMMARY-EXIT-001
#[test]
fn a_fresh_summary_is_produced_and_exits_0() {
    let mut client = InMemoryHistory::new();
    let lock = NoopLock::new();
    let probe = FakeLockProbe::free();
    let inputs = inputs_at(19_490, 200_00);

    let run = run_summary(&mut client, &lock, &probe, TrustState::Ok, &inputs);

    assert_eq!(run.exit_code(), ExitCode::Produced);
    assert_eq!(run.exit_code().code(), 0);
    match run {
        SummaryRun::Produced(report) => {
            assert!(!report.stale, "a fresh run is not stale");
            // The capture was durably appended this run.
            let stored = read_history(&client).expect("integrity-clean read");
            assert_eq!(stored.len(), 1);
        }
        other => panic!("expected Produced, got {other:?}"),
    }
}

// @spec SUMMARY-EXIT-001
#[test]
fn a_stale_lock_held_summary_is_still_produced_and_exits_0() {
    // An open TUI holds the lock → read-only → stale, but a usable summary, exit 0.
    let mut client = InMemoryHistory::new();
    // Seed a prior stored point so the uncaptured delta has a baseline.
    client.set_rows(vec![row(&point_at(19_485, 190_00))]);
    let lock = NoopLock::new();
    let probe = FakeLockProbe::held();
    let inputs = inputs_at(19_490, 200_00);

    let run = run_summary(&mut client, &lock, &probe, TrustState::Ok, &inputs);

    assert_eq!(run.exit_code(), ExitCode::Produced, "offline/lock-held still exits 0");
    match run {
        SummaryRun::Produced(report) => {
            assert!(report.stale, "lock-held run is stale-marked in-band");
            assert!(matches!(report.delta, Delta::Uncaptured { .. }));
        }
        other => panic!("expected Produced, got {other:?}"),
    }
}

// @spec SUMMARY-EXIT-002
#[test]
fn corrupt_history_integrity_flag_exits_2() {
    // A non-monotonic (duplicated/out-of-order) key on read is a History integrity
    // flag: no trustworthy summary → exit 2. (SUMMARY-EXIT-002)
    let mut client = InMemoryHistory::new();
    let p = point_at(19_485, 190_00);
    // Two rows with the SAME key → NonMonotonicKeys on read.
    client.set_rows(vec![row(&p), row(&p)]);
    let lock = NoopLock::new();
    let probe = FakeLockProbe::free();
    let inputs = inputs_at(19_490, 200_00);

    let run = run_summary(&mut client, &lock, &probe, TrustState::Ok, &inputs);

    assert_eq!(run.exit_code(), ExitCode::NoTrustworthySummary);
    assert_eq!(run.exit_code().code(), 2);
    assert!(matches!(
        run,
        SummaryRun::Fatal(FatalError::HistoryIntegrity(HistoryError::NonMonotonicKeys))
    ));
}

// @spec SUMMARY-EXIT-002
#[test]
fn bad_credentials_exits_2() {
    let mut client = InMemoryHistory::new();
    let lock = NoopLock::new();
    let probe = FakeLockProbe::free();
    let inputs = inputs_at(19_490, 200_00);

    let run = run_summary(&mut client, &lock, &probe, TrustState::BadCredentials, &inputs);

    assert_eq!(run.exit_code(), ExitCode::NoTrustworthySummary);
    assert!(matches!(run, SummaryRun::Fatal(FatalError::BadCredentials)));
}

// @spec SUMMARY-EXIT-002
#[test]
fn an_unreadable_cache_exits_2() {
    let mut client = InMemoryHistory::new();
    let lock = NoopLock::new();
    let probe = FakeLockProbe::free();
    let inputs = inputs_at(19_490, 200_00);

    let run = run_summary(&mut client, &lock, &probe, TrustState::UnreadableCache, &inputs);

    assert_eq!(run.exit_code(), ExitCode::NoTrustworthySummary);
    assert!(matches!(run, SummaryRun::Fatal(FatalError::UnreadableCache)));
}

// @spec SUMMARY-EXIT-003
#[test]
fn a_capture_append_that_fails_mid_run_is_non_fatal_and_exits_0() {
    // The batchUpdate fails (offline): non-fatal — print the uncaptured-delta
    // summary, note the failure, exit 0 (retried next run). (SUMMARY-EXIT-003)
    let mut client = InMemoryHistory::new();
    client.set_rows(vec![row(&point_at(19_485, 190_00))]);
    client.set_write_fails(true);
    let lock = NoopLock::new();
    let probe = FakeLockProbe::free();
    let inputs = inputs_at(19_490, 200_00);

    let run = run_summary(&mut client, &lock, &probe, TrustState::Ok, &inputs);

    assert_eq!(run.exit_code(), ExitCode::Produced, "a failed append is non-fatal (exit 0)");
    match run {
        SummaryRun::Produced(report) => {
            assert!(report.stale, "an uncaptured run is stale-marked");
            assert!(matches!(report.delta, Delta::Uncaptured { .. }));
        }
        other => panic!("expected Produced, got {other:?}"),
    }
    // The failed append left no duplicate (last-wins → retried next run).
    let stored = read_history(&client).expect("integrity-clean read");
    assert_eq!(stored.len(), 1, "the failed append wrote nothing new");
}

// @spec SUMMARY-EXIT-002
#[test]
fn an_unreachable_history_on_read_exits_2() {
    // The History tab is unreachable on the integrity-checked read: no trustworthy
    // summary (the stored series cannot be read) → exit 2. (SUMMARY-EXIT-002)
    let mut client = InMemoryHistory::new();
    client.set_unreachable(true);
    let lock = NoopLock::new();
    let probe = FakeLockProbe::free();
    let inputs = inputs_at(19_490, 200_00);

    let run = run_summary(&mut client, &lock, &probe, TrustState::Ok, &inputs);

    assert_eq!(run.exit_code(), ExitCode::NoTrustworthySummary);
    assert!(matches!(
        run,
        SummaryRun::Fatal(FatalError::HistoryIntegrity(HistoryError::Unreachable))
    ));
}

// @spec SUMMARY-EXIT-001
#[test]
fn no_priced_symbol_produces_a_stale_uncaptured_report_with_no_trading_day_key() {
    // A degraded-only cycle (no priced symbol → no trading-day key) still produces a
    // usable summary, exit 0: it is stale and uses the uncaptured-delta path against
    // the latest stored point, and the headline trading day is None (never keyed by
    // the calendar run date). (SUMMARY-EXIT-001, SUMMARY-DELTA-005, SUMMARY-OUT-002)
    let mut client = InMemoryHistory::new();
    client.set_rows(vec![row(&point_at(19_485, 190_00))]);
    let lock = NoopLock::new();
    let probe = FakeLockProbe::free();
    let mut inputs = inputs_at(19_490, 200_00);
    inputs.trading_day_key = None; // nothing priced → no key

    let run = run_summary(&mut client, &lock, &probe, TrustState::Ok, &inputs);

    assert_eq!(run.exit_code(), ExitCode::Produced, "still produces a summary, exit 0");
    match run {
        SummaryRun::Produced(report) => {
            assert!(report.stale, "a no-priced (uncaptured) run is stale-marked");
            assert!(
                matches!(report.delta, Delta::Uncaptured { .. }),
                "no priced symbol → uncaptured delta, never point-to-point"
            );
            // The headline trading day is None — NOT the calendar run date keyed as a
            // trading day (honoring the never-calendar-day-as-key discipline).
            assert_eq!(report.trading_day, None, "no trading-day key to headline");
        }
        other => panic!("expected Produced, got {other:?}"),
    }
    // Nothing was appended (no point to capture): the prior stored point stands alone.
    let stored = read_history(&client).expect("integrity-clean read");
    assert_eq!(stored.len(), 1, "a degraded-only cycle appends nothing");
}
