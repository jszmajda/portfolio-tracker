//! The replay -> project -> read-marks -> cache cycle (RUNTIME-CYCLE).
//!
//! ```text
//!  load log (store)  ->  replay ledger-core, then tax  ->  hold Snapshot + accruals
//!         ^                                                        |
//!         |                                                        v
//!    inject cached marks  <-- cache marks (+ quote-epochs) <-- read marks back (sheets-view)
//! ```
//!
//! `runtime` is the **replay caller** (RUNTIME-CYCLE-001): [`load_and_run_cycle`]
//! loads the event log via `store` (runtime owns the load step), then replays
//! `ledger-core` then `tax` (with `config` and the **prior cycle's cached marks**),
//! and holds the resulting `Snapshot`, accruals, and tax estimates that
//! `tui`/`summary`/`reports` render. ([`run_cycle`] / [`replay_with_cached_marks`]
//! take a pre-loaded [`store::EventLogs`] for a caller that already holds it — e.g.
//! a read-only `summary` replaying from cache.)
//!
//! It is the **marks-cache owner-of-record** (RUNTIME-CYCLE-002): `sheets-view`
//! reads `GOOGLEFINANCE` prices back and produces the marks; `runtime` caches them
//! and **injects them into the next replay**, resolving "marks are produced after
//! the replay that needs them." A cycle takes a [`MarksCache`] in (the prior
//! cycle's), and returns the new one out (this cycle's read-back), so the caller
//! threads the cache between cycles.
//!
//! It owns the **quote-epoch reduction** (RUNTIME-CYCLE-003): `sheets-view` stamps
//! each mark with its per-symbol `GOOGLEFINANCE` quote date; `runtime` reduces them
//! to the ONE trading-day key (the most-recent quote-epoch across priced symbols)
//! that `reports` (History), `summary`, and `tui` all assume — while preserving
//! each symbol's own stamp and degraded flag for per-symbol freshness.

use std::collections::BTreeMap;

use ledger_core::{Marks, Snapshot, Symbol};
use pt_core::{Cents, Date};
use reports::{PricedMark, PricedMarks, TradingDayKey};
use sheets_view::{
    read_marks, render_open_lots, render_positions, render_realized, render_tax, DegradeReason,
    Mark, MarksProduced, Publisher, RepublishOutcome, SettleConfig, SheetsViewClient, ViewError,
};
use store::{Cache, EventLogs, Lock, SheetsClient, Store, StoreError};
use tax::{Accrual, AnnualRow, TaxContext, UnrealizedEstimate};

use crate::history::{capture_cycle, CaptureOutcome};
use crate::sheets::BackoffPolicy;
use reports::HistoryClient;

// ===========================================================================
// The marks cache `runtime` owns between cycles (RUNTIME-CYCLE-002). The prior
// cycle's read-back marks (with their per-symbol quote-epoch) plus the degraded
// set; injected into the next replay's `ledger-core::Marks`.
// ===========================================================================

/// The marks `runtime` caches between cycles and injects into the next replay:
/// per-symbol good marks (price + quote-epoch) and the degraded set (with reason).
/// A degraded symbol is **absent** from `marks` (never a zero), so `ledger-core`
/// degrades it per-symbol. (RUNTIME-CYCLE-002/003)
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct MarksCache {
    /// Symbol -> the cached good mark (price in `Cents`, stamped with its
    /// `GOOGLEFINANCE` quote-epoch). (RUNTIME-CYCLE-002)
    pub marks: BTreeMap<Symbol, Mark>,
    /// Symbols recorded degraded (no mark) this cycle, with the reason. Preserved
    /// for per-symbol freshness display. (RUNTIME-CYCLE-003)
    pub degraded: BTreeMap<Symbol, DegradeReason>,
}

impl MarksCache {
    /// An empty cache (the first cycle, before any read-back).
    pub fn new() -> Self {
        MarksCache::default()
    }

    /// Build a cache from a `sheets-view` [`MarksProduced`] read-back. The good
    /// marks and the degraded set carry through verbatim. (RUNTIME-CYCLE-002)
    pub fn from_produced(produced: &MarksProduced) -> Self {
        MarksCache {
            marks: produced.marks.clone(),
            degraded: produced.degraded.clone(),
        }
    }

    /// The prior-cycle marks projected to `ledger_core::Marks` (symbol -> `Cents`)
    /// for **injection into the next replay** — a degraded symbol is absent (never
    /// a zero), so `ledger-core` degrades it per-symbol. (RUNTIME-CYCLE-002)
    pub fn to_ledger_marks(&self) -> Marks {
        self.marks
            .iter()
            .map(|(s, m)| (s.clone(), m.price_cents))
            .collect()
    }

    /// The prior-cycle good marks as the `prior` map `sheets-view::read_marks`
    /// carries forward (so a transiently-unknown symbol keeps its prior good mark
    /// rather than being nulled by a transient). (RUNTIME-CYCLE-002)
    pub fn prior_marks(&self) -> BTreeMap<Symbol, Mark> {
        self.marks.clone()
    }

    /// Project to `reports::PricedMarks` (symbol -> price + quote-epoch) for the
    /// composition / History capture. A degraded symbol is absent (degraded), never
    /// a zero. (RUNTIME-CYCLE-003)
    pub fn to_priced_marks(&self) -> PricedMarks {
        self.marks
            .iter()
            .map(|(s, m)| {
                (
                    s.clone(),
                    PricedMark {
                        price_cents: m.price_cents,
                        quote_epoch: m.quote_date,
                    },
                )
            })
            .collect()
    }
}

// ===========================================================================
// Quote-epoch reduction (RUNTIME-CYCLE-003). Reduce the per-symbol GOOGLEFINANCE
// quote dates to the ONE trading-day key (most-recent across priced symbols)
// reports/summary/tui assume, while preserving each symbol's own stamp.
// ===========================================================================

/// Reduce the per-symbol quote-epochs in a [`MarksCache`] to the ONE trading-day
/// key `reports`/`summary`/`tui` assume — **the most-recent quote-epoch across
/// priced symbols** — or `None` when no symbol is priced (every mark degraded /
/// the book is empty), so a degraded-only cycle keys nothing rather than
/// fabricating a calendar day. Each symbol's own stamp survives on the cache for
/// per-symbol freshness; this only computes the single series key.
/// (RUNTIME-CYCLE-003)
pub fn reduce_trading_day_key(cache: &MarksCache) -> Option<TradingDayKey> {
    cache
        .marks
        .values()
        .map(|m| m.quote_date)
        .max() // most-recent quote-epoch across priced symbols
        .map(TradingDayKey)
}

/// The per-symbol freshness view: each symbol's own quote-epoch stamp (priced) or
/// its degrade reason (degraded), preserved alongside the single reduced
/// trading-day key. `tui`/`summary` show this so a symbol stale relative to the
/// reduced key is still individually visible. (RUNTIME-CYCLE-003)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SymbolFreshness {
    /// Priced this cycle, stamped with its own `GOOGLEFINANCE` quote-epoch.
    Priced { quote_epoch: Date },
    /// Degraded (no mark) this cycle, with the reason.
    Degraded { reason: DegradeReason },
}

/// The per-symbol freshness map: every priced symbol's own stamp and every
/// degraded symbol's reason, so per-symbol freshness survives the reduction to the
/// single trading-day key. (RUNTIME-CYCLE-003)
pub fn per_symbol_freshness(cache: &MarksCache) -> BTreeMap<Symbol, SymbolFreshness> {
    let mut out: BTreeMap<Symbol, SymbolFreshness> = BTreeMap::new();
    for (s, m) in &cache.marks {
        out.insert(
            s.clone(),
            SymbolFreshness::Priced {
                quote_epoch: m.quote_date,
            },
        );
    }
    for (s, r) in &cache.degraded {
        out.insert(s.clone(), SymbolFreshness::Degraded { reason: *r });
    }
    out
}

// ===========================================================================
// The cycle (RUNTIME-CYCLE-001/002/003). `runtime` is the replay caller.
// ===========================================================================

/// What the cycle holds after replay-then-read-marks: the `Snapshot`, the tax
/// accruals + annual reserve rows + per-symbol unrealized estimates (what
/// `tui`/`summary`/`reports` render), the **new** marks cache read back this cycle
/// (to inject into the NEXT replay), and the reduced trading-day key.
/// (RUNTIME-CYCLE-001/002/003)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CycleOutcome {
    /// The replayed `Snapshot` (positions, open lots, realized gains), valued with
    /// the **prior** cycle's injected marks. (RUNTIME-CYCLE-001/002)
    pub snapshot: Snapshot,
    /// The tax accruals over the realized gains (lifecycle-folded). (RUNTIME-CYCLE-001)
    pub accruals: Vec<Accrual>,
    /// The per-`(jurisdiction, tax_year)` annual reserve rows (accrued / moved /
    /// paid / outstanding / shortfall). (RUNTIME-CYCLE-001)
    pub annual_rows: Vec<AnnualRow>,
    /// The per-symbol unrealized tax estimate (the effective-rate input the views
    /// render). (RUNTIME-CYCLE-001)
    pub estimates: BTreeMap<Symbol, UnrealizedEstimate>,
    /// The marks read back THIS cycle (to cache and inject into the NEXT replay).
    /// (RUNTIME-CYCLE-002)
    pub marks: MarksCache,
    /// The single trading-day key reduced from the per-symbol quote-epochs — the
    /// key `reports`/`summary`/`tui` use. `None` when no symbol is priced.
    /// (RUNTIME-CYCLE-003)
    pub trading_day_key: Option<TradingDayKey>,
}

/// Replay-only result (the first half of the cycle), so a caller that does not
/// read marks back this run (offline / read-only `summary`) still gets the
/// `Snapshot` + tax holdings valued with the injected prior marks.
/// (RUNTIME-CYCLE-001)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Replayed {
    pub snapshot: Snapshot,
    pub accruals: Vec<Accrual>,
    pub annual_rows: Vec<AnnualRow>,
    pub estimates: BTreeMap<Symbol, UnrealizedEstimate>,
}

/// Replay `ledger-core` then `tax` over the loaded `logs`, valuing positions with
/// the **prior cycle's cached marks** (`prior`) and the per-year `ctx`, holding the
/// `Snapshot`, accruals, annual reserve rows, and per-symbol unrealized estimates.
/// `runtime` is the replay caller; this is the producer-half of the cycle, BEFORE
/// this run's marks are read back. (RUNTIME-CYCLE-001/002)
///
/// `as_of` is the valuation/holding-classification date (the run's reporting day).
/// `accrues_to_state` selects the residency state for the unrealized estimate (an
/// unstamped position uses `ctx.residency_default`).
pub fn replay_with_cached_marks(
    logs: &EventLogs,
    prior: &MarksCache,
    ctx: &TaxContext,
    as_of: Date,
) -> Replayed {
    // Inject the prior cycle's cached marks into replay — a degraded symbol is
    // absent (never a zero), so `ledger-core` degrades it per-symbol.
    // (RUNTIME-CYCLE-002)
    let injected: Marks = prior.to_ledger_marks();
    let snapshot = ledger_core::replay(&logs.ledger, &injected);

    // Replay tax over the realized gains + the tax-event lifecycle log.
    // (RUNTIME-CYCLE-001)
    let accruals = tax::compute_accruals(&snapshot.realized_gains, &logs.tax, ctx);
    let annual_rows = tax::annual_report(&snapshot.realized_gains, &logs.tax, ctx);
    let estimates = unrealized_estimates(&snapshot, ctx, as_of);

    Replayed {
        snapshot,
        accruals,
        annual_rows,
        estimates,
    }
}

/// Per-symbol unrealized tax estimates over the snapshot's open lots, the input
/// `tui`/`summary`/`reports` render (and `sheets-view` projects to the Positions
/// effective-rate column). One estimate per open-position symbol. The position's
/// residency state defaults to `ctx.residency_default`; year-to-date realized is
/// summed from the snapshot's realized gains so the marginal increment stacks on
/// it. (RUNTIME-CYCLE-001)
pub fn unrealized_estimates(
    snapshot: &Snapshot,
    ctx: &TaxContext,
    as_of: Date,
) -> BTreeMap<Symbol, UnrealizedEstimate> {
    // Year-to-date realized, split LT/ST, so the unrealized estimate's marginal
    // increment stacks on the year's already-realized gains (tax::unrealized_estimate
    // takes ytd_st / ytd_lt). Only gains in the context's tax_year count toward YTD.
    let mut ytd_st: i64 = 0;
    let mut ytd_lt: i64 = 0;
    for g in &snapshot.realized_gains {
        if tax::tax_year_of(g.sale_date) != ctx.tax_year {
            continue;
        }
        match tax::classify_term(g.acquire_date, g.sale_date) {
            tax::Term::LongTerm => ytd_lt += g.gain_cents.0,
            tax::Term::ShortTerm => ytd_st += g.gain_cents.0,
        }
    }

    let mut out: BTreeMap<Symbol, UnrealizedEstimate> = BTreeMap::new();
    for (symbol, pos) in &snapshot.positions {
        // A fully closed-out position is not part of the live unrealized estimate.
        if pos.total_qty.0 == 0 {
            continue;
        }
        let est = tax::unrealized_estimate(
            symbol,
            &snapshot.open_lots,
            as_of,
            &ctx.residency_default,
            Cents(ytd_st),
            Cents(ytd_lt),
            ctx,
        );
        out.insert(symbol.clone(), est);
    }
    out
}

/// Run ONE full cycle: replay with the prior cached marks, project the view tabs'
/// inputs, read this run's marks back via `sheets-view`, cache them, and reduce the
/// per-symbol quote-epochs to the single trading-day key. Returns the
/// [`CycleOutcome`] the consumers render plus the **new** marks cache to inject
/// into the next cycle. (RUNTIME-CYCLE-001/002/003)
///
/// On a marks read-back transport failure (offline), the error is **propagated**:
/// the caller **retains its prior cached marks** (and re-injects them next cycle),
/// per the boundary contract `sheets-view::read_marks` documents. `runtime` does
/// not silently emit an empty marks set. For an offline cycle that must still
/// render, the caller uses [`replay_with_cached_marks`] directly (the read-only
/// `summary` path).
pub fn run_cycle<C: SheetsViewClient>(
    logs: &EventLogs,
    prior: &MarksCache,
    ctx: &TaxContext,
    as_of: Date,
    client: &C,
    settle: SettleConfig,
) -> Result<CycleOutcome, ViewError> {
    // 1. Replay with the prior cached marks (the producer half). (RUNTIME-CYCLE-001/002)
    let replayed = replay_with_cached_marks(logs, prior, ctx, as_of);

    // 2. Read this run's marks back (sheets-view settle pass) — keyed on the
    //    snapshot's OPEN-position symbols, carrying the prior good marks forward so
    //    a transiently-unknown symbol is not nulled. `ledger-core::replay` keeps
    //    fully-disposed symbols (qty 0) in `positions` (via realized.keys()), but a
    //    closed position is not on the Positions tab, so it must NOT be priced: it
    //    would otherwise draw a GOOGLEFINANCE read, could be recorded degraded, and
    //    would pollute per-symbol freshness / the trading-day reduction. This filter
    //    matches the symbol set `unrealized_estimates` values (qty != 0).
    //    (RUNTIME-CYCLE-002)
    // @spec RUNTIME-CYCLE-004
    let symbols: Vec<Symbol> = replayed
        .snapshot
        .positions
        .iter()
        .filter(|(_, p)| p.total_qty.0 != 0)
        .map(|(s, _)| s.clone())
        .collect();
    let produced: MarksProduced = read_marks(client, &symbols, &prior.prior_marks(), settle)?;

    // 3. Cache the read-back marks (to inject into the NEXT cycle) and reduce the
    //    per-symbol quote-epochs to the single trading-day key. (RUNTIME-CYCLE-002/003)
    let marks = MarksCache::from_produced(&produced);
    let trading_day_key = reduce_trading_day_key(&marks);

    // 4. Close the loop WITHIN the run: replay again with THIS run's cached marks,
    //    so the held outcome (Snapshot, estimates) is valued at this run's sync —
    //    a one-shot process run (no persisted prior cache) renders priced output,
    //    never blanket-degraded. (RUNTIME-CYCLE-006)
    // @spec RUNTIME-CYCLE-006
    let repriced = replay_with_cached_marks(logs, &marks, ctx, as_of);

    Ok(CycleOutcome {
        snapshot: repriced.snapshot,
        accruals: repriced.accruals,
        annual_rows: repriced.annual_rows,
        estimates: repriced.estimates,
        marks,
        trading_day_key,
    })
}

/// A publishing cycle's result: the cycle outcome plus the republish outcome
/// (which tabs published, which are stale-bannered).
pub struct PublishedCycle {
    /// The replayed snapshot/accruals/estimates + this run's marks (as
    /// [`run_cycle`] returns).
    pub outcome: CycleOutcome,
    /// The view-tab republish outcome (`published` + any stale tabs).
    pub republish: RepublishOutcome,
}

/// The PUBLISHING cycle — the live wiring of the view-publish triggers: replay,
/// render the four view tabs (five typed bands: Positions, Open Lots, Realized,
/// the Tax accrual band, and the Tax reserve summary), then drive `sheets-view`'s
/// ONE serialized republish→settle loop (republish under the advisory write-lock,
/// THEN read the marks back — never concurrently), and reduce the read-back marks
/// exactly as [`run_cycle`] does. The headless summary and the TUI's
/// connect/refresh run this, so the published Google Sheets view exists and the
/// periodic mark-refresh and republish share the one serialized loop.
///
/// @spec SHEET-PUB-001
#[allow(clippy::too_many_arguments)]
pub fn run_cycle_publishing<C, L>(
    logs: &EventLogs,
    prior: &MarksCache,
    ctx: &TaxContext,
    as_of: Date,
    publisher: &mut Publisher<C>,
    lock: &L,
    aliases: &config::AliasMap,
    settle: SettleConfig,
    stale_banner_quote_ts: &str,
) -> Result<PublishedCycle, ViewError>
where
    C: SheetsViewClient,
    L: pt_core::Lock,
{
    // 1. Replay with the prior cached marks (the producer half), as run_cycle.
    let replayed = replay_with_cached_marks(logs, prior, ctx, as_of);

    // 2. Render the view tabs from the replayed state. The Positions effective
    //    rates carry only non-degraded estimates (a degraded symbol renders an
    //    empty rate and its post-tax columns fall back to pre-tax).
    let rates: BTreeMap<Symbol, config::Ppm> = replayed
        .estimates
        .iter()
        .filter_map(|(s, e)| e.effective_rate_ppm.map(|p| (s.clone(), p)))
        .collect();
    let positions = render_positions(&replayed.snapshot, &rates, aliases)?;
    let open_lots = render_open_lots(&replayed.snapshot, as_of);
    let realized = render_realized(&replayed.snapshot, &replayed.accruals);
    let tax_view = render_tax(&replayed.accruals, &replayed.annual_rows);
    let tabs = vec![
        positions,
        open_lots,
        realized,
        tax_view.accruals,
        tax_view.reserve_summary,
    ];

    // 3. Open-position symbols only (closed positions are never priced).
    // @spec RUNTIME-CYCLE-004
    let symbols: Vec<Symbol> = replayed
        .snapshot
        .positions
        .iter()
        .filter(|(_, p)| p.total_qty.0 != 0)
        .map(|(s, _)| s.clone())
        .collect();

    // 4. The ONE serialized republish→settle loop, lock threaded through the
    //    republish half; then cache + reduce exactly as run_cycle.
    let (republish, produced) = publisher.republish_then_settle(
        &tabs,
        &symbols,
        &prior.prior_marks(),
        settle,
        lock,
        stale_banner_quote_ts,
    )?;
    let marks = MarksCache::from_produced(&produced);
    let trading_day_key = reduce_trading_day_key(&marks);

    // 5. Close the loop WITHIN the run, exactly as run_cycle: replay again with
    //    THIS run's cached marks so the held outcome is valued at this run's sync.
    //    The published tabs keep the pre-settle render (the price formulas must be
    //    written before they can settle). (RUNTIME-CYCLE-006)
    // @spec RUNTIME-CYCLE-006
    let repriced = replay_with_cached_marks(logs, &marks, ctx, as_of);

    Ok(PublishedCycle {
        outcome: CycleOutcome {
            snapshot: repriced.snapshot,
            accruals: repriced.accruals,
            annual_rows: repriced.annual_rows,
            estimates: repriced.estimates,
            marks,
            trading_day_key,
        },
        republish,
    })
}

/// A cycle failure when `runtime` owns the WHOLE replay-caller role: either the
/// `store` load (workbook unreachable / cache rebuild failed) or the `sheets-view`
/// marks read-back failed. (RUNTIME-CYCLE-001/002)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CycleError {
    /// `store.load()` failed (the workbook was unreachable, or the cache could not
    /// be rebuilt). The caller retains its prior cached marks and retries.
    /// (RUNTIME-CYCLE-001)
    Load(StoreError),
    /// The `sheets-view` marks read-back failed (offline settle pass). The caller
    /// retains its prior cached marks and re-injects them next cycle.
    /// (RUNTIME-CYCLE-002)
    View(ViewError),
}

impl std::fmt::Display for CycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for CycleError {}

/// `runtime` as the FULL replay caller (RUNTIME-CYCLE-001): **load the event log via
/// `store`** (the half `run_cycle` delegates to its caller), then replay + read marks
/// back + cache + reduce via [`run_cycle`]. This is the entry an entry point
/// (`tui`/`summary`/`import`) drives so `runtime` genuinely owns the load step rather
/// than receiving a pre-built [`EventLogs`].
///
/// `store.load()` confirms cache currency against the workbook and rebuilds on a
/// probe change (the workbook wins), so the loaded `EventLogs` is authoritative.
/// A load failure surfaces as [`CycleError::Load`]; a marks read-back failure as
/// [`CycleError::View`] — in both cases the caller retains its prior cached marks
/// and re-injects them next cycle. (RUNTIME-CYCLE-001/002)
///
/// This is the `load_and_run_cycle` entry the entrypoints call (RUNTIME-BOOT-002):
/// it loads the event log, runs the replay & marks cycle, and returns the held
/// `Snapshot` + accruals + cached marks + trading-day key for the caller to render
/// or act on.
// @spec RUNTIME-BOOT-002
pub fn load_and_run_cycle<S, L, Ca, Vc>(
    store: &mut Store<S, L, Ca>,
    prior: &MarksCache,
    ctx: &TaxContext,
    as_of: Date,
    client: &Vc,
    settle: SettleConfig,
) -> Result<CycleOutcome, CycleError>
where
    S: SheetsClient,
    L: Lock,
    Ca: Cache,
    Vc: SheetsViewClient,
{
    // 1. Load the event log via `store` — runtime owns this step (the replay-caller
    //    role names it). The workbook wins on a probe change. (RUNTIME-CYCLE-001)
    let logs = store.load().map_err(CycleError::Load)?;

    // 2. Replay → read marks back → cache → reduce. (RUNTIME-CYCLE-001/002/003)
    run_cycle(&logs, prior, ctx, as_of, client, settle).map_err(CycleError::View)
}

/// [`load_and_run_cycle`]'s PUBLISHING twin: load via `store`, then run the
/// publish→settle cycle ([`run_cycle_publishing`]). The entry the live binary
/// drives, so every headless/interactive cycle also republishes the view tabs.
///
/// @spec SHEET-PUB-001
#[allow(clippy::too_many_arguments)]
pub fn load_run_and_publish<S, L, Ca, Vc, Pl>(
    store: &mut Store<S, L, Ca>,
    prior: &MarksCache,
    ctx: &TaxContext,
    as_of: Date,
    publisher: &mut Publisher<Vc>,
    lock: &Pl,
    aliases: &config::AliasMap,
    settle: SettleConfig,
    stale_banner_quote_ts: &str,
) -> Result<PublishedCycle, CycleError>
where
    S: SheetsClient,
    L: Lock,
    Ca: Cache,
    Vc: SheetsViewClient,
    Pl: pt_core::Lock,
{
    let logs = store.load().map_err(CycleError::Load)?;
    run_cycle_publishing(
        &logs,
        prior,
        ctx,
        as_of,
        publisher,
        lock,
        aliases,
        settle,
        stale_banner_quote_ts,
    )
    .map_err(CycleError::View)
}

// ===========================================================================
// RUNTIME-CYCLE-005: after a priced cycle, trigger the History capture under the
// advisory write-lock. The cycle held the Snapshot + reduced trading-day key +
// priced marks; runtime calls reports::append_snapshot (via the bounded
// retry-and-flag loop, RUNTIME-REPORTS-001) so a priced cycle's value is durably
// recorded, last-wins per trading-day key (REPORT-HIST-002).
// ===========================================================================

/// A cycle outcome plus the History-capture result it triggered (RUNTIME-CYCLE-005):
/// the held [`CycleOutcome`] (which the entrypoints still render) and, when the cycle
/// produced priced marks, the [`CaptureOutcome`] of the durable History append. A
/// degraded-only cycle (no trading-day key) carries `capture: None` — there is no
/// trading day to record.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CapturedCycle {
    /// The replay/marks cycle result (RUNTIME-CYCLE-001/002/003).
    pub outcome: CycleOutcome,
    /// The History-capture result, or `None` when the cycle produced no priced
    /// marks (nothing to capture). (RUNTIME-CYCLE-005)
    pub capture: Option<CaptureOutcome>,
}

/// Load → run the cycle → and, **after a priced cycle**, trigger the History capture
/// under the advisory write-lock (RUNTIME-CYCLE-005). This is the full
/// produce-then-persist path the entrypoints drive: it runs [`load_and_run_cycle`]
/// (RUNTIME-BOOT-002), then — if the cycle produced priced marks (a reduced
/// trading-day key) — calls `reports::append_snapshot` with that key and the priced
/// marks projected via `to_priced_marks`, through the bounded retry-and-flag loop
/// (RUNTIME-REPORTS-001). The append acquires the advisory write-lock INSIDE
/// `append_snapshot` (RUNTIME-LOCK-002) via the `lock` passed here, and last-wins per
/// trading-day key (REPORT-HIST-002), so a TUI-triggered and a cron-triggered capture
/// for the same day reconcile to one point.
///
/// A degraded-only cycle (no priced marks, no key) records nothing — `capture` is
/// `None` — rather than fabricating a calendar-day point (RUNTIME-CYCLE-003/004).
/// `sleep_fn` is the backoff sleep (the real path passes `thread::sleep`; tests pass
/// a no-op), so the retry loop is exercised without waiting.
// @spec RUNTIME-CYCLE-005, RUNTIME-REPORTS-001
#[allow(clippy::too_many_arguments)]
pub fn load_run_and_capture<S, L, Ca, Vc, Hc, Hl>(
    store: &mut Store<S, L, Ca>,
    prior: &MarksCache,
    ctx: &TaxContext,
    as_of: Date,
    client: &Vc,
    settle: SettleConfig,
    history: &mut Hc,
    history_lock: &Hl,
    policy: &BackoffPolicy,
    reporting_tz_date: Date,
    captured_at_epoch_secs: i64,
    sleep_fn: impl FnMut(std::time::Duration),
) -> Result<CapturedCycle, CycleError>
where
    S: SheetsClient,
    L: Lock,
    Ca: Cache,
    Vc: SheetsViewClient,
    Hc: HistoryClient,
    Hl: store::Lock,
{
    // 1. Load + run the cycle (RUNTIME-CYCLE-001..004, RUNTIME-BOOT-002).
    let outcome = load_and_run_cycle(store, prior, ctx, as_of, client, settle)?;

    // 2. After a PRICED cycle, trigger the History capture under the advisory
    //    write-lock (RUNTIME-CYCLE-005) via the bounded retry-and-flag loop
    //    (RUNTIME-REPORTS-001). A degraded-only cycle (no key) captures nothing.
    let capture = capture_cycle(
        history,
        history_lock,
        &outcome,
        &outcome.estimates,
        reporting_tz_date,
        captured_at_epoch_secs,
        policy,
        sleep_fn,
    );

    Ok(CapturedCycle { outcome, capture })
}
