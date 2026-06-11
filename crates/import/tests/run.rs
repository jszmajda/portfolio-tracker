//! Run mechanics (IMPORT-RUN-001..005): EventId derivation, the unique-id /
//! referential pre-pass, kernel validation, surviving-rows-only reconstruction,
//! excluded symbols, malformed rows, and the fresh / resumable commit.

mod common;
use common::*;

use import::testkit::{
    buy_row, founding_residency, fresh_store, position_row, sale_row, seeded_store,
};
use import::{
    check_commit_target, commit, derive_event_id, dry_run, pre_pass, reconstruct,
    validate_reconstruction, ImportError, LegacyWorkbook, RowCoord,
};
use ledger_core::Marks;
use pt_core::{Cents, MicroShares};
use store::{serde_rows, SheetsClient, Tab};

// @spec IMPORT-RUN-002
#[test]
fn event_id_is_derived_from_row_coordinate_plus_legacy_id_so_reused_ids_dont_collide() {
    // Same legacy id on two different rows → two DISTINCT EventIds (the row
    // coordinate disambiguates). Same (coord, id) → the SAME id (re-run stable).
    let c1 = RowCoord::new("Stock Actions", 10);
    let c2 = RowCoord::new("Stock Actions", 20);
    let a = derive_event_id(&c1, "DUP");
    let b = derive_event_id(&c2, "DUP");
    assert_ne!(
        a, b,
        "a reused legacy id on different rows yields distinct EventIds"
    );
    assert_eq!(
        derive_event_id(&c1, "DUP"),
        a,
        "the derivation is deterministic / re-run stable"
    );
}

// @spec IMPORT-RUN-002
#[test]
fn pre_pass_surfaces_a_duplicate_tranche_id() {
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(buy_row(
        "Stock Actions",
        2,
        "DUP",
        "GOOG",
        10,
        "1.00",
        "0",
        date(2020, 1, 1),
        PLATFORM,
    ));
    wb.actions.push(buy_row(
        "Stock Actions",
        3,
        "DUP",
        "GOOG",
        10,
        "1.00",
        "0",
        date(2020, 2, 1),
        PLATFORM,
    ));

    match pre_pass(&wb) {
        Err(ImportError::DuplicateTrancheId { tranche_id }) => assert_eq!(tranche_id, "DUP"),
        other => panic!("expected DuplicateTrancheId, got {other:?}"),
    }
}

// @spec IMPORT-RUN-002
#[test]
fn pre_pass_surfaces_a_sell_referencing_a_missing_tranche() {
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(goog_buy()); // GOOG-B1
    wb.sales.push(sale_row(
        "Stock Sales",
        9,
        "S-ORPHAN",
        "NO-SUCH-TRANCHE",
        "GOOG",
        5,
        "55.00",
        "0",
        date(2021, 6, 1),
        PLATFORM,
    ));
    match pre_pass(&wb) {
        Err(ImportError::MissingReferencedTranche { tranche_id, coord }) => {
            assert_eq!(tranche_id, "NO-SUCH-TRANCHE");
            assert_eq!(coord.row, 9);
        }
        other => panic!("expected MissingReferencedTranche, got {other:?}"),
    }
}

// @spec IMPORT-RUN-002
#[test]
fn a_clean_workbook_passes_the_pre_pass() {
    assert!(pre_pass(&combined_workbook()).is_ok());
}

// @spec IMPORT-RUN-002
#[test]
fn pre_pass_surfaces_a_duplicate_corporate_action() {
    // Two identical owner-supplied corporate actions for the same (symbol, date,
    // ratio): surfaced so the deterministic Split EventId cannot silently collide
    // (uniqueness does not rest on enumeration order).
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(buy_row(
        "Stock Actions",
        2,
        "A-B1",
        "AMZN",
        100,
        "100.00",
        "0",
        date(2020, 1, 1),
        PLATFORM,
    ));
    wb.corporate_actions
        .push(import::testkit::split_20_for_1("AMZN", date(2022, 6, 6)));
    wb.corporate_actions
        .push(import::testkit::split_20_for_1("AMZN", date(2022, 6, 6)));

    match pre_pass(&wb) {
        Err(ImportError::DuplicateCorporateAction { symbol, .. }) => assert_eq!(symbol, "AMZN"),
        other => panic!("expected DuplicateCorporateAction, got {other:?}"),
    }
}

// @spec IMPORT-RUN-003
#[test]
fn every_reconstructed_event_passes_the_kernel_validation() {
    // The combined workbook reconstructs to a kernel-valid log (validated through
    // ledger_core::validate, the same path as an entry append).
    let recon = reconstruct(&combined_workbook()).expect("reconstructs");
    validate_reconstruction(&recon).expect("the imported log is valid by construction");
}

// @spec IMPORT-RUN-004
#[test]
fn a_positions_only_symbol_with_no_source_rows_is_a_hard_error() {
    // PLTR appears in `Positions` but has NO Buy/Sell rows: the stream cannot be
    // fabricated, so it is a hard error surfaced for manual entry — never empty.
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(goog_buy());
    wb.sales.push(goog_sale());
    wb.positions.push(goog_position_legacy());
    wb.positions.push(position_row("PLTR", 100, 0, 50_000)); // no source rows!

    match dry_run(&wb, &Marks::new()) {
        Err(ImportError::PositionWithoutSourceRows { symbol }) => assert_eq!(symbol, "PLTR"),
        other => panic!("expected PositionWithoutSourceRows for PLTR, got {other:?}"),
    }
}

// @spec IMPORT-RUN-004, IMPORT-RUN-006
#[test]
fn a_reconstructed_symbol_without_a_legacy_positions_row_is_surfaced_and_blocks_commit() {
    // The INVERSE of the Positions-only case: a symbol reconstructed from surviving
    // rows but with NO legacy `Positions` row has no reconcile target. It must NOT
    // be silently committed unreconciled — it is surfaced (flagged) and blocks
    // commit until the owner adds the target.
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(goog_buy());
    wb.sales.push(goog_sale());
    wb.positions.push(goog_position_legacy());
    // MSFT has full source rows but is absent from `Positions`.
    wb.actions.push(buy_row(
        "Stock Actions",
        5,
        "M-B1",
        "MSFT",
        10,
        "200.00",
        "0",
        date(2020, 5, 1),
        PLATFORM,
    ));

    let report = dry_run(&wb, &Marks::new()).expect("dry-run still produces a report");
    let msft = report
        .symbols
        .iter()
        .find(|s| s.symbol == "MSFT")
        .expect("the unpositioned reconstructed MSFT is surfaced, not silently omitted");
    assert_eq!(
        msft.legacy_shares,
        MicroShares(0),
        "no legacy figure to reconcile against"
    );
    assert!(
        !report.commit_allowed,
        "an unreconciled reconstructed symbol blocks commit"
    );

    let mut store = fresh_store();
    assert!(
        commit(&mut store, &report.accept()).is_err(),
        "commit is refused"
    );
    assert_eq!(
        store.sheets().read_rows(Tab::Ledger).unwrap().len(),
        0,
        "writes nothing"
    );
}

// @spec IMPORT-RUN-004
#[test]
fn a_closed_position_with_surviving_rows_reconstructs() {
    // A fully-sold PLTR with its full Buy + Sell rows surviving reconstructs to a
    // closed position (zero open shares, the realized gain present).
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(buy_row(
        "Stock Actions",
        2,
        "P-B1",
        "PLTR",
        100,
        "10.00",
        "0",
        date(2020, 1, 1),
        PLATFORM,
    ));
    wb.sales.push(sale_row(
        "Stock Sales",
        2,
        "P-S1",
        "P-B1",
        "PLTR",
        100,
        "30.00",
        "0",
        date(2021, 1, 1),
        PLATFORM,
    ));
    wb.positions.push(position_row("PLTR", 0, 200_000, 0)); // realized 100×($30−$10)=$2000

    let recon = reconstruct(&wb).expect("reconstructs the surviving rows");
    validate_reconstruction(&recon).expect("kernel-valid");
    let snap = import::replay_reconstruction(&recon, &Marks::new());
    let pltr = snap.positions.get("PLTR").expect("PLTR position");
    assert_eq!(
        pltr.total_qty,
        MicroShares(0),
        "closed position has zero open shares"
    );
    assert_eq!(pltr.realized_pnl_cents, Cents(200_000), "realized $2,000");
}

// @spec IMPORT-RUN-005
#[test]
fn daily_script_hidden_symbols_are_imported_as_real_holdings() {
    // Symbols the legacy daily script hid from its view (`PrivCo`/`WXYZ` here)
    // were a view concern, not a ledger fact: they are imported as real
    // holdings, reconciled against the Stock Actions/Sales rows.
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(buy_row(
        "Stock Actions",
        2,
        "PC-B1",
        "PrivCo",
        50,
        "20.00",
        "0",
        date(2020, 1, 1),
        PLATFORM,
    ));
    wb.actions.push(buy_row(
        "Stock Actions",
        3,
        "W-B1",
        "WXYZ",
        30,
        "100.00",
        "0",
        date(2020, 1, 1),
        PLATFORM,
    ));
    wb.positions.push(position_row("PrivCo", 50, 0, 0));
    wb.positions.push(position_row("WXYZ", 30, 0, 0));

    let recon = reconstruct(&wb).expect("reconstructs");
    let symbols: std::collections::BTreeSet<String> = recon
        .ledger
        .iter()
        .filter_map(|e| match &e.event.kind {
            ledger_core::LedgerEventKind::Buy { symbol, .. } => Some(symbol.clone()),
            _ => None,
        })
        .collect();
    assert!(
        symbols.contains("PrivCo"),
        "the view-hidden PrivCo is imported"
    );
    assert!(symbols.contains("WXYZ"), "the view-hidden WXYZ is imported");
}

// @spec IMPORT-RUN-011
#[test]
fn a_genuinely_malformed_source_row_is_listed_not_dropped() {
    // A Buy row whose `$/share` is unparseable (not a derived-column #DIV/0!, but a
    // genuine garble) is listed for manual review, never silently dropped.
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(goog_buy());
    wb.sales.push(goog_sale());
    wb.positions.push(goog_position_legacy());
    wb.actions.push(buy_row(
        "Stock Actions",
        7,
        "GARBLE",
        "GOOG",
        1,
        "not-a-number",
        "0",
        date(2020, 3, 1),
        PLATFORM,
    ));

    let recon = reconstruct(&wb).expect("reconstruction continues, listing the malformed row");
    assert!(
        recon.malformed.iter().any(|m| m.coord.row == 7),
        "the malformed row is listed for manual review"
    );

    // And an outstanding malformed row BLOCKS commit: it was listed, not migrated,
    // so committing would silently drop its data. GOOG itself reconciles cleanly
    // (the dropped 1-share garble leaves GOOG at 80), so only the malformed list
    // can block here — and it must. (IMPORT-RUN-011)
    let report = dry_run(&wb, &Marks::new()).expect("dry-run still produces a report");
    assert!(
        !report.commit_allowed,
        "an outstanding malformed row blocks commit"
    );

    let mut store = fresh_store();
    match commit(&mut store, &report.accept()) {
        Err(ImportError::CommitBlockedByReconciliation { .. }) => {}
        other => panic!("expected the commit to be blocked, got {other:?}"),
    }
    assert_eq!(
        store.sheets().read_rows(Tab::Ledger).unwrap().len(),
        0,
        "a blocked commit writes nothing"
    );
}

// @spec IMPORT-RUN-001
#[test]
fn commit_seeds_a_fresh_workbook_and_writes_the_reconstructed_events() {
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(goog_buy());
    wb.sales.push(goog_sale());
    wb.positions.push(goog_position_legacy());

    let report = dry_run(&wb, &Marks::new()).expect("dry-run");
    assert!(report.commit_allowed);

    let mut store = fresh_store();
    let commit_report = commit(&mut store, &report.accept()).expect("commit to a fresh store");
    // Both the Buy and the Sell landed.
    assert_eq!(
        commit_report.appended.len(),
        2,
        "two ledger events appended"
    );
    assert!(commit_report.skipped.is_empty());

    let ledger_rows = store.sheets().read_rows(Tab::Ledger).unwrap();
    assert_eq!(
        ledger_rows.len(),
        2,
        "the workbook now holds the two reconstructed events"
    );
}

// @spec IMPORT-RUN-001
#[test]
fn commit_resumes_idempotently_into_an_own_events_only_target() {
    // Commit once, then commit AGAIN against the same store: the second commit
    // re-appends nothing (every event idempotently skipped), proving an own-events-
    // only target resumes idempotently rather than double-appending.
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(goog_buy());
    wb.sales.push(goog_sale());
    wb.positions.push(goog_position_legacy());

    let report = dry_run(&wb, &Marks::new()).expect("dry-run");
    let mut store = fresh_store();
    let first = commit(&mut store, &report.accept()).expect("first commit");
    assert_eq!(first.appended.len(), 2);

    let second = commit(&mut store, &report.accept()).expect("resume commit");
    assert!(second.appended.is_empty(), "a resume re-appends nothing");
    assert_eq!(
        second.skipped.len(),
        2,
        "both events are idempotently skipped"
    );
    assert_eq!(
        store.sheets().read_rows(Tab::Ledger).unwrap().len(),
        2,
        "no duplicates"
    );
}

// @spec IMPORT-RUN-001
#[test]
fn commit_resumes_a_genuine_partial_prefix_reappending_only_the_missing_tail_in_order() {
    // A commit interrupted partway: ONLY the first reconstructed ledger event (the
    // Buy, Seq 1) landed before the network dropped. The resume must skip that
    // seeded prefix and re-append the MISSING TAIL (the Sell) in ascending Seq
    // order — writing only the missing EventIds, with no duplicates. (IMPORT-RUN-001)
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(goog_buy());
    wb.sales.push(goog_sale());
    wb.positions.push(goog_position_legacy());

    let report = dry_run(&wb, &Marks::new()).expect("dry-run");
    let mut ordered: Vec<_> = report.reconstruction.ledger.iter().collect();
    ordered.sort_by_key(|e| e.event.seq.0);
    assert_eq!(ordered.len(), 2, "the GOOG stream is a Buy then a Sell");

    // Seed ONLY the first event (the prefix that landed before the interruption).
    let prefix_id = ordered[0].event_id.clone();
    let tail_id = ordered[1].event_id.clone();
    let seeded_prefix = serde_rows::ledger_to_row(&ordered[0].event);
    let mut store = seeded_store(vec![seeded_prefix], vec![]);
    assert_eq!(
        store.sheets().read_rows(Tab::Ledger).unwrap().len(),
        1,
        "the partial prefix is present"
    );

    // Resume: the seeded prefix is skipped; the missing tail re-appends.
    let resumed =
        commit(&mut store, &report.accept()).expect("resume into a genuine partial prefix");
    assert_eq!(
        resumed.skipped,
        vec![prefix_id],
        "the seeded prefix lands in `skipped`"
    );
    assert_eq!(
        resumed.appended,
        vec![tail_id],
        "the missing tail re-appends, in order"
    );

    // The final log holds exactly the full reconstruction, with no duplicates.
    let rows = store.sheets().read_rows(Tab::Ledger).unwrap();
    assert_eq!(
        rows.len(),
        2,
        "the resumed log equals the full reconstruction, no duplicates"
    );
    let seqs: Vec<&str> = rows.iter().map(|r| r.get("Seq")).collect();
    assert_eq!(
        seqs,
        vec!["1", "2"],
        "Seqs are dense and ascending in the reconstructed fold order"
    );
}

// @spec IMPORT-RUN-001
#[test]
fn a_target_holding_foreign_events_is_refused() {
    // A target workbook already holding an event that is NOT this import's own
    // deterministic event is refused (only a fresh / own-events-only target is
    // permitted).
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(goog_buy());
    wb.sales.push(goog_sale());
    wb.positions.push(goog_position_legacy());

    let recon = reconstruct(&wb).expect("reconstructs");
    let expected_ids: std::collections::BTreeSet<String> =
        recon.ledger.iter().map(|e| e.event_id.clone()).collect();
    let expected_tax = import::expected_tax_ids(&recon);

    // Seed a foreign ledger row (an EventId NOT among the reconstruction's).
    let mut foreign = store::Row::new();
    foreign.set("Seq", "1");
    foreign.set("EventId", "foreign-event-not-ours");
    foreign.set("Date", "18000");
    foreign.set("Kind", "Buy");
    foreign.set("LotId", "X");
    foreign.set("Symbol", "X");
    foreign.set("Qty", "1000000");
    foreign.set("UnitPriceCents", "100");
    foreign.set("FeesCents", "0");
    foreign.set("Platform", "Other");
    let store = import::testkit::seeded_store(vec![foreign], vec![]);

    match check_commit_target(&store, &expected_ids, &expected_tax) {
        Err(ImportError::TargetNotEmpty) => {}
        other => panic!("expected TargetNotEmpty, got {other:?}"),
    }
}

// @spec IMPORT-RUN-001
#[test]
fn a_brand_new_workbook_with_no_event_log_tabs_is_the_fresh_target() {
    // The REAL first commit runs against a workbook whose event-log tabs have
    // never been created (store bootstraps them on the first append,
    // STORE-WRITE-009). The target check must read a missing tab as ZERO rows —
    // the fresh workbook this spec names — never as an error.
    use store::cache::InMemoryCache;
    use store::testkit::InMemorySheets;
    use store::{NoopLock, Store};

    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(goog_buy());
    wb.sales.push(goog_sale());
    wb.positions.push(goog_position_legacy());

    let report = dry_run(&wb, &Marks::new()).expect("dry-run");
    assert!(report.commit_allowed);

    let mut store = Store::new(
        InMemorySheets::fresh_workbook(),
        NoopLock::new(),
        InMemoryCache::new(),
    );
    let commit_report =
        commit(&mut store, &report.accept()).expect("the first-ever commit bootstraps the tabs");
    assert_eq!(
        commit_report.appended.len(),
        2,
        "the events landed on the fresh workbook"
    );
    assert_eq!(store.sheets().read_rows(Tab::Ledger).unwrap().len(), 2);
}

// @spec IMPORT-RUN-001
#[test]
fn a_target_holding_a_foreign_tax_row_is_refused_not_admitted_by_the_tax_prefix() {
    // A pre-existing FOREIGN tax event is store-assigned the SAME `tax-…` prefix as
    // this import's own events, so a loose prefix test would let it through. The tax
    // tab is gated against the EXACT expected tax-id set, so a foreign `tax-…` row
    // (not one this reconstruction would write) is detected and blocks.
    let mut wb = combined_workbook();
    // Keep only GOOG/AMZN with their closed year; the tax tab will carry the
    // migration lifecycle whose ids are the expected set.
    wb.corrections.clear();

    let recon = reconstruct(&wb).expect("reconstructs with the closed year");
    let expected_ids: std::collections::BTreeSet<String> =
        recon.ledger.iter().map(|e| e.event_id.clone()).collect();
    let expected_tax = import::expected_tax_ids(&recon);
    assert!(
        !expected_tax.is_empty(),
        "the migration lifecycle has expected tax ids"
    );

    // Seed a FOREIGN tax row carrying a store-shaped `tax-…` id NOT in the expected
    // set.
    let mut foreign_tax = store::Row::new();
    foreign_tax.set("Seq", "1");
    foreign_tax.set("EventId", "tax-deadbeefdeadbeef");
    foreign_tax.set("Kind", "SeedMigration");
    let store = import::testkit::seeded_store(vec![], vec![foreign_tax]);

    match check_commit_target(&store, &expected_ids, &expected_tax) {
        Err(ImportError::TargetNotEmpty) => {}
        other => panic!("expected TargetNotEmpty for a foreign tax-… row, got {other:?}"),
    }
}
