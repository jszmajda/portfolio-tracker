//! Red (failing) tests for the `tax` CALCULATION specs (TAX-CALC-001..012).
//! TDD: written BEFORE the kernel/orchestration is implemented. The scaffold
//! compiles (signatures complete, bodies `todo!()`/`unimplemented!()`), so these
//! tests compile and FAIL AT RUNTIME (panic) — the RED phase.
//!
//! Each test carries a `// @spec TAX-CALC-NNN` comment on the test that directly
//! exercises that spec. Amounts are exact integer `Cents`; no float appears.

#![allow(clippy::inconsistent_digit_grouping)]

mod common;
use common::*;

use config::{BracketState, Jurisdiction};
use tax::{classify_term, compute_accruals, resolve_state, tax_year_of, Term};

// ===========================================================================
// TAX-CALC-001 — long-term iff sale_date strictly after the first anniversary
// of acquire_date; Feb-29 acquisition's anniversary is March 1.
// ===========================================================================

// @spec TAX-CALC-001
#[test]
fn calc_001_one_year_exactly_is_short_term() {
    // Acquire 2024-03-15; the first anniversary is 2025-03-15. A sale ON the
    // anniversary is NOT strictly after → short-term.
    assert_eq!(
        classify_term(date(2024, 3, 15), date(2025, 3, 15)),
        Term::ShortTerm,
        "a sale exactly on the first anniversary is short-term (not strictly after)"
    );
    // One day later IS strictly after → long-term.
    assert_eq!(
        classify_term(date(2024, 3, 15), date(2025, 3, 16)),
        Term::LongTerm,
        "a sale one day after the anniversary is long-term"
    );
    // The day before the anniversary is short-term.
    assert_eq!(
        classify_term(date(2024, 3, 15), date(2025, 3, 14)),
        Term::ShortTerm,
    );
}

// @spec TAX-CALC-001
#[test]
fn calc_001_feb29_anniversary_is_march_1() {
    // Acquire on a leap day 2024-02-29. The anniversary is the next existing day
    // March 1, 2025. A sale on 2025-03-01 is NOT strictly after → short-term.
    assert_eq!(
        classify_term(date(2024, 2, 29), date(2025, 3, 1)),
        Term::ShortTerm,
        "Feb-29 acquisition's anniversary is Mar-1; a sale on Mar-1 is short-term"
    );
    // A sale on 2025-03-02 IS strictly after → long-term.
    assert_eq!(
        classify_term(date(2024, 2, 29), date(2025, 3, 2)),
        Term::LongTerm,
        "a sale after the Mar-1 anniversary of a Feb-29 buy is long-term"
    );
    // A sale on the (non-existent) "anniversary" 2025-02-28 is before Mar-1 →
    // short-term.
    assert_eq!(
        classify_term(date(2024, 2, 29), date(2025, 2, 28)),
        Term::ShortTerm,
    );
}

// @spec TAX-CALC-001
#[test]
fn calc_001_leap_year_spanning_holding_is_calendar_exact() {
    // Acquire 2023-06-01; the first anniversary is 2024-06-01 (a leap year in
    // between). A sale on 2024-06-01 is short-term; 2024-06-02 is long-term.
    assert_eq!(
        classify_term(date(2023, 6, 1), date(2024, 6, 1)),
        Term::ShortTerm,
    );
    assert_eq!(
        classify_term(date(2023, 6, 1), date(2024, 6, 2)),
        Term::LongTerm,
    );
}

// ===========================================================================
// TAX-CALC-002 — tax_year = calendar year of sale_date.
// ===========================================================================

// @spec TAX-CALC-002
#[test]
fn calc_002_tax_year_is_calendar_year_of_sale_date() {
    assert_eq!(tax_year_of(date(2025, 1, 1)).0, 2025);
    assert_eq!(tax_year_of(date(2025, 12, 31)).0, 2025);
    assert_eq!(tax_year_of(date(2024, 12, 31)).0, 2024);
    assert_eq!(tax_year_of(date(2026, 6, 7)).0, 2026);
}

// ===========================================================================
// TAX-CALC-003 — federal: ST stacks on ordinary at fed ordinary brackets, LT
// stacks above ordinary at preferential LT brackets, NIIT adds on gains above
// the MAGI threshold.
// ===========================================================================

// @spec TAX-CALC-003
#[test]
fn calc_003_short_term_taxed_at_federal_ordinary_rate() {
    // A single short-term gain of $1,000 (100_000 cents). Federal flat ordinary
    // 30%, flat LT 15%, no NIIT, zero income. A held-< 1yr gain is short-term,
    // so it stacks at the ORDINARY 30% → accrual = 30_000 cents.
    let ctx = context(
        2025,
        federal(
            flat(300_000),
            flat(150_000),
            niit(0, 0),
            0,
            BracketState::Verified,
        ),
        &[],
        0,
        None,
    );
    let g = gain(
        "s1",
        1,
        "lotA",
        "AMZN",
        date(2025, 1, 10),
        date(2025, 6, 1),
        100_000,
        None,
    );
    let accruals = compute_accruals(&[g], &[], &ctx);

    let fed = find_accrual(&accruals, "s1", "lotA", &Jurisdiction::Federal)
        .expect("federal accrual must exist");
    assert_eq!(fed.term, Term::ShortTerm);
    assert_eq!(
        fed.applied_cents,
        Some(pt_core::Cents(30_000)),
        "short-term gain taxed at the 30% federal ordinary rate"
    );
}

// @spec TAX-CALC-003
#[test]
fn calc_003_long_term_taxed_at_preferential_rate() {
    // A single long-term gain of $1,000. Same context: flat ordinary 30%, flat
    // LT 15%. Held > 1yr → long-term → preferential 15% → accrual = 15_000 cents.
    let ctx = context(
        2025,
        federal(
            flat(300_000),
            flat(150_000),
            niit(0, 0),
            0,
            BracketState::Verified,
        ),
        &[],
        0,
        None,
    );
    let g = gain(
        "s1",
        1,
        "lotA",
        "AMZN",
        date(2023, 1, 10),
        date(2025, 6, 1),
        100_000,
        None,
    );
    let accruals = compute_accruals(&[g], &[], &ctx);

    let fed = find_accrual(&accruals, "s1", "lotA", &Jurisdiction::Federal)
        .expect("federal accrual must exist");
    assert_eq!(fed.term, Term::LongTerm);
    assert_eq!(
        fed.applied_cents,
        Some(pt_core::Cents(15_000)),
        "long-term gain taxed at the 15% preferential rate"
    );
}

// @spec TAX-CALC-003
#[test]
fn calc_003_niit_adds_on_gains_above_magi_threshold() {
    // NIIT 3.8% above a MAGI threshold. Income $0, threshold $500 (50_000c).
    // A long-term gain of $1,000 (100_000c): the first $500 of the cumulative
    // investment-income base is below the threshold, the next $500 is above →
    // NIIT applies to $500 = 50_000c × 3.8% = 1_900c. Plus LT 15% on the whole
    // $1,000 = 15_000c. Federal accrual = 15_000 + 1_900 = 16_900c.
    let ctx = context(
        2025,
        federal(
            flat(0),
            flat(150_000),
            niit(38_000, 50_000),
            0,
            BracketState::Verified,
        ),
        &[],
        0,
        None,
    );
    let g = gain(
        "s1",
        1,
        "lotA",
        "AMZN",
        date(2023, 1, 10),
        date(2025, 6, 1),
        100_000,
        None,
    );
    let accruals = compute_accruals(&[g], &[], &ctx);

    let fed = find_accrual(&accruals, "s1", "lotA", &Jurisdiction::Federal)
        .expect("federal accrual must exist");
    assert_eq!(
        fed.applied_cents,
        Some(pt_core::Cents(16_900)),
        "LT 15% on $1000 + NIIT 3.8% on the $500 above the MAGI threshold"
    );
}

// @spec TAX-CALC-003, TAX-CALC-014
#[test]
fn calc_003_long_term_stacks_above_income_plus_short_term() {
    // The federal preferential LT band is pushed up by ordinary income AND the
    // year's short-term gains (ST is taxed as ordinary and forms part of the LT
    // stacking base). Multi-band LT brackets: 0–$1,000 @ 0%, above $1,000 @ 20%.
    // Income 0. A chronologically-FIRST short-term gain of $1,000 fills the
    // ordinary base; a later long-term gain of $1,000 then stacks ABOVE $1,000 of
    // ST, landing entirely in the 20% LT band → LT accrual 20_000c (not 0c, which
    // is what income-only stacking would give).
    let ctx = context(
        2025,
        federal(
            flat(100_000),                              // ordinary flat 10% (ST taxed here)
            bracket_set(&[(0, 0), (100_000, 200_000)]), // LT: 0% to $1k, 20% above
            niit(0, 0),
            0,
            BracketState::Verified,
        ),
        &[],
        0,
        None,
    );
    let st = gain(
        "sST",
        1,
        "lotST",
        "AMZN",
        date(2025, 1, 2),
        date(2025, 3, 1),
        100_000,
        None,
    );
    let lt = gain(
        "sLT",
        2,
        "lotLT",
        "AMZN",
        date(2023, 1, 1),
        date(2025, 6, 1),
        100_000,
        None,
    );
    let accruals = compute_accruals(&[st, lt], &[], &ctx);

    let lt_acc = find_accrual(&accruals, "sLT", "lotLT", &Jurisdiction::Federal).unwrap();
    assert_eq!(lt_acc.term, Term::LongTerm);
    assert_eq!(
        lt_acc.applied_cents,
        Some(pt_core::Cents(20_000)),
        "LT stacks above income + ST, landing in the 20% LT band → 20_000c"
    );
}

// @spec TAX-CALC-003
#[test]
fn calc_003_niit_attributed_marginally_across_two_gains() {
    // NIIT 3.8% above a MAGI threshold of $1,000 (100_000c), income 0, LT flat 0%.
    // Two long-term gains of $1,000 each, chronologically ordered. The FIRST gain
    // sits entirely below the threshold (cumulative investment income 0→$1,000) →
    // its NIIT increment is 0. The SECOND gain crosses ($1,000→$2,000, all above) →
    // NIIT 3.8% × $1,000 = 3_800c. NIIT is attributed to the gain that
    // chronologically crosses the threshold.
    let ctx = context(
        2025,
        federal(
            flat(0),
            flat(0),
            niit(38_000, 100_000),
            0,
            BracketState::Verified,
        ),
        &[],
        0,
        None,
    );
    let first = gain(
        "sA",
        1,
        "lotA",
        "AMZN",
        date(2023, 1, 1),
        date(2025, 3, 1),
        100_000,
        None,
    );
    let second = gain(
        "sB",
        2,
        "lotB",
        "AMZN",
        date(2023, 1, 1),
        date(2025, 6, 1),
        100_000,
        None,
    );
    let accruals = compute_accruals(&[first, second], &[], &ctx);

    let a = find_accrual(&accruals, "sA", "lotA", &Jurisdiction::Federal).unwrap();
    let b = find_accrual(&accruals, "sB", "lotB", &Jurisdiction::Federal).unwrap();
    assert_eq!(
        a.applied_cents,
        Some(pt_core::Cents(0)),
        "the first gain is below the MAGI threshold → no NIIT increment"
    );
    assert_eq!(
        b.applied_cents,
        Some(pt_core::Cents(3_800)),
        "the second gain crosses the threshold → NIIT 3.8% × $1,000 = 3_800c"
    );
}

// @spec TAX-CALC-003, TAX-CALC-013
#[test]
fn calc_003_niit_lesser_of_mechanic_with_ordinary_income_in_magi() {
    // NIIT is the lesser of (net investment gain) and (MAGI-over-threshold), with
    // ordinary income counted in the MAGI base. Income $1,000 (100_000c), MAGI
    // threshold $1,500 (150_000c), NIIT 3.8%, LT flat 0%. A long-term gain of
    // $1,000: MAGI = income + gain = $2,000, over-threshold = $500 (50_000c); net
    // investment gain = $1,000. NIIT base = min($500, $1,000) = $500 → 3.8% ×
    // 50_000 = 1_900c. This exercises BOTH the income-in-MAGI path and the min()
    // cap (the over-threshold excess, not the full gain).
    let ctx = context(
        2025,
        federal(
            flat(0),
            flat(0),
            niit(38_000, 150_000),
            100_000,
            BracketState::Verified,
        ),
        &[],
        0,
        None,
    );
    let g = gain(
        "s1",
        1,
        "lotA",
        "AMZN",
        date(2023, 1, 1),
        date(2025, 6, 1),
        100_000,
        None,
    );
    let accruals = compute_accruals(&[g], &[], &ctx);
    let fed = find_accrual(&accruals, "s1", "lotA", &Jurisdiction::Federal).unwrap();
    assert_eq!(
        fed.applied_cents,
        Some(pt_core::Cents(1_900)),
        "NIIT = 3.8% × min(MAGI-over-threshold $500, gain $1,000) with income in the MAGI base"
    );
}

// ===========================================================================
// TAX-CALC-004 — state taxes all gains (LT and ST alike) at ordinary brackets,
// stacked on state income.
// ===========================================================================

// @spec TAX-CALC-004
#[test]
fn calc_004_state_taxes_long_term_gain_as_ordinary() {
    // DC flat ordinary 10%. A LONG-TERM federal gain is still taxed by the state
    // at its ORDINARY 10% (states tax cap gains as ordinary). Gain $1,000 →
    // state accrual = 10_000 cents.
    let ctx = context(
        2025,
        federal(flat(0), flat(0), niit(0, 0), 0, BracketState::Verified),
        &[state("DC", flat(100_000), 0, BracketState::Verified)],
        0,
        None,
    );
    let g = gain(
        "s1",
        1,
        "lotA",
        "AMZN",
        date(2023, 1, 10),
        date(2025, 6, 1),
        100_000,
        Some("DC"),
    );
    let accruals = compute_accruals(&[g], &[], &ctx);

    let st = find_accrual(
        &accruals,
        "s1",
        "lotA",
        &Jurisdiction::State("DC".to_string()),
    )
    .expect("DC state accrual must exist");
    assert_eq!(st.term, Term::LongTerm, "the gain is long-term federally");
    assert_eq!(
        st.applied_cents,
        Some(pt_core::Cents(10_000)),
        "the state taxes the long-term gain at its 10% ordinary rate"
    );
}

// ===========================================================================
// TAX-CALC-005 — accrual is the chronological marginal increment.
// ===========================================================================

// @spec TAX-CALC-005
#[test]
fn calc_005_accrual_is_marginal_increment_in_chronological_order() {
    // Two short-term gains in one year, federal ordinary progressive:
    //   0–$1,000 @ 10%, above $1,000 @ 30%. Income $0.
    // Gain A: $1,000 (sale 2025-03-01). Gain B: $1,000 (sale 2025-06-01).
    // Chronologically A is first: A stacks 0..1000 → 10% → 10_000c.
    // B stacks 1000..2000 → 30% → 30_000c (its marginal increment, NOT 10%).
    let ctx = context(
        2025,
        federal(
            bracket_set(&[(0, 100_000), (100_000, 300_000)]),
            flat(0),
            niit(0, 0),
            0,
            BracketState::Verified,
        ),
        &[],
        0,
        None,
    );
    // Acquire dates within the same year keep both gains short-term (taxed at the
    // ordinary progressive brackets).
    let a = gain(
        "sA",
        1,
        "lotA",
        "AMZN",
        date(2025, 1, 2),
        date(2025, 3, 1),
        100_000,
        None,
    );
    let b = gain(
        "sB",
        2,
        "lotB",
        "AMZN",
        date(2025, 1, 2),
        date(2025, 6, 1),
        100_000,
        None,
    );
    let accruals = compute_accruals(&[a, b], &[], &ctx);

    let fa = find_accrual(&accruals, "sA", "lotA", &Jurisdiction::Federal).unwrap();
    let fb = find_accrual(&accruals, "sB", "lotB", &Jurisdiction::Federal).unwrap();
    assert_eq!(fa.term, Term::ShortTerm);
    assert_eq!(fb.term, Term::ShortTerm);
    assert_eq!(
        fa.applied_cents,
        Some(pt_core::Cents(10_000)),
        "the chronologically-first gain lands in the 10% band"
    );
    assert_eq!(
        fb.applied_cents,
        Some(pt_core::Cents(30_000)),
        "the second gain's marginal increment lands in the 30% band"
    );
}

// @spec TAX-CALC-005
#[test]
fn calc_005_increment_order_follows_sale_date_not_seq() {
    // Same brackets, but the LATER-DATED gain has the LOWER seq. Chronological
    // stacking is by (sale_date, sale_seq), NOT raw seq: the earlier sale_date
    // gain stacks first regardless of seq.
    let ctx = context(
        2025,
        federal(
            bracket_set(&[(0, 100_000), (100_000, 300_000)]),
            flat(0),
            niit(0, 0),
            0,
            BracketState::Verified,
        ),
        &[],
        0,
        None,
    );
    // seq 1 is dated LATER (June); seq 2 is dated EARLIER (March). Both short-term.
    let late = gain(
        "sLate",
        1,
        "lotL",
        "AMZN",
        date(2025, 1, 2),
        date(2025, 6, 1),
        100_000,
        None,
    );
    let early = gain(
        "sEarly",
        2,
        "lotE",
        "AMZN",
        date(2025, 1, 2),
        date(2025, 3, 1),
        100_000,
        None,
    );
    let accruals = compute_accruals(&[late, early], &[], &ctx);

    let early_acc = find_accrual(&accruals, "sEarly", "lotE", &Jurisdiction::Federal).unwrap();
    let late_acc = find_accrual(&accruals, "sLate", "lotL", &Jurisdiction::Federal).unwrap();
    assert_eq!(
        early_acc.applied_cents,
        Some(pt_core::Cents(10_000)),
        "the earlier sale_date gain stacks first (10% band), regardless of seq"
    );
    assert_eq!(
        late_acc.applied_cents,
        Some(pt_core::Cents(30_000)),
        "the later sale_date gain takes the 30% marginal increment"
    );
}

// ===========================================================================
// TAX-CALC-006 — gain args clamped at zero; a within-year net loss contributes
// zero gain-tax.
// ===========================================================================

// @spec TAX-CALC-006
#[test]
fn calc_006_within_year_loss_then_gain_nets_to_zero_clamp() {
    // Chronologically: a $1,000 LOSS, then a $400 gain. The cumulative ST base
    // after the loss is max(0, -1000) = 0; after the gain max(0, -600) = 0. The
    // gain's marginal increment is T(0) − T(0) = 0 (the year is still net-negative
    // so far). Flat ordinary 30%.
    let ctx = context(
        2025,
        federal(
            flat(300_000),
            flat(0),
            niit(0, 0),
            0,
            BracketState::Verified,
        ),
        &[],
        0,
        None,
    );
    let loss = gain(
        "sL",
        1,
        "lotL",
        "AMZN",
        date(2025, 1, 2),
        date(2025, 3, 1),
        -100_000,
        None,
    );
    let g = gain(
        "sG",
        2,
        "lotG",
        "AMZN",
        date(2025, 1, 2),
        date(2025, 6, 1),
        40_000,
        None,
    );
    let accruals = compute_accruals(&[loss, g], &[], &ctx);

    let loss_acc = find_accrual(&accruals, "sL", "lotL", &Jurisdiction::Federal).unwrap();
    let gain_acc = find_accrual(&accruals, "sG", "lotG", &Jurisdiction::Federal).unwrap();
    assert_eq!(
        loss_acc.applied_cents,
        Some(pt_core::Cents(0)),
        "a loss yields a non-positive increment clamped through max(0,·): 0 here"
    );
    assert_eq!(
        gain_acc.applied_cents,
        Some(pt_core::Cents(0)),
        "a gain that only restores a within-year loss to ≤0 contributes zero gain-tax"
    );
}

// @spec TAX-CALC-006
#[test]
fn calc_006_loss_recovery_above_zero_is_taxed() {
    // A $1,000 loss then a $1,500 gain: cumulative after gain = max(0, +500) =
    // 500. The increment is T(500) − T(0) = 30% × $5 = 15_000c. Only the part
    // that lifts the year above zero is taxed.
    let ctx = context(
        2025,
        federal(
            flat(300_000),
            flat(0),
            niit(0, 0),
            0,
            BracketState::Verified,
        ),
        &[],
        0,
        None,
    );
    let loss = gain(
        "sL",
        1,
        "lotL",
        "AMZN",
        date(2025, 1, 2),
        date(2025, 3, 1),
        -100_000,
        None,
    );
    let g = gain(
        "sG",
        2,
        "lotG",
        "AMZN",
        date(2025, 1, 2),
        date(2025, 6, 1),
        150_000,
        None,
    );
    let accruals = compute_accruals(&[loss, g], &[], &ctx);

    let gain_acc = find_accrual(&accruals, "sG", "lotG", &Jurisdiction::Federal).unwrap();
    assert_eq!(
        gain_acc.applied_cents,
        Some(pt_core::Cents(15_000)),
        "only the $500 lifting the year above zero is taxed (30% → 15_000c)"
    );
}

// ===========================================================================
// TAX-CALC-007 — inserting an earlier-dated gain re-derives later gains.
// ===========================================================================

// @spec TAX-CALC-007
#[test]
fn calc_007_inserting_earlier_gain_reprices_later_gain() {
    // Brackets 0–$1,000 @ 10%, above @ 30%. With ONLY gain B ($1,000) it lands in
    // the 10% band → 10_000c. Insert an earlier-dated gain A ($1,000): now A takes
    // 10% and B is RE-DERIVED into the 30% band → 30_000c.
    let ctx = context(
        2025,
        federal(
            bracket_set(&[(0, 100_000), (100_000, 300_000)]),
            flat(0),
            niit(0, 0),
            0,
            BracketState::Verified,
        ),
        &[],
        0,
        None,
    );
    let b = gain(
        "sB",
        1,
        "lotB",
        "AMZN",
        date(2025, 1, 2),
        date(2025, 6, 1),
        100_000,
        None,
    );

    // B alone: 10% band.
    let only_b = compute_accruals(&[b.clone()], &[], &ctx);
    let b_alone = find_accrual(&only_b, "sB", "lotB", &Jurisdiction::Federal).unwrap();
    assert_eq!(b_alone.applied_cents, Some(pt_core::Cents(10_000)));

    // Insert earlier-dated A (March, before B's June). Both short-term.
    let a = gain(
        "sA",
        2,
        "lotA",
        "AMZN",
        date(2025, 1, 2),
        date(2025, 3, 1),
        100_000,
        None,
    );
    let both = compute_accruals(&[b, a], &[], &ctx);
    let b_after = find_accrual(&both, "sB", "lotB", &Jurisdiction::Federal).unwrap();
    assert_eq!(
        b_after.applied_cents,
        Some(pt_core::Cents(30_000)),
        "inserting an earlier-dated gain re-derives B into the 30% marginal band"
    );
}

// ===========================================================================
// TAX-CALC-008 — AmountOverride substitutes an absolute applied amount; both the
// derived and the applied are retained.
// ===========================================================================

// @spec TAX-CALC-008
#[test]
fn calc_008_override_substitutes_applied_amount_retaining_derived() {
    // Federal flat 30% on a $1,000 short-term gain → derived 30_000c. An override
    // to 12_345c substitutes the applied amount; the derived value is retained.
    let ctx = context(
        2025,
        federal(
            flat(300_000),
            flat(0),
            niit(0, 0),
            0,
            BracketState::Verified,
        ),
        &[],
        0,
        None,
    );
    let g = gain(
        "s1",
        1,
        "lotA",
        "AMZN",
        date(2025, 1, 2),
        date(2025, 6, 1),
        100_000,
        None,
    );
    let ev = override_(1, fed_key("s1", "lotA", 2025), 12_345, "manual");
    let accruals = compute_accruals(&[g], &[ev], &ctx);

    let fed = find_accrual(&accruals, "s1", "lotA", &Jurisdiction::Federal).unwrap();
    assert_eq!(
        fed.derived_cents,
        Some(pt_core::Cents(30_000)),
        "the derived increment is retained"
    );
    assert_eq!(
        fed.override_cents,
        Some(pt_core::Cents(12_345)),
        "the override amount is retained"
    );
    assert_eq!(
        fed.applied_cents,
        Some(pt_core::Cents(12_345)),
        "the applied amount is the override, not the derived increment"
    );
}

// ===========================================================================
// TAX-CALC-009 — unrealized estimate + effective rate in ppm.
// ===========================================================================

// @spec TAX-CALC-009
#[test]
fn calc_009_unrealized_estimate_and_effective_rate_in_ppm() {
    // One open lot, held > 1yr as of today (2026-06-07), unrealized pretax
    // $1,000 (100_000c). Federal flat LT 15%, no NIIT, no state, zero YTD. Sold
    // today it is long-term → estimated tax = 15% × $1,000 = 15_000c. Effective
    // rate = round(15_000 × 1e6 / 100_000) = 150_000 ppm (15%).
    let ctx = context(
        2026,
        federal(
            flat(0),
            flat(150_000),
            niit(0, 0),
            0,
            BracketState::Verified,
        ),
        &[],
        0,
        None,
    );
    let lot = open_lot(
        "lotA",
        "AMZN",
        date(2023, 1, 1),
        10 * SHARE,
        50_000,
        Some(100_000),
    );
    let est = tax::unrealized_estimate(
        &symbol_str("AMZN"),
        &[lot],
        date(2026, 6, 7),
        &None,
        pt_core::Cents(0),
        pt_core::Cents(0),
        &ctx,
    );
    assert_eq!(est.unrealized_pretax_cents, pt_core::Cents(100_000));
    assert_eq!(
        est.estimated_tax_cents,
        Some(pt_core::Cents(15_000)),
        "long-term-as-of-today unrealized taxed at 15%"
    );
    assert_eq!(
        est.effective_rate_ppm,
        Some(config::Ppm(150_000)),
        "effective rate = round(15_000 × 1e6 / 100_000) = 150_000 ppm"
    );
}

// @spec TAX-CALC-009
#[test]
fn calc_009_effective_rate_is_zero_when_unrealized_not_positive() {
    // An open lot underwater: unrealized pretax = −$100 (-10_000c). The effective
    // rate is 0 when unrealized ≤ 0 (and the estimated tax is 0, no gain to tax).
    let ctx = context(
        2026,
        federal(
            flat(0),
            flat(150_000),
            niit(0, 0),
            0,
            BracketState::Verified,
        ),
        &[],
        0,
        None,
    );
    let lot = open_lot(
        "lotA",
        "AMZN",
        date(2023, 1, 1),
        10 * SHARE,
        110_000,
        Some(-10_000),
    );
    let est = tax::unrealized_estimate(
        &symbol_str("AMZN"),
        &[lot],
        date(2026, 6, 7),
        &None,
        pt_core::Cents(0),
        pt_core::Cents(0),
        &ctx,
    );
    assert_eq!(
        est.effective_rate_ppm,
        Some(config::Ppm(0)),
        "effective rate is 0 when the position is not in the money"
    );
}

// @spec TAX-CALC-009, TAX-CALC-015
#[test]
fn calc_009_mixed_sign_lots_keep_effective_rate_at_most_100_percent() {
    // A position with MIXED-SIGN lots: a SHORT-TERM loss lot (−$900) alongside a
    // LONG-TERM gain lot (+$1,000). Net unrealized pretax = +$100 (10_000c). With
    // the LT regime taxed flat 50%, a naive per-regime stack would tax the LT gain
    // gross (50_000c) while the ST loss only shrinks the denominator → a 500%
    // effective rate. The estimate must instead stay bounded: estimated_tax ≤ net
    // pretax and effective_rate_ppm ≤ 1_000_000 (100%).
    let ctx = context(
        2026,
        federal(
            flat(0),
            flat(500_000),
            niit(0, 0),
            0,
            BracketState::Verified,
        ),
        &[],
        0,
        None,
    );
    // ST lot: acquired recently (short-term as of today), unrealized −90_000c.
    let st_loss = open_lot(
        "lotST",
        "AMZN",
        date(2026, 1, 2),
        10 * SHARE,
        200_000,
        Some(-90_000),
    );
    // LT lot: acquired > 1yr ago (long-term as of today), unrealized +100_000c.
    let lt_gain = open_lot(
        "lotLT",
        "AMZN",
        date(2023, 1, 1),
        10 * SHARE,
        50_000,
        Some(100_000),
    );
    let est = tax::unrealized_estimate(
        &symbol_str("AMZN"),
        &[st_loss, lt_gain],
        date(2026, 6, 7),
        &None,
        pt_core::Cents(0),
        pt_core::Cents(0),
        &ctx,
    );
    assert_eq!(
        est.unrealized_pretax_cents,
        pt_core::Cents(10_000),
        "net unrealized pretax = −90_000 + 100_000 = 10_000"
    );
    let tax_cents = est.estimated_tax_cents.expect("estimate is available").0;
    assert!(
        (0..=10_000).contains(&tax_cents),
        "estimated tax stays in [0, net pretax]: got {tax_cents}"
    );
    let rate = est.effective_rate_ppm.expect("rate is available").0;
    assert!(
        (0..=1_000_000).contains(&rate),
        "effective rate stays in [0, 100%]: got {rate} ppm"
    );
}

// @spec TAX-CALC-009, TAX-CALC-015
#[test]
fn calc_009_underwater_fmv_basis_rsu_position_owes_zero_estimated_tax() {
    // The FMV-basis RSU shape: two RSU lots vested 2024-11-15 at FMV $200.00/sh —
    // basis = FMV at vest (that value was already W-2 ordinary income, taxed at
    // vest) — marked $190.27 today: 500 sh at −$4,865.00 and 250 sh at
    // −$2,432.50 unrealized, both long-term as of 2026-06-11, with realized LT
    // YTD gains already on the books. The position is a $7,297.50 LOSS: the
    // estimated tax is an AVAILABLE zero (Some(0), clamped — never negative),
    // never the legacy $0-basis model's ~30% of the FULL market value.
    let ctx = context(
        2026,
        federal(
            flat(220_000),
            flat(150_000),
            niit(38_000, 20_000_000),
            0,
            BracketState::Verified,
        ),
        &[],
        0,
        None,
    );
    let lots = [
        open_lot(
            "RSU-GRNT1-f",
            "AMZN",
            date(2024, 11, 15),
            500 * SHARE,
            10_000_000,
            Some(-486_500),
        ),
        open_lot(
            "RSU-GRNT2-b",
            "AMZN",
            date(2024, 11, 15),
            250 * SHARE,
            5_000_000,
            Some(-243_250),
        ),
    ];
    let est = tax::unrealized_estimate(
        &symbol_str("AMZN"),
        &lots,
        date(2026, 6, 11),
        &None,
        pt_core::Cents(0),
        pt_core::Cents(2_500_000), // realized LT YTD the increment stacks on
        &ctx,
    );
    assert_eq!(
        est.unrealized_pretax_cents,
        pt_core::Cents(-729_750),
        "unrealized = value − FMV basis = $142,702.50 − $150,000.00 = −$7,297.50"
    );
    assert_eq!(
        est.estimated_tax_cents,
        Some(pt_core::Cents(0)),
        "a net loss owes zero estimated tax — clamped at 0, never negative, never a re-tax of the vest value"
    );
    assert_eq!(est.effective_rate_ppm, Some(config::Ppm(0)));
}

// ===========================================================================
// TAX-CALC-010 — the unrealized estimate is an estimate only and is degraded
// (None) when the mark is unavailable or state is NoBracketsAvailable.
// ===========================================================================

// @spec TAX-CALC-010
#[test]
fn calc_010_estimate_degraded_when_mark_unavailable() {
    // An open lot with no per-lot unrealized (degraded mark) → the estimate's
    // tax and rate are None (unavailable), never zero.
    let ctx = context(
        2026,
        federal(
            flat(0),
            flat(150_000),
            niit(0, 0),
            0,
            BracketState::Verified,
        ),
        &[],
        0,
        None,
    );
    let lot = open_lot("lotA", "AMZN", date(2023, 1, 1), 10 * SHARE, 50_000, None);
    let est = tax::unrealized_estimate(
        &symbol_str("AMZN"),
        &[lot],
        date(2026, 6, 7),
        &None,
        pt_core::Cents(0),
        pt_core::Cents(0),
        &ctx,
    );
    assert_eq!(
        est.estimated_tax_cents, None,
        "a degraded mark makes the estimate unavailable, never zero"
    );
    assert_eq!(est.effective_rate_ppm, None);
}

// @spec TAX-CALC-010
#[test]
fn calc_010_estimate_degraded_when_state_no_brackets() {
    // A lot whose resolved state is NoBracketsAvailable → the estimate is
    // degraded (None), never a confident zero.
    let ctx = context(
        2026,
        federal(
            flat(0),
            flat(150_000),
            niit(0, 0),
            0,
            BracketState::Verified,
        ),
        &[state_no_brackets("ZZ")],
        0,
        None,
    );
    let lot = open_lot(
        "lotA",
        "AMZN",
        date(2023, 1, 1),
        10 * SHARE,
        50_000,
        Some(100_000),
    );
    let est = tax::unrealized_estimate(
        &symbol_str("AMZN"),
        &[lot],
        date(2026, 6, 7),
        &Some("ZZ".to_string()),
        pt_core::Cents(0),
        pt_core::Cents(0),
        &ctx,
    );
    assert_eq!(est.bracket_state, BracketState::NoBracketsAvailable);
    assert_eq!(
        est.estimated_tax_cents, None,
        "a NoBracketsAvailable state degrades the estimate, never a confident zero"
    );
}

// ===========================================================================
// TAX-CALC-011 — accrues_to_state → jurisdiction mapping & validation.
// ===========================================================================

// @spec TAX-CALC-011
#[test]
fn calc_011_resolves_configured_state() {
    let ctx = context(
        2025,
        federal(flat(0), flat(0), niit(0, 0), 0, BracketState::Verified),
        &[state("DC", flat(100_000), 0, BracketState::Verified)],
        0,
        Some("DC"),
    );
    let res = resolve_state(&ctx, &Some("DC".to_string()));
    assert_eq!(res.jurisdiction, Jurisdiction::State("DC".to_string()));
    assert!(
        !res.fell_back_to_residency,
        "an explicit configured stamp does not fall back"
    );
}

// @spec TAX-CALC-011
#[test]
fn calc_011_missing_stamp_falls_back_to_residency_default_flagged() {
    let ctx = context(
        2025,
        federal(flat(0), flat(0), niit(0, 0), 0, BracketState::Verified),
        &[state("NJ", flat(55_250), 0, BracketState::Verified)],
        0,
        Some("NJ"),
    );
    let res = resolve_state(&ctx, &None);
    assert_eq!(
        res.jurisdiction,
        Jurisdiction::State("NJ".to_string()),
        "a missing stamp falls back to the current-residency default"
    );
    assert!(res.fell_back_to_residency, "the fallback is flagged");
}

// @spec TAX-CALC-011
#[test]
fn calc_011_unconfigured_state_resolves_no_brackets_for_state_portion() {
    // A gain stamped with an unconfigured state "ZZ": the STATE portion resolves
    // to NoBracketsAvailable (unavailable, not zero), while Federal still computes.
    let ctx = context(
        2025,
        federal(
            flat(300_000),
            flat(0),
            niit(0, 0),
            0,
            BracketState::Verified,
        ),
        &[], // no states configured
        0,
        None,
    );
    let g = gain(
        "s1",
        1,
        "lotA",
        "AMZN",
        date(2025, 1, 2),
        date(2025, 6, 1),
        100_000,
        Some("ZZ"),
    );
    let accruals = compute_accruals(&[g], &[], &ctx);

    // Federal computes a real number.
    let fed = find_accrual(&accruals, "s1", "lotA", &Jurisdiction::Federal).unwrap();
    assert_eq!(
        fed.applied_cents,
        Some(pt_core::Cents(30_000)),
        "federal still computes"
    );

    // The state portion exists but is NoBracketsAvailable with no number.
    let st = find_accrual(
        &accruals,
        "s1",
        "lotA",
        &Jurisdiction::State("ZZ".to_string()),
    )
    .expect("an unconfigured-state accrual is still emitted, degraded");
    assert_eq!(st.bracket_state, BracketState::NoBracketsAvailable);
    assert_eq!(
        st.applied_cents, None,
        "an unconfigured state is unavailable, never a confident zero"
    );
}

// ===========================================================================
// TAX-CALC-012 — BracketState is carried on every accrual; NoBracketsAvailable
// is unavailable, never zero.
// ===========================================================================

// @spec TAX-CALC-012
#[test]
fn calc_012_bracket_state_carried_on_accrual() {
    // A STALE federal bracket set: the accrual still computes a number but carries
    // the Stale tag so summary/tui can render "[est, brackets stale]".
    let ctx = context(
        2025,
        federal(flat(300_000), flat(0), niit(0, 0), 0, BracketState::Stale),
        &[],
        0,
        None,
    );
    let g = gain(
        "s1",
        1,
        "lotA",
        "AMZN",
        date(2025, 1, 2),
        date(2025, 6, 1),
        100_000,
        None,
    );
    let accruals = compute_accruals(&[g], &[], &ctx);

    let fed = find_accrual(&accruals, "s1", "lotA", &Jurisdiction::Federal).unwrap();
    assert_eq!(
        fed.bracket_state,
        BracketState::Stale,
        "the per-jurisdiction BracketState is carried on every accrual"
    );
    assert_eq!(
        fed.applied_cents,
        Some(pt_core::Cents(30_000)),
        "a Stale set still computes a (degraded-tagged) number"
    );
}
