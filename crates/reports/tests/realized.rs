//! Realized-P&L History: REPORT-REAL-001.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use common::*;
use ledger_core::LedgerEvent;
use pt_core::Date;
use reports::{
    calendar_year_of, realized_history_by_year, realized_history_for_range, CalendarPeriod,
};

/// A two-year ledger: buy AMZN, sell some in 2022 (day 19_100) and more in 2023
/// (day 19_500). Both sells realize a gain. Returns the replayed snapshot.
fn two_year_snapshot() -> ledger_core::Snapshot {
    let events: Vec<LedgerEvent> = vec![
        // 6 shares @ $100 basis.
        buy(1, 19_000, "lot-amzn", "AMZN", 6_000_000, 100_00, "schwab"),
        // Sell 2 sh @ $150 in 2022 (day 19_100) → proceeds $300, basis $200, gain $100.
        sell(
            2,
            19_100,
            "sale-2022",
            "AMZN",
            2_000_000,
            150_00,
            "schwab",
            Some("NJ"),
        ),
        // Sell 2 sh @ $200 in 2023 (day 19_500) → proceeds $400, basis $200, gain $200.
        sell(
            3,
            19_500,
            "sale-2023",
            "AMZN",
            2_000_000,
            200_00,
            "schwab",
            Some("NJ"),
        ),
    ];
    replay(&events, &ledger_marks(&[("AMZN", 200_00)]))
}

// @spec REPORT-REAL-001
#[test]
fn realized_history_grouped_by_calendar_year() {
    let snap = two_year_snapshot();
    let y2022 = calendar_year_of(Date(19_100));
    let y2023 = calendar_year_of(Date(19_500));
    assert_ne!(y2022, y2023, "fixture spans two calendar years");

    let rows = realized_history_by_year(&snap);
    // One row per distinct calendar year of a sale, ascending.
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].period, CalendarPeriod::Year(y2022));
    assert_eq!(rows[1].period, CalendarPeriod::Year(y2023));

    // 2022 row: proceeds $300, basis $200, gain $100.
    assert_eq!(rows[0].proceeds_cents, pt_core::Cents(300_00));
    assert_eq!(rows[0].basis_cents, pt_core::Cents(200_00));
    assert_eq!(rows[0].gain_cents, pt_core::Cents(100_00));
    // 2023 row: proceeds $400, basis $200, gain $200.
    assert_eq!(rows[1].proceeds_cents, pt_core::Cents(400_00));
    assert_eq!(rows[1].gain_cents, pt_core::Cents(200_00));
}

// @spec REPORT-REAL-001
#[test]
fn realized_history_rows_are_labelled_calendar() {
    // Labels say "calendar" so they are never confused with tax's IRS
    // estimated-period quarterly figures.
    let snap = two_year_snapshot();
    let rows = realized_history_by_year(&snap);
    for r in &rows {
        assert!(
            r.label.to_lowercase().contains("calendar"),
            "row label must say 'calendar', got {:?}",
            r.label
        );
    }
}

// @spec REPORT-REAL-001
#[test]
fn realized_history_for_arbitrary_range_sums_only_in_range_sales() {
    let snap = two_year_snapshot();
    // A range that covers ONLY the 2022 sale (day 19_100), excluding the 2023 one.
    let only_2022 = realized_history_for_range(&snap, Date(19_050), Date(19_200));
    assert_eq!(only_2022.gain_cents, pt_core::Cents(100_00));
    assert_eq!(only_2022.proceeds_cents, pt_core::Cents(300_00));
    assert_eq!(
        only_2022.period,
        CalendarPeriod::Range {
            start: Date(19_050),
            end: Date(19_200)
        }
    );
    assert!(only_2022.label.to_lowercase().contains("calendar"));

    // A range covering BOTH sales sums them: gain $100 + $200 = $300.
    let both = realized_history_for_range(&snap, Date(19_000), Date(19_600));
    assert_eq!(both.gain_cents, pt_core::Cents(300_00));
    assert_eq!(both.proceeds_cents, pt_core::Cents(700_00));
}

// @spec REPORT-REAL-001
#[test]
fn realized_history_empty_when_no_sales() {
    // A portfolio with no realized gains yields no calendar rows.
    let snap = small_snapshot(&ledger_marks(&[("AMZN", 200_00)]));
    // small_snapshot has one GOOG sale, so use a buy-only snapshot instead.
    let buy_only = replay(
        &[buy(
            1, 19_000, "lot-amzn", "AMZN", 3_000_000, 150_00, "schwab",
        )],
        &ledger_marks(&[("AMZN", 200_00)]),
    );
    let _ = snap;
    assert!(realized_history_by_year(&buy_only).is_empty());
}

// @spec REPORT-REAL-002
#[test]
fn realized_ytd_sums_only_the_given_years_sales() {
    let snap = two_year_snapshot();
    let y2023 = calendar_year_of(Date(19_500));
    let row = reports::realized_ytd(&snap, y2023);
    // Only the 2023 sale: proceeds $400, basis $200, gain $200 — the 2022 sale
    // is excluded.
    assert_eq!(row.proceeds_cents, pt_core::Cents(400_00));
    assert_eq!(row.basis_cents, pt_core::Cents(200_00));
    assert_eq!(row.gain_cents, pt_core::Cents(200_00));
    assert_eq!(row.period, CalendarPeriod::Year(y2023));
    assert!(
        row.label.to_lowercase().contains("calendar"),
        "labelled calendar: {}",
        row.label
    );
}

// @spec REPORT-REAL-002
#[test]
fn realized_ytd_returns_an_exact_zero_row_when_the_year_has_no_sales() {
    // Realized history is reconstructable: an empty year is a TRUE zero, returned
    // as an exact zero row (never a degraded marker, never absent).
    let snap = two_year_snapshot();
    let row = reports::realized_ytd(&snap, 1999);
    assert_eq!(row.proceeds_cents, pt_core::Cents(0));
    assert_eq!(row.basis_cents, pt_core::Cents(0));
    assert_eq!(row.gain_cents, pt_core::Cents(0));
    assert_eq!(row.period, CalendarPeriod::Year(1999));
}
