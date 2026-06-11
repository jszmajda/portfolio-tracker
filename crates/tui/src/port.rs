//! The `runtime` **port** — the seam the TUI drives, with an in-memory FAKE for
//! ALL tests (no real terminal, no Sheets). The shell runs **through `runtime`**:
//! `runtime` drives the replay cycle, holds the `Snapshot` + cached marks the
//! children render, owns the Sheets-access layer + the advisory write-lock, and is
//! where interactive submits acquire the lock. (tui-design.md → "App Shell &
//! Navigation" / "The Write-Path UX Loop")
//!
//! Tests drive the Model-Update-View logic + rendering against a [`FakeRuntime`]:
//! they assert on rendered buffers / state transitions, never a live TTY or Sheets.

use std::collections::BTreeMap;

use ledger_core::{LedgerEvent, Snapshot, Symbol};
use pt_core::Date;
use reports::{SeriesPoint, TradingDayKey};
use runtime::SymbolFreshness;
use store::AppendOutcome;
use tax::{Accrual, AnnualRow, OrphanWarning, TaxContext, TaxEvent, UnrealizedEstimate};

// ===========================================================================
// The read snapshot the TUI renders (what `runtime`'s cycle holds). Everything
// the views show comes from here; the TUI computes nothing. (tui-design.md →
// "App Shell & Navigation": runtime holds the Snapshot + cached marks the
// children render.)
// ===========================================================================

/// The connection / freshness state of the workbook the status line shows: live
/// (connected), offline (rendering stale from cache), or an integrity error. A
/// figure derived from marks while `Offline` is **stale**-marked, never live.
/// (tui-design.md → "Freshness" / "Status line")
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Connection {
    /// Connected: live figures carry their freshness context.
    Live,
    /// Offline: figures are served from the cache, **stale**-marked with the last
    /// quote-epoch (never presented as live). (tui-design.md → "Freshness")
    Offline,
}

/// Why the whole screen is blocked with the loud `✗` integrity treatment: a
/// `store`/`reports` integrity error (corrupt/edited event log or History) **or** a
/// `config` creds/unreadable-cache failure — the full no-trustworthy-data set
/// matching `summary`'s exit-2. The TUI surfaces it loudly and refuses to render
/// derived numbers. (tui-design.md → "Integrity errors")
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum IntegrityError {
    /// Corrupt / edited event log or History (`store`/`reports`).
    CorruptLog(String),
    /// `config` reports missing/invalid credentials.
    BadCredentials,
    /// The local cache was unreadable.
    UnreadableCache,
}

impl IntegrityError {
    /// The blocking message shown beside the `✗` glyph.
    pub fn message(&self) -> String {
        match self {
            IntegrityError::CorruptLog(d) => {
                format!("event log / history integrity error: {d}")
            }
            IntegrityError::BadCredentials => "credentials unavailable".to_string(),
            IntegrityError::UnreadableCache => "local cache unreadable".to_string(),
        }
    }
}

/// The replayed view state `runtime` holds, plus its freshness context — the
/// single bundle the screens render. When `integrity` is `Some`, derived numbers
/// must NOT be rendered (the screen blocks). (tui-design.md → "Cross-Screen
/// Display Conventions")
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ViewState {
    /// The replayed `Snapshot` (positions, open lots, realized gains), valued with
    /// the cycle's injected marks.
    pub snapshot: Snapshot,
    /// The tax accruals over the realized gains (lifecycle-folded).
    pub accruals: Vec<Accrual>,
    /// The per-`(jurisdiction, tax_year)` annual reserve rows.
    pub annual_rows: Vec<AnnualRow>,
    /// The per-symbol unrealized tax estimate.
    pub estimates: BTreeMap<Symbol, UnrealizedEstimate>,
    /// The marks for composition / valuation (symbol → price + quote-epoch). A
    /// symbol absent here is degraded (unpriced) — never valued at zero.
    pub marks: reports::PricedMarks,
    /// Per-symbol freshness: each priced symbol's own quote-epoch stamp, each
    /// degraded symbol's reason — preserved alongside the single reduced key.
    pub freshness: BTreeMap<Symbol, SymbolFreshness>,
    /// The single trading-day key reduced from the per-symbol quote-epochs — the
    /// key `reports`/`summary`/`tui` use; `None` when no symbol is priced.
    pub trading_day_key: Option<TradingDayKey>,
    /// The priced trading day as a **formatted calendar date** (`2026-06-09`),
    /// threaded by the binary (the key→calendar conversion is the binary's — the
    /// TUI renders, it does not derive). `None` when no priced trading day exists
    /// (no key, or a zero key): the masthead then reads `no priced day yet`, never
    /// a raw key integer. (TUI-VIEW-NAV-012)
    pub as_of_calendar: Option<String>,
    /// The wall-clock `HH:MM` of the run/refresh that produced this view, threaded
    /// by the binary; the status line renders `updated HH:MM`, omitted (never
    /// fabricated) when `None`. (TUI-VIEW-NAV-013)
    pub updated_hhmm: Option<String>,
    /// `config`'s symbol→company-display-name map for the Positions / Open Lots
    /// name columns (an unmapped symbol renders its ticker).
    /// (TUI-VIEW-POS-009, TUI-VIEW-LOT-003, CONFIG-PLATFORM-003)
    pub display_names: config::DisplayNameMap,
    /// The ordered real trading days (the History axis). (TUI-VIEW-HIST-001)
    pub trading_day_calendar: Vec<TradingDayKey>,
    /// The captured value-over-time History points, keyed by trading day — the
    /// durable series `reports` persists (not recomputed). The TUI renders the
    /// History chart + the per-symbol sparklines / day-deltas from these.
    /// (TUI-VIEW-HIST-001, TUI-VIEW-POS-001)
    pub history_points: BTreeMap<TradingDayKey, SeriesPoint>,
    /// `tax`'s orphan warnings (an accrual whose sale was reversed after Move/Pay).
    /// The Tax view renders these as the `⚠ undone` badge. (TUI-VIEW-TAX-001,
    /// TAX-VERIF-007)
    pub orphan_warnings: Vec<OrphanWarning>,
    /// The connection / staleness state for the status line + the stale marking.
    pub connection: Connection,
    /// `Some` when a `store`/`reports`/`config` integrity failure blocks all derived
    /// rendering (the loud `✗` treatment). (tui-design.md → "Integrity errors")
    pub integrity: Option<IntegrityError>,
    /// The current tax year (the tax-reserve view's year).
    pub tax_year: i32,
    /// `config`'s effective bracket state for the current year (drives `[est]` /
    /// `[est, brackets stale]` / `n/a (no brackets)`). (tui-design.md → "Estimates")
    pub bracket_state: config::BracketState,
    /// The per-`(jurisdiction, tax_year)` bracket-staleness reminders (surfaced in
    /// the status line + the config bracket form). (tui-design.md / entry-design.md)
    pub staleness: Vec<config::StalenessSignal>,
    /// The next IRS estimated-tax payment period (from `tax`'s quarterly report /
    /// `quarter_of`), rendered on the Tax & Reserves screen so the owner sees when
    /// the next remittance is due. `None` when there are no accruals / no quote.
    /// (TUI-VIEW-TAX-001)
    pub next_estimated_payment_period: Option<tax::Quarter>,
}

impl ViewState {
    /// Whether all derived rendering is blocked by an integrity failure.
    pub fn blocked(&self) -> bool {
        self.integrity.is_some()
    }
}

// ===========================================================================
// The write seam (tui-design.md → "The Write-Path UX Loop"; entry-design.md →
// "The Composer & Write Loop"). The submit order is confirm → acquire-lock →
// append + read-back-verify → confirmed. The lock try-acquire fails
// NON-destructively on held; an append/verify failure or unreachable workbook
// returns control with the entry intact and `[r]etry`.
// ===========================================================================

/// What a submit can yield, end to end through `runtime`'s write path. (tui-design.md
/// → "The Write-Path UX Loop"; entry-design.md → write-loop)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SubmitOutcome {
    /// Confirmed durable — the append landed and read-back-verified. The composer
    /// clears. (TUI-ENTRY-FLOW-003)
    Confirmed(AppendOutcome),
    /// The submit **re-validated against the live state** and the kernel rejected
    /// the candidate (the cache had lagged, so inline passed but live disagrees).
    /// The specific `LedgerError`/`TaxError` is carried back to re-render in the
    /// inline error slot beside the offending field — inline never overrides
    /// submit, and this is distinct from a write-verify failure. Nothing is
    /// written. (TUI-ENTRY-FLOW-002)
    Rejected(SubmitRejection),
    /// The advisory write-lock is **held** (a cron `summary`): the submit failed
    /// non-destructively, the entry is preserved, retry available — no queue.
    /// (TUI-ENTRY-FLOW-005)
    LockHeld { holder: String },
    /// Append / read-back-verify failed or the workbook is unreachable: control
    /// returns with the entry intact and `[r]etry`. A retry reuses the prior
    /// confirmation and is idempotent (stable `EventId`). (TUI-ENTRY-FLOW-004)
    WriteFailed(WriteFailure),
}

/// A submit-time kernel disagreement carried back for inline re-rendering — the
/// verified `LedgerError`/`TaxError` the live re-validation produced, distinct from
/// a write-verify mismatch. (TUI-ENTRY-FLOW-002)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SubmitRejection {
    Ledger(ledger_core::LedgerError),
    Tax(tax::TaxError),
}

/// Why a write failed (a `WriteVerifyMismatch` or an unreachable workbook). The
/// entry is preserved either way. (TUI-ENTRY-FLOW-004)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum WriteFailure {
    /// A read-back found a row that does not deserialize equal (a human edit).
    VerifyMismatch,
    /// The workbook was unreachable (offline) or the read-back found no row.
    Unreachable,
}

/// The `runtime` port the TUI drives. Tests inject [`FakeRuntime`]; production
/// wires the real `runtime` cycle + `store` write path.
///
/// - [`RuntimePort::view`] is the current replayed read state the screens render.
/// - [`RuntimePort::refresh`] re-reads marks + re-replays (the `views` refresh).
/// - [`RuntimePort::tax_context`] is the per-year `TaxContext` `entry` validates against.
/// - [`RuntimePort::validate_ledger`] / [`RuntimePort::validate_tax`] are the
///   advisory inline checks (kernel validation against the current snapshot).
/// - [`RuntimePort::submit_ledger`] / [`RuntimePort::submit_tax`] run the
///   authoritative submit: re-validate live → acquire-lock → append + read-back-verify.
pub trait RuntimePort {
    /// The current replayed read state (what the views render).
    fn view(&self) -> &ViewState;

    /// Re-read marks and re-replay (the `views` refresh). Updates the held view.
    /// (views-design.md → "Refresh & Staleness")
    fn refresh(&mut self);

    /// The per-year `TaxContext` the tax-accrual + activity flows validate against.
    fn tax_context(&self) -> &TaxContext;

    /// Advisory inline validation of a candidate ledger event against the current
    /// cached snapshot (writes nothing). (TUI-ENTRY-FLOW-001)
    fn validate_ledger(&self, candidate: &LedgerEvent) -> Result<(), ledger_core::LedgerError>;

    /// Advisory inline validation of a candidate tax event (writes nothing).
    /// (TUI-ENTRY-FLOW-001)
    fn validate_tax(&self, candidate: &TaxEvent) -> Result<(), tax::TaxError>;

    /// Authoritative submit of a ledger event: re-validate live → acquire-lock →
    /// append + read-back-verify. (TUI-ENTRY-FLOW-002/003/004/005)
    fn submit_ledger(&mut self, candidate: &LedgerEvent) -> SubmitOutcome;

    /// Authoritative submit of a tax event. (TUI-ENTRY-FLOW-002/003/004/005)
    fn submit_tax(&mut self, candidate: &TaxEvent) -> SubmitOutcome;

    /// The "today" date the composers default to. (TUI-ENTRY-FLOW-007)
    fn today(&self) -> Date;
}
