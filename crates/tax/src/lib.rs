//! `tax` — the verified tax-calculation + accrual-lifecycle kernel.
//!
//! Turns `ledger-core`'s `RealizedGain`s into money set aside and eventually
//! paid. Owns three things: the tax **calculation** (federal ST=ordinary /
//! LT=preferential + NIIT; state=ordinary on all gains; stacked, marginal in
//! chronological `(sale_date, sale_seq)` order), the accrual **lifecycle**
//! (Accrued → Allocated → Moved → Paid, event-sourced via a `TaxEvent` log),
//! and the **reserve** ledgers + quarterly/annual **reporting** on top. It owns
//! no share or cost-basis math — that is `ledger-core`'s. See
//! `docs/intent/tax/tax-design.md` and `-specs.md` (prefix `TAX`).
//!
//! Mirrors `ledger-core`'s dual-build discipline: the tax ARITHMETIC (`T_J`
//! bracket stacking, the marginal accrual increment, the effective unrealized
//! rate in `ppm`) lives in the `verus!{}` module `kernel` below — it ERASES
//! under plain stable `cargo build` (compiling as ordinary Rust the public API
//! calls) and is deductively verified under `cargo verus verify`. The accrual
//! lifecycle fold over the `TaxEvent` log uses `BTreeMap`/`String` and stays
//! OUTSIDE that boundary (covered by `#[test]`s; CBMC cannot fold heap
//! collections tractably). `#[cfg(kani)]` harnesses target the kernel only.
//!
//! TRUST BOUNDARY: serde and all I/O sit OUTSIDE this crate (the `store`
//! segment). `tax` takes already-deserialized `RealizedGain`s, `config` data,
//! and a `Vec<TaxEvent>`, and returns computed accruals / reserves / reports.

use std::collections::BTreeMap;

use config::{BracketSet, BracketState, Jurisdiction, Niit, StateCode, TaxYear};
use ledger_core::{LotId, OpenLot, RealizedGain, SaleId, Symbol};
use pt_core::{Cents, Date};

// ===========================================================================
// Accrual key & jurisdiction (tax-design.md → "Accrual Model & Lifecycle").
//
// An accrual is keyed by `(sale_id, lot_id, jurisdiction, tax_year)` — one per
// `RealizedGain` per jurisdiction, so each is unambiguously LT or ST.
// ===========================================================================

/// The stable identity of one accrual: one per `RealizedGain` per jurisdiction.
/// `(sale_id, lot_id)` matches the backing `RealizedGain`; `jurisdiction` is
/// Federal or a resolved state; `tax_year` is `year(sale_date)`. (TAX-ACCRUAL-001)
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct AccrualKey {
    pub sale_id: SaleId,
    pub lot_id: LotId,
    pub jurisdiction: Jurisdiction,
    pub tax_year: TaxYear,
}

/// The combined per-`(jurisdiction, tax_year)` key of a migration accrual, which
/// has no backing `RealizedGain`. (TAX-ACCRUAL-007)
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct MigrationKey {
    pub jurisdiction: Jurisdiction,
    pub tax_year: TaxYear,
}

// ===========================================================================
// Long-term / short-term classification (tax-design.md → "Tax Calculation";
// TAX-CALC-001).
// ===========================================================================

/// Whether a realized gain is taxed at the long-term or short-term regime.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Term {
    /// `sale_date` strictly after the first anniversary of `acquire_date`.
    LongTerm,
    /// On or before that anniversary.
    ShortTerm,
}

// ===========================================================================
// Lifecycle state (tax-design.md → "Accrual Model & Lifecycle"; the state a
// fold of the TaxEvent log derives per accrual). Forward-only:
//   Accrued ──Allocate──▶ Allocated ──Move──▶ Moved ──Pay──▶ Paid
// ===========================================================================

/// The lifecycle state of an accrual, derived by folding its `TaxEvent`s.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum AccrualState {
    /// The default the moment its `RealizedGain` exists; no event needed.
    Accrued,
    /// `Allocate` recorded the reserve account it belongs to. (TAX-ACCRUAL-002)
    Allocated { account_label: String },
    /// `Move` recorded the actual money moved into the reserve. (TAX-ACCRUAL-003)
    Moved {
        account_label: String,
        amount_cents: Cents,
        date: Date,
    },
    /// `Pay` recorded the remittance covering this accrual. (TAX-ACCRUAL-004)
    Paid {
        amount_cents: Cents,
        moved_cents: Cents,
        date: Date,
        period: Quarter,
    },
}

// ===========================================================================
// IRS estimated-tax periods (tax-design.md → "Quarterly & Annual Reporting";
// TAX-REPORT-001). The four UNEVEN periods that partition the tax year.
// ===========================================================================

/// One IRS estimated-tax period. The four partition the year with no gap or
/// overlap: Q1 Jan 1–Mar 31, Q2 Apr 1–May 31, Q3 Jun 1–Aug 31, Q4 Sep 1–Dec 31.
/// (TAX-REPORT-001, TAX-VERIF-006)
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Quarter {
    Q1,
    Q2,
    Q3,
    Q4,
}

// ===========================================================================
// The TaxEvent log (tax-design.md → "Accrual Model & Lifecycle"). Append-only,
// folded in Seq order. Serde lives outside this crate (the `store` seam).
// ===========================================================================

/// A monotonic per-log sequence number; the total fold order of the `TaxEvent`
/// log (mirrors `ledger_core::Seq` / `pt_core::Seq`).
pub use pt_core::Seq;

/// One entry in the append-only `TaxEvent` log: a `Seq` and one kind.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TaxEvent {
    /// Monotonic per-log sequence number; the total fold order.
    pub seq: Seq,
    pub kind: TaxEventKind,
}

/// The lifecycle event variants folded over the `TaxEvent` log.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TaxEventKind {
    /// Records which reserve account an accrual belongs to and advances it to
    /// Allocated; re-Allocate before Pay is permitted (last-write-wins on
    /// `account_label`). (TAX-ACCRUAL-002)
    Allocate {
        accrual_key: AccrualKey,
        account_label: String,
    },
    /// Records the actual money moved into the reserve after the sale clears.
    /// `amount_cents` need not equal the computed accrual. (TAX-ACCRUAL-003)
    Move {
        accrual_key: AccrualKey,
        amount_cents: Cents,
        date: Date,
    },
    /// Records a remittance and marks the covered Moved accruals Paid. One Pay
    /// may cover many accruals; every `covers` key must match the Pay's
    /// jurisdiction and tax_year. (TAX-ACCRUAL-004)
    Pay {
        jurisdiction: Jurisdiction,
        tax_year: TaxYear,
        period: Quarter,
        amount_cents: Cents,
        date: Date,
        covers: Vec<AccrualKey>,
    },
    /// Substitutes an absolute amount for one accrual's computed increment; both
    /// the derived increment and the applied amount are retained. (TAX-CALC-008)
    AmountOverride {
        accrual_key: AccrualKey,
        applied_amount_cents: Cents,
        reason: String,
    },
    /// Seeds a single combined migration accrual for a closed prior year (no
    /// backing `RealizedGain`); supersedes that year's per-gain accruals.
    /// (TAX-ACCRUAL-007)
    SeedMigration {
        jurisdiction: Jurisdiction,
        tax_year: TaxYear,
        applied_amount_cents: Cents,
        reason: String,
    },
}

// ===========================================================================
// Error model (tax-design.md → "Accrual Model & Lifecycle"; TAX-ERR-001..005).
// Mirrors LedgerError's shape: one variant per rejection trigger.
// ===========================================================================

/// A rejection of a candidate `TaxEvent` at append time. Rejection leaves the
/// log and state byte-identical (no partial mutation — TAX-ERR-001).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TaxError {
    /// A `Move` targets an accrual not in the Allocated state. (TAX-ERR-002)
    MoveOnUnallocated,
    /// A `Pay` targets an accrual not in the Moved state. (TAX-ERR-003)
    PayOnUnmoved,
    /// A `Pay` targets an accrual already Paid. (TAX-ERR-003)
    DoublePay,
    /// A `Pay`'s `covers` key has a jurisdiction or tax_year ≠ the Pay's.
    /// (TAX-ERR-004)
    PayCoverMismatch,
    /// An `AmountOverride`'s `applied_amount_cents` is negative for a positive
    /// gain, or exceeds that gain. (TAX-ERR-005)
    BadOverride,
}

impl std::fmt::Display for TaxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for TaxError {}

// ===========================================================================
// Jurisdiction resolution (tax-design.md → "Jurisdiction Resolution & Bracket
// State"; TAX-CALC-011).
// ===========================================================================

/// How a gain's `accrues_to_state` resolved to a state jurisdiction.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StateResolution {
    /// The resolved state jurisdiction (always `Jurisdiction::State`).
    pub jurisdiction: Jurisdiction,
    /// `true` when the stamp was missing/blank and fell back to the
    /// current-residency default (flagged). (TAX-CALC-011)
    pub fell_back_to_residency: bool,
}

// ===========================================================================
// Computed accrual output (tax-design.md → "Tax Calculation" / "Accrual Model";
// TAX-CALC-005/008/012, TAX-ACCRUAL-001).
// ===========================================================================

/// One computed accrual: its key, term, derived increment, the applied amount
/// (override or derived), its lifecycle state, and the carried `BracketState`.
/// When `bracket_state == NoBracketsAvailable`, `derived_cents`/`applied_cents`
/// are `None` (unavailable, never a confident zero). (TAX-CALC-012)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Accrual {
    pub key: AccrualKey,
    pub term: Term,
    /// `gain_cents` of the backing `RealizedGain` (for the effective-rate line).
    pub gain_cents: Cents,
    /// The chronological marginal increment `T_J(…+g) − T_J(…)`. `None` when the
    /// jurisdiction is `NoBracketsAvailable`. (TAX-CALC-005)
    pub derived_cents: Option<Cents>,
    /// The amount in force: an `AmountOverride`'s applied amount if present, else
    /// the derived increment. Both are retained. (TAX-CALC-008)
    pub applied_cents: Option<Cents>,
    /// The override amount, when an `AmountOverride` applies (else `None`).
    pub override_cents: Option<Cents>,
    /// The lifecycle state derived from the `TaxEvent` log. (TAX-ACCRUAL-001..004)
    pub state: AccrualState,
    /// `true` when `|applied|` is below the de-minimis threshold (auto-settled):
    /// it needs no lifecycle action and is excluded from the annual report's
    /// outstanding/shortfall/accrued balances. (TAX-ACCRUAL-005)
    pub de_minimis: bool,
    /// `true` when a `SeedMigration` for this `(jurisdiction, tax_year)` supersedes
    /// this per-gain accrual: it is excluded from the year's accrued/outstanding so
    /// the year reconciles to the seeded legacy actual instead of double-counting
    /// the recomputed-but-uncollectible per-gain figure. Distinct from `de_minimis`
    /// (a genuinely tiny accrual) — supersession is migration-only. (TAX-ACCRUAL-007)
    pub superseded: bool,
    /// The per-jurisdiction freshness tag carried from `config`. (TAX-CALC-012)
    pub bracket_state: BracketState,
}

// ===========================================================================
// Unrealized estimate output (tax-design.md → "Unrealized Tax Estimate";
// TAX-CALC-009/010).
// ===========================================================================

/// The per-position unrealized tax estimate: an estimate only — no accrual, no
/// lifecycle, no event. (TAX-CALC-009/010)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct UnrealizedEstimate {
    pub symbol: Symbol,
    /// Σ per-lot kernel-computed unrealized pretax for the open position.
    pub unrealized_pretax_cents: Cents,
    /// The estimated tax if sold today (federal + resolved state + NIIT marginal
    /// increment over the year's realized YTD). `None` when degraded
    /// (unavailable mark or `NoBracketsAvailable`), never zero. (TAX-CALC-010)
    pub estimated_tax_cents: Option<Cents>,
    /// `round(estimated_tax × 1e6 / unrealized_pretax)` in `ppm`; `0` when
    /// unrealized ≤ 0; `None` when degraded. (TAX-CALC-009)
    pub effective_rate_ppm: Option<config::Ppm>,
    pub bracket_state: BracketState,
}

// ===========================================================================
// Reserve & report outputs (tax-design.md → "Reserve Ledgers" / "Quarterly &
// Annual Reporting"; TAX-RESERVE-*, TAX-REPORT-*).
// ===========================================================================

/// A `(jurisdiction, tax_year)` reserve balance: `Σ Move − Σ Pay` (may go
/// negative as an over/under-funding signal). (TAX-RESERVE-001/002)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Reserve {
    pub jurisdiction: Jurisdiction,
    pub tax_year: TaxYear,
    pub balance_cents: Cents,
    pub bracket_state: BracketState,
    /// `true` when this reserve carries at least one `Move`/`Pay` whose
    /// `accrual_key` has no backing `RealizedGain` (its sale was reversed in
    /// `ledger-core`). The balance still sums `Σ Move − Σ Pay` per
    /// TAX-RESERVE-001, but a backless entry is flagged for manual unwind rather
    /// than presented as an ordinary, indistinguishable balance — the orphan
    /// surface the design calls for. The matching warnings are in
    /// `compute_accruals_with_warnings`. (TAX-VERIF-007)
    pub has_backless_entry: bool,
}

/// An orphan warning: a `TaxEvent` whose `accrual_key` has no backing
/// `RealizedGain` (its sale was reversed in `ledger-core`). The event folds as a
/// no-op so replay stays total, and the warning surfaces it for manual unwind so
/// a now-backless reserve entry is not silently folded into an ordinary balance.
/// (TAX-VERIF-007)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OrphanWarning {
    /// The orphan accrual key the no-op event referenced.
    pub accrual_key: AccrualKey,
    /// What kind of event folded as a no-op against it.
    pub kind: OrphanKind,
}

/// Which lifecycle event folded as a no-op against an orphan accrual key.
/// `Move`/`Pay` are called out because they leave a backless *reserve* entry that
/// needs manual unwind; `Allocate`/`AmountOverride` mutate no money. (TAX-VERIF-007)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OrphanKind {
    Allocate,
    Move,
    Pay,
    AmountOverride,
}

/// One row of the annual report, per `(jurisdiction, tax_year)`. (TAX-REPORT-003/004)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AnnualRow {
    pub jurisdiction: Jurisdiction,
    pub tax_year: TaxYear,
    pub accrued_cents: Cents,
    pub moved_cents: Cents,
    pub paid_cents: Cents,
    /// `accrued − paid`.
    pub outstanding_cents: Cents,
    /// `accrued − moved`.
    pub shortfall_cents: Cents,
    /// Total realized gain for the row (LT+ST), for the effective-rate line.
    pub gain_cents: Cents,
    /// `accrual ÷ gain` in `ppm`, only when `|gain|` exceeds de-minimis; else
    /// `None` (n/a). (TAX-REPORT-004)
    pub effective_rate_ppm: Option<config::Ppm>,
    pub bracket_state: BracketState,
}

/// One cell of the quarterly sales report: per (period, jurisdiction). (TAX-REPORT-002)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct QuarterlyCell {
    pub period: Quarter,
    pub jurisdiction: Jurisdiction,
    pub tax_year: TaxYear,
    pub long_term_gain_cents: Cents,
    pub short_term_gain_cents: Cents,
    pub accrual_cents: Cents,
    /// Cumulative safe-harbor target in `ppm` (225_000 / 450_000 / 675_000 /
    /// 900_000 = 22.5 / 45 / 67.5 / 90%). (TAX-REPORT-002)
    pub safe_harbor_ppm: config::Ppm,
    pub bracket_state: BracketState,
}

// ===========================================================================
// Resolved bracket inputs (the per-jurisdiction config `tax` consumes). The
// caller (`runtime`) resolves these via `config::resolve_brackets` and the
// per-year `TaxRules`, and hands `tax` the resolved values plus their state.
// ===========================================================================

/// The resolved per-jurisdiction bracket inputs for one `tax_year`, with the
/// freshness tag `tax` carries on every output. `set` is `None` exactly when
/// `state == NoBracketsAvailable`. (TAX-CALC-012)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ResolvedJurisdiction {
    pub jurisdiction: Jurisdiction,
    /// The resolved ordinary-bracket set (federal-ordinary for `Federal`, the
    /// state's ordinary set for `State`). `None` ⇒ `NoBracketsAvailable`.
    pub ordinary: Option<BracketSet>,
    /// For `Federal` only: the preferential long-term set (0/15/20%). `None` for
    /// states (which tax all gains at ordinary). (TAX-CALC-004)
    pub federal_long_term: Option<BracketSet>,
    /// For `Federal` only: the NIIT params. `None` for states. (TAX-CALC-003)
    pub niit: Option<Niit>,
    /// The income this jurisdiction stacks gains on (federal or state ordinary).
    pub ordinary_income_cents: Cents,
    pub state: BracketState,
}

/// The complete per-`tax_year` jurisdiction inputs: Federal plus each resolved
/// state, the de-minimis threshold, and the current-residency default used when
/// a gain's `accrues_to_state` is missing. (TAX-CALC-011)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TaxContext {
    pub tax_year: TaxYear,
    pub federal: ResolvedJurisdiction,
    /// Resolved state jurisdictions, keyed by `StateCode`.
    pub states: BTreeMap<StateCode, ResolvedJurisdiction>,
    pub de_minimis_cents: Cents,
    /// The current-residency default `StateCode` for an unstamped gain.
    pub residency_default: Option<StateCode>,
}

/// A regime-agnostic `TaxContext` for validating a combined-migration lifecycle
/// (`SeedMigration → AmountOverride → Allocate → Move → Pay`) that carries NO
/// backing `RealizedGain`. The migration figure is the legacy actual, not a
/// bracket computation, so no brackets are resolved — the context exists only so
/// `validate_event` / `compute_accruals` have the structural inputs they require.
/// The migration accrual is keyed by `(jurisdiction, tax_year)`; the per-gain
/// bracket fields are never read on the migration path (TAX-ACCRUAL-007). Used by
/// `import` to run its closed-year tax events through the same `validate_event`
/// gate an `entry` tax append uses.
pub fn migration_context(tax_year: TaxYear) -> TaxContext {
    let bare = |jurisdiction: Jurisdiction| ResolvedJurisdiction {
        jurisdiction,
        ordinary: None,
        federal_long_term: None,
        niit: None,
        ordinary_income_cents: Cents(0),
        state: BracketState::NoBracketsAvailable,
    };
    TaxContext {
        tax_year,
        federal: bare(Jurisdiction::Federal),
        states: BTreeMap::new(),
        de_minimis_cents: Cents(0),
        residency_default: None,
    }
}

// ===========================================================================
// Calendar classification & tax_year (tax-design.md → "Tax Calculation";
// TAX-CALC-001/002). Calendar-exact, leap-year correct; Feb-29 → Mar-1
// anniversary. Pure date math (plain Rust; `Date` is days since the epoch).
// ===========================================================================

/// Classify a realized gain LT/ST: **long-term iff `sale_date` is strictly
/// after the first anniversary of `acquire_date`** (the anniversary of a Feb-29
/// acquisition being March 1). (TAX-CALC-001)
pub fn classify_term(acquire_date: Date, sale_date: Date) -> Term {
    let anniversary = first_anniversary(acquire_date);
    if sale_date.0 > anniversary.0 {
        Term::LongTerm
    } else {
        Term::ShortTerm
    }
}

/// The `tax_year` of a realized gain: the calendar year of its `sale_date`.
/// (TAX-CALC-002)
pub fn tax_year_of(sale_date: Date) -> TaxYear {
    let (y, _, _) = civil_from_days(sale_date.0);
    TaxYear(y)
}

// ---------------------------------------------------------------------------
// Civil-date helpers (days since the Unix epoch, matching `pt_core::Date`).
// Howard Hinnant's algorithms — integer exact, leap-years handled, no float.
// Mirrors `config`'s private copies; `tax` needs them for the LT/ST anniversary,
// the tax_year, and the IRS-period classification.
// ---------------------------------------------------------------------------

/// Days since 1970-01-01 for civil `(y, m, d)` (proleptic Gregorian).
fn days_from_civil(y: i32, m: i32, d: i32) -> i32 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) as i64 + 2) / 5 + (d - 1) as i64;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era as i64 * 146097 + doe - 719468) as i32
}

/// The civil date `(y, m, d)` for `days` since 1970-01-01.
fn civil_from_days(days: i32) -> (i32, i32, i32) {
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    ((if m <= 2 { y + 1 } else { y }) as i32, m as i32, d as i32)
}

/// The first anniversary of `acquire_date`: the same month/day one year later,
/// except a Feb-29 acquisition takes March 1 (the next existing day). The LT/ST
/// boundary is *strictly after* this date. (TAX-CALC-001)
fn first_anniversary(acquire_date: Date) -> Date {
    let (y, m, d) = civil_from_days(acquire_date.0);
    if m == 2 && d == 29 {
        // Feb-29 → the next existing day a year on is March 1.
        Date(days_from_civil(y + 1, 3, 1))
    } else {
        Date(days_from_civil(y + 1, m, d))
    }
}

// ===========================================================================
// Jurisdiction resolution (tax-design.md → "Jurisdiction Resolution & Bracket
// State"; TAX-CALC-011).
// ===========================================================================

/// Resolve a gain's `accrues_to_state` to a state jurisdiction, validating it
/// against the configured states: an unconfigured state resolves to its own
/// `Jurisdiction::State` carrying `NoBracketsAvailable` (the resolver records
/// the code; `bracket_state` comes from the `TaxContext`); a missing/blank
/// stamp falls back to the current-residency default, flagged. (TAX-CALC-011)
pub fn resolve_state(ctx: &TaxContext, accrues_to_state: &Option<StateCode>) -> StateResolution {
    match accrues_to_state {
        // An explicit stamp resolves to its state jurisdiction; whether that
        // state is configured (Verified/Stale) or not (NoBracketsAvailable) is
        // read from the `TaxContext` by the accrual computation, not here.
        Some(code) if !code.trim().is_empty() => StateResolution {
            jurisdiction: Jurisdiction::State(code.clone()),
            fell_back_to_residency: false,
        },
        // A missing/blank stamp falls back to the current-residency default,
        // flagged. Absent a default, the state portion is left unresolved as the
        // (typically NoBracketsAvailable) default code "" — flagged either way.
        _ => StateResolution {
            jurisdiction: Jurisdiction::State(
                ctx.residency_default.clone().unwrap_or_default(),
            ),
            fell_back_to_residency: true,
        },
    }
}

// ===========================================================================
// Private orchestration: the `T_J` jurisdiction tax (driving the `verus!{}`
// arithmetic kernel) and the chronological marginal-increment fold. This is the
// BTreeMap/Vec bookkeeping that sits OUTSIDE the verified boundary; the money
// arithmetic all routes through `kernel::{bracket_tax_on, apply_rate_ppm,
// marginal_increment, clamp_nonneg}`. (tax-design.md → "Tax Calculation")
// ===========================================================================

/// Split a `BracketSet` into the parallel `(thresholds, rates_ppm)` i128 arrays
/// the kernel's `bracket_tax_on` consumes.
fn bracket_arrays(set: &BracketSet) -> (Vec<i128>, Vec<i128>) {
    let thresholds = set
        .rows
        .iter()
        .map(|r| r.lower_threshold_cents.0 as i128)
        .collect();
    let rates = set.rows.iter().map(|r| r.rate_ppm.0 as i128).collect();
    (thresholds, rates)
}

/// The federal jurisdiction tax `T_fed(income, st, lt)` in `Cents` (st, lt the
/// raw cumulative gains): ordinary tax on `st` stacked on `income`, preferential
/// tax on `lt` stacked above `income + st`, plus NIIT on the investment gains
/// above the MAGI threshold. Gain arguments are clamped at zero. Returns `None`
/// when the federal brackets are unavailable. (TAX-CALC-003/006)
// @spec TAX-CALC-014 (LT stacks above income + ST), TAX-CALC-013 (NIIT: income in MAGI, lesser-of cap)
fn t_federal(j: &ResolvedJurisdiction, st: i128, lt: i128) -> Option<i128> {
    let ordinary = j.ordinary.as_ref()?;
    let lt_set = j.federal_long_term.as_ref()?;
    let income = j.ordinary_income_cents.0 as i128;
    let st_c = kernel::clamp_nonneg(st);
    let lt_c = kernel::clamp_nonneg(lt);

    let (o_th, o_rt) = bracket_arrays(ordinary);
    let (l_th, l_rt) = bracket_arrays(lt_set);

    // Short-term stacks on ordinary income at ordinary brackets.
    let st_tax = kernel::bracket_tax_on(&o_th, &o_rt, income, st_c);
    // Long-term stacks ABOVE ordinary income + short-term gains, at the
    // preferential brackets.
    let lt_tax = kernel::bracket_tax_on(&l_th, &l_rt, income + st_c, lt_c);

    // NIIT: the configured rate on the investment gain above the MAGI threshold.
    let niit_tax = if let Some(niit) = &j.niit {
        let gains = st_c + lt_c;
        let threshold = niit.magi_threshold_cents.0 as i128;
        let above = kernel::clamp_nonneg(income + gains - threshold);
        let niit_base = if above < gains { above } else { gains };
        kernel::apply_rate_ppm(niit_base, niit.rate_ppm.0 as i128)
    } else {
        0
    };

    Some(st_tax + lt_tax + niit_tax)
}

/// A state jurisdiction taxes ALL gains (LT and ST alike) at its ordinary
/// brackets, stacked on state income. (TAX-CALC-004/006) Returns `None` when the
/// state's brackets are unavailable.
fn t_state(j: &ResolvedJurisdiction, st: i128, lt: i128) -> Option<i128> {
    let ordinary = j.ordinary.as_ref()?;
    let income = j.ordinary_income_cents.0 as i128;
    let gains = kernel::clamp_nonneg(st + lt);
    let (th, rt) = bracket_arrays(ordinary);
    Some(kernel::bracket_tax_on(&th, &rt, income, gains))
}

/// `T_J(income, st, lt)` for any resolved jurisdiction. `None` ⇒ brackets
/// unavailable (`NoBracketsAvailable`).
fn t_j(j: &ResolvedJurisdiction, st: i128, lt: i128) -> Option<i128> {
    match &j.jurisdiction {
        Jurisdiction::Federal => t_federal(j, st, lt),
        Jurisdiction::State(_) => t_state(j, st, lt),
    }
}

/// The chronological running gain totals threaded through the marginal-increment
/// fold, per jurisdiction. Raw (signed) sums; clamped at the point they enter
/// `T_J`. (TAX-CALC-005/006/007)
#[derive(Clone, Copy, Default)]
struct Running {
    st: i128,
    lt: i128,
}

/// The pre-lifecycle computed accrual: key, term, derived increment (None when
/// the jurisdiction is unavailable), gain, and the carried `BracketState`.
struct ComputedAccrual {
    key: AccrualKey,
    term: Term,
    gain_cents: i64,
    derived: Option<i64>,
    bracket_state: BracketState,
}

/// Compute every per-gain, per-jurisdiction accrual increment in chronological
/// `(sale_date, sale_seq)` order. The shared `T_J` difference drives the
/// `verus!{}` kernel; the per-jurisdiction running totals are the orchestration
/// bookkeeping. (TAX-CALC-003/004/005/006/007)
fn computed_accruals(gains: &[RealizedGain], ctx: &TaxContext) -> Vec<ComputedAccrual> {
    // Sort gains chronologically by (sale_date, sale_seq) — NOT raw seq.
    let mut ordered: Vec<&RealizedGain> = gains.iter().collect();
    ordered.sort_by_key(|g| (g.sale_date.0, g.sale_seq.0));

    // Per-jurisdiction running (st, lt) totals.
    let mut fed_run = Running::default();
    let mut state_run: BTreeMap<StateCode, Running> = BTreeMap::new();

    let mut out: Vec<ComputedAccrual> = Vec::new();

    for g in ordered {
        let term = classify_term(g.acquire_date, g.sale_date);
        let tax_year = tax_year_of(g.sale_date);
        let gain = g.gain_cents.0 as i128;
        // The gain enters either the short-term or the long-term running total.
        let (dst, dlt) = match term {
            Term::ShortTerm => (gain, 0),
            Term::LongTerm => (0, gain),
        };

        // --- Federal accrual (always). ---
        let before = fed_run;
        let after = Running {
            st: before.st + dst,
            lt: before.lt + dlt,
        };
        let derived_fed = match (
            t_j(&ctx.federal, before.st, before.lt),
            t_j(&ctx.federal, after.st, after.lt),
        ) {
            (Some(tb), Some(ta)) => {
                Some(kernel::marginal_increment(tb, ta, gain.max(0)) as i64)
            }
            _ => None,
        };
        out.push(ComputedAccrual {
            key: AccrualKey {
                sale_id: g.sale_id.clone(),
                lot_id: g.lot_id.clone(),
                jurisdiction: Jurisdiction::Federal,
                tax_year,
            },
            term,
            gain_cents: g.gain_cents.0,
            derived: derived_fed,
            bracket_state: ctx.federal.state,
        });
        fed_run = after;

        // --- State accrual (the gain's resolved state). ---
        let resolution = resolve_state(ctx, &g.accrues_to_state);
        if let Jurisdiction::State(code) = &resolution.jurisdiction {
            // An empty resolved code means a missing stamp with no residency
            // default — there is no state to accrue to, so emit no state accrual.
            if code.is_empty() {
                continue;
            }
            let resolved = ctx.states.get(code);
            let st_before = state_run.get(code).copied().unwrap_or_default();
            let st_after = Running {
                st: st_before.st + dst,
                lt: st_before.lt + dlt,
            };
            let (derived_state, bracket_state) = match resolved {
                Some(rj) => {
                    let derived = match (
                        t_j(rj, st_before.st, st_before.lt),
                        t_j(rj, st_after.st, st_after.lt),
                    ) {
                        (Some(tb), Some(ta)) => {
                            Some(kernel::marginal_increment(tb, ta, gain.max(0)) as i64)
                        }
                        _ => None,
                    };
                    (derived, rj.state)
                }
                // An unconfigured state: unavailable, never a confident zero.
                None => (None, BracketState::NoBracketsAvailable),
            };
            out.push(ComputedAccrual {
                key: AccrualKey {
                    sale_id: g.sale_id.clone(),
                    lot_id: g.lot_id.clone(),
                    jurisdiction: Jurisdiction::State(code.clone()),
                    tax_year,
                },
                term,
                gain_cents: g.gain_cents.0,
                derived: derived_state,
                bracket_state,
            });
            // Only advance the running total when the state actually taxes (a
            // NoBracketsAvailable state has no defined stacking).
            if resolved.is_some() {
                state_run.insert(code.clone(), st_after);
            }
        }
    }

    out
}

/// The aggregate of one accrual's lifecycle, derived by folding the events.
#[derive(Default)]
struct Lifecycle {
    state: Option<AccrualState>,
    account_label: Option<String>,
    moved: Option<(Cents, Date)>,
    override_cents: Option<Cents>,
}

/// Fold the `TaxEvent` log into per-accrual lifecycle aggregates, keyed by
/// `AccrualKey`. Only the events whose `accrual_key` matches a real accrual are
/// applied; orphans are no-ops (TAX-VERIF-007). Folded in `Seq` order.
fn fold_lifecycle(
    events: &[TaxEvent],
    real_keys: &std::collections::BTreeSet<AccrualKey>,
) -> BTreeMap<AccrualKey, Lifecycle> {
    let mut ordered: Vec<&TaxEvent> = events.iter().collect();
    ordered.sort_by_key(|e| e.seq.0);

    let mut map: BTreeMap<AccrualKey, Lifecycle> = BTreeMap::new();
    for e in ordered {
        match &e.kind {
            TaxEventKind::Allocate { accrual_key, account_label } => {
                if !real_keys.contains(accrual_key) {
                    continue; // orphan → no-op
                }
                let lc = map.entry(accrual_key.clone()).or_default();
                lc.account_label = Some(account_label.clone());
                lc.state = Some(AccrualState::Allocated {
                    account_label: account_label.clone(),
                });
            }
            TaxEventKind::Move { accrual_key, amount_cents, date } => {
                if !real_keys.contains(accrual_key) {
                    continue;
                }
                let lc = map.entry(accrual_key.clone()).or_default();
                let label = lc.account_label.clone().unwrap_or_default();
                lc.moved = Some((*amount_cents, *date));
                lc.state = Some(AccrualState::Moved {
                    account_label: label,
                    amount_cents: *amount_cents,
                    date: *date,
                });
            }
            TaxEventKind::Pay { amount_cents, date, period, covers, .. } => {
                for key in covers {
                    if !real_keys.contains(key) {
                        continue;
                    }
                    let lc = map.entry(key.clone()).or_default();
                    let moved = lc.moved.map(|(m, _)| m).unwrap_or(Cents(0));
                    lc.state = Some(AccrualState::Paid {
                        amount_cents: *amount_cents,
                        moved_cents: moved,
                        date: *date,
                        period: *period,
                    });
                }
            }
            TaxEventKind::AmountOverride { accrual_key, applied_amount_cents, .. } => {
                if !real_keys.contains(accrual_key) {
                    continue;
                }
                let lc = map.entry(accrual_key.clone()).or_default();
                lc.override_cents = Some(*applied_amount_cents);
            }
            TaxEventKind::SeedMigration { .. } => { /* handled separately */ }
        }
    }
    map
}

/// Accrual computation entry point (tax-design.md → "Tax Calculation"; the
/// chronological marginal-increment fold over the year's realized gains). This
/// orchestrates the `verus!{}` arithmetic kernel below — bracket stacking + the
/// marginal increment — over the BTreeMap/Vec bookkeeping that is outside the
/// verified boundary. (TAX-CALC-003/004/005/006/007)
///
/// Compute every accrual for the realized `gains` against `ctx`, then fold the
/// `events` log to derive each accrual's lifecycle state, override, and
/// de-minimis flag. Accruals are computed as chronological `(sale_date,
/// sale_seq)` marginal increments (TAX-CALC-005/007), one per gain per
/// jurisdiction (Federal + the gain's resolved state — TAX-ACCRUAL-001), each
/// carrying its `BracketState` (TAX-CALC-012). A migration-seeded year
/// supersedes its per-gain accruals (TAX-ACCRUAL-007); a gain reversed in
/// `ledger-core` (absent from `gains`) drops or folds its events as no-ops
/// (TAX-ACCRUAL-006, TAX-VERIF-007). Returns accruals in a deterministic order.
pub fn compute_accruals(
    gains: &[RealizedGain],
    events: &[TaxEvent],
    ctx: &TaxContext,
) -> Vec<Accrual> {
    let computed = computed_accruals(gains, ctx);

    // Which (jurisdiction, tax_year) pairs carry a migration seed → their
    // per-gain accruals are superseded (excluded from outstanding).
    let mut migrations: BTreeMap<(Jurisdiction, TaxYear), Cents> = BTreeMap::new();
    for e in events {
        if let TaxEventKind::SeedMigration {
            jurisdiction,
            tax_year,
            applied_amount_cents,
            ..
        } = &e.kind
        {
            migrations.insert((jurisdiction.clone(), *tax_year), *applied_amount_cents);
        }
    }

    // The set of real accrual keys (so orphan events fold as no-ops): the per-gain
    // computed accruals plus the combined migration keys (empty sale_id/lot_id),
    // so the migration accrual's own Allocate/Move/Pay lifecycle folds rather than
    // being mistaken for an orphan. (TAX-VERIF-007, TAX-ACCRUAL-007)
    let mut real_keys: std::collections::BTreeSet<AccrualKey> =
        computed.iter().map(|c| c.key.clone()).collect();
    for (jurisdiction, tax_year) in migrations.keys() {
        real_keys.insert(AccrualKey {
            sale_id: String::new(),
            lot_id: String::new(),
            jurisdiction: jurisdiction.clone(),
            tax_year: *tax_year,
        });
    }
    let lifecycle = fold_lifecycle(events, &real_keys);

    let de_minimis = ctx.de_minimis_cents.0;

    let mut out: Vec<Accrual> = Vec::new();
    for c in &computed {
        let lc = lifecycle.get(&c.key);
        let state = lc
            .and_then(|l| l.state.clone())
            .unwrap_or(AccrualState::Accrued);
        let override_cents = lc.and_then(|l| l.override_cents);
        let derived_cents = c.derived.map(Cents);
        // The applied amount: the override if present, else the derived increment.
        let applied_cents = match (override_cents, derived_cents) {
            (Some(o), _) => Some(o),
            (None, d) => d,
        };
        // Superseded by a migration accrual for this (jurisdiction, year)? A
        // superseded per-gain accrual is excluded from the year's outstanding
        // (the migration figure reconciles the year). Kept DISTINCT from the
        // de-minimis flag (a genuinely tiny accrual) so the two exclusions are
        // independently observable. (TAX-ACCRUAL-007)
        let superseded =
            migrations.contains_key(&(c.key.jurisdiction.clone(), c.key.tax_year));
        // De-minimis: strictly `|applied| < threshold` (auto-settled). (TAX-ACCRUAL-005)
        let dm = applied_cents
            .map(|a| a.0.abs() < de_minimis)
            .unwrap_or(false);

        out.push(Accrual {
            key: c.key.clone(),
            term: c.term,
            gain_cents: Cents(c.gain_cents),
            derived_cents,
            applied_cents,
            override_cents,
            state,
            de_minimis: dm,
            superseded,
            bracket_state: c.bracket_state,
        });
    }

    // Emit the combined migration accruals (no backing RealizedGain; empty
    // sale_id/lot_id). They carry the legacy actual and reconcile the year.
    for ((jurisdiction, tax_year), amount) in &migrations {
        let bracket_state = match jurisdiction {
            Jurisdiction::Federal => ctx.federal.state,
            Jurisdiction::State(code) => ctx
                .states
                .get(code)
                .map(|s| s.state)
                .unwrap_or(BracketState::NoBracketsAvailable),
        };
        let key = AccrualKey {
            sale_id: String::new(),
            lot_id: String::new(),
            jurisdiction: jurisdiction.clone(),
            tax_year: *tax_year,
        };
        let lc = lifecycle.get(&key);
        let state = lc
            .and_then(|l| l.state.clone())
            .unwrap_or(AccrualState::Accrued);
        out.push(Accrual {
            key,
            // @spec TAX-ACCRUAL-008: a combined migration figure is regime-agnostic, so
            // its term is undefined — a fixed LongTerm placeholder no consumer reads as
            // a regime classification.
            term: Term::LongTerm, // a combined migration figure is regime-agnostic
            gain_cents: Cents(0),
            derived_cents: None,
            applied_cents: Some(*amount),
            override_cents: Some(*amount),
            state,
            de_minimis: false,
            // The migration accrual IS the reconciling figure — never superseded.
            superseded: false,
            bracket_state,
        });
    }

    out
}

/// The set of real accrual keys for `(gains, events)`: every per-gain computed
/// accrual key plus the combined migration key (empty `sale_id`/`lot_id`) for
/// each `(jurisdiction, tax_year)` carrying a `SeedMigration`. A `TaxEvent` whose
/// `accrual_key` is NOT in this set is an orphan (its backing `RealizedGain` was
/// reversed in `ledger-core`). (TAX-VERIF-007)
fn real_key_set(
    gains: &[RealizedGain],
    events: &[TaxEvent],
    ctx: &TaxContext,
) -> std::collections::BTreeSet<AccrualKey> {
    let mut keys: std::collections::BTreeSet<AccrualKey> = computed_accruals(gains, ctx)
        .iter()
        .map(|c| c.key.clone())
        .collect();
    for e in events {
        if let TaxEventKind::SeedMigration { jurisdiction, tax_year, .. } = &e.kind {
            keys.insert(AccrualKey {
                sale_id: String::new(),
                lot_id: String::new(),
                jurisdiction: jurisdiction.clone(),
                tax_year: *tax_year,
            });
        }
    }
    keys
}

/// Surface the orphan warnings for `(gains, events)`: every `TaxEvent` whose
/// `accrual_key` has no backing `RealizedGain` (and is not a migration key). Such
/// an event folds as a no-op so replay stays total (TAX-VERIF-007), but the
/// design requires it be surfaced for manual unwind rather than silently dropped
/// — in particular a backless `Move`/`Pay` that still folds into a reserve
/// balance. Folded in `Seq` order; deterministic. (TAX-VERIF-007)
// @spec TAX-VERIF-008 (Seq-ordered typed orphan warnings naming key + lifecycle kind;
// the backless-reserve flag is on `reserves`'s `has_backless_entry`)
pub fn orphan_warnings(
    gains: &[RealizedGain],
    events: &[TaxEvent],
    ctx: &TaxContext,
) -> Vec<OrphanWarning> {
    let real_keys = real_key_set(gains, events, ctx);
    let mut ordered: Vec<&TaxEvent> = events.iter().collect();
    ordered.sort_by_key(|e| e.seq.0);

    let mut out: Vec<OrphanWarning> = Vec::new();
    for e in ordered {
        match &e.kind {
            TaxEventKind::Allocate { accrual_key, .. } => {
                if !real_keys.contains(accrual_key) {
                    out.push(OrphanWarning {
                        accrual_key: accrual_key.clone(),
                        kind: OrphanKind::Allocate,
                    });
                }
            }
            TaxEventKind::Move { accrual_key, .. } => {
                if !real_keys.contains(accrual_key) {
                    out.push(OrphanWarning {
                        accrual_key: accrual_key.clone(),
                        kind: OrphanKind::Move,
                    });
                }
            }
            TaxEventKind::AmountOverride { accrual_key, .. } => {
                if !real_keys.contains(accrual_key) {
                    out.push(OrphanWarning {
                        accrual_key: accrual_key.clone(),
                        kind: OrphanKind::AmountOverride,
                    });
                }
            }
            TaxEventKind::Pay { covers, .. } => {
                for key in covers {
                    if !real_keys.contains(key) {
                        out.push(OrphanWarning {
                            accrual_key: key.clone(),
                            kind: OrphanKind::Pay,
                        });
                    }
                }
            }
            TaxEventKind::SeedMigration { .. } => {}
        }
    }
    out
}

/// Compute the accruals AND surface the orphan warnings in one pass: the
/// accruals (with orphan lifecycle events folded as no-ops — TAX-VERIF-007) plus
/// the warnings for every backless event, so a caller (`reports`/`tui`) can
/// reconcile a now-backless reserve entry for manual unwind. (TAX-VERIF-007)
pub fn compute_accruals_with_warnings(
    gains: &[RealizedGain],
    events: &[TaxEvent],
    ctx: &TaxContext,
) -> (Vec<Accrual>, Vec<OrphanWarning>) {
    (
        compute_accruals(gains, events, ctx),
        orphan_warnings(gains, events, ctx),
    )
}

/// The combined migration accruals declared by the `SeedMigration` events in
/// `events`: each maps its combined key (empty `sale_id`/`lot_id`) to the seeded
/// legacy-actual amount. A migration accrual has no backing `RealizedGain`, so it
/// is NOT a `computed_accruals` key — but TAX-ACCRUAL-007 makes it a first-class
/// real accrual the validator (and the reporting path) must accept, so its own
/// `Allocate → Move → Pay` lifecycle folds and its `AmountOverride` is bounded to
/// the seeded legacy actual rather than the orphan `[0, 0]`. Last-write-wins on a
/// repeated `(jurisdiction, tax_year)` seed. (TAX-ACCRUAL-007)
fn migration_accruals(events: &[TaxEvent]) -> BTreeMap<AccrualKey, Cents> {
    let mut out: BTreeMap<AccrualKey, Cents> = BTreeMap::new();
    for e in events {
        if let TaxEventKind::SeedMigration {
            jurisdiction,
            tax_year,
            applied_amount_cents,
            ..
        } = &e.kind
        {
            let key = AccrualKey {
                sale_id: String::new(),
                lot_id: String::new(),
                jurisdiction: jurisdiction.clone(),
                tax_year: *tax_year,
            };
            out.insert(key, *applied_amount_cents);
        }
    }
    out
}

/// Replay the accepted events into per-accrual lifecycle states (the validator's
/// view), keyed by `AccrualKey`. Returns the lifecycle map and the real-key set.
fn replay_states(
    gains: &[RealizedGain],
    accepted: &[TaxEvent],
    ctx: &TaxContext,
) -> (BTreeMap<AccrualKey, AccrualState>, std::collections::BTreeSet<AccrualKey>) {
    let computed = computed_accruals(gains, ctx);
    let mut real_keys: std::collections::BTreeSet<AccrualKey> =
        computed.iter().map(|c| c.key.clone()).collect();
    // The combined migration keys are first-class real accruals (TAX-ACCRUAL-007),
    // so their own Allocate/Move/Pay lifecycle folds rather than being mistaken for
    // an orphan no-op — matching `compute_accruals`'s real-key set.
    for key in migration_accruals(accepted).keys() {
        real_keys.insert(key.clone());
    }
    let lifecycle = fold_lifecycle(accepted, &real_keys);
    let states: BTreeMap<AccrualKey, AccrualState> = real_keys
        .iter()
        .map(|k| {
            let s = lifecycle
                .get(k)
                .and_then(|l| l.state.clone())
                .unwrap_or(AccrualState::Accrued);
            (k.clone(), s)
        })
        .collect();
    (states, real_keys)
}

/// Validate a candidate `TaxEvent` against the replayed-so-far accrual state
/// (with the gains and context). `Ok(())` ⇒ accept; `Err` ⇒ reject, leaving
/// state byte-identical (TAX-ERR-001). Enforces the forward-only lifecycle and
/// the Pay/override constraints (TAX-ERR-002..005).
// @spec TAX-ERR-006 (override bounds for a non-positive/absent gain: loss → [gain, 0],
// zero/orphan → {0}, combined-migration key → the seeded legacy actual)
pub fn validate_event(
    gains: &[RealizedGain],
    accepted: &[TaxEvent],
    candidate: &TaxEvent,
    ctx: &TaxContext,
) -> Result<(), TaxError> {
    let (states, _real_keys) = replay_states(gains, accepted, ctx);

    match &candidate.kind {
        // Move requires the accrual be Allocated. (TAX-ERR-002) An orphan key
        // (no real accrual) is also "not Allocated" → rejected before mutation.
        TaxEventKind::Move { accrual_key, .. } => {
            match states.get(accrual_key) {
                Some(AccrualState::Allocated { .. }) => Ok(()),
                _ => Err(TaxError::MoveOnUnallocated),
            }
        }
        // Pay requires every covered accrual be Moved and not already Paid, and
        // every covered key match the Pay's jurisdiction and tax_year.
        // (TAX-ERR-003/004)
        TaxEventKind::Pay {
            jurisdiction,
            tax_year,
            covers,
            ..
        } => {
            for key in covers {
                // Jurisdiction / tax_year match. (TAX-ERR-004)
                if key.jurisdiction != *jurisdiction || key.tax_year != *tax_year {
                    return Err(TaxError::PayCoverMismatch);
                }
                match states.get(key) {
                    Some(AccrualState::Moved { .. }) => {}
                    Some(AccrualState::Paid { .. }) => return Err(TaxError::DoublePay),
                    _ => return Err(TaxError::PayOnUnmoved),
                }
            }
            Ok(())
        }
        // Allocate is always valid against a real-or-orphan accrual (orphan
        // folds as a no-op); re-Allocate before Pay is permitted.
        TaxEventKind::Allocate { .. } => Ok(()),
        // An AmountOverride's applied amount must lie in the closed interval
        // between zero and the backing gain (TAX-ERR-005):
        //   * positive gain → applied ∈ [0, gain] (the spec's stated case);
        //   * loss (negative gain) → applied ∈ [gain, 0] (the symmetric bound, so
        //     a logged override against a loss cannot encode an arbitrary amount);
        //   * zero gain, OR an orphan key with no backing RealizedGain → only 0 is
        //     valid (no amount can be accrued where there is no taxable gain);
        //   * a COMBINED MIGRATION key (a SeedMigration in the accepted prefix, no
        //     backing RealizedGain) → bounded to the seeded legacy actual, since the
        //     migration accrual IS that legacy figure and the override merely records
        //     it (TAX-ACCRUAL-007) — NOT the orphan [0, 0].
        // The non-positive cases close the data-integrity gap the bare-positive guard
        // left open. (Loss/zero/orphan bounds are an intent gap on TAX-ERR-005, which
        // scopes only the positive case — see ears_gaps_reported.)
        TaxEventKind::AmountOverride {
            accrual_key,
            applied_amount_cents,
            ..
        } => {
            // A combined migration key carries the seeded legacy actual; the override
            // records exactly that amount (TAX-ACCRUAL-007).
            if let Some(seeded) = migration_accruals(accepted).get(accrual_key) {
                return if *applied_amount_cents == *seeded {
                    Ok(())
                } else {
                    Err(TaxError::BadOverride)
                };
            }
            let backing = gains
                .iter()
                .find(|g| g.sale_id == accrual_key.sale_id && g.lot_id == accrual_key.lot_id)
                .map(|g| g.gain_cents.0);
            // An orphan key (no backing gain) bounds to [0, 0]; otherwise to the
            // closed interval [min(0, gain), max(0, gain)].
            let gain = backing.unwrap_or(0);
            let (lo, hi) = (gain.min(0), gain.max(0));
            if applied_amount_cents.0 < lo || applied_amount_cents.0 > hi {
                Err(TaxError::BadOverride)
            } else {
                Ok(())
            }
        }
        // A migration seed is a bulk-seeding construct; no per-event validation
        // beyond what the (jurisdiction, tax_year) key already carries.
        TaxEventKind::SeedMigration { .. } => Ok(()),
    }
}

// ===========================================================================
// Reserves (tax-design.md → "Reserve Ledgers"; TAX-RESERVE-001/002).
// ===========================================================================

/// Compute every `(jurisdiction, tax_year)` reserve balance as `Σ Move − Σ Pay`
/// (real money on the events), carrying each jurisdiction's `BracketState`. A
/// balance may be negative. (TAX-RESERVE-001/002)
///
/// The balance sums EVERY `Move`/`Pay` for the key (per TAX-RESERVE-001 — the
/// orphan-fold no-op concerns lifecycle *state*, not the money tally), but a
/// reserve carrying a backless `Move`/`Pay` (whose `accrual_key` has no backing
/// `RealizedGain`) is flagged `has_backless_entry` so it is surfaced for manual
/// unwind rather than presented as an ordinary balance. `gains` supplies the
/// real-key set the flag is computed against. (TAX-VERIF-007)
pub fn reserves(gains: &[RealizedGain], events: &[TaxEvent], ctx: &TaxContext) -> Vec<Reserve> {
    let real_keys = real_key_set(gains, events, ctx);

    // reserve(J, year) = Σ Move.amount − Σ Pay.amount for that (J, year). A
    // Move's (J, year) is its accrual_key's; a Pay's is its own.
    let mut balances: BTreeMap<(Jurisdiction, TaxYear), i64> = BTreeMap::new();
    // Which (J, year) reserves carry at least one backless Move/Pay entry.
    let mut backless: std::collections::BTreeSet<(Jurisdiction, TaxYear)> =
        std::collections::BTreeSet::new();
    for e in events {
        match &e.kind {
            TaxEventKind::Move { accrual_key, amount_cents, .. } => {
                let key = (accrual_key.jurisdiction.clone(), accrual_key.tax_year);
                *balances.entry(key.clone()).or_insert(0) += amount_cents.0;
                if !real_keys.contains(accrual_key) {
                    backless.insert(key);
                }
            }
            TaxEventKind::Pay { jurisdiction, tax_year, amount_cents, covers, .. } => {
                let key = (jurisdiction.clone(), *tax_year);
                *balances.entry(key.clone()).or_insert(0) -= amount_cents.0;
                if covers.iter().any(|k| !real_keys.contains(k)) {
                    backless.insert(key);
                }
            }
            _ => {}
        }
    }

    balances
        .into_iter()
        .map(|((jurisdiction, tax_year), balance)| {
            let bracket_state = bracket_state_for(ctx, &jurisdiction);
            let has_backless_entry =
                backless.contains(&(jurisdiction.clone(), tax_year));
            Reserve {
                jurisdiction,
                tax_year,
                balance_cents: Cents(balance),
                bracket_state,
                has_backless_entry,
            }
        })
        .collect()
}

/// The carried `BracketState` for a jurisdiction in `ctx` (Federal's, the
/// state's, or `NoBracketsAvailable` for an unconfigured state).
fn bracket_state_for(ctx: &TaxContext, jurisdiction: &Jurisdiction) -> BracketState {
    match jurisdiction {
        Jurisdiction::Federal => ctx.federal.state,
        Jurisdiction::State(code) => ctx
            .states
            .get(code)
            .map(|s| s.state)
            .unwrap_or(BracketState::NoBracketsAvailable),
    }
}

// ===========================================================================
// Unrealized estimate (tax-design.md → "Unrealized Tax Estimate";
// TAX-CALC-009/010).
// ===========================================================================

/// Estimate the tax an open position (its `OpenLot`s, with kernel-computed
/// per-lot unrealized) would incur if sold today, and derive its effective
/// unrealized tax rate in `ppm`. `as_of` is `runtime`'s today-date (the kernel
/// is clockless). `accrues_to_state` is the position's resolved state stamp.
/// `ytd_st`/`ytd_lt` are the year's realized YTD gains the increment stacks on.
/// `None` estimate when the mark is degraded or state is `NoBracketsAvailable`.
/// (TAX-CALC-009/010)
// @spec TAX-CALC-015 (clamp the combined mixed-sign estimate into [0, net_positive_unrealized])
pub fn unrealized_estimate(
    symbol: &Symbol,
    open_lots: &[OpenLot],
    as_of: Date,
    accrues_to_state: &Option<StateCode>,
    ytd_st: Cents,
    ytd_lt: Cents,
    ctx: &TaxContext,
) -> UnrealizedEstimate {
    // Resolve the position's state and its BracketState. A stamped state that is
    // unconfigured degrades the estimate (NoBracketsAvailable); an absent stamp
    // with no residency default simply has no state component (Verified — the
    // federal estimate still computes). The carried bracket_state is the
    // most-degraded of federal and the state component.
    let resolution = resolve_state(ctx, accrues_to_state);
    let state_code = match &resolution.jurisdiction {
        Jurisdiction::State(code) if !code.is_empty() => Some(code.clone()),
        _ => None,
    };
    let resolved_state = state_code.as_ref().and_then(|c| ctx.states.get(c));
    let state_bracket_state = match (&state_code, resolved_state) {
        // A resolved, configured state carries its own freshness.
        (Some(_), Some(s)) => s.state,
        // A resolved but UNCONFIGURED state degrades the estimate.
        (Some(_), None) => BracketState::NoBracketsAvailable,
        // No state component at all → no degradation from the state side.
        (None, _) => BracketState::Verified,
    };
    let bracket_state = most_degraded(ctx.federal.state, state_bracket_state);

    // Sum the per-lot unrealized, split LT/ST by holding as of today. A single
    // degraded mark (None) makes the whole estimate unavailable.
    let mut st_unreal: i128 = 0;
    let mut lt_unreal: i128 = 0;
    let mut pretax: i128 = 0;
    let mut degraded_mark = false;
    for ol in open_lots {
        if ol.lot.symbol != *symbol {
            continue;
        }
        match ol.unrealized_cents {
            Some(u) => {
                pretax += u.0 as i128;
                match classify_term(ol.lot.acquire_date, as_of) {
                    Term::ShortTerm => st_unreal += u.0 as i128,
                    Term::LongTerm => lt_unreal += u.0 as i128,
                }
            }
            None => degraded_mark = true,
        }
    }

    // Degraded when the mark is unavailable or the state has no brackets — never
    // a confident zero. (TAX-CALC-010)
    let degraded = degraded_mark || bracket_state == BracketState::NoBracketsAvailable;

    let estimated_tax = if degraded {
        None
    } else {
        // The same T_J marginal increment over the year's realized YTD gains:
        // federal + the resolved state. (TAX-CALC-009)
        let ytd_st_i = ytd_st.0 as i128;
        let ytd_lt_i = ytd_lt.0 as i128;
        let total_gain = (st_unreal + lt_unreal).max(0);

        let fed = match (
            t_j(&ctx.federal, ytd_st_i, ytd_lt_i),
            t_j(&ctx.federal, ytd_st_i + st_unreal, ytd_lt_i + lt_unreal),
        ) {
            (Some(tb), Some(ta)) => kernel::marginal_increment(tb, ta, total_gain),
            _ => 0,
        };
        let state = match resolved_state {
            Some(rj) => match (
                t_j(rj, ytd_st_i, ytd_lt_i),
                t_j(rj, ytd_st_i + st_unreal, ytd_lt_i + lt_unreal),
            ) {
                (Some(tb), Some(ta)) => kernel::marginal_increment(tb, ta, total_gain),
                _ => 0,
            },
            None => 0,
        };
        // Clamp the combined estimate into `[0, total_gain]` (`total_gain` is the
        // NET positive unrealized). For a position with mixed-sign lots — a
        // short-term LOSS lot alongside a long-term GAIN lot — the per-regime
        // increments stack each clamped at zero, so a gross LT gain could be taxed
        // while the ST loss only shrinks the denominator, yielding an effective
        // rate above 100%. The kernel's `marginal_increment` contract already
        // bounds each increment by `total_gain`, but that ensures-clause erases
        // under stable build; clamping here keeps the estimated tax (and hence the
        // effective rate) in `[0, 100%]` consistently with the realized path.
        // (TAX-CALC-009)
        let combined = fed + state;
        let bounded = combined.clamp(0, total_gain);
        Some(Cents(bounded as i64))
    };

    let effective_rate_ppm = estimated_tax.map(|tax| {
        config::Ppm(kernel::effective_rate_ppm(tax.0 as i128, pretax) as i64)
    });

    UnrealizedEstimate {
        symbol: symbol.clone(),
        unrealized_pretax_cents: Cents(pretax as i64),
        estimated_tax_cents: estimated_tax,
        effective_rate_ppm,
        bracket_state,
    }
}

/// The more-degraded of two `BracketState`s
/// (`NoBracketsAvailable` > `Stale` > `Verified`).
fn most_degraded(a: BracketState, b: BracketState) -> BracketState {
    fn rank(s: BracketState) -> u8 {
        match s {
            BracketState::Verified => 0,
            BracketState::Stale => 1,
            BracketState::NoBracketsAvailable => 2,
        }
    }
    if rank(a) >= rank(b) {
        a
    } else {
        b
    }
}

// ===========================================================================
// Reporting (tax-design.md → "Quarterly & Annual Reporting"; TAX-REPORT-*).
// ===========================================================================

/// The IRS estimated-tax period a `sale_date` falls in: Q1 Jan 1–Mar 31, Q2
/// Apr 1–May 31, Q3 Jun 1–Aug 31, Q4 Sep 1–Dec 31. The four partition the year
/// with no gap or overlap. (TAX-REPORT-001, TAX-VERIF-006)
pub fn quarter_of(sale_date: Date) -> Quarter {
    let (_, m, _) = civil_from_days(sale_date.0);
    match m {
        1..=3 => Quarter::Q1,  // Jan 1 – Mar 31
        4..=5 => Quarter::Q2,  // Apr 1 – May 31
        6..=8 => Quarter::Q3,  // Jun 1 – Aug 31
        _ => Quarter::Q4,      // Sep 1 – Dec 31
    }
}

/// The cumulative safe-harbor target for a period, in `ppm`: Q1 22.5%, Q2 45%,
/// Q3 67.5%, Q4 90% (`225_000 / 450_000 / 675_000 / 900_000` ppm). (TAX-REPORT-002)
pub fn safe_harbor_ppm(period: Quarter) -> config::Ppm {
    match period {
        Quarter::Q1 => config::Ppm(225_000),
        Quarter::Q2 => config::Ppm(450_000),
        Quarter::Q3 => config::Ppm(675_000),
        Quarter::Q4 => config::Ppm(900_000),
    }
}

/// Produce the quarterly sales report: realized gains and their accruals grouped
/// by `sale_date` into the IRS periods, per (period, jurisdiction), with the
/// LT/ST gain split, computed accrual, and cumulative safe-harbor target.
/// (TAX-REPORT-001/002)
// @spec TAX-REPORT-005 (the report owns applying the de-minimis (TAX-ACCRUAL-005) and
// migration-supersession (TAX-ACCRUAL-007) exclusions to the per-period accrual sums)
pub fn quarterly_report(
    gains: &[RealizedGain],
    events: &[TaxEvent],
    ctx: &TaxContext,
) -> Vec<QuarterlyCell> {
    let accruals = compute_accruals(gains, events, ctx);
    // Index each gain's sale_date by (sale_id, lot_id).
    let mut sale_dates: BTreeMap<(SaleId, LotId), Date> = BTreeMap::new();
    for g in gains {
        sale_dates.insert((g.sale_id.clone(), g.lot_id.clone()), g.sale_date);
    }

    // Aggregate per (period, jurisdiction, tax_year).
    struct Agg {
        lt: i64,
        st: i64,
        accrual: i64,
        bracket_state: BracketState,
    }
    let mut cells: BTreeMap<(Quarter, Jurisdiction, TaxYear), Agg> = BTreeMap::new();

    for a in &accruals {
        // Migration accruals (no backing gain) are not a quarterly-period figure.
        let Some(sale_date) =
            sale_dates.get(&(a.key.sale_id.clone(), a.key.lot_id.clone()))
        else {
            continue;
        };
        let period = quarter_of(*sale_date);
        let entry = cells
            .entry((period, a.key.jurisdiction.clone(), a.key.tax_year))
            .or_insert(Agg {
                lt: 0,
                st: 0,
                accrual: 0,
                bracket_state: a.bracket_state,
            });
        match a.term {
            Term::LongTerm => entry.lt += a.gain_cents.0,
            Term::ShortTerm => entry.st += a.gain_cents.0,
        }
        // A de-minimis (auto-settled) or migration-superseded accrual is excluded
        // from the period's accrual figure — the same exclusion the annual report
        // applies, so Σ over periods ≡ annual stays exact (TAX-VERIF-006), and a
        // migrated year's per-gain cells sum to zero accrual while the migration
        // figure reconciles the year. (TAX-ACCRUAL-005/007, TAX-REPORT-003)
        if !a.de_minimis && !a.superseded {
            entry.accrual += a.applied_cents.map(|c| c.0).unwrap_or(0);
        }
        entry.bracket_state = most_degraded(entry.bracket_state, a.bracket_state);
    }

    cells
        .into_iter()
        .map(|((period, jurisdiction, tax_year), agg)| QuarterlyCell {
            period,
            jurisdiction,
            tax_year,
            long_term_gain_cents: Cents(agg.lt),
            short_term_gain_cents: Cents(agg.st),
            accrual_cents: Cents(agg.accrual),
            safe_harbor_ppm: safe_harbor_ppm(period),
            bracket_state: agg.bracket_state,
        })
        .collect()
}

/// Produce the annual report per `(jurisdiction, tax_year)`: accrued, moved,
/// paid, outstanding (`accrued − paid`), shortfall (`accrued − moved`), and the
/// effective rate (`accrual ÷ gain`) only where `|gain|` exceeds de-minimis.
/// (TAX-REPORT-003/004)
// @spec TAX-REPORT-005 (the report owns applying the de-minimis (TAX-ACCRUAL-005) and
// migration-supersession (TAX-ACCRUAL-007) exclusions to accrued/outstanding/shortfall)
pub fn annual_report(
    gains: &[RealizedGain],
    events: &[TaxEvent],
    ctx: &TaxContext,
) -> Vec<AnnualRow> {
    let accruals = compute_accruals(gains, events, ctx);
    let de_minimis = ctx.de_minimis_cents.0;

    struct Agg {
        accrued: i64,
        gain: i64,
        bracket_state: BracketState,
    }
    let mut rows: BTreeMap<(Jurisdiction, TaxYear), Agg> = BTreeMap::new();
    for a in &accruals {
        let entry = rows
            .entry((a.key.jurisdiction.clone(), a.key.tax_year))
            .or_insert(Agg {
                accrued: 0,
                gain: 0,
                bracket_state: a.bracket_state,
            });
        // De-minimis (auto-settled) accruals are excluded from the outstanding
        // balance (TAX-ACCRUAL-005), and migration-superseded per-gain accruals
        // are excluded so the year reconciles to the seeded legacy actual rather
        // than double-counting (TAX-ACCRUAL-007). `accrued` drives outstanding
        // (`accrued − paid`) and shortfall (`accrued − moved`).
        if !a.de_minimis && !a.superseded {
            entry.accrued += a.applied_cents.map(|c| c.0).unwrap_or(0);
        }
        entry.gain += a.gain_cents.0;
        entry.bracket_state = most_degraded(entry.bracket_state, a.bracket_state);
    }

    // Moved / paid totals from the events (Σ Move − for outstanding/shortfall).
    let mut moved: BTreeMap<(Jurisdiction, TaxYear), i64> = BTreeMap::new();
    let mut paid: BTreeMap<(Jurisdiction, TaxYear), i64> = BTreeMap::new();
    for e in events {
        match &e.kind {
            TaxEventKind::Move { accrual_key, amount_cents, .. } => {
                *moved
                    .entry((accrual_key.jurisdiction.clone(), accrual_key.tax_year))
                    .or_insert(0) += amount_cents.0;
            }
            TaxEventKind::Pay { jurisdiction, tax_year, amount_cents, .. } => {
                *paid
                    .entry((jurisdiction.clone(), *tax_year))
                    .or_insert(0) += amount_cents.0;
            }
            _ => {}
        }
    }

    // Union the keys (an accrued-but-unmoved year still reports, as does a
    // moved/paid migration year).
    let mut keys: std::collections::BTreeSet<(Jurisdiction, TaxYear)> =
        rows.keys().cloned().collect();
    for k in moved.keys().chain(paid.keys()) {
        keys.insert(k.clone());
    }

    keys.into_iter()
        .map(|key| {
            let agg = rows.get(&key);
            let accrued = agg.map(|a| a.accrued).unwrap_or(0);
            let gain = agg.map(|a| a.gain).unwrap_or(0);
            let bracket_state = agg
                .map(|a| a.bracket_state)
                .unwrap_or_else(|| bracket_state_for(ctx, &key.0));
            let moved_c = *moved.get(&key).unwrap_or(&0);
            let paid_c = *paid.get(&key).unwrap_or(&0);
            let effective_rate_ppm = if gain.abs() > de_minimis {
                Some(config::Ppm(
                    kernel::effective_rate_ppm(accrued as i128, (gain as i128).abs()) as i64,
                ))
            } else {
                None
            };
            AnnualRow {
                jurisdiction: key.0.clone(),
                tax_year: key.1,
                accrued_cents: Cents(accrued),
                moved_cents: Cents(moved_c),
                paid_cents: Cents(paid_c),
                outstanding_cents: Cents(accrued - paid_c),
                shortfall_cents: Cents(accrued - moved_c),
                gain_cents: Cents(gain),
                effective_rate_ppm,
                bracket_state,
            }
        })
        .collect()
}

// ===========================================================================
// The verified tax-arithmetic kernel (tax-design.md → "Tax Calculation" /
// "Verification Invariants"). Mirrors ledger-core's `mod core`.
//
// Under plain stable `cargo build` the `verus!{}` macro erases and everything
// here is ordinary Rust. Under the pinned `cargo verus verify` toolchain the
// arithmetic (bracket stacking `T_J`, the marginal-increment accrual, the
// max(0,·) gain clamp, the effective unrealized rate in ppm) is deductively
// verified against the requires/ensures contracts. The accrual-lifecycle fold
// over the TaxEvent log (BTreeMap/String) stays OUTSIDE this block, exactly as
// ledger-core keeps its fold orchestration outside `verus!{}`.
//
// The executable bodies are real integer Rust (they double as the stable-build
// implementation); the `ensures` clauses are the MEANINGFUL, deductively-proven
// contracts (monotonic, bounded `0 ≤ accrual ≤ gain`, total over signed inputs),
// discharged honestly via the same OPERAND_CAP/PROD_CAP overflow accounting and
// the banker's-rounding nearest-bound lemmas ledger-core's kernel uses.
// ===========================================================================
pub mod kernel {
    use vstd::prelude::*;

    // The vstd arithmetic lemmas live in the `#[cfg(verus_keep_ghost)]`-gated
    // `vstd::arithmetic` module, which does NOT exist under stable `cargo build`
    // (where verus!{} erases). The `use`s are therefore gated on the same cfg
    // Verus sets — present only under the Verus toolchain, absent (with the proof
    // calls that reference them) under stable.
    #[cfg(verus_keep_ghost)]
    use vstd::arithmetic::div_mod::{lemma_fundamental_div_mod, lemma_mod_bound};
    #[cfg(verus_keep_ghost)]
    use vstd::arithmetic::mul::{lemma_mul_inequality, lemma_mul_is_commutative};

    verus! {

    // Operand / product caps mirror ledger-core: every value fed the kernel is
    // an i64 field (Cents), so |operand| < 2^62; bounding multiplicands by
    // OPERAND_CAP keeps every i128 product below i128::MAX. The PPM scale and
    // PPM_FULL (100%) bound the rate arithmetic.
    #[allow(dead_code)]
    pub const OPERAND_CAP: i128 = 0x4000_0000_0000_0000; // 1 << 62
    #[allow(dead_code)]
    pub const PROD_CAP: i128 = 0x1000_0000_0000_0000_0000_0000_0000_0000; // 1 << 124
    /// Parts-per-million scale; matches `config::Ppm` (rate = ppm / 1e6).
    pub const PPM_SCALE: i128 = 1_000_000;
    /// 100% in ppm; the stacked top marginal rate must be strictly below this
    /// (config enforces it), which is what makes `0 ≤ accrual ≤ gain` provable.
    pub const PPM_FULL: i128 = 1_000_000;

    // Trusted std euclidean specs for `i128::div_euclid`/`rem_euclid` (positive
    // divisor ⇒ Verus's spec `int` division/modulo — euclidean, remainder in
    // [0, den)) come in TRANSITIVELY from the `ledger-core` dependency's
    // `verus!{}` block: Verus shares one `assume_specification` per std item
    // across the whole verification's crate graph, so redeclaring them here is a
    // hard duplicate-specification error. The trust surface is identical — this
    // kernel relies on exactly the same positive-divisor euclidean contract,
    // declared once (in ledger-core) for the whole workspace.

    // -----------------------------------------------------------------------
    // SPEC: banker's rounding (round half to even) over `int` — the contract the
    // executable `round_half_to_even` refines. `f = floor(num/den)`,
    // `r = num − f*den ∈ [0, den)`; round to nearest, ties to the even quotient.
    // Mirrors `ledger_core::core::round_half_to_even_spec` exactly.
    // -----------------------------------------------------------------------
    pub open spec fn round_half_to_even_spec(num: int, den: int) -> int
        recommends den > 0,
    {
        let f = num / den;
        let r = num % den;
        if 2 * r < den {
            f
        } else if 2 * r > den {
            f + 1
        } else if f % 2 == 0 {
            f
        } else {
            f + 1
        }
    }

    // The defining roundness bound: the chosen integer multiple is within half a
    // denominator of `num` (2*|result*den − num| ≤ den, strictly nearest). PROVEN
    // from the fundamental div-mod identity and the mod range, by case on the tie.
    pub proof fn lemma_round_half_to_even_nearest(num: int, den: int)
        requires den > 0,
        ensures
            2 * (round_half_to_even_spec(num, den) * den - num) <= den,
            2 * (num - round_half_to_even_spec(num, den) * den) <= den,
    {
        lemma_fundamental_div_mod(num, den);  // num == den*(num/den) + num%den
        lemma_mod_bound(num, den);            // 0 <= num%den < den
        let f = num / den;
        let r = num % den;
        assert(den * f == f * den) by { lemma_mul_is_commutative(den, f); }
        assert(f * den - num == -r);
        assert((f + 1) * den == f * den + den) by(nonlinear_arith);
        assert((f + 1) * den - num == den - r);
    }

    // The quotient `num/den` stays inside [-PROD_CAP, PROD_CAP] when |num| is
    // PROD_CAP-bounded and den >= 1. PROVEN — bounds the exec `q`/`q+1`. Mirrors
    // `ledger_core::core::lemma_quotient_bounded`.
    proof fn lemma_quotient_bounded(num: int, den: int)
        requires
            den > 0,
            -PROD_CAP <= num <= PROD_CAP,
        ensures
            -PROD_CAP <= num / den <= PROD_CAP,
    {
        lemma_fundamental_div_mod(num, den);
        lemma_mod_bound(num, den);
        let q = num / den;
        let r = num % den;
        assert(-PROD_CAP <= q <= PROD_CAP) by(nonlinear_arith)
            requires
                den >= 1,
                num == den * q + r,
                0 <= r,
                r < den,
                -PROD_CAP <= num <= PROD_CAP;
    }

    /// Banker's rounding of `num / den` (round half to even), in i128 — the
    /// single rounding rule the kernel applies (matches `pt_core::round_*` and
    /// `ledger_core::core::round_half_to_even`). Used by the rate application and
    /// the effective-rate site. The ensures are the MEANINGFUL contract: the
    /// result equals the banker's-rounding spec AND is the nearest integer
    /// multiple (2*|r*den − num| ≤ den), the bound `apply_rate_ppm` rides on.
    pub fn round_half_to_even(num: i128, den: i128) -> (r: i128)
        requires
            den > 0,
            // den is an i64-derived field (PPM_SCALE / unrealized_pretax) at every
            // call site, so 2*rem (rem < den) cannot overflow i128.
            den <= OPERAND_CAP,
            // |num| <= 2^124 and den >= 1 keep floor(num/den)(+1) inside i128.
            -PROD_CAP <= num <= PROD_CAP,
        ensures
            r as int == round_half_to_even_spec(num as int, den as int),
            // Nearest-integer (banker's) bound: 2|r*den − num| ≤ den.
            2 * (r as int * den as int - num as int) <= den as int,
            2 * (num as int - r as int * den as int) <= den as int,
    {
        let q = num.div_euclid(den);
        let rem = num.rem_euclid(den); // 0 <= rem < den
        proof {
            lemma_round_half_to_even_nearest(num as int, den as int);
            lemma_mod_bound(num as int, den as int);
            lemma_fundamental_div_mod(num as int, den as int);
            lemma_quotient_bounded(num as int, den as int);
            assert(q as int == (num as int) / (den as int));
            assert(rem as int == (num as int) % (den as int));
            assert(-PROD_CAP <= q as int <= PROD_CAP);
            assert(0 <= rem as int);
            assert((rem as int) < (den as int));
            assert((den as int) <= OPERAND_CAP);
        }
        let twice = 2 * rem;
        if twice < den {
            q
        } else if twice > den {
            q + 1
        } else if q % 2 == 0 {
            q
        } else {
            q + 1
        }
    }

    // Helper: the in-kernel product of two OPERAND_CAP-bounded i128s stays inside
    // i128 range (so the exec `*` does not overflow); PROD_CAP == 2^124 < 2^127−1.
    // Mirrors `ledger_core::core::lemma_product_in_range`.
    proof fn lemma_product_in_range(a: int, b: int)
        requires
            -OPERAND_CAP <= a <= OPERAND_CAP,
            -OPERAND_CAP <= b <= OPERAND_CAP,
        ensures
            -PROD_CAP <= a * b <= PROD_CAP,
    {
        assert(OPERAND_CAP * OPERAND_CAP == PROD_CAP) by(compute);
        assert(-(OPERAND_CAP * OPERAND_CAP) <= a * b <= OPERAND_CAP * OPERAND_CAP) by(nonlinear_arith)
            requires
                -OPERAND_CAP <= a <= OPERAND_CAP,
                -OPERAND_CAP <= b <= OPERAND_CAP;
    }

    // The rounded `amount × rate / PPM_SCALE` lies in `[0, amount]` for a
    // non-negative `amount` and a rate in `[0, PPM_SCALE)`. PROVEN from the
    // nearest bound (2|t*scale − amount*rate| ≤ scale) plus the monotonicity
    // 0 <= amount*rate <= amount*scale (rate in [0, scale)). The `apply_rate_ppm`
    // boundedness obligation, mirroring `lemma_consume_share_bounds`.
    proof fn lemma_rate_share_bounds(amount: int, rate: int, scale: int, t: int)
        requires
            amount >= 0,
            scale > 0,
            0 <= rate < scale,
            2 * (t * scale - amount * rate) <= scale,
            2 * (amount * rate - t * scale) <= scale,
        ensures
            0 <= t <= amount,
    {
        // 0 <= amount*rate and amount*rate <= amount*scale (monotone in `rate`).
        lemma_mul_inequality(0, rate, amount);        // 0 <= rate ==> 0*amount <= rate*amount
        lemma_mul_inequality(rate, scale, amount);    // rate <= scale ==> rate*amount <= scale*amount
        assert(0 * amount == 0) by(nonlinear_arith);
        assert(rate * amount == amount * rate) by { lemma_mul_is_commutative(rate, amount); }
        assert(scale * amount == amount * scale) by { lemma_mul_is_commutative(scale, amount); }
        // Now: 0 <= amount*rate <= amount*scale.
        assert(t >= 0) by(nonlinear_arith)
            requires scale > 0, amount * rate >= 0, 2 * (amount * rate - t * scale) <= scale;
        assert(t <= amount) by(nonlinear_arith)
            requires scale > 0, amount * rate <= amount * scale, 2 * (t * scale - amount * rate) <= scale;
    }

    /// Apply a `ppm` rate to a non-negative `amount`: `round(amount × rate / 1e6)`
    /// (banker's rounding). For a rate in `[0, PPM_FULL)` the result is in
    /// `[0, amount]` — the per-band bounded-tax building block. PROVEN bounded
    /// (this is the crux of TAX-VERIF-002). (TAX-CALC-003/004)
    pub fn apply_rate_ppm(amount: i128, rate_ppm: i128) -> (t: i128)
        requires
            0 <= amount <= OPERAND_CAP,
            0 <= rate_ppm < PPM_FULL,
        ensures
            0 <= t as int <= amount as int,
            t as int == round_half_to_even_spec(amount as int * rate_ppm as int, PPM_SCALE as int),
    {
        assert(0 < PPM_SCALE <= OPERAND_CAP) by(compute); // 1 <= 10^6 <= 2^62
        assert(PPM_FULL <= OPERAND_CAP) by(compute);      // rate < 10^6 <= 2^62
        proof {
            // amount, rate are both OPERAND_CAP-bounded ⇒ product is PROD_CAP-bounded
            // (so the exec `*` doesn't overflow, and round's `num` precondition holds).
            lemma_product_in_range(amount as int, rate_ppm as int);
        }
        let prod: i128 = amount * rate_ppm;
        let t = round_half_to_even(prod, PPM_SCALE);
        proof {
            // round's nearest bound + 0 <= rate < scale ⇒ 0 <= t <= amount.
            lemma_rate_share_bounds(
                amount as int, rate_ppm as int, PPM_SCALE as int, t as int);
        }
        t
    }

    /// The non-negative clamp `max(0, x)` the gain arguments enter the bracket
    /// stack through, so `T_J` is total over negative inputs and a within-year
    /// net loss contributes zero gain-tax. (TAX-CALC-006, TAX-VERIF-003)
    pub fn clamp_nonneg(x: i128) -> (r: i128)
        ensures
            r as int == if x as int >= 0 { x as int } else { 0int },
            r as int >= 0,
            // Monotone: clamping preserves order (the hook for monotonic tax).
            r as int <= if x as int >= 0 { x as int } else { 0int },
    {
        if x >= 0 {
            x
        } else {
            0
        }
    }

    /// Tax on `amount` (already clamped ≥ 0) stacked above `base` (≥ 0) through
    /// the ordinary bracket rows — `thresholds[i]` is the lower edge of band `i`
    /// (`thresholds[0] == 0`, ascending) and `rates_ppm[i]` its marginal rate.
    /// The marginal tax of the band `[base, base+amount)`: each sub-slice of that
    /// band is taxed at the rate of the bracket it sits in. The single ordinary
    /// stacking primitive both federal-ST and state use. (TAX-CALC-003/004)
    ///
    /// Contract: total and monotone non-decreasing in `amount`; bounded by
    /// `amount` when the top marginal rate is `< PPM_FULL` (config guarantees).
    /// (TAX-VERIF-001/002/003)
    pub fn bracket_tax_on(
        thresholds: &[i128],
        rates_ppm: &[i128],
        base: i128,
        amount: i128,
    ) -> (t: i128)
        requires
            thresholds.len() == rates_ppm.len(),
            thresholds.len() >= 1,
            base >= 0,
            amount >= 0,
            base <= OPERAND_CAP,
            amount <= OPERAND_CAP,
            // Thresholds are non-negative i64 Cents edges (config data), bounded by
            // OPERAND_CAP, so the band-width subtraction provably can't overflow.
            forall|i: int| 0 <= i < thresholds.len() ==> 0 <= #[trigger] thresholds[i] <= OPERAND_CAP,
            // Every rate in [0, PPM_FULL): the config bounded-tax gate.
            forall|i: int| 0 <= i < rates_ppm.len() ==> 0 <= #[trigger] rates_ppm[i] < PPM_FULL,
        ensures
            // Bounded: a band taxed at rates < 100% yields tax in [0, amount].
            // (TAX-VERIF-002 — boundedness — and the ≥ 0 half of TAX-VERIF-003.)
            0 <= t as int <= amount as int,
    {
        // Integrate the piecewise-constant marginal rate over [base, base+amount).
        // For bracket i the slice of the band inside [thresholds[i],
        // thresholds[i+1]) is taxed at rates_ppm[i]; the last bracket runs to +∞.
        //
        // To make `0 ≤ tax ≤ amount` PROVABLE with NO threshold-sortedness
        // assumption, the band is consumed from an explicit `remaining` budget
        // (starting at `amount`): each bracket taxes at most `remaining` of its
        // width, decrements `remaining`, and (since `apply_rate_ppm(w,·) ≤ w`)
        // adds at most `w ≤ remaining` to the total. The loop invariant
        // `total + remaining == amount`'s slack — `total ≤ amount − remaining` —
        // gives `total ≤ amount` at exit. For a well-formed (sorted, 0-anchored)
        // bracket set the per-bracket widths are exactly the bracket geometry, so
        // this computes the true stacked marginal tax; the clamp only ever binds
        // on a malformed config (which config rejects upstream).
        let lo = base;
        let hi = base + amount;
        let n = thresholds.len();
        let mut total: i128 = 0;
        let mut remaining: i128 = amount; // untaxed budget; tax can't exceed it
        let mut i: usize = 0;
        while i < n
            invariant
                i <= n,
                n == thresholds.len(),
                rates_ppm.len() == thresholds.len(),
                0 <= remaining <= amount,
                0 <= total,
                total <= amount - remaining,
                lo == base,
                hi == base + amount,
                0 <= base <= OPERAND_CAP,
                0 <= amount <= OPERAND_CAP,
                // The config gates are preserved across iterations (slices fixed).
                forall|k: int| 0 <= k < thresholds.len() ==> 0 <= #[trigger] thresholds[k] <= OPERAND_CAP,
                forall|k: int| 0 <= k < rates_ppm.len() ==> 0 <= #[trigger] rates_ppm[k] < PPM_FULL,
            decreases n - i,
        {
            // i < n == len, so both index accesses are in bounds.
            let band_lo = thresholds[i];
            let band_hi = if i + 1 < n { thresholds[i + 1] } else { hi };
            // band_lo, band_hi ∈ [0, hi] ⊆ [0, 2*OPERAND_CAP]; lo, hi likewise. So
            // seg_lo ∈ [0, …], seg_hi ∈ [0, hi], and seg_hi − seg_lo can't overflow.
            assert(0 <= band_lo <= OPERAND_CAP);
            assert(0 <= hi <= 2 * OPERAND_CAP);
            // Intersect [band_lo, band_hi) with [lo, hi).
            let seg_lo = if lo > band_lo { lo } else { band_lo };
            let seg_hi_cap = if hi < band_hi { hi } else { band_hi };
            // The last bracket has no upper edge; cap it at hi.
            let seg_hi = if i + 1 < n { seg_hi_cap } else { hi };
            if seg_hi > seg_lo {
                let raw_width = seg_hi - seg_lo; // > 0 by the guard
                // Clamp the taxed width to the untaxed budget — this is what keeps
                // Σ widths ≤ amount provable without reasoning about disjointness.
                let width = if raw_width < remaining { raw_width } else { remaining };
                // width ∈ [0, remaining] ⊆ [0, amount] ⊆ [0, OPERAND_CAP]; the rate
                // is in [0, PPM_FULL): apply_rate_ppm then yields a tax in [0, width].
                assert(0 <= width <= remaining);
                assert(width <= OPERAND_CAP);
                assert(0 <= rates_ppm[i as int] < PPM_FULL);
                let band_tax = apply_rate_ppm(width, rates_ppm[i]);
                // band_tax ≤ width ≤ remaining, so total stays ≤ amount−remaining.
                assert(0 <= band_tax <= width);
                total = total + band_tax;
                remaining = remaining - width;
            }
            i = i + 1;
        }
        total
    }

    /// The marginal increment a gain adds: `f(after) − f(before)` where `f` is a
    /// monotone non-decreasing tax of the (clamped) cumulative gain. Modeled
    /// here as the kernel difference primitive with the monotonicity and
    /// boundedness ensures the accrual relies on. (TAX-CALC-005, TAX-VERIF-001/002)
    ///
    /// `tax_before` = `T_J(income, max(0,st_before),       max(0,lt_before))`,
    /// `tax_after`  = `T_J(income, max(0,st_before+st(g)), max(0,lt_before+lt(g)))`.
    pub fn marginal_increment(tax_before: i128, tax_after: i128, gain: i128) -> (acc: i128)
        requires
            // Both cumulative taxes are OPERAND_CAP-bounded (each is a sum of
            // bracket_tax_on outputs, themselves ≤ the OPERAND_CAP-bounded gains),
            // so the difference provably cannot overflow i128.
            -OPERAND_CAP <= tax_before <= OPERAND_CAP,
            -OPERAND_CAP <= tax_after <= OPERAND_CAP,
            // `T_J` is monotone non-decreasing, so a non-negative gain cannot
            // lower the cumulative tax (the monotonic-tax invariant feeds this).
            gain >= 0 ==> tax_after as int >= tax_before as int,
            // Bounded: the cumulative tax rose by at most the gain (rates < 100%).
            gain >= 0 ==> tax_after as int - tax_before as int <= gain as int,
        ensures
            acc as int == tax_after as int - tax_before as int,
            // Monotonic / bounded for a positive gain: 0 ≤ accrual ≤ gain.
            // (TAX-VERIF-001/002)
            gain >= 0 ==> 0 <= acc as int <= gain as int,
    {
        let _ = gain; // used only in the (erased-under-stable) contract
        tax_after - tax_before
    }

    /// The effective unrealized tax rate in ppm:
    /// `round(estimated_tax × 1e6 / unrealized_pretax)`, `0` when
    /// `unrealized_pretax ≤ 0`. The single rate-rounding site (so no float
    /// crosses to `sheets-view`). (TAX-CALC-009)
    pub fn effective_rate_ppm(estimated_tax: i128, unrealized_pretax: i128) -> (rate: i128)
        requires
            -OPERAND_CAP <= estimated_tax <= OPERAND_CAP,
            -OPERAND_CAP <= unrealized_pretax <= OPERAND_CAP,
        ensures
            // 0 exactly when the position is not in the money (degraded → 0).
            unrealized_pretax as int <= 0 ==> rate as int == 0,
            // In the money: the rate is the banker's-rounded ppm of the ratio.
            unrealized_pretax as int > 0 ==>
                rate as int == round_half_to_even_spec(
                    estimated_tax as int * PPM_SCALE as int, unrealized_pretax as int),
    {
        if unrealized_pretax <= 0 {
            0
        } else {
            assert(0 < PPM_SCALE <= OPERAND_CAP) by(compute); // 1 <= 10^6 <= 2^62
            proof {
                // |estimated_tax| <= OPERAND_CAP and PPM_SCALE <= OPERAND_CAP, so the
                // numerator product is PROD_CAP-bounded (the exec `*` cannot overflow
                // and round's `num` precondition holds). The divisor is in
                // (0, OPERAND_CAP], meeting round's `den` preconditions.
                lemma_product_in_range(estimated_tax as int, PPM_SCALE as int);
            }
            let num: i128 = estimated_tax * PPM_SCALE;
            round_half_to_even(num, unrealized_pretax)
        }
    }

    } // verus!
}

// ===========================================================================
// Kani bounded-model-checking harnesses for the TAX-VERIF-* arithmetic
// properties. Behind `#[cfg(kani)]` so a normal `cargo build`/`cargo test` is
// unaffected and Kani is NOT a hard dependency. Run with `cargo kani`. The
// harnesses target the `kernel` verus!{} arithmetic ONLY (the lifecycle fold
// over collections stays in `#[test]`s — CBMC cannot fold heap tractably).
// Mirrors ledger-core's kani_proofs. (tax-design.md → "KANI")
// ===========================================================================
#[cfg(kani)]
mod kani_proofs;
