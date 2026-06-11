//! Day-Over-Day Delta: SUMMARY-DELTA-001..005.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use common::*;
use pt_core::{Cents, Date};
use reports::TradingDayKey;
use summary::{compute_delta, CaptureOutcome, Delta, SuppressReason};

/// The total market value of the small portfolio at AMZN=`amzn_cents`/share,
/// GOOG=$120/share (3 AMZN shares + 1 GOOG share).
fn total_value(amzn_cents: i64) -> i64 {
    amzn_cents * 3 + 120_00
}

/// A contiguous trading-day calendar over `[lo, hi]` (every integer day is a trading
/// day). Used where the test wants a dense calendar (no weekend modelling).
fn dense_calendar(lo: i32, hi: i32) -> Vec<TradingDayKey> {
    (lo..=hi).map(|d| TradingDayKey(Date(d))).collect()
}

// @spec SUMMARY-DELTA-001
#[test]
fn point_to_point_baseline_is_most_recent_strictly_prior_and_reconciles() {
    // Stored: two prior points (day 19_480, 19_485). Current appended: day 19_490.
    let baseline = point_at(19_485, 190_00);
    let stored = vec![point_at(19_480, 180_00), baseline.clone()];
    let current = point_at(19_490, 200_00);
    let capture = CaptureOutcome::Appended(current.clone());

    // A dense trading-day calendar: 19_486..19_489 are real trading days between the
    // baseline (19_485) and current (19_490), so this is a genuine gap.
    let calendar = dense_calendar(19_480, 19_490);
    let delta = compute_delta(&capture, &stored, &calendar);

    match delta {
        Delta::PointToPoint {
            baseline_key,
            current_key,
            total_delta_cents,
            spans_gap,
        } => {
            // Baseline is the most recent STRICTLY-PRIOR point (day 19_485), not 19_480.
            // (SUMMARY-DELTA-001)
            assert_eq!(baseline_key, TradingDayKey(Date(19_485)));
            assert_eq!(current_key, TradingDayKey(Date(19_490)));
            // header value − delta = baseline value (reconciles). (SUMMARY-DELTA-001)
            let header = current.total_market_value_cents.0;
            let base = baseline.total_market_value_cents.0;
            assert_eq!(total_delta_cents.0, header - base);
            assert_eq!(
                header - total_delta_cents.0,
                base,
                "header − delta = baseline"
            );
            assert_eq!(
                total_delta_cents,
                Cents(total_value(200_00) - total_value(190_00))
            );
            // 19_486..19_489 are trading days with no stored point between the
            // baseline and current → a gap. (SUMMARY-DELTA-002)
            assert!(spans_gap);
        }
        other => panic!("expected PointToPoint, got {other:?}"),
    }
}

// @spec SUMMARY-DELTA-002
#[test]
fn baseline_more_than_one_trading_day_prior_flags_the_span() {
    // Baseline is several trading days before the current point → a gap; spans_gap is
    // set so the header annotates the actual span. The calendar marks 19_487..19_489
    // as real trading days between baseline (19_486) and current (19_490).
    // (SUMMARY-DELTA-002)
    let stored = vec![point_at(19_486, 190_00)];
    let current = point_at(19_490, 200_00);
    let calendar = dense_calendar(19_486, 19_490);
    let delta = compute_delta(&CaptureOutcome::Appended(current), &stored, &calendar);
    match delta {
        Delta::PointToPoint { spans_gap, .. } => assert!(spans_gap, "a multi-day span is flagged"),
        other => panic!("expected PointToPoint, got {other:?}"),
    }

    // A consecutive trading day (19_489 → 19_490) is NOT a gap (no trading day lies
    // strictly between them in the calendar).
    let stored = vec![point_at(19_489, 195_00)];
    let current = point_at(19_490, 200_00);
    let calendar = dense_calendar(19_489, 19_490);
    let delta = compute_delta(&CaptureOutcome::Appended(current), &stored, &calendar);
    match delta {
        Delta::PointToPoint { spans_gap, .. } => {
            assert!(!spans_gap, "consecutive days are not a gap")
        }
        other => panic!("expected PointToPoint, got {other:?}"),
    }
}

// @spec SUMMARY-DELTA-002
#[test]
fn a_normal_friday_to_monday_pair_across_a_weekend_is_not_a_gap() {
    // The bug a calendar-day subtraction would introduce: Fri (day N) → Mon (day
    // N+3) is 3 CALENDAR days apart but only ONE trading day apart (the weekend is
    // NOT trading days). With a real trading-day calendar that omits Sat/Sun, no
    // trading day lies strictly between Fri and Mon, so spans_gap MUST be false —
    // never the weekend mislabel the design's decision table warns about.
    // (SUMMARY-DELTA-002, summary-design.md → Decisions: "Headline date")
    //
    // Day 19_496 = Fri, 19_499 = Mon; 19_497 (Sat) / 19_498 (Sun) are the weekend,
    // ABSENT from the trading-day calendar.
    let friday = 19_496;
    let monday = 19_499;
    let calendar = vec![
        TradingDayKey(Date(friday)),
        TradingDayKey(Date(monday)),
        // 19_497 (Sat) and 19_498 (Sun) are deliberately NOT trading days.
    ];
    let stored = vec![point_at(friday, 190_00)];
    let current = point_at(monday, 200_00);
    let delta = compute_delta(&CaptureOutcome::Appended(current), &stored, &calendar);
    match delta {
        Delta::PointToPoint {
            spans_gap,
            baseline_key,
            current_key,
            ..
        } => {
            assert_eq!(baseline_key, TradingDayKey(Date(friday)));
            assert_eq!(current_key, TradingDayKey(Date(monday)));
            assert!(
                !spans_gap,
                "Fri→Mon is consecutive TRADING days (3 calendar days) — not a gap"
            );
        }
        other => panic!("expected PointToPoint, got {other:?}"),
    }
}

// @spec SUMMARY-DELTA-003
#[test]
fn delta_across_an_incomplete_baseline_is_suppressed_not_fabricated() {
    // The baseline point was a degraded (incomplete) capture.
    let stored = vec![incomplete(point_at(19_485, 190_00))];
    let current = point_at(19_490, 200_00);
    let delta = compute_delta(&CaptureOutcome::Appended(current), &stored, &[]);
    match delta {
        Delta::Suppressed {
            reason,
            baseline_key,
            current_key,
        } => {
            assert_eq!(reason, SuppressReason::BaselineIncomplete);
            assert_eq!(baseline_key, TradingDayKey(Date(19_485)));
            assert_eq!(current_key, TradingDayKey(Date(19_490)));
        }
        other => panic!("expected Suppressed, got {other:?}"),
    }
}

// @spec SUMMARY-DELTA-003
#[test]
fn delta_across_an_incomplete_current_is_suppressed() {
    let stored = vec![point_at(19_485, 190_00)];
    // Today's appended point is itself incomplete (a symbol was degraded).
    let current = incomplete(point_at(19_490, 200_00));
    let delta = compute_delta(&CaptureOutcome::Appended(current), &stored, &[]);
    match delta {
        Delta::Suppressed { reason, .. } => assert_eq!(reason, SuppressReason::CurrentIncomplete),
        other => panic!("expected Suppressed, got {other:?}"),
    }
}

// @spec SUMMARY-DELTA-004
#[test]
fn first_ever_run_with_no_strictly_prior_point_renders_dash() {
    // The store has no point strictly prior to today's (it is the first capture).
    let current = point_at(19_490, 200_00);
    let delta = compute_delta(&CaptureOutcome::Appended(current.clone()), &[current], &[]);
    assert_eq!(delta, Delta::FirstEver);

    // Empty store is also first-ever.
    let current = point_at(19_490, 200_00);
    let delta = compute_delta(&CaptureOutcome::Appended(current), &[], &[]);
    assert_eq!(delta, Delta::FirstEver);
}

// @spec SUMMARY-DELTA-005
#[test]
fn uncaptured_run_uses_a_distinct_flagged_delta_against_the_latest_stored_point() {
    // No point appended this run (lock held): the current operand is the LIVE
    // uncaptured value, the baseline is the latest STORED point — a distinct,
    // flagged uncaptured delta, NOT the plain point-to-point form. (SUMMARY-DELTA-005)
    let stored = vec![point_at(19_485, 190_00), point_at(19_488, 195_00)];
    let live = point_at(19_490, 200_00);
    let capture = CaptureOutcome::SkippedLockHeld(live.clone());

    let delta = compute_delta(&capture, &stored, &[]);

    match delta {
        Delta::Uncaptured {
            baseline_key,
            live_total_cents,
            total_delta_cents,
        } => {
            // Baseline is the LATEST stored point (19_488), not strictly-prior-to-a
            // -current-key logic (there is no appended current point).
            assert_eq!(baseline_key, Some(TradingDayKey(Date(19_488))));
            assert_eq!(live_total_cents, live.total_market_value_cents);
            let expected = live.total_market_value_cents.0 - total_value(195_00);
            assert_eq!(total_delta_cents, Some(Cents(expected)));
        }
        other => panic!("expected Uncaptured, got {other:?}"),
    }
}

// @spec SUMMARY-DELTA-005
#[test]
fn append_failed_run_also_uses_the_uncaptured_delta_path() {
    // A failed append (offline) is uncaptured too: the same distinct flagged delta.
    let stored = vec![point_at(19_488, 195_00)];
    let live = point_at(19_490, 200_00);
    let delta = compute_delta(&CaptureOutcome::AppendFailed(live.clone()), &stored, &[]);
    assert!(
        matches!(delta, Delta::Uncaptured { .. }),
        "append-failed → uncaptured delta"
    );
}

// @spec SUMMARY-DELTA-005
#[test]
fn uncaptured_first_ever_run_has_no_baseline_to_compare() {
    // Uncaptured with an empty store: no stored point to compare against.
    let live = point_at(19_490, 200_00);
    let delta = compute_delta(&CaptureOutcome::SkippedLockHeld(live), &[], &[]);
    match delta {
        Delta::Uncaptured {
            baseline_key,
            total_delta_cents,
            ..
        } => {
            assert_eq!(baseline_key, None);
            assert_eq!(
                total_delta_cents, None,
                "no stored point → no fabricated delta"
            );
        }
        other => panic!("expected Uncaptured, got {other:?}"),
    }
}
