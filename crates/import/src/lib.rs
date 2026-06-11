//! `import` — the one-time migration from the legacy workbook into the new
//! event-log tabs.
//!
//! It RECONSTRUCTS the event stream (Buy / Vest / Sell / Split) from the legacy
//! `Stock Actions` and `Stock Sales` tabs, runs every reconstructed event through
//! the verified kernel validation (`ledger_core::validate`, the same path `entry`
//! uses), and RECONCILES the replayed result against the legacy `Positions`
//! numbers before anything is committed. See `docs/intent/import/import-design.md`
//! and `-specs.md` (prefix `IMPORT`).
//!
//! It is built around two safety disciplines:
//!
//! * **Dry-run first.** Reconstruct -> validate -> reconcile -> report, writing
//!   nothing. The owner reviews the reconciliation (and the *intended*
//!   divergences) and only then commits. (IMPORT-RECON-001/003)
//! * **Faithful except where we deliberately changed the model.** RSU basis
//!   (FMV-at-vest, not the legacy `$0`) and splits (explicit events) intentionally
//!   diverge; reconciliation classifies every difference as matched, intended
//!   (predicted independently), or unexplained (a migration bug that blocks
//!   commit). (IMPORT-RECON-002)
//!
//! TRUST BOUNDARY: the legacy `$/share` parse is the single input boundary where a
//! decimal string becomes integer `Cents`; no float lives past it. The
//! reconstructed events are plain `ledger_core::LedgerEvent`/`tax::TaxEvent`
//! values, validated by the kernel — so the imported log is valid by construction
//! (IMPORT-RUN-003). `import` is one-time tooling, NOT a `verus!{}` crate;
//! `cargo test` is the gate, with the reconciliation as its correctness proof.

use std::collections::BTreeMap;

use config::{ResidencyTimeline, StateCode};
use ledger_core::{LedgerEvent, Marks, Symbol};
use pt_core::{Cents, Date, MicroShares};
use store::{Cache, Lock, SheetsClient, Store};
use tax::TaxEvent;

/// Re-export `config`'s tax-jurisdiction and tax-year types: an `import` caller
/// (and the closed-year seed) names them, so they ride the `import` surface.
pub use config::{Jurisdiction, TaxYear};

// ===========================================================================
// Legacy source model (import-design.md → "Source Mapping"). The SYNTHETIC
// in-memory fixture the tests build; the real workbook is parsed into the same
// shape for a manual run. A legacy `$/share` is carried as a decimal STRING — the
// single input boundary where it crosses into integer `Cents` (no float past it).
// ===========================================================================

/// A spreadsheet cell coordinate (e.g. tab `"Stock Actions"`, row `42`). Part of
/// the guaranteed-unique `EventId` key (row coordinate + legacy id), so a reused
/// legacy tranche id can never silently collide. (IMPORT-RUN-002)
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct RowCoord {
    /// The legacy tab the row came from (`"Stock Actions"` / `"Stock Sales"`).
    pub tab: String,
    /// The 1-based spreadsheet row number.
    pub row: u32,
}

impl RowCoord {
    pub fn new(tab: impl Into<String>, row: u32) -> Self {
        RowCoord {
            tab: tab.into(),
            row,
        }
    }
}

/// Which kind of opening action a `Stock Actions` row records. (IMPORT-MAP-001/002)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LegacyActionKind {
    /// A purchase — reconstructs a `Buy` (basis from `$/share` + fees).
    Buy,
    /// An RSU vest — reconstructs a `Vest` (FMV recovered from `$/share`).
    Vest,
}

/// One legacy `Stock Actions` row (a Buy or a Vest opening a tranche). The legacy
/// `$/share` decimal is a STRING so the parse boundary is explicit; for a Vest it
/// is the vest FMV/share (the legacy `Total Cost` is `$0`, deliberately ignored).
/// (IMPORT-MAP-001/002, IMPORT-CORP-003)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LegacyActionRow {
    /// The spreadsheet coordinate (part of the unique `EventId` key).
    pub coord: RowCoord,
    /// The legacy tranche id — becomes the reconstructed lot id. (IMPORT-MAP-001)
    pub tranche_id: String,
    pub symbol: Symbol,
    pub kind: LegacyActionKind,
    /// As-of-row-date share count (the share-frame rule: own row date frame).
    /// (IMPORT-CORP-001)
    pub qty: MicroShares,
    /// The legacy `$/share` as a decimal string (e.g. `"123.45"`); the input
    /// boundary into `Cents`. For a Vest this is the FMV/share. A `$0`, blank, or
    /// `#DIV/0!` Vest source is unrecoverable. (IMPORT-MAP-002, IMPORT-CORP-003)
    pub dollars_per_share: String,
    /// The legacy fees as a decimal string (`"0"` when none). Buys only.
    pub fees_dollars: String,
    pub date: Date,
    pub platform: String,
    /// Provenance carried onto the reconstructed event's tracking code — e.g.
    /// `"exercise"` for a legacy option-exercise row that reconstructs as a Buy
    /// (the cash paid at the strike IS the basis; the note preserves what kind
    /// of acquisition it was). `None` for a plain Buy/Vest. (IMPORT-MAP-005)
    pub tracking_code: Option<String>,
}

/// One legacy `Stock Sales` row (a specific-ID disposal of a referenced tranche).
/// A sell-to-cover `-a`/`-b` child references its vest tranche; it reconstructs as
/// an ordinary Sell at vest-date price. (IMPORT-MAP-002/003)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LegacySaleRow {
    pub coord: RowCoord,
    /// A locally-unique tag for THIS sale (becomes the reconstructed sale id /
    /// part of the `EventId` key). Distinct from the tranche it sells.
    pub sale_tag: String,
    /// The legacy tranche id this sale disposes (specific-ID lot reference).
    /// Referential integrity against `Stock Actions` is checked in the pre-pass.
    /// (IMPORT-MAP-003, IMPORT-RUN-002)
    pub tranche_id: String,
    pub symbol: Symbol,
    /// As-of-sale share count (own row date frame). (IMPORT-CORP-001)
    pub qty: MicroShares,
    /// The legacy `$/share` as a decimal string; the input boundary into `Cents`.
    pub dollars_per_share: String,
    pub fees_dollars: String,
    pub date: Date,
    pub platform: String,
}

/// One legacy `Positions` row — a RECONCILE TARGET ONLY, never a reconstruction
/// source (IMPORT-MAP-004). The importer recomputes the position by replay and
/// compares against these legacy figures. Derived columns (`rm.shs`, `Tax`,
/// `Unr.*`, `#DIV/0!`) are not modeled — they are ignored.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LegacyPositionRow {
    pub symbol: Symbol,
    /// Legacy as-of-today share count (live frame).
    pub shares: MicroShares,
    /// Legacy realized P&L for the symbol, in `Cents`.
    pub realized_pnl_cents: Cents,
    /// Legacy unrealized for the symbol, in `Cents` (the `Positions` figure).
    pub unrealized_cents: Cents,
}

/// An owner-supplied known corporate action (e.g. the AMZN 20:1 on 2022-06-06): a
/// `Split` is reconstructed and inserted at its date in `Seq` order so pre-split
/// lots rescale before post-split sales consume them. Known actions are importer
/// INPUT, never guessed from the data. (IMPORT-CORP-002)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct KnownCorporateAction {
    pub symbol: Symbol,
    pub date: Date,
    /// `ratio_num : ratio_den` (AMZN 20:1 is `20 : 1`). Both `>= 1`.
    pub ratio_num: i64,
    pub ratio_den: i64,
}

/// An owner-confirmed closed year: filing deadline passed AND owner-confirmed paid
/// (owner input, not clock-inferred). Seeded as a single combined migration
/// accrual per `(jurisdiction, tax_year)`, set to `legacy_actual_cents` via
/// `AmountOverride`, then driven `Allocate -> Move -> Pay` (outstanding = 0).
/// (IMPORT-TAX-002)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ClosedYear {
    pub jurisdiction: Jurisdiction,
    pub tax_year: TaxYear,
    /// The legacy actual tax paid for the `(jurisdiction, tax_year)`, in `Cents`.
    pub legacy_actual_cents: Cents,
    /// The reserve account label the migration accrual is allocated to.
    pub account_label: String,
}

/// A per-row correction override fed back into a RE-RUN: the owner overrides a
/// row's date / qty / share-frame so a reconstruction that failed kernel
/// validation (e.g. a post-split qty on a pre-split-dated sale) can be resolved
/// WITHOUT editing the canonical log. Keyed by the offending row's coordinate.
/// (IMPORT-CORP-004)
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct RowCorrection {
    /// Override the row's date.
    pub date: Option<Date>,
    /// Override the row's qty (in the row's own share-frame).
    pub qty: Option<MicroShares>,
    /// Re-interpret the row's qty as ALREADY in the post-split frame (so the
    /// importer does not re-apply the split to it). The share-frame escape hatch.
    pub already_post_split: bool,
}

/// The synthetic legacy workbook: the three portfolio tabs plus the owner inputs
/// (known corporate actions, closed years, the historical residency timeline, and
/// any per-row corrections). The real workbook is parsed into this same shape for
/// a manual run; ALL tests build it in memory. The legacy workbook's
/// non-portfolio tabs (an owner-configured out-of-scope list) are not modeled.
/// (IMPORT-RUN-009)
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct LegacyWorkbook {
    /// `Stock Actions` rows (Buys and Vests).
    pub actions: Vec<LegacyActionRow>,
    /// `Stock Sales` rows.
    pub sales: Vec<LegacySaleRow>,
    /// `Positions` rows — reconcile target only. (IMPORT-MAP-004)
    pub positions: Vec<LegacyPositionRow>,
    /// Owner-supplied known corporate actions. (IMPORT-CORP-002)
    pub corporate_actions: Vec<KnownCorporateAction>,
    /// Owner-confirmed closed years. (IMPORT-TAX-002)
    pub closed_years: Vec<ClosedYear>,
    /// The owner's historical residency timeline (with a founding entry). Stamps
    /// each historical Sell from `residency_on(sale_date)`. (IMPORT-TAX-001)
    pub residency: ResidencyTimeline,
    /// `true` when the founding residency entry is an owner-asserted best-guess
    /// (flagged), recorded because the oldest sale predates the earliest recalled
    /// move. (IMPORT-TAX-001)
    pub founding_residency_asserted: bool,
    /// Per-row corrections keyed by the offending row's coordinate. (IMPORT-CORP-004)
    pub corrections: BTreeMap<RowCoord, RowCorrection>,
    /// Owner-adjudicated divergences: symbol → the owner's stated reason. An
    /// otherwise-Unexplained dollar residual on a declared symbol is recorded as
    /// owner-adjudicated (residual + reason in the report) and does not block;
    /// share residuals are never adjudicable. (IMPORT-RECON-008)
    pub adjudicated: BTreeMap<Symbol, String>,
}

// ===========================================================================
// Reconstruction output (import-design.md → "Source Mapping" / "Run Mechanics").
// The reconstructed event stream plus the per-row provenance so a reconciliation
// or commit can trace each event to its legacy origin.
// ===========================================================================

/// One reconstructed ledger event with its provenance: the deterministic
/// `EventId` (row coordinate + legacy id), the legacy row it came from, and the
/// event itself. The `Seq` on the event is the importer's *intended* fold order
/// (dense, 1-based); `store` re-derives the persisted `Seq` on append, but the
/// reconstructed order is preserved so the committed log folds as the dry-run
/// reconciled. (IMPORT-RUN-001/002)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ReconstructedEvent {
    /// The deterministic `EventId` from the unique key (row coord + legacy id).
    pub event_id: String,
    /// The legacy row coordinate this event was reconstructed from (`None` for a
    /// synthetic `Split`, which has no source row — it carries its action's date).
    pub source: Option<RowCoord>,
    pub event: LedgerEvent,
}

/// The full reconstruction: the ledger events (Buy/Vest/Sell/Split) in intended
/// `Seq` order, the tax events (the closed-year migration lifecycle), and the
/// list of malformed source rows surfaced for manual review (never dropped).
/// (IMPORT-MAP-*, IMPORT-CORP-*, IMPORT-TAX-002, IMPORT-RUN-011)
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Reconstruction {
    /// Reconstructed ledger events in intended fold order (Splits inserted by date
    /// in `Seq` order). (IMPORT-CORP-002)
    pub ledger: Vec<ReconstructedEvent>,
    /// The closed-year migration tax-event lifecycle (one combined accrual per
    /// `(jurisdiction, tax_year)`: `AmountOverride -> Allocate -> Move -> Pay`).
    /// (IMPORT-TAX-002)
    pub tax: Vec<TaxEvent>,
    /// Genuinely malformed source rows, listed for manual review rather than
    /// silently dropped. (IMPORT-RUN-011)
    pub malformed: Vec<MalformedRow>,
    /// Per-symbol Σ FMV value of sell-to-cover shares (sales whose `$0` price
    /// was substituted with the vest-date FMV, `IMPORT-MAP-002`). The
    /// reconciliation prediction subtracts this: an STC consumes its FMV basis
    /// at FMV proceeds (gain 0) on BOTH sides, so its basis never contributes
    /// to the legacy-vs-new divergence. (IMPORT-RECON-002)
    pub stc_fmv_value_cents: BTreeMap<Symbol, i64>,
}

/// A genuinely malformed source row, surfaced for manual review. Distinct from a
/// `#DIV/0!`/`#N/A`/blank cell in a DERIVED column (which is simply ignored).
/// (IMPORT-RUN-011)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MalformedRow {
    pub coord: RowCoord,
    pub reason: String,
}

// ===========================================================================
// Error model (import-design.md → "Run Mechanics" / "Corporate-Action & RSU-Basis
// Reconstruction"). One variant per surfaced hard error. A hard error blocks the
// import (dry-run report or commit) until resolved; it never silently drops data.
// ===========================================================================

/// A hard error the importer surfaces. Every variant blocks the run until the
/// owner resolves it (a correction-override re-run, a manual entry, an owner-
/// supplied FMV, or a fix to the legacy source).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ImportError {
    /// A legacy tranche id appears on more than one `Stock Actions` row. Surfaced
    /// rather than silently coalesced (a reused id would drop a real event).
    /// (IMPORT-RUN-002)
    DuplicateTrancheId { tranche_id: String },
    /// A store failure while appending a SPECIFIC event during commit — carries
    /// the event id so a live failure names which append tripped (a resume
    /// investigates that exact row/event rather than guessing).
    StoreAt {
        event_id: String,
        error: store::StoreError,
    },
    /// A `Stock Sales` row references a tranche id with no matching `Stock Actions`
    /// row (referential-integrity break). (IMPORT-RUN-002)
    MissingReferencedTranche { coord: RowCoord, tranche_id: String },
    /// Two owner-supplied corporate actions share the same `(symbol, date, ratio)`:
    /// surfaced rather than silently coalesced, so the deterministic `Split`
    /// `EventId` (derived from that owner-action key) is provably unique without
    /// relying on enumeration order. (IMPORT-RUN-002)
    DuplicateCorporateAction { symbol: Symbol, date: Date },
    /// A Vest row's FMV source (`$/share`) is `$0`, blank, or `#DIV/0!`: the FMV
    /// cannot be recovered, so the importer requires an owner-supplied FMV rather
    /// than defaulting to `$0`. (IMPORT-CORP-003)
    UnrecoverableVestFmv { coord: RowCoord },
    /// A reconstructed event failed kernel validation; resolvable via a per-row
    /// correction-override re-run, never by editing the canonical log. Carries the
    /// offending row (when known) and the kernel rejection. (IMPORT-CORP-004)
    KernelRejected {
        coord: Option<RowCoord>,
        error: ledger_core::LedgerError,
    },
    /// A reconstructed migration TAX event failed the tax kernel's validation gate
    /// (`tax::validate_event`, the same path an `entry` tax append uses): the
    /// imported tax log is not valid by construction. (IMPORT-RUN-003)
    TaxKernelRejected { error: tax::TaxError },
    /// A position present ONLY as a `Positions` aggregate (no surviving Buy/Sell
    /// rows): the stream cannot be fabricated, so it is surfaced for manual entry,
    /// never migrated as empty. (IMPORT-RUN-004)
    PositionWithoutSourceRows { symbol: Symbol },
    /// The import target workbook already holds events that are NOT this import's
    /// own deterministic events: a fresh/own-events-only target is required.
    /// (IMPORT-RUN-001)
    TargetNotEmpty,
    /// A commit was attempted on a report the dry-run safety gate BLOCKS: an
    /// unexplained dollar divergence, a flagged share residual, and/or an
    /// outstanding malformed source row (listed for review, not migrated — so a
    /// commit would silently drop its data). Distinct from `TargetNotEmpty` (a
    /// write-target condition): this is the reconciliation safety gate refusing the
    /// write until the migration bug is fixed and the owner can accept a clean
    /// reconciliation. Carries the blocking causes. (IMPORT-RECON-003, IMPORT-RUN-011)
    CommitBlockedByReconciliation {
        /// Symbols whose dollar verdict is `Unexplained`.
        unexplained_symbols: Vec<Symbol>,
        /// Symbols whose share verdict is `Flagged`.
        flagged_symbols: Vec<Symbol>,
        /// Coordinates of malformed source rows still outstanding (listed for
        /// manual review, never migrated). (IMPORT-RUN-011)
        malformed_rows: Vec<RowCoord>,
    },
    /// A commit was attempted without the owner's explicit acceptance: the typed
    /// `AcceptedReport` token was in its under-review (un-accepted) state. DISTINCT
    /// from `CommitBlockedByReconciliation` (the auto-computed safety gate refusing
    /// the write): this is the OWNER-ACCEPTANCE gate refusing the write because the
    /// owner has not accepted — the gate being clear never implies acceptance.
    /// (IMPORT-RECON-005)
    OwnerAcceptanceRequired,
    /// The founding-residency precondition failed (no residency entry at or before
    /// the earliest event date). Surfaced from `config`. (IMPORT-TAX-001)
    MissingFoundingResidency,
    /// A monetary/quantity field could not be parsed at the input boundary, or an
    /// owner input was internally inconsistent. (IMPORT-RUN-011)
    Malformed { coord: RowCoord, reason: String },
    /// A failure crossing the `store` trust seam during commit. (IMPORT-RUN-001)
    Store(store::StoreError),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ImportError {}

// ===========================================================================
// Reconciliation model (import-design.md → "Dry-Run & Reconciliation";
// IMPORT-RECON-002/004). Per symbol: matched / intended-divergence / unexplained,
// plus the share reconciliation.
// ===========================================================================

/// The classification of one symbol's dollar reconciliation. (IMPORT-RECON-002)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Verdict {
    /// Within tolerance: the per-position tolerance is `ceil(½¢ × consumed-lot
    /// count)`, plus a portfolio-total tolerance. (IMPORT-RECON-002)
    Matched,
    /// The residual is within an INDEPENDENTLY PREDICTED, symbol-aggregate
    /// intended delta (RSU-basis = Σ vest FMV value for vested shares; split = `$0`).
    /// `predicted` is that delta; the residual lies within `predicted ± tolerance`.
    /// (IMPORT-RECON-002)
    IntendedDivergence {
        /// The independently predicted intended delta, in `Cents` (new − legacy).
        predicted_cents: Cents,
        /// A human-readable attribution (e.g. RSU FMV-basis, explicit split).
        explanation: String,
    },
    /// An unattributed residual beyond `predicted ± tolerance` — a migration bug
    /// that BLOCKS commit until fixed. (IMPORT-RECON-002/003)
    Unexplained {
        /// The residual beyond what was predicted + tolerance, in `Cents`.
        residual_cents: Cents,
    },
    /// An owner-ADJUDICATED divergence: the residual is real, but the owner has
    /// ruled on it (a known-wrong legacy cell, sub-dollar legacy rounding) via
    /// the owner inputs. The residual and the owner's stated reason are both
    /// recorded — the commit record carries why — and it does not block.
    /// (IMPORT-RECON-008)
    OwnerAdjudicated {
        residual_cents: Cents,
        /// The owner's stated reason, verbatim from the owner inputs.
        reason: String,
    },
}

/// How a symbol's reconstructed shares reconciled against the legacy `Positions`
/// share count. (IMPORT-RECON-004)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ShareVerdict {
    /// Reconstructed shares match legacy within the micro-share tolerance.
    Matched,
    /// A sub-threshold residual on a CLOSED position, snapped via a closing
    /// adjustment (recorded, not silently absorbed). (IMPORT-RECON-004)
    SnappedClosingAdjustment { adjustment: MicroShares },
    /// A larger share residual — flagged for review. (IMPORT-RECON-004)
    Flagged { residual: MicroShares },
}

/// One symbol's full reconciliation line: the legacy vs. reconstructed figures,
/// the dollar verdict, and the share verdict. (IMPORT-RECON-002/004)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SymbolReconciliation {
    pub symbol: Symbol,
    /// Reconstructed (replayed) share count.
    pub reconstructed_shares: MicroShares,
    /// Legacy `Positions` share count.
    pub legacy_shares: MicroShares,
    /// Reconstructed realized P&L, in `Cents`.
    pub reconstructed_realized_cents: Cents,
    /// Legacy realized P&L, in `Cents`.
    pub legacy_realized_cents: Cents,
    /// Reconstructed unrealized, in `Cents` (`None` when the symbol is unmarked).
    pub reconstructed_unrealized_cents: Option<Cents>,
    /// Legacy unrealized, in `Cents`.
    pub legacy_unrealized_cents: Cents,
    /// The number of consumed lots feeding this symbol's tolerance (`½¢ × count`).
    pub consumed_lot_count: u32,
    pub verdict: Verdict,
    pub share_verdict: ShareVerdict,
}

/// The dry-run report: the reconstruction, the per-symbol reconciliation, and
/// whether commit is permitted (blocked by ANY unexplained divergence or flagged
/// share residual). Writing nothing is the default; commit is a separate explicit
/// step gated on `commit_allowed`. (IMPORT-RECON-001/002/003)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DryRunReport {
    pub reconstruction: Reconstruction,
    pub symbols: Vec<SymbolReconciliation>,
    /// `false` when any symbol is `Unexplained` (or a share residual is `Flagged`):
    /// commit is blocked until the migration bug is fixed. (IMPORT-RECON-003)
    pub commit_allowed: bool,
}

impl DryRunReport {
    /// Record the OWNER'S EXPLICIT ACCEPTANCE of this reconciliation, producing the
    /// typed `AcceptedReport` token `commit` requires before any write. This token
    /// is DISTINCT from the auto-computed `commit_allowed` safety gate: it carries
    /// the owner's decision (a human act), never the machine's blocking computation.
    /// The two gates are independently satisfied — accepting a report whose gate is
    /// still blocked does NOT bypass the gate (`commit` re-checks `commit_allowed`),
    /// and a clear gate does NOT stand in for acceptance (`commit` refuses an
    /// unaccepted token). (IMPORT-RECON-005)
    ///
    // @spec IMPORT-RECON-005
    pub fn accept(&self) -> AcceptedReport<'_> {
        AcceptedReport {
            report: self,
            accepted: true,
        }
    }

    /// Produce the `AcceptedReport` token in its UN-ACCEPTED (under-review) state:
    /// the owner has the report in hand but has NOT yet accepted it. A commit on
    /// this token is refused with `ImportError::OwnerAcceptanceRequired`, proving the
    /// acceptance gate is independent of (and never implied by) `commit_allowed`.
    /// (IMPORT-RECON-005)
    ///
    // @spec IMPORT-RECON-005
    pub fn review(&self) -> AcceptedReport<'_> {
        AcceptedReport {
            report: self,
            accepted: false,
        }
    }
}

/// The typed owner-acceptance token `commit` requires before any write. It binds an
/// explicit owner-acceptance DECISION to a specific `DryRunReport`, and is the ONLY
/// way to present a report to `commit`. It is DISTINCT from the report's auto-
/// computed `commit_allowed` safety gate (IMPORT-RECON-003/006): that gate is the
/// machine asserting "no commit-blocking condition"; this token is the owner
/// asserting "I have reviewed and accept this reconciliation". `commit` requires
/// BOTH to be independently satisfied — the gate being clear never implies
/// acceptance, and acceptance never bypasses the gate. Construct it via
/// `DryRunReport::accept` (accepted) or `DryRunReport::review` (under review, not
/// yet accepted). (IMPORT-RECON-005)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AcceptedReport<'a> {
    report: &'a DryRunReport,
    /// The owner's explicit acceptance decision: `true` once the owner accepts.
    accepted: bool,
}

impl<'a> AcceptedReport<'a> {
    /// The reconciliation report this acceptance token binds.
    pub fn report(&self) -> &'a DryRunReport {
        self.report
    }

    /// Whether the owner has explicitly accepted (the acceptance gate, distinct from
    /// the report's `commit_allowed` safety gate). (IMPORT-RECON-005)
    pub fn is_accepted(&self) -> bool {
        self.accepted
    }
}

// ===========================================================================
// Tolerances (import-design.md → "Dry-Run & Reconciliation"). The kernel's
// largest-remainder noise is ±½¢ per consumed lot; the share tolerance bounds the
// micro-share reconciliation snap.
// ===========================================================================

/// Half a cent, in the kernel's largest-remainder noise unit. The per-position
/// dollar tolerance is `ceil(HALF_CENT_NUM × consumed-lot count / HALF_CENT_DEN)`
/// rounded up to whole cents. (IMPORT-RECON-002)
pub const HALF_CENT_NUM: i64 = 1;
pub const HALF_CENT_DEN: i64 = 2;

/// The portfolio-total dollar tolerance added on top of the per-position sum, in
/// `Cents`. (IMPORT-RECON-002)
pub const PORTFOLIO_TOLERANCE_CENTS: i64 = 100;

/// The micro-share reconciliation tolerance: a residual at or below this snaps (on
/// a closed position) or is within-match; a larger one is flagged. (IMPORT-RECON-004)
pub const SHARE_TOLERANCE_MICRO: i64 = 1_000; // 0.001 share

// ===========================================================================
// Input-boundary parsing (import-design.md → "Source Mapping"; the single place
// a legacy decimal `$/share`/fees string crosses into integer `Cents`). No float
// past here. A `#DIV/0!` / `#N/A` / blank source is recognized as unrecoverable.
// ===========================================================================

/// Parse a legacy decimal-dollar string (e.g. `"123.45"`, `"0.001"`, `"1000"`)
/// into integer `Cents`, exactly, with no float. Returns `None` for a blank,
/// `"#DIV/0!"`, `"#N/A"`, or otherwise unparseable cell (the unrecoverable
/// signal). The boundary is one rounding-free decimal-to-cents conversion (at most
/// two fractional digits are significant; more are an error, not silent
/// truncation). (IMPORT-CORP-003, IMPORT-RUN-011)
pub fn parse_dollars_to_cents(s: &str) -> Option<Cents> {
    let t = s.trim();
    // The unrecoverable / blank signals: an empty cell or a spreadsheet error.
    if t.is_empty() || t.starts_with('#') {
        return None;
    }
    // Strip a leading sign, an optional `$`, and thousands separators, then split
    // on the single decimal point. No float ever materializes.
    let (neg, body) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t),
    };
    let body = body.trim_start().trim_start_matches('$').replace(',', "");
    let mut parts = body.splitn(2, '.');
    let whole_str = parts.next().unwrap_or("");
    let frac_str = parts.next().unwrap_or("");
    // The whole part must be all digits (allow an empty whole, e.g. ".50").
    if whole_str.chars().any(|c| !c.is_ascii_digit())
        || frac_str.chars().any(|c| !c.is_ascii_digit())
    {
        return None;
    }
    if whole_str.is_empty() && frac_str.is_empty() {
        return None;
    }
    let whole: i64 = if whole_str.is_empty() {
        0
    } else {
        whole_str.parse().ok()?
    };
    // At most two fractional digits are significant; more than two non-zero
    // fractional digits cannot be represented exactly in cents → malformed, not a
    // silent truncation. Pad/normalize to exactly two cents digits.
    let cents_frac: i64 = match frac_str.len() {
        0 => 0,
        1 => frac_str.parse::<i64>().ok()? * 10,
        2 => frac_str.parse::<i64>().ok()?,
        _ => {
            // Trailing digits beyond the second must all be zero, else it cannot be
            // represented exactly in cents.
            let (head, tail) = frac_str.split_at(2);
            if tail.chars().any(|c| c != '0') {
                return None;
            }
            head.parse::<i64>().ok()?
        }
    };
    let magnitude = whole.checked_mul(100)?.checked_add(cents_frac)?;
    Some(Cents(if neg { -magnitude } else { magnitude }))
}

// ===========================================================================
// EventId derivation (import-design.md → "Run Mechanics"; IMPORT-RUN-002). From a
// GUARANTEED-UNIQUE key (row coordinate + legacy id), NOT the id alone, so a
// reused legacy id can never silently collide and drop a real event.
// ===========================================================================

/// Derive a deterministic `EventId` from the guaranteed-unique key (row coordinate
/// + legacy id). Stable across re-runs (so `store`'s idempotency dedups a partial-
/// commit resume); unique even when a legacy id is reused on two rows, because the
/// row coordinate disambiguates. (IMPORT-RUN-001/002)
///
// @spec IMPORT-RUN-002
pub fn derive_event_id(coord: &RowCoord, legacy_id: &str) -> String {
    // The key is `tab!row#legacy_id` — `!` and `#` are absent from tab names, row
    // numbers, and the legacy ids, so the join is injective: no two distinct
    // (coord, legacy_id) triples can produce the same string, hence no silent
    // collision even when `legacy_id` is reused across rows. Deterministic in its
    // inputs, so a re-run yields the same id (store idempotency dedups a resume).
    format!("imp-{}!{}#{}", coord.tab, coord.row, legacy_id)
}

// ===========================================================================
// Pre-pass (import-design.md → "Run Mechanics"; IMPORT-RUN-002). Validate legacy
// tranche-id uniqueness and Sell -> tranche referential integrity BEFORE
// reconstruction; surface a duplicate id or a missing referenced tranche.
// ===========================================================================

/// The unique-id / referential-integrity pre-pass. Validates that every legacy
/// tranche id is unique across `Stock Actions`, and that every `Stock Sales` row
/// references an existing tranche. `Ok(())` ⇒ reconstruction may proceed; `Err`
/// surfaces the first defect. (IMPORT-RUN-002)
///
// @spec IMPORT-RUN-002
pub fn pre_pass(wb: &LegacyWorkbook) -> Result<(), ImportError> {
    // Tranche-id uniqueness across `Stock Actions` (a reused id would silently
    // drop a real event downstream). (IMPORT-RUN-002)
    let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for a in &wb.actions {
        if !seen.insert(a.tranche_id.as_str()) {
            return Err(ImportError::DuplicateTrancheId {
                tranche_id: a.tranche_id.clone(),
            });
        }
    }
    // Sell -> tranche referential integrity: every sale references an existing
    // tranche. (IMPORT-RUN-002)
    for s in &wb.sales {
        if !seen.contains(s.tranche_id.as_str()) {
            return Err(ImportError::MissingReferencedTranche {
                coord: s.coord.clone(),
                tranche_id: s.tranche_id.clone(),
            });
        }
    }
    // Corporate-action uniqueness on (symbol, date, ratio): the synthetic Split
    // EventId derives from this owner-action key, so a duplicated action would
    // collide. Surfaced rather than silently coalesced, so the deterministic Split
    // id is provably unique without relying on Vec enumeration order. (IMPORT-RUN-002)
    let mut ca_seen: std::collections::BTreeSet<(String, i32, i64, i64)> =
        std::collections::BTreeSet::new();
    for ca in &wb.corporate_actions {
        let key = (ca.symbol.clone(), ca.date.0, ca.ratio_num, ca.ratio_den);
        if !ca_seen.insert(key) {
            return Err(ImportError::DuplicateCorporateAction {
                symbol: ca.symbol.clone(),
                date: ca.date,
            });
        }
    }
    Ok(())
}

// ===========================================================================
// Share-frame precondition (import-design.md → "Corporate-Action & RSU-Basis
// Reconstruction"; IMPORT-CORP-001). Every legacy quantity is interpreted in the
// share-frame of its OWN row date; the legacy remaining-shares column (a derived,
// live-frame figure) is NEVER consulted. The importer asserts this as a CHECKED
// precondition.
// ===========================================================================

/// Assert the share-frame rule as a checked precondition (IMPORT-CORP-001): every
/// reconstructed quantity is the row's OWN-frame `qty` (or a correction's qty),
/// and the legacy remaining-shares column is provably never consulted.
///
/// The structural invariant: `LegacyActionRow`/`LegacySaleRow` model only an
/// own-frame `qty` field — there is NO remaining-shares column on the source model
/// to read — so a quantity can only ever be that row's own-frame count. The
/// explicit `Split` (IMPORT-CORP-002) is what bridges a pre-split open into the
/// post-split frame; mixing a live remaining-shares figure into a row's frame is
/// the silent 20x error this rule forbids. This function makes the rule an
/// explicit, callable gate (rather than an implicit comment) and the only place a
/// future frame hazard would be detected: a degenerate **zero-share** open or sale
/// carries no own-frame quantity to interpret, so it is surfaced as `Malformed`
/// (a frame-ambiguous row) rather than reconstructed as an unframeable event.
///
/// A genuine pre/post-split-frame MISMATCH (e.g. a pre-split-dated sale carrying a
/// post-split qty) is not detectable from the row alone — it is caught downstream
/// by the kernel (`validate_reconstruction` → `KernelRejected`) and resolved via a
/// per-row correction (IMPORT-CORP-004).
///
// @spec IMPORT-CORP-001, IMPORT-CORP-005
pub fn assert_share_frame_precondition(wb: &LegacyWorkbook) -> Result<(), ImportError> {
    // Every quantity used is the row's own-frame qty (post-correction). A zero-share
    // open/sale has no own-frame count to interpret in any frame → frame-ambiguous.
    for a in &wb.actions {
        let qty = wb
            .corrections
            .get(&a.coord)
            .and_then(|c| c.qty)
            .unwrap_or(a.qty);
        if qty.0 == 0 {
            return Err(ImportError::Malformed {
                coord: a.coord.clone(),
                reason: "zero-share action row carries no own-frame quantity (frame-ambiguous)"
                    .to_string(),
            });
        }
    }
    for s in &wb.sales {
        let qty = wb
            .corrections
            .get(&s.coord)
            .and_then(|c| c.qty)
            .unwrap_or(s.qty);
        if qty.0 == 0 {
            return Err(ImportError::Malformed {
                coord: s.coord.clone(),
                reason: "zero-share sale row carries no own-frame quantity (frame-ambiguous)"
                    .to_string(),
            });
        }
    }
    Ok(())
}

// ===========================================================================
// Reconstruction (import-design.md → "Source Mapping" / "Corporate-Action &
// RSU-Basis Reconstruction" / "Historical Tax & Residency"). The heart of the
// importer: legacy rows -> validated event stream.
// ===========================================================================

/// Reconstruct the event stream from the legacy workbook. The pipeline:
///
/// 1. Pre-pass (IMPORT-RUN-002) — surfaced before reconstruction.
/// 2. Buys/Vests from `Stock Actions` (lot id = tranche id; Buy basis from
///    `$/share` + fees; Vest FMV recovered from `$/share`, hard-error on a `$0`/
///    blank/`#DIV/0!` source). (IMPORT-MAP-001/002, IMPORT-CORP-003)
/// 3. Sells from `Stock Sales`, specific-ID against the referenced tranche; sell-
///    to-cover `-a`/`-b` children are ordinary Sells. (IMPORT-MAP-002/003)
/// 4. A `Split` per owner-supplied corporate action, inserted at its date in `Seq`
///    order so pre-split lots rescale before post-split sales consume them; every
///    qty is read in its own row-date share-frame (the legacy remaining column is
///    ignored — the precondition is asserted up front by
///    `assert_share_frame_precondition`). (IMPORT-CORP-001/002)
/// 5. The closed-year migration tax lifecycle (`AmountOverride -> Allocate -> Move
///    -> Pay`, one combined accrual per `(jurisdiction, tax_year)`). (IMPORT-TAX-002)
/// 6. A `Positions`-only symbol (no surviving Buy/Sell rows) is a hard error
///    (IMPORT-RUN-004); a genuinely malformed row is collected, not dropped
///    (IMPORT-RUN-011).
///
/// Per-row corrections (`wb.corrections`) are applied as date/qty/frame overrides
/// for a re-run (IMPORT-CORP-004). The reconstructed events are NOT yet
/// kernel-validated here — `validate_reconstruction` is the gate.
///
// @spec IMPORT-MAP-001, IMPORT-MAP-002, IMPORT-MAP-003, IMPORT-MAP-004
// @spec IMPORT-CORP-001, IMPORT-CORP-002, IMPORT-CORP-003, IMPORT-CORP-004
// @spec IMPORT-TAX-001, IMPORT-TAX-002, IMPORT-RUN-002, IMPORT-RUN-004, IMPORT-RUN-005
// @spec IMPORT-RUN-010, IMPORT-RUN-011
pub fn reconstruct(wb: &LegacyWorkbook) -> Result<Reconstruction, ImportError> {
    use ledger_core::{LedgerEvent, LedgerEventKind, LotRef};
    use pt_core::Seq;

    // 1. The unique-id / referential-integrity pre-pass (IMPORT-RUN-002).
    pre_pass(wb)?;

    // 1b. The share-frame precondition (IMPORT-CORP-001): every quantity is read in
    //     the share-frame of its own row date; the legacy remaining-shares column is
    //     never consulted. Asserted as a checked precondition here.
    assert_share_frame_precondition(wb)?;

    // 2. The founding-residency precondition: the timeline must cover the earliest
    //    SALE date so `residency_on(sale_date)` is never queried in its pre-history
    //    region. Residency stamps only Sells (IMPORT-TAX-001), so the precondition
    //    keys on the oldest sale — an early Buy with later Sells is not blocked —
    //    routing through config's founding-entry rule.
    if let Some(d) = earliest_sale_date(wb) {
        config::check_import_founding_residency(&wb.residency, d)
            .map_err(|_| ImportError::MissingFoundingResidency)?;
    }

    // A reconstruction item carries its sort key (date, kind-rank, coord) and the
    // event-to-be. Kind rank orders same-date opens (0) before splits (1) before
    // sells (2), so a pre-split open rescales before a post-split sale consumes.
    struct Item {
        date: Date,
        rank: u8,
        coord_key: (String, u32),
        source: Option<RowCoord>,
        event_id: Option<String>,
        kind: LedgerEventKind,
    }
    let mut items: Vec<Item> = Vec::new();
    let mut malformed: Vec<MalformedRow> = Vec::new();

    // --- Opens: Buys and Vests from `Stock Actions`. (IMPORT-MAP-001/002) ---
    // Vest FMV per tranche: the sell-to-cover price substitution's source
    // (IMPORT-MAP-002 — an STC child sells at the vest-date price).
    let mut vest_fmv: BTreeMap<String, Cents> = BTreeMap::new();
    // Per-symbol Σ FMV value of STC-substituted shares (see Reconstruction).
    let mut stc_fmv_value_cents: BTreeMap<Symbol, i64> = BTreeMap::new();

    for a in &wb.actions {
        let corr = wb.corrections.get(&a.coord);
        let date = corr.and_then(|c| c.date).unwrap_or(a.date);
        let qty = corr.and_then(|c| c.qty).unwrap_or(a.qty);

        match a.kind {
            LegacyActionKind::Buy => {
                let unit_price = match parse_dollars_to_cents(&a.dollars_per_share) {
                    Some(c) => c,
                    None => {
                        // A genuinely malformed price on a Buy: list for manual
                        // review, never silently drop. (IMPORT-RUN-011)
                        malformed.push(MalformedRow {
                            coord: a.coord.clone(),
                            reason: format!(
                                "unparseable $/share {:?} on a Buy row",
                                a.dollars_per_share
                            ),
                        });
                        continue;
                    }
                };
                let fees = parse_dollars_to_cents(&a.fees_dollars).unwrap_or(Cents(0));
                items.push(Item {
                    date,
                    rank: 0,
                    coord_key: (a.coord.tab.clone(), a.coord.row),
                    source: Some(a.coord.clone()),
                    event_id: Some(derive_event_id(&a.coord, &a.tranche_id)),
                    kind: LedgerEventKind::Buy {
                        lot_id: a.tranche_id.clone(),
                        symbol: a.symbol.clone(),
                        qty,
                        unit_price_cents: unit_price,
                        fees_cents: fees,
                        platform: a.platform.clone(),
                        tracking_code: a.tracking_code.clone(),
                    },
                });
            }
            LegacyActionKind::Vest => {
                // FMV recovered from the legacy `$/share` column (NOT the $0 Total
                // Cost). A $0 / blank / #DIV/0! source is a hard error — never
                // default to $0. (IMPORT-MAP-002, IMPORT-CORP-003)
                let fmv = match parse_dollars_to_cents(&a.dollars_per_share) {
                    Some(c) if c.0 > 0 => c,
                    _ => {
                        return Err(ImportError::UnrecoverableVestFmv {
                            coord: a.coord.clone(),
                        })
                    }
                };
                vest_fmv.insert(a.tranche_id.clone(), fmv);
                items.push(Item {
                    date,
                    rank: 0,
                    coord_key: (a.coord.tab.clone(), a.coord.row),
                    source: Some(a.coord.clone()),
                    event_id: Some(derive_event_id(&a.coord, &a.tranche_id)),
                    kind: LedgerEventKind::Vest {
                        lot_id: a.tranche_id.clone(),
                        symbol: a.symbol.clone(),
                        qty,
                        fmv_per_share_cents: fmv,
                        platform: a.platform.clone(),
                        tracking_code: a.tracking_code.clone(),
                    },
                });
            }
        }
    }

    // --- Splits: one per owner-supplied corporate action, inserted at its date in
    //     Seq order (rank 1). (IMPORT-CORP-002) ---
    for (i, ca) in wb.corporate_actions.iter().enumerate() {
        // A Split has no legacy source ROW (it is owner input). Its EventId derives
        // from a GUARANTEED-UNIQUE owner-action key (symbol + date + ratio) — NOT
        // the Vec enumeration index — so two distinct actions can never collide and
        // uniqueness does not rest on iteration order. The pre-pass rejects a
        // duplicate (symbol, date, ratio), so the key is provably unique. The
        // internal synthetic coord is never surfaced as provenance (`source: None`).
        let coord = RowCoord::new("Corporate Actions", i as u32 + 1);
        let action_key = format!(
            "split-{}-{}-{}:{}",
            ca.symbol, ca.date.0, ca.ratio_num, ca.ratio_den
        );
        items.push(Item {
            date: ca.date,
            rank: 1,
            coord_key: (coord.tab.clone(), coord.row),
            source: None,
            event_id: Some(derive_event_id(&coord, &action_key)),
            kind: LedgerEventKind::Split {
                symbol: ca.symbol.clone(),
                ratio_num: ca.ratio_num,
                ratio_den: ca.ratio_den,
            },
        });
    }

    // --- Sells from `Stock Sales`, specific-ID against the referenced tranche
    //     (rank 2). A sell-to-cover child references its vest tranche; it is just
    //     an ordinary Sell. (IMPORT-MAP-002/003) ---
    for s in &wb.sales {
        let corr = wb.corrections.get(&s.coord);
        let mut date = corr.and_then(|c| c.date).unwrap_or(s.date);
        let qty = corr.and_then(|c| c.qty).unwrap_or(s.qty);

        // The frame escape hatch (IMPORT-CORP-004): when the owner asserts a sale's
        // qty is ALREADY in the post-split frame, order it AFTER the relevant split
        // so the lot is rescaled before this post-split qty consumes it — without
        // having to also fudge the literal date. The relevant split is the latest
        // known corporate action for the symbol on or before the sale's own date
        // that still post-dates it in the literal frame; if no own-date split has
        // applied yet, snap the sale to that split's date.
        if corr.map(|c| c.already_post_split).unwrap_or(false) {
            if let Some(split_date) = post_split_frame_date(wb, &s.symbol, date) {
                date = split_date;
            }
        }

        let mut unit_price = match parse_dollars_to_cents(&s.dollars_per_share) {
            Some(c) => c,
            None => {
                malformed.push(MalformedRow {
                    coord: s.coord.clone(),
                    reason: format!(
                        "unparseable $/share {:?} on a Sell row",
                        s.dollars_per_share
                    ),
                });
                continue;
            }
        };
        // The sell-to-cover substitution (IMPORT-MAP-002): a `$0`-priced sale of
        // a Vest tranche is the broker selling vest-day shares to cover taxes —
        // the legacy sheet recorded `$0` because the proceeds never reached the
        // owner, but the shares sold AT the vest-date price (gain ≈ 0). The
        // reconstruction substitutes the vest's FMV so the canonical log carries
        // the true economics rather than a fabricated full-basis loss.
        if unit_price.0 == 0 {
            if let Some(fmv) = vest_fmv.get(&s.tranche_id) {
                unit_price = *fmv;
                *stc_fmv_value_cents.entry(s.symbol.clone()).or_insert(0) +=
                    pt_core::scale((qty.0 as i128) * (fmv.0 as i128)) as i64;
            }
        }
        let fees = parse_dollars_to_cents(&s.fees_dollars).unwrap_or(Cents(0));
        let accrues_to_state = stamp_residency(&wb.residency, date);
        items.push(Item {
            date,
            rank: 2,
            coord_key: (s.coord.tab.clone(), s.coord.row),
            source: Some(s.coord.clone()),
            event_id: Some(derive_event_id(&s.coord, &s.sale_tag)),
            kind: LedgerEventKind::Sell {
                sale_id: derive_event_id(&s.coord, &s.sale_tag),
                symbol: s.symbol.clone(),
                qty,
                unit_price_cents: unit_price,
                fees_cents: fees,
                lot_refs: vec![LotRef {
                    lot_id: s.tranche_id.clone(),
                    qty,
                }],
                accrues_to_state,
                platform: s.platform.clone(),
                tracking_code: None,
            },
        });
    }

    // 3. Order by (date, kind-rank, coord) — date then the deterministic tie-break.
    items.sort_by(|a, b| (a.date.0, a.rank, &a.coord_key).cmp(&(b.date.0, b.rank, &b.coord_key)));

    // 4. A `Positions`-only symbol (present in Positions, absent from every source
    //    row) cannot have its stream fabricated. (IMPORT-RUN-004)
    let source_symbols: std::collections::BTreeSet<&str> = wb
        .actions
        .iter()
        .map(|a| a.symbol.as_str())
        .chain(wb.sales.iter().map(|s| s.symbol.as_str()))
        .collect();
    for p in &wb.positions {
        if !source_symbols.contains(p.symbol.as_str()) {
            return Err(ImportError::PositionWithoutSourceRows {
                symbol: p.symbol.clone(),
            });
        }
    }

    // 5. Assign dense, 1-based Seq in the reconstructed fold order and build the
    //    output events. (IMPORT-RUN-001)
    let mut ledger: Vec<ReconstructedEvent> = Vec::with_capacity(items.len());
    for (i, it) in items.into_iter().enumerate() {
        let event_id = it.event_id.expect("every reconstructed event has an id");
        ledger.push(ReconstructedEvent {
            event_id: event_id.clone(),
            source: it.source,
            event: LedgerEvent {
                id: event_id,
                seq: Seq(i as u64 + 1),
                date: it.date,
                kind: it.kind,
            },
        });
    }

    // 6. The closed-year migration tax lifecycle. (IMPORT-TAX-002)
    let tax = build_migration_tax_events(&wb.closed_years);

    Ok(Reconstruction {
        ledger,
        tax,
        malformed,
        stc_fmv_value_cents,
    })
}

/// The effective ordering date for a sale the owner has flagged `already_post_split`
/// (IMPORT-CORP-004 frame escape hatch): the date of the symbol's split that the
/// sale's post-split-frame qty presumes has applied. If the sale's literal `date`
/// precedes a known split for the symbol (so the lot would not yet be rescaled when
/// the sale folds), returns the EARLIEST such split's date so the sale is reordered
/// just after it; otherwise (the sale already post-dates every relevant split)
/// returns `None` (no reframing needed — the literal date already sits in the
/// post-split frame).
fn post_split_frame_date(wb: &LegacyWorkbook, symbol: &Symbol, date: Date) -> Option<Date> {
    wb.corporate_actions
        .iter()
        .filter(|ca| &ca.symbol == symbol && ca.date.0 > date.0)
        .map(|ca| ca.date)
        .min_by_key(|d| d.0)
}

/// The earliest SALE date across the sale rows, applying any date correction.
/// `None` when the workbook has no sale rows. The founding-residency precondition
/// keys on this (not the earliest of all events): residency stamps ONLY Sells
/// (`residency_on(sale_date)` per IMPORT-TAX-001), so only a sale can fall in the
/// timeline's undefined pre-history region — an early Buy with later Sells need
/// not be blocked.
fn earliest_sale_date(wb: &LegacyWorkbook) -> Option<Date> {
    wb.sales
        .iter()
        .map(|s| {
            wb.corrections
                .get(&s.coord)
                .and_then(|c| c.date)
                .unwrap_or(s.date)
        })
        .min_by_key(|d| d.0)
}

/// Build the closed-year migration tax-event lifecycle: for each closed year, one
/// combined migration accrual per `(jurisdiction, tax_year)` set to the legacy
/// actual via `AmountOverride`, then driven `Allocate -> Move -> Pay` (outstanding
/// = 0). The `SeedMigration` declares the combined accrual; the `AmountOverride`
/// against the combined key records the legacy actual as the applied amount; the
/// `Allocate -> Move -> Pay` is the real lifecycle `tax`'s `Pay` gate requires
/// (Moved). Seqs are dense and 1-based on the tax tab. (IMPORT-TAX-002)
fn build_migration_tax_events(closed_years: &[ClosedYear]) -> Vec<TaxEvent> {
    use pt_core::Seq;
    use tax::{AccrualKey, Quarter, TaxEventKind};

    let mut events: Vec<TaxEvent> = Vec::new();
    let mut seq = 0u64;
    let mut next = |kind: TaxEventKind| -> TaxEvent {
        seq += 1;
        TaxEvent {
            seq: Seq(seq),
            kind,
        }
    };

    for cy in closed_years {
        let key = AccrualKey {
            sale_id: String::new(),
            lot_id: String::new(),
            jurisdiction: cy.jurisdiction.clone(),
            tax_year: cy.tax_year,
        };
        // Seed the combined migration accrual (no backing RealizedGain).
        events.push(next(TaxEventKind::SeedMigration {
            jurisdiction: cy.jurisdiction.clone(),
            tax_year: cy.tax_year,
            applied_amount_cents: cy.legacy_actual_cents,
            reason: "migration: closed prior-year combined accrual".to_string(),
        }));
        // Record the legacy actual as the applied amount via AmountOverride.
        events.push(next(TaxEventKind::AmountOverride {
            accrual_key: key.clone(),
            applied_amount_cents: cy.legacy_actual_cents,
            reason: "migration: legacy actual tax".to_string(),
        }));
        // Drive the real lifecycle: Allocate -> Move -> Pay (Pay requires Moved).
        events.push(next(TaxEventKind::Allocate {
            accrual_key: key.clone(),
            account_label: cy.account_label.clone(),
        }));
        events.push(next(TaxEventKind::Move {
            accrual_key: key.clone(),
            amount_cents: cy.legacy_actual_cents,
            date: year_end(cy.tax_year),
        }));
        events.push(next(TaxEventKind::Pay {
            jurisdiction: cy.jurisdiction.clone(),
            tax_year: cy.tax_year,
            period: Quarter::Q4,
            amount_cents: cy.legacy_actual_cents,
            date: year_end(cy.tax_year),
            covers: vec![key],
        }));
    }
    events
}

/// December 31 of a tax year, as days since the Unix epoch (the annual-granularity
/// stamp for a migrated closed year — the quarterly report is N/A). (IMPORT-TAX-002)
fn year_end(year: TaxYear) -> Date {
    Date(days_from_civil(year.0, 12, 31))
}

/// Days since 1970-01-01 for civil `(y, m, d)` (proleptic Gregorian). Mirrors
/// `config`/`tax`'s private copies; `import` needs it only for the year-end stamp.
fn days_from_civil(y: i32, m: i32, d: i32) -> i32 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) as i64 + 2) / 5 + (d - 1) as i64;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era as i64 * 146097 + doe - 719468) as i32
}

/// Validate EVERY reconstructed event through the kernel (the SAME path as an
/// `entry` append) — both the ledger events (`ledger_core::validate`) and the
/// closed-year migration tax lifecycle (`tax::validate_event`) — so the imported
/// log is valid by construction. The first rejection is surfaced (a ledger
/// rejection as `KernelRejected`, resolvable via a per-row correction-override
/// re-run; a tax rejection as `TaxKernelRejected`). (IMPORT-RUN-003, IMPORT-CORP-004)
///
// @spec IMPORT-RUN-003, IMPORT-CORP-004
pub fn validate_reconstruction(recon: &Reconstruction) -> Result<(), ImportError> {
    validate_ledger_reconstruction(recon)?;
    validate_tax_reconstruction(recon)?;
    Ok(())
}

/// Validate the reconstructed LEDGER events through `ledger_core::validate`: fold
/// in intended `Seq` order, validating each against the accepted-so-far prefix
/// exactly as an `entry` append does. (IMPORT-RUN-003, IMPORT-CORP-004)
fn validate_ledger_reconstruction(recon: &Reconstruction) -> Result<(), ImportError> {
    let mut sorted: Vec<&ReconstructedEvent> = recon.ledger.iter().collect();
    sorted.sort_by_key(|e| e.event.seq.0);

    let mut accepted: Vec<LedgerEvent> = Vec::with_capacity(sorted.len());
    for re in sorted {
        if let Err(error) = ledger_core::validate(&accepted, &re.event) {
            return Err(ImportError::KernelRejected {
                coord: re.source.clone(),
                error,
            });
        }
        accepted.push(re.event.clone());
    }
    Ok(())
}

/// Validate the reconstructed TAX events (the closed-year migration lifecycle)
/// through `tax::validate_event` — the SAME gate an `entry` tax append uses. Fold
/// in `Seq` order, validating each candidate against the accepted-so-far prefix.
/// The migration accrual carries no backing `RealizedGain`, so validation runs
/// with empty gains against a regime-agnostic `tax::migration_context`; the
/// combined-migration key is a first-class real key there (TAX-ACCRUAL-007), so
/// the `AmountOverride → Allocate → Move → Pay` lifecycle validates by
/// construction. A rejection is surfaced as `TaxKernelRejected`. (IMPORT-RUN-003)
///
// @spec IMPORT-TAX-003
fn validate_tax_reconstruction(recon: &Reconstruction) -> Result<(), ImportError> {
    let mut sorted: Vec<&TaxEvent> = recon.tax.iter().collect();
    sorted.sort_by_key(|e| e.seq.0);

    // The migration lifecycle is per-(jurisdiction, tax_year); validate each event
    // against a context for the year the event belongs to. A SeedMigration names
    // the (jurisdiction, tax_year) directly; the lifecycle events name it via their
    // accrual_key / Pay header. Validation only reads the migration key (no
    // brackets), so a regime-agnostic context for the event's year suffices.
    let mut accepted: Vec<TaxEvent> = Vec::with_capacity(sorted.len());
    for te in sorted {
        let tax_year = tax_event_tax_year(&te.kind);
        let ctx = tax::migration_context(tax_year);
        if let Err(error) = tax::validate_event(&[], &accepted, te, &ctx) {
            return Err(ImportError::TaxKernelRejected { error });
        }
        accepted.push((*te).clone());
    }
    Ok(())
}

/// The `tax_year` a migration `TaxEvent` belongs to (for building the validation
/// context). Reads it from whichever field carries it for the event kind.
fn tax_event_tax_year(kind: &tax::TaxEventKind) -> TaxYear {
    use tax::TaxEventKind;
    match kind {
        TaxEventKind::SeedMigration { tax_year, .. } => *tax_year,
        TaxEventKind::Pay { tax_year, .. } => *tax_year,
        TaxEventKind::Allocate { accrual_key, .. }
        | TaxEventKind::Move { accrual_key, .. }
        | TaxEventKind::AmountOverride { accrual_key, .. } => accrual_key.tax_year,
    }
}

// ===========================================================================
// Intended-divergence prediction (import-design.md → "Dry-Run & Reconciliation";
// IMPORT-RECON-002). Predicted INDEPENDENTLY, never back-labelled from the
// residual, and at the SYMBOL-AGGREGATE level (where the kernel's sums are exact).
// ===========================================================================

/// The independently predicted intended dollar delta for a symbol, in `Cents`
/// (new − legacy): the RSU-basis delta = Σ (vest FMV value) over the symbol's
/// vested shares (legacy charged `$0` basis), plus the split delta = exactly `$0`
/// (splits are basis-neutral). Computed from the reconstruction's Vest events and
/// the known corporate actions — NOT from the reconciliation residual, so a real
/// bug cannot hide behind an intended label. (IMPORT-RECON-002)
///
// @spec IMPORT-RECON-002
pub fn predict_intended_delta_cents(recon: &Reconstruction, symbol: &Symbol) -> Cents {
    use ledger_core::LedgerEventKind;

    // The RSU-basis delta: the legacy charged a $0 basis on vested shares, so the
    // new model's combined economic P&L (realized + unrealized) for the symbol is
    // SMALLER by exactly the FMV value the new basis now carries — Σ over the
    // symbol's Vest events of `scale(qty × fmv_per_share)`. This is the (new −
    // legacy) delta on the combined realized+unrealized figure.
    //
    // The split delta is exactly $0 (a split is basis-neutral), so it contributes
    // nothing. Predicted independently from the reconstruction's Vest events and
    // the corporate actions — NEVER back-labelled from the reconciliation residual.
    let mut fmv_value: i128 = 0;
    for re in &recon.ledger {
        if let LedgerEventKind::Vest {
            symbol: s,
            qty,
            fmv_per_share_cents,
            ..
        } = &re.event.kind
        {
            if s == symbol {
                // The single scale site: scale(qty_micro × price) → Cents.
                let scaled = pt_core::scale((qty.0 as i128) * (fmv_per_share_cents.0 as i128));
                fmv_value += scaled;
            }
        }
    }
    // Sell-to-cover shares contribute NOTHING to the divergence: their FMV
    // basis is consumed at FMV proceeds (gain 0) in the new model, and the
    // legacy recorded them at $0 profit — the two sides agree, so their FMV
    // value is subtracted from the predicted delta. (IMPORT-MAP-002)
    let stc = *recon.stc_fmv_value_cents.get(symbol).unwrap_or(&0) as i128;
    Cents((-(fmv_value - stc)) as i64)
}

/// Compose the human-readable attribution for a symbol's intended divergence from
/// its ACTUAL contributors: the RSU FMV-basis clause only when the symbol has a
/// Vest event carrying non-zero FMV (the legacy charged `$0`), and the explicit
/// split clause only when a corporate action exists for the symbol (basis-neutral,
/// `$0`). A symbol with only one contributor gets only that clause — no over-claim
/// of a split it never had nor an RSU it never vested. (IMPORT-RECON-002)
fn intended_divergence_explanation(
    recon: &Reconstruction,
    wb: &LegacyWorkbook,
    symbol: &Symbol,
) -> String {
    use ledger_core::LedgerEventKind;

    let has_rsu = recon.ledger.iter().any(|re| {
        matches!(&re.event.kind,
            LedgerEventKind::Vest { symbol: s, fmv_per_share_cents, .. }
                if s == symbol && fmv_per_share_cents.0 != 0)
    });
    let has_split = wb.corporate_actions.iter().any(|ca| &ca.symbol == symbol);

    let mut clauses: Vec<&str> = Vec::new();
    if has_rsu {
        clauses.push("RSU FMV-basis (legacy charged $0)");
    }
    if has_split {
        clauses.push("explicit split ($0, basis-neutral)");
    }
    if clauses.is_empty() {
        // A predicted non-zero delta with neither contributor is unexpected; name it
        // generically rather than asserting a specific cause that is not present.
        "intended RSU/split basis divergence".to_string()
    } else {
        clauses.join(" + ")
    }
}

/// The per-position dollar tolerance for `consumed_lot_count` consumed lots:
/// `ceil(½¢ × count)` rounded up to whole cents — the kernel's largest-remainder
/// noise bound. (IMPORT-RECON-002)
pub fn position_tolerance_cents(consumed_lot_count: u32) -> Cents {
    // ceil(½¢ × count) rounded up to whole cents. ½¢ × count == count/2 cents;
    // ceil that to a whole cent. Equivalently ceil(count / 2) cents.
    let num = HALF_CENT_NUM * consumed_lot_count as i64; // == count
    let cents = (num + HALF_CENT_DEN - 1) / HALF_CENT_DEN; // ceil(count / 2)
    Cents(cents)
}

// ===========================================================================
// Dry-run (import-design.md → "Dry-Run & Reconciliation"; IMPORT-RECON-001..004).
// Reconstruct -> validate -> reconcile -> report, writing NOTHING.
// ===========================================================================

/// The default mode (IMPORT-RECON-001): reconstruct, kernel-validate, replay to a
/// `Snapshot`, reconcile per symbol against the legacy `Positions` (dollar +
/// share), classify each (matched / intended-divergence / unexplained), and report
/// — WITHOUT writing. `marks` supplies per-symbol current marks for the unrealized
/// comparison (absent ⇒ that symbol's unrealized is `None`, not compared).
/// `commit_allowed` is `false` on ANY unexplained divergence (IMPORT-RECON-003).
///
// @spec IMPORT-RECON-001, IMPORT-RECON-002, IMPORT-RECON-003, IMPORT-RECON-004, IMPORT-RECON-006
pub fn dry_run(wb: &LegacyWorkbook, marks: &Marks) -> Result<DryRunReport, ImportError> {
    use ledger_core::{LedgerEventKind, LotSource};

    // Reconstruct + kernel-validate (the same path as an entry append). A
    // reconstruction or validation failure is surfaced rather than reported.
    let recon = reconstruct(wb)?;
    validate_reconstruction(&recon)?;

    // Replay the reconstructed log to a Snapshot for the reconciliation.
    let snapshot = replay_reconstruction(&recon, marks);

    // Index the legacy Positions (reconcile target) and the per-lot source so the
    // realized-only predicted delta can attribute vest-lot basis.
    let legacy_by_symbol: BTreeMap<&str, &LegacyPositionRow> = wb
        .positions
        .iter()
        .map(|p| (p.symbol.as_str(), p))
        .collect();
    let mut lot_source: BTreeMap<String, LotSource> = BTreeMap::new();
    for re in &recon.ledger {
        match &re.event.kind {
            LedgerEventKind::Buy { lot_id, .. } => {
                lot_source.insert(lot_id.clone(), LotSource::Buy);
            }
            LedgerEventKind::Vest { lot_id, .. } => {
                lot_source.insert(lot_id.clone(), LotSource::Vest);
            }
            _ => {}
        }
    }

    // Per-symbol consumed-lot count (for the dollar tolerance) — one per realized
    // gain (each gain is one consumed (lot, qty) pair).
    let mut consumed_lots: BTreeMap<String, u32> = BTreeMap::new();
    // Per-symbol realized FMV-basis consumed on VEST lots (the realized-only RSU
    // predicted delta is the negation of this).
    let mut vest_basis_consumed: BTreeMap<String, i128> = BTreeMap::new();
    for g in &snapshot.realized_gains {
        *consumed_lots.entry(g.symbol.clone()).or_insert(0) += 1;
        if matches!(lot_source.get(&g.lot_id), Some(LotSource::Vest)) {
            *vest_basis_consumed.entry(g.symbol.clone()).or_insert(0) += g.basis_cents.0 as i128;
        }
    }

    let mut symbols: Vec<SymbolReconciliation> = Vec::new();
    // A genuinely malformed source row was listed (IMPORT-RUN-011), not migrated:
    // committing while it is outstanding would silently drop its data from the
    // canonical log. Block commit until the owner resolves it (a corrected re-run),
    // so "listed for manual review rather than dropping them" holds end-to-end.
    let mut commit_allowed = recon.malformed.is_empty();

    for legacy in &wb.positions {
        let symbol = &legacy.symbol;
        let pos = snapshot.positions.get(symbol);
        let recon_shares = pos.map(|p| p.total_qty).unwrap_or(MicroShares(0));
        let recon_realized = pos.map(|p| p.realized_pnl_cents).unwrap_or(Cents(0));
        let recon_unrealized = pos.and_then(|p| p.unrealized_cents);
        let consumed_lot_count = *consumed_lots.get(symbol).unwrap_or(&0);

        // --- Share reconciliation (IMPORT-RECON-004). ---
        let share_residual = recon_shares.0 - legacy.shares.0;
        let closed = recon_shares.0 == 0;
        let share_verdict = if share_residual.abs() <= SHARE_TOLERANCE_MICRO {
            if share_residual == 0 {
                ShareVerdict::Matched
            } else if closed {
                // A sub-threshold residual on a CLOSED position snaps via a closing
                // adjustment (the adjustment is the legacy residual to drop).
                ShareVerdict::SnappedClosingAdjustment {
                    adjustment: MicroShares(legacy.shares.0),
                }
            } else {
                ShareVerdict::Matched
            }
        } else {
            ShareVerdict::Flagged {
                residual: MicroShares(share_residual),
            }
        };
        if matches!(share_verdict, ShareVerdict::Flagged { .. }) {
            commit_allowed = false;
        }

        // --- Dollar reconciliation (IMPORT-RECON-002). ---
        // Compare combined economic P&L when a mark gives reconstructed unrealized;
        // otherwise compare realized only (and use the realized-portion predicted
        // delta). The predicted delta is computed INDEPENDENTLY of the residual.
        let (new_total, legacy_total, predicted) = match recon_unrealized {
            Some(u) => {
                let new_total = recon_realized.0 as i128 + u.0 as i128;
                let legacy_total =
                    legacy.realized_pnl_cents.0 as i128 + legacy.unrealized_cents.0 as i128;
                // Full RSU-basis delta + the $0 split delta.
                let predicted = predict_intended_delta_cents(&recon, symbol).0 as i128;
                (new_total, legacy_total, predicted)
            }
            None => {
                let new_total = recon_realized.0 as i128;
                let legacy_total = legacy.realized_pnl_cents.0 as i128;
                // The realized-only RSU delta: −Σ(FMV basis consumed on vest
                // lots), excluding sell-to-cover consumption — an STC consumes
                // FMV basis at FMV proceeds (gain 0) on both sides.
                // (IMPORT-MAP-002)
                let stc = *recon.stc_fmv_value_cents.get(symbol).unwrap_or(&0) as i128;
                let predicted = -(*vest_basis_consumed.get(symbol).unwrap_or(&0) - stc);
                (new_total, legacy_total, predicted)
            }
        };
        let diff = new_total - legacy_total;
        let residual = diff - predicted;
        let tolerance = position_tolerance_cents(consumed_lot_count).0 as i128
            + PORTFOLIO_TOLERANCE_CENTS as i128;

        let verdict = if residual.abs() <= tolerance {
            if predicted == 0 {
                Verdict::Matched
            } else {
                // Compose the attribution from the symbol's ACTUAL contributors: the
                // RSU clause only when a vest carries FMV basis, the split clause
                // only when a corporate action exists for the symbol — so the
                // human-readable explanation does not over-claim a split or RSU the
                // symbol does not have. (IMPORT-RECON-002)
                Verdict::IntendedDivergence {
                    predicted_cents: Cents(predicted as i64),
                    explanation: intended_divergence_explanation(&recon, wb, symbol),
                }
            }
        } else if let Some(reason) = wb.adjudicated.get(symbol) {
            // An owner-declared adjudicated divergence: the residual is real but
            // the OWNER has ruled on it (e.g. a known-wrong legacy cell). The
            // residual AND the owner's stated reason are recorded — the commit
            // record carries why — and it does not block. (IMPORT-RECON-008)
            Verdict::OwnerAdjudicated {
                residual_cents: Cents(residual as i64),
                reason: reason.clone(),
            }
        } else {
            Verdict::Unexplained {
                residual_cents: Cents(residual as i64),
            }
        };
        if matches!(verdict, Verdict::Unexplained { .. }) {
            commit_allowed = false;
        }

        symbols.push(SymbolReconciliation {
            symbol: symbol.clone(),
            reconstructed_shares: recon_shares,
            legacy_shares: legacy.shares,
            reconstructed_realized_cents: recon_realized,
            legacy_realized_cents: legacy.realized_pnl_cents,
            reconstructed_unrealized_cents: recon_unrealized,
            legacy_unrealized_cents: legacy.unrealized_cents,
            consumed_lot_count,
            verdict,
            share_verdict,
        });
    }

    // A symbol RECONSTRUCTED from surviving rows but with NO legacy `Positions` row
    // (the inverse of the IMPORT-RUN-004 Positions-only case) must NOT be silently
    // committed unreconciled: there is no legacy figure to reconcile it against. It
    // is surfaced as a flagged line (reconstructed figures, zero legacy) so the
    // owner adds the target — and it BLOCKS commit until they do, rather than the
    // canonical log gaining an unreconciled symbol. (Whether/how such a symbol is
    // surfaced is an intent gap on the import EARS — see ears_gaps_reported.)
    //
    // @spec IMPORT-RUN-006
    let positioned: std::collections::BTreeSet<&str> = legacy_by_symbol.keys().copied().collect();
    for (symbol, pos) in &snapshot.positions {
        if positioned.contains(symbol.as_str()) {
            continue;
        }
        commit_allowed = false;
        symbols.push(SymbolReconciliation {
            symbol: symbol.clone(),
            reconstructed_shares: pos.total_qty,
            legacy_shares: MicroShares(0),
            reconstructed_realized_cents: pos.realized_pnl_cents,
            legacy_realized_cents: Cents(0),
            reconstructed_unrealized_cents: pos.unrealized_cents,
            legacy_unrealized_cents: Cents(0),
            consumed_lot_count: *consumed_lots.get(symbol).unwrap_or(&0),
            // No legacy figure to reconcile against: surfaced, not classified clean.
            verdict: Verdict::Unexplained {
                residual_cents: pos.realized_pnl_cents,
            },
            share_verdict: ShareVerdict::Flagged {
                residual: pos.total_qty,
            },
        });
    }

    Ok(DryRunReport {
        reconstruction: recon,
        symbols,
        commit_allowed,
    })
}

// ===========================================================================
// Commit (import-design.md → "Dry-Run & Reconciliation" / "Run Mechanics";
// IMPORT-RECON-003, IMPORT-RUN-001/003). Writes the reconstructed events through
// `store` ONLY after the owner accepts the reconciliation, into a fresh / own-
// events-only target, resumable idempotently in the original reconstructed order.
// ===========================================================================

/// Whether a commit may target this workbook: it must be FRESH, or hold ONLY this
/// import's own deterministic events (a partial-commit resume). Any OTHER event
/// blocks. `expected_ledger_ids` / `expected_tax_ids` are the exact sets of
/// `EventId`s this reconstruction would write — the ledger ids and the
/// store-assigned content-addressed tax ids (`store::tax_event_id`). The TAX tab
/// is gated against the exact expected set, NOT a loose `tax-` prefix: a foreign
/// `tax-…` row written by another writer is store-assigned the same prefix, so the
/// prefix test alone would let it pass. (IMPORT-RUN-001)
pub fn check_commit_target<S: SheetsClient, L: Lock, C: Cache>(
    store: &Store<S, L, C>,
    expected_ledger_ids: &std::collections::BTreeSet<String>,
    expected_tax_ids: &std::collections::BTreeSet<String>,
) -> Result<(), ImportError> {
    use store::Tab;

    // A tab that does not exist yet IS the fresh-workbook target IMPORT-RUN-001
    // names: the brand-new workbook has no event-log tabs until the first append
    // bootstraps them (store's cold-start classification distinguishes a missing
    // tab from a transport failure). Zero rows, nothing foreign.
    let read_cold = |tab: Tab| -> Result<Vec<store::Row>, ImportError> {
        match store.sheets().read_rows(tab) {
            Err(store::StoreError::TabMissing) => Ok(Vec::new()),
            r => r.map_err(ImportError::Store),
        }
    };

    // Every EventId already in the LEDGER tab must be one of THIS import's own
    // deterministic events (a partial-commit resume). A foreign event blocks.
    for row in &read_cold(Tab::Ledger)? {
        let id = row.get("EventId");
        if !id.is_empty() && !expected_ledger_ids.contains(id) {
            return Err(ImportError::TargetNotEmpty);
        }
    }
    // Every EventId already in the TAX tab must be one of THIS import's own expected
    // tax ids — gated against the exact set (exactly as the ledger tab is), so a
    // foreign tax row (also store-assigned a `tax-…` id) is detected and blocks.
    for row in &read_cold(Tab::Tax)? {
        let id = row.get("EventId");
        if !id.is_empty() && !expected_tax_ids.contains(id) {
            return Err(ImportError::TargetNotEmpty);
        }
    }
    Ok(())
}

/// The exact set of store-assigned `EventId`s this reconstruction's tax events
/// would write, via the SAME content-addressed derivation `store` uses on append
/// (`store::tax_event_id`). Lets `check_commit_target` gate the tax tab against the
/// precise own-events set rather than a loose `tax-` prefix. (IMPORT-RUN-001)
pub fn expected_tax_ids(recon: &Reconstruction) -> std::collections::BTreeSet<String> {
    recon.tax.iter().map(store::tax_event_id).collect()
}

/// Commit the reconstructed events through `store` after the owner accepts the
/// reconciliation. Requires the typed `AcceptedReport` owner-acceptance token — an
/// un-accepted token is refused with `OwnerAcceptanceRequired` BEFORE any write,
/// independently of the safety gate (IMPORT-RECON-005). Then refuses if
/// `report.commit_allowed` is `false` (IMPORT-RECON-003), so owner acceptance never
/// bypasses the auto-computed safety gate either. Seeds a fresh / own-events-only
/// target (IMPORT-RUN-001),
/// re-appending the missing tail IN THE ORIGINAL reconstructed order so a partial
/// commit resumes idempotently (IMPORT-RUN-001), with every event already kernel-
/// validated (IMPORT-RUN-003). Returns the per-event append outcomes.
///
/// The commit HOLDS `store`'s advisory write-lock across the WHOLE commit — the
/// legacy-target check plus every append — so no foreign writer can interleave
/// between the check and the appends (no check→append TOCTOU): the freshness the
/// target check observes still holds when the appends run (IMPORT-RUN-007,
/// RUNTIME-LOCK-006). Because the lock is reentrant for a same holder
/// (RUNTIME-LOCK-005), each inner `store` append still acquires the lock inside the
/// primitive (the single-writer guarantee `store` owns, STORE-WRITE-007) and the
/// nested re-acquire is a no-op against the held outer lock — no self-deadlock. If
/// the whole-commit lock cannot be acquired (held by another writer), the commit is
/// refused before the target check, leaving no partial state.
///
// @spec IMPORT-RUN-001, IMPORT-RUN-003, IMPORT-RECON-003, IMPORT-RECON-005, IMPORT-RUN-007, RUNTIME-LOCK-006
pub fn commit<S: SheetsClient, L: Lock, C: Cache>(
    store: &mut Store<S, L, C>,
    accepted: &AcceptedReport<'_>,
) -> Result<CommitReport, ImportError> {
    // The OWNER-ACCEPTANCE gate (IMPORT-RECON-005): the typed `AcceptedReport` token
    // must carry the owner's explicit acceptance. An un-accepted (under-review) token
    // is refused BEFORE any write — independently of `commit_allowed`, so a clear
    // safety gate never stands in for the owner's decision. This and the safety gate
    // below are two independent gates; neither implies the other.
    if !accepted.accepted {
        return Err(ImportError::OwnerAcceptanceRequired);
    }
    let report = accepted.report;

    // Commit is gated on a reconciliation with NO unexplained divergence (and no
    // flagged share residual): a blocked report is refused with the dedicated
    // CommitBlockedByReconciliation variant (NOT the write-target TargetNotEmpty),
    // naming the blocking symbols so the owner can fix the migration bug rather
    // than be misled into chasing a non-empty target. (IMPORT-RECON-003)
    if !report.commit_allowed {
        let unexplained_symbols: Vec<Symbol> = report
            .symbols
            .iter()
            .filter(|s| matches!(s.verdict, Verdict::Unexplained { .. }))
            .map(|s| s.symbol.clone())
            .collect();
        let flagged_symbols: Vec<Symbol> = report
            .symbols
            .iter()
            .filter(|s| matches!(s.share_verdict, ShareVerdict::Flagged { .. }))
            .map(|s| s.symbol.clone())
            .collect();
        let malformed_rows: Vec<RowCoord> = report
            .reconstruction
            .malformed
            .iter()
            .map(|m| m.coord.clone())
            .collect();
        return Err(ImportError::CommitBlockedByReconciliation {
            unexplained_symbols,
            flagged_symbols,
            malformed_rows,
        });
    }

    // Acquire the advisory write-lock for the WHOLE commit BEFORE the target check
    // and hold it across the check + all appends, so no foreign writer interleaves
    // between the check and the appends (no check→append TOCTOU). The guard releases
    // when this function returns (after the final append). The inner append
    // primitives re-acquire this SAME lock re-entrantly under the held guard — a
    // no-op against the same holder — so there is no self-deadlock
    // (RUNTIME-LOCK-005). A held lock (another writer) refuses the commit here,
    // leaving no partial state — the same control-return contract a transport
    // failure honors. (IMPORT-RUN-007, RUNTIME-LOCK-006)
    let _commit_guard = store
        .lock()
        .acquire()
        .map_err(|_| ImportError::Store(store::StoreError::Unreachable))?;

    // The exact sets of THIS import's own deterministic EventIds (ledger ids +
    // store-assigned content-addressed tax ids), for the own-events-only target
    // check. (IMPORT-RUN-001)
    let expected_ledger_ids: std::collections::BTreeSet<String> = report
        .reconstruction
        .ledger
        .iter()
        .map(|e| e.event_id.clone())
        .collect();
    let expected_tax = expected_tax_ids(&report.reconstruction);
    check_commit_target(store, &expected_ledger_ids, &expected_tax)?;

    let mut out = CommitReport::default();

    // Append the reconstructed ledger events strictly in the intended Seq order, so
    // store's dense Seq encodes the same fold order the dry-run reconciled. A
    // resume re-appends only the missing tail (store idempotency dedups by the
    // deterministic EventId). (IMPORT-RUN-001/003)
    let mut ordered: Vec<&ReconstructedEvent> = report.reconstruction.ledger.iter().collect();
    ordered.sort_by_key(|e| e.event.seq.0);
    for re in ordered {
        let outcome = store
            .append_ledger(&re.event)
            .map_err(|e| ImportError::StoreAt {
                event_id: re.event_id.clone(),
                error: e,
            })?;
        if outcome.idempotent_skip {
            out.skipped.push(outcome.event_id);
        } else {
            out.appended.push(outcome.event_id);
        }
    }

    // Append the closed-year migration tax lifecycle in Seq order (store assigns a
    // content-addressed, retry-stable id, so a resume dedups these too).
    let mut tax_ordered: Vec<&TaxEvent> = report.reconstruction.tax.iter().collect();
    tax_ordered.sort_by_key(|e| e.seq.0);
    for te in tax_ordered {
        let outcome = store.append_tax(te).map_err(|e| ImportError::StoreAt {
            event_id: store::tax_event_id(te),
            error: e,
        })?;
        if outcome.idempotent_skip {
            out.skipped.push(outcome.event_id);
        } else {
            out.appended.push(outcome.event_id);
        }
    }

    Ok(out)
}

/// The outcome of a commit: how many events landed vs. were idempotently skipped
/// (a resume re-appends only the missing tail). (IMPORT-RUN-001)
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct CommitReport {
    /// `EventId`s freshly appended this commit.
    pub appended: Vec<String>,
    /// `EventId`s already present and equal (idempotent skip — a resume).
    pub skipped: Vec<String>,
}

// ===========================================================================
// Replay helper (import-design.md → "Dry-Run & Reconciliation"). The importer's
// own thin replay over the reconstructed ledger (the same kernel `entry`/`runtime`
// use), used by the reconciliation. Exposed for tests asserting the replayed
// `Snapshot` directly.
// ===========================================================================

/// Replay the reconstructed ledger events into a `ledger_core::Snapshot`, in the
/// importer's intended `Seq` order, with `marks` for the unrealized valuation. The
/// reconciliation compares this against the legacy `Positions`. (IMPORT-RECON-001)
pub fn replay_reconstruction(recon: &Reconstruction, marks: &Marks) -> ledger_core::Snapshot {
    let events: Vec<LedgerEvent> = recon.ledger.iter().map(|e| e.event.clone()).collect();
    ledger_core::replay(&events, marks)
}

// ===========================================================================
// Residency stamping (import-design.md → "Historical Tax & Residency";
// IMPORT-TAX-001). Stamp each historical Sell's `accrues_to_state` from
// `residency_on(sale_date)` over the owner-supplied timeline.
// ===========================================================================

/// Stamp a historical sale's `accrues_to_state` from the residency timeline:
/// `residency_on(sale_date)`. The founding-entry gate (checked in `dry_run`) keeps
/// this out of `residency_on`'s undefined pre-history region. (IMPORT-TAX-001)
///
// @spec IMPORT-TAX-001
pub fn stamp_residency(timeline: &ResidencyTimeline, sale_date: Date) -> Option<StateCode> {
    timeline.residency_on(sale_date)
}

// ===========================================================================
// Test fixtures (import-design.md → "References": the SYNTHETIC in-memory legacy
// fixture). Public so integration tests build the AMZN-split / multi-symbol cases
// against the same fixture builders. The real workbook is parsed into
// `LegacyWorkbook` for a manual run; it is NEVER used in tests.
// ===========================================================================
pub mod testkit;

// The legacy-workbook PARSE (raw cell grids → typed legacy rows): pure, no I/O;
// the binary fetches the grids read-only and hands them here. (IMPORT-RUN-008)
pub mod parse;
