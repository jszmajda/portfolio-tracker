//! The History-append retry-and-flag loop (RUNTIME-REPORTS-001) and the
//! post-priced-cycle History capture trigger (RUNTIME-CYCLE-005).
//!
//! `reports::append_snapshot` only *flags* a single `Err(WriteVerifyFailed)`
//! (`REPORT-HIST-001`); `runtime` owns the **loop** — bounded retries with backoff,
//! each attempt acquiring the advisory write-lock inside `append_snapshot`
//! (`RUNTIME-LOCK-002`) — and, on continued failure, surfaces the trading-day point
//! as **uncaptured / flagged** rather than dropping it silently (a lost point is
//! non-reconstructable: its marks are gone).
//!
//! After a cycle that produced priced marks, `runtime` **triggers** the capture
//! (RUNTIME-CYCLE-005): it builds the series point from the cycle's reduced
//! trading-day key (`RUNTIME-CYCLE-003`) and the priced marks projected via
//! `to_priced_marks`, then runs that retry loop. History last-wins per trading-day
//! key (`REPORT-HIST-002`), so a TUI-triggered and a cron-triggered capture for the
//! same day reconcile to one point rather than duplicate. A degraded-only cycle (no
//! priced marks, no trading-day key) triggers nothing — there is no day to capture.

use std::time::Duration;

use pt_core::Date;
use reports::{
    append_snapshot, build_series_point, HistoryClient, HistoryError, SeriesPoint, TradingDayKey,
};
use store::Lock;

use crate::cycle::CycleOutcome;
use crate::sheets::BackoffPolicy;

/// The outcome of the bounded History-append loop (RUNTIME-REPORTS-001): the point
/// was durably captured, or — after the retry budget was exhausted — it is flagged
/// **uncaptured** so the caller surfaces the gap (a non-reconstructable point is
/// never silently dropped).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CaptureOutcome {
    /// The point landed and read back equal within the attempt budget.
    Captured,
    /// Every attempt failed to durably confirm the write; the trading-day point is
    /// surfaced as uncaptured/flagged (not dropped). Carries the key so the caller
    /// can report exactly which day is missing. (RUNTIME-REPORTS-001)
    Uncaptured { key: TradingDayKey, attempts: u32 },
}

impl CaptureOutcome {
    /// Whether the point was durably captured.
    pub fn is_captured(&self) -> bool {
        matches!(self, CaptureOutcome::Captured)
    }

    /// Whether the point is flagged uncaptured (the retry budget was exhausted).
    pub fn is_uncaptured(&self) -> bool {
        matches!(self, CaptureOutcome::Uncaptured { .. })
    }
}

/// The bounded retry-and-flag loop around `reports::append_snapshot`
/// (RUNTIME-REPORTS-001): retry the append — each attempt acquiring the advisory
/// write-lock inside `append_snapshot` (`RUNTIME-LOCK-002`) — up to the policy's
/// attempt cap with exponential backoff, and on continued failure surface the
/// trading-day point as uncaptured/flagged rather than discard it silently.
///
/// `append_snapshot` last-wins per trading-day key (`REPORT-HIST-002`), so a retry
/// after a partial failure overwrites rather than duplicates. A read-side error
/// other than `WriteVerifyFailed` (the workbook went unreachable mid-read-back) is
/// retryable too — it is a transport failure, not a corrupt-series flag. Only
/// `WriteVerifyFailed` and `Unreachable` are retried; an integrity flag
/// (`NonMonotonicKeys` / `UnparseableRow` / `ChecksumMismatch`) means the durable
/// series is already corrupt and a retry cannot heal it, so it surfaces uncaptured
/// immediately.
///
/// `sleep_fn` is the backoff sleep (the real path passes `thread::sleep`; tests pass
/// a no-op that records the schedule), so the loop is exercised without waiting.
pub fn capture_with_retry<C, L>(
    client: &mut C,
    lock: &L,
    point: &SeriesPoint,
    policy: &BackoffPolicy,
    mut sleep_fn: impl FnMut(Duration),
) -> CaptureOutcome
where
    C: HistoryClient,
    L: Lock,
{
    let mut attempts = 0u32;
    loop {
        attempts += 1;
        // Each attempt drives the full append_snapshot (which acquires the advisory
        // write-lock INSIDE itself — RUNTIME-LOCK-002 — and read-back-verifies).
        match append_snapshot(client, lock, point) {
            Ok(()) => return CaptureOutcome::Captured,
            // A write that could not be durably confirmed, or a transient transport
            // failure on the read-back, is retryable up to the budget.
            // (RUNTIME-REPORTS-001)
            Err(HistoryError::WriteVerifyFailed) | Err(HistoryError::Unreachable) => {
                if !policy.may_retry(attempts) {
                    // The budget is exhausted: surface the point as uncaptured/
                    // flagged rather than dropping a non-reconstructable point.
                    return CaptureOutcome::Uncaptured {
                        key: point.key,
                        attempts,
                    };
                }
                sleep_fn(policy.delay_for(attempts + 1));
            }
            // An integrity flag means the durable series is already corrupt; a retry
            // cannot heal it (recovery is via Sheets version history). Surface
            // uncaptured immediately rather than spin. (RUNTIME-REPORTS-001)
            Err(_) => {
                return CaptureOutcome::Uncaptured {
                    key: point.key,
                    attempts,
                }
            }
        }
    }
}

/// Trigger the History capture after a cycle that produced priced marks
/// (RUNTIME-CYCLE-005): build the series point from the cycle's reduced
/// trading-day key (`RUNTIME-CYCLE-003`) and the priced marks projected via
/// `to_priced_marks`, then run the bounded retry-and-flag loop
/// (`RUNTIME-REPORTS-001`) — each attempt acquiring the advisory write-lock inside
/// `append_snapshot` (`RUNTIME-LOCK-002`).
///
/// Returns `None` when the cycle produced **no priced marks** (every symbol
/// degraded, so `trading_day_key` is `None`): there is no trading day to capture, so
/// `runtime` triggers nothing rather than fabricating a calendar-day point
/// (`RUNTIME-CYCLE-003`/`004`). Otherwise returns the [`CaptureOutcome`] — captured,
/// or flagged uncaptured after the budget — so a TUI-triggered and a cron-triggered
/// capture for the same day reconcile to one point via History last-wins
/// (`REPORT-HIST-002`).
#[allow(clippy::too_many_arguments)]
pub fn capture_cycle<C, L>(
    client: &mut C,
    lock: &L,
    outcome: &CycleOutcome,
    estimates: &std::collections::BTreeMap<ledger_core::Symbol, tax::UnrealizedEstimate>,
    reporting_tz_date: Date,
    captured_at_epoch_secs: i64,
    policy: &BackoffPolicy,
    sleep_fn: impl FnMut(Duration),
) -> Option<CaptureOutcome>
where
    C: HistoryClient,
    L: Lock,
{
    // A cycle with no priced marks keyed nothing (RUNTIME-CYCLE-003): no trading day
    // to capture, so trigger nothing. (RUNTIME-CYCLE-005)
    let key = outcome.trading_day_key?;

    // Project the cached marks to reports::PricedMarks (price + per-symbol
    // quote-epoch) and build the series point keyed by the runtime-reduced
    // trading-day key. (RUNTIME-CYCLE-005)
    let priced = outcome.marks.to_priced_marks();
    let point = build_series_point(
        &outcome.snapshot,
        &priced,
        estimates,
        key,
        captured_at_epoch_secs,
        reporting_tz_date,
    );

    Some(capture_with_retry(client, lock, &point, policy, sleep_fn))
}
