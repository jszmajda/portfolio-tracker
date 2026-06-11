//! `reports` — the portfolio-analytics segment.
//!
//! Computes **composition**, **value-over-time** (the durable value History), and
//! **realized-P&L history** as pure functions over `ledger-core`'s `Snapshot`, the
//! current marks (with their quote-epoch), `tax`'s per-position unrealized
//! estimate, the reporting timezone from `config`, and a persisted value History.
//! The TUI and `summary` render what it produces. It does **not** own the
//! quarterly/annual *tax* reports — those are `tax`'s (`TAX-REPORT-*`); `reports`
//! covers the non-tax analytics. See `docs/intent/reports/reports-design.md` and
//! `-specs.md` (prefix `REPORT`).
//!
//! The defining design fact: **the value series is the one piece of derived data
//! that is not reconstructable.** Positions, basis, and realized P&L can all be
//! replayed from the event log at any time — but a past day's portfolio *value*
//! depends on that day's *marks*, and `GOOGLEFINANCE` only gives live prices. So
//! `reports` splits its work by reconstructability:
//!
//! - **Reconstructable** (recomputed on demand): composition + realized history.
//! - **Non-reconstructable** (captured + durably persisted): the value/unrealized
//!   History series, authoritative in the workbook, defended like `store`'s log.
//!
//! `reports` is aggregation, not money-conservation, so it is **unit-tested**, not
//! Verus-proven (CLAUDE.md "cargo test is the gate"). Money is integer
//! `pt_core::Cents`; no float ever enters past the input boundary.

use std::collections::BTreeMap;

use ledger_core::{Snapshot, Symbol};
use pt_core::{Cents, Date, MicroShares};
use store::Lock;
use tax::UnrealizedEstimate;

pub mod testkit;

// ===========================================================================
// The History tab (reports owns its schema; sheets-view places it among the
// view tabs — see sheets_view::HISTORY_TAB). reports does NOT depend on
// sheets-view to write it; the name is re-exported for callers placing the tab.
// ===========================================================================

/// The History tab name (the durable value series). `reports` owns its schema;
/// `sheets-view`'s tab-ordering places it among the view tabs. (REPORT-HIST-001)
pub const HISTORY_TAB: &str = sheets_view::HISTORY_TAB;

// ===========================================================================
// Trading-day key (reports-design.md → "Value-Over-Time"; REPORT-VOT-001). The
// series key is the trading-day key `runtime` supplies — its reduction of the
// per-symbol quote-epochs (most-recent across priced symbols), NOT the calendar
// day of the run. reports treats it as an opaque, ordered key.
// ===========================================================================

/// The series key for one value-series point: the **trading-day key** `runtime`
/// supplies (its reduction of the per-symbol quote-epochs — most-recent across
/// priced symbols). It is `pt_core::Date`-shaped (days since epoch) so the series
/// can be ordered/compared, but it is the marks' quote-epoch trading day, **not**
/// the calendar day of the run (which would fabricate flat weekend/holiday
/// segments). (REPORT-VOT-001)
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct TradingDayKey(pub Date);

// ===========================================================================
// Marks with their quote-epoch (the composition + capture input). reports keys
// the value series by the trading-day key runtime reduces from these; a symbol
// with no mark is degraded (no entry), never a zero. Mirrors the shape of
// sheets_view::Mark (price_cents + quote_date), but reports takes a per-symbol
// Option so a degraded symbol is explicit. (reports-design.md → "Inputs")
// ===========================================================================

/// One per-symbol current mark for composition/capture: a price in `Cents`
/// stamped with `GOOGLEFINANCE`'s quote-epoch trading day. A symbol with **no**
/// mark is simply absent from the marks map (degraded), never a zero entry.
/// (reports-design.md → "Inputs"; matches `sheets_view::Mark`.)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PricedMark {
    pub price_cents: Cents,
    /// The mark's quote-epoch (`GOOGLEFINANCE`'s quote date), per symbol.
    pub quote_epoch: Date,
}

/// The marks supplied for composition/capture: symbol → priced mark. A symbol
/// **absent** here is degraded (no mark) — its composition row is degraded and
/// its History day is flagged incomplete, never valued at zero. (REPORT-COMP-002,
/// REPORT-VOT-004)
pub type PricedMarks = BTreeMap<Symbol, PricedMark>;

// ===========================================================================
// Composition (reports-design.md → "Portfolio Composition"; REPORT-COMP-*).
// Allocation of the portfolio as of now, from the Snapshot + marks + tax's
// unrealized estimate. Per symbol and per platform; pre- and post-tax; a
// consistent degraded set; the priced-coverage fraction; n/a on total <= 0.
// ===========================================================================

/// Why a composition row is degraded: it has **no mark**, or its **tax estimate**
/// is unavailable. A row degraded for *either* reason is excluded from the
/// percentage denominator in **both** the pre- and post-tax views (the consistent
/// degraded set). (REPORT-COMP-002)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DegradeCause {
    /// No current mark for this symbol — market value / unrealized unknown.
    NoMark,
    /// The mark is present but `tax`'s unrealized estimate is unavailable
    /// (e.g. `NoBracketsAvailable`) — net-of-tax unknown.
    NoTaxEstimate,
}

/// One composition row, per **symbol** or per **platform**. `market_value_cents`,
/// `unrealized_pretax_cents`, `unrealized_net_of_tax_cents`, and `share_ppm` are
/// `None` when the row is degraded — never a fabricated zero. `total_basis_cents`
/// always survives degradation (basis is reconstructable). (REPORT-COMP-001..004)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CompositionRow {
    /// The symbol (per-symbol rows) or platform name (per-platform rows).
    pub label: String,
    /// Market value at the current mark; `None` when degraded (no mark).
    pub market_value_cents: Option<Cents>,
    /// Share of the **priced** total in `ppm` (so priced shares sum to 1_000_000);
    /// `None` when this row is degraded OR when priced total ≤ 0 (n/a).
    /// (REPORT-COMP-002/004)
    pub share_ppm: Option<config::Ppm>,
    /// Total cost basis (survives degradation — basis is reconstructable).
    pub total_basis_cents: Cents,
    /// Pre-tax unrealized P&L (`market_value − basis`); `None` when degraded.
    pub unrealized_pretax_cents: Option<Cents>,
    /// Pre-tax unrealized as a fraction of total basis in `ppm` (rounded
    /// half-to-even) — the unr-% of basis. `None` when the row is degraded or its
    /// basis is ≤ 0 (no meaningful %-of-basis — never a fabricated or nonsensical
    /// percentage). (REPORT-COMP-006)
    pub unrealized_pct_of_basis_ppm: Option<config::Ppm>,
    /// Net-of-tax unrealized (`tax`'s estimate applied); `None` when degraded.
    /// (REPORT-COMP-001)
    pub unrealized_net_of_tax_cents: Option<Cents>,
    /// `Some(cause)` when this row is degraded (excluded from the % denominator in
    /// both views), else `None`. (REPORT-COMP-002)
    pub degraded: Option<DegradeCause>,
    /// `true` when this position carries a **negative** total basis — flagged
    /// rather than silently producing a nonsensical basis share. (REPORT-COMP-004)
    pub negative_basis: bool,
}

/// Whether the priced book has any value at all, distinguishing the two
/// degenerate cases the shares-n/a label must tell apart. (REPORT-COMP-004)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PricedTotalState {
    /// Priced market value > 0 — shares are well-defined.
    Positive,
    /// There are no positions at all (empty portfolio). Shares are n/a.
    NoPositions,
    /// Positions exist but every one is unpriced (degraded), so priced total ≤ 0.
    /// Shares are n/a — distinct from "no positions". (REPORT-COMP-004)
    PositionsExistButUnpriced,
}

/// The full composition: per-symbol and per-platform rows (both pre/post-tax in
/// each row), the degraded-symbol count, the priced-coverage fraction, and the
/// priced-total state that drives the shares-n/a label. (REPORT-COMP-001..004)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Composition {
    /// Per-symbol rows, ordered by symbol.
    pub by_symbol: Vec<CompositionRow>,
    /// Per-platform rows, ordered by platform name.
    pub by_platform: Vec<CompositionRow>,
    /// Count of degraded symbols (no mark or no tax estimate). (REPORT-COMP-003)
    pub degraded_count: usize,
    /// The priced-coverage fraction = priced basis ÷ total basis, in `ppm`
    /// (`1_000_000` = 100%). `None` when total basis is 0. (REPORT-COMP-003)
    pub priced_coverage_ppm: Option<config::Ppm>,
    /// The priced-total state, distinguishing positive / no-positions /
    /// positions-exist-but-unpriced for the shares label. (REPORT-COMP-004)
    pub priced_total_state: PricedTotalState,
}

/// Compute portfolio composition from the `Snapshot`, the current `marks` (with
/// per-symbol quote-epoch; a symbol absent is degraded), and `tax`'s per-symbol
/// unrealized `estimates` (a symbol absent, or whose `estimated_tax_cents` is
/// `None`, has no tax estimate → degraded post-tax, hence degraded in both views).
///
/// Per **symbol** and per **platform**: market value, share of the priced total
/// (`ppm`), total basis, pre-tax unrealized, and net-of-tax unrealized — both
/// pre- and post-tax in each row. A symbol degraded in either its mark or its tax
/// estimate is excluded from the **same** percentage denominator in both views
/// (REPORT-COMP-002), the priced-coverage fraction is surfaced (REPORT-COMP-003),
/// shares are n/a on priced total ≤ 0 with the no-positions / unpriced distinction
/// (REPORT-COMP-004), and a negative-basis position is flagged (REPORT-COMP-004).
/// Net-of-tax is aggregated across a symbol's lots and platforms by distributing
/// the symbol's exact estimated tax over its sub-positions by pre-tax weight using
/// largest-remainder apportionment, so the per-lot and per-platform net-of-tax
/// sub-totals reconcile exactly to the symbol's estimated tax (REPORT-COMP-005).
/// Each row carries its pre-tax unrealized as a fraction of basis in ppm, n/a
/// when degraded or basis ≤ 0 (REPORT-COMP-006). (REPORT-COMP-001..006)
pub fn compose(
    snapshot: &Snapshot,
    marks: &PricedMarks,
    estimates: &BTreeMap<Symbol, UnrealizedEstimate>,
) -> Composition {
    // --- Per-symbol intermediate facts (one per position). ---
    struct SymFact {
        label: String,
        basis: i64,
        // Market value at the current mark; None when degraded (no mark).
        market_value: Option<i64>,
        // Pre-tax unrealized P&L (market_value − basis); None when degraded.
        unrealized_pretax: Option<i64>,
        // Net-of-tax unrealized; None when degraded (no mark or no tax estimate).
        net_of_tax: Option<i64>,
        degraded: Option<DegradeCause>,
        negative_basis: bool,
    }

    let symbol_fact = |symbol: &Symbol, basis: i64, total_qty: MicroShares| -> SymFact {
        let negative_basis = basis < 0;
        match marks.get(symbol) {
            // No mark → degraded (NoMark) in BOTH views; never a fabricated zero.
            None => SymFact {
                label: symbol.clone(),
                basis,
                market_value: None,
                unrealized_pretax: None,
                net_of_tax: None,
                degraded: Some(DegradeCause::NoMark),
                negative_basis,
            },
            Some(mark) => {
                // Market value = scale(mark × total_qty) (the single rounding site).
                let mv = pt_core::scale(
                    (mark.price_cents.0 as i128) * (total_qty.0 as i128),
                ) as i64;
                let pretax = mv - basis;
                // The tax estimate is degraded when absent or its estimated tax is
                // None — degraded in BOTH views (the consistent degraded set).
                let est = estimates.get(symbol);
                let est_tax = est.and_then(|e| e.estimated_tax_cents);
                match est_tax {
                    None => SymFact {
                        label: symbol.clone(),
                        basis,
                        // Mark IS present, but the consistent-degraded-set rule
                        // excludes this symbol from BOTH views' denominators, so its
                        // value columns are degraded together (never a half-row).
                        market_value: None,
                        unrealized_pretax: None,
                        net_of_tax: None,
                        degraded: Some(DegradeCause::NoTaxEstimate),
                        negative_basis,
                    },
                    Some(tax_cents) => SymFact {
                        label: symbol.clone(),
                        basis,
                        market_value: Some(mv),
                        unrealized_pretax: Some(pretax),
                        // Net-of-tax unrealized = pretax − estimated tax on it.
                        net_of_tax: Some(pretax - tax_cents.0),
                        degraded: None,
                        negative_basis,
                    },
                }
            }
        }
    };

    let mut sym_facts: Vec<SymFact> = Vec::new();
    for (symbol, pos) in &snapshot.positions {
        // A position with no open shares is not part of the live allocation.
        if pos.total_qty.0 == 0 {
            continue;
        }
        sym_facts.push(symbol_fact(symbol, pos.total_basis_cents.0, pos.total_qty));
    }

    // --- Per-platform intermediate facts (aggregate open lots by platform). ---
    // A platform is degraded if ANY symbol held on it is in the degraded set, so
    // the pre/post-tax platform columns stay comparable (consistent degraded set).
    let degraded_symbols: std::collections::BTreeSet<&str> = sym_facts
        .iter()
        .filter(|f| f.degraded.is_some())
        .map(|f| f.label.as_str())
        .collect();

    struct PlatAgg {
        basis: i64,
        market_value: i64,
        net_of_tax: i64,
        degraded: bool,
    }
    let mut plat: BTreeMap<String, PlatAgg> = BTreeMap::new();
    // Net-of-tax must use ONE definition everywhere (REPORT-COMP-001). The
    // authoritative figure is the per-symbol exact `estimated_tax_cents`; the
    // per-platform and value-series sub-totals are made to RECONCILE to it rather
    // than re-deriving net by reapplying the rounded effective rate per lot (which
    // double-rounds and would not sum to the per-symbol exact net). So for each
    // priced symbol we distribute its exact `estimated_tax_cents` across that
    // symbol's open lots by each lot's positive-clamped pretax weight (largest-
    // remainder, like `shares_ppm`), so the per-lot tax shares sum EXACTLY to the
    // symbol's estimated tax, and the platform net-of-tax = Σ(lot pretax − lot tax
    // share) reconciles to the per-symbol exact net. (REPORT-COMP-001)
    //
    // First pass: record each priced lot's (platform, pretax) under its symbol, and
    // aggregate basis/market-value/degradation per platform.
    struct LotShare<'a> {
        platform: &'a str,
        pretax: i64,
    }
    let mut lots_by_symbol: BTreeMap<&str, Vec<LotShare>> = BTreeMap::new();
    for ol in &snapshot.open_lots {
        if ol.lot.remaining_qty.0 == 0 {
            continue;
        }
        let symbol = &ol.lot.symbol;
        let entry = plat.entry(ol.lot.platform.clone()).or_insert(PlatAgg {
            basis: 0,
            market_value: 0,
            net_of_tax: 0,
            degraded: false,
        });
        entry.basis += ol.lot.remaining_basis_cents.0;
        if degraded_symbols.contains(symbol.as_str()) {
            entry.degraded = true;
            continue;
        }
        match marks.get(symbol) {
            None => entry.degraded = true,
            Some(mark) => {
                let mv = pt_core::scale(
                    (mark.price_cents.0 as i128) * (ol.lot.remaining_qty.0 as i128),
                ) as i64;
                entry.market_value += mv;
                let lot_pretax = mv - ol.lot.remaining_basis_cents.0;
                lots_by_symbol
                    .entry(symbol.as_str())
                    .or_default()
                    .push(LotShare { platform: ol.lot.platform.as_str(), pretax: lot_pretax });
            }
        }
    }
    // Second pass: distribute each priced symbol's EXACT estimated tax across its
    // lots by positive-clamped pretax weight (largest-remainder), then credit each
    // lot's net-of-tax (pretax − its tax share) to its platform. The per-symbol
    // exact net (pretax − estimated_tax) is the authority; the platform sum equals
    // it because the distributed tax shares sum to estimated_tax exactly.
    for (symbol, lots) in &lots_by_symbol {
        let est_tax = estimates
            .get(*symbol)
            .and_then(|e| e.estimated_tax_cents)
            .map(|c| c.0)
            .unwrap_or(0);
        let weights: Vec<i64> = lots.iter().map(|l| l.pretax.max(0)).collect();
        let tax_shares = distribute_largest_remainder(est_tax, &weights);
        for (lot, tax_share) in lots.iter().zip(tax_shares) {
            if let Some(entry) = plat.get_mut(lot.platform) {
                entry.net_of_tax += lot.pretax - tax_share;
            }
        }
    }

    // --- Priced totals & the shares denominator. ---
    let priced_mv_total: i64 = sym_facts
        .iter()
        .filter(|f| f.degraded.is_none())
        .filter_map(|f| f.market_value)
        .sum();

    // "No positions" means no OPEN positions: zero-qty (fully closed-out) holdings
    // are excluded above (a closed-out book has no live allocation), so a snapshot
    // whose positions map is non-empty but every position is closed reports
    // `NoPositions`, not `PositionsExistButUnpriced`. (REPORT-COMP-004)
    let priced_total_state = if sym_facts.is_empty() {
        PricedTotalState::NoPositions
    } else if priced_mv_total > 0 {
        PricedTotalState::Positive
    } else {
        // Positions exist but the priced market value is ≤ 0 (all unpriced).
        PricedTotalState::PositionsExistButUnpriced
    };

    // Largest-remainder shares (ppm) over the priced symbols, so priced shares sum
    // to exactly 1_000_000 when the priced total is positive; n/a otherwise.
    let sym_shares: Vec<Option<config::Ppm>> = shares_ppm(
        &sym_facts
            .iter()
            .map(|f| if f.degraded.is_none() { f.market_value } else { None })
            .collect::<Vec<_>>(),
        priced_mv_total,
    );

    let by_symbol: Vec<CompositionRow> = sym_facts
        .iter()
        .zip(sym_shares)
        .map(|(f, share)| CompositionRow {
            label: f.label.clone(),
            market_value_cents: f.market_value.map(Cents),
            share_ppm: share,
            total_basis_cents: Cents(f.basis),
            unrealized_pretax_cents: f.unrealized_pretax.map(Cents),
            unrealized_pct_of_basis_ppm: pct_of_basis_ppm(f.unrealized_pretax, f.basis),
            unrealized_net_of_tax_cents: f.net_of_tax.map(Cents),
            degraded: f.degraded,
            negative_basis: f.negative_basis,
        })
        .collect();

    // Platform rows + shares over priced platforms.
    let plat_vec: Vec<(String, PlatAgg)> = plat.into_iter().collect();
    let priced_plat_total: i64 = plat_vec
        .iter()
        .filter(|(_, a)| !a.degraded)
        .map(|(_, a)| a.market_value)
        .sum();
    let plat_shares = shares_ppm(
        &plat_vec
            .iter()
            .map(|(_, a)| if !a.degraded { Some(a.market_value) } else { None })
            .collect::<Vec<_>>(),
        priced_plat_total,
    );
    let by_platform: Vec<CompositionRow> = plat_vec
        .iter()
        .zip(plat_shares)
        .map(|((name, a), share)| CompositionRow {
            label: name.clone(),
            market_value_cents: if a.degraded { None } else { Some(Cents(a.market_value)) },
            share_ppm: share,
            total_basis_cents: Cents(a.basis),
            unrealized_pretax_cents: if a.degraded {
                None
            } else {
                Some(Cents(a.market_value - a.basis))
            },
            unrealized_pct_of_basis_ppm: pct_of_basis_ppm(
                if a.degraded { None } else { Some(a.market_value - a.basis) },
                a.basis,
            ),
            unrealized_net_of_tax_cents: if a.degraded { None } else { Some(Cents(a.net_of_tax)) },
            degraded: if a.degraded { Some(DegradeCause::NoMark) } else { None },
            // Negative-basis is decided from the FINAL aggregated platform basis,
            // mirroring the per-symbol total test — never latched from an
            // order-dependent running sum (which would false-positive when an
            // early negative lot is later outweighed by a positive one).
            // (REPORT-COMP-004)
            negative_basis: a.basis < 0,
        })
        .collect();

    // --- Degraded count & priced-coverage fraction (basis survives degradation). ---
    let degraded_count = sym_facts.iter().filter(|f| f.degraded.is_some()).count();
    let total_basis: i64 = sym_facts.iter().map(|f| f.basis).sum();
    let priced_basis: i64 = sym_facts
        .iter()
        .filter(|f| f.degraded.is_none())
        .map(|f| f.basis)
        .sum();
    // Coverage = priced basis ÷ total basis, only when total basis is strictly
    // positive (a non-positive total basis — e.g. a negative-basis synthetic — has
    // no meaningful coverage fraction; it is flagged via `negative_basis`).
    let priced_coverage_ppm = if total_basis <= 0 {
        None
    } else {
        Some(config::Ppm(pt_core::round_half_to_even(
            (priced_basis as i128) * 1_000_000,
            total_basis as i128,
        ) as i64))
    };

    Composition {
        by_symbol,
        by_platform,
        degraded_count,
        priced_coverage_ppm,
        priced_total_state,
    }
}

/// Pre-tax unrealized as a fraction of basis in `ppm` (rounded half-to-even —
/// the same rounding discipline as the priced-coverage fraction). `None` when
/// the unrealized figure is degraded (`None`) or `basis ≤ 0`: a closed-out or
/// zero-cost position has no meaningful %-of-basis, and a fabricated or
/// nonsensical percentage is never emitted. (REPORT-COMP-006)
// @spec REPORT-COMP-006
fn pct_of_basis_ppm(unrealized_pretax: Option<i64>, basis: i64) -> Option<config::Ppm> {
    let u = unrealized_pretax?;
    if basis <= 0 {
        return None;
    }
    Some(config::Ppm(pt_core::round_half_to_even(
        (u as i128) * 1_000_000,
        basis as i128,
    ) as i64))
}

/// Largest-remainder allocation of `1_000_000` ppm across the priced rows so the
/// priced shares sum to **exactly** 100% (`1_000_000` ppm) when `total > 0`. A
/// degraded row (its value `None`) gets `None`; when `total ≤ 0` every share is
/// `None` (shares are n/a — REPORT-COMP-004). The residual ppm from flooring is
/// awarded to the largest fractional remainders (FIFO on index for ties).
fn shares_ppm(values: &[Option<i64>], total: i64) -> Vec<Option<config::Ppm>> {
    if total <= 0 {
        return values.iter().map(|_| None).collect();
    }
    // Floor each priced row's exact ppm share; track remainders for the residual.
    let mut floors: Vec<i64> = Vec::with_capacity(values.len());
    let mut rems: Vec<i128> = Vec::with_capacity(values.len());
    let mut priced_idx: Vec<usize> = Vec::new();
    let mut sum_floor: i64 = 0;
    for (i, v) in values.iter().enumerate() {
        match v {
            Some(mv) => {
                let num = (*mv as i128) * 1_000_000;
                let f = num.div_euclid(total as i128) as i64;
                let r = num.rem_euclid(total as i128);
                floors.push(f);
                rems.push(r);
                sum_floor += f;
                priced_idx.push(i);
            }
            None => {
                floors.push(0);
                rems.push(-1); // sentinel: never awarded a residual unit
            }
        }
    }
    let residual = (1_000_000 - sum_floor).max(0) as usize;
    // Rank priced rows by descending remainder, ascending index for ties.
    let mut order = priced_idx.clone();
    order.sort_by(|&a, &b| rems[b].cmp(&rems[a]).then(a.cmp(&b)));
    let mut awarded = floors;
    for &i in order.iter().take(residual) {
        awarded[i] += 1;
    }
    values
        .iter()
        .enumerate()
        .map(|(i, v)| v.map(|_| config::Ppm(awarded[i])))
        .collect()
}

/// Distribute an integer `total` across rows in proportion to non-negative
/// `weights`, returning per-row integer shares that sum to **exactly** `total`
/// (largest-remainder, FIFO on index for ties) — the same discipline as
/// [`shares_ppm`], used to split a symbol's exact estimated tax across its lots so
/// the per-platform/value-series net-of-tax reconciles to the per-symbol exact
/// figure (no rounded-rate re-derivation). When every weight is zero (no positive
/// pretax to tax), every share is `0` and the leftover `total` is dropped — a
/// non-positive aggregate pretax carries no positive tax to distribute. (REPORT-COMP-001)
fn distribute_largest_remainder(total: i64, weights: &[i64]) -> Vec<i64> {
    let sum_w: i128 = weights.iter().map(|w| (*w).max(0) as i128).sum();
    if sum_w <= 0 || weights.is_empty() {
        return vec![0; weights.len()];
    }
    let total128 = total as i128;
    let mut floors: Vec<i64> = Vec::with_capacity(weights.len());
    let mut rems: Vec<i128> = Vec::with_capacity(weights.len());
    let mut sum_floor: i128 = 0;
    for w in weights {
        let num = total128 * (*w).max(0) as i128;
        let f = num.div_euclid(sum_w);
        let r = num.rem_euclid(sum_w);
        floors.push(f as i64);
        rems.push(r);
        sum_floor += f;
    }
    // Award the residual units (total − Σfloor) to the largest remainders, ties by
    // ascending index, so the shares sum to exactly `total`.
    let residual = (total128 - sum_floor).max(0) as usize;
    let mut order: Vec<usize> = (0..weights.len()).collect();
    order.sort_by(|&a, &b| rems[b].cmp(&rems[a]).then(a.cmp(&b)));
    for &i in order.iter().take(residual) {
        floors[i] += 1;
    }
    floors
}

// ===========================================================================
// Value-Over-Time: the History series (reports-design.md → "Value-Over-Time" /
// "Snapshot Capture & Persistence"; REPORT-VOT-*, REPORT-HIST-*). A series point
// records, for one trading day, total market value, unrealized (pre/post-tax),
// per-symbol VALUES, total basis, the marks used (with quote-epoch), the capture
// timestamp + reporting-TZ date as metadata, and an incomplete flag.
// ===========================================================================

/// One captured value-series point for a single **trading day**. Keyed by
/// `key` (the trading-day key — REPORT-VOT-001). Per-symbol **values** are the
/// cross-time axis (split-neutral; share counts are metadata, never diffed —
/// REPORT-VOT-002). `incomplete` flags a capture with any degraded symbol
/// (its total is partial — REPORT-VOT-004). (REPORT-VOT-001/002/004)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SeriesPoint {
    /// The trading-day key (the series key — the marks' quote-epoch trading day).
    /// (REPORT-VOT-001)
    pub key: TradingDayKey,
    /// Total market value across **priced** symbols (partial when `incomplete`).
    pub total_market_value_cents: Cents,
    /// Total pre-tax unrealized P&L across priced symbols.
    pub total_unrealized_pretax_cents: Cents,
    /// Total net-of-tax unrealized estimate across priced symbols.
    pub total_unrealized_net_of_tax_cents: Cents,
    /// Total cost basis (reconstructable; recorded for the point's completeness).
    pub total_basis_cents: Cents,
    /// Per-symbol **value** (the split-neutral cross-time axis). (REPORT-VOT-002)
    pub per_symbol_value_cents: BTreeMap<Symbol, Cents>,
    /// Per-symbol share counts AT CAPTURE — point-in-time metadata only, **never
    /// diffed across time** (a split rescales them). (REPORT-VOT-002)
    pub per_symbol_shares: BTreeMap<Symbol, MicroShares>,
    /// The marks used for this capture (symbol → priced mark, with quote-epoch).
    pub marks: PricedMarks,
    /// The wall-clock capture timestamp (epoch seconds) — metadata, NOT the key.
    /// (reports-design.md → "Value-Over-Time")
    pub captured_at_epoch_secs: i64,
    /// The reporting-TZ calendar date of the capture — metadata, NOT the key.
    /// (reports-design.md → "Value-Over-Time"; from `config`'s reporting TZ.)
    pub reporting_tz_date: Date,
    /// `true` when any symbol was degraded this capture: the point is incomplete
    /// and its total is partial. (REPORT-VOT-004)
    pub incomplete: bool,
}

/// Build the `SeriesPoint` for a capture from the `Snapshot`, the current `marks`,
/// `tax`'s unrealized `estimates`, the runtime-supplied trading-day `key`, the
/// capture timestamp (`captured_at_epoch_secs`), and the reporting-TZ calendar
/// date (`reporting_tz_date`, derived by `runtime` from `config`'s reporting TZ).
///
/// The point is keyed by `key` (the trading-day key, NOT the run's calendar day —
/// REPORT-VOT-001); per-symbol **values** are recorded as the cross-time axis with
/// share counts only as metadata (REPORT-VOT-002); a capture with any degraded
/// symbol is flagged `incomplete` with a partial total (REPORT-VOT-004). The
/// timestamp + reporting-TZ date are metadata, never the key. (REPORT-VOT-001/002/004)
pub fn build_series_point(
    snapshot: &Snapshot,
    marks: &PricedMarks,
    estimates: &BTreeMap<Symbol, UnrealizedEstimate>,
    key: TradingDayKey,
    captured_at_epoch_secs: i64,
    reporting_tz_date: Date,
) -> SeriesPoint {
    let mut total_mv: i64 = 0;
    let mut total_pretax: i64 = 0;
    let mut total_net: i64 = 0;
    let mut total_basis: i64 = 0;
    let mut per_symbol_value: BTreeMap<Symbol, Cents> = BTreeMap::new();
    let mut per_symbol_shares: BTreeMap<Symbol, MicroShares> = BTreeMap::new();
    let mut incomplete = false;

    for (symbol, pos) in &snapshot.positions {
        if pos.total_qty.0 == 0 {
            continue;
        }
        // Share counts at capture are point-in-time metadata only (never diffed
        // across time — a split rescales them). (REPORT-VOT-002)
        per_symbol_shares.insert(symbol.clone(), pos.total_qty);
        total_basis += pos.total_basis_cents.0;

        match marks.get(symbol) {
            // No mark this capture → degraded: do NOT fabricate a value; flag the
            // point incomplete and leave its total partial. (REPORT-VOT-004)
            None => {
                incomplete = true;
            }
            Some(mark) => {
                // Per-symbol VALUE = scale(mark × total_qty) (the cross-time axis,
                // split-neutral). (REPORT-VOT-002)
                let mv = pt_core::scale(
                    (mark.price_cents.0 as i128) * (pos.total_qty.0 as i128),
                ) as i64;
                per_symbol_value.insert(symbol.clone(), Cents(mv));
                total_mv += mv;
                let pretax = mv - pos.total_basis_cents.0;
                total_pretax += pretax;
                // Net-of-tax uses the SAME definition as per-symbol composition:
                // the symbol's EXACT estimated tax (`estimated_tax_cents`), not a
                // re-derivation from the rounded effective rate (which double-rounds
                // and would not reconcile to composition's per-symbol net). So the
                // series net-of-tax total = Σ(pretax − estimated_tax) over priced
                // symbols, exactly matching the per-symbol composition figures.
                // (REPORT-COMP-001) A missing estimate flags the point incomplete
                // (the net-of-tax total is partial) but the value/pretax totals —
                // which depend on the mark, not the estimate — still count.
                match estimates.get(symbol).and_then(|e| e.estimated_tax_cents) {
                    Some(tax) => {
                        total_net += pretax - tax.0;
                    }
                    None => {
                        incomplete = true;
                        total_net += pretax; // no tax estimate → net == pretax (partial)
                    }
                }
            }
        }
    }

    SeriesPoint {
        key,
        total_market_value_cents: Cents(total_mv),
        total_unrealized_pretax_cents: Cents(total_pretax),
        total_unrealized_net_of_tax_cents: Cents(total_net),
        total_basis_cents: Cents(total_basis),
        per_symbol_value_cents: per_symbol_value,
        per_symbol_shares,
        marks: marks.clone(),
        captured_at_epoch_secs,
        reporting_tz_date,
        incomplete,
    }
}

/// One element of a value series rendered for a consumer: either a captured
/// point, or an **explicit gap** for a trading day with no capture (no
/// interpolation). (REPORT-VOT-003)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SeriesElement {
    /// A captured trading-day point.
    Point(SeriesPoint),
    /// An explicit gap: a trading day in the requested span with no captured
    /// point. Never interpolated — a flat segment must never imply a real flat
    /// value. (REPORT-VOT-003)
    Gap { key: TradingDayKey },
}

/// The full value series for consumers: the captured points in trading-day key
/// order, with **explicit gaps** for trading days in the requested span that have
/// no capture. (REPORT-VOT-003)
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ValueSeries {
    /// Points + explicit gaps, ascending by trading-day key.
    pub elements: Vec<SeriesElement>,
}

/// A day-over-day (point-to-point) delta over the value series. When **either**
/// endpoint is an incomplete point, the delta is **flagged** (not silently shown)
/// — consumers (`summary`) must honor the flag. (REPORT-VOT-004)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SeriesDelta {
    /// The earlier point's key.
    pub from: TradingDayKey,
    /// The later point's key.
    pub to: TradingDayKey,
    /// The total-market-value change across the two points.
    pub delta_market_value_cents: Cents,
    /// `true` when either endpoint was an incomplete capture: the delta is
    /// suspect and must be flagged/suppressed by consumers, never silently shown.
    /// (REPORT-VOT-004)
    pub flagged_incomplete: bool,
}

/// Render the captured `points` into a [`ValueSeries`] for a consumer, inserting
/// an **explicit gap** for every trading-day key in `trading_days` that has no
/// captured point — no interpolation (REPORT-VOT-003). `trading_days` is the span
/// of trading-day keys the consumer wants rendered (runtime supplies the trading
/// calendar). The series begins at the first capture and fabricates no pre-capture
/// values (REPORT-VOT-005). (REPORT-VOT-003/005)
pub fn render_series(points: &[SeriesPoint], trading_days: &[TradingDayKey]) -> ValueSeries {
    // Resolve captures to one point per trading-day key (last-wins): a later
    // capture sharing a key overwrites the earlier one, so the series advances only
    // when the trading-day key advances. (REPORT-VOT-001)
    let mut by_key: BTreeMap<TradingDayKey, SeriesPoint> = BTreeMap::new();
    for p in points {
        by_key.insert(p.key, p.clone()); // input order is capture order → last wins
    }

    // Walk the requested trading-day span ascending, emitting a point where one
    // was captured and an EXPLICIT gap (no interpolation) where none was — so a
    // flat segment never implies a real flat value, and pre-first-capture days are
    // gaps, not fabricated values. (REPORT-VOT-003/005)
    let mut days: Vec<TradingDayKey> = trading_days.to_vec();
    days.sort();
    days.dedup();
    let elements = days
        .into_iter()
        .map(|k| match by_key.get(&k) {
            Some(p) => SeriesElement::Point(p.clone()),
            None => SeriesElement::Gap { key: k },
        })
        .collect();
    ValueSeries { elements }
}

/// Compute the delta between two consecutive captured points (`earlier` → `later`)
/// on the value series. The delta is **flagged incomplete** when either endpoint
/// was an incomplete capture, so a consumer never silently shows a delta computed
/// across an incomplete point. (REPORT-VOT-004)
pub fn series_delta(earlier: &SeriesPoint, later: &SeriesPoint) -> SeriesDelta {
    SeriesDelta {
        from: earlier.key,
        to: later.key,
        delta_market_value_cents: Cents(
            later.total_market_value_cents.0 - earlier.total_market_value_cents.0,
        ),
        // A delta computed across an incomplete point is suspect: flag it so a
        // consumer (`summary`) suppresses/marks it, never silently shown.
        // (REPORT-VOT-004)
        flagged_incomplete: earlier.incomplete || later.incomplete,
    }
}

/// One symbol's value change across two value-series points. The cross-time axis
/// is the per-symbol **value** (`per_symbol_value_cents`), which is split-neutral;
/// share counts are point-in-time metadata and are **never diffed across time**
/// (a split rescales them), so they are not consulted here. (REPORT-VOT-002)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PerSymbolValueDelta {
    /// The symbol whose value changed.
    pub symbol: Symbol,
    /// The earlier point's value for this symbol (`None` if it had no priced value
    /// then — degraded that capture, never fabricated).
    pub from_value_cents: Option<Cents>,
    /// The later point's value for this symbol (`None` if it had no priced value
    /// then).
    pub to_value_cents: Option<Cents>,
    /// The value change `later − earlier`; `None` when either endpoint lacked a
    /// priced value (degraded), so a missing mark never fabricates a delta.
    pub delta_value_cents: Option<Cents>,
    /// The percent change relative to the **earlier** value, in `ppm` (rounded
    /// half-to-even). `None` when either endpoint lacks a priced value or the
    /// earlier value is ≤ 0 (no meaningful base) — never a fabricated
    /// percentage. (REPORT-VOT-006)
    pub delta_pct_ppm: Option<config::Ppm>,
}

/// Compare **per-symbol value** across two value-series points (`earlier` →
/// `later`) — the split-neutral cross-time axis (REPORT-VOT-002). For every symbol
/// present in either point, the delta is `later.value − earlier.value`; when either
/// endpoint lacked a priced value for that symbol (degraded that capture) the delta
/// is `None` (never fabricated). Share counts are **never** consulted — a split
/// rescales them, so diffing them would mislead. Returned in symbol order.
/// Each delta also carries the percent change relative to the earlier value
/// (`ppm`, half-to-even), `None` when either endpoint is missing or the earlier
/// value is ≤ 0. (REPORT-VOT-002, REPORT-VOT-006)
// @spec REPORT-VOT-006
pub fn per_symbol_value_delta(
    earlier: &SeriesPoint,
    later: &SeriesPoint,
) -> Vec<PerSymbolValueDelta> {
    let symbols: std::collections::BTreeSet<&Symbol> = earlier
        .per_symbol_value_cents
        .keys()
        .chain(later.per_symbol_value_cents.keys())
        .collect();
    symbols
        .into_iter()
        .map(|s| {
            let from = earlier.per_symbol_value_cents.get(s).copied();
            let to = later.per_symbol_value_cents.get(s).copied();
            let delta = match (from, to) {
                (Some(a), Some(b)) => Some(Cents(b.0 - a.0)),
                _ => None,
            };
            // The percent change needs a meaningful positive base; an earlier
            // value of ≤ 0 (or a degraded endpoint) yields None, never a
            // fabricated percentage. (REPORT-VOT-006)
            let delta_pct = match (from, delta) {
                (Some(f), Some(d)) if f.0 > 0 => Some(config::Ppm(
                    pt_core::round_half_to_even((d.0 as i128) * 1_000_000, f.0 as i128) as i64,
                )),
                _ => None,
            };
            PerSymbolValueDelta {
                symbol: s.clone(),
                from_value_cents: from,
                to_value_cents: to,
                delta_value_cents: delta,
                delta_pct_ppm: delta_pct,
            }
        })
        .collect()
}

// ===========================================================================
// History persistence (reports-design.md → "Snapshot Capture & Persistence";
// REPORT-HIST-*). The durable workbook History tab, written via runtime's locked
// Sheets-access primitive: atomic batchUpdate, read-back-verified, retry-and-flag
// on failure, last-wins by trading-day key. Integrity check on read; a cache
// currency-checked against the workbook (workbook wins, re-READ not recompute).
// ===========================================================================

/// A failure crossing the History I/O seam. A lost point is **non-reconstructable**
/// (its marks are gone), so a write that cannot be durably confirmed is a loud
/// error — never a silent drop. (REPORT-HIST-001)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum HistoryError {
    /// The History tab was unreachable / the `batchUpdate` failed, or a read-back
    /// found the written point absent or unequal: control returns to the owner to
    /// retry (a lost point is permanent). (REPORT-HIST-001)
    WriteVerifyFailed,
    /// The workbook was unreachable on a read. (REPORT-HIST-004)
    Unreachable,
    /// On read, the trading-day keys are not unique-and-non-decreasing (a
    /// duplicate or an out-of-order key): refuse to fold a corrupt, editable,
    /// non-reconstructable series — flag loudly. (REPORT-HIST-003)
    NonMonotonicKeys,
    /// On read, a History row could not be parsed into a `SeriesPoint`: refuse
    /// rather than fold a corrupt row. (REPORT-HIST-003)
    UnparseableRow,
    /// On read, the content checksum did not match: a silent out-of-band edit was
    /// detected; flag loudly (recovery via Google Sheets version history).
    /// (REPORT-HIST-003)
    ChecksumMismatch,
}

impl std::fmt::Display for HistoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for HistoryError {}

/// One durable History row as it sits in the workbook tab: the trading-day key,
/// a parsed `SeriesPoint`, and the per-row content checksum cell. `reports` owns
/// this schema; the in-memory fake / real `runtime` client read and write it.
/// The checksum is over the row's content so an out-of-band edit is detectable
/// on read (REPORT-HIST-003).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HistoryRow {
    /// The trading-day key (the row's unique, non-decreasing key). (REPORT-HIST-003)
    pub key: TradingDayKey,
    /// The captured point.
    pub point: SeriesPoint,
    /// The content checksum of `point`, used to detect out-of-band edits on read.
    /// (REPORT-HIST-003)
    pub checksum: u64,
}

/// The low-level History-tab access layer `reports` needs, over the durable
/// workbook History tab. `runtime` implements this against the Sheets API (its
/// **locked** Sheets-access primitive); tests use the in-memory fake
/// [`testkit::InMemoryHistory`]. Modeled on `store`'s `SheetsClient`, but over the
/// History tab and the last-wins-by-trading-day batchUpdate `reports` needs.
pub trait HistoryClient {
    /// Read every History row, in tab order. The integrity check
    /// ([`read_history`]) runs over the result. (REPORT-HIST-003/004)
    fn read_history(&self) -> Result<Vec<HistoryRow>, HistoryError>;

    /// Atomically upsert one trading-day point as a single `batchUpdate`,
    /// **last-wins by trading-day key**: a point for an existing key **overwrites**
    /// that row, a new key appends. Returns `Err(WriteVerifyFailed)` if the
    /// underlying call fails — leaving the tab as-is. The real client read-back-
    /// verifies and retries; the contract is the upsert. (REPORT-HIST-001/002)
    fn upsert_point(&mut self, row: &HistoryRow) -> Result<(), HistoryError>;
}

/// The content checksum of a `SeriesPoint` — a fixed, platform-stable FNV-1a over
/// the point's canonical projection, so an out-of-band edit to a persisted row is
/// detectable on read (REPORT-HIST-003). Mirrors `store`'s stable-hash discipline
/// (NOT `DefaultHasher`, whose output is not guaranteed stable across releases).
pub fn point_checksum(point: &SeriesPoint) -> u64 {
    // Canonical, order-stable byte projection of the point's content. Every field
    // that defines the captured value is folded so an out-of-band edit to any of
    // them shifts the checksum. Field separators (unit separator 0x1f) keep the
    // projection unambiguous, mirroring `store`'s content-hash discipline.
    let mut bytes: Vec<u8> = Vec::new();
    let push_i = |bytes: &mut Vec<u8>, v: i64| {
        bytes.extend_from_slice(&v.to_le_bytes());
        bytes.push(0x1f);
    };
    push_i(&mut bytes, point.key.0 .0 as i64);
    push_i(&mut bytes, point.total_market_value_cents.0);
    push_i(&mut bytes, point.total_unrealized_pretax_cents.0);
    push_i(&mut bytes, point.total_unrealized_net_of_tax_cents.0);
    push_i(&mut bytes, point.total_basis_cents.0);
    push_i(&mut bytes, point.captured_at_epoch_secs);
    push_i(&mut bytes, point.reporting_tz_date.0 as i64);
    bytes.push(if point.incomplete { 1 } else { 0 });
    bytes.push(0x1f);
    // Per-symbol values (the cross-time axis) — BTreeMap iterates in key order, so
    // the projection is deterministic.
    for (sym, v) in &point.per_symbol_value_cents {
        bytes.extend_from_slice(sym.as_bytes());
        bytes.push(0x1e); // record separator between symbol and value
        push_i(&mut bytes, v.0);
    }
    bytes.push(0x1d); // group separator: end of values, start of shares
    for (sym, q) in &point.per_symbol_shares {
        bytes.extend_from_slice(sym.as_bytes());
        bytes.push(0x1e);
        push_i(&mut bytes, q.0);
    }
    fnv1a_64(&bytes)
}

/// FNV-1a 64-bit — a fixed, documented, platform-stable hash (mirrors `store`'s
/// tax-id hash). Used for the History row content checksum so an out-of-band edit
/// is detectable across toolchains. NOT cryptographic; determinism + low collision
/// rate are all that is needed. (REPORT-HIST-003)
fn fnv1a_64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Append (capture) one trading-day point to the durable History tab via the
/// runtime-supplied locked Sheets-access primitive (`lock`) and `client`. The path
/// mirrors `store`'s append discipline (a lost point is non-reconstructable):
///
/// 1. Acquire the advisory write-lock (INSIDE this primitive), so a TUI-triggered
///    and a cron capture cannot race. (REPORT-HIST-001)
/// 2. Upsert the point as a single atomic `batchUpdate`, **last-wins by
///    trading-day key** (an existing trading-day point is overwritten, a re-run
///    never duplicates). (REPORT-HIST-001/002)
/// 3. Read back and verify the point landed equal; on failure return control to
///    the owner to retry-and-flag (never a silent loss). (REPORT-HIST-001)
///
/// The checksum is computed and stored so the read-side integrity check can detect
/// an out-of-band edit later. (REPORT-HIST-001/002/003)
pub fn append_snapshot<C: HistoryClient, L: Lock>(
    client: &mut C,
    lock: &L,
    point: &SeriesPoint,
) -> Result<(), HistoryError> {
    // 1. Acquire the advisory write-lock INSIDE this primitive, so a TUI-triggered
    //    and a cron capture cannot race. (REPORT-HIST-001) The lock is store's;
    //    its acquire error maps to a loud write failure (a lost point is permanent).
    let _guard = lock
        .acquire()
        .map_err(|_| HistoryError::WriteVerifyFailed)?;

    // 2. Build the durable row (content checksum stored for the read-side
    //    integrity check), then upsert it as a single atomic batchUpdate,
    //    last-wins by trading-day key. (REPORT-HIST-001/002/003)
    let row = HistoryRow {
        key: point.key,
        point: point.clone(),
        checksum: point_checksum(point),
    };
    client.upsert_point(&row)?;

    // 3. Read back and verify the point landed equal; a missing/unequal row means
    //    the write could not be durably confirmed — return control to the owner to
    //    retry-and-flag (never a silent loss of a non-reconstructable point).
    //    (REPORT-HIST-001)
    let back = client.read_history()?;
    match back.iter().find(|r| r.key == point.key) {
        Some(r) if r.point == *point && r.checksum == row.checksum => Ok(()),
        _ => Err(HistoryError::WriteVerifyFailed),
    }
}

/// Read the durable History series with the on-read integrity check: trading-day
/// keys **unique and non-decreasing**, every row **parses**, and each row's stored
/// **content checksum** matches its recomputed point checksum. On any failure this
/// flags **loudly** (a distinct `HistoryError`) rather than folding a corrupt,
/// non-reconstructable, human-editable series — recovery is via Google Sheets
/// version history. On success the points are returned in trading-day key order.
/// (REPORT-HIST-003)
pub fn read_history<C: HistoryClient>(client: &C) -> Result<Vec<SeriesPoint>, HistoryError> {
    // The client deserializes each stored History row into a typed `HistoryRow`
    // (key + `SeriesPoint` + checksum); a row that cannot be parsed surfaces here as
    // `Err(HistoryError::UnparseableRow)`, which this integrity check propagates
    // loudly rather than folding a corrupt, non-reconstructable series. The parse
    // is owned at the trait boundary (the in-memory fake injects it via
    // `set_read_unparseable`; the real Sheets-backed client parses real cells), and
    // the remaining two integrity sub-requirements — unique/non-decreasing keys and
    // the content checksum — are checked below. (REPORT-HIST-003)
    let rows = client.read_history()?;

    // Trading-day keys must be unique AND non-decreasing as stored (tab order is
    // the series order — a duplicate or an out-of-order key is corruption on a
    // human-editable, non-reconstructable tab). Flag loudly rather than fold it.
    // (REPORT-HIST-003)
    for pair in rows.windows(2) {
        if pair[1].key <= pair[0].key {
            return Err(HistoryError::NonMonotonicKeys);
        }
    }

    // Every row's stored content checksum must match its recomputed point checksum
    // — catching a silent out-of-band edit to a captured (irreplaceable) value.
    // (REPORT-HIST-003)
    for r in &rows {
        if r.checksum != point_checksum(&r.point) {
            return Err(HistoryError::ChecksumMismatch);
        }
    }

    Ok(rows.into_iter().map(|r| r.point).collect())
}

/// Read the value series from the local cache **currency-checked against the
/// workbook**: the workbook (authoritative) is read and compared against the
/// `cache`d points; on **any divergence** the workbook wins and the returned
/// series is the workbook's — the cache is **re-read (re-downloaded), not
/// recomputed** (the point that distinguishes History from `store`'s rebuildable
/// cache). The boolean is `true` when the cache diverged (and was re-read).
/// (REPORT-HIST-004)
pub fn read_series_cache_checked<C: HistoryClient>(
    client: &C,
    cache: &[SeriesPoint],
) -> Result<(Vec<SeriesPoint>, bool), HistoryError> {
    // Read the AUTHORITATIVE workbook series (integrity-checked). The workbook
    // wins; the cache is never trusted over it. (REPORT-HIST-004)
    let workbook = read_history(client)?;
    // Currency check: does the cache match the workbook exactly? On ANY divergence
    // the workbook wins and the returned series is the workbook's — the cache is
    // re-READ (re-downloaded), NOT recomputed (the point that distinguishes History
    // from `store`'s rebuildable cache). (REPORT-HIST-004)
    let diverged = cache != workbook.as_slice();
    Ok((workbook, diverged))
}

// ===========================================================================
// Realized-P&L history (reports-design.md → "Realized-P&L History";
// REPORT-REAL-001). Computed from the event log's realized gains
// (reconstructable), grouped by CALENDAR year or an arbitrary date range —
// explicitly NOT a competing "quarterly" view (that is tax's). Labels say
// "calendar" to avoid confusion with tax's IRS estimated periods.
// ===========================================================================

/// The grouping for the realized-P&L history: a full **calendar year**, or an
/// arbitrary inclusive date range. Both are **calendar**-based — distinct from
/// `tax`'s IRS estimated-period quarterly report. (REPORT-REAL-001)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CalendarPeriod {
    /// A full calendar year (Jan 1 – Dec 31).
    Year(i32),
    /// An arbitrary inclusive `[start, end]` date range (days since epoch).
    Range { start: Date, end: Date },
}

/// One realized-P&L history row: a **calendar** period and its totals
/// (proceeds, basis, gain), computed from the event log's realized gains. Labelled
/// "calendar" so it is never confused with `tax`'s estimated-period quarterly
/// figures. (REPORT-REAL-001)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RealizedHistoryRow {
    /// The calendar period this row covers. (REPORT-REAL-001)
    pub period: CalendarPeriod,
    /// A human label, e.g. `"calendar 2025"` — explicitly "calendar". (REPORT-REAL-001)
    pub label: String,
    /// Σ proceeds over the realized gains whose `sale_date` falls in the period.
    pub proceeds_cents: Cents,
    /// Σ basis over those gains.
    pub basis_cents: Cents,
    /// Σ gain (`proceeds − basis`) over those gains; may be negative.
    pub gain_cents: Cents,
}

/// Compute realized-P&L history **from the event log's realized gains** (the
/// `Snapshot`'s `realized_gains`, which are reconstructable), grouped by **calendar
/// year**: one [`RealizedHistoryRow`] per distinct calendar year of a sale,
/// ascending, each labelled "calendar &lt;year&gt;". Distinct from `tax`'s
/// estimated-period quarterly report. (REPORT-REAL-001)
pub fn realized_history_by_year(snapshot: &Snapshot) -> Vec<RealizedHistoryRow> {
    // Group the reconstructable realized gains by the CALENDAR year of their
    // sale_date (BTreeMap → ascending year order). (REPORT-REAL-001)
    let mut by_year: BTreeMap<i32, (i64, i64, i64)> = BTreeMap::new();
    for g in &snapshot.realized_gains {
        let year = calendar_year_of(g.sale_date);
        let entry = by_year.entry(year).or_insert((0, 0, 0));
        entry.0 += g.proceeds_cents.0;
        entry.1 += g.basis_cents.0;
        entry.2 += g.gain_cents.0;
    }
    by_year
        .into_iter()
        .map(|(year, (proceeds, basis, gain))| RealizedHistoryRow {
            period: CalendarPeriod::Year(year),
            label: format!("calendar {year}"),
            proceeds_cents: Cents(proceeds),
            basis_cents: Cents(basis),
            gain_cents: Cents(gain),
        })
        .collect()
}

/// Compute the **calendar-year-to-date** realized P&L for `year` from the event
/// log's realized gains: a single [`RealizedHistoryRow`] summing proceeds, basis,
/// and gain over the sales whose `sale_date` falls in that calendar year. A year
/// with no realized sales returns an **exact zero row** — realized history is
/// reconstructable, so zero is a true figure, not a degraded state, and a
/// consumer (the TUI's summary band) never fabricates the empty-year row itself.
/// Calendar-labelled; distinct from `tax`'s quarterly report. (REPORT-REAL-002)
// @spec REPORT-REAL-002
pub fn realized_ytd(snapshot: &Snapshot, year: i32) -> RealizedHistoryRow {
    let mut proceeds = 0i64;
    let mut basis = 0i64;
    let mut gain = 0i64;
    for g in &snapshot.realized_gains {
        if calendar_year_of(g.sale_date) == year {
            proceeds += g.proceeds_cents.0;
            basis += g.basis_cents.0;
            gain += g.gain_cents.0;
        }
    }
    RealizedHistoryRow {
        period: CalendarPeriod::Year(year),
        label: format!("calendar {year} YTD"),
        proceeds_cents: Cents(proceeds),
        basis_cents: Cents(basis),
        gain_cents: Cents(gain),
    }
}

/// Compute realized-P&L history for an **arbitrary calendar date range**
/// `[start, end]` (inclusive): a single [`RealizedHistoryRow`] summing every
/// realized gain whose `sale_date` falls in the range, labelled "calendar
/// &lt;start&gt;..&lt;end&gt;". Calendar-based; distinct from `tax`'s quarterly
/// report. (REPORT-REAL-001)
pub fn realized_history_for_range(snapshot: &Snapshot, start: Date, end: Date) -> RealizedHistoryRow {
    // Sum every realized gain whose sale_date falls in the inclusive calendar range
    // [start, end]. Calendar-based; distinct from tax's quarterly report.
    // (REPORT-REAL-001)
    let mut proceeds = 0i64;
    let mut basis = 0i64;
    let mut gain = 0i64;
    for g in &snapshot.realized_gains {
        if g.sale_date.0 >= start.0 && g.sale_date.0 <= end.0 {
            proceeds += g.proceeds_cents.0;
            basis += g.basis_cents.0;
            gain += g.gain_cents.0;
        }
    }
    RealizedHistoryRow {
        period: CalendarPeriod::Range { start, end },
        label: format!("calendar {}..{}", start.0, end.0),
        proceeds_cents: Cents(proceeds),
        basis_cents: Cents(basis),
        gain_cents: Cents(gain),
    }
}

/// The calendar year of a `Date` (days since the Unix epoch). Reuses `tax`'s
/// calendar-exact `tax_year_of` so `reports`' calendar grouping and `tax`'s
/// tax-year share one date algorithm. (REPORT-REAL-001)
pub fn calendar_year_of(date: Date) -> i32 {
    tax::tax_year_of(date).0
}
