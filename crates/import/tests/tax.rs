//! Historical tax & residency (IMPORT-TAX-001, IMPORT-TAX-002).

mod common;
use common::*;

use import::testkit::{buy_row, founding_residency};
use import::{reconstruct, stamp_residency, ImportError, LegacyWorkbook};
use ledger_core::LedgerEventKind;
use pt_core::Cents;
use tax::{AccrualState, TaxEventKind};

// @spec IMPORT-TAX-001
#[test]
fn each_historical_sell_is_stamped_from_residency_on_sale_date() {
    // A sale in 2021 over an NJ-then-DC timeline (DC from 2018) stamps DC; a sale
    // in 2012 stamps NJ.
    let tl = nj_then_dc();
    assert_eq!(stamp_residency(&tl, date(2021, 6, 1)), Some("DC".to_string()));
    assert_eq!(stamp_residency(&tl, date(2012, 6, 1)), Some("NJ".to_string()));
}

// @spec IMPORT-TAX-001
#[test]
fn reconstructed_sell_carries_the_residency_stamp() {
    let mut wb = LegacyWorkbook {
        residency: nj_then_dc(),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(goog_buy()); // 2020-01-15
    wb.sales.push(goog_sale()); // 2021-06-01 → DC

    let recon = reconstruct(&wb).expect("reconstructs");
    let sell = recon.ledger.iter().find_map(|e| match &e.event.kind {
        LedgerEventKind::Sell { accrues_to_state, .. } => Some(accrues_to_state.clone()),
        _ => None,
    }).expect("a Sell event");
    assert_eq!(sell, Some("DC".to_string()), "the 2021 sale stamps DC");
}

// @spec IMPORT-TAX-001
#[test]
fn owner_asserted_founding_state_is_recorded_when_oldest_sale_predates_recall() {
    // The founding residency entry (a best-guess, flagged) is what keeps the
    // oldest sale out of `residency_on`'s undefined pre-history region: with a
    // founding DC entry effective 2015-01-01, a 2016 sale stamps DC and the
    // reconstruction succeeds. (The flag rides the workbook.)
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        founding_residency_asserted: true,
        ..LegacyWorkbook::default()
    };
    wb.actions.push(goog_buy());
    wb.sales.push(goog_sale()); // 2021-06-01 ≥ founding → DC
    assert!(wb.founding_residency_asserted, "the founding state is flagged as asserted");
    let recon = reconstruct(&wb).expect("reconstructs over an asserted founding entry");
    assert!(recon.ledger.iter().any(|e| matches!(e.event.kind, LedgerEventKind::Sell { .. })));
}

// @spec IMPORT-TAX-001
#[test]
fn a_sale_predating_the_founding_residency_entry_is_a_hard_error() {
    // The founding entry (DC, effective 2018-01-01) postdates the oldest SALE
    // (2016): `residency_on(sale_date)` would fall in the undefined pre-history
    // region, so the founding-residency precondition fails as a hard error rather
    // than stamping a sale from an undefined timeline. (IMPORT-TAX-001)
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2018, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(buy_row("Stock Actions", 2, "B1", "GOOG", 100, "10.00", "0", date(2015, 1, 1), PLATFORM));
    wb.sales.push(import::testkit::sale_row(
        "Stock Sales", 2, "S1", "B1", "GOOG", 10, "30.00", "0", date(2016, 6, 1), PLATFORM,
    ));

    match reconstruct(&wb) {
        Err(ImportError::MissingFoundingResidency) => {}
        other => panic!("expected MissingFoundingResidency, got {other:?}"),
    }
}

// @spec IMPORT-TAX-001
#[test]
fn an_early_buy_with_later_sales_is_not_blocked_residency_keys_on_the_oldest_sale() {
    // The precondition keys on the oldest SALE, not the oldest event: residency
    // stamps only Sells. An early Buy (2016) that predates the founding entry
    // (2018) is fine as long as every SALE is on/after it (the 2020 sale stamps
    // DC). (IMPORT-TAX-001)
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2018, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(buy_row("Stock Actions", 2, "B1", "GOOG", 100, "10.00", "0", date(2016, 1, 1), PLATFORM));
    wb.sales.push(import::testkit::sale_row(
        "Stock Sales", 2, "S1", "B1", "GOOG", 10, "30.00", "0", date(2020, 6, 1), PLATFORM,
    ));

    let recon = reconstruct(&wb).expect("an early Buy with on-timeline Sells reconstructs");
    let stamp = recon.ledger.iter().find_map(|e| match &e.event.kind {
        LedgerEventKind::Sell { accrues_to_state, .. } => Some(accrues_to_state.clone()),
        _ => None,
    }).expect("a Sell");
    assert_eq!(stamp, Some("DC".to_string()), "the 2020 sale stamps DC");
}

// @spec IMPORT-TAX-002
#[test]
fn closed_year_seeds_one_combined_migration_accrual_via_override_allocate_move_pay() {
    // The combined workbook's DC 2023 closed year ($7,200 legacy actual) is seeded
    // as a single combined migration accrual per (jurisdiction, tax_year), driven
    // AmountOverride → Allocate → Move → Pay, leaving outstanding = 0.
    let wb = combined_workbook();
    let recon = reconstruct(&wb).expect("reconstructs with the closed year");

    // The tax-event lifecycle for the DC 2023 migration accrual.
    let kinds: Vec<&TaxEventKind> = recon.tax.iter().map(|e| &e.kind).collect();

    // Exactly one SeedMigration / one combined accrual for DC 2023.
    let seeds: Vec<&TaxEventKind> = kinds.iter().copied().filter(|k| {
        matches!(k, TaxEventKind::SeedMigration { jurisdiction: config::Jurisdiction::State(s), tax_year, .. }
            if s == "DC" && tax_year.0 == 2023)
    }).collect();
    assert_eq!(seeds.len(), 1, "a single combined migration seed for DC 2023");
    if let TaxEventKind::SeedMigration { applied_amount_cents, .. } = seeds[0] {
        assert_eq!(*applied_amount_cents, Cents(720_000), "set to the legacy actual");
    }

    // The full lifecycle is present: Allocate, Move, Pay for the same key.
    assert!(kinds.iter().any(|k| matches!(k, TaxEventKind::Allocate { .. })), "Allocate present");
    assert!(kinds.iter().any(|k| matches!(k, TaxEventKind::Move { .. })), "Move present");
    assert!(kinds.iter().any(|k| matches!(k, TaxEventKind::Pay { .. })), "Pay present");

    // And the lifecycle drives the migration accrual to Paid (outstanding = 0):
    // build the migration AccrualKey and fold the events via `tax`.
    let key = tax::AccrualKey {
        sale_id: String::new(),
        lot_id: String::new(),
        jurisdiction: config::Jurisdiction::State("DC".to_string()),
        tax_year: config::TaxYear(2023),
    };
    let accruals = tax::compute_accruals(&[], &recon.tax, &dc_2023_ctx());
    let migration = accruals.iter().find(|a| a.key == key).expect("the migration accrual");
    assert!(matches!(migration.state, AccrualState::Paid { .. }), "driven to Paid");
    assert_eq!(migration.applied_cents, Some(Cents(720_000)));
}

// @spec IMPORT-RUN-003, IMPORT-TAX-003
#[test]
fn every_reconstructed_tax_event_passes_the_tax_kernel_validation() {
    // IMPORT-RUN-003: EVERY reconstructed event (not just the ledger events) is
    // validated through the kernel — the same `tax::validate_event` gate an `entry`
    // tax append uses. The closed-year migration lifecycle (SeedMigration →
    // AmountOverride → Allocate → Move → Pay, against a combined key with NO backing
    // RealizedGain) must validate by construction.
    let wb = combined_workbook();
    let recon = reconstruct(&wb).expect("reconstructs with the closed year");
    assert!(!recon.tax.is_empty(), "the migration lifecycle is present");

    // The whole reconstruction (ledger + tax) is kernel-valid.
    import::validate_reconstruction(&recon).expect("the imported log is valid by construction");

    // And explicitly: each tax event folds through `tax::validate_event` in Seq
    // order against the accepted-so-far prefix, with empty backing gains and a
    // regime-agnostic migration context.
    let mut sorted: Vec<&tax::TaxEvent> = recon.tax.iter().collect();
    sorted.sort_by_key(|e| e.seq.0);
    let mut accepted: Vec<tax::TaxEvent> = Vec::new();
    for te in sorted {
        let year = match &te.kind {
            TaxEventKind::SeedMigration { tax_year, .. } | TaxEventKind::Pay { tax_year, .. } => {
                *tax_year
            }
            TaxEventKind::Allocate { accrual_key, .. }
            | TaxEventKind::Move { accrual_key, .. }
            | TaxEventKind::AmountOverride { accrual_key, .. } => accrual_key.tax_year,
        };
        tax::validate_event(&[], &accepted, te, &tax::migration_context(year))
            .expect("each migration tax event validates through the tax kernel gate");
        accepted.push(te.clone());
    }
}

/// A minimal `TaxContext` for DC 2023 so the migration accrual folds. The migration
/// figure is regime-agnostic; brackets are not exercised by the seeded amount.
fn dc_2023_ctx() -> tax::TaxContext {
    use config::{BracketRow, BracketSet, BracketState, Niit, Ppm};
    use pt_core::Date;
    let flat = |rate: i64| BracketSet {
        rows: vec![BracketRow { lower_threshold_cents: Cents(0), rate_ppm: Ppm(rate) }],
        last_verified: Date(0),
        source_note: "test".to_string(),
    };
    let fed = tax::ResolvedJurisdiction {
        jurisdiction: config::Jurisdiction::Federal,
        ordinary: Some(flat(370_000)),
        federal_long_term: Some(flat(200_000)),
        niit: Some(Niit { rate_ppm: Ppm(38_000), magi_threshold_cents: Cents(0) }),
        ordinary_income_cents: Cents(0),
        state: BracketState::Verified,
    };
    let dc = tax::ResolvedJurisdiction {
        jurisdiction: config::Jurisdiction::State("DC".to_string()),
        ordinary: Some(flat(85_000)),
        federal_long_term: None,
        niit: None,
        ordinary_income_cents: Cents(0),
        state: BracketState::Verified,
    };
    let mut states = std::collections::BTreeMap::new();
    states.insert("DC".to_string(), dc);
    tax::TaxContext {
        tax_year: config::TaxYear(2023),
        federal: fed,
        states,
        de_minimis_cents: Cents(100),
        residency_default: Some("DC".to_string()),
    }
}
