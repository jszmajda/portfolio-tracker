//! `sheets-view` — the projection half of the workbook.
//!
//! Regenerates clean, read-only, filterable **view tabs** (Positions, Open Lots,
//! Realized, Tax) from the kernels' output, and is the one place the **live
//! price** enters the system (via `GOOGLEFINANCE`). It consumes `ledger-core`'s
//! `Snapshot` (positions, open lots, realized gains) and `tax`'s accruals /
//! reserves / per-position effective unrealized rate, writes them as view tabs,
//! and reads the `GOOGLEFINANCE`-computed prices **back** as the per-symbol marks
//! `runtime` caches and injects into the next replay. It owns no accounting or
//! tax math — it renders and it prices. See
//! `docs/intent/sheets-view/sheets-view-design.md` and `-specs.md` (prefix
//! `SHEET`).
//!
//! This crate is NOT a `verus!{}` crate: all the sheet I/O and the value/formula
//! boundary live OUTSIDE the verified kernels (CLAUDE.md "Verify the money math;
//! trust the I/O"). `cargo test` is the gate. The price→`Cents` conversion uses
//! `pt_core::round_half_to_even`, the project's single canonical rounding site.
//!
//! The low-level Sheets-access layer is `runtime`'s; `sheets-view` depends on the
//! thin [`SheetsViewClient`] trait (modeled on `store`'s `SheetsClient`), with an
//! in-memory fake ([`testkit::InMemorySheetsView`]) for ALL tests.

use std::collections::BTreeMap;

use config::{AliasMap, Ppm};
use ledger_core::{LotId, SaleId, Snapshot, Symbol};
use pt_core::{Cents, Date, Lock, MicroShares};
use tax::{Accrual, AnnualRow};

pub mod testkit;

// ===========================================================================
// Workbook tab layout (sheets-view-design.md → "Workbook Tab Layout";
// SHEET-TAB-002). sheets-view owns the ORDERING; each segment owns its tab's
// content. Most-used surfaces left, config in the middle, append-only logs far
// right; `Positions` is the leftmost (landing) tab.
// ===========================================================================

/// The Positions view tab name (the landing tab — leftmost). (SHEET-TAB-002)
pub const POSITIONS_TAB: &str = "Positions";
/// The Open Lots view tab name.
pub const OPEN_LOTS_TAB: &str = "Open Lots";
/// The Realized view tab name.
pub const REALIZED_TAB: &str = "Realized";
/// The Tax view tab name.
pub const TAX_TAB: &str = "Tax";

/// The Tax tab's reserve-summary band name — a **distinct typed range** carrying
/// the per-`(Jurisdiction, Tax Year)` reserve summary, kept separate from the
/// accrual band so neither carries among-data summary rows and both stay natively
/// filterable. It is part of the Tax surface, not a fifth entry in [`VIEW_TABS`]
/// or [`workbook_tab_order`]. (SHEET-TAB-001, SHEET-FORMULA-004)
pub const TAX_RESERVE_TAB: &str = "Tax · Reserve Summary";

/// The History tab — owned by `reports`, but sits among the views in the
/// ordering. `sheets-view` does not write it; it only places it. (SHEET-TAB-002)
pub const HISTORY_TAB: &str = "History";

/// The config tab names, owned by `config`. `sheets-view` only places them.
pub const TAX_RULES_TAB: &str = "Tax Rules";
pub const RESIDENCY_TAB: &str = "Residency";
pub const PLATFORMS_ALIASES_TAB: &str = "Platforms & Aliases";

/// The append-only event-log tab names, owned by `store`. Far right (canonical
/// truth, machine-written, read by hand only for audit).
pub const LEDGER_EVENTS_TAB: &str = "Ledger Events";
pub const TAX_EVENTS_TAB: &str = "Tax Events";

/// The four view tabs `sheets-view` publishes, in left-to-right order.
/// (SHEET-TAB-001/002)
pub const VIEW_TABS: [&str; 4] = [POSITIONS_TAB, OPEN_LOTS_TAB, REALIZED_TAB, TAX_TAB];

/// The full workbook tab ordering `sheets-view` owns and idempotently reasserts
/// on republish: views → reports(History) → config → event logs, with
/// `Positions` leftmost. (SHEET-TAB-002)
pub fn workbook_tab_order() -> Vec<&'static str> {
    vec![
        // Views (sheets-view).
        POSITIONS_TAB,
        OPEN_LOTS_TAB,
        REALIZED_TAB,
        TAX_TAB,
        // Reports (History sits among the views in the ordering).
        HISTORY_TAB,
        // Config tabs.
        TAX_RULES_TAB,
        RESIDENCY_TAB,
        PLATFORMS_ALIASES_TAB,
        // Event logs (store) — far right.
        LEDGER_EVENTS_TAB,
        TAX_EVENTS_TAB,
    ]
}

// ===========================================================================
// The value / formula boundary (sheets-view-design.md → "The Engine-Value /
// Live-Formula Boundary"; SHEET-FORMULA-001/002). A `Cell` is either an
// engine-written value or a live `GOOGLEFINANCE`-dependent formula. The split is
// represented structurally so the publish path and tests can assert it.
// ===========================================================================

/// One published cell: an engine-written **value** or a live-price-dependent
/// **formula** (the `GOOGLEFINANCE` columns). The split is the whole point of
/// the value/formula boundary. (SHEET-FORMULA-001)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Cell {
    /// A value the engine computed from the log (shares, basis, realized P&L,
    /// the `tax`-written effective rate, …).
    Value(String),
    /// A live sheet formula (`=GOOGLEFINANCE(…)`, market value, unrealized, the
    /// post-tax estimate columns) that recomputes as the mark moves.
    Formula(String),
}

impl Cell {
    /// Whether this cell is a live formula (vs an engine value).
    pub fn is_formula(&self) -> bool {
        matches!(self, Cell::Formula(_))
    }

    /// The raw cell text (value text, or the formula string including its `=`).
    pub fn text(&self) -> &str {
        match self {
            Cell::Value(v) => v,
            Cell::Formula(f) => f,
        }
    }
}

/// One published view-tab row: an ordered vector of [`Cell`]s parallel to the
/// tab's header. One row per entity (SHEET-TAB-001).
pub type ViewRow = Vec<Cell>;

/// A fully-rendered view tab ready to publish atomically: its name, the typed
/// **frozen header** (never rewritten — SHEET-FORMULA-002), and the data rows
/// written from the fixed anchored start row beneath it. (SHEET-TAB-001)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ViewTab {
    pub name: String,
    /// The typed column header. Frozen; never rewritten on republish.
    /// (SHEET-FORMULA-002, SHEET-TAB-003)
    pub header: Vec<String>,
    /// One row per entity, in the tab's natural order. (SHEET-TAB-001)
    pub rows: Vec<ViewRow>,
}

/// The fixed anchored start row (1-based, beneath the frozen header) that data
/// rows are written from. The frozen header occupies row 1; data begins at row
/// 2. Per-row formulas reference cells on **their own** row. (SHEET-FORMULA-002)
pub const DATA_START_ROW: u32 = 2;

/// The fully-rendered Tax tab: the per-accrual band and the per-`(Jurisdiction,
/// Tax Year)` reserve summary, each as its **own** typed band with its own frozen
/// header. The design calls for both on the Tax tab, but SHEET-TAB-001 forbids
/// among-data summary rows — so the two are kept as distinct one-row-per-entity
/// ranges (never a blank/label/embedded-header band stuffed inside the accrual
/// range), so native filtering/sorting works on each. (SHEET-TAB-001,
/// SHEET-FORMULA-004)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TaxView {
    /// The accrual band — one row per accrual, under [`TAX_HEADER`]. Natively
    /// filterable: no among-data summary rows. (SHEET-TAB-001)
    pub accruals: ViewTab,
    /// The reserve summary band — one row per `(Jurisdiction, Tax Year)`, under
    /// [`TAX_RESERVE_HEADER`], with the kernel-exact accrued / moved / paid /
    /// outstanding / shortfall figures. A distinct typed range. (SHEET-FORMULA-004)
    pub reserve_summary: ViewTab,
}

// ===========================================================================
// View-tab headers (sheets-view-design.md → "View Tabs"; SHEET-TAB-001). One row
// per entity, typed headers, no among-data summary rows.
// ===========================================================================

/// The Positions tab header: per symbol, the live pre/post-tax glance. The
/// post-tax columns are labelled **estimates**. (SHEET-TAB-001, SHEET-FORMULA-003)
pub const POSITIONS_HEADER: &[&str] = &[
    "Symbol",
    "Shares",
    "Avg Cost/Share",
    "Total Basis",
    "Price",            // formula (GOOGLEFINANCE)
    "Market Value",     // formula
    "Unrealized P&L",   // formula, pre-tax
    "Est. Tax Rate",    // value, from tax
    "Est. Unrealized Tax (est)", // formula, post-tax estimate
    "Net Unrealized (est)",      // formula, post-tax estimate
    "Unrealized %",     // formula
    "Realized P&L",     // value
    "Quote Date",       // formula — the locale-proof companion the read-back parses
];

/// The Open Lots tab header: per open lot. (SHEET-TAB-001)
pub const OPEN_LOTS_HEADER: &[&str] = &[
    "Lot ID",
    "Symbol",
    "Acquire Date",
    "Source",
    "Shares",
    "Total Basis",
    "Basis/Share",
    "Platform",
    "Tracking Code",
    "Term",
];

/// The Realized tab header: per realized gain. (SHEET-TAB-001)
pub const REALIZED_HEADER: &[&str] = &[
    "Sale ID",
    "Lot ID",
    "Symbol",
    "Sale Date",
    "Proceeds",
    "Basis",
    "Gain",
    "Holding Days",
    "Term",
    "State",
    "Tax Year",
];

/// The Tax tab header: accruals + a reserve summary. Kernel-exact stacked figures.
/// (SHEET-TAB-001, SHEET-FORMULA-004)
pub const TAX_HEADER: &[&str] = &[
    "Sale ID",
    "Lot ID",
    "Jurisdiction",
    "Tax Year",
    "Amount",
    "Lifecycle State",
];

/// The Tax tab's reserve-summary header (one row per `(Jurisdiction, Tax Year)`).
/// Presented in a separate band; kernel-exact. (SHEET-FORMULA-004)
pub const TAX_RESERVE_HEADER: &[&str] = &[
    "Jurisdiction",
    "Tax Year",
    "Accrued",
    "Moved",
    "Paid",
    "Outstanding",
    "Shortfall",
];

// ===========================================================================
// Symbol → ticker resolution (sheets-view-design.md → "Symbol → Ticker Mapping";
// SHEET-MAP-001). Identity plus the config alias table; the `Price` formula is
// built from the resolved ticker.
// ===========================================================================

/// Resolve a ledger `Symbol` to its `GOOGLEFINANCE` ticker via `config`'s alias
/// map (identity when unset). (SHEET-MAP-001)
pub fn resolve_ticker(aliases: &AliasMap, symbol: &str) -> String {
    aliases.resolve(symbol)
}

/// Build the live `Price` formula for a symbol from its resolved ticker:
/// `=GOOGLEFINANCE("TICKER","price")`. The ticker is quoted; the cell is a live
/// formula, not an engine value. (SHEET-MAP-001, SHEET-FORMULA-001)
pub fn price_formula(ticker: &str) -> Cell {
    Cell::Formula(format!("=GOOGLEFINANCE(\"{ticker}\",\"price\")"))
}

/// Build the companion `Quote Date` formula for a symbol, used to stamp the mark
/// with `GOOGLEFINANCE`'s own quote date rather than wall-clock read time.
///
/// The encoding is part of the formula contract: the settle pass reads the tab
/// back through the plain *values* API (formatted strings), and `GOOGLEFINANCE`'s
/// raw `tradetime` renders as a locale-dependent datetime — unparseable without
/// locale rules. So the formula truncates the trade time to its date serial,
/// re-bases it to **days since 1970-01-01** (the kernel `Date` epoch) by
/// subtracting `DATE(1970,1,1)`, and renders it via `TEXT(…,"0")` — a digits-only
/// string every locale formats identically. While the quote is loading/errored the
/// cell is empty (`IFERROR`), never an error string. (SHEET-MARK-004/008)
// @spec SHEET-MARK-008
pub fn quote_date_formula(ticker: &str) -> Cell {
    Cell::Formula(format!(
        "=IFERROR(TEXT(INT(GOOGLEFINANCE(\"{ticker}\",\"tradetime\"))-DATE(1970,1,1),\"0\"),\"\")"
    ))
}

// ===========================================================================
// Marks read-back (sheets-view-design.md → "Marks: Read-Back, Settle &
// Caching"; SHEET-MARK-*). sheets-view is the marks PRODUCER; runtime caches /
// reduces them.
// ===========================================================================

/// One symbol's `Price`-cell reading on a settle pass — what the client read
/// back from the `GOOGLEFINANCE` column. (SHEET-MARK-001/003)
#[derive(Clone, PartialEq, Debug)]
pub enum PriceReading {
    /// A settled numeric price (USD) with its `GOOGLEFINANCE` quote date.
    /// (SHEET-MARK-002/004)
    Numeric { price_usd: f64, quote_date: Date },
    /// A transient `#N/A` / `Loading...` — not-yet-known; retry, keep prior mark.
    /// (SHEET-MARK-003)
    Transient,
    /// A terminal non-numeric error (a permanent `#N/A`, usually a ticker-form
    /// mismatch). Terminal: stop polling this symbol. (SHEET-MARK-003)
    Permanent,
}

/// The price-cell readings for every symbol on one settle pass: symbol → reading,
/// keyed by **row identity** (the row's `Symbol`). (SHEET-MARK-001, SHEET-MAP-002)
pub type PricePass = BTreeMap<Symbol, PriceReading>;

/// Why a symbol carries no mark this cycle. (SHEET-MARK-002/003)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DegradeReason {
    /// Stayed transient (`#N/A`/`Loading...`) past the bounded retry window.
    /// (SHEET-MARK-003)
    TimedOut,
    /// A terminal non-numeric error (permanent `#N/A`). (SHEET-MARK-003)
    Permanent,
    /// A positive price that rounded to 0 `Cents` — a degraded anomaly, never a
    /// real zero mark. (SHEET-MARK-002)
    SubCent,
}

/// One settled per-symbol mark: a price in `Cents` stamped with `GOOGLEFINANCE`'s
/// own quote date (not wall-clock). (SHEET-MARK-002/004)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Mark {
    pub price_cents: Cents,
    /// `GOOGLEFINANCE`'s quote date — the per-symbol quote-epoch stamp.
    /// (SHEET-MARK-004)
    pub quote_date: Date,
}

/// The marks `sheets-view` produces for `runtime` to cache and inject into the
/// next replay: the good per-symbol marks, the degraded symbols (with the reason,
/// no mark), and the carried-forward prior marks for symbols that were only
/// transiently unknown this pass. (sheets-view-design.md → "Trust Boundary &
/// Interfaces"; SHEET-MARK-003/005)
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct MarksProduced {
    /// Symbol → freshly-read good mark (numeric, stamped with its quote date).
    pub marks: BTreeMap<Symbol, Mark>,
    /// Symbols recorded **degraded (no mark)** this cycle, with the reason.
    /// These feed `ledger-core`'s per-symbol degradation. (SHEET-MARK-005)
    pub degraded: BTreeMap<Symbol, DegradeReason>,
}

impl MarksProduced {
    /// Project to the `ledger_core::Marks` map (symbol → `Cents`) that
    /// `runtime` injects into replay. A degraded symbol is ABSENT (so
    /// `ledger-core` degrades it per-symbol — `LEDGER-PNL-007`), never a zero.
    /// (SHEET-MARK-005)
    pub fn to_ledger_marks(&self) -> ledger_core::Marks {
        self.marks
            .iter()
            .map(|(s, m)| (s.clone(), m.price_cents))
            .collect()
    }
}

/// The single rounding site for the price→`Cents` conversion: USD price assumed,
/// `round_half_to_even(price × 100)`. A positive price that rounds to 0 `Cents`
/// is a **degraded anomaly** (`Err(DegradeReason::SubCent)`), never a real zero
/// mark. A genuine 0.0 (or non-positive) price also yields the sub-cent degrade
/// — there is no real zero mark. (SHEET-MARK-002)
///
/// Multiplication is done in integer cents via the canonical rounding helper;
/// the float price is the input boundary (no float past it). `price_usd × 100`
/// is computed by scaling the float to a `×1e6` integer micro-cents value and
/// folding through `round_half_to_even`, so the result matches the project's one
/// rounding rule. A non-positive numeric reading (`0.0`/negative) degrades SubCent;
/// a non-finite reading (`NaN`/`±∞`) degrades Permanent — never a mark.
/// (SHEET-MARK-007)
// @spec SHEET-MARK-007
pub fn price_to_cents(price_usd: f64) -> Result<Cents, DegradeReason> {
    // The float is the input boundary. Scale to an integer numerator of
    // (price × 100 × SCALE) and round once via round_half_to_even over SCALE, so
    // the half-to-even rule — not the float's own rounding — decides the cent.
    const SCALE: i128 = 1_000_000;
    if !price_usd.is_finite() {
        return Err(DegradeReason::Permanent);
    }
    // micro = round(price_usd × 100 × SCALE) as an integer numerator. The
    // multiply happens in f64 only to cross the boundary; the cent decision is
    // the integer round_half_to_even below.
    let scaled = price_usd * 100.0 * SCALE as f64;
    if !scaled.is_finite() {
        return Err(DegradeReason::Permanent);
    }
    let numerator = scaled.round() as i128;
    let cents = pt_core::round_half_to_even(numerator, SCALE);
    // A positive price rounding to 0 Cents (or any non-positive price) is a
    // degraded anomaly, not a real zero mark. (SHEET-MARK-002)
    if cents <= 0 {
        return Err(DegradeReason::SubCent);
    }
    Ok(Cents(cents as i64))
}

// ===========================================================================
// The Sheets-access seam (sheets-view-design.md → "Trust Boundary &
// Interfaces"). A thin trait `runtime` implements for real, with an in-memory
// fake for tests (testkit). Modeled on store's `SheetsClient`, but over the
// string-named VIEW tabs and the operations sheets-view needs: atomic
// publish-with-truncate, frozen-header preservation, the settle-pass price read,
// the tab-order reassert, and the stale banner.
// ===========================================================================

/// A failure crossing the sheets-view I/O seam. A failed publish never corrupts
/// truth (the canonical log is `store`'s); it leaves the view stale.
/// (SHEET-PUB-003)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ViewError {
    /// The workbook was unreachable / the `batchUpdate` failed: the view is left
    /// as-is (internally consistent with the prior snapshot, just behind), to be
    /// retried. (SHEET-PUB-003)
    PublishFailed,
    /// The price-cell settle pass timed out as a whole (the bounded window
    /// elapsed before every cell was numeric-or-terminally-errored).
    /// (SHEET-MARK-001)
    SettleTimeout,
    /// `Snapshot.positions` somehow carried two rows for one symbol — the
    /// one-row-per-symbol invariant the read-back keys on was violated.
    /// (SHEET-MAP-002)
    DuplicateSymbolRow(Symbol),
}

impl std::fmt::Display for ViewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ViewError {}

/// The low-level Sheets-access layer `sheets-view` needs, over the string-named
/// **view** tabs. `runtime` implements this against the Sheets API; tests use the
/// in-memory fake [`testkit::InMemorySheetsView`].
pub trait SheetsViewClient {
    /// Atomically rewrite a view tab as a single `batchUpdate`: write `tab.rows`
    /// from the fixed anchored start row beneath the **never-rewritten** frozen
    /// header, and **truncate the residual tail** to the exact row count, so
    /// there is no half-written window and no leftover stale rows. Returns
    /// `Err(PublishFailed)` if the underlying call fails — leaving the tab as-is.
    /// (SHEET-PUB-002, SHEET-FORMULA-002, SHEET-TAB-003)
    ///
    /// # Contract the real (`runtime`) client MUST uphold
    /// - **Frozen header.** Row 1 (the typed header) is written **once** and is
    ///   never rewritten on republish; only the data range from [`DATA_START_ROW`]
    ///   down is rewritten + tail-truncated. A republish presenting a different
    ///   header is a contract violation. (SHEET-FORMULA-002, SHEET-TAB-003)
    /// - **Value typing.** A [`Cell::Value`] is written as a Sheets
    ///   `userEnteredValue` (NOT a forced string/`stringValue`), so Sheets parses
    ///   it the way a human typing the same text would: a bare number as a number,
    ///   and a percent-suffixed string such as `"20.0000%"` (the `Est. Tax Rate`
    ///   cell — see [`fmt_ppm`]) as the **numeric fraction** `0.20` with a percent
    ///   number-format. The live post-tax formulas multiply that cell
    ///   (`=G*H`, `=G*(1-H)`), so it MUST coerce to a number, never inert text.
    ///   (SHEET-FORMULA-003)
    /// - **Formula typing.** A [`Cell::Formula`] is written as a `userEnteredValue`
    ///   formula (leading `=`), so `GOOGLEFINANCE` recalculates. (SHEET-FORMULA-001)
    fn batch_update_view(&mut self, tab: &ViewTab) -> Result<(), ViewError>;

    /// Idempotently reassert the workbook tab ordering. (SHEET-TAB-002)
    fn set_tab_order(&mut self, order: &[&str]) -> Result<(), ViewError>;

    /// Read the `Price` (and companion quote-`date`) cells of the Positions tab
    /// back on a **separate settle pass** — never inline with the formula write.
    /// The fake returns one [`PriceReading`] per symbol; the real client reads the
    /// recalculated `GOOGLEFINANCE` cells. Each call is one poll; the settle loop
    /// polls until numeric-or-terminally-errored or the bounded window elapses.
    /// (SHEET-MARK-001)
    fn read_price_pass(&self) -> Result<PricePass, ViewError>;

    /// Write the best-effort single-cell in-tab STALE banner (e.g. on a view tab
    /// whose republish failed). Best-effort: if even this write fails the TUI
    /// remains the staleness surface. (SHEET-PUB-003)
    fn write_stale_banner(&mut self, tab: &str, banner: &str) -> Result<(), ViewError>;
}

// ===========================================================================
// Rendering the four view tabs (sheets-view-design.md → "View Tabs" / "The
// Engine-Value / Live-Formula Boundary"; SHEET-TAB-001, SHEET-FORMULA-001..004,
// SHEET-MAP-001). Pure functions Snapshot+tax → ViewTab; no I/O.
// ===========================================================================

/// Format `Cents` as a dollar string (e.g. `Cents(150_00)` → `"150.00"`). Signed.
fn fmt_cents(c: Cents) -> String {
    let v = c.0;
    let sign = if v < 0 { "-" } else { "" };
    let a = v.unsigned_abs();
    format!("{sign}{}.{:02}", a / 100, a % 100)
}

/// Format `MicroShares` as a share quantity with up to 6 decimals, trimmed.
fn fmt_shares(q: MicroShares) -> String {
    let v = q.0;
    let sign = if v < 0 { "-" } else { "" };
    let a = v.unsigned_abs();
    let whole = a / 1_000_000;
    let frac = a % 1_000_000;
    if frac == 0 {
        format!("{sign}{whole}")
    } else {
        let s = format!("{frac:06}");
        let s = s.trim_end_matches('0');
        format!("{sign}{whole}.{s}")
    }
}

/// Format `Ppm` as a rate percentage string (e.g. `Ppm(150_000)` → `"15.0000%"`).
fn fmt_ppm(p: Ppm) -> String {
    // ppm / 1e6 = fraction; ×100 for percent → ppm / 1e4 percent.
    let v = p.0;
    let sign = if v < 0 { "-" } else { "" };
    let a = v.unsigned_abs();
    let whole = a / 10_000;
    let frac = a % 10_000;
    format!("{sign}{whole}.{frac:04}%")
}

/// A spreadsheet A1 cell reference for a column letter at a given 1-based row.
fn cell_ref(col: char, row: u32) -> String {
    format!("{col}{row}")
}

/// Render the **Positions** view tab from the `Snapshot`, the per-position
/// effective unrealized tax rate (`tax`'s `UnrealizedEstimate.effective_rate_ppm`,
/// a value), and the alias map for the `Price` ticker. The live-price columns are
/// row-anchored formulas referencing **their own** row's cells; everything else is
/// an engine value. The post-tax columns are estimates. (SHEET-TAB-001,
/// SHEET-FORMULA-001/002/003, SHEET-MAP-001)
///
/// `effective_rates` maps symbol → the `tax`-written effective unrealized rate
/// (`Ppm`); a symbol absent from it (degraded estimate) renders an empty rate and
/// the post-tax columns degrade to the pre-tax figure.
// @spec SHEET-FORMULA-005, SHEET-MARK-008
pub fn render_positions(
    snapshot: &Snapshot,
    effective_rates: &BTreeMap<Symbol, Ppm>,
    aliases: &AliasMap,
) -> Result<ViewTab, ViewError> {
    // Column letters for the row-anchored formulas (parallel to POSITIONS_HEADER).
    //  A Symbol | B Shares | C AvgCost | D TotalBasis | E Price | F MarketValue |
    //  G UnrealizedP&L | H EstTaxRate | I EstUnrealTax | J NetUnreal | K Unreal% |
    //  L RealizedP&L | M QuoteDate (the read-back companion, SHEET-MARK-008)
    let mut rows: Vec<ViewRow> = Vec::new();
    // Assert one row per symbol. The read-back keys on row identity (symbol), and
    // the design (sheets-view-design.md → "Symbol → Ticker Mapping") calls for an
    // explicit loud error if the invariant is ever violated. With `Snapshot.
    // positions` being a `Map<Symbol,_>` the key is unique by construction, so
    // this guard is structurally UNREACHABLE through the real API — it is a
    // DEFENSIVE assertion (the design-sanctioned "error loudly if violated"), kept
    // so a future non-Map source cannot silently double-render a symbol and
    // false-pair the mark read-back. (SHEET-MAP-002)
    let mut seen: std::collections::BTreeSet<&Symbol> = std::collections::BTreeSet::new();

    for (symbol, pos) in &snapshot.positions {
        if !seen.insert(symbol) {
            return Err(ViewError::DuplicateSymbolRow(symbol.clone()));
        }
        let r = (rows.len() as u32) + DATA_START_ROW; // this row's 1-based number
        let ticker = resolve_ticker(aliases, symbol);

        // Avg cost/share = total_basis / shares (engine value; guard zero shares).
        let avg_cost = if pos.total_qty.0 != 0 {
            // basis_cents per whole share = total_basis / (qty / 1e6).
            let per_share =
                pt_core::round_half_to_even((pos.total_basis_cents.0 as i128) * 1_000_000, pos.total_qty.0 as i128);
            fmt_cents(Cents(per_share as i64))
        } else {
            "0.00".to_string()
        };

        let eff_rate = effective_rates.get(symbol).copied();

        // Live formulas reference THIS row's own cells (SHEET-FORMULA-002).
        let market_value = Cell::Formula(format!("={}*{}", cell_ref('E', r), cell_ref('B', r)));
        let unrealized = Cell::Formula(format!("={}-{}", cell_ref('F', r), cell_ref('D', r)));
        // Post-tax estimate columns (labelled estimates): Est. Unrealized Tax =
        // Unrealized P&L × Est. Tax Rate; Net Unrealized = Unrealized P&L ×
        // (1 − Est. Tax Rate). (SHEET-FORMULA-003)
        let est_tax = Cell::Formula(format!("={}*{}", cell_ref('G', r), cell_ref('H', r)));
        let net_unreal = Cell::Formula(format!("={}*(1-{})", cell_ref('G', r), cell_ref('H', r)));
        let unreal_pct = Cell::Formula(format!("=IF({d}=0,0,{g}/{d})", d = cell_ref('D', r), g = cell_ref('G', r)));

        rows.push(vec![
            Cell::Value(symbol.clone()),                            // A Symbol
            Cell::Value(fmt_shares(pos.total_qty)),                 // B Shares
            Cell::Value(avg_cost),                                  // C Avg Cost/Share
            Cell::Value(fmt_cents(pos.total_basis_cents)),          // D Total Basis
            price_formula(&ticker),                                 // E Price (formula)
            market_value,                                           // F Market Value (formula)
            unrealized,                                             // G Unrealized P&L (formula)
            // H Est. Tax Rate: an engine VALUE from tax (empty when degraded).
            Cell::Value(eff_rate.map(fmt_ppm).unwrap_or_default()),
            est_tax,                                                // I Est. Unrealized Tax (formula)
            net_unreal,                                             // J Net Unrealized (formula)
            unreal_pct,                                             // K Unrealized % (formula)
            Cell::Value(fmt_cents(pos.realized_pnl_cents)),         // L Realized P&L (value)
            quote_date_formula(&ticker),                            // M Quote Date (formula)
        ]);
    }

    Ok(ViewTab {
        name: POSITIONS_TAB.to_string(),
        header: POSITIONS_HEADER.iter().map(|s| s.to_string()).collect(),
        rows,
    })
}

/// Render the **Open Lots** view tab from the `Snapshot`'s open lots. All
/// engine values (no live-price columns). `as_of` classifies each lot LT/ST via
/// `tax::classify_term`. (SHEET-TAB-001, SHEET-FORMULA-001)
pub fn render_open_lots(snapshot: &Snapshot, as_of: Date) -> ViewTab {
    let mut rows: Vec<ViewRow> = Vec::new();
    for ol in &snapshot.open_lots {
        let lot = &ol.lot;
        let basis_per_share = if lot.remaining_qty.0 != 0 {
            let per = pt_core::round_half_to_even(
                (lot.remaining_basis_cents.0 as i128) * 1_000_000,
                lot.remaining_qty.0 as i128,
            );
            fmt_cents(Cents(per as i64))
        } else {
            "0.00".to_string()
        };
        let term = match tax::classify_term(lot.acquire_date, as_of) {
            tax::Term::LongTerm => "LT",
            tax::Term::ShortTerm => "ST",
        };
        rows.push(vec![
            Cell::Value(lot.id.clone()),
            Cell::Value(lot.symbol.clone()),
            Cell::Value(lot.acquire_date.0.to_string()),
            Cell::Value(match lot.source {
                ledger_core::LotSource::Buy => "Buy".to_string(),
                ledger_core::LotSource::Vest => "Vest".to_string(),
            }),
            Cell::Value(fmt_shares(lot.remaining_qty)),
            Cell::Value(fmt_cents(lot.remaining_basis_cents)),
            Cell::Value(basis_per_share),
            Cell::Value(lot.platform.clone()),
            Cell::Value(lot.tracking_code.clone().unwrap_or_default()),
            Cell::Value(term.to_string()),
        ]);
    }
    ViewTab {
        name: OPEN_LOTS_TAB.to_string(),
        header: OPEN_LOTS_HEADER.iter().map(|s| s.to_string()).collect(),
        rows,
    }
}

/// Render the **Realized** view tab from the `Snapshot`'s realized gains plus the
/// per-gain lifecycle state from `tax`'s accruals (keyed by `(sale_id, lot_id)` on
/// the Federal accrual). All engine values. (SHEET-TAB-001, SHEET-FORMULA-001)
pub fn render_realized(snapshot: &Snapshot, accruals: &[Accrual]) -> ViewTab {
    // Index the Federal accrual's lifecycle state by (sale_id, lot_id) for the
    // realized view's `State` column.
    let mut state_by: BTreeMap<(SaleId, LotId), String> = BTreeMap::new();
    for a in accruals {
        if let config::Jurisdiction::Federal = a.key.jurisdiction {
            state_by.insert(
                (a.key.sale_id.clone(), a.key.lot_id.clone()),
                lifecycle_label(&a.state),
            );
        }
    }

    let mut rows: Vec<ViewRow> = Vec::new();
    for g in &snapshot.realized_gains {
        let term = match tax::classify_term(g.acquire_date, g.sale_date) {
            tax::Term::LongTerm => "LT",
            tax::Term::ShortTerm => "ST",
        };
        let tax_year = tax::tax_year_of(g.sale_date);
        let state = state_by
            .get(&(g.sale_id.clone(), g.lot_id.clone()))
            .cloned()
            .unwrap_or_else(|| "Accrued".to_string());
        rows.push(vec![
            Cell::Value(g.sale_id.clone()),
            Cell::Value(g.lot_id.clone()),
            Cell::Value(g.symbol.clone()),
            Cell::Value(g.sale_date.0.to_string()),
            Cell::Value(fmt_cents(g.proceeds_cents)),
            Cell::Value(fmt_cents(g.basis_cents)),
            Cell::Value(fmt_cents(g.gain_cents)),
            Cell::Value(g.holding_days.to_string()),
            Cell::Value(term.to_string()),
            Cell::Value(state),
            Cell::Value(tax_year.0.to_string()),
        ]);
    }
    ViewTab {
        name: REALIZED_TAB.to_string(),
        header: REALIZED_HEADER.iter().map(|s| s.to_string()).collect(),
        rows,
    }
}

/// Render the **Tax** view: the per-accrual band (kernel-exact applied amounts,
/// lifecycle state) and the per-`(jurisdiction, tax_year)` reserve summary, each
/// as its **own** typed band with its own frozen header. Both are kernel-exact,
/// stacked, as-of-last-sync — distinct from the Positions live estimate; no
/// live-price formulas.
///
/// The accrual band is strictly one row per accrual under [`TAX_HEADER`] with no
/// among-data summary rows, so native Sheets filtering/sorting works directly
/// (SHEET-TAB-001). The reserve summary is a separate band under
/// [`TAX_RESERVE_HEADER`] — never blank/label/embedded-header rows stuffed inside
/// the accrual range.
///
/// The reserve summary's five figures (accrued / moved / paid / outstanding /
/// shortfall) are the kernel-exact values from `tax::annual_report`'s
/// [`AnnualRow`], matched by `(jurisdiction, tax_year)`. (SHEET-TAB-001,
/// SHEET-FORMULA-004)
// @spec SHEET-TAB-004, SHEET-TAB-005
pub fn render_tax(accruals: &[Accrual], annual_rows: &[AnnualRow]) -> TaxView {
    // The accrual band: one row per accrual, no among-data summary rows.
    let mut accrual_rows: Vec<ViewRow> = Vec::new();
    for a in accruals {
        let amount = a.applied_cents.map(fmt_cents).unwrap_or_default();
        accrual_rows.push(vec![
            Cell::Value(a.key.sale_id.clone()),
            Cell::Value(a.key.lot_id.clone()),
            Cell::Value(jurisdiction_label(&a.key.jurisdiction)),
            Cell::Value(a.key.tax_year.0.to_string()),
            Cell::Value(amount),
            Cell::Value(lifecycle_label(&a.state)),
        ]);
    }

    // The reserve summary band: one row per (jurisdiction, tax_year), with the
    // kernel-exact accrued/moved/paid/outstanding/shortfall straight from the
    // annual report (TAX-REPORT-003/004 → AnnualRow). (SHEET-FORMULA-004)
    let mut reserve_rows: Vec<ViewRow> = Vec::new();
    for r in annual_rows {
        reserve_rows.push(vec![
            Cell::Value(jurisdiction_label(&r.jurisdiction)),
            Cell::Value(r.tax_year.0.to_string()),
            Cell::Value(fmt_cents(r.accrued_cents)),
            Cell::Value(fmt_cents(r.moved_cents)),
            Cell::Value(fmt_cents(r.paid_cents)),
            Cell::Value(fmt_cents(r.outstanding_cents)),
            Cell::Value(fmt_cents(r.shortfall_cents)),
        ]);
    }

    TaxView {
        accruals: ViewTab {
            name: TAX_TAB.to_string(),
            header: TAX_HEADER.iter().map(|s| s.to_string()).collect(),
            rows: accrual_rows,
        },
        reserve_summary: ViewTab {
            name: TAX_RESERVE_TAB.to_string(),
            header: TAX_RESERVE_HEADER.iter().map(|s| s.to_string()).collect(),
            rows: reserve_rows,
        },
    }
}

fn jurisdiction_label(j: &config::Jurisdiction) -> String {
    match j {
        config::Jurisdiction::Federal => "Federal".to_string(),
        config::Jurisdiction::State(code) => format!("State({code})"),
    }
}

fn lifecycle_label(s: &tax::AccrualState) -> String {
    match s {
        tax::AccrualState::Accrued => "Accrued".to_string(),
        tax::AccrualState::Allocated { .. } => "Allocated".to_string(),
        tax::AccrualState::Moved { .. } => "Moved".to_string(),
        tax::AccrualState::Paid { .. } => "Paid".to_string(),
    }
}

// ===========================================================================
// The marks settle pass (sheets-view-design.md → "Marks: Read-Back, Settle &
// Caching"; SHEET-MARK-001/002/003/004). The settle loop polls `read_price_pass`
// until every symbol is numeric-or-terminally-errored or the bounded window
// elapses, then converts to per-symbol marks. Carries `prior` so a transiently-
// unknown symbol keeps its prior good cached mark rather than being overwritten.
// ===========================================================================

/// The bounded polling parameters for the settle pass. `max_polls` is the bounded
/// retry window; `runtime`'s real client sleeps between polls (the kernel is
/// clockless, so the loop is poll-count-bounded here). (SHEET-MARK-001/003)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SettleConfig {
    /// The maximum number of settle-pass polls before a still-transient symbol is
    /// recorded degraded (TimedOut). (SHEET-MARK-003)
    pub max_polls: u32,
}

impl Default for SettleConfig {
    fn default() -> Self {
        SettleConfig { max_polls: 8 }
    }
}

/// Read marks back on the separate settle pass: poll `client.read_price_pass`
/// until every symbol is numeric or terminally errored, or the bounded window
/// (`cfg.max_polls`) elapses. Convert each numeric reading to `Cents`
/// (round_half_to_even × 100; sub-cent → degraded); keep the prior good cached
/// mark for a still-transient symbol; record a symbol degraded (no mark) only
/// after it stays transient past the window or is terminally errored.
/// (SHEET-MARK-001/002/003/004)
///
/// `prior` is the previous cycle's marks (so a transient `#N/A` keeps the prior
/// mark, never overwrites it with a transient). `symbols` is the set of position
/// symbols expected on the Positions tab — keying is by row identity (symbol),
/// asserting one entry per symbol (the caller's `Snapshot.positions` guarantees
/// it). (SHEET-MAP-002, SHEET-MARK-003)
///
/// # Transport failure (offline) — boundary contract
/// If the underlying `read_price_pass` errors (the workbook is unreachable /
/// offline), this **propagates the `Err`** and produces **no** `MarksProduced` —
/// it does NOT silently emit an empty marks set, and it does NOT itself carry
/// `prior` forward. The offline carry-forward is `runtime`'s: on `Err`, `runtime`
/// **retains its existing cached marks** (with their quote-epoch stamps) and
/// re-injects them into the next replay, so the kernels keep using the last
/// cached marks while offline (SHEET-MARK-005). Emitting an empty set here would
/// look like "all symbols degraded", wrongly nulling live positions — hence the
/// loud propagation instead.
// @spec SHEET-MARK-006
pub fn read_marks<C: SheetsViewClient>(
    client: &C,
    symbols: &[Symbol],
    prior: &BTreeMap<Symbol, Mark>,
    cfg: SettleConfig,
) -> Result<MarksProduced, ViewError> {
    // Settle: poll until every requested symbol is numeric-or-terminally-errored,
    // or the bounded window elapses. Track the latest reading per symbol.
    let mut latest: PricePass = BTreeMap::new();
    let mut polls = 0u32;
    loop {
        let pass = client.read_price_pass()?;
        for s in symbols {
            if let Some(r) = pass.get(s) {
                latest.insert(s.clone(), r.clone());
            }
        }
        polls += 1;
        // Settled when no requested symbol is still transient/absent.
        let settled = symbols.iter().all(|s| {
            !matches!(latest.get(s), Some(PriceReading::Transient) | None)
        });
        if settled {
            break;
        }
        if polls >= cfg.max_polls {
            break; // bounded window elapsed; still-transient symbols degrade below
        }
    }

    let mut out = MarksProduced::default();
    for s in symbols {
        match latest.get(s) {
            Some(PriceReading::Numeric { price_usd, quote_date }) => {
                match price_to_cents(*price_usd) {
                    Ok(price_cents) => {
                        out.marks.insert(
                            s.clone(),
                            Mark { price_cents, quote_date: *quote_date },
                        );
                    }
                    // A positive price → 0¢ (or any non-positive) is degraded.
                    Err(reason) => {
                        out.degraded.insert(s.clone(), reason);
                    }
                }
            }
            // Stayed transient past the bounded window → degraded (TimedOut), but
            // keep the prior good cached mark forward so it is not nulled by a
            // transient. (SHEET-MARK-003)
            Some(PriceReading::Transient) | None => {
                if let Some(prev) = prior.get(s) {
                    out.marks.insert(s.clone(), *prev);
                } else {
                    out.degraded.insert(s.clone(), DegradeReason::TimedOut);
                }
            }
            // A terminal non-numeric error → degraded (Permanent), no prior carry.
            Some(PriceReading::Permanent) => {
                out.degraded.insert(s.clone(), DegradeReason::Permanent);
            }
        }
    }
    Ok(out)
}

// ===========================================================================
// Republish (sheets-view-design.md → "Republish"; SHEET-PUB-001/002/003). The
// atomic publish + the serialized republish/mark-refresh loop + the loud-stale
// handling. `Publisher` wraps a client and owns the per-tab stale flags.
// ===========================================================================

/// The result of one republish-then-settle cycle. (SHEET-PUB-001/002/003)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RepublishOutcome {
    /// `true` when every view tab published atomically. (SHEET-PUB-002)
    pub published: bool,
    /// Tabs left stale because their `batchUpdate` failed (retried next sync).
    /// (SHEET-PUB-003)
    pub stale_tabs: Vec<String>,
}

/// The serialized republish + mark-refresh driver. Owns the client and the
/// per-tab stale flags so republish and the periodic mark-refresh run in one
/// serialized loop, never concurrently. (SHEET-PUB-001)
pub struct Publisher<C: SheetsViewClient> {
    client: C,
    /// Tabs currently flagged stale (a prior publish failed). Retried on the next
    /// republish. (SHEET-PUB-003)
    stale: std::collections::BTreeSet<String>,
}

impl<C: SheetsViewClient> Publisher<C> {
    /// Wrap a client.
    pub fn new(client: C) -> Self {
        Publisher { client, stale: std::collections::BTreeSet::new() }
    }

    /// Borrow the client (tests inspect the published tabs).
    pub fn client(&self) -> &C {
        &self.client
    }

    /// Mutably borrow the client.
    pub fn client_mut(&mut self) -> &mut C {
        &mut self.client
    }

    /// The set of tabs currently flagged stale.
    pub fn stale_tabs(&self) -> Vec<String> {
        self.stale.iter().cloned().collect()
    }

    /// Republish all four view tabs as atomic per-tab `batchUpdate`s and reassert
    /// the workbook tab order. Each tab is rewritten as a single batchUpdate that
    /// writes the new range and truncates the residual tail (SHEET-PUB-002); the
    /// frozen header is never rewritten (SHEET-FORMULA-002). On a per-tab failure
    /// the canonical log is unaffected, the tab is flagged stale, a best-effort
    /// in-tab banner is written, and the tab is retried on the next sync
    /// (SHEET-PUB-003). A tab that republishes successfully clears its stale flag.
    /// (SHEET-PUB-001/002/003, SHEET-TAB-002)
    ///
    /// Acquires the runtime-owned advisory write-lock BEFORE mutating the workbook
    /// and releases it after, so republish never writes concurrently with another
    /// Sheets-mutating primitive (SHEET-PUB-004). When the lock is held by another
    /// holder (or an acquisition I/O error), republish writes NOTHING this cycle and
    /// returns `published: false` with the current stale set — a non-destructive
    /// refusal that returns control to the owner (the lock holder finishes, the next
    /// sync retries), never a partial/concurrent write.
    ///
    // @spec SHEET-PUB-004
    pub fn republish<L: Lock>(
        &mut self,
        tabs: &[ViewTab],
        lock: &L,
        stale_banner_quote_ts: &str,
    ) -> RepublishOutcome {
        // Acquire BEFORE any workbook mutation; the guard releases on scope exit
        // (after the last batchUpdate / tab-order reassert). (SHEET-PUB-004) A held
        // lock means another Sheets-mutating primitive is writing — do not write
        // concurrently; report not-published and leave the stale set as-is.
        let _guard = match lock.acquire() {
            Ok(guard) => guard,
            Err(_) => {
                return RepublishOutcome {
                    published: false,
                    stale_tabs: self.stale_tabs(),
                }
            }
        };
        let mut all_ok = true;
        for tab in tabs {
            match self.client.batch_update_view(tab) {
                Ok(()) => {
                    self.stale.remove(&tab.name);
                }
                Err(_) => {
                    all_ok = false;
                    self.stale.insert(tab.name.clone());
                    // Best-effort in-tab banner; if even this fails the TUI
                    // remains the staleness surface. (SHEET-PUB-003)
                    let banner = format!("STALE — last sync {stale_banner_quote_ts}");
                    let _ = self.client.write_stale_banner(&tab.name, &banner);
                }
            }
        }
        // Idempotently reassert the tab ordering on republish. (SHEET-TAB-002) A
        // failure here does not corrupt truth; it is retried next sync.
        let order = workbook_tab_order();
        if self.client.set_tab_order(&order).is_err() {
            all_ok = false;
        }
        RepublishOutcome {
            published: all_ok,
            stale_tabs: self.stale_tabs(),
        }
    }

    /// One serialized cycle: republish the view tabs, THEN (on the same single
    /// thread of control, never concurrently) read the marks back on the settle
    /// pass. Returns the republish outcome and the produced marks. The settle pass
    /// runs after the write so it never reads a mid-rewrite/recalculating `Price`
    /// column. (SHEET-PUB-001, SHEET-MARK-001)
    ///
    /// Threads the advisory write-lock through `republish`, which acquires it before
    /// mutating and releases it after (SHEET-PUB-004). The settle read is a read-only
    /// pass after the write returns, so the lock guards only the mutating half.
    ///
    // @spec SHEET-PUB-004
    pub fn republish_then_settle<L: Lock>(
        &mut self,
        tabs: &[ViewTab],
        symbols: &[Symbol],
        prior: &BTreeMap<Symbol, Mark>,
        cfg: SettleConfig,
        lock: &L,
        stale_banner_quote_ts: &str,
    ) -> Result<(RepublishOutcome, MarksProduced), ViewError> {
        let outcome = self.republish(tabs, lock, stale_banner_quote_ts);
        // Serialized: the settle read happens only after the write returns.
        let marks = read_marks(&self.client, symbols, prior, cfg)?;
        Ok((outcome, marks))
    }
}
