//! `config` — the reference-data + validation segment for portfolio-tracker.
//!
//! Holds the **mutable, versioned reference data** the rest of the system reads
//! but the append-only event log does not produce: per-`tax_year` tax-rule
//! tables (filing status, federal ordinary / federal long-term / per-state
//! ordinary brackets, NIIT, annual ordinary income), the de-minimis threshold,
//! the effective-dated residency timeline, the platform suggestion list, the
//! symbol→ticker alias map, and machine/secret settings. See
//! `docs/intent/config/config-design.md` and `-specs.md` (prefix `CONFIG`).
//!
//! Money is `pt_core::Cents` (i64, hundredths of a dollar); tax rates are
//! fixed-point parts-per-million (`Ppm`, i64 — actual rate = `ppm / 1_000_000`,
//! so `370_000` = 37%, `38_000` = 3.8%, `55_250` = 5.525%). No float ever
//! enters past the input boundary; ppm represents every published federal / DC
//! / NJ rate (including NJ's 5.525%) integer-exact.
//!
//! `config` is the FIRST validation gate behind `tax`'s bounded-tax invariant
//! (`TAX-VERIF-002`): the stacked-rate `< 100%` ceiling is enforced here, so a
//! malformed or rate-excessive bracket set is rejected before `tax` sees it.
//!
//! TDD scaffold: the resolution / staleness / validation helper bodies are
//! stubbed with `unimplemented!()`/`todo!()` so the suite fails RED rather than
//! fail-to-compile. The data types and signatures are complete.
//!
//! Persistence sits behind the thin [`ConfigStore`] trait with an in-memory
//! fake ([`InMemoryConfig`]); real workbook-tab / local-file persistence is
//! wired later by `runtime`. No serde, no I/O in this crate (the HLD trust
//! seam, Sheets row ↔ data, lives outside any verified boundary).

use std::collections::BTreeMap;

use pt_core::{Cents, Date, Lock};

// ===========================================================================
// Units & identifier aliases (config-design.md → "What config holds").
// ===========================================================================

/// A tax rate as fixed-point **parts-per-million**: actual rate = `0 / 1e6`.
/// `370_000` = 37%, `38_000` = 3.8%, `55_250` = 5.525%, `1_000_000` = 100%.
/// Integer-exact for every published federal / DC / NJ rate. (CONFIG-TAXRULES-002)
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Ppm(pub i64);

/// The 100% ceiling in `ppm`. The worst-case stacked top marginal rate must be
/// strictly below this (`< 1_000_000 ppm`) for `tax`'s bounded-tax invariant to
/// hold. (CONFIG-VALID-002)
pub const PPM_FULL: i64 = 1_000_000;

/// A calendar tax year (the year of a sale's trade date). Brackets, income, and
/// filing status are keyed by it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct TaxYear(pub i32);

/// A two-letter state code, e.g. `"DC"`, `"NJ"`. Free text on events;
/// normalized at the input boundary, stored as-is here.
pub type StateCode = String;

/// A ledger symbol (ticker as the user enters it), e.g. `"BRK"`. Mirrors
/// `ledger_core::Symbol`. Identity is assumed when a symbol has no alias entry.
pub type Symbol = String;

/// The default staleness threshold: a verified set older than this many months
/// is stale. (CONFIG-STALE-002, design default 12 months.)
pub const DEFAULT_STALENESS_MONTHS: i32 = 12;

/// The default residency lookback window for the staleness active set, in years
/// (current and prior tax year). (CONFIG-STALE-003, design default.)
pub const DEFAULT_LOOKBACK_YEARS: i32 = 1;

// ===========================================================================
// Jurisdiction (config-design.md → "Resolution helpers"). Federal or a state;
// bracket resolution runs independently per jurisdiction.
// ===========================================================================

/// A tax jurisdiction: the federal government or a specific state. Bracket sets,
/// degradation, and staleness are resolved independently per jurisdiction.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Jurisdiction {
    Federal,
    State(StateCode),
}

// ===========================================================================
// Filing status (config-design.md → "Filing status"; CONFIG-TAXRULES-005).
// Selects which bracket / threshold set applies; recorded once per year.
// Config-internal — `tax` does NOT read it; it selects which set config emits.
// ===========================================================================

/// The year-end / elected filing status for a `tax_year`. A mid-year change is
/// out of scope: a year carries one status. (CONFIG-TAXRULES-005)
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub enum FilingStatus {
    Single,
    #[default]
    MarriedFilingJointly,
    MarriedFilingSeparately,
    HeadOfHousehold,
}

// ===========================================================================
// Bracket tables (config-design.md → "Tax-rule tables (per tax_year)").
// Ordered (lower_threshold_cents, rate_ppm) rows; a row applies to income at or
// above its threshold up to the next row's threshold.
// ===========================================================================

/// One bracket row: the marginal `rate_ppm` applied to amounts at or above
/// `lower_threshold_cents` (up to the next row's threshold). (CONFIG-TAXRULES-001/002)
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct BracketRow {
    /// The lower edge of this marginal band, in `Cents`. The first row is `0`.
    pub lower_threshold_cents: Cents,
    /// The marginal rate over this band, in `ppm` (`≥ 0`).
    pub rate_ppm: Ppm,
}

/// An ordered, validated set of bracket rows for one jurisdiction in one year,
/// carrying its provenance. Validity (non-empty, strictly-ascending unique
/// thresholds, first row at `0`, every rate `≥ 0`) is enforced by
/// [`validate_bracket_set`] before a write is accepted. (CONFIG-VALID-001)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BracketSet {
    /// Ascending-by-threshold rows; first row's threshold is `0`.
    pub rows: Vec<BracketRow>,
    /// The date this set was last verified against its source publication.
    /// Drives the staleness signal. (CONFIG-TAXRULES-004)
    pub last_verified: Date,
    /// The IRS / state publication this set was transcribed from. (CONFIG-TAXRULES-004)
    pub source_note: String,
}

impl BracketSet {
    /// The top marginal `rate_ppm` (the last row's rate) — the rate a single
    /// gain dollar at the highest band faces. Used by the stacked-rate ceiling.
    /// `0` for an empty set (which validation rejects anyway). (CONFIG-VALID-002)
    pub fn top_rate(&self) -> Ppm {
        self.rows.last().map(|r| r.rate_ppm).unwrap_or(Ppm(0))
    }
}

/// The Net Investment Income Tax: a flat `rate_ppm` on investment gain above the
/// `magi_threshold_cents` MAGI floor. (CONFIG-TAXRULES-001)
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Niit {
    /// The NIIT rate in `ppm` (`≥ 0`; e.g. `38_000` = 3.8%).
    pub rate_ppm: Ppm,
    /// The modified-AGI threshold above which NIIT applies, in `Cents` (`≥ 0`).
    pub magi_threshold_cents: Cents,
}

/// The complete per-`tax_year` tax-rule table set. One filing status, the
/// federal ordinary and federal long-term brackets, NIIT, the per-state ordinary
/// brackets (DC, NJ, …), and the annual ordinary-income estimate that drives
/// bracket position. (CONFIG-TAXRULES-001)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TaxRules {
    pub tax_year: TaxYear,
    /// The year-end / elected status. (CONFIG-TAXRULES-005)
    pub filing_status: FilingStatus,
    /// Federal ordinary brackets (short-term gains stack here).
    pub federal_ordinary: BracketSet,
    /// Federal long-term capital-gains brackets (0 / 15 / 20%).
    pub federal_long_term: BracketSet,
    pub niit: Niit,
    /// Per-state ordinary brackets, keyed by `StateCode` (e.g. `"DC"`, `"NJ"`).
    pub state_ordinary: BTreeMap<StateCode, BracketSet>,
    /// The year's ordinary-income estimate, in `Cents`; live/updatable while the
    /// year is open. (CONFIG-TAXRULES-003)
    pub ordinary_income_cents: Cents,
}

// ===========================================================================
// De-minimis (config-design.md → "De-minimis threshold"; CONFIG-TAXRULES-006).
// One configured value, two uses: |accrual| auto-settle floor + |gain| display
// floor in the annual report.
// ===========================================================================

/// The single de-minimis threshold, in `Cents` (e.g. `100` = $1). Compared by
/// `tax` against `|accrual amount|` for auto-settling, and reused by the annual
/// report as a `|gain|` display floor. Must be non-negative. (CONFIG-TAXRULES-006)
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct DeMinimis(pub Cents);

// ===========================================================================
// Residency timeline (config-design.md → "Residency timeline").
// Effective-dated (effective_date, state_code) entries; residency_on inclusive
// of the effective date; future-dated entries allowed. (CONFIG-RESIDENCY-*)
// ===========================================================================

/// One effective-dated residency entry: from `effective_date` onward (inclusive)
/// the owner resides in `state_code`, until a later entry supersedes it.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ResidencyEntry {
    pub effective_date: Date,
    pub state_code: StateCode,
}

/// The effective-dated residency timeline: entries kept sorted by
/// `effective_date`, with unique dates and no consecutive same-state entry.
/// `residency_on(date)` resolves to the entry with the greatest
/// `effective_date ≤ date` (inclusive of the effective date — a sale ON the move
/// date resolves to the NEW state). Future-dated entries are allowed and resolve
/// normally on/after their date. (CONFIG-RESIDENCY-001/002/003)
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ResidencyTimeline {
    /// Sorted ascending by `effective_date`; invariants enforced on write by
    /// [`validate_residency_timeline`].
    entries: Vec<ResidencyEntry>,
}

impl ResidencyTimeline {
    /// An empty timeline (no founding entry yet → import is blocked).
    pub fn new() -> Self {
        ResidencyTimeline { entries: Vec::new() }
    }

    /// Build a timeline from entries, validating the invariants (sorted, unique
    /// dates, no consecutive same-state). `Err` on any violation; the caller's
    /// existing timeline is untouched. (CONFIG-RESIDENCY-001)
    pub fn from_entries(entries: Vec<ResidencyEntry>) -> Result<Self, ConfigError> {
        validate_residency_timeline(&entries)?;
        Ok(ResidencyTimeline { entries })
    }

    /// The entries, sorted ascending by effective date.
    pub fn entries(&self) -> &[ResidencyEntry] {
        &self.entries
    }

    /// Resolve the residency state in effect on `date`: the `state_code` of the
    /// entry with the greatest `effective_date ≤ date`. `None` when `date`
    /// precedes the earliest entry (the undefined pre-history region, which the
    /// founding-entry import gate keeps unreachable). A sale on an exact
    /// effective date resolves to the NEW state. (CONFIG-RESIDENCY-002/003)
    pub fn residency_on(&self, date: Date) -> Option<StateCode> {
        // Entries are sorted ascending by effective_date (enforced on write).
        // The state in effect on `date` is the LAST entry whose effective_date
        // is `<= date` (inclusive of the move date). `None` when `date` precedes
        // the earliest entry (the undefined pre-history region).
        self.entries
            .iter()
            .rev()
            .find(|e| e.effective_date.0 <= date.0)
            .map(|e| e.state_code.clone())
    }
}

// ===========================================================================
// Platforms & aliases (config-design.md → "Platform list" / "Symbol → ticker
// aliases"; CONFIG-PLATFORM-001/002).
// ===========================================================================

/// The platform suggestion list. A TUI suggestion source, NOT a constraint: an
/// event may name a platform absent from the list. (CONFIG-PLATFORM-001)
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct PlatformList {
    names: Vec<String>,
}

impl PlatformList {
    pub fn new(names: Vec<String>) -> Self {
        PlatformList { names }
    }

    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// Whether `name` is a suggestion. Membership is advisory only — absence
    /// never blocks event entry. (CONFIG-PLATFORM-001)
    pub fn contains(&self, name: &str) -> bool {
        self.names.iter().any(|n| n == name)
    }
}

/// A small symbol→ticker alias map. Consumed by `sheets-view` to resolve the
/// `GOOGLEFINANCE` ticker form for a ledger symbol; identity is assumed when a
/// symbol has no alias entry. (CONFIG-PLATFORM-002)
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct AliasMap {
    map: BTreeMap<Symbol, String>,
}

impl AliasMap {
    pub fn new(map: BTreeMap<Symbol, String>) -> Self {
        AliasMap { map }
    }

    /// The full symbol→ticker map (the persistence adapters' wire form reads it).
    pub fn entries(&self) -> &BTreeMap<Symbol, String> {
        &self.map
    }

    /// Resolve the ticker form for `symbol`: the mapped alias if present, else
    /// the symbol itself (identity default). (CONFIG-PLATFORM-002)
    pub fn resolve(&self, symbol: &str) -> String {
        // The mapped alias if present, else the symbol itself (identity default).
        self.map
            .get(symbol)
            .cloned()
            .unwrap_or_else(|| symbol.to_string())
    }
}

/// A small symbol→**company display name** map (`AMZN` → `Amazon.com`), the
/// alias map's sibling: the owner does not always recognize tickers, so the TUI
/// renders a name column beside them. The ticker itself is assumed when a symbol
/// has no entry. Persisted/round-tripped with the rest of the domain config.
/// (CONFIG-PLATFORM-003)
// @spec CONFIG-PLATFORM-003
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct DisplayNameMap {
    map: BTreeMap<Symbol, String>,
}

impl DisplayNameMap {
    pub fn new(map: BTreeMap<Symbol, String>) -> Self {
        DisplayNameMap { map }
    }

    /// The full symbol→name map (the persistence adapters' wire form reads it).
    pub fn entries(&self) -> &BTreeMap<Symbol, String> {
        &self.map
    }

    /// Resolve the display name for `symbol`: the mapped company name if present,
    /// else the ticker itself (an unmapped symbol degrades honestly to what the
    /// ledger already shows). (CONFIG-PLATFORM-003)
    pub fn resolve(&self, symbol: &str) -> String {
        self.map
            .get(symbol)
            .cloned()
            .unwrap_or_else(|| symbol.to_string())
    }
}

// ===========================================================================
// Machine/secret settings (config-design.md → "Machine/secret settings");
// CONFIG-SETTINGS-001). Local file only — NEVER the shared workbook.
// ===========================================================================

/// Machine/secret settings stored in a LOCAL file (TOML), never in the shared
/// workbook: the workbook id, the service-account credentials path, the cache
/// location, and the reporting timezone. (CONFIG-SETTINGS-001)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Settings {
    pub workbook_id: String,
    pub credentials_path: String,
    pub cache_path: String,
    /// The locale `reports` uses for capture-date metadata; default
    /// `"US/Eastern"` (DC and NJ are both Eastern). (CONFIG-SETTINGS-001)
    pub reporting_timezone: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            workbook_id: String::new(),
            credentials_path: String::new(),
            cache_path: String::new(),
            reporting_timezone: "US/Eastern".to_string(),
        }
    }
}

// ===========================================================================
// Resolution outputs (config-design.md → "Resolution helpers").
// The per-jurisdiction tag carried alongside emitted brackets, shared with
// `tax` (BracketState = Verified | Stale | NoBracketsAvailable).
// ===========================================================================

/// The freshness tag carried alongside every bracket set `config` emits to
/// `tax`, threaded through every accrual / reserve / estimate. (CONFIG-STALE-001)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BracketState {
    /// The requested year's own verified set was found.
    Verified,
    /// No set for the requested year; degraded to the most recent prior-year
    /// set for this jurisdiction.
    Stale,
    /// No set in any year `≤ requested` for this jurisdiction — cold-start. `tax`
    /// surfaces "cannot estimate, enter brackets" rather than silently zeroing.
    NoBracketsAvailable,
}

/// The result of a per-jurisdiction bracket resolution: the resolved set (if
/// any) tagged with its [`BracketState`]. `set` is `None` exactly when `state`
/// is `NoBracketsAvailable`. (CONFIG-STALE-001)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ResolvedBrackets {
    pub jurisdiction: Jurisdiction,
    pub tax_year: TaxYear,
    pub set: Option<BracketSet>,
    pub state: BracketState,
}

/// A staleness reminder for the TUI: a `(jurisdiction, tax_year)` whose bracket
/// set is missing or older than the threshold. The refresh *procedure* is a
/// runbook, not code. (CONFIG-STALE-004)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StalenessSignal {
    pub jurisdiction: Jurisdiction,
    pub tax_year: TaxYear,
}

// ===========================================================================
// Error model (config-design.md → "Validation" / "Read contract & failure
// modes"). One variant per rejection / failure trigger.
// ===========================================================================

/// A rejection of a candidate config write, or a read-contract failure. A
/// rejected write leaves the stored config untouched (no partial mutation).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ConfigError {
    /// A bracket set is empty. (CONFIG-VALID-001)
    EmptyBracketSet,
    /// A bracket set's first row threshold is not `0`. (CONFIG-VALID-001)
    FirstRowNotZero,
    /// Bracket thresholds are not strictly ascending / not unique. (CONFIG-VALID-001)
    ThresholdsNotStrictlyAscending,
    /// A bracket row has a negative `rate_ppm`. (CONFIG-VALID-001)
    NegativeRate,
    /// For some `(tax_year, state)` the stacked top marginal rate
    /// `max(fed-ord, fed-LT) + state + NIIT` reaches `1_000_000 ppm` (100%).
    /// This gates `tax`'s bounded-tax invariant. (CONFIG-VALID-002)
    StackedRateExceedsCeiling,
    /// The NIIT rate, de-minimis, or annual income is negative. (CONFIG-VALID-003)
    NegativeAmount,
    /// The residency timeline is unsorted or has duplicate effective dates.
    /// (CONFIG-RESIDENCY-001)
    ResidencyUnsortedOrDuplicate,
    /// Two consecutive residency entries name the same state (a no-op move).
    /// (CONFIG-RESIDENCY-001)
    ResidencyConsecutiveSameState,
    /// An import would run with no residency entry at or before the earliest
    /// event date to classify (no founding entry). (CONFIG-VALID-004)
    MissingFoundingResidency,
    /// The credentials file is missing or invalid — a hard error, never empty
    /// config. (CONFIG-SETTINGS-004)
    CredentialsUnavailable,
    /// A `put_*` could not acquire the runtime-owned advisory write-lock (it is
    /// held by another holder, or an acquisition I/O error): the write is refused
    /// and stored config is left untouched — the same "leave no partial state"
    /// contract a transport failure honors. (CONFIG-SETTINGS-006)
    LockHeld,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ConfigError {}

/// A failed advisory-lock acquisition (held by another holder, or an I/O error)
/// refuses the `put_*` write — stored config is left untouched. (CONFIG-SETTINGS-006)
impl From<pt_core::LockError> for ConfigError {
    fn from(_e: pt_core::LockError) -> Self {
        ConfigError::LockHeld
    }
}

// ===========================================================================
// Validation gate (config-design.md → "Validation"; CONFIG-VALID-001..004).
// The first gate behind `tax`'s bounded-tax invariant. Pure helpers, unit-tested.
//
// STUBBED for TDD: bodies are `unimplemented!()` so the RED phase fails at
// runtime, not at compile time.
// ===========================================================================

/// Validate one bracket set: non-empty, strictly-ascending unique
/// `lower_threshold_cents`, a first row at `0`, every `rate_ppm ≥ 0`. A single
/// flat-rate `[(0, r)]` is valid. (CONFIG-VALID-001)
pub fn validate_bracket_set(set: &BracketSet) -> Result<(), ConfigError> {
    // Non-empty.
    let first = match set.rows.first() {
        Some(r) => r,
        None => return Err(ConfigError::EmptyBracketSet),
    };
    // First row at 0.
    if first.lower_threshold_cents.0 != 0 {
        return Err(ConfigError::FirstRowNotZero);
    }
    // Strictly ascending, unique thresholds.
    for pair in set.rows.windows(2) {
        if pair[1].lower_threshold_cents.0 <= pair[0].lower_threshold_cents.0 {
            return Err(ConfigError::ThresholdsNotStrictlyAscending);
        }
    }
    // Every rate non-negative.
    if set.rows.iter().any(|r| r.rate_ppm.0 < 0) {
        return Err(ConfigError::NegativeRate);
    }
    Ok(())
}

/// Validate a full per-year rule table: every bracket set well-formed
/// (CONFIG-VALID-001), NIIT / income non-negative (CONFIG-VALID-003), and — for
/// every state — the stacked top marginal rate
/// `max(fed-ord top, fed-LT top) + state top + NIIT < 1_000_000 ppm`
/// (CONFIG-VALID-002, the bounded-tax gate). De-minimis is validated separately
/// (it is not per-year). Rejects with the first applicable `ConfigError`.
pub fn validate_tax_rules(rules: &TaxRules) -> Result<(), ConfigError> {
    // Every bracket set must be well-formed (CONFIG-VALID-001).
    validate_bracket_set(&rules.federal_ordinary)?;
    validate_bracket_set(&rules.federal_long_term)?;
    for set in rules.state_ordinary.values() {
        validate_bracket_set(set)?;
    }
    // NIIT rate and annual income non-negative (CONFIG-VALID-003). (De-minimis is
    // not per-year; it is validated at its own write boundary.)
    if rules.niit.rate_ppm.0 < 0
        || rules.niit.magi_threshold_cents.0 < 0
        || rules.ordinary_income_cents.0 < 0
    {
        return Err(ConfigError::NegativeAmount);
    }
    // Stacked-rate ceiling: the worst-case stacked top marginal rate
    // max(fed-ord, fed-LT) + state + NIIT must be strictly below 100%
    // (CONFIG-VALID-002). This is the gate behind tax's bounded-tax invariant
    // (TAX-VERIF-002); a per-table check would miss the stacking. The federal +
    // NIIT baseline (no state term) is ALWAYS evaluated — a stateless tax year
    // whose federal+NIIT alone reaches 100% is just as much a bounded-tax breach
    // as a stateful one — and then each state is checked on top of that baseline.
    // (CONFIG-VALID-005 gates this state-independent federal baseline.)
    if federal_baseline_top_rate_ppm(rules) >= PPM_FULL {
        return Err(ConfigError::StackedRateExceedsCeiling);
    }
    for state in rules.state_ordinary.keys() {
        if stacked_top_rate_ppm(rules, state) >= PPM_FULL {
            return Err(ConfigError::StackedRateExceedsCeiling);
        }
    }
    Ok(())
}

/// The federal-only stacked top marginal rate baseline for `rules`:
/// `max(federal-ordinary top, federal-LT top) + NIIT`, in `ppm` — the most a
/// single gain dollar faces in a year carrying NO state bracket set. This is the
/// state-independent floor the stacked-rate ceiling always enforces, so a
/// stateless tax year cannot slip past the bounded-tax gate. (CONFIG-VALID-002,
/// CONFIG-VALID-005)
pub fn federal_baseline_top_rate_ppm(rules: &TaxRules) -> i64 {
    rules
        .federal_ordinary
        .top_rate()
        .0
        .max(rules.federal_long_term.top_rate().0)
        + rules.niit.rate_ppm.0
}

/// The worst-case stacked top marginal rate for `(rules, state)`:
/// `max(federal-ordinary top, federal-LT top) + state top + NIIT`, in `ppm` —
/// the most a single gain dollar can face. (CONFIG-VALID-002)
pub fn stacked_top_rate_ppm(rules: &TaxRules, state: &StateCode) -> i64 {
    let state_top = rules
        .state_ordinary
        .get(state)
        .map(|s| s.top_rate().0)
        .unwrap_or(0);
    federal_baseline_top_rate_ppm(rules) + state_top
}

/// Validate the residency timeline invariants: unique, sorted `effective_date`s
/// and no consecutive same-state entry. (CONFIG-RESIDENCY-001)
pub fn validate_residency_timeline(entries: &[ResidencyEntry]) -> Result<(), ConfigError> {
    for pair in entries.windows(2) {
        // Strictly ascending, unique effective dates.
        if pair[1].effective_date.0 <= pair[0].effective_date.0 {
            return Err(ConfigError::ResidencyUnsortedOrDuplicate);
        }
        // No consecutive same-state entry (a no-op move).
        if pair[1].state_code == pair[0].state_code {
            return Err(ConfigError::ResidencyConsecutiveSameState);
        }
    }
    Ok(())
}

/// The import founding-entry precondition: before an import runs, the residency
/// timeline must have at least one entry at or before `earliest_event_date` (so
/// `residency_on` is never queried in its undefined pre-history region). The
/// founding entry may be an owner-asserted best-guess state. `Ok(())` ⇒ import
/// may proceed; `Err(MissingFoundingResidency)` ⇒ blocked. (CONFIG-VALID-004)
pub fn check_import_founding_residency(
    timeline: &ResidencyTimeline,
    earliest_event_date: Date,
) -> Result<(), ConfigError> {
    // Import may proceed iff some entry is at or before the earliest event date,
    // so `residency_on` is never queried in its undefined pre-history region.
    match timeline
        .entries()
        .iter()
        .any(|e| e.effective_date.0 <= earliest_event_date.0)
    {
        true => Ok(()),
        false => Err(ConfigError::MissingFoundingResidency),
    }
}

// ===========================================================================
// Resolution & staleness helpers (config-design.md → "Resolution helpers").
// Per-jurisdiction degrade-to-prior-year (flagged Stale); cold-start
// NoBracketsAvailable; staleness from config's OWN residency timeline, never
// `tax` accrual state. Pure helpers over the (year → TaxRules) series.
// ===========================================================================

/// Resolve the bracket set for `jurisdiction` in `tax_year`, walking the
/// `rules_by_year` series independently per jurisdiction:
///
/// * the jurisdiction's set verified FOR `tax_year`, if present → `Verified`;
///   else
/// * the jurisdiction's most-recent PRIOR-year set (greatest year `< tax_year`
///   carrying a set for it) → `Stale`; else
/// * no set in any year `≤ tax_year` for it → `NoBracketsAvailable` (cold-start).
///
/// `Federal` uses the per-year `federal_ordinary` series; `State(s)` uses that
/// state's entry in `state_ordinary`. (CONFIG-STALE-001)
pub fn resolve_brackets(
    rules_by_year: &BTreeMap<TaxYear, TaxRules>,
    jurisdiction: &Jurisdiction,
    tax_year: TaxYear,
) -> ResolvedBrackets {
    // The jurisdiction's set FOR the requested year, if present → Verified.
    if let Some(set) = rules_by_year
        .get(&tax_year)
        .and_then(|r| bracket_set_for(r, jurisdiction))
    {
        return ResolvedBrackets {
            jurisdiction: jurisdiction.clone(),
            tax_year,
            set: Some(set.clone()),
            state: BracketState::Verified,
        };
    }
    // Else the most-recent PRIOR-year set carrying this jurisdiction (walking
    // down the jurisdiction's OWN sparse year series) → Stale. BTreeMap iterates
    // ascending by key; take the greatest year strictly below `tax_year`.
    if let Some(set) = rules_by_year
        .range(..tax_year)
        .rev()
        .find_map(|(_, r)| bracket_set_for(r, jurisdiction))
    {
        return ResolvedBrackets {
            jurisdiction: jurisdiction.clone(),
            tax_year,
            set: Some(set.clone()),
            state: BracketState::Stale,
        };
    }
    // Else no set in any year <= requested for this jurisdiction → cold-start.
    ResolvedBrackets {
        jurisdiction: jurisdiction.clone(),
        tax_year,
        set: None,
        state: BracketState::NoBracketsAvailable,
    }
}

/// The bracket set a `TaxRules` carries for `jurisdiction`, if any: the federal
/// ordinary set for `Federal`, or the state's entry in `state_ordinary` for
/// `State(s)`. A jurisdiction the table does not cover yields `None`.
fn bracket_set_for<'a>(
    rules: &'a TaxRules,
    jurisdiction: &Jurisdiction,
) -> Option<&'a BracketSet> {
    match jurisdiction {
        Jurisdiction::Federal => Some(&rules.federal_ordinary),
        Jurisdiction::State(s) => rules.state_ordinary.get(s),
    }
}

/// The staleness active set: `Federal` plus every state appearing in the
/// residency `timeline` within the lookback window `[current_year -
/// lookback_years, current_year]` (entries not yet effective as of
/// `current_year`'s end are excluded). Computed entirely from `config`'s own
/// data; it does NOT consult `tax` accrual state. (CONFIG-STALE-003)
pub fn active_jurisdictions(
    timeline: &ResidencyTimeline,
    current_year: TaxYear,
    lookback_years: i32,
) -> Vec<Jurisdiction> {
    // The window spans tax years [current_year - lookback_years, current_year].
    let window_start = days_from_civil(current_year.0 - lookback_years, 1, 1);
    let window_end = days_from_civil(current_year.0, 12, 31);

    // Federal is always active.
    let mut out: Vec<Jurisdiction> = vec![Jurisdiction::Federal];

    // A state is active when its residency coverage interval
    // [effective_date, next_effective_date) (the last entry runs to +inf)
    // intersects the window AND its entry is already effective (not future).
    let entries = timeline.entries();
    let mut seen: std::collections::BTreeSet<StateCode> = std::collections::BTreeSet::new();
    for (i, e) in entries.iter().enumerate() {
        let coverage_start = e.effective_date.0;
        let coverage_end = entries
            .get(i + 1)
            .map(|n| n.effective_date.0)
            .unwrap_or(i32::MAX);
        // Future entries (start after the window's end) are excluded.
        let intersects = coverage_start <= window_end && coverage_end > window_start;
        if intersects && seen.insert(e.state_code.clone()) {
            out.push(Jurisdiction::State(e.state_code.clone()));
        }
    }
    out
}

/// Whether `jurisdiction`'s newest verified set is stale FOR `current_year`: no
/// verified set exists for it that year, OR the newest set's `last_verified` is
/// older than `threshold_months` before `as_of`. Computed from `config`'s own
/// `last_verified` dates. (CONFIG-STALE-002)
pub fn is_stale(
    rules_by_year: &BTreeMap<TaxYear, TaxRules>,
    jurisdiction: &Jurisdiction,
    current_year: TaxYear,
    as_of: Date,
    threshold_months: i32,
) -> bool {
    let resolved = resolve_brackets(rules_by_year, jurisdiction, current_year);
    match resolved.state {
        // No verified set FOR the current year (degraded or cold-start) → stale.
        BracketState::Stale | BracketState::NoBracketsAvailable => true,
        // A verified current-year set: stale iff its last_verified is OLDER than
        // `threshold_months` before `as_of` (strictly older than the cutoff).
        BracketState::Verified => {
            let cutoff = subtract_months(as_of, threshold_months);
            resolved
                .set
                .map(|s| s.last_verified.0 < cutoff.0)
                .unwrap_or(true)
        }
    }
}

/// The full set of staleness signals to surface to the TUI: for each active
/// jurisdiction (CONFIG-STALE-003) that is stale for `current_year`
/// (CONFIG-STALE-002), one `(jurisdiction, current_year)` signal. The active set
/// is derived from the residency `timeline`, never `tax` state. (CONFIG-STALE-004)
pub fn staleness_signals(
    rules_by_year: &BTreeMap<TaxYear, TaxRules>,
    timeline: &ResidencyTimeline,
    current_year: TaxYear,
    as_of: Date,
    threshold_months: i32,
    lookback_years: i32,
) -> Vec<StalenessSignal> {
    active_jurisdictions(timeline, current_year, lookback_years)
        .into_iter()
        .filter(|j| is_stale(rules_by_year, j, current_year, as_of, threshold_months))
        .map(|j| StalenessSignal {
            jurisdiction: j,
            tax_year: current_year,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Civil-date helpers (days since the Unix epoch, matching `pt_core::Date`).
// Howard Hinnant's `days_from_civil` / `civil_from_days` algorithms — integer
// exact, leap-years handled, no float. Used only by the staleness window math.
// ---------------------------------------------------------------------------

/// Days since 1970-01-01 for the civil date `(y, m, d)` (proleptic Gregorian).
fn days_from_civil(y: i32, m: i32, d: i32) -> i32 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) as i64 + 2) / 5
        + (d - 1) as i64; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    (era as i64 * 146097 + doe - 719468) as i32
}

/// The civil date `(y, m, d)` for `days` since 1970-01-01.
fn civil_from_days(days: i32) -> (i32, i32, i32) {
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    ((if m <= 2 { y + 1 } else { y }) as i32, m as i32, d as i32)
}

/// `date` minus `months` calendar months, clamping the day to the target
/// month's length (so e.g. Mar 31 − 1 month = Feb 28/29). The staleness
/// threshold cutoff.
fn subtract_months(date: Date, months: i32) -> Date {
    let (y, m, d) = civil_from_days(date.0);
    // Months are 1..=12; shift to 0-based for modular arithmetic.
    let total = (y * 12 + (m - 1)) - months;
    let ny = total.div_euclid(12);
    let nm = total.rem_euclid(12) + 1;
    let last = last_day_of_month(ny, nm);
    let nd = d.min(last);
    Date(days_from_civil(ny, nm, nd))
}

/// The number of days in month `m` of year `y` (Gregorian leap rule).
fn last_day_of_month(y: i32, m: i32) -> i32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

// ===========================================================================
// Default new-sale residency stamp (config-design.md → "Residency timeline";
// CONFIG-RESIDENCY-004). config defaults the stamp; tax/import overrides per txn.
// ===========================================================================

/// The default `accrues_to_state` for a sale entered on `sale_date`:
/// `residency_on(sale_date)`. Left overridable per transaction by the caller.
/// `None` in the undefined pre-history region (which the founding-entry gate
/// keeps unreachable for imports). (CONFIG-RESIDENCY-004)
pub fn default_accrues_to_state(
    timeline: &ResidencyTimeline,
    sale_date: Date,
) -> Option<StateCode> {
    timeline.residency_on(sale_date)
}

// ===========================================================================
// Persistence seam (config-design.md → "Trust Boundary & Interfaces" /
// "Read contract & failure modes"). A THIN trait with an in-memory fake for
// TDD; real workbook-tab / local-file persistence is wired later by `runtime`.
// `config` owns its own cache; the shared Sheets-access layer + advisory lock
// are `runtime`'s.
// ===========================================================================

/// The aggregate of all domain config a snapshot holds: the per-year rule
/// tables, de-minimis, residency timeline, platforms, and aliases. (Settings are
/// the local-file half and are read separately.)
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ConfigData {
    pub rules_by_year: BTreeMap<TaxYear, TaxRules>,
    pub de_minimis: DeMinimis,
    pub residency: ResidencyTimeline,
    pub platforms: PlatformList,
    pub aliases: AliasMap,
    /// The symbol→company-display-name map, the alias map's sibling.
    /// (CONFIG-PLATFORM-003)
    pub display_names: DisplayNameMap,
}

/// The persistence seam. Domain config lives in workbook tabs mirrored to the
/// local cache (CONFIG-SETTINGS-002); on a workbook/cache divergence the
/// workbook wins and the cache is rebuilt (CONFIG-SETTINGS-005). A fresh
/// workbook with no config tabs reads as cold-start (CONFIG-SETTINGS-003); bad
/// credentials surface a hard error (CONFIG-SETTINGS-004). On every write the
/// validation gate runs and an invalid set is refused. `runtime` provides the
/// real Sheets/local-file implementation; tests use [`InMemoryConfig`].
pub trait ConfigStore {
    /// Load all domain config. A fresh workbook (no config tabs) yields an empty
    /// [`ConfigData`] — cold-start, which bracket resolution turns into
    /// `NoBracketsAvailable`. (CONFIG-SETTINGS-003)
    fn load(&self) -> Result<ConfigData, ConfigError>;

    /// Read the local-file machine/secret settings. Missing/invalid credentials
    /// surface `CredentialsUnavailable`, never empty config. (CONFIG-SETTINGS-004)
    fn load_settings(&self) -> Result<Settings, ConfigError>;

    /// Persist a full per-year rule table after running the validation gate
    /// (CONFIG-VALID-001/002/003). Acquires the runtime-owned advisory write-lock
    /// before mutating the workbook config tab and releases it after; a held lock
    /// refuses the write (`LockHeld`). An invalid set is refused, leaving stored
    /// config untouched. (CONFIG-SETTINGS-006)
    fn put_tax_rules<L: Lock>(&mut self, rules: TaxRules, lock: &L) -> Result<(), ConfigError>;

    /// Persist the residency timeline after validating its invariants
    /// (CONFIG-RESIDENCY-001), under the advisory write-lock. An invalid timeline
    /// or a held lock is refused. (CONFIG-SETTINGS-006)
    fn put_residency<L: Lock>(
        &mut self,
        timeline: ResidencyTimeline,
        lock: &L,
    ) -> Result<(), ConfigError>;

    /// Persist the de-minimis threshold after the non-negativity check
    /// (CONFIG-VALID-003), under the advisory write-lock. (CONFIG-SETTINGS-006)
    fn put_de_minimis<L: Lock>(
        &mut self,
        de_minimis: DeMinimis,
        lock: &L,
    ) -> Result<(), ConfigError>;

    /// Persist the platform suggestion list (no constraint on event entry), under
    /// the advisory write-lock. (CONFIG-PLATFORM-001, CONFIG-SETTINGS-006)
    fn put_platforms<L: Lock>(
        &mut self,
        platforms: PlatformList,
        lock: &L,
    ) -> Result<(), ConfigError>;

    /// Persist the symbol→ticker alias map, under the advisory write-lock.
    /// (CONFIG-PLATFORM-002, CONFIG-SETTINGS-006)
    fn put_aliases<L: Lock>(&mut self, aliases: AliasMap, lock: &L) -> Result<(), ConfigError>;

    /// Persist the symbol→display-name map, under the advisory write-lock.
    /// (CONFIG-PLATFORM-003, CONFIG-SETTINGS-006)
    fn put_display_names<L: Lock>(
        &mut self,
        names: DisplayNameMap,
        lock: &L,
    ) -> Result<(), ConfigError>;
}

/// An in-memory [`ConfigStore`] fake for TDD. Models the two-tier persistence:
/// the authoritative workbook tabs (`workbook`) and the rebuildable local cache
/// mirror (`cache`). Every `put_*` runs the same validation gate the real store
/// will and writes through to the workbook, keeping the cache in sync. On
/// `load`, the workbook tabs are authoritative and the cache is rebuilt from
/// them, so a diverging cache is overwritten, never trusted (CONFIG-SETTINGS-005).
/// A `cold_start()` fake models a fresh workbook (no config tabs);
/// `with_bad_credentials()` models an unreadable secrets file;
/// `with_diverging_cache()` seeds a cache that disagrees with the workbook so the
/// workbook-wins rebuild is observable.
#[derive(Clone, Debug)]
pub struct InMemoryConfig {
    /// The authoritative workbook tabs.
    workbook: ConfigData,
    /// The rebuildable local cache mirror. Carries no truth; rebuilt from
    /// `workbook` on every `load`. (CONFIG-SETTINGS-002/005)
    cache: std::cell::RefCell<ConfigData>,
    settings: Option<Settings>,
}

impl InMemoryConfig {
    /// A populated fake seeded with `data` and `settings`. The cache starts in
    /// sync with the workbook.
    pub fn new(data: ConfigData, settings: Settings) -> Self {
        InMemoryConfig {
            cache: std::cell::RefCell::new(data.clone()),
            workbook: data,
            settings: Some(settings),
        }
    }

    /// A cold-start fake: a fresh workbook with no config tabs. `load` returns
    /// empty `ConfigData`, so bracket resolution yields `NoBracketsAvailable`.
    /// (CONFIG-SETTINGS-003)
    pub fn cold_start() -> Self {
        InMemoryConfig {
            workbook: ConfigData::default(),
            cache: std::cell::RefCell::new(ConfigData::default()),
            settings: Some(Settings::default()),
        }
    }

    /// A fake whose credentials file is missing/invalid: `load_settings` returns
    /// `CredentialsUnavailable`. (CONFIG-SETTINGS-004)
    pub fn with_bad_credentials(data: ConfigData) -> Self {
        InMemoryConfig {
            cache: std::cell::RefCell::new(data.clone()),
            workbook: data,
            settings: None,
        }
    }

    /// A fake whose local cache DISAGREES with the authoritative workbook tabs.
    /// `load` must return the `workbook` content and rebuild the cache to match
    /// it (the workbook wins; the cache is overwritten). (CONFIG-SETTINGS-005)
    pub fn with_diverging_cache(workbook: ConfigData, cache: ConfigData) -> Self {
        debug_assert_ne!(
            workbook, cache,
            "with_diverging_cache expects a cache that diverges from the workbook"
        );
        InMemoryConfig {
            workbook,
            cache: std::cell::RefCell::new(cache),
            settings: Some(Settings::default()),
        }
    }

    /// The current local-cache contents (for tests asserting the cache was
    /// rebuilt from the workbook). Carries no truth. (CONFIG-SETTINGS-005)
    pub fn cache_snapshot(&self) -> ConfigData {
        self.cache.borrow().clone()
    }

    /// Mirror the authoritative workbook into the local cache after a write
    /// (CONFIG-SETTINGS-002). The cache carries no truth; it always tracks the
    /// workbook.
    fn sync_cache(&self) {
        *self.cache.borrow_mut() = self.workbook.clone();
    }
}

// @spec CONFIG-SETTINGS-006
impl ConfigStore for InMemoryConfig {
    fn load(&self) -> Result<ConfigData, ConfigError> {
        // The workbook tabs are authoritative. On load the cache is rebuilt from
        // them — a diverging cache is overwritten, never trusted — and the
        // workbook content is returned. A cold-start fake holds empty
        // `ConfigData`, which bracket resolution turns into `NoBracketsAvailable`.
        // (CONFIG-SETTINGS-002/003/005)
        *self.cache.borrow_mut() = self.workbook.clone();
        Ok(self.workbook.clone())
    }

    fn load_settings(&self) -> Result<Settings, ConfigError> {
        // The local-file half. A missing/invalid secrets file is a hard error,
        // never empty config. (CONFIG-SETTINGS-001/004)
        self.settings
            .clone()
            .ok_or(ConfigError::CredentialsUnavailable)
    }

    fn put_tax_rules<L: Lock>(&mut self, rules: TaxRules, lock: &L) -> Result<(), ConfigError> {
        // Acquire the advisory write-lock BEFORE mutating; a held lock refuses the
        // write with stored config untouched. The guard releases on scope exit (after
        // the mutation). (CONFIG-SETTINGS-006)
        let _guard = lock.acquire()?;
        // The validation gate runs on every write; an invalid set is refused,
        // leaving stored config untouched. (CONFIG-VALID-001/002/003)
        validate_tax_rules(&rules)?;
        self.workbook.rules_by_year.insert(rules.tax_year, rules);
        self.sync_cache();
        Ok(())
    }

    fn put_residency<L: Lock>(
        &mut self,
        timeline: ResidencyTimeline,
        lock: &L,
    ) -> Result<(), ConfigError> {
        let _guard = lock.acquire()?; // (CONFIG-SETTINGS-006)
        // The timeline is constructed through `from_entries` (which validates),
        // so it is well-formed by the time it arrives; re-validate defensively.
        validate_residency_timeline(timeline.entries())?;
        self.workbook.residency = timeline;
        self.sync_cache();
        Ok(())
    }

    fn put_de_minimis<L: Lock>(
        &mut self,
        de_minimis: DeMinimis,
        lock: &L,
    ) -> Result<(), ConfigError> {
        let _guard = lock.acquire()?; // (CONFIG-SETTINGS-006)
        // De-minimis must be non-negative. (CONFIG-VALID-003)
        if (de_minimis.0).0 < 0 {
            return Err(ConfigError::NegativeAmount);
        }
        self.workbook.de_minimis = de_minimis;
        self.sync_cache();
        Ok(())
    }

    fn put_platforms<L: Lock>(
        &mut self,
        platforms: PlatformList,
        lock: &L,
    ) -> Result<(), ConfigError> {
        let _guard = lock.acquire()?; // (CONFIG-SETTINGS-006)
        self.workbook.platforms = platforms;
        self.sync_cache();
        Ok(())
    }

    fn put_aliases<L: Lock>(&mut self, aliases: AliasMap, lock: &L) -> Result<(), ConfigError> {
        let _guard = lock.acquire()?; // (CONFIG-SETTINGS-006)
        self.workbook.aliases = aliases;
        self.sync_cache();
        Ok(())
    }

    // @spec CONFIG-PLATFORM-003
    fn put_display_names<L: Lock>(
        &mut self,
        names: DisplayNameMap,
        lock: &L,
    ) -> Result<(), ConfigError> {
        let _guard = lock.acquire()?; // (CONFIG-SETTINGS-006)
        self.workbook.display_names = names;
        self.sync_cache();
        Ok(())
    }
}

// ===========================================================================
// Unit tests pinning the un-spec'd-precise calendar-month semantics behind the
// staleness threshold (CONFIG-STALE-002 says only "older than the configured
// threshold (default 12 months)"; it does not fix month-precise vs day-based
// arithmetic). `subtract_months` clamps the day to the target month's length
// (Mar 31 − 1mo = Feb 28/29) — a defensible interpretation that these tests lock
// so a future edit cannot silently change it, since the function is otherwise
// only exercised incidentally through `is_stale`.
// ===========================================================================
#[cfg(test)]
#[allow(clippy::inconsistent_digit_grouping)]
mod month_arithmetic {
    use super::*;

    /// Days since 1970-01-01 for a civil date — the same algorithm the staleness
    /// window uses, exposed here so the expectations read as civil dates.
    fn day(y: i32, m: i32, d: i32) -> i32 {
        days_from_civil(y, m, d)
    }

    // @spec CONFIG-STALE-002
    #[test]
    fn subtract_months_clamps_day_to_target_month_length() {
        // Mar 31 − 1 month → Feb 28 (non-leap year clamp), not "Feb 31".
        assert_eq!(subtract_months(Date(day(2025, 3, 31)), 1), Date(day(2025, 2, 28)));
        // Mar 31 − 1 month → Feb 29 in a leap year (clamp to the leap-day length).
        assert_eq!(subtract_months(Date(day(2024, 3, 31)), 1), Date(day(2024, 2, 29)));
        // Jan 31 − 1 month → Dec 31 of the prior year (cross-year, no clamp).
        assert_eq!(subtract_months(Date(day(2024, 1, 31)), 1), Date(day(2023, 12, 31)));
        // A 12-month subtraction on a non-clamped day is the same day a year back.
        assert_eq!(subtract_months(Date(day(2025, 7, 1)), 12), Date(day(2024, 7, 1)));
    }

    // @spec CONFIG-STALE-002
    #[test]
    fn staleness_boundary_is_older_than_not_at_or_older_than() {
        // A federal set verified EXACTLY 12 calendar months before `as_of` is NOT
        // stale ("older than", strict): last_verified == cutoff is fresh.
        let as_of = Date(day(2025, 7, 1));
        let exactly_12mo_prior = day(2024, 7, 1); // == subtract_months(as_of, 12)
        let mut by_year: BTreeMap<TaxYear, TaxRules> = BTreeMap::new();
        by_year.insert(
            TaxYear(2025),
            mk_rules(2025, exactly_12mo_prior),
        );
        assert!(!is_stale(
            &by_year,
            &Jurisdiction::Federal,
            TaxYear(2025),
            as_of,
            12,
        ));

        // One day older than the cutoff IS stale.
        let mut older: BTreeMap<TaxYear, TaxRules> = BTreeMap::new();
        older.insert(TaxYear(2025), mk_rules(2025, exactly_12mo_prior - 1));
        assert!(is_stale(
            &older,
            &Jurisdiction::Federal,
            TaxYear(2025),
            as_of,
            12,
        ));
    }

    /// A minimal valid federal-only rule table verified on `last_verified`.
    fn mk_rules(tax_year: i32, last_verified: i32) -> TaxRules {
        let flat = |rate: i64| BracketSet {
            rows: vec![BracketRow {
                lower_threshold_cents: Cents(0),
                rate_ppm: Ppm(rate),
            }],
            last_verified: Date(last_verified),
            source_note: "unit".to_string(),
        };
        TaxRules {
            tax_year: TaxYear(tax_year),
            filing_status: FilingStatus::MarriedFilingJointly,
            federal_ordinary: flat(370_000),
            federal_long_term: flat(200_000),
            niit: Niit {
                rate_ppm: Ppm(38_000),
                magi_threshold_cents: Cents(250_000_00),
            },
            state_ordinary: BTreeMap::new(),
            ordinary_income_cents: Cents(300_000_00),
        }
    }
}
