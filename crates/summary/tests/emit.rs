//! Emitted Summary: SUMMARY-EMIT-001, SUMMARY-EMIT-002, SUMMARY-EMIT-003,
//! SUMMARY-EMIT-004.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use common::*;
use config::BracketState;
use pt_core::Date;
use reports::TradingDayKey;
use summary::{build_report, compute_delta, CaptureOutcome, Report, TaxQualifier};

/// Build a fresh-run report for the given inputs, with a prior stored baseline so
/// the header carries a real point-to-point delta.
fn report_for(inputs: &summary::SummaryInputs, baseline_amzn: i64) -> Report {
    let current = summary::build_current_point(inputs).expect("a priced point");
    let stored = vec![point_at(inputs.trading_day_key.unwrap().0 .0 - 5, baseline_amzn)];
    let capture = CaptureOutcome::Appended(current.clone());
    let delta = compute_delta(&capture, &stored, &[]);
    build_report(inputs, &capture, delta, stored.first())
}

// @spec SUMMARY-EMIT-001, SUMMARY-EMIT-007
#[test]
fn report_is_headlined_by_the_trading_day_with_the_run_time_secondary() {
    let inputs = inputs_at(19_490, 200_00);
    let report = report_for(&inputs, 190_00);

    // The headline date is the TRADING DAY of the displayed value (the appended
    // point's key), NOT the calendar run date. (SUMMARY-EMIT-001)
    assert_eq!(report.trading_day, Some(TradingDayKey(Date(19_490))));
    // The run timestamp is carried as the secondary "as of" line.
    assert_eq!(report.run_at_epoch_secs, inputs.run_at_epoch_secs);
    // Header + per-symbol positions + a current-year tax-reserve line all present.
    assert!(!report.positions.is_empty(), "per-symbol positions present");
    assert_eq!(report.tax_line.year, 2022, "current-year tax-reserve line present");
    // Total value = the priced market value (AMZN 3@$200 + GOOG 1@$120 = $720).
    assert_eq!(report.header.total_value_cents.0, 200_00 * 3 + 120_00);
}

// @spec SUMMARY-EMIT-002
#[test]
fn net_uses_the_same_priced_set_as_total_value_and_is_partial_when_total_is() {
    // Fully priced: Net is present, not flagged partial, and is ≤ total value (tax
    // on unrealized gains is subtracted), using the SAME priced set.
    let priced = report_for(&inputs_at(19_490, 200_00), 190_00);
    assert!(!priced.header.partial, "fully priced → not partial");
    let net = priced.header.net_post_tax_cents.expect("net present when priced");
    assert!(net.0 <= priced.header.total_value_cents.0, "net ≤ total (tax subtracted)");

    // Degraded: GOOG unpriced. Total value is partial → Net is flagged partial too,
    // and Net is computed over the SAME priced/degraded set as Total value.
    // (SUMMARY-EMIT-002)
    let degraded = report_for(&inputs_degraded(19_490, 200_00), 190_00);
    assert!(degraded.header.partial, "an unpriced symbol makes totals partial");
    // Total value counts only AMZN (the priced symbol): 3 @ $200 = $600.
    assert_eq!(degraded.header.total_value_cents.0, 200_00 * 3);
    // Net is present (Verified brackets) and over the same priced set.
    assert!(degraded.header.net_post_tax_cents.is_some());
}

// @spec SUMMARY-EMIT-003
#[test]
fn unpriced_symbols_are_named_and_totals_marked_partial() {
    let report = report_for(&inputs_degraded(19_490, 200_00), 190_00);

    // The affected symbol (GOOG) is named in the degraded-symbols footer.
    assert!(report.degraded_symbols.iter().any(|s| s == "GOOG"));
    // Totals are marked partial. (SUMMARY-EMIT-003)
    assert!(report.header.partial);
    // GOOG's position row is degraded (no price/value, delta unavailable).
    let goog = report.positions.iter().find(|p| p.symbol == "GOOG").expect("GOOG row");
    assert!(goog.degraded);
    assert!(goog.price_cents.is_none());
    assert!(goog.value_cents.is_none());
    assert!(goog.delta_cents.is_none(), "a degraded symbol's delta is never fabricated");
    // AMZN is priced and not degraded.
    let amzn = report.positions.iter().find(|p| p.symbol == "AMZN").expect("AMZN row");
    assert!(!amzn.degraded);
    assert!(amzn.price_cents.is_some());
}

// @spec SUMMARY-EMIT-008
#[test]
fn priced_symbol_with_no_folded_tax_estimate_marks_partial_without_naming_it() {
    // A symbol that IS priced (carries a mark) but lacks a folded tax estimate makes
    // the totals partial (`Header.partial`, via reports' incomplete point) WITHOUT
    // being named in `degraded_symbols` — `[partial]` can show with no named symbol,
    // and the net is never fabricated. (SUMMARY-EMIT-008)
    let mut inputs = inputs_at(19_490, 200_00);
    // Drop GOOG's unrealized estimate while leaving its mark in place: GOOG is still
    // priced (a value folds into the total) but has no tax estimate to fold.
    inputs.estimates.remove("GOOG");

    let report = report_for(&inputs, 190_00);

    // Totals are partial (the point is incomplete from the missing estimate).
    assert!(report.header.partial, "a missing folded estimate makes totals partial");
    // GOOG is NOT named as degraded (it is priced — it has a value/price row).
    assert!(
        !report.degraded_symbols.iter().any(|s| s == "GOOG"),
        "a priced-but-no-estimate symbol is not named in degraded_symbols"
    );
    let goog = report.positions.iter().find(|p| p.symbol == "GOOG").expect("GOOG row");
    assert!(!goog.degraded, "GOOG is priced (a value), so its row is not degraded");
    assert!(goog.value_cents.is_some(), "GOOG carries a priced value");
    assert!(goog.price_cents.is_some(), "GOOG carries a per-share price");
}

// @spec SUMMARY-EMIT-004
#[test]
fn net_and_tax_line_render_verified_plainly() {
    let report = report_for(&inputs_at(19_490, 200_00), 190_00);
    assert_eq!(report.header.net_qualifier, TaxQualifier::Plain);
    assert_eq!(report.tax_line.qualifier, TaxQualifier::Plain);
    assert!(report.header.net_post_tax_cents.is_some());
    assert!(report.tax_line.accrued_cents.is_some());
}

// @spec SUMMARY-EMIT-004
#[test]
fn stale_brackets_mark_net_and_tax_line_estimated_stale() {
    let inputs = with_bracket_state(inputs_at(19_490, 200_00), BracketState::Stale);
    let report = report_for(&inputs, 190_00);
    // Numbers are still shown, marked stale (nudging a refresh). (SUMMARY-EMIT-004)
    assert_eq!(report.header.net_qualifier, TaxQualifier::Stale);
    assert_eq!(report.tax_line.qualifier, TaxQualifier::Stale);
    assert!(report.header.net_post_tax_cents.is_some(), "stale still shows a number");
    assert!(report.tax_line.accrued_cents.is_some());
}

// @spec SUMMARY-EMIT-004
#[test]
fn cold_start_no_brackets_shows_na_never_a_fabricated_net_or_reserve() {
    let inputs =
        with_bracket_state(inputs_at(19_490, 200_00), BracketState::NoBracketsAvailable);
    let report = report_for(&inputs, 190_00);
    // Net shows n/a (no brackets); the tax line says set up brackets — never a
    // fabricated reserve or net. (SUMMARY-EMIT-004)
    assert_eq!(report.header.net_qualifier, TaxQualifier::NoBrackets);
    assert_eq!(report.tax_line.qualifier, TaxQualifier::NoBrackets);
    assert!(report.header.net_post_tax_cents.is_none(), "no fabricated net on cold-start");
    assert!(report.tax_line.accrued_cents.is_none(), "no fabricated reserve on cold-start");
    assert!(report.tax_line.outstanding_cents.is_none());
    // Total value (mark-based, bracket-independent) is still shown.
    assert_eq!(report.header.total_value_cents.0, 200_00 * 3 + 120_00);
}

// @spec SUMMARY-EMIT-001, SUMMARY-EMIT-005
#[test]
fn tax_line_carries_distinct_accrued_moved_and_outstanding_with_reserve_events() {
    // With non-zero Move ($300) and Pay ($110) reserve events, the tax line's three
    // figures are distinct: accrued is the computed reserve, moved is Σ Move, and
    // outstanding is tax's canonical `accrued − paid`. The summary sources tax's
    // AnnualRow fields directly, so the field choice is pinned here (not invisible as
    // it is when moved=paid=0). (SUMMARY-EMIT-001)
    let moved = 300_00;
    let paid = 110_00;
    let inputs = inputs_with_reserve_events(19_490, 200_00, moved, paid);
    let report = report_for(&inputs, 190_00);

    let accrued = report.tax_line.accrued_cents.expect("accrued present (Verified)").0;
    assert_eq!(report.tax_line.moved_cents, Some(pt_core::Cents(moved)), "moved = Σ Move");
    let outstanding = report.tax_line.outstanding_cents.expect("outstanding present").0;
    // outstanding is tax's canonical accrued − paid (NOT accrued − moved).
    assert_eq!(outstanding, accrued - paid, "outstanding = accrued − paid (tax's definition)");
    // The reserve actually has a real (non-zero) accrual from the GOOG sale, so the
    // three figures are genuinely distinct and the test is not vacuous.
    assert!(accrued != 0, "the GOOG sale accrues a real reserve");
    assert_ne!(outstanding, accrued, "paid != 0 → outstanding distinct from accrued");

    // The rendered text surfaces all three figures (the design example shows moved
    // alongside accrued and outstanding). (SUMMARY-EMIT-001)
    let text = summary::render_text(&report);
    assert!(text.contains("accrued"), "text shows accrued");
    assert!(text.contains("moved"), "text shows moved");
    assert!(text.contains("outstanding"), "text shows outstanding");

    // The versioned JSON carries moved and outstanding distinctly.
    let json = summary::render_json(&report);
    assert!(json.contains(&format!("\"moved\":{moved}")), "json moved = Σ Move");
    assert!(
        json.contains(&format!("\"outstanding\":{}", accrued - paid)),
        "json outstanding = accrued − paid"
    );
}

// @spec SUMMARY-EMIT-006
#[test]
fn tax_line_emits_the_next_estimated_payment_period_from_quarter_of_today() {
    // The next estimated-payment period is computed from tax::quarterly_report +
    // quarter_of(today): today is the reporting-TZ date (19_500 = 2023-05-23 → Q2).
    // It is surfaced on the tax line and in the JSON's tax.next_period — never
    // fabricated. (SUMMARY-EMIT-006)
    let inputs = inputs_at(19_490, 200_00);
    // The fixture's reporting_tz_date is 19_500 (2023-05-23) → Q2 (Apr 1–May 31).
    // The period summary surfaces is tax's own quarter_of partition — the very
    // partition tax::quarterly_report keys its per-period cells by. (SUMMARY-EMIT-006)
    assert_eq!(tax::quarter_of(inputs.reporting_tz_date), tax::Quarter::Q2);

    let report = report_for(&inputs, 190_00);

    // The period actually emitted is quarter_of(today). (SUMMARY-EMIT-006)
    assert_eq!(report.tax_line.next_period, Some(tax::Quarter::Q2));

    // It is surfaced in the rendered text and JSON.
    let text = summary::render_text(&report);
    assert!(text.contains("Q2"), "text surfaces the next estimated-payment period");
    let json = summary::render_json(&report);
    assert!(json.contains("\"next_period\":\"Q2\""), "json carries tax.next_period");
}

// @spec SUMMARY-EMIT-006
#[test]
fn next_period_tracks_quarter_of_today_not_the_tax_year() {
    // Move today into a different IRS period and the emitted period follows
    // quarter_of(today) — confirming it is genuinely computed, not a constant.
    // Day 19_000 = 2022-01-08 → Q1 (Jan 1–Mar 31). (SUMMARY-EMIT-006)
    let mut inputs = inputs_at(19_490, 200_00);
    inputs.reporting_tz_date = Date(19_000);
    assert_eq!(tax::quarter_of(inputs.reporting_tz_date), tax::Quarter::Q1);

    let report = report_for(&inputs, 190_00);
    assert_eq!(report.tax_line.next_period, Some(tax::Quarter::Q1));

    // And a Q4 date (19_270 = 2022-10-05 → Q4 Sep 1–Dec 31).
    inputs.reporting_tz_date = Date(19_270);
    assert_eq!(tax::quarter_of(inputs.reporting_tz_date), tax::Quarter::Q4);
    let report = report_for(&inputs, 190_00);
    assert_eq!(report.tax_line.next_period, Some(tax::Quarter::Q4));
    assert!(summary::render_json(&report).contains("\"next_period\":\"Q4\""));
}

// @spec SUMMARY-EMIT-006
#[test]
fn cold_start_emits_no_fabricated_next_period() {
    // On cold-start (NoBracketsAvailable) there is no period: the field is None and
    // the JSON carries null — never a fabricated period. (SUMMARY-EMIT-006)
    let inputs =
        with_bracket_state(inputs_at(19_490, 200_00), BracketState::NoBracketsAvailable);
    let report = report_for(&inputs, 190_00);

    assert_eq!(report.tax_line.next_period, None, "no fabricated period on cold-start");

    let json = summary::render_json(&report);
    assert!(json.contains("\"next_period\":null"), "json next_period is null on cold-start");
    // The text tax line says set up brackets and surfaces no period.
    let text = summary::render_text(&report);
    assert!(!text.contains("Next est. payment period"), "no period line on cold-start");
}

// @spec SUMMARY-DELTA-005
#[test]
fn uncaptured_run_does_not_render_plain_per_symbol_deltas() {
    // On the uncaptured (lock-held / append-failed) path the design's delta is a
    // *distinct, flagged* live-vs-stored figure — NOT the plain point-to-point form.
    // So per-symbol rows must not show an unmarked signed move that reads like a
    // normal captured per-symbol delta; their delta_cents is suppressed (None).
    // (SUMMARY-DELTA-005)
    let inputs = inputs_at(19_490, 200_00);
    let live = summary::build_current_point(&inputs).expect("a priced point");
    let stored = vec![point_at(19_485, 190_00)];
    let capture = CaptureOutcome::SkippedLockHeld(live);
    let delta = compute_delta(&capture, &stored, &[]);
    let report = build_report(&inputs, &capture, delta, stored.first());

    assert!(report.stale, "an uncaptured run is stale");
    assert!(matches!(report.delta, summary::Delta::Uncaptured { .. }));
    for p in &report.positions {
        assert!(
            p.delta_cents.is_none(),
            "per-symbol delta is not a plain point-to-point move on the uncaptured path ({})",
            p.symbol
        );
    }
}
