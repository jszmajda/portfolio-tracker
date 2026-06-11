//! The remaining segment-shaped adapters over the ONE low-level primitive:
//! RUNTIME-SHEETS-002. Each named segment seam — reports (History), sheets-view
//! (republish + marks read-back), config (domain tabs) — rides the SAME
//! `SheetsApi` primitive, asserted through the FAKE low-level API's per-method call
//! counters. No segment opens its own client. (The store seam is covered in
//! tests/sheets.rs; these three complete the set.)
#![allow(clippy::inconsistent_digit_grouping)]

use std::collections::BTreeMap;

use pt_core::{Cents, Date, MicroShares};

use runtime::testkit::FakeSheetsApi;
use runtime::{ConfigSheetsAdapter, HistorySheetsAdapter, ViewSheetsAdapter};

// ---------------------------------------------------------------------------
// reports::HistoryClient over the one primitive.
// ---------------------------------------------------------------------------

use reports::{
    append_snapshot, point_checksum, HistoryClient, PricedMark, SeriesPoint, TradingDayKey,
};
use reports::testkit::NoopLock;

const HIST_TAB: &str = "History";

fn series_point(key_days: i32, mv_cents: i64) -> SeriesPoint {
    let mut marks: BTreeMap<String, PricedMark> = BTreeMap::new();
    marks.insert(
        "AMZN".to_string(),
        PricedMark { price_cents: Cents(170_00), quote_epoch: Date(key_days) },
    );
    let mut per_symbol_value = BTreeMap::new();
    per_symbol_value.insert("AMZN".to_string(), Cents(mv_cents));
    let mut per_symbol_shares = BTreeMap::new();
    per_symbol_shares.insert("AMZN".to_string(), MicroShares(3_000_000));
    SeriesPoint {
        key: TradingDayKey(Date(key_days)),
        total_market_value_cents: Cents(mv_cents),
        total_unrealized_pretax_cents: Cents(30_00),
        total_unrealized_net_of_tax_cents: Cents(20_00),
        total_basis_cents: Cents(450_00),
        per_symbol_value_cents: per_symbol_value,
        per_symbol_shares,
        marks,
        captured_at_epoch_secs: 1_700_000_000,
        reporting_tz_date: Date(key_days),
        incomplete: false,
    }
}

/// Seed the History tab's frozen header so the adapter's read skips row 0.
fn seed_history_header(fake: &FakeSheetsApi) {
    fake.seed(
        &format!("'{HIST_TAB}'!A1:Z"),
        vec![vec!["Key".to_string(), "Checksum".to_string(), "Point".to_string()]],
    );
}

// @spec RUNTIME-SHEETS-002
#[test]
fn history_append_snapshot_rides_the_one_low_level_primitive() {
    // reports' History capture (append_snapshot: acquire-lock → last-wins upsert →
    // read-back-verify) rides the ONE runtime Sheets primitive via the adapter — no
    // second client. The upsert is a full-tab update through the primitive, and the
    // read-back-verify is a read through the same primitive. (RUNTIME-SHEETS-002,
    // REPORT-HIST-001/002)
    let fake = FakeSheetsApi::new();
    seed_history_header(&fake);
    let mut adapter = HistorySheetsAdapter::new(fake, HIST_TAB);
    let lock = NoopLock::new();

    let point = series_point(19_180, 510_00);
    append_snapshot(&mut adapter, &lock, &point).expect("capture rides the primitive");

    // The capture rode the low-level update (the batchUpdate upsert) and read
    // (the read-back-verify + the pre-upsert read).
    assert!(adapter.api().update_count() >= 1, "the upsert rode the low-level update");
    assert!(adapter.api().read_count() >= 1, "the read-back-verify rode the low-level read");

    // The point is durably present, read back through the same primitive.
    let back = adapter.read_history().expect("read back");
    assert_eq!(back.len(), 1, "one captured point");
    assert_eq!(back[0].point, point, "the point round-trips through the grid exactly");
    assert_eq!(back[0].checksum, point_checksum(&point), "the stored checksum matches");
}

// @spec RUNTIME-SHEETS-002
#[test]
fn history_upsert_is_last_wins_by_trading_day_key_through_the_primitive() {
    // A re-run of the same trading day OVERWRITES that row (last-wins by key), never
    // duplicates — the History write discipline, built on the one primitive's
    // full-tab update. (RUNTIME-SHEETS-002, REPORT-HIST-002)
    let fake = FakeSheetsApi::new();
    seed_history_header(&fake);
    let mut adapter = HistorySheetsAdapter::new(fake, HIST_TAB);
    let lock = NoopLock::new();

    append_snapshot(&mut adapter, &lock, &series_point(19_180, 500_00)).expect("first capture");
    // Re-capture the SAME trading day with a different value.
    append_snapshot(&mut adapter, &lock, &series_point(19_180, 999_00)).expect("re-capture");
    // A capture of a NEW trading day appends.
    append_snapshot(&mut adapter, &lock, &series_point(19_181, 600_00)).expect("new day");

    let back = adapter.read_history().expect("read back");
    assert_eq!(back.len(), 2, "last-wins: the repeated key did not duplicate");
    let day0 = back.iter().find(|r| r.key == TradingDayKey(Date(19_180))).unwrap();
    assert_eq!(day0.point.total_market_value_cents, Cents(999_00), "the re-capture overwrote");
}

// @spec RUNTIME-SHEETS-002
#[test]
fn history_offline_primitive_surfaces_unreachable() {
    // When the ONE primitive is offline, the History adapter maps it to reports'
    // error contract (Unreachable on a read), not a second client's invented
    // handling. (RUNTIME-SHEETS-002, REPORT-HIST-004)
    let fake = FakeSheetsApi::new();
    fake.set_unreachable(true);
    let adapter = HistorySheetsAdapter::new(fake, HIST_TAB);
    assert_eq!(adapter.read_history(), Err(reports::HistoryError::Unreachable));
}

// ---------------------------------------------------------------------------
// sheets_view::SheetsViewClient over the one primitive.
// ---------------------------------------------------------------------------

use sheets_view::{Cell, PriceReading, SheetsViewClient, ViewTab, DATA_START_ROW};

const POS_TAB: &str = "Positions";

// @spec RUNTIME-SHEETS-002
#[test]
fn view_republish_and_marks_read_back_ride_the_one_primitive() {
    // sheets-view's republish (full-tab batchUpdate + tail-truncate beneath the
    // frozen header) and the SEPARATE settle-pass marks read-back both ride the ONE
    // primitive via the adapter — the republish through update, the read-back
    // through read, never inline. (RUNTIME-SHEETS-002, SHEET-PUB-002, SHEET-MARK-001)
    let fake = FakeSheetsApi::new();
    // Seed the frozen header at row 1 so the data range (from DATA_START_ROW) sits
    // beneath it.
    fake.seed(
        &format!("'{POS_TAB}'!A1:Z"),
        vec![sheets_view::POSITIONS_HEADER.iter().map(|s| s.to_string()).collect()],
    );
    let mut adapter = ViewSheetsAdapter::new(fake, POS_TAB);

    // Republish a Positions data row: Symbol col 0, Price col 4 (a settled numeric),
    // quote-date col 12 (carried beyond the visible header for the read-back).
    let mut row: Vec<Cell> = vec![Cell::Value(String::new()); 13];
    row[0] = Cell::Value("AMZN".to_string());
    row[4] = Cell::Value("170.25".to_string());
    row[12] = Cell::Value("19180".to_string());
    let tab = ViewTab {
        name: POS_TAB.to_string(),
        header: sheets_view::POSITIONS_HEADER.iter().map(|s| s.to_string()).collect(),
        rows: vec![row],
    };
    adapter.batch_update_view(&tab).expect("republish rides the primitive");
    assert_eq!(adapter.api().update_count(), 1, "republish rode the low-level update");

    // The settle pass reads the Price cells back through the primitive — a separate
    // call (its own read), and parses the numeric reading + quote date.
    let pass = adapter.read_price_pass().expect("settle pass rides the primitive");
    assert!(adapter.api().read_count() >= 1, "the settle pass rode the low-level read");
    match pass.get("AMZN") {
        Some(PriceReading::Numeric { price_usd, quote_date }) => {
            assert!((*price_usd - 170.25).abs() < 1e-9, "the numeric price reads back");
            assert_eq!(*quote_date, Date(19_180), "the quote date reads back");
        }
        other => panic!("expected a numeric AMZN reading, got {other:?}"),
    }

    // The frozen header (row 1) survived the republish (tail-truncate only the data
    // range), proving the data write started beneath it (DATA_START_ROW).
    let grid = adapter.api().rows_at(POS_TAB);
    assert_eq!(grid[0][0], "Symbol", "the frozen header is never rewritten");
    assert_eq!(DATA_START_ROW, 2);
}

// @spec RUNTIME-SHEETS-002
#[test]
fn view_offline_primitive_surfaces_publish_failed() {
    // An offline primitive maps to sheets-view's PublishFailed (leave the view
    // stale), not a second client's handling. (RUNTIME-SHEETS-002, SHEET-PUB-003)
    let fake = FakeSheetsApi::new();
    fake.set_unreachable(true);
    let mut adapter = ViewSheetsAdapter::new(fake, POS_TAB);
    let tab = ViewTab {
        name: POS_TAB.to_string(),
        header: sheets_view::POSITIONS_HEADER.iter().map(|s| s.to_string()).collect(),
        rows: vec![],
    };
    assert_eq!(adapter.batch_update_view(&tab), Err(sheets_view::ViewError::PublishFailed));
    assert_eq!(adapter.read_price_pass(), Err(sheets_view::ViewError::PublishFailed));
}

// ---------------------------------------------------------------------------
// config::ConfigStore over the one primitive.
// ---------------------------------------------------------------------------

use config::{ConfigStore, PlatformList};

const CFG_TAB: &str = "Config";

// @spec RUNTIME-SHEETS-002
#[test]
fn config_put_and_load_ride_the_one_primitive() {
    // config's domain-tab write (put_*) and load both ride the ONE primitive via the
    // adapter — the write through update, the load through read — not a second
    // client. (RUNTIME-SHEETS-002)
    let fake = FakeSheetsApi::new();
    let mut adapter = ConfigSheetsAdapter::new(fake, CFG_TAB, Some(config::Settings::default()));

    adapter
        .put_platforms(
            PlatformList::new(vec!["schwab".to_string(), "fidelity".to_string()]),
            &NoopLock::new(),
        )
        .expect("put_platforms rides the primitive");
    assert!(adapter.api().update_count() >= 1, "the put rode the low-level update");

    let data = adapter.load().expect("load rides the primitive");
    assert!(adapter.api().read_count() >= 1, "the load rode the low-level read");
    assert_eq!(
        data.platforms.names(),
        &["schwab".to_string(), "fidelity".to_string()],
        "the platform list round-trips through the grid"
    );
}

// @spec RUNTIME-SHEETS-002
#[test]
fn config_cold_start_workbook_reads_as_empty() {
    // A fresh workbook (no config rows) reads as cold-start empty data — never a
    // hard error. (RUNTIME-SHEETS-002, CONFIG-SETTINGS-003)
    let fake = FakeSheetsApi::new();
    let adapter = ConfigSheetsAdapter::new(fake, CFG_TAB, Some(config::Settings::default()));
    let data = adapter.load().expect("cold-start load");
    assert_eq!(data, config::ConfigData::default(), "an empty workbook is cold-start");
}

// @spec RUNTIME-SHEETS-002
#[test]
fn config_missing_credentials_surface_a_hard_error_through_the_seam() {
    // The local-file settings half: missing credentials surface a hard error, never
    // empty config. (RUNTIME-SHEETS-002, CONFIG-SETTINGS-004)
    let fake = FakeSheetsApi::new();
    let adapter = ConfigSheetsAdapter::new(fake, CFG_TAB, None);
    assert_eq!(adapter.load_settings(), Err(config::ConfigError::CredentialsUnavailable));
}

// @spec CONFIG-SETTINGS-002
#[test]
fn the_config_wire_form_round_trips_every_domain_field() {
    // The adapter persists the WHOLE domain config through its wire form; a
    // field the wire drops is owner-entered config silently lost (the original
    // stub carried only platforms — the live seeding run lost the tax rules).
    // Drive a fully-populated ConfigData through put_*/load over the fake
    // primitive and assert total equality.
    use std::collections::BTreeMap;
    use config::{
        AliasMap, BracketRow, BracketSet, ConfigStore as _, DeMinimis, FilingStatus, Niit,
        PlatformList, Ppm, ResidencyEntry, ResidencyTimeline, TaxRules, TaxYear,
    };
    use pt_core::{Cents, Date, NoopLock};

    let set = |note: &str| BracketSet {
        rows: vec![
            BracketRow { lower_threshold_cents: Cents(0), rate_ppm: Ppm(40_000) },
            BracketRow { lower_threshold_cents: Cents(1_000_000), rate_ppm: Ppm(85_000) },
        ],
        last_verified: Date(20_614),
        source_note: note.to_string(),
    };
    let mut state_ordinary = BTreeMap::new();
    state_ordinary.insert("DC".to_string(), set("DC OTR"));
    let rules = TaxRules {
        tax_year: TaxYear(2026),
        filing_status: FilingStatus::MarriedFilingJointly,
        federal_ordinary: set("Rev. Proc. ordinary"),
        federal_long_term: BracketSet {
            rows: vec![
                BracketRow { lower_threshold_cents: Cents(0), rate_ppm: Ppm(0) },
                BracketRow { lower_threshold_cents: Cents(9_890_000), rate_ppm: Ppm(150_000) },
            ],
            last_verified: Date(20_614),
            source_note: "Rev. Proc. LT".to_string(),
        },
        niit: Niit { rate_ppm: Ppm(38_000), magi_threshold_cents: Cents(25_000_000) },
        state_ordinary,
        ordinary_income_cents: Cents(30_000_000),
    };

    let fake = FakeSheetsApi::new();
    fake.set_sheet_missing(CFG_TAB); // a brand-new workbook: no config tab yet
    let mut adapter = ConfigSheetsAdapter::new(fake, CFG_TAB, Some(config::Settings::default()));
    let lock = NoopLock::new();

    adapter.put_tax_rules(rules.clone(), &lock).expect("rules persist");
    adapter.put_de_minimis(DeMinimis(Cents(100)), &lock).expect("de-minimis persists");
    let timeline = ResidencyTimeline::from_entries(vec![ResidencyEntry {
        effective_date: Date(0),
        state_code: "DC".to_string(),
    }])
    .unwrap();
    adapter.put_residency(timeline.clone(), &lock).expect("residency persists");
    adapter
        .put_platforms(PlatformList::new(vec!["Schwab".to_string()]), &lock)
        .expect("platforms persist");
    let mut amap = BTreeMap::new();
    amap.insert("BRK.B".to_string(), "BRK-B".to_string());
    adapter.put_aliases(AliasMap::new(amap.clone()), &lock).expect("aliases persist");
    let mut nmap = BTreeMap::new();
    nmap.insert("AMZN".to_string(), "Amazon.com".to_string());
    nmap.insert("GOOGL".to_string(), "Alphabet".to_string());
    adapter
        .put_display_names(config::DisplayNameMap::new(nmap.clone()), &lock)
        .expect("display names persist");

    let data = adapter.load().expect("load back");
    assert_eq!(data.rules_by_year.get(&TaxYear(2026)), Some(&rules), "tax rules survive whole");
    assert_eq!(data.de_minimis, DeMinimis(Cents(100)));
    assert_eq!(data.residency, timeline);
    assert_eq!(data.platforms.names(), ["Schwab".to_string()]);
    assert_eq!(data.aliases.entries(), &amap);
    assert_eq!(data.display_names.entries(), &nmap, "display names survive whole");
}
