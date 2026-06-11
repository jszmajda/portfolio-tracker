//! Source-mapping reconstruction (IMPORT-MAP-001..004): legacy rows -> events.

mod common;
use common::*;

use import::testkit::{buy_row, founding_residency, sale_row, vest_row};
use import::{reconstruct, LegacyWorkbook, ReconstructedEvent};
use ledger_core::{LedgerEventKind, LotSource};
use pt_core::{Cents, MicroShares};

/// Pull the reconstructed `LedgerEventKind` for a given lot/sale id.
fn kinds(events: &[ReconstructedEvent]) -> Vec<LedgerEventKind> {
    events.iter().map(|e| e.event.kind.clone()).collect()
}

// @spec IMPORT-MAP-001
#[test]
fn buy_row_reconstructs_buy_with_tranche_lot_id_and_basis_from_dps_plus_fees() {
    // A 100-share buy @ $50.00 with $7.00 fees → Buy with lot id = tranche id,
    // unit_price 5000 cents, fees 700 cents.
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(buy_row(
        "Stock Actions",
        2,
        "B1",
        "GOOG",
        100,
        "50.00",
        "7.00",
        date(2020, 1, 15),
        PLATFORM,
    ));

    let recon = reconstruct(&wb).expect("clean buy reconstructs");
    let buy = recon
        .ledger
        .iter()
        .find(|e| matches!(e.event.kind, LedgerEventKind::Buy { .. }))
        .expect("a Buy event");
    match &buy.event.kind {
        LedgerEventKind::Buy {
            lot_id,
            symbol,
            qty,
            unit_price_cents,
            fees_cents,
            ..
        } => {
            assert_eq!(lot_id, "B1", "lot id is the legacy tranche id");
            assert_eq!(symbol, "GOOG");
            assert_eq!(*qty, MicroShares(100_000_000));
            assert_eq!(*unit_price_cents, Cents(5000), "$50.00 → 5000 cents");
            assert_eq!(*fees_cents, Cents(700), "$7.00 fees → 700 cents");
        }
        other => panic!("expected Buy, got {other:?}"),
    }
}

// @spec IMPORT-MAP-002
#[test]
fn vest_row_reconstructs_vest_with_fmv_recovered_from_dps_not_zero_total_cost() {
    // The AMZN vest's legacy `Total Cost` is $0, but the `$/share` column holds
    // the $2,000 FMV. The reconstructed Vest must carry FMV 200_000 cents, NOT 0.
    let wb = amzn_only_workbook();
    let recon = reconstruct(&wb).expect("vest reconstructs");
    let vest = recon
        .ledger
        .iter()
        .find(|e| matches!(e.event.kind, LedgerEventKind::Vest { .. }))
        .expect("a Vest event");
    match &vest.event.kind {
        LedgerEventKind::Vest {
            lot_id,
            symbol,
            qty,
            fmv_per_share_cents,
            ..
        } => {
            assert_eq!(lot_id, "AMZN-V1");
            assert_eq!(symbol, "AMZN");
            assert_eq!(*qty, MicroShares(100_000_000));
            assert_eq!(
                *fmv_per_share_cents,
                Cents(200_000),
                "$2000 FMV recovered, not $0"
            );
        }
        other => panic!("expected Vest, got {other:?}"),
    }
}

// @spec IMPORT-MAP-002
#[test]
fn sell_to_cover_child_becomes_ordinary_sell_at_vest_date_price() {
    // A vest with a sell-to-cover `-a` child: the child references the vest tranche
    // and sells at the vest-date FMV price (≈ zero gain). It reconstructs as an
    // ordinary Sell against the vest lot.
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(vest_row(
        "Stock Actions",
        2,
        "RSU1",
        "MSFT",
        100,
        "300.00",
        date(2022, 5, 1),
        PLATFORM,
    ));
    // The sell-to-cover child: 40 shares @ the same $300 vest-date price.
    wb.sales.push(sale_row(
        "Stock Sales",
        2,
        "RSU1-a",
        "RSU1",
        "MSFT",
        40,
        "300.00",
        "0",
        date(2022, 5, 1),
        PLATFORM,
    ));

    let recon = reconstruct(&wb).expect("vest + sell-to-cover reconstructs");
    let sell = recon
        .ledger
        .iter()
        .find(|e| matches!(e.event.kind, LedgerEventKind::Sell { .. }))
        .expect("a Sell event for the sell-to-cover");
    match &sell.event.kind {
        LedgerEventKind::Sell {
            symbol,
            qty,
            unit_price_cents,
            lot_refs,
            ..
        } => {
            assert_eq!(symbol, "MSFT");
            assert_eq!(*qty, MicroShares(40_000_000));
            assert_eq!(*unit_price_cents, Cents(30_000), "$300 vest-date price");
            assert_eq!(lot_refs.len(), 1, "specific-ID against the vest lot");
            assert_eq!(lot_refs[0].lot_id, "RSU1");
        }
        other => panic!("expected Sell, got {other:?}"),
    }
}

// @spec IMPORT-MAP-003
#[test]
fn sale_row_reconstructs_specific_id_sell_against_referenced_tranche() {
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(goog_buy());
    wb.sales.push(goog_sale());

    let recon = reconstruct(&wb).expect("buy + sale reconstructs");
    let sell = recon
        .ledger
        .iter()
        .find(|e| matches!(e.event.kind, LedgerEventKind::Sell { .. }))
        .expect("a Sell event");
    match &sell.event.kind {
        LedgerEventKind::Sell {
            symbol,
            qty,
            unit_price_cents,
            fees_cents,
            lot_refs,
            ..
        } => {
            assert_eq!(symbol, "GOOG");
            assert_eq!(*qty, MicroShares(20_000_000));
            assert_eq!(*unit_price_cents, Cents(5500), "$55.00 → 5500 cents");
            assert_eq!(*fees_cents, Cents(0));
            assert_eq!(lot_refs.len(), 1, "specific-ID against the tranche");
            assert_eq!(lot_refs[0].lot_id, "GOOG-B1");
            assert_eq!(lot_refs[0].qty, MicroShares(20_000_000));
        }
        other => panic!("expected Sell, got {other:?}"),
    }
}

// @spec IMPORT-MAP-004
#[test]
fn positions_tab_is_reconcile_target_only_never_a_reconstruction_source() {
    // A workbook whose `Positions` lists AMZN but whose Stock Actions/Sales fully
    // describe the AMZN stream reconstructs the stream from the rows; the
    // `Positions` figures are NOT consulted to produce any event. Removing the
    // `Positions` row entirely leaves the reconstructed events byte-identical.
    let with_positions = amzn_only_workbook();
    let mut without_positions = amzn_only_workbook();
    without_positions.positions.clear();

    let a = reconstruct(&with_positions).expect("reconstructs with positions");
    let b = reconstruct(&without_positions).expect("reconstructs without positions");
    assert_eq!(
        kinds(&a.ledger),
        kinds(&b.ledger),
        "Positions is a reconcile target only; it never feeds reconstruction"
    );
    // And the vest lot opened as a Vest (source classification), proving it came
    // from the action row, not the positions aggregate.
    assert!(a
        .ledger
        .iter()
        .any(|e| matches!(&e.event.kind, LedgerEventKind::Vest { .. })));
    let _ = LotSource::Vest;
}
