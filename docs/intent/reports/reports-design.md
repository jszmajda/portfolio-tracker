---
parent: high-level-design
prefix: REPORT
---

# reports

## Context and Design Philosophy

`reports` computes **portfolio analytics** — composition, value-over-time, and realized-P&L
history — as pure functions over the `ledger-core` `Snapshot`, the current marks, `tax`'s
unrealized estimate, and a persisted **value History**. The TUI and `summary` render what it
produces. It does **not** own the quarterly/annual *tax* reports — those are `tax`'s
(`TAX-REPORT-*`); `reports` covers the non-tax analytics.

The defining design fact: **the value series is the one piece of derived data that is not
reconstructable.** Positions, basis, and realized P&L can all be replayed from the event log at
any time — but a past day's portfolio *value* depends on that day's *marks*, and `GOOGLEFINANCE`
only gives live prices. So `reports` splits its work by reconstructability:

- **Reconstructable** (from the log, recomputed on demand): composition, and realized-P&L history.
- **Non-reconstructable** (captured and durably persisted): the value/unrealized History series.

History is therefore **authoritative and lives in the workbook** (durable, Google-backed), unlike
`store`'s rebuildable cache.

Principles:

- **Capture marks, recompute everything else.** Only mark-dependent value/unrealized is stored;
  positions/basis/realized are recomputed from the log.
- **Index value by the trading day, not the run.** A series point is keyed by the marks'
  quote-epoch trading day, so weekend/holiday re-runs don't fabricate flat segments.
- **Degrade, don't fabricate.** A symbol with no mark is degraded in composition and flags its
  History day incomplete — never a zero value.

## Inputs

- **From `ledger-core`:** the current `Snapshot` (positions, open lots, realized gains).
- **From `sheets-view`:** the current marks (symbol → `Cents`, with degraded entries and a
  per-mark quote-epoch).
- **From `tax`:** the per-position unrealized tax estimate (for net-of-tax composition).
- **From `config`:** the reporting timezone (default `US/Eastern` — DC and NJ are both Eastern).
- **From the History tab:** the persisted value series (`reports` owns it).

## Portfolio Composition

Allocation of the portfolio *as of now*, from the `Snapshot` + marks:

- Per **symbol** and per **platform**: market value, share of total (%), total basis, unrealized
  P&L (pre-tax), and net-of-tax unrealized (via `tax`'s estimate). Both pre- and post-tax.
- **Net-of-tax reconciliation.** A symbol's net-of-tax unrealized is derived from `tax`'s **exact**
  per-symbol estimated tax, distributed across that symbol's lots/platforms by **pre-tax weight**
  using **largest-remainder** apportionment, so the per-lot and per-platform net-of-tax sub-totals
  reconcile exactly to the symbol's estimated tax with no penny drift.
- **Consistent degraded set.** A symbol degraded in *either* its mark or its tax estimate is
  degraded in **both** the pre- and post-tax views identically and excluded from the % denominator
  (the sum of *priced* positions), so the two columns are always comparable and shares of the
  priced book sum to 100%.
- **Coverage caveat.** Alongside the degraded *count*, composition surfaces the **priced-coverage
  fraction** = priced basis ÷ total basis (basis survives degradation), so a "100%" dominated by a
  small priced subset (e.g. the largest holding is unpriced) is visibly caveated.
- **Degenerate totals.** If priced market value is **≤ 0**, shares (%) are **n/a**, and the label
  distinguishes *"no positions"* from *"positions exist but unpriced."* A **fully-closed book**
  (every position quantity is zero) is classified as *"no positions."* A position with **negative
  basis** is flagged rather than silently producing a nonsensical basis-share.
- **Gain % of basis.** Each row carries its pre-tax unrealized as a fraction of total basis
  (`ppm`, rounded half-to-even) — the unr-% the owner reads at a glance. It is **n/a** when the
  row is degraded or its basis is ≤ 0 (a closed-out or zero-cost position has no meaningful
  %-of-basis), in the same spirit as the degenerate-totals stance: never a fabricated or
  nonsensical percentage.

## Value-Over-Time (the History series)

A series point records, for one **trading day**: total market value, unrealized P&L (pre- and
post-tax estimate), per-symbol **values**, total basis, the marks used (with their quote-epoch),
the capture timestamp, and an **incomplete** flag if any symbol was degraded that capture.

- **Keyed by trading day.** A point's key is the **trading-day key `runtime` supplies** (its
  reduction of the per-symbol quote-epochs — most-recent across priced symbols), not the calendar
  day of the run. Consecutive captures sharing a quote-epoch (markets closed since — weekend,
  holiday, after-hours) resolve to the **same trading-day point** (last-wins), so the series
  advances only when prices do. The capture timestamp and reporting-TZ date are kept as metadata;
  they are not the series key.
- **Per-symbol comparison is by value.** Per-symbol **value** is split-neutral and is the
  cross-time axis. Share counts, if recorded, are point-in-time metadata and are **never diffed
  across time** (a split rescales them — see `ledger-core`'s split-epoch caveat). The per-symbol
  delta also carries the **percent change** relative to the earlier value (`ppm`, half-to-even);
  it is n/a when either endpoint lacks a priced value or the earlier value is ≤ 0 (no meaningful
  base) — never fabricated.
- **Gaps.** Trading days with no capture show as **explicit gaps** (no interpolation), so a flat
  segment never implies a real flat value.
- **Incomplete days.** A point captured with some symbols degraded is flagged **incomplete** and
  its total marked partial; **deltas across an incomplete point are themselves flagged/suppressed**
  by consumers (`summary`), never silently computed.
- **Series start.** The series begins when capturing began; pre-history daily *values* are gone
  (past marks unrecoverable). `GOOGLEFINANCE`-historical backfill is a deferred option.

## Snapshot Capture & Persistence

- **`append_snapshot(...)`** writes one trading-day point to the durable **History tab** via
  `runtime`'s **locked Sheets-access primitive** (so a TUI-triggered and a cron capture can't race):
  an atomic `batchUpdate`, **read-back-verified**, with retry-and-flag on failure — because a lost
  point is non-reconstructable. Idempotency is **last-wins by trading-day key** (a capture for an
  existing trading day overwrites it) — `reports`' own model, *distinct* from the event log's
  `EventId` skip-if-equal. A retry/re-run never duplicates; a same-day retake refreshes that day.
- **Driver.** The `summary` daily run is the primary driver (append the day's point, then read the
  series for its delta); any run with live marks may also capture — all through the lock.
- **Integrity on read.** Because History is human-editable and **non-reconstructable**, `reports`
  runs an integrity check on read — trading-day keys unique and non-decreasing, rows parse, and a
  content checksum — and on failure **flags loudly** (it cannot recompute), recovery via Google
  Sheets version history. `value-over-time` reads `reports`' **own** History cache, currency-checked
  against the workbook; on divergence the workbook wins and the cache is **re-read (re-downloaded),
  not recomputed** (the point that distinguishes History from `store`'s rebuildable cache).

## Realized-P&L History

Computed **from the event log's realized gains** (reconstructable), grouped by **calendar** year
or an arbitrary date range — explicitly **not** a competing "quarterly" view; the IRS
estimated-period quarterly report is `tax`'s alone. Labels say "calendar" to avoid confusion with
`tax`'s estimated periods.

A calendar-**YTD** row for a single year serves the consumers' current-year glance (the TUI's
summary band): the year's realized gains summed (proceeds / basis / gain), returning an **exact
zero row** when the year has no sales — realized history is reconstructable, so zero is a true
figure, not a degraded state, and a consumer never has to fabricate the empty-year row itself.

## Verification / Invariants

`reports` is aggregation, not money-conservation, so it is **unit-tested**, not Verus-proven.
Properties worth asserting:

- Composition shares over priced positions sum to 100% (or n/a when total ≤ 0), with a coverage
  fraction reported.
- The History series has **one point per trading day**, keys non-decreasing, value-compared only.
- A degraded input yields a degraded/flagged output, never a fabricated number.

## Interfaces

- **Inbound:** `ledger-core` `Snapshot`, `sheets-view` marks, `tax` unrealized estimate, `config`
  reporting TZ, the History tab.
- **Outbound (to `tui`):** composition, value-over-time, realized history to render.
- **Outbound (to `summary`):** the series + today's snapshot append, for the daily delta (with
  incomplete-point flags it must honor).
- **Persistence:** the **History tab** in the workbook (durable, authoritative) via the shared
  Sheets layer, cache-mirrored. `reports` owns its schema; `sheets-view`'s tab-ordering places it
  among the view tabs.
- `reports` does **not** compute or own the quarterly/annual tax reports (`tax` does).

## Decisions & Alternatives

| Decision | Chosen | Alternatives Considered | Rationale |
|----------|--------|------------------------|-----------|
| Series key | Marks' **quote-epoch trading day** | Calendar day of the run | Dating by run day fabricates flat segments on weekends/holidays (same close, "two days"); trading-day keying advances only when prices do. |
| What History stores | Mark-dependent value/unrealized only | Also store realized as the series | Realized/positions/basis are reconstructable from the log; only marks are not — store the irreplaceable part. |
| Realized history grouping | From the log, **calendar** year/range; quarterly is `tax`'s | `reports` offers its own quarterly | Two "quarterly" numbers (calendar vs IRS uneven) would confuse; one owner per period concept. |
| History persistence | Durable workbook tab + integrity check on read | Cache-only; recompute | Non-reconstructable → must be durable and defended like `store`'s log (it's editable). |
| Append discipline | Atomic + read-back-verify + retry-and-flag (store's) | Fire-and-forget | A silently lost point is permanent; mirror the write path that protects the event log. |
| Per-symbol cross-time axis | **Value** (split-neutral); shares never diffed | Compare share counts | Value is split-neutral; share counts jump at a split and would mislead. |
| Degraded composition | Same excluded set in pre- and post-tax; coverage fraction surfaced | Include as zero; exclude only pre-tax | A zero distorts every %, and mismatched sets make pre/post-tax incomparable; coverage caveats a thin "100%". |
| Snapshot cadence | One per trading day, last-wins | Per run; intraday | Daily value matches the portfolio cadence; last-wins keeps re-runs idempotent. |

## Open Questions & Future Decisions

### Resolved
1. ✅ `reports` owns composition + value-over-time + realized history + the durable History tab; `tax` owns tax reports.
2. ✅ Value series keyed by quote-epoch trading day (no fabricated flats); realized history from the log, calendar-grouped.
3. ✅ History durably persisted with integrity check + store-style append discipline (non-reconstructable).
4. ✅ Per-symbol value (split-neutral) is the cross-time axis; composition degraded-set consistent pre/post-tax + coverage fraction.

### Deferred
1. **Historical value backfill.** `GOOGLEFINANCE`-historical reconstruction of pre-capture daily
   values (interacts with the live-only marks model).
2. **Richer analytics.** Sector/asset-class composition (needs classification data not yet
   modelled), time-weighted return, benchmarks.

## References

- HLD: `docs/high-level-design.md` (reports = composition + value-over-time).
- Inputs: `docs/intent/ledger-core/ledger-core-design.md` (`Snapshot`, split-epoch caveat),
  `docs/intent/sheets-view/sheets-view-design.md` (marks + quote-epoch),
  `docs/intent/tax/tax-design.md` (unrealized estimate; tax-owned reports),
  `docs/intent/config/config-design.md` (reporting TZ).
- Consumer/driver: `docs/intent/summary/` (daily append + delta; honors incomplete flags) — to be drafted.
