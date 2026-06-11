//! Value-Over-Time (the History series): REPORT-VOT-001/002/003/004/005.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use common::*;
use pt_core::{Cents, Date, MicroShares};
use reports::{
    build_series_point, per_symbol_value_delta, render_series, series_delta, SeriesElement,
    TradingDayKey,
};

/// Build a series point for the small portfolio at a given trading-day key, with
/// the given priced marks and a capture timestamp.
fn point_at(key_day: i32, marks: &reports::PricedMarks, captured_at: i64) -> reports::SeriesPoint {
    // Replay with ledger marks matching the priced marks (so per-lot unrealized
    // is consistent), then build the point.
    let lm: Vec<(&str, i64)> = marks
        .iter()
        .map(|(s, m)| (s.as_str(), m.price_cents.0))
        .collect();
    let snap = small_snapshot(&ledger_marks(&lm));
    let as_of = Date(19_500);
    let ctx = ctx(2022);
    let est = estimates(&snap, as_of, &ctx);
    build_series_point(
        &snap,
        marks,
        &est,
        TradingDayKey(Date(key_day)),
        captured_at,
        Date(19_500),
    )
}

// @spec REPORT-VOT-001
#[test]
fn point_keyed_by_trading_day_not_run_calendar_day() {
    // The point's key is the trading-day key runtime supplies (19_490), NOT the
    // capture timestamp or the reporting-TZ calendar date (19_500).
    let marks = priced_marks(&[("AMZN", 200_00, 19_490), ("GOOG", 120_00, 19_490)]);
    let p = point_at(19_490, &marks, 1_700_000_000);
    assert_eq!(p.key, TradingDayKey(Date(19_490)));
    assert_eq!(p.reporting_tz_date, Date(19_500));
    assert_eq!(p.captured_at_epoch_secs, 1_700_000_000);
    // The total market value is real (AMZN $600 + GOOG $120 = $720).
    assert_eq!(p.total_market_value_cents, Cents(720_00));
    assert!(!p.incomplete);
}

// @spec REPORT-VOT-001
#[test]
fn consecutive_captures_sharing_quote_epoch_resolve_to_one_point_last_wins() {
    // Two captures share the SAME trading-day key (markets closed since — weekend),
    // so when rendered against that one trading day they resolve to a single point
    // (last-wins). The series advances only when the trading-day key advances.
    let marks_a = priced_marks(&[("AMZN", 200_00, 19_490), ("GOOG", 120_00, 19_490)]);
    let marks_b = priced_marks(&[("AMZN", 210_00, 19_490), ("GOOG", 120_00, 19_490)]);
    let first = point_at(19_490, &marks_a, 1_700_000_000);
    let second = point_at(19_490, &marks_b, 1_700_086_400); // a day later, same epoch

    // Both carry the same key.
    assert_eq!(first.key, second.key);

    // Rendered over the single trading day, the LATER capture wins (last-wins).
    let series = render_series(&[first, second.clone()], &[TradingDayKey(Date(19_490))]);
    let points: Vec<_> = series
        .elements
        .iter()
        .filter_map(|e| match e {
            SeriesElement::Point(p) => Some(p),
            SeriesElement::Gap { .. } => None,
        })
        .collect();
    assert_eq!(points.len(), 1, "one point per trading day");
    assert_eq!(
        points[0].total_market_value_cents,
        second.total_market_value_cents
    );
}

// @spec REPORT-VOT-002
#[test]
fn per_symbol_value_is_recorded_and_shares_are_metadata_only() {
    let marks = priced_marks(&[("AMZN", 200_00, 19_490), ("GOOG", 120_00, 19_490)]);
    let p = point_at(19_490, &marks, 1_700_000_000);

    // Per-symbol VALUE is the cross-time axis (split-neutral): AMZN $600, GOOG $120.
    assert_eq!(p.per_symbol_value_cents.get("AMZN"), Some(&Cents(600_00)));
    assert_eq!(p.per_symbol_value_cents.get("GOOG"), Some(&Cents(120_00)));
    // Share counts are recorded only as point-in-time metadata (AMZN 3, GOOG 1).
    assert_eq!(
        p.per_symbol_shares.get("AMZN"),
        Some(&MicroShares(3_000_000))
    );
    assert_eq!(
        p.per_symbol_shares.get("GOOG"),
        Some(&MicroShares(1_000_000))
    );
}

// @spec REPORT-VOT-002
#[test]
fn per_symbol_value_is_compared_across_time_never_share_counts() {
    // Per-symbol VALUE is the cross-time axis: from day 19_490 (AMZN $600, GOOG
    // $120) to day 19_491 (AMZN $630, GOOG $130) the value deltas are AMZN +$30,
    // GOOG +$10. Share counts are unchanged (3 / 1) and are NEVER diffed — the
    // comparison is by value alone. (REPORT-VOT-002)
    let m0 = priced_marks(&[("AMZN", 200_00, 19_490), ("GOOG", 120_00, 19_490)]);
    let earlier = point_at(19_490, &m0, 1_700_000_000);
    let m1 = priced_marks(&[("AMZN", 210_00, 19_491), ("GOOG", 130_00, 19_491)]);
    let later = point_at(19_491, &m1, 1_700_086_400);

    let deltas = per_symbol_value_delta(&earlier, &later);

    let amzn = deltas
        .iter()
        .find(|d| d.symbol == "AMZN")
        .expect("AMZN delta");
    assert_eq!(amzn.from_value_cents, Some(Cents(600_00)));
    assert_eq!(amzn.to_value_cents, Some(Cents(630_00)));
    assert_eq!(amzn.delta_value_cents, Some(Cents(30_00)));
    let goog = deltas
        .iter()
        .find(|d| d.symbol == "GOOG")
        .expect("GOOG delta");
    assert_eq!(goog.delta_value_cents, Some(Cents(10_00)));

    // Share counts are identical across the two points (no split) and are not used
    // by the value comparison — the cross-time axis is value, never shares.
    assert_eq!(
        earlier.per_symbol_shares.get("AMZN"),
        later.per_symbol_shares.get("AMZN")
    );
}

// @spec REPORT-VOT-002
#[test]
fn per_symbol_value_delta_is_none_when_an_endpoint_is_degraded() {
    // When a symbol lacks a priced value at one endpoint (degraded that capture),
    // its value delta is None — a missing mark never fabricates a cross-time delta.
    let m_full = priced_marks(&[("AMZN", 200_00, 19_490), ("GOOG", 120_00, 19_490)]);
    let earlier = point_at(19_490, &m_full, 1_700_000_000);
    let m_part = priced_marks(&[("AMZN", 210_00, 19_491)]); // GOOG degraded → no value
    let later = point_at(19_491, &m_part, 1_700_086_400);

    let deltas = per_symbol_value_delta(&earlier, &later);
    let goog = deltas
        .iter()
        .find(|d| d.symbol == "GOOG")
        .expect("GOOG row");
    assert_eq!(
        goog.to_value_cents, None,
        "GOOG has no priced value at the later point"
    );
    assert_eq!(
        goog.delta_value_cents, None,
        "no fabricated delta across a degraded endpoint"
    );
}

// @spec REPORT-VOT-003
#[test]
fn trading_days_with_no_capture_show_explicit_gaps_no_interpolation() {
    // Captures on day 19_490 and 19_492; day 19_491 has NO capture → an explicit
    // gap (no interpolation between the two values).
    let m = priced_marks(&[("AMZN", 200_00, 19_490), ("GOOG", 120_00, 19_490)]);
    let p0 = point_at(19_490, &m, 1_700_000_000);
    let m2 = priced_marks(&[("AMZN", 220_00, 19_492), ("GOOG", 120_00, 19_492)]);
    let p2 = point_at(19_492, &m2, 1_700_200_000);

    let series = render_series(
        &[p0, p2],
        &[
            TradingDayKey(Date(19_490)),
            TradingDayKey(Date(19_491)),
            TradingDayKey(Date(19_492)),
        ],
    );

    assert_eq!(series.elements.len(), 3);
    assert!(matches!(series.elements[0], SeriesElement::Point(_)));
    // The middle trading day is an EXPLICIT gap — never an interpolated value.
    assert_eq!(
        series.elements[1],
        SeriesElement::Gap {
            key: TradingDayKey(Date(19_491))
        }
    );
    assert!(matches!(series.elements[2], SeriesElement::Point(_)));
}

// @spec REPORT-VOT-004
#[test]
fn capture_with_degraded_symbol_is_flagged_incomplete_with_partial_total() {
    // GOOG has no mark this capture → the point is incomplete and its total is
    // partial (AMZN $600 only, GOOG not fabricated).
    let marks = priced_marks(&[("AMZN", 200_00, 19_490)]); // GOOG omitted
    let p = point_at(19_490, &marks, 1_700_000_000);
    assert!(p.incomplete, "a degraded-symbol capture is incomplete");
    // The total is the priced-only partial: AMZN $600, NOT a fabricated GOOG value.
    assert_eq!(p.total_market_value_cents, Cents(600_00));
    // GOOG carries no fabricated value.
    assert!(p.per_symbol_value_cents.get("GOOG").is_none());
}

// @spec REPORT-VOT-004
#[test]
fn delta_across_incomplete_point_is_flagged_not_silently_shown() {
    // A complete point then an incomplete point: the day-over-day delta must be
    // FLAGGED (so summary suppresses/marks it), never silently computed.
    let m_full = priced_marks(&[("AMZN", 200_00, 19_490), ("GOOG", 120_00, 19_490)]);
    let complete = point_at(19_490, &m_full, 1_700_000_000);
    let m_part = priced_marks(&[("AMZN", 210_00, 19_491)]); // GOOG degraded
    let incomplete = point_at(19_491, &m_part, 1_700_086_400);

    let d = series_delta(&complete, &incomplete);
    assert!(
        d.flagged_incomplete,
        "delta across an incomplete point is flagged"
    );

    // A delta between two COMPLETE points is not flagged.
    let m2 = priced_marks(&[("AMZN", 210_00, 19_491), ("GOOG", 130_00, 19_491)]);
    let complete2 = point_at(19_491, &m2, 1_700_086_400);
    let d2 = series_delta(&complete, &complete2);
    assert!(!d2.flagged_incomplete);
    // Its value change is real: total went $720 → ($630 + $130) = $760, Δ +$40.
    assert_eq!(d2.delta_market_value_cents, Cents(40_00));
}

// @spec REPORT-VOT-005
#[test]
fn series_begins_at_first_capture_no_pre_capture_values_fabricated() {
    // The only capture is on day 19_492. Rendering a span that begins BEFORE the
    // first capture must not fabricate pre-capture daily values — those days are
    // explicit gaps, never invented points.
    let m = priced_marks(&[("AMZN", 200_00, 19_492), ("GOOG", 120_00, 19_492)]);
    let only = point_at(19_492, &m, 1_700_200_000);

    let series = render_series(
        &[only],
        &[
            TradingDayKey(Date(19_490)),
            TradingDayKey(Date(19_491)),
            TradingDayKey(Date(19_492)),
        ],
    );
    // Days before the first capture are gaps, not fabricated points.
    assert_eq!(
        series.elements[0],
        SeriesElement::Gap {
            key: TradingDayKey(Date(19_490))
        }
    );
    assert_eq!(
        series.elements[1],
        SeriesElement::Gap {
            key: TradingDayKey(Date(19_491))
        }
    );
    assert!(matches!(series.elements[2], SeriesElement::Point(_)));
}

// @spec REPORT-VOT-006
#[test]
fn per_symbol_value_delta_carries_percent_change_relative_to_earlier() {
    // AMZN $600 → $630: +30/600 = +5.0% = +50,000 ppm. GOOG $120 → $130:
    // +10/120 = 83,333.33… ppm → 83,333 (half-to-even).
    let m0 = priced_marks(&[("AMZN", 200_00, 19_490), ("GOOG", 120_00, 19_490)]);
    let earlier = point_at(19_490, &m0, 1_700_000_000);
    let m1 = priced_marks(&[("AMZN", 210_00, 19_491), ("GOOG", 130_00, 19_491)]);
    let later = point_at(19_491, &m1, 1_700_086_400);

    let deltas = per_symbol_value_delta(&earlier, &later);
    let amzn = deltas
        .iter()
        .find(|d| d.symbol == "AMZN")
        .expect("AMZN delta");
    assert_eq!(amzn.delta_pct_ppm, Some(config::Ppm(50_000)));
    let goog = deltas
        .iter()
        .find(|d| d.symbol == "GOOG")
        .expect("GOOG delta");
    assert_eq!(goog.delta_pct_ppm, Some(config::Ppm(83_333)));
}

// @spec REPORT-VOT-006
#[test]
fn per_symbol_percent_change_is_na_without_a_meaningful_base() {
    // A degraded endpoint → no percent (and no $ delta); an earlier value of 0 →
    // no percent even though the $ delta is real — never a fabricated percentage.
    let m_full = priced_marks(&[("AMZN", 200_00, 19_490), ("GOOG", 120_00, 19_490)]);
    let earlier = point_at(19_490, &m_full, 1_700_000_000);
    let m_part = priced_marks(&[("AMZN", 210_00, 19_491)]); // GOOG degraded later
    let later = point_at(19_491, &m_part, 1_700_086_400);
    let deltas = per_symbol_value_delta(&earlier, &later);
    let goog = deltas
        .iter()
        .find(|d| d.symbol == "GOOG")
        .expect("GOOG row");
    assert_eq!(
        goog.delta_pct_ppm, None,
        "no percent across a degraded endpoint"
    );

    // An earlier value of exactly 0 (a zero-priced mark) is no base to divide by.
    let m_zero = priced_marks(&[("AMZN", 0, 19_490), ("GOOG", 120_00, 19_490)]);
    let zero_earlier = point_at(19_490, &m_zero, 1_700_000_000);
    let m_next = priced_marks(&[("AMZN", 210_00, 19_491), ("GOOG", 120_00, 19_491)]);
    let next = point_at(19_491, &m_next, 1_700_086_400);
    let deltas = per_symbol_value_delta(&zero_earlier, &next);
    let amzn = deltas
        .iter()
        .find(|d| d.symbol == "AMZN")
        .expect("AMZN row");
    assert_eq!(amzn.from_value_cents, Some(Cents(0)));
    assert!(amzn.delta_value_cents.is_some(), "the $ delta is real");
    assert_eq!(amzn.delta_pct_ppm, None, "earlier ≤ 0 → no percent base");
}
