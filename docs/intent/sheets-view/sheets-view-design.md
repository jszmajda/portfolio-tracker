---
parent: high-level-design
prefix: SHEET
---

# sheets-view

## Context and Design Philosophy

`sheets-view` is the **projection** half of the workbook: it regenerates clean, read-only,
filterable **view tabs** from the kernels' output, and it is the one place the **live price**
enters the system (via `GOOGLEFINANCE`). It consumes `ledger-core`'s `Snapshot` (positions, open
lots, realized gains) and `tax`'s accruals/reserves, writes them as view tabs, and reads the
`GOOGLEFINANCE`-computed prices **back** to cache as the marks the kernels use for unrealized
P&L. It owns no accounting or tax math — it renders and it prices.

It is the counterpart to `store`: `store` owns the canonical append-only **event-log tabs**;
`sheets-view` owns the derived **view tabs**. Both use the shared Sheets access layer; `config`
owns the config tabs. View tabs are **regenerated**, so they are read-only by intent — entry and
correction happen only through the TUI.

Principles:

- **A view is a projection, never an input.** Nothing read from a view tab feeds state except
  the `GOOGLEFINANCE` price marks.
- **Live price lives in the sheet; everything else is engine-written.** Columns that depend on
  the live mark are sheet formulas; everything else is a value the engine computed from the log.
- **The cached mark is authoritative; the live cell is a glance.** The kernels, reports, and TUI
  use the cached mark (timestamped, as of last sync); the in-sheet live columns recompute
  continuously and may differ by up to one refresh cycle — documented, not a discrepancy bug.
- **Failure to publish never corrupts truth.** The canonical log is `store`'s; a failed
  republish only leaves the view stale, to be retried and flagged.

## View Tabs

Four clean, one-row-per-entity, filterable tabs (replacing the hand-wired legacy tabs):

- **Positions** — per symbol: `Symbol`, `Shares`, `Avg Cost/Share`, `Total Basis`, `Price`
  *(formula)*, `Market Value` *(formula)*, `Unrealized P&L` *(formula, pre-tax)*, `Est. Tax Rate`
  *(value, from `tax`)*, `Est. Unrealized Tax` *(formula)*, `Net Unrealized` *(formula, post-tax
  estimate)*, `Unrealized %` *(formula)*, `Realized P&L` *(value)*, `Quote Date` *(formula — the
  machine-read companion the marks read-back parses; see "Marks" below)*. The live glance — pre-
  and post-tax.
- **Open Lots** — per open lot: `Lot ID`, `Symbol`, `Acquire Date`, `Source`, `Shares`,
  `Total Basis`, `Basis/Share`, `Platform`, `Tracking Code`, `Term` (LT/ST as of today).
- **Realized** — per realized gain (replacing legacy "Stock Sales" as a read view): `Sale ID`,
  `Lot ID`, `Symbol`, `Sale Date`, `Proceeds`, `Basis`, `Gain`, `Holding Days`, `Term`, `State`,
  `Tax Year`.
- **Tax** — published as **two separate typed bands**, not one mixed table: a per-accrual data
  band `(Sale ID, Lot ID, Jurisdiction, Tax Year, Amount, Lifecycle State)` (one row per accrual),
  and a physically separated `Tax · Reserve Summary` band per `(Jurisdiction, Tax Year)`: accrued
  / moved / paid / outstanding / shortfall (sourced from `tax::annual_report`). Keeping the
  reserve summary as its own band — rather than summary rows interleaved among the accruals —
  honors the no-among-data-summary-rows rule, so each band filters and sorts
  cleanly on its own. Figures here are kernel-exact (stacked, as of last sync); Positions carries
  the live post-tax *estimate*.

## Workbook Tab Layout

`sheets-view` owns the workbook's tab **ordering** (each segment still owns its own tab's
schema/content). Most-used surfaces sit left (Sheets opens leftmost), config in the middle, the
canonical append-only logs at the far right:

```
Positions │ Open Lots │ Realized │ Tax │ History │ Tax Rules │ Residency │ Platforms & Aliases │ Ledger Events │ Tax Events
└──── views (sheets-view) ────┘ └ reports ┘ └──────── config tabs ────────┘ └──── event logs (store) ────┘
```

(`History` is owned by `reports` — the durable daily snapshot series — but sits among the views
in the ordering.)

`Positions` is the landing tab (the daily post-tax glance). The event-log tabs are last — they
are the canonical truth but are machine-written and read by hand only for audit. Tab order is
established at workbook setup/migration and is idempotently reasserted on republish.

## The Engine-Value / Live-Formula Boundary

The live mark is unknown to the engine (the sheet computes it), so columns split cleanly:

- **Live sheet formulas**: `Price = GOOGLEFINANCE(ticker, "price")`, `Market Value = Price ×
  Shares`, `Unrealized P&L = Market Value − Total Basis`, `Unrealized %`, and the post-tax pair
  `Est. Unrealized Tax = Unrealized P&L × Est. Tax Rate` and `Net Unrealized = Unrealized P&L −
  Est. Unrealized Tax`. All recompute as the mark moves. Each per-row formula references **its
  own row's** cells, written from a fixed anchored start row beneath a **never-rewritten frozen
  header**, so a row shift can never mis-pair them.
- **Engine-written values**: everything log-derived — shares, basis, realized P&L, lots,
  accruals, reserves — plus the per-position **`Est. Tax Rate`** that `tax` computes (below).

**Pre- and post-tax in Positions.** The owner reviews post-tax most, so Positions shows both: a
pre-tax `Unrealized P&L` and a post-tax `Net Unrealized`. The latter uses a per-position
**effective unrealized tax rate** that `tax` writes as a value (recomputed each republish; it
moves slowly). Multiplying a live pre-tax unrealized by an as-of-sync effective rate keeps the
post-tax column **live**, while the principled *stacked* figure — which depends on YTD realized
gains and bracket position — stays kernel-exact in the **Tax tab** and the TUI. The Positions
post-tax columns are labelled **estimates**.

The `Est. Tax Rate` cell is written so Sheets **parses it as a number** (a percent-typed
`userEnteredValue` — e.g. a `"%"`-suffixed value Sheets coerces to a numeric rate), because the
live `Est. Unrealized Tax` and `Net Unrealized` formulas multiply this cell. A rate written as
plain text would make those formulas evaluate against a string and break the post-tax glance, so
the numeric encoding is part of the formula contract, not a formatting nicety.

## Symbol → Ticker Mapping

The ledger `Symbol` is not always the form `GOOGLEFINANCE` expects (class shares like `BRK.B`,
or an exchange prefix like `NASDAQ:AAPL`). `sheets-view` resolves `Symbol → GOOGLEFINANCE ticker`
via **identity plus a small alias table** (config-editable), and builds the `Price` formula from
the resolved ticker. Read-back is keyed by **row identity** (the row's `Symbol`), not by
re-parsing the formula, and **asserts one row per symbol** (guaranteed by `Snapshot.positions`
being a `Map<Symbol,_>`), erroring loudly if violated.

## Marks: Read-Back, Settle & Caching

`sheets-view` is the marks **producer** — it reads `GOOGLEFINANCE` prices back and emits per-symbol
marks; `runtime` caches them, injects them into the next replay, and reduces the per-symbol quote
dates to the one trading-day key `reports`/`summary`/`tui` use:

- **Settle before reading.** Marks are read on a **separate pass**, never inline with writing the
  formulas. After a write, `GOOGLEFINANCE` recalculates asynchronously, so `sheets-view` polls
  the `Price` cells until every one is numeric or terminally errored, with a bounded timeout.
- **Transient vs permanent.** `#N/A` / `Loading...` is treated as **not-yet-known**: retry, and
  keep the prior good cached mark — never overwrite it with a transient. A symbol is recorded as
  **degraded (no mark)** only after it stays non-numeric past the bounded retry window. (A
  permanent `#N/A` usually means a ticker-form mismatch — see the alias table.)
- **Quote-epoch timestamp (per symbol).** Each mark is stamped with `GOOGLEFINANCE`'s own quote
  date (the companion `Quote Date` column on Positions), not wall-clock read time — so a mark read
  after the sheet sat closed overnight is correctly shown as stale, not fresh. These are
  **per-symbol** stamps; `runtime` reduces them to the single trading-day key downstream consumers
  use. Like the `Est. Tax Rate` numeric encoding, the cell's encoding is part of the formula
  contract: the read-back rides the plain *values* API (formatted strings), and `GOOGLEFINANCE`'s
  raw trade time renders as a locale-dependent datetime, so the column's formula converts it to an
  **integer count of days since 1970-01-01** rendered via `TEXT(…,"0")` (empty while
  loading/errored) — a digits-only string every locale formats identically and the settle pass
  parses directly as the kernel `Date`.
- **USD and sub-cent.** Prices are assumed **USD**; conversion is `round_half_to_even(price ×
  100) → Cents`. A positive price that rounds to **0 Cents** is treated as a degraded anomaly,
  not a real zero mark (so value is never silently understated). A **non-positive** reading
  (`0.0` or negative) is likewise a **sub-cent** degraded anomaly, and a **non-finite** reading
  (`NaN`/`±∞`) is a **permanent** degraded anomaly — never a mark — so no bad reading silently
  understates or corrupts value. Foreign-currency listings are out of scope (see Open Questions).
- A non-numeric/degraded symbol feeds `ledger-core`'s per-symbol degradation.
  Offline, the kernels use the last cached marks with their quote-epoch stamps.
- **Transport failure during read-back.** A degraded mark is a per-symbol *price* anomaly read
  back from a settled sheet; a **transport failure** (the read-back itself fails) is different.
  In that case `sheets-view` propagates the error and **emits no marks for that pass** — no
  partial set, no blanket degradation. Carry-forward is **`runtime`'s**: it retains the last
  cached marks and retries, so a transient network failure never erases or false-degrades good
  marks.

## Republish

- **When**: after each successful event append, and on demand; a periodic refresh re-reads marks.
  Republish and the periodic mark-refresh run in **one serialized loop** — never concurrently —
  so neither reads a mid-rewrite/recalculating `Price` column.
- **Advisory write-lock.** Republish is a Sheets-mutating primitive, so `sheets-view::republish`
  **acquires the runtime-owned cross-process advisory write-lock before mutating** the workbook
  and releases it after. The lock type and the canonical acquisition requirement are **owned by
  `runtime`**; `sheets-view` only carries the local obligation to acquire it, so a republish never
  writes concurrently with a store event-append, a reports History-append, or a config write.
- **Atomic rewrite**: each view tab is rewritten as a **single `batchUpdate`** that writes the
  new value/formula range **and truncates the residual tail** to the exact row count — so there
  is no half-written window and no leftover stale rows when positions shrink.
- **Read-only discipline**: data is written by **anchored range from the fixed start row**; the
  frozen header is never rewritten. Only saved **Filter Views** survive a rewrite — a basic
  filter, a manual sort, or a human-inserted row on a view tab is unsupported and is overwritten
  by design (the tabs are read-only).
- **Failure → loud-stale, never silent.** If the `batchUpdate` fails, the canonical log is
  unaffected (it is `store`'s, already appended and verified); the view is left as-is (whole
  rewrite failed, so it is internally consistent with the *prior* snapshot, just behind). The
  hazard is a stale structure carrying **live prices** (e.g. a sold-out symbol still priced), so
  `sheets-view`: (1) records the view stale and **retries** on the next append / periodic / startup
  sync; (2) makes a **best-effort single-cell in-tab banner** ("STALE — last sync <quote ts>");
  (3) surfaces staleness in the **TUI**. If the network is fully down even the banner write
  fails, the TUI remains the staleness surface.

## Filtering

Tabs are one row per entity with typed headers and no among-data summary rows, so native Sheets
filtering and sorting work directly — slice by symbol, platform, term, state, or tax year (the
HLD's "live view with filtering").

## Trust Boundary & Interfaces

- **Inbound (from kernels):** `ledger-core` `Snapshot`, `tax` accruals/reserves, `tax::annual_report`
  (the per-`(Jurisdiction, Tax Year)` accrued / moved / paid / outstanding / shortfall figures that
  populate the `Tax · Reserve Summary` band), and `tax`'s per-position **effective unrealized tax
  rate** — pure values.
- **Inbound (from `config`):** the symbol→ticker alias table.
- **Outbound (to the workbook):** the four view tabs, via the shared Sheets access layer.
- **Outbound (to `runtime`):** the per-symbol marks map (symbol → `Cents`, per-symbol quote-epoch,
  degraded entries); `runtime` caches it, reduces the quote-epochs to one trading-day key, and
  feeds it to `ledger-core`'s replay for unrealized P&L.
- Sits **outside** the `verus!{}` boundary; the price→`Cents` conversion uses the canonical
  rounding.

## Decisions & Alternatives

| Decision | Chosen | Alternatives Considered | Rationale |
|----------|--------|------------------------|-----------|
| Value/formula split | Live-price columns are `GOOGLEFINANCE` formulas (row-anchored); all else engine values | All values; all formulas | The engine doesn't know the live price; row-anchored formulas keep the glance live and correctly paired. |
| Live vs cached authority | Cached mark authoritative for kernels/reports; live cell is a glance, may differ ≤ 1 cycle | Make one the only truth | Both are useful (live glance + stable report); divergence is bounded and documented, not a bug. |
| Post-tax in Positions | Live `Net Unrealized` via `tax`'s per-position effective rate (estimate); stacked-exact in Tax tab/TUI | Pre-tax only; a flat hardcoded rate | The owner reviews post-tax most; an engine-computed effective rate keeps it live and principled, with the exact stacked figure where it matters. |
| Mark read-back | Separate settle pass, poll until numeric-or-errored, bounded timeout | Read inline after writing the formula | A freshly-written formula is transiently `Loading...`/`#N/A`; inline read caches garbage. |
| Transient `#N/A` | Retry + keep prior mark; degrade only after a bounded window | Collapse `#N/A` → degraded immediately | Loading and unpriceable both show `#N/A`; immediate degrade falsely nulls a loading position. |
| Mark timestamp | `GOOGLEFINANCE` quote date | Wall-clock read time | Read time overstates freshness after the sheet sat closed; staleness must be honest. |
| Symbol→ticker | Identity + config alias table; read-back by row identity; assert 1-row-per-symbol | Build/parse formula straight from `Symbol` | Class shares / exchange prefixes else silently false-degrade. |
| Republish atomicity | Single `batchUpdate`: write + truncate tail | Clear-then-write; overwrite-in-place | No half-write window; no phantom trailing rows when positions shrink. |
| Stale handling | Retry + best-effort in-tab banner + TUI surfacing | Log-only flag | A stale structure with live prices is the worst failure; the flag must be where the owner looks. |
| Sub-cent / currency | USD assumed; positive price → 0¢ is a degraded anomaly | Accept 0; multi-currency | A silent 0 mark understates value; multi-currency is out of scope for now. |
| View editability | Read-only, regenerated; only saved Filter Views survive | Bidirectional edit; preserve basic filters | Entry is TUI-only (HLD); a view edit is overwritten on republish by design. |

## Open Questions & Future Decisions

### Deferred
1. **Mark-refresh cadence.** How often the periodic refresh re-reads marks, and how prominently
   mark age is shown in the view/TUI.
2. **Truly-unpriceable tickers.** A manual-mark override for symbols `GOOGLEFINANCE` cannot price
   even with an alias (some funds, foreign listings) — distinct from the alias table.
3. **Goal/alert columns.** Whether to carry the legacy `Goal Date` / `Alert $` affordances into a
   view tab or the TUI.

## References

- HLD: `docs/high-level-design.md` (view tabs, `GOOGLEFINANCE` oracle, read marks back).
- Inputs: `docs/intent/ledger-core/ledger-core-design.md` (`Snapshot`, per-symbol degradation),
  `docs/intent/tax/tax-design.md` (accruals, reserves), `docs/intent/config/config-design.md`
  (alias table).
- Sibling on the workbook: `docs/intent/store/store-design.md` (event-log tabs; marks delegated).
