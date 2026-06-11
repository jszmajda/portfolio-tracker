//! Red (failing) tests for the `tax` ACCRUAL LIFECYCLE specs
//! (TAX-ACCRUAL-001..007) and the append-time VALIDATION/ERROR specs
//! (TAX-ERR-001..005). The lifecycle fold over the `TaxEvent` log lives OUTSIDE
//! the `verus!{}` boundary (BTreeMap/String), so it is exercised here by
//! `#[test]`s — exactly as the design's KANI note prescribes.

#![allow(clippy::inconsistent_digit_grouping)]

mod common;
use common::*;

use config::{BracketState, Jurisdiction};
use tax::{
    annual_report, compute_accruals, quarterly_report, validate_event, AccrualState, Quarter,
    TaxError,
};

// A one-gain, federal-flat-30% context shared across lifecycle tests. A $1,000
// short-term gain → derived/applied federal accrual = 30_000c.
fn ctx() -> tax::TaxContext {
    context(
        2025,
        federal(flat(300_000), flat(0), niit(0, 0), 0, BracketState::Verified),
        &[],
        0, // de-minimis $0
        None,
    )
}
fn one_gain() -> ledger_core::RealizedGain {
    // Short-term (acquired the same year, well under a year before sale), so the
    // gain is taxed at the federal ordinary 30% → derived/applied 30_000c.
    gain("s1", 1, "lotA", "AMZN", date(2025, 1, 2), date(2025, 6, 1), 100_000, None)
}

// ===========================================================================
// TAX-ACCRUAL-001 — one accrual per (sale_id, lot_id, jurisdiction, tax_year),
// defaulting to Accrued with no event.
// ===========================================================================

// @spec TAX-ACCRUAL-001
#[test]
fn accrual_001_one_per_gain_per_jurisdiction_defaults_accrued() {
    // One gain stamped DC → exactly two accruals: Federal + DC State, both
    // Accrued with no events.
    let ctx = context(
        2025,
        federal(flat(300_000), flat(0), niit(0, 0), 0, BracketState::Verified),
        &[state("DC", flat(100_000), 0, BracketState::Verified)],
        0,
        None,
    );
    let g = gain("s1", 1, "lotA", "AMZN", date(2025, 1, 2), date(2025, 6, 1), 100_000, Some("DC"));
    let accruals = compute_accruals(&[g], &[], &ctx);

    assert_eq!(accruals.len(), 2, "one accrual per jurisdiction: Federal + DC");
    let fed = find_accrual(&accruals, "s1", "lotA", &Jurisdiction::Federal).unwrap();
    let st = find_accrual(&accruals, "s1", "lotA", &Jurisdiction::State("DC".to_string())).unwrap();
    assert_eq!(fed.state, AccrualState::Accrued, "no event ⇒ default Accrued");
    assert_eq!(st.state, AccrualState::Accrued);
    assert_eq!(fed.key.tax_year.0, 2025, "tax_year = year(sale_date)");
}

// ===========================================================================
// TAX-ACCRUAL-002 — Allocate records account_label and advances to Allocated;
// re-allocate is last-write-wins until Paid.
// ===========================================================================

// @spec TAX-ACCRUAL-002
#[test]
fn accrual_002_allocate_advances_and_records_label() {
    let events = vec![allocate(1, fed_key("s1", "lotA", 2025), "reserve-checking")];
    let accruals = compute_accruals(&[one_gain()], &events, &ctx());
    let fed = find_accrual(&accruals, "s1", "lotA", &Jurisdiction::Federal).unwrap();
    assert_eq!(
        fed.state,
        AccrualState::Allocated { account_label: "reserve-checking".to_string() },
        "Allocate advances to Allocated and records the account label"
    );
}

// @spec TAX-ACCRUAL-002
#[test]
fn accrual_002_reallocate_is_last_write_wins() {
    let events = vec![
        allocate(1, fed_key("s1", "lotA", 2025), "first"),
        allocate(2, fed_key("s1", "lotA", 2025), "second"),
    ];
    let accruals = compute_accruals(&[one_gain()], &events, &ctx());
    let fed = find_accrual(&accruals, "s1", "lotA", &Jurisdiction::Federal).unwrap();
    assert_eq!(
        fed.state,
        AccrualState::Allocated { account_label: "second".to_string() },
        "re-Allocate before Pay is last-write-wins on account_label"
    );
}

// ===========================================================================
// TAX-ACCRUAL-003 — Move records actual amount + date and advances to Moved.
// ===========================================================================

// @spec TAX-ACCRUAL-003
#[test]
fn accrual_003_move_records_actual_amount_and_advances() {
    // The moved amount need NOT equal the computed accrual (30_000c) — record
    // the actual 25_000c.
    let events = vec![
        allocate(1, fed_key("s1", "lotA", 2025), "acct"),
        move_(2, fed_key("s1", "lotA", 2025), 25_000, date(2025, 6, 15)),
    ];
    let accruals = compute_accruals(&[one_gain()], &events, &ctx());
    let fed = find_accrual(&accruals, "s1", "lotA", &Jurisdiction::Federal).unwrap();
    assert_eq!(
        fed.state,
        AccrualState::Moved {
            account_label: "acct".to_string(),
            amount_cents: pt_core::Cents(25_000),
            date: date(2025, 6, 15),
        },
        "Move records the actual money moved (not the computed accrual) and the date"
    );
}

// ===========================================================================
// TAX-ACCRUAL-004 — Pay marks every covered Moved accrual Paid, recording the
// remittance amount, date, and period.
// ===========================================================================

// @spec TAX-ACCRUAL-004
#[test]
fn accrual_004_pay_marks_covered_accruals_paid() {
    let key = fed_key("s1", "lotA", 2025);
    let events = vec![
        allocate(1, key.clone(), "acct"),
        move_(2, key.clone(), 30_000, date(2025, 6, 15)),
        pay(3, Jurisdiction::Federal, 2025, Quarter::Q2, 30_000, date(2025, 6, 16), vec![key.clone()]),
    ];
    let accruals = compute_accruals(&[one_gain()], &events, &ctx());
    let fed = find_accrual(&accruals, "s1", "lotA", &Jurisdiction::Federal).unwrap();
    match &fed.state {
        AccrualState::Paid { amount_cents, date: d, period, .. } => {
            assert_eq!(*amount_cents, pt_core::Cents(30_000));
            assert_eq!(*d, date(2025, 6, 16));
            assert_eq!(*period, Quarter::Q2);
        }
        other => panic!("a covered accrual must be Paid, got {other:?}"),
    }
}

// ===========================================================================
// TAX-ACCRUAL-005 — |amount| below de-minimis auto-settles (no lifecycle action,
// excluded from outstanding prompts).
// ===========================================================================

// @spec TAX-ACCRUAL-005
#[test]
fn accrual_005_below_de_minimis_auto_settles() {
    // De-minimis $5 (500c). A tiny sell-to-cover gain of $1 (100c) at 30% → an
    // accrual of 30c, below the threshold → auto-settled, even with no events.
    let ctx = context(
        2025,
        federal(flat(300_000), flat(0), niit(0, 0), 0, BracketState::Verified),
        &[],
        500, // de-minimis $5
        None,
    );
    let g = gain("s1", 1, "lotA", "AMZN", date(2025, 1, 2), date(2025, 6, 1), 100, None);
    let accruals = compute_accruals(&[g], &[], &ctx);
    let fed = find_accrual(&accruals, "s1", "lotA", &Jurisdiction::Federal).unwrap();
    assert_eq!(fed.applied_cents, Some(pt_core::Cents(30)), "30c accrual");
    assert!(
        fed.de_minimis,
        "an |amount| below de-minimis is flagged auto-settled (excluded from prompts)"
    );
}

// @spec TAX-ACCRUAL-005
#[test]
fn accrual_005_above_de_minimis_is_not_auto_settled() {
    // Same threshold but a $1,000 gain → 30_000c accrual, well above de-minimis.
    let ctx = context(
        2025,
        federal(flat(300_000), flat(0), niit(0, 0), 0, BracketState::Verified),
        &[],
        500,
        None,
    );
    let accruals = compute_accruals(&[one_gain()], &[], &ctx);
    let fed = find_accrual(&accruals, "s1", "lotA", &Jurisdiction::Federal).unwrap();
    assert!(!fed.de_minimis, "a material accrual is not auto-settled");
}

// ===========================================================================
// TAX-ACCRUAL-006 — a sale reversed while its accrual is still Accrued is
// omitted on replay.
// ===========================================================================

// @spec TAX-ACCRUAL-006
#[test]
fn accrual_006_reversed_accrued_sale_is_omitted() {
    // The gain disappeared from ledger-core (no longer in `gains`) and it had no
    // lifecycle events → no accrual is produced for it.
    let accruals = compute_accruals(&[], &[], &ctx());
    assert!(
        find_accrual(&accruals, "s1", "lotA", &Jurisdiction::Federal).is_none(),
        "a reversed, still-Accrued sale produces no accrual on replay"
    );
    assert!(accruals.is_empty());
}

// ===========================================================================
// TAX-ACCRUAL-007 — a closed-year combined migration accrual supersedes that
// year's per-RealizedGain accruals (they are excluded from outstanding).
// ===========================================================================

// @spec TAX-ACCRUAL-007, TAX-REPORT-005
#[test]
fn accrual_007_migration_seed_supersedes_per_gain_accruals() {
    // A closed prior year (2024) with a MATERIAL per-gain accrual — a SHORT-TERM
    // gain at the 30% ordinary rate so its derived federal accrual is a non-zero
    // 30_000c (NOT a long-term-against-flat(0) zero that masks a double-count).
    // Plus a combined migration seed of 99_999c. The per-gain federal accrual is
    // SUPERSEDED (excluded from outstanding) so the year reconciles to the legacy
    // actual: the annual federal-2024 accrued/outstanding must equal EXACTLY
    // 99_999c, NOT 99_999 + 30_000.
    let ctx = context(
        2024,
        federal(flat(300_000), flat(0), niit(0, 0), 0, BracketState::Verified),
        &[],
        0,
        None,
    );
    // Short-term: acquired and sold within the year → 30% ordinary → 30_000c.
    let g = gain("s1", 1, "lotA", "AMZN", date(2024, 1, 2), date(2024, 6, 1), 100_000, None);
    let events = vec![seed_migration(1, Jurisdiction::Federal, 2024, 99_999, "legacy 2024 actual")];
    let accruals = compute_accruals(&[g.clone()], &events, &ctx);

    // The migration accrual itself is present, carrying the legacy actual.
    let migration = accruals
        .iter()
        .find(|a| a.key.sale_id.is_empty() && a.key.jurisdiction == Jurisdiction::Federal)
        .expect("the combined migration accrual must be present");
    assert_eq!(migration.applied_cents, Some(pt_core::Cents(99_999)));

    // The per-gain federal accrual is materially non-zero (so the assertion below
    // can actually fail if supersession is not honored) yet flagged superseded.
    let per_gain = find_accrual(&accruals, "s1", "lotA", &Jurisdiction::Federal)
        .expect("the per-gain accrual is still listed");
    assert_eq!(
        per_gain.applied_cents,
        Some(pt_core::Cents(30_000)),
        "the per-gain accrual is a material 30_000c (not masked by a zero rate)"
    );
    assert!(per_gain.superseded, "the per-gain accrual is flagged superseded");

    // THE CORE PROMISE: the year reconciles to the seeded legacy actual. The
    // annual federal-2024 accrued and outstanding equal EXACTLY 99_999c — the
    // superseded per-gain 30_000c is excluded, not added on top (would be 129_999).
    let rows = annual_report(&[g.clone()], &events, &ctx);
    let fed = rows
        .iter()
        .find(|r| r.jurisdiction == Jurisdiction::Federal && r.tax_year.0 == 2024)
        .expect("a federal 2024 annual row must exist");
    assert_eq!(
        fed.accrued_cents,
        pt_core::Cents(99_999),
        "the migrated year's accrued reconciles to the legacy actual, not 129_999"
    );
    assert_eq!(
        fed.outstanding_cents,
        pt_core::Cents(99_999),
        "outstanding = accrued − paid reconciles to the legacy actual"
    );

    // And the quarterly cells for that migrated year carry ZERO per-gain accrual
    // (the migration accrual has no sale_date, so it is not a quarterly figure;
    // the superseded per-gain accrual contributes 0). (TAX-REPORT-003 cross-report)
    let cells = quarterly_report(&[g], &events, &ctx);
    let per_gain_accrual: i64 = cells
        .iter()
        .filter(|c| c.jurisdiction == Jurisdiction::Federal && c.tax_year.0 == 2024)
        .map(|c| c.accrual_cents.0)
        .sum();
    assert_eq!(
        per_gain_accrual, 0,
        "the migrated year's quarterly per-gain accrual cells sum to zero (superseded)"
    );
}

// ===========================================================================
// TAX-ACCRUAL-008 — a combined migration accrual's term is undefined; a fixed
// LongTerm placeholder is stamped that no consumer reads as a regime.
// ===========================================================================

// @spec TAX-ACCRUAL-008
#[test]
fn accrual_008_migration_accrual_term_is_a_fixed_placeholder() {
    // A combined migration figure is regime-agnostic (no backing RealizedGain, so
    // no LT/ST classification). The migration accrual carries a FIXED LongTerm
    // placeholder regardless of any gains in the year — it is a stamp, not a
    // classification. Seed a migration for a year that has NO short-term per-gain
    // basis at all, so the placeholder is observably the fixed value rather than
    // anything derived from a gain.
    let ctx = context(
        2024,
        federal(flat(300_000), flat(0), niit(0, 0), 0, BracketState::Verified),
        &[],
        0,
        None,
    );
    let events = vec![seed_migration(1, Jurisdiction::Federal, 2024, 99_999, "legacy 2024 actual")];
    let accruals = compute_accruals(&[], &events, &ctx);
    let migration = accruals
        .iter()
        .find(|a| a.key.sale_id.is_empty() && a.key.jurisdiction == Jurisdiction::Federal)
        .expect("the combined migration accrual must be present");
    assert_eq!(
        migration.term,
        tax::Term::LongTerm,
        "the migration accrual carries the fixed LongTerm placeholder (regime-agnostic)"
    );
}

// ===========================================================================
// TAX-ERR-001 — validate before append; reject leaves state byte-identical.
// ===========================================================================

// @spec TAX-ERR-001
#[test]
fn err_001_reject_leaves_state_byte_identical() {
    let key = fed_key("s1", "lotA", 2025);
    // Accepted prefix: allocate + move (so the accrual is Moved).
    let accepted = vec![
        allocate(1, key.clone(), "acct"),
        move_(2, key.clone(), 30_000, date(2025, 6, 15)),
    ];
    let before = compute_accruals(&[one_gain()], &accepted, &ctx());

    // A bad candidate: a SECOND Move on an already-Moved (not Allocated) accrual.
    let bad = move_(3, key, 1, date(2025, 6, 17));
    assert!(
        validate_event(&[one_gain()], &accepted, &bad, &ctx()).is_err(),
        "a Move on a non-Allocated accrual must be rejected"
    );

    // The replayed state over the accepted prefix is unchanged by the rejection.
    let after = compute_accruals(&[one_gain()], &accepted, &ctx());
    assert_eq!(before, after, "rejecting a candidate leaves replayed state byte-identical");
}

// ===========================================================================
// TAX-ERR-002 — Move on a non-Allocated accrual is rejected.
// ===========================================================================

// @spec TAX-ERR-002
#[test]
fn err_002_move_on_unallocated_rejected() {
    let key = fed_key("s1", "lotA", 2025);
    // No prior Allocate: the accrual is still Accrued, so Move must be rejected.
    let candidate = move_(1, key, 30_000, date(2025, 6, 15));
    assert_eq!(
        validate_event(&[one_gain()], &[], &candidate, &ctx()),
        Err(TaxError::MoveOnUnallocated),
        "Move on an unallocated (Accrued) accrual is rejected"
    );
}

// ===========================================================================
// TAX-ERR-003 — Pay on an unmoved accrual, or one already paid, is rejected.
// ===========================================================================

// @spec TAX-ERR-003
#[test]
fn err_003_pay_on_unmoved_rejected() {
    let key = fed_key("s1", "lotA", 2025);
    // Allocated but not Moved → Pay must be rejected.
    let accepted = vec![allocate(1, key.clone(), "acct")];
    let candidate = pay(2, Jurisdiction::Federal, 2025, Quarter::Q2, 30_000, date(2025, 6, 16), vec![key]);
    assert_eq!(
        validate_event(&[one_gain()], &accepted, &candidate, &ctx()),
        Err(TaxError::PayOnUnmoved),
        "Pay on an unmoved accrual is rejected"
    );
}

// @spec TAX-ERR-003
#[test]
fn err_003_double_pay_rejected() {
    let key = fed_key("s1", "lotA", 2025);
    let accepted = vec![
        allocate(1, key.clone(), "acct"),
        move_(2, key.clone(), 30_000, date(2025, 6, 15)),
        pay(3, Jurisdiction::Federal, 2025, Quarter::Q2, 30_000, date(2025, 6, 16), vec![key.clone()]),
    ];
    // A second Pay covering the already-Paid accrual must be rejected.
    let candidate = pay(4, Jurisdiction::Federal, 2025, Quarter::Q3, 1, date(2025, 9, 1), vec![key]);
    assert_eq!(
        validate_event(&[one_gain()], &accepted, &candidate, &ctx()),
        Err(TaxError::DoublePay),
        "paying an already-Paid accrual twice is rejected"
    );
}

// ===========================================================================
// TAX-ERR-004 — a Pay covering a mismatched jurisdiction/year is rejected.
// ===========================================================================

// @spec TAX-ERR-004
#[test]
fn err_004_pay_cover_mismatch_rejected() {
    // The Pay is Federal/2025, but its covers list names a STATE key — a
    // jurisdiction mismatch → rejected.
    let fed = fed_key("s1", "lotA", 2025);
    let st = state_key("s1", "lotA", "DC", 2025);
    let accepted = vec![
        allocate(1, fed.clone(), "acct"),
        move_(2, fed.clone(), 30_000, date(2025, 6, 15)),
    ];
    let candidate = pay(
        3, Jurisdiction::Federal, 2025, Quarter::Q2, 30_000, date(2025, 6, 16),
        vec![st], // wrong jurisdiction
    );
    assert_eq!(
        validate_event(&[one_gain()], &accepted, &candidate, &ctx()),
        Err(TaxError::PayCoverMismatch),
        "a Pay covering a mismatched-jurisdiction accrual is rejected"
    );
}

// @spec TAX-ERR-004
#[test]
fn err_004_pay_cover_tax_year_mismatch_rejected() {
    // The Pay is Federal/2025; its covers list names a correctly-jurisdictioned
    // Federal key but a DIFFERENT tax_year (2024) — a tax_year mismatch → rejected
    // (the tax_year arm of TAX-ERR-004, distinct from the jurisdiction arm).
    let fed_2025 = fed_key("s1", "lotA", 2025);
    let fed_2024 = fed_key("s1", "lotA", 2024);
    let accepted = vec![
        allocate(1, fed_2025.clone(), "acct"),
        move_(2, fed_2025, 30_000, date(2025, 6, 15)),
    ];
    let candidate = pay(
        3, Jurisdiction::Federal, 2025, Quarter::Q2, 30_000, date(2025, 6, 16),
        vec![fed_2024], // right jurisdiction, wrong tax_year
    );
    assert_eq!(
        validate_event(&[one_gain()], &accepted, &candidate, &ctx()),
        Err(TaxError::PayCoverMismatch),
        "a Pay covering an accrual of a different tax_year is rejected"
    );
}

// ===========================================================================
// TAX-ERR-005 — an AmountOverride negative for a positive gain, or exceeding it,
// is rejected.
// ===========================================================================

// @spec TAX-ERR-005
#[test]
fn err_005_override_negative_for_positive_gain_rejected() {
    let key = fed_key("s1", "lotA", 2025);
    let candidate = override_(1, key, -1, "bad");
    assert_eq!(
        validate_event(&[one_gain()], &[], &candidate, &ctx()),
        Err(TaxError::BadOverride),
        "a negative override for a positive gain is rejected"
    );
}

// @spec TAX-ERR-005
#[test]
fn err_005_override_exceeding_gain_rejected() {
    // The gain is $1,000 (100_000c); an override of 100_001c exceeds it.
    let key = fed_key("s1", "lotA", 2025);
    let candidate = override_(1, key, 100_001, "too big");
    assert_eq!(
        validate_event(&[one_gain()], &[], &candidate, &ctx()),
        Err(TaxError::BadOverride),
        "an override exceeding the gain is rejected"
    );
}

// @spec TAX-ERR-005
#[test]
fn err_005_override_within_gain_accepted() {
    let key = fed_key("s1", "lotA", 2025);
    let candidate = override_(1, key, 12_345, "ok");
    assert_eq!(
        validate_event(&[one_gain()], &[], &candidate, &ctx()),
        Ok(()),
        "an override within [0, gain] is accepted"
    );
}

// @spec TAX-ERR-005, TAX-ERR-006
#[test]
fn err_005_override_against_a_loss_is_bounded_to_the_loss() {
    // A LOSS gain of −$1,000 (−100_000c). The applied override must lie in
    // [gain, 0] = [−100_000, 0]: an override more negative than the loss, or
    // positive, is rejected; one inside the interval (or 0) is accepted. This
    // closes the data-integrity gap where any value was accepted against a loss.
    let key = fed_key("sL", "lotL", 2025);
    let loss = gain("sL", 1, "lotL", "AMZN", date(2025, 1, 2), date(2025, 6, 1), -100_000, None);

    // More negative than the loss → rejected.
    assert_eq!(
        validate_event(&[loss.clone()], &[], &override_(1, key.clone(), -999_999, "too low"), &ctx()),
        Err(TaxError::BadOverride),
        "an override more negative than the loss is rejected"
    );
    // Positive against a loss → rejected (outside [gain, 0]).
    assert_eq!(
        validate_event(&[loss.clone()], &[], &override_(1, key.clone(), 5_000, "wrong sign"), &ctx()),
        Err(TaxError::BadOverride),
        "a positive override against a loss is rejected"
    );
    // Inside [gain, 0] → accepted (a within-loss negative accrual, or zero).
    assert_eq!(
        validate_event(&[loss.clone()], &[], &override_(1, key.clone(), -40_000, "ok"), &ctx()),
        Ok(()),
        "an override within [gain, 0] is accepted"
    );
    assert_eq!(
        validate_event(&[loss], &[], &override_(1, key, 0, "zero"), &ctx()),
        Ok(()),
        "a zero override against a loss is accepted"
    );
}

// @spec TAX-ERR-005, TAX-ERR-006
#[test]
fn err_005_override_against_orphan_key_must_be_zero() {
    // An override against an ORPHAN key (no backing RealizedGain) can encode no
    // amount: the bound is [0, 0]. A non-zero override is rejected so a logged
    // override cannot encode an arbitrary signed amount against a ghost key.
    let orphan = fed_key("ghost", "lotZ", 2025);
    assert_eq!(
        validate_event(&[one_gain()], &[], &override_(1, orphan.clone(), -5_000_000, "ghost"), &ctx()),
        Err(TaxError::BadOverride),
        "an arbitrary override against an orphan key is rejected"
    );
    assert_eq!(
        validate_event(&[one_gain()], &[], &override_(1, orphan, 0, "zero"), &ctx()),
        Ok(()),
        "a zero override against an orphan key is harmless"
    );
}
