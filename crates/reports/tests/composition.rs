//! Portfolio Composition: REPORT-COMP-001/002/003/004.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use std::collections::BTreeMap;

use common::*;
use config::Ppm;
use ledger_core::Symbol;
use pt_core::{Cents, Date};
use reports::{compose, Composition, DegradeCause, PricedTotalState};

/// Find a composition row by its label.
fn row<'a>(rows: &'a [reports::CompositionRow], label: &str) -> &'a reports::CompositionRow {
    rows.iter()
        .find(|r| r.label == label)
        .unwrap_or_else(|| panic!("no row {label}"))
}

/// The sum of priced (non-degraded) per-symbol share_ppm.
fn priced_share_sum(comp: &Composition) -> i64 {
    comp.by_symbol
        .iter()
        .filter(|r| r.degraded.is_none())
        .filter_map(|r| r.share_ppm.map(|p| p.0))
        .sum()
}

// @spec REPORT-COMP-001
#[test]
fn composition_per_symbol_pre_and_post_tax() {
    // AMZN 3 sh @ basis $150 → basis $450, mark $200 → MV $600, unrealized $150.
    // GOOG 1 sh remaining (of 2 bought @ $100, 1 sold) → basis $100, mark $120 →
    // MV $120, unrealized $20.
    let snap = small_snapshot(&ledger_marks(&[("AMZN", 200_00), ("GOOG", 120_00)]));
    let as_of = Date(19_500);
    let ctx = ctx(2022);
    let est = estimates(&snap, as_of, &ctx);
    let marks = priced_marks(&[("AMZN", 200_00, 19_490), ("GOOG", 120_00, 19_490)]);

    let comp = compose(&snap, &marks, &est);

    let amzn = row(&comp.by_symbol, "AMZN");
    assert_eq!(amzn.market_value_cents, Some(Cents(600_00)));
    assert_eq!(amzn.total_basis_cents, Cents(450_00));
    assert_eq!(amzn.unrealized_pretax_cents, Some(Cents(150_00)));
    // Net-of-tax unrealized is present (a tax estimate exists) and is at most the
    // pre-tax figure for a gain (tax reduces the net).
    let net = amzn
        .unrealized_net_of_tax_cents
        .expect("net-of-tax present");
    assert!(
        net.0 <= 150_00 && net.0 >= 0,
        "net-of-tax in [0, pretax]: {net:?}"
    );

    let goog = row(&comp.by_symbol, "GOOG");
    assert_eq!(goog.market_value_cents, Some(Cents(120_00)));
    assert_eq!(goog.total_basis_cents, Cents(100_00));
    assert_eq!(goog.unrealized_pretax_cents, Some(Cents(20_00)));
}

// @spec REPORT-COMP-001
#[test]
fn composition_per_platform_aggregates_symbols() {
    let snap = small_snapshot(&ledger_marks(&[("AMZN", 200_00), ("GOOG", 120_00)]));
    let as_of = Date(19_500);
    let ctx = ctx(2022);
    let est = estimates(&snap, as_of, &ctx);
    let marks = priced_marks(&[("AMZN", 200_00, 19_490), ("GOOG", 120_00, 19_490)]);

    let comp = compose(&snap, &marks, &est);

    // schwab holds AMZN; fidelity holds GOOG.
    let schwab = row(&comp.by_platform, "schwab");
    assert_eq!(schwab.market_value_cents, Some(Cents(600_00)));
    assert_eq!(schwab.total_basis_cents, Cents(450_00));
    let fidelity = row(&comp.by_platform, "fidelity");
    assert_eq!(fidelity.market_value_cents, Some(Cents(120_00)));
    assert_eq!(fidelity.total_basis_cents, Cents(100_00));
}

// @spec REPORT-COMP-002
#[test]
fn priced_shares_sum_to_one_hundred_percent() {
    let snap = small_snapshot(&ledger_marks(&[("AMZN", 200_00), ("GOOG", 120_00)]));
    let as_of = Date(19_500);
    let ctx = ctx(2022);
    let est = estimates(&snap, as_of, &ctx);
    let marks = priced_marks(&[("AMZN", 200_00, 19_490), ("GOOG", 120_00, 19_490)]);

    let comp = compose(&snap, &marks, &est);

    // Shares of the priced portfolio sum to exactly 100% (1_000_000 ppm).
    assert_eq!(priced_share_sum(&comp), 1_000_000);
}

// @spec REPORT-COMP-002
#[test]
fn degraded_set_is_consistent_across_pre_and_post_tax_and_excluded_from_denominator() {
    // GOOG has NO mark this cycle → degraded. AMZN is priced.
    let snap = small_snapshot(&ledger_marks(&[("AMZN", 200_00), ("GOOG", 120_00)]));
    let as_of = Date(19_500);
    let ctx = ctx(2022);
    let est = estimates(&snap, as_of, &ctx);
    // Only AMZN has a mark; GOOG is omitted (degraded, no mark).
    let marks = priced_marks(&[("AMZN", 200_00, 19_490)]);

    let comp = compose(&snap, &marks, &est);

    let goog = row(&comp.by_symbol, "GOOG");
    // GOOG is degraded for no-mark, in BOTH views (same row): MV/unrealized/net
    // are all None — never a fabricated zero — and it carries no share.
    assert_eq!(goog.degraded, Some(DegradeCause::NoMark));
    assert_eq!(goog.market_value_cents, None);
    assert_eq!(goog.unrealized_pretax_cents, None);
    assert_eq!(goog.unrealized_net_of_tax_cents, None);
    assert_eq!(goog.share_ppm, None);

    // The priced denominator is AMZN alone, so the priced shares still sum to 100%.
    let amzn = row(&comp.by_symbol, "AMZN");
    assert_eq!(amzn.share_ppm, Some(Ppm(1_000_000)));
    assert_eq!(priced_share_sum(&comp), 1_000_000);
}

// @spec REPORT-COMP-002
#[test]
fn symbol_degraded_by_tax_estimate_is_excluded_in_both_views() {
    // Both symbols are priced (have marks), but GOOG's tax estimate is unavailable
    // → degraded in BOTH the pre- and post-tax views (the consistent degraded set),
    // and excluded from the % denominator.
    let snap = small_snapshot(&ledger_marks(&[("AMZN", 200_00), ("GOOG", 120_00)]));
    let as_of = Date(19_500);
    let ctx = ctx(2022);
    let mut est = estimates(&snap, as_of, &ctx);
    // Force GOOG's tax estimate to be unavailable (mark present, estimate None).
    let goog_sym: Symbol = "GOOG".to_string();
    let mut goog_est = est.get(&goog_sym).cloned().expect("goog est");
    goog_est.estimated_tax_cents = None;
    goog_est.effective_rate_ppm = None;
    est.insert(goog_sym.clone(), goog_est);

    let marks = priced_marks(&[("AMZN", 200_00, 19_490), ("GOOG", 120_00, 19_490)]);
    let comp = compose(&snap, &marks, &est);

    let goog = row(&comp.by_symbol, "GOOG");
    assert_eq!(goog.degraded, Some(DegradeCause::NoTaxEstimate));
    // Excluded from the denominator → no share, in both views.
    assert_eq!(goog.share_ppm, None);
    assert_eq!(goog.unrealized_net_of_tax_cents, None);
    // AMZN is now the only priced symbol → 100%.
    assert_eq!(priced_share_sum(&comp), 1_000_000);
}

// @spec REPORT-COMP-003
#[test]
fn surfaces_degraded_count_and_priced_coverage_fraction() {
    // GOOG unpriced (no mark). AMZN priced. Both have basis (basis survives
    // degradation), so the priced-coverage fraction caveats the thin "100%".
    let snap = small_snapshot(&ledger_marks(&[("AMZN", 200_00), ("GOOG", 120_00)]));
    let as_of = Date(19_500);
    let ctx = ctx(2022);
    let est = estimates(&snap, as_of, &ctx);
    let marks = priced_marks(&[("AMZN", 200_00, 19_490)]);

    let comp = compose(&snap, &marks, &est);

    // One degraded symbol (GOOG).
    assert_eq!(comp.degraded_count, 1);
    // Priced basis = AMZN $450; total basis = $450 + $100 = $550.
    // Coverage = 450/550 = 0.8181… → 818_181 ppm (round-half-to-even).
    let cov = comp.priced_coverage_ppm.expect("coverage present");
    assert_eq!(cov, Ppm(818_182));
}

// @spec REPORT-COMP-004
#[test]
fn priced_total_nonpositive_reports_shares_na_distinguishing_no_positions() {
    // No positions at all: an empty ledger → no symbols.
    let empty = small_events_empty();
    let snap = ledger_core::replay(&empty, &ledger_marks(&[]));
    let comp = compose(&snap, &BTreeMap::new(), &BTreeMap::new());
    assert_eq!(comp.priced_total_state, PricedTotalState::NoPositions);
    // No priced positions → no shares to sum.
    assert_eq!(priced_share_sum(&comp), 0);
}

// @spec REPORT-COMP-004
#[test]
fn priced_total_nonpositive_distinguishes_positions_exist_but_unpriced() {
    // Positions exist (AMZN, GOOG open) but NONE is priced (no marks at all) →
    // priced total ≤ 0, shares n/a, labelled "positions exist but unpriced",
    // distinct from "no positions".
    let snap = small_snapshot(&ledger_marks(&[]));
    let as_of = Date(19_500);
    let ctx = ctx(2022);
    let est = estimates(&snap, as_of, &ctx);
    let comp = compose(&snap, &BTreeMap::new(), &est);

    assert_eq!(
        comp.priced_total_state,
        PricedTotalState::PositionsExistButUnpriced
    );
    // Every symbol is degraded; none carries a share.
    assert!(comp.by_symbol.iter().all(|r| r.share_ppm.is_none()));
    assert_eq!(priced_share_sum(&comp), 0);
}

// @spec REPORT-COMP-004
#[test]
fn negative_basis_position_is_flagged_not_a_nonsensical_share() {
    // A position whose total basis is negative is flagged, rather than silently
    // producing a nonsensical basis share. Construct one via a fee-laden sale that
    // could not happen here, so synthesize directly: a Buy with a NEGATIVE unit
    // price is rejected by the kernel, so instead we mark a snapshot whose lot
    // basis we coerce negative via a crafted event sequence is not possible — use
    // a position-level synthetic snapshot.
    let snap = negative_basis_snapshot();
    let comp = compose(
        &snap,
        &priced_marks(&[("ACME", 50_00, 19_490)]),
        &BTreeMap::new(),
    );

    let acme = row(&comp.by_symbol, "ACME");
    assert!(
        acme.negative_basis,
        "negative-basis position must be flagged"
    );
}

// @spec REPORT-COMP-004
#[test]
fn priced_negative_basis_position_emits_market_value_share_not_a_basis_share() {
    // A FULLY-PRICED, tax-estimated (non-degraded) negative-basis position must
    // still be flagged AND carry a well-defined share — the MARKET-VALUE share, not
    // a share derived from the (negative, nonsensical) basis. With ACME the lone
    // priced symbol, its market-value share is exactly 100%, confirming the share is
    // never derived from basis. (REPORT-COMP-004)
    let snap = negative_basis_snapshot();
    let as_of = Date(19_500);
    let ctx = ctx(2022);
    // A real tax estimate (mark present, unrealized is a gain) → row is non-degraded.
    let est = estimates(&snap, as_of, &ctx);
    let marks = priced_marks(&[("ACME", 50_00, 19_490)]);

    let comp = compose(&snap, &marks, &est);

    let acme = row(&comp.by_symbol, "ACME");
    assert!(
        acme.negative_basis,
        "priced negative-basis position is still flagged"
    );
    assert_eq!(
        acme.degraded, None,
        "mark + tax estimate present → non-degraded"
    );
    // Share is the MARKET-VALUE share (well-defined), not a basis share: ACME is the
    // only priced symbol so its market-value share is exactly 100%.
    assert_eq!(acme.share_ppm, Some(Ppm(1_000_000)));
    assert_eq!(acme.market_value_cents, Some(Cents(50_00)));
    assert_eq!(priced_share_sum(&comp), 1_000_000);
}

// @spec REPORT-COMP-004
#[test]
fn platform_negative_basis_is_from_final_aggregate_not_running_sum() {
    // schwab holds two ACME lots: lot A (-$150) then lot B (+$200). A running-sum
    // check flips negative at A, but the FINAL aggregated basis (+$50) is positive,
    // so the platform must NOT be flagged negative — the flag is from the final
    // aggregate, mirroring the per-symbol test. (REPORT-COMP-004)
    let snap = platform_running_negative_then_positive_snapshot();
    let marks = priced_marks(&[("ACME", 100_00, 19_490)]);
    let comp = compose(&snap, &marks, &BTreeMap::new());

    let schwab = row(&comp.by_platform, "schwab");
    assert_eq!(
        schwab.total_basis_cents,
        Cents(50_00),
        "final aggregate basis is +$50"
    );
    assert!(
        !schwab.negative_basis,
        "a positive final aggregate must not be false-flagged negative by lot order"
    );
}

// @spec REPORT-COMP-001, REPORT-COMP-005
#[test]
fn net_of_tax_reconciles_across_symbol_and_platform_views() {
    // Net-of-tax uses ONE definition everywhere: the per-platform sub-totals
    // reconcile EXACTLY to the per-symbol exact net-of-tax for the same holding
    // (no rounded-rate re-derivation). AMZN lives entirely on schwab and GOOG
    // entirely on fidelity, so each platform's net-of-tax must equal its symbol's.
    let snap = small_snapshot(&ledger_marks(&[("AMZN", 200_00), ("GOOG", 120_00)]));
    let as_of = Date(19_500);
    let ctx = ctx(2022);
    let est = estimates(&snap, as_of, &ctx);
    let marks = priced_marks(&[("AMZN", 200_00, 19_490), ("GOOG", 120_00, 19_490)]);

    let comp = compose(&snap, &marks, &est);

    let amzn = row(&comp.by_symbol, "AMZN");
    let schwab = row(&comp.by_platform, "schwab");
    assert_eq!(
        schwab.unrealized_net_of_tax_cents, amzn.unrealized_net_of_tax_cents,
        "platform net-of-tax reconciles to the per-symbol exact figure"
    );
    let goog = row(&comp.by_symbol, "GOOG");
    let fidelity = row(&comp.by_platform, "fidelity");
    assert_eq!(
        fidelity.unrealized_net_of_tax_cents, goog.unrealized_net_of_tax_cents,
        "platform net-of-tax reconciles to the per-symbol exact figure"
    );
}

// @spec REPORT-COMP-005
#[test]
fn net_of_tax_distributes_exact_tax_across_lots_and_platforms_no_penny_drift() {
    // One symbol (ZZZ) held as three lots across TWO platforms (schwab x2,
    // fidelity x1). The symbol's EXACT estimated tax is distributed over its
    // sub-positions by pre-tax weight (largest-remainder), so the per-platform
    // net-of-tax sub-totals reconcile EXACTLY to the symbol's estimated tax with no
    // penny drift. The tax ($101.01) is deliberately indivisible by the pretax
    // weights, forcing a residual unit. (REPORT-COMP-005)
    let snap = multi_lot_multi_platform_snapshot();
    // Mark $200/sh: total MV = 6 sh x $200 = $1200, total basis $1000, pretax $200.
    let marks = priced_marks(&[("ZZZ", 200_00, 19_490)]);
    let mut est = BTreeMap::new();
    est.insert("ZZZ".to_string(), fixed_estimate("ZZZ", 200_00, 101_01));

    let comp = compose(&snap, &marks, &est);

    // The per-symbol net-of-tax is the authority: pretax $200 − tax $101.01.
    let zzz = row(&comp.by_symbol, "ZZZ");
    let symbol_net = zzz
        .unrealized_net_of_tax_cents
        .expect("priced → net present");
    assert_eq!(symbol_net, Cents(200_00 - 101_01));

    // The per-platform net-of-tax sub-totals must sum EXACTLY to the per-symbol net
    // (no penny drift from the apportionment of the exact tax across lots/platforms).
    let plat_net_sum: i64 = comp
        .by_platform
        .iter()
        .map(|r| r.unrealized_net_of_tax_cents.expect("priced platform").0)
        .sum();
    assert_eq!(
        plat_net_sum, symbol_net.0,
        "per-platform net-of-tax reconciles exactly to the symbol's net (no penny drift)"
    );

    // Equivalently, the distributed tax (Σ platform pretax − Σ platform net) equals
    // the symbol's exact estimated tax — the apportionment loses no cent.
    let plat_pretax_sum: i64 = comp
        .by_platform
        .iter()
        .map(|r| r.unrealized_pretax_cents.expect("priced platform").0)
        .sum();
    assert_eq!(
        plat_pretax_sum - plat_net_sum,
        101_01,
        "distributed tax sums to the exact estimate"
    );
}

// @spec REPORT-COMP-004
#[test]
fn fully_closed_out_book_reports_no_positions() {
    // A snapshot whose positions map is NON-empty but every position is fully closed
    // (total_qty == 0) has no live allocation, so it reports NoPositions — not
    // PositionsExistButUnpriced (which is for OPEN-but-unpriced positions). This
    // pins the boundary choice: "no positions" means no OPEN positions.
    // GOOG: 2 bought @ $100, both sold @ $130 → fully closed.
    let events = vec![
        buy(1, 19_000, "lot-goog", "GOOG", 2_000_000, 100_00, "fidelity"),
        sell(
            2,
            19_100,
            "sale-1",
            "GOOG",
            2_000_000,
            130_00,
            "fidelity",
            Some("NJ"),
        ),
    ];
    let snap = replay(&events, &ledger_marks(&[("GOOG", 130_00)]));
    assert!(
        !snap.positions.is_empty(),
        "the position map is non-empty (closed lot present)"
    );

    let comp = compose(
        &snap,
        &priced_marks(&[("GOOG", 130_00, 19_490)]),
        &BTreeMap::new(),
    );
    assert_eq!(comp.priced_total_state, PricedTotalState::NoPositions);
    assert_eq!(priced_share_sum(&comp), 0);
}

// @spec REPORT-COMP-006
#[test]
fn composition_rows_carry_unrealized_pct_of_basis() {
    // AMZN: basis $450, unrealized $150 → 150/450 = 33.3333…% → 333,333 ppm
    // (half-to-even). GOOG: basis $100, unrealized $20 → exactly 200,000 ppm.
    let snap = small_snapshot(&ledger_marks(&[("AMZN", 200_00), ("GOOG", 120_00)]));
    let est = estimates(&snap, Date(19_500), &ctx(2022));
    let marks = priced_marks(&[("AMZN", 200_00, 19_490), ("GOOG", 120_00, 19_490)]);
    let comp = compose(&snap, &marks, &est);

    assert_eq!(
        row(&comp.by_symbol, "AMZN").unrealized_pct_of_basis_ppm,
        Some(Ppm(333_333)),
        "the unr-% of basis rides the symbol row"
    );
    assert_eq!(
        row(&comp.by_symbol, "GOOG").unrealized_pct_of_basis_ppm,
        Some(Ppm(200_000))
    );
    // Platform rows carry it too: schwab holds only AMZN, so the figures match.
    assert_eq!(
        row(&comp.by_platform, "schwab").unrealized_pct_of_basis_ppm,
        Some(Ppm(333_333)),
        "the platform aggregate carries its own %-of-basis"
    );
}

// @spec REPORT-COMP-006
#[test]
fn pct_of_basis_is_na_when_degraded_or_basis_nonpositive() {
    // GOOG unpriced (degraded) → no %-of-basis, while its basis still survives.
    let snap = small_snapshot(&ledger_marks(&[("AMZN", 200_00)]));
    let est = estimates(&snap, Date(19_500), &ctx(2022));
    let marks = priced_marks(&[("AMZN", 200_00, 19_490)]); // GOOG degraded
    let comp = compose(&snap, &marks, &est);
    let goog = row(&comp.by_symbol, "GOOG");
    assert!(goog.degraded.is_some());
    assert_eq!(
        goog.unrealized_pct_of_basis_ppm, None,
        "degraded → n/a, never fabricated"
    );
    assert_eq!(
        goog.total_basis_cents,
        Cents(100_00),
        "basis survives degradation"
    );

    // A priced zero-basis position (e.g. a zero-cost acquisition) → n/a: a ≤ 0
    // basis has no meaningful %-of-basis (never a nonsensical percentage).
    let zero = replay(
        &[buy(1, 19_000, "lot-z", "ZERO", 1_000_000, 0, "schwab")],
        &ledger_marks(&[("ZERO", 100_00)]),
    );
    let est_z = estimates(&zero, Date(19_500), &ctx(2022));
    let marks_z = priced_marks(&[("ZERO", 100_00, 19_490)]);
    let comp_z = compose(&zero, &marks_z, &est_z);
    let z = row(&comp_z.by_symbol, "ZERO");
    assert!(
        z.degraded.is_none(),
        "the zero-basis row is priced, not degraded"
    );
    assert_eq!(z.total_basis_cents, Cents(0));
    assert_eq!(z.unrealized_pct_of_basis_ppm, None, "basis ≤ 0 → n/a");
}
