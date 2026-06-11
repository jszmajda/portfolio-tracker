//! The runtime side of the fresh-workbook cold start: the real client's
//! missing-tab classification (the 400 `Unable to parse range`) maps to
//! `StoreError::TabMissing`, and the adapter's `ensure_tab` bootstraps the tab
//! (schema header + fingerprint block) so a `Store` can run end-to-end against
//! a workbook that has never been written. (STORE-LOAD-007 / STORE-WRITE-009)

use runtime::sheets::StoreSheetsAdapter;
use runtime::testkit::FakeSheetsApi;
use store::cache::InMemoryCache;
use store::sheets::{NoopLock, SheetsClient};
use store::{Store, StoreError, Tab};

fn fresh_workbook_adapter() -> StoreSheetsAdapter<FakeSheetsApi> {
    let api = FakeSheetsApi::new();
    api.set_sheet_missing(Tab::Ledger.name());
    api.set_sheet_missing(Tab::Tax.name());
    StoreSheetsAdapter::new(api)
}

// @spec STORE-LOAD-007
#[test]
fn the_missing_tab_400_classifies_as_tab_missing_not_unreachable() {
    let adapter = fresh_workbook_adapter();
    assert_eq!(
        adapter.read_rows(Tab::Ledger).unwrap_err(),
        StoreError::TabMissing,
        "the 400 `Unable to parse range` is the cold start, not an outage"
    );
}

// @spec STORE-LOAD-007
#[test]
fn an_offline_workbook_still_classifies_as_unreachable() {
    let adapter = fresh_workbook_adapter();
    adapter.api().set_unreachable(true);
    assert_eq!(
        adapter.read_rows(Tab::Ledger).unwrap_err(),
        StoreError::Unreachable,
        "a transport failure must never be misread as a missing tab"
    );
}

// @spec STORE-LOAD-007
#[test]
fn a_store_over_the_real_adapter_loads_a_fresh_workbook_as_an_empty_book() {
    let mut store = Store::new(
        fresh_workbook_adapter(),
        NoopLock::new(),
        InMemoryCache::new(),
    );
    let logs = store.load().expect("cold start, not an error");
    assert!(logs.ledger.is_empty() && logs.tax.is_empty());
}

// @spec STORE-WRITE-009
#[test]
fn ensure_tab_creates_the_sheet_with_the_frozen_header() {
    let mut adapter = fresh_workbook_adapter();
    adapter.ensure_tab(Tab::Ledger).expect("bootstrap the tab");

    assert!(
        !adapter.api().sheet_missing(Tab::Ledger.name()),
        "the sheet now exists"
    );
    let grid = adapter.api().rows_at(Tab::Ledger.name());
    let want: Vec<String> = Tab::Ledger.header().iter().map(|h| h.to_string()).collect();
    assert_eq!(
        grid.first(),
        Some(&want),
        "row 1 is the frozen schema header"
    );
    assert_eq!(
        adapter
            .read_rows(Tab::Ledger)
            .expect("readable after bootstrap")
            .len(),
        0,
        "no data rows yet — the header is not data"
    );
}

// ---------------------------------------------------------------------------
// The view and History tabs bootstrap the same way: their publish/write specs
// ("publish four view tabs", "write one trading-day point to the durable
// History tab") imply creating the tab on a fresh workbook.
// ---------------------------------------------------------------------------

// @spec SHEET-TAB-001
#[test]
fn republish_to_a_fresh_workbook_creates_the_view_tab_with_its_frozen_header() {
    use runtime::adapters::ViewSheetsAdapter;
    use sheets_view::{Cell, SheetsViewClient, ViewTab};

    let api = FakeSheetsApi::new();
    api.set_sheet_missing(sheets_view::POSITIONS_TAB);
    let mut adapter = ViewSheetsAdapter::new(api, sheets_view::POSITIONS_TAB);

    let tab = ViewTab {
        name: sheets_view::POSITIONS_TAB.to_string(),
        header: sheets_view::POSITIONS_HEADER
            .iter()
            .map(|s| s.to_string())
            .collect(),
        rows: vec![vec![Cell::Value("AMZN".to_string())]],
    };
    adapter
        .batch_update_view(&tab)
        .expect("the first republish bootstraps the tab");

    assert!(!adapter.api().sheet_missing(sheets_view::POSITIONS_TAB));
    let grid = adapter.api().rows_at(sheets_view::POSITIONS_TAB);
    assert_eq!(grid[0][0], "Symbol", "row 1 is the frozen typed header");
    assert_eq!(grid[1][0], "AMZN", "the data row landed beneath it");
}

// @spec SHEET-MARK-006
#[test]
fn the_price_pass_on_a_fresh_workbook_is_empty_not_an_error() {
    use runtime::adapters::ViewSheetsAdapter;
    use sheets_view::SheetsViewClient;

    let api = FakeSheetsApi::new();
    api.set_sheet_missing(sheets_view::POSITIONS_TAB);
    let adapter = ViewSheetsAdapter::new(api, sheets_view::POSITIONS_TAB);
    let pass = adapter
        .read_price_pass()
        .expect("no tab yet ⇒ no marks, not an outage");
    assert!(pass.is_empty());
}

// @spec REPORT-HIST-001
#[test]
fn the_first_history_capture_bootstraps_the_history_tab() {
    use reports::{point_checksum, HistoryClient, HistoryRow, SeriesPoint, TradingDayKey};
    use runtime::adapters::HistorySheetsAdapter;
    use std::collections::BTreeMap;

    let api = FakeSheetsApi::new();
    api.set_sheet_missing(sheets_view::HISTORY_TAB);
    let mut adapter = HistorySheetsAdapter::new(api, sheets_view::HISTORY_TAB);

    assert_eq!(
        adapter
            .read_history()
            .expect("a missing History tab is an empty series"),
        vec![],
        "fresh workbook ⇒ no captured points yet"
    );

    let key = TradingDayKey(pt_core::Date(19_200));
    let point = SeriesPoint {
        key,
        total_market_value_cents: pt_core::Cents(100_00),
        total_unrealized_pretax_cents: pt_core::Cents(0),
        total_unrealized_net_of_tax_cents: pt_core::Cents(0),
        total_basis_cents: pt_core::Cents(100_00),
        per_symbol_value_cents: BTreeMap::new(),
        per_symbol_shares: BTreeMap::new(),
        marks: BTreeMap::new(),
        captured_at_epoch_secs: 1_700_000_000,
        reporting_tz_date: pt_core::Date(19_200),
        incomplete: false,
    };
    let row = HistoryRow {
        key,
        checksum: point_checksum(&point),
        point,
    };
    adapter
        .upsert_point(&row)
        .expect("the first capture bootstraps the tab");

    assert!(!adapter.api().sheet_missing(sheets_view::HISTORY_TAB));
    let back = adapter.read_history().expect("readable after bootstrap");
    assert_eq!(back.len(), 1, "the point landed durably");
    assert_eq!(back[0].key, row.key);
}

// @spec STORE-WRITE-009
#[test]
fn a_store_over_the_real_adapter_bootstraps_on_first_append() {
    use pt_core::{Cents, Date, MicroShares, Seq};
    let mut store = Store::new(
        fresh_workbook_adapter(),
        NoopLock::new(),
        InMemoryCache::new(),
    );

    let ev = ledger_core::LedgerEvent {
        id: "evt-cold-1".to_string(),
        seq: Seq(0),
        date: Date(19_000),
        kind: ledger_core::LedgerEventKind::Buy {
            lot_id: "lot-cold-1".to_string(),
            symbol: "AMZN".to_string(),
            qty: MicroShares(1_000_000),
            unit_price_cents: Cents(100_00),
            fees_cents: Cents(0),
            platform: "schwab".to_string(),
            tracking_code: None,
        },
    };
    let out = store
        .append_ledger(&ev)
        .expect("the first-ever write bootstraps the tab");
    assert_eq!(out.seq.0, 1, "Seq 1 on the freshly-created tab");
    assert_eq!(
        store.load().expect("reload").ledger.len(),
        1,
        "the event is durably read back"
    );
}
