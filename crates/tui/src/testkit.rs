//! In-memory FAKES for ALL `tui` tests (no real terminal, no Sheets). The
//! [`FakeRuntime`] implements [`crate::port::RuntimePort`] over a held [`ViewState`]
//! and a programmable write path, so tests drive the Model-Update-View logic +
//! rendering and assert on rendered buffers / state transitions, never a live TTY.
//! (tui-design.md → "Tests: drive ... against a FAKE runtime")

use std::collections::BTreeMap;

use ledger_core::{LedgerError, LedgerEvent, Snapshot, Symbol};
use pt_core::{Cents, Date};
use reports::{PricedMark, PricedMarks, SeriesPoint, TradingDayKey};
use runtime::SymbolFreshness;
use store::AppendOutcome;
use tax::{Accrual, TaxContext, TaxError, TaxEvent};

use crate::port::{Connection, RuntimePort, SubmitOutcome, SubmitRejection, ViewState, WriteFailure};

/// How the fake's authoritative submit behaves — so a test exercises the
/// confirmed / lock-held / write-failed write-loop branches deterministically.
/// (TUI-ENTRY-FLOW-003/004/005)
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum SubmitBehavior {
    /// The submit confirms durable (the happy path). (TUI-ENTRY-FLOW-003)
    #[default]
    Confirm,
    /// The advisory write-lock is held (a cron `summary`): fail non-destructively.
    /// (TUI-ENTRY-FLOW-005)
    LockHeld { holder: String },
    /// The append / read-back-verify found a mismatch (a human edit). (TUI-ENTRY-FLOW-004)
    VerifyMismatch,
    /// The workbook was unreachable. (TUI-ENTRY-FLOW-004)
    Unreachable,
    /// The submit re-validates against the live state and rejects with a kernel
    /// error (inline re-rendered) — inline never overrides submit. (TUI-ENTRY-FLOW-002)
    SubmitRejectLedger(LedgerError),
    /// As above, a tax-event submit-time rejection.
    SubmitRejectTax(TaxError),
}

/// A FAKE `runtime` for tests: holds the [`ViewState`] the screens render, the
/// per-year `TaxContext` the flows validate against, a programmable
/// [`SubmitBehavior`], and counters so a test confirms a submit went through the
/// write path. Inline validation runs the REAL kernel `validate` against the held
/// snapshot (the advisory check). (tui-design.md → FAKE runtime tests)
#[derive(Clone, Debug)]
pub struct FakeRuntime {
    view: ViewState,
    ctx: TaxContext,
    today: Date,
    /// The accepted ledger log (for inline `validate` against the real kernel).
    pub ledger_log: Vec<LedgerEvent>,
    /// The accepted `TaxEvent` log — threaded into `tax::validate_event` as the
    /// `accepted` slice so a Move-on-Allocated / Pay-on-Moved lifecycle can really
    /// validate (an empty slice would replay every accrual as Accrued, making Move
    /// and Pay always reject). (TUI-ENTRY-TAX-001/002/003)
    pub tax_log: Vec<TaxEvent>,
    pub behavior: SubmitBehavior,
    /// How many times [`RuntimePort::refresh`] has been called (so a test can
    /// confirm a refresh ran). Plain counter — no consume-once submit-reject logic;
    /// the inline-vs-submit + idempotent-retry tests flip `behavior` directly.
    pub refresh_count: u32,
    pub submit_count: u32,
    /// The last appended event id, so a test confirms confirmed-durability.
    pub last_event_id: Option<String>,
}

impl FakeRuntime {
    /// A fake over a held view + tax context (the happy submit path by default).
    pub fn new(view: ViewState, ctx: TaxContext) -> Self {
        let today = view.trading_day_key.map(|k| k.0).unwrap_or(Date(20_000));
        FakeRuntime {
            view,
            ctx,
            today,
            ledger_log: Vec::new(),
            tax_log: Vec::new(),
            behavior: SubmitBehavior::Confirm,
            refresh_count: 0,
            submit_count: 0,
            last_event_id: None,
        }
    }

    /// Set the submit behavior (for the write-loop branch tests).
    pub fn with_behavior(mut self, behavior: SubmitBehavior) -> Self {
        self.behavior = behavior;
        self
    }

    /// Seed the accepted ledger log (so inline `validate` has a real prefix).
    pub fn with_ledger_log(mut self, log: Vec<LedgerEvent>) -> Self {
        self.ledger_log = log;
        self
    }

    /// Seed the accepted `TaxEvent` log (so `tax::validate_event` replays the real
    /// accrual lifecycle states an inline / submit check sees). (TUI-ENTRY-TAX-*)
    pub fn with_tax_log(mut self, log: Vec<TaxEvent>) -> Self {
        self.tax_log = log;
        self
    }

    /// Replace the held view (so a test models a concurrent append / a refresh
    /// changing the snapshot). (TUI-VIEW-NAV-005)
    pub fn set_view(&mut self, view: ViewState) {
        self.view = view;
    }

    /// Mutable access to the held view (test setup).
    pub fn view_mut(&mut self) -> &mut ViewState {
        &mut self.view
    }
}

impl RuntimePort for FakeRuntime {
    fn view(&self) -> &ViewState {
        &self.view
    }

    fn refresh(&mut self) {
        self.refresh_count += 1;
        // A refresh re-reads marks; the fake leaves the held view as set by the
        // test (set_view models the new replay). (views-design.md → Refresh)
    }

    fn tax_context(&self) -> &TaxContext {
        &self.ctx
    }

    fn validate_ledger(&self, candidate: &LedgerEvent) -> Result<(), LedgerError> {
        // Advisory inline validation: the REAL kernel against the accepted prefix.
        // (TUI-ENTRY-FLOW-001)
        ledger_core::validate(&self.ledger_log, candidate)
    }

    fn validate_tax(&self, candidate: &TaxEvent) -> Result<(), TaxError> {
        // Validate against the accepted TaxEvent log so the accrual lifecycle states
        // (Allocate → Move → Pay) are in scope — an empty `accepted` would replay
        // every accrual as Accrued, making Move/Pay always reject. (TUI-ENTRY-TAX-*)
        tax::validate_event(&self.view.snapshot.realized_gains, &self.tax_log, candidate, &self.ctx)
    }

    fn submit_ledger(&mut self, candidate: &LedgerEvent) -> SubmitOutcome {
        self.submit_count += 1;
        // Submit re-validates against the LIVE state (authoritative) before the
        // append; a submit-time disagreement re-renders inline. (TUI-ENTRY-FLOW-002)
        match &self.behavior {
            SubmitBehavior::SubmitRejectLedger(e) => {
                // A live kernel rejection: carry the specific LedgerError back to be
                // re-rendered in the inline slot beside the field — distinct from a
                // write-verify mismatch. Nothing written. (TUI-ENTRY-FLOW-002)
                SubmitOutcome::Rejected(SubmitRejection::Ledger(e.clone()))
            }
            SubmitBehavior::LockHeld { holder } => SubmitOutcome::LockHeld { holder: holder.clone() },
            SubmitBehavior::VerifyMismatch => SubmitOutcome::WriteFailed(WriteFailure::VerifyMismatch),
            SubmitBehavior::Unreachable => SubmitOutcome::WriteFailed(WriteFailure::Unreachable),
            SubmitBehavior::Confirm | SubmitBehavior::SubmitRejectTax(_) => {
                if let Err(_e) = ledger_core::validate(&self.ledger_log, candidate) {
                    return SubmitOutcome::WriteFailed(WriteFailure::VerifyMismatch);
                }
                // A stable, content-derived id models `store`'s read-back-verify
                // confirming durability + the idempotent-retry id.
                let id = format!("evt-{}", self.ledger_log.len() + 1);
                self.ledger_log.push(LedgerEvent {
                    id: id.clone(),
                    seq: pt_core::Seq(self.ledger_log.len() as u64 + 1),
                    date: candidate.date,
                    kind: candidate.kind.clone(),
                });
                self.last_event_id = Some(id.clone());
                SubmitOutcome::Confirmed(AppendOutcome {
                    event_id: id,
                    seq: pt_core::Seq(self.ledger_log.len() as u64),
                    idempotent_skip: false,
                })
            }
        }
    }

    fn submit_tax(&mut self, candidate: &TaxEvent) -> SubmitOutcome {
        self.submit_count += 1;
        match &self.behavior {
            SubmitBehavior::LockHeld { holder } => SubmitOutcome::LockHeld { holder: holder.clone() },
            SubmitBehavior::VerifyMismatch => SubmitOutcome::WriteFailed(WriteFailure::VerifyMismatch),
            SubmitBehavior::Unreachable => SubmitOutcome::WriteFailed(WriteFailure::Unreachable),
            SubmitBehavior::SubmitRejectTax(e) => {
                // A live tax-kernel rejection re-renders inline beside the field —
                // distinct from a write-verify mismatch. (TUI-ENTRY-FLOW-002)
                SubmitOutcome::Rejected(SubmitRejection::Tax(e.clone()))
            }
            _ => {
                // Submit re-validates live against the accepted TaxEvent log (the
                // real lifecycle states), then appends durably. (TUI-ENTRY-TAX-*,
                // TUI-ENTRY-FLOW-002/003)
                if let Err(e) = tax::validate_event(
                    &self.view.snapshot.realized_gains,
                    &self.tax_log,
                    candidate,
                    &self.ctx,
                ) {
                    return SubmitOutcome::Rejected(SubmitRejection::Tax(e));
                }
                let id = format!("tax-{}", self.submit_count);
                self.tax_log.push(TaxEvent {
                    seq: pt_core::Seq(self.tax_log.len() as u64 + 1),
                    kind: candidate.kind.clone(),
                });
                self.last_event_id = Some(id.clone());
                SubmitOutcome::Confirmed(AppendOutcome {
                    event_id: id,
                    seq: pt_core::Seq(self.submit_count as u64),
                    idempotent_skip: false,
                })
            }
        }
    }

    fn today(&self) -> Date {
        self.today
    }
}

// ===========================================================================
// Builders — terse construction of a `ViewState` + a `TaxContext` for tests.
// ===========================================================================

/// A minimal flat-rate federal-only `TaxContext` for the given year, with a given
/// top ordinary marginal rate (ppm) so the lot-picker / unrealized estimate are
/// non-degraded. (test convenience)
pub fn flat_federal_ctx(tax_year: i32, top_ppm: i64) -> TaxContext {
    use config::{BracketRow, BracketSet, Jurisdiction, Niit, Ppm, TaxYear};
    let set = BracketSet {
        rows: vec![BracketRow { lower_threshold_cents: Cents(0), rate_ppm: Ppm(top_ppm) }],
        last_verified: Date(19_000),
        source_note: "test".to_string(),
    };
    TaxContext {
        tax_year: TaxYear(tax_year),
        federal: tax::ResolvedJurisdiction {
            jurisdiction: Jurisdiction::Federal,
            ordinary: Some(set.clone()),
            federal_long_term: Some(set),
            niit: Some(Niit::default()),
            ordinary_income_cents: Cents(0),
            state: config::BracketState::Verified,
        },
        states: BTreeMap::new(),
        de_minimis_cents: Cents(100),
        residency_default: None,
    }
}

/// A cold-start (`NoBracketsAvailable`) federal `TaxContext` — for the
/// `n/a (no brackets)` convention. (test convenience)
pub fn cold_start_ctx(tax_year: i32) -> TaxContext {
    use config::{Jurisdiction, TaxYear};
    TaxContext {
        tax_year: TaxYear(tax_year),
        federal: tax::ResolvedJurisdiction {
            jurisdiction: Jurisdiction::Federal,
            ordinary: None,
            federal_long_term: None,
            niit: None,
            ordinary_income_cents: Cents(0),
            state: config::BracketState::NoBracketsAvailable,
        },
        states: BTreeMap::new(),
        de_minimis_cents: Cents(100),
        residency_default: None,
    }
}

/// A builder for a `ViewState` — start from a snapshot and tack on marks, accruals,
/// history, freshness, and the connection/integrity state.
pub struct ViewBuilder {
    view: ViewState,
}

impl ViewBuilder {
    /// Start from a snapshot, live + unblocked, with empty marks/accruals/history.
    pub fn new(snapshot: Snapshot) -> Self {
        ViewBuilder {
            view: ViewState {
                snapshot,
                accruals: Vec::new(),
                annual_rows: Vec::new(),
                estimates: BTreeMap::new(),
                marks: PricedMarks::new(),
                freshness: BTreeMap::new(),
                trading_day_key: Some(TradingDayKey(Date(20_000))),
                // The formatted calendar date the binary threads for Date(20_000).
                as_of_calendar: Some("2024-10-04".to_string()),
                updated_hhmm: None,
                display_names: config::DisplayNameMap::default(),
                trading_day_calendar: Vec::new(),
                history_points: BTreeMap::new(),
                orphan_warnings: Vec::new(),
                connection: Connection::Live,
                integrity: None,
                tax_year: 2026,
                bracket_state: config::BracketState::Verified,
                staleness: Vec::new(),
                next_estimated_payment_period: None,
            },
        }
    }

    /// Set a symbol's mark (price + quote-epoch) and its priced freshness.
    pub fn mark(mut self, symbol: &str, price_cents: i64, quote_epoch: Date) -> Self {
        self.view.marks.insert(
            symbol.to_string(),
            PricedMark { price_cents: Cents(price_cents), quote_epoch },
        );
        self.view.freshness.insert(
            symbol.to_string(),
            SymbolFreshness::Priced { quote_epoch },
        );
        self
    }

    /// Record a symbol degraded (no mark) with a reason.
    pub fn degraded(mut self, symbol: &str, reason: sheets_view::DegradeReason) -> Self {
        self.view
            .freshness
            .insert(symbol.to_string(), SymbolFreshness::Degraded { reason });
        self
    }

    /// Set a per-symbol unrealized estimate (so composition is non-degraded post-tax).
    pub fn estimate(mut self, symbol: &str, est: tax::UnrealizedEstimate) -> Self {
        self.view.estimates.insert(symbol.to_string(), est);
        self
    }

    /// Set the accruals.
    pub fn accruals(mut self, accruals: Vec<Accrual>) -> Self {
        self.view.accruals = accruals;
        self
    }

    /// Set the annual reserve rows.
    pub fn annual_rows(mut self, rows: Vec<tax::AnnualRow>) -> Self {
        self.view.annual_rows = rows;
        self
    }

    /// Set the orphan warnings.
    pub fn orphans(mut self, warnings: Vec<tax::OrphanWarning>) -> Self {
        self.view.orphan_warnings = warnings;
        self
    }

    /// Add a captured History point on a trading day (extends the calendar).
    pub fn history_point(mut self, point: SeriesPoint) -> Self {
        let key = point.key;
        if !self.view.trading_day_calendar.contains(&key) {
            self.view.trading_day_calendar.push(key);
            self.view.trading_day_calendar.sort();
        }
        self.view.history_points.insert(key, point);
        self
    }

    /// Add a trading day to the calendar WITHOUT a captured point (an in-band gap).
    pub fn calendar_day(mut self, key: TradingDayKey) -> Self {
        if !self.view.trading_day_calendar.contains(&key) {
            self.view.trading_day_calendar.push(key);
            self.view.trading_day_calendar.sort();
        }
        self
    }

    /// Mark the view offline (stale) — figures served from cache.
    pub fn offline(mut self) -> Self {
        self.view.connection = Connection::Offline;
        self
    }

    /// A book with NO priced trading day (no key — the masthead must read
    /// `no priced day yet`, never a raw key int). (TUI-VIEW-NAV-012)
    pub fn no_priced_day(mut self) -> Self {
        self.view.trading_day_key = None;
        self.view.as_of_calendar = None;
        self
    }

    /// Set the formatted calendar as-of the binary threads. (TUI-VIEW-NAV-012)
    pub fn as_of_calendar(mut self, s: &str) -> Self {
        self.view.as_of_calendar = Some(s.to_string());
        self
    }

    /// Set the wall-clock `HH:MM` of the run/refresh. (TUI-VIEW-NAV-013)
    pub fn updated(mut self, hhmm: &str) -> Self {
        self.view.updated_hhmm = Some(hhmm.to_string());
        self
    }

    /// Map a symbol to its company display name. (TUI-VIEW-POS-009)
    pub fn display_name(mut self, symbol: &str, name: &str) -> Self {
        let mut entries = self.view.display_names.entries().clone();
        entries.insert(symbol.to_string(), name.to_string());
        self.view.display_names = config::DisplayNameMap::new(entries);
        self
    }

    /// Block the view with an integrity error (the loud `✗` treatment).
    pub fn integrity(mut self, err: crate::port::IntegrityError) -> Self {
        self.view.integrity = Some(err);
        self
    }

    /// Set the effective bracket state (drives `[est]` / `[est, brackets stale]` /
    /// `n/a (no brackets)`).
    pub fn bracket_state(mut self, state: config::BracketState) -> Self {
        self.view.bracket_state = state;
        self
    }

    /// Set the bracket-staleness signals (the status-line `⚠` nudge).
    pub fn staleness(mut self, signals: Vec<config::StalenessSignal>) -> Self {
        self.view.staleness = signals;
        self
    }

    /// Set the next IRS estimated-tax payment period (the Tax & Reserves screen).
    /// (TUI-VIEW-TAX-001)
    pub fn next_payment_period(mut self, period: tax::Quarter) -> Self {
        self.view.next_estimated_payment_period = Some(period);
        self
    }

    /// The built view.
    pub fn build(self) -> ViewState {
        self.view
    }
}

/// Build a `SeriesPoint` quickly for a History test: a trading-day key, a total
/// market value, per-symbol values, and the incomplete flag.
pub fn series_point(
    key: TradingDayKey,
    total_value: i64,
    per_symbol: &[(&str, i64)],
    incomplete: bool,
) -> SeriesPoint {
    let mut per_symbol_value_cents = BTreeMap::new();
    for (s, v) in per_symbol {
        per_symbol_value_cents.insert(s.to_string(), Cents(*v));
    }
    SeriesPoint {
        key,
        total_market_value_cents: Cents(total_value),
        total_unrealized_pretax_cents: Cents(0),
        total_unrealized_net_of_tax_cents: Cents(0),
        total_basis_cents: Cents(0),
        per_symbol_value_cents,
        per_symbol_shares: BTreeMap::new(),
        marks: PricedMarks::new(),
        captured_at_epoch_secs: 0,
        reporting_tz_date: key.0,
        incomplete,
    }
}

/// Render the model + a fake runtime to a `String` of the rendered buffer's cell
/// contents (rows joined by `\n`), so a test asserts on rendered text. Uses
/// ratatui's `TestBackend` semantics via the in-memory Buffer. (FAKE runtime
/// rendering tests)
pub fn render_string(model: &crate::Model, port: &impl RuntimePort, width: u16, height: u16) -> String {
    let buf = crate::render_to_buffer(model, port, width, height);
    buffer_to_string(&buf)
}

/// Flatten a `ratatui` Buffer to a row-joined `String` (symbol cells), trimming
/// trailing spaces per row — the testable rendered text. Multi-byte glyphs that
/// occupy one cell render in that cell's symbol; trailing empty cells are trimmed.
pub fn buffer_to_string(buf: &ratatui::buffer::Buffer) -> String {
    let area = buf.area;
    let mut lines = Vec::new();
    for y in 0..area.height {
        let mut line = String::new();
        for x in 0..area.width {
            line.push_str(buf[(x, y)].symbol());
        }
        lines.push(line.trim_end().to_string());
    }
    lines.join("\n")
}

// ===========================================================================
// Snapshot builders (a thin layer over ledger_core::replay) so view tests have
// realistic positions / open lots / realized gains without hand-rolling Snapshots.
// ===========================================================================

/// Replay a ledger log into a `Snapshot` with the given marks (symbol →
/// price_cents). A thin convenience over `ledger_core::replay`.
pub fn replay(log: &[LedgerEvent], marks: &[(&str, i64)]) -> Snapshot {
    let m: ledger_core::Marks = marks
        .iter()
        .map(|(s, c)| (s.to_string(), Cents(*c)))
        .collect();
    ledger_core::replay(log, &m)
}

/// A Buy `LedgerEvent` (terse builder).
pub fn buy(seq: u64, date: i32, lot_id: &str, symbol: &str, qty_whole: i64, price_cents: i64, platform: &str) -> LedgerEvent {
    LedgerEvent {
        id: format!("e{seq}"),
        seq: pt_core::Seq(seq),
        date: Date(date),
        kind: ledger_core::LedgerEventKind::Buy {
            lot_id: lot_id.to_string(),
            symbol: symbol.to_string(),
            qty: pt_core::MicroShares(qty_whole * pt_core::SHARE_SCALE),
            unit_price_cents: Cents(price_cents),
            fees_cents: Cents(0),
            platform: platform.to_string(),
            tracking_code: None,
        },
    }
}

/// A symbol type alias re-export for tests.
pub type Sym = Symbol;

/// An accrual in the **Moved** state for a `(sale, lot)` key, Federal, year
/// [`crate::testkit`]-default — a terse builder for the Tax view tests.
pub fn accrual_moved(sale: &str, lot: &str) -> Accrual {
    use config::{Jurisdiction, TaxYear};
    Accrual {
        key: tax::AccrualKey {
            sale_id: sale.to_string(),
            lot_id: lot.to_string(),
            jurisdiction: Jurisdiction::Federal,
            tax_year: TaxYear(2026),
        },
        term: tax::Term::LongTerm,
        gain_cents: Cents(10_000_00),
        derived_cents: Some(Cents(240_000)),
        applied_cents: Some(Cents(240_000)),
        override_cents: None,
        state: tax::AccrualState::Moved {
            account_label: "Reserve-Fed".to_string(),
            amount_cents: Cents(240_000),
            date: Date(20_100),
        },
        de_minimis: false,
        superseded: false,
        bracket_state: config::BracketState::Verified,
    }
}
