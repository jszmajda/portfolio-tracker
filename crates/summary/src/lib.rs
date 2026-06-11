//! `summary` — the headless daily-summary command (the `dailies` replacement).
//!
//! A non-interactive run that **captures** the day's value point (via `reports`'
//! trading-day History), computes a **point-to-point day-over-day delta**, and
//! prints a terminal portfolio summary (text, or a versioned `--json`) to stdout for
//! the `dailies` framework. It is **thin orchestration + formatting** — the numbers
//! come from `reports`, `tax`, and `ledger-core`; `summary` adds no analytics of its
//! own. See `docs/intent/summary/summary-design.md` and `-specs.md` (prefix
//! `SUMMARY`).
//!
//! Principles (from the design):
//!
//! - **Format, don't compute.** Composition, value, and the delta come from
//!   `reports`; reserve status + the unrealized net estimate come from `tax`.
//! - **Point-to-point, reconciling deltas.** Both delta operands are History points,
//!   so `header value − delta = baseline value` always holds (SUMMARY-DELTA-001).
//! - **Honest deltas.** A delta across an incomplete (degraded) point, or against a
//!   live-uncaptured value, is rendered **distinctly** — never a plain real move
//!   (SUMMARY-DELTA-003/005).
//! - **Degrade, never lie to dailies.** Offline / lock-held → a clearly **stale** but
//!   usable summary, exit 0 (SUMMARY-OUT-002, SUMMARY-EXIT-001). A failure that means
//!   *no trustworthy summary* (corrupt History, bad creds, unreadable cache) exits
//!   **2** (SUMMARY-EXIT-002).
//!
//! `summary` is orchestration + formatting — **unit-tested**, not Verus-proven
//! (CLAUDE.md "cargo test is the gate"). All money is integer `pt_core::Cents`; no
//! float past the input boundary.

use std::collections::BTreeMap;

use ledger_core::{Snapshot, Symbol};
use pt_core::{Cents, Date, MicroShares};
use reports::{
    append_snapshot, build_series_point, read_history, HistoryClient, HistoryError, PricedMarks,
    SeriesPoint, TradingDayKey,
};
use store::Lock;
use tax::{quarter_of, AnnualRow, UnrealizedEstimate};

pub use config::BracketState;
pub use tax::Quarter;

pub mod testkit;

/// The `--json` schema version. Consumers gate on shape changes (no recreating the
/// legacy scrape fragility). (SUMMARY-OUT-001)
pub const JSON_SCHEMA_VERSION: u32 = 1;

// ===========================================================================
// Inputs (summary-design.md → "Interfaces"). Everything `summary` formats comes
// from `runtime`'s replay (Snapshot + tax estimates + annual rows + the reduced
// trading-day key + the priced marks) plus `config`'s bracket state — `summary`
// computes nothing of its own. Held in one struct so the orchestration entry takes
// a single, already-resolved bundle.
// ===========================================================================

/// The already-resolved inputs `summary` formats, supplied by `runtime`'s replay
/// cycle. `summary` adds no analytics; it captures, deltas, and renders these.
/// (summary-design.md → "Interfaces")
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SummaryInputs {
    /// The replayed `Snapshot` (positions, open lots, realized gains), valued with
    /// the cycle's injected marks.
    pub snapshot: Snapshot,
    /// `tax`'s per-symbol unrealized estimate (the net-of-tax input the header and
    /// positions render). (SUMMARY-EMIT-002)
    pub estimates: BTreeMap<Symbol, UnrealizedEstimate>,
    /// The per-`(jurisdiction, tax_year)` annual reserve rows (`tax`), the
    /// tax-reserve line's source. (SUMMARY-EMIT-001)
    pub annual_rows: Vec<AnnualRow>,
    /// The current marks (symbol → price + quote-epoch). A symbol **absent** here is
    /// degraded (unpriced) — never valued at zero. (SUMMARY-EMIT-003)
    pub marks: PricedMarks,
    /// The single trading-day key `runtime` reduced from the per-symbol
    /// quote-epochs — the key the capture point and the delta use. `None` when no
    /// symbol is priced (a degraded-only cycle keys nothing). (SUMMARY-DELTA-001)
    pub trading_day_key: Option<TradingDayKey>,
    /// The **trading-day calendar** `runtime` supplies — the ordered real trading
    /// days (the same calendar `reports::render_series` consumes), so "more than one
    /// trading day prior" is a *trading-day* span, not a calendar-day subtraction
    /// (which mislabels a normal Fri→Mon pair as a weekend "gap"). A gap exists when
    /// at least one calendar trading day lies strictly between the baseline and the
    /// current point. Empty when `runtime` supplies no calendar (no gap is then
    /// flagged — the header still annotates the actual baseline day).
    /// (SUMMARY-DELTA-002)
    pub trading_day_calendar: Vec<TradingDayKey>,
    /// The reporting-TZ calendar date of the capture (metadata on the point).
    pub reporting_tz_date: Date,
    /// The wall-clock run time (epoch seconds) — the secondary "as of" line, and the
    /// capture timestamp. (SUMMARY-EMIT-001)
    pub run_at_epoch_secs: i64,
    /// The effective `config` bracket state for the current tax year, driving how
    /// the header Net `[est]` and the tax line render. (SUMMARY-EMIT-004)
    pub bracket_state: BracketState,
    /// The current tax year (the tax-reserve line's year). (SUMMARY-EMIT-001)
    pub tax_year: i32,
}

// ===========================================================================
// The advisory write-lock held probe (summary-design.md → "Daily Capture";
// SUMMARY-CAP-002). `runtime` owns the cross-process advisory write-lock; before
// capture, `summary` checks whether it is HELD (an open interactive TUI) and, if
// so, runs read-only. This thin probe is `runtime::AdvisoryLock::try_acquire().
// is_held()` in production; tests inject a fake.
// ===========================================================================

/// The advisory-write-lock **held** probe `summary` consults before capture. When
/// the lock is held (an open interactive TUI), `summary` runs **read-only** — it
/// skips the capture and uses the uncaptured-delta path, noting it. `runtime`
/// implements this over `AdvisoryLock::try_acquire().is_held()`; tests inject a
/// fake. (SUMMARY-CAP-002)
pub trait WriteLockProbe {
    /// `true` when the cross-process advisory write-lock is currently held by
    /// another holder (so `summary` must run read-only). (SUMMARY-CAP-002)
    fn is_held(&self) -> bool;
}

// ===========================================================================
// Day-over-day delta (summary-design.md → "Day-Over-Day Delta"; SUMMARY-DELTA-*).
// Both operands are History points so everything reconciles; degraded/uncaptured
// operands are rendered distinctly, never a fabricated move.
// ===========================================================================

/// Why a delta is suppressed (rendered `—‡` with a footer reason) rather than shown
/// as a plain move. Honors `reports`' `REPORT-VOT-004`. (SUMMARY-DELTA-003)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SuppressReason {
    /// The current operand (today's appended point) was an incomplete capture.
    CurrentIncomplete,
    /// The baseline (most recent strictly-prior point) was an incomplete capture.
    BaselineIncomplete,
    /// Both endpoints were incomplete.
    BothIncomplete,
}

/// The day-over-day delta `summary` renders. Both the normal and the degraded /
/// first-run / uncaptured cases are distinct variants, so a fabricated move is
/// impossible by construction. (SUMMARY-DELTA-001..005)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Delta {
    /// The first ever run: no strictly-prior point. The delta column shows `—`.
    /// (SUMMARY-DELTA-004)
    FirstEver,
    /// The normal point-to-point delta: current = today's appended point, baseline
    /// = the most recent **strictly-prior** trading-day point. `header value −
    /// delta = baseline value` reconciles. (SUMMARY-DELTA-001/002)
    PointToPoint {
        /// The baseline (most recent strictly-prior) point's trading-day key.
        baseline_key: TradingDayKey,
        /// The current (today's appended) point's trading-day key.
        current_key: TradingDayKey,
        /// The total-value change (`current − baseline`). (SUMMARY-DELTA-001)
        total_delta_cents: Cents,
        /// `true` when the baseline is **more than one trading day prior** (a gap),
        /// so the header annotates the actual span. Computed against the
        /// **trading-day calendar** (a trading-day span), never calendar-day
        /// subtraction — a normal Fri→Mon consecutive pair is *not* a gap.
        /// (SUMMARY-DELTA-002)
        spans_gap: bool,
    },
    /// A point-to-point delta whose computation crossed an **incomplete** endpoint:
    /// the delta is suppressed (`—‡`) with a footer reason, never fabricated.
    /// (SUMMARY-DELTA-003)
    Suppressed {
        baseline_key: TradingDayKey,
        current_key: TradingDayKey,
        reason: SuppressReason,
    },
    /// No point was appended this run (offline / append failed / lock held): a
    /// **distinct, flagged** delta from the live uncaptured value against the latest
    /// stored point — never the plain point-to-point form. The current operand is
    /// the live composition value, explicitly **uncaptured**. (SUMMARY-DELTA-005)
    Uncaptured {
        /// The latest stored point's key (the baseline), or `None` if the store is
        /// empty (no stored point to compare against).
        baseline_key: Option<TradingDayKey>,
        /// The live (uncaptured) total value the delta was measured from.
        live_total_cents: Cents,
        /// `current − baseline` against the latest stored point; `None` when there
        /// is no stored point to compare against (first-ever uncaptured run).
        total_delta_cents: Option<Cents>,
    },
}

// ===========================================================================
// The rendered report model (summary-design.md → "What It Emits"). A neutral
// in-memory model the text and --json renderers both project from, so the two
// outputs never drift.
// ===========================================================================

/// How a money figure that depends on the tax brackets (the header Net, a tax-line
/// number) is qualified, per `config`'s bracket state. (SUMMARY-EMIT-004)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaxQualifier {
    /// `Verified` → shown plainly.
    Plain,
    /// `Stale` → shown, marked `[est, brackets stale]`.
    Stale,
    /// `NoBracketsAvailable` (cold-start) → `n/a (no brackets)`; never fabricated.
    NoBrackets,
}

/// One per-symbol position row in the rendered report: shares, price, market value,
/// and the day delta vs the baseline point. A degraded (unpriced, or degraded in
/// either delta operand) symbol carries `None` value/price and a suppressed delta.
/// (SUMMARY-EMIT-001/003, SUMMARY-DELTA-003)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PositionRow {
    pub symbol: Symbol,
    /// Share count at the current mark (metadata; never diffed across time).
    pub shares: MicroShares,
    /// The current per-share price; `None` when unpriced (degraded). (SUMMARY-EMIT-003)
    pub price_cents: Option<Cents>,
    /// Market value at the current mark; `None` when unpriced. (SUMMARY-EMIT-003)
    pub value_cents: Option<Cents>,
    /// The day value-delta vs the baseline point; `None` when suppressed (degraded
    /// in either operand) or unavailable (first-ever / no baseline value).
    /// (SUMMARY-DELTA-003)
    pub delta_cents: Option<Cents>,
    /// `true` when this symbol is unpriced/degraded — rendered `—‡` (delta
    /// unavailable) and counted in the degraded-symbols footer. (SUMMARY-EMIT-003)
    pub degraded: bool,
}

/// The header block: total market value and post-tax net, each with the day delta.
/// (SUMMARY-EMIT-001/002)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Header {
    /// Total market value across **priced** symbols (partial when any symbol is
    /// degraded). (SUMMARY-EMIT-001/003)
    pub total_value_cents: Cents,
    /// Post-tax net (`tax`'s unrealized net estimate over the SAME priced/degraded
    /// set as Total value). `None` when the bracket state is `NoBracketsAvailable`
    /// (cold-start) — never a fabricated net. (SUMMARY-EMIT-002/004)
    pub net_post_tax_cents: Option<Cents>,
    /// How the Net is qualified per the bracket state. (SUMMARY-EMIT-004)
    pub net_qualifier: TaxQualifier,
    /// `true` when the totals are partial (any symbol unpriced) — Net is flagged
    /// partial whenever Total value is. (SUMMARY-EMIT-002/003)
    pub partial: bool,
}

/// The current-year tax-reserve line: accrued / moved / outstanding for the year,
/// summed across jurisdictions from `tax`'s annual rows, qualified by the bracket
/// state. `outstanding` is `tax`'s canonical figure (`accrued − paid`), the
/// cross-segment definition (tax-design.md → "Reserves"). The line also surfaces the
/// **next estimated-payment period** — `quarter_of(today)` over `tax`'s quarterly
/// report — which is `None` on cold-start so no fabricated period is emitted.
/// (SUMMARY-EMIT-001/004/006)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TaxLine {
    /// The reserve year.
    pub year: i32,
    /// Σ accrued across jurisdictions this year; `None` on cold-start (no brackets).
    pub accrued_cents: Option<Cents>,
    /// Σ moved this year; `None` on cold-start.
    pub moved_cents: Option<Cents>,
    /// Σ outstanding this year (`tax`'s `accrued − paid`); `None` on cold-start.
    pub outstanding_cents: Option<Cents>,
    /// The next IRS estimated-payment period — the period today falls in,
    /// `quarter_of(today)`, computed against `tax`'s quarterly report. `None` on
    /// cold-start (`NoBracketsAvailable`): never a fabricated period.
    /// (SUMMARY-EMIT-006)
    pub next_period: Option<Quarter>,
    /// How the tax line is qualified per the bracket state. (SUMMARY-EMIT-004)
    pub qualifier: TaxQualifier,
}

/// Why an uncaptured run is stale — distinguished so the note never conflates a
/// concurrent writer with a connectivity failure. (SUMMARY-CAP-003)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StaleReason {
    /// A concurrent writer (an open interactive TUI) held the advisory write-lock, so
    /// `summary` ran read-only and skipped the capture. (SUMMARY-CAP-002/003)
    LockHeld,
    /// No point could be durably appended this run — the append failed (offline /
    /// batchUpdate failed), or there was no priced symbol to capture. Distinct from a
    /// held lock: nothing was *blocking* the write, the write *failed*.
    /// (SUMMARY-CAP-003, SUMMARY-EXIT-003)
    OfflineOrAppendFailed,
}

/// The full rendered report model. The text and `--json` renderers both project
/// from this, so the two outputs never drift. (summary-design.md → "What It Emits")
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Report {
    /// The trading day of the displayed value (the appended point's key) — the
    /// **headline** date. `None` for an uncaptured-only run with no priced symbol.
    /// (SUMMARY-EMIT-001)
    pub trading_day: Option<TradingDayKey>,
    /// The baseline trading day of the delta (the most recent strictly-prior point),
    /// when one exists. (SUMMARY-DELTA-001)
    pub baseline_day: Option<TradingDayKey>,
    /// The wall-clock run time (epoch seconds) — the secondary "as of" line.
    pub run_at_epoch_secs: i64,
    /// The header (total value + net, each with the day delta). (SUMMARY-EMIT-001/002)
    pub header: Header,
    /// The total-value day delta. (SUMMARY-DELTA-*)
    pub delta: Delta,
    /// Per-symbol position rows. (SUMMARY-EMIT-001)
    pub positions: Vec<PositionRow>,
    /// The current-year tax-reserve line. (SUMMARY-EMIT-001/004)
    pub tax_line: TaxLine,
    /// The degraded (unpriced) symbols this run — named in the footer; totals are
    /// partial whenever this is non-empty. (SUMMARY-EMIT-003)
    pub degraded_symbols: Vec<Symbol>,
    /// `true` when the run is **stale**: printed from the cache (offline) or
    /// read-only (lock held) — the staleness marked in-band. (SUMMARY-OUT-002)
    pub stale: bool,
    /// Why the run is stale, when it is — `LockHeld` (a concurrent writer) versus
    /// `OfflineOrAppendFailed` (no point durably appended). `None` for a fresh
    /// (non-stale) run. The note tells the operator which case occurred rather than
    /// conflating them. (SUMMARY-CAP-003)
    pub stale_reason: Option<StaleReason>,
}

// ===========================================================================
// Exit codes (summary-design.md → "Exit Codes & Failure Behavior"; SUMMARY-EXIT-*).
// ===========================================================================

/// The `summary` process exit contract. (SUMMARY-EXIT-001/002/003)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExitCode {
    /// **0** — a summary was produced: fresh, **or** stale (offline / lock held /
    /// append failed), staleness marked in-band. (SUMMARY-EXIT-001/003)
    Produced = 0,
    /// **2** — no trustworthy summary could be produced: a `reports` History
    /// integrity flag, missing/invalid credentials, or an unreadable cache.
    /// (SUMMARY-EXIT-002)
    NoTrustworthySummary = 2,
}

impl ExitCode {
    /// The numeric exit code the process returns to `dailies`.
    pub fn code(self) -> i32 {
        self as i32
    }
}

/// Why no trustworthy summary could be produced (exit 2). (SUMMARY-EXIT-002)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FatalError {
    /// A `reports` History integrity flag (corrupt / edited non-reconstructable
    /// series, an unparseable row). (SUMMARY-EXIT-002)
    HistoryIntegrity(HistoryError),
    /// Missing or invalid credentials. (SUMMARY-EXIT-002)
    BadCredentials,
    /// The local cache was unreadable. (SUMMARY-EXIT-002)
    UnreadableCache,
}

impl std::fmt::Display for FatalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for FatalError {}

/// The outcome of a `summary` run: a produced report + exit 0, or a fatal error +
/// exit 2. The report is always usable when present (fresh or stale-marked).
/// (SUMMARY-EXIT-001/002)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SummaryRun {
    /// A summary was produced (exit 0). The report carries `stale` in-band.
    Produced(Report),
    /// No trustworthy summary (exit 2). (SUMMARY-EXIT-002)
    Fatal(FatalError),
}

impl SummaryRun {
    /// The process exit code for this run. (SUMMARY-EXIT-001/002)
    pub fn exit_code(&self) -> ExitCode {
        match self {
            SummaryRun::Produced(_) => ExitCode::Produced,
            SummaryRun::Fatal(_) => ExitCode::NoTrustworthySummary,
        }
    }
}

// ===========================================================================
// The capture step (summary-design.md → "Daily Capture"; SUMMARY-CAP-001/002,
// SUMMARY-DELTA-005, SUMMARY-EXIT-003). Trigger reports.append_snapshot for the
// current trading day BEFORE computing the delta; read-only if the lock is held; a
// failed append is non-fatal (uncaptured-delta path, noted).
// ===========================================================================

/// The result of the capture attempt: whether a point was durably appended this
/// run, and the point built for the current trading day (always built, so the
/// uncaptured path can compare the live value). (SUMMARY-CAP-001/002, SUMMARY-EXIT-003)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CaptureOutcome {
    /// The point was durably appended (a fresh run). (SUMMARY-CAP-001)
    Appended(SeriesPoint),
    /// Read-only: the advisory write-lock was held, so the capture was skipped and
    /// the run is stale (uncaptured-delta path). (SUMMARY-CAP-002, SUMMARY-DELTA-005)
    SkippedLockHeld(SeriesPoint),
    /// The append failed mid-run (offline / batchUpdate failed): non-fatal — the
    /// uncaptured-delta path is used and the failure noted, exit 0 (retried next
    /// run, last-wins → no duplicate). (SUMMARY-EXIT-003, SUMMARY-DELTA-005)
    AppendFailed(SeriesPoint),
}

impl CaptureOutcome {
    /// The point built for the current trading day (always available).
    pub fn point(&self) -> &SeriesPoint {
        match self {
            CaptureOutcome::Appended(p)
            | CaptureOutcome::SkippedLockHeld(p)
            | CaptureOutcome::AppendFailed(p) => p,
        }
    }

    /// `true` when no point was durably appended this run (lock held or append
    /// failed) — the run is stale and uses the uncaptured-delta path.
    /// (SUMMARY-DELTA-005, SUMMARY-OUT-002)
    pub fn is_uncaptured(&self) -> bool {
        !matches!(self, CaptureOutcome::Appended(_))
    }

    /// Why this outcome is stale, distinguishing a held lock from an offline /
    /// append-failed run — `None` for a fresh (appended) run. The two are NOT
    /// conflated: a concurrent writer (`SkippedLockHeld`) is a different operator
    /// situation than a connectivity / write failure (`AppendFailed`). This is the
    /// source of truth the report carries and `render_text` notes distinctly.
    /// (SUMMARY-CAP-003)
    // @spec SUMMARY-CAP-003
    pub fn stale_reason(&self) -> Option<StaleReason> {
        match self {
            CaptureOutcome::Appended(_) => None,
            CaptureOutcome::SkippedLockHeld(_) => Some(StaleReason::LockHeld),
            CaptureOutcome::AppendFailed(_) => Some(StaleReason::OfflineOrAppendFailed),
        }
    }
}

/// Build the current trading-day `SeriesPoint` from the inputs (delegates to
/// `reports::build_series_point`), keyed by the runtime-reduced trading-day key.
/// Returns `None` when no symbol is priced (no trading-day key — a degraded-only
/// cycle keys nothing). (SUMMARY-CAP-001)
pub fn build_current_point(inputs: &SummaryInputs) -> Option<SeriesPoint> {
    let key = inputs.trading_day_key?;
    Some(build_series_point(
        &inputs.snapshot,
        &inputs.marks,
        &inputs.estimates,
        key,
        inputs.run_at_epoch_secs,
        inputs.reporting_tz_date,
    ))
}

/// Trigger the daily capture for the current trading day BEFORE computing the delta
/// (`reports.append_snapshot`, last-wins per trading day), honoring the advisory
/// write-lock:
///
/// - If the lock is **held** (`probe.is_held()`), run **read-only**: skip the
///   capture, return [`CaptureOutcome::SkippedLockHeld`]. (SUMMARY-CAP-002)
/// - Otherwise append the point via `reports::append_snapshot`. On success return
///   [`CaptureOutcome::Appended`]; on failure return [`CaptureOutcome::AppendFailed`]
///   (non-fatal — the uncaptured path is used and the failure noted; exit 0).
///   (SUMMARY-CAP-001, SUMMARY-EXIT-003)
///
/// Returns `None` when there is no current point to capture (no priced symbol → no
/// trading-day key). (SUMMARY-CAP-001)
pub fn capture<C: HistoryClient, L: Lock, P: WriteLockProbe>(
    client: &mut C,
    lock: &L,
    probe: &P,
    inputs: &SummaryInputs,
) -> Option<CaptureOutcome> {
    // Build the current trading-day point (None when no symbol is priced — a
    // degraded-only cycle keys nothing, so there is nothing to capture).
    let point = build_current_point(inputs)?;

    // If the advisory write-lock is HELD (an open interactive TUI), run read-only:
    // skip the capture entirely and use the uncaptured-delta path, noting it.
    // (SUMMARY-CAP-002)
    if probe.is_held() {
        return Some(CaptureOutcome::SkippedLockHeld(point));
    }

    // Trigger reports.append_snapshot for the current trading day BEFORE computing
    // the delta — last-wins per trading day, the lock acquired INSIDE the primitive.
    // A failed append is NON-fatal: use the uncaptured-delta path and note it (exit 0,
    // retried next run — last-wins → no duplicate). (SUMMARY-CAP-001, SUMMARY-EXIT-003)
    match append_snapshot(client, lock, &point) {
        Ok(()) => Some(CaptureOutcome::Appended(point)),
        Err(_) => Some(CaptureOutcome::AppendFailed(point)),
    }
}

// ===========================================================================
// Delta computation (summary-design.md → "Day-Over-Day Delta"; SUMMARY-DELTA-*).
// ===========================================================================

/// Compute the day-over-day delta given the capture outcome, the stored History
/// series (`stored`, in trading-day key order, integrity-checked by the caller), and
/// the **trading-day calendar** (`calendar`, `runtime`'s ordered real trading days):
///
/// - **Appended**: the normal point-to-point delta — baseline = the most recent
///   point whose key is **strictly less than** the current point's. `header − delta
///   = baseline` reconciles (SUMMARY-DELTA-001); the span is flagged when the
///   baseline is more than one **trading day** prior — measured against `calendar`,
///   never calendar-day subtraction, so a normal Fri→Mon pair is not a "gap"
///   (SUMMARY-DELTA-002); a delta across an **incomplete** endpoint is suppressed
///   (SUMMARY-DELTA-003); no strictly-prior point yields `FirstEver`
///   (SUMMARY-DELTA-004).
/// - **Uncaptured** (lock held / append failed): a distinct, flagged
///   [`Delta::Uncaptured`] from the live value against the latest stored point —
///   never the plain point-to-point form. (SUMMARY-DELTA-005)
pub fn compute_delta(
    capture: &CaptureOutcome,
    stored: &[SeriesPoint],
    calendar: &[TradingDayKey],
) -> Delta {
    match capture {
        // --- The normal point-to-point path (a point WAS appended this run). ---
        CaptureOutcome::Appended(current) => {
            // Baseline = the most recent stored point whose trading-day key is
            // STRICTLY LESS than the current point's (not "the second row" — that
            // breaks if today's append failed; not today itself). (SUMMARY-DELTA-001)
            let baseline = stored
                .iter()
                .filter(|p| p.key < current.key)
                .max_by_key(|p| p.key);

            let Some(baseline) = baseline else {
                // No strictly-prior point: the first ever run renders `—`.
                // (SUMMARY-DELTA-004)
                return Delta::FirstEver;
            };

            // A delta computed across an INCOMPLETE endpoint is suppressed (`—‡`)
            // with a footer reason — never a fabricated number (honors
            // REPORT-VOT-004). (SUMMARY-DELTA-003)
            if current.incomplete || baseline.incomplete {
                let reason = match (current.incomplete, baseline.incomplete) {
                    (true, true) => SuppressReason::BothIncomplete,
                    (true, false) => SuppressReason::CurrentIncomplete,
                    (false, true) => SuppressReason::BaselineIncomplete,
                    (false, false) => unreachable!(),
                };
                return Delta::Suppressed {
                    baseline_key: baseline.key,
                    current_key: current.key,
                    reason,
                };
            }

            // The reconciling point-to-point delta: header − delta = baseline.
            // (SUMMARY-DELTA-001) A baseline more than one TRADING day prior is a
            // gap, so the header annotates the actual span. The span is measured
            // against the trading-day CALENDAR — a gap exists when at least one
            // calendar trading day lies strictly between the baseline and the current
            // key — never calendar-day subtraction (which would mislabel a normal
            // Fri→Mon consecutive pair, 3 calendar days apart, as a weekend "gap").
            // (SUMMARY-DELTA-002)
            let total_delta_cents =
                Cents(current.total_market_value_cents.0 - baseline.total_market_value_cents.0);
            let spans_gap = calendar
                .iter()
                .any(|d| *d > baseline.key && *d < current.key);
            Delta::PointToPoint {
                baseline_key: baseline.key,
                current_key: current.key,
                total_delta_cents,
                spans_gap,
            }
        }

        // --- The uncaptured path (lock held / append failed — no appended point). ---
        // A distinct, flagged delta from the LIVE uncaptured value against the LATEST
        // stored point — never the plain point-to-point form. (SUMMARY-DELTA-005)
        CaptureOutcome::SkippedLockHeld(live) | CaptureOutcome::AppendFailed(live) => {
            let baseline = stored.iter().max_by_key(|p| p.key);
            let live_total_cents = live.total_market_value_cents;
            match baseline {
                Some(b) => Delta::Uncaptured {
                    baseline_key: Some(b.key),
                    live_total_cents,
                    total_delta_cents: Some(Cents(
                        live_total_cents.0 - b.total_market_value_cents.0,
                    )),
                },
                // No stored point to compare against (first-ever uncaptured run):
                // never fabricate a delta. (SUMMARY-DELTA-005)
                None => Delta::Uncaptured {
                    baseline_key: None,
                    live_total_cents,
                    total_delta_cents: None,
                },
            }
        }
    }
}

// ===========================================================================
// Report assembly (summary-design.md → "What It Emits"). Builds the neutral Report
// model from the inputs, the capture outcome, and the delta.
// ===========================================================================

/// Map a `config` [`BracketState`] to the rendered [`TaxQualifier`]: `Verified` →
/// `Plain`, `Stale` → `Stale`, `NoBracketsAvailable` → `NoBrackets`. (SUMMARY-EMIT-004)
pub fn qualifier_for(state: BracketState) -> TaxQualifier {
    match state {
        BracketState::Verified => TaxQualifier::Plain,
        BracketState::Stale => TaxQualifier::Stale,
        BracketState::NoBracketsAvailable => TaxQualifier::NoBrackets,
    }
}

/// Assemble the neutral [`Report`] model from the inputs, the capture outcome, the
/// computed delta, and the **baseline point** (the most recent strictly-prior /
/// latest-stored point the delta was measured against, for per-symbol deltas). The
/// header Net uses the SAME priced/degraded set as Total value and is flagged
/// partial whenever Total value is (SUMMARY-EMIT-002); unpriced symbols are named and
/// the totals marked partial (SUMMARY-EMIT-003); the Net and tax line render per the
/// bracket state (SUMMARY-EMIT-004); per-symbol deltas are suppressed across a
/// degraded operand (SUMMARY-DELTA-003); the run is marked stale when uncaptured
/// (SUMMARY-OUT-002). The headline `trading_day` is carried as the opaque trading-day
/// key — `runtime` owns the trading-day-key→calendar-date formatting; the model (and
/// the JSON) carry the raw key, never a calendar date (SUMMARY-EMIT-007). A priced
/// symbol that lacks a folded tax estimate marks the totals partial (`Header.partial`,
/// via `reports`' incomplete point) without being named in `degraded_symbols`
/// (SUMMARY-EMIT-008). (SUMMARY-EMIT-001..004)
// @spec SUMMARY-EMIT-007, SUMMARY-EMIT-008
pub fn build_report(
    inputs: &SummaryInputs,
    capture: &CaptureOutcome,
    delta: Delta,
    baseline: Option<&SeriesPoint>,
) -> Report {
    let point = capture.point();
    let qualifier = qualifier_for(inputs.bracket_state);
    let stale = capture.is_uncaptured();
    let stale_reason = capture.stale_reason();

    // --- Totals (from the series point — priced symbols only). ---
    let total_value_cents = point.total_market_value_cents;
    // Partial whenever any open position has no priced value this run. (SUMMARY-EMIT-003)
    let partial = point.incomplete;

    // The header Net uses the SAME priced/degraded set as Total value. Net = total
    // value − estimated tax on the priced unrealized; the series point already folds
    // the priced set into pretax and net-of-tax, so the estimated tax is their
    // difference and Net = total value − that. On cold-start (NoBracketsAvailable)
    // Net is n/a — never a fabricated net. (SUMMARY-EMIT-002/004)
    let net_post_tax_cents = match inputs.bracket_state {
        BracketState::NoBracketsAvailable => None,
        _ => {
            let estimated_tax =
                point.total_unrealized_pretax_cents.0 - point.total_unrealized_net_of_tax_cents.0;
            Some(Cents(total_value_cents.0 - estimated_tax))
        }
    };

    // --- Per-symbol position rows. ---
    // A symbol is degraded this run when it has no priced value in the current
    // point (no mark, or no tax estimate folded). Its price/value are None and its
    // delta is never fabricated. (SUMMARY-EMIT-003, SUMMARY-DELTA-003)
    //
    // Per-symbol deltas are suppressed wholesale for the first-ever run (no baseline)
    // and a globally-suppressed (incomplete-endpoint) delta. They are ALSO suppressed
    // on the **uncaptured** path: the design's uncaptured delta is a *distinct,
    // flagged* live-vs-stored figure (SUMMARY-DELTA-005), not the plain point-to-point
    // per-symbol move — so a per-symbol row must not show an unmarked signed move that
    // reads like a normal captured delta. (SUMMARY-DELTA-005)
    let suppress_all_symbol_deltas = matches!(
        delta,
        Delta::Suppressed { .. } | Delta::FirstEver | Delta::Uncaptured { .. }
    );
    let mut positions: Vec<PositionRow> = Vec::new();
    let mut degraded_symbols: Vec<Symbol> = Vec::new();
    for (symbol, pos) in &inputs.snapshot.positions {
        if pos.total_qty.0 == 0 {
            continue; // a closed-out position is not part of the live allocation
        }
        let value = point.per_symbol_value_cents.get(symbol).copied();
        let degraded = value.is_none();
        if degraded {
            degraded_symbols.push(symbol.clone());
        }
        // Per-share price (from the mark) when priced.
        let price_cents = inputs.marks.get(symbol).map(|m| m.price_cents);
        // Per-symbol value-delta vs the baseline point: only when this symbol is
        // priced in BOTH points and the run's delta is not globally suppressed.
        // (SUMMARY-DELTA-003)
        let delta_cents = if degraded || suppress_all_symbol_deltas {
            None
        } else {
            baseline
                .and_then(|b| b.per_symbol_value_cents.get(symbol).copied())
                .and_then(|from| value.map(|to| Cents(to.0 - from.0)))
        };
        positions.push(PositionRow {
            symbol: symbol.clone(),
            shares: pos.total_qty,
            price_cents,
            value_cents: value,
            delta_cents,
            degraded,
        });
    }

    // --- The current-year tax-reserve line (from tax's annual rows). ---
    let tax_line = build_tax_line(inputs, qualifier);

    Report {
        trading_day: inputs.trading_day_key,
        baseline_day: delta_baseline_key(&delta),
        run_at_epoch_secs: inputs.run_at_epoch_secs,
        header: Header {
            total_value_cents,
            net_post_tax_cents,
            net_qualifier: qualifier,
            partial,
        },
        delta,
        positions,
        tax_line,
        degraded_symbols,
        stale,
        stale_reason,
    }
}

/// The baseline trading-day key a delta carries (the most recent strictly-prior /
/// latest-stored point), or `None` (first-ever / uncaptured-with-empty-store).
fn delta_baseline_key(delta: &Delta) -> Option<TradingDayKey> {
    match delta {
        Delta::FirstEver => None,
        Delta::PointToPoint { baseline_key, .. } | Delta::Suppressed { baseline_key, .. } => {
            Some(*baseline_key)
        }
        Delta::Uncaptured { baseline_key, .. } => *baseline_key,
    }
}

/// Build the current-year tax-reserve line from `tax`'s annual rows, summed across
/// jurisdictions for the current tax year, plus the next estimated-payment period.
/// On cold-start (`NoBracketsAvailable`) every figure — including the period — is
/// `None`: the line says "set up tax brackets" rather than a fabricated reserve or a
/// fabricated period. The three reserve figures are summed across jurisdictions from
/// `tax`'s annual rows using `tax`'s canonical definitions (`outstanding = accrued −
/// paid`), never a locally re-derived figure. The next period is `quarter_of(today)`
/// computed against `tax`'s quarterly report. (SUMMARY-EMIT-001/004/006)
// @spec SUMMARY-EMIT-005, SUMMARY-EMIT-006
fn build_tax_line(inputs: &SummaryInputs, qualifier: TaxQualifier) -> TaxLine {
    if matches!(qualifier, TaxQualifier::NoBrackets) {
        // Cold-start: no fabricated reserve AND no fabricated period — the period is
        // `None` exactly when there are no brackets. (SUMMARY-EMIT-006)
        return TaxLine {
            year: inputs.tax_year,
            accrued_cents: None,
            moved_cents: None,
            outstanding_cents: None,
            next_period: None,
            qualifier,
        };
    }
    // Σ over the current year's annual rows (federal + states).
    let mut accrued = 0i64;
    let mut moved = 0i64;
    let mut outstanding = 0i64;
    for r in &inputs.annual_rows {
        if r.tax_year.0 != inputs.tax_year {
            continue;
        }
        accrued += r.accrued_cents.0;
        moved += r.moved_cents.0;
        outstanding += r.outstanding_cents.0;
    }
    TaxLine {
        year: inputs.tax_year,
        accrued_cents: Some(Cents(accrued)),
        moved_cents: Some(Cents(moved)),
        outstanding_cents: Some(Cents(outstanding)),
        next_period: next_estimated_period(inputs),
        qualifier,
    }
}

/// The next IRS estimated-payment period to surface on the tax line — the period
/// **today** falls in, `tax::quarter_of(today)`, where today is the reporting-TZ
/// calendar date `runtime` supplied. The period boundaries are `tax`'s own: this is
/// the very `quarter_of` partition `tax::quarterly_report` keys its per-period
/// realized-gain / accrual cells by (the report calls `quarter_of` on each
/// `sale_date`), so the period `summary` surfaces is `tax`'s, never a local
/// re-derivation — the design's "next estimated-payment period from
/// `tax::quarterly_report` + `quarter_of(today)`". The cold-start branch above
/// returns `None` (no brackets ⇒ no period), so a real period is always returned
/// here. (SUMMARY-EMIT-006)
// @spec SUMMARY-EMIT-006
fn next_estimated_period(inputs: &SummaryInputs) -> Option<Quarter> {
    Some(quarter_of(inputs.reporting_tz_date))
}

// ===========================================================================
// Renderers (summary-design.md → "Output Modes & Integration"; SUMMARY-OUT-001/002).
// The text report (default) and the versioned --json object project from the SAME
// Report model so they never drift.
// ===========================================================================

/// Render the [`Report`] as the compact ASCII terminal report (the default, to
/// stdout). The headline date is the trading day; the run time is a secondary "as
/// of" line; suppressed/first-run deltas render `—‡` / `—`; a degraded footer names
/// unpriced symbols; staleness is marked in-band. (SUMMARY-OUT-001/002,
/// SUMMARY-EMIT-001/003, SUMMARY-DELTA-003/004)
pub fn render_text(report: &Report) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();

    // Header: the headline date is the TRADING DAY; the run time is secondary.
    // (SUMMARY-EMIT-001) Dates are rendered as the integer days-since-epoch key
    // (the trading-day key is opaque; runtime supplies the calendar formatting in
    // production — the model carries the key, the test asserts the key/markers).
    let day = report
        .trading_day
        .map(|k| k.0 .0.to_string())
        .unwrap_or_else(|| "uncaptured".to_string());
    let span = match report.baseline_day {
        Some(b) => format!("since day {}", b.0 .0),
        None => "first point".to_string(),
    };
    let stale_marker = if report.stale { "  [STALE]" } else { "" };
    let _ = writeln!(
        s,
        "Portfolio -- trading day {day}    (delta {span}){stale_marker}"
    );
    let _ = writeln!(s, "  as of run {}", report.run_at_epoch_secs);
    let _ = writeln!(s, "{}", "-".repeat(62));

    // Total value + the day delta.
    let _ = write!(
        s,
        " Total value      {}      {}",
        money(report.header.total_value_cents.0),
        render_total_delta(&report.delta),
    );
    if report.header.partial {
        let _ = write!(s, "   [partial]");
    }
    let _ = writeln!(s);

    // Net (post-tax), qualified per the bracket state.
    let net = match (
        report.header.net_post_tax_cents,
        report.header.net_qualifier,
    ) {
        (_, TaxQualifier::NoBrackets) => "n/a (no brackets)".to_string(),
        (Some(c), TaxQualifier::Plain) => format!("{}            [est]", money(c.0)),
        (Some(c), TaxQualifier::Stale) => {
            format!("{}   [est, brackets stale]", money(c.0))
        }
        (None, _) => "n/a".to_string(),
    };
    let _ = writeln!(s, " Net (post-tax)   {net}");
    let _ = writeln!(s, "{}", "-".repeat(62));

    // Per-symbol positions.
    for p in &report.positions {
        match (p.price_cents, p.value_cents) {
            (Some(price), Some(value)) => {
                let _ = writeln!(
                    s,
                    " {:<6} {} @ {}    {}     {}",
                    p.symbol,
                    shares(p.shares.0),
                    money(price.0),
                    money(value.0),
                    p.delta_cents
                        .map(|d| signed_money(d.0))
                        .unwrap_or_else(|| "-".to_string()),
                );
            }
            // Degraded (unpriced) symbol: never a fabricated value; delta `-‡`.
            // (SUMMARY-EMIT-003, SUMMARY-DELTA-003)
            _ => {
                let _ = writeln!(
                    s,
                    " {:<6} {}   (unpriced)         -          -#",
                    p.symbol,
                    shares(p.shares.0),
                );
            }
        }
    }
    let _ = writeln!(s, "{}", "-".repeat(62));

    // The current-year tax-reserve line, qualified per the bracket state.
    match report.tax_line.qualifier {
        TaxQualifier::NoBrackets => {
            let _ = writeln!(
                s,
                " Tax reserve {}: n/a -- set up tax brackets",
                report.tax_line.year
            );
        }
        q => {
            let mark = if matches!(q, TaxQualifier::Stale) {
                "  [est, brackets stale]"
            } else {
                ""
            };
            // accrued . moved . outstanding (tax's accrued − paid) — all three are
            // surfaced (the design example shows moved alongside accrued/outstanding).
            // (SUMMARY-EMIT-001)
            let _ = writeln!(
                s,
                " Tax reserve {}: accrued {} . moved {} . outstanding {}{}",
                report.tax_line.year,
                money(report.tax_line.accrued_cents.map(|c| c.0).unwrap_or(0)),
                money(report.tax_line.moved_cents.map(|c| c.0).unwrap_or(0)),
                money(report.tax_line.outstanding_cents.map(|c| c.0).unwrap_or(0)),
                mark,
            );
            // The next estimated-payment period (quarter_of(today) over tax's
            // quarterly report). Rendered only when present — never fabricated on
            // cold-start. (SUMMARY-EMIT-006)
            if let Some(period) = report.tax_line.next_period {
                let _ = writeln!(s, " Next est. payment period: {}", quarter_label(period));
            }
        }
    }
    let _ = writeln!(s, "{}", "-".repeat(62));

    // Footers: suppressed-delta legend + the degraded-symbols warning.
    if matches!(report.delta, Delta::Suppressed { .. })
        || report.positions.iter().any(|p| p.degraded)
    {
        let _ = writeln!(s, " -# delta unavailable (degraded point)");
    }
    if !report.degraded_symbols.is_empty() {
        let _ = writeln!(
            s,
            " [!] {} symbol(s) unpriced ({}) -- totals partial",
            report.degraded_symbols.len(),
            report.degraded_symbols.join(", "),
        );
    }
    if report.stale {
        // Distinguish WHY the run is stale — a concurrent writer held the advisory
        // write-lock (read-only) versus no point could be durably appended (offline /
        // append failed). Never conflated in one note. (SUMMARY-CAP-003)
        let note = match report.stale_reason {
            Some(StaleReason::LockHeld) => {
                " note: stale summary (read-only -- another writer holds the lock)"
            }
            Some(StaleReason::OfflineOrAppendFailed) => {
                " note: stale summary (offline / append failed -- no point was recorded)"
            }
            // A stale run always carries a reason; fall back conservatively rather
            // than fabricate a cause.
            None => " note: stale summary",
        };
        let _ = writeln!(s, "{note}");
    }

    s
}

/// A short label for an IRS estimated-payment period. (SUMMARY-EMIT-006)
fn quarter_label(q: Quarter) -> &'static str {
    match q {
        Quarter::Q1 => "Q1",
        Quarter::Q2 => "Q2",
        Quarter::Q3 => "Q3",
        Quarter::Q4 => "Q4",
    }
}

/// Render the total-value delta for the text header per its variant: `-` for the
/// first ever run, `-‡` for a suppressed delta, the signed move otherwise.
/// (SUMMARY-DELTA-003/004)
fn render_total_delta(delta: &Delta) -> String {
    match delta {
        Delta::FirstEver => "-".to_string(),
        Delta::Suppressed { .. } => "-#".to_string(),
        Delta::PointToPoint {
            total_delta_cents, ..
        } => signed_money(total_delta_cents.0),
        Delta::Uncaptured {
            total_delta_cents, ..
        } => match total_delta_cents {
            Some(d) => format!("{} (uncaptured)", signed_money(d.0)),
            None => "-".to_string(),
        },
    }
}

/// Format `Cents` as a dollar string (e.g. `$1,284.30`). Integer-exact; no float.
fn money(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let abs = cents.unsigned_abs();
    let dollars = abs / 100;
    let rem = abs % 100;
    format!("{sign}${}.{:02}", group_thousands(dollars), rem)
}

/// Format `Cents` as a signed dollar string (`+$8.42` / `-$4.10`).
fn signed_money(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "+" };
    let abs = cents.unsigned_abs();
    let dollars = abs / 100;
    let rem = abs % 100;
    format!("{sign}${}.{:02}", group_thousands(dollars), rem)
}

/// Group an unsigned integer with thousands separators (`1284` -> `1,284`).
fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let bytes = digits.as_bytes();
    let mut out = String::new();
    let len = bytes.len();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// Format `MicroShares` as a share count (drops the micro-scale for display).
fn shares(micro: i64) -> String {
    let whole = micro / pt_core::SHARE_SCALE;
    let frac = (micro % pt_core::SHARE_SCALE).unsigned_abs();
    if frac == 0 {
        whole.to_string()
    } else {
        // Trim trailing zeros from the 6-dp fractional micro-share.
        let frac_str = format!("{frac:06}");
        let trimmed = frac_str.trim_end_matches('0');
        format!("{whole}.{trimmed}")
    }
}

/// Render the [`Report`] as the versioned `--json` object (a `schema_version`-tagged
/// shape so consumers can gate on shape changes), with `stale` in-band. (SUMMARY-OUT-001/002)
pub fn render_json(report: &Report) -> String {
    use std::fmt::Write as _;

    // Hand-built JSON (no serde dep): a `schema_version`-tagged object whose shape
    // matches summary-design.md → "Output Modes & Integration". Integer cents only;
    // a degraded / cold-start figure is `null` (never a fabricated zero).
    // (SUMMARY-OUT-001/002)
    let opt_i = |v: Option<i64>| match v {
        Some(n) => n.to_string(),
        None => "null".to_string(),
    };
    let opt_key = |k: Option<TradingDayKey>| match k {
        Some(t) => t.0 .0.to_string(),
        None => "null".to_string(),
    };

    let total_delta = match &report.delta {
        Delta::PointToPoint {
            total_delta_cents, ..
        } => Some(total_delta_cents.0),
        Delta::Uncaptured {
            total_delta_cents, ..
        } => total_delta_cents.map(|c| c.0),
        // First-ever / suppressed: no trustworthy total move → null (never fabricated).
        Delta::FirstEver | Delta::Suppressed { .. } => None,
    };

    let mut positions = String::from("[");
    for (i, p) in report.positions.iter().enumerate() {
        if i > 0 {
            positions.push(',');
        }
        let _ = write!(
            positions,
            "{{\"symbol\":{:?},\"shares\":{},\"price_cents\":{},\"value_cents\":{},\"delta_cents\":{},\"degraded\":{}}}",
            p.symbol,
            p.shares.0,
            opt_i(p.price_cents.map(|c| c.0)),
            opt_i(p.value_cents.map(|c| c.0)),
            opt_i(p.delta_cents.map(|c| c.0)),
            p.degraded,
        );
    }
    positions.push(']');

    let degraded_symbols = {
        let mut v = String::from("[");
        for (i, sym) in report.degraded_symbols.iter().enumerate() {
            if i > 0 {
                v.push(',');
            }
            let _ = write!(v, "{sym:?}");
        }
        v.push(']');
        v
    };

    let brackets_state = match report.header.net_qualifier {
        TaxQualifier::Plain => "Verified",
        TaxQualifier::Stale => "Stale",
        TaxQualifier::NoBrackets => "NoBracketsAvailable",
    };

    // The next estimated-payment period: `"Q2"` etc., or `null` on cold-start (never
    // a fabricated period). (SUMMARY-EMIT-006, SUMMARY-OUT-003)
    let next_period = match report.tax_line.next_period {
        Some(q) => format!("{:?}", quarter_label(q)),
        None => "null".to_string(),
    };

    let tax = format!(
        "{{\"year\":{},\"accrued\":{},\"moved\":{},\"outstanding\":{},\"next_period\":{},\"brackets_state\":{:?}}}",
        report.tax_line.year,
        opt_i(report.tax_line.accrued_cents.map(|c| c.0)),
        opt_i(report.tax_line.moved_cents.map(|c| c.0)),
        opt_i(report.tax_line.outstanding_cents.map(|c| c.0)),
        next_period,
        brackets_state,
    );

    let mut s = String::new();
    let _ = write!(
        s,
        "{{\"schema_version\":{},\"trading_day\":{},\"baseline_day\":{},\"run_at\":{},\
\"total_value_cents\":{},\"net_post_tax_cents\":{},\"total_delta_cents\":{},\
\"positions\":{},\"tax\":{},\"stale\":{},\"degraded_symbols\":{}}}",
        JSON_SCHEMA_VERSION,
        opt_key(report.trading_day),
        opt_key(report.baseline_day),
        report.run_at_epoch_secs,
        report.header.total_value_cents.0,
        opt_i(report.header.net_post_tax_cents.map(|c| c.0)),
        opt_i(total_delta),
        positions,
        tax,
        report.stale,
        degraded_symbols,
    );
    s
}

// ===========================================================================
// Output-mode selection & dispatch (summary-design.md → "Output Modes &
// Integration"; SUMMARY-OUT-001). `summary` defaults to a TEXT report on stdout and
// offers `--json`. The mode is selected from the process arguments; the selected
// renderer projects the produced report, and the run's exit code is paired with the
// rendered output so the calling entry point can `write` it to stdout and
// `process::exit` with the contract code. The argv→stdout→exit glue is a thin
// `fn main` over this pure, fully-tested seam.
// ===========================================================================

/// The output mode `summary` renders in. **Text is the default** (the `dailies`
/// replacement prints a terminal report to stdout); `--json` selects the versioned,
/// `schema_version`-tagged object. (SUMMARY-OUT-001)
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum OutputMode {
    /// The compact ASCII terminal report (the default, to stdout). (SUMMARY-OUT-001)
    #[default]
    Text,
    /// The versioned `--json` object. (SUMMARY-OUT-001)
    Json,
}

/// Select the [`OutputMode`] from the process arguments (the args *after* the
/// program name, e.g. `std::env::args().skip(1)`): **default Text**, `--json`
/// selects Json. Unknown flags do not change the mode (a forward-compatible
/// default). (SUMMARY-OUT-001)
pub fn select_mode<I, S>(args: I) -> OutputMode
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    if args.into_iter().any(|a| a.as_ref() == "--json") {
        OutputMode::Json
    } else {
        OutputMode::Text
    }
}

/// Dispatch a finished [`SummaryRun`] to its rendered output for the selected
/// [`OutputMode`], paired with the process [`ExitCode`] the entry point returns to
/// `dailies`. A produced report renders as text (default) or the versioned `--json`
/// object (both staleness-marked in-band); a fatal run renders a short diagnostic and
/// carries exit 2 — never a confident report. The thin `fn main` writes the string to
/// stdout and `std::process::exit`s with `exit.code()`. (SUMMARY-OUT-001/002,
/// SUMMARY-EXIT-001/002/003)
pub fn dispatch(run: &SummaryRun, mode: OutputMode) -> (String, ExitCode) {
    let exit = run.exit_code();
    let out = match (run, mode) {
        (SummaryRun::Produced(report), OutputMode::Text) => render_text(report),
        (SummaryRun::Produced(report), OutputMode::Json) => render_json(report),
        // No trustworthy summary: a short diagnostic, never a fabricated report.
        // (SUMMARY-EXIT-002)
        (SummaryRun::Fatal(err), OutputMode::Text) => {
            format!("summary: no trustworthy summary -- {err}\n")
        }
        (SummaryRun::Fatal(err), OutputMode::Json) => {
            format!("{{\"schema_version\":{JSON_SCHEMA_VERSION},\"error\":{err:?}}}")
        }
    };
    (out, exit)
}

// ===========================================================================
// Top-level orchestration (summary-design.md → all sections). Capture → delta →
// assemble → produce, with the exit contract. Integrity / creds / unreadable-cache
// failures exit 2; everything else produces a report (stale-marked if degraded),
// exit 0.
// ===========================================================================

/// The credential / cache trust state `runtime` reports before `summary` runs. A
/// failure here means *no trustworthy summary* (exit 2) — `summary` never prints a
/// confident report on bad creds or an unreadable cache. (SUMMARY-EXIT-002)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrustState {
    /// Credentials valid and the cache readable — proceed.
    Ok,
    /// Missing/invalid credentials — exit 2. (SUMMARY-EXIT-002)
    BadCredentials,
    /// The local cache was unreadable — exit 2. (SUMMARY-EXIT-002)
    UnreadableCache,
}

/// Run the whole `summary`: check trust (creds / cache), read the integrity-checked
/// stored History, capture today's point (honoring the held lock), compute the
/// delta, assemble + return the report with its exit code.
///
/// Exit **2** (no trustworthy summary) on: `trust != Ok` (bad creds / unreadable
/// cache), or a `reports` History integrity flag on read. Exit **0** otherwise — a
/// produced report, stale-marked in-band when offline / lock held / append failed.
/// (SUMMARY-EXIT-001/002/003, SUMMARY-CAP-001/002, SUMMARY-DELTA-*, SUMMARY-OUT-002)
pub fn run_summary<C: HistoryClient, L: Lock, P: WriteLockProbe>(
    client: &mut C,
    lock: &L,
    probe: &P,
    trust: TrustState,
    inputs: &SummaryInputs,
) -> SummaryRun {
    // 1. Trust gate: bad creds / an unreadable cache mean no trustworthy summary —
    //    exit 2, never a confident report. (SUMMARY-EXIT-002)
    match trust {
        TrustState::BadCredentials => return SummaryRun::Fatal(FatalError::BadCredentials),
        TrustState::UnreadableCache => return SummaryRun::Fatal(FatalError::UnreadableCache),
        TrustState::Ok => {}
    }

    // 2. Read the integrity-checked stored History (BEFORE the capture, so the delta
    //    baseline is the most recent strictly-prior point, not today's append). A
    //    History integrity flag (corrupt / out-of-order / unparseable / unreachable)
    //    means no trustworthy summary — exit 2. (SUMMARY-EXIT-002)
    let stored = match read_history(client) {
        Ok(points) => points,
        Err(e) => return SummaryRun::Fatal(FatalError::HistoryIntegrity(e)),
    };

    // 3. Capture today's point (honoring the held lock). No priced symbol → no point
    //    to capture; still produce a report (degraded, stale) from the inputs.
    //    A failed append is non-fatal (uncaptured path, exit 0). (SUMMARY-CAP-001/002,
    //    SUMMARY-EXIT-003)
    let Some(capture) = capture(client, lock, probe, inputs) else {
        // Nothing priced this run (no trading-day key): a degraded, stale report from
        // the inputs, treated as uncaptured against the latest stored point. The
        // placeholder's live value is 0 (nothing priced) and its key is a neutral
        // sentinel — NEVER the calendar (`reporting_tz_date`) day, honoring the
        // "never the calendar day as a key" discipline. The produced Report carries
        // `trading_day = None` (no priced trading day to headline), and the
        // Uncaptured delta reads only the live total (0) — the placeholder key is
        // never surfaced. (SUMMARY-DELTA-005, summary-design.md → "Day-Over-Day Delta")
        let placeholder = SeriesPoint {
            key: TradingDayKey::default(),
            total_market_value_cents: Cents(0),
            total_unrealized_pretax_cents: Cents(0),
            total_unrealized_net_of_tax_cents: Cents(0),
            total_basis_cents: Cents(0),
            per_symbol_value_cents: BTreeMap::new(),
            per_symbol_shares: BTreeMap::new(),
            marks: inputs.marks.clone(),
            captured_at_epoch_secs: inputs.run_at_epoch_secs,
            reporting_tz_date: inputs.reporting_tz_date,
            incomplete: true,
        };
        let outcome = CaptureOutcome::AppendFailed(placeholder);
        let delta = compute_delta(&outcome, &stored, &inputs.trading_day_calendar);
        let baseline = baseline_point_for(&delta, &stored);
        let report = build_report(inputs, &outcome, delta, baseline);
        return SummaryRun::Produced(report);
    };

    // 4. Compute the day-over-day delta and find the baseline point it measured
    //    against (for per-symbol deltas), then assemble + produce the report.
    //    (SUMMARY-DELTA-*, SUMMARY-EMIT-*, SUMMARY-EXIT-001)
    let delta = compute_delta(&capture, &stored, &inputs.trading_day_calendar);
    let baseline = baseline_point_for(&delta, &stored);
    let report = build_report(inputs, &capture, delta, baseline);
    SummaryRun::Produced(report)
}

/// Locate the baseline `SeriesPoint` a computed delta refers to within the stored
/// series (for per-symbol value deltas). `None` when the delta carries no baseline
/// (first-ever / uncaptured-with-empty-store).
fn baseline_point_for<'a>(delta: &Delta, stored: &'a [SeriesPoint]) -> Option<&'a SeriesPoint> {
    let key = delta_baseline_key(delta)?;
    stored.iter().find(|p| p.key == key)
}
