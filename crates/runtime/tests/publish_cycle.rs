//! The live wiring of the view-publish triggers: the publishing cycle renders
//! the four view tabs (five typed bands) and drives sheets-view's ONE
//! serialized republish→settle loop, so the published Google Sheets view
//! actually exists — including bootstrapping the tabs on a fresh workbook.

use std::collections::BTreeMap;

use ledger_core::{LedgerEvent, LedgerEventKind};
use pt_core::{Cents, Date, MicroShares, NoopLock, Seq};
use runtime::adapters::ViewSheetsAdapter;
use runtime::cycle::{load_run_and_publish, MarksCache};
use runtime::testkit::FakeSheetsApi;
use sheets_view::{Publisher, SettleConfig};
use store::cache::InMemoryCache;
use store::testkit::InMemorySheets;
use store::{serde_rows, Store, Tab};

const AS_OF: Date = Date(19_200);

fn buy(seq: u64, id: &str, symbol: &str) -> LedgerEvent {
    LedgerEvent {
        id: id.to_string(),
        seq: Seq(seq),
        date: Date(19_000),
        kind: LedgerEventKind::Buy {
            lot_id: format!("lot-{id}"),
            symbol: symbol.to_string(),
            qty: MicroShares(3_000_000),
            unit_price_cents: Cents(150_00),
            fees_cents: Cents(1_25),
            platform: "schwab".to_string(),
            tracking_code: None,
        },
    }
}

// @spec SHEET-PUB-001
#[test]
fn the_publishing_cycle_publishes_all_five_bands_then_settles() {
    // A workbook seeded with one buy but NO view tabs yet (fresh on the view side).
    let sheets = InMemorySheets::new();
    sheets.push_raw(Tab::Ledger, serde_rows::ledger_to_row(&buy(1, "b1", "AMZN")));
    let mut store = Store::new(sheets, store::NoopLock::new(), InMemoryCache::new());

    let api = FakeSheetsApi::new();
    for t in [
        sheets_view::POSITIONS_TAB,
        sheets_view::OPEN_LOTS_TAB,
        sheets_view::REALIZED_TAB,
        sheets_view::TAX_TAB,
        sheets_view::TAX_RESERVE_TAB,
    ] {
        api.set_sheet_missing(t);
    }
    let mut publisher = Publisher::new(ViewSheetsAdapter::new(api, sheets_view::POSITIONS_TAB));

    let ctx = tax::migration_context(config::TaxYear(2022));
    let published = load_run_and_publish(
        &mut store,
        &MarksCache::new(),
        &ctx,
        AS_OF,
        &mut publisher,
        &NoopLock::new(),
        &config::AliasMap::new(BTreeMap::new()),
        SettleConfig { max_polls: 1 },
        "sync-test",
    )
    .expect("the publishing cycle runs");

    assert!(published.republish.published, "all bands published: {:?}", published.republish.stale_tabs);
    assert!(published.outcome.snapshot.positions.contains_key("AMZN"), "the cycle still replays");

    // Every band exists on the workbook with its frozen header and (for
    // Positions) the data row beneath it.
    let api = publisher.client().api();
    for (tab, first_col) in [
        (sheets_view::POSITIONS_TAB, "Symbol"),
        (sheets_view::OPEN_LOTS_TAB, sheets_view::OPEN_LOTS_HEADER[0]),
        (sheets_view::REALIZED_TAB, sheets_view::REALIZED_HEADER[0]),
        (sheets_view::TAX_TAB, sheets_view::TAX_HEADER[0]),
        (sheets_view::TAX_RESERVE_TAB, sheets_view::TAX_RESERVE_HEADER[0]),
    ] {
        assert!(!api.sheet_missing(tab), "{tab} was bootstrapped");
        let grid = api.rows_at(tab);
        assert_eq!(grid[0][0], first_col, "{tab} row 1 is its frozen typed header");
    }
    let positions = api.rows_at(sheets_view::POSITIONS_TAB);
    assert_eq!(positions[1][0], "AMZN", "the Positions data row landed beneath the header");
    assert!(
        positions[1].iter().any(|c| c.contains("GOOGLEFINANCE")),
        "the live-price oracle formula was published"
    );
}

// @spec SHEET-PUB-001
#[test]
fn an_empty_book_still_publishes_the_empty_bands() {
    // A fully fresh workbook (no events, no view tabs): the cycle publishes the
    // five empty bands rather than skipping the view, so the Sheets side exists
    // from the very first run.
    let store_sheets = InMemorySheets::fresh_workbook();
    let mut store = Store::new(store_sheets, store::NoopLock::new(), InMemoryCache::new());

    let api = FakeSheetsApi::new();
    api.set_sheet_missing(sheets_view::POSITIONS_TAB);
    let mut publisher = Publisher::new(ViewSheetsAdapter::new(api, sheets_view::POSITIONS_TAB));

    let ctx = tax::migration_context(config::TaxYear(2022));
    let published = load_run_and_publish(
        &mut store,
        &MarksCache::new(),
        &ctx,
        AS_OF,
        &mut publisher,
        &NoopLock::new(),
        &config::AliasMap::new(BTreeMap::new()),
        SettleConfig { max_polls: 1 },
        "sync-test",
    )
    .expect("a fully fresh workbook publishes empty views");

    assert!(published.republish.published);
    let api = publisher.client().api();
    assert!(!api.sheet_missing(sheets_view::POSITIONS_TAB), "Positions exists, empty");
    assert_eq!(api.rows_at(sheets_view::POSITIONS_TAB).len(), 1, "header only — no data rows");
}
