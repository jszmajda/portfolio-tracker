//! The remaining segment-shaped adapters over the ONE low-level [`SheetsApi`]
//! primitive (RUNTIME-SHEETS-002): `reports::HistoryClient`,
//! `sheets_view::SheetsViewClient`, and `config::ConfigStore`.
//!
//! `StoreSheetsAdapter` (in [`crate::sheets`]) is the worked example; these three
//! complete the set so EACH named segment seam — store, reports (History),
//! sheets-view (republish + marks read-back), config — rides the SAME primitive,
//! and **no segment opens its own Sheets client**. Each adapter keeps its segment's
//! OWN write discipline (History last-wins-by-key, the view's full-tab
//! `batchUpdate` + tail-truncate + a separate settle-pass read, config's domain
//! tabs) but builds it on the one primitive's read/append/update/clear.
//!
//! The cell-grid projection here is deliberately thin: the segment owns its row
//! schema (its checksum, its integrity check, its validation gate). The adapter's
//! job — and what RUNTIME-SHEETS-002 demands — is only that the segment's I/O
//! passes through the single authenticated/throttled primitive, never a second
//! client. Tests drive each adapter through [`crate::testkit::FakeSheetsApi`] and
//! assert the operation rode the primitive (the read/append/update/clear counters).

use std::collections::BTreeMap;

use ledger_core::Symbol;

use crate::sheets::{is_missing_tab, Grid, SheetsApi, SheetsError};

// ===========================================================================
// reports::HistoryClient over the ONE primitive (RUNTIME-SHEETS-002).
// ===========================================================================

/// Adapts a low-level [`SheetsApi`] to `reports`'s [`reports::HistoryClient`], so
/// the durable History tab's append + read-back-verify + last-wins-by-trading-day
/// upsert rides the ONE runtime Sheets primitive — not a second, separately-
/// authenticated client (RUNTIME-SHEETS-002). `reports` owns the row schema (the
/// content checksum, the on-read integrity check, the last-wins semantics — see
/// [`reports::append_snapshot`] / [`reports::read_history`]); this adapter only
/// reads/writes the grid through the primitive.
///
/// The upsert is **last-wins by trading-day key** (REPORT-HIST-002): a point for an
/// existing key overwrites that row, a new key appends. The adapter reads the full
/// tab, replaces-or-appends the row for the key, and rewrites the data range as one
/// `update_range` (the full-tab `batchUpdate` the History primitive needs), so a
/// re-run never duplicates.
pub struct HistorySheetsAdapter<A: SheetsApi> {
    api: A,
    /// The History tab name (the durable workbook tab `reports` keys its series on).
    tab: String,
}

/// The History tab's A1 data range (data rows beneath the frozen header row).
const HISTORY_DATA_RANGE_SUFFIX: &str = "!A2:Z";
/// The History tab's full A1 range (header + data), for a read.
const HISTORY_FULL_RANGE_SUFFIX: &str = "!A1:Z";

impl<A: SheetsApi> HistorySheetsAdapter<A> {
    /// Build the adapter over a low-level Sheets API client and a History tab name.
    /// (RUNTIME-SHEETS-002)
    pub fn new(api: A, tab: impl Into<String>) -> Self {
        HistorySheetsAdapter { api, tab: tab.into() }
    }

    /// Borrow the underlying low-level API (diagnostics / tests).
    pub fn api(&self) -> &A {
        &self.api
    }

    fn data_range(&self) -> String {
        format!("'{}'{}", self.tab, HISTORY_DATA_RANGE_SUFFIX)
    }

    fn full_range(&self) -> String {
        format!("'{}'{}", self.tab, HISTORY_FULL_RANGE_SUFFIX)
    }
}

/// The History tab's frozen header row, written once when the tab is
/// bootstrapped on a fresh workbook (reads skip row 1 regardless).
const HISTORY_HEADER: [&str; 3] = ["TradingDay", "Checksum", "Point"];

impl<A: SheetsApi> reports::HistoryClient for HistorySheetsAdapter<A> {
    fn read_history(&self) -> Result<Vec<reports::HistoryRow>, reports::HistoryError> {
        // A History tab that does not exist yet is a fresh workbook with no
        // captured points — an empty series, not an outage (the cold-start
        // posture of STORE-LOAD-007, applied to the History read).
        let grid = match self.api.read_range(&self.full_range()) {
            Err(ref e) if is_missing_tab(e) => return Ok(Vec::new()),
            r => r.map_err(map_history_err)?,
        };
        // Row 0 is the header; data rows follow. A row that cannot be parsed into a
        // typed HistoryRow is the segment's loud "UnparseableRow" (REPORT-HIST-003).
        let mut out = Vec::new();
        for cells in grid.into_iter().skip(1) {
            if cells.iter().all(|c| c.is_empty()) {
                continue; // a blank residual row is not a History row
            }
            let row = history_row_from_cells(&cells).ok_or(reports::HistoryError::UnparseableRow)?;
            out.push(row);
        }
        Ok(out)
    }

    fn upsert_point(&mut self, row: &reports::HistoryRow) -> Result<(), reports::HistoryError> {
        // Last-wins by trading-day key (REPORT-HIST-002): read the full tab, replace
        // the row for this key (or append a new one), and rewrite the data range as
        // ONE update_range (the full-tab batchUpdate) so a re-run never duplicates.
        let mut rows = self.read_history()?;
        match rows.iter_mut().find(|r| r.key == row.key) {
            Some(existing) => *existing = row.clone(),
            None => rows.push(row.clone()),
        }
        let grid: Grid = rows.iter().map(history_row_to_cells).collect();
        match self.api.update_range(&self.data_range(), &grid) {
            // First-ever capture on a fresh workbook: REPORT-HIST-001's "write
            // one trading-day point to the durable workbook History tab"
            // implies the tab — create it (header row first), then retry once.
            Err(ref e) if is_missing_tab(e) => {
                self.api
                    .ensure_sheet(&self.tab)
                    .map_err(|_| reports::HistoryError::WriteVerifyFailed)?;
                let header: Vec<String> = HISTORY_HEADER.iter().map(|h| h.to_string()).collect();
                self.api
                    .update_range(&format!("'{}'!A1", self.tab), &vec![header])
                    .map_err(|_| reports::HistoryError::WriteVerifyFailed)?;
                self.api
                    .update_range(&self.data_range(), &grid)
                    .map_err(|_| reports::HistoryError::WriteVerifyFailed)
            }
            r => r.map_err(|_| reports::HistoryError::WriteVerifyFailed),
        }
    }
}

/// Project a `HistoryRow` to a single grid row. `reports` owns the schema; the
/// adapter rides the primitive with a content-preserving projection (the key, the
/// stored checksum, and a JSON encoding of the typed point) so the round-trip is
/// exact and `reports`'s on-read checksum integrity check still fires.
fn history_row_to_cells(row: &reports::HistoryRow) -> Vec<String> {
    vec![
        row.key.0 .0.to_string(),
        row.checksum.to_string(),
        encode_point(&row.point),
    ]
}

/// Parse a grid row back to a `HistoryRow` (the inverse of [`history_row_to_cells`]).
/// `None` on any malformed cell — the segment's loud `UnparseableRow`.
fn history_row_from_cells(cells: &[String]) -> Option<reports::HistoryRow> {
    let key_days: i32 = cells.first()?.trim().parse().ok()?;
    let checksum: u64 = cells.get(1)?.trim().parse().ok()?;
    let point = decode_point(cells.get(2)?)?;
    Some(reports::HistoryRow {
        key: reports::TradingDayKey(pt_core::Date(key_days)),
        point,
        checksum,
    })
}

// ===========================================================================
// sheets_view::SheetsViewClient over the ONE primitive (RUNTIME-SHEETS-002).
// ===========================================================================

/// Adapts a low-level [`SheetsApi`] to `sheets_view`'s
/// [`sheets_view::SheetsViewClient`], so the view-tab republish (full-tab atomic
/// `batchUpdate` + tail-truncate, with the never-rewritten frozen header) and the
/// **separate settle-pass** marks read-back ride the ONE runtime Sheets primitive
/// (RUNTIME-SHEETS-002). `sheets-view` owns the cell typing (value vs. formula) and
/// the settle loop; this adapter only writes/reads the grid through the primitive.
///
/// The republish writes the data range (from [`sheets_view::DATA_START_ROW`] down)
/// as one `update_range`, which the [`crate::testkit::FakeSheetsApi`] tail-truncates
/// — never the frozen header row 1 (SHEET-FORMULA-002). The price settle pass reads
/// the recalculated `Price`/quote-date cells back as a `read_range` — a separate
/// call, never inline with the write (SHEET-MARK-001).
pub struct ViewSheetsAdapter<A: SheetsApi> {
    api: A,
    /// The Positions tab name, whose `Price` column the settle pass reads back.
    positions_tab: String,
}

impl<A: SheetsApi> ViewSheetsAdapter<A> {
    /// Build the adapter over a low-level Sheets API client and the Positions tab
    /// name (the marks read-back source). (RUNTIME-SHEETS-002)
    pub fn new(api: A, positions_tab: impl Into<String>) -> Self {
        ViewSheetsAdapter { api, positions_tab: positions_tab.into() }
    }

    /// Borrow the underlying low-level API (diagnostics / tests).
    pub fn api(&self) -> &A {
        &self.api
    }

    /// The data range for a named view tab (beneath the frozen header).
    fn data_range(name: &str) -> String {
        format!("'{name}'!A{}:Z", sheets_view::DATA_START_ROW)
    }
}

impl<A: SheetsApi> sheets_view::SheetsViewClient for ViewSheetsAdapter<A> {
    fn batch_update_view(&mut self, tab: &sheets_view::ViewTab) -> Result<(), sheets_view::ViewError> {
        // Full-tab atomic batchUpdate + tail-truncate: write the data rows from the
        // anchored start row as ONE update_range. The frozen header (row 1) is never
        // rewritten here. The cell typing (Cell::Value vs Cell::Formula) is the
        // segment's; the adapter writes each cell's text through the primitive.
        // (SHEET-PUB-002, SHEET-FORMULA-002, SHEET-TAB-003)
        let grid: Grid = tab
            .rows
            .iter()
            .map(|row| row.iter().map(|c| c.text().to_string()).collect())
            .collect();
        match self.api.update_range(&Self::data_range(&tab.name), &grid) {
            // A view tab that does not exist yet is a fresh workbook:
            // SHEET-TAB-001's "publish four read-only view tabs" implies
            // creating the tab — bootstrap it (sheet + the segment-owned frozen
            // typed header the `ViewTab` carries, the one write the header ever
            // gets), then retry the data write.
            Err(ref e) if is_missing_tab(e) => {
                self.api
                    .ensure_sheet(&tab.name)
                    .map_err(|_| sheets_view::ViewError::PublishFailed)?;
                self.api
                    .update_range(&format!("'{}'!A1", tab.name), &vec![tab.header.clone()])
                    .map_err(|_| sheets_view::ViewError::PublishFailed)?;
                self.api
                    .update_range(&Self::data_range(&tab.name), &grid)
                    .map_err(|_| sheets_view::ViewError::PublishFailed)
            }
            r => r.map_err(|_| sheets_view::ViewError::PublishFailed),
        }
    }

    fn set_tab_order(&mut self, _order: &[&str]) -> Result<(), sheets_view::ViewError> {
        // Tab ordering is a workbook-metadata operation, not a values-grid touch;
        // the real client issues a spreadsheets.batchUpdate updateSheetProperties.
        // The in-memory primitive models only the values grid, so this is a no-op at
        // the grid seam (the order reassert is idempotent — SHEET-TAB-002).
        Ok(())
    }

    fn read_price_pass(&self) -> Result<sheets_view::PricePass, sheets_view::ViewError> {
        // The SEPARATE settle pass (never inline with the formula write —
        // SHEET-MARK-001): read the Positions tab's Symbol + recalculated Price +
        // quote-date columns back through the ONE primitive. The grid layout is the
        // Positions header order (Symbol col 0, Price col 4, with the companion quote
        // date carried alongside); a non-numeric Price cell is a transient/permanent
        // reading the settle loop interprets.
        let range = format!("'{}'!A{}:Z", self.positions_tab, sheets_view::DATA_START_ROW);
        // A Positions tab that does not exist yet (a fresh workbook before the
        // first republish bootstraps it) holds no marks: an empty pass.
        let grid = match self.api.read_range(&range) {
            Err(ref e) if is_missing_tab(e) => Vec::new(),
            r => r.map_err(|_| sheets_view::ViewError::PublishFailed)?,
        };
        let mut pass: sheets_view::PricePass = BTreeMap::new();
        for cells in grid {
            let symbol = match cells.first() {
                Some(s) if !s.is_empty() => s.clone(),
                _ => continue,
            };
            let reading = price_reading_from_cells(&cells);
            pass.insert(Symbol::from(symbol), reading);
        }
        Ok(pass)
    }

    fn write_stale_banner(&mut self, tab: &str, banner: &str) -> Result<(), sheets_view::ViewError> {
        // The best-effort single-cell STALE banner (SHEET-PUB-003): one cell write at
        // the tab's banner anchor through the primitive. Best-effort: a failure is
        // surfaced so the caller falls back to the TUI as the staleness surface.
        let range = format!("'{tab}'!AA1");
        self.api
            .update_range(&range, &vec![vec![banner.to_string()]])
            .map_err(|_| sheets_view::ViewError::PublishFailed)
    }
}

/// Interpret a Positions data row's price columns into a [`sheets_view::PriceReading`].
/// Column 4 is `Price`; a numeric value with its companion `Quote Date` (column 12 —
/// the locale-proof integer days-since-1970 the formula contract emits,
/// SHEET-MARK-008) is `Numeric`; an empty/`Loading...` cell is `Transient`; an
/// `#N/A`/`#ERROR!` cell is `Permanent`. (SHEET-MARK-001/003)
fn price_reading_from_cells(cells: &[String]) -> sheets_view::PriceReading {
    use sheets_view::PriceReading;
    const PRICE_COL: usize = 4;
    const QUOTE_DATE_COL: usize = 12;
    let price = cells.get(PRICE_COL).map(|s| s.trim()).unwrap_or("");
    if price.is_empty() || price.eq_ignore_ascii_case("Loading...") || price == "#N/A" {
        // A bare transient/loading cell — retry. (A permanent #N/A is distinguished
        // by an explicit error marker below.)
        if price == "#N/A" {
            return PriceReading::Transient;
        }
        return PriceReading::Transient;
    }
    if price.starts_with('#') {
        // A terminal sheet error (e.g. #ERROR!, a permanent ticker mismatch).
        return PriceReading::Permanent;
    }
    match price.parse::<f64>() {
        Ok(price_usd) => {
            let quote_days = cells
                .get(QUOTE_DATE_COL)
                .and_then(|s| s.trim().parse::<i32>().ok())
                .unwrap_or(0);
            PriceReading::Numeric { price_usd, quote_date: pt_core::Date(quote_days) }
        }
        Err(_) => PriceReading::Permanent,
    }
}

// ===========================================================================
// config::ConfigStore over the ONE primitive (RUNTIME-SHEETS-002).
// ===========================================================================

/// Adapts a low-level [`SheetsApi`] to `config`'s [`config::ConfigStore`], so
/// `config`'s domain-tab reads and `put_*` writes ride the ONE runtime Sheets
/// primitive (RUNTIME-SHEETS-002). `config` owns the validation gate and the
/// workbook-wins-over-cache rebuild; this adapter only reads/writes the domain tabs
/// through the primitive.
///
/// A full domain-config (de)serialization is `config`'s schema (one tab per domain
/// table). The adapter reads each domain tab through the primitive's `read_range`
/// and writes through `update_range` (full-tab), so the seam — not a second client —
/// is what RUNTIME-SHEETS-002 asserts. `put_*` runs `config`'s validation gate via
/// the in-memory mirror before writing through, so an invalid set is refused with
/// stored config untouched.
pub struct ConfigSheetsAdapter<A: SheetsApi> {
    api: A,
    /// The single config tab the adapter persists the serialized domain config to.
    tab: String,
    /// The local-file settings (machine/secrets), read separately from the workbook.
    settings: Option<config::Settings>,
}

impl<A: SheetsApi> ConfigSheetsAdapter<A> {
    /// Build the adapter over a low-level Sheets API client, the config tab name,
    /// and the local-file settings. (RUNTIME-SHEETS-002)
    pub fn new(api: A, tab: impl Into<String>, settings: Option<config::Settings>) -> Self {
        ConfigSheetsAdapter { api, tab: tab.into(), settings }
    }

    /// Borrow the underlying low-level API (diagnostics / tests).
    pub fn api(&self) -> &A {
        &self.api
    }

    fn full_range(&self) -> String {
        format!("'{}'!A1:Z", self.tab)
    }

    /// Read the workbook config tab into a typed [`config::ConfigData`]. A fresh
    /// workbook (no rows — or no config TAB at all, the literal "workbook has no
    /// config tabs" case) reads as cold-start (empty data). (CONFIG-SETTINGS-003)
    fn read_config(&self) -> Result<config::ConfigData, config::ConfigError> {
        let grid = match self.api.read_range(&self.full_range()) {
            Err(ref e) if is_missing_tab(e) => return Ok(config::ConfigData::default()),
            r => r.map_err(|_| config::ConfigError::CredentialsUnavailable)?,
        };
        // The whole domain config is serialized into cell A1 as JSON (config owns the
        // real per-tab schema; the adapter rides the one primitive). No rows ⇒ a
        // fresh workbook ⇒ cold-start empty data.
        match grid.first().and_then(|r| r.first()) {
            Some(cell) if !cell.is_empty() => {
                decode_config(cell).ok_or(config::ConfigError::CredentialsUnavailable)
            }
            _ => Ok(config::ConfigData::default()),
        }
    }

    /// Write a full domain config back through the primitive (full-tab update).
    /// The first-ever write to a fresh workbook bootstraps the config tab
    /// (write-implies-create, as the view/event tabs do).
    fn write_config(&mut self, data: &config::ConfigData) -> Result<(), config::ConfigError> {
        let grid = vec![vec![encode_config(data)]];
        match self.api.update_range(&self.full_range(), &grid) {
            Err(ref e) if is_missing_tab(e) => {
                self.api
                    .ensure_sheet(&self.tab)
                    .map_err(|_| config::ConfigError::CredentialsUnavailable)?;
                self.api
                    .update_range(&self.full_range(), &grid)
                    .map_err(|_| config::ConfigError::CredentialsUnavailable)
            }
            r => r.map_err(|_| config::ConfigError::CredentialsUnavailable),
        }
    }

    /// Run a `put_*` through `config`'s validation gate (via the in-memory mirror)
    /// and, only if it validates, persist the resulting full config through the
    /// primitive UNDER the advisory write-lock. The lock is acquired BEFORE the
    /// workbook tab is mutated and released after; a held lock refuses the write
    /// with stored config untouched (CONFIG-SETTINGS-006). The mirror's own `put_*`
    /// re-acquires the SAME lock re-entrantly under the held guard — a no-op against
    /// the same holder — so the validation gate runs without self-deadlock. An
    /// invalid set is refused with stored config untouched.
    fn validated_put<L: pt_core::Lock>(
        &mut self,
        lock: &L,
        mutate: impl FnOnce(&mut config::InMemoryConfig, &L) -> Result<(), config::ConfigError>,
    ) -> Result<(), config::ConfigError> {
        use config::ConfigStore as _;
        // Acquire BEFORE mutating the workbook tab; release on scope exit.
        // (CONFIG-SETTINGS-006)
        let _guard = lock.acquire()?;
        let current = self.read_config()?;
        let mut mirror = config::InMemoryConfig::new(current, config::Settings::default());
        mutate(&mut mirror, lock)?; // the SAME validation gate; refuses an invalid set.
        let validated = mirror.load()?;
        self.write_config(&validated)
    }
}

// @spec CONFIG-SETTINGS-006
impl<A: SheetsApi> config::ConfigStore for ConfigSheetsAdapter<A> {
    fn load(&self) -> Result<config::ConfigData, config::ConfigError> {
        self.read_config()
    }

    fn load_settings(&self) -> Result<config::Settings, config::ConfigError> {
        // The local-file machine/secrets half: missing credentials surface a hard
        // error, never empty config. (CONFIG-SETTINGS-004)
        self.settings
            .clone()
            .ok_or(config::ConfigError::CredentialsUnavailable)
    }

    fn put_tax_rules<L: pt_core::Lock>(
        &mut self,
        rules: config::TaxRules,
        lock: &L,
    ) -> Result<(), config::ConfigError> {
        self.validated_put(lock, |m, l| m.put_tax_rules(rules, l))
    }

    fn put_residency<L: pt_core::Lock>(
        &mut self,
        timeline: config::ResidencyTimeline,
        lock: &L,
    ) -> Result<(), config::ConfigError> {
        self.validated_put(lock, |m, l| m.put_residency(timeline, l))
    }

    fn put_de_minimis<L: pt_core::Lock>(
        &mut self,
        de_minimis: config::DeMinimis,
        lock: &L,
    ) -> Result<(), config::ConfigError> {
        self.validated_put(lock, |m, l| m.put_de_minimis(de_minimis, l))
    }

    fn put_platforms<L: pt_core::Lock>(
        &mut self,
        platforms: config::PlatformList,
        lock: &L,
    ) -> Result<(), config::ConfigError> {
        self.validated_put(lock, |m, l| m.put_platforms(platforms, l))
    }

    fn put_aliases<L: pt_core::Lock>(
        &mut self,
        aliases: config::AliasMap,
        lock: &L,
    ) -> Result<(), config::ConfigError> {
        self.validated_put(lock, |m, l| m.put_aliases(aliases, l))
    }

    // @spec CONFIG-PLATFORM-003
    fn put_display_names<L: pt_core::Lock>(
        &mut self,
        names: config::DisplayNameMap,
        lock: &L,
    ) -> Result<(), config::ConfigError> {
        self.validated_put(lock, |m, l| m.put_display_names(names, l))
    }
}

// ===========================================================================
// Thin content-preserving projections. The segments own their real cell schemas;
// the adapters ride the one primitive with an exact JSON round-trip so the seam
// (RUNTIME-SHEETS-002), not the schema, is what is exercised here.
// ===========================================================================

fn encode_point(point: &reports::SeriesPoint) -> String {
    serde_json::to_string(&PointWire::from(point)).unwrap_or_default()
}

fn decode_point(s: &str) -> Option<reports::SeriesPoint> {
    serde_json::from_str::<PointWire>(s).ok().map(Into::into)
}

fn encode_config(data: &config::ConfigData) -> String {
    // config has no serde derive (the trust seam lives outside its boundary), so
    // the adapter round-trips the domain config through an explicit wire form
    // carrying EVERY field — tax rules by year, de-minimis, the residency
    // timeline, platforms, aliases. A field the wire form drops is config the
    // owner entered and silently lost; the round-trip test pins totality.
    serde_json::to_string(&ConfigWire::from(data)).unwrap_or_default()
}

fn decode_config(s: &str) -> Option<config::ConfigData> {
    serde_json::from_str::<ConfigWire>(s).ok().and_then(|w| w.into_data())
}

/// A serde wire form of `SeriesPoint` (it has no serde derive — the trust seam is
/// `runtime`'s). Round-trips every field so `reports`'s checksum still matches.
#[derive(serde::Serialize, serde::Deserialize)]
struct PointWire {
    key: i32,
    total_market_value_cents: i64,
    total_unrealized_pretax_cents: i64,
    total_unrealized_net_of_tax_cents: i64,
    total_basis_cents: i64,
    per_symbol_value_cents: BTreeMap<String, i64>,
    per_symbol_shares: BTreeMap<String, i64>,
    marks: BTreeMap<String, (i64, i32)>,
    captured_at_epoch_secs: i64,
    reporting_tz_date: i32,
    incomplete: bool,
}

impl From<&reports::SeriesPoint> for PointWire {
    fn from(p: &reports::SeriesPoint) -> Self {
        PointWire {
            key: p.key.0 .0,
            total_market_value_cents: p.total_market_value_cents.0,
            total_unrealized_pretax_cents: p.total_unrealized_pretax_cents.0,
            total_unrealized_net_of_tax_cents: p.total_unrealized_net_of_tax_cents.0,
            total_basis_cents: p.total_basis_cents.0,
            per_symbol_value_cents: p
                .per_symbol_value_cents
                .iter()
                .map(|(s, c)| (s.clone(), c.0))
                .collect(),
            per_symbol_shares: p
                .per_symbol_shares
                .iter()
                .map(|(s, q)| (s.clone(), q.0))
                .collect(),
            marks: p
                .marks
                .iter()
                .map(|(s, m)| (s.clone(), (m.price_cents.0, m.quote_epoch.0)))
                .collect(),
            captured_at_epoch_secs: p.captured_at_epoch_secs,
            reporting_tz_date: p.reporting_tz_date.0,
            incomplete: p.incomplete,
        }
    }
}

impl From<PointWire> for reports::SeriesPoint {
    fn from(w: PointWire) -> Self {
        reports::SeriesPoint {
            key: reports::TradingDayKey(pt_core::Date(w.key)),
            total_market_value_cents: pt_core::Cents(w.total_market_value_cents),
            total_unrealized_pretax_cents: pt_core::Cents(w.total_unrealized_pretax_cents),
            total_unrealized_net_of_tax_cents: pt_core::Cents(w.total_unrealized_net_of_tax_cents),
            total_basis_cents: pt_core::Cents(w.total_basis_cents),
            per_symbol_value_cents: w
                .per_symbol_value_cents
                .into_iter()
                .map(|(s, c)| (s, pt_core::Cents(c)))
                .collect(),
            per_symbol_shares: w
                .per_symbol_shares
                .into_iter()
                .map(|(s, q)| (s, pt_core::MicroShares(q)))
                .collect(),
            marks: w
                .marks
                .into_iter()
                .map(|(s, (price, epoch))| {
                    (
                        s,
                        reports::PricedMark {
                            price_cents: pt_core::Cents(price),
                            quote_epoch: pt_core::Date(epoch),
                        },
                    )
                })
                .collect(),
            captured_at_epoch_secs: w.captured_at_epoch_secs,
            reporting_tz_date: pt_core::Date(w.reporting_tz_date),
            incomplete: w.incomplete,
        }
    }
}

/// A minimal serde wire form for the config seam round-trip: the platform
/// suggestion list (the simplest domain table with a public reader/constructor —
/// `PlatformList::names` / `::new` — to faithfully round-trip without a serde derive
/// on `config`'s types). Proves the put/load rides the one primitive.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct ConfigWire {
    /// One entry per tax year (a Vec, not a map — JSON map keys must be strings).
    #[serde(default)]
    rules_by_year: Vec<TaxRulesWire>,
    #[serde(default)]
    de_minimis_cents: i64,
    /// `(effective_date_days, state_code)`, ascending.
    #[serde(default)]
    residency: Vec<(i32, String)>,
    #[serde(default)]
    platforms: Vec<String>,
    #[serde(default)]
    aliases: BTreeMap<String, String>,
    /// Symbol → company display name (the alias map's sibling). Defaults empty so
    /// a pre-existing workbook cell (written before the field existed) still
    /// decodes. (CONFIG-PLATFORM-003)
    #[serde(default)]
    display_names: BTreeMap<String, String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct TaxRulesWire {
    tax_year: i32,
    /// `Single` / `MarriedFilingJointly` / `MarriedFilingSeparately` / `HeadOfHousehold`.
    filing_status: String,
    federal_ordinary: BracketSetWire,
    federal_long_term: BracketSetWire,
    niit_rate_ppm: i64,
    niit_magi_threshold_cents: i64,
    state_ordinary: BTreeMap<String, BracketSetWire>,
    ordinary_income_cents: i64,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct BracketSetWire {
    /// `(lower_threshold_cents, rate_ppm)`, ascending.
    rows: Vec<(i64, i64)>,
    last_verified_days: i32,
    source_note: String,
}

fn bracket_set_wire(s: &config::BracketSet) -> BracketSetWire {
    BracketSetWire {
        rows: s.rows.iter().map(|r| (r.lower_threshold_cents.0, r.rate_ppm.0)).collect(),
        last_verified_days: s.last_verified.0,
        source_note: s.source_note.clone(),
    }
}

fn bracket_set_from_wire(w: &BracketSetWire) -> config::BracketSet {
    config::BracketSet {
        rows: w
            .rows
            .iter()
            .map(|&(cents, ppm)| config::BracketRow {
                lower_threshold_cents: pt_core::Cents(cents),
                rate_ppm: config::Ppm(ppm),
            })
            .collect(),
        last_verified: pt_core::Date(w.last_verified_days),
        source_note: w.source_note.clone(),
    }
}

fn filing_status_str(f: &config::FilingStatus) -> &'static str {
    match f {
        config::FilingStatus::Single => "Single",
        config::FilingStatus::MarriedFilingJointly => "MarriedFilingJointly",
        config::FilingStatus::MarriedFilingSeparately => "MarriedFilingSeparately",
        config::FilingStatus::HeadOfHousehold => "HeadOfHousehold",
    }
}

fn filing_status_from(s: &str) -> Option<config::FilingStatus> {
    Some(match s {
        "Single" => config::FilingStatus::Single,
        "MarriedFilingJointly" => config::FilingStatus::MarriedFilingJointly,
        "MarriedFilingSeparately" => config::FilingStatus::MarriedFilingSeparately,
        "HeadOfHousehold" => config::FilingStatus::HeadOfHousehold,
        _ => return None,
    })
}

impl From<&config::ConfigData> for ConfigWire {
    fn from(d: &config::ConfigData) -> Self {
        ConfigWire {
            rules_by_year: d
                .rules_by_year
                .values()
                .map(|r| TaxRulesWire {
                    tax_year: r.tax_year.0,
                    filing_status: filing_status_str(&r.filing_status).to_string(),
                    federal_ordinary: bracket_set_wire(&r.federal_ordinary),
                    federal_long_term: bracket_set_wire(&r.federal_long_term),
                    niit_rate_ppm: r.niit.rate_ppm.0,
                    niit_magi_threshold_cents: r.niit.magi_threshold_cents.0,
                    state_ordinary: r
                        .state_ordinary
                        .iter()
                        .map(|(code, set)| (code.clone(), bracket_set_wire(set)))
                        .collect(),
                    ordinary_income_cents: r.ordinary_income_cents.0,
                })
                .collect(),
            de_minimis_cents: d.de_minimis.0 .0,
            residency: d
                .residency
                .entries()
                .iter()
                .map(|e| (e.effective_date.0, e.state_code.clone()))
                .collect(),
            platforms: d.platforms.names().to_vec(),
            aliases: d.aliases.entries().clone(),
            display_names: d.display_names.entries().clone(),
        }
    }
}

impl ConfigWire {
    /// Rebuild the typed `ConfigData`; `None` on an unintelligible wire value
    /// (an unknown filing status, an invalid residency timeline) — the caller
    /// surfaces the failure rather than silently defaulting owner config away.
    fn into_data(self) -> Option<config::ConfigData> {
        let mut rules_by_year = BTreeMap::new();
        for w in &self.rules_by_year {
            rules_by_year.insert(
                config::TaxYear(w.tax_year),
                config::TaxRules {
                    tax_year: config::TaxYear(w.tax_year),
                    filing_status: filing_status_from(&w.filing_status)?,
                    federal_ordinary: bracket_set_from_wire(&w.federal_ordinary),
                    federal_long_term: bracket_set_from_wire(&w.federal_long_term),
                    niit: config::Niit {
                        rate_ppm: config::Ppm(w.niit_rate_ppm),
                        magi_threshold_cents: pt_core::Cents(w.niit_magi_threshold_cents),
                    },
                    state_ordinary: w
                        .state_ordinary
                        .iter()
                        .map(|(code, set)| (code.clone(), bracket_set_from_wire(set)))
                        .collect(),
                    ordinary_income_cents: pt_core::Cents(w.ordinary_income_cents),
                },
            );
        }
        let residency = if self.residency.is_empty() {
            config::ResidencyTimeline::new()
        } else {
            config::ResidencyTimeline::from_entries(
                self.residency
                    .iter()
                    .map(|(days, code)| config::ResidencyEntry {
                        effective_date: pt_core::Date(*days),
                        state_code: code.clone(),
                    })
                    .collect(),
            )
            .ok()?
        };
        Some(config::ConfigData {
            rules_by_year,
            de_minimis: config::DeMinimis(pt_core::Cents(self.de_minimis_cents)),
            residency,
            platforms: config::PlatformList::new(self.platforms),
            aliases: config::AliasMap::new(self.aliases),
            display_names: config::DisplayNameMap::new(self.display_names),
        })
    }
}

/// Map a low-level [`SheetsError`] to `reports`'s History error contract: a
/// transport/API failure on a read is `Unreachable` (REPORT-HIST-004).
fn map_history_err(_e: SheetsError) -> reports::HistoryError {
    reports::HistoryError::Unreachable
}
