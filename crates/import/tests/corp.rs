//! Corporate actions, share-frame, and FMV recovery (IMPORT-CORP-001..004).

mod common;
use common::*;

use import::testkit::{founding_residency, sale_row, vest_row};
use import::{
    assert_share_frame_precondition, reconstruct, validate_reconstruction, ImportError,
    LegacyWorkbook, RowCorrection,
};
use ledger_core::{LedgerEventKind, Marks};
use pt_core::{Cents, MicroShares};

// @spec IMPORT-CORP-001
#[test]
fn every_quantity_read_in_its_own_row_date_share_frame_split_bridges_frames() {
    // The AMZN vest qty (100) is as-of-vest (pre-split); the sale qty (400) is
    // as-of-sale (post-split). The importer must read each in its own frame and
    // let the explicit Split bridge them: after the 20:1 split the 100 pre-split
    // shares become 2000, and the post-split sale of 400 consumes correctly,
    // leaving 1600. The legacy remaining-shares column is NOT consulted.
    let wb = amzn_only_workbook();
    let recon = reconstruct(&wb).expect("reconstructs across the split frame");
    validate_reconstruction(&recon).expect("kernel-valid across frames");

    let snap = import::replay_reconstruction(&recon, &Marks::new());
    let amzn = snap.positions.get("AMZN").expect("AMZN position");
    assert_eq!(
        amzn.total_qty,
        MicroShares(1600 * 1_000_000),
        "100 vest × 20 split − 400 sold = 1600 post-split shares"
    );
}

// @spec IMPORT-CORP-001
#[test]
fn share_frame_precondition_is_asserted_and_the_remaining_column_is_provably_never_consulted() {
    // The checked precondition holds on a well-framed workbook. And it is PROVABLY
    // independent of any legacy remaining-shares figure: the reconstruction depends
    // only on each row's OWN-frame qty (vest 100 pre-split, sale 400 post-split),
    // so perturbing the legacy `Positions` "remaining" share count does not change
    // the reconstructed shares — the remaining column is never consulted.
    let baseline = amzn_only_workbook();
    assert_share_frame_precondition(&baseline).expect("the share-frame precondition holds");

    let recon_base = reconstruct(&baseline).expect("reconstructs");
    let shares_base = import::replay_reconstruction(&recon_base, &Marks::new())
        .positions
        .get("AMZN")
        .expect("AMZN")
        .total_qty;

    // Garble the legacy remaining-shares figure (the live-frame derived column).
    let mut perturbed = amzn_only_workbook();
    perturbed.positions[0].shares = MicroShares(999_999 * 1_000_000);
    let recon_pert = reconstruct(&perturbed).expect("reconstructs");
    let shares_pert = import::replay_reconstruction(&recon_pert, &Marks::new())
        .positions
        .get("AMZN")
        .expect("AMZN")
        .total_qty;

    assert_eq!(
        shares_base, shares_pert,
        "the reconstructed shares ignore the legacy remaining-shares figure entirely"
    );
}

// @spec IMPORT-CORP-001, IMPORT-CORP-005
#[test]
fn a_frame_ambiguous_zero_share_row_is_surfaced_by_the_precondition() {
    // A zero-share open carries no own-frame quantity to interpret in any frame:
    // the checked precondition surfaces it as a frame-ambiguous Malformed row
    // rather than reconstructing an unframeable event.
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(vest_row(
        "Stock Actions",
        2,
        "Z-V1",
        "AMZN",
        0,
        "2000.00",
        date(2021, 3, 1),
        PLATFORM,
    ));
    match assert_share_frame_precondition(&wb) {
        Err(ImportError::Malformed { coord, .. }) => assert_eq!(coord.row, 2),
        other => panic!("expected a frame-ambiguous Malformed row, got {other:?}"),
    }
    // And it is surfaced through reconstruct's precondition gate too.
    match reconstruct(&wb) {
        Err(ImportError::Malformed { coord, .. }) => assert_eq!(coord.row, 2),
        other => panic!("expected reconstruct to surface the frame-ambiguous row, got {other:?}"),
    }
    let _ = sale_row; // (kept available for sibling tests)
}

// @spec IMPORT-CORP-002
#[test]
fn split_event_reconstructed_and_inserted_by_date_before_post_split_sales() {
    // A Split event is reconstructed for the owner-supplied AMZN 20:1 and ordered
    // (by Seq) AFTER the pre-split vest but BEFORE the post-split sale.
    let wb = amzn_only_workbook();
    let recon = reconstruct(&wb).expect("reconstructs");

    let vest_pos = recon
        .ledger
        .iter()
        .position(|e| matches!(e.event.kind, LedgerEventKind::Vest { .. }));
    let split_pos = recon
        .ledger
        .iter()
        .position(|e| matches!(e.event.kind, LedgerEventKind::Split { .. }));
    let sell_pos = recon
        .ledger
        .iter()
        .position(|e| matches!(e.event.kind, LedgerEventKind::Sell { .. }));

    let (v, s, x) = (
        vest_pos.expect("a Vest"),
        split_pos.expect("a Split"),
        sell_pos.expect("a Sell"),
    );
    assert!(v < s, "vest before split");
    assert!(s < x, "split before post-split sale");

    // The Split carries the owner-supplied ratio.
    let split = &recon.ledger[s];
    match &split.event.kind {
        LedgerEventKind::Split {
            symbol,
            ratio_num,
            ratio_den,
        } => {
            assert_eq!(symbol, "AMZN");
            assert_eq!((*ratio_num, *ratio_den), (20, 1));
        }
        other => panic!("expected Split, got {other:?}"),
    }
    // Seqs are strictly ascending in the intended fold order.
    let seqs: Vec<u64> = recon.ledger.iter().map(|e| e.event.seq.0).collect();
    let mut sorted = seqs.clone();
    sorted.sort_unstable();
    assert_eq!(
        seqs, sorted,
        "reconstructed events are in ascending Seq order"
    );
}

// @spec IMPORT-CORP-003
#[test]
fn vest_with_zero_blank_or_div0_fmv_source_is_a_hard_error_never_defaults_to_zero() {
    for bad in ["0", "0.00", "", "#DIV/0!", "#N/A"] {
        let mut wb = LegacyWorkbook {
            residency: founding_residency("DC", date(2015, 1, 1)),
            ..LegacyWorkbook::default()
        };
        wb.actions.push(import::testkit::vest_row(
            "Stock Actions",
            2,
            "V-BAD",
            "AMZN",
            100,
            bad,
            date(2021, 3, 1),
            PLATFORM,
        ));
        match reconstruct(&wb) {
            Err(ImportError::UnrecoverableVestFmv { coord }) => {
                assert_eq!(coord.row, 2, "the offending vest row is surfaced ({bad:?})");
            }
            other => panic!("expected UnrecoverableVestFmv for {bad:?}, got {other:?}"),
        }
    }
}

// @spec IMPORT-CORP-003
#[test]
fn a_recoverable_nonzero_fmv_does_not_error() {
    let wb = amzn_only_workbook(); // FMV "2000.00"
    assert!(
        reconstruct(&wb).is_ok(),
        "a real FMV is recovered, no error"
    );
}

// @spec IMPORT-CORP-004
#[test]
fn kernel_rejection_surfaces_as_import_error_resolvable_by_per_row_correction() {
    // A sale dated BEFORE the split but carrying the post-split qty (400) against
    // the 100-share pre-split vest fails kernel validation (InsufficientShares):
    // the split has not yet rescaled the lot. The importer surfaces it as a
    // KernelRejected import error — NOT a silent drop, NOT an edit to the log.
    let mut wb = amzn_only_workbook();
    // Move the sale to BEFORE the split date with the post-split qty.
    wb.sales[0].date = date(2022, 1, 1);

    let recon = reconstruct(&wb).expect("reconstruction itself succeeds (ordering by date)");
    match validate_reconstruction(&recon) {
        Err(ImportError::KernelRejected { .. }) => {}
        other => panic!("expected KernelRejected, got {other:?}"),
    }

    // The owner resolves it via a per-row correction-override re-run: declare the
    // sale qty is ALREADY in the post-split frame is wrong here; instead correct
    // the sale qty to the pre-split count (20) so it validates. This is fed back
    // into a re-run, never an edit to the canonical log.
    let mut corrected = wb.clone();
    corrected.corrections.insert(
        wb.sales[0].coord.clone(),
        RowCorrection {
            qty: Some(MicroShares(20_000_000)),
            ..Default::default()
        },
    );
    let recon2 = reconstruct(&corrected).expect("re-run reconstructs");
    validate_reconstruction(&recon2).expect("the corrected re-run is kernel-valid");

    // sanity: the unrealized/realized aren't what we assert here; just no panic.
    let _ = Cents(0);
}

// @spec IMPORT-CORP-004
#[test]
fn already_post_split_frame_override_reorders_a_post_split_qty_after_the_split() {
    // The FRAME third of the per-row correction (the share-frame escape hatch): a
    // sale dated BEFORE the split (2022-01-01) but carrying a POST-split qty (400)
    // against the 100-share pre-split vest fails kernel validation as-is (the lot
    // has not been rescaled yet). Rather than fudge the qty, the owner asserts the
    // qty is ALREADY in the post-split frame via `already_post_split = true`; the
    // importer then orders the sale AFTER the split (so the 100 → 2000 rescale
    // applies first) and the 400 consumes correctly, leaving 1600.
    let mut wb = amzn_only_workbook();
    wb.sales[0].date = date(2022, 1, 1); // before the 2022-06-06 split, post-split qty

    // Without the override: KernelRejected (proves the escape hatch is load-bearing).
    let rejected = reconstruct(&wb).expect("reconstructs");
    assert!(
        matches!(
            validate_reconstruction(&rejected),
            Err(ImportError::KernelRejected { .. })
        ),
        "a pre-split-dated post-split qty is rejected without the frame override"
    );

    // With `already_post_split = true`: reordered after the split, kernel-valid.
    let mut corrected = wb.clone();
    corrected.corrections.insert(
        wb.sales[0].coord.clone(),
        RowCorrection {
            already_post_split: true,
            ..Default::default()
        },
    );
    let recon = reconstruct(&corrected).expect("re-run reconstructs with the frame override");
    validate_reconstruction(&recon).expect("the frame-overridden re-run is kernel-valid");

    // The split now precedes the sale in the fold order.
    let split_pos = recon
        .ledger
        .iter()
        .position(|e| matches!(e.event.kind, LedgerEventKind::Split { .. }))
        .expect("a Split");
    let sell_pos = recon
        .ledger
        .iter()
        .position(|e| matches!(e.event.kind, LedgerEventKind::Sell { .. }))
        .expect("a Sell");
    assert!(
        split_pos < sell_pos,
        "the frame override orders the post-split sale after the split"
    );

    let snap = import::replay_reconstruction(&recon, &Marks::new());
    assert_eq!(
        snap.positions.get("AMZN").expect("AMZN").total_qty,
        MicroShares(1600 * 1_000_000),
        "100 vest × 20 split − 400 sold = 1600, via the frame escape hatch"
    );
}
