//! `entry` — the write-path surface of the TUI (prefix `TUI-ENTRY`). Every flow is
//! a **composer** (a small form) that rides the parent's write-path loop and
//! renders in the Ledger language. `entry` specifies *what each flow composes and
//! validates*, not *how durability works* (that is the `tui` sub-HLD's contract,
//! threaded through [`crate::port`]). It mutates only through the verified kernels +
//! `store`; it computes nothing itself. (entry-design.md)
//!
//! The shared loop (entry-design.md → "The Composer & Write Loop"):
//! ```text
//!  fields → inline validate (advisory, cached) → submit → append + read-back-verify → confirmed
//!    ▲           │ reject: the LedgerError/TaxError/config error beside the field      │ fail:
//!    └─ edit ◀───┘ nothing written                                                     keep entry, [r]etry
//! ```

use ledger_core::{LedgerError, LedgerEvent, LedgerEventKind, LotRef, LotSource, Snapshot, Symbol};
use pt_core::{Cents, Date, MicroShares};
use tax::{AccrualKey, TaxError, TaxEvent, TaxEventKind};

use crate::port::{RuntimePort, SubmitOutcome, SubmitRejection, WriteFailure};

// ===========================================================================
// Confirmation gating (entry-design.md → "Confirmations"; TUI-ENTRY-FLOW-006).
// Confirm gates EXACTLY: Reversal, Pay, Override, and tax-rule edits. Plain
// appends (Buy / Vest / Sell / Split / residency / platform / alias) do not gate.
// ===========================================================================

/// Which flow a composer drives — fixes the confirm-gating and the kind of event
/// composed. (entry-design.md → flow families)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FlowKind {
    Buy,
    Vest,
    Sell,
    Split,
    Reversal,
    Allocate,
    Move,
    Pay,
    Override,
    ResidencyEdit,
    TaxRuleEdit,
    PlatformAliasEdit,
}

impl FlowKind {
    /// Whether this flow's submit is **gated by a confirm** that restates what will
    /// be written: Reversal, Pay, Override, and tax-rule edits only. Plain appends
    /// do not gate. (TUI-ENTRY-FLOW-006)
    pub fn requires_confirm(self) -> bool {
        matches!(
            self,
            FlowKind::Reversal | FlowKind::Pay | FlowKind::Override | FlowKind::TaxRuleEdit
        )
    }
}

// ===========================================================================
// The composer & write-loop state machine (TUI-ENTRY-FLOW-001..005).
// ===========================================================================

/// The phase a composer is in along the write loop. (entry-design.md → write-loop;
/// TUI-ENTRY-FLOW-001..005)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Phase {
    /// Editing fields; inline advisory validation runs as fields change.
    /// (TUI-ENTRY-FLOW-001)
    Editing,
    /// A confirm step is pending (gated flows only) — restating what will be
    /// written. (TUI-ENTRY-FLOW-006)
    Confirming,
    /// The submit re-validated against the live state and the kernel rejected the
    /// candidate: control returns to **editing** with the specific kernel error
    /// re-rendered in the inline slot beside the offending field — inline never
    /// overrides submit, and this is distinct from a write-verify retry. Nothing
    /// was written. (TUI-ENTRY-FLOW-002)
    Rejected(InlineError),
    /// The submit failed non-destructively (lock held / write failed): the entry is
    /// preserved with an `[r]etry`. (TUI-ENTRY-FLOW-004/005)
    Retry(RetryReason),
    /// A Buy/Vest names a symbol matching no existing position and no alias, and the
    /// owner has not yet confirmed a new position: the submit is held (nothing
    /// written) with the new-symbol `warn` shown until `[c]onfirm`. The warning does
    /// not block — confirming clears it. (TUI-ENTRY-ACT-006)
    ConfirmNewSymbol(String),
    /// Confirmed durable — the composer clears. (TUI-ENTRY-FLOW-003)
    Confirmed,
}

/// Why a submit returned control with the entry intact. (TUI-ENTRY-FLOW-004/005)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RetryReason {
    /// The advisory write-lock is held (a cron `summary`): `⚠ lock held` in `warn`.
    /// (TUI-ENTRY-FLOW-005)
    LockHeld { holder: String },
    /// A read-back mismatch (a human edit). (TUI-ENTRY-FLOW-004)
    VerifyMismatch,
    /// The workbook was unreachable. (TUI-ENTRY-FLOW-004)
    Unreachable,
}

impl RetryReason {
    /// The warn/notice text shown beside the `[r]etry`. (TUI-ENTRY-FLOW-004/005)
    pub fn notice(&self) -> String {
        match self {
            RetryReason::LockHeld { holder } => {
                format!(
                    "{} lock held by {holder} — entry preserved · [r]etry",
                    crate::theme::GLYPH_WARN
                )
            }
            RetryReason::VerifyMismatch => {
                "write could not be verified — entry preserved · [r]etry".to_string()
            }
            RetryReason::Unreachable => {
                "workbook unreachable — entry preserved · [r]etry".to_string()
            }
        }
    }
}

/// An inline validation error rendered **beside the offending field**, in the
/// `error` role, writing nothing. The cause is the verified kernel/config error.
/// (TUI-ENTRY-FLOW-001/002)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum InlineError {
    Ledger(LedgerError),
    Tax(TaxError),
    Config(config::ConfigError),
    /// A composer-local parse/shape error (e.g. a non-numeric qty) before the kernel
    /// is even consulted.
    Field(String),
}

impl InlineError {
    /// The error text rendered inline beside the field (the verified error's words).
    pub fn text(&self) -> String {
        match self {
            InlineError::Ledger(e) => format!("{e:?}"),
            InlineError::Tax(e) => format!("{e:?}"),
            InlineError::Config(e) => format!("{e:?}"),
            InlineError::Field(s) => s.clone(),
        }
    }
}

// ===========================================================================
// The activity composers (TUI-ENTRY-ACT-*). Each composes a LedgerEvent.
// ===========================================================================

/// A Buy composer: symbol, qty, unit price, date, fees, platform, tracking code.
/// (TUI-ENTRY-ACT-001)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BuyForm {
    pub lot_id: String,
    pub symbol: Symbol,
    pub qty: MicroShares,
    pub unit_price: Cents,
    pub fees: Cents,
    pub date: Date,
    pub platform: String,
    pub tracking_code: Option<String>,
    /// `true` once the owner confirmed a new-symbol position. (TUI-ENTRY-ACT-006)
    pub confirmed_new_symbol: bool,
}

impl BuyForm {
    /// A Buy composer seeded with the flow defaults: the date defaults to
    /// `port.today()`, the platform to the first `config` suggestion, and the typed
    /// symbol resolves through the alias table — all overridable. (TUI-ENTRY-FLOW-007)
    pub fn with_defaults<P: RuntimePort>(
        port: &P,
        lot_id: String,
        typed_symbol: &str,
        platforms: &config::PlatformList,
        aliases: &config::AliasMap,
    ) -> Self {
        BuyForm {
            lot_id,
            symbol: aliases.resolve(typed_symbol),
            qty: MicroShares(0),
            unit_price: Cents(0),
            fees: Cents(0),
            date: port.today(),
            platform: default_platform(platforms),
            tracking_code: None,
            confirmed_new_symbol: false,
        }
    }

    /// Compose the Buy into a `LedgerEvent` (id assigned by `store` at append). The
    /// composer carries the candidate `lot_id`; the EventId is `store`-assigned, so
    /// a placeholder id rides through (re-stamped on append). (TUI-ENTRY-ACT-001)
    pub fn compose(&self) -> LedgerEvent {
        LedgerEvent {
            id: String::new(),
            seq: pt_core::Seq(0),
            date: self.date,
            kind: LedgerEventKind::Buy {
                lot_id: self.lot_id.clone(),
                symbol: self.symbol.clone(),
                qty: self.qty,
                unit_price_cents: self.unit_price,
                fees_cents: self.fees,
                platform: self.platform.clone(),
                tracking_code: self.tracking_code.clone(),
            },
        }
    }
}

/// A Vest composer: symbol, qty, FMV/share, date, platform, tracking code (no fee
/// field). (TUI-ENTRY-ACT-002)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct VestForm {
    pub lot_id: String,
    pub symbol: Symbol,
    pub qty: MicroShares,
    pub fmv_per_share: Cents,
    pub date: Date,
    pub platform: String,
    pub tracking_code: Option<String>,
    pub confirmed_new_symbol: bool,
}

impl VestForm {
    /// A Vest composer seeded with the flow defaults (date today, platform from
    /// `config`, symbol resolved via the alias table). (TUI-ENTRY-FLOW-007)
    pub fn with_defaults<P: RuntimePort>(
        port: &P,
        lot_id: String,
        typed_symbol: &str,
        platforms: &config::PlatformList,
        aliases: &config::AliasMap,
    ) -> Self {
        VestForm {
            lot_id,
            symbol: aliases.resolve(typed_symbol),
            qty: MicroShares(0),
            fmv_per_share: Cents(0),
            date: port.today(),
            platform: default_platform(platforms),
            tracking_code: None,
            confirmed_new_symbol: false,
        }
    }

    /// Compose the Vest into a `LedgerEvent` (no fee field). (TUI-ENTRY-ACT-002)
    pub fn compose(&self) -> LedgerEvent {
        LedgerEvent {
            id: String::new(),
            seq: pt_core::Seq(0),
            date: self.date,
            kind: LedgerEventKind::Vest {
                lot_id: self.lot_id.clone(),
                symbol: self.symbol.clone(),
                qty: self.qty,
                fmv_per_share_cents: self.fmv_per_share,
                platform: self.platform.clone(),
                tracking_code: self.tracking_code.clone(),
            },
        }
    }
}

/// A Split composer: symbol, ratio num:den, date. Rejects `BadSplitRatio` inline.
/// (TUI-ENTRY-ACT-004)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SplitForm {
    pub symbol: Symbol,
    pub ratio_num: i64,
    pub ratio_den: i64,
    pub date: Date,
}

impl SplitForm {
    /// Compose the Split into a `LedgerEvent`. (TUI-ENTRY-ACT-004)
    pub fn compose(&self) -> LedgerEvent {
        LedgerEvent {
            id: String::new(),
            seq: pt_core::Seq(0),
            date: self.date,
            kind: LedgerEventKind::Split {
                symbol: self.symbol.clone(),
                ratio_num: self.ratio_num,
                ratio_den: self.ratio_den,
            },
        }
    }
}

/// A Sell composer: symbol, qty, unit price, date, fees, platform, tracking code,
/// plus the **lot picker** allocation. (TUI-ENTRY-ACT-003)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SellForm {
    pub sale_id: String,
    pub symbol: Symbol,
    pub qty: MicroShares,
    pub unit_price: Cents,
    pub fees: Cents,
    pub date: Date,
    pub platform: String,
    pub tracking_code: Option<String>,
    pub accrues_to_state: Option<String>,
    /// The lot-picker allocation (lot id → take qty). (TUI-ENTRY-LOT-001)
    pub picker: LotPicker,
}

impl SellForm {
    /// A Sell composer seeded with the flow defaults, threading `config` + the live
    /// snapshot: the date defaults to `port.today()`; `accrues_to_state` to
    /// `residency.residency_on(sale_date)`; the platform to the first `config`
    /// suggestion; the typed symbol resolves through the alias table; and the lot
    /// picker is built live from the snapshot for the resolved symbol on the default
    /// platform. All overridable. (TUI-ENTRY-FLOW-007)
    pub fn with_defaults<P: RuntimePort>(
        port: &P,
        sale_id: String,
        typed_symbol: &str,
        residency: &config::ResidencyTimeline,
        platforms: &config::PlatformList,
        aliases: &config::AliasMap,
    ) -> Self {
        let date = port.today();
        let symbol = aliases.resolve(typed_symbol);
        let platform = default_platform(platforms);
        let picker = LotPicker::build(
            &port.view().snapshot,
            &symbol,
            &platform,
            MicroShares(0),
            date,
        );
        SellForm {
            sale_id,
            symbol,
            qty: MicroShares(0),
            unit_price: Cents(0),
            fees: Cents(0),
            date,
            platform,
            tracking_code: None,
            accrues_to_state: residency.residency_on(date),
            picker,
        }
    }

    /// Compose the Sell into a `LedgerEvent`, driving the lot picker's allocation
    /// as the `lot_refs`. An empty allocation composes a FIFO sell (empty
    /// `lot_refs`). (TUI-ENTRY-ACT-003)
    pub fn compose(&self) -> LedgerEvent {
        LedgerEvent {
            id: String::new(),
            seq: pt_core::Seq(0),
            date: self.date,
            kind: LedgerEventKind::Sell {
                sale_id: self.sale_id.clone(),
                symbol: self.symbol.clone(),
                qty: self.qty,
                unit_price_cents: self.unit_price,
                fees_cents: self.fees,
                lot_refs: self.picker.lot_refs(),
                accrues_to_state: self.accrues_to_state.clone(),
                platform: self.platform.clone(),
                tracking_code: self.tracking_code.clone(),
            },
        }
    }
}

// ===========================================================================
// New-symbol guard (TUI-ENTRY-ACT-006). When a Buy/Vest names a symbol matching
// no existing position AND no alias, WARN (not block) and require a confirm-new-
// position step. (entry-design.md → "New-symbol guard")
// ===========================================================================

/// The default platform a composer seeds: the first `config` platform suggestion,
/// or empty when none are configured (the field is still overridable).
/// (TUI-ENTRY-FLOW-007)
pub fn default_platform(platforms: &config::PlatformList) -> String {
    platforms.names().first().cloned().unwrap_or_default()
}

/// Whether `symbol` is a known position or alias in the current snapshot/aliases.
/// (TUI-ENTRY-ACT-006)
pub fn symbol_is_known(symbol: &Symbol, snapshot: &Snapshot, aliases: &config::AliasMap) -> bool {
    if snapshot.positions.contains_key(symbol) {
        return true;
    }
    // An alias resolves a known ticker; treat a symbol present as an alias key as
    // known (it is not first-seen). The alias map resolves a symbol to a ticker;
    // a symbol that resolves to anything other than itself was deliberately mapped.
    aliases.resolve(symbol) != *symbol
}

/// The new-symbol warning (does not block): `new symbol 'AMZM' — first seen ·
/// [c]onfirm new position`. (TUI-ENTRY-ACT-006)
pub fn new_symbol_warning(symbol: &Symbol) -> String {
    format!(
        "{} new symbol '{symbol}' — first seen · [c]onfirm new position",
        crate::theme::GLYPH_WARN
    )
}

// ===========================================================================
// The lot picker (TUI-ENTRY-LOT-*) — the signature interaction. A Sell's
// specific-identification lot selection with a running total + a live gain/tax
// preview. (entry-design.md → "The Lot Picker")
// ===========================================================================

/// One lot row in the picker: the symbol's open lots **on the sale's platform**,
/// with term (LT/ST as of the sale date), remaining qty, basis/share, and the
/// owner's allocated take. A `rem 0` lot is greyed (unallocatable). (TUI-ENTRY-LOT-001/004)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PickerLot {
    pub lot_id: String,
    pub source: LotSource,
    pub acquire_date: Date,
    pub term: tax::Term,
    pub remaining_qty: MicroShares,
    pub basis_per_share: Cents,
    /// The owner's allocated take for this lot (0 = none).
    pub take: MicroShares,
}

impl PickerLot {
    /// `true` when this lot is greyed — a `rem 0` lot, listed but unallocatable.
    /// (TUI-ENTRY-LOT-004)
    pub fn greyed(&self) -> bool {
        self.remaining_qty.0 == 0
    }
}

/// The lot picker for a Sell: the candidate lots on the sale's platform + the sale
/// qty, with a running allocation total and the empty/insufficient states.
/// (TUI-ENTRY-LOT-001..005)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LotPicker {
    pub symbol: Symbol,
    pub platform: String,
    pub sale_qty: MicroShares,
    pub lots: Vec<PickerLot>,
}

impl LotPicker {
    /// Build a picker for a Sell from the snapshot's open lots, keeping only the
    /// symbol's lots **on the sale's platform**, term-classified as of the sale
    /// date. (TUI-ENTRY-LOT-001)
    pub fn build(
        snapshot: &Snapshot,
        symbol: &Symbol,
        platform: &str,
        sale_qty: MicroShares,
        sale_date: Date,
    ) -> Self {
        let lots = snapshot
            .open_lots
            .iter()
            .filter(|ol| ol.lot.symbol == *symbol && ol.lot.platform == platform)
            .map(|ol| {
                let l = &ol.lot;
                let bps = if l.remaining_qty.0 != 0 {
                    Cents(pt_core::round_half_to_even(
                        (l.remaining_basis_cents.0 as i128) * (pt_core::SHARE_SCALE as i128),
                        l.remaining_qty.0 as i128,
                    ) as i64)
                } else {
                    Cents(0)
                };
                PickerLot {
                    lot_id: l.id.clone(),
                    source: l.source,
                    acquire_date: l.acquire_date,
                    term: tax::classify_term(l.acquire_date, sale_date),
                    remaining_qty: l.remaining_qty,
                    basis_per_share: bps,
                    take: MicroShares(0),
                }
            })
            .collect();
        LotPicker {
            symbol: symbol.clone(),
            platform: platform.to_string(),
            sale_qty,
            lots,
        }
    }

    /// The allocated total across lots. (TUI-ENTRY-LOT-002)
    pub fn allocated(&self) -> MicroShares {
        MicroShares(self.lots.iter().map(|l| l.take.0).sum())
    }

    /// `true` when the allocation sums **exactly** to the sale qty (the ✓ state).
    /// (TUI-ENTRY-LOT-002)
    pub fn is_complete(&self) -> bool {
        self.allocated().0 == self.sale_qty.0 && self.sale_qty.0 > 0
    }

    /// The platform's total remaining across the candidate lots (for the
    /// insufficient state). (TUI-ENTRY-LOT-004)
    pub fn platform_remaining(&self) -> MicroShares {
        MicroShares(self.lots.iter().map(|l| l.remaining_qty.0).sum())
    }

    /// The `lot_refs` for the composed Sell — the non-zero allocations. An empty
    /// allocation composes a FIFO sell (the kernel's FIFO fallback). (TUI-ENTRY-LOT-001)
    pub fn lot_refs(&self) -> Vec<LotRef> {
        self.lots
            .iter()
            .filter(|l| l.take.0 > 0)
            .map(|l| LotRef {
                lot_id: l.lot_id.clone(),
                qty: l.take,
            })
            .collect()
    }

    /// Set a lot's take, capped at its remaining (each lot capped at its remaining;
    /// a greyed `rem 0` lot stays unallocatable). Returns the applied take.
    /// (TUI-ENTRY-LOT-002/004)
    pub fn set_take(&mut self, lot_id: &str, take: MicroShares) -> MicroShares {
        if let Some(l) = self.lots.iter_mut().find(|l| l.lot_id == lot_id) {
            let capped = MicroShares(take.0.clamp(0, l.remaining_qty.0));
            l.take = capped;
            return capped;
        }
        MicroShares(0)
    }

    /// Reset every allocation (called when the sale qty changes — a stale ✓ must
    /// never reach submit). (TUI-ENTRY-LOT-003)
    pub fn reset_allocation(&mut self) {
        for l in self.lots.iter_mut() {
            l.take = MicroShares(0);
        }
    }

    /// Change the sale qty and **reset the allocation** (TUI-ENTRY-LOT-003).
    pub fn set_sale_qty(&mut self, qty: MicroShares) {
        self.sale_qty = qty;
        self.reset_allocation();
    }

    /// FIFO fill: allocate oldest-first (ascending acquire_date) up to the sale qty,
    /// as a preview the owner can still adjust. (TUI-ENTRY-LOT-001)
    pub fn fill_fifo(&mut self) {
        self.reset_allocation();
        let mut order: Vec<usize> = (0..self.lots.len())
            .filter(|&i| self.lots[i].remaining_qty.0 > 0)
            .collect();
        order.sort_by_key(|&i| self.lots[i].acquire_date.0);
        let mut need = self.sale_qty.0;
        for i in order {
            if need <= 0 {
                break;
            }
            let take = need.min(self.lots[i].remaining_qty.0);
            self.lots[i].take = MicroShares(take);
            need -= take;
        }
    }

    /// The explicit empty / insufficient state, distinct from a normal
    /// under-allocation. (TUI-ENTRY-LOT-004)
    pub fn state(&self) -> PickerState {
        if self.lots.is_empty() {
            return PickerState::NoOpenLots;
        }
        if self.platform_remaining().0 < self.sale_qty.0 {
            return PickerState::Insufficient {
                remaining: self.platform_remaining(),
                shortfall: MicroShares(self.sale_qty.0 - self.platform_remaining().0),
            };
        }
        if self.is_complete() {
            PickerState::Complete
        } else {
            PickerState::UnderAllocated {
                allocated: self.allocated(),
                needed: self.sale_qty,
            }
        }
    }
}

/// The lot picker's explicit state. (TUI-ENTRY-LOT-002/004)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum PickerState {
    /// No open lots for the symbol on the sale's platform. (TUI-ENTRY-LOT-004)
    NoOpenLots,
    /// The platform's total remaining is below the sale qty — ✓ unreachable, the
    /// shortfall named. (TUI-ENTRY-LOT-004)
    Insufficient {
        remaining: MicroShares,
        shortfall: MicroShares,
    },
    /// Allocated, but not yet equal to the sale qty (the running `N / M`).
    UnderAllocated {
        allocated: MicroShares,
        needed: MicroShares,
    },
    /// Allocated exactly to the sale qty — ✓. (TUI-ENTRY-LOT-002)
    Complete,
}

impl PickerState {
    /// The explicit state message. (TUI-ENTRY-LOT-004)
    pub fn message(&self, symbol: &Symbol, platform: &str) -> String {
        match self {
            PickerState::NoOpenLots => {
                format!("no open lots for {symbol} on {platform} — check platform")
            }
            PickerState::Insufficient {
                remaining,
                shortfall,
            } => format!(
                "insufficient — {} remaining, short {} on {platform}",
                crate::theme::shares(*remaining),
                crate::theme::shares(*shortfall),
            ),
            PickerState::UnderAllocated { allocated, needed } => format!(
                "allocated {} / {}",
                crate::theme::shares(*allocated),
                crate::theme::shares(*needed),
            ),
            PickerState::Complete => "allocated ✓".to_string(),
        }
    }
}

/// The live estimated gain/tax preview on the proposed allocation, stacked at the
/// sale's `sale_date` position, `[est]`-flagged and degraded on a missing mark. It
/// **never blocks the sell**. (TUI-ENTRY-LOT-005)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GainTaxPreview {
    /// The estimated total gain on the proposed allocation; `None` when degraded
    /// (the mark is unavailable). (TUI-ENTRY-LOT-005)
    pub est_gain: Option<Cents>,
    /// The estimated tax on the gain; `None` when degraded. (TUI-ENTRY-LOT-005)
    pub est_tax: Option<Cents>,
    /// `true` when degraded (a missing mark): the preview shows `‡`, never a zero.
    pub degraded: bool,
}

// ===========================================================================
// Reversal flow (TUI-ENTRY-ACT-005). Pick a target from a recent-events list that
// greys/omits already-reversed events and prior Reversals, showing any backward-
// dependency block inline before submit. (entry-design.md → "Reversal")
// ===========================================================================

/// One reversible-target candidate, from the `runtime`/`ledger-core` reversible-
/// targets read helper. A blocked target shows its dependency-block reason inline
/// (since the kernel's `BadReversal` is an append-time rejection, not a queryable
/// list). (TUI-ENTRY-ACT-005)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ReversalCandidate {
    pub event_id: String,
    /// A one-line summary (`#142  Sell  AMZN  100 @ $95.50  2023-03-09  Robinhood`).
    pub summary: String,
    /// `Some(reason)` when reversing this target is blocked by a later surviving
    /// event that depends on it — shown inline rather than failing on submit; the
    /// candidate is unselectable while blocked. (TUI-ENTRY-ACT-005)
    pub block_reason: Option<String>,
    /// `true` when already reversed or itself a prior Reversal — greyed/omitted.
    pub greyed: bool,
}

impl ReversalCandidate {
    /// Whether this candidate can be selected for reversal (not blocked, not
    /// greyed). (TUI-ENTRY-ACT-005)
    pub fn selectable(&self) -> bool {
        self.block_reason.is_none() && !self.greyed
    }
}

/// Build the reversible-targets list from the surviving ledger log: the recent
/// events, with already-reversed events and prior Reversals greyed, and each
/// candidate's dependency block computed against the current snapshot. (TUI-ENTRY-ACT-005)
///
/// This is the read helper `entry` consumes (`runtime`/`ledger-core` own the real
/// dependency computation; here it is a pure pass over the log so the block is
/// visible *before* committing).
pub fn reversible_targets(log: &[LedgerEvent]) -> Vec<ReversalCandidate> {
    use std::collections::BTreeSet;
    // Already-reversed targets and the Reversal events themselves.
    let mut reversed: BTreeSet<String> = BTreeSet::new();
    for e in log {
        if let LedgerEventKind::Reversal { target_event_id } = &e.kind {
            reversed.insert(target_event_id.clone());
        }
    }
    log.iter()
        .map(|e| {
            let is_reversal = matches!(e.kind, LedgerEventKind::Reversal { .. });
            let greyed = is_reversal || reversed.contains(&e.id);
            // The dependency block: ask the kernel to validate a candidate Reversal
            // of this target against the surviving log. A `BadReversal` that is NOT
            // an "already reversed" / "is a reversal" case is a dependency block.
            let block_reason = if greyed {
                None
            } else {
                let candidate = LedgerEvent {
                    id: format!("__rev_probe__{}", e.id),
                    seq: pt_core::Seq(0),
                    date: e.date,
                    kind: LedgerEventKind::Reversal {
                        target_event_id: e.id.clone(),
                    },
                };
                match ledger_core::validate(log, &candidate) {
                    Ok(()) => None,
                    Err(LedgerError::BadReversal) => Some(format!(
                        "reversing {} is blocked — a later event consumed its lot",
                        e.id
                    )),
                    Err(other) => Some(format!("{other:?}")),
                }
            };
            ReversalCandidate {
                event_id: e.id.clone(),
                summary: summarize_event(e),
                block_reason,
                greyed,
            }
        })
        .collect()
}

/// A one-line ledger-event summary for the Reversal list.
fn summarize_event(e: &LedgerEvent) -> String {
    let kind = match &e.kind {
        LedgerEventKind::Buy {
            symbol,
            qty,
            unit_price_cents,
            platform,
            ..
        } => format!(
            "Buy {symbol} {} @ {} {platform}",
            crate::theme::shares(*qty),
            crate::theme::money(*unit_price_cents)
        ),
        LedgerEventKind::Vest {
            symbol,
            qty,
            fmv_per_share_cents,
            platform,
            ..
        } => format!(
            "Vest {symbol} {} @ {} {platform}",
            crate::theme::shares(*qty),
            crate::theme::money(*fmv_per_share_cents)
        ),
        LedgerEventKind::Sell {
            symbol,
            qty,
            unit_price_cents,
            platform,
            ..
        } => format!(
            "Sell {symbol} {} @ {} {platform}",
            crate::theme::shares(*qty),
            crate::theme::money(*unit_price_cents)
        ),
        LedgerEventKind::Split {
            symbol,
            ratio_num,
            ratio_den,
        } => {
            format!("Split {symbol} {ratio_num}:{ratio_den}")
        }
        LedgerEventKind::Reversal { target_event_id } => format!("Reversal of {target_event_id}"),
    };
    format!("#{}  {kind}  {}", e.seq.0, e.date.0)
}

/// Compose a Reversal `LedgerEvent` for a target. (TUI-ENTRY-ACT-005)
pub fn compose_reversal(target_event_id: &str, date: Date) -> LedgerEvent {
    LedgerEvent {
        id: String::new(),
        seq: pt_core::Seq(0),
        date,
        kind: LedgerEventKind::Reversal {
            target_event_id: target_event_id.to_string(),
        },
    }
}

// ===========================================================================
// Tax-accrual actions (TUI-ENTRY-TAX-*). Each appends a TaxEvent through the write
// loop. Multi-select allows batch actions, snapshotted at confirm + re-validated
// at submit. (entry-design.md → "Tax-Accrual Actions")
// ===========================================================================

/// Compose an Allocate `TaxEvent` (assign an accrual to a reserve account;
/// re-allocatable until Paid). (TUI-ENTRY-TAX-001)
pub fn compose_allocate(key: AccrualKey, account_label: String) -> TaxEvent {
    TaxEvent {
        seq: pt_core::Seq(0),
        kind: TaxEventKind::Allocate {
            accrual_key: key,
            account_label,
        },
    }
}

/// Compose a Move `TaxEvent` (record the **actual** amount + date; may differ from
/// the estimate). (TUI-ENTRY-TAX-002)
pub fn compose_move(key: AccrualKey, amount_cents: Cents, date: Date) -> TaxEvent {
    TaxEvent {
        seq: pt_core::Seq(0),
        kind: TaxEventKind::Move {
            accrual_key: key,
            amount_cents,
            date,
        },
    }
}

/// Compose a Pay `TaxEvent` for a `(jurisdiction, tax_year, period)`, checking off
/// the covered Moved accruals; amount + covered set are independent. (TUI-ENTRY-TAX-003)
pub fn compose_pay(
    jurisdiction: config::Jurisdiction,
    tax_year: config::TaxYear,
    period: tax::Quarter,
    amount_cents: Cents,
    date: Date,
    covers: Vec<AccrualKey>,
) -> TaxEvent {
    TaxEvent {
        seq: pt_core::Seq(0),
        kind: TaxEventKind::Pay {
            jurisdiction,
            tax_year,
            period,
            amount_cents,
            date,
            covers,
        },
    }
}

/// Compose an AmountOverride `TaxEvent` (replace the computed amount with an
/// absolute amount + reason — a logged exception). (TUI-ENTRY-TAX-004)
pub fn compose_override(key: AccrualKey, applied_amount_cents: Cents, reason: String) -> TaxEvent {
    TaxEvent {
        seq: pt_core::Seq(0),
        kind: TaxEventKind::AmountOverride {
            accrual_key: key,
            applied_amount_cents,
            reason,
        },
    }
}

/// The advisory `covered $X / paid $Y ✓` line for a Pay: the covered set's summed
/// applied amount vs the Pay amount. **Advisory** — a partial / over-payment is
/// allowed, surfacing the delta, never a submit gate. (TUI-ENTRY-TAX-003)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PayAdvisory {
    pub covered_cents: Cents,
    pub paid_cents: Cents,
}

impl PayAdvisory {
    /// `true` when the Pay amount equals the covered set's summed amount (the ✓).
    pub fn balanced(&self) -> bool {
        self.covered_cents.0 == self.paid_cents.0
    }

    /// The signed delta `paid − covered` (surfaced when not balanced; never a gate).
    pub fn delta(&self) -> Cents {
        Cents(self.paid_cents.0 - self.covered_cents.0)
    }

    /// The advisory line text. (TUI-ENTRY-TAX-003)
    pub fn text(&self) -> String {
        if self.balanced() {
            format!(
                "covered {} / paid {}  ✓",
                crate::theme::money(self.covered_cents),
                crate::theme::money(self.paid_cents)
            )
        } else {
            format!(
                "covered {} / paid {}  (Δ {})",
                crate::theme::money(self.covered_cents),
                crate::theme::money(self.paid_cents),
                crate::theme::signed_money(self.delta())
            )
        }
    }
}

/// A batch selection of accruals snapshotted at confirm. On submit each is
/// re-validated; a vanished (reversed) or repriced (config-edited) accrual returns
/// to the form with the delta flagged rather than committing a stale set.
/// (TUI-ENTRY-TAX-005)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BatchSnapshot {
    /// The selected accrual keys + their applied amount AT CONFIRM (the snapshot).
    pub selected: Vec<(AccrualKey, Option<Cents>)>,
}

/// Re-validate a batch snapshot against the *current* accruals: any selected key
/// that vanished (no longer an accrual) or repriced (its applied amount changed)
/// is flagged. An empty flag list means the batch is safe to submit. (TUI-ENTRY-TAX-005)
pub fn revalidate_batch(snapshot: &BatchSnapshot, current: &[tax::Accrual]) -> Vec<BatchDelta> {
    let mut out = Vec::new();
    for (key, at_confirm) in &snapshot.selected {
        match current.iter().find(|a| &a.key == key) {
            None => out.push(BatchDelta::Vanished(key.clone())),
            Some(acc) => {
                if acc.applied_cents != *at_confirm {
                    out.push(BatchDelta::Repriced {
                        key: key.clone(),
                        at_confirm: *at_confirm,
                        now: acc.applied_cents,
                    });
                }
            }
        }
    }
    out
}

/// A flagged change in a batch since confirm. (TUI-ENTRY-TAX-005)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum BatchDelta {
    /// The selected accrual vanished (its sale was reversed). (TUI-ENTRY-TAX-005)
    Vanished(AccrualKey),
    /// The selected accrual repriced (a config edit). (TUI-ENTRY-TAX-005)
    Repriced {
        key: AccrualKey,
        at_confirm: Option<Cents>,
        now: Option<Cents>,
    },
}

// ===========================================================================
// Config edits (TUI-ENTRY-CFG-*). Forms over `config`, each running config's
// validation and writing the config tabs / local file. (entry-design.md → "Config
// Edits")
// ===========================================================================

/// A residency-move composer: add `(effective_date, state)`. Future-dated allowed;
/// consecutive same-state rejected; a founding-entry violation renders inline. The
/// change affects only future-dated `accrues_to_state` defaults, not already-stamped
/// events. (TUI-ENTRY-CFG-001)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ResidencyForm {
    pub effective_date: Date,
    pub state: String,
}

impl ResidencyForm {
    /// The note this form always carries: a residency change affects only
    /// **future-dated** `accrues_to_state` defaults. (TUI-ENTRY-CFG-001)
    pub fn future_only_note() -> &'static str {
        "affects only future-dated accrues_to_state defaults, not already-stamped events"
    }

    /// Validate adding this move to the existing timeline (the `config` invariants):
    /// strictly-ascending unique dates + no consecutive same-state. Returns the
    /// `config` error inline on rejection. (TUI-ENTRY-CFG-001)
    pub fn validate(&self, existing: &[config::ResidencyEntry]) -> Result<(), config::ConfigError> {
        let mut entries: Vec<config::ResidencyEntry> = existing.to_vec();
        entries.push(config::ResidencyEntry {
            effective_date: self.effective_date,
            state_code: self.state.clone(),
        });
        entries.sort_by_key(|e| e.effective_date.0);
        config::validate_residency_timeline(&entries)
    }
}

/// The bracket-edit confirm restatement: the retroactive blast radius — "this
/// reprices N unpaid accruals' estimates" (since unpaid accruals are computed-live).
/// (TUI-ENTRY-CFG-002)
pub fn bracket_reprice_notice(unpaid_count: usize) -> String {
    format!("this reprices {unpaid_count} unpaid accruals' estimates")
}

// ===========================================================================
// The write-loop driver (TUI-ENTRY-FLOW-002..005). Submit = confirm (if gated) →
// acquire-lock → append + read-back-verify → confirmed. Inline never overrides
// submit (submit re-validates live). (entry-design.md → write-loop)
// ===========================================================================

/// Run the **submit** for a composed ledger event through the port: the port
/// re-validates against the live state (authoritative), then acquires the lock and
/// appends + read-back-verifies. Maps the port outcome to the composer [`Phase`].
/// A `Confirmed` clears the composer; a lock-held / write-failed returns control
/// with the entry intact + `[r]etry`. (TUI-ENTRY-FLOW-002/003/004/005)
pub fn submit_ledger<P: RuntimePort>(port: &mut P, candidate: &LedgerEvent) -> Phase {
    map_outcome(port.submit_ledger(candidate))
}

/// Submit a Buy through the write loop, **gated by the new-symbol guard**: when the
/// Buy names a symbol matching no existing position and no alias and the owner has
/// not yet confirmed a new position, the submit is held (nothing written) with the
/// new-symbol `warn` shown until `[c]onfirm` — a confirmed (or already-known) symbol
/// submits normally. The warning does not block; it makes opening a new position a
/// deliberate, visible act. (TUI-ENTRY-ACT-006)
pub fn submit_buy<P: RuntimePort>(
    port: &mut P,
    form: &BuyForm,
    aliases: &config::AliasMap,
) -> Phase {
    if let Some(warn) = new_symbol_guard(
        &form.symbol,
        form.confirmed_new_symbol,
        port.view(),
        aliases,
    ) {
        return warn;
    }
    submit_ledger(port, &form.compose())
}

/// Submit a Vest through the write loop, gated by the same new-symbol guard as Buy.
/// (TUI-ENTRY-ACT-006)
pub fn submit_vest<P: RuntimePort>(
    port: &mut P,
    form: &VestForm,
    aliases: &config::AliasMap,
) -> Phase {
    if let Some(warn) = new_symbol_guard(
        &form.symbol,
        form.confirmed_new_symbol,
        port.view(),
        aliases,
    ) {
        return warn;
    }
    submit_ledger(port, &form.compose())
}

/// Submit a Sell through the write loop, **gated by the residency guard**: when the
/// Sell's `accrues_to_state` is `None` — the residency default `residency_on(sale_date)`
/// did not resolve for the sale date, and the owner entered no explicit state — the
/// submit is **blocked** (nothing written) with an inline error beside the
/// accrues-to-state field rather than guessing a jurisdiction. An explicit state
/// (entered or defaulted) submits normally. (CONFIG-RESIDENCY-005)
// @spec CONFIG-RESIDENCY-005
pub fn submit_sell<P: RuntimePort>(port: &mut P, form: &SellForm) -> Phase {
    if let Some(block) = sell_residency_guard(form) {
        return block;
    }
    submit_ledger(port, &form.compose())
}

/// The residency pre-submit guard for a **manual** Sell (CONFIG-RESIDENCY-005): when
/// the Sell's `accrues_to_state` resolved to `None` (the residency timeline had no
/// entry at/before the sale date AND the owner entered no explicit state), block the
/// submit with the inline error below, in the `error` role — never silently stamping
/// a guessed jurisdiction. `None` (the submit proceeds) when an explicit state is
/// present. The kernel accepts a `None` stamp (it just falls to the residency
/// default at tax time), so this is a TUI-level entry gate, not a kernel rule.
/// (CONFIG-RESIDENCY-005)
pub fn sell_residency_guard(form: &SellForm) -> Option<Phase> {
    match &form.accrues_to_state {
        Some(s) if !s.trim().is_empty() => None,
        _ => Some(Phase::Rejected(InlineError::Field(
            missing_accrues_to_state(),
        ))),
    }
}

/// The inline error shown beside the accrues-to-state field when a manual Sell has
/// no resolved residency default and no explicit state. (CONFIG-RESIDENCY-005)
pub fn missing_accrues_to_state() -> String {
    "no residency on the sale date — enter an explicit accrues-to state".to_string()
}

/// The new-symbol pre-submit guard: `Some(Phase::ConfirmNewSymbol(..))` when the
/// symbol is unknown (no position, no alias) and not yet confirmed; `None` when the
/// symbol is known or the new position was confirmed (so the submit proceeds).
/// (TUI-ENTRY-ACT-006)
fn new_symbol_guard(
    symbol: &Symbol,
    confirmed: bool,
    view: &crate::port::ViewState,
    aliases: &config::AliasMap,
) -> Option<Phase> {
    if !confirmed && !symbol_is_known(symbol, &view.snapshot, aliases) {
        Some(Phase::ConfirmNewSymbol(new_symbol_warning(symbol)))
    } else {
        None
    }
}

/// Run the submit for a composed tax event. (TUI-ENTRY-FLOW-002/003/004/005)
pub fn submit_tax<P: RuntimePort>(port: &mut P, candidate: &TaxEvent) -> Phase {
    map_outcome(port.submit_tax(candidate))
}

/// The outcome of a **batch** tax-action submit: either the snapshot re-validated
/// clean and the per-event submit phases ran, or a selected accrual vanished /
/// repriced since confirm and control returned to the form with the deltas flagged
/// (nothing committed). (TUI-ENTRY-TAX-005)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum BatchSubmit {
    /// The batch re-validated clean; the per-candidate submit phases (in order).
    Submitted(Vec<Phase>),
    /// A selected accrual changed since confirm — return to the form, deltas
    /// flagged, nothing committed. (TUI-ENTRY-TAX-005)
    Stale(Vec<BatchDelta>),
}

/// Submit a batch tax action: **re-validate the confirm-time snapshot against the
/// live accruals immediately before submit**; if any selected accrual vanished (its
/// sale was reversed) or repriced (a config edit), return to the form with the delta
/// flagged rather than commit a stale set. Only when the snapshot is clean does it
/// submit the composed events through the write loop. (TUI-ENTRY-TAX-005)
pub fn submit_tax_batch<P: RuntimePort>(
    port: &mut P,
    snapshot: &BatchSnapshot,
    candidates: &[TaxEvent],
) -> BatchSubmit {
    let deltas = revalidate_batch(snapshot, &port.view().accruals);
    if !deltas.is_empty() {
        return BatchSubmit::Stale(deltas);
    }
    let phases = candidates.iter().map(|c| submit_tax(port, c)).collect();
    BatchSubmit::Submitted(phases)
}

fn map_outcome(outcome: SubmitOutcome) -> Phase {
    match outcome {
        SubmitOutcome::Confirmed(_) => Phase::Confirmed,
        // A live kernel disagreement re-renders inline beside the field — NOT a
        // write-verify retry. (TUI-ENTRY-FLOW-002)
        SubmitOutcome::Rejected(SubmitRejection::Ledger(e)) => {
            Phase::Rejected(InlineError::Ledger(e))
        }
        SubmitOutcome::Rejected(SubmitRejection::Tax(e)) => Phase::Rejected(InlineError::Tax(e)),
        SubmitOutcome::LockHeld { holder } => Phase::Retry(RetryReason::LockHeld { holder }),
        SubmitOutcome::WriteFailed(WriteFailure::VerifyMismatch) => {
            Phase::Retry(RetryReason::VerifyMismatch)
        }
        SubmitOutcome::WriteFailed(WriteFailure::Unreachable) => {
            Phase::Retry(RetryReason::Unreachable)
        }
    }
}

/// Run the **inline advisory** validation of a candidate ledger event against the
/// cached snapshot. Renders nothing on `Ok`; on `Err` the verified `LedgerError`
/// is returned for inline rendering beside the field — nothing is written.
/// (TUI-ENTRY-FLOW-001)
pub fn inline_validate_ledger<P: RuntimePort>(
    port: &P,
    candidate: &LedgerEvent,
) -> Option<InlineError> {
    port.validate_ledger(candidate)
        .err()
        .map(InlineError::Ledger)
}

/// Inline advisory validation of a candidate tax event. (TUI-ENTRY-FLOW-001)
pub fn inline_validate_tax<P: RuntimePort>(port: &P, candidate: &TaxEvent) -> Option<InlineError> {
    port.validate_tax(candidate).err().map(InlineError::Tax)
}

// ===========================================================================
// The field-string → typed-candidate re-parse (TUI-ENTRY-FLOW-008/009; the
// inverse of the per-flow field builders in `form`). The render layer projects a
// composed form to `Field` strings one way (`$X.YY` money, formatted shares, a raw
// integer day-key); this is the inverse the live submit runs over the *edited*
// field strings before `compose()`. A malformed/empty/oversized numeric field
// surfaces an `InlineError::Field` beside that field rather than panicking — the
// composer-local parse error precedes the kernel. (TUI-ENTRY-ACT-001/002/003/004)
// ===========================================================================

/// Parse a money field rendered as `$1,284.30` (the `theme::money` form) back into
/// `Cents`. Accepts an optional leading `$` and `,` group separators, an optional
/// leading sign, and 0–2 decimal places; rejects anything else. (TUI-ENTRY-FLOW-008)
pub fn parse_money(s: &str) -> Result<Cents, InlineError> {
    let t = s.trim();
    if t.is_empty() {
        return Err(InlineError::Field("enter an amount".to_string()));
    }
    let (neg, body) = match t.strip_prefix('-').or_else(|| t.strip_prefix('\u{2212}')) {
        Some(rest) => (true, rest),
        None => (false, t),
    };
    let body = body.trim_start().trim_start_matches('$');
    let cleaned: String = body.chars().filter(|c| *c != ',').collect();
    if cleaned.is_empty() {
        return Err(InlineError::Field(format!("not a valid amount: '{s}'")));
    }
    let (whole_str, frac_str) = match cleaned.split_once('.') {
        Some((w, f)) => (w, f),
        None => (cleaned.as_str(), ""),
    };
    if frac_str.len() > 2 {
        return Err(InlineError::Field(format!(
            "at most two decimal places: '{s}'"
        )));
    }
    let whole_part = if whole_str.is_empty() { "0" } else { whole_str };
    let whole: i64 = whole_part
        .parse()
        .map_err(|_| InlineError::Field(format!("not a valid amount: '{s}'")))?;
    let cents_frac: i64 = if frac_str.is_empty() {
        0
    } else {
        let padded = format!("{frac_str:0<2}");
        padded
            .parse()
            .map_err(|_| InlineError::Field(format!("not a valid amount: '{s}'")))?
    };
    let magnitude = whole
        .checked_mul(100)
        .and_then(|w| w.checked_add(cents_frac))
        .ok_or_else(|| InlineError::Field(format!("amount out of range: '{s}'")))?;
    let cents = if neg { -magnitude } else { magnitude };
    if cents.unsigned_abs() as i64 > pt_core::MONEY_CAP {
        return Err(InlineError::Field(format!("amount out of range: '{s}'")));
    }
    Ok(Cents(cents))
}

/// Parse a share-count field rendered as `834` / `1.5` (the `theme::shares` form)
/// back into `MicroShares`. Accepts an optional sign and up to six fractional
/// digits; rejects anything else or an out-of-range magnitude. (TUI-ENTRY-FLOW-008)
pub fn parse_shares(s: &str) -> Result<MicroShares, InlineError> {
    let t = s.trim();
    if t.is_empty() {
        return Err(InlineError::Field("enter a share count".to_string()));
    }
    let (neg, body) = match t.strip_prefix('-').or_else(|| t.strip_prefix('\u{2212}')) {
        Some(rest) => (true, rest),
        None => (false, t),
    };
    let cleaned: String = body.chars().filter(|c| *c != ',').collect();
    let (whole_str, frac_str) = match cleaned.split_once('.') {
        Some((w, f)) => (w, f),
        None => (cleaned.as_str(), ""),
    };
    if frac_str.len() > 6 {
        return Err(InlineError::Field(format!(
            "at most six decimal places: '{s}'"
        )));
    }
    let whole_part = if whole_str.is_empty() { "0" } else { whole_str };
    let whole: i64 = whole_part
        .parse()
        .map_err(|_| InlineError::Field(format!("not a valid share count: '{s}'")))?;
    let micro_frac: i64 = if frac_str.is_empty() {
        0
    } else {
        let padded = format!("{frac_str:0<6}");
        padded
            .parse()
            .map_err(|_| InlineError::Field(format!("not a valid share count: '{s}'")))?
    };
    let magnitude = whole
        .checked_mul(pt_core::SHARE_SCALE)
        .and_then(|w| w.checked_add(micro_frac))
        .ok_or_else(|| InlineError::Field(format!("share count out of range: '{s}'")))?;
    Ok(MicroShares(if neg { -magnitude } else { magnitude }))
}

/// Parse a date field rendered as the raw integer day-key (the `date_field` form)
/// back into a `Date`. (TUI-ENTRY-FLOW-008)
pub fn parse_date(s: &str) -> Result<Date, InlineError> {
    let t = s.trim();
    if t.is_empty() {
        return Err(InlineError::Field("enter a date (day key)".to_string()));
    }
    t.parse::<i32>()
        .map(Date)
        .map_err(|_| InlineError::Field(format!("not a valid day key: '{s}'")))
}

/// Parse an `a:b` split-ratio field back into `(num, den)`. (TUI-ENTRY-FLOW-008)
pub fn parse_ratio(s: &str) -> Result<(i64, i64), InlineError> {
    let t = s.trim();
    let (num, den) = t
        .split_once(':')
        .ok_or_else(|| InlineError::Field(format!("ratio must be num:den, got '{s}'")))?;
    let num: i64 = num
        .trim()
        .parse()
        .map_err(|_| InlineError::Field(format!("not a valid ratio numerator: '{s}'")))?;
    let den: i64 = den
        .trim()
        .parse()
        .map_err(|_| InlineError::Field(format!("not a valid ratio denominator: '{s}'")))?;
    Ok((num, den))
}

/// An optional tracking-code field: blank ⇒ `None`, else `Some(trimmed)`.
fn parse_optional(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

impl BuyForm {
    /// Re-parse the rendered+edited [`crate::form::Field`] strings (in the
    /// `for_buy` tab order: symbol, qty, unit price, date, fees, platform, tracking
    /// code) back into the typed form, so the live submit composes the OWNER'S edits.
    /// A bad/empty/oversized numeric field returns an `InlineError::Field` beside
    /// that field index. (TUI-ENTRY-ACT-001; TUI-ENTRY-FLOW-008)
    pub fn apply_fields(
        &mut self,
        fields: &[crate::form::Field],
    ) -> Result<(), (usize, InlineError)> {
        self.symbol = fields
            .first()
            .map(|f| f.value.trim().to_string())
            .unwrap_or_default();
        self.qty = parse_shares(field_at(fields, 1)).map_err(|e| (1, e))?;
        self.unit_price = parse_money(field_at(fields, 2)).map_err(|e| (2, e))?;
        self.date = parse_date(field_at(fields, 3)).map_err(|e| (3, e))?;
        self.fees = parse_money(field_at(fields, 4)).map_err(|e| (4, e))?;
        self.platform = field_at(fields, 5).trim().to_string();
        self.tracking_code = parse_optional(field_at(fields, 6));
        Ok(())
    }
}

impl VestForm {
    /// Re-parse the `for_vest` fields (symbol, qty, FMV/share, date, platform,
    /// tracking code) back into the typed form. (TUI-ENTRY-ACT-002; TUI-ENTRY-FLOW-008)
    pub fn apply_fields(
        &mut self,
        fields: &[crate::form::Field],
    ) -> Result<(), (usize, InlineError)> {
        self.symbol = fields
            .first()
            .map(|f| f.value.trim().to_string())
            .unwrap_or_default();
        self.qty = parse_shares(field_at(fields, 1)).map_err(|e| (1, e))?;
        self.fmv_per_share = parse_money(field_at(fields, 2)).map_err(|e| (2, e))?;
        self.date = parse_date(field_at(fields, 3)).map_err(|e| (3, e))?;
        self.platform = field_at(fields, 4).trim().to_string();
        self.tracking_code = parse_optional(field_at(fields, 5));
        Ok(())
    }
}

impl SplitForm {
    /// Re-parse the `for_split` fields (symbol, ratio num:den, date). (TUI-ENTRY-ACT-004;
    /// TUI-ENTRY-FLOW-008)
    pub fn apply_fields(
        &mut self,
        fields: &[crate::form::Field],
    ) -> Result<(), (usize, InlineError)> {
        self.symbol = fields
            .first()
            .map(|f| f.value.trim().to_string())
            .unwrap_or_default();
        let (num, den) = parse_ratio(field_at(fields, 1)).map_err(|e| (1, e))?;
        self.ratio_num = num;
        self.ratio_den = den;
        self.date = parse_date(field_at(fields, 2)).map_err(|e| (2, e))?;
        Ok(())
    }
}

impl SellForm {
    /// Re-parse the `for_sell` fields (symbol, qty, unit price, date, fees, platform,
    /// accrues-to-state, tracking code) back into the typed form. A changed qty
    /// **resets the lot-picker allocation** (TUI-ENTRY-LOT-003) so a stale ✓ can never
    /// reach submit; the picker allocation itself is edited through the picker
    /// context. (TUI-ENTRY-ACT-003; TUI-ENTRY-FLOW-008)
    pub fn apply_fields(
        &mut self,
        fields: &[crate::form::Field],
    ) -> Result<(), (usize, InlineError)> {
        self.symbol = fields
            .first()
            .map(|f| f.value.trim().to_string())
            .unwrap_or_default();
        let qty = parse_shares(field_at(fields, 1)).map_err(|e| (1, e))?;
        if qty != self.qty {
            self.qty = qty;
            self.picker.set_sale_qty(qty);
        }
        self.unit_price = parse_money(field_at(fields, 2)).map_err(|e| (2, e))?;
        self.date = parse_date(field_at(fields, 3)).map_err(|e| (3, e))?;
        self.fees = parse_money(field_at(fields, 4)).map_err(|e| (4, e))?;
        self.platform = field_at(fields, 5).trim().to_string();
        self.accrues_to_state = parse_optional(field_at(fields, 6));
        self.tracking_code = parse_optional(field_at(fields, 7));
        Ok(())
    }
}

impl ResidencyForm {
    /// Re-parse the `for_residency` fields (effective date, state). (TUI-ENTRY-CFG-001;
    /// TUI-ENTRY-FLOW-008)
    pub fn apply_fields(
        &mut self,
        fields: &[crate::form::Field],
    ) -> Result<(), (usize, InlineError)> {
        self.effective_date = parse_date(field_at(fields, 0)).map_err(|e| (0, e))?;
        self.state = field_at(fields, 1).trim().to_string();
        Ok(())
    }
}

/// The value of the field at `i`, or the empty string when absent (a short field
/// list surfaces as an empty parse rather than panicking).
fn field_at(fields: &[crate::form::Field], i: usize) -> &str {
    fields.get(i).map(|f| f.value.as_str()).unwrap_or("")
}

// ===========================================================================
// The typed composer (TUI-ENTRY-FLOW-008/010). An `EntryContext` carries a typed
// `Composer` alongside its render-only `FormModel`: the `FormModel` owns *how the
// fields are navigated/edited*, the `Composer` owns *what is composed*. On submit
// the live loop re-parses the edited field strings into the composer
// (`apply_fields`), composes the candidate, and submits it through the port. The
// composer is the home for the FLOW-010 freeze: the composed event is frozen at
// first submit and re-submitted byte-identical on `[r]etry`. (entry-design.md →
// "The Composer & Write Loop")
// ===========================================================================

/// The typed composer an Entry context drives — the source of truth for *what is
/// composed*, re-parsed from the edited [`crate::form::Field`] strings at submit.
/// (TUI-ENTRY-FLOW-008)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Composer {
    Buy(BuyForm),
    Vest(VestForm),
    Sell(SellForm),
    Split(SplitForm),
    Residency(ResidencyForm),
}

/// A composed candidate event awaiting submit — a ledger event, a tax event, or a
/// config edit (validated by `config`, not appended through the event log). The
/// frozen value re-submitted byte-identical on `[r]etry`. (TUI-ENTRY-FLOW-010)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Candidate {
    Ledger(LedgerEvent),
    Tax(TaxEvent),
}

impl Composer {
    /// The flow this composer drives (fixes the confirm-gating + the render title).
    pub fn kind(&self) -> FlowKind {
        match self {
            Composer::Buy(_) => FlowKind::Buy,
            Composer::Vest(_) => FlowKind::Vest,
            Composer::Sell(_) => FlowKind::Sell,
            Composer::Split(_) => FlowKind::Split,
            Composer::Residency(_) => FlowKind::ResidencyEdit,
        }
    }

    /// Re-parse the edited field strings into the typed composer (the inverse of the
    /// `form::for_*` builders). On a bad/empty/oversized numeric field, returns the
    /// offending field index + the inline error so the shell pins it beneath that
    /// field. (TUI-ENTRY-FLOW-008)
    pub fn apply_fields(
        &mut self,
        fields: &[crate::form::Field],
    ) -> Result<(), (usize, InlineError)> {
        match self {
            Composer::Buy(f) => f.apply_fields(fields),
            Composer::Vest(f) => f.apply_fields(fields),
            Composer::Sell(f) => f.apply_fields(fields),
            Composer::Split(f) => f.apply_fields(fields),
            Composer::Residency(_) => {
                // Residency is a `config` edit, not a ledger candidate; its fields are
                // applied through `residency_form_mut` when its own submit lands.
                Ok(())
            }
        }
    }

    /// Compose the current typed form into the candidate event the submit appends, or
    /// `None` for a config-only flow (residency) that does not append to the event
    /// log. (TUI-ENTRY-ACT-001/002/003/004)
    pub fn compose(&self) -> Option<Candidate> {
        match self {
            Composer::Buy(f) => Some(Candidate::Ledger(f.compose())),
            Composer::Vest(f) => Some(Candidate::Ledger(f.compose())),
            Composer::Sell(f) => Some(Candidate::Ledger(f.compose())),
            Composer::Split(f) => Some(Candidate::Ledger(f.compose())),
            Composer::Residency(_) => None,
        }
    }

    /// Mutable access to the Sell's lot picker, when this composer is a Sell — the
    /// picker context edits the allocation here so `compose()` reads the owner's
    /// picks. (TUI-ENTRY-LOT-001/002/003)
    pub fn picker_mut(&mut self) -> Option<&mut LotPicker> {
        match self {
            Composer::Sell(f) => Some(&mut f.picker),
            _ => None,
        }
    }
}

// ===========================================================================
// Launched-with-identity (entry-design.md → "Launched with identity"). When
// `views` launches a flow on a selection, `entry` receives only the IDENTITY and
// re-resolves it live against the current Snapshot at flow start. (TUI-VIEW-TAX-002)
// ===========================================================================

/// The identity `views` passes when launching an `entry` flow on a selection — an
/// accrual key, a lot id, or a symbol. `entry` re-resolves it live at flow start
/// (never the rendered, possibly-stale values). (entry-design.md → "Launched with
/// identity"; TUI-VIEW-TAX-002)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LaunchIdentity {
    Accrual(AccrualKey),
    Lot(String),
    Symbol(Symbol),
}

/// Re-resolve a launched accrual identity live against the current accruals,
/// returning the fresh accrual (never the stale rendered values). `None` when the
/// accrual vanished (its sale was reversed since the view rendered). (entry-design.md
/// → "Launched with identity"; TUI-VIEW-TAX-002)
pub fn reresolve_accrual<'a>(
    key: &AccrualKey,
    accruals: &'a [tax::Accrual],
) -> Option<&'a tax::Accrual> {
    accruals.iter().find(|a| &a.key == key)
}
