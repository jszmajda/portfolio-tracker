//! Dry-run & reconciliation (IMPORT-RECON-001..004).

mod common;
use common::*;

use import::testkit::{buy_row, founding_residency, fresh_store, position_row, sale_row, vest_row};
use import::{
    commit, dry_run, predict_intended_delta_cents, ImportError, LegacyWorkbook, ShareVerdict,
    Verdict,
};
use ledger_core::Marks;
use pt_core::{Cents, MicroShares};
use store::{SheetsClient, Tab};

// @spec IMPORT-RECON-001
#[test]
fn dry_run_reconstructs_validates_reconciles_and_reports_without_writing() {
    // The dry-run produces a report with a reconciliation per symbol and writes
    // nothing (it takes no `store`). GOOG (clean) reconciles MATCHED.
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(goog_buy());
    wb.sales.push(goog_sale());
    wb.positions.push(goog_position_legacy());

    let mut marks = Marks::new();
    marks.insert("GOOG".to_string(), Cents(6000));

    let report = dry_run(&wb, &marks).expect("dry-run succeeds");
    let goog = report
        .symbols
        .iter()
        .find(|s| s.symbol == "GOOG")
        .expect("GOOG line");
    assert!(
        matches!(goog.verdict, Verdict::Matched),
        "clean GOOG reconciles matched"
    );
    assert_eq!(
        goog.reconstructed_realized_cents,
        Cents(10_000),
        "20 × ($55−$50) = $100"
    );
    assert!(
        report.commit_allowed,
        "a fully-matched dry-run permits commit"
    );
}

// @spec IMPORT-RECON-002
#[test]
fn amzn_rsu_basis_divergence_is_classified_intended_at_symbol_aggregate() {
    // AMZN: legacy realized was computed on the $0 RSU basis; the new model charges
    // the FMV basis. The residual is exactly the independently predicted RSU-basis
    // delta = Σ vest FMV value over the sold/vested shares, so it classifies as an
    // INTENDED divergence (not unexplained), and the split delta is exactly $0.
    let wb = amzn_only_workbook();
    let mut marks = Marks::new();
    marks.insert("AMZN".to_string(), Cents(12_000)); // $120 post-split

    let report = dry_run(&wb, &marks).expect("dry-run succeeds");
    let amzn = report
        .symbols
        .iter()
        .find(|s| s.symbol == "AMZN")
        .expect("AMZN line");
    match &amzn.verdict {
        Verdict::IntendedDivergence {
            predicted_cents, ..
        } => {
            // The predicted delta is computed independently of the residual.
            let independent = predict_intended_delta_cents(
                &import::reconstruct(&wb).unwrap(),
                &"AMZN".to_string(),
            );
            assert_eq!(
                *predicted_cents, independent,
                "the verdict's predicted delta equals the independently predicted one"
            );
        }
        other => panic!("expected IntendedDivergence for AMZN, got {other:?}"),
    }
}

// @spec IMPORT-RECON-002
#[test]
fn split_delta_is_exactly_zero_and_predicted_independently_of_the_residual() {
    // A pure-split symbol (a buy, a 20:1 split, no RSU) has an intended delta of
    // exactly $0 (splits are basis-neutral) — predicted from the corporate action,
    // never back-labelled from a residual.
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(buy_row(
        "Stock Actions",
        2,
        "TSLA-B1",
        "TSLA",
        10,
        "900.00",
        "0",
        date(2020, 1, 1),
        PLATFORM,
    ));
    wb.corporate_actions
        .push(import::testkit::split_20_for_1("TSLA", date(2020, 8, 31)));

    let recon = import::reconstruct(&wb).expect("reconstructs");
    let predicted = predict_intended_delta_cents(&recon, &"TSLA".to_string());
    assert_eq!(
        predicted,
        Cents(0),
        "a split is basis-neutral: predicted delta is exactly $0"
    );
}

// @spec IMPORT-RECON-002
#[test]
fn realized_only_reconciliation_without_a_mark_uses_the_consumed_vest_basis_delta() {
    // No mark supplied → the dollar reconciliation compares REALIZED P&L only, and
    // the predicted delta is the realized-portion RSU delta: −Σ(FMV basis consumed
    // on the SOLD vest shares), NOT the full vest FMV. A vest of 10 @ $100 FMV with
    // a partial sale of 4 @ $150:
    //   new realized  = 4 × ($150 − $100) = $200  = 20_000c
    //   legacy realized (on the $0 basis) = 4 × $150 = $600 = 60_000c
    //   diff          = 20_000 − 60_000 = −40_000c
    //   predicted     = −(4 × $100 FMV basis consumed) = −40_000c
    //   residual      = diff − predicted = 0 → IntendedDivergence(−40_000).
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(vest_row(
        "Stock Actions",
        2,
        "RV1",
        "NVDA",
        10,
        "100.00",
        date(2021, 1, 1),
        PLATFORM,
    ));
    wb.sales.push(sale_row(
        "Stock Sales",
        2,
        "RV1-S1",
        "RV1",
        "NVDA",
        4,
        "150.00",
        "0",
        date(2022, 6, 1),
        PLATFORM,
    ));
    // Legacy realized on the $0 RSU basis = $600; legacy unrealized irrelevant here.
    wb.positions.push(position_row("NVDA", 6, 60_000, 0));

    // NO mark for NVDA → realized-only comparison.
    let report = dry_run(&wb, &Marks::new()).expect("dry-run");
    let nvda = report
        .symbols
        .iter()
        .find(|s| s.symbol == "NVDA")
        .expect("NVDA line");
    match &nvda.verdict {
        Verdict::IntendedDivergence {
            predicted_cents, ..
        } => {
            assert_eq!(
                *predicted_cents,
                Cents(-40_000),
                "realized-only predicted delta = −(FMV basis consumed on the 4 sold vest shares)"
            );
        }
        other => panic!("expected IntendedDivergence (realized-only), got {other:?}"),
    }
    assert_eq!(
        nvda.reconstructed_realized_cents,
        Cents(20_000),
        "new realized $200"
    );
    assert!(
        report.commit_allowed,
        "the realized-only RSU divergence is intended, not blocking"
    );
}

// @spec IMPORT-RECON-003
#[test]
fn an_unexplained_divergence_blocks_commit() {
    // Corrupt the legacy GOOG realized figure so the reconstructed realized differs
    // by an amount that is NOT predicted by any RSU/split intent. The verdict is
    // Unexplained and commit is blocked.
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(goog_buy());
    wb.sales.push(goog_sale());
    // Legacy claims realized $999.00 where the reconstruction yields $100 — a
    // $899 unattributed residual, far beyond any tolerance, with no RSU/split.
    wb.positions.push(position_row("GOOG", 80, 99_900, 80_000));

    let mut marks = Marks::new();
    marks.insert("GOOG".to_string(), Cents(6000));

    let report = dry_run(&wb, &marks).expect("dry-run still produces a report");
    let goog = report
        .symbols
        .iter()
        .find(|s| s.symbol == "GOOG")
        .expect("GOOG line");
    assert!(
        matches!(goog.verdict, Verdict::Unexplained { .. }),
        "a real residual is Unexplained"
    );
    assert!(
        !report.commit_allowed,
        "an unexplained divergence blocks commit"
    );

    // And `commit()` on a blocked report is REFUSED with the dedicated
    // CommitBlockedByReconciliation variant (NOT TargetNotEmpty), naming the
    // unexplained symbol — and writes NOTHING. (IMPORT-RECON-003)
    let mut store = fresh_store();
    match commit(&mut store, &report.accept()) {
        Err(ImportError::CommitBlockedByReconciliation {
            unexplained_symbols,
            flagged_symbols,
            malformed_rows,
        }) => {
            assert!(
                unexplained_symbols.contains(&"GOOG".to_string()),
                "GOOG is the blocking symbol"
            );
            assert!(
                flagged_symbols.is_empty(),
                "no share residual is flagged here"
            );
            assert!(malformed_rows.is_empty(), "no malformed row here");
        }
        other => panic!("expected CommitBlockedByReconciliation, got {other:?}"),
    }
    assert_eq!(
        store.sheets().read_rows(Tab::Ledger).unwrap().len(),
        0,
        "a blocked commit writes nothing"
    );
}

// @spec IMPORT-RECON-003
#[test]
fn a_flagged_share_residual_blocks_commit_with_the_reconciliation_variant() {
    // A large share mismatch flags the share verdict, blocking commit. `commit()`
    // is refused with CommitBlockedByReconciliation naming the flagged symbol.
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(goog_buy());
    wb.sales.push(goog_sale());
    // Legacy claims 90 shares where the reconstruction yields 80 — flagged.
    wb.positions.push(position_row("GOOG", 90, 10_000, 90_000));

    let report = dry_run(&wb, &Marks::new()).expect("dry-run");
    assert!(
        !report.commit_allowed,
        "a flagged share residual blocks commit"
    );

    let mut store = fresh_store();
    match commit(&mut store, &report.accept()) {
        Err(ImportError::CommitBlockedByReconciliation {
            flagged_symbols, ..
        }) => {
            assert!(
                flagged_symbols.contains(&"GOOG".to_string()),
                "GOOG share residual is flagged"
            );
        }
        other => panic!("expected CommitBlockedByReconciliation, got {other:?}"),
    }
    assert_eq!(
        store.sheets().read_rows(Tab::Ledger).unwrap().len(),
        0,
        "writes nothing"
    );
}

// @spec IMPORT-RECON-004
#[test]
fn shares_reconcile_per_symbol_within_micro_share_tolerance() {
    // GOOG reconstructs to exactly 80 shares, matching the legacy Positions count.
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(goog_buy());
    wb.sales.push(goog_sale());
    wb.positions.push(goog_position_legacy());

    let report = dry_run(&wb, &Marks::new()).expect("dry-run succeeds");
    let goog = report
        .symbols
        .iter()
        .find(|s| s.symbol == "GOOG")
        .expect("GOOG line");
    assert_eq!(goog.reconstructed_shares, MicroShares(80_000_000));
    assert_eq!(goog.legacy_shares, MicroShares(80_000_000));
    assert!(
        matches!(goog.share_verdict, ShareVerdict::Matched),
        "shares reconcile matched"
    );
}

// @spec IMPORT-RECON-004
#[test]
fn sub_threshold_residual_on_a_closed_position_snaps_via_closing_adjustment() {
    // A fully-sold (closed) position whose legacy Positions count is a hair off
    // (a sub-micro-share residual) snaps via a closing adjustment, not a flag.
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(buy_row(
        "Stock Actions",
        2,
        "C-B1",
        "CLSD",
        100,
        "10.00",
        "0",
        date(2020, 1, 1),
        PLATFORM,
    ));
    wb.sales.push(sale_row(
        "Stock Sales",
        2,
        "C-S1",
        "C-B1",
        "CLSD",
        100,
        "12.00",
        "0",
        date(2021, 1, 1),
        PLATFORM,
    ));
    // Legacy Positions has a tiny residual (0.0005 share) on the closed position.
    wb.positions.push(import::LegacyPositionRow {
        symbol: "CLSD".to_string(),
        shares: MicroShares(500), // 0.0005 share, below SHARE_TOLERANCE_MICRO
        realized_pnl_cents: Cents(20_000),
        unrealized_cents: Cents(0),
    });

    let report = dry_run(&wb, &Marks::new()).expect("dry-run succeeds");
    let clsd = report
        .symbols
        .iter()
        .find(|s| s.symbol == "CLSD")
        .expect("CLSD line");
    match &clsd.share_verdict {
        ShareVerdict::SnappedClosingAdjustment { adjustment } => {
            assert_eq!(
                *adjustment,
                MicroShares(500),
                "the sub-threshold residual is snapped"
            );
        }
        other => panic!("expected a snapped closing adjustment, got {other:?}"),
    }
}

// @spec IMPORT-RECON-004
#[test]
fn a_larger_share_residual_is_flagged() {
    // A large share mismatch (10 shares) is flagged, not snapped, and blocks commit.
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(goog_buy());
    wb.sales.push(goog_sale());
    // Legacy claims 90 shares where the reconstruction yields 80.
    wb.positions.push(position_row("GOOG", 90, 10_000, 90_000));

    let report = dry_run(&wb, &Marks::new()).expect("dry-run succeeds");
    let goog = report
        .symbols
        .iter()
        .find(|s| s.symbol == "GOOG")
        .expect("GOOG line");
    assert!(
        matches!(goog.share_verdict, ShareVerdict::Flagged { .. }),
        "a large residual is flagged"
    );
    assert!(
        !report.commit_allowed,
        "a flagged share residual blocks commit"
    );
}

// @spec IMPORT-RECON-005
#[test]
fn commit_is_refused_without_the_accepted_report_token_even_when_commit_allowed() {
    // The owner-acceptance token (`AcceptedReport`) is DISTINCT from the auto-
    // computed `commit_allowed` safety gate: clearing the gate never implies
    // acceptance. A clean GOOG dry-run clears the gate (`commit_allowed == true`),
    // yet a commit WITHOUT the owner's acceptance token is refused and writes
    // nothing — the two gates are independently satisfied. (IMPORT-RECON-005)
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(goog_buy());
    wb.sales.push(goog_sale());
    wb.positions.push(goog_position_legacy());

    let report = dry_run(&wb, &Marks::new()).expect("dry-run");
    assert!(
        report.commit_allowed,
        "the clean GOOG reconciliation clears the safety gate"
    );

    // The auto gate is clear, but the owner has NOT accepted: a review token without
    // acceptance must refuse the commit, independently of `commit_allowed`.
    let not_accepted = report.review();
    let mut store = fresh_store();
    match commit(&mut store, &not_accepted) {
        Err(ImportError::OwnerAcceptanceRequired) => {}
        other => panic!(
            "expected OwnerAcceptanceRequired (the gate being clear never implies acceptance), got {other:?}"
        ),
    }
    assert_eq!(
        store.sheets().read_rows(Tab::Ledger).unwrap().len(),
        0,
        "a commit refused for want of owner acceptance writes nothing"
    );

    // With the SAME clean report explicitly accepted by the owner, the commit goes
    // through: both gates now independently satisfied.
    let accepted = report.accept();
    let commit_report =
        commit(&mut store, &accepted).expect("an accepted, gate-clear report commits");
    assert_eq!(commit_report.appended.len(), 2, "the Buy and the Sell land");
    assert_eq!(store.sheets().read_rows(Tab::Ledger).unwrap().len(), 2);
}

// @spec IMPORT-RECON-005
#[test]
fn owner_acceptance_never_bypasses_the_safety_gate() {
    // The other independence direction: owner acceptance must NOT bypass the auto-
    // computed safety gate. An accepted token over a report whose gate is BLOCKED
    // (an unexplained dollar divergence) is still refused by `commit_allowed` —
    // acceptance and the gate are independently required. (IMPORT-RECON-005)
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(goog_buy());
    wb.sales.push(goog_sale());
    // A large unattributed residual: the dollar verdict is Unexplained, blocking.
    wb.positions.push(position_row("GOOG", 80, 99_900, 80_000));

    let mut marks = Marks::new();
    marks.insert("GOOG".to_string(), Cents(6000));

    let report = dry_run(&wb, &marks).expect("dry-run");
    assert!(
        !report.commit_allowed,
        "the unexplained divergence blocks the safety gate"
    );

    // Even with the owner's explicit acceptance, the blocked gate refuses the commit.
    let accepted = report.accept();
    let mut store = fresh_store();
    match commit(&mut store, &accepted) {
        Err(ImportError::CommitBlockedByReconciliation { unexplained_symbols, .. }) => {
            assert!(unexplained_symbols.contains(&"GOOG".to_string()));
        }
        other => panic!("expected CommitBlockedByReconciliation (acceptance never bypasses the gate), got {other:?}"),
    }
    assert_eq!(
        store.sheets().read_rows(Tab::Ledger).unwrap().len(),
        0,
        "writes nothing"
    );
}

// @spec IMPORT-RECON-006
#[test]
fn commit_allowed_clears_only_when_no_blocking_condition_is_present() {
    // The auto-computed `commit_allowed` safety gate clears ONLY when the
    // reconciliation carries no commit-blocking condition. The combined workbook
    // (clean GOOG, AMZN whose only divergence is the INTENDED RSU/split delta, a
    // closed-year migration) has: no Unexplained dollar verdict, no Flagged share
    // residual, no malformed source rows, no `Positions`-only symbol, and no
    // reconstructed-but-unpositioned symbol — the complete blocking set is empty —
    // so the gate clears. (The individual blocking members are exercised by the
    // sibling Unexplained / Flagged / malformed / Positions-only / unpositioned
    // tests; this is the gate-clears direction.)
    let wb = combined_workbook();
    let report = dry_run(&wb, &combined_marks()).expect("dry-run succeeds");

    assert!(
        report.reconstruction.malformed.is_empty(),
        "no malformed source rows in the clean combined workbook"
    );
    assert!(
        !report
            .symbols
            .iter()
            .any(|s| matches!(s.verdict, Verdict::Unexplained { .. })),
        "no Unexplained dollar verdict"
    );
    assert!(
        !report
            .symbols
            .iter()
            .any(|s| matches!(s.share_verdict, ShareVerdict::Flagged { .. })),
        "no Flagged share residual"
    );
    assert!(
        report.commit_allowed,
        "the gate clears when the complete commit-blocking set is empty"
    );
}

// @spec IMPORT-RECON-008
#[test]
fn an_owner_adjudicated_divergence_is_recorded_and_does_not_block() {
    // The same corrupted GOOG realized figure the Unexplained test uses — but the
    // owner has declared the divergence adjudicated, with a stated reason. The
    // verdict carries BOTH the residual and the verbatim reason, the gate stays
    // clear, and the adjudication rides the accepted report into the commit.
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(goog_buy());
    wb.sales.push(goog_sale());
    wb.positions.push(position_row("GOOG", 80, 99_900, 80_000));
    wb.adjudicated.insert(
        "GOOG".to_string(),
        "known-wrong legacy Profit cell".to_string(),
    );

    let mut marks = Marks::new();
    marks.insert("GOOG".to_string(), Cents(6000));

    let report = dry_run(&wb, &marks).expect("dry-run succeeds");
    let goog = report
        .symbols
        .iter()
        .find(|s| s.symbol == "GOOG")
        .expect("GOOG line");
    match &goog.verdict {
        Verdict::OwnerAdjudicated {
            residual_cents,
            reason,
        } => {
            assert_eq!(
                *residual_cents,
                Cents(-89_900),
                "the real residual is carried"
            );
            assert_eq!(
                reason, "known-wrong legacy Profit cell",
                "the reason, verbatim"
            );
        }
        other => panic!("expected OwnerAdjudicated, got {other:?}"),
    }
    assert!(
        report.commit_allowed,
        "an adjudicated divergence does not block"
    );

    // The adjudicated report (residual + reason in its verdict) is what the owner
    // accepts and `commit` consumes — the commit record carries why.
    let mut store = fresh_store();
    let commit_report = commit(&mut store, &report.accept()).expect("commit proceeds");
    assert!(
        !commit_report.appended.is_empty(),
        "the accepted import writes through"
    );
}

// @spec IMPORT-RECON-008
#[test]
fn an_undeclared_symbols_unexplained_verdict_still_blocks() {
    // The adjudication names AMZN; GOOG's residual is undeclared — it stays
    // Unexplained and the gate stays blocked.
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(goog_buy());
    wb.sales.push(goog_sale());
    wb.positions.push(position_row("GOOG", 80, 99_900, 80_000));
    wb.adjudicated
        .insert("AMZN".to_string(), "some other symbol's ruling".to_string());

    let mut marks = Marks::new();
    marks.insert("GOOG".to_string(), Cents(6000));

    let report = dry_run(&wb, &marks).expect("dry-run succeeds");
    let goog = report
        .symbols
        .iter()
        .find(|s| s.symbol == "GOOG")
        .expect("GOOG line");
    assert!(
        matches!(goog.verdict, Verdict::Unexplained { .. }),
        "an undeclared symbol's residual stays Unexplained"
    );
    assert!(!report.commit_allowed, "and it still blocks commit");
}

// @spec IMPORT-RECON-008
#[test]
fn an_adjudication_never_applies_to_a_share_residual() {
    // GOOG reconstructs to 80 shares but legacy claims 90 — a Flagged share
    // residual. The owner's GOOG adjudication is a DOLLAR ruling only: the share
    // verdict stays Flagged and commit stays blocked.
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(goog_buy());
    wb.sales.push(goog_sale());
    wb.positions.push(position_row("GOOG", 90, 10_000, 90_000));
    wb.adjudicated.insert(
        "GOOG".to_string(),
        "adjudications rule on dollars, not shares".to_string(),
    );

    let report = dry_run(&wb, &Marks::new()).expect("dry-run succeeds");
    let goog = report
        .symbols
        .iter()
        .find(|s| s.symbol == "GOOG")
        .expect("GOOG line");
    assert!(
        matches!(goog.share_verdict, ShareVerdict::Flagged { .. }),
        "the share residual stays flagged"
    );
    assert!(
        !report.commit_allowed,
        "a flagged share residual blocks regardless of adjudication"
    );
}
