//! Production `runtime` projection helpers for the binary: project a held
//! [`runtime::CycleOutcome`] (the replay -> project -> read-marks -> cache cycle's
//! result) into the bundles the entry points render — `summary::SummaryInputs` and
//! `tui::ViewState` — plus the per-year `TaxContext` the replay runs against.
//!
//! These are PURE (they take an already-held `CycleOutcome`), so they ARE unit-
//! tested without the network; the live cycle that produces the `CycleOutcome`
//! (in [`crate::tui_app`]) is the only networked step, confirmed by the env-gated
//! e2e and the manual `pt` run. All money stays integer `pt_core::Cents`; no float
//! crosses.

use std::collections::BTreeMap;
use std::error::Error;

use ledger_core::{LedgerEvent, Symbol};
use pt_core::Date;
use runtime::lock::{AdvisoryLock, Clock, LockOutcome};
use runtime::{
    assign_ledger_event_id, assign_tax_event_id, revalidate_ledger, revalidate_tax, Boot,
    CycleOutcome, MarksCache, RevalidateError, StoreLockAdapter,
};
use sheets_view::SettleConfig;
use store::{Cache, Lock, SheetsClient, Store, StoreError};
use tax::{TaxContext, TaxEvent};
use tui::port::{SubmitOutcome, SubmitRejection, WriteFailure};

// ===========================================================================
// Projection from a held CycleOutcome into the entry-point bundles. These are PURE
// (they take an already-held CycleOutcome) so they are unit-tested without the
// network — the live cycle that produces the CycleOutcome is the only networked
// step.
// ===========================================================================

/// Project a held [`CycleOutcome`] into the `summary` input bundle. The numbers
/// come straight off the cycle (snapshot, estimates, annual rows, the priced
/// marks, the reduced trading-day key); `summary` adds no analytics — it captures,
/// deltas, and renders. The bracket state + tax year are supplied (from `config`),
/// the calendar is the History axis the caller threads in.
#[allow(clippy::too_many_arguments)]
pub fn summary_inputs_from_cycle(
    outcome: &CycleOutcome,
    bracket_state: config::BracketState,
    tax_year: i32,
    trading_day_calendar: Vec<reports::TradingDayKey>,
    reporting_tz_date: Date,
    run_at_epoch_secs: i64,
) -> summary::SummaryInputs {
    summary::SummaryInputs {
        snapshot: outcome.snapshot.clone(),
        estimates: outcome.estimates.clone(),
        annual_rows: outcome.annual_rows.clone(),
        marks: outcome.marks.to_priced_marks(),
        trading_day_key: outcome.trading_day_key,
        trading_day_calendar,
        reporting_tz_date,
        run_at_epoch_secs,
        bracket_state,
        tax_year,
    }
}

/// Project a held [`CycleOutcome`] into the `tui` read-state bundle the screens
/// render. `runtime` holds everything; the TUI computes nothing. The freshness map
/// is the cycle's per-symbol stamps; orphan warnings + the next estimated-payment
/// period come from `tax` over the same snapshot. History points + the calendar are
/// threaded in by the caller (the durable series `reports` persists), as are the
/// run wall-clock (`updated_hhmm`) and `config`'s display-name map.
///
/// The masthead's **as-of calendar date** is derived HERE (the binary owns the
/// key→calendar conversion; the TUI renders, it does not derive): the priced
/// trading-day key formats via [`iso_date`], and a key-less or zero-key cycle
/// threads `None` so the TUI reads `no priced day yet` — a raw key int never
/// reaches the screen. (TUI-VIEW-NAV-012/013)
// @spec TUI-VIEW-NAV-012, TUI-VIEW-NAV-013
#[allow(clippy::too_many_arguments)]
pub fn view_state_from_cycle(
    outcome: &CycleOutcome,
    freshness: BTreeMap<Symbol, runtime::SymbolFreshness>,
    history_points: BTreeMap<reports::TradingDayKey, reports::SeriesPoint>,
    trading_day_calendar: Vec<reports::TradingDayKey>,
    orphan_warnings: Vec<tax::OrphanWarning>,
    next_estimated_payment_period: Option<tax::Quarter>,
    bracket_state: config::BracketState,
    staleness: Vec<config::StalenessSignal>,
    tax_year: i32,
    connection: tui::port::Connection,
    updated_hhmm: Option<String>,
    display_names: config::DisplayNameMap,
) -> tui::port::ViewState {
    let as_of_calendar = outcome
        .trading_day_key
        .filter(|k| k.0 .0 != 0)
        .map(|k| iso_date(k.0));
    tui::port::ViewState {
        snapshot: outcome.snapshot.clone(),
        accruals: outcome.accruals.clone(),
        annual_rows: outcome.annual_rows.clone(),
        estimates: outcome.estimates.clone(),
        marks: outcome.marks.to_priced_marks(),
        freshness,
        trading_day_key: outcome.trading_day_key,
        as_of_calendar,
        updated_hhmm,
        display_names,
        trading_day_calendar,
        history_points,
        orphan_warnings,
        connection,
        integrity: None,
        tax_year,
        bracket_state,
        staleness,
        next_estimated_payment_period,
    }
}

/// Bundle the durable History points (as read from the workbook tab) into the
/// pair `ViewState` carries: points keyed by trading day (**last-wins**,
/// mirroring the tab's upsert discipline) and the ascending trading-day calendar
/// (the History axis). A **zero-key point is garbage** — a zero trading-day key
/// means *no priced day* (a capture taken before any symbol priced) — and is
/// dropped here so it never becomes a chart column, a sparkline cell, or a
/// day-change prior. This is what the binary threads into
/// [`view_state_from_cycle`] so the Positions day-change column and the History
/// chart render from real captures — pure projection, unit-tested without the
/// network. (TUI-VIEW-POS-005, TUI-VIEW-HIST-003)
// @spec TUI-VIEW-POS-005, TUI-VIEW-HIST-003
pub fn history_bundle(
    points: Vec<reports::SeriesPoint>,
) -> (
    BTreeMap<reports::TradingDayKey, reports::SeriesPoint>,
    Vec<reports::TradingDayKey>,
) {
    let mut map: BTreeMap<reports::TradingDayKey, reports::SeriesPoint> = BTreeMap::new();
    for p in points {
        if p.key.0 .0 == 0 {
            continue; // a zero key is "no priced day" — a garbage capture
        }
        map.insert(p.key, p); // input order is capture order → last wins
    }
    let calendar: Vec<reports::TradingDayKey> = map.keys().copied().collect();
    (map, calendar)
}

// ===========================================================================
// The live replay cycle, wired THROUGH the `runtime` composition root. This is the
// production wiring the binary's `tui_app` drives (the only networked step): the
// `store` and `sheets-view` client are built by `runtime::Boot` so they ride the
// ONE captured-creds Sheets client and the store's write primitives acquire the
// runtime-owned advisory lock. The live HTTP round-trip is the env-gated e2e; what
// `cargo test` pins (via `Boot::store(StoreLockAdapter::new(boot.lock()))`) is that
// the binary delegates construction to the root rather than re-wiring its own
// collaborators or a `NoopLock`. (RUNTIME-BOOT-001/003, RUNTIME-LOCK-002)
// ===========================================================================

/// Run ONE live PUBLISHING cycle against the workbook THROUGH the composition root:
/// load the event log via the `Boot`-built `store`, replay `ledger-core` then `tax`,
/// republish the view tabs and read the marks back in `sheets-view`'s one serialized
/// loop (the republish half under the advisory write-lock), cache + reduce the
/// trading-day key. Every live run — the headless summary, the TUI's
/// connect/refresh — therefore keeps the published Google Sheets view in existence
/// and current. (SHEET-PUB-001)
///
/// The `store` rides the runtime-owned advisory write-lock (adapted via
/// [`StoreLockAdapter`] over [`Boot::lock`]) inside its write primitives, so
/// single-writer coverage is independent of the caller (RUNTIME-LOCK-002); the view
/// client rides the SAME ONE Sheets client the root captured creds for
/// (RUNTIME-BOOT-001). No entrypoint constructs or re-wires these collaborators.
/// (RUNTIME-BOOT-003)
///
/// @spec SHEET-PUB-001
pub fn run_live_cycle(
    boot: &Boot,
    ctx: &TaxContext,
    as_of: Date,
    aliases: &config::AliasMap,
) -> Result<CycleOutcome, Box<dyn Error>> {
    let mut store = boot.store(StoreLockAdapter::new(boot.lock()))?;
    let view_client = boot.view_client(sheets_view::POSITIONS_TAB)?;
    let mut publisher = sheets_view::Publisher::new(view_client);

    let published = runtime::load_run_and_publish(
        &mut store,
        &MarksCache::new(),
        ctx,
        as_of,
        &mut publisher,
        &StoreLockAdapter::new(boot.lock()),
        aliases,
        SettleConfig::default(),
        &iso_date(as_of),
    )?;
    Ok(published.outcome)
}

/// Days-since-epoch → `YYYY-MM-DD` (the civil-from-days derivation), for the
/// human-readable stale banner and headline stamps the binary renders.
pub fn iso_date(d: Date) -> String {
    // Howard Hinnant's civil_from_days, over days since 1970-01-01.
    let z = d.0 as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{day:02}")
}

/// Epoch seconds → the `HH:MM` wall-clock label the status line's `updated`
/// segment renders, in the reporting timezone. `US/Eastern` (the settings
/// default — DC and NJ are both Eastern) applies the EST/EDT offset with the US
/// DST rule (second Sunday of March → first Sunday of November, transitions at
/// 02:00 local standard time); any other timezone string falls back to UTC.
/// (TUI-VIEW-NAV-013)
// @spec TUI-VIEW-NAV-013
pub fn hhmm_in_reporting_tz(epoch_secs: i64, reporting_timezone: &str) -> String {
    let offset_secs: i64 = if reporting_timezone == "US/Eastern" {
        if is_us_eastern_dst(epoch_secs) {
            -4 * 3600
        } else {
            -5 * 3600
        }
    } else {
        0
    };
    let local = epoch_secs + offset_secs;
    let secs_of_day = local.rem_euclid(86_400);
    format!(
        "{:02}:{:02}",
        secs_of_day / 3_600,
        (secs_of_day % 3_600) / 60
    )
}

/// Whether `epoch_secs` falls in US-Eastern daylight time: from 02:00 EST on the
/// second Sunday of March to 02:00 EDT (01:00 EST) on the first Sunday of
/// November.
fn is_us_eastern_dst(epoch_secs: i64) -> bool {
    // Work in EST (UTC−5) — the standard frame both transition instants are
    // defined against (spring-forward at 02:00 EST; fall-back at 01:00 EST).
    let est = epoch_secs - 5 * 3600;
    let day = est.div_euclid(86_400) as i32;
    let (year, _, _) = civil_from_days(day);
    let second_sunday_march = nth_sunday(year, 3, 2);
    let first_sunday_november = nth_sunday(year, 11, 1);
    let dst_start = (second_sunday_march as i64) * 86_400 + 2 * 3_600; // 02:00 EST
    let dst_end = (first_sunday_november as i64) * 86_400 + 3_600; // 01:00 EST = 02:00 EDT
    est >= dst_start && est < dst_end
}

/// Days-since-epoch of the `n`th Sunday of `(year, month)` (n = 1-based).
fn nth_sunday(year: i32, month: i32, n: i32) -> i32 {
    let first = days_from_civil(year, month, 1);
    // 1970-01-01 was a Thursday; day-of-week 0 = Sunday.
    let dow = (first + 4).rem_euclid(7); // 0 = Sunday
    let first_sunday = first + ((7 - dow) % 7);
    first_sunday + (n - 1) * 7
}

/// Days since 1970-01-01 for the civil date `(y, m, d)` (proleptic Gregorian) —
/// the inverse of [`iso_date`]'s civil-from-days.
fn days_from_civil(y: i32, m: i32, d: i32) -> i32 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) as i64 + 2) / 5 + (d - 1) as i64;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era as i64 * 146_097 + doe - 719_468) as i32
}

/// The civil date `(y, m, d)` for `days` since 1970-01-01.
fn civil_from_days(days: i32) -> (i32, i32, i32) {
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    ((if m <= 2 { y + 1 } else { y }) as i32, m as i32, d as i32)
}

/// The per-year tax context the binary replays against, assembled from the
/// workbook's domain config: the year's `TaxRules` resolve to the federal
/// (ordinary + long-term + NIIT) and per-state jurisdictions, with `config`'s
/// verified / stale / cold-start resolution — an exact-year set is `Verified`,
/// the most recent prior year's is `Stale` (estimates still appear, flagged),
/// and no set at all degrades to the regime-agnostic migration context
/// (`NoBracketsAvailable`, surfaced honestly — never a fabricated tax).
/// (RUNTIME-CYCLE-001; config-design.md → "Resolution helpers")
pub fn context_from_config(data: &config::ConfigData, tax_year: i32, today: Date) -> TaxContext {
    use config::{BracketState, Jurisdiction, TaxYear};
    let year = TaxYear(tax_year);

    // Resolve the year's coherent rules row: exact year → Verified; else the
    // most recent prior year → Stale; else cold-start.
    let (rules, state) = match data.rules_by_year.get(&year) {
        Some(r) => (Some(r), BracketState::Verified),
        None => match data.rules_by_year.range(..year).next_back() {
            Some((_, r)) => (Some(r), BracketState::Stale),
            None => (None, BracketState::NoBracketsAvailable),
        },
    };
    let Some(rules) = rules else {
        // Cold start: no brackets in any year — the honest degrade.
        let mut ctx = tax::migration_context(year);
        ctx.de_minimis_cents = data.de_minimis.0;
        ctx.residency_default = data.residency.residency_on(today);
        return ctx;
    };

    let federal = tax::ResolvedJurisdiction {
        jurisdiction: Jurisdiction::Federal,
        ordinary: Some(rules.federal_ordinary.clone()),
        federal_long_term: Some(rules.federal_long_term.clone()),
        niit: Some(rules.niit.clone()),
        ordinary_income_cents: rules.ordinary_income_cents,
        state,
    };
    let states = rules
        .state_ordinary
        .iter()
        .map(|(code, set)| {
            (
                code.clone(),
                tax::ResolvedJurisdiction {
                    jurisdiction: Jurisdiction::State(code.clone()),
                    ordinary: Some(set.clone()),
                    federal_long_term: None, // states tax all gains as ordinary
                    niit: None,
                    ordinary_income_cents: rules.ordinary_income_cents,
                    state,
                },
            )
        })
        .collect();

    TaxContext {
        tax_year: year,
        federal,
        states,
        de_minimis_cents: data.de_minimis.0,
        residency_default: data.residency.residency_on(today),
    }
}

/// The bracket state the view bundle carries for the current tax year — the
/// single source of the TUI's `[est]` / `[est, brackets stale]` /
/// `n/a (no brackets)` qualifier: `config`'s resolved state when the workbook
/// config is readable, the honest cold-start otherwise. The TUI renders this
/// bundle field; it never resolves brackets itself, and a binary that fails to
/// thread it would annotate verified-bracket figures `n/a (no brackets)` on
/// every row. (TUI-VIEW-POS-011)
// @spec TUI-VIEW-POS-011
pub fn view_bracket_state(
    data: Option<&config::ConfigData>,
    tax_year: i32,
) -> config::BracketState {
    data.map(|d| bracket_state_for(d, tax_year))
        .unwrap_or(config::BracketState::NoBracketsAvailable)
}

/// The bracket state the summary header surfaces for the year: `Verified` when
/// the year's own rules exist, `Stale` on a prior-year fallback, cold otherwise.
pub fn bracket_state_for(data: &config::ConfigData, tax_year: i32) -> config::BracketState {
    use config::{BracketState, TaxYear};
    let year = TaxYear(tax_year);
    if data.rules_by_year.contains_key(&year) {
        BracketState::Verified
    } else if data.rules_by_year.range(..year).next_back().is_some() {
        BracketState::Stale
    } else {
        BracketState::NoBracketsAvailable
    }
}

/// The pre-config fallback context (no workbook config readable — offline /
/// cold): the regime-agnostic migration context.
pub fn context_for_year(tax_year: i32) -> TaxContext {
    tax::migration_context(config::TaxYear(tax_year))
}

// ===========================================================================
// The interactive TUI write path (TUI-ENTRY-FLOW-002/003/004/005). This is the
// authoritative submit the TUI's `LivePort` drives, factored out of the binary's
// live wiring so it is unit-tested against the FAKE store (`InMemorySheets`) + a
// REAL reentrant advisory lock — no network. It closes the live-wiring half of
// RUNTIME-EVENTID-001 and RUNTIME-REVALIDATE-001 by wiring their machinery into a
// real append.
//
// The order (entry-design.md → "The Composer & Write Loop"; the StoreLockAdapter
// held-lock-policy doc): load the REFRESHED view (store rebuilds on a probe change)
// → assign a globally-unique, retry-stable EventId (RUNTIME-EVENTID-001) → re-validate
// the candidate against the refreshed log (RUNTIME-REVALIDATE-001) → acquire the rich
// advisory write-lock (so a foreign Held yields the holder; the store's inner acquire
// is re-entrant against the SAME holder, no deadlock) → store append + read-back-verify
// (STORE-WRITE-001..006) → map to SubmitOutcome.
// ===========================================================================

/// Run the authoritative submit of a LEDGER event through the store + the rich
/// advisory lock, returning the [`SubmitOutcome`] the TUI composer maps to a `Phase`.
///
/// The retry-idempotency contract (TUI-ENTRY-FLOW-010): the composer re-submits the
/// **byte-identical** frozen event on `[r]etry`. The id this assigns is content-addressed
/// (RUNTIME-EVENTID-001 over `store`'s content hash, STORE-SCHEMA-004), so a retry assigns
/// the SAME id and `store`'s idempotency (STORE-WRITE-003) recognises it as the same event
/// — the retry lands exactly once.
// @spec TUI-ENTRY-FLOW-002, TUI-ENTRY-FLOW-003, TUI-ENTRY-FLOW-004, TUI-ENTRY-FLOW-005, TUI-ENTRY-FLOW-010, RUNTIME-EVENTID-001, RUNTIME-REVALIDATE-001
pub fn submit_ledger_through_store<S, L, C, Clk>(
    store: &mut Store<S, L, C>,
    lock: &AdvisoryLock<Clk>,
    candidate: &LedgerEvent,
) -> SubmitOutcome
where
    S: SheetsClient,
    L: Lock,
    C: Cache,
    Clk: Clock,
{
    // 1. Load the REFRESHED view (store rebuilds on a probe change — the workbook
    //    wins), so the re-validation sees any rows appended out-of-band since the
    //    event was composed. A load failure is an unreachable workbook: return
    //    control with the entry intact. (RUNTIME-REVALIDATE-001, STORE-WRITE-001)
    let logs = match store.load() {
        Ok(logs) => logs,
        Err(e) => return write_failed(e),
    };

    // 2. Assign a globally-unique, retry-stable EventId against the cross-tab union
    //    (RUNTIME-EVENTID-001). The byte-identical retry assigns the SAME id, so the
    //    store idempotency recognises the retry as the same event (TUI-ENTRY-FLOW-010).
    //    The composer carries no id (it does not author the id); content de-dup
    //    (`ledger_id_for_existing_content`) reuses the id of an already-landed identical
    //    event so a retry-after-landing is recognised rather than minting a colliding
    //    fresh id. (RUNTIME-EVENTID-001 "same content ⇒ same event")
    let event_id = match ledger_id_for_existing_content(&logs, candidate) {
        Some(existing) => existing,
        None => assign_ledger_event_id(&logs, candidate),
    };
    let stamped = LedgerEvent {
        id: event_id,
        ..candidate.clone()
    };

    // 3. Content de-dup precedes re-validation (RUNTIME-EVENTID-001 "same content ⇒
    //    same event"): if the assigned id is ALREADY in the refreshed log, this is a
    //    byte-identical retry of an event that already landed (TUI-ENTRY-FLOW-010). The
    //    kernel re-validation would falsely reject it (the lot/sale is already in the
    //    log — a DuplicateLotId / already-consumed conflict against ITSELF), so it is
    //    skipped: the append's idempotency check (STORE-WRITE-003) recognises the retry
    //    and lands it exactly once. Re-validation only gates a genuinely NEW event.
    let is_retry = !stamped.id.is_empty() && logs.ledger.iter().any(|e| e.id == stamped.id);
    if !is_retry {
        // Re-validate live against the REFRESHED log BEFORE the write — the authority
        // (RUNTIME-REVALIDATE-001, TUI-ENTRY-FLOW-002). A disagreement is carried back
        // to re-render inline beside the field, distinct from a write-verify failure.
        if let Err(RevalidateError::Ledger(e)) = revalidate_ledger(&logs, &stamped) {
            return SubmitOutcome::Rejected(SubmitRejection::Ledger(e));
        }
    }

    // 4. Acquire the rich advisory write-lock. A foreign Held fails non-destructively
    //    with the holder named (TUI-ENTRY-FLOW-005); an unwritable-path Error returns
    //    control (TUI-ENTRY-FLOW-004). On Acquired we hold the handle across the append
    //    — the store's inner acquire re-enters the SAME holder (no deadlock).
    let _handle = match lock.try_acquire() {
        LockOutcome::Acquired(h) => h,
        LockOutcome::Held { holder, .. } => return SubmitOutcome::LockHeld { holder },
        LockOutcome::Error { .. } => return SubmitOutcome::WriteFailed(WriteFailure::Unreachable),
    };

    // 5. Append + read-back-verify through the store (STORE-WRITE-001..006).
    match store.append_ledger(&stamped) {
        Ok(outcome) => SubmitOutcome::Confirmed(outcome),
        Err(e) => write_failed(e),
    }
}

/// Run the authoritative submit of a TAX event through the store + the rich advisory
/// lock. The tax id is content-addressed inside `store` (RUNTIME-EVENTID-001 prefix-
/// disjoint from ledger ids), so a byte-identical retry is idempotent
/// (TUI-ENTRY-FLOW-010). Same order as the ledger path.
// @spec TUI-ENTRY-FLOW-002, TUI-ENTRY-FLOW-003, TUI-ENTRY-FLOW-004, TUI-ENTRY-FLOW-005, TUI-ENTRY-FLOW-010, RUNTIME-EVENTID-001, RUNTIME-REVALIDATE-001
pub fn submit_tax_through_store<S, L, C, Clk>(
    store: &mut Store<S, L, C>,
    lock: &AdvisoryLock<Clk>,
    ctx: &TaxContext,
    candidate: &TaxEvent,
) -> SubmitOutcome
where
    S: SheetsClient,
    L: Lock,
    C: Cache,
    Clk: Clock,
{
    let logs = match store.load() {
        Ok(logs) => logs,
        Err(e) => return write_failed(e),
    };

    // Content de-dup precedes re-validation (RUNTIME-EVENTID-001): the tax id is the
    // content address `store` persists, so a byte-identical retry recomputes the SAME
    // id. If it is already in the refreshed log this is a retry that already landed
    // (TUI-ENTRY-FLOW-010); re-validation would falsely reject it (the accrual is
    // already Moved/Paid by THIS very event), so it is skipped and the append's
    // idempotency (STORE-WRITE-003) lands it exactly once.
    let content_id = assign_tax_event_id(&logs, candidate);
    let is_retry = runtime::cross_tab_event_ids(&logs).contains(&content_id);
    if !is_retry {
        // Re-validate the tax event against the refreshed realized gains + tax lifecycle
        // (RUNTIME-REVALIDATE-001). A disagreement re-renders inline. (TUI-ENTRY-FLOW-002)
        if let Err(RevalidateError::Tax(e)) = revalidate_tax(&logs, candidate, ctx) {
            return SubmitOutcome::Rejected(SubmitRejection::Tax(e));
        }
    }

    let _handle = match lock.try_acquire() {
        LockOutcome::Acquired(h) => h,
        LockOutcome::Held { holder, .. } => return SubmitOutcome::LockHeld { holder },
        LockOutcome::Error { .. } => return SubmitOutcome::WriteFailed(WriteFailure::Unreachable),
    };

    match store.append_tax(candidate) {
        Ok(outcome) => SubmitOutcome::Confirmed(outcome),
        Err(e) => write_failed(e),
    }
}

/// The id of an EXISTING ledger event whose content matches `candidate` (ignoring the
/// store-assigned id + `Seq`), if any — the "same content ⇒ same event" lookup
/// (RUNTIME-EVENTID-001). A byte-identical retry of an event that already landed reuses
/// that id so `store`'s idempotency recognises it as the same event rather than minting
/// a fresh, colliding id (which would re-validate as a self-conflict —
/// TUI-ENTRY-FLOW-010). Content equality is the same row projection `store`'s
/// idempotency check compares, with the id/`Seq` cells normalised out.
fn ledger_id_for_existing_content(
    logs: &store::EventLogs,
    candidate: &LedgerEvent,
) -> Option<String> {
    let cand_content = ledger_content_cells(candidate);
    logs.ledger
        .iter()
        .find(|e| ledger_content_cells(e) == cand_content)
        .map(|e| e.id.clone())
}

/// The canonical content cells of a ledger event with the id + `Seq` normalised out —
/// the same content `store`'s idempotency compares. (RUNTIME-EVENTID-001)
fn ledger_content_cells(event: &LedgerEvent) -> store::Row {
    let normalised = LedgerEvent {
        id: String::new(),
        seq: pt_core::Seq(0),
        ..event.clone()
    };
    let mut row = store::serde_rows::ledger_to_row(&normalised);
    row.cells.remove("Seq");
    row.cells.remove("EventId");
    row
}

/// Map a `store` write failure to the non-destructive [`SubmitOutcome::WriteFailed`]:
/// a read-back mismatch (a human edit) is `VerifyMismatch`; anything else (an
/// unreachable workbook, a held lock surfaced through the inner adapter, an integrity
/// defect on the rebuild) returns control as `Unreachable`. The entry is preserved
/// and `[r]etry` is offered either way. (TUI-ENTRY-FLOW-004)
fn write_failed(e: StoreError) -> SubmitOutcome {
    match e {
        StoreError::WriteVerifyMismatch => SubmitOutcome::WriteFailed(WriteFailure::VerifyMismatch),
        _ => SubmitOutcome::WriteFailed(WriteFailure::Unreachable),
    }
}
