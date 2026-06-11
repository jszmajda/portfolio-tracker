//! Red (failing) tests for the `tax` RESERVE (TAX-RESERVE-001/002) and
//! REPORTING (TAX-REPORT-001..004) specs. These ride on the lifecycle fold over
//! the `TaxEvent` log (outside the verus boundary), so they are `#[test]`s.

#![allow(clippy::inconsistent_digit_grouping)]

mod common;
use common::*;

use config::{BracketState, Jurisdiction};
use tax::{
    annual_report, quarter_of, quarterly_report, reserves, safe_harbor_ppm, Quarter,
};

fn ctx() -> tax::TaxContext {
    context(
        2025,
        federal(flat(300_000), flat(0), niit(0, 0), 0, BracketState::Verified),
        &[],
        0,
        None,
    )
}
fn one_gain() -> ledger_core::RealizedGain {
    // Short-term → federal ordinary 30% → 30_000c accrual.
    gain("s1", 1, "lotA", "AMZN", date(2025, 1, 2), date(2025, 6, 1), 100_000, None)
}

fn fed_reserve(reserves: &[tax::Reserve], tax_year: i32) -> Option<&tax::Reserve> {
    reserves
        .iter()
        .find(|r| r.jurisdiction == Jurisdiction::Federal && r.tax_year.0 == tax_year)
}

// ===========================================================================
// TAX-RESERVE-001 — reserve(J, year) = Σ Move − Σ Pay.
// ===========================================================================

// @spec TAX-RESERVE-001
#[test]
fn reserve_001_balance_is_sum_moves_minus_sum_pays() {
    let key = fed_key("s1", "lotA", 2025);
    // Move 30_000 + Move 5_000 = 35_000 in; Pay 20_000 out → balance 15_000.
    let key2 = fed_key("s2", "lotB", 2025);
    // Both keys are backed by real gains, so the reserve is not flagged backless.
    let gains = vec![
        gain("s1", 1, "lotA", "AMZN", date(2025, 1, 2), date(2025, 6, 1), 100_000, None),
        gain("s2", 2, "lotB", "AMZN", date(2025, 1, 2), date(2025, 6, 1), 100_000, None),
    ];
    let events = vec![
        allocate(1, key.clone(), "acct"),
        move_(2, key.clone(), 30_000, date(2025, 6, 15)),
        allocate(3, key2.clone(), "acct"),
        move_(4, key2.clone(), 5_000, date(2025, 7, 1)),
        pay(5, Jurisdiction::Federal, 2025, Quarter::Q2, 20_000, date(2025, 6, 16), vec![key]),
    ];
    let r = reserves(&gains, &events, &ctx());
    let fed = fed_reserve(&r, 2025).expect("a federal 2025 reserve must exist");
    assert_eq!(
        fed.balance_cents,
        pt_core::Cents(15_000),
        "reserve = Σ Move (35_000) − Σ Pay (20_000) = 15_000"
    );
    assert!(!fed.has_backless_entry, "backed Move/Pay → not a backless reserve");
}

// ===========================================================================
// TAX-RESERVE-002 — a reserve may go negative (over/under-funding signal), the
// Pay is not rejected for it.
// ===========================================================================

// @spec TAX-RESERVE-002
#[test]
fn reserve_002_may_go_negative() {
    let key = fed_key("s1", "lotA", 2025);
    // Move 10_000 in, Pay 25_000 out → balance −15_000 (allowed).
    let gains = vec![one_gain()];
    let events = vec![
        allocate(1, key.clone(), "acct"),
        move_(2, key.clone(), 10_000, date(2025, 6, 15)),
        pay(3, Jurisdiction::Federal, 2025, Quarter::Q2, 25_000, date(2025, 6, 16), vec![key]),
    ];
    let r = reserves(&gains, &events, &ctx());
    let fed = fed_reserve(&r, 2025).expect("a federal 2025 reserve must exist");
    assert_eq!(
        fed.balance_cents,
        pt_core::Cents(-15_000),
        "a reserve may go negative as an over/under-funding signal"
    );
}

// ===========================================================================
// TAX-REPORT-001 — quarterly report groups by sale_date into the IRS periods.
// ===========================================================================

// @spec TAX-REPORT-001
#[test]
fn report_001_quarter_of_partitions_the_year() {
    // Boundaries of the four IRS periods (Q1 Jan–Mar, Q2 Apr–May, Q3 Jun–Aug,
    // Q4 Sep–Dec).
    assert_eq!(quarter_of(date(2025, 1, 1)), Quarter::Q1);
    assert_eq!(quarter_of(date(2025, 3, 31)), Quarter::Q1);
    assert_eq!(quarter_of(date(2025, 4, 1)), Quarter::Q2);
    assert_eq!(quarter_of(date(2025, 5, 31)), Quarter::Q2);
    assert_eq!(quarter_of(date(2025, 6, 1)), Quarter::Q3);
    assert_eq!(quarter_of(date(2025, 8, 31)), Quarter::Q3);
    assert_eq!(quarter_of(date(2025, 9, 1)), Quarter::Q4);
    assert_eq!(quarter_of(date(2025, 12, 31)), Quarter::Q4);
}

// @spec TAX-REPORT-001
#[test]
fn report_001_quarterly_report_groups_gains_by_sale_date() {
    // Two gains, one in Q1 (Feb) one in Q3 (Jul). The report places each in its
    // period's federal cell.
    // Both short-term (acquired the prior month), so they land in short-term gain.
    let g1 = gain("s1", 1, "lotA", "AMZN", date(2025, 1, 2), date(2025, 2, 1), 100_000, None);
    let g2 = gain("s2", 2, "lotB", "AMZN", date(2025, 6, 1), date(2025, 7, 1), 100_000, None);
    let cells = quarterly_report(&[g1, g2], &[], &ctx());

    let q1 = cells
        .iter()
        .find(|c| c.period == Quarter::Q1 && c.jurisdiction == Jurisdiction::Federal)
        .expect("a Q1 federal cell must exist");
    let q3 = cells
        .iter()
        .find(|c| c.period == Quarter::Q3 && c.jurisdiction == Jurisdiction::Federal)
        .expect("a Q3 federal cell must exist");
    // The Feb gain is short-term; both land in their period's short-term gain.
    assert_eq!(q1.short_term_gain_cents, pt_core::Cents(100_000), "the Feb gain is in Q1");
    assert_eq!(q3.short_term_gain_cents, pt_core::Cents(100_000), "the Jul gain is in Q3");
}

// ===========================================================================
// TAX-REPORT-002 — per (period, jurisdiction): LT/ST split, accrual, cumulative
// safe-harbor target.
// ===========================================================================

// @spec TAX-REPORT-002
#[test]
fn report_002_safe_harbor_targets_are_cumulative() {
    assert_eq!(safe_harbor_ppm(Quarter::Q1), config::Ppm(225_000), "22.5%");
    assert_eq!(safe_harbor_ppm(Quarter::Q2), config::Ppm(450_000), "45%");
    assert_eq!(safe_harbor_ppm(Quarter::Q3), config::Ppm(675_000), "67.5%");
    assert_eq!(safe_harbor_ppm(Quarter::Q4), config::Ppm(900_000), "90%");
}

// @spec TAX-REPORT-002
#[test]
fn report_002_cell_carries_lt_st_split_and_accrual_and_target() {
    // A long-term gain in Q3. The Q3 federal cell shows the LT split, the
    // computed accrual, and the Q3 safe-harbor target.
    let g = gain("s1", 1, "lotA", "AMZN", date(2023, 1, 1), date(2025, 7, 1), 100_000, None);
    let ctx = context(
        2025,
        federal(flat(0), flat(150_000), niit(0, 0), 0, BracketState::Verified),
        &[],
        0,
        None,
    );
    let cells = quarterly_report(&[g], &[], &ctx);
    let q3 = cells
        .iter()
        .find(|c| c.period == Quarter::Q3 && c.jurisdiction == Jurisdiction::Federal)
        .expect("a Q3 federal cell must exist");
    assert_eq!(q3.long_term_gain_cents, pt_core::Cents(100_000), "the gain is long-term");
    assert_eq!(q3.short_term_gain_cents, pt_core::Cents(0));
    assert_eq!(q3.accrual_cents, pt_core::Cents(15_000), "LT 15% → 15_000c accrual");
    assert_eq!(q3.safe_harbor_ppm, config::Ppm(675_000), "Q3 cumulative target 67.5%");
}

// ===========================================================================
// TAX-REPORT-003 — annual report per (jurisdiction, tax_year): accrued, moved,
// paid, outstanding (accrued − paid), shortfall (accrued − moved).
// ===========================================================================

// @spec TAX-REPORT-003
#[test]
fn report_003_annual_row_totals() {
    let key = fed_key("s1", "lotA", 2025);
    // Accrued 30_000; moved 25_000; paid 20_000.
    // outstanding = 30_000 − 20_000 = 10_000; shortfall = 30_000 − 25_000 = 5_000.
    let events = vec![
        allocate(1, key.clone(), "acct"),
        move_(2, key.clone(), 25_000, date(2025, 6, 15)),
        pay(3, Jurisdiction::Federal, 2025, Quarter::Q2, 20_000, date(2025, 6, 16), vec![key]),
    ];
    let rows = annual_report(&[one_gain()], &events, &ctx());
    let fed = rows
        .iter()
        .find(|r| r.jurisdiction == Jurisdiction::Federal && r.tax_year.0 == 2025)
        .expect("a federal 2025 annual row must exist");
    assert_eq!(fed.accrued_cents, pt_core::Cents(30_000));
    assert_eq!(fed.moved_cents, pt_core::Cents(25_000));
    assert_eq!(fed.paid_cents, pt_core::Cents(20_000));
    assert_eq!(fed.outstanding_cents, pt_core::Cents(10_000), "outstanding = accrued − paid");
    assert_eq!(fed.shortfall_cents, pt_core::Cents(5_000), "shortfall = accrued − moved");
}

// ===========================================================================
// TAX-REPORT-004 — effective rate (accrual ÷ gain) only where |gain| exceeds
// de-minimis; otherwise n/a.
// ===========================================================================

// @spec TAX-REPORT-004
#[test]
fn report_004_effective_rate_only_above_de_minimis() {
    // De-minimis $5 (500c). A material $1,000 gain @ 30% → rate = round(30_000 ×
    // 1e6 / 100_000) = 300_000 ppm.
    let ctx = context(
        2025,
        federal(flat(300_000), flat(0), niit(0, 0), 0, BracketState::Verified),
        &[],
        500,
        None,
    );
    let rows = annual_report(&[one_gain()], &[], &ctx);
    let fed = rows
        .iter()
        .find(|r| r.jurisdiction == Jurisdiction::Federal && r.tax_year.0 == 2025)
        .unwrap();
    assert_eq!(
        fed.effective_rate_ppm,
        Some(config::Ppm(300_000)),
        "effective rate (accrual ÷ gain) is shown for a material gain"
    );
}

// @spec TAX-REPORT-004
#[test]
fn report_004_effective_rate_na_below_de_minimis() {
    // De-minimis $5 (500c). A $1 gain (100c, below de-minimis) → effective rate
    // n/a (None).
    let ctx = context(
        2025,
        federal(flat(300_000), flat(0), niit(0, 0), 0, BracketState::Verified),
        &[],
        500,
        None,
    );
    let g = gain("s1", 1, "lotA", "AMZN", date(2025, 1, 2), date(2025, 6, 1), 100, None);
    let rows = annual_report(&[g], &[], &ctx);
    let fed = rows
        .iter()
        .find(|r| r.jurisdiction == Jurisdiction::Federal && r.tax_year.0 == 2025)
        .unwrap();
    assert_eq!(
        fed.effective_rate_ppm, None,
        "effective rate is n/a when |gain| does not exceed de-minimis"
    );
}

// ===========================================================================
// TAX-ACCRUAL-005 — a below-de-minimis (auto-settled) accrual is EXCLUDED from
// the outstanding balance; a material one contributes its full applied amount.
// (The flag's only behavioral manifestation is the annual report aggregation.)
// ===========================================================================

// @spec TAX-ACCRUAL-005, TAX-REPORT-005
#[test]
fn accrual_005_de_minimis_excluded_from_outstanding() {
    // De-minimis $5 (500c). A tiny $1 gain (100c) at 30% → 30c accrual, below the
    // threshold → auto-settled. It must contribute 0 to the annual outstanding,
    // not 30c. With no Move/Pay, outstanding = accrued − paid.
    let ctx = context(
        2025,
        federal(flat(300_000), flat(0), niit(0, 0), 0, BracketState::Verified),
        &[],
        500,
        None,
    );
    let tiny = gain("s1", 1, "lotA", "AMZN", date(2025, 1, 2), date(2025, 6, 1), 100, None);
    let rows = annual_report(&[tiny], &[], &ctx);
    let fed = rows
        .iter()
        .find(|r| r.jurisdiction == Jurisdiction::Federal && r.tax_year.0 == 2025)
        .expect("a federal 2025 annual row must exist");
    assert_eq!(
        fed.accrued_cents,
        pt_core::Cents(0),
        "a below-de-minimis accrual contributes 0 to accrued"
    );
    assert_eq!(
        fed.outstanding_cents,
        pt_core::Cents(0),
        "a below-de-minimis accrual is excluded from the outstanding balance"
    );
}

// @spec TAX-ACCRUAL-005
#[test]
fn accrual_005_material_accrual_contributes_full_amount_to_outstanding() {
    // Same $5 de-minimis, but a material $1,000 gain → 30_000c accrual (above the
    // threshold) contributes its FULL applied amount to the outstanding balance.
    let ctx = context(
        2025,
        federal(flat(300_000), flat(0), niit(0, 0), 0, BracketState::Verified),
        &[],
        500,
        None,
    );
    let rows = annual_report(&[one_gain()], &[], &ctx);
    let fed = rows
        .iter()
        .find(|r| r.jurisdiction == Jurisdiction::Federal && r.tax_year.0 == 2025)
        .unwrap();
    assert_eq!(fed.accrued_cents, pt_core::Cents(30_000), "a material accrual is included");
    assert_eq!(
        fed.outstanding_cents,
        pt_core::Cents(30_000),
        "a material accrual contributes its full applied amount to outstanding"
    );
}
