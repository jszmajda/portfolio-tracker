//! Red (failing) tests for the TAX-VERIF-* verification invariants. TDD:
//! written BEFORE the kernel/orchestration is implemented.
//!
//! Two layers cover each invariant, mirroring `ledger-core`'s `tests/verif.rs`:
//!   1. `#[cfg(kani)]` `#[kani::proof]` harness stubs — bounded model checking
//!      under `cargo kani` (compiled out under plain `cargo test`, so Kani is
//!      not a hard dependency). Kani targets the verus!{} ARITHMETIC kernel
//!      only; the fold-level invariants (-004/-005/-006/-007) stay in #[test]s
//!      because CBMC cannot fold BTreeMap/String/Vec tractably.
//!   2. Runnable property/unit tests over bounded inputs — these gate under
//!      `cargo test -p tax --test verif` even without Kani, and fail RED until
//!      the kernel and lifecycle fold are implemented.

#![allow(clippy::inconsistent_digit_grouping)]

mod common;
use common::*;

use config::{BracketState, Jurisdiction};
use tax::{
    compute_accruals, quarter_of, quarterly_report, reserves, validate_event, Quarter, Term,
};

// ===========================================================================
// TAX-VERIF-001 — monotonic tax: accrual(g) is non-decreasing in g.
// ===========================================================================

// @spec TAX-VERIF-001
#[test]
fn verif_001_accrual_monotonic_in_gain() {
    // Progressive federal brackets. As the single gain's size grows over a fixed
    // zero base, its accrual never decreases.
    let ctx = context(
        2025,
        federal(
            bracket_set(&[(0, 100_000), (50_000, 250_000), (500_000, 400_000)]),
            flat(0),
            niit(0, 0),
            0,
            BracketState::Verified,
        ),
        &[],
        0,
        None,
    );
    let mut prev = -1i64;
    for g_cents in [0, 1, 100, 25_000, 49_999, 50_000, 100_000, 500_000, 1_000_000] {
        // Short-term so the gain exercises the progressive ordinary brackets.
        let g = gain("s1", 1, "lotA", "AMZN", date(2025, 1, 2), date(2025, 6, 1), g_cents, None);
        let accruals = compute_accruals(&[g], &[], &ctx);
        let acc = find_accrual(&accruals, "s1", "lotA", &Jurisdiction::Federal)
            .unwrap()
            .applied_cents
            .unwrap()
            .0;
        assert!(
            acc >= prev,
            "accrual must be non-decreasing in gain: {acc} < {prev} at gain {g_cents}"
        );
        prev = acc;
    }
}

// ===========================================================================
// TAX-VERIF-002 — bounded tax: for a positive gain over a non-negative YTD base,
// 0 ≤ accrual ≤ gain.
// ===========================================================================

// @spec TAX-VERIF-002
#[test]
fn verif_002_accrual_bounded_zero_to_gain() {
    // A high-but-sub-100% stacked rate (fed ordinary 37% + state 10.75% + NIIT
    // 3.8% = 51.55% top). Every positive gain's per-jurisdiction accrual is in
    // [0, gain].
    let ctx = context(
        2025,
        federal(flat(370_000), flat(200_000), niit(38_000, 0), 0, BracketState::Verified),
        &[state("DC", flat(107_500), 0, BracketState::Verified)],
        0,
        Some("DC"),
    );
    for g_cents in [1, 50, 1_000, 100_000, 5_000_000] {
        let g = gain("s1", 1, "lotA", "AMZN", date(2024, 1, 1), date(2025, 6, 1), g_cents, Some("DC"));
        let accruals = compute_accruals(&[g], &[], &ctx);
        for a in &accruals {
            if let Some(applied) = a.applied_cents {
                assert!(
                    0 <= applied.0 && applied.0 <= g_cents,
                    "0 ≤ accrual ≤ gain violated: accrual {} for gain {g_cents} ({:?})",
                    applied.0,
                    a.key.jurisdiction
                );
            }
        }
    }
}

// ===========================================================================
// TAX-VERIF-003 — total calculation: T_J is total over signed inputs (negatives
// clamped); no input panics or diverges.
// ===========================================================================

// @spec TAX-VERIF-003
#[test]
fn verif_003_total_over_signed_inputs() {
    // A mix of losses and gains, including a leading loss (negative cumulative
    // base). compute_accruals must not panic and must yield a defined accrual for
    // every gain (clamped at zero where the year is net-negative).
    let ctx = context(
        2025,
        federal(flat(300_000), flat(150_000), niit(38_000, 0), 0, BracketState::Verified),
        &[],
        0,
        None,
    );
    let gains = vec![
        gain("s1", 1, "lotA", "AMZN", date(2024, 1, 1), date(2025, 2, 1), -500_000, None),
        gain("s2", 2, "lotB", "AMZN", date(2024, 1, 1), date(2025, 3, 1), 100_000, None),
        gain("s3", 3, "lotC", "AMZN", date(2023, 1, 1), date(2025, 4, 1), -200_000, None),
        gain("s4", 4, "lotD", "AMZN", date(2023, 1, 1), date(2025, 5, 1), 800_000, None),
    ];
    let accruals = compute_accruals(&gains, &[], &ctx); // must not panic
    for a in &accruals {
        // Every accrual is defined (Some) since brackets are present.
        assert!(a.applied_cents.is_some(), "T_J is total: every accrual is defined");
    }
}

// ===========================================================================
// TAX-VERIF-004 — lifecycle state machine: forward-only, no backward / skip /
// double-pay (the validator enforces it).
// ===========================================================================

// @spec TAX-VERIF-004
#[test]
fn verif_004_lifecycle_forward_only() {
    let ctx = context(
        2025,
        federal(flat(300_000), flat(0), niit(0, 0), 0, BracketState::Verified),
        &[],
        0,
        None,
    );
    let g = gain("s1", 1, "lotA", "AMZN", date(2024, 1, 1), date(2025, 6, 1), 100_000, None);
    let key = fed_key("s1", "lotA", 2025);

    // Skip Allocate → Move on Accrued is rejected (no skip).
    assert!(
        validate_event(&[g.clone()], &[], &move_(1, key.clone(), 30_000, date(2025, 6, 1)), &ctx).is_err(),
        "Move skipping Allocate is rejected"
    );
    // Skip Move → Pay on Allocated is rejected (no skip).
    let allocated = vec![allocate(1, key.clone(), "acct")];
    assert!(
        validate_event(
            &[g.clone()],
            &allocated,
            &pay(2, Jurisdiction::Federal, 2025, Quarter::Q2, 30_000, date(2025, 6, 1), vec![key.clone()]),
            &ctx,
        )
        .is_err(),
        "Pay skipping Move is rejected"
    );
    // The full forward chain Allocate → Move → Pay is accepted at each step.
    assert!(validate_event(&[g.clone()], &[], &allocate(1, key.clone(), "acct"), &ctx).is_ok());
    let moved = vec![
        allocate(1, key.clone(), "acct"),
        move_(2, key.clone(), 30_000, date(2025, 6, 1)),
    ];
    assert!(validate_event(
        &[g],
        &moved,
        &pay(3, Jurisdiction::Federal, 2025, Quarter::Q2, 30_000, date(2025, 6, 2), vec![key]),
        &ctx,
    )
    .is_ok());
}

// ===========================================================================
// TAX-VERIF-005 — reserve conservation: reserve(J, year) ≡ Σ Move − Σ Pay.
// ===========================================================================

// @spec TAX-VERIF-005
#[test]
fn verif_005_reserve_conservation() {
    let ctx = context(
        2025,
        federal(flat(300_000), flat(0), niit(0, 0), 0, BracketState::Verified),
        &[],
        0,
        None,
    );
    let k1 = fed_key("s1", "lotA", 2025);
    let k2 = fed_key("s2", "lotB", 2025);
    let gains = vec![
        gain("s1", 1, "lotA", "AMZN", date(2024, 1, 1), date(2025, 6, 1), 100_000, None),
        gain("s2", 2, "lotB", "AMZN", date(2024, 1, 1), date(2025, 7, 1), 100_000, None),
    ];
    let events = vec![
        allocate(1, k1.clone(), "acct"),
        move_(2, k1.clone(), 30_000, date(2025, 6, 15)),
        allocate(3, k2.clone(), "acct"),
        move_(4, k2.clone(), 7_000, date(2025, 7, 1)),
        pay(5, Jurisdiction::Federal, 2025, Quarter::Q2, 20_000, date(2025, 6, 16), vec![k1]),
        pay(6, Jurisdiction::Federal, 2025, Quarter::Q3, 7_000, date(2025, 9, 1), vec![k2]),
    ];
    // Independently sum Moves and Pays for the federal/2025 key.
    let sum_move = 30_000 + 7_000;
    let sum_pay = 20_000 + 7_000;
    let r = reserves(&gains, &events, &ctx);
    let fed = r
        .iter()
        .find(|r| r.jurisdiction == Jurisdiction::Federal && r.tax_year.0 == 2025)
        .unwrap();
    assert_eq!(
        fed.balance_cents,
        pt_core::Cents(sum_move - sum_pay),
        "reserve ≡ Σ Move − Σ Pay"
    );
}

// ===========================================================================
// TAX-VERIF-006 — quarterly partition: the four IRS periods partition the year
// with no gap/overlap, so Σ over periods (gains, accruals) ≡ annual.
// ===========================================================================

// @spec TAX-VERIF-006
#[test]
fn verif_006_quarters_partition_year_no_gap_or_overlap() {
    // Every day of 2025 maps to exactly one quarter, and the boundaries are
    // contiguous (Mar 31→Apr 1 = Q1→Q2, May 31→Jun 1 = Q2→Q3, Aug 31→Sep 1 =
    // Q3→Q4). Walk the whole year.
    let mut counts = [0usize; 4];
    let start = date(2025, 1, 1).0;
    let end = date(2025, 12, 31).0;
    for d in start..=end {
        let q = quarter_of(pt_core::Date(d));
        counts[match q {
            Quarter::Q1 => 0,
            Quarter::Q2 => 1,
            Quarter::Q3 => 2,
            Quarter::Q4 => 3,
        }] += 1;
    }
    // Q1 Jan1–Mar31 = 31+28+31 = 90; Q2 Apr+May = 30+31 = 61; Q3 Jun+Jul+Aug =
    // 30+31+31 = 92; Q4 Sep–Dec = 30+31+30+31 = 122. Sum = 365 (no gap/overlap).
    assert_eq!(counts, [90, 61, 92, 122], "the four IRS periods partition 2025 exactly");
    assert_eq!(counts.iter().sum::<usize>(), 365);
}

// @spec TAX-VERIF-006
#[test]
fn verif_006_sum_over_periods_equals_annual_accrual() {
    // Two federal gains in different quarters; Σ of the quarterly federal accrual
    // cells equals the total federal accrual.
    let ctx = context(
        2025,
        federal(flat(300_000), flat(0), niit(0, 0), 0, BracketState::Verified),
        &[],
        0,
        None,
    );
    let g1 = gain("s1", 1, "lotA", "AMZN", date(2025, 1, 2), date(2025, 2, 1), 100_000, None);
    let g2 = gain("s2", 2, "lotB", "AMZN", date(2025, 6, 1), date(2025, 7, 1), 200_000, None);
    let cells = quarterly_report(&[g1, g2], &[], &ctx);
    let quarterly_total: i64 = cells
        .iter()
        .filter(|c| c.jurisdiction == Jurisdiction::Federal)
        .map(|c| c.accrual_cents.0)
        .sum();
    // Annual: both short-term at 30% → 30_000 + 60_000 = 90_000.
    assert_eq!(
        quarterly_total, 90_000,
        "Σ over periods of the accrual equals the annual accrual"
    );
}

// ===========================================================================
// TAX-VERIF-007 — orphan totality: a TaxEvent whose accrual_key has no backing
// RealizedGain folds as a no-op; replay stays total.
// ===========================================================================

// @spec TAX-VERIF-007
#[test]
fn verif_007_orphan_event_folds_as_noop() {
    let ctx = context(
        2025,
        federal(flat(300_000), flat(0), niit(0, 0), 0, BracketState::Verified),
        &[],
        0,
        None,
    );
    let g = gain("s1", 1, "lotA", "AMZN", date(2024, 1, 1), date(2025, 6, 1), 100_000, None);
    // An Allocate for a NON-EXISTENT accrual (sale "ghost" has no RealizedGain).
    let orphan = allocate(1, fed_key("ghost", "lotZ", 2025), "acct");
    // Plus a real Allocate for s1.
    let real = allocate(2, fed_key("s1", "lotA", 2025), "acct");

    // Replaying the orphan + the real event must not panic and must produce the
    // same accrual for s1 as replaying the real event alone (the orphan is a
    // no-op).
    let with_orphan = compute_accruals(&[g.clone()], &[orphan, real.clone()], &ctx);
    let without_orphan = compute_accruals(&[g], &[real], &ctx);

    let a1 = find_accrual(&with_orphan, "s1", "lotA", &Jurisdiction::Federal).unwrap();
    let a2 = find_accrual(&without_orphan, "s1", "lotA", &Jurisdiction::Federal).unwrap();
    assert_eq!(
        a1.state, a2.state,
        "an orphan event folds as a no-op; replay over any log stays total"
    );
    // And there is no accrual fabricated for the ghost sale.
    assert!(
        find_accrual(&with_orphan, "ghost", "lotZ", &Jurisdiction::Federal).is_none(),
        "an orphan event fabricates no accrual"
    );
}

// @spec TAX-VERIF-007, TAX-VERIF-008
#[test]
fn verif_007_orphan_move_surfaces_a_warning_and_flags_a_backless_reserve() {
    use tax::{compute_accruals_with_warnings, orphan_warnings, reserves, OrphanKind};
    let ctx = context(
        2025,
        federal(flat(300_000), flat(0), niit(0, 0), 0, BracketState::Verified),
        &[],
        0,
        None,
    );
    // No backing gains at all: the ghost accrual_key's sale was reversed in
    // ledger-core, leaving a Move with no backing RealizedGain.
    let ghost = fed_key("ghost", "lotZ", 2025);
    let events = vec![
        allocate(1, ghost.clone(), "acct"),
        move_(2, ghost.clone(), 50_000, date(2025, 6, 15)),
    ];

    // The orphan Move (and Allocate) is SURFACED as a warning — not silently
    // dropped — so it can be unwound manually.
    let warnings = orphan_warnings(&[], &events, &ctx);
    assert!(
        warnings.iter().any(|w| w.accrual_key == ghost && w.kind == OrphanKind::Move),
        "the orphan Move is surfaced as a warning"
    );

    // The reserve still tallies Σ Move − Σ Pay (50_000) per TAX-RESERVE-001, BUT
    // is flagged backless so it is distinguishable from an ordinary balance.
    let r = reserves(&[], &events, &ctx);
    let fed = r
        .iter()
        .find(|r| r.jurisdiction == Jurisdiction::Federal && r.tax_year.0 == 2025)
        .expect("a federal 2025 reserve still exists");
    assert_eq!(fed.balance_cents, pt_core::Cents(50_000), "balance = Σ Move − Σ Pay");
    assert!(
        fed.has_backless_entry,
        "a backless Move makes the reserve distinguishable from a backed one"
    );

    // A BACKED reserve (same Move against a real gain) is NOT flagged backless —
    // the warning distinguishes the two.
    let g = gain("s1", 1, "lotA", "AMZN", date(2024, 1, 1), date(2025, 6, 1), 100_000, None);
    let backed = vec![
        allocate(1, fed_key("s1", "lotA", 2025), "acct"),
        move_(2, fed_key("s1", "lotA", 2025), 50_000, date(2025, 6, 15)),
    ];
    let (_accruals, backed_warnings) = compute_accruals_with_warnings(&[g.clone()], &backed, &ctx);
    assert!(backed_warnings.is_empty(), "a backed Move surfaces no orphan warning");
    let r2 = reserves(&[g], &backed, &ctx);
    let fed2 = r2
        .iter()
        .find(|r| r.jurisdiction == Jurisdiction::Federal && r.tax_year.0 == 2025)
        .unwrap();
    assert!(!fed2.has_backless_entry, "a backed reserve is not flagged backless");
}

// @spec TAX-VERIF-002
#[test]
fn verif_002_bracket_tax_bounded_even_on_malformed_set() {
    // bracket_tax_on consumes an explicit `remaining` budget and clamps each
    // band's taxed width to it, so even a MALFORMED bracket set (unsorted,
    // overlapping, non-0-anchored thresholds) returns a value in [0, amount]
    // rather than panicking or exceeding the band. config rejects malformed sets
    // upstream (CONFIG-VALID-*), so this clamp only ever binds defensively; this
    // test pins that the totality safeguard is intentional and observable.
    use tax::kernel::bracket_tax_on;
    let amount: i128 = 1_000_000;
    // Unsorted + overlapping thresholds, each rate < 100% (PPM_FULL).
    let thresholds: Vec<i128> = vec![500_000, 0, 200_000, 200_000];
    let rates: Vec<i128> = vec![900_000, 100_000, 500_000, 800_000];
    let t = bracket_tax_on(&thresholds, &rates, 0, amount);
    assert!(
        (0..=amount).contains(&t),
        "tax stays in [0, amount] even on a malformed bracket set: got {t}"
    );
}

// A sanity check that the calendar classification used by the report is exact at
// the term boundary (long-term vs short-term feeding the LT/ST split).
// @spec TAX-VERIF-006
#[test]
fn verif_006_term_split_feeds_quarterly_cells() {
    use tax::classify_term;
    // The same gain dated one day apart flips term, so the report's LT/ST split
    // tracks the calendar boundary.
    assert_eq!(classify_term(date(2024, 6, 1), date(2025, 6, 1)), Term::ShortTerm);
    assert_eq!(classify_term(date(2024, 6, 1), date(2025, 6, 2)), Term::LongTerm);
}

// ===========================================================================
// Kani bounded-model-checking harness STUBS (one per kernel-arithmetic VERIF
// invariant). Compiled out under plain `cargo test` (Kani not a hard
// dependency); run with `cargo kani`. The crate already carries
// src/kani_proofs.rs with the executable harnesses over the verus kernel; these
// mirror the arithmetic set so each kernel invariant has a proof obligation
// reachable from the test suite too. Bodies are stubs (unimplemented!) so they
// fail until the kernel contracts land. The fold-level invariants
// (-004/-005/-006/-007) are NOT model-checked (heap-collection folds).
// ===========================================================================
#[cfg(kani)]
mod kani_harnesses {
    // @spec TAX-VERIF-001
    #[kani::proof]
    fn kani_verif_001_monotonic_tax() {
        unimplemented!("bracket-stack tax is non-decreasing in the gain (kernel arithmetic)")
    }

    // @spec TAX-VERIF-002
    #[kani::proof]
    fn kani_verif_002_bounded_tax() {
        unimplemented!("0 ≤ accrual ≤ gain for a positive gain over a non-negative base")
    }

    // @spec TAX-VERIF-003
    #[kani::proof]
    fn kani_verif_003_total_calculation() {
        unimplemented!("T_J total over signed inputs (clamped at zero); no panic")
    }
}
