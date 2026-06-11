//! The real-Sheets END-TO-END round-trip (env-gated; NOT part of the offline
//! `cargo test` suite). It drives `runtime`'s REAL low-level Google Sheets client
//! ([`runtime::GoogleSheetsApi`]) — the ONE place Sheets mechanics live — against a
//! live workbook, and proves the whole arc reconciles:
//!
//!   append a couple of events through the locked write path  (real append + read-
//!   back-verify on the event-log tab)
//!     -> load + replay (ledger-core)  -> Snapshot
//!     -> project the Positions view tab (sheets-view) + write it back
//!     -> read marks back off the view tab (the separate settle pass)
//!     -> build a report (reports::build_series_point) + a headless summary
//!     -> assert the round-trip reconciles (the events we appended come back as
//!        the replayed positions, and the summary's headline value matches the
//!        report built from the marks we round-tripped).
//!
//! GATING: it runs ONLY when `PT_E2E=1` AND the workbook id + credentials are
//! supplied (env `PT_WORKBOOK_ID` + `GOOGLE_APPLICATION_CREDENTIALS`), so the
//! offline gate never authenticates / hits the network. It is `#[ignore]` so even
//! `cargo test -- --include-ignored` opts in explicitly.
//!
//! IDEMPOTENT / SELF-CLEANING: every tab it touches is under a dedicated, unique
//! `pt_e2e_<pid>_…` prefix it creates at the start and deletes at the end (with a
//! best-effort sweep of any stale prior-run prefixes), so a re-run is always safe
//! and it NEVER touches the legacy workbook or any real view/event tab.
//!
//! @spec RUNTIME-SHEETS-001  (the live HTTP round-trip of the real client: auth +
//!                            read/append/update/clear + sheet management)
//! @spec RUNTIME-SHEETS-002  (a segment's write discipline rides the ONE primitive)
//! @spec RUNTIME-CYCLE-001   (load -> replay -> hold Snapshot, the replay caller)
//! @spec RUNTIME-CYCLE-002   (marks read back off the view tab, the settle pass)

#![allow(clippy::inconsistent_digit_grouping)]

use std::collections::BTreeMap;

use ledger_core::{LedgerEvent, LedgerEventKind};
use pt_core::{Cents, Date, MicroShares, Seq};
use reports::{build_series_point, PricedMark, PricedMarks, TradingDayKey};
use runtime::{GoogleSheetsApi, SheetsApi};
use store::serde_rows;
use store::Tab;

// ---------------------------------------------------------------------------
// Env gate + the dedicated test-tab prefix.
// ---------------------------------------------------------------------------

/// `Some((workbook_id, creds_path))` when the e2e is enabled (PT_E2E=1 + creds
/// supplied); `None` otherwise (the test then SKIPs, printing why).
fn e2e_config() -> Option<(String, String)> {
    if std::env::var("PT_E2E").ok().as_deref() != Some("1") {
        return None;
    }
    let wb = std::env::var("PT_WORKBOOK_ID").ok().filter(|s| !s.is_empty())?;
    let creds = std::env::var("GOOGLE_APPLICATION_CREDENTIALS")
        .ok()
        .filter(|s| !s.is_empty())?;
    Some((wb, creds))
}

/// The dedicated, unique tab prefix for THIS run — `pt_e2e_<pid>_`. Unique per
/// process so concurrent / re-runs never collide, and easy to sweep.
fn run_prefix() -> String {
    format!("pt_e2e_{}_", std::process::id())
}

/// The general `pt_e2e_` family prefix, so a best-effort sweep can reclaim tabs a
/// previously-crashed run left behind (idempotent cleanliness).
const FAMILY_PREFIX: &str = "pt_e2e_";

// ---------------------------------------------------------------------------
// The events the round-trip appends: a Buy (AMZN, open) and a Buy (GOOG, open).
// Two opens so the replayed Snapshot has two priced positions to project + value.
// ---------------------------------------------------------------------------

fn buy(seq: u64, date: i32, lot: &str, symbol: &str, qty_micro: i64, unit_cents: i64) -> LedgerEvent {
    LedgerEvent {
        id: format!("pt-e2e-{seq}"),
        seq: Seq(seq),
        date: Date(date),
        kind: LedgerEventKind::Buy {
            lot_id: lot.to_string(),
            symbol: symbol.to_string(),
            qty: MicroShares(qty_micro),
            unit_price_cents: Cents(unit_cents),
            fees_cents: Cents(0),
            platform: "schwab".to_string(),
            tracking_code: None,
        },
    }
}

#[test]
#[ignore = "live Sheets e2e; enable with PT_E2E=1 + PT_WORKBOOK_ID + GOOGLE_APPLICATION_CREDENTIALS"]
fn real_sheets_round_trip_reconciles() {
    let Some((workbook_id, creds_path)) = e2e_config() else {
        eprintln!(
            "SKIP real_sheets_round_trip_reconciles: set PT_E2E=1, PT_WORKBOOK_ID, and \
             GOOGLE_APPLICATION_CREDENTIALS to run the live round-trip."
        );
        return;
    };

    let api = GoogleSheetsApi::from_credentials_file(workbook_id, &creds_path)
        .expect("build the real Google Sheets client from the service-account creds");

    let prefix = run_prefix();
    let ledger_tab = format!("{prefix}ledger");
    let positions_tab = format!("{prefix}positions");

    // Best-effort sweep of any stale prior-run tabs (a crashed earlier run), then
    // create THIS run's dedicated tabs. RAII-ish: we delete in a guard at the end.
    sweep_stale_tabs(&api);
    api.ensure_sheet(&ledger_tab).expect("ensure the dedicated test ledger tab");
    api.ensure_sheet(&positions_tab).expect("ensure the dedicated test positions tab");

    // Run the body, ALWAYS cleaning up the tabs afterward (even on a panic), so a
    // re-run is safe regardless of outcome.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        round_trip(&api, &ledger_tab, &positions_tab);
    }));

    // --- Cleanup: delete this run's dedicated tabs (idempotent). ---
    let _ = api.delete_sheet(&ledger_tab);
    let _ = api.delete_sheet(&positions_tab);

    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

/// The round-trip body: append events through the real client, read them back,
/// replay to a Snapshot, project + write the Positions view tab, read marks back,
/// build a report + summary, and assert the whole arc reconciles.
fn round_trip(api: &GoogleSheetsApi, ledger_tab: &str, positions_tab: &str) {
    // 1. APPEND through the locked write path. The dedicated test tab uses the same
    //    `store::Row` schema as the real Ledger tab (the header + serde_rows
    //    projection), so the round-trip exercises store's real serialization. We
    //    write the header row, then append each event row via the REAL client's
    //    append (RUNTIME-SHEETS-002 write discipline on the ONE primitive).
    let header: Vec<String> = Tab::Ledger.header().iter().map(|h| h.to_string()).collect();
    api.update_range(&format!("'{ledger_tab}'!A1"), &vec![header])
        .expect("write the ledger header row");

    let events = vec![
        buy(1, 19_000, "lot-amzn", "AMZN", 3_000_000, 150_00),
        buy(2, 19_010, "lot-goog", "GOOG", 2_000_000, 100_00),
    ];
    for ev in &events {
        let row = serde_rows::ledger_to_row(ev);
        let cells = row.to_cells(Tab::Ledger);
        api.append_rows(&format!("'{ledger_tab}'!A2"), &vec![cells])
            .expect("append the event row through the real client");
    }

    // 2. READ BACK + deserialize: read the whole ledger tab, skip the header, and
    //    rebuild the typed events via store's serde — the SAME load path store uses.
    let grid = api
        .read_range(&format!("'{ledger_tab}'!A1:Z"))
        .expect("read the ledger tab back");
    assert!(grid.len() >= 3, "header + two appended rows came back, got {}", grid.len());
    let mut loaded: Vec<LedgerEvent> = Vec::new();
    for cells in grid.iter().skip(1) {
        if cells.iter().all(|c| c.is_empty()) {
            continue;
        }
        let row = store::Row::from_cells(Tab::Ledger, cells);
        let ev = serde_rows::row_to_ledger(&row).expect("a round-tripped row deserializes");
        loaded.push(ev);
    }
    loaded.sort_by_key(|e| e.seq.0);
    assert_eq!(loaded.len(), 2, "both appended events round-tripped");

    // 3. PROJECT the Positions view tab (sheets-view) and WRITE it to the test
    //    positions tab through the real client (RUNTIME-SHEETS-002: the view's
    //    full-tab batchUpdate rides the ONE primitive). The Price column is a LIVE
    //    `GOOGLEFINANCE` formula — the project's price oracle — so this exercises
    //    the real value/formula seam, not a static value. We replay first with an
    //    EMPTY marks set (so the projection's structure does not depend on any
    //    injected price); the live marks are read back off the formula next.
    let aliases = config::AliasMap::default();
    let effective_rates: BTreeMap<String, config::Ppm> = BTreeMap::new();
    let structural = ledger_core::replay(&loaded, &BTreeMap::new());
    assert_eq!(structural.positions.len(), 2, "two positions replayed (AMZN + GOOG)");
    assert_eq!(
        structural.positions["AMZN"].total_qty,
        MicroShares(3_000_000),
        "the AMZN open round-tripped to 3 shares"
    );

    let positions_view =
        sheets_view::render_positions(&structural, &effective_rates, &aliases)
            .expect("project the Positions view tab");
    let header_cells: Vec<String> =
        sheets_view::POSITIONS_HEADER.iter().map(|h| h.to_string()).collect();
    api.update_range(&format!("'{positions_tab}'!A1"), &vec![header_cells])
        .expect("write the positions header");
    let data_grid: Vec<Vec<String>> = positions_view
        .rows
        .iter()
        .map(|r| r.iter().map(|c| c.text().to_string()).collect())
        .collect();
    assert!(!data_grid.is_empty(), "the projection produced data rows");
    api.update_range(&format!("'{positions_tab}'!A2"), &data_grid)
        .expect("write the positions data rows (GOOGLEFINANCE Price formulas)");

    // 4. READ MARKS BACK off the view tab (the SEPARATE settle pass; SHEET-MARK-001).
    //    The Price column is a GOOGLEFINANCE formula Google recalculates server-side
    //    after the write, so we poll the Price cells until they settle to numbers
    //    (the bounded settle window the design names) — recovering the REAL live
    //    marks, never a value we injected.
    let recovered = settle_marks(api, positions_tab, &["AMZN", "GOOG"]);
    assert!(
        recovered.contains_key("AMZN") && recovered.contains_key("GOOG"),
        "both symbols' GOOGLEFINANCE prices settled to live marks: got {recovered:?}"
    );
    for (sym, c) in &recovered {
        assert!(c.0 > 0, "{sym} settled to a positive live price ({} cents)", c.0);
    }

    // 5. REPLAY again WITH the live recovered marks so the Snapshot is valued at the
    //    real oracle prices, then BUILD A REPORT + SUMMARY and assert the whole arc
    //    RECONCILES: the report's total value == Σ(live-mark × shares) recomputed
    //    independently, and the headless summary agrees with the report (no drift).
    let live_marks: ledger_core::Marks = recovered.clone();
    let snapshot = ledger_core::replay(&loaded, &live_marks);

    let estimates: BTreeMap<String, tax::UnrealizedEstimate> = BTreeMap::new();
    let key = TradingDayKey(Date(19_180));
    let priced: PricedMarks = marks_priced(&recovered);
    let point = build_series_point(&snapshot, &priced, &estimates, key, 1_700_000_000, Date(20_000));

    // Independent recomputation from the round-tripped live marks + the replayed
    // shares: value = scale(mark_cents × qty_micro) (the split-neutral per-symbol
    // value). This is the reconciliation: the report built from the snapshot equals
    // the value implied by the marks we read back off the live view tab.
    let mut expected_total: i64 = 0;
    for (sym, pos) in &snapshot.positions {
        if pos.total_qty.0 == 0 {
            continue;
        }
        let mark = recovered.get(sym).expect("every priced position recovered a live mark");
        let value = pt_core::scale((mark.0 as i128) * (pos.total_qty.0 as i128)) as i64;
        expected_total += value;
    }
    assert_eq!(
        point.total_market_value_cents.0, expected_total,
        "the report's total value reconciles with the live marks read back off the view tab"
    );

    // The headless summary renders the SAME point's value — assert it agrees with
    // the report (the two consumers never drift). build_current_point keys on the
    // runtime-reduced trading-day key.
    let inputs = summary_inputs(&snapshot, &priced, &estimates, key);
    let summary_point = summary::build_current_point(&inputs)
        .expect("the summary builds the current point (a symbol is priced)");
    assert_eq!(
        summary_point.total_market_value_cents, point.total_market_value_cents,
        "summary and report agree on the round-tripped total (no drift)"
    );

    eprintln!(
        "E2E OK: appended 2 events -> replayed 2 positions -> live GOOGLEFINANCE marks \
         {recovered:?} -> report total = {} cents (reconciled; summary agrees).",
        point.total_market_value_cents.0
    );
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

/// Project a symbol -> `Cents` mark map to `reports::PricedMarks`, stamping each
/// with the e2e's single quote-epoch (Date(19_180)).
fn marks_priced(marks: &BTreeMap<String, Cents>) -> PricedMarks {
    marks
        .iter()
        .map(|(s, c)| {
            (
                s.clone(),
                PricedMark { price_cents: *c, quote_epoch: Date(19_180) },
            )
        })
        .collect()
}

/// Build the `summary::SummaryInputs` from the replayed snapshot + the round-tripped
/// marks (cold-start brackets — the e2e wires no tax tables).
fn summary_inputs(
    snapshot: &ledger_core::Snapshot,
    priced: &PricedMarks,
    estimates: &BTreeMap<String, tax::UnrealizedEstimate>,
    key: TradingDayKey,
) -> summary::SummaryInputs {
    summary::SummaryInputs {
        snapshot: snapshot.clone(),
        estimates: estimates.clone(),
        annual_rows: Vec::new(),
        marks: priced.clone(),
        trading_day_key: Some(key),
        trading_day_calendar: vec![key],
        reporting_tz_date: Date(20_000),
        run_at_epoch_secs: 1_700_000_000,
        bracket_state: config::BracketState::NoBracketsAvailable,
        tax_year: 2026,
    }
}

/// The bounded GOOGLEFINANCE settle pass: poll the Positions tab's Price column
/// (col 4, the live `=GOOGLEFINANCE(...)` formula) until every requested symbol
/// settles to a numeric price (or the window elapses), recovering symbol -> mark
/// (`Cents`) — the live oracle round-trip the design's separate settle pass names
/// (SHEET-MARK-001/003). Column 0 is the Symbol; a still-`Loading...`/`#N/A` cell
/// is retried; a numeric cell is folded to `Cents` via the project's one rounding
/// rule (`sheets_view::price_to_cents`). Reads run through the REAL client.
fn settle_marks(api: &GoogleSheetsApi, positions_tab: &str, symbols: &[&str]) -> BTreeMap<String, Cents> {
    const SYMBOL_COL: usize = 0;
    const PRICE_COL: usize = 4;
    const MAX_POLLS: u32 = 12;
    let mut out: BTreeMap<String, Cents> = BTreeMap::new();
    for _ in 0..MAX_POLLS {
        let grid = match api.read_range(&format!("'{positions_tab}'!A2:Z")) {
            Ok(g) => g,
            Err(_) => {
                std::thread::sleep(std::time::Duration::from_millis(700));
                continue;
            }
        };
        for row in &grid {
            let Some(sym) = row.get(SYMBOL_COL).filter(|s| !s.is_empty()) else {
                continue;
            };
            if out.contains_key(sym) {
                continue;
            }
            let Some(price) = row.get(PRICE_COL).map(|s| s.trim()) else { continue };
            // Skip the transient states GOOGLEFINANCE returns while recalculating.
            if price.is_empty()
                || price.eq_ignore_ascii_case("Loading...")
                || price.starts_with('#')
            {
                continue;
            }
            if let Ok(usd) = price.trim_start_matches('$').replace(',', "").parse::<f64>() {
                if let Ok(cents) = sheets_view::price_to_cents(usd) {
                    out.insert(sym.clone(), cents);
                }
            }
        }
        if symbols.iter().all(|s| out.contains_key(*s)) {
            break;
        }
        // GOOGLEFINANCE recalculates server-side after the write; give it a beat.
        std::thread::sleep(std::time::Duration::from_millis(700));
    }
    out
}

/// Best-effort sweep of stale `pt_e2e_*` tabs a crashed prior run may have left, so
/// the workbook stays clean and a re-run never accumulates junk. Never errors out
/// the test (a sweep failure is non-fatal — the per-run cleanup is the guarantee).
fn sweep_stale_tabs(api: &GoogleSheetsApi) {
    if let Ok(ids) = api.sheet_ids() {
        for title in ids.keys() {
            if title.starts_with(FAMILY_PREFIX) {
                let _ = api.delete_sheet(title);
            }
        }
    }
}
